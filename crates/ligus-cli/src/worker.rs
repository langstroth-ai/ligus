//! The agent worker: drain queued `@mentions` for the agents hosted on this
//! box and answer them. Shared by `ligus worker` (headless daemon) and
//! `ligus serve` (the home host for the iOS app), which also listens for the
//! live events a turn produces through a [`TurnSink`].
//!
//! A turn runs on the agent's workspace runtime (spec §12.5 — never a personal
//! key). HTTP model runtimes that may front a local server get the offline
//! harness's tool loop (built-in file tools + MCP tools, see
//! `ligus_runtime::turn_engine`); file-mutating subprocess agents run in
//! an eagerly cut worktree as before. Either way, a turn's file changes come
//! back as a review-gated `FileDiff` proposal on the chat, synced like the
//! desktop's per-turn proposals.
//!
//! The chat service is shared behind a mutex and only locked for short,
//! synchronous store operations — never across a model call — so an HTTP API
//! running beside the worker stays responsive while a turn streams.

use std::collections::HashSet;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use ligus_core::{workspace_config_session_id, ChatSession, ModelProviderKind, WorkspaceAgent, WorkspaceRuntime};
use ligus_runtime::ligus_mcp::{LigusHost, LigusMcp};
use ligus_runtime::{pending_turns, resolve_workspace_credential, turns_for, ChatService, PendingMention, ResolvedRuntime};
use uuid::Uuid;

use crate::reply::{self, ToolSettings};
use ligus_runtime::turn_engine::{self, Checkout, EngineOptions, Responder, ToolOptions, TurnHost, TurnInput};
use crate::repos::{repos_dir, RepoBindings};
use crate::Config;

/// Lifecycle of one agent turn, as reported to a [`TurnSink`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnState {
    Queued,
    Running,
    Done,
    Error,
}

impl TurnState {
    pub fn as_str(self) -> &'static str {
        match self {
            TurnState::Queued => "queued",
            TurnState::Running => "running",
            TurnState::Done => "done",
            TurnState::Error => "error",
        }
    }
}

/// Live observer of the worker's turns. Every method defaults to a no-op.
pub trait TurnSink: Send + Sync {
    /// A turn changed state.
    fn status(&self, _chat: Uuid, _agent: Uuid, _state: TurnState, _detail: Option<&str>) {}
    /// A streamed reply fragment for message `message` in `chat`.
    fn delta(&self, _chat: Uuid, _message: Uuid, _text: &str) {}
    /// The store changed (a reply began or completed, a proposal was filed).
    fn changed(&self) {}
}

/// The silent sink `ligus worker` uses.
pub struct NoSink;
impl TurnSink for NoSink {}

/// Everything a drain pass needs besides the service.
pub struct WorkerCtx<'a> {
    pub cfg: &'a Config,
    pub ws: Uuid,
    pub my_host: &'a str,
    pub host_label: &'a str,
    pub settings: ToolSettings,
    /// This host's Ligus MCP server (Track D). `None`: agents use the reply
    /// directives only.
    pub ligus: Option<std::sync::Arc<LigusMcp>>,
    /// Host liveness from the relay's presence channel (`ligus serve`), so
    /// the dispatcher never hands over a turn a live device holds. `None`:
    /// every holder counts as live (only a revoked or removed holder's turn
    /// is handed over).
    pub presence: Option<&'a ligus_runtime::HostPresence>,
    /// This host's running turns, each with its own stop signal (`ligus
    /// serve` stops them when a shutdown's grace runs out). `None`: turns
    /// can't be stopped.
    pub stops: Option<std::sync::Arc<ligus_runtime::turn_engine::TurnStops>>,
}

/// What the drain loop remembers between ticks.
#[derive(Default)]
pub struct DrainState {
    /// Mentions whose turn hard-failed here (not retried).
    pub failed: HashSet<Uuid>,
}

/// Where a headless host's chat service lives, for its Ligus MCP server.
pub trait ServiceSource: Send + Sync {
    fn service(&self) -> &Mutex<ChatService>;
    /// Wake the drain loop (a hand-off queued a turn).
    fn kick(&self) {}
    /// An agent asked the chat's humans a question (push it).
    fn ask_posted(&self, _chat: Uuid, _ask: &ligus_core::flow::HumanAsk) {}
    /// The code host behind the chat's bound repository (read-only tools).
    fn forge_for(&self, _chat: Uuid) -> Option<ligus_runtime::ligus_mcp::forge_tools::ForgeBinding> {
        None
    }
}

impl ServiceSource for Mutex<ChatService> {
    fn service(&self) -> &Mutex<ChatService> {
        self
    }
}

/// The Ligus host of `ligus worker` / `ligus serve`: hand-offs are queued
/// (`pending_turns`) and picked up by the next drain pass, which this kicks.
pub struct CliLigusHost(pub std::sync::Arc<dyn ServiceSource>);

impl LigusHost for CliLigusHost {
    fn with_service(&self, f: &mut dyn FnMut(&mut ChatService)) {
        f(&mut lock(self.0.service()))
    }
    fn start_turn(&self, _chat: Uuid, _agent: Uuid, _trigger: Uuid) {
        self.0.kick();
    }
    fn ask_posted(&self, chat: Uuid, ask: &ligus_core::flow::HumanAsk) {
        self.0.ask_posted(chat, ask);
    }
    fn forge_for(&self, chat: Uuid) -> Option<ligus_runtime::ligus_mcp::forge_tools::ForgeBinding> {
        self.0.forge_for(chat)
    }
}

/// A Ligus MCP server over `source`.
pub fn ligus_server(source: std::sync::Arc<dyn ServiceSource>) -> std::sync::Arc<LigusMcp> {
    LigusMcp::new(std::sync::Arc::new(CliLigusHost(source)))
}

fn lock(svc: &Mutex<ChatService>) -> MutexGuard<'_, ChatService> {
    svc.lock().unwrap_or_else(|e| e.into_inner())
}

/// Claude Code permission args for a headless worker. **Read-only by default** —
/// a worker that can write files (and, with bypass, run shell commands) is
/// powerful, so write access is opt-in via
/// `LIGUS_WORKER_PERMISSION_MODE=acceptEdits|bypassPermissions`. Only then does a
/// worker's claude turn actually edit files (and get worktree-isolated + turned
/// into a review-gated proposal).
fn worker_claude_args() -> Vec<String> {
    match ligus_core::names::var("LIGUS_WORKER_PERMISSION_MODE").as_deref() {
        Ok("acceptEdits") => vec!["--permission-mode".into(), "acceptEdits".into()],
        Ok("bypassPermissions") => vec!["--permission-mode".into(), "bypassPermissions".into()],
        _ => Vec::new(),
    }
}

/// The worker's permission tier (`LIGUS_WORKER_PERMISSION_MODE`), read-only
/// (`default`) unless raised. Also accepts `runCommands` for the
/// subscription CLIs.
fn worker_permission_mode() -> String {
    match ligus_core::names::var("LIGUS_WORKER_PERMISSION_MODE").as_deref() {
        Ok(m @ ("acceptEdits" | "runCommands" | "bypassPermissions")) => m.to_string(),
        _ => "default".to_string(),
    }
}

/// Permission-tier args for a runtime on this worker: Claude Code's
/// `--permission-mode`, or the `agy` / `copilot` equivalents.
fn worker_args(provider: ModelProviderKind, mode: &str) -> Vec<String> {
    match provider {
        ModelProviderKind::ClaudeCode => worker_claude_args(),
        ModelProviderKind::Antigravity => ligus_runtime::provider::antigravity::permission_args(mode),
        ModelProviderKind::Copilot => ligus_runtime::provider::copilot::permission_args(mode),
        _ => Vec::new(),
    }
}

/// Whether a keyless runtime is usable: native Ollama, or an OpenAI-wire
/// server on this machine / the LAN / the tailnet (a llama.cpp gateway).
/// Subscription CLI agents (claude / codex / agy / copilot) are keyless too:
/// their login lives in the CLI's own config on this host.
fn keyless_ok(wr: &WorkspaceRuntime) -> bool {
    wr.provider == ModelProviderKind::Ollama
        || matches!(
            wr.provider,
            ModelProviderKind::ClaudeCode | ModelProviderKind::Codex | ModelProviderKind::Antigravity | ModelProviderKind::Copilot
        )
        || (matches!(wr.provider, ModelProviderKind::Custom | ModelProviderKind::LigusDaemon)
            && ligus_core::endpoint::endpoint_is_local_server(&wr.endpoint))
}

/// Serve `GET /v1/health` (always 200 while the process runs, with the
/// sandbox tier) and `GET /v1/ready` (200 once `ready` is set, else 503) on
/// `addr`, for an orchestrator's liveness and readiness probes. Nothing else
/// is served there and nothing secret is returned.
pub async fn spawn_health(addr: &str, ready: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Result<()> {
    use axum::{http::StatusCode, routing::get, Json, Router};
    let tier = ligus_runtime::executor::sandbox_status().tier();
    let r = ready.clone();
    let app = Router::new()
        .route(
            "/v1/health",
            get(move || async move {
                Json(serde_json::json!({ "ok": true, "version": env!("CARGO_PKG_VERSION"), "role": "worker", "sandbox": tier }))
            }),
        )
        .route(
            "/v1/ready",
            get(move || {
                let ok = r.load(std::sync::atomic::Ordering::Relaxed);
                async move {
                    let code = if ok { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
                    (code, Json(serde_json::json!({ "ready": ok })))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| anyhow!("worker health listener {addr}: {e}"))?;
    println!("worker: health on http://{}/v1/health", listener.local_addr()?);
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("worker: health listener stopped: {e}");
        }
    });
    Ok(())
}

/// Where this host's local runtimes come from: `LIGUS_RUNTIMES_FILE` (a
/// read-only mounted file, e.g. a Kubernetes ConfigMap), else
/// `<data>/runtimes.toml`.
pub fn local_runtimes_path(data_dir: &std::path::Path) -> std::path::PathBuf {
    crate::env_opt("LIGUS_RUNTIMES_FILE").map(std::path::PathBuf::from).unwrap_or_else(|| data_dir.join(LOCAL_RUNTIMES_FILE))
}

/// The host-local runtimes file inside the data dir.
pub const LOCAL_RUNTIMES_FILE: &str = "runtimes.toml";

#[derive(serde::Deserialize, Default)]
struct LocalRuntimesToml {
    #[serde(default, rename = "runtime")]
    runtimes: Vec<WorkspaceRuntime>,
}

/// This box's own runtimes (`<data>/runtimes.toml`, the `ligus config export`
/// shape: `[[runtime]] id, name, provider, model, endpoint, credential`).
///
/// The desktop binds an agent to one of its *device-local* runtimes, whose id
/// means nothing on another box; a worker only knows the workspace-owned
/// runtimes on the config log. This file maps such ids to what the agent runs
/// on here — typically the lab's keyless llama.cpp gateway — and an entry with
/// `id = "*"` catches every agent sent to this host whose runtime isn't known.
/// It is the box owner's own config, never synced. Workspace runtimes win.
pub fn local_runtimes(data_dir: &std::path::Path) -> Vec<WorkspaceRuntime> {
    let path = local_runtimes_path(data_dir);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    match toml::from_str::<LocalRuntimesToml>(&text) {
        Ok(doc) => doc.runtimes,
        Err(e) => {
            eprintln!("worker: ignoring {}: {e}", path.display());
            Vec::new()
        }
    }
}

/// The runtime an agent runs on here: a workspace runtime with its id, else a
/// host-local one with its id, else the host-local catch-all (`id = "*"`).
/// `runtimes` is workspace runtimes first, then [`local_runtimes`].
pub fn runtime_for<'a>(runtimes: &'a [WorkspaceRuntime], agent: &WorkspaceAgent) -> Option<&'a WorkspaceRuntime> {
    runtimes.iter().find(|r| r.id == agent.runtime_id).or_else(|| runtimes.iter().find(|r| r.id == "*"))
}

/// Build a ResolvedRuntime for an agent from its workspace runtime (or this
/// host's own, see [`local_runtimes`]). A detached agent never runs on a
/// member's personal key (§12.5) — this returns an error if neither knows the
/// agent's runtime.
pub fn agent_workspace_runtime(
    runtimes: &[WorkspaceRuntime],
    agent: &WorkspaceAgent,
) -> Result<(ResolvedRuntime, String)> {
    let wr = runtime_for(runtimes, agent).ok_or_else(|| {
        anyhow!(
            "@{} runs on '{}', which is neither a workspace runtime nor in this host's {LOCAL_RUNTIMES_FILE}",
            agent.name,
            agent.runtime_id
        )
    })?;
    let gateway = Some(wr.gateway.trim()).filter(|g| !g.is_empty()).map(ligus_runtime::model_gateway::GatewaySelection::named);
    // Through a gateway the runtime's own key is never sent: the gateway's
    // credential (this host's operator config) is.
    let api_key = if gateway.is_some() { None } else { resolve_workspace_credential(&wr.credential) };
    if api_key.is_none() && gateway.is_none() && !keyless_ok(wr) {
        bail!("workspace runtime '{}' has no secret on this box (provision LIGUS_WS_SECRET_…)", wr.name);
    }
    // claude-code / agy / copilot take the worker's permission tier; other
    // providers ignore args here.
    let args = worker_args(wr.provider, &worker_permission_mode());
    let rt = ResolvedRuntime {
        provider: wr.provider,
        model: wr.model.clone(),
        endpoint: wr.endpoint.clone(),
        api_key,
        args,
        model_provider_id: None,
        model_base_url: None,
        context_window_tokens: None,
        keep_alive: None,
        think: None,
        // Only local-capable runtimes act on it (`runs_agent_tools`).
        tools: reply::worker_tools_enabled(),
        max_tool_rounds: None,
        allow_remote_file_tools: false,
        driver: wr.driver,
        gateway,
    };
    Ok((rt, wr.id.clone()))
}

/// One drain pass: answer every queued mention for an agent this worker hosts
/// that another device hasn't already claimed. Returns `Err` only on a
/// tick-fatal store error (the caller logs and retries) — per-mention failures
/// are isolated so one bad mention can't abort the whole pass.
pub async fn drain_tick(
    ctx: &WorkerCtx<'_>,
    svc: &Mutex<ChatService>,
    drain: &mut DrainState,
    sink: &dyn TurnSink,
) -> Result<()> {
    let failed = &mut drain.failed;
    // Bound the hard-failed set over a long uptime — past a generous cap,
    // drop it so transiently-failed mentions get another chance and memory
    // can't creep unbounded.
    if failed.len() >= 8192 {
        failed.clear();
    }
    let (work, runtimes, refused) = {
        let mut svc = lock(svc);
        // A box that can't write (revoked, or awaiting its owner's approval)
        // can't claim or answer anything either.
        if svc.write_block().is_some() {
            return Ok(());
        }
        // Liveness is published to the relay's presence channel by the caller
        // (`ligus serve`'s presence loop, `ligus worker`'s loop), not the log.
        let hosted: HashSet<Uuid> = svc
            .load(workspace_config_session_id(ctx.ws))?
            .map(|s| s.workspace_agents)
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.host_id == ctx.my_host)
            .map(|a| a.id)
            .collect();
        let mut runtimes = svc.list_workspace_runtimes(ctx.ws)?;
        runtimes.extend(local_runtimes(&ctx.cfg.data_dir));
        let mut candidates: Vec<PendingMention> = Vec::new();
        // This workspace's chats, each read as (workspace, chat).
        for id in svc.store().sessions_of_workspace(ctx.ws)? {
            // Skip a session that fails to load rather than aborting the whole tick.
            let s = match svc.load_in(ctx.ws, id) {
                Ok(Some(s)) => s,
                Ok(None) => continue,
                Err(e) => {
                    eprintln!("worker: load {id} failed: {e}");
                    continue;
                }
            };
            // Only this workspace's chats: the store may still hold another
            // workspace's events (an older client pushed them into the room).
            if s.workspace_id != ctx.ws || id == workspace_config_session_id(s.workspace_id) {
                continue;
            }
            // Mentions, @mention cascades and Ligus hand-offs (D.4.1).
            candidates.extend(
                pending_turns(&s)
                    .into_iter()
                    .filter(|pm| hosted.contains(&pm.agent_id) && !failed.contains(&pm.message_id)),
            );
        }
        // The device-keyed dispatcher (design repos-and-checkouts 11): this
        // host is eligible for every mention of an agent it hosts. It claims
        // with its signed coverage of the chat's repo (the repo gate's own
        // check, read-only, before claiming: review F8), waits while a
        // covering host goes first, and hands over a turn whose holder went
        // silent. Back off when another device holds it.
        let roster = svc.workspace_roster(ctx.ws).ok();
        let online: HashSet<Uuid> = match ctx.presence {
            Some(p) => {
                let now = ligus_core::Timestamp::now();
                svc.list_hosts(ctx.ws).unwrap_or_default().iter().filter(|h| p.is_online(h, &now)).filter_map(|h| Uuid::parse_str(&h.id).ok()).collect()
            }
            None => HashSet::new(),
        };
        let live = |d: Uuid| ctx.presence.is_none() || online.contains(&d);
        let mut work = Vec::new();
        let mut refused = false;
        for pm in candidates {
            let Some(s) = svc.load(pm.session_id)? else { continue };
            // Agent access controls: this host runs the agent on its own
            // credentials, so who may use it (and its daily budgets) is
            // decided here, before claiming. A refused turn is answered with
            // a short note and drops out of the queue.
            match svc.gate_agent_turn(pm.session_id, s.workspace_id, pm.message_id, pm.agent_id) {
                Ok(Ok(_)) => {}
                Ok(Err(note)) => {
                    eprintln!("worker: @{} in {} refused: {note}", pm.agent_name, pm.session_id);
                    // Told after the service is released (the sink reads it).
                    refused = true;
                    continue;
                }
                Err(e) => {
                    eprintln!("worker: access check for {} failed: {e}", pm.message_id);
                    continue;
                }
            }
            let coverage = match roster.as_ref() {
                Some(r) => ligus_runtime::repos::dispatch_coverage(svc.store(), svc.device_id(), &s, r, &ligus_runtime::repos::cloning()),
                None => ligus_core::dispatch::Coverage::NoRepo,
            };
            // The same claim key the desktop uses (a hand-off is keyed by
            // its receiver), and the service's own wait clock per holder.
            let receiver = ligus_runtime::claim_receiver(&s, &pm);
            let dctx = ligus_runtime::DispatchCtx { eligible: true, coverage, waited: std::time::Duration::ZERO, holder_live: &live };
            match svc.dispatch_turn(pm.session_id, s.workspace_id, pm.message_id, receiver, dctx) {
                Ok(ligus_core::dispatch::Step::Run) => work.push(pm),
                Ok(_) => {}
                Err(e) => eprintln!("worker: claim {} failed: {e}", pm.message_id),
            }
        }
        (work, runtimes, refused)
    };
    if refused {
        sink.changed();
    }
    for pm in &work {
        sink.status(pm.session_id, pm.agent_id, TurnState::Queued, None);
    }
    for pm in work {
        // Shutting down: the rest waits for the next start.
        if ctx.stops.as_ref().is_some_and(|s| s.is_closed()) {
            sink.status(pm.session_id, pm.agent_id, TurnState::Done, Some("the host is shutting down"));
            continue;
        }
        if let Err(e) = run_turn(ctx, svc, &runtimes, &pm, sink).await {
            eprintln!("worker: @{} in {} failed: {e}", pm.agent_name, pm.session_id);
            failed.insert(pm.message_id);
            sink.status(pm.session_id, pm.agent_id, TurnState::Error, Some(&e.to_string()));
        }
    }
    Ok(())
}

/// Apply every open request that names this host (Phase B, "Apply on
/// ‹host›"): once each, in the chat's worktree of this host's checkout, with
/// the result written back. Never on this host's own approval: only a
/// person's `ProposalApplyRequested` names a host.
pub fn apply_tick(ctx: &WorkerCtx<'_>, svc: &Mutex<ChatService>) -> Vec<(Uuid, ligus_runtime::apply_host::Outcome)> {
    let repo_for = |chat: Uuid| {
        // Called only for a proposal without a repo: one with a repo applies
        // only in this box's checkout of that repo (apply_host::checkout_for,
        // in the apply tick).
        let repo = RepoBindings::load(&ctx.cfg.data_dir).resolve(&repos_dir(&ctx.cfg.data_dir), chat);
        if let Some(r) = repo.as_deref() {
            crate::repos::ensure_chat_base(r);
        }
        repo
    };
    // This box serves one workspace: its pin is the service's own.
    let founder = lock(svc).trusted_founder().map(str::to_string);
    let tick = ligus_runtime::sync_service::apply_tick(svc, ctx.ws, founder.as_deref(), &repo_for);
    if let Some(e) = &tick.listing_error {
        eprintln!("apply: listing requests failed: {e}");
    }
    for (request, proposal, outcome) in &tick.results {
        match outcome {
            Ok(o) => eprintln!("apply: request {request} on proposal {proposal}: {o:?}"),
            Err(e) => eprintln!("apply: request {request} failed: {e}"),
        }
    }
    tick.outcomes()
}

/// The note a reply cut off by a restart ends with.
pub const INTERRUPTED_NOTE: &str = "[interrupted: the host restarted before this reply finished. Ask again to retry.]";

/// Startup recovery, run before the worker loop: finish every reply *this
/// device* was still streaming when the process stopped (a restart or Ctrl-C
/// mid-turn). The placeholder is stamped `reply_to` the mention, so
/// `pending_mentions` already counts it as the answer and the turn would never
/// run again; left alone it would also stay `streaming` forever on every peer.
/// Each one is completed with the text checkpointed so far plus
/// [`INTERRUPTED_NOTE`], so the asker sees it failed and can ask again.
/// Returns `(chat, message)` for each reply finished this way.
pub fn recover_interrupted_turns(svc: &mut ChatService) -> Result<Vec<(Uuid, Uuid)>> {
    // Startup of `ligus worker` and `ligus serve` both pass here: warn once
    // when LIGUS_MCP_MIN_ROLE sits below this box's default floor (S3).
    ligus_runtime::author_gate::warn_if_lowered(worker_permission_mode() != "default");
    let me = svc.device_id();
    let mut out = Vec::new();
    for id in svc.store().list_session_ids()? {
        let s = match svc.load(id) {
            Ok(Some(s)) => s,
            Ok(None) => continue,
            Err(e) => {
                eprintln!("worker: recovery skipped {id}: {e}");
                continue;
            }
        };
        if id == workspace_config_session_id(s.workspace_id) {
            continue;
        }
        for m in s.messages.iter().filter(|m| m.is_streaming && m.origin_device_id == Some(me)) {
            let body = if m.body.trim().is_empty() {
                format!("[error] {INTERRUPTED_NOTE}")
            } else {
                format!("{}\n\n{INTERRUPTED_NOTE}", m.body.trim_end())
            };
            match svc.complete_assistant_message(s.id, s.workspace_id, m.id, body) {
                Ok(()) => out.push((s.id, m.id)),
                Err(e) => eprintln!("worker: finishing interrupted reply {} failed: {e}", m.id),
            }
        }
    }
    Ok(out)
}

/// Total wall-clock backstop for one turn (the provider's idle timeout is the
/// real "wedged" gate), as in the app: `LIGUS_TURN_TIMEOUT_SECS`, default 1 h.
pub(crate) fn turn_timeout() -> Duration {
    Duration::from_secs(
        ligus_core::names::var("LIGUS_TURN_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&s| s > 0)
            .unwrap_or(3600),
    )
}

/// Answer one claimed mention end to end: placeholder, streamed reply (with
/// throttled crash-recovery checkpoints), final body, and the turn's file
/// changes as a proposal.
/// `s` as of the message being answered: the agent replies to *that* message
/// ("Reply to the latest message"), not to a later post in the same burst
/// addressed to someone else ("@ligus …" then "@coder …" sent together, each
/// answered on its own host). The cascade trigger is the trailing reply, so
/// nothing is cut there.
pub(crate) fn through_trigger(s: &ChatSession, trigger: Uuid) -> ChatSession {
    let mut s = s.clone();
    if let Some(at) = s.messages.iter().position(|m| m.id == trigger) {
        s.messages.truncate(at + 1);
    }
    s
}

async fn run_turn(
    ctx: &WorkerCtx<'_>,
    svc: &Mutex<ChatService>,
    runtimes: &[WorkspaceRuntime],
    pm: &PendingMention,
    sink: &dyn TurnSink,
) -> Result<()> {
    // `Queued` was already reported: every exit must end in Done or Error, or
    // the phone's turn strip waits forever.
    // The chat of this worker's workspace: its log is (workspace, chat).
    let Some(s) = lock(svc).load_in(ctx.ws, pm.session_id)? else {
        sink.status(pm.session_id, pm.agent_id, TurnState::Done, Some("chat no longer exists"));
        return Ok(());
    };
    let Some(agent) = s.workspace_agents.iter().find(|a| a.id == pm.agent_id).cloned() else {
        sink.status(pm.session_id, pm.agent_id, TurnState::Done, Some("agent no longer exists"));
        return Ok(());
    };
    let (mut rt, rt_label) = agent_workspace_runtime(runtimes, &agent)?;
    // The gateway's attribution (when its operator turned it on) hashes these.
    if let Some(g) = rt.gateway.as_mut() {
        g.workspace_id = Some(s.workspace_id);
        g.agent_id = Some(agent.id);
    }
    // The host-side repo gate (repos-and-checkouts 5.3, review F8): a chat
    // with a repo runs only in this box's confirmed checkout of it, never
    // in other code; the turn is already claimed, so waiting on a clone is
    // safe.
    let repo_turn = match crate::repos_cmd::repo_for_turn(&ctx.cfg.data_dir, svc, &s, ctx.host_label).await {
        Ok(r) => r,
        Err(reason) => {
            let _ = lock(svc).post_system_note(s.id, s.workspace_id, format!("@{} did not run: {reason}", agent.name));
            sink.changed();
            return Err(anyhow!("refused by the repo gate"));
        }
    };
    let mut turns = turns_for(&through_trigger(&s, pm.message_id), Some(agent.id), &agent.name);
    let (head, me) = {
        let g = lock(svc);
        (g.attachment_rules(s.workspace_id).unwrap_or_default(), g.author().id.clone())
    };
    crate::materialize_turns(ctx.cfg, &s, &head, &me, &mut turns).await;
    let system = format!(
        "You are @{}, an agent in a Ligus workspace chat. Reply to the latest message concisely and helpfully.\n\n{}\n\n{}",
        agent.name,
        ligus_runtime::prompt::runtime_identity_block(&rt, &format!("worker {}", ctx.host_label)),
        // Which checkout (and branch) this turn's files come from, re-read
        // every turn: the phone can switch a chat's repo between turns.
        repo_turn.context
    );
    // Who the turn answers (what the reply's `@you` means).
    let system = match ligus_runtime::prompt::requester_guidance(&s, Some(pm.message_id)) {
        g if g.is_empty() => system,
        g => format!("{system}\n\n{g}"),
    };
    // H1 gate: resolve the triggering message's author + role. At or above
    // this host's floor (Admin for a write-capable tier, Contributor for a
    // read-only one; LIGUS_MCP_MIN_ROLE overrides) the turn runs at its
    // configured tier; below it a CLI runs read-only or is refused and an
    // HTTP model gets no tools. Unresolved author → fail closed.
    // The role is the one the author held when they posted the message (the
    // config roster at its frontier), not today's; an agent's message (a
    // hand-off, an @mention cascade) has none of its own and resolves to the
    // lowest of the humans behind its chain (the rule every host shares).
    let author_role = lock(svc).trigger_author_role_in(ctx.ws, pm.session_id, pm.message_id).ok().flatten();
    // Phase C: the shell is the Owner's (every person behind the turn a
    // workspace Owner), or an Owner granted it to this agent.
    let shell_role = lock(svc).trigger_shell_role_in(ctx.ws, pm.session_id, pm.message_id).ok().flatten();
    // The signed grants, when this host applies them (`LIGUS_SANDBOX_GRANTS`;
    // by default only on the workspace Owner's own hosts).
    let grants = lock(svc).sandbox_grants_here(s.workspace_id, ligus_runtime::sandbox::grants_setting_from_env()).0;
    let shell_allowed = ligus_runtime::author_gate::shell_allowed(shell_role, grants.shell_granted(agent.id));
    let mut rt = rt;
    let min_role = reply::mcp_min_role(ligus_runtime::author_gate::turn_writes(rt.provider, &rt.args));
    let gate_plan = ligus_runtime::author_gate::gate_turn(rt.provider, &mut rt.args, author_role);
    let allow_tools = gate_plan.allows_tools();
    if !allow_tools {
        eprintln!(
            "withholding tools for message {} — author role {:?} below minimum {:?}",
            pm.message_id, author_role, min_role
        );
        // Guard signal (device-local, content-free): the worker MCP role gate.
        let subject = match author_role {
            Some(_) => ligus_core::signal::GuardSubject::Nestmate,
            None => ligus_core::signal::GuardSubject::Unknown,
        };
        let _ = lock(svc).record_signal(
            s.workspace_id,
            Some(s.id),
            &ligus_core::signal::GuardSignal::GuardDecision {
                gate: ligus_core::signal::GuardGate::ToolCall,
                verdict: ligus_core::signal::GuardVerdict::Reject,
                subject,
                source: "worker".into(),
                reason: "role_floor".into(),
            },
        );
    }

    // The workspace's sandbox envelope (dearth mode, a chat lockdown): enforce
    // it for this runtime or refuse the turn, exactly as the desktop does.
    let envelope = ligus_core::sandbox::envelope_for_session(&s);
    let enforcement = ligus_runtime::sandbox::enforce(rt.provider, &envelope, &Vec::new());
    if let Some(reason) = enforcement.refuse {
        let _ = lock(svc).post_system_note(s.id, s.workspace_id, format!("@{} did not run: {reason}", agent.name));
        sink.changed();
        // An error puts the mention in the failed set, so it isn't retried
        // (and the note re-posted) every tick.
        return Err(anyhow!("refused by the workspace sandbox"));
    }
    if let ligus_runtime::author_gate::GatePlan::Refuse(reason) = &gate_plan {
        let _ = lock(svc).post_system_note(s.id, s.workspace_id, format!("@{} did not run: {reason}", agent.name));
        sink.changed();
        return Err(anyhow!("refused by the tool role gate"));
    }
    // No OS sandbox on this box: a CLI agent turn is refused at any tier.
    if let Err(reason) = ligus_runtime::executor::turn_allowed(rt.provider) {
        let _ = lock(svc).post_system_note(s.id, s.workspace_id, format!("@{} did not run: {reason}", agent.name));
        sink.changed();
        return Err(anyhow!("refused: no agent sandbox on this host"));
    }
    // Without the shell the CLI runs without its shell tool; one that can't
    // drop it is refused.
    if let Err(reason) = ligus_runtime::author_gate::gate_shell(rt.provider, &mut rt.args, shell_allowed) {
        let _ = lock(svc).post_system_note(s.id, s.workspace_id, format!("@{} did not run: {reason}", agent.name));
        sink.changed();
        return Err(anyhow!("refused: no shell for this turn"));
    }
    rt.args.extend(enforcement.extra_args);
    let mut tool_settings = ctx.settings.clone();
    tool_settings.run_command &= enforcement.allow_run_command && shell_allowed;

    // The MCP servers this turn may reach (`LIGUS_MCP_CONFIG`, narrowed by
    // the envelope), read only for a runtime that lists them (the Anthropic
    // tool loop, an HTTP runtime that runs the tool loop): a broken config
    // fails such a turn, as before, and never a CLI agent's.
    let lists_mcp = (rt.provider == ModelProviderKind::Anthropic && rt.api_key.is_some()) || rt.runs_agent_tools();
    let mcp_servers: Vec<_> = if allow_tools && lists_mcp {
        crate::mcp_config::load_from_env()?.into_iter().filter(|s| envelope.egress.allows_mcp(&s.id)).collect()
    } else {
        Vec::new()
    };

    // A file-mutating subprocess agent runs in an eagerly cut worktree (like
    // the desktop) so its edits return as a review-gated proposal. Only turns
    // that can actually write: a write-mode claude or aider/pi/codex/hermes.
    let write_capable = match rt.provider {
        ModelProviderKind::ClaudeCode => rt.args.iter().any(|a| a == "acceptEdits" || a == "bypassPermissions"),
        ModelProviderKind::Antigravity | ModelProviderKind::Copilot => worker_permission_mode() != "default",
        ModelProviderKind::Pi | ModelProviderKind::Aider | ModelProviderKind::Codex | ModelProviderKind::Hermes => true,
        _ => false,
    };
    // Every child of this turn runs in this host's OS sandbox under the
    // turn's envelope and gate.
    let mut policy = ligus_runtime::executor::TurnPolicy::new(ligus_runtime::executor::HostKind::Worker, envelope.clone(), gate_plan.clone());
    policy.shell_allowed = shell_allowed;
    policy.grants = grants.paths_for(Some(agent.id));

    sink.status(s.id, agent.id, TurnState::Running, None);
    println!("↳ @{} replying in {} …", ligus_runtime::display::shown(&agent.name), s.id);
    let host = WorkerHost {
        svc,
        sink,
        chat: s.id,
        ligus: ctx.ligus.clone(),
        mcp_servers,
        // Only a bound checkout is ever a turn's directory: never the
        // service's own cwd. A read-only turn with no worktree runs in a
        // fresh per-turn scratch dir (`executor::prepare`).
        checkout: Checkout {
            context: String::new(),
            workspace_root: None,
            repo_root: repo_turn.repo_root.clone(),
            base: repo_turn.base.as_ref().map(|(_, tip, base)| (tip.clone(), base.clone())),
        },
        agent_name: agent.name.clone(),
    };
    // This turn's own stop signal, registered while it runs.
    let stop_guard = ctx.stops.as_ref().map(|t| t.register(s.id));
    let input = TurnInput {
        workspace_id: s.workspace_id,
        chat_id: s.id,
        session: through_trigger(&s, pm.message_id),
        // Stamp the mention this reply answers, so pending_mentions correlates
        // it precisely even when several tasks target the same agent.
        reply_to: Some(pm.message_id),
        responder: Responder {
            author: agent.name.clone(),
            effect_author: format!("@{}", agent.name),
            runtime_id: rt_label.clone(),
            agent_id: Some(agent.id),
            runtime: rt,
        },
        system,
        turns,
        gate: gate_plan,
        envelope,
        shell_allowed,
        policy,
        executor: ligus_runtime::executor::default_executor(),
        tools: ToolOptions {
            builtin: tool_settings.builtin_tools,
            // No in-place fallback on a headless host.
            write_in_place: false,
            run_command: tool_settings.run_command,
            mcp_after_reads: tool_settings.mcp_after_reads,
            remote_file_tools: tool_settings.allow_remote_file_tools,
        },
        write_capable,
        // Never run a writing agent in place: on the always-on box that
        // would be the bound checkout itself, or the service's home.
        require_isolation: true,
        stop: stop_guard.as_ref().map(|g| g.signal()),
        timeout: turn_timeout(),
        options: EngineOptions { empty_reply_notice: true, warm_context_probe: true, ..Default::default() },
    };
    // On the heap: the engine's future is large, and the serve worker's
    // thread (a current-thread runtime) would carry it on its own stack,
    // which overflowed on Windows.
    let result = Box::pin(turn_engine::run(&host, input)).await;
    drop(stop_guard);
    match result {
        Ok(outcome) => {
            sink.changed();
            if outcome.empty {
                sink.status(s.id, agent.id, TurnState::Error, Some("the model returned an empty reply"));
                println!("   @{} returned an empty reply", ligus_runtime::display::shown(&agent.name));
            } else {
                sink.status(s.id, agent.id, TurnState::Done, None);
                println!("   @{} done", ligus_runtime::display::shown(&agent.name));
            }
            Ok(())
        }
        Err(e) if e.stopped => {
            // Only a shutdown stops a worker's turn: say so, so the asker
            // knows to ask again.
            if e.message_id.is_some() {
                let _ = lock(svc).post_system_note(s.id, s.workspace_id, format!("@{} stopped: the host is shutting down. Ask again to retry.", agent.name));
            }
            sink.changed();
            Err(anyhow!("{}", e.message))
        }
        Err(e) => {
            sink.changed();
            Err(anyhow!("{}", e.message))
        }
    }
}

/// `ligus worker` / `ligus serve` as the turn engine's host.
struct WorkerHost<'a> {
    svc: &'a Mutex<ChatService>,
    sink: &'a dyn TurnSink,
    chat: Uuid,
    ligus: Option<std::sync::Arc<LigusMcp>>,
    mcp_servers: Vec<ligus_runtime::McpServerSpec>,
    checkout: Checkout,
    agent_name: String,
}

impl TurnHost for WorkerHost<'_> {
    fn service(&self) -> &Mutex<ChatService> {
        self.svc
    }
    fn mcp_servers(&self, _egress: &ligus_core::sandbox::EgressPolicy, _shell: bool) -> impl std::future::Future<Output = Vec<ligus_runtime::McpServerSpec>> + Send {
        let servers = self.mcp_servers.clone();
        async move { servers }
    }
    fn ligus(&self) -> Option<std::sync::Arc<LigusMcp>> {
        self.ligus.clone()
    }
    fn checkout(&self, _session: &ChatSession, _message_id: Uuid) -> impl std::future::Future<Output = std::result::Result<Checkout, String>> + Send {
        // The box's repo gate ran before the turn was posted.
        let c = self.checkout.clone();
        async move { Ok(c) }
    }
    fn isolation_refusal(&self, _author: &str) -> String {
        format!(
            "[error] @{} can edit files, so it only runs in an isolated worktree of a repository bound on this host \
             (`ligus bind-repo`), and none could be cut for this chat",
            self.agent_name
        )
    }
    fn delta(&self, message_id: Uuid, text: &str) {
        self.sink.delta(self.chat, message_id, text);
    }
    fn changed(&self) {
        self.sink.changed();
    }
    fn error_text(&self, error: &str) -> String {
        format!("provider: {error}")
    }
    fn timeout_text(&self, secs: u64) -> String {
        format!("runtime timed out after {secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ligus_core::{WorkspaceCredential, WorkspaceRuntime};

    fn rt_with(provider: ModelProviderKind, args: &[&str]) -> ResolvedRuntime {
        let wr = WorkspaceRuntime::new("gate", "Gate", provider, "m");
        let mut rt = agent_workspace_runtime(&[wr], &agent("gate")).map(|r| r.0).unwrap_or_else(|_| {
            // Runtimes that need a secret on this box: shape one by hand.
            let wr = WorkspaceRuntime::new("gate", "Gate", ModelProviderKind::ClaudeCode, "m");
            let mut rt = agent_workspace_runtime(&[wr], &agent("gate")).unwrap().0;
            rt.provider = provider;
            rt
        });
        rt.args = args.iter().map(|a| a.to_string()).collect();
        rt
    }

    fn gate_subprocess_tools(rt: &mut ResolvedRuntime, allow: bool) -> std::result::Result<(), String> {
        use ligus_core::WorkspaceRole;
        let role = if allow { WorkspaceRole::Owner } else { WorkspaceRole::Viewer };
        let plan = ligus_runtime::author_gate::gate(rt.provider, Some(role), WorkspaceRole::Admin);
        ligus_runtime::author_gate::apply(&plan, rt.provider, &mut rt.args)
    }

    /// A member below the tool floor must not get a write- or shell-capable
    /// CLI turn on this box (the CLI's own tools bypass Ligus's tool loop).
    #[test]
    fn below_the_floor_cli_agents_run_read_only_or_not_at_all() {
        let mut rt = rt_with(ModelProviderKind::ClaudeCode, &["--permission-mode", "bypassPermissions"]);
        gate_subprocess_tools(&mut rt, false).unwrap();
        assert!(!rt.args.iter().any(|a| a == "bypassPermissions" || a == "acceptEdits"), "{:?}", rt.args);
        let mut rt = rt_with(ModelProviderKind::Copilot, &["--allow-all-tools"]);
        gate_subprocess_tools(&mut rt, false).unwrap();
        assert!(rt.args.contains(&"--deny-tool=shell".to_string()) && !rt.args.contains(&"--allow-all-tools".to_string()));
        for p in [ModelProviderKind::Codex, ModelProviderKind::Aider, ModelProviderKind::Pi, ModelProviderKind::Hermes] {
            assert!(gate_subprocess_tools(&mut rt_with(p, &[]), false).is_err(), "{p:?}");
        }
        // At or above the floor nothing changes.
        let mut rt = rt_with(ModelProviderKind::ClaudeCode, &["--permission-mode", "bypassPermissions"]);
        gate_subprocess_tools(&mut rt, true).unwrap();
        assert!(rt.args.iter().any(|a| a == "bypassPermissions"));
    }

    fn agent(rt: &str) -> WorkspaceAgent {
        WorkspaceAgent::new("coder", rt)
    }

    /// Two posts in one burst ("@coder …" then "@ligus …"): the box's queue
    /// sees the @coder mention even though it isn't the chat's last message,
    /// and @coder answers it without the later @ligus post as its "latest".
    #[test]
    fn a_burst_of_posts_queues_each_mention_and_answers_it_in_context() {
        use ligus_core::{ChatMessage, MessageRole};
        let mut s = ChatSession::new("t", Uuid::nil(), "rt");
        let coder = agent("gw");
        let coder_id = coder.id;
        s.workspace_agents = vec![coder];
        let ask = ChatMessage::new(MessageRole::User, "Alex", "@coder run the tests");
        let other = ChatMessage::new(MessageRole::User, "Alex", "@ligus what's the status?");
        let ask_id = ask.id;
        s.messages = vec![ask, other];
        let queued = ligus_runtime::pending_mentions(&s);
        assert_eq!(queued.len(), 1);
        assert_eq!((queued[0].message_id, queued[0].agent_id), (ask_id, coder_id));
        let seen = turns_for(&through_trigger(&s, ask_id), Some(coder_id), "coder");
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert!(format!("{seen:?}").contains("run the tests") && !format!("{seen:?}").contains("status"));
        // Unknown trigger (deleted): the transcript is left whole.
        assert_eq!(through_trigger(&s, Uuid::new_v4()).messages.len(), 2);
    }

    #[test]
    fn keyless_local_gateway_resolves_with_tools_on() {
        let mut wr = WorkspaceRuntime::new("gw", "Lab gateway", ModelProviderKind::Custom, "gpt-oss:20b");
        wr.endpoint = "http://100.101.102.103:8080/v1/chat/completions".into();
        let (rt, label) = agent_workspace_runtime(&[wr], &agent("gw")).unwrap();
        assert_eq!(label, "gw");
        assert!(rt.api_key.is_none());
        assert!(rt.runs_agent_tools(), "a tailnet llama.cpp gateway runs the tool loop");
    }

    #[test]
    fn local_runtimes_map_device_local_ids_and_catch_all() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(LOCAL_RUNTIMES_FILE),
            r#"
[[runtime]]
id = "lab"
name = "Lab gateway"
provider = "custom"
model = "gpt-oss:20b"
endpoint = "http://100.101.102.103:8080/v1/chat/completions"

[[runtime]]
id = "*"
name = "Default"
provider = "custom"
model = "gemma4:12b"
endpoint = "http://127.0.0.1:8080/v1/chat/completions"
credential = { kind = "none" }
"#,
        )
        .unwrap();
        let rts = local_runtimes(dir.path());
        assert_eq!(rts.len(), 2);
        assert_eq!(runtime_for(&rts, &agent("lab")).unwrap().model, "gpt-oss:20b");
        assert_eq!(runtime_for(&rts, &agent("desktop-local-id")).unwrap().model, "gemma4:12b");
        let (rt, _) = agent_workspace_runtime(&rts, &agent("desktop-local-id")).unwrap();
        assert!(rt.runs_agent_tools());
        // A workspace runtime with the agent's id wins over the catch-all.
        let mut all = vec![WorkspaceRuntime::new("desktop-local-id", "WS", ModelProviderKind::Ollama, "llama3.2")];
        all.extend(rts);
        assert_eq!(runtime_for(&all, &agent("desktop-local-id")).unwrap().name, "WS");
        assert!(runtime_for(&[], &agent("x")).is_none());
    }

    #[test]
    fn subscription_clis_are_keyless_and_read_only_by_default() {
        for p in [ModelProviderKind::Antigravity, ModelProviderKind::Copilot, ModelProviderKind::ClaudeCode] {
            let wr = WorkspaceRuntime::new("cli", "CLI", p, "");
            let (rt, _) = agent_workspace_runtime(&[wr], &agent("cli")).unwrap();
            assert!(rt.api_key.is_none());
            assert!(!rt.runs_agent_tools(), "CLI agents bring their own tools");
        }
        let d = worker_args(ModelProviderKind::Copilot, "default");
        assert!(d.contains(&"--deny-tool=shell".to_string()));
        assert!(worker_args(ModelProviderKind::Antigravity, "default").is_empty());
        assert_eq!(worker_args(ModelProviderKind::Antigravity, "acceptEdits"), vec!["--mode", "accept-edits"]);
        assert!(worker_args(ModelProviderKind::Ollama, "bypassPermissions").is_empty());
    }

    #[test]
    fn keyless_hosted_api_is_refused() {
        let mut wr = WorkspaceRuntime::new("oa", "OpenAI", ModelProviderKind::OpenAI, "gpt-4o");
        wr.endpoint = "https://api.openai.com/v1/chat/completions".into();
        wr.credential = WorkspaceCredential::SecretRef { secret_ref: "definitely_unset_ref_xyz".into() };
        assert!(agent_workspace_runtime(&[wr], &agent("oa")).is_err());
    }

    #[test]
    fn hosted_api_never_gets_agent_tools() {
        let mut wr = WorkspaceRuntime::new("oa", "OpenAI", ModelProviderKind::OpenAI, "gpt-4o");
        wr.credential = WorkspaceCredential::Inline { secret: "sk-test".into() };
        let (rt, _) = agent_workspace_runtime(&[wr], &agent("oa")).unwrap();
        assert!(!rt.runs_agent_tools());
    }

    #[test]
    fn builtin_tools_need_a_private_endpoint_unless_opted_in() {
        let mut wr = WorkspaceRuntime::new("gw", "GW", ModelProviderKind::Custom, "m");
        wr.endpoint = "https://models.example.com/v1/chat/completions".into();
        wr.credential = WorkspaceCredential::Inline { secret: "k".into() };
        let (rt, _) = agent_workspace_runtime(&[wr], &agent("gw")).unwrap();
        let mut settings = ToolSettings::default();
        assert!(!reply::wants_builtin_tools(&rt, &settings), "public endpoint: no file tools by default");
        settings.allow_remote_file_tools = true;
        assert!(reply::wants_builtin_tools(&rt, &settings));
        settings.builtin_tools = false;
        assert!(!reply::wants_builtin_tools(&rt, &settings));
    }
}

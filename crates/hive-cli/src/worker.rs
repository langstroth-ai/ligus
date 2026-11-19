//! The agent worker: drain queued `@mentions` for the agents hosted on this
//! box and answer them. Shared by `ligus worker` (headless daemon) and
//! `ligus serve` (the home host for the iOS app), which also listens for the
//! live events a turn produces through a [`TurnSink`].
//!
//! A turn runs on the agent's workspace runtime (spec §12.5 — never a personal
//! key). HTTP model runtimes that may front a local server get the offline
//! harness's tool loop (built-in file tools + MCP tools, see
//! [`reply::generate_reply_streaming`]); file-mutating subprocess agents run in
//! an eagerly cut worktree as before. Either way, a turn's file changes come
//! back as a review-gated `FileDiff` proposal on the chat, synced like the
//! desktop's per-turn proposals.
//!
//! The chat service is shared behind a mutex and only locked for short,
//! synchronous store operations — never across a model call — so an HTTP API
//! running beside the worker stays responsive while a turn streams.

use std::collections::HashSet;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use hive_core::{workspace_config_session_id, ChatSession, ModelProviderKind, WorkspaceAgent, WorkspaceRuntime};
use hive_runtime::ligus_mcp::{Budgets, LigusHost, LigusMcp, TurnSpec};
use hive_runtime::{pending_turns, resolve_workspace_credential, turns_for, ChatService, PendingMention, ResolvedRuntime};
use uuid::Uuid;

use crate::reply::{self, ToolSettings, TurnFiles};
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
}

/// Where a headless host's chat service lives, for its Ligus MCP server.
pub trait ServiceSource: Send + Sync {
    fn service(&self) -> &Mutex<ChatService>;
    /// Wake the drain loop (a hand-off queued a turn).
    fn kick(&self) {}
    /// An agent asked the chat's humans a question (push it).
    fn ask_posted(&self, _chat: Uuid, _ask: &hive_core::flow::HumanAsk) {}
    /// The code host behind the chat's bound repository (read-only tools).
    fn forge_for(&self, _chat: Uuid) -> Option<hive_runtime::ligus_mcp::forge_tools::ForgeBinding> {
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
    fn ask_posted(&self, chat: Uuid, ask: &hive_core::flow::HumanAsk) {
        self.0.ask_posted(chat, ask);
    }
    fn forge_for(&self, chat: Uuid) -> Option<hive_runtime::ligus_mcp::forge_tools::ForgeBinding> {
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
    match hive_core::compat::var("LIGUS_WORKER_PERMISSION_MODE").as_deref() {
        Ok("acceptEdits") => vec!["--permission-mode".into(), "acceptEdits".into()],
        Ok("bypassPermissions") => vec!["--permission-mode".into(), "bypassPermissions".into()],
        _ => Vec::new(),
    }
}

/// The worker's permission tier (`LIGUS_WORKER_PERMISSION_MODE`), read-only
/// (`default`) unless raised. Also accepts `runCommands` for the
/// subscription CLIs.
fn worker_permission_mode() -> String {
    match hive_core::compat::var("LIGUS_WORKER_PERMISSION_MODE").as_deref() {
        Ok(m @ ("acceptEdits" | "runCommands" | "bypassPermissions")) => m.to_string(),
        _ => "default".to_string(),
    }
}

/// Permission-tier args for a runtime on this worker: Claude Code's
/// `--permission-mode`, or the `agy` / `copilot` equivalents.
fn worker_args(provider: ModelProviderKind, mode: &str) -> Vec<String> {
    match provider {
        ModelProviderKind::ClaudeCode => worker_claude_args(),
        ModelProviderKind::Antigravity => hive_runtime::provider::antigravity::permission_args(mode),
        ModelProviderKind::Copilot => hive_runtime::provider::copilot::permission_args(mode),
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
        || (matches!(wr.provider, ModelProviderKind::Custom | ModelProviderKind::HiveDaemon)
            && hive_core::endpoint::endpoint_is_local_server(&wr.endpoint))
}

/// Serve `GET /v1/health` (always 200 while the process runs, with the
/// sandbox tier) and `GET /v1/ready` (200 once `ready` is set, else 503) on
/// `addr`, for an orchestrator's liveness and readiness probes. Nothing else
/// is served there and nothing secret is returned.
pub async fn spawn_health(addr: &str, ready: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Result<()> {
    use axum::{http::StatusCode, routing::get, Json, Router};
    let tier = hive_runtime::executor::sandbox_status().tier();
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
    let gateway = Some(wr.gateway.trim()).filter(|g| !g.is_empty()).map(hive_runtime::model_gateway::GatewaySelection::named);
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
    failed: &mut HashSet<Uuid>,
    sink: &dyn TurnSink,
) -> Result<()> {
    // Bound the hard-failed set over a long uptime — past a generous cap,
    // drop it so transiently-failed mentions get another chance and memory
    // can't creep unbounded.
    if failed.len() >= 8192 {
        failed.clear();
    }
    let (work, runtimes) = {
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
        for id in svc.store().list_session_ids()? {
            // Skip a session that fails to load rather than aborting the whole tick.
            let s = match svc.load(id) {
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
        // Claim each turn so a worker and the agent-owner's desktop (or another
        // worker) don't both answer the same mention. Back off if we lost.
        let mut work = Vec::new();
        for pm in candidates {
            let Some(ws) = svc.load(pm.session_id)?.map(|s| s.workspace_id) else { continue };
            match svc.claim_turn(pm.session_id, ws, pm.message_id) {
                Ok(true) => work.push(pm),
                Ok(false) => {}
                Err(e) => eprintln!("worker: claim {} failed: {e}", pm.message_id),
            }
        }
        (work, runtimes)
    };
    for pm in &work {
        sink.status(pm.session_id, pm.agent_id, TurnState::Queued, None);
    }
    for pm in work {
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
pub fn apply_tick(ctx: &WorkerCtx<'_>, svc: &Mutex<ChatService>) -> Vec<(Uuid, hive_runtime::apply_host::Outcome)> {
    let repo_for = |chat: Uuid| {
        // Only a proposal without a repo applies in the legacy binding; one
        // with a repo applies only in this box's checkout of that repo
        // (apply_host::checkout_for, in the apply tick).
        let repo = RepoBindings::load(&ctx.cfg.data_dir).resolve(&repos_dir(&ctx.cfg.data_dir), chat);
        if let Some(r) = repo.as_deref() {
            crate::repos::ensure_chat_base(r);
        }
        repo
    };
    // This box serves one workspace: its pin is the service's own.
    let founder = lock(svc).trusted_founder().map(str::to_string);
    let tick = hive_runtime::sync_service::apply_tick(svc, ctx.ws, founder.as_deref(), &repo_for);
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
    hive_runtime::author_gate::warn_if_lowered(worker_permission_mode() != "default");
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
        hive_core::compat::var("LIGUS_TURN_TIMEOUT_SECS")
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
/// addressed to someone else ("@hive …" then "@coder …" sent together, each
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
    let Some(s) = lock(svc).load(pm.session_id)? else {
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
    crate::materialize_turns(ctx.cfg, &mut turns).await;
    let system = format!(
        "You are @{}, an agent in a Ligus workspace chat. Reply to the latest message concisely and helpfully.\n\n{}\n\n{}",
        agent.name,
        hive_runtime::prompt::runtime_identity_block(&rt, &format!("worker {}", ctx.host_label)),
        // Which checkout (and branch) this turn's files come from, re-read
        // every turn: the phone can switch a chat's repo between turns.
        repo_turn.context
    );
    // Who the turn answers (what the reply's `@you` means).
    let system = match hive_runtime::prompt::requester_guidance(&s, Some(pm.message_id)) {
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
    let author_role = lock(svc).trigger_author_role(pm.session_id, pm.message_id).ok().flatten();
    let mut rt = rt;
    let min_role = reply::mcp_min_role(hive_runtime::author_gate::turn_writes(rt.provider, &rt.args));
    let gate_plan = hive_runtime::author_gate::gate_turn(rt.provider, &mut rt.args, author_role);
    let allow_tools = gate_plan.allows_tools();
    if !allow_tools {
        eprintln!(
            "withholding tools for message {} — author role {:?} below minimum {:?}",
            pm.message_id, author_role, min_role
        );
        // Guard signal (device-local, content-free): the worker MCP role gate.
        let subject = match author_role {
            Some(_) => hive_core::signal::GuardSubject::Nestmate,
            None => hive_core::signal::GuardSubject::Unknown,
        };
        let _ = lock(svc).record_signal(
            s.workspace_id,
            Some(s.id),
            &hive_core::signal::GuardSignal::GuardDecision {
                gate: hive_core::signal::GuardGate::ToolCall,
                verdict: hive_core::signal::GuardVerdict::Reject,
                subject,
                source: "worker".into(),
                reason: "role_floor".into(),
            },
        );
    }

    // The workspace's sandbox envelope (dearth mode, a chat lockdown): enforce
    // it for this runtime or refuse the turn, exactly as the desktop does.
    let envelope = hive_core::sandbox::envelope_for_session(&s);
    let enforcement = hive_runtime::sandbox::enforce(rt.provider, &envelope, &Vec::new());
    if let Some(reason) = enforcement.refuse {
        let _ = lock(svc).post_system_note(s.id, s.workspace_id, format!("@{} did not run: {reason}", agent.name));
        sink.changed();
        // An error puts the mention in the failed set, so it isn't retried
        // (and the note re-posted) every tick.
        return Err(anyhow!("refused by the workspace sandbox"));
    }
    if let hive_runtime::author_gate::GatePlan::Refuse(reason) = &gate_plan {
        let _ = lock(svc).post_system_note(s.id, s.workspace_id, format!("@{} did not run: {reason}", agent.name));
        sink.changed();
        return Err(anyhow!("refused by the tool role gate"));
    }
    // No OS sandbox on this box: a CLI agent turn is refused at any tier.
    if let Err(reason) = hive_runtime::executor::turn_allowed(rt.provider) {
        let _ = lock(svc).post_system_note(s.id, s.workspace_id, format!("@{} did not run: {reason}", agent.name));
        sink.changed();
        return Err(anyhow!("refused: no agent sandbox on this host"));
    }
    rt.args.extend(enforcement.extra_args);
    let mut tool_settings = ctx.settings.clone();
    tool_settings.run_command &= enforcement.allow_run_command;
    tool_settings.mcp_egress = envelope.egress.clone();
    tool_settings.protected_dirs = envelope.protected_dirs.clone();

    // The checkout this chat runs in on this box (the gate above).
    let repo_root = repo_turn.repo_root.clone();

    let mid = lock(svc).begin_assistant_message(
        s.id,
        s.workspace_id,
        agent.name.clone(),
        rt_label.clone(),
        Some(agent.id),
        // Stamp the mention this reply answers, so pending_mentions correlates
        // it precisely even when several tasks target the same agent.
        Some(pm.message_id),
    )?;
    sink.changed();
    sink.status(s.id, agent.id, TurnState::Running, None);
    println!("↳ @{} replying in {} …", agent.name, s.id);

    // A file-mutating subprocess agent runs in an eagerly cut worktree (like
    // the desktop) so its edits return as a review-gated proposal. Only turns
    // that can actually write: a write-mode claude or aider/pi/codex/hermes.
    let write_capable = match rt.provider {
        ModelProviderKind::ClaudeCode => rt.args.iter().any(|a| a == "acceptEdits" || a == "bypassPermissions"),
        ModelProviderKind::Antigravity | ModelProviderKind::Copilot => worker_permission_mode() != "default",
        ModelProviderKind::Pi | ModelProviderKind::Aider | ModelProviderKind::Codex | ModelProviderKind::Hermes => true,
        _ => false,
    };
    // Only a bound checkout is ever a turn's directory: never the service's
    // own cwd (its home, or the data dir). A read-only turn with no worktree
    // runs in a fresh per-turn scratch dir (`executor::prepare`).
    let eager = match repo_root.as_deref() {
        Some(root) if write_capable && hive_runtime::git_worktree::is_git_repo(root) => {
            match hive_runtime::git_worktree::cut_turn_worktree(root, &s.id.to_string(), &mid.to_string()) {
                Ok(wt) => Some(wt),
                Err(e) => {
                    eprintln!("worker: worktree unavailable: {e}");
                    None
                }
            }
        }
        _ => None,
    };
    // Never run a writing agent in place: on the always-on box that would be
    // the bound checkout itself, or the service's working directory (its home),
    // with no review-gated proposal in between.
    if write_capable && eager.is_none() {
        let e = anyhow!(
            "@{} can edit files, so it only runs in an isolated worktree of a repository bound on this host \
             (`ligus bind-repo`), and none could be cut for this chat",
            agent.name
        );
        let _ = lock(svc).complete_assistant_message(s.id, s.workspace_id, mid, format!("[error] {e}"));
        sink.changed();
        return Err(e);
    }
    let work_dir = eager.as_ref().map(|w| w.path.to_string_lossy().into_owned());
    let files = TurnFiles { repo_root, chat_id: s.id.to_string(), turn_key: mid.to_string() };

    // Deltas go to the sink on every fragment; the store gets a throttled
    // checkpoint (~1×/750ms) for crash recovery, as in the app. The final body
    // is written once on completion.
    const FLUSH_EVERY: Duration = Duration::from_millis(750);
    let mut pending = String::new();
    let mut last_flush = Instant::now();
    let on_delta = |text: String| {
        sink.delta(s.id, mid, &text);
        pending.push_str(&text);
        if last_flush.elapsed() >= FLUSH_EVERY {
            let chunk = std::mem::take(&mut pending);
            let _ = lock(svc).append_chunk(s.id, s.workspace_id, mid, chunk);
            last_flush = Instant::now();
        }
    };
    // Track D: this turn's Ligus MCP server (agent-signed proposals,
    // hand-offs, asks, reads), when the runtime can use it; else directives.
    let restricted = !envelope.egress.is_open();
    let ligus = ctx
        .ligus
        .as_ref()
        .filter(|_| allow_tools && hive_runtime::ligus_mcp::offered_for(&rt, restricted))
        .map(|mcp| {
            std::sync::Arc::new(mcp.begin_turn(TurnSpec {
                workspace_id: s.workspace_id,
                chat_id: s.id,
                agent_id: Some(agent.id),
                author: format!("@{}", agent.name),
                message_id: mid,
                worktree: eager.as_ref().map(|w| w.path.clone()),
                budgets: Budgets::from_env(),
                ttl: turn_timeout() + Duration::from_secs(300),
            }))
        });
    let cli = rt.provider.is_cli_agent();
    let socket = match ligus.as_ref().filter(|_| cli).map(|t| t.listen()) {
        Some(Ok(p)) => Some(p),
        Some(Err(e)) => {
            eprintln!("worker: Ligus MCP socket unavailable, using directives: {e}");
            None
        }
        None => None,
    };
    let ligus = ligus.filter(|_| !cli || socket.is_some());
    let system = match &ligus {
        Some(_) => hive_runtime::prompt::with_ligus_tools(&system, !cli, &[]),
        None => system,
    };
    let scope = match &ligus {
        Some(t) => hive_runtime::agent_turn::AgentTurnScope { ligus: (!cli).then(|| t.clone()), ..t.agent_scope() },
        // No Ligus tools this turn: the spend budget still holds.
        None => hive_runtime::agent_turn::AgentTurnScope {
            spend: Some(hive_runtime::agent_turn::SpendMeter::new(hive_runtime::ligus_mcp::Budgets::from_env().max_spend_usd)),
            ..Default::default()
        },
    };
    // Every child of this turn runs in this host's OS sandbox under the
    // turn's envelope and gate.
    let mut sandbox = hive_runtime::executor::TurnPolicy::new(hive_runtime::executor::HostKind::Worker, envelope.clone(), gate_plan.clone());
    sandbox.worktree_is_isolated = eager.is_some();
    sandbox.ligus_mcp_socket = socket;
    let fut = hive_runtime::executor::with_turn(sandbox, hive_runtime::agent_turn::with_agent_turn(scope, reply::generate_reply_streaming(
        &rt,
        &system,
        &turns,
        allow_tools,
        work_dir.as_deref(),
        Some(&files),
        &tool_settings,
        on_delta,
    )));
    let result = match tokio::time::timeout(turn_timeout(), fut).await {
        Ok(r) => r,
        Err(_) => Err(anyhow!("runtime timed out after {}s", turn_timeout().as_secs())),
    };

    match result {
        Ok(reply) => {
            let worktree = eager.or(reply.worktree);
            let captured = worktree.as_ref().and_then(|wt| wt.diff().ok()).filter(|d| !d.trim().is_empty());
            // Self-medication, as on the desktop: a credential in the diff or an
            // unrequested CI change locks the chat down before review.
            {
                let changed = worktree.as_ref().and_then(|wt| wt.changed_files().ok()).unwrap_or_default();
                let request = turns.iter().rev().find(|t| t.role == "user").map_or("", |t| t.content.as_str());
                let findings = hive_runtime::guard_scan::assess_turn(&[], captured.as_deref(), &changed, request);
                if !findings.is_empty() {
                    if let Err(e) = lock(svc).self_medicate(s.workspace_id, s.id, Some(mid), &findings) {
                        eprintln!("worker: lockdown failed: {e}");
                    }
                }
            }
            // A model that produced nothing (and changed no files) never posts
            // an empty reply: the chat gets a notice saying so, as on the desktop.
            let mcp_effects = ligus.as_ref().map(|t| t.effects());
            let empty = reply.text.trim().is_empty() && captured.is_none() && mcp_effects.as_ref().is_none_or(|m| m.is_empty());
            {
                let mut svc = lock(svc);
                // The same finalize as the desktop (D.4.3): directives are
                // stripped and applied here too, MCP effects win over
                // duplicates.
                let full = if empty { hive_runtime::empty_reply_notice(&format!("@{}", agent.name)) } else { reply.text };
                let fin = hive_runtime::finalize::finalize_reply(
                    &mut svc,
                    hive_runtime::finalize::FinalizeInput {
                        ctx: hive_runtime::effects::EffectCtx {
                            session_id: s.id,
                            workspace_id: s.workspace_id,
                            author: format!("@{}", agent.name),
                            agent_id: Some(agent.id),
                            message_id: Some(mid),
                            via: hive_runtime::effects::Via::Directive,
                        },
                        workspace_root: "",
                        full: &full,
                        edits_in_review: captured.is_some(),
                        mcp: mcp_effects.as_ref(),
                        base_commit: None,
                        apply_flow: true,
                    },
                );
                if let Err(e) = fin {
                    eprintln!("worker: finalize failed: {e}");
                    svc.complete_assistant_message(s.id, s.workspace_id, mid, full)?;
                }
                if let (Some(wt), Some(diff)) = (worktree.as_ref(), captured) {
                    let files = wt.changed_files().unwrap_or_default();
                    let n = files.len();
                    let title =
                        format!("@{} proposed changes to {} file{}", agent.name, n, if n == 1 { "" } else { "s" });
                    // Authorless (like the desktop's): the human reviewing can
                    // approve it — the self-approval guard excludes no one.
                    let mut prop = hive_core::ActionProposal::file_diff(title, "", diff, files);
                    prop.producing_agent_id = Some(agent.id);
                    // What it was made on (the repo itself comes from the chat).
                    if let Some((_, tip, base_ref)) = &repo_turn.base {
                        prop.base_commit = Some(tip.clone());
                        prop.base_ref = base_ref.clone();
                    }
                    prop.produced_on = Some(svc.device_id());
                    if let Some((t, b, tc)) = mcp_effects.as_ref().and_then(|m| m.diff_intent.clone()) {
                        prop.title = t;
                        prop.body = b;
                        prop.task_class = tc;
                    }
                    // Filed as the agent's own effect (A3 key, Agent tier).
                    if let Err(e) = svc.as_agent(Some(agent.id), |svc| svc.upsert_proposal(s.id, s.workspace_id, prop)) {
                        eprintln!("worker: proposal failed: {e}");
                    }
                }
            }
            finish_ligus(svc, ligus.as_deref(), &s, &format!("@{}", agent.name));
            if let Some(wt) = worktree {
                let _ = wt.remove();
            }
            sink.changed();
            if empty {
                sink.status(s.id, agent.id, TurnState::Error, Some("the model returned an empty reply"));
                println!("   @{} returned an empty reply", agent.name);
            } else {
                sink.status(s.id, agent.id, TurnState::Done, None);
                println!("   @{} done", agent.name);
            }
            Ok(())
        }
        Err(e) => {
            finish_ligus(svc, ligus.as_deref(), &s, &format!("@{}", agent.name));
            if let Some(wt) = eager {
                let _ = wt.remove();
            }
            let _ = lock(svc).complete_assistant_message(s.id, s.workspace_id, mid, format!("[error] {e}"));
            sink.changed();
            Err(e)
        }
    }
}

/// End a Ligus turn: revoke, record the tool summary, show the self-report.
fn finish_ligus(svc: &Mutex<ChatService>, turn: Option<&hive_runtime::ligus_mcp::TurnHandle>, s: &ChatSession, author: &str) {
    let Some(t) = turn else { return };
    t.finish();
    if let Some(r) = t.report() {
        let files = if r.files_touched.is_empty() { String::new() } else { format!(" Files: {}.", r.files_touched.join(", ")) };
        let _ = lock(svc).post_system_note(s.id, s.workspace_id, format!("\u{1f4cb} {author} reported {}: {}.{files}", r.status, r.summary.trim_end_matches('.')));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::{WorkspaceCredential, WorkspaceRuntime};

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
        use hive_core::WorkspaceRole;
        let role = if allow { WorkspaceRole::Owner } else { WorkspaceRole::Viewer };
        let plan = hive_runtime::author_gate::gate(rt.provider, Some(role), WorkspaceRole::Admin);
        hive_runtime::author_gate::apply(&plan, rt.provider, &mut rt.args)
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

    /// Two posts in one burst ("@coder …" then "@hive …"): the box's queue
    /// sees the @coder mention even though it isn't the chat's last message,
    /// and @coder answers it without the later @hive post as its "latest".
    #[test]
    fn a_burst_of_posts_queues_each_mention_and_answers_it_in_context() {
        use hive_core::{ChatMessage, MessageRole};
        let mut s = ChatSession::new("t", Uuid::nil(), "rt");
        let coder = agent("gw");
        let coder_id = coder.id;
        s.workspace_agents = vec![coder];
        let ask = ChatMessage::new(MessageRole::User, "Alex", "@coder run the tests");
        let other = ChatMessage::new(MessageRole::User, "Alex", "@hive what's the status?");
        let ask_id = ask.id;
        s.messages = vec![ask, other];
        let queued = hive_runtime::pending_mentions(&s);
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

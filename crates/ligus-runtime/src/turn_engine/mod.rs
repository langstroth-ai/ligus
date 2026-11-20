//! The agent-turn engine: one claimed turn, run end to end, the same on every
//! host (the desktop, `ligus worker` / `ligus serve`, `lt`). See
//! `docs/design/v2/turn-engine.md`.
//!
//! The engine runs a turn that is **already claimed**, with the context and
//! the policy the host decided ([`TurnInput`]): the author gate, the shell
//! rule, the sandbox envelope, the base [`TurnPolicy`] and the executor are
//! inputs. What differs per host (catalogs, the repo gate, worktree reuse,
//! finalize follow-ups, the UI) is a [`TurnHost`].

pub mod ligus;
pub mod mcp;
pub mod stop;
#[cfg(test)]
mod tests;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ligus_core::sandbox::{EgressPolicy, SandboxEnvelope};
use ligus_core::{ActionProposal, ChatSession, MessageRole, ModelProviderKind};
use uuid::Uuid;

use crate::author_gate::GatePlan;
use crate::executor::{TurnExecutor, TurnPolicy};
use crate::git_worktree::{LazyTurnWorktree, Worktree};
use crate::ligus_mcp::LigusMcp;
use crate::mcp::{McpServerSpec, McpTool};
use crate::provider::dispatch::{self, StreamActivity};
use crate::{ChatService, ChatTurn, ProviderError, ResolvedRuntime};

pub use mcp::InjectionHits;
pub use stop::{StopSignal, TurnStopGuard, TurnStops};

/// A stopped turn's error message.
pub const STOPPED: &str = "stopped by user";

/// Most tokens one reply may take (both hosts' value).
pub const MAX_TOKENS: u32 = 1024;

/// How often streamed text is checkpointed to the store (crash recovery).
const FLUSH_EVERY: Duration = Duration::from_millis(750);

/// Who answers.
#[derive(Debug, Clone)]
pub struct Responder {
    /// The reply message's author.
    pub author: String,
    /// The author its effects (proposals, the Ligus turn, finalize) carry.
    /// The desktop uses `author`; the worker `@name`.
    pub effect_author: String,
    pub runtime_id: String,
    pub agent_id: Option<Uuid>,
    /// The runtime as it runs: the gate's tier, the shell rule and the
    /// envelope's flags already applied to its args by the host.
    pub runtime: ResolvedRuntime,
}

/// What a turn's tools may do (the host's settings, narrowed by the
/// envelope and the shell rule).
#[derive(Debug, Clone, Default)]
pub struct ToolOptions {
    /// Offer Ligus's built-in file tools to runtimes that run the tool loop.
    pub builtin: bool,
    /// Outside a repository, the built-in tools may write in place.
    pub write_in_place: bool,
    /// Offer `run_command` (already off without the shell or under a closed
    /// envelope).
    pub run_command: bool,
    /// Turn off the exfiltration guard (MCP calls after project reads).
    pub mcp_after_reads: bool,
    /// Offer the file tools to a non-private endpoint (the operator's
    /// opt-in; the runtime's own switch also counts).
    pub remote_file_tools: bool,
}

/// Per-host switches that keep each host's behaviour as it was.
#[derive(Debug, Clone, Default)]
pub struct EngineOptions {
    /// Credit the humans in the chat as git authors of a CLI agent's commits.
    pub git_attribution: bool,
    /// A reply with no text, no diff and no tool effects becomes a notice.
    pub empty_reply_notice: bool,
    /// Scan tool results for prompt-injection markers (self-medication).
    pub scan_injections: bool,
    /// Record the turn's notes and show the self-report's confidence.
    pub detailed_report: bool,
    /// Warm the context-window probe before a tool loop.
    pub warm_context_probe: bool,
}

/// Everything the host decided about a claimed turn.
pub struct TurnInput {
    pub workspace_id: Uuid,
    pub chat_id: Uuid,
    /// The chat as of the message the turn answers.
    pub session: ChatSession,
    /// The message the turn answers (stamped on the reply).
    pub reply_to: Option<Uuid>,
    pub responder: Responder,
    pub system: String,
    pub turns: Vec<ChatTurn>,
    /// The author gate's plan for the trigger's author.
    pub gate: GatePlan,
    pub envelope: SandboxEnvelope,
    /// The shell rule's decision (the workspace Owner's, or granted).
    pub shell_allowed: bool,
    /// The base sandbox policy. The engine sets only the worktree's isolation
    /// and the Ligus MCP socket.
    pub policy: TurnPolicy,
    /// Every child the turn spawns is confined by it.
    pub executor: Arc<dyn TurnExecutor>,
    pub tools: ToolOptions,
    /// The runtime's tier edits files (a CLI agent's own tools).
    pub write_capable: bool,
    /// A writing CLI agent never runs in place: refused without a worktree.
    pub require_isolation: bool,
    /// The turn's own stop signal (the Stop button, a closing client). It is
    /// sticky: a Stop pressed before the engine waits on it still stops the
    /// turn at its next await point.
    pub stop: Option<StopSignal>,
    /// The streaming phase's wall-clock backstop.
    pub timeout: Duration,
    pub options: EngineOptions,
}

/// Where a turn runs on this host: the repo gate's answer.
#[derive(Debug, Clone, Default)]
pub struct Checkout {
    /// Added to the system prompt (which checkout and branch).
    pub context: String,
    /// The directory a turn that isn't isolated runs in.
    pub workspace_root: Option<String>,
    /// The checkout per-turn worktrees are cut from.
    pub repo_root: Option<PathBuf>,
    /// For a chat with a repo: the chat branch's tip and its base branch.
    pub base: Option<(String, String)>,
}

/// What the engine asks finalize to do.
pub struct FinalizeArgs<'a> {
    pub ctx: crate::effects::EffectCtx,
    pub full: &'a str,
    pub edits_in_review: bool,
    pub mcp: Option<&'a crate::finalize::McpEffectsLog>,
}

/// A finished turn: the committed answer.
#[derive(Debug, Clone)]
pub struct TurnOutcome {
    pub message_id: Uuid,
    pub body: String,
    /// The FileDiff proposal filed from the turn's worktree.
    pub proposal: Option<Uuid>,
    /// The model produced nothing (the notice was posted instead).
    pub empty: bool,
}

/// A turn that didn't finish.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnError {
    pub message: String,
    /// The person stopped it (not a failure).
    pub stopped: bool,
    /// The reply placeholder, when one was posted.
    pub message_id: Option<Uuid>,
}

impl std::fmt::Display for TurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TurnError {}

/// A live phase of a turn's reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Completed,
    Error,
}

/// What differs per host. Everything but [`TurnHost::service`] has a default.
pub trait TurnHost: Sync {
    /// The chat service, locked only for short store calls.
    fn service(&self) -> &Mutex<ChatService>;
    /// Called on the service each time the engine locks it (a host serving
    /// several workspaces pins it to this turn's founder).
    fn pin(&self, _svc: &mut ChatService) {}

    /// The MCP servers the turn may reach under `egress` (`shell`: whether
    /// the turn has the shell; the engine drops shell tools from what the
    /// servers list either way). Empty: no MCP tools.
    fn mcp_servers(&self, _egress: &EgressPolicy, _shell: bool) -> impl Future<Output = Vec<McpServerSpec>> + Send {
        async { Vec::new() }
    }
    /// The tools the servers listed (before shell tools are dropped).
    fn mcp_listed(&self, _tagged: &[(String, McpTool)]) {}

    /// This host's Ligus MCP server.
    fn ligus(&self) -> Option<Arc<LigusMcp>> {
        None
    }
    /// Extra prompt blocks kept when the Ligus tools replace the directives.
    fn directive_guidance(&self) -> Vec<&'static str> {
        Vec::new()
    }

    /// The repo gate: where the turn runs. Called once the reply placeholder
    /// `message_id` is posted; `Err` is the text the placeholder is completed
    /// with.
    fn checkout(&self, _session: &ChatSession, _message_id: Uuid) -> impl Future<Output = Result<Checkout, String>> + Send {
        async { Ok(Checkout::default()) }
    }
    /// Cut a writing CLI agent's worktree.
    fn cut_worktree(&self, repo_root: &Path, chat_id: &str, turn_id: &str) -> std::io::Result<Worktree> {
        if self.reuse_worktrees() {
            crate::git_worktree::cut_turn_worktree_reusing(repo_root, chat_id, turn_id)
        } else {
            crate::git_worktree::cut_turn_worktree(repo_root, chat_id, turn_id)
        }
    }
    fn reuse_worktrees(&self) -> bool {
        false
    }
    /// Done with a turn's worktree (its diff is captured).
    fn release_worktree(&self, wt: Worktree, chat_id: Uuid) {
        let result = if self.reuse_worktrees() { wt.park(&chat_id.to_string()) } else { wt.remove() };
        if let Err(e) = result {
            tracing::warn!(target: "worktree", "couldn't release a turn worktree: {e}");
        }
    }
    /// The placeholder text when a writing agent can't be isolated.
    fn isolation_refusal(&self, _author: &str) -> String {
        "This chat's sandbox requires an isolated git worktree for agents that edit files, and one couldn't be made here (no git repository bound, or the worktree failed).".into()
    }

    /// `propose_workflow`, validated in-turn against this chat.
    fn workflow_tool(&self, _session: &ChatSession) -> Option<crate::workflow_tool::WorkflowTool> {
        None
    }

    /// Finalize the reply (directives, effects). The default is the shared
    /// finalize with flow directives applied here; on its failure the reply
    /// is completed as written.
    fn finalize(&self, svc: &mut ChatService, a: FinalizeArgs<'_>) -> Result<String, String> {
        let (sid, wid, mid) = (a.ctx.session_id, a.ctx.workspace_id, a.ctx.message_id);
        let input = crate::finalize::FinalizeInput {
            ctx: a.ctx,
            workspace_root: "",
            full: a.full,
            edits_in_review: a.edits_in_review,
            mcp: a.mcp,
            base_commit: None,
            apply_flow: true,
        };
        match crate::finalize::finalize_reply(svc, input) {
            Ok(f) => Ok(f.cleaned),
            Err(e) => {
                tracing::warn!(target: "dispatch", "finalize failed: {e}");
                if let Some(mid) = mid {
                    svc.complete_assistant_message(sid, wid, mid, a.full.to_string()).map_err(|e| e.to_string())?;
                }
                Ok(a.full.to_string())
            }
        }
    }
    /// Adjust a turn's FileDiff proposal before it is filed.
    fn before_file_proposal(&self, _proposal: &mut ActionProposal) {}
    /// A proposal was filed from the turn's worktree.
    fn proposal_filed(&self, _svc: &mut ChatService, _proposal: Uuid) {}

    // ── The event sink ────────────────────────────────────────────────
    /// A streamed reply fragment.
    fn delta(&self, _message_id: Uuid, _text: &str) {}
    /// Tool calls and results, thinking, plan windows.
    fn activity(&self, _message_id: Uuid, _activity: &StreamActivity) {}
    /// The reply completed or failed (`text`: the body, or the error).
    fn phase(&self, _message_id: Uuid, _phase: Phase, _text: &str) {}
    /// The reply placeholder was posted.
    fn begun(&self, _message_id: Uuid) {}
    /// The store changed (a reply began or completed, a proposal was filed).
    fn changed(&self) {}
    /// Self-medication locked the chat down.
    fn locked_down(&self) {
        self.changed();
    }

    // ── Content-free telemetry ───────────────────────────────────────
    fn tools_selected(&self, _per_server: &[(String, u32, u32)]) {}
    fn progress(&self, _progress: &crate::patch_leaving::ProgressTracker) {}
    fn egress_denials(&self, _denied: &[crate::egress::HostPort]) {}
    fn usage_error(&self, _provider: ModelProviderKind, _message: &str) {}
    /// An agent turn passed agent access and is about to run (`agent`: the
    /// responder's name). A host may tell the owner when someone else uses
    /// the agent (`use_.notify`).
    fn agent_used(&self, _agent: &str, _use: &crate::agent_access::AgentUse) {}

    /// The text a provider error is shown as.
    fn error_text(&self, error: &str) -> String {
        error.to_string()
    }
    /// The text the turn's timeout is shown as.
    fn timeout_text(&self, secs: u64) -> String {
        format!(
            "runtime timed out after {secs}s with no completion — the agent may be waiting on login/auth, input, or a blocked tool. Verify it with Settings \u{2192} Models \u{2192} Test, or raise LIGUS_TURN_TIMEOUT_SECS."
        )
    }
}

/// The host's service, locked and pinned for this turn.
fn locked<H: TurnHost>(host: &H) -> MutexGuard<'_, ChatService> {
    let mut g = host.service().lock().unwrap_or_else(|e| e.into_inner());
    host.pin(&mut g);
    g
}

/// `base` and `extra` as one system prompt.
pub fn join_system(base: &str, extra: &str) -> String {
    if extra.is_empty() {
        base.to_string()
    } else {
        format!("{base}\n\n{extra}")
    }
}

/// The endpoint a runtime actually talks to (native Ollama defaults local).
pub fn effective_endpoint(rt: &ResolvedRuntime) -> String {
    if rt.endpoint.trim().is_empty() && rt.provider == ModelProviderKind::Ollama {
        "http://localhost:11434".into()
    } else {
        rt.endpoint.clone()
    }
}

/// Whether an HTTP model turn gets the built-in file tools: they're on, the
/// runtime runs the tool loop, and the endpoint is on this machine or a
/// private network unless someone opted into sending files to it.
pub fn wants_builtin_tools(rt: &ResolvedRuntime, tools: &ToolOptions) -> bool {
    tools.builtin
        && rt.runs_agent_tools()
        && (tools.remote_file_tools || rt.allow_remote_file_tools || ligus_core::endpoint::endpoint_is_private(&effective_endpoint(rt)))
}

/// The turn's wall-clock backstop: `LIGUS_TURN_TIMEOUT_SECS`, default 1 h.
pub fn turn_timeout() -> Duration {
    Duration::from_secs(
        ligus_core::names::var("LIGUS_TURN_TIMEOUT_SECS").ok().and_then(|v| v.parse().ok()).filter(|&s| s > 0).unwrap_or(3600),
    )
}

/// Run one claimed turn end to end.
/// Why a turn stopped right after its placeholder: another device's answer
/// to the same turn committed first.
pub const TURN_FENCED: &str = "another device answered this turn first";

/// Whether this device's placeholder `mid` was fenced by the dispatcher.
fn placeholder_fenced(svc: &ChatService, chat_id: Uuid, mid: Uuid) -> bool {
    svc.load(chat_id).ok().flatten().is_some_and(|s| s.messages.iter().any(|m| m.id == mid && m.superseded))
}

pub async fn run<H: TurnHost>(host: &H, input: TurnInput) -> Result<TurnOutcome, TurnError> {
    let TurnInput {
        workspace_id,
        chat_id,
        session,
        reply_to,
        responder,
        system,
        turns,
        gate,
        envelope,
        shell_allowed,
        mut policy,
        executor,
        tools,
        write_capable,
        require_isolation,
        stop,
        timeout,
        options,
    } = input;
    let stop = stop.unwrap_or_default();
    // Stopped before it began: nothing to undo.
    if stop.is_stopped() {
        return Err(TurnError { message: STOPPED.into(), stopped: true, message_id: None });
    }
    // Agent access controls (`agent_access`): who may use this agent and its
    // daily budgets, decided here on every host before anything is spent.
    // A host checks before it claims too; this is the backstop every path
    // (a cascade, a hand-off, a workflow stage, a fallback) goes through.
    // Refused: the trigger is answered with a short note no model wrote.
    let mut responder = responder;
    let gated = match (responder.agent_id, reply_to) {
        // Bound first: the service isn't held while the host is told.
        (Some(agent), Some(trigger)) => Some((agent, locked(host).gate_agent_turn(chat_id, workspace_id, trigger, agent))),
        _ => None,
    };
    let agent_use = match gated {
        Some((agent, gated)) => match gated {
            Ok(Ok(u)) => Some((agent, u)),
            Ok(Err(note)) => {
                host.changed();
                return Err(TurnError { message: note, stopped: false, message_id: None });
            }
            Err(e) => {
                return Err(TurnError { message: format!("couldn't check who may use this agent: {e}"), stopped: false, message_id: None });
            }
        },
        None => None,
    };
    let usage_day = match &agent_use {
        Some((agent, u)) => {
            // A gateway's attribution names the member the turn runs for.
            if let Some(g) = responder.runtime.gateway.as_mut() {
                g.workspace_id = g.workspace_id.or(Some(workspace_id));
                g.agent_id = g.agent_id.or(Some(*agent));
                g.member_id = Some(u.requester.clone()).filter(|m| !m.is_empty());
            }
            host.agent_used(&responder.effect_author, u);
            locked(host).record_agent_turn(workspace_id, *agent, &u.requester).ok()
        }
        None => None,
    };
    let rt = &responder.runtime;
    let tools_allowed = gate.allows_tools();
    let restricted = !envelope.egress.is_open();
    let cli = rt.provider.is_cli_agent();
    let anthropic_loop = tools_allowed && rt.provider == ModelProviderKind::Anthropic && rt.api_key.is_some();

    // MCP tools: the Anthropic loop, and HTTP runtimes that run the tool
    // loop. Injection markers in their results lock the chat down after.
    let hits = InjectionHits::default();
    let mcp_tools = if tools_allowed && (anthropic_loop || rt.runs_agent_tools()) {
        let servers = host.mcp_servers(&envelope.egress, shell_allowed).await;
        // Listing starts stdio servers: under the turn's policy and executor,
        // like every other child of the turn.
        let listing = mcp::tool_set(servers, shell_allowed, options.scan_injections.then(|| hits.clone()), |t| host.mcp_listed(t));
        let listing = crate::executor::with_executor(executor.clone(), crate::executor::with_turn(policy.clone(), listing));
        tokio::select! {
            biased;
            _ = stop.stopped() => return Err(TurnError { message: STOPPED.into(), stopped: true, message_id: None }),
            l = listing => l,
        }
    } else {
        None
    };
    // An Anthropic runtime with neither MCP tools nor the Ligus tools streams
    // plain chat.
    let anthropic_loop = anthropic_loop && (mcp_tools.is_some() || (host.ligus().is_some() && crate::ligus_mcp::offered_for(rt, restricted)));
    let builtin_wanted = tools_allowed && wants_builtin_tools(rt, &tools) && crate::executor::local_tools_allowed_on(executor.kind());

    // Git commit attribution: credit the humans who drove this turn, so a
    // CLI agent's commits on this host aren't credited to its owner.
    let humans: Vec<ligus_core::ActorIdentity> = if options.git_attribution && cli {
        session.messages.iter().filter(|m| matches!(m.role, MessageRole::User)).filter_map(|m| m.actor_identity.clone()).collect()
    } else {
        Vec::new()
    };
    let git_env = humans.last().map(crate::git_attribution::author_env).unwrap_or_default();
    let system = match humans.last() {
        Some(req) => {
            let note = crate::git_attribution::commit_attribution_note(req, &humans);
            if note.is_empty() { system } else { format!("{system}\n\n{note}") }
        }
        None => system,
    };

    let mid = locked(host)
        .begin_assistant_message(chat_id, workspace_id, responder.author.clone(), responder.runtime_id.clone(), responder.agent_id, reply_to)
        .map_err(|e| TurnError { message: e.to_string(), stopped: false, message_id: None })?;
    stop.bind_message(mid);
    host.begun(mid);
    host.changed();
    let fail = |message: String| TurnError { message, stopped: false, message_id: Some(mid) };
    // Stopped before the stream: the placeholder is completed empty.
    let stopped_early = || {
        tracing::info!(target: "dispatch", "turn stopped by user before it streamed");
        let _ = locked(host).complete_assistant_message(chat_id, workspace_id, mid, String::new());
        host.changed();
        host.phase(mid, Phase::Completed, "");
        TurnError { message: STOPPED.into(), stopped: true, message_id: Some(mid) }
    };
    // The placeholder is the dispatcher's commit (design repos-and-checkouts
    // 11.5): fenced means another device's answer to this turn committed
    // first, so this run stops here.
    if placeholder_fenced(&locked(host), chat_id, mid) {
        let _ = locked(host).complete_assistant_message(chat_id, workspace_id, mid, String::new());
        host.changed();
        return Err(fail(TURN_FENCED.into()));
    }

    if stop.is_stopped() {
        return Err(stopped_early());
    }
    // The repo gate: a chat with a repo runs only in this host's checkout.
    let gated = tokio::select! {
        biased;
        _ = stop.stopped() => return Err(stopped_early()),
        c = host.checkout(&session, mid) => c,
    };
    if stop.is_stopped() {
        return Err(stopped_early());
    }
    let checkout = match gated {
        Ok(c) => c,
        Err(note) => {
            let _ = locked(host).complete_assistant_message(chat_id, workspace_id, mid, note.clone());
            host.changed();
            host.phase(mid, Phase::Error, &note);
            return Err(fail(note));
        }
    };
    let system = join_system(&system, &checkout.context);
    let workspace_root = checkout.workspace_root.clone().unwrap_or_default();
    let repo_root = checkout.repo_root.clone().filter(|r| !r.as_os_str().is_empty() && crate::git_worktree::is_git_repo(r));
    let turn_key = mid.to_string();

    // Per-turn isolation: a writing CLI agent runs in a worktree cut up
    // front from the chat's branch; an HTTP turn with the built-in tools cuts
    // one on its first write. Either diff comes back as a proposal.
    let worktree = match &repo_root {
        Some(root) if cli && write_capable && !anthropic_loop => match host.cut_worktree(root, &chat_id.to_string(), &turn_key) {
            Ok(wt) => Some(wt),
            Err(e) => {
                tracing::warn!(target: "worktree", "isolation unavailable, running in place: {e}");
                None
            }
        },
        _ => None,
    };
    let lazy = repo_root.as_ref().filter(|_| builtin_wanted && !anthropic_loop).map(|root| {
        Arc::new(LazyTurnWorktree::new(root.clone(), chat_id.to_string(), turn_key.clone()).with_reuse(host.reuse_worktrees()))
    });
    // Until it writes, a lazily isolated turn reads its own chat's base
    // worktree: the tree its worktree will be cut from.
    let builtin_read_root = match (&lazy, &repo_root) {
        (Some(_), Some(root)) => match crate::git_worktree::chat_worktree(root, &chat_id.to_string()) {
            Ok(dir) => dir.to_string_lossy().into_owned(),
            Err(e) => {
                tracing::warn!(target: "worktree", "chat base worktree unavailable, reading the checkout: {e}");
                root.to_string_lossy().into_owned()
            }
        },
        _ => String::new(),
    };
    if worktree.is_none() && lazy.is_none() && !cli && mcp_tools.is_some() && crate::git_worktree::is_git_repo(Path::new(&workspace_root)) {
        tracing::warn!(target: "dispatch", provider = ?rt.provider, "API turn has MCP tools enabled and runs in place: its file writes are not worktree-isolated or diff-gated");
    }
    if require_isolation && cli && write_capable && worktree.is_none() {
        let note = host.isolation_refusal(&responder.effect_author);
        let _ = locked(host).complete_assistant_message(chat_id, workspace_id, mid, note.clone());
        host.changed();
        host.phase(mid, Phase::Error, &note);
        return Err(fail(note));
    }
    let effective_root = worktree.as_ref().map(|w| w.path.to_string_lossy().into_owned()).unwrap_or_else(|| workspace_root.clone());
    // Never an empty working dir for a CLI (the spawn would fail): `None`
    // runs it in its turn's scratch dir.
    let working_dir = (!effective_root.trim().is_empty()).then_some(effective_root.as_str());

    // The Ligus MCP server for this turn (agent-signed proposals, hand-offs,
    // asks, reads), when the runtime can use it; else the reply directives.
    let ligus_server = host.ligus();
    let iface = ligus::begin(
        ligus_server.as_ref(),
        ligus::Begin {
            runtime: rt,
            restricted,
            tools_allowed,
            workspace_id,
            chat_id,
            agent_id: responder.agent_id,
            author: &responder.effect_author,
            message_id: mid,
            worktree: worktree.as_ref().map(|w| w.path.clone()),
            timeout,
        },
    );
    let system = iface.system(system, &host.directive_guidance());
    let folded = iface.folded();

    // Built-in file tools, confined to the turn's dir: in a repo their edits
    // land in the lazily cut worktree; outside one, in place only if allowed.
    let builtin_root = if lazy.is_some() { builtin_read_root.as_str() } else { effective_root.as_str() };
    let builtin = if builtin_wanted && !anthropic_loop && !builtin_root.trim().is_empty() {
        let p = crate::agent_tools::builtin_policy(builtin_root, lazy.is_some(), tools.write_in_place, tools.run_command && shell_allowed)
            .with_protected_dirs(&envelope.protected_dirs);
        let made = match &lazy {
            Some(l) => crate::BuiltinTools::lazy(p, l.clone(), tools.write_in_place),
            None => crate::BuiltinTools::new(p),
        };
        match made {
            Ok(b) => Some(b),
            Err(e) => {
                tracing::warn!(target: "dispatch", dir = %builtin_root, "built-in tools unavailable: {e}");
                None
            }
        }
    } else {
        None
    };
    let last_user = turns.iter().rev().find(|t| t.role == "user").map_or("", |t| t.content.as_str()).to_string();

    let mut pending = String::new();
    let mut last_flush = Instant::now();
    let mut on_delta = |text: String| {
        host.delta(mid, &text);
        pending.push_str(&text);
        if last_flush.elapsed() >= FLUSH_EVERY {
            let chunk = std::mem::take(&mut pending);
            let _ = locked(host).append_chunk(chat_id, workspace_id, mid, chunk);
            last_flush = Instant::now();
        }
    };
    let on_activity = |act: StreamActivity| {
        if options.scan_injections {
            if let StreamActivity::ToolResult { content, .. } = &act {
                hits.scan(content);
            }
        }
        host.activity(mid, &act);
    };

    // The tools an in-process loop offers: built-in, MCP, the folded Ligus
    // tools and, when asked for, `propose_workflow`.
    let (http_tools, anthropic_mcp) = if anthropic_loop {
        (None, mcp_tools)
    } else {
        let workflow_tool = if (builtin.is_some() || mcp_tools.is_some()) && folded.is_none() && crate::workflow_tool::wanted(&last_user) {
            // Loaded first: the hook never runs under the service lock.
            let session = locked(host).load(chat_id).ok().flatten();
            session.and_then(|s| host.workflow_tool(&s))
        } else {
            None
        };
        let tools_set = (builtin.is_some() || mcp_tools.is_some() || folded.is_some()).then(|| {
            let ligus_slots = if folded.is_some() { crate::ligus_mcp::tools::FOLDED.len() } else { 0 };
            let t = crate::agent_tools::CompositeTools::with_hints(
                builtin,
                mcp_tools,
                crate::agent_tools::MAX_OFFERED_TOOLS - usize::from(workflow_tool.is_some()) - ligus_slots,
                &crate::agent_tools::tool_hints(&system, &turns),
            )
            .allow_mcp_after_reads(tools.mcp_after_reads);
            let t = match workflow_tool {
                Some(w) => t.with_workflow_tool(w),
                None => t,
            };
            match folded.clone() {
                Some(turn) => t.with_ligus_tools(turn),
                None => t,
            }
        });
        (tools_set, None)
    };
    if let Some(t) = &http_tools {
        host.tools_selected(&t.mcp_selection());
    }
    let rounds = crate::agent_tools::tool_round_budget(rt.provider, rt.max_tool_rounds);
    if options.warm_context_probe && http_tools.as_ref().is_some_and(|t| !t.is_empty()) {
        // A Stop here falls through to the stream's select below.
        tokio::select! {
            biased;
            _ = stop.stopped() => {}
            _ = dispatch::warm_context_probe(rt) => {}
        }
    }
    tracing::info!(
        target: "dispatch",
        provider = ?rt.provider,
        model = %rt.model,
        endpoint = %rt.endpoint,
        dir = %effective_root,
        isolated = worktree.is_some(),
        isolates_on_write = lazy.is_some(),
        tools = http_tools.as_ref().map_or(0, |t| t.defs().len()),
        "dispatching turn"
    );

    // Every child this turn spawns runs under the policy, confined by the
    // executor.
    policy.worktree_is_isolated = worktree.is_some() || lazy.is_some();
    policy.ligus_mcp_socket = iface.socket.clone();
    let denials = policy.denials.clone();
    let scope = iface.scope();
    let turn_notes = scope.notes.clone();
    let meter = scope.spend.clone();
    let stream = async {
        if anthropic_loop {
            let Some(api_key) = rt.api_key.clone() else { return Err(ProviderError::Subprocess("no API key".into())) };
            let (mut defs, exec) = match anthropic_mcp {
                Some((d, e)) => (d, Some(e)),
                None => (Vec::new(), None),
            };
            if iface.offered() {
                let mut l = crate::ligus_mcp::tools::tool_loop_defs();
                l.extend(defs);
                defs = l;
            }
            let model = mcp::AnthropicMessages {
                client: crate::AnthropicClient::new(),
                api_key,
                model: rt.model.clone(),
                system: Some(system.clone()),
                max_tokens: MAX_TOKENS,
            };
            let routed = crate::ligus_mcp::WithLigus { turn: iface.turn.as_deref(), inner: exec.as_ref() };
            let text = crate::tool_loop::run_with_messages(&model, &routed, mcp::turns_to_messages(&turns), defs, crate::agent_tools::ANTHROPIC_TOOL_ROUNDS).await?;
            on_delta(text.clone());
            Ok(text)
        } else {
            let set = http_tools.as_ref().filter(|t| !t.is_empty()).map(|t| dispatch::ToolSet { defs: t.defs(), executor: t, max_rounds: rounds });
            dispatch::stream_with_tools(rt, Some(&system), &turns, working_dir, &git_env, MAX_TOKENS, set, &mut on_delta, on_activity).await
        }
    };
    let fut = crate::executor::with_executor(executor, crate::executor::with_turn(policy, crate::agent_turn::with_agent_turn(scope, stream)));
    let mut stopped = false;
    let result = tokio::select! {
        biased;
        _ = stop.stopped() => {
            stopped = true;
            Err(ProviderError::Subprocess(STOPPED.into()))
        }
        r = tokio::time::timeout(timeout, fut) => match r {
            Ok(r) => r,
            Err(_) => Err(ProviderError::Subprocess(host.timeout_text(timeout.as_secs()))),
        },
    };
    host.egress_denials(&denials.lock().map(|d| d.clone()).unwrap_or_default());
    // The turn's estimated cost, counted against the agent's daily cap
    // (`None`: the runtime reported no usage; counted in turns only).
    if let (Some((agent, u)), Some(day)) = (&agent_use, &usage_day) {
        let usd = meter.as_ref().map(|m| m.total()).filter(|t| *t > 0.0);
        if let Err(e) = locked(host).record_agent_cost(workspace_id, *agent, &u.requester, day, usd) {
            tracing::warn!(target: "dispatch", "couldn't count the turn's cost: {e}");
        }
    }
    if let Some(t) = &http_tools {
        host.progress(t.progress());
    }
    // The worktree a lazily isolating turn cut on its first write, if any.
    let worktree = worktree.or_else(|| lazy.as_ref().and_then(|l| l.finish()));

    match result {
        Ok(full) => {
            tracing::info!(target: "dispatch", chars = full.len(), "turn completed");
            let accepted = http_tools.as_ref().map(|t| t.accepted_workflows()).unwrap_or_default();
            let full = crate::workflow_tool::append_as_directives(&full, &accepted);
            // The worktree's changes, read before the reply is finalized so
            // its directives know the edits are already in review.
            let captured = worktree.as_ref().and_then(|wt| wt.diff().ok()).filter(|d| !d.trim().is_empty());
            let changed = worktree.as_ref().and_then(|wt| wt.changed_files().ok()).unwrap_or_default();
            // Self-medication: an injection marker in a tool result, a
            // credential in the diff, or an unrequested CI change locks the
            // chat down before its proposal reaches anyone.
            self_medicate(host, workspace_id, chat_id, mid, &hits.take(), captured.as_deref(), &changed, &last_user);
            let effects = iface.effects();
            let empty = options.empty_reply_notice
                && full.trim().is_empty()
                && captured.is_none()
                && effects.as_ref().is_none_or(|m| m.is_empty());
            let full = if empty { crate::empty_reply_notice(&responder.effect_author) } else { full };
            let body = {
                let mut s = locked(host);
                host.finalize(
                    &mut s,
                    FinalizeArgs {
                        ctx: crate::effects::EffectCtx {
                            session_id: chat_id,
                            workspace_id,
                            author: responder.effect_author.clone(),
                            agent_id: responder.agent_id,
                            message_id: Some(mid),
                            via: crate::effects::Via::Directive,
                        },
                        full: &full,
                        edits_in_review: captured.is_some(),
                        mcp: effects.as_ref(),
                    },
                )
            };
            // The worktree's changes as a review-gated proposal; then the
            // worktree goes (the stored diff is the portable artifact).
            let mut proposal = None;
            if let Some(wt) = worktree {
                if let (true, Some(diff)) = (body.is_ok(), captured) {
                    let n = changed.len();
                    let title = format!("{} proposed changes to {} file{}", responder.effect_author, n, if n == 1 { "" } else { "s" });
                    // Authorless: the human reviewing can approve it.
                    let mut prop = ActionProposal::file_diff(title, "", diff, changed.clone());
                    prop.producing_agent_id = responder.agent_id;
                    if let Some((tip, base_ref)) = &checkout.base {
                        prop.base_commit = Some(tip.clone());
                        prop.base_ref = base_ref.clone();
                    }
                    prop.produced_on = Some(locked(host).device_id());
                    // `ligus_propose_diff` named and described it.
                    if let Some((t, b, tc)) = effects.as_ref().and_then(|m| m.diff_intent.clone()) {
                        prop.title = t;
                        prop.body = b;
                        prop.task_class = tc;
                    }
                    host.before_file_proposal(&mut prop);
                    let id = prop.id;
                    let mut s = locked(host);
                    // Filed as the agent's own effect (A3 key, Agent tier).
                    match s.as_agent(responder.agent_id, |s| s.upsert_proposal(chat_id, workspace_id, prop)) {
                        Ok(()) => {
                            host.proposal_filed(&mut s, id);
                            proposal = Some(id);
                        }
                        Err(e) => tracing::warn!(target: "dispatch", "the turn's proposal wasn't filed: {e}"),
                    }
                    drop(s);
                    host.changed();
                }
                host.release_worktree(wt, chat_id);
            }
            let notes = turn_notes.lock().map(|n| n.clone()).unwrap_or_default();
            iface.finish(|| locked(host), chat_id, workspace_id, &responder.effect_author, &notes, options.detailed_report);
            host.changed();
            let body = match body {
                Ok(b) => b,
                Err(e) => {
                    // Finalize failed: the reply never stays a live placeholder.
                    // It keeps what the model wrote unless finalize completed it.
                    let mut s = locked(host);
                    let streaming = s.load(chat_id).ok().flatten().is_some_and(|c| c.messages.iter().any(|m| m.id == mid && m.is_streaming));
                    if streaming {
                        let _ = s.complete_assistant_message(chat_id, workspace_id, mid, full.clone());
                    }
                    drop(s);
                    host.changed();
                    host.phase(mid, Phase::Error, &e);
                    return Err(fail(e));
                }
            };
            host.phase(mid, Phase::Completed, &body);
            Ok(TurnOutcome { message_id: mid, body, proposal, empty })
        }
        Err(e) => {
            iface.finish(|| locked(host), chat_id, workspace_id, &responder.effect_author, &[], options.detailed_report);
            self_medicate(host, workspace_id, chat_id, mid, &hits.take(), None, &[], &last_user);
            // A failed or stopped turn's partial edits are discarded.
            if let Some(wt) = worktree {
                host.release_worktree(wt, chat_id);
            }
            let msg = e.to_string();
            if stopped {
                tracing::info!(target: "dispatch", "turn stopped by user");
                let _ = locked(host).complete_assistant_message(chat_id, workspace_id, mid, String::new());
                host.changed();
                host.phase(mid, Phase::Completed, "");
                return Err(TurnError { message: STOPPED.into(), stopped: true, message_id: Some(mid) });
            }
            tracing::error!(target: "dispatch", error = %msg, "turn failed");
            host.usage_error(rt.provider, &msg);
            let shown = host.error_text(&msg);
            let _ = locked(host).complete_assistant_message(chat_id, workspace_id, mid, format!("[error] {shown}"));
            host.changed();
            host.phase(mid, Phase::Error, &msg);
            Err(fail(shown))
        }
    }
}

/// Lock the chat down for every infection signal the turn showed.
#[allow(clippy::too_many_arguments)]
fn self_medicate<H: TurnHost>(
    host: &H,
    workspace_id: Uuid,
    chat_id: Uuid,
    message_id: Uuid,
    markers: &[&'static str],
    diff: Option<&str>,
    changed: &[String],
    request: &str,
) {
    let findings = crate::guard_scan::assess_turn(markers, diff, changed, request);
    if findings.is_empty() {
        return;
    }
    match locked(host).self_medicate(workspace_id, chat_id, Some(message_id), &findings) {
        Ok(n) => tracing::warn!(target: "guards", chat = %chat_id, signals = n, "chat locked down by self-medication"),
        Err(e) => tracing::error!(target: "guards", "couldn't lock chat {chat_id} down: {e}"),
    }
    host.locked_down();
}

// ---------------------------------------------------------------------------
// The gate, as every host decides it
// ---------------------------------------------------------------------------

/// What a host decided about a turn's policy.
#[derive(Debug, Clone)]
pub struct Gated {
    /// The runtime with the gate's tier, the shell rule and the envelope's
    /// flags applied.
    pub runtime: ResolvedRuntime,
    pub gate: GatePlan,
    pub envelope: SandboxEnvelope,
    pub shell_allowed: bool,
    /// The envelope allows `run_command` at all.
    pub allow_run_command: bool,
    /// The signed path grants for the turn's agent.
    pub grants: ligus_core::sandbox::PathGrants,
}

/// The gate as the desktop and the worker decide it, for a host that has no
/// rules of its own (`lt`): the author gate for the trigger's author (the
/// role they held when they posted; unresolved fails closed), the shell rule
/// (the workspace Owner's, or granted to the agent; grants apply here per
/// `grants_setting`), the chat's sandbox envelope, and this host's OS
/// sandbox. `Err` is why the turn doesn't run.
pub fn decide_gate(
    svc: &ChatService,
    session: &ChatSession,
    trigger: Uuid,
    agent_id: Option<Uuid>,
    rt: ResolvedRuntime,
    grants_setting: Option<bool>,
) -> Result<Gated, String> {
    let author_role = svc.trigger_author_role(session.id, trigger).ok().flatten();
    let shell_role = svc.trigger_shell_role(session.id, trigger).ok().flatten();
    let grants = svc.sandbox_grants_here(session.workspace_id, grants_setting).0;
    let granted = agent_id.is_some_and(|id| grants.shell_granted(id));
    decide_gate_with(session, rt, author_role, shell_role, granted, grants.paths_for(agent_id))
}

/// [`decide_gate`] with the roles already resolved: `author_role` for the
/// author gate, `shell_role` and `shell_granted` for the shell rule.
pub fn decide_gate_with(
    session: &ChatSession,
    mut rt: ResolvedRuntime,
    author_role: Option<ligus_core::WorkspaceRole>,
    shell_role: Option<ligus_core::WorkspaceRole>,
    shell_granted: bool,
    grants: ligus_core::sandbox::PathGrants,
) -> Result<Gated, String> {
    let shell_allowed = crate::author_gate::shell_allowed(shell_role, shell_granted);
    let gate = crate::author_gate::gate_turn(rt.provider, &mut rt.args, author_role);
    let envelope = ligus_core::sandbox::envelope_for_session(session);
    let enforcement = crate::sandbox::enforce(rt.provider, &envelope, &Vec::new());
    if let Some(reason) = enforcement.refuse {
        return Err(reason);
    }
    if let GatePlan::Refuse(reason) = &gate {
        return Err(reason.clone());
    }
    crate::executor::turn_allowed(rt.provider)?;
    crate::author_gate::gate_shell(rt.provider, &mut rt.args, shell_allowed)?;
    rt.args.extend(enforcement.extra_args);
    Ok(Gated { runtime: rt, gate, envelope, shell_allowed, allow_run_command: enforcement.allow_run_command, grants })
}

impl Gated {
    /// The base sandbox policy for this decision on a `host` (the engine adds
    /// the worktree's isolation and the Ligus MCP socket).
    pub fn policy(&self, host: crate::executor::HostKind) -> TurnPolicy {
        let mut p = TurnPolicy::new(host, self.envelope.clone(), self.gate.clone());
        p.shell_allowed = self.shell_allowed;
        p.grants = self.grants.clone();
        p
    }
}

// ---------------------------------------------------------------------------
// The repo gate, for a host without one of its own
// ---------------------------------------------------------------------------

/// The host-side repo gate (repos-and-checkouts 5.3) for a host with no rules
/// of its own (`lt`): a chat without a repo runs in `legacy` (this device's
/// bound checkout, if any); a chat with one runs only in this device's
/// confirmed checkout of it, on its chat branch brought up to date with the
/// chat's ledger (a base this device just cut is pinned). `Err` is the
/// refusal (still cloning, no checkout here, a moved remote).
pub fn repo_checkout(svc: &Mutex<ChatService>, session: &ChatSession, host_label: &str, legacy: Option<PathBuf>) -> Result<Checkout, String> {
    use crate::repos::{self as rr, TurnCheckout};
    let gate = {
        let svc = svc.lock().unwrap_or_else(|e| e.into_inner());
        let roster = svc.workspace_roster(session.workspace_id).map_err(|e| e.to_string())?;
        rr::turn_checkout(svc.store(), svc.device_id(), host_label, session, &roster, &rr::cloning())
    };
    let ready = match gate {
        TurnCheckout::NoRepo => {
            let root = legacy.filter(|p| crate::git_worktree::is_git_repo(p));
            return Ok(Checkout { workspace_root: None, repo_root: root, ..Default::default() });
        }
        TurnCheckout::Ready(r) => r,
        TurnCheckout::Cloning { repo_name } => return Err(format!("{repo_name} is still cloning on {host_label}; ask again when it finishes")),
        TurnCheckout::Refused { reason, .. } => return Err(reason),
    };
    let prepared = rr::prepare_chat(&ready, session, host_label)?;
    if let Some(pin) = &prepared.pin {
        let r = svc.lock().unwrap_or_else(|e| e.into_inner()).pin_chat_base(session.id, session.workspace_id, ready.repo.id, pin);
        if let Err(e) = r {
            tracing::warn!(target: "repos", "couldn't pin the chat's base: {e}");
        }
    }
    let base_branch = session.repo.as_ref().map(|r| r.base_branch.clone()).unwrap_or_default();
    let context = rr::turn_context(&TurnCheckout::Ready(ready.clone()), session.id, &base_branch);
    Ok(Checkout {
        context,
        workspace_root: Some(prepared.chat_worktree.to_string_lossy().into_owned()),
        repo_root: Some(ready.path),
        base: Some((prepared.tip, base_branch)),
    })
}

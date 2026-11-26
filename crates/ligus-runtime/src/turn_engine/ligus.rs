//! One turn's Ligus interface (Track D): the Ligus MCP server's per-turn
//! handle, the socket a CLI agent reaches, or the folded tools an in-process
//! tool loop offers. The desktop's `agent_iface` and the worker had a copy.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use crate::ligus_mcp::{Budgets, LigusMcp, TurnHandle, TurnSpec};
use crate::{ChatService, ResolvedRuntime};

/// One turn's Ligus interface.
pub struct TurnIface {
    pub turn: Option<Arc<TurnHandle>>,
    /// The socket a CLI agent reaches (allowed through the sandbox).
    pub socket: Option<PathBuf>,
    /// The turn runs an in-process tool loop (folded tools).
    pub tool_loop: bool,
}

/// What [`begin`] needs about the turn.
pub struct Begin<'a> {
    pub runtime: &'a ResolvedRuntime,
    /// The envelope closes egress.
    pub restricted: bool,
    /// The author gate allows tools.
    pub tools_allowed: bool,
    pub workspace_id: Uuid,
    pub chat_id: Uuid,
    pub agent_id: Option<Uuid>,
    pub author: &'a str,
    pub message_id: Uuid,
    pub worktree: Option<PathBuf>,
    /// The turn's hard timeout (the credential lives 5 minutes longer).
    pub timeout: Duration,
}

impl TurnIface {
    pub fn none() -> Self {
        Self {
            turn: None,
            socket: None,
            tool_loop: false,
        }
    }
}

/// Start the Ligus server's turn when the runtime can use it and the author
/// gate allows tools. Otherwise the turn keeps the reply directives.
pub fn begin(server: Option<&Arc<LigusMcp>>, b: Begin<'_>) -> TurnIface {
    let Some(server) = server else {
        return TurnIface::none();
    };
    if !b.tools_allowed || !crate::ligus_mcp::offered_for(b.runtime, b.restricted) {
        return TurnIface::none();
    }
    let turn = server.begin_turn(TurnSpec {
        workspace_id: b.workspace_id,
        chat_id: b.chat_id,
        agent_id: b.agent_id,
        author: b.author.to_string(),
        message_id: b.message_id,
        worktree: b.worktree,
        budgets: Budgets::from_env(),
        ttl: b.timeout + Duration::from_secs(300),
    });
    let cli = b.runtime.provider.is_cli_agent();
    let socket = if cli {
        match turn.listen() {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!(target: "ligus_mcp", "Ligus MCP socket unavailable, using directives: {e}");
                return TurnIface::none();
            }
        }
    } else {
        None
    };
    TurnIface {
        turn: Some(Arc::new(turn)),
        socket,
        tool_loop: !cli,
    }
}

impl TurnIface {
    pub fn offered(&self) -> bool {
        self.turn.is_some()
    }

    /// The system prompt for this turn: directive teaching replaced by the
    /// Ligus tools when they're offered (D.4.4).
    pub fn system(&self, system: String, extra_blocks: &[&str]) -> String {
        if !self.offered() {
            return system;
        }
        crate::prompt::with_ligus_tools(&system, self.tool_loop, extra_blocks)
    }

    /// The folded tools' turn, for an in-process tool loop.
    pub fn folded(&self) -> Option<Arc<TurnHandle>> {
        self.turn.clone().filter(|_| self.tool_loop)
    }

    /// The agent turn scope for dispatch (MCP attach, pause, spend, the ACP
    /// permission asker, the folded tools).
    pub fn scope(&self) -> crate::agent_turn::AgentTurnScope {
        match &self.turn {
            Some(t) => crate::agent_turn::AgentTurnScope {
                ligus: self.folded(),
                ..t.agent_scope()
            },
            // No Ligus tools this turn: the spend budget still holds.
            None => crate::agent_turn::AgentTurnScope {
                spend: Some(crate::agent_turn::SpendMeter::new(
                    Budgets::from_env().max_spend_usd,
                )),
                ..Default::default()
            },
        }
    }

    /// What the tools did (finalize's dedupe; the diff intent).
    pub fn effects(&self) -> Option<crate::finalize::McpEffectsLog> {
        self.turn.as_ref().map(|t| t.effects())
    }

    /// End the turn: revoke the credential, record the content-free tool
    /// summary and (`detailed`) the turn's notes, and show the agent's
    /// self-report (`detailed`: with its confidence).
    /// Locks the service (`lock`) only after the turn is finished:
    /// finishing records the tool summary through the host, which locks it
    /// too.
    pub fn finish<'a>(
        &self,
        lock: impl FnOnce() -> std::sync::MutexGuard<'a, ChatService>,
        chat_id: Uuid,
        workspace_id: Uuid,
        author: &str,
        notes: &[String],
        detailed: bool,
    ) {
        let Some(t) = &self.turn else { return };
        t.finish();
        let mut svc = lock();
        if detailed {
            for n in notes {
                let json = serde_json::json!({ "note": n }).to_string();
                let _ = svc.record_device_fact(workspace_id, chat_id, "turnNote", &json);
            }
        }
        if let Some(r) = t.report() {
            let files = if r.files_touched.is_empty() {
                String::new()
            } else {
                format!(" Files: {}.", r.files_touched.join(", "))
            };
            let conf = match (&r.confidence, detailed) {
                (Some(c), true) => format!(" Confidence: {c}."),
                _ => String::new(),
            };
            let _ = svc.post_system_note(
                chat_id,
                workspace_id,
                format!(
                    "\u{1f4cb} {author} reported {}: {}.{files}{conf}",
                    r.status,
                    r.summary.trim_end_matches('.')
                ),
            );
        }
    }
}

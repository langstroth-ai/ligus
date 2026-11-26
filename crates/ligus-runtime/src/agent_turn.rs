//! The per-turn agent scope (Track D): what a runtime adapter needs to know
//! about the turn it runs, beyond the sandbox plan (`executor::with_turn`).
//!
//! - how a CLI reaches the Ligus MCP server ([`McpAttach`]: the bridge
//!   command and the env it needs; the credential never appears in argv);
//! - the pause counter: while an ask-the-human is pending, idle timeouts are
//!   suspended and only the ask's own timeout applies (D.4.2);
//! - the human asker that ACP permission escalations go to;
//! - the turn's spend meter ([`SpendMeter`]): ACP `usage_update` reports and
//!   the API runtimes' token usage (priced by [`crate::spend`]) are charged
//!   to it, and once the per-turn `maxSpendUsd` budget is used up the turn is
//!   cancelled, not just refused further Ligus tool calls.
//!
//! A host sets it around dispatch with [`with_agent_turn`]; adapters read it
//! with [`current`]. With no scope set, adapters behave as before: no Ligus
//! MCP server, no escalations (out-of-tier permission requests are denied).

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// How a child agent process reaches the Ligus MCP server: a stdio MCP
/// server entry (`ligus mcp-bridge`) and the env it needs. The env is passed
/// to the agent through its MCP config (ACP `mcpServers[].env`, or a 0700
/// config file), never on a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpAttach {
    /// Server name in the agent's MCP config.
    pub name: String,
    pub command: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// The per-turn unix socket (allowed through the sandbox).
    pub socket: PathBuf,
    /// A 0700 directory owned by the turn's MCP endpoint, where config files
    /// for legacy adapters are written.
    pub config_dir: PathBuf,
}

impl McpAttach {
    /// A stdio entry in the ACP `session/new` `mcpServers` shape.
    pub fn acp_entry(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "command": self.command.to_string_lossy(),
            "args": self.args,
            "env": self.env.iter().map(|(k, v)| serde_json::json!({"name": k, "value": v})).collect::<Vec<_>>(),
        })
    }

    /// The `mcpServers` entry for Claude Code / Copilot JSON configs.
    pub fn json_entry(&self) -> serde_json::Value {
        let env: serde_json::Map<String, serde_json::Value> =
            self.env.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect();
        serde_json::json!({
            "type": "stdio",
            "command": self.command.to_string_lossy(),
            "args": self.args,
            "env": env,
        })
    }

    /// Write `{"mcpServers":{<name>: entry, ..extra}}` to a 0600 file in the
    /// endpoint's 0700 dir and return its path.
    pub fn write_config_file(&self, file: &str, extra: &serde_json::Map<String, serde_json::Value>) -> std::io::Result<PathBuf> {
        let mut servers = extra.clone();
        servers.insert(self.name.clone(), self.json_entry());
        let body = serde_json::json!({ "mcpServers": servers }).to_string();
        let path = self.config_dir.join(file);
        crate::private_file::write_private_atomic(&path, body.as_bytes())?;
        Ok(path)
    }
}

/// An answer from a person to a question an agent asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HumanAnswer {
    pub answer: String,
    pub option_index: Option<usize>,
}

/// Something that can put a question to the chat's humans and wait for the
/// first answer (D.4.2). `None` = timed out (or the turn ended).
pub trait HumanAsker: Send + Sync {
    fn ask(&self, question: String, options: Vec<String>, timeout: Duration) -> Pin<Box<dyn Future<Output = Option<HumanAnswer>> + Send + '_>>;

    /// Put a permission request to the people who may grant it, as a grant
    /// card (`docs/design/agent-questions.md` 6.2). The default asks it as
    /// a question with Ligus's labels (an asker without grant cards).
    fn ask_grant(&self, req: GrantRequest) -> Pin<Box<dyn Future<Output = Option<HumanAnswer>> + Send + '_>> {
        let tool = serde_json::json!({"title": if req.title.is_empty() { "a tool" } else { req.title.as_str() }, "kind": req.tool});
        let (question, choices) = crate::provider::acp::permission_question(&req.agent_label, &tool, req.standing);
        self.ask(question, choices, req.timeout)
    }
}

/// An ACP permission request, as the host puts it on a grant card.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GrantRequest {
    /// The asking agent's name (Ligus's words around it).
    pub agent_label: String,
    /// The ACP tool kind (`execute`, `edit`, `fetch`, …).
    pub tool: String,
    /// The agent's own description of the call (unverified).
    pub title: String,
    /// The call's own content: the raw input, the command, the locations.
    pub detail: String,
    /// The detail was too large to review on a card: no Allow.
    pub too_large: bool,
    pub cwd: String,
    /// The agent offered only a permission for the rest of its session.
    pub standing: bool,
    /// The call reaches outside this turn's sandbox (a path outside the
    /// worktree, or open egress): the grant needs the person's passkey.
    pub outside_sandbox: bool,
    /// The runtime reports the call comes from one of the turn's
    /// sub-agents (ACP tool-call metadata).
    pub subagent: bool,
    pub timeout: Duration,
}

/// The most detail a grant card carries before it offers no Allow.
pub const MAX_GRANT_DETAIL_BYTES: usize = 8 * 1024;

/// The scope of one agent turn.
#[derive(Clone, Default)]
pub struct AgentTurnScope {
    pub mcp: Option<McpAttach>,
    /// > 0 while the turn is paused on a human.
    pub paused: Arc<AtomicUsize>,
    pub asker: Option<Arc<dyn HumanAsker>>,
    /// The turn's spend meter (`maxSpendUsd`).
    pub spend: Option<Arc<SpendMeter>>,
    /// The Ligus MCP turn, for in-process tool loops (folded tools).
    pub ligus: Option<Arc<crate::ligus_mcp::TurnHandle>>,
    /// Content-free notes for the host to record as device-local facts
    /// (e.g. `acp_fallback:copilot`).
    pub notes: Arc<std::sync::Mutex<Vec<String>>>,
}

impl std::fmt::Debug for AgentTurnScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentTurnScope")
            .field("mcp", &self.mcp)
            .field("paused", &self.paused.load(Ordering::SeqCst))
            .field("asker", &self.asker.is_some())
            .finish()
    }
}

/// Why a spend report ended the turn.
#[derive(Debug, Clone, PartialEq)]
pub enum SpendError {
    /// The budget is used up.
    Exceeded { total: f64, max: f64 },
    /// A cost that can't be counted (negative, not a number).
    Invalid(String),
}

impl std::fmt::Display for SpendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpendError::Exceeded { total, max } => write!(
                f,
                "spend budget reached: this turn has cost about ${total:.2} of its ${max:.2} maxSpendUsd (LIGUS_MCP_MAX_SPEND_USD), so it was stopped"
            ),
            SpendError::Invalid(why) => write!(f, "the turn was stopped because its spend can't be counted against maxSpendUsd: {why}"),
        }
    }
}

/// What one turn has spent, against its `maxSpendUsd` (if any). Shared by
/// the runtime adapters (which charge it) and the Ligus MCP server (which
/// refuses tool calls once it is used up).
#[derive(Debug, Default)]
pub struct SpendMeter {
    max: Option<f64>,
    total: std::sync::Mutex<f64>,
}

impl SpendMeter {
    pub fn new(max: Option<f64>) -> Arc<Self> {
        Arc::new(Self { max: max.filter(|m| m.is_finite() && *m > 0.0), total: std::sync::Mutex::new(0.0) })
    }

    pub fn max(&self) -> Option<f64> {
        self.max
    }

    pub fn total(&self) -> f64 {
        *self.total.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether the budget is used up.
    pub fn exhausted(&self) -> bool {
        self.max.is_some_and(|m| self.total() >= m)
    }

    /// Charge `usd`. `Err` = the turn must end: the budget is used up, or
    /// the amount can't be counted (only matters when there is a budget).
    pub fn charge(&self, usd: f64) -> Result<(), SpendError> {
        if !usd.is_finite() || usd < 0.0 {
            return match self.max {
                Some(_) => Err(SpendError::Invalid(format!("a cost of {usd} was reported"))),
                None => Ok(()),
            };
        }
        let total = {
            let mut t = self.total.lock().unwrap_or_else(|e| e.into_inner());
            *t += usd;
            *t
        };
        match self.max {
            Some(max) if total >= max => Err(SpendError::Exceeded { total, max }),
            _ => Ok(()),
        }
    }
}

/// Charge the current turn's meter (no-op without one).
pub fn charge_spend(usd: f64) -> Result<(), SpendError> {
    match current().and_then(|s| s.spend.clone()) {
        Some(m) => m.charge(usd),
        None => Ok(()),
    }
}

/// Whether the current turn has a spend budget.
pub fn has_spend_budget() -> bool {
    current().and_then(|s| s.spend.clone()).is_some_and(|m| m.max().is_some())
}

/// Holds the turn paused (idle timers suspended) until dropped.
pub struct PauseGuard(Arc<AtomicUsize>);

impl Drop for PauseGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl AgentTurnScope {
    pub fn pause(&self) -> PauseGuard {
        pause_counter(&self.paused)
    }
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst) > 0
    }
}

/// Pause `counter` until the guard drops.
pub fn pause_counter(counter: &Arc<AtomicUsize>) -> PauseGuard {
    counter.fetch_add(1, Ordering::SeqCst);
    PauseGuard(counter.clone())
}

tokio::task_local! {
    static SCOPE: Arc<AgentTurnScope>;
}

/// Run `fut` with `scope` as the agent turn scope.
pub async fn with_agent_turn<F: Future>(scope: AgentTurnScope, fut: F) -> F::Output {
    SCOPE.scope(Arc::new(scope), fut).await
}

/// The current agent turn scope, if a host set one.
pub fn current() -> Option<Arc<AgentTurnScope>> {
    SCOPE.try_with(Arc::clone).ok()
}

/// Record a content-free note on the current turn (no-op without a scope).
pub fn note(n: String) {
    if let Some(s) = current() {
        if let Ok(mut v) = s.notes.lock() {
            if v.len() < 64 {
                v.push(n);
            }
        }
    }
}

/// Whether the current turn is paused on a human.
pub fn paused() -> bool {
    current().is_some_and(|s| s.is_paused())
}

/// Await `fut` with an idle timeout that does not run while the turn is
/// paused on a human. `Err(())` = idle for `idle` while not paused.
pub async fn idle_timeout<F: Future>(idle: Duration, fut: F) -> Result<F::Output, ()> {
    let scope = current();
    tokio::pin!(fut);
    loop {
        match tokio::time::timeout(idle, &mut fut).await {
            Ok(v) => return Ok(v),
            Err(_) if scope.as_ref().is_some_and(|s| s.is_paused()) => continue,
            Err(_) => return Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn idle_timeout_is_suspended_while_paused() {
        let scope = AgentTurnScope::default();
        let counter = scope.paused.clone();
        with_agent_turn(scope, async move {
            // Paused: a 50ms idle window doesn't fire for a 200ms wait.
            let g = pause_counter(&counter);
            let r = idle_timeout(Duration::from_millis(50), tokio::time::sleep(Duration::from_millis(200))).await;
            assert!(r.is_ok());
            drop(g);
            // Not paused: it fires.
            let r = idle_timeout(Duration::from_millis(50), tokio::time::sleep(Duration::from_millis(500))).await;
            assert!(r.is_err());
        })
        .await;
    }
}

//! The Ligus MCP server core: per-turn credentials, the tools, budgets and
//! the audit trail. It runs inside the host process; agents reach it over a
//! per-turn unix socket through `ligus mcp-bridge` (see [`super::socket`]),
//! and in-process tool loops call it directly ([`TurnHandle::call`]).
//!
//! Hard limits, enforced here and again by authz: no admin or governance
//! events, no votes, no `ProposalApplied`, no apply, git or filesystem
//! writes; proposals only. Every effect goes through
//! [`crate::effects`] inside [`crate::ChatService::as_agent`].

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ligus_core::flow::directives::{MarkDirective, UnmarkDirective};
use ligus_core::flow::{self, ContextParcel, FlowRecord, HumanAsk, MarkKind, TurnToolSummary};
use ligus_core::{ChatSession, ProposalKind, ProposalStatus, ProposedCommand};
use serde_json::{json, Value};
use uuid::Uuid;

use super::cred::{self, TurnBinding};
use crate::effects::{self, EffectCtx, NewProposal, Via};
use crate::finalize::{kind_key, McpEffectsLog};
use crate::ChatService;

/// Per-turn budgets (D.3.4). Device-local host policy, never a fold rule.
#[derive(Debug, Clone, PartialEq)]
pub struct Budgets {
    pub max_tool_calls: u32,
    pub max_proposals: u32,
    pub max_handoffs: u32,
    pub max_asks: u32,
    pub max_read_messages: u32,
    /// Read-only code-host calls (`ligus_forge_*`), each a request from the
    /// host to the forge on the agent's behalf.
    pub max_forge_calls: u32,
    /// Workspace memory proposals (`ligus_memory_propose`).
    pub max_memory_proposals: u32,
    /// Charged from ACP `usage_update` reports and from the API runtimes'
    /// reported token usage ([`crate::spend`]); reaching it cancels the turn.
    pub max_spend_usd: Option<f64>,
    /// Shared by every turn descended from one human message.
    pub cascade: CascadeBudgets,
}

impl Default for Budgets {
    fn default() -> Self {
        Self { max_tool_calls: 64, max_proposals: 8, max_handoffs: 2, max_asks: 3, max_read_messages: 500, max_forge_calls: 16, max_memory_proposals: 4, max_spend_usd: None, cascade: CascadeBudgets::default() }
    }
}

/// Budgets for a whole cascade: every turn that descends from one human
/// message through hand-offs (and replies to agent replies) draws on these,
/// on top of its own per-turn budget. Without them one message could fan
/// out to `MAX_CASCADE_DEPTH` turns times each turn's budget.
#[derive(Debug, Clone, PartialEq)]
pub struct CascadeBudgets {
    pub max_tool_calls: u32,
    pub max_proposals: u32,
    pub max_handoffs: u32,
    /// Questions, ACP permission escalations included.
    pub max_asks: u32,
}

impl Default for CascadeBudgets {
    fn default() -> Self {
        Self { max_tool_calls: 192, max_proposals: 12, max_handoffs: 6, max_asks: 6 }
    }
}

impl CascadeBudgets {
    /// Defaults, overridden by this device's env
    /// (`LIGUS_MCP_MAX_CASCADE_TOOL_CALLS`, `…_PROPOSALS`, `…_HANDOFFS`,
    /// `…_ASKS`).
    pub fn from_env() -> Self {
        let n = |k: &str, d: u32| ligus_core::names::var(k).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(d);
        let d = Self::default();
        Self {
            max_tool_calls: n("LIGUS_MCP_MAX_CASCADE_TOOL_CALLS", d.max_tool_calls),
            max_proposals: n("LIGUS_MCP_MAX_CASCADE_PROPOSALS", d.max_proposals),
            max_handoffs: n("LIGUS_MCP_MAX_CASCADE_HANDOFFS", d.max_handoffs),
            max_asks: n("LIGUS_MCP_MAX_CASCADE_ASKS", d.max_asks),
        }
    }
}

impl Budgets {
    /// Defaults, overridden by this device's env (`LIGUS_MCP_MAX_TOOL_CALLS`,
    /// `…_PROPOSALS`, `…_HANDOFFS`, `…_ASKS`, `…_READ_MESSAGES`,
    /// `LIGUS_MCP_MAX_SPEND_USD`).
    pub fn from_env() -> Self {
        let n = |k: &str, d: u32| ligus_core::names::var(k).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(d);
        let d = Self::default();
        Self {
            max_tool_calls: n("LIGUS_MCP_MAX_TOOL_CALLS", d.max_tool_calls),
            max_proposals: n("LIGUS_MCP_MAX_PROPOSALS", d.max_proposals),
            max_handoffs: n("LIGUS_MCP_MAX_HANDOFFS", d.max_handoffs),
            max_asks: n("LIGUS_MCP_MAX_ASKS", d.max_asks),
            max_read_messages: n("LIGUS_MCP_MAX_READ_MESSAGES", d.max_read_messages),
            max_forge_calls: n("LIGUS_MCP_MAX_FORGE_CALLS", d.max_forge_calls),
            max_memory_proposals: n("LIGUS_MCP_MAX_MEMORY_PROPOSALS", d.max_memory_proposals),
            max_spend_usd: ligus_core::names::var("LIGUS_MCP_MAX_SPEND_USD").ok().and_then(|v| v.trim().parse().ok()).filter(|x: &f64| *x > 0.0),
            cascade: CascadeBudgets::from_env(),
        }
    }
}

#[path = "decision.rs"]
pub mod decision;

/// What the server needs from its host.
pub trait LigusHost: Send + Sync {
    /// Run `f` with the host's chat service (under its lock).
    fn with_service(&self, f: &mut dyn FnMut(&mut ChatService));
    /// A hand-off to an agent: start (queue) its turn now (D.4.1). The
    /// trigger is the handing agent's reply.
    fn start_turn(&self, _chat_id: Uuid, _agent_id: Uuid, _trigger_message_id: Uuid) {}
    /// An ask was posted (push it to phones, refresh the UI).
    fn ask_posted(&self, _chat_id: Uuid, _ask: &HumanAsk) {}
    /// The code host behind `chat_id`'s bound repository, for the read-only
    /// `ligus_forge_*` tools (`None` = this host has none for that chat).
    fn forge_for(&self, _chat_id: Uuid) -> Option<super::forge_tools::ForgeBinding> {
        None
    }
    /// Keep a file an agent offered, under the id of the proposal it is
    /// offered in, until a person approves that proposal, when this host
    /// uploads it (issue #118). Only offers this host staged are ever
    /// uploaded. A host that can't keep files refuses offers.
    fn stage_attachment(&self, _proposal_id: Uuid, _sha256: &str, _bytes: &[u8]) -> Result<(), String> {
        Err("this host can't offer files".into())
    }
}

/// One turn's binding and context.
#[derive(Debug, Clone)]
pub struct TurnSpec {
    pub workspace_id: Uuid,
    pub chat_id: Uuid,
    /// The responding agent (`None` = the chat's primary runtime).
    pub agent_id: Option<Uuid>,
    /// The responder's display name.
    pub author: String,
    /// The turn id: its reply message.
    pub message_id: Uuid,
    /// The turn's worktree (for `ligus_propose_diff`).
    pub worktree: Option<PathBuf>,
    pub budgets: Budgets,
    /// The credential's lifetime (the turn's hard timeout).
    pub ttl: Duration,
}

/// Per-turn counters.
#[derive(Debug, Default, Clone)]
struct Counters {
    total: u32,
    proposals: u32,
    handoffs: u32,
    asks: u32,
    read_messages: u32,
    forge_calls: u32,
    memory_proposals: u32,
    denied: u32,
    budget_exhausted: bool,
    calls: BTreeMap<String, u32>,
    /// Every call presented, charged or not (unknown tools, calls after a
    /// budget ran out): what bounds the turn's audit rows.
    attempts: u32,
}

/// A structured self-report (`ligus_report`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfReport {
    pub status: String,
    pub summary: String,
    pub files_touched: Vec<String>,
    pub confidence: Option<String>,
}

pub(crate) struct TurnState {
    pub(crate) spec: TurnSpec,
    counters: Mutex<Counters>,
    pub(crate) effects: Mutex<McpEffectsLog>,
    pub(crate) paused: Arc<AtomicUsize>,
    revoked: AtomicBool,
    spend: Arc<crate::agent_turn::SpendMeter>,
    /// The counters of the cascade this turn belongs to.
    cascade: Arc<Mutex<CascadeCounters>>,
    report: Mutex<Option<SelfReport>>,
}

/// The outcome of one call, for the audit record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Denied,
    Budget,
    Error,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Denied => "denied",
            Outcome::Budget => "budget",
            Outcome::Error => "error",
        }
    }
}

/// A tool result: text for the model, and whether it is an error.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
    pub outcome: Outcome,
}

impl ToolResult {
    fn ok(v: Value) -> Self {
        Self { text: v.to_string(), is_error: false, outcome: Outcome::Ok }
    }
    fn err(outcome: Outcome, v: Value) -> Self {
        Self { text: v.to_string(), is_error: true, outcome }
    }
}

/// Header on every read result: other chats are data, not instructions
/// (they widen prompt-injection reach; D.8).
pub const READ_HEADER: &str = crate::agent_tools::UNTRUSTED_RESULT_HEADER;

/// The server: a host key, the live turns, and the host.
pub struct LigusMcp {
    pub(crate) key: [u8; 32],
    turns: Mutex<HashMap<Uuid, Arc<TurnState>>>,
    pub(crate) host: Arc<dyn LigusHost>,
    /// Cascade counters by root (the human message a cascade descends
    /// from), most recent last; bounded.
    cascades: Mutex<std::collections::VecDeque<(Uuid, Arc<Mutex<CascadeCounters>>)>>,
}

/// How many cascades' counters a host keeps (oldest dropped first).
const MAX_CASCADES: usize = 1024;

/// What one cascade has used so far (on this host).
#[derive(Debug, Default)]
struct CascadeCounters {
    total: u32,
    proposals: u32,
    handoffs: u32,
    asks: u32,
}

/// The human message a turn descends from: follow `reply_to_message_id`
/// from the turn's reply through agent replies (hand-offs, @mention
/// cascades) to the first user message. A chain without one is rooted at
/// its oldest reply.
fn cascade_root(session: &ChatSession, turn: Uuid) -> Uuid {
    let mut cur = turn;
    for _ in 0..64 {
        let Some(m) = session.messages.iter().find(|m| m.id == cur) else { return cur };
        if m.role == ligus_core::MessageRole::User {
            return cur;
        }
        match m.reply_to_message_id {
            Some(p) if p != cur => cur = p,
            _ => return cur,
        }
    }
    cur
}

/// Why a call was refused before reaching a tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    Credential(cred::CredError),
    UnknownTurn,
    Revoked,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Credential(e) => write!(f, "invalid turn credential ({e:?})"),
            AuthError::UnknownTurn => write!(f, "the turn this credential names is not running"),
            AuthError::Revoked => write!(f, "the turn has ended"),
        }
    }
}

fn agent_cred_id(agent: Option<Uuid>) -> Uuid {
    agent.unwrap_or(Uuid::nil())
}

impl LigusMcp {
    pub fn new(host: Arc<dyn LigusHost>) -> Arc<Self> {
        Arc::new(Self { key: cred::host_key(), turns: Mutex::new(HashMap::new()), host, cascades: Mutex::new(Default::default()) })
    }

    /// Register a turn and mint its credential.
    pub fn begin_turn(self: &Arc<Self>, spec: TurnSpec) -> TurnHandle {
        let expires = cred::now_unix() + spec.ttl.as_secs().max(1);
        let binding = TurnBinding { turn_id: spec.message_id, chat_id: spec.chat_id, agent_id: agent_cred_id(spec.agent_id), expires_unix: expires };
        let credential = cred::mint(&self.key, &binding);
        let spend = crate::agent_turn::SpendMeter::new(spec.budgets.max_spend_usd);
        let cascade = self.cascade_for(spec.chat_id, spec.message_id);
        let state = Arc::new(TurnState {
            cascade,
            spec,
            counters: Mutex::new(Counters::default()),
            effects: Mutex::new(McpEffectsLog::default()),
            paused: Arc::new(AtomicUsize::new(0)),
            revoked: AtomicBool::new(false),
            spend,
            report: Mutex::new(None),
        });
        self.turns.lock().unwrap_or_else(|e| e.into_inner()).insert(binding.turn_id, state.clone());
        TurnHandle { mcp: self.clone(), state, cred: credential, socket: Mutex::new(None), finished: AtomicBool::new(false) }
    }

    /// The counters of the cascade `turn` (a reply in `chat`) belongs to.
    fn cascade_for(&self, chat: Uuid, turn: Uuid) -> Arc<Mutex<CascadeCounters>> {
        let mut root = turn;
        self.host.with_service(&mut |svc| {
            if let Ok(Some(s)) = svc.load(chat) {
                root = cascade_root(&s, turn);
            }
        });
        let mut cs = self.cascades.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = cs.iter().position(|(r, _)| *r == root) {
            let e = cs.remove(i).expect("present");
            let c = e.1.clone();
            cs.push_back(e);
            return c;
        }
        let c = Arc::new(Mutex::new(CascadeCounters::default()));
        cs.push_back((root, c.clone()));
        while cs.len() > MAX_CASCADES {
            cs.pop_front();
        }
        c
    }

    /// Check a credential: MAC, expiry, a live (not revoked) turn, and the
    /// chat and agent it binds.
    pub(crate) fn authorize(&self, credential: &str) -> Result<Arc<TurnState>, AuthError> {
        let b = cred::verify(&self.key, credential, cred::now_unix()).map_err(AuthError::Credential)?;
        let st = self.turns.lock().unwrap_or_else(|e| e.into_inner()).get(&b.turn_id).cloned().ok_or(AuthError::UnknownTurn)?;
        if st.revoked.load(Ordering::SeqCst) {
            return Err(AuthError::Revoked);
        }
        if st.spec.chat_id != b.chat_id || agent_cred_id(st.spec.agent_id) != b.agent_id {
            return Err(AuthError::UnknownTurn);
        }
        Ok(st)
    }

    fn revoke(&self, turn_id: Uuid) {
        if let Some(st) = self.turns.lock().unwrap_or_else(|e| e.into_inner()).remove(&turn_id) {
            st.revoked.store(true, Ordering::SeqCst);
        }
    }

    /// Call `tool` with `args` under `credential`. Every call is audited.
    pub async fn call(&self, credential: &str, tool: &str, args: &Value) -> Result<ToolResult, AuthError> {
        let st = self.authorize(credential)?;
        let started = Instant::now();
        // Every call counts toward maxToolCalls, unknown tools included: past
        // it, nothing runs and only the first refusal is audited, so a turn
        // can't flood the bounded audit ring and push older records out.
        let attempt = {
            let mut c = st.counters.lock().unwrap_or_else(|e| e.into_inner());
            c.attempts = c.attempts.saturating_add(1);
            c.attempts
        };
        let over = attempt > st.spec.budgets.max_tool_calls;
        let (res, read_chat) = if over {
            let r = ToolResult::err(
                Outcome::Budget,
                json!({"error": "budget_exhausted", "budget": "maxToolCalls", "message": crate::tool_loop::FINAL_ROUND_NUDGE}),
            );
            (r, None)
        } else {
            self.dispatch(&st, tool, args).await
        };
        {
            let mut c = st.counters.lock().unwrap_or_else(|e| e.into_inner());
            match res.outcome {
                Outcome::Denied => c.denied += 1,
                Outcome::Budget => c.budget_exhausted = true,
                _ => {}
            }
        }
        if attempt <= st.spec.budgets.max_tool_calls.saturating_add(1) {
            self.audit(&st, tool, res.outcome, started.elapsed(), read_chat);
        }
        Ok(res)
    }

    /// One device-local, content-free audit fact per call (D.3.5).
    fn audit(&self, st: &TurnState, tool: &str, outcome: Outcome, latency: Duration, read_chat: Option<Uuid>) {
        let mut rec = json!({
            "ts": flow::now_ms(),
            "turnId": st.spec.message_id.to_string(),
            "chatId": st.spec.chat_id.to_string(),
            "agentKey": flow::agent_key(st.spec.agent_id),
            "tool": if super::tools::is_ligus_tool(tool) { tool } else { "unknown" },
            "outcome": outcome.as_str(),
            "latencyMs": latency.as_millis() as u64,
        });
        if let Some(c) = read_chat {
            rec["readChatId"] = json!(c.to_string());
        }
        let line = rec.to_string();
        let (ws, chat) = (st.spec.workspace_id, st.spec.chat_id);
        self.host.with_service(&mut |svc| {
            if let Err(e) = svc.record_tool_audit(ws, chat, flow::now_ms(), &line) {
                tracing::warn!(target: "ligus_mcp", "audit record failed: {e}");
            }
        });
    }

    /// Run `f` with the service; returns its result.
    fn svc<R>(&self, mut f: impl FnMut(&mut ChatService) -> R) -> R {
        let mut out = None;
        self.host.with_service(&mut |svc| out = Some(f(svc)));
        out.expect("the host ran the closure")
    }

    fn ctx(st: &TurnState) -> EffectCtx {
        EffectCtx {
            session_id: st.spec.chat_id,
            workspace_id: st.spec.workspace_id,
            author: st.spec.author.clone(),
            agent_id: st.spec.agent_id,
            message_id: Some(st.spec.message_id),
            via: Via::Mcp,
        }
    }

    /// Count one call against the turn's budgets and its cascade's; `Err`
    /// when one is used up (nothing is counted then).
    fn charge(st: &TurnState, tool: &str) -> Result<(), ToolResult> {
        let b = &st.spec.budgets;
        let cb = &b.cascade;
        let mut c = st.counters.lock().unwrap_or_else(|e| e.into_inner());
        let mut cc = st.cascade.lock().unwrap_or_else(|e| e.into_inner());
        let exhausted = |name: &str| {
            ToolResult::err(
                Outcome::Budget,
                json!({"error": "budget_exhausted", "budget": name, "message": crate::tool_loop::FINAL_ROUND_NUDGE}),
            )
        };
        if c.total >= b.max_tool_calls {
            return Err(exhausted("maxToolCalls"));
        }
        if cc.total >= cb.max_tool_calls {
            return Err(exhausted("cascadeToolCalls"));
        }
        if st.spend.exhausted() {
            return Err(exhausted("maxSpendUsd"));
        }
        if tool == "ligus_memory_propose" {
            if c.memory_proposals >= b.max_memory_proposals {
                return Err(exhausted("maxMemoryProposals"));
            }
            c.memory_proposals += 1;
        }
        let kind = match tool {
            "ligus_propose_diff" | "ligus_propose_decision" | "ligus_propose_command" | "ligus_propose_workflow" | "ligus_open_alternatives"
            | "ligus_offer_file" => Some(0),
            "ligus_handoff" => Some(1),
            "ligus_ask_human" | "ligus_ask_person" | "ligus_ask_room" => Some(2),
            _ => None,
        };
        if let Some(k) = kind {
            let (n, max, name, cn, cmax, cname) = match k {
                0 => (&mut c.proposals, b.max_proposals, "maxProposals", &mut cc.proposals, cb.max_proposals, "cascadeProposals"),
                1 => (&mut c.handoffs, b.max_handoffs, "maxHandoffs", &mut cc.handoffs, cb.max_handoffs, "cascadeHandoffs"),
                _ => (&mut c.asks, b.max_asks, "maxAsks", &mut cc.asks, cb.max_asks, "cascadeAsks"),
            };
            // A card past the asks budget posts nothing (section 4.1).
            let card = matches!(tool, "ligus_ask_person" | "ligus_ask_room");
            let over = |name: &str| if card { ToolResult::err(Outcome::Budget, json!({"error": "ask_budget", "budget": name})) } else { exhausted(name) };
            if *n >= max {
                return Err(over(name));
            }
            if *cn >= cmax {
                return Err(over(cname));
            }
            *n += 1;
            *cn += 1;
        }
        c.total += 1;
        cc.total += 1;
        *c.calls.entry(tool.to_string()).or_insert(0) += 1;
        Ok(())
    }

    /// Give back the ask a card charged when it was refused before posting
    /// (a bad argument is not a question asked).
    pub(super) fn refund_ask(st: &TurnState) {
        let mut c = st.counters.lock().unwrap_or_else(|e| e.into_inner());
        let mut cc = st.cascade.lock().unwrap_or_else(|e| e.into_inner());
        c.asks = c.asks.saturating_sub(1);
        cc.asks = cc.asks.saturating_sub(1);
    }

    /// Count an ACP permission escalation (a question to the chat's people)
    /// against the turn's and the cascade's question budgets. `false` = used
    /// up: the request is denied without asking.
    fn charge_escalation(st: &TurnState) -> bool {
        let b = &st.spec.budgets;
        let mut c = st.counters.lock().unwrap_or_else(|e| e.into_inner());
        let mut cc = st.cascade.lock().unwrap_or_else(|e| e.into_inner());
        if c.asks >= b.max_asks || cc.asks >= b.cascade.max_asks {
            c.budget_exhausted = true;
            return false;
        }
        c.asks += 1;
        cc.asks += 1;
        true
    }

    fn charge_reads(st: &TurnState, n: u32) -> Result<(), ToolResult> {
        let mut c = st.counters.lock().unwrap_or_else(|e| e.into_inner());
        if c.read_messages.saturating_add(n) > st.spec.budgets.max_read_messages {
            return Err(ToolResult::err(
                Outcome::Budget,
                json!({"error": "budget_exhausted", "budget": "maxReadMessages", "message": crate::tool_loop::FINAL_ROUND_NUDGE}),
            ));
        }
        c.read_messages += n;
        Ok(())
    }

    async fn dispatch(&self, st: &Arc<TurnState>, tool: &str, args: &Value) -> (ToolResult, Option<Uuid>) {
        if !super::tools::TOOLS.iter().any(|(n, _)| *n == tool) {
            // Unknown, or an attempt at something the server never offers
            // (apply, vote, admin): refused and audited as denied.
            return (ToolResult::err(Outcome::Denied, json!({"error": "unknown_tool", "message": format!("Ligus has no tool {tool:?}; agents can only propose, never apply or vote.")})), None);
        }
        if let Err(r) = Self::charge(st, tool) {
            return (r, None);
        }
        if super::forge_tools::is_forge_tool(tool) {
            return (self.forge_tool(st, tool, args).await, None);
        }
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
        let strs = |k: &str| -> Vec<String> {
            args.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default()
        };
        let ctx = Self::ctx(st);
        let denied_or_err = |e: String| {
            let lower = e.to_ascii_lowercase();
            let outcome = if lower.contains("agent tier") || lower.contains("unauthorized") || lower.contains("not allowed") || lower.contains("can't") {
                Outcome::Denied
            } else {
                Outcome::Error
            };
            ToolResult::err(outcome, json!({"error": if outcome == Outcome::Denied { "denied" } else { "failed" }, "message": e}))
        };
        let log = |f: &mut dyn FnMut(&mut McpEffectsLog)| f(&mut st.effects.lock().unwrap_or_else(|e| e.into_inner()));
        match tool {
            "ligus_propose_diff" => {
                let title = s("title");
                if title.is_empty() {
                    return (denied_or_err("a title is required".into()), None);
                }
                let Some(wt) = st.spec.worktree.clone() else {
                    return (denied_or_err("this turn has no worktree, so there is no diff to propose".into()), None);
                };
                let files = worktree_changes(&wt);
                if files.is_empty() {
                    return (denied_or_err("your worktree has no changes yet; edit files first, then call ligus_propose_diff".into()), None);
                }
                let tc = Some(s("taskClass")).filter(|t| !t.is_empty());
                log(&mut |l| l.diff_intent = Some((title.clone(), s("body"), tc.clone())));
                (
                    ToolResult::ok(json!({"ok": true, "files": files.len(), "message": format!("Your changes to {} file(s) will be filed for review as \u{201c}{title}\u{201d} when your turn ends. A human approves and applies them.", files.len())})),
                    None,
                )
            }
            "ligus_propose_decision" | "ligus_propose_command" => {
                let command = if tool == "ligus_propose_command" {
                    let cwd = Some(s("cwd")).filter(|c| !c.is_empty() && c != ".");
                    Some(ProposedCommand { argv: strs("argv"), cwd })
                } else {
                    None
                };
                let kind = if command.is_some() { ProposalKind::Command } else { ProposalKind::Decision };
                let np = NewProposal {
                    title: s("title"),
                    body: s("body"),
                    kind: Some(kind),
                    required_approvals: args.get("requiredApprovals").and_then(Value::as_u64).map(|n| n.min(16) as u32),
                    command,
                    task_class: None,
                };
                let title = np.title.to_lowercase();
                match self.svc(|svc| effects::propose(svc, &ctx, np.clone())) {
                    Ok(id) => {
                        log(&mut |l| {
                            l.proposals.insert((kind_key(kind), title.clone()));
                        });
                        (ToolResult::ok(json!({"ok": true, "proposalId": id.to_string(), "status": "open", "message": "Filed for human review. It is not executed until a person approves and implements it."})), None)
                    }
                    Err(e) => (denied_or_err(e), None),
                }
            }
            "ligus_offer_file" => (self.offer_file(st, &ctx, &s("path"), &s("title"), &s("body")), None),
            "ligus_propose_workflow" => {
                let Some(def) = args.get("definition").filter(|d| d.is_object()) else {
                    return (denied_or_err("definition must be an object".into()), None);
                };
                let json = def.to_string();
                match self.svc(|svc| effects::file_workflow(svc, &ctx, &json)) {
                    Ok(name) => {
                        log(&mut |l| {
                            l.workflows.insert(name.trim().to_lowercase());
                        });
                        (ToolResult::ok(json!({"ok": true, "workflow": name, "message": "Filed for review; a human approves it before it can run."})), None)
                    }
                    Err(e) => (denied_or_err(e), None),
                }
            }
            "ligus_open_alternatives" => {
                let options: Vec<crate::directives::ParsedAlternative> = args
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|o| crate::directives::ParsedAlternative {
                                title: o.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
                                body: o.get("body").and_then(Value::as_str).unwrap_or("").to_string(),
                                kind: None,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if !(2..=6).contains(&options.len()) {
                    return (denied_or_err("give 2 to 6 options".into()), None);
                }
                let alts = crate::directives::ParsedAlternatives {
                    question: s("question"),
                    body: s("body"),
                    task_class: Some(s("taskClass")).filter(|t| !t.is_empty()),
                    agent_quorum: args.get("agentQuorum").and_then(Value::as_u64).map(|n| n.min(8) as u32),
                    options,
                };
                let q = alts.question.to_lowercase();
                let r = self.svc(|svc| {
                    let trigger = svc.load_in(ctx.workspace_id, ctx.session_id).ok().flatten().and_then(|sess| effects::trigger_of(&sess, ctx.message_id));
                    effects::open_alternatives(svc, &ctx, &alts, trigger, None)
                });
                match r {
                    Ok(set) => {
                        log(&mut |l| {
                            l.set_questions.insert(q.clone());
                        });
                        (ToolResult::ok(json!({"ok": true, "setId": set.to_string()})), None)
                    }
                    Err(e) => (denied_or_err(e), None),
                }
            }
            "ligus_assess" => {
                let set = Uuid::parse_str(&s("setId")).ok();
                let a = crate::directives::ParsedAssessment {
                    alternative: s("alternative"),
                    score: args.get("score").and_then(Value::as_u64).unwrap_or(0).min(100) as u32,
                    rationale: s("rationale"),
                };
                match self.svc(|svc| effects::assess(svc, &ctx, set, &a)) {
                    Ok((set, pid)) => {
                        log(&mut |l| {
                            l.assessed.insert(a.alternative.clone());
                        });
                        let _ = self.svc(|svc| svc.settle_proposal_set(ctx.session_id, ctx.workspace_id, set, ligus_core::Timestamp::now()));
                        (ToolResult::ok(json!({"ok": true, "setId": set.to_string(), "proposalId": pid.to_string()})), None)
                    }
                    Err(e) => (denied_or_err(e), None),
                }
            }
            "ligus_handoff" => {
                let to = s("to");
                let parcel = ContextParcel { key_files: strs("keyFiles"), decisions: strs("decisions"), ruled_out: strs("ruledOut"), notes: s("notes") };
                let deep = self.svc(|svc| svc.load_in(ctx.workspace_id, ctx.session_id).ok().flatten().is_some_and(|sess| sess.cascade_depth() >= ligus_core::MAX_CASCADE_DEPTH));
                if deep {
                    return (denied_or_err("the hand-off chain is too deep; let a person take it from here".into()), None);
                }
                match self.svc(|svc| effects::handoff(svc, &ctx, &to, parcel.clone())) {
                    Ok(h) => {
                        log(&mut |l| {
                            l.handoffs_to.insert(to.trim_start_matches('@').to_lowercase());
                        });
                        if let Some(agent) = h.agent {
                            self.host.start_turn(ctx.session_id, agent, st.spec.message_id);
                        }
                        let started = h.agent.is_some();
                        (ToolResult::ok(json!({"ok": true, "handoffId": h.card.id.to_string(), "receiverStarted": started})), None)
                    }
                    Err(e) => (denied_or_err(e), None),
                }
            }
            "ligus_mark" => {
                let kind = match s("kind").as_str() {
                    "claim" => MarkKind::Claim,
                    "lock" => MarkKind::UnderConstruction,
                    "review" => MarkKind::NeedsReview,
                    _ => return (denied_or_err("kind is claim, lock or review".into()), None),
                };
                let lines = args.get("lines").and_then(Value::as_array).and_then(|a| match (a.first().and_then(Value::as_u64), a.get(1).and_then(Value::as_u64)) {
                    (Some(x), Some(y)) if x >= 1 && y >= x => Some((x as u32, y as u32)),
                    _ => None,
                });
                let m = MarkDirective { kind, path: s("path"), lines, note: s("note") };
                match self.svc(|svc| effects::mark(svc, &ctx, &m)) {
                    Ok(id) => {
                        log(&mut |l| {
                            l.mark_paths.insert(m.path.clone());
                        });
                        (ToolResult::ok(json!({"ok": true, "markId": id.to_string()})), None)
                    }
                    Err(e) => (denied_or_err(e), None),
                }
            }
            "ligus_unmark" => {
                let kind = match s("kind").as_str() {
                    "claim" => Some(MarkKind::Claim),
                    "lock" => Some(MarkKind::UnderConstruction),
                    "review" => Some(MarkKind::NeedsReview),
                    _ => None,
                };
                let u = UnmarkDirective { id: Uuid::parse_str(&s("id")).ok(), path: Some(s("path")).filter(|p| !p.is_empty()), kind };
                match self.svc(|svc| effects::unmark(svc, &ctx, &u)) {
                    Ok(n) => (ToolResult::ok(json!({"ok": true, "cleared": n})), None),
                    Err(e) => (denied_or_err(e), None),
                }
            }
            "ligus_react" => {
                let mid = Uuid::parse_str(&s("messageId")).unwrap_or(st.spec.message_id);
                let emoji = s("emoji");
                match self.svc(|svc| effects::react(svc, &ctx, mid, &emoji)) {
                    Ok(()) => {
                        if mid == st.spec.message_id {
                            log(&mut |l| {
                                l.reactions.insert(emoji.clone());
                            });
                        }
                        (ToolResult::ok(json!({"ok": true})), None)
                    }
                    Err(e) => (denied_or_err(e), None),
                }
            }
            "ligus_list_chats" => {
                let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1, 50) as usize;
                let channel = s("channel").to_lowercase();
                let ws = st.spec.workspace_id;
                let chats = self.svc(|svc| list_chats(svc, ws, &channel, limit));
                (ToolResult::ok(json!({"header": READ_HEADER, "chats": chats})), None)
            }
            "ligus_read_chat" => {
                let chat = Uuid::parse_str(&s("chatId")).unwrap_or(st.spec.chat_id);
                let since = Uuid::parse_str(&s("sinceMessageId")).ok();
                let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, 100) as usize;
                let ws = st.spec.workspace_id;
                let Some(session) = self.svc(|svc| svc.load_in(ws, chat).ok().flatten()).filter(|sess| sess.workspace_id == ws) else {
                    return (ToolResult::err(Outcome::Denied, json!({"error": "denied", "message": "no such chat in this workspace"})), Some(chat));
                };
                if !self.cross_read_allowed(st, &session) {
                    return (ToolResult::err(Outcome::Denied, json!({"error": "denied", "message": "that chat is more restricted than this one (locked down or allow-listed); its content can't be read from here"})), Some(chat));
                }
                let msgs = read_messages(&session, since, limit);
                if let Err(r) = Self::charge_reads(st, msgs.len() as u32) {
                    return (r, Some(chat));
                }
                (ToolResult::ok(json!({"header": READ_HEADER, "chatId": chat.to_string(), "title": session.title, "messages": msgs})), Some(chat))
            }
            "ligus_read_proposals" => {
                let chat = Uuid::parse_str(&s("chatId")).unwrap_or(st.spec.chat_id);
                let ws = st.spec.workspace_id;
                let Some(session) = self.svc(|svc| svc.load_in(ws, chat).ok().flatten()).filter(|sess| sess.workspace_id == ws) else {
                    return (ToolResult::err(Outcome::Denied, json!({"error": "denied", "message": "no such chat in this workspace"})), Some(chat));
                };
                if !self.cross_read_allowed(st, &session) {
                    return (ToolResult::err(Outcome::Denied, json!({"error": "denied", "message": "that chat is more restricted than this one (locked down or allow-listed); its content can't be read from here"})), Some(chat));
                }
                let status = s("status");
                let set = Uuid::parse_str(&s("setId")).ok();
                let list: Vec<Value> = session
                    .proposals
                    .iter()
                    .filter(|p| status.is_empty() || status_str(p.status) == status)
                    .filter(|p| set.is_none_or(|x| p.set_id == Some(x)))
                    .rev()
                    .take(50)
                    .map(|p| {
                        json!({
                            "id": p.id.to_string(), "title": p.title, "kind": kind_key(p.kind), "status": status_str(p.status),
                            "author": p.author_actor_id, "producingAgentId": p.producing_agent_id.map(|a| a.to_string()),
                            "setId": p.set_id.map(|x| x.to_string()), "body": p.body.chars().take(2000).collect::<String>(),
                            "changedFiles": p.changed_files, "argv": p.command.as_ref().map(|c| c.argv.clone()),
                        })
                    })
                    .collect();
                // Each proposal is read content like a message (up to 2,000
                // body chars): charged against maxReadMessages.
                if let Err(r) = Self::charge_reads(st, list.len() as u32) {
                    return (r, Some(chat));
                }
                (ToolResult::ok(json!({"header": READ_HEADER, "proposals": list})), (chat != st.spec.chat_id).then_some(chat))
            }
            "ligus_read_board" => {
                let ws = st.spec.workspace_id;
                let chat = st.spec.chat_id;
                let (config, session) = self.svc(|svc| (svc.flow_config(ws).unwrap_or_default(), svc.load_in(ws, chat).ok().flatten()));
                let marks: Vec<Value> = config
                    .marks
                    .values()
                    .take(100)
                    .map(|m| json!({"id": m.id.to_string(), "kind": m.kind.as_str(), "path": m.path, "lines": m.lines, "by": m.by, "note": m.note}))
                    .collect();
                let (handoffs, asks): (Vec<Value>, Vec<Value>) = session
                    .map(|sess| {
                        let h = sess.flow.handoffs.iter().rev().take(20).map(|h| json!({"from": h.from_name, "to": h.to_name, "task": h.task, "via": h.via})).collect();
                        let a = sess.flow.asks.values().filter(|a| a.is_open()).map(|a| json!({"askId": a.ask.ask_id.to_string(), "question": a.ask.question})).collect();
                        (h, a)
                    })
                    .unwrap_or_default();
                (ToolResult::ok(json!({"header": READ_HEADER, "marks": marks, "handoffs": handoffs, "openAsks": asks})), None)
            }
            "ligus_memory_search" | "ligus_memory_read" | "ligus_memory_propose" | "ligus_task_update" | "ligus_task_state" => {
                self.memory_tool(st, tool, args)
            }
            "ligus_ask_human" => self.ask_human(st, &ctx, args).await,
            "ligus_ask_person" => self.ask_card(st, &ctx, args, true).await,
            "ligus_ask_room" => self.ask_card(st, &ctx, args, false).await,
            "ligus_report" => {
                let status = s("status");
                if !matches!(status.as_str(), "done" | "blocked" | "needsReview") {
                    return (denied_or_err("status is done, blocked or needsReview".into()), None);
                }
                // Posted by the host as a system note: one line, bounded, so it
                // can't pass for the host's own messages or flood the chat.
                let one_line = |t: &str, max: usize| -> String {
                    t.split_whitespace().collect::<Vec<_>>().join(" ").chars().filter(|c| !c.is_control()).take(max).collect()
                };
                let summary = one_line(&s("summary"), 400);
                let files: Vec<String> = strs("filesTouched").iter().take(32).map(|f| one_line(f, 256)).filter(|f| !f.is_empty()).collect();
                let confidence = Some(s("confidence")).filter(|c| matches!(c.as_str(), "low" | "medium" | "high"));
                *st.report.lock().unwrap_or_else(|e| e.into_inner()) = Some(SelfReport { status, summary, files_touched: files, confidence });
                (ToolResult::ok(json!({"ok": true})), None)
            }
            _ => (ToolResult::err(Outcome::Denied, json!({"error": "unknown_tool"})), None),
        }
    }

    /// `ligus_offer_file` (issue #118): an agent offers a file from its
    /// worktree. Checked like a person's share (policy, blocklist, type by
    /// content, size, archive bombs) and never carrying one of this device's
    /// secrets, then staged on this host and filed as a proposal. Nothing is
    /// uploaded until a person approves it.
    fn offer_file(&self, st: &TurnState, ctx: &EffectCtx, path: &str, title: &str, body: &str) -> ToolResult {
        let fail = |outcome: Outcome, m: String| ToolResult::err(outcome, json!({"error": if outcome == Outcome::Denied { "denied" } else { "failed" }, "message": m}));
        if title.is_empty() {
            return fail(Outcome::Error, "a title is required".into());
        }
        let Some(wt) = st.spec.worktree.clone() else {
            return fail(Outcome::Denied, "this turn has no worktree to offer a file from".into());
        };
        let rules = match self.svc(|svc| svc.attachment_rules(ctx.workspace_id)) {
            Ok(r) => r,
            Err(e) => return fail(Outcome::Error, e.to_string()),
        };
        if !rules.policy.enabled || !rules.policy.agents {
            return fail(Outcome::Denied, "this workspace doesn't let agents offer files".into());
        }
        // Inside the worktree only: the file is opened first (no symlink as
        // its last component) and the path of what was opened is checked,
        // so swapping a directory for a symlink between a check and the read
        // can't reach outside the worktree.
        let (bytes, name) = match crate::attachment_safety::read_inside(&wt, path, rules.policy.max_bytes) {
            Ok(x) => x,
            Err(e) => return fail(Outcome::Denied, e),
        };
        // Never let a credential leave in a file: this device's own secrets,
        // private keys and environment files.
        if let Some(why) = crate::attachment_safety::credential_in(&name, &bytes) {
            return fail(Outcome::Denied, why);
        }
        let prepared = match crate::attachment_blob::check_file(&name, bytes, &rules) {
            Ok(p) => p,
            Err(e) => return fail(Outcome::Denied, e),
        };
        let proposal_id = Uuid::new_v4();
        if let Err(e) = self.host.stage_attachment(proposal_id, &prepared.sha256, &prepared.plaintext) {
            return fail(Outcome::Denied, e);
        }
        let offer = ligus_core::attachment::AttachmentRef {
            // Bound to this host and the proposal when it is filed.
            id: Uuid::nil(),
            blob_id: String::new(),
            sha256: prepared.sha256.clone(),
            size: prepared.plaintext.len() as u64,
            content_type: prepared.content_type.clone(),
            name: prepared.name.clone(),
            message_id: None,
            proposal_id: None,
        };
        let author = ctx.author_id();
        let r = self.svc(|svc| {
            svc.as_agent(ctx.agent_id, |svc| svc.file_attachment_offer(ctx.session_id, ctx.workspace_id, title, body, &author, ctx.agent_id, offer.clone(), proposal_id))
        });
        match r {
            Ok(id) => ToolResult::ok(json!({"ok": true, "proposalId": id.to_string(), "name": offer.name, "message": "Filed for review. The file is shared only after a person approves it."})),
            Err(e) => fail(Outcome::Denied, e.to_string()),
        }
    }

    /// The workspace memory and task-state tools. Reads render every entry
    /// and slot as untrusted, nonce-delimited data
    /// (`ligus_core::memory::render_*`); the only memory write is a proposal,
    /// filed as the agent's effect; a task update writes the agent's own slot.
    fn memory_tool(&self, st: &TurnState, tool: &str, args: &Value) -> (ToolResult, Option<Uuid>) {
        use ligus_core::memory::{self as mem, MemoryOp, MemorySource, TaskStateUpdate};
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
        let strs = |k: &str| -> Vec<String> {
            args.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default()
        };
        let uuid = |k: &str| Uuid::parse_str(&s(k)).ok();
        let bad = |m: &str| ToolResult::err(Outcome::Error, json!({"error": "failed", "message": m}));
        let refused = |e: String| {
            let outcome = if e.to_ascii_lowercase().contains("unauthorized") || e.contains("agent tier") { Outcome::Denied } else { Outcome::Error };
            ToolResult::err(outcome, json!({"error": if outcome == Outcome::Denied { "denied" } else { "failed" }, "message": e}))
        };
        let ws = st.spec.workspace_id;
        let nonce = mem::fresh_nonce();
        match tool {
            "ligus_memory_search" => {
                let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10).clamp(1, mem::MAX_SEARCH_RESULTS as u64) as usize;
                let m = self.svc(|svc| svc.memory(ws).unwrap_or_default());
                let visible = self.memory_visibility(st, &m);
                let hits = mem::search_where(&m, &s("query"), &strs("tags"), limit, |e| visible(e));
                if let Err(r) = Self::charge_reads(st, hits.len() as u32) {
                    return (r, None);
                }
                let mut text = mem::memory_header(&nonce);
                for h in &hits {
                    text.push_str(&mem::render_entry(h.entry, &nonce, true));
                }
                let list: Vec<Value> = hits
                    .iter()
                    .map(|h| json!({"entryId": h.entry.id.to_string(), "rev": h.entry.head().to_string(), "score": h.score}))
                    .collect();
                (ToolResult::ok(json!({"header": READ_HEADER, "count": hits.len(), "entries": list, "content": text})), None)
            }
            "ligus_memory_read" => {
                let Some(id) = uuid("entryId") else { return (bad("entryId is an entry's id"), None) };
                let m = self.svc(|svc| svc.memory(ws).unwrap_or_default());
                let Some(e) = m.entries.get(&id).filter(|e| e.live()) else { return (bad("no such memory entry"), None) };
                if !self.memory_visibility(st, &m)(e) {
                    return (ToolResult::err(Outcome::Denied, json!({"error": "denied", "message": "that note was written under stricter egress than this turn's"})), None);
                }
                if let Err(r) = Self::charge_reads(st, 1 + e.conflicts.len() as u32) {
                    return (r, None);
                }
                let mut text = mem::memory_header(&nonce);
                text.push_str(&mem::render_entry(e, &nonce, true));
                for c in &e.conflicts {
                    let shadow = mem::MemoryEntry { id: e.id, current: c.clone(), created_by: e.created_by.clone(), ..Default::default() };
                    text.push_str(&format!("Unresolved {} (made from rev {}):\n", if c.removal { "removal" } else { "edit" }, c.base.map(|b| b.to_string()).unwrap_or_default()));
                    text.push_str(&mem::render_entry(&shadow, &nonce, !c.removal));
                }
                let source = &e.current.source;
                (
                    ToolResult::ok(json!({
                        "header": READ_HEADER,
                        "entryId": e.id.to_string(),
                        "rev": e.head().to_string(),
                        "revisions": e.revision_count,
                        "conflicts": e.conflicts.len(),
                        "sourceChatId": source.chat_id.map(|c| c.to_string()),
                        "content": text,
                    })),
                    None,
                )
            }
            "ligus_memory_propose" => {
                let entry = uuid("entryId");
                let base = uuid("base");
                let (title, body, tags) = (s("title"), args.get("body").and_then(Value::as_str).unwrap_or("").to_string(), strs("tags"));
                let op = match s("op").as_str() {
                    "create" => MemoryOp::Create { entry_id: Uuid::new_v4(), title, body, tags },
                    "edit" => match (entry, base) {
                        (Some(entry_id), Some(base)) => MemoryOp::Edit { entry_id, base, title, body, tags },
                        _ => return (bad("an edit names entryId and base (the rev you read)"), None),
                    },
                    "remove" => match (entry, base) {
                        (Some(entry_id), Some(base)) => MemoryOp::Remove { entry_id, base },
                        _ => return (bad("a removal names entryId and base (the rev you read)"), None),
                    },
                    _ => return (bad("op is create, edit or remove"), None),
                };
                if let MemoryOp::Create { title, body, tags, .. } | MemoryOp::Edit { title, body, tags, .. } = &op {
                    if let Err(why) = mem::content_ok(title, body, &mem::normalize_tags(tags)) {
                        return (bad(why), None);
                    }
                }
                let source = MemorySource { chat_id: Some(st.spec.chat_id), message_id: None, turn_id: Some(st.spec.message_id), egress: None };
                let rationale = s("rationale");
                let agent = st.spec.agent_id;
                match self.svc(|svc| svc.propose_memory_as_agent(ws, agent, op.clone(), &rationale, source.clone())) {
                    Ok(pid) => (
                        ToolResult::ok(json!({"ok": true, "proposalId": pid.to_string(), "message": "Filed for a person to accept or reject; memory doesn't change until then."})),
                        None,
                    ),
                    Err(e) => (refused(e.to_string()), None),
                }
            }
            "ligus_task_update" => {
                let Some(status) = mem::TaskStatus::parse(&s("status")) else {
                    return (bad("status is working, blocked, waiting, done or abandoned"), None);
                };
                let task_id = uuid("taskId").unwrap_or(st.spec.chat_id);
                let ttl_ms = args.get("ttlSecs").and_then(Value::as_i64).map(|x| x.clamp(60, mem::MAX_TASK_TTL_MS / 1000) * 1000);
                let update = TaskStateUpdate {
                    task_id,
                    agent_key: String::new(),
                    turn_id: st.spec.message_id,
                    status,
                    summary: s("summary"),
                    progress: args.get("progress").and_then(Value::as_u64).map(|p| p.min(100) as u8),
                    open_questions: strs("openQuestions"),
                    partial_results: strs("partialResults"),
                    ttl_ms,
                };
                if let Err(why) = mem::task_shape_ok(&TaskStateUpdate { agent_key: "x".into(), ..update.clone() }) {
                    return (bad(why), None);
                }
                let (chat, agent) = (st.spec.chat_id, st.spec.agent_id);
                match self.svc(|svc| svc.update_task_state(chat, ws, agent, update.clone())) {
                    Ok(()) => (ToolResult::ok(json!({"ok": true})), None),
                    Err(e) => (refused(e.to_string()), None),
                }
            }
            "ligus_task_state" => {
                let chat = uuid("chatId").unwrap_or(st.spec.chat_id);
                let Some(session) = self.svc(|svc| svc.load_in(ws, chat).ok().flatten()).filter(|c| c.workspace_id == ws) else {
                    return (bad("no such chat in this workspace"), None);
                };
                if !self.cross_read_allowed(st, &session) {
                    return (ToolResult::err(Outcome::Denied, json!({"error": "denied", "message": "that chat's egress is stricter than this turn's"})), None);
                }
                let now = flow::now_ms();
                let mut slots: Vec<&mem::TaskSlot> = session.flow.tasks.iter().filter(|t| !t.stale(now)).collect();
                slots.sort_by_key(|a| std::cmp::Reverse(a.at_ms));
                if let Err(r) = Self::charge_reads(st, slots.len() as u32) {
                    return (r, Some(chat));
                }
                let mut text = mem::task_header(&nonce);
                for t in &slots {
                    text.push_str(&mem::render_slot(t, &nonce));
                }
                (
                    ToolResult::ok(json!({"header": READ_HEADER, "chatId": chat.to_string(), "slots": slots.len(), "content": text})),
                    (chat != st.spec.chat_id).then_some(chat),
                )
            }
            _ => (ToolResult::err(Outcome::Denied, json!({"error": "unknown_tool"})), None),
        }
    }

    /// Which memory entries this turn may see (design R10): an entry's
    /// tier is the strictest egress it was recorded under, tightened by its
    /// source chats' egress now (a lockdown raised after the note was
    /// written still counts); the turn sees it only when its own chat's
    /// egress is at least as strict (the cross-chat read rule). An entry
    /// with no recorded tier and no source chat is the workspace default,
    /// which every turn satisfies.
    fn memory_visibility(&self, st: &TurnState, m: &ligus_core::memory::MemoryState) -> impl Fn(&ligus_core::memory::MemoryEntry) -> bool {
        use ligus_core::sandbox::{envelope_for_session, EgressPolicy};
        let ws = st.spec.workspace_id;
        let chats: std::collections::BTreeSet<Uuid> = m.live().flat_map(ligus_core::memory::source_chats).collect();
        let (reader, now) = self.svc(|svc| {
            let reader = svc.load_in(st.spec.workspace_id, st.spec.chat_id).ok().flatten().map(|c| envelope_for_session(&c).egress).unwrap_or(EgressPolicy::Open);
            let now: std::collections::BTreeMap<Uuid, EgressPolicy> = chats
                .iter()
                .filter_map(|c| svc.load_in(ws, *c).ok().flatten().filter(|s| s.workspace_id == ws).map(|s| (*c, envelope_for_session(&s).egress)))
                .collect();
            (reader, now)
        });
        move |e| {
            let mut tier = e.egress.clone().unwrap_or(EgressPolicy::Open);
            for c in ligus_core::memory::source_chats(e) {
                if let Some(x) = now.get(&c) {
                    tier = tier.stricter(x);
                }
            }
            ligus_core::memory::egress_allows(&reader, &tier)
        }
    }

    /// A read-only code-host call: the egress tier, the forge budget, the
    /// host's binding for this chat, then the read itself.
    async fn forge_tool(&self, st: &TurnState, tool: &str, args: &Value) -> ToolResult {
        let deny = |m: &str| ToolResult::err(Outcome::Denied, json!({"error": "denied", "message": m}));
        let egress = self.svc(|svc| svc.load_in(st.spec.workspace_id, st.spec.chat_id).ok().flatten()).map(|s| ligus_core::sandbox::envelope_for_session(&s).egress);
        if !egress.as_ref().is_some_and(super::forge_tools::egress_allows) {
            return deny("this chat's sandbox doesn't allow code-host reads (its egress is closed, or its allow-list doesn't name ligus.forge)");
        }
        {
            let mut c = st.counters.lock().unwrap_or_else(|e| e.into_inner());
            if c.forge_calls >= st.spec.budgets.max_forge_calls {
                return ToolResult::err(Outcome::Budget, json!({"error": "budget_exhausted", "budget": "maxForgeCalls", "message": crate::tool_loop::FINAL_ROUND_NUDGE}));
            }
            c.forge_calls += 1;
        }
        let Some(binding) = self.host.forge_for(st.spec.chat_id) else {
            return deny("this chat has no repository on a connected code host on this device");
        };
        super::forge_tools::call(&binding, tool, args).await
    }

    /// Whether this turn may read `target`'s content: its own chat always;
    /// another chat only when this turn's egress is at least as strict as
    /// that chat's. A locked-down (or allow-listed) chat's content stays
    /// where its egress is cut, so an injection or a leaked credential in it
    /// can't be carried out through a chat with open egress (D.8).
    fn cross_read_allowed(&self, st: &TurnState, target: &ChatSession) -> bool {
        if target.id == st.spec.chat_id {
            return true;
        }
        let Some(reader) = self.svc(|svc| svc.load_in(st.spec.workspace_id, st.spec.chat_id).ok().flatten()) else { return false };
        let mine = ligus_core::sandbox::envelope_for_session(&reader).egress;
        let theirs = ligus_core::sandbox::envelope_for_session(target).egress;
        mine.stricter(&theirs) == mine
    }

    /// `ligus_ask_human`: post the ask, pause the turn, wait for the first
    /// human answer (or the timeout), and return it.
    async fn ask_human(&self, st: &Arc<TurnState>, ctx: &EffectCtx, args: &Value) -> (ToolResult, Option<Uuid>) {
        let question: String = args.get("question").and_then(Value::as_str).unwrap_or("").trim().chars().take(flow::MAX_ASK_QUESTION_CHARS).collect();
        if question.is_empty() {
            return (ToolResult::err(Outcome::Error, json!({"error": "failed", "message": "a question is required"})), None);
        }
        let options: Vec<String> = args
            .get("options")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str()).map(|o| o.trim().chars().take(flow::MAX_ASK_OPTION_CHARS).collect::<String>()).filter(|o| !o.is_empty()).take(flow::MAX_ASK_OPTIONS).collect())
            .unwrap_or_default();
        let timeout = Duration::from_secs(args.get("timeoutSecs").and_then(Value::as_u64).unwrap_or(600).clamp(1, 3600));
        let posted = match post_ask(&*self.host, ctx, st.spec.message_id, question, options, timeout) {
            Ok(a) => a,
            Err(e) => return (ToolResult::err(Outcome::Denied, json!({"error": "denied", "message": e})), None),
        };
        let _pause = crate::agent_turn::pause_counter(&st.paused);
        let ask_id = posted.ask_id;
        match wait_answer(&*self.host, ctx.session_id, &posted.ask, timeout, || st.revoked.load(Ordering::SeqCst)).await {
            Some(ans) => (
                ToolResult::ok(json!({"answer": ans.answer, "optionIndex": ans.option_index, "answeredBy": ans.by})),
                None,
            ),
            None => {
                close_ask(&*self.host, ctx, st.spec.message_id, ask_id);
                (ToolResult::err(Outcome::Ok, json!({"error": "no_answer"})), None)
            }
        }
    }

    /// The turn's summary so far (content-free).
    fn summary(&self, st: &TurnState) -> TurnToolSummary {
        let c = st.counters.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut calls = c.calls;
        while calls.len() > flow::MAX_SUMMARY_TOOLS {
            let k = calls.keys().next_back().cloned().unwrap_or_default();
            calls.remove(&k);
        }
        TurnToolSummary {
            turn_id: st.spec.message_id,
            agent_key: flow::agent_key(st.spec.agent_id),
            calls,
            denied: c.denied,
            budget_exhausted: c.budget_exhausted,
            at_ms: flow::now_ms(),
        }
    }
}

fn status_str(s: ProposalStatus) -> &'static str {
    match s {
        ProposalStatus::Open => "open",
        ProposalStatus::Approved => "approved",
        ProposalStatus::Rejected => "rejected",
        ProposalStatus::Applied => "applied",
    }
}

fn list_chats(svc: &ChatService, ws: Uuid, channel: &str, limit: usize) -> Vec<Value> {
    let config_id = ligus_core::workspace_config_session_id(ws);
    let channels = svc.list_channels(ws).unwrap_or_default();
    let mut out: Vec<(i64, Value)> = Vec::new();
    for id in svc.session_ids().unwrap_or_default().into_iter().filter(|id| *id != config_id).take(2000) {
        let Ok(Some(sess)) = svc.load_in(ws, id) else { continue };
        if sess.workspace_id != ws {
            continue;
        }
        let ch = channels.iter().find(|c| c.id == sess.channel_id).map(|c| c.name.clone()).unwrap_or_default();
        if !channel.is_empty() && ch.to_lowercase() != channel {
            continue;
        }
        let last = sess.messages.iter().rev().find(|m| !m.superseded).map(|m| flow::ms(&m.created_at)).unwrap_or(0);
        let count = sess.messages.iter().filter(|m| !m.superseded).count();
        out.push((last, json!({"chatId": id.to_string(), "title": sess.title, "channel": ch, "messages": count})));
    }
    out.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    out.into_iter().take(limit).map(|(_, v)| v).collect()
}

fn read_messages(session: &ChatSession, since: Option<Uuid>, limit: usize) -> Vec<Value> {
    let start = since.and_then(|id| session.messages.iter().position(|m| m.id == id)).map(|i| i + 1).unwrap_or(0);
    let msgs: Vec<_> = session.messages[start..].iter().filter(|m| !m.is_streaming && !m.superseded).collect();
    let tail = if since.is_some() { &msgs[..msgs.len().min(limit)] } else { &msgs[msgs.len().saturating_sub(limit)..] };
    tail.iter()
        .map(|m| {
            json!({
                "id": m.id.to_string(),
                "author": m.author,
                "role": format!("{:?}", m.role).to_lowercase(),
                "body": m.body.chars().take(8000).collect::<String>(),
            })
        })
        .collect()
}

/// The files a worktree changed (staged or not, untracked included).
fn worktree_changes(wt: &std::path::Path) -> Vec<String> {
    let out = ligus_core::process_util::git().arg("-C").arg(wt).args(["status", "--porcelain", "--untracked-files=all"]).output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| l.get(3..)).map(str::to_owned).collect(),
        _ => Vec::new(),
    }
}

/// A posted ask.
pub struct PostedAsk {
    pub ask_id: Uuid,
    /// The record as posted: an answer is waited for only on this ask.
    pub ask: HumanAsk,
}

/// Post a [`HumanAsk`] as the agent (Agent tier) and tell the host.
pub fn post_ask(host: &dyn LigusHost, ctx: &EffectCtx, turn_id: Uuid, question: String, options: Vec<String>, timeout: Duration) -> Result<PostedAsk, String> {
    post_ask_as(host, ctx, turn_id, question, options, timeout, false)
}

/// [`post_ask`], marked as a permission escalation when `permission`.
pub fn post_ask_as(host: &dyn LigusHost, ctx: &EffectCtx, turn_id: Uuid, question: String, options: Vec<String>, timeout: Duration, permission: bool) -> Result<PostedAsk, String> {
    let ask = HumanAsk {
        permission,
        ask_id: Uuid::new_v4(),
        turn_id,
        agent_key: ctx.agent_key(),
        question,
        options,
        expires_at_ms: flow::now_ms() + timeout.as_millis() as i64,
        closed: false,
        at_ms: flow::now_ms(),
        ..Default::default()
    };
    let mut r = Ok(());
    host.with_service(&mut |svc| {
        r = svc.as_agent(ctx.agent_id, |svc| svc.record_flow(ctx.session_id, ctx.workspace_id, FlowRecord::HumanAsk(ask.clone()))).map_err(|e| e.to_string());
    });
    r?;
    host.ask_posted(ctx.session_id, &ask);
    Ok(PostedAsk { ask_id: ask.ask_id, ask })
}

/// Close an unanswered ask (timeout, cancel).
pub fn close_ask(host: &dyn LigusHost, ctx: &EffectCtx, turn_id: Uuid, ask_id: Uuid) {
    let close = HumanAsk { ask_id, turn_id, agent_key: ctx.agent_key(), closed: true, at_ms: flow::now_ms(), ..Default::default() };
    host.with_service(&mut |svc| {
        let _ = svc.as_agent(ctx.agent_id, |svc| svc.record_flow(ctx.session_id, ctx.workspace_id, FlowRecord::HumanAsk(close.clone())));
    });
}

/// Wait for the first answer to `ask_id` in the chat's fold (answers arrive
/// from any device through sync), until `timeout` or `cancelled()`.
///
/// The folded ask under that id must be the one this host posted (its
/// question, options and turn, signed here): someone who got a record under
/// the same id in first owns the id in the fold, and an answer to their ask
/// is never taken as an answer to this one.
pub async fn wait_answer(host: &dyn LigusHost, chat: Uuid, posted: &HumanAsk, timeout: Duration, cancelled: impl Fn() -> bool) -> Option<flow::HumanAnswered> {
    let deadline = Instant::now() + timeout;
    let ask_id = posted.ask_id;
    loop {
        let mut found = None;
        let mut foreign = false;
        host.with_service(&mut |svc| {
            let device = svc.device_id();
            let Some(st) = svc.load(chat).ok().flatten().and_then(|s| s.flow.asks.get(&ask_id).cloned()) else { return };
            let ours = st.posted_by.is_none()
                && st.host.is_none_or(|h| h == device)
                && st.ask.turn_id == posted.turn_id
                && st.ask.agent_key == posted.agent_key
                && st.ask.question == posted.question
                && st.ask.options == posted.options
                && st.ask.permission == posted.permission;
            if ours {
                found = st.answer;
            } else {
                foreign = true;
            }
        });
        if foreign {
            tracing::warn!(target: "ligus_mcp", "ask {ask_id} is held by another record; not waiting on it");
            return None;
        }
        if found.is_some() {
            return found;
        }
        if Instant::now() >= deadline || cancelled() {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// A live turn on the server. Dropping it (or [`TurnHandle::finish`])
/// revokes the credential.
pub struct TurnHandle {
    pub(crate) mcp: Arc<LigusMcp>,
    pub(crate) state: Arc<TurnState>,
    cred: String,
    pub(crate) socket: Mutex<Option<super::socket::SocketEndpoint>>,
    finished: AtomicBool,
}

impl TurnHandle {
    /// The turn's credential (`LIGUS_TURN_CRED`).
    pub fn credential(&self) -> &str {
        &self.cred
    }

    pub fn turn_id(&self) -> Uuid {
        self.state.spec.message_id
    }

    /// Call a tool in-process (tool-loop runtimes), full or folded name.
    pub async fn call(&self, tool: &str, args: &Value) -> ToolResult {
        let (name, args) = match super::tools::unfold(tool, args) {
            Ok(x) => x,
            Err(e) => return ToolResult::err(Outcome::Error, json!({"error": "failed", "message": e})),
        };
        match self.mcp.call(&self.cred, &name, &args).await {
            Ok(r) => r,
            Err(e) => ToolResult::err(Outcome::Denied, json!({"error": "denied", "message": e.to_string()})),
        }
    }

    /// What the tools did (for finalize's dedupe and the diff intent).
    pub fn effects(&self) -> McpEffectsLog {
        self.state.effects.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The agent's self-report, if it made one.
    pub fn report(&self) -> Option<SelfReport> {
        self.state.report.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Whether any Ligus tool was called.
    pub fn used(&self) -> bool {
        self.state.counters.lock().unwrap_or_else(|e| e.into_inner()).total > 0
    }

    /// The agent turn scope for adapters: the MCP attach (when a socket is
    /// serving), the pause counter, and the spend sink.
    pub fn scope(&self) -> crate::agent_turn::AgentTurnScope {
        crate::agent_turn::AgentTurnScope {
            mcp: self.socket.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|s| s.attach(&self.cred)),
            paused: self.state.paused.clone(),
            spend: Some(self.state.spend.clone()),
            ..Default::default()
        }
    }

    /// The tool executor for in-process tool loops (the four folded tools).
    pub fn executor(&self) -> InProcessTools<'_> {
        InProcessTools { turn: self }
    }

    /// End the turn: revoke the credential, stop the socket, and record the
    /// synced, content-free `TurnToolSummary` (Agent tier) when any tool was
    /// called. Returns the summary.
    pub fn finish(&self) -> TurnToolSummary {
        let summary = self.mcp.summary(&self.state);
        self.mcp.revoke(self.state.spec.message_id);
        *self.socket.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if self.finished.swap(true, Ordering::SeqCst) {
            return summary;
        }
        if !summary.calls.is_empty() || summary.denied > 0 {
            let spec = self.state.spec.clone();
            let rec = FlowRecord::TurnToolSummary(summary.clone());
            self.mcp.host.with_service(&mut |svc| {
                if let Err(e) = svc.as_agent(spec.agent_id, |svc| svc.record_flow(spec.chat_id, spec.workspace_id, rec.clone())) {
                    tracing::warn!(target: "ligus_mcp", "turn tool summary not recorded: {e}");
                }
            });
        }
        summary
    }
}

impl Drop for TurnHandle {
    fn drop(&mut self) {
        if !self.finished.load(Ordering::SeqCst) {
            self.mcp.revoke(self.state.spec.message_id);
        }
    }
}

/// [`crate::tool_loop::ToolExecutor`] over the folded Ligus tools.
pub struct InProcessTools<'a> {
    turn: &'a TurnHandle,
}

impl crate::tool_loop::ToolExecutor for InProcessTools<'_> {
    async fn call(&self, name: &str, input: &Value) -> (String, bool) {
        let r = self.turn.call(name, input).await;
        (r.text, r.is_error)
    }
}

/// Puts ACP permission escalations to the chat's humans as asks (D.2.2,
/// D.4.2): the same `HumanAsk` / `HumanAnswered` records `ligus_ask_human`
/// uses, so a person answers from the desktop or the phone.
pub struct HostAsker {
    pub host: Arc<dyn LigusHost>,
    pub ctx: EffectCtx,
    pub turn_id: Uuid,
    /// The turn whose question budgets an escalation is charged to.
    turn: Option<Arc<TurnState>>,
}

impl crate::agent_turn::HumanAsker for HostAsker {
    fn ask_grant(&self, req: crate::agent_turn::GrantRequest) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<crate::agent_turn::HumanAnswer>> + Send + '_>> {
        Box::pin(async move {
            if self.turn.as_ref().is_some_and(|st| !LigusMcp::charge_escalation(st)) {
                return None;
            }
            let ask = self.grant_card(&req)?;
            let posted = decision::post_card(&*self.host, &self.ctx, ask).ok()?;
            let outcome = decision::wait_card(&*self.host, self.ctx.session_id, &posted, req.timeout, || self.revoked()).await;
            match outcome {
                // The fold counted it (the named people, their passkey where
                // needed, the digest of this card); the host checks the
                // approver again before it acts.
                decision::CardOutcome::Answered(st) => {
                    let acted = decision::acted_on(&st).and_then(|v| v.event_id);
                    decision::close_card(&*self.host, &self.ctx, &posted.ask, acted);
                    let a = st.answer?;
                    if req.too_large || !self.answered_by_approver(&a) {
                        return None;
                    }
                    Some(crate::agent_turn::HumanAnswer { answer: a.answer, option_index: a.option_index.map(|i| i as usize) })
                }
                decision::CardOutcome::Foreign => None,
                _ => {
                    decision::close_card(&*self.host, &self.ctx, &posted.ask, None);
                    None
                }
            }
        })
    }

    fn ask(&self, question: String, options: Vec<String>, timeout: Duration) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<crate::agent_turn::HumanAnswer>> + Send + '_>> {
        Box::pin(async move {
            // It asks the chat's people like ligus_ask_human: past the
            // question budgets (the turn's maxAsks, the cascade's) it is
            // denied without asking.
            if self.turn.as_ref().is_some_and(|st| !LigusMcp::charge_escalation(st)) {
                return None;
            }
            let q: String = question.chars().take(flow::MAX_ASK_QUESTION_CHARS).collect();
            let posted = post_ask_as(&*self.host, &self.ctx, self.turn_id, q, options, timeout, true).ok()?;
            match wait_answer(&*self.host, self.ctx.session_id, &posted.ask, timeout, || self.revoked()).await {
                // A permission answer authorizes a run on this host: it counts
                // only from the host's own person or the agent's owner. Any
                // other first answer (a teammate on another device) denies.
                Some(a) if self.answered_by_approver(&a) => {
                    Some(crate::agent_turn::HumanAnswer { answer: a.answer, option_index: a.option_index.map(|i| i as usize) })
                }
                Some(_) => None,
                None => {
                    close_ask(&*self.host, &self.ctx, self.turn_id, posted.ask_id);
                    None
                }
            }
        })
    }
}

impl HostAsker {
    /// The turn was revoked (stopped or ended): a wait stops.
    fn revoked(&self) -> bool {
        self.turn.as_ref().is_some_and(|st| st.revoked.load(Ordering::SeqCst))
    }

    /// The grant card for `req` (section 6.2): Ligus's words and labels, the
    /// call's own detail, the people who may grant it (this host's person
    /// and the agent's owner), and whether their passkey is needed (the chat
    /// in lockdown, dearth mode on, or the call outside the sandbox).
    fn grant_card(&self, req: &crate::agent_turn::GrantRequest) -> Option<HumanAsk> {
        let ctx = &self.ctx;
        let mut out = None;
        self.host.with_service(&mut |svc| {
            let Some(session) = svc.load_in(ctx.workspace_id, ctx.session_id).ok().flatten() else { return };
            let mut people = vec![svc.author().id.clone()];
            if let Some(owner) = ctx.agent_id.and_then(|id| session.workspace_agents.iter().find(|a| a.id == id)).map(|a| a.owner_actor_id.clone()) {
                if !owner.is_empty() && !people.contains(&owner) {
                    people.push(owner);
                }
            }
            let guarded = !session.guards.lockdowns.is_empty() || session.guards.dearth_on().is_some();
            let presence = if guarded || req.outside_sandbox { flow::PRESENCE_REQUIRED } else { "none" };
            let labels = if req.standing { ligus_proto::ask_text::STANDING_GRANT_LABELS } else { ligus_proto::ask_text::GRANT_LABELS };
            let question = if req.standing {
                format!("{} asks permission to run this for the rest of its session.", req.agent_label)
            } else {
                format!("{} asks permission to run this once.", req.agent_label)
            };
            let now = flow::now_ms();
            out = Some(HumanAsk {
                ask_id: Uuid::new_v4(),
                turn_id: self.turn_id,
                agent_key: ctx.agent_key(),
                question,
                options: labels.iter().map(|s| s.to_string()).collect(),
                expires_at_ms: now + req.timeout.as_millis() as i64,
                at_ms: now,
                permission: true,
                v: flow::ASK_V2,
                audience: Some(flow::AskAudience::people(people)),
                effect: Some(flow::AskEffect {
                    kind: flow::EFFECT_PERMISSION.into(),
                    tool: req.tool.clone(),
                    title: ligus_proto::ask_text::cut(&ligus_proto::ask_text::clean_option(&req.title), 200),
                    detail: req.detail.clone(),
                    cwd: req.cwd.clone(),
                    host_device_id: Some(svc.device_id()),
                    presence: presence.into(),
                    standing: req.standing,
                    too_large: req.too_large,
                    ..Default::default()
                }),
                origin: req.subagent.then(|| flow::AskOrigin { via: flow::ORIGIN_SUBAGENT.into(), ..Default::default() }),
                ..Default::default()
            });
        });
        out
    }

    /// Whether the winning answer was signed by the host's own person or the
    /// asking agent's owner.
    fn answered_by_approver(&self, ans: &flow::HumanAnswered) -> bool {
        let mut ok = false;
        let ctx = &self.ctx;
        self.host.with_service(&mut |svc| {
            let Some(author) = svc.flow_answer_author(ctx.session_id, ans) else { return };
            let owner = ctx.agent_id.and_then(|id| {
                svc.load_in(ctx.workspace_id, ctx.session_id).ok().flatten().and_then(|s| s.workspace_agents.iter().find(|a| a.id == id).map(|a| a.owner_actor_id.clone()))
            });
            ok = !author.is_empty() && (author == svc.author().id || owner.as_deref() == Some(author.as_str()));
        });
        ok
    }
}

impl TurnHandle {
    /// An asker for this turn's ACP permission escalations.
    pub fn asker(&self) -> Arc<dyn crate::agent_turn::HumanAsker> {
        Arc::new(HostAsker { host: self.mcp.host.clone(), ctx: LigusMcp::ctx(&self.state), turn_id: self.state.spec.message_id, turn: Some(self.state.clone()) })
    }

    /// [`Self::scope`] with the asker set: what a host wraps a CLI turn in.
    pub fn agent_scope(&self) -> crate::agent_turn::AgentTurnScope {
        crate::agent_turn::AgentTurnScope { asker: Some(self.asker()), ..self.scope() }
    }
}

/// A tool executor that routes the Ligus tools (full or folded names) to a
/// turn and everything else to `inner` (an MCP registry executor).
pub struct WithLigus<'a, E> {
    pub turn: Option<&'a TurnHandle>,
    pub inner: Option<&'a E>,
}

impl<E: crate::tool_loop::ToolExecutor> crate::tool_loop::ToolExecutor for WithLigus<'_, E> {
    async fn call(&self, name: &str, input: &Value) -> (String, bool) {
        if super::tools::is_ligus_tool(name) {
            if let Some(t) = self.turn {
                let r = t.call(name, input).await;
                return (r.text, r.is_error);
            }
        }
        match self.inner {
            Some(e) => e.call(name, input).await,
            None => (format!("Error: unknown tool {name:?}. Use only the tools you were given."), true),
        }
    }
}

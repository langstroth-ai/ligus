//! The app side of [`hive_core::flow`]: settings, the hooks the dispatch path
//! calls (prime a turn's context, tag a finished turn, record hand-offs), and
//! the Tauri commands behind the Flow pane.
//!
//! Every hook is best effort: a flow failure is logged and never fails or
//! delays a turn.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use hive_core::flow::{self, FlowRecord, FlowState, HandoffCard, RouteDecision, RouteMode, TurnTag};
use hive_core::{ChatMessage, ChatSession, MessageRole};
use hive_runtime::provider::ChatTurn;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use uuid::Uuid;

use hive_runtime::chat_service::ChatService;

use crate::{map_err, AppState, DispatchTarget, Responder};

/// Device-local flow settings (Settings → Flow). Persisted in settings.json.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct FlowSettings {
    /// Record hand-off cards and give receivers their context parcels.
    pub handoff_cards: bool,
    /// Outcome-weighted routing of un-addressed messages: `off`, `shadow`
    /// (log what it would pick; the primary answers) or `live`.
    pub route_mode: RouteMode,
    /// Half-life of an outcome in the routing score.
    pub half_life_days: f64,
    /// Route cool-down after a severe failure (or two failures within ten
    /// minutes); 0 turns route stops off.
    pub cooldown_minutes: f64,
    /// Exploration share bounds: the router sends between these shares of
    /// decisions to an agent other than the top scorer, more when the top
    /// scorer is weak, unproven or falling.
    pub epsilon_min: f64,
    pub epsilon_max: f64,
}

impl Default for FlowSettings {
    fn default() -> Self {
        Self { handoff_cards: true, route_mode: RouteMode::Shadow, half_life_days: 14.0, cooldown_minutes: 15.0, epsilon_min: 0.05, epsilon_max: 0.30 }
    }
}

impl FlowSettings {
    pub(crate) fn score_params(&self) -> flow::score::ScoreParams {
        flow::score::ScoreParams { half_life_days: self.half_life_days.clamp(0.5, 365.0), ..Default::default() }
    }

    pub(crate) fn route_policy(&self) -> flow::route::RoutePolicy {
        let lo = self.epsilon_min.clamp(0.0, 0.5);
        flow::route::RoutePolicy { epsilon_min: lo, epsilon_max: self.epsilon_max.clamp(lo, 0.5), ..Default::default() }
    }
}

pub(crate) fn settings(state: &AppState) -> FlowSettings {
    state.settings.lock().unwrap().flow.clone()
}

// ---------------------------------------------------------------------------
// Workspace view: every chat of a workspace plus its flow config, cached until
// the event log grows.
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub(crate) struct WorkspaceView {
    pub sessions: Arc<Vec<ChatSession>>,
    pub config: Arc<FlowState>,
}

type ViewCache = HashMap<Uuid, (i64, WorkspaceView)>;

fn view_cache() -> &'static Mutex<ViewCache> {
    static CACHE: OnceLock<Mutex<ViewCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// All chats of `workspace_id` (not the config log) and the workspace's flow
/// configuration.
pub(crate) fn workspace_view(state: &AppState, workspace_id: Uuid) -> WorkspaceView {
    let svc = state.service.lock().unwrap();
    view_with(&svc, workspace_id)
}

/// [`workspace_view`] for a caller already holding the service lock.
pub(crate) fn view_with(svc: &ChatService, workspace_id: Uuid) -> WorkspaceView {
    let row = svc.store().max_row_id().unwrap_or(0);
    if let Some((at, view)) = view_cache().lock().unwrap().get(&workspace_id) {
        if *at == row {
            return view.clone();
        }
    }
    let config_id = hive_core::workspace_config_session_id(workspace_id);
    let mut sessions = Vec::new();
    for id in svc.store().list_session_ids().unwrap_or_default() {
        if id == config_id {
            continue;
        }
        if let Ok(Some(s)) = svc.load(id) {
            if s.workspace_id == workspace_id {
                sessions.push(s);
            }
        }
    }
    let config = svc.flow_config(workspace_id).unwrap_or_default();
    let view = WorkspaceView { sessions: Arc::new(sessions), config: Arc::new(config) };
    view_cache().lock().unwrap().insert(workspace_id, (row, view.clone()));
    view
}

/// Append a flow record, logging (not returning) a failure.
pub(crate) fn record(state: &AppState, session_id: Uuid, workspace_id: Uuid, record: FlowRecord) {
    let kind = record.type_str();
    if let Err(e) = state.service.lock().unwrap().record_flow(session_id, workspace_id, record) {
        tracing::warn!(target: "flow", kind, "couldn't record a flow record: {e}");
    }
}


// ---------------------------------------------------------------------------
// Routing (un-addressed messages)
// ---------------------------------------------------------------------------

/// How long a routed thread stays with its agent (thread memory).
const THREAD_MEMORY_MS: i64 = 2 * 3_600_000;
/// How far back an approved change to a named file counts as file memory.
const FILE_MEMORY_MS: i64 = 30 * 86_400_000;

/// Whether a message names its responder (an `@agent` or `@primary`). An
/// explicit choice always wins: the router never sees it.
fn addressed(body: &str, session: &ChatSession) -> bool {
    let m = hive_runtime::mentions::parse_mentions(body, session);
    m.primary || !m.agents.is_empty()
}

/// The workspace secret flow keys are made under.
pub(crate) fn key_secret(state: &AppState, workspace_id: Uuid) -> Vec<u8> {
    let key = {
        let s = state.settings.lock().unwrap();
        // Only the active room's passphrase is known here; a workspace without
        // one (relay-less) keys under its id.
        s.workspace_passphrase.as_deref().filter(|p| !p.is_empty()).map(hive_core::derive_workspace_key)
    };
    flow::keys::workspace_secret(key, workspace_id)
}

fn key_secret_cache(state: &AppState, workspace_id: Uuid) -> Vec<u8> {
    type SecretCache = Mutex<HashMap<Uuid, (Option<String>, Vec<u8>)>>;
    static CACHE: OnceLock<SecretCache> = OnceLock::new();
    let pass = state.settings.lock().unwrap().workspace_passphrase.clone();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some((p, k)) = cache.lock().unwrap().get(&workspace_id) {
        if *p == pass {
            return k.clone();
        }
    }
    let k = key_secret(state, workspace_id);
    cache.lock().unwrap().insert(workspace_id, (pass, k.clone()));
    k
}

/// The candidates the router may pick from: the primary plus the chat's agents
/// (its participants, or the whole roster when none are picked). An agent
/// sent to a host that is offline (by presence) is left out.
fn candidates(state: &AppState, svc: &ChatService, session: &ChatSession) -> Vec<flow::route::Candidate> {
    let now = hive_core::Timestamp::now();
    let hosts = svc.list_hosts(session.workspace_id).unwrap_or_default();
    let here = svc.device_id().to_string();
    let host_live = |id: &str| id.is_empty() || id == here || hosts.iter().any(|h| h.id == id && state.host_presence.is_online(h, &now));
    let mut out = vec![flow::route::Candidate { agent: flow::PRIMARY.into(), runtime_id: session.runtime_id.clone(), low_risk_only: false }];
    for a in &session.workspace_agents {
        if !session.participant_agent_ids.is_empty() && !session.participant_agent_ids.contains(&a.id) {
            continue;
        }
        if !host_live(&a.host_id) {
            continue;
        }
        out.push(flow::route::Candidate { agent: flow::agent_key(Some(a.id)), runtime_id: a.runtime_id.clone(), low_risk_only: false });
    }
    out
}

/// The agent already carrying this chat's routed thread: the router's last
/// pick here, if recent and its last turn here didn't fail.
fn thread_memory(session: &ChatSession, now_ms: i64) -> Option<String> {
    let last = session.flow.route_decisions.values().max_by_key(|d| d.at_ms)?;
    if now_ms - last.at_ms > THREAD_MEMORY_MS || last.shadow_choice == flow::PRIMARY {
        return None;
    }
    let failed = session
        .flow
        .turns
        .iter()
        .rev()
        .find(|t| t.agent == last.shadow_choice)
        .is_some_and(|t| t.error);
    (!failed).then(|| last.shadow_choice.clone())
}

/// The agent with the most recent approved change to a file the message
/// names (matched by opaque key).
fn file_memory(view: &WorkspaceView, secret: &[u8], body: &str, now_ms: i64) -> Option<String> {
    let keys: Vec<String> = flow::route::named_paths(body).iter().map(|p| flow::keys::file_key(secret, p)).collect();
    if keys.is_empty() {
        return None;
    }
    view.sessions
        .iter()
        .flat_map(|s| s.flow.touches.iter())
        .filter(|t| keys.contains(&t.file_key) && now_ms - t.at_ms <= FILE_MEMORY_MS)
        .max_by_key(|t| t.at_ms)
        .map(|t| t.agent.clone())
}

/// The outcome scores for a workspace.
pub(crate) fn score_book(view: &WorkspaceView, s: &FlowSettings, now_ms: i64) -> flow::score::ScoreBook {
    let samples = flow::score::outcomes(&view.sessions);
    let turns = flow::score::turns_spent(&view.sessions);
    flow::score::ScoreBook::new(&samples, &turns, s.score_params(), now_ms)
}

/// Decide who answers an un-addressed message. Returns the dispatch target
/// and, when this call made a new decision, the decision to record.
///
/// Every dispatch gate (`send_message`, the sync path's `plan_response`, the
/// deferred-claim fallback) goes through here, so they agree: a decision
/// already in the log for this message is honoured; otherwise the same
/// deterministic draw (seeded by the message id) is made.
pub(crate) fn route_target(
    state: &AppState,
    svc: &ChatService,
    session: &ChatSession,
    trigger_id: Uuid,
    body: &str,
    base: Option<DispatchTarget>,
) -> (Option<DispatchTarget>, Option<RouteDecision>) {
    if !matches!(base, Some(DispatchTarget::Primary)) || addressed(body, session) {
        return (base, None);
    }
    if let Some(d) = session.flow.route_decisions.get(&trigger_id) {
        return (target_of(d, session).or(base), None);
    }
    let s = settings(state);
    if s.route_mode == RouteMode::Off {
        return (base, None);
    }
    let now = flow::now_ms();
    let view = view_with(svc, session.workspace_id);
    let book = score_book(&view, &s, now);
    let cands = candidates(state, svc, session);
    let config_stops = view.config.route_stops.clone();
    let thread = thread_memory(session, now);
    let secret = key_secret_cache(state, session.workspace_id);
    let file = file_memory(&view, &secret, body, now);
    let input = flow::route::RouteInput {
        trigger_id,
        text: body,
        candidates: &cands,
        scores: &book,
        stops: &config_stops,
        thread_memory: thread.as_deref(),
        file_memory: file.as_deref(),
        mode: s.route_mode,
        policy: s.route_policy(),
        now_ms: now,
    };
    match flow::route::decide(&input) {
        Some(d) => (target_of(&d, session).or(base), Some(d)),
        None => (base, None),
    }
}

/// The dispatch target a recorded decision names (live mode only).
fn target_of(d: &RouteDecision, session: &ChatSession) -> Option<DispatchTarget> {
    if d.mode != RouteMode::Live || d.chosen == flow::PRIMARY {
        return Some(DispatchTarget::Primary);
    }
    let id = flow::agent_from_key(&d.chosen)?;
    session.workspace_agents.iter().any(|a| a.id == id).then_some(DispatchTarget::Agent(id))
}

/// [`crate::dispatch_target`] with routing applied — what every gate calls.
pub(crate) fn routed_target(
    state: &AppState,
    svc: &ChatService,
    session: &ChatSession,
    trigger_id: Uuid,
    body: &str,
    local_only: bool,
    links: &hive_core::DeviceLinks,
) -> (Option<DispatchTarget>, Option<RouteDecision>) {
    let base = crate::dispatch_target(body, session, local_only, links);
    route_target(state, svc, session, trigger_id, body, base)
}

/// Record a routing decision for `trigger_id` if the log doesn't have one
/// yet (the device that answers records it, so phone- and serve-posted
/// messages are logged too).
pub(crate) fn record_route_if_missing(state: &AppState, session_id: Uuid, trigger_id: Uuid) {
    let decision = {
        let svc = state.service.lock().unwrap();
        let Ok(Some(session)) = svc.load(session_id) else { return };
        if session.flow.route_decisions.contains_key(&trigger_id) {
            return;
        }
        let Some(m) = session.messages.iter().find(|m| m.id == trigger_id && m.role == MessageRole::User) else { return };
        let local_only = state.is_local_only_ws(session.workspace_id);
        let links = crate::device_links_of(&svc, session.workspace_id);
        routed_target(state, &svc, &session, trigger_id, &m.body.clone(), local_only, &links).1.map(|d| (session.workspace_id, d))
    };
    if let Some((wid, d)) = decision {
        record(state, session_id, wid, FlowRecord::RouteDecision(d));
    }
}

// ---------------------------------------------------------------------------
// Reply directives: stripped in `finalize_reply`, applied after the turn.
// ---------------------------------------------------------------------------

fn stash() -> &'static Mutex<HashMap<Uuid, flow::directives::FlowDirectives>> {
    static STASH: OnceLock<Mutex<HashMap<Uuid, flow::directives::FlowDirectives>>> = OnceLock::new();
    STASH.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Strip `[[handoff:]]` / `[[mark:]]` / `[[unmark:]]` directives from a reply
/// and keep them for [`after_turn`]. Returns the reply without them.
pub(crate) fn take_directives(message_id: Uuid, body: &str) -> String {
    let parsed = flow::directives::parse(body);
    let cleaned = parsed.cleaned.clone();
    if !parsed.is_empty() {
        let mut s = stash().lock().unwrap();
        // Bounded: a reply whose turn never reaches after_turn can't leak.
        if s.len() > 256 {
            s.clear();
        }
        s.insert(message_id, parsed);
    }
    cleaned
}

fn take_stashed(message_id: Uuid) -> flow::directives::FlowDirectives {
    stash().lock().unwrap().remove(&message_id).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Before a turn: prime the context.
// ---------------------------------------------------------------------------

/// Constant guidance appended to a turn's system prompt for the flow
/// directives the settings enable. Constant per settings, so the cached
/// prompt prefix is unaffected turn to turn.
pub(crate) fn with_guidance(state: &AppState, system: String) -> String {
    let s = settings(state);
    let mut parts: Vec<&str> = Vec::new();
    if s.handoff_cards {
        parts.push(HANDOFF_GUIDANCE);
    }
    if parts.is_empty() {
        system
    } else {
        format!("{system}\n\n{}", parts.join("\n"))
    }
}

const HANDOFF_GUIDANCE: &str = r#"When you hand work to another agent with an @mention, give it a compact context parcel so it starts primed instead of rereading the chat: end the reply with [[handoff: {"keyFiles": ["path", …], "decisions": ["…"], "ruledOut": ["…"], "notes": "…"}]]. Keep it short; it is stripped from the visible reply. If you receive a hand-off, its card and parcel appear under "[Ligus flow context]" in the latest message."#;

/// What [`prime_turns`] added.
#[derive(Debug, Default, Clone)]
pub(crate) struct Primed {
    pub parcel: bool,
}

/// Append the flow context block to the turn's last user message: the
/// hand-off card (and context parcel) waiting for this responder. Kept out of
/// the system prompt so the prompt prefix stays byte-stable for caching.
pub(crate) fn prime_turns(
    state: &AppState,
    session: &ChatSession,
    responder: &Responder,
    turns: &mut [ChatTurn],
) -> Primed {
    let s = settings(state);
    let me = flow::agent_key(responder.agent_id);
    let mut blocks: Vec<String> = Vec::new();
    let mut primed = Primed::default();
    if s.handoff_cards {
        if let Some(card) = session.flow.pending_handoff_for(&me) {
            primed.parcel = card.parcel.as_ref().is_some_and(|p| !p.is_empty());
            blocks.push(card.render());
            // How long the hand-off waited for its receiver.
            let wait = flow::now_ms() - card.at_ms;
            record(
                state,
                session.id,
                session.workspace_id,
                FlowRecord::EdgeWait(flow::EdgeWait {
                    edge: "handoff".into(),
                    reference: card.id.to_string(),
                    wait_ms: wait.max(0),
                    host_id: String::new(),
                    at_ms: flow::now_ms(),
                }),
            );
        }
    }
    if blocks.is_empty() {
        return primed;
    }
    if let Some(last) = turns.iter_mut().rev().find(|t| t.role == "user") {
        last.content.push_str("\n\n[Ligus flow context]\n");
        last.content.push_str(&blocks.join("\n"));
    }
    primed
}

// ---------------------------------------------------------------------------
// After a turn: tag it, record hand-offs and marks.
// ---------------------------------------------------------------------------

/// The trigger message a reply answered.
fn trigger_of<'a>(session: &'a ChatSession, reply: &ChatMessage) -> Option<&'a ChatMessage> {
    let id = reply.reply_to_message_id?;
    session.messages.iter().find(|m| m.id == id)
}

/// Tool activity observed while a turn streams (it isn't persisted on the
/// message): how many tools ran and how the last test command ended.
#[derive(Debug, Default, Clone)]
pub(crate) struct Activity {
    pub calls: u32,
    test_calls: Vec<String>,
    pub last_test: Option<bool>,
}

fn activity() -> &'static Mutex<HashMap<Uuid, Activity>> {
    static ACT: OnceLock<Mutex<HashMap<Uuid, Activity>>> = OnceLock::new();
    ACT.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Watch a turn's live tool activity. The exit status is what the tool runner
/// reported, never the agent's own account.
pub(crate) fn observe_activity(message_id: Uuid, act: &hive_runtime::provider::dispatch::StreamActivity) {
    use hive_runtime::provider::dispatch::StreamActivity;
    let mut all = activity().lock().unwrap();
    if all.len() > 256 && !all.contains_key(&message_id) {
        all.clear();
    }
    let a = all.entry(message_id).or_default();
    match act {
        StreamActivity::Tool { id, input_json, .. } => {
            a.calls += 1;
            if flow::task_class::is_test_command(input_json) {
                a.test_calls.push(id.clone());
            }
        }
        StreamActivity::ToolResult { call_id, is_error, .. } => {
            if a.test_calls.contains(call_id) {
                a.last_test = Some(!is_error);
            }
        }
        StreamActivity::Thinking { .. } => {}
    }
}

fn take_activity(message_id: Uuid) -> Activity {
    activity().lock().unwrap().remove(&message_id).unwrap_or_default()
}

/// The measured result of the last test command a turn ran, from tool calls
/// persisted on the reply (runtimes that persist them).
pub(crate) fn last_test_result(reply: &ChatMessage) -> Option<bool> {
    reply
        .tool_calls
        .iter()
        .rev()
        .find(|c| flow::task_class::is_test_command(&c.input_json))
        .and_then(|c| reply.tool_results.iter().find(|r| r.call_id == c.id))
        .map(|r| !r.is_error)
}

fn role_of(
    session: &ChatSession,
    trigger: Option<&ChatMessage>,
    class: &str,
    reply: &ChatMessage,
    filed: usize,
    tool_calls: u32,
) -> &'static str {
    let text = trigger.map(|t| t.body.as_str()).unwrap_or("");
    if text.contains("[Workflow ·") {
        flow::roles::STAGE
    } else if filed > 0 {
        flow::roles::PROPOSER
    } else if class == flow::task_class::REVIEW
        || text.contains("[Task board · unreviewed proposal]")
        || text.contains("[Task board · needs review]")
        || session.proposals.iter().any(|p| p.refinements.iter().any(|r| r.created_at.inner() >= reply.created_at.inner() && r.actor_id == reply.author))
    {
        flow::roles::REVIEWER
    } else if tool_calls > 0 || !reply.tool_calls.is_empty() {
        flow::roles::TOOL_RUNNER
    } else {
        flow::roles::CONVERSATION
    }
}

/// Facts about a finished turn, gathered by the dispatch path.
pub(crate) struct TurnFacts<'a> {
    pub responder: &'a Responder,
    pub message_id: Option<Uuid>,
    pub started_ms: i64,
    pub error: Option<&'a str>,
    pub primed: Primed,
}

/// Tag a finished turn and record what it handed on. Best effort.
pub(crate) fn after_turn(app: &AppHandle, state: &AppState, session_id: Uuid, workspace_id: Uuid, facts: TurnFacts<'_>) {
    let _ = app;
    let Some(message_id) = facts.message_id else { return };
    let session = match state.service.lock().unwrap().load(session_id) {
        Ok(Some(s)) => s,
        _ => return,
    };
    let Some(reply) = session.messages.iter().find(|m| m.id == message_id).cloned() else { return };
    let responder = facts.responder;
    let me = flow::agent_key(responder.agent_id);
    let trigger = trigger_of(&session, &reply);
    let class = trigger
        .and_then(|t| session.flow.route_decisions.get(&t.id).map(|d| d.task_class.clone()))
        .unwrap_or_else(|| flow::task_class::classify(trigger.map(|t| t.body.as_str()).unwrap_or("")).to_string());
    let filed: Vec<Uuid> = session
        .proposals
        .iter()
        .filter(|p| flow::ms(&p.created_at) >= facts.started_ms)
        .filter(|p| p.producing_agent_id == responder.agent_id || p.author_actor_id == responder.author)
        .filter(|p| !p.author_actor_id.is_empty() || p.producing_agent_id.is_some() || p.diff.is_some())
        .map(|p| p.id)
        .collect();
    let act = take_activity(message_id);
    let tests_passed = act.last_test.or_else(|| last_test_result(&reply));
    let arm = trigger
        .and_then(|t| session.flow.route_decisions.get(&t.id))
        .map(|d| match d.mode {
            flow::RouteMode::Live if d.chosen == me => flow::arms::ROUTED,
            flow::RouteMode::Live => flow::arms::DIRECT,
            _ => flow::arms::SHADOW,
        })
        .unwrap_or(flow::arms::DIRECT);
    let now = flow::now_ms();
    let tag = TurnTag {
        message_id,
        trigger_id: trigger.map(|t| t.id),
        agent: me.clone(),
        runtime_id: responder.runtime_id.clone(),
        host_id: state.local_host_id(),
        task_class: class.clone(),
        role: role_of(&session, trigger, &class, &reply, filed.len(), act.calls).to_string(),
        arm: arm.to_string(),
        upstream_arm: String::new(),
        error: facts.error.is_some(),
        error_kind: facts.error.map(|e| flow::route::classify_failure(e).as_str().to_string()).unwrap_or_default(),
        tests_passed,
        tool_calls: act.calls.max(reply.tool_calls.len() as u32),
        duration_ms: (now - facts.started_ms).max(0),
        cost_usd: 0.0,
        proposal_ids: filed.clone(),
        handoff_parcel: facts.primed.parcel,
        at_ms: now,
    };
    record(state, session_id, workspace_id, FlowRecord::TurnTagged(tag));

    // Measured progress: a test run passing after the chat's last one failed.
    if tests_passed == Some(true)
        && session.flow.turns.iter().rev().find_map(|t| t.tests_passed) == Some(false)
    {
        record(
            state,
            session_id,
            workspace_id,
            FlowRecord::Progress(flow::ProgressMark { message_id, agent: me.clone(), units: 1.0, kind: "tests_passing".into(), at_ms: now }),
        );
    }

    if let Some(err) = facts.error {
        stop_route_after_failure(state, &session, responder, &me, err, now);
    }

    let directives = take_stashed(message_id);
    if settings(state).handoff_cards && facts.error.is_none() {
        record_handoff(state, &session, &reply, &me, &class, tests_passed, &filed, &directives);
    }
}

/// Window in which a second ordinary failure stops a route.
const FAILURE_WINDOW_MS: i64 = 10 * 60_000;

/// A failed turn may pull its route out of rotation at once (a rate limit,
/// an exhausted plan window, a provider outage, an unreachable local server)
/// or after a repeat, instead of waiting for the score to drift down. A
/// provider-wide failure stops every agent on that runtime.
fn stop_route_after_failure(state: &AppState, session: &ChatSession, responder: &Responder, me: &str, err: &str, now: i64) {
    let cooldown = (settings(state).cooldown_minutes * 60_000.0) as i64;
    let kind = flow::route::classify_failure(err);
    let last = session
        .flow
        .turns
        .iter()
        .rev()
        .filter(|t| t.agent == me && t.at_ms < now)
        .find(|t| t.error)
        .map(|t| t.at_ms);
    let Some(mut stop) = flow::route::stop_after_failure(me, kind, now, last, cooldown, FAILURE_WINDOW_MS) else { return };
    stop.host_id = state.local_host_id();
    tracing::info!(target: "flow", route = %me, reason = kind.as_str(), "route stopped for a cool-down");
    if kind.is_runtime_wide() {
        let mut wide = stop.clone();
        wide.route = flow::route::runtime_route(&responder.runtime_id);
        record(state, session.id, session.workspace_id, FlowRecord::RouteStop(wide));
    }
    record(state, session.id, session.workspace_id, FlowRecord::RouteStop(stop));
}

/// Record a hand-off card when the reply hands work to another agent.
#[allow(clippy::too_many_arguments)]
fn record_handoff(
    state: &AppState,
    session: &ChatSession,
    reply: &ChatMessage,
    me: &str,
    class: &str,
    tests_passed: Option<bool>,
    filed: &[Uuid],
    directives: &flow::directives::FlowDirectives,
) {
    let Some(to) = hive_runtime::cascade_target(session, reply) else { return };
    let to_key = flow::agent_key(Some(to.id));
    if to_key == me {
        return;
    }
    let parcel = directives
        .parcels
        .iter()
        .find(|p| p.to.as_deref().is_none_or(|t| t.eq_ignore_ascii_case(&to.name)))
        .map(|p| p.parcel.clone());
    let proposal = filed.last().and_then(|id| session.proposals.iter().find(|p| p.id == *id));
    let since_human = session
        .messages
        .iter()
        .rev()
        .take_while(|m| m.role != MessageRole::User)
        .filter(|m| matches!(m.role, MessageRole::Assistant | MessageRole::Agent))
        .count() as u32;
    let card = HandoffCard {
        id: Uuid::new_v4(),
        from: me.to_string(),
        to: to_key,
        from_name: reply.author.clone(),
        to_name: to.name.clone(),
        message_id: Some(reply.id),
        task: flow::handoff::task_line(&reply.body, &to.name),
        task_class: class.to_string(),
        branch: format!("ligus/chat/{}", session.id),
        files: proposal.map(|p| p.changed_files.clone()).unwrap_or_default(),
        quality: flow::HandoffQuality {
            tests_passed,
            proposal_id: proposal.map(|p| p.id),
            proposal_status: proposal.map(|p| format!("{:?}", p.status).to_lowercase()).unwrap_or_default(),
            turns: since_human,
            cost_usd: 0.0,
            error: false,
        },
        via: "mention".into(),
        parcel,
        at_ms: flow::now_ms(),
    };
    record(state, session.id, session.workspace_id, FlowRecord::Handoff(card));
}

/// Record the hand-off a workflow stage receives from the agent stages it
/// depends on (one card per upstream agent stage that produced output).
pub(crate) fn record_stage_handoff(
    state: &AppState,
    session: &ChatSession,
    run: &hive_core::WorkflowRun,
    node: &hive_core::WorkflowNode,
    agent: Option<&hive_core::WorkspaceAgent>,
) {
    if !settings(state).handoff_cards {
        return;
    }
    let to = flow::agent_key(agent.map(|a| a.id));
    let to_name = agent.map(|a| a.name.clone()).unwrap_or_else(|| "Ligus".into());
    for dep in &node.depends_on {
        let Some(up) = run.definition.nodes.iter().find(|n| &n.id == dep) else { continue };
        let hive_core::WorkflowNodeKind::Agent { agent_id, .. } = &up.kind else { continue };
        let Some(up_state) = run.node_state(dep) else { continue };
        let from = flow::agent_key(*agent_id);
        if from == to {
            continue;
        }
        let from_name = agent_id
            .and_then(|id| session.workspace_agents.iter().find(|a| a.id == id))
            .map(|a| a.name.clone())
            .unwrap_or_else(|| "Ligus".into());
        let card = HandoffCard {
            id: Uuid::new_v4(),
            from,
            to: to.clone(),
            from_name,
            to_name: to_name.clone(),
            message_id: up_state.message_id,
            task: format!("{} → {} ({})", up.name, node.name, run.definition.name),
            task_class: flow::task_class::classify(&node.name).to_string(),
            branch: format!("ligus/chat/{}", session.id),
            files: vec![],
            quality: flow::HandoffQuality {
                error: up_state.status == hive_core::NodeRunStatus::Failed,
                turns: up_state.attempts.max(1),
                ..Default::default()
            },
            via: "stage".into(),
            parcel: None,
            at_ms: flow::now_ms(),
        };
        record(state, session.id, session.workspace_id, FlowRecord::Handoff(card));
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub(crate) fn flow_get_settings(state: State<'_, AppState>) -> FlowSettings {
    settings(&state)
}

#[tauri::command]
pub(crate) fn flow_set_settings(state: State<'_, AppState>, settings: FlowSettings) -> Result<FlowSettings, String> {
    let mut s = state.settings.lock().unwrap();
    s.flow = settings;
    crate::save_settings(&state.data_dir, &s);
    Ok(s.flow.clone())
}

/// Hand-off cards recorded in a chat, for the transcript.
#[tauri::command]
pub(crate) fn flow_handoffs(state: State<'_, AppState>, session_id: String) -> Result<Vec<HandoffCard>, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let svc = state.service.lock().unwrap();
    Ok(svc.load(sid).map_err(map_err)?.map(|s| s.flow.handoffs).unwrap_or_default())
}


/// Display names for agent keys across a workspace.
fn agent_names(view: &WorkspaceView) -> HashMap<String, String> {
    let mut out = HashMap::new();
    out.insert(flow::PRIMARY.to_string(), "Ligus (primary)".to_string());
    for s in view.sessions.iter() {
        for a in &s.workspace_agents {
            out.insert(flow::agent_key(Some(a.id)), a.name.clone());
        }
    }
    out
}

fn name_of(names: &HashMap<String, String>, key: &str) -> String {
    names.get(key).cloned().unwrap_or_else(|| key.chars().take(8).collect())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScoreRow {
    pub agent: String,
    pub agent_name: String,
    pub task_class: String,
    pub score: f64,
    pub recent: f64,
    pub weight: f64,
    pub outcomes: u32,
    pub successes: u32,
    pub turns_per_success: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DecisionRow {
    pub at_ms: i64,
    pub session_id: Uuid,
    pub task_class: String,
    pub mode: RouteMode,
    pub chosen: String,
    pub would_pick: String,
    pub preferred: String,
    pub explore: bool,
    pub reason: flow::route::RouteReason,
    pub epsilon: f64,
}

/// Outcomes per experiment arm (shadow vs routed): E2's comparison.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArmRow {
    pub arm: String,
    pub turns: u32,
    pub proposals: u32,
    pub accepted: u32,
    pub judged: u32,
    pub acceptance_rate: Option<f64>,
    pub turns_per_accepted: Option<f64>,
    pub cost_usd: f64,
    pub cost_per_accepted: Option<f64>,
    /// Share of the arm's turns that ran on a local model.
    pub local_share: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RoutingReport {
    pub mode: RouteMode,
    pub since_ms: i64,
    pub decisions: u32,
    /// Share of decisions where the router would have sent the work away from
    /// the primary (shadow) or did (live).
    pub off_primary_share: f64,
    pub scores: Vec<ScoreRow>,
    pub arms: Vec<ArmRow>,
    pub recent: Vec<DecisionRow>,
    /// Routes in cool-down now.
    pub stops: Vec<StopRow>,
    pub exploration: ExplorationRow,
}

/// Whether exploration pays: how often the router chose off the incumbent,
/// and how those turns did against the incumbent's own turns.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExplorationRow {
    pub explored: u32,
    pub share: f64,
    pub mean_epsilon: f64,
    /// Explored turns with a measured success (accepted proposal or passing tests).
    pub explored_wins: u32,
    pub explored_judged: u32,
    /// Success rate of the incumbent's turns in the same window.
    pub incumbent_rate: Option<f64>,
}

/// A turn's measured success: an accepted proposal or passing tests (None =
/// nothing judged yet).
fn turn_success(sess: &ChatSession, t: &TurnTag) -> Option<bool> {
    let mut judged = None;
    for pid in &t.proposal_ids {
        if let Some(p) = sess.proposals.iter().find(|p| p.id == *pid) {
            match p.status {
                hive_core::ProposalStatus::Approved | hive_core::ProposalStatus::Applied => return Some(true),
                hive_core::ProposalStatus::Rejected => judged = Some(false),
                _ if p.superseded_by.is_some() => judged = Some(false),
                _ => {}
            }
        }
    }
    if t.error {
        return Some(false);
    }
    t.tests_passed.or(judged)
}

fn exploration(view: &WorkspaceView, since: i64) -> ExplorationRow {
    let mut row = ExplorationRow::default();
    let (mut n, mut eps) = (0u32, 0.0);
    let (mut inc_ok, mut inc_judged) = (0u32, 0u32);
    for sess in view.sessions.iter() {
        for d in sess.flow.route_decisions.values().filter(|d| d.at_ms >= since) {
            n += 1;
            eps += d.epsilon;
            if d.explore {
                row.explored += 1;
            }
            let Some(t) = sess.flow.turns.iter().find(|t| t.trigger_id == Some(d.trigger_id) && t.agent == d.shadow_choice) else { continue };
            // Only live decisions ran the pick; shadow ones measure nothing here.
            if d.mode != RouteMode::Live {
                continue;
            }
            if let Some(ok) = turn_success(sess, t) {
                if d.explore {
                    row.explored_judged += 1;
                    row.explored_wins += u32::from(ok);
                } else if d.shadow_choice == d.preferred {
                    inc_judged += 1;
                    inc_ok += u32::from(ok);
                }
            }
        }
    }
    row.share = if n == 0 { 0.0 } else { row.explored as f64 / n as f64 };
    row.mean_epsilon = if n == 0 { 0.0 } else { eps / n as f64 };
    row.incumbent_rate = (inc_judged > 0).then(|| inc_ok as f64 / inc_judged as f64);
    row
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StopRow {
    pub route: String,
    pub name: String,
    pub reason: String,
    pub until_ms: i64,
    pub failures: u32,
}

/// Whether a runtime id runs on a local model (a local server or Ollama).
pub(crate) fn is_local_runtime(state: &AppState, runtime_id: &str) -> bool {
    state.combined_runtimes().iter().find(|r| r.id == runtime_id).is_some_and(|r| {
        r.provider_kind == hive_core::ModelProviderKind::Ollama || hive_core::endpoint::endpoint_is_local_server(&r.endpoint)
    })
}

pub(crate) fn routing_report(state: &AppState, workspace_id: Uuid, days: u32) -> RoutingReport {
    let s = settings(state);
    let now = flow::now_ms();
    let since = now - i64::from(days.max(1)) * 86_400_000;
    let view = workspace_view(state, workspace_id);
    let names = agent_names(&view);
    let book = score_book(&view, &s, now);
    let mut scores: Vec<ScoreRow> = book
        .all()
        .map(|d| ScoreRow {
            agent_name: name_of(&names, &d.agent),
            agent: d.agent.clone(),
            task_class: d.task_class.clone(),
            score: d.score,
            recent: d.recent,
            weight: d.weight,
            outcomes: d.outcomes,
            successes: d.successes,
            turns_per_success: d.turns_per_success,
        })
        .collect();
    scores.sort_by(|a, b| a.task_class.cmp(&b.task_class).then(b.score.total_cmp(&a.score)));

    let mut recent: Vec<DecisionRow> = view
        .sessions
        .iter()
        .flat_map(|sess| sess.flow.route_decisions.values().map(move |d| (sess.id, d)))
        .filter(|(_, d)| d.at_ms >= since)
        .map(|(sid, d)| DecisionRow {
            at_ms: d.at_ms,
            session_id: sid,
            task_class: d.task_class.clone(),
            mode: d.mode,
            chosen: name_of(&names, &d.chosen),
            would_pick: name_of(&names, &d.shadow_choice),
            preferred: name_of(&names, &d.preferred),
            explore: d.explore,
            reason: d.reason,
            epsilon: d.epsilon,
        })
        .collect();
    recent.sort_by_key(|r| std::cmp::Reverse(r.at_ms));
    let decisions = recent.len() as u32;
    let off_primary = view
        .sessions
        .iter()
        .flat_map(|sess| sess.flow.route_decisions.values())
        .filter(|d| d.at_ms >= since && d.shadow_choice != flow::PRIMARY)
        .count();

    // Per-arm outcomes from tagged turns and the proposals they filed.
    let mut arms: HashMap<String, ArmRow> = HashMap::new();
    let mut local_turns: HashMap<String, u32> = HashMap::new();
    for sess in view.sessions.iter() {
        for t in sess.flow.turns.iter().filter(|t| t.at_ms >= since && !t.arm.is_empty()) {
            let row = arms.entry(t.arm.clone()).or_insert_with(|| ArmRow { arm: t.arm.clone(), ..Default::default() });
            row.turns += 1;
            row.cost_usd += t.cost_usd;
            if is_local_runtime(state, &t.runtime_id) {
                *local_turns.entry(t.arm.clone()).or_default() += 1;
            }
            for pid in &t.proposal_ids {
                let Some(p) = sess.proposals.iter().find(|p| p.id == *pid) else { continue };
                row.proposals += 1;
                match p.status {
                    hive_core::ProposalStatus::Approved | hive_core::ProposalStatus::Applied => {
                        row.accepted += 1;
                        row.judged += 1;
                    }
                    hive_core::ProposalStatus::Rejected => row.judged += 1,
                    hive_core::ProposalStatus::Open if p.superseded_by.is_some() => row.judged += 1,
                    hive_core::ProposalStatus::Open => {}
                }
            }
        }
    }
    let mut arms: Vec<ArmRow> = arms
        .into_values()
        .map(|mut r| {
            r.acceptance_rate = (r.judged > 0).then(|| r.accepted as f64 / r.judged as f64);
            r.turns_per_accepted = (r.accepted > 0).then(|| r.turns as f64 / r.accepted as f64);
            r.cost_per_accepted = (r.accepted > 0 && r.cost_usd > 0.0).then(|| r.cost_usd / r.accepted as f64);
            r.local_share = local_turns.get(&r.arm).copied().unwrap_or(0) as f64 / r.turns.max(1) as f64;
            r
        })
        .collect();
    arms.sort_by(|a, b| a.arm.cmp(&b.arm));
    recent.truncate(50);
    let mut stops: Vec<StopRow> = view
        .config
        .route_stops
        .values()
        .filter(|st| st.active(now))
        .map(|st| StopRow {
            name: match st.route.strip_prefix("runtime:") {
                Some(rt) => format!("every agent on {rt}"),
                None => name_of(&names, &st.route),
            },
            route: st.route.clone(),
            reason: st.reason.as_str().to_string(),
            until_ms: st.until_ms,
            failures: st.failures,
        })
        .collect();
    stops.sort_by_key(|st| st.until_ms);
    RoutingReport {
        exploration: exploration(&view, since),
        stops,
        mode: s.route_mode,
        since_ms: since,
        decisions,
        off_primary_share: if decisions == 0 { 0.0 } else { off_primary as f64 / decisions as f64 },
        scores,
        arms,
        recent,
    }
}

/// Put a stopped route back into rotation now.
#[tauri::command]
pub(crate) fn flow_clear_route_stop(state: State<'_, AppState>, route: String) -> Result<(), String> {
    let wid = state.active_workspace_id();
    let now = flow::now_ms();
    let current = workspace_view(&state, wid).config.route_stops.get(&route).cloned();
    let Some(mut stop) = current else { return Ok(()) };
    stop.until_ms = now;
    stop.at_ms = now;
    state
        .service
        .lock()
        .unwrap()
        .record_flow(hive_core::workspace_config_session_id(wid), wid, FlowRecord::RouteStop(stop))
        .map_err(map_err)
}

/// The routing report for the active workspace (Flow pane → Routing).
#[tauri::command]
pub(crate) fn flow_routing_report(state: State<'_, AppState>, days: Option<u32>) -> RoutingReport {
    let wid = state.active_workspace_id();
    routing_report(&state, wid, days.unwrap_or(14))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::{ChatToolCall, ChatToolResult};

    #[test]
    fn reads_the_last_test_run_from_tool_results() {
        let mut m = ChatMessage::new(MessageRole::Assistant, "coder", "done");
        assert_eq!(last_test_result(&m), None);
        let call = |id: &str, input: &str| ChatToolCall { id: id.into(), name: "Bash".into(), input_json: input.into(), server_id: None };
        let res = |id: &str, err: bool| ChatToolResult { call_id: id.into(), content: String::new(), is_error: err };
        m.tool_calls = vec![call("1", r#"{"command":"cargo test"}"#), call("2", r#"{"command":"ls"}"#), call("3", r#"{"command":"cargo test -p x"}"#)];
        m.tool_results = vec![res("1", true), res("2", false), res("3", false)];
        assert_eq!(last_test_result(&m), Some(true));
        m.tool_results[2].is_error = true;
        assert_eq!(last_test_result(&m), Some(false));
    }

    #[test]
    fn observes_test_runs_in_live_activity() {
        use hive_runtime::provider::dispatch::StreamActivity;
        let id = Uuid::new_v4();
        observe_activity(id, &StreamActivity::Tool { id: "a".into(), name: "Bash".into(), input_json: r#"{"command":"npm test"}"#.into() });
        observe_activity(id, &StreamActivity::Tool { id: "b".into(), name: "Read".into(), input_json: "{}".into() });
        observe_activity(id, &StreamActivity::ToolResult { call_id: "b".into(), is_error: true, content: String::new() });
        observe_activity(id, &StreamActivity::ToolResult { call_id: "a".into(), is_error: false, content: String::new() });
        let a = take_activity(id);
        assert_eq!(a.calls, 2);
        assert_eq!(a.last_test, Some(true));
        assert_eq!(take_activity(id).calls, 0, "taken once");
    }

    #[test]
    fn directives_are_stripped_and_kept_for_after_the_turn() {
        let id = Uuid::new_v4();
        let body = "Over to you @reviewer\n[[handoff: {\"decisions\":[\"kept the API\"]}]]";
        assert_eq!(take_directives(id, body), "Over to you @reviewer");
        let kept = take_stashed(id);
        assert_eq!(kept.parcels[0].parcel.decisions, vec!["kept the API"]);
        assert!(take_stashed(id).is_empty());
        // A reply without directives passes through untouched and stashes nothing.
        let plain = Uuid::new_v4();
        assert_eq!(take_directives(plain, "hi"), "hi");
        assert!(take_stashed(plain).is_empty());
    }

    #[test]
    fn settings_default_and_decode_partially() {
        let s: FlowSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, FlowSettings::default());
        assert!(s.handoff_cards);
        // Routing ships in shadow mode.
        assert_eq!(s.route_mode, RouteMode::Shadow);
        let live: FlowSettings = serde_json::from_str(r#"{"routeMode":"live"}"#).unwrap();
        assert_eq!(live.route_mode, RouteMode::Live);
    }

    #[test]
    fn exploration_report_compares_explored_turns_with_the_incumbent() {
        let mut s = session_with(&["a", "b"]);
        let (a, b) = (flow::agent_key(Some(s.workspace_agents[0].id)), flow::agent_key(Some(s.workspace_agents[1].id)));
        let mut add = |explore: bool, pick: &str, ok: bool| {
            let trigger = Uuid::new_v4();
            s.flow.apply(&FlowRecord::RouteDecision(hive_core::flow::RouteDecision {
                trigger_id: trigger,
                task_class: "refactor".into(),
                chosen: pick.into(),
                preferred: a.clone(),
                explore,
                mode: RouteMode::Live,
                shadow_choice: pick.into(),
                reason: flow::route::RouteReason::Sampled,
                epsilon: 0.1,
                scores: Default::default(),
                excluded: vec![],
                at_ms: 100,
            }));
            s.flow.apply(&FlowRecord::TurnTagged(TurnTag {
                message_id: Uuid::new_v4(),
                trigger_id: Some(trigger),
                agent: pick.into(),
                runtime_id: String::new(),
                host_id: String::new(),
                task_class: "refactor".into(),
                role: "conversation".into(),
                arm: "routed".into(),
                upstream_arm: String::new(),
                error: false,
                error_kind: String::new(),
                tests_passed: Some(ok),
                tool_calls: 1,
                duration_ms: 0,
                cost_usd: 0.0,
                proposal_ids: vec![],
                handoff_parcel: false,
                at_ms: 101,
            }));
        };
        add(true, &b, true);
        add(false, &a, true);
        add(false, &a, false);
        add(false, &a, true);
        let view = WorkspaceView { sessions: Arc::new(vec![s]), config: Arc::new(FlowState::default()) };
        let e = exploration(&view, 0);
        assert_eq!((e.explored, e.explored_wins, e.explored_judged), (1, 1, 1));
        assert!((e.share - 0.25).abs() < 1e-9);
        assert!((e.incumbent_rate.unwrap() - 2.0 / 3.0).abs() < 1e-9);
    }

    fn session_with(agents: &[&str]) -> ChatSession {
        let mut s = ChatSession::new("t", Uuid::new_v4(), "rt");
        for a in agents {
            s.workspace_agents.push(hive_core::WorkspaceAgent::new(*a, "rt2"));
        }
        s
    }

    #[test]
    fn addressed_messages_never_route() {
        let s = session_with(&["coder"]);
        assert!(addressed("@coder fix it", &s));
        assert!(addressed("@primary fix it", &s));
        assert!(!addressed("fix it", &s));
    }

    #[test]
    fn recorded_decisions_name_the_target_only_in_live_mode() {
        let s = session_with(&["coder"]);
        let id = s.workspace_agents[0].id;
        let mut d = hive_core::flow::RouteDecision {
            trigger_id: Uuid::new_v4(),
            task_class: "refactor".into(),
            chosen: flow::agent_key(Some(id)),
            preferred: flow::agent_key(Some(id)),
            explore: false,
            mode: RouteMode::Live,
            shadow_choice: flow::agent_key(Some(id)),
            reason: flow::route::RouteReason::Sampled,
            epsilon: 0.05,
            scores: Default::default(),
            excluded: vec![],
            at_ms: 0,
        };
        assert!(matches!(target_of(&d, &s), Some(DispatchTarget::Agent(a)) if a == id));
        d.mode = RouteMode::Shadow;
        assert!(matches!(target_of(&d, &s), Some(DispatchTarget::Primary)));
        // An agent that left the roster: fall back to the base target.
        d.mode = RouteMode::Live;
        d.chosen = flow::agent_key(Some(Uuid::new_v4()));
        assert!(target_of(&d, &s).is_none());
    }

    #[test]
    fn thread_memory_keeps_a_recent_pick_unless_it_failed() {
        let mut s = session_with(&["coder"]);
        let key = flow::agent_key(Some(s.workspace_agents[0].id));
        let now = 10 * 3_600_000;
        let d = hive_core::flow::RouteDecision {
            trigger_id: Uuid::new_v4(),
            task_class: "refactor".into(),
            chosen: key.clone(),
            preferred: key.clone(),
            explore: false,
            mode: RouteMode::Live,
            shadow_choice: key.clone(),
            reason: flow::route::RouteReason::Sampled,
            epsilon: 0.05,
            scores: Default::default(),
            excluded: vec![],
            at_ms: now - 60_000,
        };
        s.flow.apply(&FlowRecord::RouteDecision(d.clone()));
        assert_eq!(thread_memory(&s, now), Some(key.clone()));
        assert_eq!(thread_memory(&s, now + 3 * 3_600_000), None, "stale");
        let mut tag = TurnTag {
            message_id: Uuid::new_v4(),
            trigger_id: None,
            agent: key.clone(),
            runtime_id: String::new(),
            host_id: String::new(),
            task_class: "refactor".into(),
            role: "conversation".into(),
            arm: "routed".into(),
            upstream_arm: String::new(),
            error: true,
            error_kind: "timeout".into(),
            tests_passed: None,
            tool_calls: 0,
            duration_ms: 0,
            cost_usd: 0.0,
            proposal_ids: vec![],
            handoff_parcel: false,
            at_ms: now,
        };
        s.flow.apply(&FlowRecord::TurnTagged(tag.clone()));
        assert_eq!(thread_memory(&s, now), None, "its last turn failed");
        tag.message_id = Uuid::new_v4();
        tag.error = false;
        s.flow.apply(&FlowRecord::TurnTagged(tag));
        assert_eq!(thread_memory(&s, now), Some(key));
    }
}

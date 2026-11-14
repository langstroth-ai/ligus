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
use tauri::{AppHandle, Emitter, State};
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
    /// Tell agents feeding a full review queue to stop opening new work:
    /// `on`, `off`, or `ab` (half the chats, split by chat id, for comparing).
    pub stop_upstream: String,
    /// Open proposals at the review gate above which it is over its limit.
    pub queue_limit: u32,
    /// A wait longer than this counts as a long wait.
    pub long_wait_minutes: f64,
    /// Opt-in: when the gate backs up, notify members who could review.
    pub recruit_approvers: bool,
    /// Opt-in: let this device take a workflow stage queued on a busy or
    /// offline host, when it can run that agent's runtime.
    pub offer_idle_host: bool,
    /// Show active workspace marks in every agent turn's context, and teach
    /// agents the mark directives.
    pub marks_in_context: bool,
    /// Opt-in: agents pick up task-board work on their own when an item's
    /// stimulus crosses their threshold (otherwise the board only suggests).
    pub board_auto_pickup: bool,
    /// Keep a finished turn's worktree (with its build output and dependency
    /// caches) for the chat's next turn instead of building a fresh one.
    pub reuse_worktrees: bool,
}

impl Default for FlowSettings {
    fn default() -> Self {
        Self {
            handoff_cards: true,
            route_mode: RouteMode::Shadow,
            half_life_days: 14.0,
            cooldown_minutes: 15.0,
            epsilon_min: 0.05,
            epsilon_max: 0.30,
            stop_upstream: "on".into(),
            queue_limit: 5,
            long_wait_minutes: 120.0,
            recruit_approvers: false,
            offer_idle_host: false,
            marks_in_context: true,
            board_auto_pickup: false,
            reuse_worktrees: true,
        }
    }
}

impl FlowSettings {
    pub(crate) fn score_params(&self) -> flow::score::ScoreParams {
        flow::score::ScoreParams { half_life_days: self.half_life_days.clamp(0.5, 365.0), ..Default::default() }
    }

    pub(crate) fn tremble_policy(&self) -> flow::tremble::TremblePolicy {
        flow::tremble::TremblePolicy {
            queue_limit: self.queue_limit.max(1),
            long_wait_ms: (self.long_wait_minutes.max(1.0) * 60_000.0) as i64,
        }
    }

    /// Whether the stop-upstream note applies in `session_id`, and the A/B
    /// arm to tag the turn with (empty when not splitting).
    pub(crate) fn upstream_note(&self, session_id: Uuid) -> (bool, &'static str) {
        match self.stop_upstream.as_str() {
            "off" => (false, ""),
            "ab" => {
                let arm = flow::tremble::upstream_arm(session_id);
                (arm == "note", arm)
            }
            _ => (true, ""),
        }
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

/// Why the router must not send work to an agent on `provider`, if it can't:
/// the chat's sandbox envelope refuses that runtime (egress restricted under
/// dearth mode or a lockdown, and it can't be confined), or the runtime's plan
/// window is past this agent's hand-off threshold (routed work is a hand-off,
/// so it backs off before a person's own requests do). The primary is never
/// filtered: it answers when no one else may.
pub(crate) fn route_block(
    provider: hive_core::ModelProviderKind,
    envelope: &hive_core::sandbox::SandboxEnvelope,
    window: Option<&hive_core::throttle::PlanWindow>,
    agent_key: &str,
    now_secs: i64,
) -> Option<&'static str> {
    if hive_runtime::sandbox::enforce(provider, envelope, &Vec::new()).refuse.is_some() {
        return Some("envelope");
    }
    let d = hive_core::throttle::ThrottlePolicy::default().decide(
        hive_core::throttle::TurnPriority::Workflow,
        agent_key,
        window,
        now_secs,
    );
    matches!(d, hive_core::throttle::ThrottleDecision::Defer { .. }).then_some("throttled")
}

/// The candidates the router may pick from: the primary plus the chat's agents
/// (its participants, or the whole roster when none are picked). An agent
/// sent to a host that is offline (by presence), on a runtime the chat's
/// sandbox envelope refuses, or on a throttled runtime is left out (see
/// [`route_block`]).
fn candidates(state: &AppState, svc: &ChatService, session: &ChatSession, view: &WorkspaceView) -> Vec<flow::route::Candidate> {
    let careers = careers(view);
    let now = hive_core::Timestamp::now();
    let hosts = svc.list_hosts(session.workspace_id).unwrap_or_default();
    let here = svc.device_id().to_string();
    let host_live = |id: &str| id.is_empty() || id == here || hosts.iter().any(|h| h.id == id && state.host_presence.is_online(h, &now));
    let envelope = crate::guards::turn_envelope(session);
    let windows = state.guards.plan_windows.lock().unwrap().clone();
    let now_secs = flow::now_ms() / 1000;
    let mut out = vec![flow::route::Candidate { agent: flow::PRIMARY.into(), runtime_id: session.runtime_id.clone(), low_risk_only: false }];
    for a in &session.workspace_agents {
        if !session.participant_agent_ids.is_empty() && !session.participant_agent_ids.contains(&a.id) {
            continue;
        }
        if !host_live(&a.host_id) {
            continue;
        }
        let key = flow::agent_key(Some(a.id));
        let provider = state.resolve_runtime(&a.runtime_id).provider;
        if route_block(provider, &envelope, windows.get(&provider), &key, now_secs).is_some() {
            continue;
        }
        // A new agent on probation only takes low-risk classes until it has a record.
        let low_risk_only = careers.get(&key).is_some_and(|(c, _)| *c == flow::Career::Probation);
        out.push(flow::route::Candidate { agent: key, runtime_id: a.runtime_id.clone(), low_risk_only });
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
    let cands = candidates(state, svc, session, &view);
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
// Careers and roster diversity
// ---------------------------------------------------------------------------

/// Agents created before careers existed start graduated, so upgrading
/// doesn't put an established roster on probation. Fixed, so every device
/// agrees.
const CAREERS_SINCE_UNIX: i64 = 1_794_614_400; // 2026-11-14 00:00 UTC

/// Every agent on the workspace roster (union across chats).
fn roster(view: &WorkspaceView) -> Vec<hive_core::WorkspaceAgent> {
    let mut out: Vec<hive_core::WorkspaceAgent> = Vec::new();
    for s in view.sessions.iter() {
        for a in &s.workspace_agents {
            if !out.iter().any(|x| x.id == a.id) {
                out.push(a.clone());
            }
        }
    }
    out
}

/// Career per agent key (the primary is always a proposer).
pub(crate) fn careers(view: &WorkspaceView) -> HashMap<String, (flow::Career, &'static str)> {
    let samples = flow::score::outcomes(&view.sessions);
    let agents = roster(view);
    let inputs: Vec<flow::career::CareerInput<'_>> = std::iter::once(flow::career::CareerInput {
        agent: flow::PRIMARY.into(),
        profile: None,
        grandfathered: true,
    })
    .chain(agents.iter().map(|a| flow::career::CareerInput {
        agent: flow::agent_key(Some(a.id)),
        profile: view.config.profiles.get(&a.id),
        grandfathered: a.created_at.inner().unix_timestamp() < CAREERS_SINCE_UNIX,
    }))
    .collect();
    let queue = flow::tremble::gate_queue(&view.sessions, &Default::default(), flow::now_ms()).open as usize;
    flow::career::careers(&inputs, &samples, queue, &Default::default())
        .into_iter()
        .map(|(k, c, why)| (k, (c, why)))
        .collect()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RosterRow {
    pub agent_id: Option<Uuid>,
    pub name: String,
    pub runtime_id: String,
    pub career: flow::Career,
    pub why: String,
    pub judged: u32,
    pub approved: u32,
    pub profile: Option<flow::AgentFlowProfile>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RosterReport {
    pub agents: Vec<RosterRow>,
    pub diversity: flow::career::Diversity,
}

pub(crate) fn roster_report(state: &AppState, workspace_id: Uuid) -> RosterReport {
    let view = workspace_view(state, workspace_id);
    let samples = flow::score::outcomes(&view.sessions);
    let careers = careers(&view);
    let runtimes = state.combined_runtimes();
    let provider = |rt: &str| {
        runtimes.iter().find(|r| r.id == rt).map(|r| format!("{:?}", r.provider_kind)).unwrap_or_else(|| rt.to_string())
    };
    let agents = roster(&view);
    let mut rows = Vec::new();
    for a in &agents {
        let key = flow::agent_key(Some(a.id));
        let (c, why) = careers.get(&key).copied().unwrap_or((flow::Career::Proposer, "track record"));
        let (judged, approved) = flow::career::track_record(&samples, &key);
        rows.push(RosterRow {
            agent_id: Some(a.id),
            name: a.name.clone(),
            runtime_id: a.runtime_id.clone(),
            career: c,
            why: why.to_string(),
            judged,
            approved,
            profile: view.config.profiles.get(&a.id).cloned(),
        });
    }
    let pairs: Vec<(String, String)> = agents.iter().map(|a| (a.runtime_id.clone(), provider(&a.runtime_id))).collect();
    RosterReport { agents: rows, diversity: flow::career::diversity(&pairs) }
}

/// Careers, track records and roster diversity (Flow → Roster).
#[tauri::command]
pub(crate) fn flow_roster(state: State<'_, AppState>) -> RosterReport {
    let wid = state.active_workspace_id();
    roster_report(&state, wid)
}

/// Pin an agent's career stage (or `None` for automatic), and set its
/// task-board threshold bias and opt-out.
#[tauri::command]
pub(crate) fn flow_set_agent_profile(
    state: State<'_, AppState>,
    agent_id: String,
    career: Option<flow::Career>,
    threshold_bias: f64,
    board_opt_out: bool,
) -> Result<(), String> {
    let id = Uuid::parse_str(&agent_id).map_err(map_err)?;
    let wid = state.active_workspace_id();
    let profile = flow::AgentFlowProfile {
        agent_id: id,
        career,
        threshold_bias: threshold_bias.clamp(-3.0, 3.0),
        board_opt_out,
        at_ms: flow::now_ms(),
    };
    state
        .service
        .lock()
        .unwrap()
        .record_flow(hive_core::workspace_config_session_id(wid), wid, FlowRecord::AgentProfile(profile))
        .map_err(map_err)
}

// ---------------------------------------------------------------------------
// Threshold task board
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BoardRow {
    #[serde(flatten)]
    pub item: flow::BoardItem,
    pub chat_title: String,
    /// Who would pick it up now (threshold crossed by the most), if anyone.
    pub suggested: Option<String>,
    pub suggested_name: Option<String>,
    /// Lowest threshold among free agents: the stimulus it still needs.
    pub nearest_threshold: Option<f64>,
}

struct BoardAgent {
    key: String,
    name: String,
    agent: Option<hive_core::WorkspaceAgent>,
}

fn board_agents(view: &WorkspaceView) -> Vec<BoardAgent> {
    std::iter::once(BoardAgent { key: flow::PRIMARY.into(), name: "primary".into(), agent: None })
        .chain(roster(view).into_iter().map(|a| BoardAgent { key: flow::agent_key(Some(a.id)), name: a.name.clone(), agent: Some(a) }))
        .collect()
}

/// Agents with an unanswered mention, or in a route cool-down, are busy.
fn busy_agents(view: &WorkspaceView, now: i64) -> std::collections::HashSet<String> {
    let mut out: std::collections::HashSet<String> = view
        .sessions
        .iter()
        .flat_map(|s| hive_runtime::pending_mentions(s).into_iter().map(|p| flow::agent_key(Some(p.agent_id))))
        .collect();
    for (k, st) in &view.config.route_stops {
        if st.active(now) {
            out.insert(k.clone());
        }
    }
    out
}

/// The open items with who would pick each up.
pub(crate) fn board(view: &WorkspaceView, now: i64) -> Vec<BoardRow> {
    let marks: Vec<&flow::WorkspaceMark> = view.config.marks.values().collect();
    let items = flow::board::open_items(&view.sessions, &marks, now);
    let careers = careers(view);
    let busy = busy_agents(view, now);
    let agents = board_agents(view);
    let pickers: Vec<flow::board::Picker<'_>> = agents
        .iter()
        .map(|a| flow::board::Picker {
            agent: a.key.clone(),
            name: a.name.clone(),
            career: careers.get(&a.key).map(|c| c.0).unwrap_or(flow::Career::Proposer),
            profile: a.agent.as_ref().and_then(|x| view.config.profiles.get(&x.id)),
            busy: busy.contains(&a.key),
        })
        .collect();
    items
        .into_iter()
        .map(|item| {
            let core = flow::zones::review_bar(&view.config.zones, &item.files).is_some_and(|(_, _, k)| k == flow::ZoneKind::Core);
            let author = item
                .proposal_id
                .and_then(|pid| view.sessions.iter().flat_map(|s| s.proposals.iter()).find(|p| p.id == pid))
                .map(|p| p.producing_agent_id.map(|id| flow::agent_key(Some(id))).unwrap_or_else(|| p.author_actor_id.clone()));
            let pick = if item.claimed_by.is_empty() { flow::board::pick(&item, &pickers, author.as_deref(), core) } else { None };
            let nearest = pickers
                .iter()
                .filter(|p| !p.busy)
                .map(|p| flow::board::threshold(&p.agent, p.career, item.kind, p.profile, core))
                .filter(|t| t.is_finite())
                .min_by(f64::total_cmp);
            BoardRow {
                chat_title: view.sessions.iter().find(|s| s.id == item.session_id).map(|s| s.title.clone()).unwrap_or_default(),
                suggested: pick.map(|p| p.agent.clone()),
                suggested_name: pick.map(|p| p.name.clone()),
                nearest_threshold: nearest,
                item,
            }
        })
        .collect()
}

/// Open, unassigned work (Flow → Board).
#[tauri::command]
pub(crate) fn flow_board(state: State<'_, AppState>) -> Vec<BoardRow> {
    let wid = state.active_workspace_id();
    board(&workspace_view(&state, wid), flow::now_ms())
}

/// Hand a board item to an agent: claim it with a mark and post the task in
/// the item's chat addressed to the agent, which runs through the normal
/// dispatch on whichever device or host runs that agent.
pub(crate) async fn pickup(app: &AppHandle, state: &State<'_, AppState>, item_id: &str, agent: Option<String>) -> Result<(), String> {
    let wid = state.active_workspace_id();
    let now = flow::now_ms();
    let view = workspace_view(state, wid);
    let rows = board(&view, now);
    let row = rows.into_iter().find(|r| r.item.id == item_id).ok_or("that item is no longer open")?;
    if !row.item.claimed_by.is_empty() {
        return Err(format!("{} already took it", row.item.claimed_by));
    }
    let key = agent.or(row.suggested).ok_or("no agent's threshold is crossed yet — pick one")?;
    let agents = board_agents(&view);
    let who = agents.iter().find(|a| a.key == key).ok_or("unknown agent")?;
    let sid = if row.item.session_id.is_nil() {
        view.sessions.iter().filter(|s| !s.archived).max_by_key(|s| s.last_activity_at()).map(|s| s.id).ok_or("no chat to post in")?
    } else {
        row.item.session_id
    };
    let mark = flow::WorkspaceMark {
        id: Uuid::new_v4(),
        kind: flow::MarkKind::Claim,
        path: String::new(),
        lines: None,
        task_id: row.item.id.clone(),
        note: row.item.title.clone(),
        agent: key.clone(),
        by: who.name.clone(),
        session_id: Some(sid),
        at_ms: now,
        expires_ms: flow::MarkKind::Claim.default_ttl_ms().map(|t| now + t),
    };
    let posted = {
        let mut svc = state.service.lock().unwrap();
        svc.record_flow(sid, wid, FlowRecord::Mark(mark)).map_err(map_err)?;
        svc.post_user_message(sid, wid, flow::board::pickup_message(&row.item, &who.name)).map_err(map_err)?
    };
    let _ = app.emit("workspace://synced", 1);
    crate::respond_once(app, state, sid, Some(posted.id), true).await
}

#[tauri::command]
pub(crate) async fn flow_pickup(app: AppHandle, state: State<'_, AppState>, item_id: String, agent: Option<String>) -> Result<(), String> {
    pickup(&app, &state, &item_id, agent).await
}

/// Opt-in auto pickup: hand at most one crossed item per tick to its picker,
/// when that agent runs on this device and the item's chat is free.
async fn auto_pickup(app: &AppHandle, state: &State<'_, AppState>) {
    if !settings(state).board_auto_pickup {
        return;
    }
    let wid = state.active_workspace_id();
    let view = workspace_view(state, wid);
    let here = state.local_host_id();
    let me = state.local_actor_id();
    let local_only = state.is_local_only_ws(wid);
    let busy_chats = state.responding.lock().unwrap().clone();
    let agents = board_agents(&view);
    let runs_here = |key: &str| {
        agents.iter().find(|a| a.key == key).is_some_and(|a| match &a.agent {
            None => true,
            Some(x) => local_only || x.host_id == here || (x.host_id.is_empty() && (x.owner_actor_id.is_empty() || x.owner_actor_id == me)),
        })
    };
    let Some(row) = board(&view, flow::now_ms())
        .into_iter()
        .find(|r| r.item.claimed_by.is_empty() && r.suggested.as_deref().is_some_and(runs_here) && !busy_chats.contains(&r.item.session_id))
    else {
        return;
    };
    if let Err(e) = pickup(app, state, &row.item.id, row.suggested.clone()).await {
        tracing::info!(target: "flow", item = %row.item.id, "auto pickup skipped: {e}");
    }
}

// ---------------------------------------------------------------------------
// Worktree reuse
// ---------------------------------------------------------------------------

/// Cut a turn's isolated worktree, reusing a parked one of the same chat when
/// the setting is on.
pub(crate) fn cut_turn_worktree(
    state: &AppState,
    repo_root: &std::path::Path,
    chat_id: &str,
    turn_id: &str,
) -> std::io::Result<hive_runtime::git_worktree::Worktree> {
    if settings(state).reuse_worktrees {
        hive_runtime::git_worktree::cut_turn_worktree_reusing(repo_root, chat_id, turn_id)
    } else {
        hive_runtime::git_worktree::cut_turn_worktree(repo_root, chat_id, turn_id)
    }
}

/// Done with a turn's worktree (its diff is captured): park it for the chat's
/// next turn, or remove it.
pub(crate) fn release_worktree(state: &AppState, wt: hive_runtime::git_worktree::Worktree, session_id: Uuid) {
    let result = if settings(state).reuse_worktrees { wt.park(&session_id.to_string()) } else { wt.remove() };
    if let Err(e) = result {
        tracing::warn!(target: "worktree", "couldn't release a turn worktree: {e}");
    }
}

// ---------------------------------------------------------------------------
// Home beacon
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HomeReport {
    pub beacon: Option<flow::HomeBeacon>,
    /// This device's view, to compare against the beacon.
    pub local_relay_url: String,
    pub local_base_branch: String,
    pub local_host_id: String,
    pub coordinator_label: String,
    pub coordinator_online: Option<bool>,
    /// Where this device differs from home.
    pub mismatches: Vec<String>,
}

fn local_base_branch(state: &AppState) -> String {
    let root = state.workspace_repo_root();
    if root.trim().is_empty() || !hive_runtime::git_worktree::is_git_repo(std::path::Path::new(&root)) {
        return String::new();
    }
    let base = hive_runtime::git_worktree::chat_base(std::path::Path::new(&root));
    if base != "HEAD" {
        return base;
    }
    hive_core::process_util::git()
        .arg("-C")
        .arg(&root)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn local_relay(state: &AppState) -> String {
    state.settings.lock().unwrap().relay_url.clone().unwrap_or_default().trim().trim_end_matches('/').to_string()
}

/// Differences between a device's settings and the beacon.
pub(crate) fn beacon_mismatches(beacon: &flow::HomeBeacon, relay: &str, base: &str) -> Vec<String> {
    let mut out = Vec::new();
    let home_relay = beacon.relay_url.trim().trim_end_matches('/');
    if home_relay != relay {
        out.push(if relay.is_empty() {
            format!("This device has no relay; home syncs through {home_relay}.")
        } else if home_relay.is_empty() {
            "Home has no relay, but this device syncs through one.".to_string()
        } else {
            format!("This device syncs through {relay}; home is {home_relay}.")
        });
    }
    if !beacon.base_branch.is_empty() && !base.is_empty() && beacon.base_branch != base {
        out.push(format!("Chats here start from {base}; home starts them from {}.", beacon.base_branch));
    }
    out
}

pub(crate) fn home_report(state: &AppState, workspace_id: Uuid) -> HomeReport {
    let (beacon, hosts) = {
        let svc = state.service.lock().unwrap();
        (svc.flow_config(workspace_id).unwrap_or_default().beacon, svc.list_hosts(workspace_id).unwrap_or_default())
    };
    let relay = local_relay(state);
    let base = local_base_branch(state);
    let now = hive_core::Timestamp::now();
    let (coordinator_label, coordinator_online) = match beacon.as_ref().filter(|b| !b.coordinator_host_id.is_empty()) {
        Some(b) => match hosts.iter().find(|h| h.id == b.coordinator_host_id) {
            Some(h) => (h.label.clone(), Some(state.host_presence.is_online(h, &now))),
            None => (b.coordinator_host_id.chars().take(8).collect(), None),
        },
        None => (String::new(), None),
    };
    HomeReport {
        mismatches: beacon.as_ref().map(|b| beacon_mismatches(b, &relay, &base)).unwrap_or_default(),
        beacon,
        local_relay_url: relay,
        local_base_branch: base,
        local_host_id: state.local_host_id(),
        coordinator_label,
        coordinator_online,
    }
}

/// The workspace's home record and how this device compares (Flow → Home).
#[tauri::command]
pub(crate) fn flow_home(state: State<'_, AppState>) -> HomeReport {
    let wid = state.active_workspace_id();
    home_report(&state, wid)
}

/// Publish this device's relay, base branch and host as the workspace's home
/// (admins only). Never carries a token or passphrase.
#[tauri::command]
pub(crate) fn flow_publish_beacon(state: State<'_, AppState>, note: String, coordinator: bool) -> Result<(), String> {
    let wid = state.active_workspace_id();
    let previous = state.service.lock().unwrap().flow_config(wid).map_err(map_err)?.beacon;
    let beacon = flow::HomeBeacon {
        relay_url: local_relay(&state),
        base_branch: local_base_branch(&state),
        coordinator_host_id: if coordinator {
            state.local_host_id()
        } else {
            previous.map(|b| b.coordinator_host_id).unwrap_or_default()
        },
        note: note.trim().chars().take(200).collect(),
        updated_by: state.service.lock().unwrap().author().display_name.clone(),
        at_ms: flow::now_ms(),
    };
    state
        .service
        .lock()
        .unwrap()
        .record_flow(hive_core::workspace_config_session_id(wid), wid, FlowRecord::Beacon(beacon))
        .map_err(map_err)
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
    if s.marks_in_context {
        parts.push(MARK_GUIDANCE);
    }
    if parts.is_empty() {
        system
    } else {
        format!("{system}\n\n{}", parts.join("\n"))
    }
}

const HANDOFF_GUIDANCE: &str = r#"When you hand work to another agent with an @mention, give it a compact context parcel so it starts primed instead of rereading the chat: end the reply with [[handoff: {"keyFiles": ["path", …], "decisions": ["…"], "ruledOut": ["…"], "notes": "…"}]]. Keep it short; it is stripped from the visible reply. If you receive a hand-off, its card and parcel appear under "[Ligus flow context]" in the latest message."#;

const MARK_GUIDANCE: &str = r#"Coordinate through workspace marks rather than mentions when you only need to signal: before working on a file, claim it with [[mark: {"kind": "claim", "path": "src/x.rs", "note": "…"}]]; lock a file or range you are mid-way through with [[mark: {"kind": "lock", "path": "src/x.rs", "lines": [10, 40]}]]; flag something for a reviewer with [[mark: {"kind": "review", "path": "…", "note": "…"}]]; clear yours with [[unmark: {"path": "src/x.rs"}]]. Respect other agents' locks and don't redo claimed work. Active marks appear under "[Ligus flow context]"."#;

/// The most marks shown in an agent's context.
const MARKS_IN_CONTEXT: usize = 15;

/// What [`prime_turns`] added.
#[derive(Debug, Default, Clone)]
pub(crate) struct Primed {
    pub parcel: bool,
    /// The stop-upstream note was in the context.
    pub upstream_note: bool,
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
    if s.marks_in_context {
        let config = state.service.lock().unwrap().flow_config(session.workspace_id).unwrap_or_default();
        if let Some(block) = flow::marks::context_block(config.marks.values(), flow::now_ms(), MARKS_IN_CONTEXT) {
            blocks.push(block);
        }
    }
    let (note_on, _) = s.upstream_note(session.id);
    if note_on {
        let view = workspace_view(state, session.workspace_id);
        let gate = flow::tremble::gate_queue(&view.sessions, &s.tremble_policy(), flow::now_ms());
        if gate.over_limit {
            blocks.push(flow::tremble::stop_upstream_note(&gate));
            primed.upstream_note = true;
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
        // Plan-window signals feed the throttles (guards), not flow's
        // per-turn tool facts.
        StreamActivity::Thinking { .. } | StreamActivity::RateLimit { .. } => {}
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
        upstream_arm: settings(state).upstream_note(session_id).1.to_string(),
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
    if !filed.is_empty() {
        backpressure_after_filing(state, &session, &reply.author, now);
    }

    let directives = take_stashed(message_id);
    apply_mark_directives(state, &session, &reply.author, &me, &directives, now);
    if settings(state).handoff_cards && facts.error.is_none() {
        record_handoff(state, &session, &reply, &me, &class, tests_passed, &filed, &directives);
    }
}

/// How often the same backpressure response may fire in a workspace.
const RECRUIT_EVERY_MS: i64 = 3_600_000;

fn recently_fired(view: &WorkspaceView, action: &str, now: i64) -> bool {
    view.sessions
        .iter()
        .flat_map(|s| s.flow.recruits.iter())
        .any(|r| r.action == action && now - r.at_ms < RECRUIT_EVERY_MS)
}

/// After a turn filed a proposal: if the review gate is backed up, post the
/// stop-upstream note (once an hour) and, if opted in, ask more eligible
/// reviewers. Both are recorded as recruit notices.
fn backpressure_after_filing(state: &AppState, session: &ChatSession, author: &str, now: i64) {
    let s = settings(state);
    let view = workspace_view(state, session.workspace_id);
    let policy = s.tremble_policy();
    let gate = flow::tremble::gate_queue(&view.sessions, &policy, now);
    let (note_on, _) = s.upstream_note(session.id);
    if gate.over_limit && note_on && !recently_fired(&view, "stop_upstream", now) {
        let _ = state.service.lock().unwrap().post_system_note(
            session.id,
            session.workspace_id,
            format!("\u{1f6a6} {author} filed into a full review queue. {}", flow::tremble::stop_upstream_note(&gate)),
        );
        record(state, session.id, session.workspace_id, FlowRecord::Recruit(recruit("stop_upstream", &gate, now)));
    }
    let backed_up = gate.over_limit || gate.oldest_wait_ms > policy.long_wait_ms;
    if s.recruit_approvers && backed_up && !recently_fired(&view, "approvers", now) {
        let mut r = recruit("approvers", &gate, now);
        r.member_ids = flow::tremble::eligible_approvers(&view.sessions);
        if !r.member_ids.is_empty() {
            record(state, session.id, session.workspace_id, FlowRecord::Recruit(r));
        }
    }
}

fn recruit(action: &str, gate: &flow::tremble::GateQueue, now: i64) -> flow::RecruitNotice {
    flow::RecruitNotice {
        id: Uuid::new_v4(),
        action: action.into(),
        queue_len: gate.open,
        oldest_wait_ms: gate.oldest_wait_ms,
        member_ids: vec![],
        to_host: String::new(),
        from_host: String::new(),
        reference: String::new(),
        at_ms: now,
    }
}

/// Recruit notices naming this device's user that it hasn't shown yet.
fn unseen_recruits_for_me(state: &AppState, view: &WorkspaceView, now: i64) -> Vec<flow::RecruitNotice> {
    static SEEN: OnceLock<Mutex<std::collections::HashSet<Uuid>>> = OnceLock::new();
    let me = state.local_actor_id();
    let mut seen = SEEN.get_or_init(|| Mutex::new(Default::default())).lock().unwrap();
    let mut out = Vec::new();
    for sess in view.sessions.iter() {
        let mine: Vec<&str> = sess
            .members
            .iter()
            .filter(|m| m.id == me || m.actor.id == me)
            .map(|m| m.id.as_str())
            .collect();
        for r in sess.flow.recruits.iter().filter(|r| r.action == "approvers" && now - r.at_ms < RECRUIT_EVERY_MS) {
            if r.member_ids.iter().any(|id| mine.contains(&id.as_str()) || *id == me) && seen.insert(r.id) {
                out.push(r.clone());
            }
        }
    }
    out
}

/// Background flow tick (every 30 s): opt-in task-board pickup, and
/// reviewer-recruit notifications for this device's user.
pub(crate) async fn run_tick(app: AppHandle) {
    use tauri::Manager;
    use tauri_plugin_notification::NotificationExt;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        let state = app.state::<AppState>();
        let wid = state.active_workspace_id();
        let view = workspace_view(&state, wid);
        let now = flow::now_ms();
        auto_pickup(&app, &state).await;
        for r in unseen_recruits_for_me(&state, &view, now) {
            let _ = app
                .notification()
                .builder()
                .title("Ligus")
                .body(format!("The review queue needs you: {} proposal(s) waiting", r.queue_len))
                .show();
        }
    }
}

/// How long a remote workflow stage may sit unclaimed before an idle device
/// offers to take it.
pub(crate) const IDLE_OFFER_AFTER: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether this device can run `agent`'s runtime itself (configured here,
/// not a fallback).
fn can_run_here(state: &AppState, agent: &hive_core::WorkspaceAgent) -> bool {
    let Some(rt) = state.combined_runtimes().into_iter().find(|r| r.id == agent.runtime_id) else { return false };
    state.resolve_runtime(&agent.runtime_id).provider == rt.provider_kind
}

/// Opt-in: take a workflow stage that has waited [`IDLE_OFFER_AFTER`] for its
/// host. Only when this device is idle (no other chat responding), can run
/// the agent's runtime, and wins the stage prompt's turn claim — the host's
/// worker claims the same prompt when it starts, so whoever claims first runs
/// it and the other backs off. Returns whether it claimed the stage.
pub(crate) fn offer_stage_here(state: &AppState, session_id: Uuid, prompt_id: Uuid, agent_name: &str) -> Option<crate::Responder> {
    let s = settings(state);
    if !s.offer_idle_host {
        return None;
    }
    if state.responding.lock().unwrap().len() > 1 {
        return None;
    }
    let mut svc = state.service.lock().unwrap();
    let session = svc.load(session_id).ok().flatten()?;
    if session.turn_claims.contains_key(&prompt_id) {
        return None;
    }
    // The host already began answering (a placeholder replying to the prompt).
    if session.messages.iter().any(|m| m.reply_to_message_id == Some(prompt_id)) {
        return None;
    }
    let agent = session.workspace_agents.iter().find(|a| a.name == agent_name)?.clone();
    if !can_run_here(state, &agent) {
        return None;
    }
    let workspace_id = session.workspace_id;
    if !svc.claim_turn(session_id, workspace_id, prompt_id).ok()? {
        return None;
    }
    let from_host = agent.host_id.clone();
    drop(svc);
    let now = flow::now_ms();
    let mut r = recruit("idle_host", &flow::tremble::GateQueue::default(), now);
    r.from_host = from_host;
    r.to_host = state.local_host_id();
    r.reference = prompt_id.to_string();
    record(state, session_id, workspace_id, FlowRecord::Recruit(r));
    let mut responder = crate::responder_for(state, &session, Some(&agent));
    // It runs here now.
    responder.host_id = String::new();
    Some(responder)
}

/// Record how long a workflow stage waited for its host: prompt posted →
/// the host's reply began.
pub(crate) fn record_stage_wait(state: &AppState, session_id: Uuid, prompt_id: Uuid, reply_id: Uuid, host_id: &str, run_node: String) {
    let wait = {
        let svc = state.service.lock().unwrap();
        let Ok(Some(s)) = svc.load(session_id) else { return };
        let at = |id: Uuid| s.messages.iter().find(|m| m.id == id).map(|m| flow::ms(&m.created_at));
        match (at(prompt_id), at(reply_id)) {
            (Some(a), Some(b)) => (b - a).max(0),
            _ => return,
        }
    };
    let wid = match state.service.lock().unwrap().load(session_id) {
        Ok(Some(s)) => s.workspace_id,
        _ => return,
    };
    record(
        state,
        session_id,
        wid,
        FlowRecord::EdgeWait(flow::EdgeWait { edge: "stage".into(), reference: run_node, wait_ms: wait, host_id: host_id.to_string(), at_ms: flow::now_ms() }),
    );
}

/// Gate waits measured from the proposals themselves (created → decided).
fn gate_waits(view: &WorkspaceView, since: i64) -> Vec<flow::EdgeWait> {
    let mut out = Vec::new();
    for sess in view.sessions.iter() {
        for p in &sess.proposals {
            if p.status == hive_core::ProposalStatus::Open {
                continue;
            }
            let Some(decided) = p.approvals.iter().map(|a| flow::ms(&a.created_at)).max() else { continue };
            if decided < since {
                continue;
            }
            out.push(flow::EdgeWait {
                edge: "gate".into(),
                reference: p.id.to_string(),
                wait_ms: (decided - flow::ms(&p.created_at)).max(0),
                host_id: String::new(),
                at_ms: decided,
            });
        }
    }
    out
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QueueReport {
    pub gate: flow::tremble::GateQueue,
    pub edges: Vec<flow::tremble::EdgeStats>,
    pub alert: Option<String>,
    pub queue_limit: u32,
    pub stop_upstream: String,
    pub recruits: Vec<flow::RecruitNotice>,
}

pub(crate) fn queue_report(state: &AppState, workspace_id: Uuid) -> QueueReport {
    let s = settings(state);
    let policy = s.tremble_policy();
    let now = flow::now_ms();
    let since = now - 7 * 86_400_000;
    let view = workspace_view(state, workspace_id);
    let gate = flow::tremble::gate_queue(&view.sessions, &policy, now);
    let gate_w = gate_waits(&view, since);
    let mut waits: Vec<&flow::EdgeWait> = view.sessions.iter().flat_map(|s| s.flow.waits.iter()).collect();
    waits.extend(gate_w.iter());
    let edges = flow::tremble::edge_stats(&waits, &policy, since);
    let alert = flow::tremble::bottleneck_alert(&gate, &edges, &policy);
    let mut recruits: Vec<flow::RecruitNotice> =
        view.sessions.iter().flat_map(|s| s.flow.recruits.iter().cloned()).filter(|r| r.at_ms >= since).collect();
    recruits.sort_by_key(|r| std::cmp::Reverse(r.at_ms));
    recruits.truncate(20);
    QueueReport { gate, edges, alert, queue_limit: policy.queue_limit, stop_upstream: s.stop_upstream.clone(), recruits }
}

/// The review queue and hand-off waits for the active workspace (Flow → Queue).
#[tauri::command]
pub(crate) fn flow_queue_report(state: State<'_, AppState>) -> QueueReport {
    let wid = state.active_workspace_id();
    queue_report(&state, wid)
}

/// Leave and clear the marks a reply's directives asked for. An agent only
/// clears its own marks.
fn apply_mark_directives(
    state: &AppState,
    session: &ChatSession,
    by: &str,
    me: &str,
    d: &flow::directives::FlowDirectives,
    now: i64,
) {
    for m in &d.marks {
        let mark = flow::WorkspaceMark {
            id: Uuid::new_v4(),
            kind: m.kind,
            path: m.path.clone(),
            lines: m.lines,
            task_id: String::new(),
            note: m.note.clone(),
            agent: me.to_string(),
            by: by.to_string(),
            session_id: Some(session.id),
            at_ms: now,
            expires_ms: m.kind.default_ttl_ms().map(|t| now + t),
        };
        record(state, session.id, session.workspace_id, FlowRecord::Mark(mark));
    }
    if d.unmarks.is_empty() {
        return;
    }
    let config = state.service.lock().unwrap().flow_config(session.workspace_id).unwrap_or_default();
    for u in &d.unmarks {
        for m in config.marks.values().filter(|m| m.agent == me) {
            let hit = u.id == Some(m.id)
                || (u.path.as_deref().is_some_and(|p| m.covers(p, None)) && u.kind.is_none_or(|k| k == m.kind))
                || (u.id.is_none() && u.path.is_none() && u.kind == Some(m.kind));
            if hit {
                record(state, session.id, session.workspace_id, FlowRecord::MarkCleared { mark_id: m.id });
            }
        }
    }
}

/// Before a turn's diff is filed for review: flag other agents' locks and
/// claims on the files it touches (the proposal still goes up; a person
/// decides).
pub(crate) fn before_file_proposal(state: &AppState, session_id: Uuid, workspace_id: Uuid, agent: Option<Uuid>, proposal: &mut hive_core::ActionProposal) {
    let (config, protected) = {
        let svc = state.service.lock().unwrap();
        let config = svc.flow_config(workspace_id).unwrap_or_default();
        // A change under a workspace guard's human gate (dearth brood paths, a
        // lockdown) keeps the strictest bar: a low-bar zone can't lower it.
        let protected = svc
            .load(session_id)
            .ok()
            .flatten()
            .is_some_and(|s| hive_core::guard::guards_protect(&s, &proposal.changed_files));
        (config, protected)
    };
    // Path zones: the review bar comes from where the change lands.
    if let Some(kind) = flow::zones::apply_review_bar(proposal, &config.zones, protected) {
        let why = match kind {
            flow::ZoneKind::Core => "it touches core paths",
            flow::ZoneKind::LowBar => "every file is in a low-bar path",
        };
        proposal.body.push_str(&format!(
            "\n\n_Review bar: {} approval(s) from {:?} or above, because {why}._",
            proposal.required_approvals, proposal.approval_role_floor
        ));
    }
    let me = flow::agent_key(agent);
    let hits = flow::marks::conflicts(config.marks.values(), &proposal.changed_files, &me, flow::now_ms());
    if hits.is_empty() {
        return;
    }
    let lines: Vec<String> = hits.iter().map(|m| format!("- {}", m.describe())).collect();
    proposal.body.push_str(&format!("\n\n**Touches files others marked:**\n{}", lines.join("\n")));
    let _ = state.service.lock().unwrap().post_system_note(
        session_id,
        workspace_id,
        format!("\u{26a0}\u{fe0f} “{}” touches files others have marked:\n{}", proposal.title, lines.join("\n")),
    );
}

/// The files a unified diff changes, with how (`create`, `delete`, `edit`).
pub(crate) fn diff_touches(diff: &str) -> Vec<(String, &'static str)> {
    let mut out: Vec<(String, &'static str)> = Vec::new();
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let path = rest.rsplit_once(" b/").map(|(_, b)| b).unwrap_or(rest).to_string();
            out.push((path, "edit"));
        } else if line.starts_with("new file mode") {
            if let Some(last) = out.last_mut() {
                last.1 = "create";
            }
        } else if line.starts_with("deleted file mode") {
            if let Some(last) = out.last_mut() {
                last.1 = "delete";
            }
        }
    }
    out
}

/// After a file-diff proposal is applied: record a workspace touch per
/// changed file (opaque keys only), attributed to the turn that produced it,
/// and clear the producer's own claims and locks on those files.
pub(crate) fn after_apply(state: &AppState, session_id: Uuid, workspace_id: Uuid, proposal: &hive_core::ActionProposal, repo_root: &str) {
    let Some(diff) = proposal.diff.as_deref() else { return };
    let session = match state.service.lock().unwrap().load(session_id) {
        Ok(Some(s)) => s,
        _ => return,
    };
    let tag = session.flow.turns.iter().find(|t| t.proposal_ids.contains(&proposal.id));
    let agent = proposal
        .producing_agent_id
        .map(|id| flow::agent_key(Some(id)))
        .or_else(|| tag.map(|t| t.agent.clone()))
        .unwrap_or_else(|| flow::PRIMARY.to_string());
    let secret = key_secret_cache(state, workspace_id);
    let repo = std::path::Path::new(repo_root).file_name().and_then(|n| n.to_str()).unwrap_or("workspace");
    let repo_key = flow::keys::repo_key(&secret, repo);
    let now = flow::now_ms();
    let touches = diff_touches(diff);
    for (path, kind) in &touches {
        record(
            state,
            session_id,
            workspace_id,
            FlowRecord::Touch(flow::WorkspaceTouch {
                message_id: tag.map(|t| t.message_id),
                proposal_id: proposal.id,
                agent: agent.clone(),
                file_key: flow::keys::file_key(&secret, path),
                repo_key: repo_key.clone(),
                kind: (*kind).to_string(),
                at_ms: now,
            }),
        );
    }
    let config = state.service.lock().unwrap().flow_config(workspace_id).unwrap_or_default();
    for m in config.marks.values() {
        let mine = m.agent == agent && matches!(m.kind, flow::MarkKind::Claim | flow::MarkKind::UnderConstruction);
        if mine && touches.iter().any(|(p, _)| m.covers(p, None)) {
            record(state, session_id, workspace_id, FlowRecord::MarkCleared { mark_id: m.id });
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MarkRow {
    #[serde(flatten)]
    pub mark: flow::WorkspaceMark,
    pub chat_title: String,
}

/// Active workspace marks (Flow → Marks).
#[tauri::command]
pub(crate) fn flow_marks(state: State<'_, AppState>) -> Vec<MarkRow> {
    let wid = state.active_workspace_id();
    let view = workspace_view(&state, wid);
    let now = flow::now_ms();
    flow::marks::active(view.config.marks.values(), now)
        .into_iter()
        .map(|m| MarkRow {
            chat_title: m
                .session_id
                .and_then(|sid| view.sessions.iter().find(|s| s.id == sid))
                .map(|s| s.title.clone())
                .unwrap_or_default(),
            mark: m.clone(),
        })
        .collect()
}

/// Leave a mark as the local user.
#[tauri::command]
pub(crate) fn flow_add_mark(
    state: State<'_, AppState>,
    kind: String,
    path: String,
    note: String,
    session_id: Option<String>,
) -> Result<(), String> {
    let kind = flow::MarkKind::parse(&kind).ok_or("unknown mark kind")?;
    let path = flow::keys::normalize_path(path.trim());
    if path.is_empty() || path.contains("..") {
        return Err("give a workspace-relative path".into());
    }
    let wid = state.active_workspace_id();
    let now = flow::now_ms();
    let by = state.service.lock().unwrap().author().display_name.clone();
    let mark = flow::WorkspaceMark {
        id: Uuid::new_v4(),
        kind,
        path,
        lines: None,
        task_id: String::new(),
        note: note.trim().chars().take(200).collect(),
        agent: String::new(),
        by,
        session_id: session_id.and_then(|s| Uuid::parse_str(&s).ok()),
        at_ms: now,
        expires_ms: kind.default_ttl_ms().map(|t| now + t),
    };
    state
        .service
        .lock()
        .unwrap()
        .record_flow(hive_core::workspace_config_session_id(wid), wid, FlowRecord::Mark(mark))
        .map_err(map_err)
}

#[tauri::command]
pub(crate) fn flow_clear_mark(state: State<'_, AppState>, mark_id: String) -> Result<(), String> {
    let id = Uuid::parse_str(&mark_id).map_err(map_err)?;
    let wid = state.active_workspace_id();
    state
        .service
        .lock()
        .unwrap()
        .record_flow(hive_core::workspace_config_session_id(wid), wid, FlowRecord::MarkCleared { mark_id: id })
        .map_err(map_err)
}

/// Path zones (Flow → Zones).
#[tauri::command]
pub(crate) fn flow_zones(state: State<'_, AppState>) -> Result<Vec<flow::PathZone>, String> {
    let wid = state.active_workspace_id();
    Ok(state.service.lock().unwrap().flow_config(wid).map_err(map_err)?.zones)
}

/// Add a path zone (admins only — it sets the review bar).
#[tauri::command]
pub(crate) fn flow_set_zone(
    state: State<'_, AppState>,
    pattern: String,
    kind: flow::ZoneKind,
    approval_floor: Option<hive_core::WorkspaceRole>,
    required_approvals: Option<u32>,
    note: String,
) -> Result<(), String> {
    let pattern = pattern.trim().to_string();
    if pattern.is_empty() || pattern.contains("..") {
        return Err("give a workspace-relative path or pattern".into());
    }
    let wid = state.active_workspace_id();
    let zone = flow::PathZone {
        id: Uuid::new_v4(),
        pattern,
        kind,
        approval_floor,
        required_approvals,
        note: note.trim().chars().take(200).collect(),
        at_ms: flow::now_ms(),
    };
    state
        .service
        .lock()
        .unwrap()
        .record_flow(hive_core::workspace_config_session_id(wid), wid, FlowRecord::Zone(zone))
        .map_err(map_err)
}

#[tauri::command]
pub(crate) fn flow_remove_zone(state: State<'_, AppState>, zone_id: String) -> Result<(), String> {
    let id = Uuid::parse_str(&zone_id).map_err(map_err)?;
    let wid = state.active_workspace_id();
    state
        .service
        .lock()
        .unwrap()
        .record_flow(hive_core::workspace_config_session_id(wid), wid, FlowRecord::ZoneRemoved { zone_id: id })
        .map_err(map_err)
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

/// A hand-off card as the transcript shows it: the recorded card plus, once
/// the receiver has answered, that answer's message id (derived on read from
/// the chat's turn tags; never part of the recorded card).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HandoffCardView {
    #[serde(flatten)]
    card: HandoffCard,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_message_id: Option<Uuid>,
}

fn handoff_views(flow: &FlowState) -> Vec<HandoffCardView> {
    flow.handoffs
        .iter()
        .map(|card| HandoffCardView { reply_message_id: flow.handoff_reply(card), card: card.clone() })
        .collect()
}

/// Hand-off cards recorded in a chat, for the transcript.
#[tauri::command]
pub(crate) fn flow_handoffs(state: State<'_, AppState>, session_id: String) -> Result<Vec<HandoffCardView>, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let svc = state.service.lock().unwrap();
    Ok(svc.load(sid).map_err(map_err)?.map(|s| handoff_views(&s.flow)).unwrap_or_default())
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
    fn routing_skips_runtimes_the_envelope_refuses_or_the_plan_window_throttles() {
        use hive_core::sandbox::{EgressPolicy, SandboxEnvelope};
        use hive_core::throttle::{PlanWindow, WindowStatus};
        use hive_core::ModelProviderKind as P;
        let open = SandboxEnvelope::default();
        let closed = SandboxEnvelope { egress: EgressPolicy::Closed, ..Default::default() };
        assert!(open.egress.is_open() && !closed.egress.is_open());
        // Egress restricted: an unconfinable CLI is left out, a confinable one stays.
        assert_eq!(route_block(P::Aider, &open, None, "a", 0), None);
        assert_eq!(route_block(P::Aider, &closed, None, "a", 0), Some("envelope"));
        assert_eq!(route_block(P::ClaudeCode, &closed, None, "a", 0), None);
        assert_eq!(route_block(P::Anthropic, &closed, None, "a", 0), None);
        // A rejected plan window: no routed hand-offs to it until it resets.
        let hot = PlanWindow { status: WindowStatus::Rejected, utilization_pct: Some(100), resets_at: Some(1_000), window: None, observed_at: 0 };
        assert_eq!(route_block(P::ClaudeCode, &open, Some(&hot), "a", 10), Some("throttled"));
        assert_eq!(route_block(P::ClaudeCode, &open, Some(&hot), "a", 1_000), None, "reset since");
        let cool = PlanWindow { status: WindowStatus::Allowed, utilization_pct: Some(10), resets_at: Some(1_000), window: None, observed_at: 0 };
        assert_eq!(route_block(P::ClaudeCode, &open, Some(&cool), "a", 10), None);
    }

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

    #[test]
    fn diff_touches_classify_files() {
        let diff = "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-x\n+y\n\
diff --git a/new.md b/new.md\nnew file mode 100644\n--- /dev/null\n+++ b/new.md\n\
diff --git a/old.txt b/old.txt\ndeleted file mode 100644\n";
        assert_eq!(diff_touches(diff), vec![("src/a.rs".to_string(), "edit"), ("new.md".to_string(), "create"), ("old.txt".to_string(), "delete")]);
        assert!(diff_touches("").is_empty());
    }

    #[test]
    fn new_agents_start_on_probation_and_older_ones_are_grandfathered() {
        let mut s = session_with(&["veteran", "newbie"]);
        s.workspace_agents[0].created_at = hive_core::Timestamp::from_unix_seconds(CAREERS_SINCE_UNIX - 10);
        s.workspace_agents[1].created_at = hive_core::Timestamp::from_unix_seconds(CAREERS_SINCE_UNIX + 10);
        let (vet, new) = (flow::agent_key(Some(s.workspace_agents[0].id)), flow::agent_key(Some(s.workspace_agents[1].id)));
        let view = WorkspaceView { sessions: Arc::new(vec![s]), config: Arc::new(FlowState::default()) };
        let c = careers(&view);
        assert_eq!(c[flow::PRIMARY].0, flow::Career::Proposer);
        assert_eq!(c[&vet].0, flow::Career::Proposer);
        assert_eq!(c[&new].0, flow::Career::Probation);
    }

    #[test]
    fn board_suggests_the_agent_whose_threshold_is_crossed_most() {
        let mut s = session_with(&["veteran", "newbie"]);
        s.workspace_agents[1].created_at = hive_core::Timestamp::from_unix_seconds(CAREERS_SINCE_UNIX + 10);
        let newbie = flow::agent_key(Some(s.workspace_agents[1].id));
        let mut p = hive_core::ActionProposal::new("Rename parser", hive_core::ProposalKind::Decision, "veteran");
        p.created_at = hive_core::Timestamp::from_unix_seconds(1_000);
        s.proposals.push(p);
        let view = WorkspaceView { sessions: Arc::new(vec![s]), config: Arc::new(FlowState::default()) };
        let rows = board(&view, 1_000_000 + 8 * 3_600_000);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].item.kind, flow::BoardItemKind::UnreviewedProposal);
        // The agent on probation takes review work first; the author never reviews its own.
        assert_eq!(rows[0].suggested.as_deref(), Some(newbie.as_str()));
        // A fresh item calls less strongly.
        let fresh = board(&view, 1_000_000);
        assert!(fresh[0].item.stimulus < rows[0].item.stimulus);
        assert!(fresh[0].nearest_threshold.is_some());
    }

    #[test]
    fn beacon_mismatches_name_the_difference() {
        let b = hive_core::flow::HomeBeacon { relay_url: "https://relay.example/".into(), base_branch: "main".into(), ..Default::default() };
        assert!(beacon_mismatches(&b, "https://relay.example", "main").is_empty());
        let m = beacon_mismatches(&b, "", "develop");
        assert_eq!(m.len(), 2);
        assert!(m[0].contains("no relay"));
        assert!(m[1].contains("start from develop"));
        // An unknown local base branch is not a mismatch.
        assert!(beacon_mismatches(&b, "https://relay.example", "").is_empty());
    }

    #[test]
    fn stop_upstream_modes() {
        let mut s = FlowSettings::default();
        let id = Uuid::new_v4();
        assert_eq!(s.upstream_note(id), (true, ""));
        s.stop_upstream = "off".into();
        assert_eq!(s.upstream_note(id), (false, ""));
        s.stop_upstream = "ab".into();
        let (on, arm) = s.upstream_note(id);
        assert_eq!(on, arm == "note");
        assert!(arm == "note" || arm == "control");
        assert_eq!(s.tremble_policy().queue_limit, 5);
    }

    #[test]
    fn gate_waits_come_from_decided_proposals() {
        let mut s = session_with(&[]);
        let mut p = hive_core::ActionProposal::new("x", hive_core::ProposalKind::Decision, "coder");
        p.created_at = hive_core::Timestamp::from_unix_seconds(1_000);
        p.cast_vote(hive_core::ProposalApproval {
            actor_id: "u".into(),
            role: hive_core::WorkspaceRole::Owner,
            approved: true,
            created_at: hive_core::Timestamp::from_unix_seconds(4_600), diff_digest: None });
        s.proposals.push(p);
        s.proposals.push(hive_core::ActionProposal::new("open", hive_core::ProposalKind::Decision, "coder"));
        let view = WorkspaceView { sessions: Arc::new(vec![s]), config: Arc::new(FlowState::default()) };
        let w = gate_waits(&view, 0);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].wait_ms, 3_600_000);
        assert!(gate_waits(&view, 5_000_000).is_empty());
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

    /// The transcript's hand-off view adds the receiver's answer once it
    /// exists, and otherwise serializes exactly as the recorded card.
    #[test]
    fn handoff_views_name_the_reply_once_the_receiver_answered() {
        let mut flow = FlowState::default();
        let card = HandoffCard {
            id: Uuid::new_v4(),
            from: "a".into(),
            to: "b".into(),
            from_name: "coder".into(),
            to_name: "reviewer".into(),
            message_id: Some(Uuid::new_v4()),
            task: "check it".into(),
            task_class: "review".into(),
            branch: String::new(),
            files: vec![],
            quality: Default::default(),
            via: "mention".into(),
            parcel: None,
            at_ms: 100,
        };
        flow.apply(&FlowRecord::Handoff(card.clone()));
        let before = serde_json::to_value(&handoff_views(&flow)[0]).unwrap();
        assert_eq!(before, serde_json::to_value(&card).unwrap(), "no reply yet: the card as recorded");
        let reply = Uuid::new_v4();
        flow.apply(&FlowRecord::TurnTagged(TurnTag {
            message_id: reply,
            trigger_id: card.message_id,
            agent: "b".into(),
            runtime_id: String::new(),
            host_id: String::new(),
            task_class: "review".into(),
            role: "conversation".into(),
            arm: String::new(),
            upstream_arm: String::new(),
            error: false,
            error_kind: String::new(),
            tests_passed: None,
            tool_calls: 0,
            duration_ms: 0,
            cost_usd: 0.0,
            proposal_ids: vec![],
            handoff_parcel: false,
            at_ms: 200,
        }));
        let after = serde_json::to_value(&handoff_views(&flow)[0]).unwrap();
        assert_eq!(after["replyMessageId"], serde_json::json!(reply));
        assert_eq!(after["id"], serde_json::json!(card.id));
    }
}

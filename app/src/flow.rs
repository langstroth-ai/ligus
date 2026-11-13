//! The app side of [`hive_core::flow`]: settings, the hooks the dispatch path
//! calls (prime a turn's context, tag a finished turn, record hand-offs), and
//! the Tauri commands behind the Flow pane.
//!
//! Every hook is best effort: a flow failure is logged and never fails or
//! delays a turn.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use hive_core::flow::{self, FlowRecord, HandoffCard, TurnTag};
use hive_core::{ChatMessage, ChatSession, MessageRole};
use hive_runtime::provider::ChatTurn;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use uuid::Uuid;

use crate::{map_err, AppState, Responder};

/// Device-local flow settings (Settings → Flow). Persisted in settings.json.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct FlowSettings {
    /// Record hand-off cards and give receivers their context parcels.
    pub handoff_cards: bool,
}

impl Default for FlowSettings {
    fn default() -> Self {
        Self { handoff_cards: true }
    }
}

pub(crate) fn settings(state: &AppState) -> FlowSettings {
    state.settings.lock().unwrap().flow.clone()
}

/// Append a flow record, logging (not returning) a failure.
pub(crate) fn record(state: &AppState, session_id: Uuid, workspace_id: Uuid, record: FlowRecord) {
    let kind = record.type_str();
    if let Err(e) = state.service.lock().unwrap().record_flow(session_id, workspace_id, record) {
        tracing::warn!(target: "flow", kind, "couldn't record a flow record: {e}");
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

    let directives = take_stashed(message_id);
    if settings(state).handoff_cards && facts.error.is_none() {
        record_handoff(state, &session, &reply, &me, &class, tests_passed, &filed, &directives);
    }
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
    }
}

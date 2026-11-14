//! Track D on the desktop: the Ligus MCP server for this host's agent turns
//! (agent-signed effects, budgets, audit, ask-the-human), the ACP permission
//! escalations, and hand-off auto-start. Kept out of `lib.rs`: a turn calls
//! [`begin`], wraps its dispatch in [`TurnIface::scope`], and calls
//! [`TurnIface::finish`] after finalizing.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use hive_core::flow::HumanAsk;
use hive_runtime::ligus_mcp::{Budgets, LigusHost, LigusMcp, TurnHandle, TurnSpec};
use hive_runtime::{ChatService, ResolvedRuntime};
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;

use crate::AppState;

struct AppHost {
    app: AppHandle,
}

impl LigusHost for AppHost {
    fn with_service(&self, f: &mut dyn FnMut(&mut ChatService)) {
        let state = self.app.state::<AppState>();
        let mut svc = state.service.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut svc);
    }

    fn start_turn(&self, chat_id: Uuid, agent_id: Uuid, trigger: Uuid) {
        let app = self.app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = crate::handoff_turn(&app, chat_id, agent_id, trigger).await {
                tracing::warn!(target: "dispatch", chat = %chat_id, "hand-off turn failed: {e}");
            }
        });
    }

    fn ask_posted(&self, _chat_id: Uuid, _ask: &HumanAsk) {
        // The open chat refetches and shows the question with its answer buttons.
        let _ = self.app.emit("workspace://synced", 1);
    }

    fn forge_for(&self, _chat_id: Uuid) -> Option<hive_runtime::ligus_mcp::forge_tools::ForgeBinding> {
        let state = self.app.state::<AppState>();
        let root = state.workspace_root.lock().unwrap_or_else(|e| e.into_inner()).clone();
        (!root.trim().is_empty()).then(|| crate::forges::binding_for_dir(&state.data_dir, std::path::Path::new(&root))).flatten()
    }
}

/// This app's Ligus MCP server (one per process; its host key never leaves
/// memory).
fn server(app: &AppHandle) -> Arc<LigusMcp> {
    static S: OnceLock<Arc<LigusMcp>> = OnceLock::new();
    S.get_or_init(|| LigusMcp::new(Arc::new(AppHost { app: app.clone() }))).clone()
}

/// One turn's Ligus interface.
pub(crate) struct TurnIface {
    pub turn: Option<Arc<TurnHandle>>,
    /// The socket a CLI agent reaches (allowed through the sandbox).
    pub socket: Option<PathBuf>,
    /// The turn runs an in-process tool loop (folded tools).
    pub tool_loop: bool,
}

/// Start the Ligus server for a turn when its runtime can use it and the
/// author gate allows tools. Otherwise the turn keeps the reply directives.
#[allow(clippy::too_many_arguments)]
pub(crate) fn begin(
    app: &AppHandle,
    rt: &ResolvedRuntime,
    restricted: bool,
    tools_allowed: bool,
    workspace_id: Uuid,
    chat_id: Uuid,
    agent_id: Option<Uuid>,
    author: &str,
    message_id: Uuid,
    worktree: Option<PathBuf>,
) -> TurnIface {
    let none = TurnIface { turn: None, socket: None, tool_loop: false };
    if !tools_allowed || !hive_runtime::ligus_mcp::offered_for(rt, restricted) {
        return none;
    }
    let ttl = std::time::Duration::from_secs(
        hive_core::compat::var("LIGUS_TURN_TIMEOUT_SECS").ok().and_then(|v| v.parse().ok()).filter(|s: &u64| *s > 0).unwrap_or(3600) + 300,
    );
    let turn = server(app).begin_turn(TurnSpec {
        workspace_id,
        chat_id,
        agent_id,
        author: author.to_string(),
        message_id,
        worktree,
        budgets: Budgets::from_env(),
        ttl,
    });
    let cli = rt.provider.is_cli_agent();
    let socket = if cli {
        match turn.listen() {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!(target: "ligus_mcp", "Ligus MCP socket unavailable, using directives: {e}");
                return none;
            }
        }
    } else {
        None
    };
    TurnIface { turn: Some(Arc::new(turn)), socket, tool_loop: !cli }
}

impl TurnIface {
    pub fn offered(&self) -> bool {
        self.turn.is_some()
    }

    /// The system prompt for this turn: directive teaching replaced by the
    /// Ligus tools when they're offered (D.4.4).
    pub fn system(&self, system: String) -> String {
        if !self.offered() {
            return system;
        }
        hive_runtime::prompt::with_ligus_tools(&system, self.tool_loop, &crate::flow::directive_guidance())
    }

    /// The agent turn scope for dispatch (MCP attach, pause, spend, the
    /// ACP permission asker).
    pub fn scope(&self) -> hive_runtime::agent_turn::AgentTurnScope {
        match &self.turn {
            Some(t) => t.agent_scope(),
            // No Ligus tools this turn: the spend budget still holds.
            None => hive_runtime::agent_turn::AgentTurnScope {
                spend: Some(hive_runtime::agent_turn::SpendMeter::new(Budgets::from_env().max_spend_usd)),
                ..Default::default()
            },
        }
    }

    /// What the tools did (finalize's dedupe; the diff intent).
    pub fn effects(&self) -> Option<hive_runtime::finalize::McpEffectsLog> {
        self.turn.as_ref().map(|t| t.effects())
    }

    /// End the turn: revoke the credential, record the content-free tool
    /// summary, and show the agent's self-report.
    pub fn finish(&self, state: &AppState, chat_id: Uuid, workspace_id: Uuid, author: &str, notes: &[String]) {
        let Some(t) = &self.turn else { return };
        t.finish();
        let mut svc = state.service.lock().unwrap_or_else(|e| e.into_inner());
        for n in notes {
            let json = serde_json::json!({ "note": n }).to_string();
            let _ = svc.record_device_fact(workspace_id, chat_id, "turnNote", &json);
        }
        if let Some(r) = t.report() {
            let files = if r.files_touched.is_empty() { String::new() } else { format!(" Files: {}.", r.files_touched.join(", ")) };
            let conf = r.confidence.map(|c| format!(" Confidence: {c}.")).unwrap_or_default();
            let _ = svc.post_system_note(chat_id, workspace_id, format!("\u{1f4cb} {author} reported {}: {}.{files}{conf}", r.status, r.summary.trim_end_matches('.')));
        }
    }
}

/// An open question an agent asked in a chat (D.4.2).
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AskDto {
    pub ask_id: String,
    pub agent_name: String,
    pub question: String,
    pub options: Vec<String>,
    pub expires_at_ms: i64,
    pub answer: Option<String>,
    pub answered_by: Option<String>,
    pub open: bool,
}

pub(crate) fn ask_dtos(session: &hive_core::ChatSession) -> Vec<AskDto> {
    let name = |key: &str| {
        hive_core::flow::agent_from_key(key)
            .and_then(|id| session.workspace_agents.iter().find(|a| a.id == id).map(|a| a.name.clone()))
            .unwrap_or_else(|| "hive".into())
    };
    let mut v: Vec<&hive_core::flow::AskState> = session.flow.asks.values().collect();
    v.sort_by_key(|a| a.ask.at_ms);
    v.into_iter()
        .rev()
        .take(20)
        .map(|a| AskDto {
            ask_id: a.ask.ask_id.to_string(),
            agent_name: name(&a.ask.agent_key),
            question: a.ask.question.clone(),
            options: a.ask.options.clone(),
            expires_at_ms: a.ask.expires_at_ms,
            answer: a.answer.as_ref().map(|x| x.answer.clone()),
            answered_by: a.answer.as_ref().map(|x| x.by.clone()).filter(|b| !b.is_empty()),
            open: a.is_open(),
        })
        .collect()
}

/// The agents' questions in a chat, newest first.
#[tauri::command]
pub(crate) fn list_asks(state: tauri::State<'_, AppState>, session_id: String) -> Result<Vec<AskDto>, String> {
    let sid = Uuid::parse_str(&session_id).map_err(|e| e.to_string())?;
    let svc = state.service.lock().unwrap();
    Ok(svc.load(sid).map_err(|e| e.to_string())?.map(|s| ask_dtos(&s)).unwrap_or_default())
}

/// Answer an agent's question as this device's person. Answering grants
/// nothing by itself; the first answer in canonical order wins.
#[tauri::command]
pub(crate) fn answer_ask(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    session_id: String,
    ask_id: String,
    answer: String,
    option_index: Option<u32>,
) -> Result<(), String> {
    let sid = Uuid::parse_str(&session_id).map_err(|e| e.to_string())?;
    let aid = Uuid::parse_str(&ask_id).map_err(|e| e.to_string())?;
    let mut svc = state.service.lock().unwrap();
    let session = svc.load(sid).map_err(|e| e.to_string())?.ok_or("unknown chat")?;
    answer_in(&mut svc, &session, aid, answer, option_index)?;
    drop(svc);
    let _ = app.emit("workspace://synced", 1);
    Ok(())
}

/// Record a person's answer (shared with tests).
pub(crate) fn answer_in(svc: &mut ChatService, session: &hive_core::ChatSession, ask_id: Uuid, answer: String, option_index: Option<u32>) -> Result<(), String> {
    let st = session.flow.asks.get(&ask_id).ok_or("no such question")?;
    if !st.is_open() {
        return Err("that question is already answered or closed".into());
    }
    let text = match option_index.and_then(|i| st.ask.options.get(i as usize)) {
        Some(o) if answer.trim().is_empty() => o.clone(),
        _ => answer.trim().to_string(),
    };
    if text.is_empty() {
        return Err("an answer is required".into());
    }
    let by = svc.author().display_name.clone();
    let rec = hive_core::flow::FlowRecord::HumanAnswered(hive_core::flow::HumanAnswered {
        ask_id,
        answer: text.chars().take(hive_core::flow::MAX_ANSWER_CHARS).collect(),
        option_index,
        by,
        at_ms: hive_core::flow::now_ms(),
    });
    svc.record_flow(session.id, session.workspace_id, rec).map_err(|e| e.to_string())
}

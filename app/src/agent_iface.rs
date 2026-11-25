//! Track D on the desktop: the Ligus MCP server for this host's agent turns
//! (agent-signed effects, budgets, audit, ask-the-human), the ACP permission
//! escalations, and hand-off auto-start. Kept out of `lib.rs`; a turn's own
//! Ligus interface lives in the turn engine.

use std::sync::{Arc, OnceLock};

use ligus_core::flow::HumanAsk;
use ligus_runtime::ligus_mcp::{LigusHost, LigusMcp};
use ligus_runtime::ChatService;
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

    fn ask_posted(&self, chat_id: Uuid, ask: &HumanAsk) {
        // The open chat refetches and shows the question with its answer buttons.
        let _ = self.app.emit("workspace://synced", 1);
        crate::notices::ask_posted(&self.app, &self.app.state::<AppState>(), chat_id, ask.turn_id, ask.ask_id);
    }

    fn stage_attachment(&self, proposal_id: Uuid, sha256: &str, bytes: &[u8]) -> Result<(), String> {
        let state = self.app.state::<AppState>();
        crate::attachments::stage(&state.data_dir, proposal_id, sha256, bytes)
    }

    /// The code host of the turn's own chat (review F14): its repo's
    /// checkout on this device, else its workspace's bound folder; never
    /// the folder the window happens to show (another chat's forge and
    /// credentials).
    fn forge_for(&self, chat_id: Uuid) -> Option<ligus_runtime::ligus_mcp::forge_tools::ForgeBinding> {
        let state = self.app.state::<AppState>();
        let root = match crate::repos_ui::chat_checkout(&state, chat_id) {
            Some(p) => p.to_string_lossy().into_owned(),
            None => {
                let ws = state.service.lock().unwrap_or_else(|e| e.into_inner()).workspace_of(chat_id).ok().flatten()?;
                crate::legacy_folder(&state, ws).ok()?
            }
        };
        (!root.trim().is_empty()).then(|| crate::forges::binding_for_dir(&state.data_dir, std::path::Path::new(&root))).flatten()
    }
}

/// This app's Ligus MCP server (one per process; its host key never leaves
/// memory). A turn's interface to it is the turn engine's
/// (`ligus_runtime::turn_engine::ligus`).
pub(crate) fn server(app: &AppHandle) -> Arc<LigusMcp> {
    static S: OnceLock<Arc<LigusMcp>> = OnceLock::new();
    S.get_or_init(|| LigusMcp::new(Arc::new(AppHost { app: app.clone() }))).clone()
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
    /// `permission`: the turn's host asks to run something (only its
    /// person or the agent's owner can grant it); `question` otherwise.
    pub kind: &'static str,
    /// Posted by someone other than the agent or its turn's host: theirs,
    /// and never a permission request.
    pub posted_by: Option<String>,
}

pub(crate) fn ask_dtos(session: &ligus_core::ChatSession) -> Vec<AskDto> {
    let name = |key: &str| {
        ligus_core::flow::agent_from_key(key)
            .and_then(|id| session.workspace_agents.iter().find(|a| a.id == id).map(|a| a.name.clone()))
            .unwrap_or_else(|| "ligus".into())
    };
    let mut v: Vec<&ligus_core::flow::AskState> = session.flow.asks.values().collect();
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
            kind: if a.ask.permission { "permission" } else { "question" },
            posted_by: a.poster_name(&session.members).map(|n| ligus_proto::names::clean_display_name(&n)),
        })
        .collect()
}

/// The agents' questions in a chat, newest first.
#[tauri::command]
pub(crate) async fn list_asks(ligus_app: tauri::AppHandle, session_id: String) -> Result<Vec<AskDto>, String> {
    crate::off_main(ligus_app, move |ligus_app| list_asks_sync(tauri::Manager::state::<AppState>(ligus_app), session_id)).await
}

pub(crate) fn list_asks_sync(state: tauri::State<'_, AppState>, session_id: String) -> Result<Vec<AskDto>, String> {
    let sid = Uuid::parse_str(&session_id).map_err(|e| e.to_string())?;
    let svc = state.service.lock().unwrap();
    Ok(svc.load_resolving(sid).map_err(|e| e.to_string())?.map(|s| ask_dtos(&s)).unwrap_or_default())
}

/// Answer an agent's question as this device's person. Answering grants
/// nothing by itself; the first answer in canonical order wins.
#[tauri::command]
pub(crate) async fn answer_ask(ligus_app: tauri::AppHandle, session_id: String, ask_id: String, answer: String, option_index: Option<u32>) -> Result<(), String> {
    crate::off_main(ligus_app, move |ligus_app| answer_ask_sync(ligus_app.clone(), tauri::Manager::state::<AppState>(ligus_app), session_id, ask_id, answer, option_index)).await
}

pub(crate) fn answer_ask_sync(
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
    let session = svc.load_resolving(sid).map_err(|e| e.to_string())?.ok_or("unknown chat")?;
    answer_in(&mut svc, &session, aid, answer, option_index)?;
    drop(svc);
    let _ = app.emit("workspace://synced", 1);
    Ok(())
}

/// Record a person's answer (shared with tests).
pub(crate) fn answer_in(svc: &mut ChatService, session: &ligus_core::ChatSession, ask_id: Uuid, answer: String, option_index: Option<u32>) -> Result<(), String> {
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
    let rec = ligus_core::flow::FlowRecord::HumanAnswered(ligus_core::flow::HumanAnswered {
        ask_id,
        answer: text.chars().take(ligus_core::flow::MAX_ANSWER_CHARS).collect(),
        option_index,
        by,
        at_ms: ligus_core::flow::now_ms(),
    });
    svc.record_flow(session.id, session.workspace_id, rec).map_err(|e| e.to_string())
}

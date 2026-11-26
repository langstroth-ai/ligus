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

/// An agent's question in a chat, as the decision card draws it
/// (`docs/design/agent-questions.md` 7.1): the same card every surface
/// builds ([`ligus_runtime::ask_view`]).
pub(crate) type AskDto = ligus_runtime::ask_view::AskView;

/// The cards of a chat, newest first, for this device's person.
pub(crate) fn ask_dtos(svc: &ChatService, session: &ligus_core::ChatSession) -> Vec<AskDto> {
    let me = svc.author().id.clone();
    let here = svc.device_id();
    let hosts = svc.list_hosts(session.workspace_id).unwrap_or_default();
    let label = |d: Uuid| -> Option<String> {
        if d == here {
            return Some("this device".into());
        }
        hosts.iter().find(|h| h.id == d.to_string()).map(|h| crate::shown(&h.label)).filter(|l| !l.is_empty())
    };
    ligus_runtime::ask_view::views(session, &me, ligus_core::flow::now_ms(), 20, &label)
}

/// The agents' questions in a chat, newest first.
#[tauri::command]
pub(crate) async fn list_asks(ligus_app: tauri::AppHandle, session_id: String) -> Result<Vec<AskDto>, String> {
    crate::off_main(ligus_app, move |ligus_app| list_asks_sync(tauri::Manager::state::<AppState>(ligus_app), session_id)).await
}

pub(crate) fn list_asks_sync(state: tauri::State<'_, AppState>, session_id: String) -> Result<Vec<AskDto>, String> {
    let sid = Uuid::parse_str(&session_id).map_err(|e| e.to_string())?;
    let svc = state.service.lock().unwrap();
    Ok(svc.load_resolving(sid).map_err(|e| e.to_string())?.map(|s| ask_dtos(&svc, &s)).unwrap_or_default())
}

/// Answer a card as this device's person: an option, free text where the
/// card allows it, or "Not now". The answer is bound to the digest of the
/// signed question this device holds; a grant whose effect needs it is
/// signed with the person's passkey (the presence provider asks).
#[tauri::command]
pub(crate) async fn answer_ask(
    ligus_app: tauri::AppHandle,
    session_id: String,
    ask_id: String,
    option_index: Option<u32>,
    text: Option<String>,
    decline: Option<bool>,
) -> Result<(), String> {
    crate::off_main(ligus_app, move |ligus_app| {
        let choice = ligus_runtime::chat_service::AskChoice { option: option_index, text: text.unwrap_or_default(), decline: decline.unwrap_or(false) };
        answer_ask_sync(ligus_app.clone(), tauri::Manager::state::<AppState>(ligus_app), session_id, ask_id, choice)
    })
    .await
}

pub(crate) fn answer_ask_sync(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    session_id: String,
    ask_id: String,
    choice: ligus_runtime::chat_service::AskChoice,
) -> Result<(), String> {
    let sid = Uuid::parse_str(&session_id).map_err(|e| e.to_string())?;
    let aid = Uuid::parse_str(&ask_id).map_err(|e| e.to_string())?;
    let mut svc = state.service.lock().unwrap();
    let session = svc.load_resolving(sid).map_err(|e| e.to_string())?.ok_or("unknown chat")?;
    answer_in(&mut svc, &session, aid, choice)?;
    drop(svc);
    let _ = app.emit("workspace://synced", 1);
    Ok(())
}

/// Record a person's answer (shared with tests).
pub(crate) fn answer_in(svc: &mut ChatService, session: &ligus_core::ChatSession, ask_id: Uuid, choice: ligus_runtime::chat_service::AskChoice) -> Result<(), String> {
    svc.answer_ask(session.id, session.workspace_id, ask_id, choice).map(|_| ()).map_err(|e| match e {
        ligus_runtime::chat_service::ChatError::Unauthorized(m) | ligus_runtime::chat_service::ChatError::NotFound(m) => m,
        other => other.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ligus_core::flow::{AskAudience, FlowRecord, HumanAsk, ASK_V2};

    fn service() -> (ChatService, Uuid, Uuid) {
        let store = ligus_runtime::EventStore::open_in_memory().unwrap();
        let author = ligus_core::ActorIdentity::new("u1", "Mike", ligus_core::ActorKind::Human);
        let mut svc = ChatService::new(store, ligus_core::SigningKeypair::generate().unwrap(), ligus_core::SigningKeypair::generate().unwrap(), author);
        svc.enable_test_presence();
        let wid = Uuid::new_v4();
        let chat = svc.create_chat("A", wid, "rt").unwrap().id;
        (svc, wid, chat)
    }

    fn post(svc: &mut ChatService, wid: Uuid, chat: Uuid, ask: HumanAsk) {
        svc.record_flow(chat, wid, FlowRecord::HumanAsk(ask)).unwrap();
    }

    /// The card's DTO: plain text cleaned, who it asks, eligibility, and the
    /// effect line's data; a v2 answer is bound to the question's digest.
    #[test]
    fn cards_carry_what_the_card_draws_and_answers_bind_the_digest() {
        let (mut svc, wid, chat) = service();
        let me = svc.author().id.clone();
        let id = Uuid::new_v4();
        post(
            &mut svc,
            wid,
            chat,
            HumanAsk {
                ask_id: id,
                turn_id: Uuid::new_v4(),
                agent_key: "primary".into(),
                question: "Use Postgres\u{202E} or SQLite?".into(),
                options: vec!["Postgres".into(), "SQLite".into()],
                at_ms: 1,
                v: ASK_V2,
                audience: Some(AskAudience::people(vec![me.clone()])),
                allow_text: true,
                ..Default::default()
            },
        );
        let s = svc.load_resolving(chat).unwrap().unwrap();
        let d = ask_dtos(&svc, &s);
        assert_eq!(d.len(), 1);
        let v = serde_json::to_value(&d[0]).unwrap();
        assert_eq!(v["question"], "Use Postgres or SQLite?", "readers clean too");
        assert_eq!(v["kind"], "question");
        assert_eq!(v["eligible"], true);
        assert_eq!(v["audience"]["kind"], "people");
        assert!(v["turnId"].is_string() && v["live"] == true && v["allowText"] == true);
        answer_in(&mut svc, &s, id, ligus_runtime::chat_service::AskChoice { option: Some(1), text: "keep the dev db".into(), decline: false }).unwrap();
        let s = svc.load_resolving(chat).unwrap().unwrap();
        let st = &s.flow.asks[&id];
        let digest = svc.ask_digest_here(wid, chat, id).unwrap().unwrap();
        assert_eq!(st.answer.as_ref().map(|a| a.ask_digest.clone()), Some(digest), "bound to the signed question here");
        let v = serde_json::to_value(&ask_dtos(&svc, &s)[0]).unwrap();
        assert_eq!((v["answer"].as_str(), v["answerText"].as_str(), v["live"].as_bool()), (Some("SQLite"), Some("keep the dev db"), Some(false)));
        assert_eq!(v["answeredBy"][0]["memberId"], me);
    }

    /// An older card's "Not now" writes nothing (it would settle it for
    /// everyone); on a v2 card to one person it declines it.
    #[test]
    fn not_now_is_local_on_an_older_card() {
        let (mut svc, wid, chat) = service();
        let me = svc.author().id.clone();
        let old = Uuid::new_v4();
        post(&mut svc, wid, chat, HumanAsk { ask_id: old, turn_id: Uuid::new_v4(), agent_key: "primary".into(), question: "q".into(), at_ms: 1, ..Default::default() });
        let s = svc.load_resolving(chat).unwrap().unwrap();
        let decline = ligus_runtime::chat_service::AskChoice { decline: true, ..Default::default() };
        assert!(answer_in(&mut svc, &s, old, decline.clone()).is_err());
        let new = Uuid::new_v4();
        post(
            &mut svc,
            wid,
            chat,
            HumanAsk {
                ask_id: new,
                turn_id: Uuid::new_v4(),
                agent_key: "primary".into(),
                question: "q".into(),
                options: vec!["a".into(), "b".into()],
                at_ms: 2,
                v: ASK_V2,
                audience: Some(AskAudience::people(vec![me])),
                ..Default::default()
            },
        );
        let s = svc.load_resolving(chat).unwrap().unwrap();
        answer_in(&mut svc, &s, new, decline).unwrap();
        let s = svc.load_resolving(chat).unwrap().unwrap();
        assert!(s.flow.asks[&new].declined && s.flow.asks[&old].is_open());
    }
}

//! What each chat is waiting on, for the sidebar's state chips and the
//! command palette: open agent questions, proposals still waiting on a
//! decision, and whether the chat's agents are paused by a lockdown.
//!
//! One read over the active workspace's chats (the same walk `list_chats`
//! does), so the sidebar doesn't have to load every chat's proposals and asks
//! one by one. Live turn state (working, finished, failed) isn't here: it rides
//! the `chat://stream` events the webview already receives.

use tauri::State;

use ligus_core::{ActionProposal, ChatSession, ProposalStatus};

use crate::{map_err, shown, AppState};

/// One proposal still waiting on a decision.
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttentionProposalDto {
    pub id: String,
    pub title: String,
    /// "open" | "approved" (quorum met, waiting to be applied).
    pub status: String,
    /// Waiting on this person: approved and ready to apply, or open with no
    /// vote of theirs yet on a proposal they didn't write.
    pub needs_me: bool,
}

/// What one chat is waiting on. Chats waiting on nothing are left out.
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatAttentionDto {
    pub session_id: String,
    /// The chat's workspace: a chat is (workspace, chat).
    pub workspace_id: String,
    /// Agent questions nobody has answered yet that are still waiting.
    pub open_asks: u32,
    pub proposals: Vec<AttentionProposalDto>,
    /// A lockdown holds the chat's agents until a person clears it.
    pub paused: bool,
}

fn waiting(p: &ActionProposal) -> bool {
    !p.dismissed && matches!(p.status, ProposalStatus::Open | ProposalStatus::Approved)
}

fn needs_me(p: &ActionProposal, me: &str) -> bool {
    match p.status {
        ProposalStatus::Approved => true,
        ProposalStatus::Open => p.author_actor_id != me && !p.approvals.iter().any(|a| a.actor_id == me),
        _ => false,
    }
}

/// The chat's attention, or None when it waits on nothing.
pub(crate) fn chat_attention(s: &ChatSession, me: &str, now_ms: i64) -> Option<ChatAttentionDto> {
    let open_asks = s
        .flow
        .asks
        .values()
        .filter(|a| a.is_open() && (a.ask.expires_at_ms == 0 || a.ask.expires_at_ms > now_ms))
        .count() as u32;
    let proposals: Vec<AttentionProposalDto> = s
        .proposals
        .iter()
        .filter(|p| waiting(p))
        .map(|p| AttentionProposalDto {
            id: p.id.to_string(),
            title: shown(&p.title),
            status: if p.status == ProposalStatus::Approved { "approved" } else { "open" }.into(),
            needs_me: needs_me(p, me),
        })
        .collect();
    let paused = !s.guards.lockdowns_of(s.id).is_empty();
    if open_asks == 0 && proposals.is_empty() && !paused {
        return None;
    }
    Some(ChatAttentionDto { session_id: s.id.to_string(), workspace_id: s.workspace_id.to_string(), open_asks, proposals, paused })
}

#[tauri::command]
pub(crate) async fn list_chat_attention(ligus_app: tauri::AppHandle) -> Result<Vec<ChatAttentionDto>, String> {
    crate::off_main(ligus_app, move |ligus_app| list_chat_attention_sync(tauri::Manager::state::<AppState>(ligus_app))).await
}

pub(crate) fn list_chat_attention_sync(state: State<AppState>) -> Result<Vec<ChatAttentionDto>, String> {
    let active = state.active_workspace_id();
    let me = state.local_actor_id();
    let known_rooms = state.known_room_ids();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let svc = state.service.lock().unwrap();
    let config_id = ligus_core::workspace_config_session_id(active);
    let mut out = Vec::new();
    for id in svc.store().list_session_ids().map_err(map_err)? {
        if id == config_id {
            continue;
        }
        let Some(s) = crate::workspace_chat(&svc, id, active, &me, &known_rooms).map_err(map_err)? else { continue };
        if s.archived {
            continue;
        }
        out.extend(chat_attention(&s, &me, now_ms));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ligus_core::flow::{AskState, HumanAnswered, HumanAsk};
    use ligus_core::guard::{ChatLockdown, LockdownSignal};
    use ligus_core::{ProposalApproval, ProposalKind, WorkspaceRole};
    use uuid::Uuid;

    fn session() -> ChatSession {
        ChatSession::new("t", Uuid::new_v4(), "rt")
    }

    fn ask(s: &mut ChatSession, expires_at_ms: i64, answered: bool, closed: bool) {
        let id = Uuid::new_v4();
        s.flow.asks.insert(
            id,
            AskState {
                ask: HumanAsk { ask_id: id, question: "30s or 60s?".into(), expires_at_ms, closed, ..Default::default() },
                answer: answered.then(|| HumanAnswered { ask_id: id, answer: "30s".into(), ..Default::default() }),
                ..Default::default()
            },
        );
    }

    fn proposal(s: &mut ChatSession, title: &str, author: &str) -> usize {
        s.proposals.push(ActionProposal::new(title, ProposalKind::Decision, author));
        s.proposals.len() - 1
    }

    #[test]
    fn a_quiet_chat_is_left_out() {
        assert_eq!(chat_attention(&session(), "me", 1_000), None);
    }

    #[test]
    fn counts_only_asks_still_waiting() {
        let mut s = session();
        ask(&mut s, 0, false, false); // open, no timeout
        ask(&mut s, 5_000, false, false); // open, still waiting at t=1000
        ask(&mut s, 500, false, false); // timed out
        ask(&mut s, 0, true, false); // answered
        ask(&mut s, 0, false, true); // closed by the host
        let a = chat_attention(&s, "me", 1_000).unwrap();
        assert_eq!(a.open_asks, 2);
        assert!(a.proposals.is_empty());
        assert!(!a.paused);
    }

    #[test]
    fn lists_proposals_waiting_on_a_decision_and_whether_they_wait_on_me() {
        let mut s = session();
        proposal(&mut s, "Agent's change", "agent-1");
        let mine = proposal(&mut s, "My change", "me");
        let voted = proposal(&mut s, "Already voted", "agent-1");
        s.proposals[voted].approvals.push(ProposalApproval {
            actor_id: "me".into(),
            role: WorkspaceRole::Owner,
            approved: true,
            created_at: Default::default(),
            diff_digest: None,
        });
        let ready = proposal(&mut s, "Ready to apply", "agent-1");
        s.proposals[ready].status = ProposalStatus::Approved;
        let applied = proposal(&mut s, "Done", "agent-1");
        s.proposals[applied].status = ProposalStatus::Applied;
        let hidden = proposal(&mut s, "Dismissed", "agent-1");
        s.proposals[hidden].dismissed = true;
        let _ = mine;

        let a = chat_attention(&s, "me", 0).unwrap();
        let got: Vec<(&str, &str, bool)> =
            a.proposals.iter().map(|p| (p.title.as_str(), p.status.as_str(), p.needs_me)).collect();
        assert_eq!(
            got,
            vec![
                ("Agent's change", "open", true),
                ("My change", "open", false),
                ("Already voted", "open", false),
                ("Ready to apply", "approved", true),
            ]
        );
    }

    #[test]
    fn a_lockdown_pauses_the_chat() {
        let mut s = session();
        let chat = s.id;
        s.guards.lockdowns.entry(chat).or_default().push(ChatLockdown {
            id: Uuid::new_v4(),
            signal: LockdownSignal::Manual,
            detail: String::new(),
            raised_by: "ana".into(),
            raised_at: Default::default(),
            message_id: None,
            raise_event_id: None,
        });
        let a = chat_attention(&s, "me", 0).unwrap();
        assert!(a.paused);
        assert_eq!(a.open_asks, 0);
    }
}

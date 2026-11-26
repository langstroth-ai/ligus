//! What each chat is waiting on, for the sidebar's state chips and the
//! command palette: open agent questions, proposals still waiting on a
//! decision, and whether the chat's agents are paused by a lockdown.
//!
//! One read over the active workspace's chats (the same walk `list_chats`
//! does), so the sidebar doesn't have to load every chat's proposals and asks
//! one by one. Live turn state (working, finished, failed) isn't here: it rides
//! the `chat://stream` events the webview already receives.

use tauri::State;

use std::collections::BTreeSet;

use ligus_core::{ActionProposal, ChatSession, ProposalStatus, Timestamp};
use uuid::Uuid;

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

/// One agent question still waiting, as an Inbox row names it.
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttentionAskDto {
    pub ask_id: String,
    pub agent_name: String,
    /// `question`, `permission` or `secret`.
    pub kind: &'static str,
    /// This person may answer it (else the row says who it waits on).
    pub eligible: bool,
    /// "you", "Admins", "the room", or the named people.
    pub audience: String,
}

/// What one chat is waiting on. Chats waiting on nothing are left out.
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatAttentionDto {
    pub session_id: String,
    /// The chat's workspace: a chat is (workspace, chat).
    pub workspace_id: String,
    /// Agent questions still waiting that this person may answer.
    pub open_asks: u32,
    /// Every question still waiting (expiry is a view), newest first;
    /// `eligible` says which this person may answer.
    pub asks: Vec<AttentionAskDto>,
    pub proposals: Vec<AttentionProposalDto>,
    /// A lockdown holds the chat's agents until a person clears it.
    pub paused: bool,
}

/// Waiting on a decision and in this device's inbox: archived and
/// hidden-for-me proposals never count (`ligus_core::proposal_archive`; an
/// archive can't take out one that waits on someone, a hide-for-me can).
fn waiting(p: &ActionProposal, s: &ChatSession, hidden: &BTreeSet<Uuid>, now: Timestamp) -> bool {
    p.superseded_by.is_none()
        && matches!(p.status, ProposalStatus::Open | ProposalStatus::Approved)
        && ligus_runtime::chat_service::in_inbox(p, s, hidden, now)
}

fn needs_me(p: &ActionProposal, me: &str) -> bool {
    match p.status {
        ProposalStatus::Approved => true,
        ProposalStatus::Open => {
            p.author_actor_id != me && !p.approvals.iter().any(|a| a.actor_id == me)
        }
        _ => false,
    }
}

/// The chat's attention, or None when it waits on nothing.
pub(crate) fn chat_attention(
    s: &ChatSession,
    me: &str,
    now_ms: i64,
    hidden: &BTreeSet<Uuid>,
) -> Option<ChatAttentionDto> {
    let now = Timestamp::from_unix_seconds(now_ms / 1000);
    let mut live: Vec<&ligus_core::flow::AskState> =
        s.flow.asks.values().filter(|a| a.is_live(now_ms)).collect();
    live.sort_by_key(|a| std::cmp::Reverse(a.ask.at_ms));
    let asks: Vec<AttentionAskDto> = live
        .iter()
        .map(|a| {
            let v = ligus_runtime::ask_view::view(s, a, me, now_ms, &|_| None);
            let audience = match (v.audience.kind, v.audience.min_role) {
                ("people", _) if v.eligible && v.audience.member_ids.len() == 1 => {
                    "you".to_string()
                }
                ("people", _) => v.audience.names.join(", "),
                (_, Some("admin")) => "Admins".into(),
                (_, Some("owner")) => "Owners".into(),
                _ => "the room".into(),
            };
            AttentionAskDto {
                ask_id: v.ask_id,
                agent_name: v.agent_name,
                kind: v.kind,
                eligible: v.eligible,
                audience,
            }
        })
        .collect();
    let open_asks = asks.iter().filter(|a| a.eligible).count() as u32;
    let proposals: Vec<AttentionProposalDto> = s
        .proposals
        .iter()
        .filter(|p| waiting(p, s, hidden, now))
        .map(|p| AttentionProposalDto {
            id: p.id.to_string(),
            title: shown(&p.title),
            status: if p.status == ProposalStatus::Approved {
                "approved"
            } else {
                "open"
            }
            .into(),
            needs_me: needs_me(p, me),
        })
        .collect();
    let paused = !s.guards.lockdowns_of(s.id).is_empty();
    if asks.is_empty() && proposals.is_empty() && !paused {
        return None;
    }
    Some(ChatAttentionDto {
        session_id: s.id.to_string(),
        workspace_id: s.workspace_id.to_string(),
        open_asks,
        asks,
        proposals,
        paused,
    })
}

#[tauri::command]
pub(crate) async fn list_chat_attention(
    ligus_app: tauri::AppHandle,
) -> Result<Vec<ChatAttentionDto>, String> {
    crate::off_main(ligus_app, move |ligus_app| {
        list_chat_attention_sync(tauri::Manager::state::<AppState>(ligus_app))
    })
    .await
}

pub(crate) fn list_chat_attention_sync(
    state: State<AppState>,
) -> Result<Vec<ChatAttentionDto>, String> {
    let active = state.active_workspace_id();
    let me = state.local_actor_id();
    let known_rooms = state.known_room_ids();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let svc = state.service.lock().unwrap();
    let config_id = ligus_core::workspace_config_session_id(active);
    let hidden = svc.hidden_proposals(active).map_err(map_err)?;
    let mut out = Vec::new();
    for id in svc.store().list_session_ids().map_err(map_err)? {
        if id == config_id {
            continue;
        }
        let Some(s) =
            crate::workspace_chat(&svc, id, active, &me, &known_rooms).map_err(map_err)?
        else {
            continue;
        };
        if s.archived {
            continue;
        }
        out.extend(chat_attention(&s, &me, now_ms, &hidden));
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
        let mut s = ChatSession::new("t", Uuid::new_v4(), "rt");
        s.members.push(ligus_core::WorkspaceMember {
            id: "me".into(),
            actor: ligus_core::ActorIdentity::new("me", "Mike", ligus_core::ActorKind::Human),
            role: WorkspaceRole::Contributor,
            title: String::new(),
            index: 1,
            joined_at: Default::default(),
        });
        s
    }

    /// An ask put to someone else is listed (the Inbox shows it only when
    /// nothing else waits) but doesn't count as needing you.
    #[test]
    fn asks_for_others_are_listed_but_not_counted() {
        let mut s = session();
        let id = Uuid::new_v4();
        s.flow.asks.insert(
            id,
            AskState {
                ask: HumanAsk {
                    ask_id: id,
                    question: "Ship?".into(),
                    options: vec!["Yes".into(), "No".into()],
                    v: ligus_core::flow::ASK_V2,
                    audience: Some(ligus_core::flow::AskAudience::room(WorkspaceRole::Admin)),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let a = chat_attention(&s, "me", 1_000, &BTreeSet::new()).unwrap();
        assert_eq!(a.open_asks, 0);
        assert_eq!(a.asks.len(), 1);
        assert!(!a.asks[0].eligible);
        assert_eq!(a.asks[0].audience, "Admins");
    }

    fn ask(s: &mut ChatSession, expires_at_ms: i64, answered: bool, closed: bool) {
        let id = Uuid::new_v4();
        s.flow.asks.insert(
            id,
            AskState {
                ask: HumanAsk {
                    ask_id: id,
                    question: "30s or 60s?".into(),
                    expires_at_ms,
                    closed,
                    ..Default::default()
                },
                answer: answered.then(|| HumanAnswered {
                    ask_id: id,
                    answer: "30s".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
    }

    fn proposal(s: &mut ChatSession, title: &str, author: &str) -> usize {
        s.proposals
            .push(ActionProposal::new(title, ProposalKind::Decision, author));
        s.proposals.len() - 1
    }

    #[test]
    fn a_quiet_chat_is_left_out() {
        assert_eq!(
            chat_attention(&session(), "me", 1_000, &BTreeSet::new()),
            None
        );
    }

    #[test]
    fn counts_only_asks_still_waiting() {
        let mut s = session();
        ask(&mut s, 0, false, false); // open, no timeout
        ask(&mut s, 5_000, false, false); // open, still waiting at t=1000
        ask(&mut s, 500, false, false); // timed out
        ask(&mut s, 0, true, false); // answered
        ask(&mut s, 0, false, true); // closed by the host
        let a = chat_attention(&s, "me", 1_000, &BTreeSet::new()).unwrap();
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
        // Marked archived while it still waits: an archive can't hide it.
        let archived = proposal(&mut s, "Archived while open", "agent-1");
        s.proposals[archived].dismissed = true;
        // Hidden for me on this device: out of my counts.
        let hidden = proposal(&mut s, "Hidden for me", "agent-1");
        let hidden_set = BTreeSet::from([s.proposals[hidden].id]);
        // Stale and archived by the hygiene sweep: out.
        let stale = proposal(&mut s, "Stale", "agent-1");
        let stale_id = s.proposals[stale].id;
        s.proposals[stale].dismissed = true;
        s.guards.stale_proposals.insert(stale_id);
        let _ = mine;

        let a = chat_attention(&s, "me", 0, &hidden_set).unwrap();
        let got: Vec<(&str, &str, bool)> = a
            .proposals
            .iter()
            .map(|p| (p.title.as_str(), p.status.as_str(), p.needs_me))
            .collect();
        assert_eq!(
            got,
            vec![
                ("Agent's change", "open", true),
                ("My change", "open", false),
                ("Already voted", "open", false),
                ("Ready to apply", "approved", true),
                ("Archived while open", "open", true),
            ]
        );
    }

    #[test]
    fn a_lockdown_pauses_the_chat() {
        let mut s = session();
        let chat = s.id;
        s.guards
            .lockdowns
            .entry(chat)
            .or_default()
            .push(ChatLockdown {
                id: Uuid::new_v4(),
                signal: LockdownSignal::Manual,
                detail: String::new(),
                raised_by: "ana".into(),
                raised_at: Default::default(),
                message_id: None,
                raise_event_id: None,
            });
        let a = chat_attention(&s, "me", 0, &BTreeSet::new()).unwrap();
        assert!(a.paused);
        assert_eq!(a.open_asks, 0);
    }
}

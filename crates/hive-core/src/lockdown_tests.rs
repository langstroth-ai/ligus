//! Chat lockdown (self-medication) through projection: who may raise and who
//! may clear it, and what it does to a chat's envelope and human gate.

use uuid::Uuid;

use crate::events::SessionEvent;
use crate::testkit::project_honest as project;
use crate::guard::{human_gate_reason, ChatLockdown, LockdownSignal};
use crate::identity::{ActorIdentity, ActorKind, WorkspaceMember, WorkspaceRole};
use crate::lease_projection_tests::{base, ev, seed};
use crate::proposals::ActionProposal;
use crate::sandbox::{envelope_for_session, EgressPolicy};
use crate::time_util::Timestamp;

fn raise(chat: Uuid) -> SessionEvent {
    SessionEvent::ChatLockdownRaised {
        chat_id: chat,
        lockdown: ChatLockdown {
            id: Uuid::new_v4(),
            signal: LockdownSignal::SecretInDiff,
            detail: "aws_access_key".into(),
            raised_by: "worker".into(),
            raised_at: Timestamp::epoch(),
            message_id: None,
        },
    }
}

fn clear(chat: Uuid) -> SessionEvent {
    SessionEvent::ChatLockdownCleared { chat_id: chat }
}

/// Project the config log, then look at it as chat `chat` would (the overlay
/// `ChatService::load` performs).
fn as_chat(envs: &[crate::SessionEventEnvelope], chat: Uuid) -> crate::ChatSession {
    let mut s = project(envs).unwrap();
    s.guards.chat_id = Some(chat);
    s
}

fn with_bot() -> crate::ChatSession {
    let mut s = base();
    s.members.push(WorkspaceMember {
        id: "bot".into(),
        actor: ActorIdentity::new("bot", "bot", ActorKind::Agent),
        role: WorkspaceRole::Admin,
        title: String::new(),
        index: 0,
        joined_at: Timestamp::epoch(),
    });
    s
}

#[test]
fn anyone_with_the_key_can_raise_only_a_human_member_can_clear() {
    let s = with_bot();
    let chat = Uuid::new_v4();
    let mut envs = vec![seed(&s), ev(&s, 1, "some-worker-host", None, raise(chat))];
    assert!(as_chat(&envs, chat).guards.lockdown_active(), "a non-member worker host may tighten");
    // A non-member clearing it: dropped.
    envs.push(ev(&s, 2, "stranger", None, clear(chat)));
    // A Viewer: dropped.
    envs.push(ev(&s, 3, "victor", None, clear(chat)));
    // An agent-kind member, even an Admin: dropped.
    envs.push(ev(&s, 4, "bot", None, clear(chat)));
    assert!(as_chat(&envs, chat).guards.lockdown_active());
    // A human Contributor clears it.
    envs.push(ev(&s, 5, "alice", None, clear(chat)));
    assert!(!as_chat(&envs, chat).guards.lockdown_active());
    // A later signal locks it again.
    envs.push(ev(&s, 6, "owner", None, raise(chat)));
    assert!(as_chat(&envs, chat).guards.lockdown_active());
}

#[test]
fn lockdown_is_per_chat() {
    let s = base();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let envs = vec![seed(&s), ev(&s, 1, "owner", None, raise(a))];
    assert!(as_chat(&envs, a).guards.lockdown_active());
    assert!(!as_chat(&envs, b).guards.lockdown_active());
}

#[test]
fn lockdown_closes_egress_and_gates_every_proposal() {
    let s = base();
    let chat = Uuid::new_v4();
    let envs = vec![seed(&s), ev(&s, 1, "owner", None, raise(chat))];
    let mut c = as_chat(&envs, chat);
    let e = envelope_for_session(&c);
    assert_eq!(e.egress, EgressPolicy::Closed);
    assert!(e.human_gate && e.isolate_writes);
    let p = ActionProposal::file_diff("p", "", "d".into(), vec!["README.md".into()]);
    c.proposals.push(p.clone());
    assert!(human_gate_reason(&c, &p).unwrap().contains("locked down"));
}

/// The cause's message id is optional and additive: a lockdown without one
/// serializes exactly as before (no `messageId` key), one with it round-trips,
/// and the projection keeps it so the UI can jump to the cause by id.
#[test]
fn lockdown_message_id_is_left_out_when_absent_and_kept_through_projection() {
    let SessionEvent::ChatLockdownRaised { lockdown, .. } = raise(Uuid::nil()) else { unreachable!() };
    let json = serde_json::to_value(&lockdown).unwrap();
    assert!(json.get("messageId").is_none(), "{json}");
    let old: ChatLockdown = serde_json::from_value(json).unwrap();
    assert_eq!(old.message_id, None);

    let reply = Uuid::new_v4();
    let with = ChatLockdown { message_id: Some(reply), ..lockdown };
    let json = serde_json::to_value(&with).unwrap();
    assert_eq!(json["messageId"], serde_json::json!(reply));
    assert_eq!(serde_json::from_value::<ChatLockdown>(json).unwrap(), with);

    let s = with_bot();
    let chat = Uuid::new_v4();
    let envs = vec![
        seed(&s),
        ev(&s, 1, "some-worker-host", None, SessionEvent::ChatLockdownRaised { chat_id: chat, lockdown: with }),
    ];
    assert_eq!(as_chat(&envs, chat).guards.lockdowns_of(chat)[0].message_id, Some(reply));
}

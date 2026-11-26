//! A chat's lockdown as a client shows it: the signals in force and whether
//! the person on this device may clear them.
//!
//! Clearing is a `ChatLockdownCleared` event, which the fold accepts only
//! from a human member at Contributor or above (with presence under rules
//! v2). [`can_clear`] is that rule, for the UI; every device re-checks it on
//! ingest.

use ligus_core::guard::ChatLockdown;
use ligus_core::{ChatSession, WorkspaceMember};
use uuid::Uuid;

/// Whether `me` may clear a lockdown: a human member at Contributor or above.
/// A Viewer, an agent or an unknown identity never may.
pub fn can_clear(members: &[WorkspaceMember], me: &str) -> bool {
    ligus_core::guard::is_human_member(members, me)
}

/// A chat's lockdown for `me`.
#[derive(Debug, Clone, PartialEq)]
pub struct LockdownView {
    /// The signals in force (empty: not locked down).
    pub signals: Vec<ChatLockdown>,
    /// Whether `me` may clear them ([`can_clear`]).
    pub can_clear: bool,
}

impl LockdownView {
    pub fn active(&self) -> bool {
        !self.signals.is_empty()
    }

    /// The ids of the raise events a clear names (each signal the person was
    /// shown).
    pub fn raise_ids(&self) -> Vec<Uuid> {
        self.signals
            .iter()
            .filter_map(|l| l.raise_event_id)
            .collect()
    }
}

/// `chat_id`'s lockdown as `me` sees it, from its projected session.
pub fn chat_lockdown(session: &ChatSession, chat_id: Uuid, me: &str) -> LockdownView {
    LockdownView {
        signals: session.guards.lockdowns_of(chat_id).to_vec(),
        can_clear: can_clear(&session.members, me),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ligus_core::{ActorIdentity, ActorKind, Timestamp, WorkspaceRole};

    fn member(id: &str, kind: ActorKind, role: WorkspaceRole) -> WorkspaceMember {
        WorkspaceMember {
            id: id.into(),
            actor: ActorIdentity::new(id.to_string(), id.to_string(), kind),
            role,
            title: String::new(),
            index: 1,
            joined_at: Timestamp::now(),
        }
    }

    #[test]
    fn only_a_human_contributor_or_above_may_clear() {
        let members = vec![
            member("ana", ActorKind::Human, WorkspaceRole::Contributor),
            member("own", ActorKind::Human, WorkspaceRole::Owner),
            member("vic", ActorKind::Human, WorkspaceRole::Viewer),
            member("bot", ActorKind::Agent, WorkspaceRole::Admin),
        ];
        assert!(can_clear(&members, "ana"));
        assert!(can_clear(&members, "own"));
        assert!(!can_clear(&members, "vic"));
        assert!(!can_clear(&members, "bot"));
        assert!(!can_clear(&members, "nobody"));
    }

    #[test]
    fn an_unlocked_chat_has_no_signals() {
        let mut s = ChatSession::new("t", Uuid::new_v4(), "");
        s.members
            .push(member("ana", ActorKind::Human, WorkspaceRole::Admin));
        let v = chat_lockdown(&s, s.id, "ana");
        assert!(!v.active());
        assert!(v.can_clear);
        assert!(v.raise_ids().is_empty());
    }
}

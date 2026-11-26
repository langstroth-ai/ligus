//! Archiving proposals on [`ChatService`] (see `ligus_core::proposal_archive`):
//! Archive and Restore (synced, `ProposalDismissed`), Hide for me
//! (device-local), and the workspace's auto-archive setting.

use std::collections::BTreeSet;

use ligus_core::proposal_archive::{self, Archived, ProposalArchivePolicy};
use ligus_core::{ActionProposal, ChatSession, SessionEvent, Timestamp};
use uuid::Uuid;

use super::{ChatError, ChatService, Result};

/// Where a proposal shows on this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shelf {
    /// Waiting on someone, or settled and not yet archived.
    Inbox,
    /// Archived, by a person or by the auto-archive rule.
    Archived(Archived),
    /// Hidden on this device by this person ("Hide for me").
    Hidden,
}

/// Where `p` shows on this device: `hidden` is this device's hidden set.
/// Hidden wins over archived so "Show again" brings it back where it was.
pub fn shelf(p: &ActionProposal, session: &ChatSession, hidden: &BTreeSet<Uuid>, now: Timestamp) -> Shelf {
    if hidden.contains(&p.id) {
        return Shelf::Hidden;
    }
    match proposal_archive::archived(p, session, now) {
        Some(a) => Shelf::Archived(a),
        None => Shelf::Inbox,
    }
}

/// Whether `p` belongs in the inbox, the needs-you counts and agents'
/// context on this device: neither archived nor hidden here.
pub fn in_inbox(p: &ActionProposal, session: &ChatSession, hidden: &BTreeSet<Uuid>, now: Timestamp) -> bool {
    shelf(p, session, hidden, now) == Shelf::Inbox
}

impl ChatService {
    /// Archive (synced, for everyone) or restore proposals of a chat.
    /// Contributor+ (the fold's content floor; a Viewer's is refused when it
    /// is signed). A proposal that still waits on someone (open, or approved
    /// and not yet applied) can't be archived: reject it first, or hide it
    /// for yourself. Restoring an auto-archived proposal writes a restore so
    /// it stays in the inbox on every device. Returns the ids that changed.
    pub fn archive_proposals(&mut self, session_id: Uuid, workspace_id: Uuid, proposal_ids: &[Uuid], archived: bool) -> Result<Vec<Uuid>> {
        let Some(session) = self.load_in(workspace_id, session_id)? else {
            return Ok(Vec::new());
        };
        let now = Timestamp::now();
        let mut changed = Vec::new();
        for p in session.proposals.iter().filter(|p| proposal_ids.contains(&p.id)) {
            let is_archived = proposal_archive::archived(p, &session, now).is_some();
            if archived {
                if proposal_archive::waits_on_someone(p, &session) {
                    return Err(ChatError::Unauthorized(format!(
                        "“{}” still waits on a decision, so it can't be archived: reject it first, or hide it for yourself",
                        p.title
                    )));
                }
                if !proposal_archive::marked_archived(p) {
                    changed.push(p.id);
                }
            } else if proposal_archive::marked_archived(p) || is_archived {
                changed.push(p.id);
            }
        }
        for proposal_id in &changed {
            self.append_signed(session_id, workspace_id, SessionEvent::ProposalDismissed { proposal_id: *proposal_id, dismissed: archived })?;
        }
        Ok(changed)
    }

    /// The proposals hidden on this device in `workspace_id` ("Hide for me").
    pub fn hidden_proposals(&self, workspace_id: Uuid) -> Result<BTreeSet<Uuid>> {
        Ok(self.store.hidden_proposals(workspace_id)?)
    }

    /// Hide proposals on this device only, or show them again. Nothing is
    /// signed or synced: another device of the same person still shows them.
    pub fn set_proposals_hidden(&self, workspace_id: Uuid, proposal_ids: &[Uuid], hidden: bool) -> Result<()> {
        Ok(self.store.set_proposals_hidden(workspace_id, proposal_ids, hidden)?)
    }

    /// The workspace's auto-archive days in force (`None`: off).
    pub fn proposal_archive_days(&self, workspace_id: Uuid) -> Result<Option<u32>> {
        let config_id = ligus_core::workspace_config_session_id(workspace_id);
        let set = self.load_in(workspace_id, config_id)?.and_then(|c| c.guards.proposal_archive);
        Ok(ProposalArchivePolicy::days_in_force(set.as_ref()))
    }

    /// Set the workspace's auto-archive days (0 = off; at most a year).
    /// An Admin's (refused for anyone else when it is signed, and on every
    /// device's fold).
    pub fn set_proposal_archive_days(&mut self, workspace_id: Uuid, after_days: u32) -> Result<()> {
        if after_days > proposal_archive::MAX_AFTER_DAYS {
            return Err(ChatError::Unauthorized(format!("auto-archive is at most {} days", proposal_archive::MAX_AFTER_DAYS)));
        }
        let config_id = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(config_id, workspace_id, SessionEvent::ProposalArchivePolicySet { policy: ProposalArchivePolicy { after_days } })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_store::EventStore;
    use ligus_core::{ActorIdentity, ProposalKind, SigningKeypair};

    fn service() -> ChatService {
        let store = EventStore::open_in_memory().unwrap();
        let author = ActorIdentity::new("u1", "Mara", ligus_core::ActorKind::Human);
        ChatService::new(store, SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), author)
    }

    fn chat_with(svc: &mut ChatService, author: &str) -> (Uuid, Uuid, Uuid) {
        let chat = svc.create_chat("Demo", Uuid::new_v4(), "anthropic").unwrap();
        let (sid, wid) = (chat.id, chat.workspace_id);
        svc.ensure_workspace_config(wid).unwrap();
        let p = ActionProposal::new("Ship it", ProposalKind::Decision, author);
        let pid = p.id;
        svc.upsert_proposal(sid, wid, p).unwrap();
        (sid, wid, pid)
    }

    fn dismiss_events(svc: &ChatService, wid: Uuid, sid: Uuid) -> usize {
        svc.store.list_in(wid, sid).unwrap().iter().filter(|e| matches!(e.payload, SessionEvent::ProposalDismissed { .. })).count()
    }

    #[test]
    fn a_proposal_that_waits_on_someone_isnt_archived() {
        let mut svc = service();
        let (sid, wid, pid) = chat_with(&mut svc, "agent");
        let err = svc.archive_proposals(sid, wid, &[pid], true).unwrap_err().to_string();
        assert!(err.contains("still waits on a decision"), "{err}");
        assert_eq!(dismiss_events(&svc, wid, sid), 0);
    }

    #[test]
    fn archive_and_restore_a_settled_proposal_once_each() {
        let mut svc = service();
        let (sid, wid, pid) = chat_with(&mut svc, "agent");
        svc.vote_on_proposal(sid, wid, pid, false).unwrap();
        assert_eq!(svc.archive_proposals(sid, wid, &[pid], true).unwrap(), vec![pid]);
        assert!(svc.archive_proposals(sid, wid, &[pid], true).unwrap().is_empty(), "already archived: nothing written");
        let s = svc.load_in(wid, sid).unwrap().unwrap();
        assert_eq!(shelf(&s.proposals[0], &s, &BTreeSet::new(), Timestamp::now()), Shelf::Archived(Archived::ByPerson));
        assert_eq!(svc.archive_proposals(sid, wid, &[pid], false).unwrap(), vec![pid]);
        assert_eq!(dismiss_events(&svc, wid, sid), 2);
    }

    #[test]
    fn restoring_an_auto_archived_proposal_writes_a_restore() {
        let mut svc = service();
        let (sid, wid, pid) = chat_with(&mut svc, "agent");
        svc.vote_on_proposal(sid, wid, pid, false).unwrap();
        // Auto-archive after one day; seen a week later it is archived.
        svc.set_proposal_archive_days(wid, 1).unwrap();
        assert_eq!(svc.proposal_archive_days(wid).unwrap(), Some(1));
        let s = svc.load_in(wid, sid).unwrap().unwrap();
        let later = Timestamp(Timestamp::now().inner() + time::Duration::days(7));
        assert_eq!(shelf(&s.proposals[0], &s, &BTreeSet::new(), later), Shelf::Archived(Archived::Auto));
        // Nothing was written when it aged out.
        assert_eq!(dismiss_events(&svc, wid, sid), 0);
        // Not archived yet today: a restore has nothing to do.
        assert!(svc.archive_proposals(sid, wid, &[pid], false).unwrap().is_empty());
        svc.set_proposal_archive_days(wid, 0).unwrap();
        assert_eq!(svc.proposal_archive_days(wid).unwrap(), None);
        assert!(svc.set_proposal_archive_days(wid, 400).is_err());
    }

    #[test]
    fn hide_for_me_stays_on_this_device_and_writes_nothing() {
        let mut svc = service();
        let (sid, wid, pid) = chat_with(&mut svc, "agent");
        let before = svc.store.list_in(wid, sid).unwrap().len();
        svc.set_proposals_hidden(wid, &[pid], true).unwrap();
        let hidden = svc.hidden_proposals(wid).unwrap();
        let s = svc.load_in(wid, sid).unwrap().unwrap();
        // Hidden wins even over "waits on someone": it is this person's own view.
        assert_eq!(shelf(&s.proposals[0], &s, &hidden, Timestamp::now()), Shelf::Hidden);
        assert!(!in_inbox(&s.proposals[0], &s, &hidden, Timestamp::now()));
        assert_eq!(svc.store.list_in(wid, sid).unwrap().len(), before, "no event");
        svc.set_proposals_hidden(wid, &[pid], false).unwrap();
        assert!(in_inbox(&s.proposals[0], &s, &svc.hidden_proposals(wid).unwrap(), Timestamp::now()));
    }
}

//! Proposal archiving: old proposals stop taking up room in the Review pane.
//!
//! Three ways a proposal leaves the inbox, none of which deletes it:
//!
//! - **Archive** (synced, for everyone): `ProposalDismissed { dismissed: true }`,
//!   the event the Review pane's Dismiss always wrote. Restore is the same event
//!   with `dismissed: false`. The fold stamps when (`archived_at`, the event's
//!   recorded time) and who (`archived_by`, the account on its signed stamp).
//! - **Auto-archive** (a derived view rule, never an event): a settled proposal
//!   (applied, rejected or superseded) counts as archived once it has been
//!   settled for the workspace's [`ProposalArchivePolicy`] days (default
//!   [`DEFAULT_AFTER_DAYS`]; 0 = off). "Settled at" is the settling event's own
//!   recorded time, which every device folds alike, so no device writes
//!   anything on a timer and all of them agree on which proposals have aged
//!   out (each against its own clock for "now").
//! - **Hide for me** (device-local, see the runtime's hidden-proposal store):
//!   there is no per-person channel that syncs between one person's devices,
//!   so it stays on the device.
//!
//! A proposal that still waits on someone (open, or approved and not yet
//! applied) is never archived for the inbox and the needs-you counts, even
//! when someone marked it archived: an archive can't hide work that needs a
//! decision. Rejecting it settles it; then it can be archived. A proposal the
//! hygiene sweep marked stale (its diff no longer applies) is the one open
//! proposal an archive does take out.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::proposals::{ActionProposal, ProposalStatus};
use crate::session::ChatSession;
use crate::time_util::Timestamp;

/// Days a settled proposal stays in the inbox before it is auto-archived, when
/// the workspace hasn't set its own.
pub const DEFAULT_AFTER_DAYS: u32 = 7;
/// The longest an Admin may set (a year). Longer is refused by the fold.
pub const MAX_AFTER_DAYS: u32 = 365;

/// The workspace's auto-archive setting (`ProposalArchivePolicySet`, config
/// log, Admin). Absent = [`DEFAULT_AFTER_DAYS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposalArchivePolicy {
    /// Days after settling; 0 turns auto-archive off.
    pub after_days: u32,
}

impl ProposalArchivePolicy {
    /// The days in force for a workspace's folded setting (`None`: off).
    pub fn days_in_force(set: Option<&ProposalArchivePolicy>) -> Option<u32> {
        let d = set.map(|p| p.after_days).unwrap_or(DEFAULT_AFTER_DAYS);
        (d > 0).then_some(d)
    }
}

/// Whether `p` is settled: applied, rejected, or replaced by a newer revision.
pub fn is_settled(p: &ActionProposal) -> bool {
    matches!(p.status, ProposalStatus::Applied | ProposalStatus::Rejected) || p.superseded_by.is_some()
}

/// Whether `p` still waits on someone: open or approved-not-applied, not
/// superseded, and not found stale by the hygiene sweep.
pub fn waits_on_someone(p: &ActionProposal, session: &ChatSession) -> bool {
    !is_settled(p) && !session.guards.stale_proposals.contains(&p.id)
}

/// Why a proposal is archived, if it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Archived {
    /// Someone archived it (or the hygiene sweep, for a stale proposal).
    ByPerson,
    /// It has been settled for the workspace's auto-archive days.
    Auto,
}

/// Whether `p`, in `session` (whose guards carry the workspace's policy, see
/// the runtime's config overlay), counts as archived at `now`.
pub fn archived(p: &ActionProposal, session: &ChatSession, now: Timestamp) -> Option<Archived> {
    if waits_on_someone(p, session) {
        return None;
    }
    if marked_archived(p) {
        return Some(Archived::ByPerson);
    }
    if p.keep_unarchived || !is_settled(p) {
        return None;
    }
    let days = ProposalArchivePolicy::days_in_force(session.guards.proposal_archive.as_ref())?;
    // A proposal seeded from a snapshot that predates `settled_at` ages from
    // when it was filed.
    let since = p.settled_at.unwrap_or(p.created_at);
    let due = since.inner().saturating_add(time::Duration::days(days as i64));
    (now.inner() >= due).then_some(Archived::Auto)
}

/// Whether someone who may archive (Contributor+) left `p` archived: the
/// fold's mark, or `dismissed` for a proposal seeded without one.
pub fn marked_archived(p: &ActionProposal) -> bool {
    p.archive_mark.unwrap_or(p.dismissed)
}

/// When `p` is (or will be) auto-archived, for "archives in 3 days" hints.
pub fn auto_archive_due(p: &ActionProposal, session: &ChatSession) -> Option<Timestamp> {
    if marked_archived(p) || p.keep_unarchived || !is_settled(p) {
        return None;
    }
    let days = ProposalArchivePolicy::days_in_force(session.guards.proposal_archive.as_ref())?;
    let since = p.settled_at.unwrap_or(p.created_at);
    Some(Timestamp(since.inner().saturating_add(time::Duration::days(days as i64))))
}

/// The fold's bookkeeping for one event that may have settled, unsettled,
/// archived or restored proposals. `before` is each proposal's settled and
/// dismissed state before the event applied; `at` is the event's recorded
/// time; `account` the account on its signed stamp; `may_archive` whether its
/// author may archive (Contributor+ at its position).
pub(crate) fn note_event(
    session: &mut ChatSession,
    before: &[(Uuid, bool, bool)],
    payload: &crate::events::SessionEvent,
    at: Timestamp,
    account: Option<Uuid>,
    may_archive: bool,
) {
    for p in session.proposals.iter_mut() {
        let was = before.iter().find(|(id, _, _)| *id == p.id).map(|(_, s, _)| *s);
        let now = is_settled(p);
        match (was, now) {
            // Settled by this event (or filed settled): its time, never
            // earlier than when it was filed (a skewed clock can't age it out
            // before it existed).
            (Some(false) | None, true) => {
                p.settled_at = Some(at.max(p.created_at));
                p.keep_unarchived = false;
            }
            (Some(true), false) => {
                p.settled_at = None;
                p.keep_unarchived = false;
            }
            _ => {}
        }
    }
    if let crate::events::SessionEvent::ProposalDismissed { proposal_id, dismissed } = payload {
        let Some(p) = session.proposals.iter_mut().find(|p| p.id == *proposal_id) else {
            return;
        };
        if !may_archive {
            // Pin what it was before this event, so a proposal seeded
            // without a mark doesn't fall back to the flag it just flipped.
            if p.archive_mark.is_none() {
                p.archive_mark = before.iter().find(|(id, _, _)| *id == p.id).map(|(_, _, d)| *d);
            }
            return;
        }
        p.archive_mark = Some(*dismissed);
        if *dismissed {
            p.archived_at = Some(at);
            p.archived_by = account;
        } else {
            p.archived_at = None;
            p.archived_by = None;
            // Restoring a settled proposal keeps it out of auto-archive.
            p.keep_unarchived = is_settled(p);
        }
    }
}

/// Each proposal's settled and dismissed state, taken before an event applies.
pub(crate) fn settled_states(session: &ChatSession) -> Vec<(Uuid, bool, bool)> {
    session.proposals.iter().map(|p| (p.id, is_settled(p), p.dismissed)).collect()
}

/// Whether `payload` can change a proposal's settled or archived state.
pub(crate) fn touches_proposals(payload: &crate::events::SessionEvent) -> bool {
    use crate::events::SessionEvent as E;
    matches!(
        payload,
        E::ProposalUpserted { .. }
            | E::ProposalVoteCast { .. }
            | E::ProposalDismissed { .. }
            | E::ProposalApplied { .. }
            | E::ProposalReverted { .. }
            | E::ProposalSetDecided { .. }
            | E::ProposalSetAlternativeStopped { .. }
            | E::ProposalSetTuned { .. }
            | E::ProposalSetOpened { .. }
            | E::ProposalSetAlternativeAdded { .. }
    )
}

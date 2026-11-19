//! Coordinator leases: who coordinates a scope (a workflow run, a workspace)
//! and since which **epoch**.
//!
//! Whatever coordinates a scope holds a lease. The lease itself is durable — a
//! signed `CoordinatorLeaseClaimed` event on the log the scope's state lives on
//! (a run's chat log, or the workspace-config log) — while its *liveness* rides
//! the relay's ephemeral presence channel as signed heartbeats (see
//! `ligus_runtime::lease_presence`). Other devices start succession only when the
//! heartbeat lapses; a holder that is alive but unhealthy hands over on purpose
//! (a release naming a successor).
//!
//! ## Fencing
//!
//! The epoch is a fencing token. The fold accepts a claim only when its epoch is
//! **strictly greater** than the current one, and the first such claim in the
//! canonical `(lamport, event_id)` order wins on every device. Two devices that
//! both try to succeed at the same epoch (split brain) therefore converge on one
//! holder; the loser sees the higher epoch and stops. Writes a coordinator makes
//! under its lease (e.g. `WorkflowRun::lease_epoch`) are dropped from the fold
//! once a later epoch has been claimed before them in canonical order.
//!
//! The fold cannot see wall-clock time, so "only on lapse" is enforced by the
//! claiming device (it reads the heartbeat first), not by the fold. What the fold
//! *does* enforce, deterministically:
//!   * the claim is signed by the device it names as holder (checked in
//!     `events::project_with_options`, which has the envelope);
//!   * the epoch moves forward (stale or replayed epochs are fenced);
//!   * during a planned handover, only the named successor may take the very
//!     next epoch (others can still take a later one if the successor dies).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::time_util::Timestamp;

/// Default lease length. Heartbeats are published well inside it.
pub const DEFAULT_LEASE_MS: u64 = 60_000;
/// How often a holder renews (beats) its lease by default.
pub const DEFAULT_BEAT_EVERY_MS: u64 = 15_000;
/// Bound on the fenced-claim history kept per scope (diagnostics only).
const MAX_FENCED_HISTORY: usize = 8;
/// Furthest a claim may jump ahead of the current epoch (or of zero, for the
/// first claim). Honest devices claim `current + 1`; a rogue claim of
/// `u64::MAX` would otherwise freeze the scope forever, since nothing can be
/// strictly greater, and overflow every successor's `epoch + 1`.
pub const MAX_EPOCH_STEP: u64 = 1024;

/// What a lease coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LeaseScope {
    /// The workspace's primary coordinator (e.g. a serve box).
    Workspace,
    /// The device driving one workflow run.
    WorkflowRun,
}

impl LeaseScope {
    pub fn as_str(self) -> &'static str {
        match self {
            LeaseScope::Workspace => "workspace",
            LeaseScope::WorkflowRun => "workflow_run",
        }
    }
}

/// Why a claim was made. Informational; the fold treats them alike except that
/// a handover claim is what a reserved successor normally sends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LeaseClaimReason {
    /// First holder of the scope.
    #[default]
    Initial,
    /// Succession after the previous holder's heartbeat lapsed (emergency path).
    Lapsed,
    /// Planned handover from a live holder (supersedure).
    Handover,
    /// The previous holder released without naming a successor.
    Released,
}

/// A lease claim: `holder` coordinates `scope`/`scope_id` from `epoch` on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoordinatorLease {
    pub scope: LeaseScope,
    pub scope_id: String,
    /// Device id of the holder (never a person).
    pub holder: Uuid,
    pub epoch: u64,
    pub lease_ms: u64,
    #[serde(default)]
    pub reason: LeaseClaimReason,
    #[serde(default)]
    pub claimed_at: Timestamp,
}

/// The holder gives the lease up, optionally naming the successor that may take
/// the next epoch (planned handover). Signed by the holder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseRelease {
    pub scope: LeaseScope,
    pub scope_id: String,
    pub holder: Uuid,
    pub epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub successor: Option<Uuid>,
}

/// A claim the fold refused, kept for the split-brain view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FencedClaim {
    pub holder: Uuid,
    pub epoch: u64,
    pub verdict: LeaseVerdict,
}

/// The folded lease of one scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseState {
    pub current: CoordinatorLease,
    /// The holder released the lease (no live coordinator until the next claim).
    #[serde(default)]
    pub released: bool,
    /// Planned handover: only this device may take `current.epoch + 1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handover_to: Option<Uuid>,
    /// Recently fenced claims (newest last, bounded).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fenced: Vec<FencedClaim>,
}

/// Outcome of folding a claim or release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LeaseVerdict {
    Accepted,
    /// The epoch is not ahead of the current one: a stale holder or a replay.
    StaleEpoch,
    /// The next epoch is reserved for a named successor.
    ReservedForSuccessor,
    /// A release by someone other than the current holder, or for an old epoch.
    NotHolder,
    /// Malformed (empty scope id, zero epoch or zero lease length).
    Invalid,
}

/// Map key of a scope.
pub fn lease_key(scope: LeaseScope, scope_id: &str) -> String {
    format!("{}:{}", scope.as_str(), scope_id)
}

/// All leases folded on one log, keyed by [`lease_key`].
pub type LeaseTable = BTreeMap<String, LeaseState>;

fn record_fenced(state: &mut LeaseState, claim: &CoordinatorLease, verdict: LeaseVerdict) {
    state.fenced.push(FencedClaim { holder: claim.holder, epoch: claim.epoch, verdict });
    if state.fenced.len() > MAX_FENCED_HISTORY {
        let drop = state.fenced.len() - MAX_FENCED_HISTORY;
        state.fenced.drain(..drop);
    }
}

/// Fold a claim into `table`. Deterministic: a pure function of the table and
/// the claim, applied in canonical order on every device.
pub fn fold_claim(table: &mut LeaseTable, claim: &CoordinatorLease) -> LeaseVerdict {
    if claim.scope_id.is_empty() || claim.epoch == 0 || claim.lease_ms == 0 {
        return LeaseVerdict::Invalid;
    }
    let key = lease_key(claim.scope, &claim.scope_id);
    let Some(state) = table.get_mut(&key) else {
        if claim.epoch > MAX_EPOCH_STEP {
            return LeaseVerdict::Invalid;
        }
        table.insert(
            key,
            LeaseState { current: claim.clone(), released: false, handover_to: None, fenced: Vec::new() },
        );
        return LeaseVerdict::Accepted;
    };
    if claim.epoch <= state.current.epoch {
        record_fenced(state, claim, LeaseVerdict::StaleEpoch);
        return LeaseVerdict::StaleEpoch;
    }
    if claim.epoch - state.current.epoch > MAX_EPOCH_STEP {
        record_fenced(state, claim, LeaseVerdict::Invalid);
        return LeaseVerdict::Invalid;
    }
    if let Some(successor) = state.handover_to {
        if claim.epoch == state.current.epoch + 1 && claim.holder != successor {
            record_fenced(state, claim, LeaseVerdict::ReservedForSuccessor);
            return LeaseVerdict::ReservedForSuccessor;
        }
    }
    let fenced = std::mem::take(&mut state.fenced);
    *state = LeaseState { current: claim.clone(), released: false, handover_to: None, fenced };
    LeaseVerdict::Accepted
}

/// Fold a release. Only the current holder may release its current epoch.
pub fn fold_release(table: &mut LeaseTable, release: &LeaseRelease) -> LeaseVerdict {
    let key = lease_key(release.scope, &release.scope_id);
    let Some(state) = table.get_mut(&key) else { return LeaseVerdict::NotHolder };
    if state.current.holder != release.holder || state.current.epoch != release.epoch {
        return LeaseVerdict::NotHolder;
    }
    state.released = true;
    state.handover_to = release.successor;
    LeaseVerdict::Accepted
}

/// Whether a write stamped with `epoch` by a coordinator of `scope`/`scope_id`
/// is still current. `None` (an unstamped or pre-lease write) is not fenced.
pub fn write_is_current(table: &LeaseTable, scope: LeaseScope, scope_id: &str, epoch: Option<u64>) -> bool {
    match (epoch, table.get(&lease_key(scope, scope_id))) {
        (Some(e), Some(state)) => e >= state.current.epoch,
        _ => true,
    }
}

/// Liveness of a lease as seen by a reader at `now_ms`, given the newest
/// verified heartbeat (holder, epoch, unix ms).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LeaseHealth {
    /// Beat within the lease.
    Live,
    /// Beat late but within the lease: the holder is slow (supersedure candidate).
    Degraded,
    /// No beat within the lease: succession may start.
    Lapsed,
    /// Released by its holder.
    Released,
}

/// A heartbeat is "late" once it is older than this share of the lease.
const DEGRADED_SHARE: f64 = 0.5;

/// Classify a lease's health from its last verified beat. A beat for an older
/// epoch or another holder does not renew the current lease.
pub fn lease_health(state: &LeaseState, last_beat: Option<(Uuid, u64, i64)>, now_ms: i64) -> LeaseHealth {
    if state.released {
        return LeaseHealth::Released;
    }
    let lease = state.current.lease_ms as i64;
    // The claim itself counts as the first beat.
    let claimed_ms = state.current.claimed_at.inner().unix_timestamp() * 1000;
    let beat_ms = match last_beat {
        Some((holder, epoch, at)) if holder == state.current.holder && epoch == state.current.epoch => {
            at.max(claimed_ms)
        }
        _ => claimed_ms,
    };
    let age = now_ms - beat_ms;
    if age > lease {
        LeaseHealth::Lapsed
    } else if age as f64 > lease as f64 * DEGRADED_SHARE {
        LeaseHealth::Degraded
    } else {
        LeaseHealth::Live
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(holder: Uuid, epoch: u64) -> CoordinatorLease {
        CoordinatorLease {
            scope: LeaseScope::WorkflowRun,
            scope_id: "run-1".into(),
            holder,
            epoch,
            lease_ms: DEFAULT_LEASE_MS,
            reason: LeaseClaimReason::Initial,
            claimed_at: Timestamp::now(),
        }
    }

    #[test]
    fn first_claim_wins_and_epochs_move_forward() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut t = LeaseTable::new();
        assert_eq!(fold_claim(&mut t, &claim(a, 1)), LeaseVerdict::Accepted);
        // Split brain: b also claims epoch 1 — fenced.
        assert_eq!(fold_claim(&mut t, &claim(b, 1)), LeaseVerdict::StaleEpoch);
        assert_eq!(t[&lease_key(LeaseScope::WorkflowRun, "run-1")].current.holder, a);
        // Succession at epoch 2.
        assert_eq!(fold_claim(&mut t, &claim(b, 2)), LeaseVerdict::Accepted);
        // The old holder renewing its old epoch is fenced.
        assert_eq!(fold_claim(&mut t, &claim(a, 2)), LeaseVerdict::StaleEpoch);
        assert_eq!(fold_claim(&mut t, &claim(a, 1)), LeaseVerdict::StaleEpoch);
        let st = &t[&lease_key(LeaseScope::WorkflowRun, "run-1")];
        assert_eq!((st.current.holder, st.current.epoch), (b, 2));
        assert_eq!(st.fenced.len(), 3);
    }

    #[test]
    fn invalid_claims_are_refused() {
        let mut t = LeaseTable::new();
        let mut c = claim(Uuid::new_v4(), 0);
        assert_eq!(fold_claim(&mut t, &c), LeaseVerdict::Invalid);
        c.epoch = 1;
        c.lease_ms = 0;
        assert_eq!(fold_claim(&mut t, &c), LeaseVerdict::Invalid);
        assert!(t.is_empty());
    }

    #[test]
    fn handover_reserves_the_next_epoch_for_the_successor() {
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut t = LeaseTable::new();
        fold_claim(&mut t, &claim(a, 1));
        let rel = LeaseRelease {
            scope: LeaseScope::WorkflowRun,
            scope_id: "run-1".into(),
            holder: a,
            epoch: 1,
            successor: Some(b),
        };
        // Only the holder can release.
        let mut forged = rel.clone();
        forged.holder = c;
        assert_eq!(fold_release(&mut t, &forged), LeaseVerdict::NotHolder);
        assert_eq!(fold_release(&mut t, &rel), LeaseVerdict::Accepted);
        // A third device racing for epoch 2 loses to the reserved successor.
        assert_eq!(fold_claim(&mut t, &claim(c, 2)), LeaseVerdict::ReservedForSuccessor);
        assert_eq!(fold_claim(&mut t, &claim(b, 2)), LeaseVerdict::Accepted);
        // If the successor had died, epoch 3 would be open to anyone.
        assert_eq!(fold_claim(&mut t, &claim(c, 3)), LeaseVerdict::Accepted);
    }

    #[test]
    fn writes_under_an_old_epoch_are_not_current() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut t = LeaseTable::new();
        fold_claim(&mut t, &claim(a, 1));
        assert!(write_is_current(&t, LeaseScope::WorkflowRun, "run-1", Some(1)));
        fold_claim(&mut t, &claim(b, 2));
        assert!(!write_is_current(&t, LeaseScope::WorkflowRun, "run-1", Some(1)));
        assert!(write_is_current(&t, LeaseScope::WorkflowRun, "run-1", Some(2)));
        // Unstamped (legacy) writes and unknown scopes are never fenced.
        assert!(write_is_current(&t, LeaseScope::WorkflowRun, "run-1", None));
        assert!(write_is_current(&t, LeaseScope::WorkflowRun, "other", Some(1)));
    }

    #[test]
    fn health_from_beats() {
        let a = Uuid::new_v4();
        let mut t = LeaseTable::new();
        let mut c = claim(a, 3);
        c.claimed_at = Timestamp::from_unix_seconds(1_000);
        fold_claim(&mut t, &c);
        let st = t.values().next().unwrap().clone();
        let base = 1_000_000;
        assert_eq!(lease_health(&st, None, base + 10_000), LeaseHealth::Live);
        assert_eq!(lease_health(&st, None, base + 40_000), LeaseHealth::Degraded);
        assert_eq!(lease_health(&st, None, base + 61_000), LeaseHealth::Lapsed);
        // A fresh beat for the current epoch renews it.
        assert_eq!(lease_health(&st, Some((a, 3, base + 60_000)), base + 61_000), LeaseHealth::Live);
        // A beat for an old epoch or from someone else does not.
        assert_eq!(lease_health(&st, Some((a, 2, base + 60_000)), base + 61_000), LeaseHealth::Lapsed);
        assert_eq!(lease_health(&st, Some((Uuid::new_v4(), 3, base + 60_000)), base + 61_000), LeaseHealth::Lapsed);
    }
}

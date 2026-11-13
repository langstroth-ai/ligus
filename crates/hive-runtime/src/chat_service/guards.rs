//! Guard operations on [`ChatService`]: coordinator leases and the device-local
//! guard signal log. A child module so it can sign with the device key without
//! widening `ChatService`'s public surface.

use hive_core::lease::{
    lease_key, CoordinatorLease, LeaseClaimReason, LeaseRelease, LeaseScope, LeaseState,
};
use hive_core::signal::{GuardSignal, GuardSignalRecord};
use hive_core::{SessionEvent, Timestamp};
use uuid::Uuid;

use super::{ChatService, Result};
use crate::envelope_verifier::{build_roster, DeviceKeyResolver};
use crate::lease_presence::LeaseBeat;

impl ChatService {
    /// The folded lease for `scope`/`scope_id` on `session_id`'s log.
    pub fn lease(&self, session_id: Uuid, scope: LeaseScope, scope_id: &str) -> Result<Option<LeaseState>> {
        Ok(self
            .load(session_id)?
            .and_then(|s| s.guards.leases.get(&lease_key(scope, scope_id)).cloned()))
    }

    /// Claim the lease for `scope`/`scope_id` at `epoch` as this device, on the
    /// log at `session_id`. Returns the lease as folded afterwards: the caller
    /// holds it only if `current.holder == device_id()` and the epoch matches —
    /// a concurrent claim already folded locally wins otherwise.
    #[allow(clippy::too_many_arguments)]
    pub fn claim_lease(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        scope: LeaseScope,
        scope_id: &str,
        epoch: u64,
        lease_ms: u64,
        reason: LeaseClaimReason,
    ) -> Result<Option<LeaseState>> {
        let lease = CoordinatorLease {
            scope,
            scope_id: scope_id.to_string(),
            holder: self.device_id,
            epoch,
            lease_ms,
            reason,
            claimed_at: Timestamp::now(),
        };
        self.append_signed(session_id, workspace_id, SessionEvent::CoordinatorLeaseClaimed { lease })?;
        self.lease(session_id, scope, scope_id)
    }

    /// Release a lease this device holds at `epoch`, optionally reserving the
    /// next epoch for `successor` (planned handover).
    pub fn release_lease(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        scope: LeaseScope,
        scope_id: &str,
        epoch: u64,
        successor: Option<Uuid>,
    ) -> Result<()> {
        let release = LeaseRelease {
            scope,
            scope_id: scope_id.to_string(),
            holder: self.device_id,
            epoch,
            successor,
        };
        self.append_signed(session_id, workspace_id, SessionEvent::CoordinatorLeaseReleased { release })?;
        Ok(())
    }

    /// Sign a lease heartbeat with this device's key.
    pub fn sign_lease_beat(&self, beat: LeaseBeat) -> LeaseBeat {
        beat.sign(&self.keypair)
    }

    /// The certified signing key of `device` in `workspace_id`'s trust roster,
    /// or `None` if the device has no verified certificate (or is revoked).
    pub fn certified_device_key(&self, workspace_id: Uuid, device: Uuid) -> Result<Option<Vec<u8>>> {
        let roster = build_roster(&self.store.trust_envelopes(workspace_id)?);
        if roster.is_revoked(device) {
            return Ok(None);
        }
        Ok(roster.public_key(device).map(<[u8]>::to_vec))
    }

    /// The envelope's secret redaction, applied to agent output on its way into
    /// the log: credentials are masked, and the catch is recorded as a
    /// content-free hygiene signal.
    pub(super) fn redact_for_log(&self, workspace_id: Uuid, session_id: Uuid, text: String) -> String {
        let (masked, n) = crate::guard_scan::redact_secrets(&text);
        if n == 0 {
            return text;
        }
        let _ = self.record_signal(
            workspace_id,
            Some(session_id),
            &GuardSignal::Hygiene { hygiene_kind: hive_core::signal::HygieneKind::Redaction, count: n },
        );
        masked
    }

    /// Record a device-local guard signal (never synced).
    pub fn record_signal(&self, workspace_id: Uuid, session_id: Option<Uuid>, signal: &GuardSignal) -> Result<()> {
        self.store.record_signal(workspace_id, session_id, signal)?;
        Ok(())
    }

    /// Device-local guard signals for a workspace after `after_row`.
    pub fn list_signals(
        &self,
        workspace_id: Uuid,
        after_row: i64,
        kind: Option<&str>,
        limit: usize,
    ) -> Result<Vec<GuardSignalRecord>> {
        Ok(self.store.list_signals(workspace_id, after_row, kind, limit)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::lease::{LeaseScope, DEFAULT_LEASE_MS};
    use hive_core::signal::HygieneKind;

    fn service() -> ChatService {
        let store = crate::event_store::EventStore::open_in_memory().unwrap();
        let kp = hive_core::SigningKeypair::generate().unwrap();
        let account_kp = hive_core::SigningKeypair::generate().unwrap();
        let author = hive_core::ActorIdentity::new("ada", "Ada", hive_core::ActorKind::Human);
        ChatService::new(store, Uuid::new_v4(), kp, account_kp, author)
    }

    #[test]
    fn claim_release_and_handover_round_trip_through_the_store() {
        let mut svc = service();
        let ws = Uuid::new_v4();
        let chat = svc.create_chat("c", ws, "anthropic").unwrap();
        let me = svc.device_id();
        let st = svc
            .claim_lease(chat.id, ws, LeaseScope::WorkflowRun, "run-1", 1, DEFAULT_LEASE_MS, LeaseClaimReason::Initial)
            .unwrap()
            .unwrap();
        assert_eq!((st.current.holder, st.current.epoch), (me, 1));
        // A replayed claim at the same epoch is fenced, not an error.
        let st = svc
            .claim_lease(chat.id, ws, LeaseScope::WorkflowRun, "run-1", 1, DEFAULT_LEASE_MS, LeaseClaimReason::Initial)
            .unwrap()
            .unwrap();
        assert_eq!(st.fenced.len(), 1);
        let succ = Uuid::new_v4();
        svc.release_lease(chat.id, ws, LeaseScope::WorkflowRun, "run-1", 1, Some(succ)).unwrap();
        let st = svc.lease(chat.id, LeaseScope::WorkflowRun, "run-1").unwrap().unwrap();
        assert!(st.released);
        assert_eq!(st.handover_to, Some(succ));
        // This device is not the reserved successor: its epoch-2 claim is fenced.
        let st = svc
            .claim_lease(chat.id, ws, LeaseScope::WorkflowRun, "run-1", 2, DEFAULT_LEASE_MS, LeaseClaimReason::Lapsed)
            .unwrap()
            .unwrap();
        assert_eq!(st.current.epoch, 1);
    }

    #[test]
    fn agent_output_is_redacted_before_it_reaches_the_log() {
        let mut svc = service();
        let ws = Uuid::new_v4();
        let chat = svc.create_chat("c", ws, "anthropic").unwrap();
        let mid = svc.begin_assistant_message(chat.id, ws, "Agent", "anthropic", None, None).unwrap();
        svc.append_chunk(chat.id, ws, mid, "key AKIAIOSFODNN7EXAMPLE ").unwrap();
        svc.complete_assistant_message(chat.id, ws, mid, "final AKIAIOSFODNN7EXAMPLE").unwrap();
        let raw: String = svc
            .store()
            .list(chat.id)
            .unwrap()
            .iter()
            .map(|e| serde_json::to_string(e).unwrap())
            .collect();
        assert!(!raw.contains("AKIAIOSFODNN7EXAMPLE"), "no credential in any stored (and so synced) event");
        let s = svc.load(chat.id).unwrap().unwrap();
        assert!(s.messages.iter().any(|m| m.body.contains("[redacted aws_access_key]")));
        assert_eq!(svc.list_signals(ws, 0, Some("hygiene"), 10).unwrap().len(), 2);
    }

    #[test]
    fn signals_are_local_bounded_and_filterable() {
        let svc = service();
        let ws = Uuid::new_v4();
        for i in 0..5 {
            svc.record_signal(ws, None, &GuardSignal::Hygiene { hygiene_kind: HygieneKind::Redaction, count: i }).unwrap();
        }
        svc.record_signal(Uuid::new_v4(), None, &GuardSignal::Hygiene { hygiene_kind: HygieneKind::Redaction, count: 9 }).unwrap();
        let all = svc.list_signals(ws, 0, None, 100).unwrap();
        assert_eq!(all.len(), 5);
        let after = svc.list_signals(ws, all[2].row_id, None, 100).unwrap();
        assert_eq!(after.len(), 2);
        assert!(svc.list_signals(ws, 0, Some("progress"), 100).unwrap().is_empty());
        // Nothing reached the synced event log.
        assert!(svc.store().list_session_ids().unwrap().is_empty());
    }
}

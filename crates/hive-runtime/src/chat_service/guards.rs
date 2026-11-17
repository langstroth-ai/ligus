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

    /// The workspace's guard state, from its config log (dearth mode, pending
    /// joins). Default when the config log doesn't exist yet.
    pub fn workspace_guards(&self, workspace_id: Uuid) -> Result<hive_core::guard::GuardState> {
        Ok(self
            .load(hive_core::workspace_config_session_id(workspace_id))?
            .map(|s| s.guards)
            .unwrap_or_default())
    }

    /// Switch dearth mode. When switching on, every device trusted right now is
    /// admitted; any other needs a second Admin approval. Admin+ (authorized
    /// here and again on every device's ingest).
    pub fn set_dearth_mode(&mut self, workspace_id: Uuid, mut dearth: hive_core::guard::DearthMode) -> Result<()> {
        if dearth.enabled && dearth.admitted_devices.is_empty() {
            let roster = build_roster(&self.store.trust_envelopes(workspace_id)?);
            // Only devices of current members (the authorization-checked
            // config roster): the verification roster also certifies a
            // keyholder whose self-enrollment the projection refused.
            let members: Vec<Uuid> = self
                .load(hive_core::workspace_config_session_id(workspace_id))?
                .map(|c| c.members.iter().filter_map(|m| m.actor.account_id).collect())
                .unwrap_or_default();
            dearth.admitted_devices = roster
                .certified_devices()
                .into_iter()
                .filter(|d| members.iter().any(|a| roster.device_signs_for(*d, *a)))
                .collect();
            // This device is always admitted by the Admin switching it on.
            if !dearth.admitted_devices.contains(&self.device_id) {
                dearth.admitted_devices.push(self.device_id);
            }
        }
        dearth.set_by = self.author.id.clone();
        dearth.set_at = Timestamp::now();
        let config_id = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(config_id, workspace_id, SessionEvent::DearthModeChanged { dearth })?;
        Ok(())
    }

    /// Approve a pending join (a member id or a device id) as this Admin.
    pub fn approve_join(&mut self, workspace_id: Uuid, subject: &str) -> Result<()> {
        let config_id = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(config_id, workspace_id, SessionEvent::JoinApproved { subject: subject.to_string() })?;
        Ok(())
    }

    /// Devices certified during dearth mode still waiting for a second
    /// approval, as `(device, account)`.
    pub fn pending_devices(&self, workspace_id: Uuid) -> Result<Vec<(Uuid, Uuid)>> {
        let roster = build_roster(&self.store.trust_envelopes(workspace_id)?);
        Ok(roster
            .pending_devices()
            .iter()
            .filter(|(ws, ..)| *ws == workspace_id)
            .map(|(_, d, a)| (*d, *a))
            .collect())
    }

    /// Self-medication: lock `chat_id` down for each finding (signal +
    /// content-free detail). `message_id` is the reply whose turn showed the
    /// signals (none for a manual lockdown), kept so the UI can jump to the
    /// cause. Posts one note in the chat so members see why, and records
    /// hygiene signals. Returns how many lockdowns were raised.
    pub fn self_medicate(
        &mut self,
        workspace_id: Uuid,
        chat_id: Uuid,
        message_id: Option<Uuid>,
        findings: &[(hive_core::guard::LockdownSignal, String)],
    ) -> Result<usize> {
        use hive_core::guard::{ChatLockdown, LockdownSignal};
        use hive_core::signal::HygieneKind;
        if findings.is_empty() {
            return Ok(0);
        }
        let config_id = self.ensure_workspace_config(workspace_id)?;
        let mut notes = Vec::new();
        for (signal, detail) in findings {
            let lockdown = ChatLockdown {
                id: Uuid::new_v4(),
                signal: *signal,
                detail: detail.chars().take(120).collect(),
                raised_by: self.author.id.clone(),
                raised_at: Timestamp::now(),
                message_id,
            };
            self.append_signed(config_id, workspace_id, SessionEvent::ChatLockdownRaised { chat_id, lockdown })?;
            let (kind, text) = match signal {
                LockdownSignal::InjectionDetected => (HygieneKind::InjectionDetected, format!("a prompt-injection marker in content an agent read ({detail})")),
                LockdownSignal::SecretInDiff => (HygieneKind::SecretInDiff, format!("a credential in an agent's changes ({detail})")),
                LockdownSignal::CiConfigChanged => (HygieneKind::CiConfigChanged, format!("an unrequested change to CI configuration ({detail})")),
                LockdownSignal::Manual => (HygieneKind::EnvelopeTightened, "a member locked it".to_string()),
            };
            let _ = self.record_signal(workspace_id, Some(chat_id), &GuardSignal::Hygiene { hygiene_kind: kind, count: 1 });
            notes.push(text);
        }
        let _ = self.record_signal(
            workspace_id,
            Some(chat_id),
            &GuardSignal::Hygiene { hygiene_kind: hive_core::signal::HygieneKind::EnvelopeTightened, count: 1 },
        );
        if self.load(chat_id)?.is_some() {
            self.post_system_note(
                chat_id,
                workspace_id,
                format!(
                    "This chat is locked down: {}. Agents here have no network or MCP access and every proposal needs a human member's approval until a member clears the lockdown.",
                    notes.join("; ")
                ),
            )?;
        }
        Ok(findings.len())
    }

    /// Clear a chat's lockdown (a human member, Contributor+).
    pub fn clear_lockdown(&mut self, workspace_id: Uuid, chat_id: Uuid) -> Result<()> {
        let config_id = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(config_id, workspace_id, SessionEvent::ChatLockdownCleared { chat_id })?;
        if self.load(chat_id)?.is_some() {
            let who = self.author.display_name.clone();
            self.post_system_note(chat_id, workspace_id, format!("{who} cleared this chat's lockdown."))?;
        }
        Ok(())
    }

    /// Mark an open proposal stale (its diff no longer applies) and take it out
    /// of the inbox; restoring it from the inbox un-dismisses it.
    pub fn mark_proposal_stale(&mut self, session_id: Uuid, workspace_id: Uuid, proposal_id: Uuid) -> Result<()> {
        self.append_signed(session_id, workspace_id, SessionEvent::ProposalMarkedStale { proposal_id })?;
        self.set_proposals_dismissed(session_id, workspace_id, &[proposal_id], true)?;
        Ok(())
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
    fn dearth_round_trip_admits_current_devices_and_gates_on_role() {
        let mut svc = service();
        let ws = Uuid::new_v4();
        svc.ensure_workspace_config(ws).unwrap();
        let d = hive_core::guard::DearthMode {
            enabled: true,
            reason: "untrusted repo".into(),
            brood_paths: vec![".github/**".into()],
            allowed_mcp_servers: vec![],
            admitted_devices: vec![],
            set_by: String::new(),
            set_at: Timestamp::now(),
        };
        svc.set_dearth_mode(ws, d).unwrap();
        let g = svc.workspace_guards(ws).unwrap();
        let on = g.dearth_on().unwrap();
        assert!(on.admitted_devices.contains(&svc.device_id()));
        assert_eq!(on.set_by, svc.author().id);
        // A chat in the workspace sees it (overlay) and gets the tighter envelope.
        let chat = svc.create_chat("c", ws, "anthropic").unwrap();
        let s = svc.load(chat.id).unwrap().unwrap();
        assert!(s.guards.dearth_on().is_some());
        assert!(!hive_core::sandbox::envelope_for_session(&s).egress.is_open());
    }

    /// Hardening A-7: switching dearth mode on admits the devices trusted
    /// now. An outsider's uninvited self-enrollment (refused by the config
    /// projection, but certifying their device in the verification roster)
    /// must not ride into the admitted set.
    #[test]
    fn dearth_does_not_admit_an_uninvited_self_enrollment() {
        let mut svc = service();
        let ws = Uuid::new_v4();
        let cfg = svc.ensure_workspace_config(ws).unwrap();
        let (eve_acct, eve_dev) = (Uuid::new_v4(), Uuid::new_v4());
        let (acct_kp, dev_kp) = (hive_core::SigningKeypair::generate().unwrap(), hive_core::SigningKeypair::generate().unwrap());
        let mut actor = hive_core::ActorIdentity::new(eve_acct.to_string(), "eve", hive_core::ActorKind::Human);
        actor.account_id = Some(eve_acct);
        let cert = hive_core::DeviceCertificate::issue(&acct_kp, eve_acct, eve_dev, &dev_kp.public_key_bytes(), Timestamp::now());
        let member = hive_core::WorkspaceMember {
            id: eve_acct.to_string(),
            actor: actor.clone(),
            role: hive_core::WorkspaceRole::Contributor,
            title: String::new(),
            index: 0,
            joined_at: Timestamp::now(),
        };
        for (i, e) in [
            SessionEvent::AccountKeyRegistered { account_id: eve_acct, signing_public_key: acct_kp.public_key_bytes().to_vec() },
            SessionEvent::DeviceCertificateAdded { certificate: cert },
            SessionEvent::MemberAdded { member, invite: None },
        ]
        .into_iter()
        .enumerate()
        {
            let mut env = hive_core::SessionEventEnvelope::new(cfg, ws, 100 + i as i64, e);
            env.lamport = 100 + i as u64;
            env.actor_stamp = Some(hive_core::ActorStamp { actor: actor.clone(), recorded_at: Timestamp::now(), cfg_frontier: None, presence: None, runtime_effect: false, prev: None });
            hive_core::sign_envelope(&mut env, eve_dev, &dev_kp);
            svc.store.ingest(&env).unwrap();
        }
        assert!(!svc.load(cfg).unwrap().unwrap().members.iter().any(|m| m.id == eve_acct.to_string()), "precondition");
        let d = hive_core::guard::DearthMode {
            enabled: true,
            reason: "lock".into(),
            brood_paths: vec![],
            allowed_mcp_servers: vec![],
            admitted_devices: vec![],
            set_by: String::new(),
            set_at: Timestamp::now(),
        };
        svc.set_dearth_mode(ws, d).unwrap();
        let g = svc.workspace_guards(ws).unwrap();
        let on = g.dearth_on().unwrap();
        assert!(on.admitted_devices.contains(&svc.device_id()));
        assert!(!on.admitted_devices.contains(&eve_dev), "a non-member's device was admitted");
    }

    #[test]
    fn self_medication_locks_the_chat_until_a_member_clears_it() {
        use hive_core::guard::LockdownSignal;
        let mut svc = service();
        let ws = Uuid::new_v4();
        svc.ensure_workspace_config(ws).unwrap();
        let chat = svc.create_chat("c", ws, "anthropic").unwrap();
        let reply = Uuid::new_v4();
        let n = svc
            .self_medicate(ws, chat.id, Some(reply), &[(LockdownSignal::SecretInDiff, "aws_access_key".into())])
            .unwrap();
        assert_eq!(n, 1);
        let s = svc.load(chat.id).unwrap().unwrap();
        assert!(s.guards.lockdown_active());
        assert_eq!(s.guards.lockdowns_of(chat.id)[0].message_id, Some(reply), "the cause's reply is kept");
        assert_eq!(hive_core::sandbox::envelope_for_session(&s).egress, hive_core::sandbox::EgressPolicy::Closed);
        assert!(s.messages.iter().any(|m| m.body.contains("locked down")));
        // Other chats are untouched.
        let other = svc.create_chat("o", ws, "anthropic").unwrap();
        assert!(!svc.load(other.id).unwrap().unwrap().guards.lockdown_active());
        svc.clear_lockdown(ws, chat.id).unwrap();
        assert!(!svc.load(chat.id).unwrap().unwrap().guards.lockdown_active());
        assert!(!svc.list_signals(ws, 0, Some("hygiene"), 10).unwrap().is_empty());
    }

    fn named(name: &str) -> ChatService {
        let store = crate::event_store::EventStore::open_in_memory().unwrap();
        let author = hive_core::ActorIdentity::new(name, name, hive_core::ActorKind::Human);
        ChatService::new(
            store,
            Uuid::new_v4(),
            hive_core::SigningKeypair::generate().unwrap(),
            hive_core::SigningKeypair::generate().unwrap(),
            author,
        )
    }

    /// Deliver every event `from` has that `to` lacks (both directions = a sync).
    fn deliver(from: &ChatService, to: &mut ChatService) {
        for sid in from.store.list_session_ids().unwrap() {
            for env in from.store.list(sid).unwrap() {
                to.store.ingest(&env).unwrap();
            }
        }
    }

    fn run_write(svc: &mut ChatService, chat: Uuid, ws: Uuid, run_id: Uuid, input: &str, epoch: u64) {
        let def = hive_core::WorkflowDefinition {
            id: Uuid::nil(),
            name: "wf".into(),
            description: String::new(),
            input_label: None,
            nodes: Vec::new(),
            created_at: Timestamp::now(),
            disabled: false,
        };
        let mut run = hive_core::workflow::new_run(&def, input, "a");
        run.id = run_id;
        run.lease_epoch = Some(epoch);
        svc.upsert_workflow_run(chat, ws, run).unwrap();
    }

    #[test]
    fn two_devices_racing_for_the_same_epoch_converge_and_the_loser_is_fenced() {
        let (mut a, mut b) = (named("ada"), named("bea"));
        let ws = Uuid::new_v4();
        a.ensure_workspace_config(ws).unwrap();
        let chat = a.create_chat("c", ws, "anthropic").unwrap().id;
        let bea = hive_core::WorkspaceMember {
            id: "bea".into(),
            actor: b.author().clone(),
            role: hive_core::WorkspaceRole::Contributor,
            title: String::new(),
            index: 2,
            joined_at: Timestamp::now(),
        };
        a.add_member(chat, ws, bea).unwrap();
        let run = Uuid::new_v4().to_string();
        let run_id = Uuid::parse_str(&run).unwrap();
        a.claim_lease(chat, ws, LeaseScope::WorkflowRun, &run, 1, DEFAULT_LEASE_MS, LeaseClaimReason::Initial).unwrap();
        deliver(&a, &mut b);
        // Partition: both believe the lease lapsed and claim epoch 2, then write.
        let mine_b = b.claim_lease(chat, ws, LeaseScope::WorkflowRun, &run, 2, DEFAULT_LEASE_MS, LeaseClaimReason::Lapsed).unwrap().unwrap();
        assert_eq!(mine_b.current.holder, b.device_id(), "locally, each thinks it won");
        run_write(&mut b, chat, ws, run_id, "from-b", 2);
        let mine_a = a.claim_lease(chat, ws, LeaseScope::WorkflowRun, &run, 2, DEFAULT_LEASE_MS, LeaseClaimReason::Lapsed).unwrap().unwrap();
        // a still held epoch 1, so its own replay at 2 also folds locally.
        assert_eq!(mine_a.current.epoch, 2);
        run_write(&mut a, chat, ws, run_id, "from-a", 2);
        // Heal the partition.
        deliver(&a, &mut b);
        deliver(&b, &mut a);
        let la = a.lease(chat, LeaseScope::WorkflowRun, &run).unwrap().unwrap();
        let lb = b.lease(chat, LeaseScope::WorkflowRun, &run).unwrap().unwrap();
        assert_eq!((la.current.holder, la.current.epoch), (lb.current.holder, lb.current.epoch), "both devices agree");
        let winner_is_a = la.current.holder == a.device_id();
        let expected = if winner_is_a { "from-a" } else { "from-b" };
        for svc in [&a, &b] {
            let s = svc.load(chat).unwrap().unwrap();
            let r = s.workflow_runs.iter().find(|r| r.id == run_id).unwrap();
            assert_eq!(r.input, expected, "only the winner's write survives on every device");
        }
        // The loser can see it was fenced (its claim is recorded).
        assert!(!la.fenced.is_empty());
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

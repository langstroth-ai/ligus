//! Dearth mode in the trust roster: a device certified while the entrance is
//! closed needs a second approval from an Admin of another account.

use hive_core::guard::DearthMode;
use hive_core::{
    workspace_config_session_id, ActorIdentity, ActorKind, ActorStamp, ChatSession, DeviceCertificate, SessionEvent,
    SessionEventEnvelope, SigningKeypair, Timestamp, WorkspaceMember, WorkspaceRole,
};
use uuid::Uuid;

use crate::envelope_verifier::{build_roster, DeviceKeyResolver};

struct Who {
    id: String,
    account: Uuid,
    kp: SigningKeypair,
    /// The device this person signs with (certified at setup).
    device: Uuid,
    dk: SigningKeypair,
}

fn who(name: &str) -> Who {
    Who {
        id: name.into(),
        account: Uuid::new_v4(),
        kp: SigningKeypair::generate().unwrap(),
        device: Uuid::new_v4(),
        dk: SigningKeypair::generate().unwrap(),
    }
}

fn actor(w: &Who) -> ActorIdentity {
    let mut a = ActorIdentity::new(w.id.clone(), w.id.clone(), ActorKind::Human);
    a.account_id = Some(w.account);
    a
}

/// A config log written by each person's signing device; people whose test
/// recovery key is registered stamp its presence (rules v2 from the seed).
struct Log {
    ws: Uuid,
    lamport: u64,
    envs: Vec<SessionEventEnvelope>,
    present: std::collections::BTreeSet<Uuid>,
    signing_devices: Vec<Uuid>,
}

impl Log {
    fn draft(&mut self, by: &Who, payload: SessionEvent) -> SessionEventEnvelope {
        self.lamport += 1;
        let mut e = SessionEventEnvelope::new(workspace_config_session_id(self.ws), self.ws, self.lamport as i64, payload);
        e.lamport = self.lamport;
        e.actor_stamp = Some(ActorStamp { actor: actor(by), recorded_at: Timestamp::epoch(), cfg_frontier: None, presence: None, runtime_effect: false, prev: None });
        hive_core::sign_envelope(&mut e, by.device, &by.dk);
        if self.present.contains(&by.account) {
            hive_core::testkit::prove_recovery(&mut e, by.account, by.device, &by.dk);
        }
        e
    }
    fn push(&mut self, by: &Who, mut payload: SessionEvent) {
        // Switching dearth mode on admits everyone's signing device (the
        // setup's), as the tests' `admitted` lists did before they existed.
        if let SessionEvent::DearthModeChanged { dearth } = &mut payload {
            if dearth.enabled {
                dearth.admitted_devices.extend(self.signing_devices.iter().copied());
            }
        }
        let e = self.draft(by, payload);
        self.envs.push(e);
    }
    /// `w` publishes their account key and signing device, and registers
    /// their test recovery key.
    fn identity(&mut self, w: &Who) {
        self.push(w, SessionEvent::AccountKeyRegistered { account_id: w.account, signing_public_key: w.kp.public_key_bytes().to_vec() });
        let c = DeviceCertificate::issue(&w.kp, w.account, w.device, &w.dk.public_key_bytes(), Timestamp::epoch());
        self.push(w, SessionEvent::DeviceCertificateAdded { certificate: c });
        let mut e = self.draft(w, hive_core::testkit::recovery_registration(w.account));
        hive_core::testkit::prove_recovery(&mut e, w.account, w.device, &w.dk);
        self.envs.push(e);
        self.present.insert(w.account);
        self.signing_devices.push(w.device);
    }
    fn cert(&mut self, owner: &Who, device: Uuid) -> SigningKeypair {
        let dk = SigningKeypair::generate().unwrap();
        let c = DeviceCertificate::issue(&owner.kp, owner.account, device, &dk.public_key_bytes(), Timestamp::epoch());
        self.push(owner, SessionEvent::DeviceCertificateAdded { certificate: c });
        dk
    }
}

fn dearth(on: bool, admitted: Vec<Uuid>) -> SessionEvent {
    SessionEvent::DearthModeChanged {
        dearth: DearthMode {
            enabled: on,
            reason: String::new(),
            brood_paths: vec![],
            allowed_mcp_servers: vec![],
            admitted_devices: admitted,
            set_by: String::new(),
            set_at: Timestamp::epoch(),
        },
    }
}

fn setup() -> (Log, Who, Who, Who, Uuid) {
    let (owner, admin2, alice) = (who("owner"), who("admin2"), who("alice"));
    let ws = Uuid::new_v4();
    let mut cfg = ChatSession::new("", ws, "");
    cfg.id = workspace_config_session_id(ws);
    cfg.creator_actor_id = owner.id.clone();
    cfg.members.push(WorkspaceMember { id: owner.id.clone(), actor: actor(&owner), role: WorkspaceRole::Owner, title: String::new(), index: 0, joined_at: Timestamp::epoch() });
    let mut log = Log { ws, lamport: 0, envs: Vec::new(), present: Default::default(), signing_devices: Vec::new() };
    log.push(&owner, SessionEvent::SessionSnapshot { session: Box::new(cfg) });
    log.identity(&owner);
    for (w, role) in [(&admin2, WorkspaceRole::Admin), (&alice, WorkspaceRole::Contributor)] {
        let m = WorkspaceMember { id: w.id.clone(), actor: actor(w), role, title: String::new(), index: 0, joined_at: Timestamp::epoch() };
        log.push(&owner, SessionEvent::MemberAdded { member: m, invite: None });
        log.identity(w);
    }
    let laptop = Uuid::new_v4();
    log.cert(&owner, laptop);
    (log, owner, admin2, alice, laptop)
}

#[test]
fn a_device_certified_during_dearth_needs_another_admins_approval() {
    let (mut log, owner, admin2, alice, laptop) = setup();
    log.push(&owner, dearth(true, vec![laptop]));
    let new_box = Uuid::new_v4();
    log.cert(&owner, new_box);
    let r = build_roster(&log.envs);
    assert!(r.public_key(laptop).is_some(), "devices trusted before dearth mode stay trusted");
    assert!(r.public_key(new_box).is_none(), "the owner's certificate alone is one approval");
    assert_eq!(r.pending_devices(), &[(log.ws, new_box, owner.account)]);

    // The owner approving their own device again: still one account.
    log.push(&owner, SessionEvent::JoinApproved { subject: new_box.to_string() });
    // A Contributor's approval is dropped by ingest authorization.
    log.push(&alice, SessionEvent::JoinApproved { subject: new_box.to_string() });
    assert!(build_roster(&log.envs).public_key(new_box).is_none());

    // A second Admin, another account: admitted.
    log.push(&admin2, SessionEvent::JoinApproved { subject: new_box.to_string() });
    let r = build_roster(&log.envs);
    assert!(r.public_key(new_box).is_some());
    assert!(r.pending_devices().is_empty());
}

#[test]
fn a_contributor_cannot_switch_dearth_off_in_the_roster() {
    let (mut log, owner, _admin2, alice, laptop) = setup();
    log.push(&owner, dearth(true, vec![laptop]));
    log.push(&alice, dearth(false, vec![]));
    let new_box = Uuid::new_v4();
    log.cert(&alice, new_box);
    assert!(build_roster(&log.envs).public_key(new_box).is_none(), "bypass attempt: the forged switch-off is ignored");
    // An Admin switching it off lets the certificate count again.
    log.push(&owner, dearth(false, vec![]));
    assert!(build_roster(&log.envs).public_key(new_box).is_some());
}

#[test]
fn without_dearth_mode_one_certificate_is_enough() {
    let (mut log, owner, ..) = setup();
    let new_box = Uuid::new_v4();
    log.cert(&owner, new_box);
    assert!(build_roster(&log.envs).public_key(new_box).is_some());
}

/// Dearth mode in the default (Relay) identity mode: a device certified while
/// the entrance is closed and not yet approved by a second Admin must not
/// have its events accepted. It used to fall through to "unknown device",
/// which Relay mode accepts, so the second approval bound nothing.
#[test]
fn a_pending_device_is_not_accepted_in_relay_mode() {
    use crate::envelope_verifier::verdict_for;
    use crate::identity_verifier::{gate_ingest, IdentityCache, IdentityMode, IngestAction};
    let (mut log, owner, _admin2, _alice, laptop) = setup();
    log.push(&owner, dearth(true, vec![laptop]));
    let new_box = Uuid::new_v4();
    let dk = log.cert(&owner, new_box);
    // The pending device signs a chat message as the owner's account.
    let mut e = SessionEventEnvelope::new(Uuid::new_v4(), log.ws, 99, SessionEvent::SessionTitleChanged { title: "x".into() });
    e.lamport = 99;
    e.actor_stamp = Some(ActorStamp { actor: actor(&owner), recorded_at: Timestamp::epoch(), cfg_frontier: None, presence: None, runtime_effect: false, prev: None });
    hive_core::sign_envelope(&mut e, new_box, &dk);
    let mut envs = log.envs.clone();
    envs.push(e.clone());
    let roster = build_roster(&envs);
    assert_eq!(roster.pending_devices().len(), 1, "control: the device is pending");
    let action = gate_ingest(IdentityMode::Relay, verdict_for(&roster, &e), Some(owner.account), None, &IdentityCache::new());
    assert!(!matches!(action, IngestAction::Accept), "an unapproved dearth-mode device's event was accepted: {action:?}");
}

/// The same entrance, without publishing a certificate at all: a device the
/// roster has never seen (and an unsigned event) must not be accepted while
/// dearth mode is on in Relay mode either, or skipping the certificate is a
/// way around the second approval.
#[test]
fn an_uncertified_device_is_not_accepted_during_dearth_in_relay_mode() {
    use crate::envelope_verifier::verdict_for;
    use crate::identity_verifier::{gate_ingest, IdentityCache, IdentityMode, IngestAction};
    let (mut log, owner, _admin2, _alice, laptop) = setup();
    log.push(&owner, dearth(true, vec![laptop]));
    let stranger = SigningKeypair::generate().unwrap();
    let mut e = SessionEventEnvelope::new(Uuid::new_v4(), log.ws, 99, SessionEvent::SessionTitleChanged { title: "x".into() });
    e.lamport = 99;
    hive_core::sign_envelope(&mut e, Uuid::new_v4(), &stranger);
    let mut unsigned = SessionEventEnvelope::new(Uuid::new_v4(), log.ws, 98, SessionEvent::SessionTitleChanged { title: "y".into() });
    unsigned.lamport = 98;
    let roster = build_roster(&log.envs);
    for ev in [&e, &unsigned] {
        let action = gate_ingest(IdentityMode::Relay, verdict_for(&roster, ev), None, None, &IdentityCache::new());
        assert!(!matches!(action, IngestAction::Accept), "an unverifiable event was accepted during dearth mode: {action:?}");
    }
    // Control: with dearth mode off the lenient Relay policy is unchanged.
    let (log2, ..) = setup();
    let roster = build_roster(&log2.envs);
    let mut e2 = e.clone();
    e2.workspace_id = log2.ws;
    hive_core::sign_envelope(&mut e2, Uuid::new_v4(), &stranger);
    assert!(matches!(
        gate_ingest(IdentityMode::Relay, verdict_for(&roster, &e2), None, None, &IdentityCache::new()),
        IngestAction::Accept
    ));
}

/// An Admin removes Alice and revokes her laptop. Alice still holds an old
/// epoch's key, so she can still publish: she certifies a fresh device with
/// her account key and enrolls herself back on the config log. Projection
/// refuses the self-enrollment of a removed member, and the trust roster must
/// agree: her fresh device is not a trusted nestmate.
#[test]
fn a_removed_member_cannot_reenter_the_roster_with_a_fresh_device() {
    use crate::envelope_verifier::{verdict_for, Verdict};
    let (mut log, owner, _admin2, alice, _laptop) = setup();
    let alice_laptop = Uuid::new_v4();
    log.cert(&alice, alice_laptop);
    log.push(&owner, SessionEvent::MemberRemoved { member_id: alice.id.clone(), watermark: Vec::new() });
    log.push(&owner, SessionEvent::DeviceRevoked { device_id: alice_laptop, watermark: Vec::new() });
    // Alice comes back on her own.
    let fresh = Uuid::new_v4();
    let fk = log.cert(&alice, fresh);
    log.push(
        &alice,
        SessionEvent::MemberAdded { invite: None,
            member: WorkspaceMember { id: alice.id.clone(), actor: actor(&alice), role: WorkspaceRole::Contributor, title: String::new(), index: 0, joined_at: Timestamp::epoch() },
        },
    );
    let cfg = hive_core::project(&log.envs).unwrap();
    assert!(!cfg.members.iter().any(|m| m.id == alice.id), "control: projection refuses the self re-enrollment");
    let roster = build_roster(&log.envs);
    let mut e = SessionEventEnvelope::new(Uuid::new_v4(), log.ws, 99, SessionEvent::SessionTitleChanged { title: "x".into() });
    e.lamport = 99;
    e.actor_stamp = Some(ActorStamp { actor: actor(&alice), recorded_at: Timestamp::epoch(), cfg_frontier: None, presence: None, runtime_effect: false, prev: None });
    hive_core::sign_envelope(&mut e, fresh, &fk);
    assert_ne!(verdict_for(&roster, &e), Verdict::Valid, "the removed member's fresh device is trusted again");
}

/// The other direction: a `MemberRemoved` that projection refuses (a
/// Contributor "removing" the owner, or one in a chat log) must not revoke the
/// victim's devices in the trust roster. The roster used to drop the account
/// on any raw removal, so one unauthorized event quarantined every later
/// event of the workspace owner.
#[test]
fn an_unauthorized_removal_does_not_revoke_the_victims_devices() {
    use crate::envelope_verifier::{verdict_for, Verdict};
    let (mut log, owner, _admin2, alice, laptop) = setup();
    let owner_dk = {
        // Re-certify a device whose key we hold for the owner.
        let d = Uuid::new_v4();
        (d, log.cert(&owner, d))
    };
    log.push(&alice, SessionEvent::MemberRemoved { member_id: owner.id.clone(), watermark: Vec::new() });
    // Same thing, unsigned and unstamped, on a chat log.
    let mut chat_rm = SessionEventEnvelope::new(Uuid::new_v4(), log.ws, 70, SessionEvent::MemberRemoved { member_id: owner.id.clone(), watermark: Vec::new() });
    chat_rm.lamport = 70;
    log.envs.push(chat_rm);
    let cfg = hive_core::project(&log.envs).unwrap();
    assert!(cfg.members.iter().any(|m| m.id == owner.id), "control: projection refuses the removal");
    let roster = build_roster(&log.envs);
    assert!(!crate::envelope_verifier::DeviceKeyResolver::is_revoked(&roster, laptop), "the owner's laptop was revoked by an unauthorized removal");
    let mut e = SessionEventEnvelope::new(Uuid::new_v4(), log.ws, 99, SessionEvent::SessionTitleChanged { title: "x".into() });
    e.lamport = 99;
    e.actor_stamp = Some(ActorStamp { actor: actor(&owner), recorded_at: Timestamp::epoch(), cfg_frontier: None, presence: None, runtime_effect: false, prev: None });
    hive_core::sign_envelope(&mut e, owner_dk.0, &owner_dk.1);
    assert_eq!(verdict_for(&roster, &e), Verdict::Valid);
}

/// `DeviceRevoked` is Admin-only in projection, and the roster must agree: a
/// Contributor's revocation of the owner's laptop (or an unsigned one on a
/// chat log) must not cut the owner off.
#[test]
fn an_unauthorized_device_revocation_is_ignored() {
    use crate::envelope_verifier::DeviceKeyResolver;
    let (mut log, owner, admin2, alice, laptop) = setup();
    log.push(&alice, SessionEvent::DeviceRevoked { device_id: laptop, watermark: Vec::new() });
    let mut chat_rv = SessionEventEnvelope::new(Uuid::new_v4(), log.ws, 70, SessionEvent::DeviceRevoked { device_id: laptop, watermark: Vec::new() });
    chat_rv.lamport = 70;
    log.envs.push(chat_rv);
    assert!(!build_roster(&log.envs).is_revoked(laptop), "a Contributor revoked the owner's laptop");
    // An Admin's revocation still works.
    log.push(&admin2, SessionEvent::DeviceRevoked { device_id: laptop, watermark: Vec::new() });
    assert!(build_roster(&log.envs).is_revoked(laptop));
    let _ = owner;
}

/// Dearth mode under a founder pin (B2 residual): a member who forges a
/// lower-lamport config seed naming themself Owner must not be able to pick
/// the projection dearth mode is read from. The roster's own projection uses
/// the pin, so the dearth policy must too, or the forged base (with no dearth
/// switch in force) admits a device certified while the entrance is closed.
#[test]
fn a_forged_seed_cannot_switch_dearth_off_under_a_founder_pin() {
    use crate::envelope_verifier::{build_roster_pinned, FounderPin};
    let owner = who("");
    let owner = Who { id: owner.account.to_string(), ..owner };
    let alice = who("alice");
    let ws = Uuid::new_v4();
    let seed = |creator: &Who, members: &[(&Who, WorkspaceRole)]| {
        let mut cfg = ChatSession::new("", ws, "");
        cfg.id = workspace_config_session_id(ws);
        cfg.creator_actor_id = creator.id.clone();
        for (w, role) in members {
            cfg.members.push(WorkspaceMember { id: w.id.clone(), actor: actor(w), role: *role, title: String::new(), index: 0, joined_at: Timestamp::epoch() });
        }
        SessionEvent::SessionSnapshot { session: Box::new(cfg) }
    };
    let mut log = Log { ws, lamport: 0, envs: Vec::new(), present: Default::default(), signing_devices: Vec::new() };
    log.push(&owner, seed(&owner, &[(&owner, WorkspaceRole::Owner), (&alice, WorkspaceRole::Contributor)]));
    log.identity(&owner);
    log.push(&alice, SessionEvent::AccountKeyRegistered { account_id: alice.account, signing_public_key: alice.kp.public_key_bytes().to_vec() });
    let laptop = Uuid::new_v4();
    log.cert(&owner, laptop);
    log.push(&owner, dearth(true, vec![laptop]));
    // Alice forges a seed below the founder's, naming herself Owner.
    let mut forged = SessionEventEnvelope::new(workspace_config_session_id(ws), ws, 0, seed(&alice, &[(&alice, WorkspaceRole::Owner)]));
    forged.lamport = 0;
    forged.actor_stamp = Some(ActorStamp { actor: actor(&alice), recorded_at: Timestamp::epoch(), cfg_frontier: None, presence: None, runtime_effect: false, prev: None });
    log.envs.push(forged);
    let new_box = Uuid::new_v4();
    log.cert(&alice, new_box);
    let pin = FounderPin { founder_account: owner.account, fk: FounderPin::fingerprint(&owner.kp.public_key_bytes()) };
    let r = build_roster_pinned(&log.envs, Some(&pin));
    assert!(r.public_key(laptop).is_some(), "control: the founder's device is trusted");
    assert!(r.public_key(new_box).is_none(), "a forged seed switched dearth mode off for the pinned roster");
}

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
}

fn who(name: &str) -> Who {
    Who { id: name.into(), account: Uuid::new_v4(), kp: SigningKeypair::generate().unwrap() }
}

fn actor(w: &Who) -> ActorIdentity {
    let mut a = ActorIdentity::new(w.id.clone(), w.id.clone(), ActorKind::Human);
    a.account_id = Some(w.account);
    a
}

struct Log {
    ws: Uuid,
    lamport: u64,
    envs: Vec<SessionEventEnvelope>,
}

impl Log {
    fn push(&mut self, by: &Who, payload: SessionEvent) {
        self.lamport += 1;
        let mut e = SessionEventEnvelope::new(workspace_config_session_id(self.ws), self.ws, self.lamport as i64, payload);
        e.lamport = self.lamport;
        e.actor_stamp = Some(ActorStamp { actor: actor(by), recorded_at: Timestamp::epoch() });
        self.envs.push(e);
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
    for (w, role) in [(&owner, WorkspaceRole::Owner), (&admin2, WorkspaceRole::Admin), (&alice, WorkspaceRole::Contributor)] {
        cfg.members.push(WorkspaceMember { id: w.id.clone(), actor: actor(w), role, title: String::new(), index: 0, joined_at: Timestamp::epoch() });
    }
    let mut log = Log { ws, lamport: 0, envs: Vec::new() };
    log.push(&owner, SessionEvent::SessionSnapshot { session: Box::new(cfg) });
    for w in [&owner, &admin2, &alice] {
        log.push(w, SessionEvent::AccountKeyRegistered { account_id: w.account, signing_public_key: w.kp.public_key_bytes().to_vec() });
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
    e.actor_stamp = Some(ActorStamp { actor: actor(&owner), recorded_at: Timestamp::epoch() });
    hive_core::sign_envelope(&mut e, new_box, &dk);
    let mut envs = log.envs.clone();
    envs.push(e.clone());
    let roster = build_roster(&envs);
    assert_eq!(roster.pending_devices().len(), 1, "control: the device is pending");
    let action = gate_ingest(IdentityMode::Relay, verdict_for(&roster, &e), Some(owner.account), None, &IdentityCache::new());
    assert!(!matches!(action, IngestAction::Accept), "an unapproved dearth-mode device's event was accepted: {action:?}");
}

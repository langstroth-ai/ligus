//! The account-key dispute DoS (`hive_core::account_keys`), in the config
//! fold: a member who registers a second key under someone else's account id
//! (from a fresh device, from their own, with or without certifying that
//! device under the key, late or backdated before the victim's registration
//! or admission) is refused and attributed; the victim's key stands and
//! their events keep folding. Only the account's own lineage asserting two
//! keys disputes it.

use hive_core::crypto::{sign_envelope, DeviceCertificate, SigningKeypair};
use hive_core::events::Verdict;
use hive_core::testkit::{Proof, SignedWorld};
use hive_core::{CfgFrontier, SessionEvent, SessionEventEnvelope, WorkspaceRole};
use proptest::prelude::*;
use uuid::Uuid;

/// owner founds; eve (the attacker) is an Admin-added Contributor; vic joins
/// through an invite (self-enrollment, the normal join) and publishes her
/// identity. `ghost` has a device and a key nobody certified.
fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["owner", "eve", "vic", "ghost", "vic2"], None);
    w.admit("owner", "eve", WorkspaceRole::Contributor);
    let invite = Uuid::new_v4();
    w.write(
        "owner",
        None,
        SessionEvent::JoinApproved { subject: hive_core::authorization::invite_subject(invite), admission: None },
        None,
        Proof::Recovery,
    );
    let m = w.member_of("vic", WorkspaceRole::Contributor);
    let admitted = w.push_bare("vic", SessionEvent::MemberAdded { member: m, invite: Some(invite), proof: None });
    assert_eq!(w.verdict(admitted), Verdict::Allowed, "precondition: vic's self-enrollment is allowed");
    w.publish_identity("vic");
    w.register_recovery("vic");
    w
}

/// Where a forgery sorts.
#[derive(Debug, Clone, Copy)]
enum At {
    /// At the head (after everything).
    Late,
    /// Just before vic's own registration.
    BeforeRegistration,
    /// Before vic's admission.
    BeforeAdmission,
}

/// Who signs a forgery.
#[derive(Debug, Clone, Copy)]
enum By {
    /// A fresh device nobody certified ("ghost").
    Ghost,
    /// The attacker member's own certified device.
    Eve,
}

/// The config position just before the first config event matching `pred`.
fn head_before(w: &SignedWorld, pred: impl Fn(&SessionEventEnvelope) -> bool) -> Option<CfgFrontier> {
    let target = w.cfg.iter().find(|e| pred(e)).expect("target event").clone();
    w.cfg
        .iter()
        .filter(|e| e.canonical_key() < target.canonical_key())
        .map(|e| CfgFrontier { lamport: e.lamport, event_id: e.event_id })
        .max()
}

/// `payload` stamped as `victim` (account and actor), signed by `by`'s
/// device, at `at` (backdated: its log head an earlier event, its id sorting
/// first among equal lamports).
fn forge(w: &mut SignedWorld, by: &str, victim: &str, payload: SessionEvent, at: At) -> SessionEventEnvelope {
    let head = match at {
        At::Late => w.head(),
        At::BeforeRegistration => head_before(w, |e| {
            matches!(&e.payload, SessionEvent::AccountKeyRegistered { account_id, .. } if *account_id == w.people[victim].account_id)
        }),
        At::BeforeAdmission => head_before(w, |e| {
            matches!(&e.payload, SessionEvent::MemberAdded { member, .. } if member.actor.account_id == Some(w.people[victim].account_id))
        }),
    };
    let mut env = w.draft(by, None, payload, None);
    let p = &w.people[by];
    if let Some(s) = env.actor_stamp.as_mut() {
        let mut actor = w.people[victim].actor();
        actor.device_id = Some(p.device_id);
        s.actor = actor;
        s.prev = None;
        s.log_head = head;
    }
    env.lamport = hive_core::lamport::next(head);
    if !matches!(at, At::Late) {
        env.event_id = Uuid::from_u128(1 + env.event_id.as_u128() % 1000);
    }
    env.clear_raw();
    sign_envelope(&mut env, p.device_id, &p.device);
    env
}

/// The attack: register `key` for `victim`, optionally certifying the
/// signer's own device for `victim` under `key` first (self-evident).
fn attack(w: &mut SignedWorld, by: By, victim: &str, key: &SigningKeypair, with_cert: bool, at: At) -> Vec<SessionEventEnvelope> {
    let by = match by {
        By::Ghost => "ghost",
        By::Eve => "eve",
    };
    let mut out = Vec::new();
    if with_cert {
        let cert = DeviceCertificate::issue(key, w.people[victim].account_id, &w.people[by].device.public_key_bytes(), hive_core::Timestamp::epoch());
        out.push(forge(w, by, victim, SessionEvent::DeviceCertificateAdded { certificate: cert }, at));
    }
    let reg = SessionEvent::AccountKeyRegistered { account_id: w.people[victim].account_id, signing_public_key: key.public_key_bytes().to_vec() };
    out.push(forge(w, by, victim, reg, at));
    for e in &out {
        w.push_env(None, e.clone());
    }
    out
}

/// vic's key is hers, her device is certified for her, and her later events
/// fold; the attacker's device signs nothing as her.
fn assert_vic_unharmed(w: &mut SignedWorld, attacker_device: Uuid, what: &str) {
    let vic = w.people["vic"].account_id;
    let roster = w.roster();
    assert_eq!(roster.account_keys.get(&vic), Some(&w.people["vic"].account.public_key_bytes().to_vec()), "{what}: vic's key");
    assert!(roster.account_certified(vic, w.people["vic"].device_id), "{what}: vic's device");
    assert!(!roster.account_certified(vic, attacker_device), "{what}: the attacker's device signs as vic");
    let chat = w.chat("vic", "after");
    let said = w.say(chat, "vic", SessionEvent::SessionTitleChanged { title: "still here".into() });
    assert_eq!(w.verdict(said), Verdict::Allowed, "{what}: vic's event folds");
    assert_eq!(w.state(chat).title, "still here", "{what}");
}

#[test]
fn a_registration_of_someone_elses_account_is_refused_wherever_it_sorts() {
    let base = world();
    for by in [By::Ghost, By::Eve] {
        for with_cert in [false, true] {
            for at in [At::Late, At::BeforeRegistration, At::BeforeAdmission] {
                let mut w = base.clone();
                let key = SigningKeypair::generate().unwrap();
                let forged = attack(&mut w, by, "vic", &key, with_cert, at);
                let what = format!("{by:?} cert={with_cert} {at:?}");
                let device = forged[0].signer_device_id.unwrap();
                assert_vic_unharmed(&mut w, device, &what);
            }
        }
    }
}

#[test]
fn a_forged_registration_of_the_founder_is_refused_even_before_the_founders_own() {
    let base = world();
    for with_cert in [false, true] {
        let mut w = base.clone();
        let key = SigningKeypair::generate().unwrap();
        attack(&mut w, By::Eve, "owner", &key, with_cert, At::BeforeRegistration);
        let owner = w.people["owner"].account_id;
        assert_eq!(w.roster().account_keys.get(&owner), Some(&w.people["owner"].account.public_key_bytes().to_vec()), "cert={with_cert}");
        // The founder still governs: a presence-confirmed role change folds.
        let id = w.write(
            "owner",
            None,
            SessionEvent::MemberRoleChanged {
                change: hive_core::events::MemberRoleChange { member_id: "eve".into(), old_role: WorkspaceRole::Contributor, new_role: WorkspaceRole::Viewer },
                watermark: Vec::new(),
            },
            None,
            Proof::Recovery,
        );
        assert_eq!(w.verdict(id), Verdict::Allowed, "cert={with_cert}");
    }
}

#[test]
fn the_accounts_own_device_registering_a_second_key_is_a_genuine_dispute() {
    let mut w = world();
    let vic = w.people["vic"].account_id;
    let other = SigningKeypair::generate().unwrap();
    // vic's own device certifies itself under another key and registers it:
    // the account's own lineage asserts two keys.
    let cert = DeviceCertificate::issue(&other, vic, &w.people["vic"].device.public_key_bytes(), hive_core::Timestamp::epoch());
    w.push_bare("vic", SessionEvent::DeviceCertificateAdded { certificate: cert });
    w.push_bare("vic", SessionEvent::AccountKeyRegistered { account_id: vic, signing_public_key: other.public_key_bytes().to_vec() });
    let roster = w.roster();
    assert!(!roster.account_keys.contains_key(&vic), "disputed: no key");
    assert!(!roster.account_certified(vic, w.people["vic"].device_id), "fail closed");
}

#[test]
fn a_rotation_the_current_key_did_not_sign_is_refused_and_a_fork_disputes() {
    // The forged rotation (eve, stamped as vic, signed by eve's own key).
    let mut w = world();
    let vic = w.people["vic"].account_id;
    let attacker = SigningKeypair::generate().unwrap();
    let sig = attacker.sign(&attacker.public_key_bytes()).to_vec();
    let rot = SessionEvent::AccountKeyRotated { account_id: vic, new_signing_public_key: attacker.public_key_bytes().to_vec(), succession_signature: sig };
    let forged = forge(&mut w, "eve", "vic", rot, At::Late);
    w.push_env(None, forged);
    let eve_device = w.people["eve"].device_id;
    assert_vic_unharmed(&mut w, eve_device, "forged rotation");

    // vic's own key designating two successors (with her presence): a fork.
    let mut w = world();
    let (k2, k3) = (SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap());
    for k in [&k2, &k3] {
        let sig = w.people["vic"].account.sign(&k.public_key_bytes()).to_vec();
        w.write(
            "vic",
            None,
            SessionEvent::AccountKeyRotated { account_id: vic, new_signing_public_key: k.public_key_bytes().to_vec(), succession_signature: sig },
            None,
            Proof::Recovery,
        );
    }
    assert!(!w.roster().account_keys.contains_key(&vic), "a fork of the account's own rotations disputes it");
}

/// Residual (documented): a member an Admin added directly never
/// self-enrolled, so nothing anchors their key. A registration from a device
/// no certificate under its key names is still refused; one that also
/// certifies its own device under the key disputes the account (fail closed:
/// neither key certifies anything), as before.
#[test]
fn an_admin_added_member_has_no_anchor_residual() {
    let base = world();
    let eve = base.people["eve"].account_id;
    let mut w = base.clone();
    let key = SigningKeypair::generate().unwrap();
    attack(&mut w, By::Ghost, "eve", &key, false, At::Late);
    assert_eq!(w.roster().account_keys.get(&eve), Some(&w.people["eve"].account.public_key_bytes().to_vec()), "no certificate: refused");
    let mut w = base.clone();
    attack(&mut w, By::Ghost, "eve", &key, true, At::Late);
    let roster = w.roster();
    assert!(!roster.account_keys.contains_key(&eve), "self-evident conflict on an unanchored account: disputed");
    assert!(!roster.account_certified(eve, w.people["ghost"].device_id), "the forger never signs as eve");
}

proptest! {
    // PROPTEST_CASES overrides this; each case folds a few logs.
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// Any mix of the attacks, in any order of the log's envelopes: the fold
    /// (a function of the event set) gives vic her key and keeps her device,
    /// and never certifies an attacker's device for her.
    #[test]
    fn prop_forgeries_never_dispute_or_take_a_self_enrolled_account(
        attacks in proptest::collection::vec((any::<bool>(), any::<bool>(), 0u8..3), 1..4),
        seed in any::<u64>(),
    ) {
        let mut w = world();
        let mut devices = Vec::new();
        for (ghost, cert, at) in &attacks {
            let key = SigningKeypair::generate().unwrap();
            let at = [At::Late, At::BeforeRegistration, At::BeforeAdmission][*at as usize];
            let by = if *ghost { By::Ghost } else { By::Eve };
            let f = attack(&mut w, by, "vic", &key, *cert, at);
            devices.push(f[0].signer_device_id.unwrap());
        }
        let mut shuffled = w.cfg.clone();
        let mut s = seed | 1;
        for i in (1..shuffled.len()).rev() {
            s ^= s << 13; s ^= s >> 7; s ^= s << 17;
            shuffled.swap(i, (s % (i as u64 + 1)) as usize);
        }
        let (_, a) = hive_core::events::project_config(&w.cfg, None, None);
        let (_, b) = hive_core::events::project_config(&shuffled, None, None);
        prop_assert_eq!(a.current(), b.current());
        prop_assert_eq!(a.anchors(), b.anchors());
        let vic = w.people["vic"].account_id;
        prop_assert_eq!(a.anchors().get(&vic), Some(&w.people["vic"].device_id), "vic's own admission anchors her key");
        prop_assert_eq!(a.current().account_keys.get(&vic), Some(&w.people["vic"].account.public_key_bytes().to_vec()));
        prop_assert!(a.current().account_certified(vic, w.people["vic"].device_id));
        for d in devices {
            prop_assert!(!a.current().account_certified(vic, d));
        }
    }
}

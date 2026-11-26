//! The adversarial review of the 0.4 identity code (R1, R2, R4, N2): each
//! failed before its fix.

use ligus_core::crypto::{
    device_id_for, phone_consent_preimage, DeviceCertificate, SigAlg, SigningKeypair,
};
use ligus_core::events::Verdict;
use ligus_core::retirement::MemberRetirement;
use ligus_core::testkit::{Person, Phone, SignedWorld};
use ligus_core::{SessionEvent, Timestamp, WorkspaceRole};

fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["ana", "ben", "cy"], None);
    w.admit("ana", "ben", WorkspaceRole::Admin);
    w.admit("ana", "cy", WorkspaceRole::Contributor);
    let id = w.push(
        "ana",
        SessionEvent::WorkspaceRulesSet {
            version: 3,
            presence: None,
        },
    );
    assert_eq!(w.verdict(id), Verdict::Allowed);
    w
}

/// R1: a planted, refused certificate with a statement under a key of the
/// planter's own doesn't make his foreign certificate of a phone count.
#[test]
fn a_planted_statement_never_makes_a_foreign_phone_certificate_count() {
    let mut w = world();
    let (ana, ben) = (w.people["ana"].account_id, w.people["ben"].account_id);
    let phone = Phone::new();
    let mut mine = w.phone_certificate("ana", &phone);
    mine.phone_consent = Some(phone.sign(&phone_consent_preimage(w.wid, ana, phone.device_id)));
    w.push(
        "ana",
        SessionEvent::DeviceCertificateAdded { certificate: mine },
    );
    let theirs = w.phone_certificate("ben", &phone);
    w.push(
        "ben",
        SessionEvent::DeviceCertificateAdded {
            certificate: theirs,
        },
    );
    assert!(
        !w.roster().account_certified(ben, phone.device_id),
        "control"
    );
    let fake = Phone::new();
    let mut plant = w.phone_certificate("ben", &fake);
    plant.device_id = phone.device_id;
    plant.phone_consent = Some(fake.sign(&phone_consent_preimage(w.wid, ben, phone.device_id)));
    let id = w.push_bare(
        "ben",
        SessionEvent::DeviceCertificateAdded { certificate: plant },
    );
    assert_eq!(
        w.verdict(id),
        Verdict::Denied,
        "the plant itself is refused (id squatting)"
    );
    let r = w.roster();
    assert!(
        !r.account_certified(ben, phone.device_id),
        "a planted certificate made Ben's foreign certificate count"
    );
    assert!(
        !r.account_holds_device(ben, phone.device_id),
        "Ben holds Ana's phone"
    );
}

/// R1b: a refused, mis-keyed certificate doesn't make a legacy phone
/// contested.
#[test]
fn a_planted_certificate_never_silences_a_legacy_phone() {
    let mut w = world();
    let cy = w.people["cy"].account_id;
    let phone = Phone::new();
    let c = w.phone_certificate("cy", &phone);
    w.push(
        "cy",
        SessionEvent::DeviceCertificateAdded { certificate: c },
    );
    assert!(
        w.roster().account_certified(cy, phone.device_id),
        "control: a sole certifier counts"
    );
    let fake = Phone::new();
    let mut plant = w.phone_certificate("ben", &fake);
    plant.device_id = phone.device_id;
    let id = w.push_bare(
        "ben",
        SessionEvent::DeviceCertificateAdded { certificate: plant },
    );
    assert_eq!(w.verdict(id), Verdict::Denied);
    assert!(
        w.roster().account_certified(cy, phone.device_id),
        "a refused, mis-keyed certificate took Cy's phone away"
    );
}

/// R1c: nor does an unsigned certificate of the real key for a made-up
/// account.
#[test]
fn an_unsigned_certificate_never_silences_a_legacy_phone() {
    let mut w = world();
    let cy = w.people["cy"].account_id;
    let phone = Phone::new();
    let c = w.phone_certificate("cy", &phone);
    w.push(
        "cy",
        SessionEvent::DeviceCertificateAdded {
            certificate: c.clone(),
        },
    );
    let mut plant = c;
    plant.account_id = uuid::Uuid::new_v4();
    let id = w.push_bare(
        "ben",
        SessionEvent::DeviceCertificateAdded { certificate: plant },
    );
    let _ = w.verdict(id);
    assert!(
        w.roster().account_certified(cy, phone.device_id),
        "an unsigned certificate for a made-up account took Cy's phone away"
    );
}

/// R2: a member who certified someone else's unaccepted device can't
/// revoke it through the never-finished withdrawal.
#[test]
fn a_foreign_certifier_cannot_burn_an_unaccepted_device() {
    let mut w = world();
    let (ana, cy) = (w.people["ana"].account_id, w.people["cy"].account_id);
    let kp = SigningKeypair::generate().unwrap();
    let dev = device_id_for(SigAlg::Ed25519, &kp.public_key_bytes());
    let cert = DeviceCertificate::issue(
        &w.people["ana"].account,
        ana,
        &kp.public_key_bytes(),
        Timestamp::epoch(),
    );
    w.push(
        "ana",
        SessionEvent::DeviceCertificateAdded { certificate: cert },
    );
    let foreign = DeviceCertificate::issue(
        &w.people["cy"].account,
        cy,
        &kp.public_key_bytes(),
        Timestamp::epoch(),
    );
    w.push(
        "cy",
        SessionEvent::DeviceCertificateAdded {
            certificate: foreign,
        },
    );
    let id = w.push(
        "cy",
        SessionEvent::DeviceRevoked {
            device_id: dev,
            watermark: Vec::new(),
        },
    );
    assert_eq!(
        w.verdict(id),
        Verdict::Denied,
        "a Contributor revoked the Owner's new computer by certifying its key first"
    );
    assert!(!w.roster().devices.get(&dev).is_some_and(|d| d.revoked));
}

fn retiring_world() -> (SignedWorld, String, uuid::Uuid, SigningKeypair, uuid::Uuid) {
    let dee_acct = uuid::Uuid::new_v4();
    let dee = dee_acct.to_string();
    let people = vec![
        Person::new("ana"),
        Person::new("ben"),
        Person::new(&dee).with_account_id(dee_acct),
    ];
    let mut w = SignedWorld::genesis_of(people, None);
    w.admit("ana", "ben", WorkspaceRole::Admin);
    let m = w.member_of(&dee, WorkspaceRole::Contributor);
    w.push(
        "ana",
        SessionEvent::MemberAdded {
            member: m,
            invite: None,
            proof: None,
        },
    );
    w.publish_identity(&dee);
    w.push(
        "ana",
        SessionEvent::WorkspaceRulesSet {
            version: 3,
            presence: None,
        },
    );
    let ben = w.people["ben"].account_id;
    let kp = SigningKeypair::generate().unwrap();
    let dev = device_id_for(SigAlg::Ed25519, &kp.public_key_bytes());
    let cert = DeviceCertificate::issue(
        &w.people["ben"].account,
        ben,
        &kp.public_key_bytes(),
        Timestamp::epoch(),
    );
    w.push(
        "ben",
        SessionEvent::DeviceCertificateAdded {
            certificate: cert.clone(),
        },
    );
    let mut env = w.draft(
        "ben",
        None,
        SessionEvent::DeviceCertificateAdded { certificate: cert },
        None,
    );
    if let Some(s) = env.actor_stamp.as_mut() {
        s.actor.device_id = Some(dev);
        s.prev = None;
    }
    env.clear_raw();
    ligus_core::crypto::sign_envelope(&mut env, dev, &kp);
    w.push_env(None, env);
    (w, dee, dee_acct, kp, dev)
}

/// R4: a retirement signature can't be replayed after the member is added
/// back.
#[test]
fn a_retirement_never_replays() {
    let (mut w, dee, dee_acct, _, dev) = retiring_world();
    let ben = w.people["ben"].account_id;
    let m = w.member_of(&dee, WorkspaceRole::Contributor);
    let r = MemberRetirement::sign(&w.people[&dee].account, w.wid, &dee, dee_acct, ben, dev);
    let first = w.push_bare(
        &dee,
        SessionEvent::MemberRetired {
            retirement: r.clone(),
        },
    );
    assert_eq!(w.verdict(first), Verdict::Allowed);
    let back = w.push(
        "ana",
        SessionEvent::MemberAdded {
            member: m,
            invite: None,
            proof: None,
        },
    );
    assert_eq!(w.verdict(back), Verdict::Allowed);
    let replay = w.push_bare("ben", SessionEvent::MemberRetired { retirement: r });
    assert_eq!(
        w.verdict(replay),
        Verdict::Denied,
        "an old retirement signature removed a re-added member again"
    );
}

/// N2: the retired member's host on the linked computer passes to the
/// person, it isn't dropped.
#[test]
fn the_linked_computers_host_passes_to_the_person() {
    let (mut w, dee, dee_acct, _, _) = retiring_world();
    let ben = w.people["ben"].account_id;
    let dev = w.people[&dee].device_id;
    let ppl = w.people.clone();
    let kp = &ppl[&dee].device;
    let mut host =
        ligus_core::WorkspaceHost::new(dev.to_string(), ligus_core::HostKind::Device, "build-1");
    host.owner_actor_id = dee.clone();
    w.push_bare(&dee, SessionEvent::WorkspaceHostUpserted { host });
    let cert = DeviceCertificate::issue(
        &w.people["ben"].account,
        ben,
        &kp.public_key_bytes(),
        Timestamp::epoch(),
    );
    w.push(
        "ben",
        SessionEvent::DeviceCertificateAdded {
            certificate: cert.clone(),
        },
    );
    let mut env = w.draft(
        "ben",
        None,
        SessionEvent::DeviceCertificateAdded { certificate: cert },
        None,
    );
    if let Some(s) = env.actor_stamp.as_mut() {
        s.actor.device_id = Some(dev);
        s.prev = None;
    }
    env.clear_raw();
    ligus_core::crypto::sign_envelope(&mut env, dev, kp);
    w.push_env(None, env);
    let r = MemberRetirement::sign(&w.people[&dee].account, w.wid, &dee, dee_acct, ben, dev);
    let id = w.push_bare(&dee, SessionEvent::MemberRetired { retirement: r });
    assert_eq!(w.verdict(id), Verdict::Allowed);
    let c = w.config();
    assert!(
        c.workspace_hosts
            .iter()
            .any(|h| h.id == dev.to_string() && h.owner_actor_id == "ben"),
        "{:?}",
        c.workspace_hosts
    );
}

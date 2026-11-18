//! Device-id squatting at ingest. A device id is derived from the device's
//! key (`hive_core::crypto::device_id_for`), so a member who certifies
//! another member's device id under a key of their own (even with a
//! backdated copy that sorts first) certifies nothing: the trust roster
//! refuses the certificate, the victim's events still verify, and the
//! squatter can't sign as the victim through the victim's id. What a member
//! can still do is certify the victim's real device key for themselves; that
//! signs nothing, and must not make the victim's events fail for good on
//! arrival order.

use hive_core::crypto::{sign_envelope, DeviceCertificate, SigningKeypair};
use hive_core::testkit::SignedWorld;
use hive_core::{ChatMessage, MessageRole, SessionEvent, Timestamp, WorkspaceRole};
use hive_runtime::envelope_verifier::{build_roster, verdict_for, DeviceKeyResolver, QuarantineReason, Verdict};

fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["owner", "con", "mal"], None);
    w.admit("owner", "con", WorkspaceRole::Contributor);
    w.admit("owner", "mal", WorkspaceRole::Contributor);
    w
}

fn post(by: &str, body: &str) -> SessionEvent {
    SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, by, body) }
}

#[test]
fn a_backdated_squat_neither_erases_nor_impersonates_at_ingest() {
    let mut w = world();
    let key = SigningKeypair::generate().unwrap();
    let (mal_account, con_device) = (w.people["mal"].account_id, w.people["con"].device_id);
    let cert = DeviceCertificate::issue_for_id(&w.people["mal"].account, mal_account, con_device, &key.public_key_bytes(), Timestamp::epoch());
    w.push("mal", SessionEvent::DeviceCertificateAdded { certificate: cert.clone() });
    // A second publication of it, backdated to sort before con's own.
    let mut early = w.draft("mal", None, SessionEvent::DeviceCertificateAdded { certificate: cert }, None);
    early.lamport = 2;
    w.finish(&mut early, "mal", None);
    w.push_env(None, early);
    let chat = w.chat("owner", "c");
    let genuine = w.say(chat, "con", post("con", "mine"));
    let genuine = w.chats[&chat].iter().find(|e| e.event_id == genuine).unwrap().clone();
    let mut forged = w.draft("con", Some(chat), post("con", "not mine"), None);
    forged.clear_raw();
    sign_envelope(&mut forged, con_device, &key);

    let roster = build_roster(&w.cfg);
    assert_eq!(roster.public_key(con_device), Some(&w.people["con"].device.public_key_bytes()[..]), "the squat changed the id's key");
    assert!(!roster.device_signs_for(con_device, mal_account), "the squat certified the id for mal");
    assert_eq!(verdict_for(&roster, &genuine), Verdict::Valid, "the victim's own event is refused");
    assert_eq!(verdict_for(&roster, &forged), Verdict::Quarantine(QuarantineReason::BadSignature), "the squatter signed as the victim");
}

/// Arrival order: a device that holds a member's certificate of the victim's
/// real device key (an adoption) but not yet the victim's own must hold the
/// victim's event until that certificate arrives, never refuse it for good.
#[test]
fn a_victims_event_before_its_certificate_is_held_not_refused_after_an_adoption() {
    let mut w = world();
    let (mal_account, con_device) = (w.people["mal"].account_id, w.people["con"].device_id);
    let cert = DeviceCertificate::issue(&w.people["mal"].account, mal_account, &w.people["con"].device.public_key_bytes(), Timestamp::epoch());
    assert_eq!(cert.device_id, con_device);
    w.push("mal", SessionEvent::DeviceCertificateAdded { certificate: cert });
    let chat = w.chat("owner", "c");
    let genuine = w.say(chat, "con", post("con", "mine"));
    let genuine = w.chats[&chat].iter().find(|e| e.event_id == genuine).unwrap().clone();
    // This device hasn't received con's own certificate yet.
    let con_account = w.people["con"].account_id;
    let partial: Vec<_> = w
        .cfg
        .iter()
        .filter(|e| !matches!(&e.payload, SessionEvent::DeviceCertificateAdded { certificate } if certificate.account_id == con_account))
        .cloned()
        .collect();
    let v = verdict_for(&build_roster(&partial), &genuine);
    assert!(matches!(v, Verdict::Unverifiable(_)), "the victim's event was refused for good before its certificate arrived: {v:?}");
    assert_eq!(verdict_for(&build_roster(&w.cfg), &genuine), Verdict::Valid);
    // Someone signing under the victim's id with another key is refused outright.
    let key = SigningKeypair::generate().unwrap();
    let mut forged = w.draft("con", Some(chat), post("con", "not mine"), None);
    forged.clear_raw();
    sign_envelope(&mut forged, con_device, &key);
    assert_eq!(verdict_for(&build_roster(&partial), &forged), Verdict::Quarantine(QuarantineReason::BadSignature));
}

/// A device that published its own certificate for one account is provably
/// that account's: an event it signs as another member is impersonation,
/// refused outright (not held).
#[test]
fn a_device_that_vouched_for_its_account_impersonating_another_is_refused() {
    let mut w = world();
    let chat = w.chat("owner", "c");
    let mal = &w.people["mal"];
    let (mal_device, mal_key) = (mal.device_id, SigningKeypair::from_seed(&mal.device.seed_bytes()).unwrap());
    let mut forged = w.draft("con", Some(chat), post("con", "as con"), None);
    forged.clear_raw();
    sign_envelope(&mut forged, mal_device, &mal_key);
    assert_eq!(verdict_for(&build_roster(&w.cfg), &forged), Verdict::Quarantine(QuarantineReason::Impersonation));
}

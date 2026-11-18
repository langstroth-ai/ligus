//! Device-id squatting at ingest (sibling of erase-by-forgery): a member
//! certifies another member's device id under their own account with a key
//! of their own, and backdates a copy of that certificate so it sorts first
//! (the trust roster folds certificates in canonical order and never judged
//! their lamports). Their binding must stay theirs: the victim's events still
//! verify, and the squatter can't sign as the victim through the victim's id.

use hive_core::crypto::{sign_envelope, DeviceCertificate, SigningKeypair};
use hive_core::testkit::SignedWorld;
use hive_core::{ChatMessage, MessageRole, SessionEvent, Timestamp, WorkspaceRole};
use hive_runtime::envelope_verifier::{build_roster, verdict_for, Verdict};

#[test]
fn a_backdated_squat_neither_erases_nor_impersonates_at_ingest() {
    let mut w = SignedWorld::genesis(&["owner", "con", "mal"], None);
    w.admit("owner", "con", WorkspaceRole::Contributor);
    w.admit("owner", "mal", WorkspaceRole::Contributor);
    let key = SigningKeypair::generate().unwrap();
    let (mal_account, con_device) = (w.people["mal"].account_id, w.people["con"].device_id);
    let cert = DeviceCertificate::issue(&w.people["mal"].account, mal_account, con_device, &key.public_key_bytes(), Timestamp::epoch());
    w.push("mal", SessionEvent::DeviceCertificateAdded { certificate: cert.clone() });
    // A second publication of it, backdated to sort before con's own.
    let mut early = w.draft("mal", None, SessionEvent::DeviceCertificateAdded { certificate: cert }, None);
    early.lamport = 2;
    w.finish(&mut early, "mal", None);
    w.push_env(None, early);
    let chat = w.chat("owner", "c");
    let genuine = w.say(chat, "con", SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "con", "mine") });
    let genuine = w.chats[&chat].iter().find(|e| e.event_id == genuine).unwrap().clone();
    let mut forged = w.draft("con", Some(chat), SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "con", "not mine") }, None);
    forged.clear_raw();
    sign_envelope(&mut forged, con_device, &key);

    let roster = build_roster(&w.cfg);
    assert_eq!(verdict_for(&roster, &genuine), Verdict::Valid, "the victim's own event is refused");
    assert_ne!(verdict_for(&roster, &forged), Verdict::Valid, "the squatter signed as the victim");
}

/// Arrival order: a device that holds a squatter's certificate of the
/// victim's device id but not yet the victim's own must hold the victim's
/// event until that certificate arrives, never refuse it for good.
#[test]
fn a_victims_event_before_its_certificate_is_held_not_refused_after_a_squat() {
    let mut w = SignedWorld::genesis(&["owner", "con", "mal"], None);
    w.admit("owner", "con", WorkspaceRole::Contributor);
    w.admit("owner", "mal", WorkspaceRole::Contributor);
    let (mal_account, con_device) = (w.people["mal"].account_id, w.people["con"].device_id);
    let key = SigningKeypair::generate().unwrap();
    let cert = DeviceCertificate::issue(&w.people["mal"].account, mal_account, con_device, &key.public_key_bytes(), Timestamp::epoch());
    w.push("mal", SessionEvent::DeviceCertificateAdded { certificate: cert });
    let chat = w.chat("owner", "c");
    let genuine = w.say(chat, "con", SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "con", "mine") });
    let genuine = w.chats[&chat].iter().find(|e| e.event_id == genuine).unwrap().clone();
    // This device hasn't received con's own certificate yet.
    let partial: Vec<_> = w
        .cfg
        .iter()
        .filter(|e| !matches!(&e.payload, SessionEvent::DeviceCertificateAdded { certificate } if certificate.account_id == w.people["con"].account_id))
        .cloned()
        .collect();
    let v = verdict_for(&build_roster(&partial), &genuine);
    assert!(matches!(v, Verdict::Unverifiable(_)), "the victim's event was refused for good before its certificate arrived: {v:?}");
    assert_eq!(verdict_for(&build_roster(&w.cfg), &genuine), Verdict::Valid);
    // A squatter signing as the victim is still refused outright.
    let mut forged = w.draft("con", Some(chat), SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "con", "not mine") }, None);
    forged.clear_raw();
    sign_envelope(&mut forged, con_device, &key);
    assert!(matches!(verdict_for(&build_roster(&partial), &forged), Verdict::Quarantine(_)));
}

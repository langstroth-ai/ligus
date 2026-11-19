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

/// A device that published its own certificate for one account, signing as
/// another member: held (never applied) until that member certifies the
/// device, not refused for good, since the member may certify it (a device
/// may act for more than one account) and another device may already hold
/// that certificate.
#[test]
fn a_device_signing_as_another_member_is_held_until_that_member_certifies_it() {
    let mut w = world();
    let chat = w.chat("owner", "c");
    let mal = &w.people["mal"];
    let (mal_device, mal_key) = (mal.device_id, SigningKeypair::from_seed(&mal.device.seed_bytes()).unwrap());
    let mut forged = w.draft("con", Some(chat), post("con", "as con"), None);
    forged.clear_raw();
    sign_envelope(&mut forged, mal_device, &mal_key);
    let v = verdict_for(&build_roster(&w.cfg), &forged);
    assert_eq!(v, Verdict::Unverifiable(QuarantineReason::UncertifiedDevice));
    assert!(matches!(
        hive_runtime::identity_verifier::gate_ingest(
            hive_runtime::identity_verifier::IdentityMode::Relay,
            v,
            None,
            None,
            &hive_runtime::identity_verifier::IdentityCache::new()
        ),
        hive_runtime::identity_verifier::IngestAction::Hold(_)
    ), "held, never applied");
}

/// Someone who knows a person's device key (it is the same device in every
/// workspace) certifies it for themselves in a workspace the person hasn't
/// joined yet. When the person joins, their account key registration and
/// certificate, signed by that device, must still establish them: refusing
/// them for good (the device is "certified for someone else") would lock
/// the person out of the workspace on every device that saw the adoption
/// first.
#[test]
fn an_adoption_before_a_person_joins_never_locks_them_out() {
    let mut w = SignedWorld::genesis(&["owner", "con", "mal"], None);
    w.admit("owner", "mal", WorkspaceRole::Contributor);
    adopt(&mut w, "mal", "con");
    let before_con = w.cfg.len();
    w.admit("owner", "con", WorkspaceRole::Contributor);
    let con_account = w.people["con"].account_id;
    // Each of con's trust events, arriving with only what came before it.
    for (i, e) in w.cfg.iter().enumerate().skip(before_con) {
        if !matches!(&e.payload, SessionEvent::AccountKeyRegistered { account_id, .. } if *account_id == con_account)
            && !matches!(&e.payload, SessionEvent::DeviceCertificateAdded { certificate } if certificate.account_id == con_account)
        {
            continue;
        }
        let v = verdict_for(&build_roster(&w.cfg[..i]), e);
        assert!(!matches!(v, Verdict::Quarantine(_)), "con's {:?} refused for good: {v:?}", e.payload);
    }
    let chat = w.chat("owner", "c");
    let id = w.say(chat, "con", post("con", "hello"));
    let env = w.chats[&chat].iter().find(|e| e.event_id == id).unwrap().clone();
    assert_eq!(verdict_for(&build_roster(&w.cfg), &env), Verdict::Valid);
}

/// The signing device's certificate for the stamped account, published by
/// `by` (with presence): an adoption when `by` isn't the device's holder.
fn adopt(w: &mut SignedWorld, by: &str, device_of: &str) {
    let cert = DeviceCertificate::issue(&w.people[by].account, w.people[by].account_id, &w.people[device_of].device.public_key_bytes(), Timestamp::epoch());
    w.push(by, SessionEvent::DeviceCertificateAdded { certificate: cert });
}

/// `device_of`'s device signs `payload` on `chat` stamped as `as_whom`.
fn signed_by_device_as(w: &mut SignedWorld, chat: uuid::Uuid, device_of: &str, as_whom: &str, body: &str) -> hive_core::SessionEventEnvelope {
    let p = &w.people[device_of];
    let (device, key) = (p.device_id, SigningKeypair::from_seed(&p.device.seed_bytes()).unwrap());
    let mut env = w.draft(as_whom, Some(chat), post(as_whom, body), None);
    env.clear_raw();
    sign_envelope(&mut env, device, &key);
    env
}

/// Quarantine is for good (the event is dropped, kept only as a lamport
/// witness), so it must never depend on which certificates arrived first:
/// mal's device vouched for mal (it published its own certificate), and
/// con certified mal's device key for con too (a device may act for more
/// than one account). An event mal's device signs as con, arriving before
/// con's certificate, is held, not refused; once the certificate is here it
/// is valid, as on every device that received the certificate first.
#[test]
fn a_vouched_devices_event_for_an_account_whose_certificate_comes_later_is_held_not_refused() {
    let mut w = world();
    adopt(&mut w, "con", "mal");
    let chat = w.chat("owner", "c");
    let env = signed_by_device_as(&mut w, chat, "mal", "con", "as con, with con's consent");
    let full = verdict_for(&build_roster(&w.cfg), &env);
    assert_eq!(full, Verdict::Valid, "con certified mal's device for con");
    let (con_account, mal_device) = (w.people["con"].account_id, w.people["mal"].device_id);
    let before: Vec<_> = w
        .cfg
        .iter()
        .filter(|e| !matches!(&e.payload, SessionEvent::DeviceCertificateAdded { certificate } if certificate.account_id == con_account && certificate.device_id == mal_device))
        .cloned()
        .collect();
    let early = verdict_for(&build_roster(&before), &env);
    assert!(!matches!(early, Verdict::Quarantine(_)), "refused for good before con's certificate arrived, valid after: {early:?}");
}

/// The same for an account this device doesn't know yet: mal certified
/// con's device key for mal (an adoption), and con's own key registration
/// and certificate haven't arrived. con's own event, signed by con's own
/// device, must be held, not refused for good.
#[test]
fn an_honest_event_before_its_accounts_key_arrives_is_held_not_refused_after_an_adoption() {
    let mut w = world();
    adopt(&mut w, "mal", "con");
    let chat = w.chat("owner", "c");
    let id = w.say(chat, "con", post("con", "mine"));
    let env = w.chats[&chat].iter().find(|e| e.event_id == id).unwrap().clone();
    assert_eq!(verdict_for(&build_roster(&w.cfg), &env), Verdict::Valid);
    let con_account = w.people["con"].account_id;
    let before: Vec<_> = w
        .cfg
        .iter()
        .filter(|e| match &e.payload {
            SessionEvent::AccountKeyRegistered { account_id, .. } => *account_id != con_account,
            SessionEvent::DeviceCertificateAdded { certificate } => certificate.account_id != con_account,
            _ => true,
        })
        .cloned()
        .collect();
    let early = verdict_for(&build_roster(&before), &env);
    assert!(!matches!(early, Verdict::Quarantine(_)), "con's own event refused for good before con's key arrived: {early:?}");
}

mod convergence {
    //! Every device ends with the same ingest verdicts once every event has
    //! arrived, whatever arrived first: a verdict that refuses for good
    //! (quarantine) on a partial roster is the verdict on the full one.
    //! Certificates (own and adoptions) arrive in any order and any subset;
    //! no revocation, removal or second account key is generated (those
    //! refuse by design once they are known).
    use super::*;
    use proptest::prelude::*;

    const NAMES: [&str; 4] = ["owner", "a", "b", "c"];

    fn cases() -> u32 {
        std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(64)
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: cases(), ..ProptestConfig::default() })]
        #[test]
        fn quarantine_never_depends_on_arrival(
            adoptions in proptest::collection::vec((0usize..4, 0usize..4), 0..5),
            events in proptest::collection::vec((0usize..4, 0usize..4), 1..5),
            masks in proptest::collection::vec(any::<u64>(), 1..4),
        ) {
            let mut w = SignedWorld::genesis(&NAMES, None);
            for n in &NAMES[1..] {
                w.admit("owner", n, WorkspaceRole::Contributor);
            }
            for (by, of) in &adoptions {
                if by != of {
                    adopt(&mut w, NAMES[*by], NAMES[*of]);
                }
            }
            let chat = w.chat("owner", "c");
            let envs: Vec<_> = events
                .iter()
                .enumerate()
                .map(|(i, (dev, as_whom))| signed_by_device_as(&mut w, chat, NAMES[*dev], NAMES[*as_whom], &format!("e{i}")))
                .collect();
            let full = build_roster(&w.cfg);
            let finals: Vec<Verdict> = envs.iter().map(|e| verdict_for(&full, e)).collect();
            // Certificates are what may arrive late (the seed, members and
            // keys arrive first, in log order).
            let late: Vec<usize> = w
                .cfg
                .iter()
                .enumerate()
                .filter(|(_, e)| matches!(e.payload, SessionEvent::DeviceCertificateAdded { .. }))
                .map(|(i, _)| i)
                .collect();
            for mask in masks {
                let partial: Vec<_> = w
                    .cfg
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| late.iter().position(|l| l == i).is_none_or(|k| mask >> (k % 64) & 1 == 1))
                    .map(|(_, e)| e.clone())
                    .collect();
                let roster = build_roster(&partial);
                for (e, fin) in envs.iter().zip(&finals) {
                    let early = verdict_for(&roster, e);
                    if matches!(early, Verdict::Quarantine(_)) {
                        prop_assert_eq!(&early, fin, "refused for good on a partial roster, {:?} on the full one", fin);
                    }
                }
            }
        }
    }
}

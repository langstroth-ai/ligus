//! Rules version 3 (identity model v2, "Rules 3 in 0.4"): a certificate
//! counts only with its device's own acceptance; member records whose ids
//! disagree and model keys on the log are refused; hosts of former members
//! are inert; the version never goes back.

use ligus_core::crypto::{phone_consent_preimage, DeviceCertificate, SigningKeypair};
use ligus_core::events::Verdict;
use ligus_core::testkit::{Phone, SignedWorld};
use ligus_core::workspace_runtime::{WorkspaceCredential, WorkspaceRuntime};
use ligus_core::{ActorIdentity, ActorKind, ModelProviderKind, SessionEvent, Timestamp, WorkspaceMember, WorkspaceRole};
use uuid::Uuid;

/// Ana (Owner), Ben (Admin), Cy (Contributor), then rules 3.
fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["ana", "ben", "cy"], None);
    w.admit("ana", "ben", WorkspaceRole::Admin);
    w.admit("ana", "cy", WorkspaceRole::Contributor);
    let id = w.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    assert_eq!(w.verdict(id), Verdict::Allowed, "the Owner switches to rules 3");
    assert_eq!(w.roster().rules, 3);
    w
}

fn foreign_cert(w: &SignedWorld, by: &str, of_key: &[u8]) -> DeviceCertificate {
    DeviceCertificate::issue(&w.people[by].account, w.people[by].account_id, of_key, Timestamp::epoch())
}

#[test]
fn a_foreign_certificate_counts_for_nothing() {
    let mut w = world();
    let (ana, ben) = (w.people["ana"].account_id, w.people["ben"].account_id);
    let ana_dev = w.people["ana"].device_id;
    let c = foreign_cert(&w, "ben", &w.people["ana"].device.public_key_bytes());
    let id = w.push("ben", SessionEvent::DeviceCertificateAdded { certificate: c });
    assert_eq!(w.verdict(id), Verdict::Allowed, "stored and folded, but it certifies nothing");
    let r = w.roster();
    assert!(r.account_certified(ana, ana_dev));
    assert!(!r.account_certified(ben, ana_dev), "Ben's certificate of Ana's key counts");
    assert_eq!(r.devices[&ana_dev].holders.iter().copied().collect::<Vec<_>>(), vec![ana]);
    // Under rules 2 the same log counts it (as 0.3 does).
    let mut w2 = SignedWorld::genesis(&["ana", "ben"], None);
    w2.admit("ana", "ben", WorkspaceRole::Admin);
    let c = foreign_cert(&w2, "ben", &w2.people["ana"].device.public_key_bytes());
    w2.push("ben", SessionEvent::DeviceCertificateAdded { certificate: c });
    assert!(w2.roster().account_certified(w2.people["ben"].account_id, w2.people["ana"].device_id), "control: rules 2");
}

/// A device another device of the account enrolled counts once it accepted
/// (it publishes its own copy of the certificate, or signs as the account),
/// and stays delegated.
#[test]
fn an_enrolled_device_counts_once_it_accepts() {
    let mut w = world();
    let ana = w.people["ana"].account_id;
    let kp = SigningKeypair::generate().unwrap();
    let dev = ligus_core::crypto::device_id_for(ligus_core::crypto::SigAlg::Ed25519, &kp.public_key_bytes());
    let cert = DeviceCertificate::issue(&w.people["ana"].account, ana, &kp.public_key_bytes(), Timestamp::epoch());
    w.push("ana", SessionEvent::DeviceCertificateAdded { certificate: cert.clone() });
    assert!(!w.roster().account_certified(ana, dev), "not accepted yet");
    // The device re-publishes its own certificate (stamped as Ana, signed by it).
    let mut env = w.draft("ana", None, SessionEvent::DeviceCertificateAdded { certificate: cert }, None);
    if let Some(s) = env.actor_stamp.as_mut() {
        s.actor.device_id = Some(dev);
        s.prev = None;
    }
    env.clear_raw();
    ligus_core::crypto::sign_envelope(&mut env, dev, &kp);
    w.push_env(None, env);
    let r = w.roster();
    assert!(r.account_certified(ana, dev), "accepted");
    assert!(r.devices[&dev].delegated_for(ana), "still a delegated device");
}

fn consent(phone: &Phone, ws: Uuid, account: Uuid) -> Vec<u8> {
    phone.sign(&phone_consent_preimage(ws, account, phone.device_id))
}

#[test]
fn a_contested_phone_counts_only_with_its_statement_and_its_person_revokes_it() {
    let mut w = world();
    let (ana, ben) = (w.people["ana"].account_id, w.people["ben"].account_id);
    let phone = Phone::new();
    // Sole certifier: counts without a statement (phones enrolled before 0.4).
    let c = w.phone_certificate("cy", &phone);
    let mut solo = w.clone();
    solo.push("cy", SessionEvent::DeviceCertificateAdded { certificate: c });
    assert!(solo.roster().account_certified(solo.people["cy"].account_id, phone.device_id));

    // Ana enrolls it with its statement; Ben (who outranks nobody here but is
    // an Admin) certifies the same key without one.
    let mut mine = w.phone_certificate("ana", &phone);
    mine.phone_consent = Some(consent(&phone, w.wid, ana));
    w.push("ana", SessionEvent::DeviceCertificateAdded { certificate: mine });
    let theirs = w.phone_certificate("ben", &phone);
    w.push("ben", SessionEvent::DeviceCertificateAdded { certificate: theirs });
    let r = w.roster();
    assert!(r.account_certified(ana, phone.device_id));
    assert!(!r.account_certified(ben, phone.device_id), "a certificate without the phone's statement counts");
    assert!(r.account_owns_device(ana, phone.device_id), "Ana may revoke her own phone");

    // A statement naming another account doesn't count for this one.
    let mut w3 = world();
    let mut forged = w3.phone_certificate("ben", &phone);
    forged.phone_consent = Some(consent(&phone, w3.wid, w3.people["ana"].account_id));
    w3.push("ben", SessionEvent::DeviceCertificateAdded { certificate: forged });
    let other = w3.phone_certificate("ana", &phone);
    w3.push("ana", SessionEvent::DeviceCertificateAdded { certificate: other });
    assert!(!w3.roster().account_certified(w3.people["ben"].account_id, phone.device_id), "a statement for Ana counted for Ben");
}

/// Ana's lost phone, contested by Ben (Admin, above Cy): Cy revokes it.
#[test]
fn the_phones_person_revokes_it_whatever_the_ranks() {
    let mut w = world();
    let cy = w.people["cy"].account_id;
    let phone = Phone::new();
    let mut mine = w.phone_certificate("cy", &phone);
    mine.phone_consent = Some(consent(&phone, w.wid, cy));
    w.push("cy", SessionEvent::DeviceCertificateAdded { certificate: mine });
    let theirs = w.phone_certificate("ben", &phone);
    w.push("ben", SessionEvent::DeviceCertificateAdded { certificate: theirs });
    let id = w.push("cy", SessionEvent::DeviceRevoked { device_id: phone.device_id, watermark: Vec::new() });
    assert_eq!(w.verdict(id), Verdict::Allowed, "Cy couldn't revoke her own phone past Ben");
}

#[test]
fn member_records_with_disagreeing_ids_are_refused() {
    let mut w = world();
    let account = Uuid::new_v4();
    let mut actor = ActorIdentity::new("not-the-account", "Dee", ActorKind::Human);
    actor.account_id = Some(account);
    let bad = WorkspaceMember { id: "not-the-account".into(), actor, role: WorkspaceRole::Viewer, title: String::new(), index: 0, joined_at: Timestamp::epoch() };
    let id = w.push("ana", SessionEvent::MemberAdded { member: bad, invite: None, proof: None });
    assert_eq!(w.verdict(id), Verdict::Denied);
    let mut actor = ActorIdentity::new(account.to_string(), "Dee", ActorKind::Human);
    actor.account_id = Some(account);
    let skew = WorkspaceMember { id: "dee".into(), actor: actor.clone(), role: WorkspaceRole::Viewer, title: String::new(), index: 0, joined_at: Timestamp::epoch() };
    let id = w.push("ana", SessionEvent::MemberAdded { member: skew, invite: None, proof: None });
    assert_eq!(w.verdict(id), Verdict::Denied);
    let good = WorkspaceMember { id: account.to_string(), actor, role: WorkspaceRole::Viewer, title: String::new(), index: 0, joined_at: Timestamp::epoch() };
    let id = w.push("ana", SessionEvent::MemberAdded { member: good, invite: None, proof: None });
    assert_eq!(w.verdict(id), Verdict::Allowed);
}

#[test]
fn a_model_key_on_the_log_is_refused() {
    let mut w = world();
    let mut rt = WorkspaceRuntime::new("gw", "Gateway", ModelProviderKind::Custom, "m");
    rt.credential = serde_json::from_value(serde_json::json!({ "kind": "inline", "secret": "sk-test" })).unwrap();
    assert!(matches!(rt.credential, WorkspaceCredential::Inline { .. }));
    let id = w.push("ana", SessionEvent::WorkspaceRuntimeUpserted { runtime: rt.clone() });
    assert_eq!(w.verdict(id), Verdict::Denied);
    let id = w.push("ana", SessionEvent::WorkspaceRuntimesUpdated { runtimes: vec![rt] });
    assert_eq!(w.verdict(id), Verdict::Denied);
}

#[test]
fn a_former_members_host_is_inert() {
    let mut w = world();
    let mut host = ligus_core::WorkspaceHost::new(w.people["cy"].device_id.to_string(), ligus_core::HostKind::Device, "cy-mbp");
    host.owner_actor_id = "cy".into();
    w.push_bare("cy", SessionEvent::WorkspaceHostUpserted { host });
    assert!(w.config().workspace_hosts.iter().any(|h| h.owner_actor_id == "cy"));
    w.push("ana", SessionEvent::MemberRemoved { member_id: "cy".into(), watermark: Vec::new() });
    assert!(!w.config().workspace_hosts.iter().any(|h| h.owner_actor_id == "cy"), "a removed member's host stayed");
}

#[test]
fn the_rules_never_go_back() {
    let mut w = world();
    let id = w.push("ana", SessionEvent::WorkspaceRulesSet { version: 2, presence: None });
    assert_eq!(w.verdict(id), Verdict::Denied);
    assert_eq!(w.roster().rules, 3);
}

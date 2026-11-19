//! Device-id squatting is refused by construction. A device id is derived
//! from the device's first signing key (`hive_core::crypto::device_id_for`),
//! and a certificate whose id isn't the one its key derives never verifies:
//! a member can't certify another member's device id (or one that will be
//! theirs) under a key of their own, wherever the certificate sorts. What a
//! member *can* still do is certify someone else's real device key for their
//! own account; nobody can sign as the device through that, and it makes the
//! device no more theirs: who holds a device is decided by the event set,
//! never by a position (`hive_core::authz::DeviceState::holders`).

use hive_core::crypto::{sign_envelope, DeviceCertificate, SigningKeypair};
use hive_core::testkit::{Phone, SignedWorld};
use hive_core::{SessionEvent, SessionEventEnvelope, Timestamp, Verdict, WorkspaceRole};

/// `mal` certifies `con`'s device id under mal's own account with a key of
/// mal's own, with mal's presence: refused.
fn squat(w: &mut SignedWorld) -> SigningKeypair {
    let key = SigningKeypair::generate().unwrap();
    let mal = &w.people["mal"];
    let cert = DeviceCertificate::issue_for_id(&mal.account, mal.account_id, w.people["con"].device_id, &key.public_key_bytes(), Timestamp::epoch());
    let id = w.push("mal", SessionEvent::DeviceCertificateAdded { certificate: cert });
    assert_eq!(w.verdict(id), Verdict::Denied, "a certificate under someone else's device id was allowed");
    key
}

fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["owner", "con", "mal"], None);
    w.admit("owner", "con", WorkspaceRole::Contributor);
    w.admit("owner", "mal", WorkspaceRole::Contributor);
    w
}

fn ka(device: uuid::Uuid) -> SessionEvent {
    SessionEvent::DeviceKeyAgreementPublished { device_id: device, ka_public: vec![7; 32] }
}

/// `by`'s config event drafted as if only the first `upto` config events
/// existed: it names that older log head and sorts among them (backdated),
/// with `by`'s presence.
fn backdated(w: &mut SignedWorld, by: &str, upto: usize, payload: SessionEvent) -> SessionEventEnvelope {
    let rest = w.cfg.split_off(upto);
    let mut env = w.draft(by, None, payload, None);
    let a = w.recovery_presence(by, &env);
    w.finish(&mut env, by, Some(a));
    w.cfg.extend(rest);
    env
}

/// Where `con`'s own device certificate sits on the config log.
fn cons_certificate_at(w: &SignedWorld) -> usize {
    let con = w.people["con"].account_id;
    w.cfg
        .iter()
        .position(|e| matches!(&e.payload, SessionEvent::DeviceCertificateAdded { certificate } if certificate.account_id == con))
        .unwrap()
}

#[test]
fn a_squatted_device_id_keeps_its_own_key() {
    let mut w = world();
    let key = squat(&mut w);
    let con_device = w.people["con"].device_id;
    let roster = w.roster();
    let d = &roster.devices[&con_device];
    assert_eq!(d.public_key, w.people["con"].device.public_key_bytes().to_vec());
    assert!(!d.accounts.contains(&w.people["mal"].account_id), "the squat bound the id for mal");
    assert_ne!(d.public_key, key.public_key_bytes().to_vec());
    // con's device publishes its own key-agreement key: a key event, so the
    // fold checks the signature against the device's certified key.
    let id = w.push_bare("con", ka(con_device));
    assert_eq!(w.verdict(id), Verdict::Allowed, "the victim's own key event no longer verifies");
}

/// Backdating doesn't help: a squat certificate naming an older log head,
/// sorting before the victim's own certificate, is refused all the same.
#[test]
fn a_backdated_squat_is_refused_wherever_it_sorts() {
    let mut w = world();
    let at = cons_certificate_at(&w);
    let key = SigningKeypair::generate().unwrap();
    let mal = &w.people["mal"];
    let cert = DeviceCertificate::issue_for_id(&mal.account, mal.account_id, w.people["con"].device_id, &key.public_key_bytes(), Timestamp::epoch());
    let early = backdated(&mut w, "mal", at - 1, SessionEvent::DeviceCertificateAdded { certificate: cert });
    assert!(early.canonical_key() < w.cfg[at].canonical_key(), "control: the squat sorts first");
    let id = w.push_env(None, early);
    assert_eq!(w.verdict(id), Verdict::Denied);
    let con_device = w.people["con"].device_id;
    assert_eq!(w.roster().devices[&con_device].public_key, w.people["con"].device.public_key_bytes().to_vec());
    let ka = w.push_bare("con", ka(con_device));
    assert_eq!(w.verdict(ka), Verdict::Allowed);
}

#[test]
fn a_squatter_cannot_sign_as_the_victim_through_the_victims_device_id() {
    let mut w = world();
    let key = squat(&mut w);
    let con_device = w.people["con"].device_id;
    // mal drafts con's key-agreement publication, stamped as con, signed
    // with mal's key under con's device id.
    let mut forged = w.draft("con", None, ka(con_device), None);
    forged.clear_raw();
    sign_envelope(&mut forged, con_device, &key);
    let id = w.push_env(None, forged);
    assert_eq!(w.verdict(id), Verdict::Denied, "the squatter signed as the victim");
}

/// An agent hosted on the victim's device: the squatter can't certify a key
/// for it through the squatted id (that would let them speak as the agent),
/// and the victim still can.
#[test]
fn a_squatter_cannot_certify_a_key_for_an_agent_on_the_victims_device() {
    let mut w = world();
    let con_device = w.people["con"].device_id;
    let mut agent = hive_core::WorkspaceAgent::new("coder", "gw");
    agent.owner_actor_id = "con".into();
    agent.host_id = con_device.to_string();
    let agent_id = agent.id;
    let up = w.push("owner", SessionEvent::AgentUpserted { agent });
    assert_eq!(w.verdict(up), Verdict::Allowed, "control: an Owner binds the agent to con's device");
    let key = squat(&mut w);

    let evil = SigningKeypair::generate().unwrap();
    let cert = hive_core::AgentKeyCertificate::issue(&key, con_device, agent_id, &evil.public_key_bytes(), Timestamp::epoch());
    let mut env = w.draft("mal", None, SessionEvent::AgentKeyCertified { cert: cert.clone() }, None);
    let presence = w.recovery_presence("mal", &env);
    if let Some(s) = env.actor_stamp.as_mut() {
        s.presence = Some(presence);
    }
    env.clear_raw();
    sign_envelope(&mut env, con_device, &key);
    let id = w.push_env(None, env);
    assert_eq!(w.verdict(id), Verdict::Denied, "the squatter certified a key for the victim's agent");
    assert!(w.roster().valid_agent_key(cert.key_id).is_none());

    // The victim's own device still certifies one.
    let good = SigningKeypair::generate().unwrap();
    let cert = hive_core::AgentKeyCertificate::issue(&w.people["con"].device, con_device, agent_id, &good.public_key_bytes(), Timestamp::epoch());
    let id = w.push("con", SessionEvent::AgentKeyCertified { cert: cert.clone() });
    assert_eq!(w.verdict(id), Verdict::Allowed, "the victim's device no longer hosts its agent's key");
    assert!(w.roster().valid_agent_key(cert.key_id).is_some());
}

/// Revoking a device your own account holds is self-service (R5); a squat
/// attempt on someone else's device id makes nothing yours, and least of all
/// the founder's (whom nobody cuts).
#[test]
fn a_squatter_cannot_revoke_the_victims_device_as_their_own() {
    for victim in ["con", "owner"] {
        let mut w = world();
        let device = w.people[victim].device_id;
        let key = SigningKeypair::generate().unwrap();
        let mal = &w.people["mal"];
        let cert = DeviceCertificate::issue_for_id(&mal.account, mal.account_id, device, &key.public_key_bytes(), Timestamp::epoch());
        w.push("mal", SessionEvent::DeviceCertificateAdded { certificate: cert });
        let id = w.push("mal", SessionEvent::DeviceRevoked { device_id: device, watermark: Vec::new() });
        assert_eq!(w.verdict(id), Verdict::Denied, "mal revoked {victim}'s device through a squatted certificate");
        assert!(!w.roster().devices[&device].revoked, "{victim}'s device is revoked");
        // The victim's device still writes.
        let ka = w.push_bare(victim, ka(device));
        assert_eq!(w.verdict(ka), Verdict::Allowed, "{victim}'s device no longer signs");
        // The victim still revokes their own device (a lost laptop).
        let own = w.push(victim, SessionEvent::DeviceRevoked { device_id: device, watermark: Vec::new() });
        assert_eq!(w.verdict(own), Verdict::Allowed, "the squat blocked {victim}'s own revocation");
    }
}

/// What's left: certifying someone else's real device key for your own
/// account. Allowed as a certificate (it is well formed), but it holds
/// nothing: not when it sorts after the victim's own certificate, and not
/// when it is backdated before it. The adopter can't revoke the device as
/// theirs (nor can an Admin who has nothing else of it to revoke), and an
/// unbound agent of theirs doesn't run on it.
#[test]
fn certifying_someone_elses_device_key_holds_nothing_backdated_or_not() {
    for backdate in [false, true] {
        let mut w = SignedWorld::genesis(&["owner", "ada", "con", "mal"], None);
        w.admit("owner", "ada", WorkspaceRole::Admin);
        // mal is a member before con joins: mal can name con's device (its
        // key known from elsewhere) before con's own certificate.
        w.admit("owner", "mal", WorkspaceRole::Contributor);
        w.admit("owner", "con", WorkspaceRole::Contributor);
        let at = cons_certificate_at(&w);
        let (con_device, mal_account) = (w.people["con"].device_id, w.people["mal"].account_id);
        let mal = &w.people["mal"];
        let cert = DeviceCertificate::issue(&mal.account, mal_account, &w.people["con"].device.public_key_bytes(), Timestamp::epoch());
        assert_eq!(cert.device_id, con_device);
        let adopt = SessionEvent::DeviceCertificateAdded { certificate: cert };
        let revoke = SessionEvent::DeviceRevoked { device_id: con_device, watermark: Vec::new() };
        let (adopted, revoked) = if backdate {
            // Both sort before con's own certificate (lamports one past the
            // older heads they name).
            let a = backdated(&mut w, "mal", at - 2, adopt);
            let id = a.event_id;
            w.cfg.insert(at - 2, a);
            let r = backdated(&mut w, "mal", at - 1, revoke.clone());
            assert!(r.canonical_key() < w.cfg[at + 1].canonical_key(), "control: the revocation sorts before con's certificate");
            (id, w.push_env(None, r))
        } else {
            (w.push("mal", adopt), w.push("mal", revoke.clone()))
        };
        assert_eq!(w.verdict(adopted), Verdict::Allowed, "control: a well-formed certificate of mal's (backdated: {backdate})");
        assert_eq!(w.verdict(revoked), Verdict::Denied, "mal revoked con's device (backdated: {backdate})");
        // An Admin who has nothing else of it to revoke can't either, when
        // only mal's certificate is before the revocation.
        if backdate {
            let r = backdated(&mut w, "ada", at - 1, revoke.clone());
            let id = w.push_env(None, r);
            assert_eq!(w.verdict(id), Verdict::Denied, "an Admin revoked a device only an adopter had certified there");
        }
        let roster = w.roster();
        let d = &roster.devices[&con_device];
        assert!(!d.revoked);
        assert!(d.accounts.contains(&mal_account), "control: mal's certificate counts as a certificate");
        assert!(roster.account_holds_device(w.people["con"].account_id, con_device));
        assert!(!roster.account_holds_device(mal_account, con_device), "an adopter holds the device");
        // An unbound agent of mal's never runs on con's device.
        let mut agent = hive_core::WorkspaceAgent::new("helper", "gw");
        agent.owner_actor_id = "mal".into();
        let agent_id = agent.id;
        let up = w.push("mal", SessionEvent::AgentUpserted { agent });
        assert_eq!(w.verdict(up), Verdict::Allowed);
        assert!(!w.roster().agent_bound_to(agent_id, con_device), "mal's agent runs on con's device");
        // con still revokes their own device.
        let own = w.push("con", revoke);
        assert_eq!(w.verdict(own), Verdict::Allowed, "the adoption blocked con's own revocation");
    }
}

/// A phone (its certificate published by its person's laptop, so not by the
/// phone itself, which can't enroll itself): someone else certifying its key
/// for themselves can't sign as it, and its person and the workspace's Owner
/// still revoke it. (Its person's laptop and the adopter's publish
/// certificates alike, and the phone vouches for neither on the log, so the
/// adopter holds it too, though not as their own to revoke: see
/// `an_adopter_cannot_revoke_a_phone_that_vouched_for_no_one`.)
#[test]
fn certifying_someone_elses_phone_key_signs_nothing() {
    let mut w = SignedWorld::genesis(&["owner", "con", "mal"], None);
    w.admit("owner", "con", WorkspaceRole::Contributor);
    w.admit("owner", "mal", WorkspaceRole::Contributor);
    let phone = Phone::new();
    let cert = w.phone_certificate("con", &phone);
    let enrolled = w.push("con", SessionEvent::DeviceCertificateAdded { certificate: cert });
    assert_eq!(w.verdict(enrolled), Verdict::Allowed, "control: con enrolls a phone");
    let adopt = w.phone_certificate("mal", &phone);
    w.push("mal", SessionEvent::DeviceCertificateAdded { certificate: adopt });
    assert!(w.roster().account_holds_device(w.people["con"].account_id, phone.device_id), "the adoption took con's phone away");
    // mal signs a key event as con under the phone's id with a key of mal's.
    let key = p256::ecdsa::SigningKey::from_slice(&[9u8; 32]).unwrap();
    let mut forged = w.draft("con", None, ka(phone.device_id), None);
    if let Some(s) = forged.actor_stamp.as_mut() {
        s.sig_alg = hive_core::crypto::SigAlg::P256;
    }
    forged.clear_raw();
    hive_core::crypto::sign_envelope_p256(&mut forged, phone.device_id, &key);
    let id = w.push_env(None, forged);
    assert_eq!(w.verdict(id), Verdict::Denied, "mal signed as con through con's phone id");
    let revoke = SessionEvent::DeviceRevoked { device_id: phone.device_id, watermark: Vec::new() };
    let mut owner = w.clone();
    let by_owner = owner.push("owner", revoke.clone());
    assert_eq!(owner.verdict(by_owner), Verdict::Allowed, "an Owner can't revoke con's phone after the adoption");
    // con's own revocation goes down the ranks over the adopter too (see
    // `an_adopter_cannot_revoke_a_phone_that_vouched_for_no_one`).
    let by_con = w.push("con", revoke);
    assert_eq!(w.verdict(by_con), Verdict::Denied, "con, mal's peer, revoked a phone mal holds too");
}

/// An adopter of a device that vouched for no one on the log (a phone, a box
/// its person's desktop enrolled) is one of its holders, as its person is:
/// the log can't tell the two certificates apart. Holding it that way must
/// not make it the adopter's to revoke as their own (R5): any member could
/// then revoke anyone's phone by certifying its key for themselves. A
/// device more than one account certified, that vouched for none of them,
/// is revoked down the ranks over all of them; its person still revokes it
/// once the adopter is gone, and an Owner always can.
#[test]
fn an_adopter_cannot_revoke_a_phone_that_vouched_for_no_one() {
    let mut w = SignedWorld::genesis(&["owner", "con", "mal"], None);
    w.admit("owner", "con", WorkspaceRole::Contributor);
    w.admit("owner", "mal", WorkspaceRole::Contributor);
    let phone = Phone::new();
    let cert = w.phone_certificate("con", &phone);
    w.push("con", SessionEvent::DeviceCertificateAdded { certificate: cert });
    let adopt = w.phone_certificate("mal", &phone);
    let adopted = w.push("mal", SessionEvent::DeviceCertificateAdded { certificate: adopt });
    assert_eq!(w.verdict(adopted), Verdict::Allowed, "control: a well-formed certificate of mal's");
    let revoke = SessionEvent::DeviceRevoked { device_id: phone.device_id, watermark: Vec::new() };
    let mut by_mal = w.clone();
    let id = by_mal.push("mal", revoke.clone());
    assert_eq!(by_mal.verdict(id), Verdict::Denied, "mal revoked con's phone by certifying its key for mal");
    assert!(!by_mal.roster().devices[&phone.device_id].revoked);
    // An Owner revokes it.
    let mut by_owner = w.clone();
    let id = by_owner.push("owner", revoke.clone());
    assert_eq!(by_owner.verdict(id), Verdict::Allowed);
    // Once mal is gone, con revokes it again.
    let mal_id = w.people["mal"].actor().id;
    let removed = w.push("owner", SessionEvent::MemberRemoved { member_id: mal_id, watermark: Vec::new() });
    assert_eq!(w.verdict(removed), Verdict::Allowed, "control: the Owner removes mal");
    let id = w.push("con", revoke);
    assert_eq!(w.verdict(id), Verdict::Allowed, "con can't revoke their own phone once the adopter is gone");
}

/// The same for an Ed25519 device that never signed anything on the config
/// log (a box enrolled by its person's desktop): an adopter can't revoke it.
#[test]
fn an_adopter_cannot_revoke_a_box_that_vouched_for_no_one() {
    let mut w = SignedWorld::genesis(&["owner", "con", "mal"], None);
    w.admit("owner", "con", WorkspaceRole::Contributor);
    w.admit("owner", "mal", WorkspaceRole::Contributor);
    let boxk = SigningKeypair::generate().unwrap();
    let enroll = DeviceCertificate::issue(&w.people["con"].account, w.people["con"].account_id, &boxk.public_key_bytes(), Timestamp::epoch());
    let device = enroll.device_id;
    w.push("con", SessionEvent::DeviceCertificateAdded { certificate: enroll });
    let adopt = DeviceCertificate::issue(&w.people["mal"].account, w.people["mal"].account_id, &boxk.public_key_bytes(), Timestamp::epoch());
    w.push("mal", SessionEvent::DeviceCertificateAdded { certificate: adopt });
    let revoke = SessionEvent::DeviceRevoked { device_id: device, watermark: Vec::new() };
    let id = w.push("mal", revoke);
    assert_eq!(w.verdict(id), Verdict::Denied, "mal revoked con's box by certifying its key for mal");
}

/// A phone certificate never takes over a device in the fold, with or
/// without presence: one naming a desktop's id or another phone's id with a
/// new key names an id its key doesn't derive (refused wherever it sorts),
/// and the only certificate a phone's key derives an existing id for is
/// that phone's own (a re-publication).
#[test]
fn a_phone_certificate_never_takes_over_a_device_in_the_fold() {
    let mut w = world();
    let phone = Phone::new();
    let first = w.phone_certificate("con", &phone);
    let id = w.push("con", SessionEvent::DeviceCertificateAdded { certificate: first });
    assert_eq!(w.verdict(id), Verdict::Allowed, "control: con enrolls a phone");
    let other = Phone::new();
    let people = w.people.clone();
    let con = &people["con"];
    for target in [con.device_id, phone.device_id, people["owner"].device_id] {
        let cert = DeviceCertificate::issue_p256_for_id(&con.account, con.account_id, target, &other.public_key(), Timestamp::epoch());
        let with = w.push("con", SessionEvent::DeviceCertificateAdded { certificate: cert.clone() });
        let without = w.push_bare("con", SessionEvent::DeviceCertificateAdded { certificate: cert });
        assert_eq!((w.verdict(with), w.verdict(without)), (Verdict::Denied, Verdict::Denied), "a phone certificate took over {target}");
    }
    let roster = w.roster();
    assert_eq!(roster.devices[&con.device_id].public_key, con.device.public_key_bytes().to_vec());
    assert!(!roster.devices[&con.device_id].phone);
    assert_eq!(roster.devices[&phone.device_id].public_key, phone.public_key());
    assert!(!roster.devices.contains_key(&other.device_id));
}

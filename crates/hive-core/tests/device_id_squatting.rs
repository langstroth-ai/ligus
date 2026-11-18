//! Device-id squatting (a sibling of erase-by-forgery). Device ids are
//! chosen by the device, and a certificate is signed by its account's key,
//! so a member can certify *another member's* device id under their own
//! account with a key of their own. That binding is the squatter's own: it
//! must never replace the victim's key for the victim's events (an erasure:
//! every key event the victim's device signs fails to verify) nor let the
//! squatter sign as the victim through the victim's device id.

use hive_core::crypto::{sign_envelope, DeviceCertificate, SigningKeypair};
use hive_core::testkit::SignedWorld;
use hive_core::{SessionEvent, Timestamp, Verdict, WorkspaceRole};

/// `mal` certifies `con`'s device id under mal's own account, with mal's
/// presence (a new device for an account with a recovery key needs it).
fn squat(w: &mut SignedWorld) -> SigningKeypair {
    let key = SigningKeypair::generate().unwrap();
    let mal = &w.people["mal"];
    let cert = DeviceCertificate::issue(&mal.account, mal.account_id, w.people["con"].device_id, &key.public_key_bytes(), Timestamp::epoch());
    let id = w.push("mal", SessionEvent::DeviceCertificateAdded { certificate: cert });
    assert_eq!(w.verdict(id), Verdict::Allowed, "a member certifies a device of their own account");
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

#[test]
fn a_squatted_device_id_keeps_its_own_key_for_its_own_account() {
    let mut w = world();
    squat(&mut w);
    let con_device = w.people["con"].device_id;
    // con's device publishes its own key-agreement key: a key event, so the
    // fold checks the signature against the device's certified key.
    let id = w.push_bare("con", ka(con_device));
    assert_eq!(w.verdict(id), Verdict::Allowed, "the victim's own key event no longer verifies");
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

/// Revoking a device your own account certified is self-service (R5), but a
/// squatter's certificate of someone else's device id is not that device:
/// revoking "their" device must not revoke the victim's, and least of all
/// the founder's (whom nobody cuts).
#[test]
fn a_squatter_cannot_revoke_the_victims_device_as_their_own() {
    for victim in ["con", "owner"] {
        let mut w = world();
        let device = w.people[victim].device_id;
        let key = SigningKeypair::generate().unwrap();
        let mal = &w.people["mal"];
        let cert = DeviceCertificate::issue(&mal.account, mal.account_id, device, &key.public_key_bytes(), Timestamp::epoch());
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

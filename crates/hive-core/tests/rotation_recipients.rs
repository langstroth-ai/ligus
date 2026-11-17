//! Regression (council addendum to Phase A): a key rotation seals only to
//! the devices of **current members** that published their own key-agreement
//! key. A device an agent is merely bound to (a host outside the member
//! set) is not a recipient, and a removed member's devices drop out.

use hive_core::testkit::SignedWorld;
use hive_core::{SessionEvent, WorkspaceAgent, WorkspaceRole};

#[test]
fn rotations_seal_only_to_current_members_devices() {
    let mut w = SignedWorld::genesis(&["owner", "mara", "bea"], None);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    // Bea published an identity and a key-agreement key, but isn't a member.
    w.publish_identity("bea");
    for who in ["owner", "mara", "bea"] {
        let device_id = w.people[who].device_id;
        w.push_bare(who, SessionEvent::DeviceKeyAgreementPublished { device_id, ka_public: vec![who.len() as u8; 32] });
    }
    // An agent bound to Bea's device (an Admin's act, with presence).
    let mut agent = WorkspaceAgent::new("coder", "gw");
    agent.host_id = w.people["bea"].device_id.to_string();
    w.push("owner", SessionEvent::AgentUpserted { agent });
    let r = w.roster().rotation_recipients();
    let (o, m, b) = (w.people["owner"].device_id, w.people["mara"].device_id, w.people["bea"].device_id);
    assert!(r.contains_key(&o) && r.contains_key(&m));
    assert!(!r.contains_key(&b), "a bound host outside the members got the key");
    // After Mara's removal her device drops out.
    w.push("owner", SessionEvent::MemberRemoved { member_id: "mara".into(), watermark: vec![] });
    let r = w.roster().rotation_recipients();
    assert!(r.contains_key(&o) && !r.contains_key(&m));
}

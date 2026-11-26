//! X2 (identity model v2, section 16): a workspace moved to rules 3 is
//! read-only on the released 0.3 core, both as 0.4 founds one (genesis
//! rules 2, then rules 3 with the founder's first credential) and as an
//! Owner switches an older one; the switch with no presence is refused on
//! both, and nothing before it folds differently.

use ligus_core::crypto::DeviceCertificate;
use ligus_core::testkit::SignedWorld;
use ligus_core::{SessionEvent, SessionEventEnvelope, Timestamp, WorkspaceRole};
use ligus_core_v03 as old;

fn to_old(envs: &[SessionEventEnvelope]) -> Vec<old::SessionEventEnvelope> {
    envs.iter().map(|e| serde_json::from_str::<old::SessionEventEnvelope>(&serde_json::to_string(e).unwrap()).unwrap()).collect()
}

#[test]
fn a_workspace_on_rules_3_is_read_only_on_0_3() {
    // As 0.4 founds: genesis rules 2 (testkit), the founder's credential,
    // then rules 3 with presence.
    let mut w = SignedWorld::genesis(&["ana", "ben"], None);
    w.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    w.admit("ana", "ben", WorkspaceRole::Admin);
    let c = DeviceCertificate::issue(&w.people["ben"].account, w.people["ben"].account_id, &w.people["ana"].device.public_key_bytes(), Timestamp::epoch());
    w.push("ben", SessionEvent::DeviceCertificateAdded { certificate: c });
    let (_, new_t) = ligus_core::project_config(&w.cfg, None, None);
    let (old_s, _) = old::project_config(&to_old(&w.cfg), None, None);
    assert_eq!(new_t.current().rules, 3);
    assert!(old_s.unwrap().authz.read_only(), "0.3 keeps writing on a rules 3 workspace");
    assert!(!new_t.current().account_certified(w.people["ben"].account_id, w.people["ana"].device_id));
}

#[test]
fn a_switch_without_presence_is_refused_on_both() {
    let mut w = SignedWorld::genesis(&["ana"], None);
    let id = w.push_bare("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    let new_v = ligus_core::fold_decisions(&w.cfg, None, None, ligus_core::WorkspaceAuthz::Unchecked);
    let old_v = old::fold_decisions(&to_old(&w.cfg), None, None, old::WorkspaceAuthz::Unchecked);
    assert_eq!(format!("{:?}", new_v[&id]), "Denied");
    assert_eq!(format!("{:?}", old_v[&id]), "Denied");
    let (old_s, _) = old::project_config(&to_old(&w.cfg), None, None);
    assert!(!old_s.unwrap().authz.read_only());
}

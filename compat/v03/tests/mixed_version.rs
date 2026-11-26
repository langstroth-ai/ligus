//! X1 (identity model v2, section 16): the released 0.3 core and this core
//! fold the same rules-2 logs alike. The new kinds (`profileUpdated`,
//! `deviceLabelSet`, `memberRetired`) and the additive `phoneConsent` field
//! are interleaved, valid and malformed, and from phones and non-members; a
//! 0.3 build reads the new kinds as `Unknown`. Every verdict of every
//! pre-existing kind, the roster (members, roles, devices, holders) and the
//! projection apart from the render-only parts must be equal.

use std::collections::BTreeMap;

use ligus_core::crypto::{phone_consent_preimage, DeviceCertificate, SigningKeypair};
use ligus_core::profile::{Avatar, DeviceLabel, ProfileChange};
use ligus_core::retirement::MemberRetirement;
use ligus_core::testkit::{Phone, SignedWorld};
use ligus_core::{ChatMessage, MessageRole, SessionEvent, SessionEventEnvelope, Timestamp, WorkspaceRole};
use uuid::Uuid;

use ligus_core_v03 as old;

fn png() -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    v.extend_from_slice(&[3u8; 24]);
    v
}

fn to_old(envs: &[SessionEventEnvelope]) -> Vec<old::SessionEventEnvelope> {
    envs.iter()
        .map(|e| serde_json::from_str::<old::SessionEventEnvelope>(&serde_json::to_string(e).unwrap()).expect("0.3 reads every envelope"))
        .collect()
}

fn new_kind(e: &SessionEvent) -> bool {
    matches!(e, SessionEvent::ProfileUpdated { .. } | SessionEvent::DeviceLabelSet { .. } | SessionEvent::MemberRetired { .. })
}

/// A rules-2 workspace with the new kinds mixed in.
fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["ana", "ben", "cy", "eve"], None);
    w.admit("ana", "ben", WorkspaceRole::Admin);
    w.admit("ana", "cy", WorkspaceRole::Contributor);
    w.publish_identity("eve");
    let (ana, ben, cy, eve) = (w.people["ana"].account_id, w.people["ben"].account_id, w.people["cy"].account_id, w.people["eve"].account_id);
    let chat = w.chat("ana", "plans");
    let name = |a: Uuid, n: &str| SessionEvent::ProfileUpdated {
        profile: ProfileChange { account_id: a, display_name: Some(n.into()), avatar: None, clear_avatar: false, issued_at_ms: 1 },
    };
    w.push_bare("ana", name(ana, "Ana Ruiz"));
    w.push_bare("ben", name(ana, "Not Ana")); // someone else's: denied on 0.4
    w.push_bare("cy", name(cy, " bad  name ")); // malformed
    w.push_bare("eve", name(eve, "Eve")); // a non-member
    w.push_bare("ben", SessionEvent::ProfileUpdated {
        profile: ProfileChange { account_id: ben, display_name: None, avatar: Some(Avatar::new("image/png", &png()).unwrap()), clear_avatar: false, issued_at_ms: 2 },
    });
    let desk = w.people["ana"].device_id;
    w.push_bare("ana", SessionEvent::DeviceLabelSet { label: DeviceLabel { device_id: desk, account_id: ana, label: "Ana's MacBook".into() } });
    w.push_bare("ben", SessionEvent::DeviceLabelSet { label: DeviceLabel { device_id: desk, account_id: ana, label: "forged".into() } });
    // A retirement under rules 2: refused on 0.4, Unknown on 0.3.
    let r = MemberRetirement::sign(&w.people["cy"].account, w.wid, "cy", cy, ana, desk);
    w.push_bare("cy", SessionEvent::MemberRetired { retirement: r });
    // A foreign certificate and a phone with a statement and a foreign copy.
    let c = DeviceCertificate::issue(&w.people["ben"].account, ben, &w.people["ana"].device.public_key_bytes(), Timestamp::epoch());
    w.push("ben", SessionEvent::DeviceCertificateAdded { certificate: c });
    let phone = Phone::new();
    let mut pc = w.phone_certificate("ana", &phone);
    pc.phone_consent = Some(phone.sign(&phone_consent_preimage(w.wid, ana, phone.device_id)));
    w.push("ana", SessionEvent::DeviceCertificateAdded { certificate: pc });
    let foreign = w.phone_certificate("ben", &phone);
    w.push("ben", SessionEvent::DeviceCertificateAdded { certificate: foreign });
    // A phone writes a new kind (denied by the phone tier on 0.4).
    w.phone_write("ana", &phone, None, name(ana, "From the phone"));
    // Ordinary traffic around them.
    w.say(chat, "ana", SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "ana", "hello") });
    w.say(chat, "cy", SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "cy", "hi") });
    w.push("ana", SessionEvent::MemberRemoved { member_id: "cy".into(), watermark: Vec::new() });
    w.push_bare("ana", name(ana, "Ana R."));
    let _ = SigningKeypair::generate();
    w
}

#[test]
fn the_released_core_and_this_one_fold_the_same_rules_2_logs() {
    let w = world();
    let old_cfg = to_old(&w.cfg);
    // Verdicts of every pre-existing kind are equal.
    let new_v = ligus_core::fold_decisions(&w.cfg, None, None, ligus_core::WorkspaceAuthz::Unchecked);
    let old_v = old::fold_decisions(&old_cfg, None, None, old::WorkspaceAuthz::Unchecked);
    let mut compared = 0;
    for e in &w.cfg {
        if new_kind(&e.payload) {
            continue;
        }
        let (n, o) = (new_v.get(&e.event_id), old_v.get(&e.event_id));
        assert_eq!(n.map(|v| format!("{v:?}")), o.map(|v| format!("{v:?}")), "{} differs", e.payload.kind_str());
        compared += 1;
    }
    assert!(compared > 10);
    // The roster agrees.
    let (new_s, new_t) = ligus_core::project_config(&w.cfg, None, None);
    let (old_s, old_t) = old::project_config(&old_cfg, None, None);
    let (new_s, old_s) = (new_s.unwrap(), old_s.unwrap());
    let members = |ids: Vec<(String, String)>| ids;
    assert_eq!(
        members(new_s.members.iter().map(|m| (m.id.clone(), format!("{:?}", m.role))).collect()),
        members(old_s.members.iter().map(|m| (m.id.clone(), format!("{:?}", m.role))).collect()),
    );
    let (nr, or) = (new_t.current(), old_t.current());
    assert_eq!(nr.rules, or.rules);
    let devices = |d: BTreeMap<Uuid, (Vec<Uuid>, Vec<Uuid>, bool)>| d;
    assert_eq!(
        devices(nr.devices.iter().map(|(k, d)| (*k, (d.accounts.iter().copied().collect(), d.holders.iter().copied().collect(), d.revoked))).collect()),
        devices(or.devices.iter().map(|(k, d)| (*k, (d.accounts.iter().copied().collect(), d.holders.iter().copied().collect(), d.revoked))).collect()),
        "devices or holders differ"
    );
    assert_eq!(new_s.workspace_hosts.len(), old_s.workspace_hosts.len());
    // The chats fold alike too.
    for (id, log) in &w.chats {
        let nt = new_t.clone();
        let ot = old_t.clone();
        let n = ligus_core::project_authorized(log, None, None, ligus_core::WorkspaceAuthz::Timeline(&nt)).map(|s| s.messages.iter().map(|m| m.id).collect::<Vec<_>>());
        let o = old::project_authorized(&to_old(log), None, None, old::WorkspaceAuthz::Timeline(&ot)).map(|s| s.messages.iter().map(|m| m.id).collect::<Vec<_>>());
        assert_eq!(n, o, "chat {id}");
    }
    // And the render-only parts are where they differ: 0.4 has them.
    assert_eq!(new_s.profiles.get(&w.people["ana"].account_id).and_then(|p| p.display_name.as_deref()), Some("Ana R."));
}

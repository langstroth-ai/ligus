//! Workspace model v3, 16.3: the released 0.4 core and this core fold the
//! same rules-3 logs alike. `memberLeft` (valid, refused, from a box, from a
//! phone, from a non-member) is interleaved with ordinary traffic and
//! governance; a 0.4 build reads it as `Unknown`. Every verdict of every
//! pre-existing kind, the roster (members, roles, devices, holders), the
//! rotation set and the chats must be equal, and the member who left stays
//! a member on both builds.

use std::collections::BTreeMap;

use ligus_core::testkit::{Phone, SignedWorld};
use ligus_core::{ActorKind, ChatMessage, MessageRole, SessionEvent, SessionEventEnvelope, WorkspaceRole};
use uuid::Uuid;

use ligus_core_v04 as old;

fn to_old(envs: &[SessionEventEnvelope]) -> Vec<old::SessionEventEnvelope> {
    envs.iter()
        .map(|e| serde_json::from_str::<old::SessionEventEnvelope>(&serde_json::to_string(e).unwrap()).expect("0.4 reads every envelope"))
        .collect()
}

fn leave(w: &SignedWorld, who: &str) -> SessionEvent {
    SessionEvent::MemberLeft { member_id: who.into(), account_id: Some(w.people[who].account_id), watermark: Vec::new() }
}

/// A rules-3 workspace with leaves mixed in.
fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["ana", "ben", "cy", "box", "eve"], None);
    w.admit("ana", "ben", WorkspaceRole::Admin);
    w.admit("ana", "cy", WorkspaceRole::Contributor);
    w.kinds.insert("box".into(), ActorKind::Service);
    let m = w.member_of("box", WorkspaceRole::Contributor);
    w.push("ana", SessionEvent::MemberAdded { member: m, invite: None, proof: None });
    w.publish_identity("box");
    w.publish_identity("eve");
    w.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    let chat = w.chat("ana", "plans");
    w.say(chat, "cy", SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "cy", "hello") });
    w.push("cy", leave(&w, "cy")); // valid on 0.5
    w.push_bare("box", leave(&w, "box")); // a box: valid on 0.5
    w.push("ana", leave(&w, "ana")); // the founder: refused on 0.5
    w.push("ben", leave(&w, "cy")); // someone else's: refused
    w.push_bare("eve", leave(&w, "eve")); // a non-member
    let phone = Phone::new();
    let pc = w.phone_certificate("ben", &phone);
    w.push("ben", SessionEvent::DeviceCertificateAdded { certificate: pc });
    let l = leave(&w, "ben");
    w.phone_write("ben", &phone, None, l); // a phone: refused
    // The member who asked to leave keeps writing on both builds.
    w.say(chat, "cy", SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "cy", "still here") });
    w.say(chat, "box", SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "box", "box here") });
    w.push("ben", SessionEvent::SessionTitleChanged { title: "Team".into() });
    w.push("ben", leave(&w, "ben")); // valid
    // An Admin removes the box that asked to leave.
    w.push("ben", SessionEvent::MemberRemoved { member_id: "box".into(), watermark: Vec::new() });
    w.say(chat, "ana", SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "ana", "bye box") });
    w
}

fn is_leave(e: &SessionEventEnvelope) -> bool {
    matches!(e.payload, SessionEvent::MemberLeft { .. })
}

#[test]
fn the_released_core_and_this_one_fold_the_same_rules_3_logs() {
    let w = world();
    let old_cfg = to_old(&w.cfg);
    // 0.4 reads the new kind as Unknown.
    let leaves = old_cfg.iter().filter(|e| matches!(e.payload, old::SessionEvent::Unknown)).count();
    assert_eq!(leaves, w.cfg.iter().filter(|e| is_leave(e)).count());
    assert!(leaves >= 6);
    // Verdicts of every pre-existing kind are equal.
    let new_v = ligus_core::fold_decisions(&w.cfg, None, None, ligus_core::WorkspaceAuthz::Unchecked);
    let old_v = old::fold_decisions(&old_cfg, None, None, old::WorkspaceAuthz::Unchecked);
    let mut compared = 0;
    for e in w.cfg.iter().filter(|e| !is_leave(e)) {
        let (n, o) = (new_v.get(&e.event_id), old_v.get(&e.event_id));
        assert_eq!(n.map(|v| format!("{v:?}")), o.map(|v| format!("{v:?}")), "{} differs", e.payload.kind_str());
        compared += 1;
    }
    assert!(compared > 15);
    // The roster agrees, and the member who left is still one.
    let (new_s, new_t) = ligus_core::project_config(&w.cfg, None, None);
    let (old_s, old_t) = old::project_config(&old_cfg, None, None);
    let (new_s, old_s) = (new_s.unwrap(), old_s.unwrap());
    let members = |ids: Vec<(String, String)>| ids;
    assert_eq!(
        members(new_s.members.iter().map(|m| (m.id.clone(), format!("{:?}", m.role))).collect()),
        members(old_s.members.iter().map(|m| (m.id.clone(), format!("{:?}", m.role))).collect()),
    );
    assert!(new_s.members.iter().any(|m| m.id == "cy") && old_s.members.iter().any(|m| m.id == "cy"));
    assert!(new_s.left.contains_key("cy") && new_s.left.contains_key("ben"), "0.5 shows them as asked to leave");
    assert!(!new_s.left.contains_key("box"), "removed since");
    let (nr, or) = (new_t.current(), old_t.current());
    assert_eq!(nr.rules, or.rules);
    let devices = |d: BTreeMap<Uuid, (Vec<Uuid>, Vec<Uuid>, bool)>| d;
    assert_eq!(
        devices(nr.devices.iter().map(|(k, d)| (*k, (d.accounts.iter().copied().collect(), d.holders.iter().copied().collect(), d.revoked))).collect()),
        devices(or.devices.iter().map(|(k, d)| (*k, (d.accounts.iter().copied().collect(), d.holders.iter().copied().collect(), d.revoked))).collect()),
        "devices or holders differ"
    );
    assert_eq!(
        nr.rotation_recipients().into_keys().collect::<Vec<_>>(),
        or.rotation_recipients().into_keys().collect::<Vec<_>>(),
        "the rotation set differs"
    );
    // The chats fold alike too.
    for (id, log) in &w.chats {
        let nt = new_t.clone();
        let ot = old_t.clone();
        let n = ligus_core::project_authorized(log, None, None, ligus_core::WorkspaceAuthz::Timeline(&nt)).map(|s| s.messages.iter().map(|m| (m.id, m.body.clone())).collect::<Vec<_>>());
        let o = old::project_authorized(&to_old(log), None, None, old::WorkspaceAuthz::Timeline(&ot)).map(|s| s.messages.iter().map(|m| (m.id, m.body.clone())).collect::<Vec<_>>());
        assert_eq!(n, o, "chat {id}");
        // Cy asked to leave and still writes, on both builds.
        assert!(n.unwrap().iter().any(|(_, b)| b == "still here"));
    }
}

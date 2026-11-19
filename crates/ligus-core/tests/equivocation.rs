//! Equivocation (owner decision on review P-A2c): event ids are chosen by
//! their author, so a signer can publish two different envelopes under one
//! id. Every device denies both, whatever order they arrived in, and
//! watermark coverage never passes through an equivocated id.

use ligus_core::events::{fold_decisions, project_authorized, WorkspaceAuthz};
use ligus_core::testkit::SignedWorld;
use ligus_core::watermark::heads_of;
use ligus_core::{workspace_config_session_id, ChatMessage, MessageRole, SessionEvent, SessionEventEnvelope, Verdict, WorkspaceRole};
use proptest::prelude::*;
use std::collections::BTreeSet;
use uuid::Uuid;

fn post(w: &mut SignedWorld, chat: Uuid, by: &str, body: &str) -> Uuid {
    w.say(chat, by, SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, by, body) })
}

/// A second envelope under `id`'s event id with other content, signed by
/// the same device (an equivocation), appended to the chat log.
fn equivocate(w: &mut SignedWorld, chat: Uuid, by: &str, id: Uuid) {
    let orig = w.chats[&chat].iter().find(|e| e.event_id == id).unwrap().clone();
    let mut twin = w.draft(by, Some(chat), SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, by, "the other story") }, None);
    twin.event_id = id;
    twin.lamport = orig.lamport;
    if let Some(s) = twin.actor_stamp.as_mut() {
        s.prev = orig.actor_stamp.as_ref().and_then(|o| o.prev);
    }
    w.finish(&mut twin, by, None);
    w.push_env(Some(chat), twin);
}

fn chat_verdicts(w: &SignedWorld, log: &[SessionEventEnvelope]) -> std::collections::BTreeMap<Uuid, Verdict> {
    let t = w.timeline();
    fold_decisions(log, None, None, WorkspaceAuthz::Timeline(&t))
}

fn bodies(w: &SignedWorld, log: &[SessionEventEnvelope]) -> Vec<String> {
    let t = w.timeline();
    project_authorized(log, None, None, WorkspaceAuthz::Timeline(&t)).unwrap().messages.into_iter().map(|m| m.body).collect()
}

#[test]
fn both_envelopes_of_an_equivocated_id_are_denied_in_any_order() {
    let mut w = SignedWorld::genesis(&["owner", "con"], None);
    w.admit("owner", "con", WorkspaceRole::Contributor);
    let chat = w.chat("owner", "c");
    let a = post(&mut w, chat, "con", "first");
    let b = post(&mut w, chat, "con", "one story");
    let c = post(&mut w, chat, "con", "third");
    equivocate(&mut w, chat, "con", b);
    let log = w.chats[&chat].clone();
    let mut rev = log.clone();
    rev.reverse();
    let v = chat_verdicts(&w, &log);
    assert_eq!(v[&b], Verdict::Denied, "an equivocated id never folds");
    assert_eq!((v[&a], v[&c]), (Verdict::Allowed, Verdict::Allowed));
    assert_eq!(v, chat_verdicts(&w, &rev), "the same in either arrival order");
    let shown = bodies(&w, &log);
    assert_eq!(shown, bodies(&w, &rev));
    assert!(!shown.iter().any(|m| m == "one story" || m == "the other story"), "{shown:?}");
    // An identical duplicate (the same envelope twice) is not an equivocation.
    let mut dup = log.clone();
    dup.push(log.iter().find(|e| e.event_id == a).unwrap().clone());
    assert_eq!(chat_verdicts(&w, &dup)[&a], Verdict::Allowed);
}

/// Coverage never passes through an equivocated id: the removed member's
/// event behind it isn't covered, so it's judged as of the removal.
#[test]
fn watermark_coverage_stops_at_an_equivocated_id() {
    let mut w = SignedWorld::genesis(&["owner", "con"], None);
    w.admit("owner", "con", WorkspaceRole::Contributor);
    let chat = w.chat("owner", "c");
    let e1 = post(&mut w, chat, "con", "one");
    let e2 = post(&mut w, chat, "con", "two");
    let e3 = post(&mut w, chat, "con", "three");
    let signers: BTreeSet<Uuid> = [w.people["con"].device_id].into_iter().collect();
    let mut wm = heads_of(&w.cfg, workspace_config_session_id(w.wid), &signers);
    wm.extend(heads_of(&w.chats[&chat], chat, &signers));
    equivocate(&mut w, chat, "con", e2);
    w.push("owner", SessionEvent::MemberRemoved { member_id: "con".into(), watermark: wm });
    let v = w.verdicts();
    assert_eq!(v[&e3], Verdict::Allowed, "the head is covered");
    assert_eq!(v[&e2], Verdict::Denied, "the equivocated id");
    assert_eq!(v[&e1], Verdict::Denied, "behind an equivocated id: not covered, judged as of the removal");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Which ids are denied, and the folded chat, don't depend on arrival
    /// order; every equivocated id is denied.
    #[test]
    fn prop_equivocation_is_order_independent(n in 1usize..12, mask in any::<u16>(), seed in any::<u64>()) {
        let mut w = SignedWorld::genesis(&["owner", "con"], None);
        w.admit("owner", "con", WorkspaceRole::Contributor);
        let chat = w.chat("owner", "c");
        let ids: Vec<Uuid> = (0..n).map(|i| post(&mut w, chat, "con", &format!("m{i}"))).collect();
        let eq: Vec<Uuid> = ids.iter().enumerate().filter(|(i, _)| mask & (1 << i) != 0).map(|(_, id)| *id).collect();
        for id in &eq {
            equivocate(&mut w, chat, "con", *id);
        }
        let log = w.chats[&chat].clone();
        let mut shuffled = log.clone();
        let mut s = seed;
        for i in (1..shuffled.len()).rev() {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            shuffled.swap(i, (s >> 33) as usize % (i + 1));
        }
        let (v1, v2) = (chat_verdicts(&w, &log), chat_verdicts(&w, &shuffled));
        prop_assert_eq!(&v1, &v2);
        prop_assert_eq!(bodies(&w, &log), bodies(&w, &shuffled));
        for id in &eq {
            prop_assert_eq!(v1[id], Verdict::Denied);
        }
        for id in ids.iter().filter(|i| !eq.contains(i)) {
            prop_assert_eq!(v1[id], Verdict::Allowed);
        }
    }
}

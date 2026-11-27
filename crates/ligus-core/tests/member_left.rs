//! `MemberLeft` (workspace model v3, 10.4; invariant W3): a member leaves by
//! itself, never the founder or the last Owner; two Owners leaving at once
//! leave one; a delegated or phone device can't write it for a person; under
//! rules 3 authorization and the rotation set are identical with and without
//! it; of a leave and a retirement for one member, the first applies.

use ligus_core::crypto::{DeviceCertificate, SigningKeypair};
use ligus_core::events::Verdict;
use ligus_core::testkit::{Phone, SignedWorld};
use ligus_core::{ActorKind, MemberRoleChange, SessionEvent, SessionEventEnvelope, Timestamp, WorkspaceRole};

fn leave(w: &SignedWorld, who: &str) -> SessionEvent {
    SessionEvent::MemberLeft { member_id: w.people[who].actor().id, account_id: Some(w.people[who].account_id), watermark: Vec::new() }
}

/// Ana (founder, Owner), Ben (Admin), Cy (Contributor), a box (Service), on
/// rules 3. Everyone but the box holds a recovery key.
fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["ana", "ben", "cy", "box"], None);
    w.admit("ana", "ben", WorkspaceRole::Admin);
    w.admit("ana", "cy", WorkspaceRole::Contributor);
    w.kinds.insert("box".into(), ActorKind::Service);
    let m = w.member_of("box", WorkspaceRole::Contributor);
    w.push("ana", SessionEvent::MemberAdded { member: m, invite: None, proof: None });
    w.publish_identity("box");
    let id = w.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    assert_eq!(w.verdict(id), Verdict::Allowed);
    w
}

fn id_of(w: &SignedWorld, who: &str) -> String {
    w.people[who].actor().id
}

#[test]
fn a_person_and_a_box_leave_and_stay_members_under_rules_3() {
    let mut w = world();
    let cy = leave(&w, "cy");
    let bare = w.push_bare("cy", cy.clone());
    assert_eq!(w.verdict(bare), Verdict::Denied, "Cy holds a recovery key: her leave needs it");
    let ok = w.push("cy", cy);
    assert_eq!(w.verdict(ok), Verdict::Allowed);
    let b = leave(&w, "box");
    let boxed = w.push_bare("box", b);
    assert_eq!(w.verdict(boxed), Verdict::Allowed, "a box leaves with its own device key, no passkey");
    let c = w.config();
    assert!(c.left.contains_key(&id_of(&w, "cy")) && c.left.contains_key(&id_of(&w, "box")));
    assert!(c.members.iter().any(|m| m.id == id_of(&w, "cy")), "display only: Cy is still a member");
    // She still writes.
    let chat = w.chat("ana", "plans");
    let said = w.say(chat, "cy", SessionEvent::SessionTitleChanged { title: "still here".into() });
    assert_eq!(w.verdict(said), Verdict::Allowed);
    // Twice is refused.
    let again = w.push("cy", leave(&w, "cy"));
    assert_eq!(w.verdict(again), Verdict::Denied);
    // An Admin removes her: the leave is gone with her.
    let cy_id = id_of(&w, "cy");
    w.push("ben", SessionEvent::MemberRemoved { member_id: cy_id.clone(), watermark: Vec::new() });
    assert!(!w.config().left.contains_key(&cy_id));
}

#[test]
fn only_the_member_itself_leaves_for_its_own_account() {
    let mut w = world();
    // Ben "leaving" Cy.
    let mut forged = leave(&w, "cy");
    let id = w.push("ben", forged.clone());
    assert_eq!(w.verdict(id), Verdict::Denied, "an Admin removes someone else; nobody leaves for them");
    // Cy naming Ben's account.
    if let SessionEvent::MemberLeft { account_id, .. } = &mut forged {
        *account_id = Some(w.people["ben"].account_id);
    }
    let id = w.push("cy", forged);
    assert_eq!(w.verdict(id), Verdict::Denied);
    // On a chat log.
    let chat = w.chat("ana", "plans");
    let l = leave(&w, "cy");
    let on_chat = w.write("cy", Some(chat), l, None, ligus_core::testkit::Proof::Recovery);
    assert_eq!(w.verdict(on_chat), Verdict::Denied);
    assert!(w.config().left.is_empty());
}

#[test]
fn never_the_founder_and_never_under_rules_2() {
    let mut w = world();
    let id = w.push("ana", leave(&w, "ana"));
    assert_eq!(w.verdict(id), Verdict::Denied, "the founder stays");
    let mut old = SignedWorld::genesis(&["ana", "ben"], None);
    old.admit("ana", "ben", WorkspaceRole::Admin);
    let id = old.push("ben", leave(&old, "ben"));
    assert_eq!(old.verdict(id), Verdict::Denied, "rules 2: an Admin removes you instead");
    assert!(old.config().left.is_empty());
}

#[test]
fn a_phone_or_a_linked_device_cant_make_its_person_leave() {
    let mut w = world();
    let phone = Phone::new();
    let cert = w.phone_certificate("ben", &phone);
    w.push("ben", SessionEvent::DeviceCertificateAdded { certificate: cert });
    let l = leave(&w, "ben");
    let by_phone = w.phone_write("ben", &phone, None, l);
    assert_eq!(w.verdict(by_phone), Verdict::Denied);

    // A computer Ben's desktop enrolled (delegated), accepted by itself.
    let ben = w.people["ben"].account_id;
    let kp = SigningKeypair::generate().unwrap();
    let dev = ligus_core::crypto::device_id_for(ligus_core::crypto::SigAlg::Ed25519, &kp.public_key_bytes());
    let cert = DeviceCertificate::issue(&w.people["ben"].account, ben, &kp.public_key_bytes(), Timestamp::epoch());
    w.push("ben", SessionEvent::DeviceCertificateAdded { certificate: cert.clone() });
    let resign = |w: &mut SignedWorld, payload: SessionEvent, presence: bool| -> SessionEventEnvelope {
        let mut env = w.draft("ben", None, payload, None);
        if let Some(s) = env.actor_stamp.as_mut() {
            s.actor.device_id = Some(dev);
            s.prev = None;
        }
        if presence {
            let a = w.recovery_presence("ben", &env);
            if let Some(s) = env.actor_stamp.as_mut() {
                s.presence = Some(a);
            }
        }
        env.clear_raw();
        ligus_core::crypto::sign_envelope(&mut env, dev, &kp);
        env
    };
    let accept = resign(&mut w, SessionEvent::DeviceCertificateAdded { certificate: cert }, false);
    w.push_env(None, accept);
    assert!(w.roster().devices[&dev].delegated_for(ben));
    let l = leave(&w, "ben");
    let env = resign(&mut w, l, true);
    let id = w.push_env(None, env);
    assert_eq!(w.verdict(id), Verdict::Denied, "a linked computer can't make Ben leave");
    assert!(w.config().left.is_empty());
}

/// The founder steps down to Admin; Ben and Cy, both Owners, leave at once:
/// whichever sorts first leaves, the other is refused, in any arrival order.
#[test]
fn two_owners_leaving_at_once_leave_one() {
    let mut w = SignedWorld::genesis(&["ana", "ben", "cy"], None);
    w.admit("ana", "ben", WorkspaceRole::Owner);
    w.admit("ana", "cy", WorkspaceRole::Owner);
    w.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    let ana = id_of(&w, "ana");
    let down = w.push("ana", SessionEvent::MemberRoleChanged {
        change: MemberRoleChange { member_id: ana, old_role: WorkspaceRole::Owner, new_role: WorkspaceRole::Admin },
        watermark: Vec::new(),
    });
    assert_eq!(w.verdict(down), Verdict::Allowed);
    // Both drafted at the same head (concurrent).
    let lb = leave(&w, "ben");
    let lc = leave(&w, "cy");
    let mut eb = w.draft("ben", None, lb, None);
    let a = w.recovery_presence("ben", &eb);
    w.finish(&mut eb, "ben", Some(a));
    let mut ec = w.draft("cy", None, lc, None);
    let a = w.recovery_presence("cy", &ec);
    w.finish(&mut ec, "cy", Some(a));
    let first = if eb.canonical_key() < ec.canonical_key() { eb.event_id } else { ec.event_id };
    for order in [[eb.clone(), ec.clone()], [ec.clone(), eb.clone()]] {
        let mut v = w.clone();
        for e in order {
            v.push_env(None, e);
        }
        let (b, c) = (v.verdict(eb.event_id), v.verdict(ec.event_id));
        assert_eq!([b, c].iter().filter(|x| **x == Verdict::Allowed).count(), 1, "exactly one leaves");
        assert_eq!(v.verdict(first), Verdict::Allowed, "the first in canonical order");
        let cfg = v.config();
        let owners = cfg.members.iter().filter(|m| m.role == WorkspaceRole::Owner && !cfg.left.contains_key(&m.id)).count();
        assert_eq!(owners, 1);
    }
}

#[test]
fn the_last_owner_cant_leave() {
    let mut w = SignedWorld::genesis(&["ana", "ben"], None);
    w.admit("ana", "ben", WorkspaceRole::Owner);
    w.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    let ana = id_of(&w, "ana");
    w.push("ana", SessionEvent::MemberRoleChanged {
        change: MemberRoleChange { member_id: ana, old_role: WorkspaceRole::Owner, new_role: WorkspaceRole::Admin },
        watermark: Vec::new(),
    });
    let id = w.push("ben", leave(&w, "ben"));
    assert_eq!(w.verdict(id), Verdict::Denied);
    let refusal = ligus_core::leave::LAST_OWNER;
    assert!(refusal.starts_with("Couldn't leave: you're the last Owner"));
}

/// Of a leave and a retirement for one member, the first applies. Under
/// rules 3 a leave is display only, so a later retirement still retires
/// (authorization is unchanged by the leave) and replaces it; a leave after
/// a retirement is refused.
#[test]
fn a_leave_and_a_retirement_the_first_applies() {
    use ligus_core::retirement::MemberRetirement;
    let mut w = SignedWorld::genesis(&["ana", "ben", "dee"], None);
    w.admit("ana", "ben", WorkspaceRole::Admin);
    let m = w.member_of("dee", WorkspaceRole::Contributor);
    w.push("ana", SessionEvent::MemberAdded { member: m, invite: None, proof: None });
    w.publish_identity("dee");
    w.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    let (ben, dee) = (w.people["ben"].account_id, w.people["dee"].account_id);
    let ws = w.wid;
    // A computer of Ben's, accepted (the linked computer Dee was).
    let kp = SigningKeypair::generate().unwrap();
    let dev = ligus_core::crypto::device_id_for(ligus_core::crypto::SigAlg::Ed25519, &kp.public_key_bytes());
    let cert = DeviceCertificate::issue(&w.people["ben"].account, ben, &kp.public_key_bytes(), Timestamp::epoch());
    w.push("ben", SessionEvent::DeviceCertificateAdded { certificate: cert.clone() });
    let mut env = w.draft("ben", None, SessionEvent::DeviceCertificateAdded { certificate: cert }, None);
    if let Some(s) = env.actor_stamp.as_mut() {
        s.actor.device_id = Some(dev);
        s.prev = None;
    }
    env.clear_raw();
    ligus_core::crypto::sign_envelope(&mut env, dev, &kp);
    w.push_env(None, env);
    let retirement = MemberRetirement::sign(&w.people["dee"].account, ws, "dee", dee, ben, dev);
    let dee_id = id_of(&w, "dee");
    assert_eq!(dee_id, "dee");

    // Retired first: the leave after it is refused.
    let mut a = w.clone();
    let r = a.push_bare("dee", SessionEvent::MemberRetired { retirement: retirement.clone() });
    let l = a.push_bare("dee", leave(&a, "dee"));
    assert_eq!(a.verdict(r), Verdict::Allowed);
    assert_eq!(a.verdict(l), Verdict::Denied);
    assert!(a.config().left.is_empty());

    // Left first: shown as asked to leave, and the retirement still applies.
    let mut b = w.clone();
    let l = b.push_bare("dee", leave(&b, "dee"));
    assert_eq!(b.verdict(l), Verdict::Allowed);
    assert!(b.config().left.contains_key("dee"));
    let r = b.push_bare("dee", SessionEvent::MemberRetired { retirement });
    assert_eq!(b.verdict(r), Verdict::Allowed, "rules 3: a leave changes no other verdict");
    let c = b.config();
    assert!(!c.members.iter().any(|m| m.id == "dee") && !c.left.contains_key("dee"));
}

fn is_leave(e: &SessionEventEnvelope) -> bool {
    matches!(e.payload, SessionEvent::MemberLeft { .. })
}

/// Rules 3: the same log with and without its leaves (valid, refused and
/// malformed ones, interleaved with ordinary traffic and governance) gives
/// the same verdict for every other event, the same roster and the same
/// rotation set.
#[test]
fn under_rules_3_authorization_is_identical_with_and_without_leaves() {
    let mut w = world();
    let chat = w.chat("ana", "plans");
    w.say(chat, "cy", SessionEvent::SessionTitleChanged { title: "a".into() });
    let cy_left = w.push("cy", leave(&w, "cy")); // valid
    w.push_bare("box", leave(&w, "box")); // valid
    w.push("ana", leave(&w, "ana")); // the founder: refused
    let forged = leave(&w, "cy");
    w.push("ben", forged); // someone else's: refused
    w.push("cy", leave(&w, "cy")); // twice: refused
    w.say(chat, "cy", SessionEvent::SessionTitleChanged { title: "b".into() });
    w.say(chat, "box", SessionEvent::SessionTitleChanged { title: "c".into() });
    let ben = id_of(&w, "ben");
    w.push("ana", SessionEvent::MemberRoleChanged {
        change: MemberRoleChange { member_id: ben, old_role: WorkspaceRole::Admin, new_role: WorkspaceRole::Owner },
        watermark: Vec::new(),
    });
    let ben_left = w.push("ben", leave(&w, "ben")); // valid
    w.push("ben", SessionEvent::SessionTitleChanged { title: "Team".into() });
    w.say(chat, "ben", SessionEvent::SessionTitleChanged { title: "d".into() });

    // Without them: what a build that doesn't know the kind holds, the same
    // signed bytes folded as `Unknown` (dropping the envelopes instead would
    // break their authors' chains, which no build does).
    let mut without = w.clone();
    for e in without.cfg.iter_mut().filter(|e| is_leave(e)) {
        e.payload = SessionEvent::Unknown;
    }
    assert!(w.cfg.iter().filter(|e| is_leave(e)).count() >= 6);
    let (vw, vo) = (w.verdicts(), without.verdicts());
    let mut compared = 0;
    for (id, v) in &vo {
        let e = w.cfg.iter().chain(w.chats.values().flatten()).find(|e| e.event_id == *id).unwrap();
        if is_leave(e) {
            continue;
        }
        assert_eq!(vw.get(id), Some(v), "{} changed with the leaves in", e.payload.kind_str());
        compared += 1;
    }
    assert!(compared > 20);
    let (rw, ro) = (w.roster(), without.roster());
    assert_eq!(rw, ro, "the roster changed");
    assert_eq!(rw.rotation_recipients(), ro.rotation_recipients(), "the rotation set changed");
    let (cw, co) = (w.config(), without.config());
    let ids = |c: &ligus_core::ChatSession| c.members.iter().map(|m| (m.id.clone(), m.role)).collect::<Vec<_>>();
    assert_eq!(ids(&cw), ids(&co));
    assert_eq!(w.verdict(cy_left), Verdict::Allowed);
    assert_eq!(w.verdict(ben_left), Verdict::Allowed);
    assert_eq!(cw.left.len(), 3, "Cy, the box and Ben asked to leave: {:?}", cw.left.keys().collect::<Vec<_>>());
    assert!(co.left.is_empty());
    assert_eq!(w.state(chat).title, without.state(chat).title);
}

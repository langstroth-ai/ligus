//! Adversarial probes of `MemberLeft` (workspace model v3, 10.4): forged
//! authorship for an accountless member, runtime-effect and agent stamps,
//! chat logs, and a removed-then-re-added member.

use ligus_core::events::Verdict;
use ligus_core::identity::{ActorIdentity, WorkspaceMember};
use ligus_core::testkit::{Proof, SignedWorld};
use ligus_core::{ActorKind, SessionEvent, SessionEventEnvelope, Timestamp, WorkspaceRole};

fn leave(w: &SignedWorld, who: &str) -> SessionEvent {
    SessionEvent::MemberLeft { member_id: w.people[who].actor().id, account_id: Some(w.people[who].account_id), watermark: Vec::new() }
}

/// Ana (founder), Ben (Admin), Cy (Contributor), on rules 3.
fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["ana", "ben", "cy"], None);
    w.admit("ana", "ben", WorkspaceRole::Admin);
    w.admit("ana", "cy", WorkspaceRole::Contributor);
    let id = w.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    assert_eq!(w.verdict(id), Verdict::Allowed);
    w
}

/// `by`'s device signs an envelope whose stamp names `actor` instead of `by`.
fn signed_as(w: &mut SignedWorld, by: &str, actor: ActorIdentity, payload: SessionEvent) -> SessionEventEnvelope {
    let mut env = w.draft(by, None, payload, None);
    if let Some(s) = env.actor_stamp.as_mut() {
        let dev = s.actor.device_id;
        s.actor = actor;
        s.actor.device_id = dev;
    }
    env.clear_raw();
    let p = &w.people[by];
    ligus_core::crypto::sign_envelope(&mut env, p.device_id, &p.device);
    env
}

fn accountless(id: &str) -> WorkspaceMember {
    WorkspaceMember { id: id.into(), actor: ActorIdentity::new(id, "Dana", ActorKind::Human), role: WorkspaceRole::Contributor, title: String::new(), index: 0, joined_at: Timestamp::epoch() }
}

#[test]
fn another_members_device_cant_make_an_accountless_member_leave() {
    let mut w = world();
    let add = w.push("ana", SessionEvent::MemberAdded { member: accountless("dana"), invite: None, proof: None });
    assert_eq!(w.verdict(add), Verdict::Allowed, "precondition: an accountless member is admitted under rules 3");
    let dana = accountless("dana").actor;
    let forged = signed_as(&mut w, "cy", dana, SessionEvent::MemberLeft { member_id: "dana".into(), account_id: None, watermark: Vec::new() });
    let id = w.push_env(None, forged);
    assert_eq!(w.verdict(id), Verdict::Denied, "Cy's device wrote Dana's leave");
    assert!(!w.config().left.contains_key("dana"));
}

#[test]
fn a_removed_persons_device_cant_make_an_accountless_member_leave() {
    let mut w = world();
    w.push("ana", SessionEvent::MemberAdded { member: accountless("dana"), invite: None, proof: None });
    w.push("ben", SessionEvent::MemberRemoved { member_id: "cy".into(), watermark: Vec::new() });
    let dana = accountless("dana").actor;
    let forged = signed_as(&mut w, "cy", dana, SessionEvent::MemberLeft { member_id: "dana".into(), account_id: None, watermark: Vec::new() });
    let id = w.push_env(None, forged);
    assert_eq!(w.verdict(id), Verdict::Denied, "a non-member's device wrote Dana's leave");
}

#[test]
fn runtime_effect_agent_stamp_and_chat_log_leaves_are_refused() {
    let mut w = world();
    let mut env = w.draft("cy", None, leave(&w, "cy"), None);
    let a = w.recovery_presence("cy", &env);
    if let Some(s) = env.actor_stamp.as_mut() {
        s.runtime_effect = true;
    }
    w.finish(&mut env, "cy", Some(a));
    let id = w.push_env(None, env);
    assert_eq!(w.verdict(id), Verdict::Denied);
    let mut agent = w.people["cy"].actor();
    agent.kind = ActorKind::Agent;
    let l = leave(&w, "cy");
    let env = signed_as(&mut w, "cy", agent, l);
    let id = w.push_env(None, env);
    assert_eq!(w.verdict(id), Verdict::Denied);
    let chat = w.chat("ana", "plans");
    let l = leave(&w, "cy");
    w.write("cy", Some(chat), l, None, Proof::Recovery);
    assert!(w.state(chat).left.is_empty());
    assert!(ligus_core::events::project(&w.chats[&chat]).unwrap().left.is_empty(), "unchecked chat fold");
    assert!(w.config().left.is_empty());
}

#[test]
fn a_removed_then_readded_member_is_not_shown_as_left() {
    use ligus_core::testkit::Person;
    let cy_acct = uuid::Uuid::new_v4();
    let people = vec![Person::new("ana"), Person::new("ben"), Person::new(&cy_acct.to_string()).with_account_id(cy_acct)];
    let cy = cy_acct.to_string();
    let mut w = SignedWorld::genesis_of(people, None);
    w.admit("ana", "ben", WorkspaceRole::Admin);
    w.admit("ana", &cy, WorkspaceRole::Contributor);
    w.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    let l = w.push(&cy, leave(&w, &cy));
    assert_eq!(w.verdict(l), Verdict::Allowed);
    w.push("ben", SessionEvent::MemberRemoved { member_id: cy.clone(), watermark: Vec::new() });
    let after = w.push(&cy, leave(&w, &cy));
    assert_eq!(w.verdict(after), Verdict::Denied, "not a member");
    let m = w.member_of(&cy, WorkspaceRole::Contributor);
    let readd = w.push("ben", SessionEvent::MemberAdded { member: m, invite: None, proof: None });
    assert_eq!(w.verdict(readd), Verdict::Allowed);
    assert!(w.config().left.is_empty());
    let again = w.push(&cy, leave(&w, &cy));
    assert_eq!(w.verdict(again), Verdict::Allowed, "a new stay, a new leave");
}

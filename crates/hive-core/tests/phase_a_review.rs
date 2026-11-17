//! Adversarial review of the Phase A protocol before the 2.0 wire freeze:
//! each test is a counterexample to a Phase A claim, written to fail on the
//! branch as reviewed and pass once the fix that follows it lands.

#![allow(dead_code, unused_imports)]

use hive_core::crypto::DeviceCertificate;
use hive_core::flow::{FlowRecord, HumanAnswered};
use hive_core::presence::PresencePolicy;
use hive_core::testkit::{Phone, SignedWorld};
use hive_core::watermark::heads_of;
use hive_core::{workspace_config_session_id, ActorKind, ChatMessage, MessageRole, SessionEvent, Timestamp, Verdict, WorkspaceRole};
use std::collections::BTreeSet;
use uuid::Uuid;

/// A config event by `by` at an exact lamport (a backdated write), with
/// their recovery-key presence.
fn cfg_at(w: &mut SignedWorld, by: &str, payload: SessionEvent, lamport: u64) -> Uuid {
    let mut env = w.draft(by, None, payload, None);
    env.lamport = lamport;
    env.clear_raw();
    let a = w.recovery_presence(by, &env);
    w.finish(&mut env, by, Some(a));
    w.push_env(None, env)
}

/// The honest watermark of `who`'s devices on every log of `w`.
fn honest_watermark(w: &SignedWorld, who: &str) -> Vec<hive_core::watermark::Watermark> {
    let signers: BTreeSet<Uuid> = [w.people[who].device_id].into_iter().collect();
    let mut out = heads_of(&w.cfg, workspace_config_session_id(w.wid), &signers);
    for (id, log) in &w.chats {
        out.extend(heads_of(log, *id, &signers));
    }
    out
}

fn lamport_of(w: &SignedWorld, id: Uuid) -> u64 {
    w.cfg.iter().find(|e| e.event_id == id).unwrap().lamport
}

/// Claim 8: a service member never satisfies a human gate. Answering an
/// agent's question is one; the check reads only the stamp's self-declared
/// kind, which the service's device signs itself.
#[test]
fn a_service_member_cannot_answer_as_a_human_by_stamping_itself_human() {
    let mut w = SignedWorld::genesis(&["owner", "box"], None);
    w.kinds.insert("box".into(), ActorKind::Service);
    w.admit("owner", "box", WorkspaceRole::Contributor);
    let chat = w.chat("owner", "c");
    let answer = SessionEvent::FlowRecorded {
        record: FlowRecord::HumanAnswered(HumanAnswered { ask_id: Uuid::new_v4(), answer: "yes".into(), option_index: None, by: "box".into(), at_ms: 0 }),
    };
    // Honestly stamped (Service): refused already.
    let honest = w.say(chat, "box", answer.clone());
    // Stamped Human by the service's own device.
    let mut env = w.draft("box", Some(chat), answer, None);
    env.actor_stamp.as_mut().unwrap().actor.kind = ActorKind::Human;
    w.finish(&mut env, "box", None);
    let lying = w.push_env(Some(chat), env);
    let v = w.verdicts();
    assert_eq!(v[&honest], Verdict::Denied);
    assert_eq!(v[&lying], Verdict::Denied, "the member record (Service), not the stamp, decides who is human");
}

/// Claim 5: the relying party decides which page every member's desktop
/// opens for each passkey ceremony (`PresencePolicy::page_url`), and what a
/// re-registration under it binds. Only the Owner may change it.
#[test]
fn only_the_owner_changes_the_relying_party() {
    let mut w = SignedWorld::genesis(&["owner", "ada"], None);
    w.admit("owner", "ada", WorkspaceRole::Admin);
    let evil = PresencePolicy::for_rp("evil.example");
    assert_eq!(evil.page_url(), "https://evil.example/presence");
    let by_admin = w.push("ada", SessionEvent::WorkspaceRulesSet { version: 2, presence: Some(evil.clone()) });
    assert_eq!(w.verdict(by_admin), Verdict::Denied, "an Admin can't point every member's passkey page elsewhere");
    assert_eq!(w.roster().presence_policy, PresencePolicy::ligus());
    let by_owner = w.push("owner", SessionEvent::WorkspaceRulesSet { version: 2, presence: Some(PresencePolicy::for_rp("example.org")) });
    assert_eq!(w.verdict(by_owner), Verdict::Allowed);
}

/// Claim 1: the genesis `WorkspaceRulesSet` is the only presence-free rules
/// set. Before anyone holds a credential (a CLI-founded workspace), a second
/// presence-free one must not re-point the relying party.
#[test]
fn there_is_one_presence_free_genesis_rules_set() {
    let mut w = SignedWorld::genesis(&["owner"], None);
    // Keep the seed and the founder's identity only: no credential yet.
    w.cfg.truncate(3);
    let genesis = w.push_bare("owner", SessionEvent::WorkspaceRulesSet { version: 2, presence: Some(PresencePolicy::ligus()) });
    let second = w.push_bare("owner", SessionEvent::WorkspaceRulesSet { version: 2, presence: Some(PresencePolicy::for_rp("example.org")) });
    let v = w.verdicts();
    assert_eq!(v[&genesis], Verdict::Allowed);
    assert_eq!(v[&second], Verdict::Denied, "a second genesis");
    assert_eq!(w.roster().presence_policy, PresencePolicy::ligus());
}

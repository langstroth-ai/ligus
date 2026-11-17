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

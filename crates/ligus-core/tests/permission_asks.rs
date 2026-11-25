//! Asks fold with who signed them (`flow::asks`): a permission prompt counts
//! only from the asking turn's host, a close only from the ask's own signer
//! (or its host), and a permission answer only from the host's person or the
//! agent's owner. Nothing here is refused: every record is kept, and one
//! that doesn't qualify folds as inert, so every version accepts the same
//! events.

use ligus_core::flow::{FlowRecord, HumanAnswered, HumanAsk, PRIMARY};
use ligus_core::testkit::SignedWorld;
use ligus_core::{ChatMessage, MessageRole, SessionEvent, Verdict, WorkspaceRole};
use uuid::Uuid;

/// Ana (Owner) hosts a turn of the chat's runtime on her device; Bo is a
/// Contributor on his own device.
struct Room {
    w: SignedWorld,
    chat: Uuid,
    turn: Uuid,
}

fn room() -> Room {
    let mut w = SignedWorld::genesis(&["ana", "bo"], None);
    w.admit("ana", "bo", WorkspaceRole::Contributor);
    let chat = w.chat("ana", "Fixes");
    let mut ask = ChatMessage::new(MessageRole::User, "bo", "please clean the build");
    ask.actor_identity = Some(w.people["bo"].actor());
    let trigger = ask.id;
    w.say(chat, "bo", SessionEvent::MessageAppended { message: ask });
    // Ana's device runs the turn: its placeholder commits it there.
    let mut reply = ChatMessage::new(MessageRole::Assistant, "Ligus", "");
    reply.reply_to_message_id = Some(trigger);
    reply.origin_device_id = Some(w.people["ana"].device_id);
    reply.is_streaming = true;
    let turn = reply.id;
    let id = w.say(
        chat,
        "ana",
        SessionEvent::MessageAppended { message: reply },
    );
    assert_eq!(w.verdict(id), Verdict::Allowed);
    Room { w, chat, turn }
}

fn ask(turn: Uuid, permission: bool) -> HumanAsk {
    HumanAsk {
        ask_id: Uuid::new_v4(),
        turn_id: turn,
        agent_key: PRIMARY.into(),
        question: "Ligus asks permission to run rm -rf build (execute). Allow it once?".into(),
        options: vec!["Allow once".into(), "Deny".into()],
        at_ms: 1,
        permission,
        ..Default::default()
    }
}

fn record(r: &mut Room, by: &str, rec: FlowRecord) -> Uuid {
    let chat = r.chat;
    let id =
        r.w.say(chat, by, SessionEvent::FlowRecorded { record: rec });
    assert_eq!(
        r.w.verdict(id),
        Verdict::Allowed,
        "every record is accepted, as before"
    );
    id
}

fn answer(ask_id: Uuid, by: &str) -> FlowRecord {
    FlowRecord::HumanAnswered(HumanAnswered {
        ask_id,
        answer: "Allow once".into(),
        option_index: Some(0),
        by: by.into(),
        at_ms: 2,
    })
}

#[test]
fn the_turns_host_posts_a_permission_prompt_and_a_contributors_forgery_is_a_plain_ask() {
    let mut r = room();
    let real = ask(r.turn, true);
    record(&mut r, "ana", FlowRecord::HumanAsk(real.clone()));
    // Bo signs the same shape: permission on Ana's turn, under the runtime's name.
    let forged = ask(r.turn, true);
    record(&mut r, "bo", FlowRecord::HumanAsk(forged.clone()));
    // And on a turn nobody runs.
    let mut stray = ask(Uuid::new_v4(), true);
    stray.agent_key = Uuid::new_v4().to_string();
    record(&mut r, "bo", FlowRecord::HumanAsk(stray.clone()));
    let s = r.w.state(r.chat);
    let a = &s.flow.asks[&real.ask_id];
    assert!(
        a.ask.permission,
        "the host's own prompt is a permission prompt"
    );
    assert!(a.posted_by.is_none());
    for id in [forged.ask_id, stray.ask_id] {
        let f = &s.flow.asks[&id];
        assert!(
            !f.ask.permission,
            "a forged permission prompt folds as a question"
        );
        assert_eq!(
            f.posted_by.as_ref().map(|p| p.id.as_str()),
            Some("bo"),
            "and is attributed to its signer"
        );
        assert!(f.is_open());
    }
    // Ana's device asking on a turn it doesn't run is no prompt either.
    let mut elsewhere = ask(Uuid::new_v4(), true);
    elsewhere.ask_id = Uuid::new_v4();
    record(&mut r, "ana", FlowRecord::HumanAsk(elsewhere.clone()));
    assert!(
        !r.w.state(r.chat).flow.asks[&elsewhere.ask_id]
            .ask
            .permission
    );
}

#[test]
fn only_the_hosts_person_or_the_agents_owner_grants_a_permission_prompt() {
    let mut r = room();
    let real = ask(r.turn, true);
    record(&mut r, "ana", FlowRecord::HumanAsk(real.clone()));
    // Bo answers first: kept, counts for nothing.
    record(&mut r, "bo", answer(real.ask_id, "Bo"));
    let s = r.w.state(r.chat);
    assert!(s.flow.asks[&real.ask_id].answer.is_none());
    assert!(
        s.flow.asks[&real.ask_id].is_open(),
        "still waiting for someone who may grant it"
    );
    record(&mut r, "ana", answer(real.ask_id, "Ana"));
    let s = r.w.state(r.chat);
    assert_eq!(
        s.flow.asks[&real.ask_id]
            .answer
            .as_ref()
            .map(|a| a.by.as_str()),
        Some("Ana")
    );

    // A question (or a forged prompt, which is one) takes anyone's answer.
    let forged = ask(r.turn, true);
    record(&mut r, "bo", FlowRecord::HumanAsk(forged.clone()));
    record(&mut r, "ana", answer(forged.ask_id, "Ana"));
    assert_eq!(
        r.w.state(r.chat).flow.asks[&forged.ask_id]
            .answer
            .as_ref()
            .map(|a| a.by.as_str()),
        Some("Ana")
    );
}

#[test]
fn only_the_asks_signer_or_its_host_closes_it() {
    let mut r = room();
    let real = ask(r.turn, true);
    record(&mut r, "ana", FlowRecord::HumanAsk(real.clone()));
    // Bo closes it, naming the asking turn exactly: ignored.
    record(
        &mut r,
        "bo",
        FlowRecord::HumanAsk(HumanAsk {
            closed: true,
            ..real.clone()
        }),
    );
    let s = r.w.state(r.chat);
    let a = &s.flow.asks[&real.ask_id];
    assert!(a.is_open(), "someone else's close has no effect");
    assert!(!a.ask.closed);
    // The asking host closes it.
    record(
        &mut r,
        "ana",
        FlowRecord::HumanAsk(HumanAsk {
            closed: true,
            ..real.clone()
        }),
    );
    let s = r.w.state(r.chat);
    assert!(!s.flow.asks[&real.ask_id].is_open());
    // A closed ask takes no answer.
    record(&mut r, "ana", answer(real.ask_id, "Ana"));
    assert!(r.w.state(r.chat).flow.asks[&real.ask_id].answer.is_none());

    // Bo's own question: Bo closes it, Ana can't.
    let mut q = ask(r.turn, false);
    q.question = "which build dir?".into();
    record(&mut r, "bo", FlowRecord::HumanAsk(q.clone()));
    record(
        &mut r,
        "ana",
        FlowRecord::HumanAsk(HumanAsk {
            closed: true,
            ..q.clone()
        }),
    );
    assert!(r.w.state(r.chat).flow.asks[&q.ask_id].is_open());
    record(
        &mut r,
        "bo",
        FlowRecord::HumanAsk(HumanAsk {
            closed: true,
            ..q.clone()
        }),
    );
    assert!(!r.w.state(r.chat).flow.asks[&q.ask_id].is_open());
}

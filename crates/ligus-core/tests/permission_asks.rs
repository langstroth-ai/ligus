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

/// A chat's seed snapshot is the founder's word: asks it carries are never
/// permission prompts, and eviction reads only what older builds read from
/// it (so both keep the same asks).
#[test]
fn a_seed_snapshot_carries_no_permission_prompt_and_no_eviction_hint() {
    let mut w = SignedWorld::genesis(&["ana", "bo"], None);
    w.admit("ana", "bo", WorkspaceRole::Contributor);
    let mut s = ligus_core::ChatSession::new("Seeded", w.wid, "anthropic");
    s.creator_actor_id = "bo".into();
    s.members = vec![w.people["bo"].member(WorkspaceRole::Owner)];
    let forged = ask(Uuid::new_v4(), true);
    s.flow.asks.insert(
        forged.ask_id,
        ligus_core::flow::AskState {
            ask: forged.clone(),
            signer: Some(w.people["ana"].device_id),
            host: Some(w.people["ana"].device_id),
            legacy_settled: true,
            ..Default::default()
        },
    );
    let chat = s.id;
    w.write("bo", Some(chat), SessionEvent::SessionSnapshot { session: Box::new(s) }, None, ligus_core::testkit::Proof::None);
    let st = &w.state(chat).flow.asks[&forged.ask_id];
    assert!(!st.ask.permission, "a seeded ask is never a permission prompt");
    assert!(st.signer.is_none() && st.host.is_none());
    assert!(!st.legacy_settled, "eviction reads the snapshot's answer and closed flags only");
    assert!(st.is_open());
}

/// A grant counts only when the answer's signing device is certified for
/// the person its stamp names.
#[test]
fn a_grant_signed_by_another_device_under_the_hosts_name_counts_for_nothing() {
    let mut r = room();
    let real = ask(r.turn, true);
    record(&mut r, "ana", FlowRecord::HumanAsk(real.clone()));
    let chat = r.chat;
    let mut env = r.w.draft("bo", Some(chat), SessionEvent::FlowRecorded { record: answer(real.ask_id, "Ana") }, None);
    if let Some(st) = env.actor_stamp.as_mut() {
        st.actor = ligus_core::ActorIdentity::new("ana", "ana", ligus_core::ActorKind::Human);
    }
    r.w.finish(&mut env, "bo", None);
    r.w.push_env(Some(chat), env);
    let s = r.w.state(chat);
    assert!(s.flow.asks[&real.ask_id].answer.is_none(), "Bo's device can't grant as Ana");
}

/// Bo certifies Ana's device key for his own account (allowed: an adopter
/// holds nothing). That doesn't make him the host's person: his answer
/// counts for nothing, and Ana's still does.
#[test]
fn adopting_the_hosts_device_key_doesnt_make_you_its_person() {
    let mut r = room();
    let real = ask(r.turn, true);
    record(&mut r, "ana", FlowRecord::HumanAsk(real.clone()));
    let (bo, ana) = (&r.w.people["bo"], &r.w.people["ana"]);
    let cert = ligus_core::crypto::DeviceCertificate::issue(&bo.account, bo.account_id, &ana.device.public_key_bytes(), ligus_core::Timestamp::epoch());
    let adopt = r.w.push("bo", SessionEvent::DeviceCertificateAdded { certificate: cert });
    assert_eq!(r.w.verdict(adopt), Verdict::Allowed, "the probe's premise: adopting a key is allowed");
    let ana_device = r.w.people["ana"].device_id;
    assert!(r.w.roster().devices[&ana_device].accounts.contains(&r.w.people["bo"].account_id));
    record(&mut r, "bo", answer(real.ask_id, "Bo"));
    let s = r.w.state(r.chat);
    assert!(s.flow.asks[&real.ask_id].answer.is_none(), "an adopter's answer isn't a grant");
    record(&mut r, "ana", answer(real.ask_id, "Ana"));
    assert_eq!(r.w.state(r.chat).flow.asks[&real.ask_id].answer.as_ref().map(|a| a.by.as_str()), Some("Ana"));
}

/// An ask id belongs to its first signer. Bo backdates a forged ask under
/// the id the host uses (stamped as if he hadn't seen the host's record):
/// his folds first and holds the id as his own plain question; the host's
/// later record under that id changes nothing, and Bo's ask is never a
/// permission prompt. (The host never takes an answer to it as one to its
/// own: it waits only on the ask it posted.)
#[test]
fn a_backdated_ask_under_the_hosts_id_stays_its_signers_plain_question() {
    let mut r = room();
    let real = ask(r.turn, true);
    let chat = r.chat;
    let before = r.w.chats[&chat].clone();
    record(&mut r, "ana", FlowRecord::HumanAsk(real.clone()));
    let mut forged = real.clone();
    forged.question = "Ligus asks: deny the cleanup?".into();
    forged.options = vec!["Yes, deny".into(), "No".into()];
    let mut env = r.w.draft("bo", Some(chat), SessionEvent::FlowRecorded { record: FlowRecord::HumanAsk(forged.clone()) }, None);
    // Stamped as if he had seen only the log up to the request: it sorts
    // before the host's ask.
    ligus_core::lamport::stamp_after(&mut env, &before[..before.len() - 1]);
    r.w.finish(&mut env, "bo", None);
    let id = r.w.push_env(Some(chat), env);
    assert_eq!(r.w.verdict(id), Verdict::Allowed);
    let s = r.w.state(chat);
    let st = &s.flow.asks[&real.ask_id];
    assert_eq!(st.ask.question, forged.question, "the backdated record folds first and holds the id");
    assert!(!st.ask.permission, "the pre-empting ask is no permission prompt");
    assert_eq!(st.posted_by.as_ref().map(|p| p.id.as_str()), Some("bo"));
    assert_eq!(st.ask.options, forged.options, "the host's later record under the id changes nothing");
}

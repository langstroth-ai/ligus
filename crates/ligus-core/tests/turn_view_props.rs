//! The `Turn` view is a pure function of the chat's fold: for any subset of
//! a turn's events (claim, placeholder, question, its close, completion,
//! removal of the reply), every arrival order folds to the same `Turn`, and
//! its state is the one those events describe.

use ligus_core::flow::{FlowRecord, HumanAsk};
use ligus_core::ids::TurnId;
use ligus_core::turn::TurnState;
use ligus_core::{ChatMessage, ChatSession, MessageRole, SessionEvent, SessionEventEnvelope};
use proptest::prelude::*;
use uuid::Uuid;

const DEVICE: Uuid = Uuid::from_u128(0xd1);

#[derive(Debug, Clone, Copy)]
struct Present {
    claim: bool,
    reply: bool,
    ask: bool,
    close_ask: bool,
    complete: bool,
    remove: bool,
}

fn present() -> impl Strategy<Value = Present> {
    (any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>())
        .prop_map(|(claim, reply, ask, close_ask, complete, remove)| Present { claim, reply, ask, close_ask, complete, remove })
}

struct Fixture {
    trigger: Uuid,
    reply: Uuid,
    events: Vec<SessionEventEnvelope>,
}

fn env(base: &ChatSession, lamport: u64, signer: Option<Uuid>, payload: SessionEvent) -> SessionEventEnvelope {
    let mut e = SessionEventEnvelope::new(base.id, base.workspace_id, lamport as i64, payload);
    e.lamport = lamport;
    e.signer_device_id = signer;
    e
}

fn fixture(p: Present) -> Fixture {
    let base = ChatSession::new("c", Uuid::from_u128(0xa), "x");
    let trigger_msg = ChatMessage::new(MessageRole::User, "mara", "@ligus go");
    let trigger = trigger_msg.id;
    let mut placeholder = ChatMessage::new(MessageRole::Assistant, "ligus", "");
    placeholder.is_streaming = true;
    placeholder.reply_to_message_id = Some(trigger);
    placeholder.origin_device_id = Some(DEVICE);
    let reply = placeholder.id;
    let ask = HumanAsk { ask_id: Uuid::from_u128(0xa5), turn_id: reply, agent_key: "primary".into(), question: "which?".into(), ..Default::default() };
    let mut events = vec![
        env(&base, 1, None, SessionEvent::SessionSnapshot { session: Box::new(base.clone()) }),
        env(&base, 2, None, SessionEvent::MessageAppended { message: trigger_msg }),
    ];
    if p.claim {
        events.push(env(&base, 3, Some(DEVICE), SessionEvent::TurnClaimed { trigger_id: trigger, device_id: DEVICE, epoch: 0, supersedes: None, repo_id: None }));
    }
    if p.reply {
        events.push(env(&base, 4, Some(DEVICE), SessionEvent::MessageAppended { message: placeholder }));
    }
    if p.ask {
        events.push(env(&base, 5, Some(DEVICE), SessionEvent::FlowRecorded { record: FlowRecord::HumanAsk(ask.clone()) }));
        if p.close_ask {
            let closed = HumanAsk { closed: true, ..ask };
            events.push(env(&base, 6, Some(DEVICE), SessionEvent::FlowRecorded { record: FlowRecord::HumanAsk(closed) }));
        }
    }
    if p.complete {
        events.push(env(&base, 7, Some(DEVICE), SessionEvent::MessageCompleted { message_id: reply, body: "done".into() }));
    }
    if p.remove {
        events.push(env(&base, 8, None, SessionEvent::MessageRemoved { message_id: reply }));
    }
    Fixture { trigger, reply, events }
}

fn fold(events: &[SessionEventEnvelope]) -> ChatSession {
    ligus_core::project(&ligus_core::testkit::honest_chain(events)).expect("a chat")
}

/// The state the present events describe.
fn expected(p: Present) -> TurnState {
    let removed = p.reply && p.remove;
    if p.reply && !removed {
        if p.complete {
            TurnState::Posted
        } else if p.ask && !p.close_ask {
            TurnState::WaitingOnAsk
        } else {
            TurnState::Running
        }
    } else if p.claim && !removed {
        TurnState::Claimed
    } else {
        TurnState::Queued
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]
    #[test]
    fn a_turn_is_the_same_in_every_arrival_order(p in present(), seed in any::<u64>()) {
        let f = fixture(p);
        let canonical = fold(&f.events).turn(f.trigger, None).expect("the trigger is in the chat");
        // A deterministic shuffle of the arrival order from `seed`.
        let mut shuffled = f.events.clone();
        let mut s = seed | 1;
        for i in (1..shuffled.len()).rev() {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            shuffled.swap(i, (s % (i as u64 + 1)) as usize);
        }
        let again = fold(&shuffled).turn(f.trigger, None).expect("the trigger is in the chat");
        prop_assert_eq!(&canonical, &again);

        prop_assert_eq!(canonical.state, expected(p), "{:?}", p);
        prop_assert_eq!(canonical.key, f.trigger, "a one-responder turn is claimed under its trigger");
        let committed = p.reply && !p.remove;
        prop_assert_eq!(canonical.id, committed.then_some(TurnId(f.reply)));
        prop_assert_eq!(canonical.claim.map(|c| c.0), (p.claim && !(p.reply && p.remove)).then_some(DEVICE));
        prop_assert_eq!(canonical.asks.len(), usize::from(committed && p.ask));
    }
}

#[test]
fn a_removed_trigger_has_no_turn() {
    let p = Present { claim: true, reply: true, ask: false, close_ask: false, complete: true, remove: false };
    let mut f = fixture(p);
    let base = fold(&f.events[..1]);
    f.events.push(env(&base, 9, None, SessionEvent::MessageRemoved { message_id: f.trigger }));
    assert!(fold(&f.events).turn(f.trigger, None).is_none());
}

//! The `Turn` view: one turn of a chat, read from the chat's fold.
//!
//! A turn was a trigger id, claims, dispatch slots, flow records and
//! in-memory waiters, each read on its own. This names it. Nothing new is
//! written: the view is a pure function of the folded chat
//! ([`ChatSession`]), so every device that holds the same events sees the
//! same turn in the same state.
//!
//! Ids: `trigger` is the message that started the turn, `key` the claim key
//! its claims and dispatch slot are kept under ([`turn_claim_key`]: the
//! trigger itself for a turn with one responder), and `id` the turn's
//! placeholder reply ([`TurnId`]), once one is committed.

use uuid::Uuid;

use crate::ids::TurnId;
use crate::session::{turn_claim_key, ChatSession};

/// Where a turn stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnState {
    /// No device holds it and no answer is committed.
    Queued,
    /// A device holds it and hasn't committed its placeholder yet.
    Claimed,
    /// Its committed reply is streaming.
    Running,
    /// Its committed reply is streaming and one of its questions is open.
    WaitingOnAsk,
    /// Its committed reply is complete.
    Posted,
}

/// One turn of a chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    /// The claim key ([`turn_claim_key`]).
    pub key: Uuid,
    /// The message that started the turn.
    pub trigger: Uuid,
    /// The receiving agent of a hand-off (`None`: the trigger's one turn).
    pub agent: Option<Uuid>,
    /// The committed placeholder reply, once there is one.
    pub id: Option<TurnId>,
    /// The device holding the turn and its claim generation.
    pub claim: Option<(Uuid, u32)>,
    pub state: TurnState,
    /// The questions the turn asked (open or not), by ask id.
    pub asks: Vec<Uuid>,
}

impl Turn {
    /// The turn `trigger` started for `agent` (`None`: its plain turn), as
    /// `session` folds it. `None` when the trigger isn't in the chat (never
    /// arrived, or removed). Whether a message starts a turn at all
    /// (mentions, routing) is dispatch's call; this reads where one stands.
    pub fn of(session: &ChatSession, trigger: Uuid, agent: Option<Uuid>) -> Option<Turn> {
        session.message_index(trigger)?;
        let plain = turn_claim_key(trigger, None);
        let mut key = turn_claim_key(trigger, agent);
        // A receiver's answer commits under the plain trigger when that is
        // where its turn was claimed (`dispatch::note_answer_at`).
        if agent.is_some() && !session.turn_dispatch.contains_key(&key) {
            let answered_by_agent = session
                .turn_dispatch
                .get(&plain)
                .and_then(|s| s.answer)
                .and_then(|a| session.messages.iter().find(|m| m.id == a))
                .is_some_and(|m| m.responder_id == agent);
            if answered_by_agent {
                key = plain;
            }
        }
        let slot = session.turn_dispatch.get(&key);
        let claim = session.turn_claims.get(&key).map(|d| (*d, slot.map(|s| s.epoch).unwrap_or(0)));
        let reply = slot.and_then(|s| s.answer).and_then(|a| session.messages.iter().find(|m| m.id == a));
        let id = reply.map(|m| TurnId(m.id));
        let asks: Vec<Uuid> = match id {
            Some(t) => session.flow.asks.values().filter(|a| a.ask.turn_id == t.0).map(|a| a.ask.ask_id).collect(),
            None => Vec::new(),
        };
        let state = match reply {
            Some(m) if m.is_streaming => {
                let waiting = asks.iter().any(|a| session.flow.asks.get(a).is_some_and(|s| s.is_open()));
                if waiting {
                    TurnState::WaitingOnAsk
                } else {
                    TurnState::Running
                }
            }
            Some(_) => TurnState::Posted,
            None if claim.is_some() => TurnState::Claimed,
            None => TurnState::Queued,
        };
        Some(Turn { key, trigger, agent, id, claim, state, asks })
    }
}

impl ChatSession {
    /// [`Turn::of`] this chat.
    pub fn turn(&self, trigger: Uuid, agent: Option<Uuid>) -> Option<Turn> {
        Turn::of(self, trigger, agent)
    }
}

//! Asks folded with their envelope's provenance.
//!
//! [`FlowState::apply`] sees only a record; who signed it decides three
//! things here, all fold-local (nothing is refused, so every version accepts
//! the same events and keeps the same asks):
//!
//! * **A permission ask** counts only when its signer is the asking turn's
//!   host: the device whose reply placeholder committed that turn (or an
//!   agent key it certified), with the agent bound to it. Anyone else's
//!   `permission: true` folds as a plain question, attributed to them.
//! * **A close** counts only from the ask's own signer, or the other half
//!   of its host pair (the host device, or an agent key it certified).
//! * **An answer to a permission ask** counts only from the host's person
//!   or the agent's owner. Other answers stay in the log and count for
//!   nothing.
//!
//! Every input is the chat log in canonical order and the workspace roster
//! at each event's position, so every device decides the same.

use uuid::Uuid;

use super::{
    agent_from_key, AskPoster, AskState, FlowRecord, FlowState, MAX_ASKS_PER_CHAT, PRIMARY,
};
use crate::authz::RosterState;
use crate::events::SessionEventEnvelope;
use crate::session::ChatSession;

/// What the fold knows about the envelope carrying an ask or an answer.
#[derive(Debug, Clone, Default)]
pub struct AskProvenance {
    /// The key that signed it (a device, or an agent key).
    pub signer: Option<Uuid>,
    /// The device that key resolves to.
    pub host: Option<Uuid>,
    /// The signer's actor.
    pub by: AskPoster,
    /// Nothing to judge it by (the device-local workspace, whose events are
    /// all this device's own, or a record folded without its envelope): it
    /// folds as it always has.
    pub legacy: bool,
    /// An ask: its signer is the asking turn's host.
    pub from_host: bool,
    /// An ask: its signer is the asking agent's own key.
    pub own_agent_key: bool,
    /// An answer: its author may grant the ask's permission.
    pub may_grant: bool,
}

impl AskProvenance {
    /// A record without provenance: folded as before these rules.
    pub fn legacy() -> Self {
        Self {
            legacy: true,
            from_host: true,
            may_grant: true,
            ..Default::default()
        }
    }

    /// The provenance of `record` in `env`, judged at its position:
    /// `roster` is the workspace roster there, `session` the fold so far.
    /// `None` when `record` isn't an ask or an answer.
    pub fn of(
        record: &FlowRecord,
        env: &SessionEventEnvelope,
        session: &ChatSession,
        roster: &RosterState,
        trusted: bool,
    ) -> Option<Self> {
        if !matches!(
            record,
            FlowRecord::HumanAsk(_) | FlowRecord::HumanAnswered(_)
        ) {
            return None;
        }
        if trusted {
            return Some(Self::legacy());
        }
        let stamp = env.actor_stamp.as_ref();
        let by = AskPoster {
            id: stamp.map(|s| s.actor.id.clone()).unwrap_or_default(),
            name: stamp
                .map(|s| s.actor.display_name.clone())
                .unwrap_or_default(),
        };
        let signer = env.signer_device_id;
        let key = signer.and_then(|k| roster.valid_agent_key(k));
        let host = key.map(|k| k.host_device_id).or(signer);
        let mut p = Self {
            signer,
            host,
            by,
            ..Default::default()
        };
        match record {
            FlowRecord::HumanAsk(a) => {
                let agent = agent_from_key(&a.agent_key);
                p.own_agent_key = key.is_some_and(|k| Some(k.agent_id) == agent);
                p.from_host = host.is_some_and(|h| {
                    turn_host_ok(
                        session,
                        roster,
                        a.turn_id,
                        &a.agent_key,
                        agent,
                        key.map(|k| k.agent_id),
                        h,
                    )
                });
            }
            FlowRecord::HumanAnswered(ans) => {
                p.may_grant = session
                    .flow
                    .asks
                    .get(&ans.ask_id)
                    .is_none_or(|st| !st.ask.permission || may_grant(st, roster, &p.by.id, p.signer));
            }
            _ => {}
        }
        Some(p)
    }
}

/// Whether `host` runs the turn `turn_id` for `agent_key`: its reply
/// placeholder committed that turn on `host`, the reply is that agent's,
/// and the agent is bound to `host` (the chat's runtime: `host` is a
/// trusted device, signing as itself).
fn turn_host_ok(
    session: &ChatSession,
    roster: &RosterState,
    turn_id: Uuid,
    agent_key: &str,
    agent: Option<Uuid>,
    key_agent: Option<Uuid>,
    host: Uuid,
) -> bool {
    let committed = session
        .turn_dispatch
        .values()
        .any(|t| t.answer == Some(turn_id) && t.answered_by == Some(host));
    if !committed {
        return false;
    }
    let Some(reply) = session.messages.iter().find(|m| m.id == turn_id) else {
        return false;
    };
    match agent {
        Some(a) => {
            reply.responder_id == Some(a)
                && key_agent.is_none_or(|k| k == a)
                && roster.agent_bound_to(a, host)
        }
        None => {
            agent_key == PRIMARY
                && reply.responder_id.is_none()
                && key_agent.is_none()
                && roster.device_trusted(host)
        }
    }
}

/// Whether `author` may grant `st`'s permission: the asking agent's owner,
/// or the person of the host that asked (an account that certified it).
/// The answer's signing device must be certified for the author's account,
/// whatever its stamp names.
fn may_grant(st: &AskState, roster: &RosterState, author: &str, signer: Option<Uuid>) -> bool {
    if author.is_empty() {
        return false;
    }
    let Some(account) = roster.members.get(author).and_then(|m| m.account_id) else { return false };
    if !signer.is_some_and(|d| roster.account_certified(account, d)) {
        return false;
    }
    let account = Some(account);
    let owner = agent_from_key(&st.ask.agent_key)
        .and_then(|a| roster.agents.get(&a))
        .map(|a| a.owner.as_str());
    let is_owner = owner.is_some_and(|o| {
        o == author || account.is_some_and(|acct| Uuid::parse_str(o).ok() == Some(acct))
    });
    let host_person = st
        .host
        .zip(account)
        .is_some_and(|(h, acct)| roster.account_certified(acct, h));
    is_owner || host_person
}

impl FlowState {
    /// Asks a base snapshot carries: what the snapshot claims about who
    /// posted them isn't evidence, so none is a permission prompt or has a
    /// known signer, and eviction reads exactly what older builds read from
    /// the same snapshot (its answer and closed flags).
    pub fn reseat_snapshot_asks(&mut self) {
        for st in self.asks.values_mut() {
            st.ask.permission = false;
            st.signer = None;
            st.host = None;
            st.legacy_settled = !st.is_open();
        }
    }

    /// Fold an ask or an answer with its provenance (see the module docs).
    pub fn apply_ask(&mut self, record: &FlowRecord, p: &AskProvenance) {
        match record {
            FlowRecord::HumanAsk(a) => match self.asks.get_mut(&a.ask_id) {
                // Only the asking turn may close it; the question is fixed.
                Some(st) if st.ask.turn_id == a.turn_id && st.ask.agent_key == a.agent_key => {
                    st.legacy_settled |= a.closed;
                    let unknown = st.signer.is_none() && st.host.is_none();
                    let pair = (p.signer.is_some() && p.signer == st.signer)
                        || (p.host.is_some() && p.host == st.host);
                    if p.legacy || unknown || pair {
                        st.ask.closed |= a.closed;
                    }
                }
                Some(_) => {}
                None => {
                    if self.asks.len() >= MAX_ASKS_PER_CHAT {
                        if let Some(oldest) = self
                            .asks
                            .iter()
                            .filter(|(_, s)| s.evictable())
                            .min_by_key(|(_, s)| s.ask.at_ms)
                            .map(|(k, _)| *k)
                        {
                            self.asks.remove(&oldest);
                        }
                    }
                    let mut ask = a.clone();
                    ask.permission &= p.from_host;
                    let theirs = !p.legacy && !p.from_host && !p.own_agent_key;
                    self.asks.insert(
                        a.ask_id,
                        AskState {
                            ask,
                            answer: None,
                            posted_by: theirs.then(|| p.by.clone()),
                            signer: p.signer,
                            host: p.host,
                            legacy_settled: a.closed,
                        },
                    );
                }
            },
            FlowRecord::HumanAnswered(ans) => {
                if let Some(st) = self.asks.get_mut(&ans.ask_id) {
                    st.legacy_settled = true;
                    // First answer that counts, in canonical order; a closed
                    // ask takes none.
                    if st.answer.is_none() && !st.ask.closed && (p.legacy || p.may_grant) {
                        st.answer = Some(ans.clone());
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{HumanAnswered, HumanAsk};

    fn by(signer: u128) -> AskProvenance {
        let d = Uuid::from_u128(signer);
        AskProvenance {
            signer: Some(d),
            host: Some(d),
            by: AskPoster {
                id: format!("p{signer}"),
                name: String::new(),
            },
            from_host: true,
            may_grant: false,
            ..Default::default()
        }
    }

    /// Eviction picks the asks a build without these rules would: an ask
    /// someone else "closed" (or answered without the right to) is still
    /// open here, but it is settled for eviction, so both keep the same asks
    /// and judge a later answer's `optionIndex` alike.
    #[test]
    fn eviction_follows_what_every_version_folds() {
        let mut f = FlowState::default();
        let first = HumanAsk {
            ask_id: Uuid::from_u128(1),
            turn_id: Uuid::from_u128(100),
            agent_key: "a".into(),
            at_ms: 1,
            permission: true,
            ..Default::default()
        };
        f.apply_ask(&FlowRecord::HumanAsk(first.clone()), &by(7));
        f.apply_ask(
            &FlowRecord::HumanAsk(HumanAsk {
                closed: true,
                ..first.clone()
            }),
            &by(8),
        );
        assert!(f.asks[&first.ask_id].is_open() && f.asks[&first.ask_id].legacy_settled);
        // An ineligible answer to a permission ask: open here, settled for eviction.
        let second = HumanAsk {
            ask_id: Uuid::from_u128(2),
            at_ms: 2,
            ..first.clone()
        };
        f.apply_ask(&FlowRecord::HumanAsk(second.clone()), &by(7));
        f.apply_ask(
            &FlowRecord::HumanAnswered(HumanAnswered {
                ask_id: second.ask_id,
                answer: "yes".into(),
                ..Default::default()
            }),
            &by(8),
        );
        assert!(f.asks[&second.ask_id].is_open() && f.asks[&second.ask_id].legacy_settled);
        for i in 3..=MAX_ASKS_PER_CHAT as u128 {
            f.apply_ask(
                &FlowRecord::HumanAsk(HumanAsk {
                    ask_id: Uuid::from_u128(i),
                    at_ms: 10 + i as i64,
                    ..first.clone()
                }),
                &by(7),
            );
        }
        let mut legacy = FlowState::default();
        for r in [
            FlowRecord::HumanAsk(first.clone()),
            FlowRecord::HumanAsk(HumanAsk {
                closed: true,
                ..first.clone()
            }),
            FlowRecord::HumanAsk(second.clone()),
            FlowRecord::HumanAnswered(HumanAnswered {
                ask_id: second.ask_id,
                answer: "yes".into(),
                ..Default::default()
            }),
        ] {
            legacy.apply(&r);
        }
        for i in 3..=MAX_ASKS_PER_CHAT as u128 {
            legacy.apply(&FlowRecord::HumanAsk(HumanAsk {
                ask_id: Uuid::from_u128(i),
                at_ms: 10 + i as i64,
                ..first.clone()
            }));
        }
        for n in 0..2u128 {
            let next = FlowRecord::HumanAsk(HumanAsk {
                ask_id: Uuid::from_u128(1000 + n),
                at_ms: 9000,
                ..first.clone()
            });
            f.apply_ask(&next, &by(7));
            legacy.apply(&next);
            let keys = |s: &FlowState| s.asks.keys().copied().collect::<Vec<_>>();
            assert_eq!(keys(&f), keys(&legacy));
        }
        assert!(!f.asks.contains_key(&first.ask_id) && !f.asks.contains_key(&second.ask_id));
    }

    /// A permission flag from anyone but the turn's host folds off, and the
    /// ask is theirs; the agent's own key is never "someone else".
    #[test]
    fn a_permission_flag_needs_the_host() {
        let mut f = FlowState::default();
        let a = HumanAsk {
            ask_id: Uuid::from_u128(1),
            permission: true,
            ..Default::default()
        };
        f.apply_ask(
            &FlowRecord::HumanAsk(a.clone()),
            &AskProvenance {
                from_host: false,
                ..by(9)
            },
        );
        assert!(!f.asks[&a.ask_id].ask.permission);
        assert_eq!(
            f.asks[&a.ask_id].posted_by.as_ref().map(|p| p.id.as_str()),
            Some("p9")
        );
        let b = HumanAsk {
            ask_id: Uuid::from_u128(2),
            permission: true,
            ..Default::default()
        };
        f.apply_ask(
            &FlowRecord::HumanAsk(b.clone()),
            &AskProvenance {
                from_host: false,
                own_agent_key: true,
                ..by(9)
            },
        );
        assert!(!f.asks[&b.ask_id].ask.permission && f.asks[&b.ask_id].posted_by.is_none());
    }
}

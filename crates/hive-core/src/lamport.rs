//! Lamport bounds: an event's lamport is a fact about its log, not its
//! author's word.
//!
//! The canonical fold order is `(lamport, event_id)`, and the lamport is set
//! by whoever signs the event. Unchecked, an author (or a box that drafted a
//! phone-signed event and kept it back) could give an event a huge lamport
//! and release it later: it would sort after events that are genuinely newer
//! (a stale approve beating a later reject, a delayed clear lifting a newer
//! lockdown), and a lamport near `u64::MAX` would drag every honest clock
//! after it.
//!
//! So every event names the newest event of its own log its author held when
//! signing, [`ActorStamp::log_head`](crate::ActorStamp::log_head) (its
//! canonical key, `{l, e}`), and its lamport must be exactly one past it:
//!
//! * `log_head` absent: the event is the first its author saw on the log (a
//!   seed), and its lamport is `1`;
//! * `log_head = {l, e}`: the log holds `e` at lamport `l`, `e` itself obeys
//!   this rule, and the event's lamport is `l + 1`;
//! * and its signer's previous event on the log (`prev`), when the log holds
//!   it, has a lower lamport.
//!
//! Together: `lamport = 1 + max(lamport(log_head), lamport(prev))`, where
//! the head is the newest event the author held, so a lamport can never be
//! more than one past a real event, and by induction never more than the
//! length of the longest chain of real events on the log. An event that
//! breaks the rule is denied by every fold ([`Plausibility::Implausible`]);
//! one whose head this device doesn't hold yet is held until it arrives
//! ([`Plausibility::Unresolved`]).
//!
//! Honest authors always satisfy it: they set `log_head` to the newest event
//! of the log that is [`Plausibility::Plausible`] on their device
//! ([`head`]) and the lamport to one past it ([`next`]). Concurrent writers
//! and offline devices each count from what they held, which is always a
//! real event; their own previous event is among what they held, so it is
//! never newer than the head. The rule is structural: it reads only ids and
//! lamports, never signatures or roles, so a device judges it the same way
//! whatever it thinks of the events' authors, and every event on the log
//! (denied or not) is a witness for the events after it.

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::events::SessionEventEnvelope;
use crate::identity::CfgFrontier;

/// How far past the newest plausible event a missing head may claim to be
/// and still be waited for (as the config frontier's A-14 slack).
pub const HEAD_SLACK: u64 = crate::authz::timeline::FRONTIER_SLACK;

/// Whether an event's lamport is one its log can account for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plausibility {
    /// One past a plausible event the log holds (or `1`, with no head).
    Plausible,
    /// Its head isn't on this device yet (or isn't decided yet): held.
    Unresolved,
    /// Not what its head and chain allow: denied.
    Implausible,
}

/// Every event's plausibility on one log (by event id; an id held under
/// more than one envelope is `Plausible` when any copy is, else
/// `Unresolved` when any copy is).
#[derive(Debug, Clone, Default)]
pub struct Bounds {
    by_id: HashMap<Uuid, Vec<(u64, Plausibility)>>,
    newest: Option<CfgFrontier>,
}

impl Bounds {
    /// Judge every envelope of one log.
    pub fn of(envelopes: &[SessionEventEnvelope]) -> Self {
        let refs: Vec<&SessionEventEnvelope> = envelopes.iter().collect();
        Self::of_refs(&refs)
    }

    /// [`Bounds::of`] over references.
    pub fn of_refs(envelopes: &[&SessionEventEnvelope]) -> Self {
        // Every lamport an id is claimed under (to tell a missing head from
        // one named under the wrong lamport).
        let mut claimed: HashMap<Uuid, Vec<u64>> = HashMap::new();
        for e in envelopes {
            claimed.entry(e.event_id).or_default().push(e.lamport);
        }
        let mut order: Vec<&SessionEventEnvelope> = envelopes.to_vec();
        order.sort_by_key(|e| e.canonical_key());
        let first = Self::run(&order, &claimed, &HashSet::new());
        // A-14: a missing head too far past the newest plausible event can't
        // be waited for: the event is left out (denied, so it blocks no side
        // effect), and is judged again as the log grows. Leaving out an
        // unresolved event changes no plausible one, so the newest stays.
        let newest = first.0.newest.map_or(0, |n| n.lamport);
        let forced: HashSet<(Uuid, u64)> = first
            .1
            .iter()
            .filter(|(_, h)| h.lamport > newest.saturating_add(HEAD_SLACK))
            .map(|(e, _)| (e.event_id, e.lamport))
            .collect();
        if forced.is_empty() {
            first.0
        } else {
            Self::run(&order, &claimed, &forced).0
        }
    }

    /// One pass in canonical order (a head is always one lamport below its
    /// event, so it is judged first). Returns the events whose head is
    /// missing, with that head.
    fn run<'a>(
        order: &[&'a SessionEventEnvelope],
        claimed: &HashMap<Uuid, Vec<u64>>,
        forced: &HashSet<(Uuid, u64)>,
    ) -> (Self, Vec<(&'a SessionEventEnvelope, CfgFrontier)>) {
        let mut out = Self::default();
        let mut missing = Vec::new();
        for e in order {
            let v = if forced.contains(&(e.event_id, e.lamport)) {
                Plausibility::Implausible
            } else {
                out.judge(e, claimed, &mut missing)
            };
            if v == Plausibility::Plausible {
                let k = CfgFrontier { lamport: e.lamport, event_id: e.event_id };
                if out.newest.is_none_or(|n| k > n) {
                    out.newest = Some(k);
                }
            }
            out.by_id.entry(e.event_id).or_default().push((e.lamport, v));
        }
        (out, missing)
    }

    fn judge<'a>(
        &self,
        e: &'a SessionEventEnvelope,
        claimed: &HashMap<Uuid, Vec<u64>>,
        missing: &mut Vec<(&'a SessionEventEnvelope, CfgFrontier)>,
    ) -> Plausibility {
        let stamp = e.actor_stamp.as_ref();
        // The signer's previous event, when held, is older.
        if let Some(p) = stamp.and_then(|s| s.prev) {
            if claimed.get(&p).is_some_and(|ls| ls.iter().all(|l| *l >= e.lamport)) {
                return Plausibility::Implausible;
            }
        }
        let Some(h) = stamp.and_then(|s| s.log_head) else {
            return if e.lamport == 1 { Plausibility::Plausible } else { Plausibility::Implausible };
        };
        if h.lamport.checked_add(1) != Some(e.lamport) || h.event_id == e.event_id {
            return Plausibility::Implausible;
        }
        match self.by_id.get(&h.event_id) {
            Some(copies) => {
                let at: Vec<Plausibility> = copies.iter().filter(|c| c.0 == h.lamport).map(|c| c.1).collect();
                if at.contains(&Plausibility::Plausible) {
                    Plausibility::Plausible
                } else if at.contains(&Plausibility::Unresolved) {
                    Plausibility::Unresolved
                } else {
                    Plausibility::Implausible
                }
            }
            // Held under another lamport only: a forged reference.
            None if claimed.contains_key(&h.event_id) => Plausibility::Implausible,
            None => {
                missing.push((e, h));
                Plausibility::Unresolved
            }
        }
    }

    /// The verdict on `event_id` (`None` when the log doesn't hold it).
    pub fn of_id(&self, event_id: Uuid) -> Option<Plausibility> {
        let copies = self.by_id.get(&event_id)?;
        let has = |p| copies.iter().any(|c| c.1 == p);
        Some(if has(Plausibility::Plausible) {
            Plausibility::Plausible
        } else if has(Plausibility::Unresolved) {
            Plausibility::Unresolved
        } else {
            Plausibility::Implausible
        })
    }

    /// The verdict on this very envelope (its id at its lamport).
    pub fn of_env(&self, e: &SessionEventEnvelope) -> Plausibility {
        self.by_id
            .get(&e.event_id)
            .and_then(|copies| copies.iter().find(|c| c.0 == e.lamport).map(|c| c.1))
            .unwrap_or(Plausibility::Unresolved)
    }

    /// The newest plausible event: what an honest author's next event on
    /// this log names as its `log_head`.
    pub fn head(&self) -> Option<CfgFrontier> {
        self.newest
    }
}

/// An inert stand-in for `env` that keeps only what the lamport rule reads:
/// its log, id, lamport, `log_head` and `prev`. No payload (`Unknown`, a
/// no-op in every fold), no author (an empty `System` actor), no signer.
/// For a fold of part of a log (a trust roster, events refused at ingest):
/// the events after it still count from it, and it can change nothing else.
pub fn inert_witness(env: &SessionEventEnvelope) -> SessionEventEnvelope {
    let stamp = env.actor_stamp.as_ref();
    let mut w = SessionEventEnvelope::new(env.session_id, env.workspace_id, 0, crate::events::SessionEvent::Unknown);
    w.event_id = env.event_id;
    w.id = env.id;
    w.lamport = env.lamport;
    w.timestamp = crate::time_util::Timestamp::epoch();
    w.actor_stamp = Some(crate::identity::ActorStamp {
        actor: crate::identity::ActorIdentity::new("", "", crate::identity::ActorKind::System),
        recorded_at: crate::time_util::Timestamp::epoch(),
        cfg_frontier: None,
        presence: None,
        runtime_effect: false,
        prev: stamp.and_then(|s| s.prev),
        sig_alg: Default::default(),
        log_head: stamp.and_then(|s| s.log_head),
    });
    w
}

/// The `log_head` an honest author stamps on its next event on this log.
pub fn head(envelopes: &[SessionEventEnvelope]) -> Option<CfgFrontier> {
    Bounds::of(envelopes).head()
}

/// The lamport of an event that names `log_head`: one past it, or `1` for
/// the first event on a log.
pub fn next(log_head: Option<CfgFrontier>) -> u64 {
    log_head.map_or(1, |h| h.lamport.saturating_add(1))
}

/// Stamp `env` (already carrying its `actor_stamp`) as the next event after
/// `log_head`: the head in the stamp and the lamport one past it. Sign after.
pub fn stamp_next(env: &mut SessionEventEnvelope, log_head: Option<CfgFrontier>) {
    env.lamport = next(log_head);
    if let Some(s) = env.actor_stamp.as_mut() {
        s.log_head = log_head;
    }
}

/// Stamp `env` as an honest author holding `log` would: [`stamp_next`]
/// after the newest plausible event of `log` (which `env` isn't in yet).
pub fn stamp_after(env: &mut SessionEventEnvelope, log: &[SessionEventEnvelope]) {
    stamp_next(env, head(log));
}

/// The canonical keys of `log`'s plausible events, ascending: the positions
/// an honest author can name.
pub fn plausible_keys(log: &[SessionEventEnvelope]) -> Vec<CfgFrontier> {
    let bounds = Bounds::of(log);
    let mut keys: Vec<CfgFrontier> = log
        .iter()
        .filter(|e| bounds.of_env(e) == Plausibility::Plausible)
        .map(|e| CfgFrontier { lamport: e.lamport, event_id: e.event_id })
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// Stamp `env` as an author who held `log` only up to `back` plausible
/// events before its newest one (an offline device, or a backdated draft):
/// it names that older event as its head. `back = 0` is [`stamp_after`].
/// Its `prev`, if any, must still be older (the caller's to keep so).
pub fn stamp_back(env: &mut SessionEventEnvelope, log: &[SessionEventEnvelope], back: usize) {
    let keys = plausible_keys(log);
    let head = keys.len().checked_sub(1 + back).map(|i| keys[i]).or_else(|| keys.first().copied());
    stamp_next(env, head);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::SessionEvent;
    use crate::identity::{ActorIdentity, ActorKind, ActorStamp};
    use crate::time_util::Timestamp;

    fn ev(head: Option<&SessionEventEnvelope>, lamport: Option<u64>) -> SessionEventEnvelope {
        let mut e = SessionEventEnvelope::new(Uuid::nil(), Uuid::nil(), 1, SessionEvent::SessionTitleChanged { title: "t".into() });
        e.actor_stamp = Some(ActorStamp {
            actor: ActorIdentity::new("a", "a", ActorKind::Human),
            recorded_at: Timestamp::epoch(),
            cfg_frontier: None,
            presence: None,
            runtime_effect: false,
            prev: None,
            sig_alg: Default::default(),
            log_head: None,
        });
        stamp_next(&mut e, head.map(|h| CfgFrontier { lamport: h.lamport, event_id: h.event_id }));
        if let Some(l) = lamport {
            e.lamport = l;
        }
        e
    }

    #[test]
    fn a_chain_is_plausible_and_an_inflated_link_is_not() {
        let a = ev(None, None);
        let b = ev(Some(&a), None);
        let c = ev(Some(&b), None);
        let mut big = ev(Some(&b), Some(u64::MAX));
        if let Some(s) = big.actor_stamp.as_mut() {
            s.log_head = Some(CfgFrontier { lamport: u64::MAX - 1, event_id: b.event_id });
        }
        let after_big = ev(Some(&big), None);
        let all = vec![a.clone(), b.clone(), c.clone(), big.clone(), after_big.clone()];
        let bounds = Bounds::of(&all);
        for e in [&a, &b, &c] {
            assert_eq!(bounds.of_env(e), Plausibility::Plausible);
        }
        assert_eq!(bounds.of_env(&big), Plausibility::Implausible, "a head named under the wrong lamport");
        assert_eq!(bounds.of_env(&after_big), Plausibility::Implausible, "built on an implausible head");
        assert_eq!(bounds.head().map(|h| h.event_id), Some(c.event_id), "the newest plausible event");
    }

    #[test]
    fn a_missing_head_holds_and_a_seed_is_lamport_one() {
        let a = ev(None, None);
        let b = ev(Some(&a), None);
        let bounds = Bounds::of(std::slice::from_ref(&b));
        assert_eq!(bounds.of_env(&b), Plausibility::Unresolved);
        let five = ev(None, Some(5));
        assert_eq!(Bounds::of(std::slice::from_ref(&five)).of_env(&five), Plausibility::Implausible, "a first event is lamport 1");
    }
}

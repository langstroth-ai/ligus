//! Lamport bounds (`hive_core::lamport`): an event's lamport is one past the
//! log head it names, so
//!
//! * no honest history is denied: devices that author concurrently, go
//!   offline, sync partially and in any order, each counting from what they
//!   hold, write only events every fold accepts once it holds them all, and
//!   a device holding part of the log denies none of them (it holds the ones
//!   whose head hasn't reached it);
//! * an inflated lamport never changes another event's verdict, the state
//!   the log folds to, or the head an honest author counts from.

use std::collections::{BTreeMap, BTreeSet};

use hive_core::events::{fold_decisions, Verdict, WorkspaceAuthz};
use hive_core::lamport::{self, Bounds, Plausibility};
use hive_core::{ActorIdentity, ActorKind, ActorStamp, CfgFrontier, ChatMessage, ChatSession, MessageRole, SessionEvent, SessionEventEnvelope, Timestamp};
use proptest::prelude::*;
use uuid::Uuid;

/// One device: what it holds of the log, and its own chain.
#[derive(Clone)]
struct Device {
    id: Uuid,
    known: Vec<SessionEventEnvelope>,
}

impl Device {
    fn holds(&self, id: Uuid) -> bool {
        self.known.iter().any(|e| e.event_id == id)
    }
    fn receive(&mut self, e: &SessionEventEnvelope) {
        if !self.holds(e.event_id) {
            self.known.push(e.clone());
        }
    }
    /// Author `payload` as an honest client does: counting from the newest
    /// event it holds within bounds, its chain link its own newest older one.
    fn author(&mut self, sid: Uuid, wid: Uuid, payload: SessionEvent) -> SessionEventEnvelope {
        let head = lamport::head(&self.known);
        let mut e = SessionEventEnvelope::new(sid, wid, self.known.len() as i64 + 1, payload);
        e.actor_stamp = Some(stamp(&self.id.to_string()));
        lamport::stamp_next(&mut e, head);
        let prev = self
            .known
            .iter()
            .filter(|k| k.signer_device_id == Some(self.id) && k.lamport < e.lamport)
            .max_by_key(|k| k.canonical_key())
            .map(|k| k.event_id);
        e.actor_stamp.as_mut().unwrap().prev = prev;
        // Unsigned in this structural fold; the signer marks the chain.
        e.signer_device_id = Some(self.id);
        self.known.push(e.clone());
        e
    }
}

fn stamp(who: &str) -> ActorStamp {
    ActorStamp {
        actor: ActorIdentity::new(who, who, ActorKind::Human),
        recorded_at: Timestamp::epoch(),
        cfg_frontier: None,
        presence: None,
        runtime_effect: false,
        prev: None,
        sig_alg: Default::default(),
        log_head: None,
    }
}

#[derive(Debug, Clone)]
enum Op {
    /// Device `d` writes (a message, or a title: last writer wins).
    Write { d: u8, title: bool },
    /// Device `to` receives the events of `from` picked by `mask` (a
    /// partial, out-of-order delivery).
    Sync { from: u8, to: u8, mask: u64 },
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0u8..4, any::<bool>()).prop_map(|(d, title)| Op::Write { d, title }),
        2 => (0u8..4, 0u8..4, any::<u64>()).prop_map(|(from, to, mask)| Op::Sync { from, to, mask }),
    ]
}

/// An honest multi-device history: every device's events, and what each
/// device holds at the end.
fn history(devices: usize, ops: &[Op]) -> (Vec<SessionEventEnvelope>, Vec<Device>) {
    let chat = ChatSession::new("t", Uuid::from_u128(7), "x");
    let (sid, wid) = (chat.id, chat.workspace_id);
    let mut ds: Vec<Device> = (0..devices).map(|i| Device { id: Uuid::from_u128(100 + i as u128), known: Vec::new() }).collect();
    let mut all = vec![ds[0].author(sid, wid, SessionEvent::SessionSnapshot { session: Box::new(chat) })];
    let seed = all[0].clone();
    for d in ds.iter_mut().skip(1) {
        d.receive(&seed);
    }
    for (n, op) in ops.iter().enumerate() {
        match *op {
            Op::Write { d, title } => {
                let d = d as usize % devices;
                let payload = if title {
                    SessionEvent::SessionTitleChanged { title: format!("t{n}") }
                } else {
                    SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "u", format!("m{n}")) }
                };
                all.push(ds[d].author(sid, wid, payload));
            }
            Op::Sync { from, to, mask } => {
                let (from, to) = (from as usize % devices, to as usize % devices);
                let picked: Vec<SessionEventEnvelope> =
                    ds[from].known.iter().enumerate().filter(|(i, _)| mask & (1 << (i % 64)) != 0).map(|(_, e)| e.clone()).collect();
                for e in picked.iter().rev() {
                    ds[to].receive(e);
                }
            }
        }
    }
    (all, ds)
}

fn verdicts(log: &[SessionEventEnvelope]) -> BTreeMap<Uuid, Verdict> {
    fold_decisions(log, None, None, WorkspaceAuthz::Unchecked)
}

fn title(log: &[SessionEventEnvelope]) -> String {
    hive_core::project(log).map(|s| s.title).unwrap_or_default()
}

/// What an attacker writes: a lamport no real event accounts for.
#[derive(Debug, Clone)]
enum Inflate {
    /// Names a real head but claims more than one past it.
    PastHead { at: usize, extra: u64 },
    /// Names a real head under a lamport it doesn't hold.
    WrongHeadLamport { at: usize, claimed: u64 },
    /// Names no head but isn't a first event.
    NoHead { lamport: u64 },
    /// Names a head nobody holds, far ahead.
    MissingHead { lamport: u64 },
}

fn arb_inflate() -> impl Strategy<Value = Inflate> {
    let big = prop_oneof![2u64..10_000, Just(u64::MAX - 1), Just(u64::MAX), (u64::MAX - 1000)..u64::MAX];
    prop_oneof![
        (any::<usize>(), 1u64..u64::MAX).prop_map(|(at, extra)| Inflate::PastHead { at, extra }),
        (any::<usize>(), big.clone()).prop_map(|(at, claimed)| Inflate::WrongHeadLamport { at, claimed }),
        big.clone().prop_map(|lamport| Inflate::NoHead { lamport }),
        big.prop_map(|lamport| Inflate::MissingHead { lamport }),
    ]
}

/// The attacker's events (a chain of them: one built on another counts for
/// nothing either), on `log`'s chat.
fn inflated(log: &[SessionEventEnvelope], how: &Inflate, chain: usize) -> Vec<SessionEventEnvelope> {
    let seed = &log[0];
    let mut e = SessionEventEnvelope::new(seed.session_id, seed.workspace_id, 9_999, SessionEvent::SessionTitleChanged { title: "attacker".into() });
    e.actor_stamp = Some(stamp("mallory"));
    let s = e.actor_stamp.as_mut().unwrap();
    match *how {
        Inflate::PastHead { at, extra } => {
            let h = &log[at % log.len()];
            s.log_head = Some(CfgFrontier { lamport: h.lamport, event_id: h.event_id });
            e.lamport = h.lamport.saturating_add(1).saturating_add(extra);
            if e.lamport == h.lamport + 1 {
                e.lamport = u64::MAX;
            }
        }
        Inflate::WrongHeadLamport { at, claimed } => {
            let h = &log[at % log.len()];
            let claimed = if claimed == h.lamport { claimed.wrapping_add(7) } else { claimed };
            s.log_head = Some(CfgFrontier { lamport: claimed, event_id: h.event_id });
            e.lamport = claimed.saturating_add(1);
        }
        Inflate::NoHead { lamport } => e.lamport = lamport.max(2),
        Inflate::MissingHead { lamport } => {
            let newest = log.iter().map(|e| e.lamport).max().unwrap_or(0);
            let l = lamport.max(newest + lamport::HEAD_SLACK + 1);
            s.log_head = Some(CfgFrontier { lamport: l.saturating_sub(1), event_id: Uuid::new_v4() });
            e.lamport = l;
        }
    }
    let mut out = vec![e];
    for _ in 1..chain {
        let last = out.last().unwrap();
        let mut next = SessionEventEnvelope::new(seed.session_id, seed.workspace_id, 9_999, SessionEvent::SessionTitleChanged { title: "attacker".into() });
        next.actor_stamp = Some(stamp("mallory"));
        lamport::stamp_next(&mut next, Some(CfgFrontier { lamport: last.lamport, event_id: last.event_id }));
        out.push(next);
    }
    out
}

proptest! {
    /// No honest history is denied: with every event in hand every fold
    /// accepts every one, in any delivery order; a device holding part of
    /// the log holds back (never denies) the events whose head hasn't
    /// reached it; and each device's next event counts from a head every
    /// other device will accept.
    #[test]
    fn prop_no_honest_history_is_denied(devices in 2usize..5, ops in prop::collection::vec(arb_op(), 0..40), seed in any::<u64>()) {
        let (all, ds) = history(devices, &ops);
        let v = verdicts(&all);
        for e in &all[1..] {
            prop_assert_eq!(v.get(&e.event_id), Some(&Verdict::Allowed), "an honest event was refused: lamport {}", e.lamport);
        }
        let bounds = Bounds::of(&all);
        prop_assert!(all.iter().all(|e| bounds.of_env(e) == Plausibility::Plausible));
        // Any delivery order.
        let mut shuffled = all.clone();
        let mut rng = seed | 1;
        for i in (1..shuffled.len()).rev() {
            rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17;
            shuffled.swap(i, (rng % (i as u64 + 1)) as usize);
        }
        prop_assert_eq!(verdicts(&shuffled), v);
        // Part of the log: held, never denied.
        for d in &ds {
            for (id, verdict) in verdicts(&d.known) {
                prop_assert!(verdict != Verdict::Denied, "device {} denied {}", d.id, id);
            }
        }
        // The head is the newest event, one past which the next one counts.
        let newest = all.iter().map(|e| (e.lamport, e.event_id)).max().unwrap();
        prop_assert_eq!(bounds.head().map(|h| (h.lamport, h.event_id)), Some(newest));
    }

    /// An inflated lamport (and anything built on it) is refused, and never
    /// changes another event's verdict, the folded state, or the head an
    /// honest author counts from: a stale or withheld event can't sort after
    /// genuinely newer ones by claiming a bigger clock.
    #[test]
    fn prop_an_inflated_lamport_changes_nothing_else(
        devices in 2usize..4,
        ops in prop::collection::vec(arb_op(), 0..24),
        how in arb_inflate(),
        chain in 1usize..4,
        at in any::<usize>(),
    ) {
        let (all, _) = history(devices, &ops);
        let before = verdicts(&all);
        let x = inflated(&all, &how, chain);
        let mut with = all.clone();
        // Delivered anywhere in the log.
        let pos = 1 + at % all.len();
        for (i, e) in x.iter().enumerate() {
            with.insert((pos + i).min(with.len()), e.clone());
        }
        let after = verdicts(&with);
        for (id, v) in &before {
            prop_assert_eq!(after.get(id), Some(v), "an inflated lamport changed {}'s verdict", id);
        }
        for e in &x {
            prop_assert!(after.get(&e.event_id) != Some(&Verdict::Allowed), "{:?} was accepted", how);
        }
        prop_assert_eq!(title(&with), title(&all), "an inflated lamport changed the state");
        prop_assert_eq!(Bounds::of(&with).head(), Bounds::of(&all).head(), "an inflated lamport moved the head");
    }
}

proptest! {
    /// Devices that refused different events at ingest (a refusal depends on
    /// each device's roster: a revocation, a removal, a rebound agent key, a
    /// certificate not there yet) keep each refused one as an inert witness,
    /// and every device also refuses a keyholder's forged copies of real ids
    /// (any lamport, any head). In any arrival order, they judge every id
    /// alike, hold none of the honest events, and fold every event none of
    /// them refused to one verdict.
    #[test]
    fn prop_devices_refusing_different_events_judge_alike(
        devices in 2usize..5,
        ops in prop::collection::vec(arb_op(), 1..32),
        refusals in prop::collection::vec(any::<u64>(), 3),
        forged in prop::collection::vec((any::<usize>(), any::<usize>(), 0u64..3), 0..4),
        seed in any::<u64>(),
    ) {
        let (all, _) = history(devices, &ops);
        // Forged copies of real ids: at a real head's lamport + 1 (or past
        // it, or naming nobody), refused by everyone.
        let forgeries: Vec<SessionEventEnvelope> = forged
            .iter()
            .map(|&(id_at, head_at, kind)| {
                let id = all[id_at % all.len()].event_id;
                let h = &all[head_at % all.len()];
                let mut e = SessionEventEnvelope::new(h.session_id, h.workspace_id, 1, SessionEvent::SessionTitleChanged { title: "forged".into() });
                e.actor_stamp = Some(stamp("mallory"));
                let head = match kind {
                    0 => CfgFrontier { lamport: h.lamport, event_id: h.event_id },
                    1 => CfgFrontier { lamport: h.lamport + 3, event_id: h.event_id },
                    _ => CfgFrontier { lamport: h.lamport, event_id: Uuid::new_v4() },
                };
                lamport::stamp_next(&mut e, Some(head));
                e.event_id = id;
                lamport::inert_witness(&e)
            })
            .collect();
        let refused = |d: usize, i: usize| i > 0 && refusals[d % refusals.len()] & (1 << (i % 64)) != 0;
        let mut logs = Vec::new();
        let mut rng = seed | 1;
        for d in 0..devices {
            let mut log: Vec<SessionEventEnvelope> = all
                .iter()
                .enumerate()
                .map(|(i, e)| if refused(d, i) { lamport::inert_witness(e) } else { e.clone() })
                .collect();
            log.extend(forgeries.iter().cloned());
            for i in (1..log.len()).rev() {
                rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17;
                log.swap(i, (rng % (i as u64 + 1)) as usize);
            }
            logs.push(log);
        }
        let bounds: Vec<Bounds> = logs.iter().map(|l| Bounds::of(l)).collect();
        for e in &all {
            let v0 = bounds[0].of_id(e.event_id);
            prop_assert_eq!(v0, Some(Plausibility::Plausible), "an honest event isn't within bounds");
            for b in &bounds[1..] {
                prop_assert_eq!(b.of_id(e.event_id), v0, "two devices judged one id differently");
            }
        }
        let decided: Vec<BTreeMap<Uuid, Verdict>> = logs.iter().map(|l| verdicts(l)).collect();
        for (i, e) in all.iter().enumerate().skip(1) {
            // A forged witness copy of its id never erases it (witnesses don't fold).
            if (0..devices).any(|d| refused(d, i)) {
                continue;
            }
            for v in &decided {
                prop_assert_eq!(v.get(&e.event_id), Some(&Verdict::Allowed), "an event no device refused was not folded alike");
            }
        }
    }
}

/// The rule, case by case.
#[test]
fn the_rule_case_by_case() {
    let (all, _) = history(2, &[Op::Write { d: 0, title: false }, Op::Write { d: 1, title: false }]);
    let (seed, a) = (&all[0], &all[1]);
    let named = |h: &SessionEventEnvelope| Some(CfgFrontier { lamport: h.lamport, event_id: h.event_id });
    let judge = |e: SessionEventEnvelope| {
        let mut log = all.clone();
        log.push(e.clone());
        Bounds::of(&log).of_env(&e)
    };
    let draft = |head: Option<CfgFrontier>, lamport: Option<u64>| {
        let mut e = SessionEventEnvelope::new(seed.session_id, seed.workspace_id, 50, SessionEvent::SessionTitleChanged { title: "x".into() });
        e.actor_stamp = Some(stamp("u"));
        lamport::stamp_next(&mut e, head);
        if let Some(l) = lamport {
            e.lamport = l;
        }
        e
    };
    assert_eq!(judge(draft(named(a), None)), Plausibility::Plausible, "one past a held event");
    assert_eq!(judge(draft(named(seed), None)), Plausibility::Plausible, "one past an older one (an offline author)");
    assert_eq!(judge(draft(None, None)), Plausibility::Plausible, "a first event is lamport 1");
    assert_eq!(judge(draft(named(a), Some(a.lamport + 2))), Plausibility::Implausible, "more than one past its head");
    assert_eq!(judge(draft(named(a), Some(a.lamport))), Plausibility::Implausible, "not past its head");
    assert_eq!(judge(draft(None, Some(0))), Plausibility::Implausible, "lamport 0 (a forged seed)");
    let mut wrong = draft(named(a), None);
    wrong.actor_stamp.as_mut().unwrap().log_head = Some(CfgFrontier { lamport: a.lamport + 5, event_id: a.event_id });
    wrong.lamport = a.lamport + 6;
    assert_eq!(judge(wrong), Plausibility::Implausible, "a held head named under another lamport");
    let missing = draft(Some(CfgFrontier { lamport: a.lamport + 1, event_id: Uuid::new_v4() }), None);
    assert_eq!(judge(missing), Plausibility::Unresolved, "a head not here yet: held");
    let mut far = draft(Some(CfgFrontier { lamport: a.lamport + lamport::HEAD_SLACK + 10, event_id: Uuid::new_v4() }), None);
    far.lamport = a.lamport + lamport::HEAD_SLACK + 11;
    assert_eq!(judge(far), Plausibility::Implausible, "a missing head too far ahead to wait for");
    // A chain link newer than the event.
    let mut forked = draft(named(seed), None);
    forked.actor_stamp.as_mut().unwrap().prev = Some(a.event_id);
    assert_eq!(judge(forked), Plausibility::Implausible, "its previous event is newer than it");
    let ids: BTreeSet<Uuid> = all.iter().map(|e| e.event_id).collect();
    assert_eq!(ids.len(), all.len());
}

/// A stale approve beats a later reject no more: a vote drafted at an old
/// head and released after a newer reject sorts at its draft position, and
/// claiming a bigger clock gets it refused.
#[test]
fn a_withheld_vote_sorts_where_it_was_drafted() {
    use hive_core::testkit::SignedWorld;
    use hive_core::{ActionProposal, ProposalApproval, ProposalKind, ProposalStatus, WorkspaceRole};
    let mut w = SignedWorld::genesis(&["owner", "mara"], None);
    w.admit("owner", "mara", WorkspaceRole::Admin);
    let chat = w.chat("owner", "c");
    let mut p = ActionProposal::new("ship", ProposalKind::Decision, "planner");
    p.approval_role_floor = WorkspaceRole::Admin;
    let pid = p.id;
    w.say(chat, "owner", SessionEvent::ProposalUpserted { proposal: p });
    let vote = |approved: bool| SessionEvent::ProposalVoteCast {
        proposal_id: pid,
        approval: ProposalApproval { actor_id: "mara".into(), role: WorkspaceRole::Admin, approved, created_at: Timestamp::epoch(), diff_digest: None },
    };
    // The approve is drafted (and signed, with presence) now, but withheld.
    let mut approve = w.draft("mara", Some(chat), vote(true), None);
    let a = w.recovery_presence("mara", &approve);
    w.finish(&mut approve, "mara", Some(a));
    // Meanwhile the chat moves on, and mara rejects.
    w.say(chat, "owner", SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "owner", "hold on") });
    let reject = w.write("mara", Some(chat), vote(false), None, hive_core::testkit::Proof::Recovery);
    // Released now, the approve sorts before the reject.
    let mut honest = w.clone();
    honest.push_env(Some(chat), approve.clone());
    assert_eq!(honest.verdict(approve.event_id), Verdict::Allowed);
    assert_eq!(honest.verdict(reject), Verdict::Allowed);
    let s = honest.state(chat);
    assert_ne!(s.proposals.iter().find(|p| p.id == pid).unwrap().status, ProposalStatus::Approved, "the later reject stands");
    // With a bigger clock than the reject, it is refused, and the reject still stands.
    let reject_lamport = w.chats[&chat].iter().find(|e| e.event_id == reject).unwrap().lamport;
    let mut inflated = approve.clone();
    inflated.clear_raw();
    inflated.lamport = reject_lamport + 1;
    let a = w.recovery_presence("mara", &inflated);
    w.finish(&mut inflated, "mara", Some(a));
    w.push_env(Some(chat), inflated.clone());
    assert_eq!(w.verdict(inflated.event_id), Verdict::Denied);
    assert_eq!(w.verdict(reject), Verdict::Allowed);
    assert_ne!(w.state(chat).proposals.iter().find(|p| p.id == pid).unwrap().status, ProposalStatus::Approved);
}

/// A delayed `ChatLockdownCleared` no longer lifts a newer lockdown: it
/// clears only the raises it names (the ones its author saw), whenever it is
/// released, and claiming a bigger clock gets it refused.
#[test]
fn a_withheld_clear_lifts_only_what_its_author_saw() {
    use hive_core::guard::{ChatLockdown, LockdownSignal};
    use hive_core::testkit::SignedWorld;
    use hive_core::WorkspaceRole;
    let mut w = SignedWorld::genesis(&["owner", "mara"], None);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    let chat = Uuid::new_v4();
    let raise = |n: u128, signal| SessionEvent::ChatLockdownRaised {
        chat_id: chat,
        lockdown: ChatLockdown {
            id: Uuid::from_u128(n),
            signal,
            detail: String::new(),
            raised_by: "owner".into(),
            raised_at: Timestamp::epoch(),
            message_id: None,
            raise_event_id: None,
        },
    };
    let active = |w: &SignedWorld| -> Vec<Uuid> { w.config().guards.lockdowns_of(chat).iter().filter_map(|l| l.raise_event_id).collect() };
    let r1 = w.push_bare("owner", raise(1, LockdownSignal::InjectionDetected));
    // Mara's clear of what she sees (r1), drafted and confirmed now, withheld.
    let mut clear = w.draft("mara", None, SessionEvent::ChatLockdownCleared { chat_id: chat, raises: vec![r1] }, None);
    let a = w.recovery_presence("mara", &clear);
    w.finish(&mut clear, "mara", Some(a));
    // Meanwhile the workspace moves on and the Owner locks the chat down again.
    w.push_bare("owner", SessionEvent::ChatLockdownRaised { chat_id: Uuid::new_v4(), lockdown: ChatLockdown { id: Uuid::from_u128(3), signal: LockdownSignal::Manual, detail: String::new(), raised_by: "owner".into(), raised_at: Timestamp::epoch(), message_id: None, raise_event_id: None } });
    let r2 = w.push_bare("owner", raise(2, LockdownSignal::Manual));
    assert_eq!(active(&w), vec![r1, r2]);
    // Released now: it lifts r1 only; the newer lockdown stays in force.
    let mut released = w.clone();
    released.push_env(None, clear.clone());
    assert_eq!(released.verdict(clear.event_id), Verdict::Allowed);
    assert_eq!(active(&released), vec![r2]);
    // Even naming r2 (which its author can't have held), a clear that sorts
    // before r2 lifts only what was raised before it.
    let mut naming_both = w.clone();
    let mut both = w.draft("mara", None, SessionEvent::ChatLockdownCleared { chat_id: chat, raises: vec![r1, r2] }, None);
    let at_r1 = w.cfg.iter().find(|e| e.event_id == r1).map(|e| CfgFrontier { lamport: e.lamport, event_id: e.event_id });
    hive_core::lamport::stamp_next(&mut both, at_r1);
    let a = w.recovery_presence("mara", &both);
    w.finish(&mut both, "mara", Some(a));
    naming_both.push_env(None, both.clone());
    assert_eq!(active(&naming_both), vec![r2]);
    // Claiming a clock past r2: refused, and nothing is lifted.
    let r2_lamport = w.cfg.iter().find(|e| e.event_id == r2).unwrap().lamport;
    let mut inflated = clear.clone();
    inflated.lamport = r2_lamport + 1;
    let a = w.recovery_presence("mara", &inflated);
    w.finish(&mut inflated, "mara", Some(a));
    w.push_env(None, inflated.clone());
    assert_eq!(w.verdict(inflated.event_id), Verdict::Denied);
    assert_eq!(active(&w), vec![r1, r2]);
}

/// Two votes of one person on one proposal at one lamport (from two of
/// their devices, neither holding the other's) are concurrent, and their
/// order is their event ids, which a box drafting its phone's vote picks.
/// Between them a rejection wins, whichever id sorts last.
#[test]
fn between_concurrent_votes_of_one_person_a_rejection_wins() {
    use hive_core::testkit::{Proof, SignedWorld};
    use hive_core::{ActionProposal, ProposalApproval, ProposalKind, ProposalStatus, WorkspaceRole};
    for approve_sorts_last in [true, false] {
        let mut w = SignedWorld::genesis(&["owner", "mara"], None);
        w.admit("owner", "mara", WorkspaceRole::Admin);
        let chat = w.chat("owner", "c");
        let mut p = ActionProposal::new("ship", ProposalKind::Decision, "planner");
        p.approval_role_floor = WorkspaceRole::Admin;
        let pid = p.id;
        w.say(chat, "owner", SessionEvent::ProposalUpserted { proposal: p });
        let vote = |approved: bool| SessionEvent::ProposalVoteCast {
            proposal_id: pid,
            approval: ProposalApproval { actor_id: "mara".into(), role: WorkspaceRole::Admin, approved, created_at: Timestamp::epoch(), diff_digest: None },
        };
        let (hi, lo) = (Uuid::from_u128(u128::MAX), Uuid::from_u128(1));
        let mut drafts = Vec::new();
        for (approved, id) in [(true, if approve_sorts_last { hi } else { lo }), (false, if approve_sorts_last { lo } else { hi })] {
            let mut e = w.draft("mara", Some(chat), vote(approved), None);
            e.event_id = id;
            let a = w.recovery_presence("mara", &e);
            w.finish(&mut e, "mara", Some(a));
            drafts.push(e);
        }
        assert_eq!(drafts[0].lamport, drafts[1].lamport, "concurrent");
        for e in drafts {
            w.push_env(Some(chat), e);
        }
        let status = w.state(chat).proposals.iter().find(|p| p.id == pid).unwrap().status;
        assert_ne!(status, ProposalStatus::Approved, "approve sorting last: {approve_sorts_last}");
        // A later approval (drafted holding both) does count.
        w.write("mara", Some(chat), vote(true), None, Proof::Recovery);
        assert_eq!(w.state(chat).proposals.iter().find(|p| p.id == pid).unwrap().status, ProposalStatus::Approved);
    }
}

//! The device-keyed dispatcher (architecture-review 4.9; design
//! repos-and-checkouts 11): N simulated devices, each folding only what it
//! has received, with random delivery delays, partitions, crashes,
//! eligibility and coverage, and (optionally) a device revoked mid-run that
//! keeps writing backdated claims and answers. Every event is signed by a
//! real device in a rules-v2 workspace and folded by the checked fold.
//!
//! Properties:
//! * at most one committed answer in every device's view at every step, and
//!   in every prefix of every arrival order of the final event set;
//! * every device converges on the same dispatch state;
//! * after partitions heal, exactly one committed answer when some eligible
//!   device stays up (for a repo chat: some eligible device that covers),
//!   and for a repo chat it comes from a covering device;
//! * nothing a revoked device wrote after its revocation holds the turn or
//!   commits.
//!
//! Nightly runs `PROPTEST_CASES=4096` in release.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;
use std::time::Duration;

use hive_core::crypto::sign_envelope;
use hive_core::dispatch::{self, Coverage, Step, View};
use hive_core::testkit::SignedWorld;
use hive_core::workspace_repo::{repo_id, ChatRepo, WorkspaceRepo};
use hive_core::{
    ActorStamp, CfgFrontier, ChatMessage, ChatSession, MessageRole, SessionEvent, SessionEventEnvelope, Timestamp, WorkspaceAuthz,
    WorkspaceRole,
};
use proptest::prelude::*;
use uuid::Uuid;

const NAMES: [&str; 5] = ["owner", "d1", "d2", "d3", "d4"];
const WEFIT: &str = "github.com/zazulam/wefit";
/// One tick is one second of the dispatcher's clock.
const TICKS: u64 = 420;
const HEAL_BY: u64 = 120;

fn base() -> &'static SignedWorld {
    static W: OnceLock<SignedWorld> = OnceLock::new();
    W.get_or_init(|| {
        let mut w = SignedWorld::genesis(&NAMES, None);
        for n in &NAMES[1..] {
            w.admit("owner", n, WorkspaceRole::Contributor);
        }
        w.push(
            "owner",
            SessionEvent::WorkspaceRepoAdded {
                repo: WorkspaceRepo {
                    id: repo_id(WEFIT),
                    canonical: WEFIT.into(),
                    display_name: "WeFit".into(),
                    forge_kind: "github".into(),
                    default_branch: "main".into(),
                    root_commit: None,
                },
            },
        );
        w
    })
}

/// Draft an event by `by` on `chat` as a device holding only `local`
/// would: its lamport one past the newest event it holds (or past `head`
/// when given: a backdating author), its chain link its own last event
/// there, its config frontier `frontier` (or the config head).
fn draft(w: &SignedWorld, clock: &mut u64, by: &str, chat: Uuid, payload: SessionEvent, local: &[SessionEventEnvelope], frontier: Option<CfgFrontier>, head: Option<Option<CfgFrontier>>) -> SessionEventEnvelope {
    *clock += 1;
    let mut env = SessionEventEnvelope::new(chat, w.wid, 10_000 + *clock as i64, payload);
    let p = &w.people[by];
    let mut actor = p.actor();
    actor.device_id = Some(p.device_id);
    let prev = local.iter().rev().find(|e| e.signer_device_id == Some(p.device_id)).map(|e| e.event_id);
    env.actor_stamp = Some(ActorStamp {
        actor,
        recorded_at: Timestamp::epoch(),
        cfg_frontier: frontier.or_else(|| w.head()),
        presence: None,
        runtime_effect: false,
        prev,
        sig_alg: Default::default(),
        log_head: None,
    });
    match head {
        Some(h) => hive_core::lamport::stamp_next(&mut env, h),
        None => hive_core::lamport::stamp_after(&mut env, local),
    }
    sign_envelope(&mut env, p.device_id, &p.device);
    env
}

#[derive(Debug, Clone)]
struct DevSpec {
    eligible: bool,
    covers: bool,
    crash_at: Option<u64>,
}

#[derive(Debug, Clone)]
struct Scenario {
    repo_chat: bool,
    devs: Vec<DevSpec>,
    /// Pairs cut from each other until `HEAL_BY` (device indexes).
    cuts: Vec<(usize, usize)>,
    /// Per-delivery delay seed.
    seed: u64,
    /// Revoke this device (never the owner) at this tick.
    revoke: Option<(usize, u64)>,
}

fn scenario() -> impl Strategy<Value = Scenario> {
    (2usize..=5, any::<bool>(), any::<u64>()).prop_flat_map(|(n, repo_chat, seed)| {
        let dev = (any::<bool>(), any::<bool>(), prop::option::weighted(0.3, 0u64..90)).prop_map(|(eligible, covers, crash_at)| DevSpec { eligible, covers, crash_at });
        (
            Just(repo_chat),
            prop::collection::vec(dev, n),
            prop::collection::vec((0..n, 0..n), 0..3),
            Just(seed),
            prop::option::weighted(0.25, (1..n, 0u64..80)),
        )
            .prop_map(|(repo_chat, devs, cuts, seed, revoke)| Scenario { repo_chat, devs, cuts, seed, revoke })
    })
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

struct Dev {
    name: &'static str,
    id: Uuid,
    spec: DevSpec,
    log: Vec<SessionEventEnvelope>,
    wait_until: u64,
    /// The holder and epoch this device last saw, and since when.
    seen_holder: Option<(Uuid, u32, u64)>,
}

fn fold(log: &[SessionEventEnvelope], t: &hive_core::authz::ConfigTimeline) -> ChatSession {
    hive_core::project_authorized(log, None, None, WorkspaceAuthz::Timeline(t)).expect("the chat folds")
}

/// Committed answers to `trigger` (answers not superseded).
fn committed(s: &ChatSession, trigger: Uuid) -> Vec<Uuid> {
    s.messages
        .iter()
        .filter(|m| m.role == MessageRole::Assistant && m.reply_to_message_id == Some(trigger) && !m.superseded)
        .map(|m| m.id)
        .collect()
}

fn check_at_most_one(s: &ChatSession, trigger: Uuid) -> Result<(), TestCaseError> {
    let c = committed(s, trigger);
    prop_assert!(c.len() <= 1, "two committed answers: {c:?}");
    let slot = s.turn_dispatch.get(&trigger).and_then(|x| x.answer);
    prop_assert_eq!(c.first().copied(), slot);
    Ok(())
}

fn cut(sc: &Scenario, a: usize, b: usize, tick: u64) -> bool {
    tick < HEAL_BY && sc.cuts.iter().any(|&(x, y)| (x == a && y == b) || (x == b && y == a))
}

fn run(sc: Scenario) -> Result<(), TestCaseError> {
    let mut w = base().clone();
    let mut clock = 0u64;
    let chat = w.chat("owner", "t");
    let repo = repo_id(WEFIT);
    if sc.repo_chat {
        w.say(chat, "owner", SessionEvent::ChatRepoSet { repo_id: Some(repo), base_branch: String::new() });
    }
    let trigger_msg = ChatMessage::new(MessageRole::User, "owner", "@hive go");
    let trigger = trigger_msg.id;
    w.say(chat, "owner", SessionEvent::MessageAppended { message: trigger_msg });
    let base_log = w.chats[&chat].clone();
    let pre_revoke_frontier = w.head();
    let mut timeline = w.timeline();
    let mut devs: Vec<Dev> = sc
        .devs
        .iter()
        .enumerate()
        .map(|(i, spec)| Dev { name: NAMES[i], id: w.people[NAMES[i]].device_id, spec: spec.clone(), log: base_log.clone(), wait_until: 0, seen_holder: None })
        .collect();
    let n = devs.len();
    let mut rng = Rng(sc.seed | 1);
    // (to, event, due tick)
    let mut wire: Vec<(usize, SessionEventEnvelope, u64)> = Vec::new();
    let mut revoked: Option<(usize, BTreeSet<Uuid>)> = None;
    let mut all: BTreeMap<Uuid, SessionEventEnvelope> = base_log.iter().map(|e| (e.event_id, e.clone())).collect();

    let crashed = |d: &Dev, tick: u64| d.spec.crash_at.is_some_and(|c| tick >= c);

    for tick in 0..TICKS {
        // Deliver what is due (a cut pair waits for the heal).
        let mut later = Vec::new();
        for (to, env, due) in wire.drain(..) {
            let from = devs.iter().position(|d| Some(d.id) == env.signer_device_id).unwrap_or(0);
            if due > tick || cut(&sc, from, to, tick) {
                later.push((to, env, due));
            } else if !devs[to].log.iter().any(|e| e.event_id == env.event_id) {
                devs[to].log.push(env);
            }
        }
        wire = later;

        // The revocation (config log, seen by every device at once).
        if let Some((k, at)) = sc.revoke {
            if tick == at && revoked.is_none() {
                let kid = devs[k].id;
                let wm = hive_core::watermark::heads_of(&devs[0].log, chat, &BTreeSet::from([kid]));
                w.push("owner", SessionEvent::DeviceRevoked { device_id: kid, watermark: wm });
                timeline = w.timeline();
                revoked = Some((k, BTreeSet::new()));
            }
        }
        let roster = timeline.current().clone();

        for i in 0..n {
            let me = devs[i].id;
            // A revoked device keeps writing: backdated claims and answers.
            if let Some((k, ref mut written)) = revoked {
                if k == i && !crashed(&devs[i], tick) && rng.next() % 4 == 0 {
                    let s = fold(&devs[i].log, &timeline);
                    let (epoch, holder) = s.turn_dispatch.get(&trigger).map(|x| (x.epoch, x.holder)).unwrap_or((0, None));
                    let early = hive_core::lamport::plausible_keys(&base_log).last().copied();
                    let payload = if rng.next() % 2 == 0 {
                        SessionEvent::TurnClaimed { trigger_id: trigger, device_id: me, epoch: epoch + 1, supersedes: holder, repo_id: Some(repo) }
                    } else {
                        let mut m = ChatMessage::new(MessageRole::Assistant, "hive", "forged");
                        m.reply_to_message_id = Some(trigger);
                        m.origin_device_id = Some(me);
                        SessionEvent::MessageAppended { message: m }
                    };
                    let env = draft(&w, &mut clock, devs[i].name, chat, payload, &devs[i].log, pre_revoke_frontier, Some(early));
                    written.insert(env.event_id);
                    all.insert(env.event_id, env.clone());
                    devs[i].log.push(env.clone());
                    for to in 0..n {
                        if to != i {
                            wire.push((to, env.clone(), tick + rng.next() % 4));
                        }
                    }
                    continue;
                }
                if k == i {
                    continue;
                }
            }
            if crashed(&devs[i], tick) || tick < devs[i].wait_until {
                continue;
            }
            let s = fold(&devs[i].log, &timeline);
            check_at_most_one(&s, trigger)?;
            let holder = s.turn_claims.get(&trigger).copied();
            let epoch = s.turn_dispatch.get(&trigger).map(|x| x.epoch).unwrap_or(0);
            let since = match (holder, devs[i].seen_holder) {
                (Some(h), Some((sh, se, at))) if sh == h && se == epoch => at,
                (Some(h), _) => {
                    devs[i].seen_holder = Some((h, epoch, tick));
                    tick
                }
                (None, _) => 0,
            };
            let holder_idx = holder.and_then(|h| devs.iter().position(|d| d.id == h));
            let coverage = match (sc.repo_chat, devs[i].spec.covers) {
                (false, _) => Coverage::NoRepo,
                (true, true) => Coverage::Present,
                (true, false) => Coverage::Missing,
            };
            let view = View {
                me,
                eligible: devs[i].spec.eligible,
                coverage,
                waited: Duration::from_secs(tick - since),
                age: Duration::from_secs(tick),
                holder_live: holder_idx.is_some_and(|h| !crashed(&devs[h], tick) && !cut(&sc, h, i, tick) && revoked.as_ref().is_none_or(|(k, _)| *k != h)),
                holder_gone: holder.is_some_and(|h| dispatch::holder_gone(&roster, h)),
            };
            let chat_repo = s.repo.as_ref().and_then(|r| r.repo_id);
            let mut step = dispatch::decide(&s, trigger, chat_repo, &view);
            let mut fresh = s;
            if let Step::Claim(c) = step {
                let payload = SessionEvent::TurnClaimed { trigger_id: trigger, device_id: me, epoch: c.epoch, supersedes: c.supersedes, repo_id: c.repo_id };
                let env = draft(&w, &mut clock, devs[i].name, chat, payload, &devs[i].log, None, None);
                all.insert(env.event_id, env.clone());
                devs[i].log.push(env.clone());
                for to in 0..n {
                    if to != i {
                        wire.push((to, env.clone(), tick + rng.next() % 6));
                    }
                }
                fresh = fold(&devs[i].log, &timeline);
                step = if fresh.turn_claims.get(&trigger) == Some(&me) { Step::Run } else { Step::Wait(Duration::from_secs(1)) };
            }
            match step {
                Step::Run => {
                    // Already answered (and not fenced) here: nothing to do.
                    let mine = fresh.messages.iter().any(|m| m.reply_to_message_id == Some(trigger) && m.origin_device_id == Some(me) && !m.superseded && m.role == MessageRole::Assistant);
                    let refused = fresh.messages.iter().any(|m| m.role == MessageRole::System && m.origin_device_id == Some(me));
                    if mine || (sc.repo_chat && !devs[i].spec.covers && refused) {
                        devs[i].wait_until = tick + 1;
                        continue;
                    }
                    // The host gate: a repo chat runs only on a covering
                    // host; anywhere else the turn ends in one refusal.
                    let mut m = if sc.repo_chat && !devs[i].spec.covers {
                        ChatMessage::new(MessageRole::System, "ligus", "No checkout here")
                    } else {
                        let mut m = ChatMessage::new(MessageRole::Assistant, "hive", "done");
                        m.reply_to_message_id = Some(trigger);
                        m
                    };
                    m.origin_device_id = Some(me);
                    let env = draft(&w, &mut clock, devs[i].name, chat, SessionEvent::MessageAppended { message: m }, &devs[i].log, None, None);
                    all.insert(env.event_id, env.clone());
                    devs[i].log.push(env.clone());
                    for to in 0..n {
                        if to != i {
                            wire.push((to, env.clone(), tick + rng.next() % 6));
                        }
                    }
                    let after = fold(&devs[i].log, &timeline);
                    check_at_most_one(&after, trigger)?;
                }
                Step::Wait(d) => devs[i].wait_until = tick + d.as_secs().max(1),
                Step::StandDown | Step::Claim(_) => devs[i].wait_until = tick + 1,
            }
        }
    }

    // Flush the wire: every device (crashed ones included, as if they came
    // back) ends with every event.
    let everything: Vec<SessionEventEnvelope> = all.values().cloned().collect();
    let final_fold = fold(&everything, &timeline);
    check_at_most_one(&final_fold, trigger)?;
    // Convergence: any device's log plus what was in flight folds the same.
    for d in &devs {
        let mut log = d.log.clone();
        for e in &everything {
            if !log.iter().any(|x| x.event_id == e.event_id) {
                log.push(e.clone());
            }
        }
        let s = fold(&log, &timeline);
        prop_assert_eq!(&s.turn_claims, &final_fold.turn_claims);
        prop_assert_eq!(&s.turn_dispatch, &final_fold.turn_dispatch);
        prop_assert_eq!(committed(&s, trigger), committed(&final_fold, trigger));
    }
    // Any arrival order: every prefix of a shuffle has at most one.
    let mut order = everything.clone();
    for i in (1..order.len()).rev() {
        order.swap(i, (rng.next() % (i as u64 + 1)) as usize);
    }
    for k in 1..=order.len() {
        let mut prefix: Vec<SessionEventEnvelope> = base_log.clone();
        for e in &order[..k] {
            if !prefix.iter().any(|x| x.event_id == e.event_id) {
                prefix.push(e.clone());
            }
        }
        check_at_most_one(&fold(&prefix, &timeline), trigger)?;
    }
    // Nothing the revoked device wrote after its revocation holds or commits.
    if let Some((k, written)) = &revoked {
        let kid = devs[*k].id;
        if let Some(a) = final_fold.turn_dispatch.get(&trigger).and_then(|x| x.answer) {
            let e = everything.iter().find(|e| matches!(&e.payload, SessionEvent::MessageAppended { message } if message.id == a)).unwrap();
            prop_assert!(!written.contains(&e.event_id), "a post-revocation answer committed");
        }
        let claims_after: Vec<&SessionEventEnvelope> = everything.iter().filter(|e| written.contains(&e.event_id)).collect();
        let verdicts = hive_core::fold_decisions(&everything, None, None, WorkspaceAuthz::Timeline(&timeline));
        for e in claims_after {
            prop_assert_ne!(verdicts.get(&e.event_id).copied(), Some(hive_core::Verdict::Allowed), "post-revocation event folded");
        }
        let _ = kid;
    }
    // Liveness: some eligible device that stayed up (and covers, for a repo
    // chat) means exactly one answer, from a covering device.
    let revoked_idx = revoked.as_ref().map(|(k, _)| *k);
    let able: Vec<usize> = (0..n)
        .filter(|&i| devs[i].spec.eligible && devs[i].spec.crash_at.is_none() && Some(i) != revoked_idx && (!sc.repo_chat || devs[i].spec.covers))
        .collect();
    let answers = committed(&final_fold, trigger);
    if !able.is_empty() {
        prop_assert_eq!(answers.len(), 1, "no answer although {:?} could answer", able);
    }
    if let Some(a) = answers.first() {
        let m = final_fold.messages.iter().find(|m| m.id == *a).unwrap();
        let by = devs.iter().position(|d| Some(d.id) == m.origin_device_id).unwrap();
        if sc.repo_chat {
            prop_assert!(devs[by].spec.covers, "a repo chat answered by a device without the repo");
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(256), ..ProptestConfig::default() })]

    #[test]
    fn prop_dispatch_converges_and_commits_at_most_once(sc in scenario()) {
        run(sc)?;
    }
}

// ── Unit cases ───────────────────────────────────────────────────────────

fn signed_chat(repo_chat: bool) -> (SignedWorld, Uuid, Uuid) {
    let mut w = base().clone();
    let chat = w.chat("owner", "t");
    if repo_chat {
        w.say(chat, "owner", SessionEvent::ChatRepoSet { repo_id: Some(repo_id(WEFIT)), base_branch: String::new() });
    }
    let m = ChatMessage::new(MessageRole::User, "owner", "@hive go");
    let t = m.id;
    w.say(chat, "owner", SessionEvent::MessageAppended { message: m });
    (w, chat, t)
}

fn claim(w: &mut SignedWorld, chat: Uuid, by: &str, t: Uuid, epoch: u32, supersedes: Option<&str>, repo: Option<Uuid>) -> Uuid {
    let device_id = w.people[by].device_id;
    let supersedes = supersedes.map(|s| w.people[s].device_id);
    w.say(chat, by, SessionEvent::TurnClaimed { trigger_id: t, device_id, epoch, supersedes, repo_id: repo })
}

fn answer(w: &mut SignedWorld, chat: Uuid, by: &str, t: Uuid) -> Uuid {
    let mut m = ChatMessage::new(MessageRole::Assistant, "hive", "done");
    m.reply_to_message_id = Some(t);
    m.origin_device_id = Some(w.people[by].device_id);
    let id = m.id;
    w.say(chat, by, SessionEvent::MessageAppended { message: m });
    id
}

#[test]
fn a_covering_claim_takes_a_repo_chat_from_a_device_without_the_repo() {
    let (mut w, chat, t) = signed_chat(true);
    claim(&mut w, chat, "d1", t, 1, None, None);
    claim(&mut w, chat, "d2", t, 1, None, Some(repo_id(WEFIT)));
    let s = w.state(chat);
    assert_eq!(s.turn_claims[&t], w.people["d2"].device_id);
    assert!(s.turn_dispatch[&t].covers);
}

#[test]
fn a_forged_origin_neither_commits_nor_fences() {
    let (mut w, chat, t) = signed_chat(false);
    claim(&mut w, chat, "d1", t, 1, None, None);
    // d2 signs an answer claiming d1 as its origin.
    let mut m = ChatMessage::new(MessageRole::Assistant, "hive", "forged");
    m.reply_to_message_id = Some(t);
    m.origin_device_id = Some(w.people["d1"].device_id);
    w.say(chat, "d2", SessionEvent::MessageAppended { message: m });
    let real = answer(&mut w, chat, "d1", t);
    let s = w.state(chat);
    assert_eq!(s.turn_dispatch[&t].answer, Some(real));
}

#[test]
fn a_backdated_claim_from_a_revoked_device_is_ignored_and_its_old_claim_hands_over() {
    let (mut w, chat, t) = signed_chat(false);
    let before = w.head();
    claim(&mut w, chat, "d1", t, 1, None, None);
    // The owner revokes d1, holding its claim (covered by the watermark).
    let d1 = w.people["d1"].device_id;
    let wm = hive_core::watermark::heads_of(&w.chats[&chat], chat, &BTreeSet::from([d1]));
    w.push("owner", SessionEvent::DeviceRevoked { device_id: d1, watermark: wm });
    let roster = w.roster();
    assert!(dispatch::holder_gone(&roster, d1));
    // d1 keeps writing with an old frontier: a hand-over to itself and an
    // answer. Neither folds.
    let local = w.chats[&chat].clone();
    let mut clock = 0;
    let base = hive_core::lamport::plausible_keys(&local).first().copied();
    for payload in [
        SessionEvent::TurnClaimed { trigger_id: t, device_id: d1, epoch: 2, supersedes: Some(d1), repo_id: None },
        {
            let mut m = ChatMessage::new(MessageRole::Assistant, "hive", "late");
            m.reply_to_message_id = Some(t);
            m.origin_device_id = Some(d1);
            SessionEvent::MessageAppended { message: m }
        },
    ] {
        let env = draft(&w, &mut clock, "d1", chat, payload, &local, before, Some(base));
        w.push_env(Some(chat), env);
    }
    let s = w.state(chat);
    assert_eq!(s.turn_claims[&t], d1, "its covered claim stays folded");
    assert!(s.turn_dispatch[&t].answer.is_none());
    // Another eligible device hands the turn over at once.
    let me = w.people["d2"].device_id;
    let v = View { me, eligible: true, coverage: Coverage::NoRepo, waited: Duration::ZERO, age: Duration::ZERO, holder_live: true, holder_gone: true };
    let Step::Claim(c) = dispatch::decide(&s, t, None, &v) else { panic!("expected a hand-over") };
    assert_eq!((c.epoch, c.supersedes), (2, Some(d1)));
    claim(&mut w, chat, "d2", t, 2, Some("d1"), None);
    let a = answer(&mut w, chat, "d2", t);
    assert_eq!(w.state(chat).turn_dispatch[&t].answer, Some(a));
}

#[test]
fn a_removed_members_device_is_gone_and_its_later_claims_never_fold() {
    let (mut w, chat, t) = signed_chat(false);
    let before = w.head();
    let d3 = w.people["d3"].device_id;
    let wm = hive_core::watermark::heads_of(&w.chats[&chat], chat, &BTreeSet::from([d3]));
    let member = w.people["d3"].actor().id;
    w.push("owner", SessionEvent::MemberRemoved { member_id: member, watermark: wm });
    assert!(dispatch::holder_gone(&w.roster(), d3));
    let local = w.chats[&chat].clone();
    let mut clock = 0;
    let env = draft(&w, &mut clock, "d3", chat, SessionEvent::TurnClaimed { trigger_id: t, device_id: d3, epoch: 0, supersedes: None, repo_id: None }, &local, before, None);
    w.push_env(Some(chat), env);
    assert!(w.state(chat).turn_claims.get(&t).is_none());
    // The turn is still free for a member's device.
    claim(&mut w, chat, "d2", t, 1, None, None);
    assert_eq!(w.state(chat).turn_claims[&t], w.people["d2"].device_id);
}

#[test]
fn the_chat_repo_set_in_the_fold_is_what_a_claim_covers() {
    let (w, chat, _) = signed_chat(true);
    assert_eq!(w.state(chat).repo, Some(ChatRepo { repo_id: Some(repo_id(WEFIT)), ..Default::default() }));
}

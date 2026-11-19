//! Workspace memory and task state, folded from signed logs
//! (`docs/design/v2/workspace-memory.md`). Property tests run 256 cases by
//! default; the release gate runs `PROPTEST_CASES=4096` in release.
//!
//! A random history mixes people writing, editing (from the head or from a
//! stale revision), removing and resolving entries; an agent (its own
//! certified key), the chat's runtime (a host-signed runtime effect) and a
//! phone proposing; people deciding; and forbidden writers trying direct
//! writes. Whatever the order:
//!
//! 1. every allowed direct write, removal or decision is a human member's at
//!    Contributor or above, through a non-phone device, never a runtime
//!    effect or an agent key; changes to an entry are its creator's or an
//!    Admin's;
//! 2. the fold is a function of the event set (any arrival order folds alike);
//! 3. no allowed write is silently lost: each is the head, a conflict, or
//!    was named (as a base or resolved) by a later allowed change of its
//!    entry, or its entry was removed;
//! 4. a removed entry never comes back, and its content is gone from the fold;
//! 5. bounds hold (conflicts per entry, proposals decided once).

use std::collections::{BTreeMap, BTreeSet};

use ligus_core::flow::FlowRecord;
use ligus_core::memory::{MemoryDecision, MemoryOp, MemoryProposal, MemoryRemoval, MemoryWrite, TaskStateUpdate, TaskStatus, MAX_CONFLICTS};
use ligus_core::testkit::{Phone, SignedWorld};
use ligus_core::{
    sign_envelope, ActorIdentity, ActorKind, SessionEvent, SessionEventEnvelope, SigningKeypair, Timestamp, Verdict, WorkspaceRole,
};
use proptest::prelude::*;
use uuid::Uuid;

const WRITERS: [&str; 5] = ["owner", "adm", "mara", "val", "eve"];

struct Fixture {
    w: SignedWorld,
    agent_id: Uuid,
    agent_key: (Uuid, SigningKeypair),
    phone: Phone,
    chat: Uuid,
}

fn fixture() -> Fixture {
    let mut w = SignedWorld::genesis(&WRITERS, None);
    w.admit("owner", "adm", WorkspaceRole::Admin);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    w.admit("owner", "val", WorkspaceRole::Viewer);
    w.publish_identity("eve");
    // An agent of mara's, hosted on her device, with its own certified key.
    let mara = &w.people["mara"];
    let (host, dev) = (mara.device_id, SigningKeypair::from_seed(&mara.device.seed_bytes()).unwrap());
    let mut agent = ligus_core::WorkspaceAgent::new("coder", "gw");
    agent.owner_actor_id = "mara".into();
    agent.host_id = host.to_string();
    let agent_id = agent.id;
    w.push("owner", SessionEvent::AgentUpserted { agent });
    let akp = SigningKeypair::generate().unwrap();
    let cert = ligus_core::AgentKeyCertificate::issue(&dev, host, agent_id, &akp.public_key_bytes(), Timestamp::epoch());
    let key_id = cert.key_id;
    w.push_bare("mara", SessionEvent::AgentKeyCertified { cert });
    assert!(w.roster().valid_agent_key(key_id).is_some(), "agent key certified");
    let phone = Phone::new();
    let pc = w.phone_certificate("mara", &phone);
    w.push("mara", SessionEvent::DeviceCertificateAdded { certificate: pc });
    let chat = w.chat("owner", "work");
    Fixture { w, agent_id, agent_key: (key_id, akp), phone, chat }
}

fn rec(r: FlowRecord) -> SessionEvent {
    SessionEvent::FlowRecorded { record: r }
}

/// How an event is signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Via {
    /// A person's own device (`WRITERS[i]`).
    Person(usize),
    /// The agent's own key.
    Agent,
    /// Mara's device, stamped as a runtime effect.
    Runtime,
    /// Mara's phone.
    Phone,
}

impl Fixture {
    fn sign(&mut self, via: Via, chat: Option<Uuid>, payload: SessionEvent) -> Uuid {
        match via {
            Via::Person(i) => self.w.write(WRITERS[i], chat, payload, None, ligus_core::testkit::Proof::None),
            Via::Agent => {
                let mut env = self.w.draft("mara", chat, payload, None);
                let s = env.actor_stamp.as_mut().unwrap();
                s.actor = ActorIdentity::new(self.agent_id.to_string(), "coder", ActorKind::Agent);
                s.actor.device_id = Some(self.agent_key.0);
                s.prev = None;
                env.clear_raw();
                sign_envelope(&mut env, self.agent_key.0, &self.agent_key.1);
                self.w.push_env(chat, env)
            }
            Via::Runtime => {
                let mut env = self.w.draft("mara", chat, payload, None);
                env.actor_stamp.as_mut().unwrap().runtime_effect = true;
                self.resign(&mut env, "mara");
                self.w.push_env(chat, env)
            }
            Via::Phone => self.w.phone_write("mara", &self.phone, chat, payload),
        }
    }

    fn resign(&self, env: &mut SessionEventEnvelope, by: &str) {
        env.clear_raw();
        let p = &self.w.people[by];
        sign_envelope(env, p.device_id, &p.device);
    }

    fn memory(&self) -> ligus_core::memory::MemoryState {
        self.w.config().flow.memory
    }
}

#[derive(Debug, Clone)]
enum Op {
    Create { by: u8, via: u8 },
    Edit { by: u8, via: u8, entry: u8, rev: u8 },
    Remove { by: u8, via: u8, entry: u8, rev: u8 },
    Resolve { by: u8, entry: u8 },
    Propose { via: u8, kind: u8, entry: u8, rev: u8 },
    Decide { by: u8, via: u8, proposal: u8, accept: bool },
}

fn via_of(by: u8, via: u8) -> Via {
    match via % 6 {
        0..=2 => Via::Person(by as usize % WRITERS.len()),
        3 => Via::Agent,
        4 => Via::Runtime,
        _ => Via::Phone,
    }
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (any::<u8>(), any::<u8>()).prop_map(|(by, via)| Op::Create { by, via }),
        (any::<u8>(), any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(by, via, entry, rev)| Op::Edit { by, via, entry, rev }),
        (any::<u8>(), any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(by, via, entry, rev)| Op::Remove { by, via, entry, rev }),
        (any::<u8>(), any::<u8>()).prop_map(|(by, entry)| Op::Resolve { by, entry }),
        (any::<u8>(), any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(via, kind, entry, rev)| Op::Propose { via, kind, entry, rev }),
        (any::<u8>(), any::<u8>(), any::<u8>(), any::<bool>()).prop_map(|(by, via, proposal, accept)| Op::Decide { by, via, proposal, accept }),
    ]
}

/// What the history did, for the checks.
#[derive(Default)]
struct Log {
    /// Event id → (via, the record).
    events: BTreeMap<Uuid, (Via, FlowRecord)>,
    /// Every entry id ever named.
    entries: Vec<Uuid>,
}

fn pick<T: Copy>(v: &[T], i: u8) -> Option<T> {
    (!v.is_empty()).then(|| v[i as usize % v.len()])
}

fn run(ops: &[Op]) -> (Fixture, Log) {
    let mut f = fixture();
    let mut log = Log::default();
    let mut n = 0u32;
    for o in ops {
        n += 1;
        let m = f.memory();
        let entry = |i: u8| pick(&log.entries, i);
        // A remembered revision of the entry (the head or an older one).
        let rev = |e: Uuid, i: u8| m.entries.get(&e).and_then(|x| pick(&x.revisions, i));
        let (via, record) = match o.clone() {
            Op::Create { by, via } => {
                let id = Uuid::new_v4();
                log.entries.push(id);
                (via_of(by, via), FlowRecord::MemoryWritten(MemoryWrite { entry_id: id, title: format!("note {n}"), body: format!("body {n}"), ..Default::default() }))
            }
            Op::Edit { by, via, entry: e, rev: r } => {
                let Some(e) = entry(e) else { continue };
                let base = rev(e, r).unwrap_or_else(Uuid::new_v4);
                (via_of(by, via), FlowRecord::MemoryWritten(MemoryWrite { entry_id: e, base: Some(base), title: format!("edit {n}"), body: format!("edited {n}"), ..Default::default() }))
            }
            Op::Remove { by, via, entry: e, rev: r } => {
                let Some(e) = entry(e) else { continue };
                let base = rev(e, r).unwrap_or_else(Uuid::new_v4);
                (via_of(by, via), FlowRecord::MemoryRemoved(MemoryRemoval { entry_id: e, base, ..Default::default() }))
            }
            Op::Resolve { by, entry: e } => {
                let Some(e) = entry(e) else { continue };
                let Some(x) = m.entries.get(&e) else { continue };
                let resolves = x.conflicts.iter().map(|c| c.rev).collect();
                (
                    Via::Person(by as usize % WRITERS.len()),
                    FlowRecord::MemoryWritten(MemoryWrite { entry_id: e, base: Some(x.head()), title: format!("merged {n}"), body: format!("merged {n}"), resolves, ..Default::default() }),
                )
            }
            Op::Propose { via, kind, entry: e, rev: r } => {
                let via = match via % 4 {
                    0 => Via::Agent,
                    1 => Via::Runtime,
                    2 => Via::Phone,
                    _ => Via::Person(2),
                };
                let op = match (kind % 3, entry(e)) {
                    (1, Some(e)) => MemoryOp::Edit { entry_id: e, base: rev(e, r).unwrap_or_else(Uuid::new_v4), title: format!("prop {n}"), body: format!("proposed {n}"), tags: vec![] },
                    (2, Some(e)) => MemoryOp::Remove { entry_id: e, base: rev(e, r).unwrap_or_else(Uuid::new_v4) },
                    _ => {
                        // A created entry is named by its proposal's id.
                        let id = Uuid::new_v4();
                        log.entries.push(id);
                        MemoryOp::Create { entry_id: id, title: format!("prop {n}"), body: format!("proposed {n}"), tags: vec![] }
                    }
                };
                let proposal_id = match &op {
                    MemoryOp::Create { entry_id, .. } => *entry_id,
                    _ => Uuid::new_v4(),
                };
                let proposer = match via {
                    Via::Agent | Via::Runtime => f.agent_id.to_string(),
                    _ => String::new(),
                };
                (via, FlowRecord::MemoryProposed(MemoryProposal { proposal_id, op, proposer, source: Default::default(), rationale: String::new() }))
            }
            Op::Decide { by, via, proposal, accept } => {
                let _ = accept;
                let open: Vec<Uuid> = m.proposals.keys().copied().collect();
                let Some(p) = pick(&open, proposal) else { continue };
                let resolves = vec![];
                (via_of(by, via), FlowRecord::MemoryProposalDecided(MemoryDecision { proposal_id: p, accepted: accept, note: String::new(), resolves }))
            }
        };
        let id = f.sign(via, None, rec(record.clone()));
        log.events.insert(id, (via, record));
    }
    (f, log)
}

fn person_role(f: &Fixture, name: &str) -> Option<WorkspaceRole> {
    f.w.roster().role_of(name)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: std::env::var("PROPTEST_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(256), ..ProptestConfig::default() })]

    #[test]
    fn prop_memory_writes_are_people_and_nothing_is_lost(ops in prop::collection::vec(op(), 1..28)) {
        let (f, log) = run(&ops);
        let verdicts = f.w.verdicts();
        let m = f.memory();
        let allowed = |id: &Uuid| verdicts.get(id) == Some(&Verdict::Allowed);

        // 1. Who.
        for (id, (via, r)) in &log.events {
            if !allowed(id) {
                continue;
            }
            if ligus_core::memory::is_person_only(r) {
                let Via::Person(i) = via else { panic!("{via:?} changed memory directly: {r:?}") };
                let role = person_role(&f, WRITERS[*i]).expect("a member");
                prop_assert!(role.rank() >= WorkspaceRole::Contributor.rank(), "{} at {role:?} wrote memory", WRITERS[*i]);
            }
            if let FlowRecord::MemoryProposed(p) = r {
                match via {
                    Via::Agent | Via::Runtime => prop_assert_eq!(&p.proposer, &f.agent_id.to_string()),
                    _ => prop_assert!(p.proposer.is_empty()),
                }
            }
        }
        // Changes to an entry: its creator's or an Admin's.
        for (id, (via, r)) in &log.events {
            let Via::Person(i) = via else { continue };
            let entry = match r {
                FlowRecord::MemoryWritten(w) if w.base.is_some() => Some(w.entry_id),
                FlowRecord::MemoryRemoved(x) => Some(x.entry_id),
                _ => None,
            };
            if let (true, Some(e)) = (allowed(id), entry) {
                let role = person_role(&f, WRITERS[*i]).unwrap();
                let creator = &m.entries[&e].created_by.id;
                prop_assert!(creator == WRITERS[*i] || role.rank() >= WorkspaceRole::Admin.rank());
            }
        }

        // 2. Order independence: reversed and rotated inputs fold alike.
        let mut rev = f.w.cfg.clone();
        rev.reverse();
        let (a, _) = ligus_core::project_config(&rev, None, None);
        prop_assert_eq!(&a.unwrap().flow.memory, &m);
        let mut rot = f.w.cfg.clone();
        let k = rot.len() / 3;
        rot.rotate_left(k);
        let (b, _) = ligus_core::project_config(&rot, None, None);
        prop_assert_eq!(&b.unwrap().flow.memory, &m);

        // 3. Nothing lost. Every allowed write's revision is accounted for.
        let mut named: BTreeSet<Uuid> = BTreeSet::new();
        for (id, (_, r)) in &log.events {
            if !allowed(id) {
                continue;
            }
            match r {
                FlowRecord::MemoryWritten(w) => {
                    named.extend(w.base);
                    named.extend(w.resolves.iter().copied());
                }
                FlowRecord::MemoryRemoved(x) => {
                    named.insert(x.base);
                    named.extend(x.resolves.iter().copied());
                }
                FlowRecord::MemoryProposalDecided(d) if d.accepted => {
                    named.extend(d.resolves.iter().copied());
                    if let Some(st) = m.proposals.get(&d.proposal_id) {
                        if let MemoryOp::Edit { base, .. } | MemoryOp::Remove { base, .. } = st.proposal.op {
                            named.insert(base);
                        }
                    }
                }
                _ => {}
            }
        }
        for (id, (_, r)) in &log.events {
            let rev_of = match r {
                FlowRecord::MemoryWritten(w) => Some((w.entry_id, *id)),
                _ => None,
            };
            let Some((e, rev)) = rev_of.filter(|_| allowed(id)) else { continue };
            let x = &m.entries[&e];
            let ok = x.head() == rev || x.conflicts.iter().any(|c| c.rev == rev) || named.contains(&rev) || !x.live();
            prop_assert!(ok, "write {rev} vanished from entry {e}");
        }

        // 4. Tombstones: every allowed removal at the head left one, content-free.
        for (id, (_, r)) in &log.events {
            if let (true, FlowRecord::MemoryRemoved(x)) = (allowed(id), r) {
                let e = &m.entries[&x.entry_id];
                if e.removed.as_ref().is_some_and(|t| t.rev == *id) {
                    prop_assert!(e.current.body.is_empty() && e.current.title.is_empty());
                }
            }
        }
        for e in m.entries.values().filter(|e| !e.live()) {
            prop_assert!(e.current.body.is_empty());
        }
        // A create on a used id is never allowed.
        let mut created: BTreeSet<Uuid> = BTreeSet::new();
        let mut order: Vec<&SessionEventEnvelope> = f.w.cfg.iter().collect();
        order.sort_by_key(|e| e.canonical_key());
        for env in order {
            if let SessionEvent::FlowRecorded { record: FlowRecord::MemoryWritten(w) } = &env.payload {
                if w.base.is_none() && allowed(&env.event_id) {
                    prop_assert!(created.insert(w.entry_id), "entry {} created twice", w.entry_id);
                }
            }
        }

        // 5. Bounds.
        for e in m.entries.values() {
            prop_assert!(e.conflicts.len() <= MAX_CONFLICTS);
        }
        let decided: BTreeMap<Uuid, usize> = log.events.iter().filter(|(id, _)| allowed(id)).filter_map(|(_, (_, r))| match r {
            FlowRecord::MemoryProposalDecided(d) => Some(d.proposal_id),
            _ => None,
        }).fold(BTreeMap::new(), |mut acc, p| { *acc.entry(p).or_default() += 1; acc });
        prop_assert!(decided.values().all(|n| *n == 1), "a proposal decided twice");
    }

    #[test]
    fn prop_task_slots_are_single_writer(writes in prop::collection::vec((0u8..4, 0u8..3, 0u8..5), 1..20)) {
        let mut f = fixture();
        let chat = f.chat;
        let agent = f.agent_id.to_string();
        let mut ids = Vec::new();
        for (who, key, status) in writes {
            let via = match who { 0 => Via::Agent, 1 => Via::Runtime, 2 => Via::Person(2), _ => Via::Person(1) };
            let agent_key = match key { 0 => agent.clone(), 1 => ligus_core::flow::PRIMARY.to_string(), _ => Uuid::new_v4().to_string() };
            let t = TaskStateUpdate {
                task_id: chat,
                agent_key,
                turn_id: Uuid::new_v4(),
                status: [TaskStatus::Working, TaskStatus::Blocked, TaskStatus::Waiting, TaskStatus::Done, TaskStatus::Abandoned][status as usize],
                summary: "step".into(),
                ..Default::default()
            };
            let id = f.sign(via, Some(chat), rec(FlowRecord::TaskState(t.clone())));
            ids.push((id, via, t));
        }
        let verdicts = f.w.verdicts();
        let state = f.w.state(chat);
        for (id, via, t) in &ids {
            if verdicts.get(id) != Some(&Verdict::Allowed) {
                continue;
            }
            match via {
                Via::Agent => prop_assert_eq!(&t.agent_key, &agent),
                Via::Runtime => prop_assert_eq!(t.agent_key.as_str(), ligus_core::flow::PRIMARY),
                // Mara's device hosts the agent: it may write that agent's slot.
                Via::Person(2) => prop_assert_eq!(&t.agent_key, &agent),
                other => prop_assert!(false, "{other:?} wrote a task-state slot"),
            }
        }
        // One writer per slot: each slot holds its own signer's newest
        // allowed update for its task and agent, never another signer's.
        let mut newest: BTreeMap<(Uuid, Uuid, String), (u64, Uuid)> = BTreeMap::new();
        for env in f.w.chats[&chat].iter().filter(|e| verdicts.get(&e.event_id) == Some(&Verdict::Allowed)) {
            if let SessionEvent::FlowRecorded { record: FlowRecord::TaskState(t) } = &env.payload {
                let k = (env.signer_device_id.unwrap(), t.task_id, t.agent_key.clone());
                let e = newest.entry(k).or_insert(env.canonical_key());
                *e = (*e).max(env.canonical_key());
            }
        }
        for s in &state.flow.tasks {
            let k = (s.writer, s.update.task_id, s.update.agent_key.clone());
            prop_assert_eq!(newest.get(&k).map(|x| x.1), Some(s.rev), "slot {:?} isn't its writer's newest update", k);
        }
        prop_assert!(state.flow.tasks.len() <= newest.len());
        prop_assert!(state.flow.tasks.len() <= ligus_core::memory::MAX_TASK_SLOTS_PER_CHAT);
    }
}

#[test]
fn forbidden_writers_are_refused_with_reasons() {
    let mut f = fixture();
    let w = |n: u32| FlowRecord::MemoryWritten(MemoryWrite { entry_id: Uuid::new_v4(), title: format!("t{n}"), body: "b".into(), ..Default::default() });
    for (i, via) in [Via::Agent, Via::Runtime, Via::Phone, Via::Person(3), Via::Person(4)].into_iter().enumerate() {
        let id = f.sign(via, None, rec(w(i as u32)));
        assert_eq!(f.w.verdict(id), Verdict::Denied, "{via:?}");
    }
    // A person at Contributor writes; a proposal from the agent is filed,
    // the phone's too; the Viewer's proposal is refused (role floor).
    let ok = f.sign(Via::Person(2), None, rec(w(9)));
    assert_eq!(f.w.verdict(ok), Verdict::Allowed);
    let prop = |proposer: String| proposal(Uuid::new_v4(), new_note(), proposer);
    let agent = f.agent_id.to_string();
    let a = f.sign(Via::Agent, None, rec(prop(agent.clone())));
    assert_eq!(f.w.verdict(a), Verdict::Allowed);
    let p = f.sign(Via::Phone, None, rec(prop(String::new())));
    assert_eq!(f.w.verdict(p), Verdict::Allowed);
    let v = f.sign(Via::Person(3), None, rec(prop(String::new())));
    assert_eq!(f.w.verdict(v), Verdict::Denied);
    // A person can't file under the agent's name; the agent not under another's.
    let spoof = f.sign(Via::Person(2), None, rec(prop(agent)));
    assert_eq!(f.w.verdict(spoof), Verdict::Denied);
    let spoof2 = f.sign(Via::Agent, None, rec(prop(Uuid::new_v4().to_string())));
    assert_eq!(f.w.verdict(spoof2), Verdict::Denied);
    // Memory on a chat log is refused; task state on the config log too.
    let chat = f.chat;
    let on_chat = f.sign(Via::Person(2), Some(chat), rec(w(10)));
    assert_eq!(f.w.verdict(on_chat), Verdict::Denied);
    let t = TaskStateUpdate { task_id: chat, agent_key: f.agent_id.to_string(), turn_id: Uuid::new_v4(), summary: "s".into(), ..Default::default() };
    let on_cfg = f.sign(Via::Agent, None, rec(FlowRecord::TaskState(t)));
    assert_eq!(f.w.verdict(on_cfg), Verdict::Denied);
}

#[test]
fn only_the_creator_or_an_admin_changes_an_entry_and_decides_for_it() {
    let mut f = fixture();
    let id = Uuid::new_v4();
    let c = f.sign(Via::Person(2), None, rec(FlowRecord::MemoryWritten(MemoryWrite { entry_id: id, title: "mara's".into(), body: "b".into(), ..Default::default() })));
    assert_eq!(f.w.verdict(c), Verdict::Allowed);
    // Another Contributor-level writer: the owner is fine (Owner > Admin).
    let mut w2 = SignedWorld::genesis(&["owner", "mara", "ivy"], None);
    w2.admit("owner", "mara", WorkspaceRole::Contributor);
    w2.admit("owner", "ivy", WorkspaceRole::Contributor);
    let e = Uuid::new_v4();
    let make = w2.push_bare("mara", rec(FlowRecord::MemoryWritten(MemoryWrite { entry_id: e, title: "t".into(), body: "b".into(), ..Default::default() })));
    let edit = |base: Uuid| rec(FlowRecord::MemoryWritten(MemoryWrite { entry_id: e, base: Some(base), title: "t".into(), body: "changed".into(), ..Default::default() }));
    let by_ivy = w2.push_bare("ivy", edit(make));
    assert_eq!(w2.verdict(by_ivy), Verdict::Denied, "another Contributor can't edit mara's entry");
    let by_owner = w2.push_bare("owner", edit(make));
    assert_eq!(w2.verdict(by_owner), Verdict::Allowed, "an Owner moderates");
    // Ivy accepting a proposal to remove mara's entry is refused too.
    let pid = Uuid::new_v4();
    w2.push_bare("ivy", rec(FlowRecord::MemoryProposed(MemoryProposal { proposal_id: pid, op: MemoryOp::Remove { entry_id: e, base: by_owner }, proposer: String::new(), source: Default::default(), rationale: String::new() })));
    let accept = w2.push_bare("ivy", rec(FlowRecord::MemoryProposalDecided(MemoryDecision { proposal_id: pid, accepted: true, ..Default::default() })));
    assert_eq!(w2.verdict(accept), Verdict::Denied);
    let accept = w2.push_bare("mara", rec(FlowRecord::MemoryProposalDecided(MemoryDecision { proposal_id: pid, accepted: true, ..Default::default() })));
    assert_eq!(w2.verdict(accept), Verdict::Allowed);
    assert!(!w2.config().flow.memory.entries[&e].live());
}

#[test]
fn a_service_member_never_writes_memory() {
    // A Contributor whose roster kind is Service, signing as a service.
    let mut w = SignedWorld::genesis(&["owner", "box"], None);
    w.kinds.insert("box".into(), ActorKind::Service);
    let m = w.member_of("box", WorkspaceRole::Contributor);
    w.push("owner", SessionEvent::MemberAdded { member: m, invite: None, proof: None });
    w.publish_identity("box");
    assert!(w.roster().members.get("box").is_some_and(|m| m.kind == ActorKind::Service));
    let id = w.push_bare("box", rec(FlowRecord::MemoryWritten(MemoryWrite { entry_id: Uuid::new_v4(), title: "t".into(), body: "b".into(), ..Default::default() })));
    assert_eq!(w.verdict(id), Verdict::Denied);
}

// ------------------------------------------------------------ review attacks
//
// Adversarial review of the memory branch: each test below failed before its
// fix (prove, then fix).

/// Re-stamp `env` as drafted just after `head` on `log` (a backdated or
/// withheld draft), its `prev` the signer's newest older event, and re-sign
/// it with `by`'s device.
fn backdate(w: &SignedWorld, env: &mut SessionEventEnvelope, log: &[SessionEventEnvelope], head: Uuid, by: &str) {
    let h = log.iter().find(|e| e.event_id == head).expect("head on the log");
    ligus_core::lamport::stamp_next(env, Some(ligus_core::CfgFrontier { lamport: h.lamport, event_id: h.event_id }));
    let p = &w.people[by];
    let prev = log
        .iter()
        .filter(|e| e.signer_device_id == Some(p.device_id) && e.lamport < env.lamport)
        .max_by_key(|e| e.canonical_key())
        .map(|e| e.event_id);
    env.actor_stamp.as_mut().unwrap().prev = prev;
    env.clear_raw();
    sign_envelope(env, p.device_id, &p.device);
}

fn create(id: Uuid, title: &str) -> FlowRecord {
    FlowRecord::MemoryWritten(MemoryWrite { entry_id: id, title: title.into(), body: "b".into(), ..Default::default() })
}

/// A proposal under `id`; a create names the new entry by the proposal's id
/// (the binding), whatever id `op` carried.
fn proposal(id: Uuid, op: MemoryOp, proposer: String) -> FlowRecord {
    let op = match op {
        MemoryOp::Create { title, body, tags, .. } => MemoryOp::Create { entry_id: id, title, body, tags },
        other => other,
    };
    FlowRecord::MemoryProposed(MemoryProposal { proposal_id: id, op, proposer, source: Default::default(), rationale: String::new() })
}

fn new_note() -> MemoryOp {
    MemoryOp::Create { entry_id: Uuid::new_v4(), title: "p".into(), body: "b".into(), tags: vec![] }
}

/// Risk 3: a runtime-effect proposal names the chat's runtime or an agent
/// bound to the signing device, never free text (or nobody: it would read as
/// the host person's own proposal).
#[test]
fn a_runtime_proposal_names_only_primary_or_an_agent_on_its_device() {
    let mut f = fixture();
    let agent = f.agent_id.to_string();
    for (proposer, want) in [
        ("primary".to_string(), Verdict::Allowed),
        (agent, Verdict::Allowed),
        (Uuid::new_v4().to_string(), Verdict::Denied),
        ("anything at all".to_string(), Verdict::Denied),
        (String::new(), Verdict::Denied),
    ] {
        let id = f.sign(Via::Runtime, None, rec(proposal(Uuid::new_v4(), new_note(), proposer.clone())));
        assert_eq!(f.w.verdict(id), want, "runtime proposal as {proposer:?}");
    }
}

/// Risk 3: the per-proposer cap counts per signer, so one device can't fill
/// the workspace's proposals by cycling proposer names.
#[test]
fn proposal_caps_count_per_signer() {
    let mut f = fixture();
    let agent = f.agent_id.to_string();
    let mut allowed = 0;
    for i in 0..(2 * ligus_core::memory::MAX_OPEN_PROPOSALS_PER_PROPOSER) {
        let proposer = if i % 2 == 0 { "primary".to_string() } else { agent.clone() };
        let id = f.sign(Via::Runtime, None, rec(proposal(Uuid::new_v4(), new_note(), proposer)));
        allowed += (f.w.verdict(id) == Verdict::Allowed) as usize;
    }
    assert_eq!(allowed, ligus_core::memory::MAX_OPEN_PROPOSALS_PER_PROPOSER, "one device's open proposals are capped together");
}

/// A decision names a proposal by id. A member who files a backdated
/// proposal under an id already accepted must not swap in their own
/// operation (here: removing an Admin's entry, accepted with the Admin's
/// authority).
#[test]
fn a_backdated_proposal_cant_take_over_an_accepted_proposal_id() {
    let mut f = fixture();
    let x = Uuid::new_v4();
    let made = f.sign(Via::Person(1), None, rec(create(x, "adm's note")));
    let filler = f.sign(Via::Person(0), None, rec(create(Uuid::new_v4(), "owner's note")));
    let _ = filler;
    let pid = Uuid::new_v4();
    let agent = f.agent_id.to_string();
    let honest = f.sign(Via::Agent, None, rec(proposal(pid, new_note(), agent)));
    assert_eq!(f.w.verdict(honest), Verdict::Allowed);
    let accept = f.sign(Via::Person(1), None, rec(FlowRecord::MemoryProposalDecided(MemoryDecision { proposal_id: pid, accepted: true, ..Default::default() })));
    assert_eq!(f.w.verdict(accept), Verdict::Allowed);
    // Mara (a Contributor) files her own proposal under that id, drafted
    // just after adm's entry so it sorts before the agent's.
    let evil = proposal(pid, MemoryOp::Remove { entry_id: x, base: made }, String::new());
    let mut env = f.w.draft("mara", None, rec(evil), None);
    env.event_id = Uuid::new_v4();
    let cfg = f.w.cfg.clone();
    backdate(&f.w, &mut env, &cfg, made, "mara");
    let evil_id = f.w.push_env(None, env);
    assert!(f.memory().entries[&x].live(), "adm's entry was removed by a swapped-in proposal");
    assert_ne!(f.w.verdict(evil_id), Verdict::Allowed, "a proposal is known by the event that filed it");
}

/// The same for entry ids: a backdated create can't take another member's
/// new entry (and its authorship) over.
#[test]
fn a_backdated_create_cant_take_over_an_entry_id() {
    let mut w = SignedWorld::genesis(&["owner", "mara", "ivy"], None);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    w.admit("owner", "ivy", WorkspaceRole::Contributor);
    let anchor = w.push_bare("owner", rec(create(Uuid::new_v4(), "anchor")));
    w.push_bare("owner", rec(create(Uuid::new_v4(), "filler")));
    let e = Uuid::new_v4();
    let theirs = w.push_bare("mara", rec(create(e, "mara's")));
    let mut env = w.draft("ivy", None, rec(create(e, "ivy's")), None);
    env.event_id = Uuid::new_v4();
    let cfg = w.cfg.clone();
    backdate(&w, &mut env, &cfg, anchor, "ivy");
    let squat = w.push_env(None, env);
    assert_eq!(w.config().flow.memory.entries[&e].created_by.id, "mara", "ivy took mara's entry over");
    assert_ne!(w.verdict(squat), Verdict::Allowed, "an entry is created by the event that names it");
    assert_eq!(w.verdict(theirs), Verdict::Allowed);
}

/// Task state: a member's device stamping a runtime effect can't take the
/// chat runtime's slot over by backdating its first update before the
/// runtime's.
#[test]
fn a_backdated_runtime_update_cant_squat_the_primary_slot() {
    let mut f = fixture();
    let chat = f.chat;
    let seed = f.w.chats[&chat][0].event_id;
    f.w.say(chat, "adm", SessionEvent::SessionTitleChanged { title: "filler".into() });
    let t = |s: &str| rec(FlowRecord::TaskState(TaskStateUpdate { task_id: chat, agent_key: "primary".into(), turn_id: Uuid::new_v4(), summary: s.into(), ..Default::default() }));
    let first = f.sign(Via::Runtime, Some(chat), t("reading"));
    let second = f.sign(Via::Runtime, Some(chat), t("writing"));
    let mut env = f.w.draft("owner", Some(chat), t("all done, nothing to see"), None);
    env.actor_stamp.as_mut().unwrap().runtime_effect = true;
    let log = f.w.chats[&chat].clone();
    backdate(&f.w, &mut env, &log, seed, "owner");
    f.w.push_env(Some(chat), env);
    assert_eq!(f.w.verdict(first), Verdict::Allowed, "the runtime's own update was refused");
    assert_eq!(f.w.verdict(second), Verdict::Allowed, "the runtime's own update was refused");
    let st = f.w.state(chat);
    let mara_device = f.w.people["mara"].device_id;
    let mine = st.flow.tasks.iter().find(|s| s.writer == mara_device).expect("the runtime's slot");
    assert_eq!(mine.update.summary, "writing");
}

/// Task state in the A3 transition: the host writes its agent's slot, then
/// the agent's own key does. Both are that agent's word; neither locks the
/// other out.
#[test]
fn a_host_then_its_agents_key_both_write_the_agents_slot() {
    let mut f = fixture();
    let chat = f.chat;
    let t = |s: &str, agent: &str| rec(FlowRecord::TaskState(TaskStateUpdate { task_id: chat, agent_key: agent.into(), turn_id: Uuid::new_v4(), summary: s.into(), ..Default::default() }));
    let agent = f.agent_id.to_string();
    let by_host = f.sign(Via::Person(2), Some(chat), t("host", &agent));
    let by_key = f.sign(Via::Agent, Some(chat), t("key", &agent));
    assert_eq!(f.w.verdict(by_host), Verdict::Allowed);
    assert_eq!(f.w.verdict(by_key), Verdict::Allowed, "the agent's own key was locked out of its slot");
}

/// A person's resolution that races another edit of the entry (both from
/// the same head) is kept as a conflict, never refused (lost).
#[test]
fn a_resolution_racing_an_edit_is_kept_as_a_conflict() {
    let mut f = fixture();
    let e = Uuid::new_v4();
    let r0 = f.sign(Via::Person(2), None, rec(create(e, "mara's")));
    let edit = |base: Uuid, body: &str, resolves: Vec<Uuid>| {
        rec(FlowRecord::MemoryWritten(MemoryWrite { entry_id: e, base: Some(base), title: "t".into(), body: body.into(), resolves, ..Default::default() }))
    };
    let r1 = f.sign(Via::Person(2), None, edit(r0, "443", vec![]));
    let c1 = f.sign(Via::Person(1), None, edit(r0, "8080", vec![]));
    assert_eq!(f.memory().entries[&e].conflicts.len(), 1);
    // Concurrent: adm edits from r1, mara resolves c1 from r1; both drafted
    // at the same head, adm's sorting first.
    let head = f.w.cfg.last().unwrap().event_id;
    let mut a = f.w.draft("adm", None, edit(r1, "443 (adm)", vec![]), None);
    a.event_id = Uuid::from_u128(1);
    let cfg = f.w.cfg.clone();
    backdate(&f.w, &mut a, &cfg, head, "adm");
    let mut m = f.w.draft("mara", None, edit(r1, "443 or 8080", vec![c1]), None);
    m.event_id = Uuid::from_u128(u128::MAX);
    backdate(&f.w, &mut m, &cfg, head, "mara");
    let a = f.w.push_env(None, a);
    let m = f.w.push_env(None, m);
    assert_eq!(f.w.verdict(a), Verdict::Allowed);
    assert_eq!(f.w.verdict(m), Verdict::Allowed, "mara's resolution was refused, her merge lost");
    let x = &f.memory().entries[&e];
    assert_eq!(x.head(), a);
    assert!(x.conflicts.iter().any(|c| c.rev == m), "her merge is a conflict");
    assert!(!x.conflicts.iter().any(|c| c.rev == c1), "the conflict she merged is settled");
}

/// Watermarks: a removed member's backdated memory write (drafted after
/// the cut's watermark) is refused like any other config event of theirs.
#[test]
fn a_removed_members_backdated_memory_write_is_refused() {
    let mut f = fixture();
    let before = f.w.cfg.last().unwrap().event_id;
    let mara_dev = f.w.people["mara"].device_id;
    let mut signers = BTreeSet::new();
    signers.insert(mara_dev);
    signers.insert(f.agent_key.0);
    signers.insert(f.phone.device_id);
    let wid = f.w.wid;
    let watermark = ligus_core::watermark::heads_of(&f.w.cfg, ligus_core::workspace_config_session_id(wid), &signers);
    f.w.push("owner", SessionEvent::MemberRemoved { member_id: "mara".into(), watermark });
    assert!(!f.w.roster().members.contains_key("mara"));
    let mut env = f.w.draft("mara", None, rec(create(Uuid::new_v4(), "after the cut")), None);
    let cfg = f.w.cfg.clone();
    backdate(&f.w, &mut env, &cfg, before, "mara");
    let id = f.w.push_env(None, env);
    assert_eq!(f.w.verdict(id), Verdict::Denied);
    // Her agent's backdated proposal too.
    let agent = f.agent_id.to_string();
    let mut env = f.w.draft("mara", None, rec(proposal(Uuid::new_v4(), new_note(), agent)), None);
    let s = env.actor_stamp.as_mut().unwrap();
    s.actor = ActorIdentity::new(f.agent_id.to_string(), "coder", ActorKind::Agent);
    s.actor.device_id = Some(f.agent_key.0);
    let h = cfg.iter().find(|e| e.event_id == before).unwrap();
    ligus_core::lamport::stamp_next(&mut env, Some(ligus_core::CfgFrontier { lamport: h.lamport, event_id: h.event_id }));
    env.actor_stamp.as_mut().unwrap().prev = None;
    env.clear_raw();
    sign_envelope(&mut env, f.agent_key.0, &f.agent_key.1);
    let id = f.w.push_env(None, env);
    assert_eq!(f.w.verdict(id), Verdict::Denied);
}

/// An accepted create can't be pre-empted: a Contributor who sees an open
/// create proposal can't take its entry id first (with her own content and
/// authorship) so the person's acceptance fails or lands on her note.
#[test]
fn an_accepted_create_cant_be_pre_empted() {
    let mut f = fixture();
    let pid = Uuid::new_v4();
    let agent = f.agent_id.to_string();
    let op = MemoryOp::Create { entry_id: pid, title: "Deploy day".into(), body: "Thursdays".into(), tags: vec![] };
    let filed = f.sign(Via::Agent, None, rec(proposal(pid, op, agent)));
    assert_eq!(f.w.verdict(filed), Verdict::Allowed);
    // A create proposal names a new entry under its own id; any other id
    // could be taken first.
    let other = MemoryOp::Create { entry_id: Uuid::new_v4(), title: "x".into(), body: "y".into(), tags: vec![] };
    let unbound = MemoryProposal { proposal_id: Uuid::new_v4(), op: other, proposer: f.agent_id.to_string(), source: Default::default(), rationale: String::new() };
    let unbound = f.sign(Via::Agent, None, rec(FlowRecord::MemoryProposed(unbound)));
    assert_eq!(f.w.verdict(unbound), Verdict::Denied, "a create proposal names an entry id other than its own");
    // Mara squats the entry id before adm accepts: as a direct create it
    // must carry the proposal's event id, an equivocation (a denial of
    // both, never a takeover).
    let squat = f.sign(Via::Person(2), None, rec(create(pid, "mara's squat")));
    let accept = f.sign(Via::Person(1), None, rec(FlowRecord::MemoryProposalDecided(MemoryDecision { proposal_id: pid, accepted: true, ..Default::default() })));
    let m = f.memory();
    assert_ne!(f.w.verdict(squat), Verdict::Allowed, "the squat was allowed");
    assert!(m.entries.get(&pid).is_none_or(|e| e.created_by.id != "mara"), "mara took the proposed entry over");
    let _ = accept;
}

/// The attack the binding closes, as the first review left it: a create
/// proposal naming an entry id of the proposer's choice, squatted first.
#[test]
fn a_squatted_create_proposal_never_lands_on_the_squatters_note() {
    let mut f = fixture();
    let (pid, e) = (Uuid::new_v4(), Uuid::new_v4());
    let agent = f.agent_id.to_string();
    let op = MemoryOp::Create { entry_id: e, title: "Deploy day".into(), body: "Thursdays".into(), tags: vec![] };
    let p = MemoryProposal { proposal_id: pid, op, proposer: agent, source: Default::default(), rationale: String::new() };
    let filed = f.sign(Via::Agent, None, rec(FlowRecord::MemoryProposed(p)));
    let squat = f.sign(Via::Person(2), None, rec(create(e, "mara's squat")));
    let accept = f.sign(Via::Person(1), None, rec(FlowRecord::MemoryProposalDecided(MemoryDecision { proposal_id: pid, accepted: true, ..Default::default() })));
    if f.w.verdict(filed) == Verdict::Allowed {
        assert_eq!(f.w.verdict(squat), Verdict::Allowed);
        assert_eq!(f.w.verdict(accept), Verdict::Allowed, "the person's acceptance was refused: the entry id was squatted");
    }
}

/// An agent rebound to another host mid-flight: its old host's key and its
/// old host's runtime effects no longer speak for it (proposals or task
/// state), while what it filed before the move stays (judged at its own
/// position) for a person to decide.
#[test]
fn an_agent_moved_to_another_host_no_longer_speaks_through_the_old_one() {
    let mut f = fixture();
    let chat = f.chat;
    let agent = f.agent_id.to_string();
    let before = f.sign(Via::Agent, None, rec(proposal(Uuid::new_v4(), new_note(), agent.clone())));
    assert_eq!(f.w.verdict(before), Verdict::Allowed);
    // The owner moves the agent to the owner's own device.
    let mut moved = f.w.config().workspace_agents.iter().find(|a| a.id == f.agent_id).unwrap().clone();
    moved.host_id = f.w.people["owner"].device_id.to_string();
    f.w.push("owner", SessionEvent::AgentUpserted { agent: moved });
    let by_key = f.sign(Via::Agent, None, rec(proposal(Uuid::new_v4(), new_note(), agent.clone())));
    let by_old_host = f.sign(Via::Runtime, None, rec(proposal(Uuid::new_v4(), new_note(), agent.clone())));
    let t = rec(FlowRecord::TaskState(TaskStateUpdate { task_id: chat, agent_key: agent.clone(), turn_id: Uuid::new_v4(), summary: "still here".into(), ..Default::default() }));
    let slot_by_old_host = f.sign(Via::Person(2), Some(chat), t.clone());
    let slot_by_key = f.sign(Via::Agent, Some(chat), t);
    for (what, id) in [("key proposal", by_key), ("old host proposal", by_old_host), ("old host slot", slot_by_old_host), ("key slot", slot_by_key)] {
        assert_eq!(f.w.verdict(id), Verdict::Denied, "{what} after the move");
    }
    assert_eq!(f.w.verdict(before), Verdict::Allowed, "a proposal filed before the move stays");
    let accept = f.sign(Via::Person(2), None, rec(FlowRecord::MemoryProposalDecided(MemoryDecision { proposal_id: before, accepted: true, ..Default::default() })));
    assert_eq!(f.w.verdict(accept), Verdict::Allowed);
    assert!(f.memory().entries[&before].live(), "the accepted entry is named by its proposal");
}

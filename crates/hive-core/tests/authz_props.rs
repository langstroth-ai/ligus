//! The authorization engine (Phase 0, A2): properties and unit cases.
//!
//! A workspace is a config log (the roster) plus a chat log. Chat events are
//! stamped with the config frontier their author had folded. Property tests
//! run 256 cases by default (`PROPTEST_CASES=4096` nightly).

use std::collections::BTreeMap;

use hive_core::authz::{votes::diff_digest, ConfigTimeline, HoldReason};
use hive_core::flow::FlowRecord;
use hive_core::proposal_sets::ProposalSet;
use hive_core::{
    fold_decisions, project_authorized, project_config, workspace_config_session_id, ActionProposal, ActorIdentity,
    ActorKind, ActorStamp, CfgFrontier, ChatSession, MemberRoleChange, ProposalApproval, ProposalStatus,
    SessionEvent, SessionEventEnvelope, Timestamp, Verdict, WorkspaceAuthz, WorkspaceMember, WorkspaceRole,
};
use proptest::prelude::*;
use uuid::Uuid;

const PEOPLE: [&str; 5] = ["owner", "ada", "mara", "val", "eve"];

fn member(id: &str, role: WorkspaceRole) -> WorkspaceMember {
    let mut actor = ActorIdentity::new(id, id, ActorKind::Human);
    actor.account_id = Some(account(id));
    WorkspaceMember { id: id.into(), actor, role, title: String::new(), index: 0, joined_at: Timestamp::epoch() }
}

fn account(id: &str) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, id.as_bytes())
}

fn role_from(n: u8) -> WorkspaceRole {
    [WorkspaceRole::Owner, WorkspaceRole::Admin, WorkspaceRole::Contributor, WorkspaceRole::Viewer][n as usize % 4]
}

/// One workspace: its config log and one chat log. Every person signs with
/// their own certified device; the members (owner, ada, mara, val) hold a
/// registered test recovery key, so their config events and their gated chat
/// events carry its presence (rules v2 from the seed). `eve` is a stranger.
#[derive(Clone)]
struct World {
    wid: Uuid,
    sid: Uuid,
    /// One lamport clock for both logs (so a suffix sorts after a prefix).
    clock: u64,
    cfg: Vec<SessionEventEnvelope>,
    chat: Vec<SessionEventEnvelope>,
    present: std::collections::BTreeSet<String>,
}

/// `who`'s account key (deterministic, as `trust_events` publishes it).
fn account_key(who: &str) -> hive_core::SigningKeypair {
    hive_core::SigningKeypair::from_seed(&[who.len() as u8 + 1; 32]).unwrap()
}

/// `who`'s own signing device.
fn device_of(who: &str) -> (Uuid, hive_core::SigningKeypair) {
    let id = Uuid::new_v5(&Uuid::NAMESPACE_OID, format!("device:{who}").as_bytes());
    let mut seed = [0u8; 32];
    for (i, b) in who.bytes().enumerate() {
        seed[i % 32] ^= b;
    }
    seed[31] = 0x5a;
    (id, hive_core::SigningKeypair::from_seed(&seed).unwrap())
}

/// Chat kinds a test author confirms with presence (the gated ones).
fn gated_on_chat(e: &SessionEvent) -> bool {
    matches!(
        e,
        SessionEvent::ProposalVoteCast { .. }
            | SessionEvent::ProposalApplied { .. }
            | SessionEvent::ProposalSetDecided { .. }
            | SessionEvent::ProposalSetTuned { .. }
            | SessionEvent::WorkflowDefinitionsUpdated { .. }
    )
}

impl World {
    fn new() -> Self {
        let wid = Uuid::new_v4();
        let mut seed = ChatSession::new("", wid, "");
        seed.id = workspace_config_session_id(wid);
        seed.creator_actor_id = "owner".into();
        seed.members = vec![member("owner", WorkspaceRole::Owner)];
        let mut chat = ChatSession::new("chat", wid, "anthropic");
        chat.creator_actor_id = "owner".into();
        chat.members = vec![member("owner", WorkspaceRole::Owner)];
        let sid = chat.id;
        let mut w = Self { wid, sid, clock: 0, cfg: Vec::new(), chat: Vec::new(), present: Default::default() };
        w.cfg_push("owner", SessionEvent::SessionSnapshot { session: Box::new(seed) });
        w.identity("owner");
        w.chat_push("owner", SessionEvent::SessionSnapshot { session: Box::new(chat) });
        for (id, role) in [("ada", WorkspaceRole::Admin), ("mara", WorkspaceRole::Contributor), ("val", WorkspaceRole::Viewer)] {
            w.cfg_push("owner", SessionEvent::MemberAdded { invite: None, member: member(id, role) });
            w.identity(id);
        }
        w
    }

    /// `who` publishes their account key and device, and registers their
    /// test recovery key (the founder's grant, or the Admin's who added them).
    fn identity(&mut self, who: &str) {
        let (device, dk) = device_of(who);
        let acct = account_key(who);
        self.cfg_push(who, SessionEvent::AccountKeyRegistered { account_id: account(who), signing_public_key: acct.public_key_bytes().to_vec() });
        let cert = hive_core::DeviceCertificate::issue(&acct, account(who), device, &dk.public_key_bytes(), Timestamp::epoch());
        self.cfg_push(who, SessionEvent::DeviceCertificateAdded { certificate: cert });
        let id = self.cfg_push(who, hive_core::testkit::recovery_registration(account(who)));
        let env = self.cfg.iter_mut().find(|e| e.event_id == id).unwrap();
        hive_core::testkit::prove_recovery(env, account(who), device, &dk);
        self.present.insert(who.to_string());
    }

    /// Sign `env` as `by`'s device, with their presence when `gated` and
    /// they hold a registered recovery key. The stamp names the device's
    /// previous event on that log (its chain).
    fn seal(&self, env: &mut SessionEventEnvelope, by: &str, gated: bool) {
        let (device, dk) = device_of(by);
        if let Some(s) = env.actor_stamp.as_mut() {
            s.actor.device_id = Some(device);
        }
        hive_core::sign_envelope(env, device, &dk);
        if gated && self.present.contains(by) {
            hive_core::testkit::prove_recovery(env, account(by), device, &dk);
        }
    }

    /// Name `by`'s device's previous event on `env`'s log (its chain).
    fn chain(&self, env: &mut SessionEventEnvelope, by: &str) {
        let device = device_of(by).0;
        let log = if env.session_id == self.sid { &self.chat } else { &self.cfg };
        let prev = log.iter().rev().find(|e| e.signer_device_id == Some(device)).map(|e| e.event_id);
        if let Some(s) = env.actor_stamp.as_mut() {
            s.prev = prev;
        }
    }

    /// Re-seal an event a test changed after writing it (its presence and
    /// signature cover what changed).
    fn reseal(&mut self, id: Uuid) {
        let (on_cfg, idx) = match self.cfg.iter().position(|e| e.event_id == id) {
            Some(i) => (true, i),
            None => (false, self.chat.iter().position(|e| e.event_id == id).unwrap()),
        };
        let mut env = if on_cfg { self.cfg[idx].clone() } else { self.chat[idx].clone() };
        env.clear_raw();
        let by = env.actor_stamp.as_ref().map(|s| s.actor.id.clone()).unwrap_or_default();
        let gated = on_cfg || gated_on_chat(&env.payload);
        if let Some(s) = env.actor_stamp.as_mut() {
            s.presence = None;
        }
        self.seal(&mut env, &by, gated);
        if on_cfg {
            self.cfg[idx] = env;
        } else {
            self.chat[idx] = env;
        }
    }

    fn stamp(by: &str, frontier: Option<CfgFrontier>) -> Option<ActorStamp> {
        let mut actor = ActorIdentity::new(by, by, ActorKind::Human);
        actor.account_id = Some(account(by));
        Some(ActorStamp { actor, recorded_at: Timestamp::epoch(), cfg_frontier: frontier, presence: None, runtime_effect: false, prev: None, sig_alg: Default::default() })
    }

    fn head(&self) -> Option<CfgFrontier> {
        self.cfg.iter().map(|e| CfgFrontier { lamport: e.lamport, event_id: e.event_id }).max()
    }

    /// A chat event by `by` at the head, without presence.
    fn chat_push_bare(&mut self, by: &str, e: SessionEvent) -> Uuid {
        let present = self.present.remove(by);
        let id = self.chat_push(by, e);
        if present {
            self.present.insert(by.to_string());
        }
        id
    }

    /// A config event by `by`'s device without presence.
    fn cfg_push_bare(&mut self, by: &str, e: SessionEvent) -> Uuid {
        let present = self.present.remove(by);
        let id = self.cfg_push(by, e);
        if present {
            self.present.insert(by.to_string());
        }
        id
    }

    /// The watermark an honest author of a cut of `member` (or `device`)
    /// writes: the heads of every signer that signed as them, on both logs.
    fn watermark(&self, member: Option<&str>, device: Option<Uuid>) -> Vec<hive_core::watermark::Watermark> {
        let mut signers: std::collections::BTreeSet<Uuid> = device.into_iter().collect();
        if let Some(m) = member {
            for e in self.cfg.iter().chain(&self.chat) {
                if e.actor_stamp.as_ref().is_some_and(|s| s.actor.id == m) {
                    signers.extend(e.signer_device_id);
                }
            }
        }
        let mut out = hive_core::watermark::heads_of(&self.cfg, workspace_config_session_id(self.wid), &signers);
        out.extend(hive_core::watermark::heads_of(&self.chat, self.sid, &signers));
        out
    }

    /// Fill in the honest watermark of a removal, demotion or revocation
    /// written with an empty one.
    fn honest(&self, e: SessionEvent) -> SessionEvent {
        match e {
            SessionEvent::MemberRemoved { member_id, watermark } if watermark.is_empty() => {
                let watermark = self.watermark(Some(&member_id), None);
                SessionEvent::MemberRemoved { member_id, watermark }
            }
            SessionEvent::MemberRoleChanged { change, watermark } if watermark.is_empty() => {
                let watermark = self.watermark(Some(&change.member_id), None);
                SessionEvent::MemberRoleChanged { change, watermark }
            }
            SessionEvent::DeviceRevoked { device_id, watermark } if watermark.is_empty() => {
                let watermark = self.watermark(None, Some(device_id));
                SessionEvent::DeviceRevoked { device_id, watermark }
            }
            other => other,
        }
    }

    /// A config event exactly as given, `back` lamports below the next one
    /// (a backdated write; at least just after the seed).
    fn cfg_at(&mut self, by: &str, e: SessionEvent, back: u64) -> Uuid {
        self.clock += 1;
        let mut env = SessionEventEnvelope::new(workspace_config_session_id(self.wid), self.wid, self.clock as i64, e);
        env.lamport = self.clock.saturating_sub(back).max(2);
        env.actor_stamp = Self::stamp(by, self.head());
        self.chain(&mut env, by);
        self.seal(&mut env, by, true);
        let id = env.event_id;
        self.cfg.push(env);
        id
    }

    /// A config event exactly as given (no honest watermark filled in).
    fn cfg_push_raw(&mut self, by: &str, e: SessionEvent) -> Uuid {
        self.clock += 1;
        let mut env = SessionEventEnvelope::new(workspace_config_session_id(self.wid), self.wid, self.clock as i64, e);
        env.lamport = self.clock;
        env.actor_stamp = Self::stamp(by, self.head());
        self.chain(&mut env, by);
        self.seal(&mut env, by, true);
        let id = env.event_id;
        self.cfg.push(env);
        id
    }

    fn cfg_push(&mut self, by: &str, e: SessionEvent) -> Uuid {
        let e = self.honest(e);
        self.clock += 1;
        let mut env = SessionEventEnvelope::new(workspace_config_session_id(self.wid), self.wid, self.clock as i64, e);
        env.lamport = self.clock;
        env.actor_stamp = Self::stamp(by, self.head());
        self.chain(&mut env, by);
        self.seal(&mut env, by, true);
        let id = env.event_id;
        self.cfg.push(env);
        id
    }

    /// A chat event at the current config head (an honest author).
    fn chat_push(&mut self, by: &str, e: SessionEvent) -> Uuid {
        let f = self.head();
        self.chat_at(by, e, f)
    }

    fn chat_at(&mut self, by: &str, e: SessionEvent, frontier: Option<CfgFrontier>) -> Uuid {
        self.clock += 1;
        let mut env = SessionEventEnvelope::new(self.sid, self.wid, self.clock as i64, e);
        env.lamport = self.clock;
        env.actor_stamp = Self::stamp(by, frontier);
        self.chain(&mut env, by);
        let gated = gated_on_chat(&env.payload);
        self.seal(&mut env, by, gated);
        let id = env.event_id;
        self.chat.push(env);
        id
    }

    fn timeline(&self) -> ConfigTimeline {
        project_config(&self.cfg, None, None).1
    }

    fn state(&self) -> ChatSession {
        let t = self.timeline();
        project_authorized(&self.chat, None, None, WorkspaceAuthz::Timeline(&t)).unwrap()
    }

    fn config(&self) -> ChatSession {
        project_config(&self.cfg, None, None).0.unwrap()
    }

    fn verdicts(&self) -> BTreeMap<Uuid, Verdict> {
        let t = self.timeline();
        let mut v = fold_decisions(&self.chat, None, None, WorkspaceAuthz::Timeline(&t));
        v.extend(fold_decisions(&self.cfg, None, None, WorkspaceAuthz::Unchecked));
        v
    }
}

fn vote(by: &str, pid: Uuid, role: WorkspaceRole, approved: bool, digest: Option<String>) -> SessionEvent {
    SessionEvent::ProposalVoteCast {
        proposal_id: pid,
        approval: ProposalApproval { actor_id: by.into(), role, approved, created_at: Timestamp::epoch(), diff_digest: digest },
    }
}

fn proposal(id: Uuid, content: u8, status: ProposalStatus) -> ActionProposal {
    let mut p = ActionProposal::file_diff("p", "planner", format!("diff --git a/f b/f\n+{content}\n"), vec!["f".into()]);
    p.id = id;
    p.created_at = Timestamp::epoch();
    p.status = status;
    p
}

/// A random history: roster changes on the config log, proposals, upserts and
/// votes on the chat, from members and a stranger.
#[derive(Debug, Clone)]
enum Op {
    SetRole { who: u8, role: u8 },
    Upsert { by: u8, prop: u8, content: u8, status: u8 },
    Vote { by: u8, prop: u8, declared: u8, approved: bool, digest: u8 },
    Stale { by: u8, prop: u8, back: u8 },
    Applied { by: u8, prop: u8 },
    /// The owner removes a member (an honest watermark).
    Remove { who: u8 },
    /// A message stamped with a frontier `back` config events old (a lagging
    /// author, or a removed one backdating).
    Backdate { by: u8, back: u8 },
    /// Anyone cuts anyone (`kind`: remove, demote to Viewer, revoke their
    /// device), `back` lamports below the head; backdated cuts name an empty
    /// watermark (the counter-cut of review P-A2b).
    Cut { by: u8, who: u8, kind: u8, back: u8 },
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (1u8..4, 0u8..4).prop_map(|(who, role)| Op::SetRole { who, role }),
        (0u8..5, 0u8..3, 0u8..3, 0u8..4).prop_map(|(by, prop, content, status)| Op::Upsert { by, prop, content, status }),
        (0u8..5, 0u8..3, 0u8..4, any::<bool>(), 0u8..3)
            .prop_map(|(by, prop, declared, approved, digest)| Op::Vote { by, prop, declared, approved, digest }),
        (0u8..5, 0u8..3, 0u8..6).prop_map(|(by, prop, back)| Op::Stale { by, prop, back }),
        (0u8..5, 0u8..3).prop_map(|(by, prop)| Op::Applied { by, prop }),
        (1u8..4).prop_map(|who| Op::Remove { who }),
        (0u8..5, 0u8..8).prop_map(|(by, back)| Op::Backdate { by, back }),
        (0u8..4, 0u8..4, 0u8..3, 0u8..4).prop_map(|(by, who, kind, back)| Op::Cut { by, who, kind, back }),
    ]
}

const PROPS: [u128; 3] = [0xA1, 0xA2, 0xA3];

fn status_from(n: u8) -> ProposalStatus {
    [ProposalStatus::Open, ProposalStatus::Approved, ProposalStatus::Rejected, ProposalStatus::Applied][n as usize % 4]
}

/// Play `ops` into `w`. `declared` and `status` let a property rewrite what
/// the payload claims.
fn play(w: &mut World, ops: &[Op], declared: &dyn Fn(u8) -> WorkspaceRole, status: &dyn Fn(u8) -> ProposalStatus) {
    let mut content: BTreeMap<u8, u8> = BTreeMap::new();
    for op in ops {
        match op.clone() {
            Op::SetRole { who, role } => {
                let id = PEOPLE[who as usize];
                let old = w.config().role_of(id);
                w.cfg_push("owner", SessionEvent::MemberRoleChanged {
                    change: MemberRoleChange { member_id: id.into(), old_role: old, new_role: role_from(role) }, watermark: Vec::new(),
                });
            }
            Op::Upsert { by, prop, content: c, status: st } => {
                content.insert(prop, c);
                let p = proposal(Uuid::from_u128(PROPS[prop as usize]), c, status(st));
                w.chat_push(PEOPLE[by as usize], SessionEvent::ProposalUpserted { proposal: p });
            }
            Op::Vote { by, prop, declared: d, approved, digest } => {
                let pid = Uuid::from_u128(PROPS[prop as usize]);
                let current = content.get(&prop).map(|c| diff_digest(&proposal(pid, *c, ProposalStatus::Open)));
                let digest = match digest {
                    0 => None,
                    1 => current,
                    _ => Some(diff_digest(&proposal(pid, 99, ProposalStatus::Open))),
                };
                let by = PEOPLE[by as usize];
                w.chat_push(by, vote(by, pid, declared(d), approved, digest));
            }
            Op::Stale { by, prop, back } => {
                // A frontier `back` config events old (an author lagging, or
                // claiming to).
                let mut keys: Vec<CfgFrontier> =
                    w.cfg.iter().map(|e| CfgFrontier { lamport: e.lamport, event_id: e.event_id }).collect();
                keys.sort();
                let f = keys[keys.len().saturating_sub(1 + back as usize)];
                let pid = Uuid::from_u128(PROPS[prop as usize]);
                let by = PEOPLE[by as usize];
                w.chat_at(by, vote(by, pid, WorkspaceRole::Owner, true, None), Some(f));
            }
            Op::Applied { by, prop } => {
                let pid = Uuid::from_u128(PROPS[prop as usize]);
                let Some(c) = content.get(&prop) else { continue };
                let d = diff_digest(&proposal(pid, *c, ProposalStatus::Open));
                w.chat_push(PEOPLE[by as usize], SessionEvent::ProposalApplied { proposal_id: pid, diff_digest: d, commit: None, at: Timestamp::epoch(), repo_id: None, applied_on: None, base_commit: None, applied_tree_hash: None, request_id: None });
            }
            Op::Remove { who } => {
                w.cfg_push("owner", SessionEvent::MemberRemoved { member_id: PEOPLE[who as usize].into(), watermark: Vec::new() });
            }
            Op::Backdate { by, back } => {
                let f = w.back(back);
                let by = PEOPLE[by as usize];
                w.chat_at(by, message(by, "backdated"), Some(f));
            }
            Op::Cut { by, who, kind, back } => {
                let (by, who) = (PEOPLE[by as usize], PEOPLE[who as usize]);
                let e = match kind {
                    0 => SessionEvent::MemberRemoved { member_id: who.into(), watermark: Vec::new() },
                    1 => SessionEvent::MemberRoleChanged {
                        change: MemberRoleChange { member_id: who.into(), old_role: w.config().role_of(who), new_role: WorkspaceRole::Viewer },
                        watermark: Vec::new(),
                    },
                    _ => SessionEvent::DeviceRevoked { device_id: device_of(who).0, watermark: Vec::new() },
                };
                if back == 0 {
                    w.cfg_push(by, e);
                } else {
                    w.cfg_at(by, e, back as u64);
                }
            }
        }
    }
}

impl World {
    /// The config position `back` events before the head.
    fn back(&self, back: u8) -> CfgFrontier {
        let mut keys: Vec<CfgFrontier> = self.cfg.iter().map(|e| CfgFrontier { lamport: e.lamport, event_id: e.event_id }).collect();
        keys.sort();
        keys[keys.len().saturating_sub(1 + back as usize)]
    }
}

fn honest_roles(n: u8) -> WorkspaceRole {
    role_from(n)
}
fn honest_status(n: u8) -> ProposalStatus {
    status_from(n)
}

fn world(ops: &[Op]) -> World {
    let mut w = World::new();
    play(&mut w, ops, &honest_roles, &honest_status);
    w
}

proptest! {
    #[test]
    /// Review P-A2b (owner decision): cuts go strictly down the ranks, so of
    /// any two allowed cuts, the subject of one is never the author of a cut
    /// of that cut's author.
    fn prop_no_mutual_cuts(ops in prop::collection::vec(arb_op(), 0..24)) {
        let w = world(&ops);
        let t = w.timeline();
        let by_key: BTreeMap<(u64, Uuid), String> =
            w.cfg.iter().map(|e| (e.canonical_key(), e.actor_stamp.as_ref().map(|s| s.actor.id.clone()).unwrap_or_default())).collect();
        let subject = |c: &hive_core::authz::Cut| match &c.kind {
            hive_core::authz::CutKind::Removed(m) | hive_core::authz::CutKind::Demoted(m) => Some(m.clone()),
            hive_core::authz::CutKind::Revoked(d) => PEOPLE.iter().find(|p| device_of(p).0 == *d).map(|p| p.to_string()),
        };
        for a in t.cuts() {
            for b in t.cuts() {
                let (Some(sa), Some(sb)) = (subject(a), subject(b)) else { continue };
                let (aa, ab) = (&by_key[&a.key], &by_key[&b.key]);
                prop_assert!(!(sa == *ab && sb == *aa && aa != ab), "mutual cuts {:?} {:?}", a, b);
            }
        }
    }

    /// Appending events never changes the verdict on an event already
    /// decided: for any history L and suffix S, every event of L that L
    /// allowed (denied) is allowed (denied) in L·S. Only a held event may be
    /// decided later, and an event of the subject of a cut S brings.
    #[test]
    fn prop_authz_is_prefix_stable(ops in prop::collection::vec(arb_op(), 0..24), cut in 0usize..24) {
        let cut = cut.min(ops.len());
        let prefix = world(&ops[..cut]);
        let mut whole = prefix.clone();
        // The suffix's cuts carry honest watermarks: a cut that names too
        // little (a backdated one with an empty watermark) re-judges its
        // subject's history and whatever depended on it, by design (a
        // known limitation, review P-A2e).
        let suffix: Vec<Op> = ops[cut..]
            .iter()
            .map(|o| match o.clone() {
                Op::Cut { by, who, kind, .. } => Op::Cut { by, who, kind, back: 0 },
                o => o,
            })
            .collect();
        play(&mut whole, &suffix, &honest_roles, &honest_status);
        let before = prefix.verdicts();
        let after = whole.verdicts();
        let old: std::collections::BTreeSet<(u64, Uuid)> = prefix.cfg.iter().map(|e| e.canonical_key()).collect();
        let new_cuts: Vec<hive_core::authz::Cut> = whole.timeline().cuts().iter().filter(|c| !old.contains(&c.key)).cloned().collect();
        let cut_subject = |id: &Uuid| {
            whole.cfg.iter().chain(&whole.chat).filter(|e| e.event_id == *id).any(|e| new_cuts.iter().any(|c| c.applies(e)))
        };
        for (id, v) in &before {
            if *v != Verdict::Held && !cut_subject(id) {
                let e = whole.cfg.iter().chain(&whole.chat).find(|e| e.event_id == *id).unwrap();
                let who = e.actor_stamp.as_ref().map(|s| s.actor.id.clone()).unwrap_or_default();
                prop_assert_eq!(after.get(id), Some(v), "event {} ({} by {}) changed verdict", id, e.payload.kind_str(), who);
            }
        }
    }

    /// Projection is a function of the event set: any delivery order gives
    /// the same chat, the same held set and the same verdicts.
    #[test]
    fn prop_fold_order_independent(ops in prop::collection::vec(arb_op(), 0..24), seed in any::<u64>()) {
        let w = world(&ops);
        let mut shuffled = w.clone();
        let mut rng = seed | 1;
        let mut next = || { rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17; rng };
        for v in [&mut shuffled.cfg, &mut shuffled.chat] {
            for i in (1..v.len()).rev() {
                let j = (next() % (i as u64 + 1)) as usize;
                v.swap(i, j);
            }
        }
        let (a, b) = (w.state(), shuffled.state());
        prop_assert_eq!(&a.proposals, &b.proposals);
        prop_assert_eq!(&a.authz.held, &b.authz.held);
        prop_assert_eq!(w.verdicts(), shuffled.verdicts());
    }

    /// The role in a vote payload never matters: only the signer's role at
    /// the vote's position counts.
    #[test]
    fn prop_vote_role_capped(ops in prop::collection::vec(arb_op(), 0..24), salt in any::<u8>()) {
        let honest = world(&ops);
        let mut lying = World { cfg: Vec::new(), chat: Vec::new(), clock: 0, ..honest.clone() };
        // Same ids and seeds; only the declared roles differ.
        lying.cfg = honest.cfg.clone();
        lying.chat = honest.chat.clone();
        let mut changed = Vec::new();
        for e in &mut lying.chat {
            if let SessionEvent::ProposalVoteCast { approval, .. } = &mut e.payload {
                approval.role = role_from(salt.wrapping_add(approval.actor_id.len() as u8));
                changed.push(e.event_id);
            }
        }
        // (Each liar signs and confirms what they claim.)
        for id in changed {
            lying.reseal(id);
        }
        prop_assert_eq!(honest.state().proposals, lying.state().proposals);
    }

    /// Status is derived, never asserted: whatever status an upsert carries,
    /// the fold is the same.
    #[test]
    fn prop_status_derived(ops in prop::collection::vec(arb_op(), 0..24), salt in 0u8..4) {
        let honest = world(&ops);
        let mut asserted = honest.clone();
        for e in &mut asserted.chat {
            if let SessionEvent::ProposalUpserted { proposal } = &mut e.payload {
                proposal.status = status_from(salt);
            }
        }
        prop_assert_eq!(honest.state().proposals, asserted.state().proposals);
    }

    /// A vote for content d never counts for other content d': a forged
    /// upsert with a different patch, at any lamport before or after the
    /// vote, can't inherit it (S1's lower-lamport forged upsert).
    #[test]
    fn prop_vote_binds_digest(forged_lamport in 1u64..40, forger in 0u8..5) {
        let mut w = World::new();
        let pid = Uuid::from_u128(0xB0);
        let honest = proposal(pid, 1, ProposalStatus::Open);
        let d = diff_digest(&honest);
        w.chat_push("mara", SessionEvent::ProposalUpserted { proposal: honest });
        w.chat_push("owner", vote("owner", pid, WorkspaceRole::Owner, true, Some(d.clone())));
        // The forger's upsert, signed at an arbitrary lamport.
        let forged = proposal(pid, 2, ProposalStatus::Approved);
        let id = w.chat_push(PEOPLE[forger as usize], SessionEvent::ProposalUpserted { proposal: forged });
        w.chat.iter_mut().find(|e| e.event_id == id).unwrap().lamport = forged_lamport;
        let s = w.state();
        let p = s.proposals.iter().find(|p| p.id == pid).unwrap();
        if diff_digest(p) != d {
            prop_assert!(p.status != ProposalStatus::Approved, "a vote for d counted for d'");
            prop_assert!(p.approvals.is_empty());
        } else {
            prop_assert_eq!(p.status, ProposalStatus::Approved);
        }
    }
}

#[test]
fn held_until_config_frontier_arrives() {
    let mut w = World::new();
    let pid = Uuid::from_u128(0xC0);
    w.chat_push("mara", SessionEvent::ProposalUpserted { proposal: proposal(pid, 1, ProposalStatus::Open) });
    // val is promoted on a device we haven't synced yet; her vote names that
    // config event as its frontier.
    let mut ahead = w.clone();
    ahead.cfg_push("owner", SessionEvent::MemberRoleChanged {
        change: MemberRoleChange { member_id: "val".into(), old_role: WorkspaceRole::Viewer, new_role: WorkspaceRole::Admin }, watermark: Vec::new(),
    });
    let promotion = ahead.cfg.last().unwrap().clone();
    let f = CfgFrontier { lamport: promotion.lamport, event_id: promotion.event_id };
    w.clock = ahead.clock;
    let vid = w.chat_at("val", vote("val", pid, WorkspaceRole::Viewer, true, None), Some(f));

    let s = w.state();
    let p = s.proposals.iter().find(|p| p.id == pid).unwrap();
    assert_eq!(p.status, ProposalStatus::Open, "a held vote doesn't count");
    assert_eq!(s.authz.held.len(), 1);
    assert_eq!(s.authz.held[0].event_id, vid);
    assert_eq!(s.authz.held[0].reason, HoldReason::ConfigFrontierAhead);
    assert!(s.authz.blocks(pid, None), "no side effect while a vote on it is held");
    assert_eq!(w.verdicts()[&vid], Verdict::Held);

    // The promotion arrives: the vote is decided, at Admin.
    w.cfg.push(promotion);
    let s = w.state();
    let p = s.proposals.iter().find(|p| p.id == pid).unwrap();
    assert_eq!(p.status, ProposalStatus::Approved);
    assert_eq!(p.approvals[0].role, WorkspaceRole::Admin);
    assert!(s.authz.held.is_empty());
    assert!(!s.authz.blocks(pid, None));
}

/// A-14: a vote naming a config event nobody will ever deliver can't hold
/// its proposal forever. Once the config log has passed the claimed lamport
/// (or the claim is absurdly far ahead), the vote is left out, not held, and
/// blocks no side effect.
#[test]
fn forged_frontier_hold_cannot_block_side_effects_indefinitely() {
    let mut w = World::new();
    let pid = Uuid::from_u128(0xC7);
    w.chat_push("mara", SessionEvent::ProposalUpserted { proposal: proposal(pid, 1, ProposalStatus::Open) });
    let head = w.head().unwrap();
    // Just ahead of the head: a plausible lagging config event, held.
    let near = CfgFrontier { lamport: head.lamport + 1, event_id: Uuid::new_v4() };
    w.clock += 5;
    let vid = w.chat_at("val", vote("val", pid, WorkspaceRole::Viewer, false, None), Some(near));
    assert!(w.state().authz.blocks(pid, None), "held while it could still arrive");
    // The config log moves past the claimed lamport: the hold is dropped.
    for _ in 0..3 {
        w.cfg_push("owner", SessionEvent::JoinApproved { subject: "noop".into() });
    }
    let s = w.state();
    assert!(!s.authz.blocks(pid, None), "a frontier the log has passed can't block side effects");
    assert_eq!(w.verdicts()[&vid], Verdict::Denied);
    // Absurdly far ahead: never waited for.
    let far = CfgFrontier { lamport: u64::MAX - 1, event_id: Uuid::new_v4() };
    let fid = w.chat_at("val", vote("val", pid, WorkspaceRole::Viewer, false, None), Some(far));
    assert!(!w.state().authz.blocks(pid, None));
    assert_eq!(w.verdicts()[&fid], Verdict::Denied);
}

#[test]
fn held_when_no_config_log() {
    let mut w = World::new();
    let pid = Uuid::from_u128(0xC1);
    let set = ProposalSet::new("Q", "planner", vec![pid]);
    let set_id = set.id;
    w.chat_push("mara", SessionEvent::ProposalUpserted { proposal: proposal(pid, 1, ProposalStatus::Open) });
    w.chat_push("mara", SessionEvent::ProposalSetOpened { set: Box::new(set) });
    w.chat_push("owner", vote("owner", pid, WorkspaceRole::Owner, true, None));
    let s = project_authorized(&w.chat, None, None, WorkspaceAuthz::NoConfigLog).unwrap();
    assert!(s.proposal_sets.is_empty(), "the set waits for the roster");
    assert!(s.proposals.is_empty(), "every chat event waits for the roster");
    assert_eq!(s.authz.held.len(), 3);
    assert!(s.authz.held.iter().all(|h| h.reason == HoldReason::NoConfigLog));
    assert!(s.authz.blocks(pid, None) && s.authz.blocks_set(set_id, &[pid]));
    // With the config log they fold.
    let s = w.state();
    assert_eq!(s.proposal_sets.len(), 1);
    assert_eq!(s.proposals[0].status, ProposalStatus::Approved);
}

#[test]
fn a_vote_without_a_frontier_or_with_a_forged_one_is_refused() {
    let mut w = World::new();
    let pid = Uuid::from_u128(0xC2);
    w.chat_push("mara", SessionEvent::ProposalUpserted { proposal: proposal(pid, 1, ProposalStatus::Open) });
    let none = w.chat_at("owner", vote("owner", pid, WorkspaceRole::Owner, true, None), None);
    let head = w.head().unwrap();
    let wrong = w.chat_at(
        "owner",
        vote("owner", pid, WorkspaceRole::Owner, true, None),
        Some(CfgFrontier { lamport: head.lamport + 7, event_id: head.event_id }),
    );
    let v = w.verdicts();
    assert_eq!((v[&none], v[&wrong]), (Verdict::Denied, Verdict::Denied));
    assert_eq!(w.state().proposals[0].status, ProposalStatus::Open);
}

#[test]
fn a_vote_counts_only_from_the_voter_it_names() {
    let mut w = World::new();
    let pid = Uuid::from_u128(0xC3);
    w.chat_push("mara", SessionEvent::ProposalUpserted { proposal: proposal(pid, 1, ProposalStatus::Open) });
    // val signs a vote that names the owner.
    let forged = w.chat_push("val", vote("owner", pid, WorkspaceRole::Owner, true, None));
    assert_eq!(w.verdicts()[&forged], Verdict::Denied);
    assert_eq!(w.state().proposals[0].status, ProposalStatus::Open);
}

#[test]
fn applied_only_from_a_human_member_for_the_approved_content() {
    let mut w = World::new();
    let pid = Uuid::from_u128(0xC4);
    let p = proposal(pid, 1, ProposalStatus::Open);
    let d = diff_digest(&p);
    w.chat_push("mara", SessionEvent::ProposalUpserted { proposal: p });
    let applied = |d: &str| SessionEvent::ProposalApplied { proposal_id: pid, diff_digest: d.into(), commit: None, at: Timestamp::epoch(), repo_id: None, applied_on: None, base_commit: None, applied_tree_hash: None, request_id: None };
    // Not approved yet: refused.
    let early = w.chat_push("ada", applied(&d));
    w.chat_push("owner", vote("owner", pid, WorkspaceRole::Owner, true, Some(d.clone())));
    // A Viewer, a stranger and a wrong digest: refused.
    let viewer = w.chat_push("val", applied(&d));
    let stranger = w.chat_push("eve", applied(&d));
    let wrong = w.chat_push("ada", applied("00"));
    let v = w.verdicts();
    for id in [early, viewer, stranger, wrong] {
        assert_eq!(v[&id], Verdict::Denied);
    }
    assert_eq!(w.state().proposals[0].status, ProposalStatus::Approved);
    let ok = w.chat_push("mara", applied(&d));
    assert_eq!(w.verdicts()[&ok], Verdict::Allowed);
    assert_eq!(w.state().proposals[0].status, ProposalStatus::Applied);
}

// ---- R5 self-revoke ----

fn trust_events(w: &mut World, who: &str, device: Uuid) {
    use hive_core::crypto::{DeviceCertificate, SigningKeypair};
    let acct = SigningKeypair::from_seed(&[who.len() as u8 + 1; 32]).unwrap();
    let dev = SigningKeypair::generate().unwrap();
    w.cfg_push(who, SessionEvent::AccountKeyRegistered { account_id: account(who), signing_public_key: acct.public_key_bytes().to_vec() });
    let cert = DeviceCertificate::issue(&acct, account(who), device, &dev.public_key_bytes(), Timestamp::epoch());
    w.cfg_push(who, SessionEvent::DeviceCertificateAdded { certificate: cert });
}

#[test]
fn self_revoke_allowed_other_revoke_needs_admin() {
    let mut w = World::new();
    let (maras_laptop, vals_phone) = (Uuid::from_u128(0xD1), Uuid::from_u128(0xD2));
    trust_events(&mut w, "mara", maras_laptop);
    trust_events(&mut w, "val", vals_phone);
    // mara (Contributor) can't revoke val's device...
    let other = w.cfg_push("mara", SessionEvent::DeviceRevoked { device_id: vals_phone, watermark: Vec::new() });
    // ...but can revoke her own.
    let own = w.cfg_push("mara", SessionEvent::DeviceRevoked { device_id: maras_laptop, watermark: Vec::new() });
    // An Admin can revoke anyone's.
    let admin = w.cfg_push("ada", SessionEvent::DeviceRevoked { device_id: vals_phone, watermark: Vec::new() });
    let v = w.verdicts();
    assert_eq!((v[&other], v[&own], v[&admin]), (Verdict::Denied, Verdict::Allowed, Verdict::Allowed));
    let cfg = w.config();
    assert!(cfg.guards.revoked_devices.contains(&maras_laptop) && cfg.guards.revoked_devices.contains(&vals_phone));
}

#[test]
fn self_revoke_needs_the_certificate_not_a_claim() {
    let mut w = World::new();
    let d = Uuid::from_u128(0xD3);
    trust_events(&mut w, "val", d);
    // mara claims a device her account never certified.
    let claim = w.cfg_push("mara", SessionEvent::DeviceRevoked { device_id: d, watermark: Vec::new() });
    assert_eq!(w.verdicts()[&claim], Verdict::Denied);
}

// ---- S2 agent binding ----

#[test]
fn contributor_cannot_rebind_agent_host_or_runtime() {
    let mut w = World::new();
    let mut agent = hive_core::WorkspaceAgent::new("coder", "gw");
    agent.owner_actor_id = "mara".into();
    let created = w.cfg_push("mara", SessionEvent::AgentUpserted { agent: agent.clone() });
    let mut moved = agent.clone();
    moved.host_id = "worker-1".into();
    let host = w.cfg_push("mara", SessionEvent::AgentUpserted { agent: moved.clone() });
    let mut rt = agent.clone();
    rt.runtime_id = "other".into();
    let runtime = w.cfg_push("mara", SessionEvent::AgentRosterUpdated { agents: vec![rt] });
    let mut renamed = agent.clone();
    renamed.name = "coder2".into();
    let rename = w.cfg_push("mara", SessionEvent::AgentUpserted { agent: renamed.clone() });
    // Someone else's agent: a Contributor can't even rename it.
    let mut theirs = renamed.clone();
    theirs.role = "Reviewer".into();
    let foreign = w.cfg_push("val", SessionEvent::AgentUpserted { agent: theirs });
    // A new agent born bound to a host needs Admin too.
    let mut bound = hive_core::WorkspaceAgent::new("box", "gw");
    bound.host_id = "worker-1".into();
    let born_bound = w.cfg_push("mara", SessionEvent::AgentUpserted { agent: bound.clone() });
    let admin = w.cfg_push("ada", SessionEvent::AgentUpserted { agent: { let mut m = renamed.clone(); m.host_id = "worker-1".into(); m } });
    let v = w.verdicts();
    assert_eq!(v[&created], Verdict::Allowed);
    assert_eq!(v[&host], Verdict::Denied);
    assert_eq!(v[&runtime], Verdict::Denied);
    assert_eq!(v[&rename], Verdict::Allowed);
    assert_eq!(v[&foreign], Verdict::Denied);
    assert_eq!(v[&born_bound], Verdict::Denied);
    assert_eq!(v[&admin], Verdict::Allowed);
    let cfg = w.config();
    let a = cfg.workspace_agents.iter().find(|a| a.id == agent.id).unwrap();
    assert_eq!((a.host_id.as_str(), a.runtime_id.as_str(), a.name.as_str()), ("worker-1", "gw", "coder2"));
}

// ---- S4 run writes ----

#[test]
fn workflow_run_without_epoch_only_from_creator_device() {
    let mut w = World::new();
    let (creator, other) = (Uuid::from_u128(0xE1), Uuid::from_u128(0xE2));
    let def = hive_core::workflow::preset_review_gate();
    let mut run = hive_core::workflow::new_run(&def, "go", "mara");
    run.creator_device_id = Some(creator);
    let sign = |w: &mut World, id: Uuid, dev: Uuid| w.chat.iter_mut().find(|e| e.event_id == id).unwrap().signer_device_id = Some(dev);
    let first = w.chat_push("mara", SessionEvent::WorkflowRunUpserted { run: Box::new(run.clone()) });
    sign(&mut w, first, creator);
    let mut hijack = run.clone();
    hijack.input = "rm -rf".into();
    hijack.creator_device_id = Some(other);
    let foreign = w.chat_push("mara", SessionEvent::WorkflowRunUpserted { run: Box::new(hijack) });
    sign(&mut w, foreign, other);
    let mut update = run.clone();
    update.status = hive_core::workflow::WorkflowRunStatus::Completed;
    let own = w.chat_push("mara", SessionEvent::WorkflowRunUpserted { run: Box::new(update) });
    sign(&mut w, own, creator);
    let v = w.verdicts();
    assert_eq!((v[&first], v[&foreign], v[&own]), (Verdict::Allowed, Verdict::Denied, Verdict::Allowed));
    let s = w.state();
    assert_eq!(s.workflow_runs[0].input, "go");
    assert_eq!(s.workflow_runs[0].creator_device_id, Some(creator));
    assert_eq!(s.workflow_runs[0].status, hive_core::workflow::WorkflowRunStatus::Completed);
}

// ---- S5 lockdown bounds ----

fn lockdown(id: u128, by: &str) -> hive_core::guard::ChatLockdown {
    hive_core::guard::ChatLockdown {
        id: Uuid::from_u128(id),
        signal: hive_core::guard::LockdownSignal::Manual,
        detail: String::new(),
        raised_by: by.into(),
        raised_at: Timestamp::epoch(),
        message_id: None,
    }
}

#[test]
fn lockdown_bounds_s5() {
    let mut w = World::new();
    let chat = w.sid;
    // One raiser: at most 3 active, whatever name the payload claims.
    for i in 0..6 {
        w.cfg_push("val", SessionEvent::ChatLockdownRaised { chat_id: chat, lockdown: lockdown(i, &format!("alias{i}")) });
    }
    assert_eq!(w.config().guards.lockdowns_of(chat).len(), 3);
    // Many raisers (all members): at most 16 active per chat.
    for i in 0..10u128 {
        w.cfg_push("owner", SessionEvent::MemberAdded { member: member(&format!("worker{i}"), WorkspaceRole::Viewer), invite: None });
    }
    for i in 0..40u128 {
        let by = format!("worker{}", i % 10);
        w.cfg_push(&by, SessionEvent::ChatLockdownRaised { chat_id: chat, lockdown: lockdown(100 + i, &by) });
    }
    assert_eq!(w.config().guards.lockdowns_of(chat).len(), 16);
    // A clear frees the slots.
    w.cfg_push("mara", SessionEvent::ChatLockdownCleared { chat_id: chat });
    w.cfg_push("val", SessionEvent::ChatLockdownRaised { chat_id: chat, lockdown: lockdown(999, "val") });
    assert_eq!(w.config().guards.lockdowns_of(chat).len(), 1);
}

// ---- A demotion never lifts a lockdown ----

/// The lockdowns active on `chat` in `w`'s config fold.
fn active_lockdowns(w: &hive_core::testkit::SignedWorld, chat: Uuid) -> std::collections::BTreeSet<Uuid> {
    w.config().guards.lockdowns_of(chat).iter().map(|l| l.id).collect()
}

proptest! {
    /// A Contributor raises a lockdown (after, optionally, raising and
    /// clearing one), and the Owner then cuts them. The cut's watermark is
    /// empty, stale (taken before the raise, as a device that hadn't synced
    /// it would write) or honest, and the raise may sort after the cut
    /// (backdated below it). A demotion never lifts a lockdown: every
    /// lockdown active without the cut stays active with it, so lifting one
    /// still takes a `ChatLockdownCleared` by a human Contributor with
    /// presence. A removal or a revocation keeps failing closed: the raise
    /// stands only if the watermark covers it.
    #[test]
    fn prop_a_demotion_never_lifts_a_lockdown(kind in 0u8..3, wm in 0u8..3, backdate in any::<bool>(), cleared_first in any::<bool>()) {
        use hive_core::testkit::SignedWorld;
        let mut w = SignedWorld::genesis(&["founder", "cara"], None);
        w.admit("founder", "cara", WorkspaceRole::Contributor);
        let chat = Uuid::new_v4();
        let cara_heads = |w: &SignedWorld| {
            let signers = [w.people["cara"].device_id].into_iter().collect();
            hive_core::watermark::heads_of(&w.cfg, workspace_config_session_id(w.wid), &signers)
        };
        let raise = |id: u128| SessionEvent::ChatLockdownRaised { chat_id: chat, lockdown: lockdown(id, "cara") };
        if cleared_first {
            w.push_bare("cara", raise(1));
            w.push("cara", SessionEvent::ChatLockdownCleared { chat_id: chat });
        }
        let stale = cara_heads(&w);
        if !backdate {
            w.push_bare("cara", raise(2));
        }
        let watermark = match wm {
            0 => vec![],
            1 => stale,
            _ => cara_heads(&w),
        };
        let covered = !backdate && wm == 2;
        let cut = match kind {
            0 => SessionEvent::MemberRoleChanged {
                change: MemberRoleChange { member_id: "cara".into(), old_role: WorkspaceRole::Contributor, new_role: WorkspaceRole::Viewer },
                watermark,
            },
            1 => SessionEvent::MemberRemoved { member_id: "cara".into(), watermark },
            _ => SessionEvent::DeviceRevoked { device_id: w.people["cara"].device_id, watermark },
        };
        // What is active without the cut.
        let mut without = w.clone();
        if backdate {
            without.push_bare("cara", raise(2));
        }
        let before = active_lockdowns(&without, chat);
        prop_assert!(before.contains(&Uuid::from_u128(2)));
        // Unrelated traffic, so a backdated raise sorts after the clear.
        w.push("founder", SessionEvent::WorkspaceRulesSet { version: 2, presence: None });
        let cut_id = w.push("founder", cut);
        prop_assert_eq!(w.verdict(cut_id), Verdict::Allowed);
        if backdate {
            // Written after the cut, sorting just below it.
            let lamport = w.cfg.iter().find(|e| e.event_id == cut_id).unwrap().lamport - 1;
            let mut env = w.draft("cara", None, raise(2), None);
            env.lamport = lamport;
            env.clear_raw();
            w.finish(&mut env, "cara", None);
            w.push_env(None, env);
        }
        let after = active_lockdowns(&w, chat);
        if kind == 0 {
            prop_assert!(after.is_superset(&before), "a demotion lifted a lockdown: {:?} -> {:?}", before, after);
        } else {
            prop_assert_eq!(after.contains(&Uuid::from_u128(2)), covered, "{:?}", after);
        }
    }
}

// ---- S6 flow bounds ----

fn zone(i: u128, pattern: &str) -> FlowRecord {
    serde_json::from_value(serde_json::json!({
        "type": "zone", "id": Uuid::from_u128(i), "pattern": pattern, "kind": "core"
    }))
    .unwrap()
}

#[test]
fn flow_bounds_s6() {
    use hive_core::authz::bounds::*;
    let mut w = World::new();
    for i in 0..(MAX_ZONES as u128 + 10) {
        w.cfg_push("ada", SessionEvent::FlowRecorded { record: zone(i, &format!("src/z{i}/")) });
    }
    assert_eq!(w.config().flow.zones.len(), MAX_ZONES);
    let mut w = World::new();
    let long = "a/".repeat(200);
    let deep = (0..20).map(|i| format!("d{i}")).collect::<Vec<_>>().join("/");
    let stars = "**/a/**/b/**/c/**/d/**/e";
    for (i, p) in [long.as_str(), deep.as_str(), stars].into_iter().enumerate() {
        w.cfg_push("ada", SessionEvent::FlowRecorded { record: zone(i as u128, p) });
    }
    assert!(w.config().flow.zones.is_empty(), "oversized patterns are refused");
    // Marks per chat, profiles, recruits per proposal.
    let mut w = World::new();
    for i in 0..(MAX_MARKS_PER_CHAT as u128 + 5) {
        let mark: FlowRecord = serde_json::from_value(serde_json::json!({
            "type": "mark", "id": Uuid::from_u128(i), "kind": "claim", "path": format!("f{i}"), "by": "m", "sessionId": w.sid, "atMs": 0
        }))
        .unwrap();
        w.cfg_push("mara", SessionEvent::FlowRecorded { record: mark });
    }
    assert_eq!(w.config().flow.marks.len(), MAX_MARKS_PER_CHAT);
    for i in 0..(MAX_AGENT_PROFILES as u128 + 5) {
        let p: FlowRecord = serde_json::from_value(serde_json::json!({"type": "agentProfile", "agentId": Uuid::from_u128(i)})).unwrap();
        w.cfg_push("mara", SessionEvent::FlowRecorded { record: p });
    }
    assert_eq!(w.config().flow.profiles.len(), MAX_AGENT_PROFILES);
    for i in 0..(MAX_RECRUITS_PER_PROPOSAL as u128 + 5) {
        let r: FlowRecord = serde_json::from_value(serde_json::json!({
            "type": "recruit", "id": Uuid::from_u128(i), "action": "wake", "queueLen": 1, "oldestWaitMs": 0, "reference": "p1", "atMs": 0
        }))
        .unwrap();
        w.chat_push("mara", SessionEvent::FlowRecorded { record: r });
    }
    assert_eq!(w.state().flow.recruits.len(), MAX_RECRUITS_PER_PROPOSAL);
}

proptest! {
    /// The zone glob does at most a constant times len(pattern)·len(path)
    /// steps on any input (no backtracking blow-up), up to 4 KiB each.
    #[test]
    fn prop_glob_linear_time(
        pattern in "([a-c*]{0,6}|\\*\\*)(/([a-c*]{0,6}|\\*\\*)){0,40}",
        path in "[a-c]{0,8}(/[a-c]{0,8}){0,60}",
        reps in 1usize..12,
    ) {
        let pattern = pattern.repeat(reps);
        let path = path.repeat(reps);
        prop_assume!(pattern.len() <= 4096 && path.len() <= 4096);
        let (_, steps) = hive_core::flow::zones::glob_match_counted(&pattern, &path);
        let bound = 4 * (pattern.len() as u64 + 2) * (path.len() as u64 + 2);
        prop_assert!(steps <= bound, "{steps} steps > {bound}");
    }
}

#[test]
fn glob_semantics_are_unchanged() {
    use hive_core::flow::zones::glob_match;
    for (p, s, want) in [
        ("**/*.md", "docs/a/b.md", true),
        ("**/*.md", "b.md", true),
        ("src/**/mod.rs", "src/mod.rs", true),
        ("src/**/mod.rs", "src/a/b/mod.rs", true),
        ("src/*.rs", "src/a/b.rs", false),
        ("crates/hive-core/", "crates/hive-core/src/x.rs", true),
        ("crates/hive-core", "crates/hive-core/src/x.rs", true),
        ("a*b*c", "aXbYc", true),
        ("a*b*c", "aXbY", false),
        ("**", "anything/at/all", true),
        ("*/x", "a/b/x", false),
    ] {
        assert_eq!(glob_match(p, s), want, "{p} vs {s}");
    }
    // The old backtracking matcher's worst case finishes immediately.
    let p = "**/".repeat(30) + "x";
    let s = "a/".repeat(200) + "y";
    assert!(!glob_match(&p, &s));
}

// ---- rules version ----

/// A version this build doesn't enforce makes the workspace read-only here,
/// but only once its switch is allowed: any switch to version 2 or later needs
/// the Owner's passkey presence (an allowed switch going read-only, and the
/// device folding on, is `presence_tests::switching_to_any_later_rules_version_needs_presence`
/// in hive-runtime, which has a software authenticator).
#[test]
fn unknown_rules_version_needs_presence_to_make_workspace_read_only() {
    let mut w = World::new();
    assert_eq!(w.config().authz.rules, 2, "workspaces start at rules v2");
    assert!(!w.config().authz.read_only());
    // Only the Owner sets rules.
    let by_admin = w.cfg_push("ada", SessionEvent::WorkspaceRulesSet { version: 9, presence: None });
    assert_eq!(w.verdicts()[&by_admin], Verdict::Denied);
    assert!(!w.config().authz.read_only());
    // The Owner's device key alone can't switch to an unknown version.
    let by_owner = w.cfg_push_bare("owner", SessionEvent::WorkspaceRulesSet { version: hive_core::authz::RULES_SUPPORTED + 1, presence: None });
    assert_eq!(w.verdicts()[&by_owner], Verdict::Denied);
    assert!(!w.config().authz.read_only(), "a switch without presence froze the workspace");
    w.chat_push("mara", SessionEvent::ProposalUpserted { proposal: proposal(Uuid::from_u128(0xF1), 1, ProposalStatus::Open) });
    let s = w.state();
    assert_eq!(s.proposals.len(), 1);
    assert!(!s.authz.read_only());
    // Version 1 doesn't exist, even with the Owner's presence.
    let v1 = w.cfg_push("owner", SessionEvent::WorkspaceRulesSet { version: 1, presence: None });
    assert_eq!(w.verdicts()[&v1], Verdict::Denied);
    // With presence, an unknown version makes this build read-only.
    let ok = w.cfg_push("owner", SessionEvent::WorkspaceRulesSet { version: hive_core::authz::RULES_SUPPORTED + 1, presence: None });
    assert_eq!(w.verdicts()[&ok], Verdict::Allowed);
    assert!(w.config().authz.read_only());
}

// ---- upgrade refold ----

/// An event of a kind an older build didn't know is stored as received (see
/// raw_bytes.rs); once the build knows the kind, the stored bytes fold with
/// their real meaning.
#[test]
fn upgrade_refold_recovers_unknown_events() {
    let mut w = World::new();
    let pid = Uuid::from_u128(0xF2);
    let p = proposal(pid, 1, ProposalStatus::Open);
    let d = diff_digest(&p);
    w.chat_push("mara", SessionEvent::ProposalUpserted { proposal: p });
    w.chat_push("owner", vote("owner", pid, WorkspaceRole::Owner, true, Some(d.clone())));
    let applied = w.chat_push("ada", SessionEvent::ProposalApplied { proposal_id: pid, diff_digest: d, commit: Some("abc".into()), at: Timestamp::epoch(), repo_id: None, applied_on: None, base_commit: None, applied_tree_hash: None, request_id: None });
    let wire = serde_json::to_string(w.chat.iter().find(|e| e.event_id == applied).unwrap()).unwrap();

    // An older build: the kind is unknown, and the event folds as a no-op.
    #[derive(serde::Deserialize)]
    #[serde(tag = "kind", rename_all = "camelCase")]
    enum OldEvent {
        ProposalUpserted {},
        #[serde(other)]
        Unknown,
    }
    let payload = serde_json::from_str::<SessionEventEnvelope>(&wire).unwrap();
    let old: OldEvent = serde_json::from_str(payload.raw().unwrap().payload()).unwrap();
    assert!(matches!(old, OldEvent::Unknown));
    let mut as_old = w.clone();
    as_old.chat.retain(|e| e.event_id != applied);
    assert_eq!(as_old.state().proposals[0].status, ProposalStatus::Approved);

    // This build re-parses the same stored bytes and applies it.
    let mut upgraded = as_old.clone();
    upgraded.chat.push(serde_json::from_str(&wire).unwrap());
    assert_eq!(serde_json::to_string(upgraded.chat.last().unwrap()).unwrap(), wire);
    assert_eq!(upgraded.state().proposals[0].status, ProposalStatus::Applied);
}

// ---- membership: every author is a member at the event's position ----

fn message(by: &str, body: &str) -> SessionEvent {
    let mut m = hive_core::ChatMessage::new(hive_core::MessageRole::User, by, body);
    m.actor_identity = Some(ActorIdentity::new(by, by, ActorKind::Human));
    SessionEvent::MessageAppended { message: m }
}

fn enroll(id: &str, invite: Option<Uuid>) -> SessionEvent {
    SessionEvent::MemberAdded { member: member(id, WorkspaceRole::Contributor), invite }
}

fn issue(invite: Uuid) -> SessionEvent {
    SessionEvent::JoinApproved { subject: hive_core::authorization::invite_subject(invite) }
}

proptest! {
    /// A keyholder who isn't on the roster (eve) posts: whatever the order of
    /// delivery and whatever frontier she names, no device folds her message;
    /// members' messages around it all fold.
    #[test]
    fn prop_non_member_message_is_dropped_everywhere(
        posts in prop::collection::vec((0usize..5, any::<bool>()), 1..16),
        seed in any::<u64>(),
    ) {
        let mut w = World::new();
        let mut expected = 0;
        for (i, (who, stale)) in posts.iter().enumerate() {
            let by = PEOPLE[*who];
            let body = format!("m{i}");
            if *stale {
                // Name the very first config position (before anyone joined).
                let first = w.cfg.iter().map(|e| CfgFrontier { lamport: e.lamport, event_id: e.event_id }).min();
                w.chat_at(by, message(by, &body), first);
                if by == "owner" { expected += 1; }
            } else {
                w.chat_push(by, message(by, &body));
                if by != "eve" { expected += 1; }
            }
        }
        let mut shuffled = w.clone();
        let mut rng = seed | 1;
        for i in (1..shuffled.chat.len()).rev() {
            rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17;
            shuffled.chat.swap(i, (rng % (i as u64 + 1)) as usize);
        }
        for s in [w.state(), shuffled.state()] {
            prop_assert!(s.messages.iter().all(|m| m.author != "eve"), "a non-member's message folded");
            prop_assert_eq!(s.messages.len(), expected);
        }
    }

    /// Removal is not undone by the removed: whatever invites exist (unused
    /// ones included), an ex-member's self-enrollment after their removal is
    /// refused, and their later posts never fold.
    #[test]
    fn prop_ex_member_self_enrollment_after_removal_is_refused(invites in 0usize..4, post_first in any::<bool>()) {
        let mut w = World::new();
        w.chat_push("mara", message("mara", "while a member"));
        w.cfg_push("owner", SessionEvent::MemberRemoved { member_id: "mara".into(), watermark: Vec::new() });
        let mut ids = Vec::new();
        for _ in 0..invites {
            let id = Uuid::new_v4();
            w.cfg_push("owner", issue(id));
            ids.push(id);
        }
        if post_first {
            w.chat_push("mara", message("mara", "after removal"));
        }
        for id in ids.iter().copied().map(Some).chain([None]) {
            w.cfg_push("mara", enroll("mara", id));
        }
        w.chat_push("mara", message("mara", "after re-enrolling"));
        prop_assert!(w.config().role_of("mara") == WorkspaceRole::Viewer && !w.config().members.iter().any(|m| m.id == "mara"));
        let s = w.state();
        let bodies: Vec<&str> = s.messages.iter().map(|m| m.body.as_str()).collect();
        prop_assert_eq!(bodies, vec!["while a member"], "only the pre-removal post stands");
    }
}

#[test]
fn an_invited_member_enrolls_once_and_the_invite_is_spent() {
    let mut w = World::new();
    let id = Uuid::new_v4();
    // Without an issued invite, key possession alone: refused.
    let early = w.cfg_push("joe", enroll("joe", Some(id)));
    // A non-Admin can't issue one either.
    let by_mara = w.cfg_push("mara", issue(id));
    let joe_post_before = w.chat_push("joe", message("joe", "not yet"));
    w.cfg_push("ada", issue(id));
    let ok = w.cfg_push("joe", enroll("joe", Some(id)));
    let joe_post = w.chat_push("joe", message("joe", "hello"));
    // The same invite admits no one else.
    let reuse = w.cfg_push("zed", enroll("zed", Some(id)));
    // Admin-level self-enrollment with an invite: refused.
    let id2 = Uuid::new_v4();
    w.cfg_push("owner", issue(id2));
    let mut admin = member("kim", WorkspaceRole::Admin);
    admin.role = WorkspaceRole::Admin;
    let greedy = w.cfg_push("kim", SessionEvent::MemberAdded { member: admin, invite: Some(id2) });
    let v = w.verdicts();
    assert_eq!(v[&early], Verdict::Denied);
    assert_eq!(v[&by_mara], Verdict::Denied);
    assert_eq!(v[&joe_post_before], Verdict::Denied);
    assert_eq!(v[&ok], Verdict::Allowed);
    assert_eq!(v[&joe_post], Verdict::Allowed);
    assert_eq!(v[&reuse], Verdict::Denied);
    assert_eq!(v[&greedy], Verdict::Denied);
    assert_eq!(w.config().role_of("joe"), WorkspaceRole::Contributor);
    assert_eq!(w.state().messages.iter().map(|m| m.body.as_str()).collect::<Vec<_>>(), vec!["hello"]);
}

#[test]
fn a_chat_founded_by_a_non_member_does_not_exist() {
    let mut w = World::new();
    let mut seed = ChatSession::new("eve's", w.wid, "anthropic");
    seed.creator_actor_id = "eve".into();
    seed.members = vec![member("eve", WorkspaceRole::Owner)];
    let mut forged = World { chat: Vec::new(), sid: seed.id, ..w.clone() };
    forged.chat_push("eve", SessionEvent::SessionSnapshot { session: Box::new(seed) });
    forged.chat_push("eve", message("eve", "hi"));
    let t = forged.timeline();
    assert!(project_authorized(&forged.chat, None, None, WorkspaceAuthz::Timeline(&t)).is_none());
    // A member's chat does.
    w.chat_push("mara", message("mara", "hi"));
    assert_eq!(w.state().messages.len(), 1);
}

// ---- A3: agent identities ----

struct AgentSetup {
    agent_id: Uuid,
    key_id: Uuid,
}

/// `owner_name` owns an agent bound to their device `host`, which certifies a
/// key for it on the config log.
fn agent_world(owner_name: &str) -> (World, AgentSetup) {
    use hive_core::crypto::{DeviceCertificate, SigningKeypair};
    let mut w = World::new();
    let acct = account_key(owner_name);
    let dev = SigningKeypair::from_seed(&[owner_name.len() as u8 + 80; 32]).unwrap();
    let host = Uuid::new_v5(&Uuid::NAMESPACE_OID, format!("{owner_name}-device").as_bytes());
    w.cfg_push(owner_name, SessionEvent::AccountKeyRegistered { account_id: account(owner_name), signing_public_key: acct.public_key_bytes().to_vec() });
    let cert = DeviceCertificate::issue(&acct, account(owner_name), host, &dev.public_key_bytes(), Timestamp::epoch());
    w.cfg_push(owner_name, SessionEvent::DeviceCertificateAdded { certificate: cert });
    let mut agent = hive_core::WorkspaceAgent::new("coder", "gw");
    agent.owner_actor_id = owner_name.into();
    let agent_id = agent.id;
    w.cfg_push("owner", SessionEvent::AgentUpserted { agent: { let mut a = agent.clone(); a.host_id = host.to_string(); a } });
    let akp = SigningKeypair::from_seed(&[7; 32]).unwrap();
    let acert = hive_core::AgentKeyCertificate::issue(&dev, host, agent_id, &akp.public_key_bytes(), Timestamp::epoch());
    let key_id = acert.key_id;
    let id = w.cfg_push(owner_name, SessionEvent::AgentKeyCertified { cert: acert });
    w.cfg.iter_mut().find(|e| e.event_id == id).unwrap().signer_device_id = Some(host);
    (w, AgentSetup { agent_id, key_id })
}

impl World {
    /// A chat event signed by an agent's own key.
    fn agent_push(&mut self, a: &AgentSetup, e: SessionEvent) -> Uuid {
        let id = self.chat_push("x", e);
        let env = self.chat.iter_mut().find(|x| x.event_id == id).unwrap();
        let mut actor = ActorIdentity::new(a.agent_id.to_string(), "coder", ActorKind::Agent);
        actor.device_id = Some(a.key_id);
        let f = env.actor_stamp.as_ref().unwrap().cfg_frontier;
        env.actor_stamp = Some(ActorStamp { actor, recorded_at: Timestamp::epoch(), cfg_frontier: f, presence: None, runtime_effect: false, prev: None, sig_alg: Default::default() });
        env.signer_device_id = Some(a.key_id);
        id
    }
}

fn agent_reply(a: &AgentSetup, id: u128) -> (Uuid, SessionEvent) {
    let mut m = hive_core::ChatMessage::new(hive_core::MessageRole::Assistant, "coder", "on it");
    m.id = Uuid::from_u128(id);
    m.responder_id = Some(a.agent_id);
    (m.id, SessionEvent::MessageAppended { message: m })
}

#[test]
fn agent_key_certification_is_checked() {
    let (w, a) = agent_world("mara");
    assert!(w.timeline().current().valid_agent_key(a.key_id).is_some());
    // A certificate from a device the agent isn't bound to is refused.
    let mut forged = w.clone();
    let akp = hive_core::crypto::SigningKeypair::generate().unwrap();
    let other = hive_core::crypto::SigningKeypair::generate().unwrap();
    let stranger = Uuid::new_v4();
    let c = hive_core::AgentKeyCertificate::issue(&other, stranger, a.agent_id, &akp.public_key_bytes(), Timestamp::epoch());
    let id = forged.cfg_push("mara", SessionEvent::AgentKeyCertified { cert: c.clone() });
    forged.cfg.iter_mut().find(|e| e.event_id == id).unwrap().signer_device_id = Some(stranger);
    assert_eq!(forged.verdicts()[&id], Verdict::Denied);
    assert!(forged.timeline().current().valid_agent_key(c.key_id).is_none());
}

#[test]
fn an_agent_signs_its_own_reply() {
    let (mut w, a) = agent_world("mara");
    let (mid, reply) = agent_reply(&a, 0x51);
    let ok = w.agent_push(&a, reply);
    let chunk = w.agent_push(&a, SessionEvent::MessageCompleted { message_id: mid, body: "done".into() });
    // Not someone else's message.
    let (_, theirs) = {
        let mut m = hive_core::ChatMessage::new(hive_core::MessageRole::Assistant, "hive", "x");
        m.id = Uuid::from_u128(0x52);
        (m.id, SessionEvent::MessageAppended { message: m })
    };
    let other = w.agent_push(&a, theirs);
    let v = w.verdicts();
    assert_eq!((v[&ok], v[&chunk], v[&other]), (Verdict::Allowed, Verdict::Allowed, Verdict::Denied));
    assert_eq!(w.state().messages.iter().find(|m| m.id == mid).unwrap().body, "done");
}

#[test]
fn agent_tier_never_votes_on_human_gate() {
    let (mut w, a) = agent_world("owner");
    let pid = Uuid::from_u128(0x61);
    w.chat_push("mara", SessionEvent::ProposalUpserted { proposal: proposal(pid, 1, ProposalStatus::Open) });
    let me = a.agent_id.to_string();
    let v = w.agent_push(&a, vote(&me, pid, WorkspaceRole::Owner, true, None));
    assert_eq!(w.verdicts()[&v], Verdict::Denied);
    assert_eq!(w.state().proposals[0].status, ProposalStatus::Open);
}

#[test]
fn agent_tier_never_applies() {
    let (mut w, a) = agent_world("owner");
    let pid = Uuid::from_u128(0x62);
    let p = proposal(pid, 1, ProposalStatus::Open);
    let d = diff_digest(&p);
    w.chat_push("mara", SessionEvent::ProposalUpserted { proposal: p });
    w.chat_push("owner", vote("owner", pid, WorkspaceRole::Owner, true, Some(d.clone())));
    let applied = w.agent_push(&a, SessionEvent::ProposalApplied { proposal_id: pid, diff_digest: d, commit: None, at: Timestamp::epoch(), repo_id: None, applied_on: None, base_commit: None, applied_tree_hash: None, request_id: None });
    assert_eq!(w.verdicts()[&applied], Verdict::Denied);
    assert_eq!(w.state().proposals[0].status, ProposalStatus::Approved);
}

#[test]
fn agent_tier_cannot_emit_admin_events() {
    let (mut w, a) = agent_world("owner");
    let set = ProposalSet::new("Q", "planner", vec![]);
    let set_id = set.id;
    w.chat_push("owner", SessionEvent::ProposalSetOpened { set: Box::new(set) });
    let chat_events = [
        SessionEvent::ProposalSetDecided { set_id, winner: None, at: Timestamp::epoch() },
        SessionEvent::ProposalSetTuned { set_id, agent_quorum: 1, required_approvals: 0, quality_bar: 0 },
        SessionEvent::WorkflowDefinitionsUpdated { definitions: vec![] },
        SessionEvent::SessionArchivedChanged { archived: true },
    ];
    for e in chat_events {
        let id = w.agent_push(&a, e);
        assert_eq!(w.verdicts()[&id], Verdict::Denied);
    }
    // Config-log governance signed by the agent key: refused too.
    for e in [
        SessionEvent::MemberAdded { member: member("ghost", WorkspaceRole::Admin), invite: None },
        SessionEvent::WorkspaceRulesSet { version: 1, presence: None },
        SessionEvent::ChatLockdownCleared { chat_id: w.sid },
    ] {
        let id = w.cfg_push("x", e);
        let env = w.cfg.iter_mut().find(|x| x.event_id == id).unwrap();
        let mut actor = ActorIdentity::new(a.agent_id.to_string(), "coder", ActorKind::Agent);
        actor.device_id = Some(a.key_id);
        env.actor_stamp = Some(ActorStamp { actor, recorded_at: Timestamp::epoch(), cfg_frontier: None, presence: None, runtime_effect: false, prev: None, sig_alg: Default::default() });
        env.signer_device_id = Some(a.key_id);
        assert_eq!(w.verdicts()[&id], Verdict::Denied);
    }
    // What the tier allows on the config log does fold (a lockdown only tightens).
    let id = w.cfg_push("x", SessionEvent::ChatLockdownRaised { chat_id: w.sid, lockdown: lockdown(0x77, "coder") });
    let env = w.cfg.iter_mut().find(|x| x.event_id == id).unwrap();
    let mut actor = ActorIdentity::new(a.agent_id.to_string(), "coder", ActorKind::Agent);
    actor.device_id = Some(a.key_id);
    env.actor_stamp = Some(ActorStamp { actor, recorded_at: Timestamp::epoch(), cfg_frontier: None, presence: None, runtime_effect: false, prev: None, sig_alg: Default::default() });
    env.signer_device_id = Some(a.key_id);
    assert_eq!(w.verdicts()[&id], Verdict::Allowed);
}

#[test]
fn agent_cert_invalid_after_rebind() {
    let (mut w, a) = agent_world("mara");
    let (_, before) = agent_reply(&a, 0x71);
    let ok = w.agent_push(&a, before);
    // An Admin rebinds the agent to another host: the old key stops counting
    // from that position on.
    let mut agent = w.config().workspace_agents.iter().find(|x| x.id == a.agent_id).unwrap().clone();
    agent.host_id = "worker-2".into();
    w.cfg_push("ada", SessionEvent::AgentUpserted { agent });
    let (_, after) = agent_reply(&a, 0x72);
    let late = w.agent_push(&a, after);
    let v = w.verdicts();
    assert_eq!((v[&ok], v[&late]), (Verdict::Allowed, Verdict::Denied));
    assert!(w.timeline().current().valid_agent_key(a.key_id).is_none());
}

#[test]
fn agent_of_viewer_capped_at_viewer() {
    let (mut w, a) = agent_world("val");
    // val is a Viewer: her agent may raise a lockdown (Viewer floor) but not
    // file a proposal (Contributor floor), which a Contributor's agent may.
    let mut p = proposal(Uuid::from_u128(0x81), 1, ProposalStatus::Open);
    p.author_actor_id = String::new(); // agent output is filed authorless
    p.producing_agent_id = Some(a.agent_id);
    let filed = w.agent_push(&a, SessionEvent::ProposalUpserted { proposal: p.clone() });
    assert_eq!(w.verdicts()[&filed], Verdict::Denied);
    let (mut w2, a2) = agent_world("mara");
    p.producing_agent_id = Some(a2.agent_id);
    let filed2 = w2.agent_push(&a2, SessionEvent::ProposalUpserted { proposal: p });
    assert_eq!(w2.verdicts()[&filed2], Verdict::Allowed);
    // Once its owner leaves, the agent's events stop counting.
    w2.cfg_push("owner", SessionEvent::MemberRemoved { member_id: "mara".into(), watermark: Vec::new() });
    let (_, r) = agent_reply(&a2, 0x82);
    let after = w2.agent_push(&a2, r);
    assert_eq!(w2.verdicts()[&after], Verdict::Denied);
}

#[test]
fn rules_v2_tier_applies_to_host_signed_agent_output() {
    // Under rules v2 a host-signed event that speaks for an agent is judged
    // with the Agent tier (the tier itself here; the fold under v2, which
    // needs a passkey-confirmed switch, is in hive-runtime's presence_tests).
    use hive_core::authz::agent::{agent_authored, tier_allows};
    let agent = Uuid::new_v4();
    let mut p = proposal(Uuid::from_u128(0x91), 1, ProposalStatus::Open);
    p.author_actor_id = String::new(); // agent output is filed authorless
    p.producing_agent_id = Some(agent);
    let upsert = SessionEvent::ProposalUpserted { proposal: p.clone() };
    assert_eq!(agent_authored(&upsert), Some(agent));
    let fold = ChatSession::new("c", Uuid::nil(), "x");
    assert!(tier_allows(&upsert, agent, Uuid::nil(), &fold).is_ok());
    p.approvals.push(ProposalApproval { actor_id: "owner".into(), role: WorkspaceRole::Owner, approved: true, created_at: Timestamp::epoch(), diff_digest: None });
    assert!(tier_allows(&SessionEvent::ProposalUpserted { proposal: p }, agent, Uuid::nil(), &fold).is_err(), "an agent never asserts approvals");
}

/// P0.6 (A2 fold rule): a `ligus serve` box enrolled as its owner's device
/// signs as the owner without the owner being there. Without the owner's
/// passkey presence (B3) its vote doesn't count toward the human gate on any
/// device (the box's API asks the phone for one); the same
/// owner's vote from a device holding their account key does. The box having
/// once published its own identity under its own account changes nothing.
#[test]
fn delegated_host_vote_not_counted_for_human_gate() {
    use hive_core::crypto::{DeviceCertificate, SigningKeypair};
    let mut w = World::new();
    let owner_acct = account("owner");
    let owner_key = account_key("owner");
    let (desk_key, box_key, box_acct_key) =
        (SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap());
    let (desk, bx, box_acct) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let cert = |k: &SigningKeypair, acct, dev, dk: &SigningKeypair| SessionEvent::DeviceCertificateAdded {
        certificate: DeviceCertificate::issue(k, acct, dev, &dk.public_key_bytes(), Timestamp::epoch()),
    };
    let signed = |w: &mut World, by: &str, e: SessionEvent, signer: Uuid| {
        w.cfg_push(by, e);
        w.cfg.last_mut().unwrap().signer_device_id = Some(signer);
    };
    // The owner's desktop publishes its own identity.
    signed(&mut w, "owner", SessionEvent::AccountKeyRegistered {
        account_id: owner_acct,
        signing_public_key: owner_key.public_key_bytes().to_vec(),
    }, desk);
    signed(&mut w, "owner", cert(&owner_key, owner_acct, desk, &desk_key), desk);
    // The box first published an identity of its own...
    signed(&mut w, "sff", SessionEvent::AccountKeyRegistered {
        account_id: box_acct,
        signing_public_key: box_acct_key.public_key_bytes().to_vec(),
    }, bx);
    signed(&mut w, "sff", cert(&box_acct_key, box_acct, bx, &box_key), bx);
    // ...then the owner's desktop enrolled it as one of the owner's devices.
    signed(&mut w, "owner", cert(&owner_key, owner_acct, bx, &box_key), desk);

    let roster = w.timeline().current().clone();
    assert!(roster.devices[&bx].delegated_for(owner_acct));
    assert!(!roster.devices[&bx].delegated_for(box_acct), "the box holds its own account key");
    assert!(!roster.devices[&desk].delegated_for(owner_acct));

    let pid = Uuid::from_u128(0xD1);
    w.chat_push("owner", SessionEvent::ProposalUpserted { proposal: proposal(pid, 1, ProposalStatus::Open) });
    let digest = Some(diff_digest(&proposal(pid, 1, ProposalStatus::Open)));
    // (No presence: the owner isn't there; the box signs alone.)
    let from_box = w.chat_push_bare("owner", vote("owner", pid, WorkspaceRole::Owner, true, digest.clone()));
    w.chat.last_mut().unwrap().signer_device_id = Some(bx);
    let p = w.state().proposals.into_iter().find(|p| p.id == pid).unwrap();
    assert!(p.approvals.is_empty(), "the delegated host's vote counted: {:?}", p.approvals);
    assert_eq!(p.status, ProposalStatus::Open);
    assert_eq!(w.verdicts()[&from_box], Verdict::Denied);

    // The owner, at their desktop.
    let from_desk = w.chat_push("owner", vote("owner", pid, WorkspaceRole::Owner, true, digest));
    w.chat.last_mut().unwrap().signer_device_id = Some(desk);
    let p = w.state().proposals.into_iter().find(|p| p.id == pid).unwrap();
    assert_eq!(p.approvals.len(), 1);
    assert_eq!(p.status, ProposalStatus::Approved);
    assert_eq!(w.verdicts()[&from_desk], Verdict::Allowed);
}

/// Hardening A-1: a delegated host can't shed its delegation by publishing
/// its owner's certificate for itself again (any device can re-publish a cert
/// it was handed). Delegation is decided by the first certified publication of
/// the (device, account) certificate, and stays.
#[test]
fn delegated_host_republishing_its_cert_stays_delegated() {
    use hive_core::crypto::{DeviceCertificate, SigningKeypair};
    let mut w = World::new();
    let owner_acct = account("owner");
    let owner_key = account_key("owner");
    let (desk_key, box_key) = (SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap());
    let (desk, bx) = (Uuid::new_v4(), Uuid::new_v4());
    let box_cert = DeviceCertificate::issue(&owner_key, owner_acct, bx, &box_key.public_key_bytes(), Timestamp::epoch());
    let signed = |w: &mut World, e: SessionEvent, signer: Uuid| {
        w.cfg_push("owner", e);
        w.cfg.last_mut().unwrap().signer_device_id = Some(signer);
    };
    signed(&mut w, SessionEvent::AccountKeyRegistered {
        account_id: owner_acct,
        signing_public_key: owner_key.public_key_bytes().to_vec(),
    }, desk);
    signed(&mut w, SessionEvent::DeviceCertificateAdded {
        certificate: DeviceCertificate::issue(&owner_key, owner_acct, desk, &desk_key.public_key_bytes(), Timestamp::epoch()),
    }, desk);
    // The desktop enrolls the box...
    signed(&mut w, SessionEvent::DeviceCertificateAdded { certificate: box_cert.clone() }, desk);
    assert!(w.timeline().current().devices[&bx].delegated_for(owner_acct));
    // ...and the box re-publishes the very certificate it was handed.
    signed(&mut w, SessionEvent::DeviceCertificateAdded { certificate: box_cert }, bx);
    assert!(w.timeline().current().devices[&bx].delegated_for(owner_acct), "the box shed its delegation");

    let pid = Uuid::from_u128(0xD7);
    w.chat_push("owner", SessionEvent::ProposalUpserted { proposal: proposal(pid, 1, ProposalStatus::Open) });
    let digest = Some(diff_digest(&proposal(pid, 1, ProposalStatus::Open)));
    w.chat_push_bare("owner", vote("owner", pid, WorkspaceRole::Owner, true, digest));
    w.chat.last_mut().unwrap().signer_device_id = Some(bx);
    let p = w.state().proposals.into_iter().find(|p| p.id == pid).unwrap();
    assert!(p.approvals.is_empty(), "the delegated host's vote counted: {:?}", p.approvals);

    // And nobody can make the desktop delegated by re-publishing its cert
    // from another device (the desktop's own publication came first).
    let desk_cert = DeviceCertificate::issue(&owner_key, owner_acct, desk, &desk_key.public_key_bytes(), Timestamp::epoch());
    signed(&mut w, SessionEvent::DeviceCertificateAdded { certificate: desk_cert }, bx);
    assert!(!w.timeline().current().devices[&desk].delegated_for(owner_acct));
}

/// Hardening A-2: an unbound agent runs on its owner's device, so its owner
/// is its host binding. A Contributor can't create an agent owned by someone
/// else (it would run on their device, with their runtimes and keys).
#[test]
fn contributor_cannot_create_an_agent_owned_by_someone_else() {
    let mut w = World::new();
    let mut theirs = hive_core::WorkspaceAgent::new("helper", "claude-code");
    theirs.owner_actor_id = "owner".into();
    let one = w.cfg_push("mara", SessionEvent::AgentUpserted { agent: theirs.clone() });
    let mut also = hive_core::WorkspaceAgent::new("helper2", "claude-code");
    also.owner_actor_id = "ada".into();
    let list = w.cfg_push("mara", SessionEvent::AgentRosterUpdated { agents: vec![also] });
    // Her own agent: fine. An Admin may create one for someone else.
    let mut mine = hive_core::WorkspaceAgent::new("mine", "claude-code");
    mine.owner_actor_id = "mara".into();
    let own = w.cfg_push("mara", SessionEvent::AgentUpserted { agent: mine });
    let mut for_val = hive_core::WorkspaceAgent::new("vals", "claude-code");
    for_val.owner_actor_id = "val".into();
    let admin = w.cfg_push("ada", SessionEvent::AgentUpserted { agent: for_val });
    let v = w.verdicts();
    assert_eq!((v[&one], v[&list]), (Verdict::Denied, Verdict::Denied));
    assert_eq!((v[&own], v[&admin]), (Verdict::Allowed, Verdict::Allowed));
}
/// Hardening A-3: a chat's creator is Owner of the chat's own roster, but that
/// never grants a workspace role. Workspace-config kinds on a chat log (agent
/// bindings, runtimes) are capped by the author's workspace role at the
/// event's position, so a Contributor can't re-bind a workspace agent inside
/// a chat they created (the chat's agents shadow the config log's).
#[test]
fn chat_creator_cannot_rebind_a_workspace_agent_in_their_chat() {
    let mut w = World::new();
    let mut agent = hive_core::WorkspaceAgent::new("coder", "gw");
    agent.owner_actor_id = "owner".into();
    agent.host_id = "sff".into();
    w.cfg_push("owner", SessionEvent::AgentUpserted { agent: agent.clone() });
    // mara (Contributor) founds a chat of her own.
    let mut seed = ChatSession::new("mara's", w.wid, "anthropic");
    seed.creator_actor_id = "mara".into();
    seed.members = vec![member("mara", WorkspaceRole::Owner)];
    let mut m = World { chat: Vec::new(), sid: seed.id, ..w.clone() };
    m.chat_push("mara", SessionEvent::SessionSnapshot { session: Box::new(seed) });
    let mut moved = agent.clone();
    moved.host_id = String::new();
    moved.owner_actor_id = "mara".into();
    moved.runtime_id = "other".into();
    let rebind = m.chat_push("mara", SessionEvent::AgentUpserted { agent: moved });
    let rt = hive_core::WorkspaceRuntime::new("gw", "gw", hive_core::ModelProviderKind::Anthropic, "m");
    let runtime = m.chat_push("mara", SessionEvent::WorkspaceRuntimeUpserted { runtime: rt });
    let v = m.verdicts();
    assert_eq!(v[&rebind], Verdict::Denied, "a chat-level rebind of a workspace agent folded");
    assert_eq!(v[&runtime], Verdict::Denied, "a Contributor defined a runtime in her chat");
    let s = m.state();
    assert!(s.workspace_agents.is_empty() && s.workspace_runtimes.is_empty());
}

/// Hardening A-4 (containment of the backdated-frontier residual): a demoted
/// Admin keeps naming a config frontier from before the demotion. A vote (or
/// an apply) can't name a frontier older than the one the content it judges
/// was filed at, so the old role never weighs on a proposal filed after the
/// demotion reached its author.
#[test]
fn a_backdated_vote_cannot_predate_the_content_it_judges() {
    let mut w = World::new();
    let before = w.head();
    w.cfg_push("owner", SessionEvent::MemberRoleChanged {
        change: MemberRoleChange { member_id: "ada".into(), old_role: WorkspaceRole::Admin, new_role: WorkspaceRole::Viewer }, watermark: Vec::new(),
    });
    let pid = Uuid::from_u128(0xA4);
    let mut p = proposal(pid, 1, ProposalStatus::Open);
    p.approval_role_floor = WorkspaceRole::Admin;
    w.chat_push("mara", SessionEvent::ProposalUpserted { proposal: p.clone() });
    let digest = Some(diff_digest(&p));
    let stale = w.chat_at("ada", vote("ada", pid, WorkspaceRole::Admin, true, digest.clone()), before);
    let v = w.verdicts();
    assert_eq!(v[&stale], Verdict::Denied, "a vote backdated before its proposal counted");
    let got = w.state().proposals.into_iter().find(|x| x.id == pid).unwrap();
    assert_eq!(got.status, ProposalStatus::Open, "approved by a demoted Admin: {:?}", got.approvals);
    // At the current head she is judged at her real role (a Viewer can't vote).
    let honest = w.chat_push("ada", vote("ada", pid, WorkspaceRole::Admin, true, digest.clone()));
    assert_eq!(w.verdicts()[&honest], Verdict::Denied);
    // A real Admin's vote at the head counts.
    w.cfg_push("owner", SessionEvent::MemberRoleChanged {
        change: MemberRoleChange { member_id: "val".into(), old_role: WorkspaceRole::Viewer, new_role: WorkspaceRole::Admin }, watermark: Vec::new(),
    });
    let admin = w.chat_push("val", vote("val", pid, WorkspaceRole::Admin, true, digest));
    assert_eq!(w.verdicts()[&admin], Verdict::Allowed);
    assert_ne!(w.state().proposals.into_iter().find(|x| x.id == pid).unwrap().status, ProposalStatus::Open);
}

/// Hardening A-5 (containment of "a Contributor lowers required_approvals"):
/// an Admin's core path zone sets the review bar, but it was applied only by
/// honest authoring code, over the declared file list. A proposal that
/// touches a core path (by its diff, whatever it declares) is held to the
/// zone's bar by the fold; it can only be raised, never lowered.
#[test]
fn core_zone_bar_holds_in_the_fold() {
    let mut w = World::new();
    w.cfg_push("owner", SessionEvent::FlowRecorded { record: zone(0x20, "src/") });
    let pid = Uuid::from_u128(0xA5);
    let mut p = ActionProposal::file_diff("p", "mara", "diff --git a/src/core.rs b/src/core.rs\n+x\n".into(), vec!["docs/a.md".into()]);
    p.id = pid;
    p.created_at = Timestamp::epoch();
    p.required_approvals = 1;
    p.approval_role_floor = WorkspaceRole::Viewer;
    w.chat_push("mara", SessionEvent::ProposalUpserted { proposal: p.clone() });
    let digest = Some(diff_digest(&p));
    w.chat_push("val", vote("val", pid, WorkspaceRole::Viewer, true, digest.clone()));
    w.chat_push("owner", vote("owner", pid, WorkspaceRole::Owner, true, digest.clone()));
    let got = w.state().proposals.into_iter().find(|x| x.id == pid).unwrap();
    assert_eq!((got.required_approvals, got.approval_role_floor), (2, WorkspaceRole::Admin));
    assert_eq!(got.status, ProposalStatus::Open, "a core change passed on a lowered bar");
    w.chat_push("ada", vote("ada", pid, WorkspaceRole::Admin, true, digest));
    assert_eq!(w.state().proposals.into_iter().find(|x| x.id == pid).unwrap().status, ProposalStatus::Approved);
}

/// Hardening A-6: an agent files proposals as itself (or authorless, naming
/// itself as producer), never under a human's name: that name is what the
/// self-approval guard and the reviewers read.
#[test]
fn agent_cannot_file_a_proposal_under_a_humans_name() {
    let (mut w, a) = agent_world("mara");
    let mut forged = ActionProposal::file_diff("p", "ada", "diff --git a/f b/f\n+1\n".into(), vec!["f".into()]);
    forged.producing_agent_id = Some(a.agent_id);
    let as_ada = w.agent_push(&a, SessionEvent::ProposalUpserted { proposal: forged });
    let mut other = ActionProposal::file_diff("p", "", "diff --git a/f b/f\n+2\n".into(), vec!["f".into()]);
    other.producing_agent_id = Some(Uuid::new_v4());
    let as_other_agent = w.agent_push(&a, SessionEvent::ProposalUpserted { proposal: other });
    let mut honest = ActionProposal::file_diff("p", "", "diff --git a/f b/f\n+3\n".into(), vec!["f".into()]);
    honest.producing_agent_id = Some(a.agent_id);
    let ok = w.agent_push(&a, SessionEvent::ProposalUpserted { proposal: honest });
    let v = w.verdicts();
    assert_eq!((v[&as_ada], v[&as_other_agent], v[&ok]), (Verdict::Denied, Verdict::Denied, Verdict::Allowed));
}

/// Hardening A-11: a message is its author's. Any member could rewrite
/// another's message (a second `MessageAppended` with its id, a chunk, or a
/// `MessageCompleted`) or remove it; the gate on an agent turn then read one
/// body while every chat showed another. Only the author who first appended a
/// message continues it; removal is the author's or an Admin's.
#[test]
fn only_its_author_rewrites_a_message() {
    let mut w = World::new();
    let (ada_msg, mid) = {
        let SessionEvent::MessageAppended { message } = message("ada", "ship it") else { unreachable!() };
        (message.clone(), message.id)
    };
    w.chat_push("ada", SessionEvent::MessageAppended { message: ada_msg.clone() });
    let mut forged = ada_msg.clone();
    forged.body = "rm -rf everything".into();
    let reappend = w.chat_push("mara", SessionEvent::MessageAppended { message: forged });
    let chunk = w.chat_push("mara", SessionEvent::MessageChunkReceived { message_id: mid, chunk: " and more".into() });
    let complete = w.chat_push("mara", SessionEvent::MessageCompleted { message_id: mid, body: "rewritten".into() });
    let remove = w.chat_push("mara", SessionEvent::MessageRemoved { message_id: mid });
    let own = w.chat_push("ada", SessionEvent::MessageCompleted { message_id: mid, body: "ship it now".into() });
    let v = w.verdicts();
    assert_eq!((v[&reappend], v[&chunk], v[&complete], v[&remove]), (Verdict::Denied, Verdict::Denied, Verdict::Denied, Verdict::Denied));
    assert_eq!(v[&own], Verdict::Allowed);
    assert_eq!(w.state().messages.iter().find(|m| m.id == mid).unwrap().body, "ship it now");
    // An Admin may remove a member's message (moderation).
    let SessionEvent::MessageAppended { message: m2 } = message("mara", "oops") else { unreachable!() };
    let m2id = m2.id;
    w.chat_push("mara", SessionEvent::MessageAppended { message: m2 });
    let moderated = w.chat_push("ada", SessionEvent::MessageRemoved { message_id: m2id });
    assert_eq!(w.verdicts()[&moderated], Verdict::Allowed);
}

/// P0.5 summaries are synced by any Contributor, so the fold must keep them
/// bounded and honest: (1) a summary's `hostId` is the device that signed
/// it (another member can't replace a host's day, which is keyed by host);
/// (2) counts and cost are clamped, so one event can't make every scorer
/// expand billions of samples or push an agent's cost below zero; (3) the
/// number of distinct agent keys is capped.
#[test]
fn outcome_summaries_are_bounded_and_bound_to_their_host() {
    use hive_core::flow::score::{summary_outcomes, summary_turns_spent};
    use hive_core::flow::{MAX_SUMMARY_AGENTS, MAX_SUMMARY_COUNT};
    let mut w = World::new();
    let push_signed = |w: &mut World, by: &str, signer: Uuid, v: serde_json::Value| {
        let rec: FlowRecord = serde_json::from_value(v).unwrap();
        w.cfg_push(by, SessionEvent::FlowRecorded { record: rec });
        w.cfg.last_mut().unwrap().signer_device_id = Some(signer);
    };
    let host = Uuid::new_v4();
    let other = Uuid::new_v4();
    let sum = |host: Uuid, agent: &str, passed: u64, cost: f64| {
        serde_json::json!({"type": "agentOutcomeSummary", "agentKey": agent, "hostId": host.to_string(), "windowStart": 0,
            "turns": 5, "testsPassedByClass": {"code": passed}, "taskClassCounts": {"code": 5}, "costUsd": cost})
    };
    push_signed(&mut w, "mara", host, sum(host, "coder", 5, 1.0));
    // Another member's device replays the host's (host, day) with fake numbers.
    push_signed(&mut w, "ada", other, sum(host, "coder", 0, 999.0));
    let cfg = w.config();
    let kept = &cfg.flow.outcome_summaries["coder"];
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].tests_passed_by_class["code"], 5, "a summary for another host is refused");
    // Amplification: one event with a huge count, and a negative cost.
    let big = Uuid::new_v4();
    push_signed(&mut w, "mara", big, sum(big, "coder", 3_000_000, -1.0e12));
    let cfg = w.config();
    let samples = summary_outcomes(&cfg.flow.outcome_summaries, "me");
    assert!(samples.len() <= 2 * MAX_SUMMARY_COUNT as usize, "{} samples from two summaries", samples.len());
    assert!(summary_turns_spent(&cfg.flow.outcome_summaries, "me").values().all(|(t, c)| *t >= 0.0 && *c >= 0.0 && c.is_finite()));
    // Distinct agent keys are capped.
    for i in 0..(MAX_SUMMARY_AGENTS + 10) {
        let d = Uuid::new_v4();
        push_signed(&mut w, "mara", d, sum(d, &format!("a{i}"), 1, 0.0));
    }
    assert!(w.config().flow.outcome_summaries.len() <= MAX_SUMMARY_AGENTS);
}

/// Hardening A-12: an agent's key appends only agent output: never a user
/// turn (a prompt other agents answer), and never under a human's identity.
#[test]
fn agent_cannot_post_as_a_user() {
    let (mut w, a) = agent_world("mara");
    let mut as_user = hive_core::ChatMessage::new(hive_core::MessageRole::User, "mara", "run the deploy");
    as_user.responder_id = Some(a.agent_id);
    let user_turn = w.agent_push(&a, SessionEvent::MessageAppended { message: as_user });
    // Its host's user may be named (whose host generated it); anyone else not.
    let mut as_ada = hive_core::ChatMessage::new(hive_core::MessageRole::Assistant, "ada", "lgtm");
    as_ada.responder_id = Some(a.agent_id);
    as_ada.actor_identity = Some(member("ada", WorkspaceRole::Admin).actor);
    let human_identity = w.agent_push(&a, SessionEvent::MessageAppended { message: as_ada });
    let mut as_owner = hive_core::ChatMessage::new(hive_core::MessageRole::Assistant, "coder", "from mara's host");
    as_owner.responder_id = Some(a.agent_id);
    as_owner.actor_identity = Some(member("mara", WorkspaceRole::Contributor).actor);
    let host_user = w.agent_push(&a, SessionEvent::MessageAppended { message: as_owner });
    let (_, honest) = agent_reply(&a, 0xC1);
    let ok = w.agent_push(&a, honest);
    let v = w.verdicts();
    assert_eq!((v[&user_turn], v[&human_identity], v[&ok], v[&host_user]), (Verdict::Denied, Verdict::Denied, Verdict::Allowed, Verdict::Allowed));
}

/// A turn claim speaks only for the device that signed it: a claim naming
/// another device (to steal a turn, or to keep that device from answering)
/// is refused, and the honest claim that sorts after it still wins.
#[test]
fn a_turn_claim_counts_only_from_the_device_it_names() {
    let mut w = World::new();
    let (a, b) = (Uuid::from_u128(0xDA), Uuid::from_u128(0xDB));
    let trigger = Uuid::from_u128(0x77);
    let forged = w.chat_push("mara", SessionEvent::TurnClaimed { trigger_id: trigger, device_id: a });
    w.chat.iter_mut().find(|e| e.event_id == forged).unwrap().signer_device_id = Some(b);
    let honest = w.chat_push("owner", SessionEvent::TurnClaimed { trigger_id: trigger, device_id: b });
    w.chat.iter_mut().find(|e| e.event_id == honest).unwrap().signer_device_id = Some(b);
    let v = w.verdicts();
    assert_eq!(v[&forged], Verdict::Denied);
    assert_eq!(v[&honest], Verdict::Allowed);
    assert_eq!(w.state().turn_claims.get(&trigger), Some(&b));
}

proptest! {
    /// Phase A, item 2: after a member's removal, nothing they sign later
    /// folds, whatever config frontier it claims (backdated before the
    /// removal included), in any delivery order; what they posted before it
    /// (the remover held it) stands.
    #[test]
    fn prop_removed_members_backdated_events_never_fold(
        ops in prop::collection::vec(arb_op(), 0..16),
        who in 1u8..3,
        backs in prop::collection::vec(0u8..12, 1..4),
        seed in any::<u64>(),
    ) {
        let mut w = world(&ops);
        let name = PEOPLE[who as usize];
        let member_before = w.config().members.iter().any(|m| m.id == name)
            && !w.timeline().current().devices.get(&device_of(name).0).is_some_and(|d| d.revoked);
        let before = w.chat_push(name, message(name, "before"));
        let removal = w.cfg_push("owner", SessionEvent::MemberRemoved { member_id: name.into(), watermark: Vec::new() });
        let mut later = Vec::new();
        for b in backs {
            let f = w.back(b);
            later.push(w.chat_at(name, message(name, "after"), Some(f)));
            later.push(w.chat_at(name, vote(name, Uuid::from_u128(PROPS[0]), WorkspaceRole::Owner, true, None), Some(f)));
        }
        let mut shuffled = w.clone();
        let mut rng = seed | 1;
        let mut next = || { rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17; rng };
        for v in [&mut shuffled.cfg, &mut shuffled.chat] {
            for i in (1..v.len()).rev() {
                let j = (next() % (i as u64 + 1)) as usize;
                v.swap(i, j);
            }
        }
        let (a, b) = (w.verdicts(), shuffled.verdicts());
        prop_assert_eq!(&a, &b, "delivery order changed a verdict");
        if member_before && a[&removal] == Verdict::Allowed {
            for id in &later {
                prop_assert_eq!(a[id], Verdict::Denied, "a removed member's later event folded");
            }
            prop_assert_eq!(a[&before], Verdict::Allowed, "the removal took back an event its author held");
        }
    }

    /// A demoted member's later votes count with the new role, however old
    /// a frontier they claim.
    #[test]
    fn prop_demoted_members_backdated_votes_count_at_the_new_role(back in 0u8..10) {
        let mut w = World::new();
        let pid = Uuid::from_u128(PROPS[0]);
        let mut p = proposal(pid, 1, ProposalStatus::Open);
        p.approval_role_floor = WorkspaceRole::Admin;
        w.chat_push("mara", SessionEvent::ProposalUpserted { proposal: p });
        let old = w.back(0);
        w.cfg_push("owner", SessionEvent::MemberRoleChanged {
            change: MemberRoleChange { member_id: "ada".into(), old_role: WorkspaceRole::Admin, new_role: WorkspaceRole::Viewer },
            watermark: Vec::new(),
        });
        let f = if back == 0 { old } else { w.back(back) };
        w.chat_at("ada", vote("ada", pid, WorkspaceRole::Admin, true, None), Some(f));
        let s = w.state();
        let p = s.proposals.iter().find(|p| p.id == pid).unwrap();
        prop_assert!(p.status != ProposalStatus::Approved, "a demoted Admin's backdated vote met an Admin floor");
    }
}

/// A revoked device's later events never fold, even stamped as a member in
/// good standing and at a frontier after the revocation; its earlier ones
/// stand.
#[test]
fn a_revoked_devices_later_events_never_fold() {
    let mut w = World::new();
    let before = w.chat_push("mara", message("mara", "before"));
    let maras = device_of("mara").0;
    w.cfg_push("owner", SessionEvent::DeviceRevoked { device_id: maras, watermark: Vec::new() });
    let after = w.chat_push("mara", message("mara", "after"));
    let old = w.back(5);
    let backdated = w.chat_at("mara", message("mara", "backdated"), Some(old));
    let v = w.verdicts();
    assert_eq!((v[&before], v[&after], v[&backdated]), (Verdict::Allowed, Verdict::Denied, Verdict::Denied));
}

/// A removal written with an empty watermark cuts every event of the member
/// it doesn't name (an author who held none of them), and a forged event
/// can't join the member's chain: it isn't reachable back from the heads.
#[test]
fn a_watermark_covers_only_the_chain_it_names() {
    let mut w = World::new();
    let first = w.chat_push("mara", message("mara", "one"));
    let second = w.chat_push("mara", message("mara", "two"));
    // The remover held only `first`.
    let wm = vec![hive_core::watermark::Watermark { signer: device_of("mara").0, log: w.sid, heads: vec![first], gaps: vec![] }];
    w.cfg_push_raw("owner", SessionEvent::MemberRemoved { member_id: "mara".into(), watermark: wm });
    // A forged event that names `first` as its prev: a fork, not an ancestor.
    let old = w.back(6);
    let forged = w.chat_at("mara", message("mara", "forged"), Some(old));
    {
        let env = w.chat.iter_mut().find(|e| e.event_id == forged).unwrap();
        env.actor_stamp.as_mut().unwrap().prev = Some(first);
    }
    w.reseal(forged);
    let v = w.verdicts();
    assert_eq!((v[&first], v[&second], v[&forged]), (Verdict::Allowed, Verdict::Denied, Verdict::Denied));
}

/// On the config log too: a removed Admin's config event backdated before
/// the removal (and not in the remover's watermark) is denied, in any order.
#[test]
fn a_removed_admins_backdated_config_event_is_denied() {
    let mut w = World::new();
    let removal_at = w.clock;
    w.cfg_push("owner", SessionEvent::MemberRemoved { member_id: "ada".into(), watermark: Vec::new() });
    // Ada adds a confederate with a lamport before her removal.
    let id = w.cfg_push("ada", SessionEvent::MemberAdded { invite: None, member: member("eve", WorkspaceRole::Admin) });
    {
        let env = w.cfg.iter_mut().find(|e| e.event_id == id).unwrap();
        env.lamport = removal_at;
    }
    w.reseal(id);
    assert_eq!(w.verdicts()[&id], Verdict::Denied);
    assert!(!w.config().members.iter().any(|m| m.id == "eve"));
}

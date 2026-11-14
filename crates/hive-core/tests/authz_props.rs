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

/// One workspace: its config log and one chat log.
#[derive(Clone)]
struct World {
    wid: Uuid,
    sid: Uuid,
    /// One lamport clock for both logs (so a suffix sorts after a prefix).
    clock: u64,
    cfg: Vec<SessionEventEnvelope>,
    chat: Vec<SessionEventEnvelope>,
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
        let mut w = Self { wid, sid, clock: 0, cfg: Vec::new(), chat: Vec::new() };
        w.cfg_push("owner", SessionEvent::SessionSnapshot { session: Box::new(seed) });
        w.chat_push("owner", SessionEvent::SessionSnapshot { session: Box::new(chat) });
        for (id, role) in [("ada", WorkspaceRole::Admin), ("mara", WorkspaceRole::Contributor), ("val", WorkspaceRole::Viewer)] {
            w.cfg_push("owner", SessionEvent::MemberAdded { member: member(id, role) });
        }
        w
    }

    fn stamp(by: &str, frontier: Option<CfgFrontier>) -> Option<ActorStamp> {
        let mut actor = ActorIdentity::new(by, by, ActorKind::Human);
        actor.account_id = Some(account(by));
        Some(ActorStamp { actor, recorded_at: Timestamp::epoch(), cfg_frontier: frontier })
    }

    fn head(&self) -> Option<CfgFrontier> {
        self.cfg.iter().map(|e| CfgFrontier { lamport: e.lamport, event_id: e.event_id }).max()
    }

    fn cfg_push(&mut self, by: &str, e: SessionEvent) -> Uuid {
        self.clock += 1;
        let mut env = SessionEventEnvelope::new(workspace_config_session_id(self.wid), self.wid, self.clock as i64, e);
        env.lamport = self.clock;
        env.actor_stamp = Self::stamp(by, self.head());
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
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (1u8..4, 0u8..4).prop_map(|(who, role)| Op::SetRole { who, role }),
        (0u8..5, 0u8..3, 0u8..3, 0u8..4).prop_map(|(by, prop, content, status)| Op::Upsert { by, prop, content, status }),
        (0u8..5, 0u8..3, 0u8..4, any::<bool>(), 0u8..3)
            .prop_map(|(by, prop, declared, approved, digest)| Op::Vote { by, prop, declared, approved, digest }),
        (0u8..5, 0u8..3, 0u8..6).prop_map(|(by, prop, back)| Op::Stale { by, prop, back }),
        (0u8..5, 0u8..3).prop_map(|(by, prop)| Op::Applied { by, prop }),
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
                    change: MemberRoleChange { member_id: id.into(), old_role: old, new_role: role_from(role) },
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
                w.chat_push(PEOPLE[by as usize], SessionEvent::ProposalApplied { proposal_id: pid, diff_digest: d, commit: None, at: Timestamp::epoch() });
            }
        }
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
    /// Appending events never changes the verdict on an event already
    /// decided: for any history L and suffix S, every event of L that L
    /// allowed (denied) is allowed (denied) in L·S. Only a held event may be
    /// decided later.
    #[test]
    fn prop_authz_is_prefix_stable(ops in prop::collection::vec(arb_op(), 0..24), cut in 0usize..24) {
        let cut = cut.min(ops.len());
        let prefix = world(&ops[..cut]);
        let mut whole = prefix.clone();
        play(&mut whole, &ops[cut..], &honest_roles, &honest_status);
        let before = prefix.verdicts();
        let after = whole.verdicts();
        for (id, v) in &before {
            if *v != Verdict::Held {
                prop_assert_eq!(after.get(id), Some(v), "event {} changed verdict", id);
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
        for e in &mut lying.chat {
            if let SessionEvent::ProposalVoteCast { approval, .. } = &mut e.payload {
                approval.role = role_from(salt.wrapping_add(approval.actor_id.len() as u8));
            }
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
        change: MemberRoleChange { member_id: "val".into(), old_role: WorkspaceRole::Viewer, new_role: WorkspaceRole::Admin },
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
    assert_eq!(s.proposals[0].status, ProposalStatus::Open, "the vote waits for the roster");
    assert_eq!(s.authz.held.len(), 2);
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
    let applied = |d: &str| SessionEvent::ProposalApplied { proposal_id: pid, diff_digest: d.into(), commit: None, at: Timestamp::epoch() };
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
    let other = w.cfg_push("mara", SessionEvent::DeviceRevoked { device_id: vals_phone });
    // ...but can revoke her own.
    let own = w.cfg_push("mara", SessionEvent::DeviceRevoked { device_id: maras_laptop });
    // An Admin can revoke anyone's.
    let admin = w.cfg_push("ada", SessionEvent::DeviceRevoked { device_id: vals_phone });
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
    let claim = w.cfg_push("mara", SessionEvent::DeviceRevoked { device_id: d });
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
    // Many raisers: at most 16 active per chat.
    for i in 0..40u128 {
        let by = format!("worker{i}");
        w.cfg_push(&by, SessionEvent::ChatLockdownRaised { chat_id: chat, lockdown: lockdown(100 + i, &by) });
    }
    assert_eq!(w.config().guards.lockdowns_of(chat).len(), 16);
    // A clear frees the slots.
    w.cfg_push("mara", SessionEvent::ChatLockdownCleared { chat_id: chat });
    w.cfg_push("val", SessionEvent::ChatLockdownRaised { chat_id: chat, lockdown: lockdown(999, "val") });
    assert_eq!(w.config().guards.lockdowns_of(chat).len(), 1);
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

#[test]
fn unknown_rules_version_makes_workspace_read_only() {
    let mut w = World::new();
    assert_eq!(w.config().authz.rules, 1, "workspaces start at rules v1");
    assert!(!w.config().authz.read_only());
    // Only the Owner sets rules.
    let by_admin = w.cfg_push("ada", SessionEvent::WorkspaceRulesSet { version: 9 });
    assert_eq!(w.verdicts()[&by_admin], Verdict::Denied);
    assert!(!w.config().authz.read_only());
    w.cfg_push("owner", SessionEvent::WorkspaceRulesSet { version: hive_core::authz::RULES_SUPPORTED + 1 });
    let cfg = w.config();
    assert!(cfg.authz.read_only(), "a version this build doesn't enforce is read-only");
    // The device keeps folding.
    w.chat_push("mara", SessionEvent::ProposalUpserted { proposal: proposal(Uuid::from_u128(0xF1), 1, ProposalStatus::Open) });
    let s = w.state();
    assert_eq!(s.proposals.len(), 1);
    assert!(s.authz.read_only());
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
    let applied = w.chat_push("ada", SessionEvent::ProposalApplied { proposal_id: pid, diff_digest: d, commit: Some("abc".into()), at: Timestamp::epoch() });
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

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
            w.cfg_push("owner", SessionEvent::MemberAdded { invite: None, member: member(id, role) });
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
        w.cfg_push("owner", SessionEvent::MemberRemoved { member_id: "mara".into() });
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
    let acct = SigningKeypair::from_seed(&[owner_name.len() as u8 + 40; 32]).unwrap();
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
        env.actor_stamp = Some(ActorStamp { actor, recorded_at: Timestamp::epoch(), cfg_frontier: f });
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
    let applied = w.agent_push(&a, SessionEvent::ProposalApplied { proposal_id: pid, diff_digest: d, commit: None, at: Timestamp::epoch() });
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
        SessionEvent::WorkspaceRulesSet { version: 1 },
        SessionEvent::ChatLockdownCleared { chat_id: w.sid },
    ] {
        let id = w.cfg_push("x", e);
        let env = w.cfg.iter_mut().find(|x| x.event_id == id).unwrap();
        let mut actor = ActorIdentity::new(a.agent_id.to_string(), "coder", ActorKind::Agent);
        actor.device_id = Some(a.key_id);
        env.actor_stamp = Some(ActorStamp { actor, recorded_at: Timestamp::epoch(), cfg_frontier: None });
        env.signer_device_id = Some(a.key_id);
        assert_eq!(w.verdicts()[&id], Verdict::Denied);
    }
    // What the tier allows on the config log does fold (a lockdown only tightens).
    let id = w.cfg_push("x", SessionEvent::ChatLockdownRaised { chat_id: w.sid, lockdown: lockdown(0x77, "coder") });
    let env = w.cfg.iter_mut().find(|x| x.event_id == id).unwrap();
    let mut actor = ActorIdentity::new(a.agent_id.to_string(), "coder", ActorKind::Agent);
    actor.device_id = Some(a.key_id);
    env.actor_stamp = Some(ActorStamp { actor, recorded_at: Timestamp::epoch(), cfg_frontier: None });
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
    p.producing_agent_id = Some(a.agent_id);
    let filed = w.agent_push(&a, SessionEvent::ProposalUpserted { proposal: p.clone() });
    assert_eq!(w.verdicts()[&filed], Verdict::Denied);
    let (mut w2, a2) = agent_world("mara");
    p.producing_agent_id = Some(a2.agent_id);
    let filed2 = w2.agent_push(&a2, SessionEvent::ProposalUpserted { proposal: p });
    assert_eq!(w2.verdicts()[&filed2], Verdict::Allowed);
    // Once its owner leaves, the agent's events stop counting.
    w2.cfg_push("owner", SessionEvent::MemberRemoved { member_id: "mara".into() });
    let (_, r) = agent_reply(&a2, 0x82);
    let after = w2.agent_push(&a2, r);
    assert_eq!(w2.verdicts()[&after], Verdict::Denied);
}

#[test]
fn rules_v2_tier_applies_to_host_signed_agent_output() {
    // Under rules v2 a host-signed event that speaks for an agent is judged
    // with the Agent tier; tested on the tier itself because this build folds
    // with rules v1 (RULES_SUPPORTED) until Track B enforces v2.
    use hive_core::authz::agent::{agent_authored, tier_allows};
    let agent = Uuid::new_v4();
    let mut p = proposal(Uuid::from_u128(0x91), 1, ProposalStatus::Open);
    p.producing_agent_id = Some(agent);
    let upsert = SessionEvent::ProposalUpserted { proposal: p.clone() };
    assert_eq!(agent_authored(&upsert), Some(agent));
    let fold = ChatSession::new("c", Uuid::nil(), "x");
    assert!(tier_allows(&upsert, agent, Uuid::nil(), &fold).is_ok());
    p.approvals.push(ProposalApproval { actor_id: "owner".into(), role: WorkspaceRole::Owner, approved: true, created_at: Timestamp::epoch(), diff_digest: None });
    assert!(tier_allows(&SessionEvent::ProposalUpserted { proposal: p }, agent, Uuid::nil(), &fold).is_err(), "an agent never asserts approvals");
}

/// P0.6 (A2 fold rule): a `ligus serve` box enrolled as its owner's device
/// signs as the owner without the owner being there. Until Track B's presence
/// proofs its vote doesn't count toward the human gate on any device (the
/// box's API refuses it up front with 403 `presence_required`); the same
/// owner's vote from a device holding their account key does. The box having
/// once published its own identity under its own account changes nothing.
#[test]
fn delegated_host_vote_not_counted_for_human_gate() {
    use hive_core::crypto::{DeviceCertificate, SigningKeypair};
    let mut w = World::new();
    let owner_acct = account("owner");
    let owner_key = SigningKeypair::generate().unwrap();
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
    let from_box = w.chat_push("owner", vote("owner", pid, WorkspaceRole::Owner, true, digest.clone()));
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
    let owner_key = SigningKeypair::generate().unwrap();
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
    w.chat_push("owner", vote("owner", pid, WorkspaceRole::Owner, true, digest));
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

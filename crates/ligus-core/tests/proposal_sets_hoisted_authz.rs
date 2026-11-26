//! Adversarial: proposal-set events forged by someone below Contributor, or by
//! a keyholder who isn't a member, are dropped when a chat log is folded
//! against the workspace roster hoisted from the config log. Without the
//! roster (plain `project`) the same forgeries take effect, which is what the
//! check closes.

use ligus_core::authz::ConfigTimeline;
use ligus_core::proposal_sets::{
    CheckOutcome, PreflightReport, ProposalSet, ScoutAssessment, SetPhase,
};
use ligus_core::{
    project, project_authorized, ActionProposal, ActorIdentity, ActorKind, ActorStamp, CfgFrontier,
    ChatSession, MemberRoleChange, ProposalApproval, ProposalKind, SessionEvent,
    SessionEventEnvelope, Timestamp, WorkspaceAuthz, WorkspaceMember, WorkspaceRole,
};
use uuid::Uuid;

fn member(id: &str, role: WorkspaceRole) -> WorkspaceMember {
    WorkspaceMember {
        id: id.into(),
        actor: ActorIdentity::new(id, id, ActorKind::Human),
        role,
        title: String::new(),
        index: 0,
        joined_at: Timestamp::epoch(),
    }
}

fn stamp(by: &str, frontier: Option<CfgFrontier>) -> Option<ActorStamp> {
    Some(ActorStamp {
        actor: ActorIdentity::new(by, by, ActorKind::Human),
        recorded_at: Timestamp::epoch(),
        cfg_frontier: frontier,
        presence: None,
        runtime_effect: false,
        prev: None,
        sig_alg: Default::default(),
        log_head: None,
    })
}

/// The workspace-config log: `owner` founded it, then added `mara` as a
/// Contributor, `ada` as an Admin and `val` as a Viewer (each with their
/// passkey presence, rules v2 from the seed). `eve` holds a device key but
/// was never added.
struct Config {
    w: ligus_core::testkit::SignedWorld,
}

impl Config {
    fn new() -> Self {
        let mut w = ligus_core::testkit::SignedWorld::genesis(
            &["owner", "mara", "ada", "val", "eve"],
            None,
        );
        for (id, role) in [
            ("mara", WorkspaceRole::Contributor),
            ("ada", WorkspaceRole::Admin),
            ("val", WorkspaceRole::Viewer),
        ] {
            w.admit("owner", id, role);
        }
        Self { w }
    }
    fn head(&self) -> Option<CfgFrontier> {
        self.w.head()
    }
    fn timeline(&self) -> ConfigTimeline {
        self.w.timeline()
    }
}

struct Log {
    sid: Uuid,
    wid: Uuid,
    lamport: u64,
    envs: Vec<SessionEventEnvelope>,
    cfg: Config,
}

impl Log {
    /// A chat created by `owner`: its own roster holds only the creator, as a
    /// real chat log does (members live on the config log).
    fn new() -> Self {
        let cfg = Config::new();
        let mut base = ChatSession::new("Cache", cfg.w.wid, "anthropic");
        base.creator_actor_id = "owner".into();
        base.members = vec![member("owner", WorkspaceRole::Owner)];
        let (sid, wid) = (base.id, base.workspace_id);
        let mut log = Self {
            sid,
            wid,
            lamport: 0,
            envs: Vec::new(),
            cfg,
        };
        log.push(
            "owner",
            SessionEvent::SessionSnapshot {
                session: Box::new(base),
            },
        );
        log
    }
    /// An honest author stamps the config head it has folded.
    fn push(&mut self, by: &str, e: SessionEvent) {
        self.lamport += 1;
        // A human's decision, tuning or vote carries their passkey presence
        // (rules v2), signed by their device; the rest stays a bare stamp.
        let gated = matches!(
            e,
            SessionEvent::ProposalSetDecided { .. }
                | SessionEvent::ProposalSetTuned { .. }
                | SessionEvent::ProposalVoteCast { .. }
        );
        if gated && self.cfg.w.people.contains_key(by) && by != "eve" {
            let head = self.cfg.head();
            let mut env = self.cfg.w.draft(by, Some(self.sid), e, head);
            env.sequence = self.lamport as i64;
            ligus_core::lamport::stamp_after(&mut env, &self.envs);
            let p = &self.cfg.w.people[by];
            ligus_core::testkit::prove_recovery(&mut env, p.account_id, p.device_id, &p.device);
            self.envs.push(env);
            return;
        }
        let mut env = SessionEventEnvelope::new(self.sid, self.wid, self.lamport as i64, e);
        env.actor_stamp = stamp(by, self.cfg.head());
        ligus_core::lamport::stamp_after(&mut env, &self.envs);
        self.envs.push(env);
    }
    /// The owner changes `id`'s role; a demotion carries the honest
    /// watermark (every event `id` signed here, on both logs).
    fn set_role(&mut self, id: &str, old_role: WorkspaceRole, new_role: WorkspaceRole) {
        let signers: std::collections::BTreeSet<Uuid> = self
            .envs
            .iter()
            .chain(&self.cfg.w.cfg)
            .filter(|e| e.actor_stamp.as_ref().is_some_and(|s| s.actor.id == id))
            .filter_map(|e| e.signer_device_id)
            .collect();
        let cfg_log = self.cfg.w.cfg[0].session_id;
        let mut watermark = ligus_core::watermark::heads_of(&self.envs, self.sid, &signers);
        watermark.extend(ligus_core::watermark::heads_of(
            &self.cfg.w.cfg,
            cfg_log,
            &signers,
        ));
        let change = MemberRoleChange {
            member_id: id.into(),
            old_role,
            new_role,
        };
        self.cfg.w.push(
            "owner",
            SessionEvent::MemberRoleChanged { change, watermark },
        );
    }
    fn hoisted(&self) -> ChatSession {
        let t = self.cfg.timeline();
        project_authorized(&self.envs, None, None, WorkspaceAuthz::Timeline(&t)).unwrap()
    }
    fn unchecked(&self) -> ChatSession {
        project(&self.envs).unwrap()
    }
}

fn scout(pid: Uuid, name: &str, score: u32) -> ScoutAssessment {
    ScoutAssessment {
        id: Uuid::new_v4(),
        proposal_id: pid,
        scout_agent_id: Some(Uuid::new_v4()),
        scout_name: name.into(),
        scout_runtime: "claude-code".into(),
        score,
        rationale: String::new(),
        round: 0,
        attempt: 0,
        created_at: Timestamp::epoch(),
    }
}

fn report(pass: bool) -> PreflightReport {
    PreflightReport {
        tests: if pass {
            CheckOutcome::passed("ok")
        } else {
            CheckOutcome::failed("3 tests failed")
        },
        base_unchanged: CheckOutcome::passed("patch applies"),
        hosts_online: CheckOutcome::passed("this device"),
    }
}

/// Mara (a Contributor known only to the config log) opens a set over two
/// alternatives, scouts back A, quorum picks it and she approves it: the set
/// is piping toward a decision. `floor` is the set's decision floor.
fn piping_set(log: &mut Log, floor: WorkspaceRole) -> (Uuid, Uuid, Uuid) {
    let set_id = Uuid::new_v4();
    let mut ids = Vec::new();
    for title in ["Redis", "In-process LRU"] {
        let p = ActionProposal::new(title, ProposalKind::Decision, "planner");
        ids.push(p.id);
        log.push("mara", SessionEvent::ProposalUpserted { proposal: p });
    }
    let mut set = ProposalSet::new("Which cache?", "planner", ids.clone());
    set.id = set_id;
    set.approval_role_floor = floor;
    set.agent_quorum = 1;
    log.push(
        "mara",
        SessionEvent::ProposalSetOpened { set: Box::new(set) },
    );
    let (a, b) = (ids[0], ids[1]);
    log.push(
        "mara",
        SessionEvent::ScoutAssessmentPosted {
            set_id,
            assessment: scout(a, "coder", 90),
        },
    );
    log.push(
        "mara",
        SessionEvent::ProposalSetQuorumReached {
            set_id,
            proposal_id: a,
            tie_break: false,
            at: Timestamp::epoch(),
        },
    );
    for who in ["mara", "ada"] {
        log.push(
            who,
            SessionEvent::ProposalVoteCast {
                proposal_id: a,
                approval: ProposalApproval {
                    actor_id: who.into(),
                    role: if who == "ada" {
                        WorkspaceRole::Admin
                    } else {
                        WorkspaceRole::Contributor
                    },
                    approved: true,
                    created_at: Timestamp::epoch(),
                    diff_digest: None,
                },
            },
        );
    }
    (set_id, a, b)
}

#[test]
fn a_contributor_known_only_to_the_config_log_drives_a_set() {
    let mut log = Log::new();
    let (set_id, a, _) = piping_set(&mut log, WorkspaceRole::Contributor);
    let s = log.hoisted();
    let set = s
        .proposal_sets
        .iter()
        .find(|x| x.id == set_id)
        .expect("mara's set folds");
    assert_eq!(set.phase, SetPhase::Piping);
    assert_eq!(set.front_runner, Some(a));
}

#[test]
fn forged_preflight_tune_and_decide_from_a_viewer_or_stranger_are_dropped() {
    let mut log = Log::new();
    let (set_id, a, b) = piping_set(&mut log, WorkspaceRole::Contributor);
    // val (Viewer) records a failing pre-flight: it would send the set back to
    // scouting and decay every assessment.
    log.push(
        "val",
        SessionEvent::ProposalSetPreflightRecorded {
            set_id,
            proposal_id: a,
            report: report(false),
            at: Timestamp::epoch(),
        },
    );
    // eve (no member at all) loosens the dial and stops the front-runner.
    log.push(
        "eve",
        SessionEvent::ProposalSetTuned {
            set_id,
            agent_quorum: 9,
            required_approvals: 9,
            quality_bar: 100,
        },
    );
    log.push(
        "eve",
        SessionEvent::ProposalSetAlternativeStopped {
            set_id,
            proposal_id: a,
            stopped_by: Some(b),
            reason: ligus_core::proposal_sets::StopReason::Quorum,
        },
    );
    log.push(
        "eve",
        SessionEvent::ProposalSetDecided {
            set_id,
            winner: None,
            at: Timestamp::epoch(),
        },
    );
    log.push(
        "val",
        SessionEvent::ProposalReverted {
            proposal_id: a,
            at: Timestamp::epoch(),
        },
    );

    // Unchecked, the forgeries land: the set is back in scouting, retuned and
    // closed by someone who isn't even a member.
    let raw = log.unchecked();
    let raw_set = raw.proposal_sets.iter().find(|x| x.id == set_id).unwrap();
    assert_eq!(
        raw_set.phase,
        SetPhase::Decided,
        "precondition: without the roster eve's close folds"
    );
    assert_eq!(raw_set.winner, None);
    assert_ne!(
        raw_set.agent_quorum, 1,
        "precondition: eve's tune folds unchecked"
    );

    // Against the hoisted roster none of them fold.
    let s = log.hoisted();
    let set = s.proposal_sets.iter().find(|x| x.id == set_id).unwrap();
    assert_eq!(
        set.phase,
        SetPhase::Piping,
        "the set is still heading for its decision"
    );
    assert_eq!(set.front_runner, Some(a));
    assert!(!set.is_frozen(a), "eve's stop was dropped");
    assert_eq!(set.agent_quorum, 1, "eve's tune was dropped");
    assert!(set.preflights.is_empty(), "val's pre-flight was dropped");

    // The real members finish it: owner records a passing pre-flight and decides.
    log.push(
        "owner",
        SessionEvent::ProposalSetPreflightRecorded {
            set_id,
            proposal_id: a,
            report: report(true),
            at: Timestamp::epoch(),
        },
    );
    log.push(
        "owner",
        SessionEvent::ProposalSetDecided {
            set_id,
            winner: Some(a),
            at: Timestamp::epoch(),
        },
    );
    let s = log.hoisted();
    let set = s.proposal_sets.iter().find(|x| x.id == set_id).unwrap();
    assert_eq!((set.phase, set.winner), (SetPhase::Decided, Some(a)));
}

#[test]
fn a_member_below_the_sets_decision_floor_cannot_decide_or_tune() {
    let mut log = Log::new();
    let (set_id, a, _) = piping_set(&mut log, WorkspaceRole::Admin);
    log.push(
        "owner",
        SessionEvent::ProposalSetPreflightRecorded {
            set_id,
            proposal_id: a,
            report: report(true),
            at: Timestamp::epoch(),
        },
    );
    // mara is a Contributor: below this set's Admin floor.
    log.push(
        "mara",
        SessionEvent::ProposalSetTuned {
            set_id,
            agent_quorum: 1,
            required_approvals: 1,
            quality_bar: 10,
        },
    );
    log.push(
        "mara",
        SessionEvent::ProposalSetDecided {
            set_id,
            winner: Some(a),
            at: Timestamp::epoch(),
        },
    );
    let s = log.hoisted();
    let set = s.proposal_sets.iter().find(|x| x.id == set_id).unwrap();
    assert_ne!(set.phase, SetPhase::Decided);
    assert_ne!(set.quality_bar, 10);
    // ada is an Admin: her decision folds.
    log.push(
        "ada",
        SessionEvent::ProposalSetDecided {
            set_id,
            winner: Some(a),
            at: Timestamp::epoch(),
        },
    );
    let s = log.hoisted();
    assert_eq!(
        s.proposal_sets
            .iter()
            .find(|x| x.id == set_id)
            .unwrap()
            .winner,
        Some(a)
    );
}

#[test]
fn a_demotion_on_the_config_log_is_what_counts_not_the_chats_stale_roster() {
    // The chat's own roster still lists mara as a Contributor (a pre-hoist
    // add); the workspace demoted her to Viewer before she acted.
    let mut log = Log::new();
    log.push(
        "owner",
        SessionEvent::MemberAdded {
            invite: None,
            proof: None,
            member: member("mara", WorkspaceRole::Contributor),
        },
    );
    log.set_role("mara", WorkspaceRole::Contributor, WorkspaceRole::Viewer);
    let (set_id, _, _) = piping_set(&mut log, WorkspaceRole::Contributor);
    let s = log.hoisted();
    assert!(
        s.proposal_sets.iter().all(|x| x.id != set_id),
        "a Viewer can't open a set"
    );
}

/// Only the workspace roster grants a workspace role: a member added to one
/// chat's own roster (never admitted on the config log) counts for nothing,
/// in or out of dearth mode. (Before Phase 0 such a member counted outside
/// dearth mode; 0.1 starts every workspace on the config roster.)
#[test]
fn a_chat_only_member_counts_for_nothing() {
    let mut log = Log::new();
    // "temp" was added to this chat only, never admitted on the config log.
    log.push(
        "owner",
        SessionEvent::MemberAdded {
            invite: None,
            proof: None,
            member: member("temp", WorkspaceRole::Contributor),
        },
    );
    let p = ActionProposal::new("A", ProposalKind::Decision, "planner");
    let set = ProposalSet::new("Q", "planner", vec![p.id]);
    let set_id = set.id;
    log.push("temp", SessionEvent::ProposalUpserted { proposal: p });
    log.push(
        "temp",
        SessionEvent::ProposalSetOpened { set: Box::new(set) },
    );
    let open = log.hoisted();
    assert!(
        open.proposal_sets.iter().all(|x| x.id != set_id),
        "a chat-only member can't open a set"
    );
    assert!(
        log.unchecked().proposal_sets.iter().any(|x| x.id == set_id),
        "control: it folds unchecked"
    );
}

// ---- security review (0.1): R1 and R2, fixed by the Phase 0 authz engine ----

// ---- security review (0.1): proven, not yet fixed; see the review report ----

/// A set's human gate is "`required_approvals` from members at or above the
/// role floor". The role in a vote payload is the voter's own claim: the fold
/// ignores it and counts the signer's real role at the vote's position, so a
/// Viewer declaring Admin can't satisfy an Admin-floor set (R1).
#[test]
fn a_viewers_self_declared_admin_vote_does_not_meet_an_admin_floor() {
    let mut log = Log::new();
    let set_id = Uuid::new_v4();
    let p = ActionProposal::new("Redis", ProposalKind::Decision, "planner");
    let a = p.id;
    log.push("mara", SessionEvent::ProposalUpserted { proposal: p });
    let mut set = ProposalSet::new("Which cache?", "planner", vec![a]);
    set.id = set_id;
    set.approval_role_floor = WorkspaceRole::Admin;
    set.agent_quorum = 1;
    log.push(
        "mara",
        SessionEvent::ProposalSetOpened { set: Box::new(set) },
    );
    log.push(
        "mara",
        SessionEvent::ScoutAssessmentPosted {
            set_id,
            assessment: scout(a, "coder", 90),
        },
    );
    log.push(
        "mara",
        SessionEvent::ProposalSetQuorumReached {
            set_id,
            proposal_id: a,
            tie_break: false,
            at: Timestamp::epoch(),
        },
    );
    log.push(
        "val",
        SessionEvent::ProposalVoteCast {
            proposal_id: a,
            approval: ProposalApproval {
                actor_id: "val".into(),
                role: WorkspaceRole::Admin,
                approved: true,
                created_at: Timestamp::epoch(),
                diff_digest: None,
            },
        },
    );
    log.push(
        "ada",
        SessionEvent::ProposalSetPreflightRecorded {
            set_id,
            proposal_id: a,
            report: report(true),
            at: Timestamp::epoch(),
        },
    );
    log.push(
        "ada",
        SessionEvent::ProposalSetDecided {
            set_id,
            winner: Some(a),
            at: Timestamp::epoch(),
        },
    );
    let raw = log.unchecked();
    assert_eq!(
        raw.proposal_sets
            .iter()
            .find(|x| x.id == set_id)
            .unwrap()
            .winner,
        Some(a),
        "precondition: the claimed role counts unchecked"
    );
    let s = log.hoisted();
    let set = s.proposal_sets.iter().find(|x| x.id == set_id).unwrap();
    assert_ne!(
        set.winner,
        Some(a),
        "a Viewer's vote, claiming Admin, met the Admin-floor human gate"
    );
    // An Admin's own vote does meet it.
    log.push(
        "ada",
        SessionEvent::ProposalVoteCast {
            proposal_id: a,
            approval: ProposalApproval {
                actor_id: "ada".into(),
                role: WorkspaceRole::Viewer,
                approved: true,
                created_at: Timestamp::epoch(),
                diff_digest: None,
            },
        },
    );
    log.push(
        "ada",
        SessionEvent::ProposalSetDecided {
            set_id,
            winner: Some(a),
            at: Timestamp::epoch(),
        },
    );
    let s = log.hoisted();
    assert_eq!(
        s.proposal_sets
            .iter()
            .find(|x| x.id == set_id)
            .unwrap()
            .winner,
        Some(a),
        "the signer's real Admin role counts, whatever the payload says"
    );
}

/// Hoisted authorization judges every event against the roster at the config
/// frontier its author had folded, not the roster as it is now: a decision a
/// Viewer signed stays refused after she is promoted, and an Admin's decision
/// stands after she is demoted (R2).
#[test]
fn hoisted_authorization_is_not_retroactive() {
    let mut log = Log::new();
    let (set_id, a, _) = piping_set(&mut log, WorkspaceRole::Contributor);
    log.push(
        "owner",
        SessionEvent::ProposalSetPreflightRecorded {
            set_id,
            proposal_id: a,
            report: report(true),
            at: Timestamp::epoch(),
        },
    );
    log.push(
        "val",
        SessionEvent::ProposalSetDecided {
            set_id,
            winner: None,
            at: Timestamp::epoch(),
        },
    );
    let before = log.hoisted();
    assert_eq!(
        before
            .proposal_sets
            .iter()
            .find(|x| x.id == set_id)
            .unwrap()
            .phase,
        SetPhase::Piping,
        "control"
    );
    // Later, val is promoted. Her old, refused close must stay refused.
    log.set_role("val", WorkspaceRole::Viewer, WorkspaceRole::Admin);
    let after = log.hoisted();
    assert_eq!(
        after
            .proposal_sets
            .iter()
            .find(|x| x.id == set_id)
            .unwrap()
            .phase,
        SetPhase::Piping,
        "a refused close became valid retroactively"
    );

    // And the converse: ada decides while an Admin, then is demoted. The
    // decision stands.
    log.push(
        "ada",
        SessionEvent::ProposalSetDecided {
            set_id,
            winner: Some(a),
            at: Timestamp::epoch(),
        },
    );
    assert_eq!(
        log.hoisted()
            .proposal_sets
            .iter()
            .find(|x| x.id == set_id)
            .unwrap()
            .winner,
        Some(a)
    );
    log.set_role("ada", WorkspaceRole::Admin, WorkspaceRole::Viewer);
    let demoted = log.hoisted();
    let set = demoted
        .proposal_sets
        .iter()
        .find(|x| x.id == set_id)
        .unwrap();
    assert_eq!(
        (set.phase, set.winner),
        (SetPhase::Decided, Some(a)),
        "a later demotion undid a decision"
    );
}

//! Proposal sets through the real event fold: envelopes in canonical order,
//! wire compatibility with older peers, and convergence regardless of arrival.

use hive_core::proposal_sets::{CheckOutcome, PreflightReport, ProposalSet, ScoutAssessment, SetPhase, StopReason};
use hive_core::{
    project, ActionProposal, ChatSession, ProposalApproval, ProposalKind, SessionEvent, SessionEventEnvelope,
    Timestamp, WorkspaceRole,
};
use uuid::Uuid;

struct Log {
    sid: Uuid,
    wid: Uuid,
    envs: Vec<SessionEventEnvelope>,
}

impl Log {
    fn new() -> Self {
        let base = ChatSession::new("Demo", Uuid::nil(), "anthropic");
        let (sid, wid) = (base.id, base.workspace_id);
        let seed = SessionEventEnvelope::new(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(base) });
        Self { sid, wid, envs: vec![seed] }
    }
    fn push(&mut self, e: SessionEvent) {
        let seq = self.envs.len() as i64 + 1;
        self.envs.push(SessionEventEnvelope::new(self.sid, self.wid, seq, e));
    }
    fn state(&self) -> ChatSession {
        project(&self.envs).unwrap()
    }
}

fn scout(pid: Uuid, agent: Uuid, name: &str, score: u32) -> ScoutAssessment {
    ScoutAssessment {
        id: Uuid::new_v4(),
        proposal_id: pid,
        scout_agent_id: Some(agent),
        scout_name: name.into(),
        scout_runtime: "claude-code".into(),
        score,
        rationale: "fits the constraints".into(),
        round: 0,
        attempt: 0,
        created_at: Timestamp::now(),
    }
}

/// File two alternatives and open a set over them, the way the app does.
fn open_set(log: &mut Log) -> (Uuid, Uuid, Uuid) {
    let set_id = Uuid::new_v4();
    let mut ids = Vec::new();
    for title in ["Redis", "In-process LRU"] {
        let mut p = ActionProposal::new(title, ProposalKind::Decision, "planner");
        p.set_id = Some(set_id);
        p.task_class = Some("design".into());
        ids.push(p.id);
        log.push(SessionEvent::ProposalUpserted { proposal: p });
    }
    let mut set = ProposalSet::new("Which cache?", "planner", ids.clone());
    set.id = set_id;
    set.task_class = Some("design".into());
    log.push(SessionEvent::ProposalSetOpened { set: Box::new(set) });
    (set_id, ids[0], ids[1])
}

#[test]
fn full_cycle_scouting_quorum_stop_preflight_decide() {
    let mut log = Log::new();
    let (set_id, a, b) = open_set(&mut log);
    let (s1, s2, s3) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    log.push(SessionEvent::ScoutAssessmentPosted { set_id, assessment: scout(a, s1, "coder", 85) });
    log.push(SessionEvent::ScoutAssessmentPosted { set_id, assessment: scout(a, s2, "reviewer", 75) });
    log.push(SessionEvent::ScoutAssessmentPosted { set_id, assessment: scout(b, s3, "local", 90) });
    log.push(SessionEvent::ProposalSetQuorumReached { set_id, proposal_id: a, tie_break: false, at: Timestamp::now() });
    log.push(SessionEvent::ProposalSetAlternativeStopped {
        set_id,
        proposal_id: b,
        stopped_by: Some(a),
        reason: StopReason::Quorum,
    });
    let s = log.state();
    let set = &s.proposal_sets[0];
    assert_eq!(set.phase, SetPhase::Piping);
    assert_eq!(set.front_runner, Some(a));
    assert!(set.is_frozen(b));
    assert!(s.proposals.iter().all(|p| p.set_id == Some(set_id)));

    // The human gate, then the pre-flight, then the decision.
    log.push(SessionEvent::ProposalVoteCast {
        proposal_id: a,
        approval: ProposalApproval {
            actor_id: "mara".into(),
            role: WorkspaceRole::Contributor,
            approved: true,
            created_at: Timestamp::now(),
        },
    });
    log.push(SessionEvent::ProposalSetPreflightRecorded {
        set_id,
        proposal_id: a,
        report: PreflightReport {
            tests: CheckOutcome::skipped("no test command set"),
            base_unchanged: CheckOutcome::passed("patch applies"),
            hosts_online: CheckOutcome::passed("this device"),
        },
        at: Timestamp::now(),
    });
    log.push(SessionEvent::ProposalSetDecided { set_id, winner: Some(a), at: Timestamp::now() });
    let s = log.state();
    assert_eq!(s.proposal_sets[0].phase, SetPhase::Decided);
    assert_eq!(s.proposal_sets[0].winner, Some(a));
}

#[test]
fn the_fold_converges_regardless_of_arrival_order() {
    let mut log = Log::new();
    let (set_id, a, _b) = open_set(&mut log);
    log.push(SessionEvent::ScoutAssessmentPosted { set_id, assessment: scout(a, Uuid::new_v4(), "coder", 85) });
    log.push(SessionEvent::ScoutAssessmentPosted { set_id, assessment: scout(a, Uuid::new_v4(), "reviewer", 80) });
    log.push(SessionEvent::ProposalSetQuorumReached { set_id, proposal_id: a, tie_break: false, at: Timestamp::now() });
    let forward = log.state();
    let mut shuffled = log.envs.clone();
    shuffled.reverse();
    let backward = project(&shuffled).unwrap();
    assert_eq!(forward.proposal_sets, backward.proposal_sets);
    assert_eq!(forward.proposals, backward.proposals);
}

#[test]
fn an_agent_cannot_decide_without_human_quorum() {
    // Even with every scout behind it and a passing pre-flight, a decision
    // event is inert until a qualifying human approved the alternative.
    let mut log = Log::new();
    let (set_id, a, _) = open_set(&mut log);
    for name in ["coder", "reviewer", "local"] {
        log.push(SessionEvent::ScoutAssessmentPosted { set_id, assessment: scout(a, Uuid::new_v4(), name, 100) });
    }
    log.push(SessionEvent::ProposalSetPreflightRecorded {
        set_id,
        proposal_id: a,
        report: PreflightReport {
            tests: CheckOutcome::passed("ok"),
            base_unchanged: CheckOutcome::passed("ok"),
            hosts_online: CheckOutcome::passed("ok"),
        },
        at: Timestamp::now(),
    });
    // The alternative's own author approving doesn't count either.
    log.push(SessionEvent::ProposalVoteCast {
        proposal_id: a,
        approval: ProposalApproval {
            actor_id: "planner".into(),
            role: WorkspaceRole::Owner,
            approved: true,
            created_at: Timestamp::now(),
        },
    });
    log.push(SessionEvent::ProposalSetDecided { set_id, winner: Some(a), at: Timestamp::now() });
    assert_eq!(log.state().proposal_sets[0].phase, SetPhase::Scouting);
}

#[test]
fn new_events_round_trip_and_a_2_0_peer_sees_them_as_unknown() {
    let set_id = Uuid::new_v4();
    let ev = SessionEvent::ProposalSetDecided { set_id, winner: None, at: Timestamp::epoch() };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains("\"kind\":\"proposalSetDecided\""));
    assert_eq!(serde_json::from_str::<SessionEvent>(&json).unwrap(), ev);
    assert_eq!(ev.kind_str(), "proposalSetDecided");

    // What an older build does with a kind it doesn't know: decode as Unknown.
    #[derive(serde::Deserialize, Debug, PartialEq)]
    #[serde(tag = "kind", rename_all = "camelCase")]
    enum OldEvent {
        ProposalUpserted { proposal: serde_json::Value },
        #[serde(other)]
        Unknown,
    }
    assert_eq!(serde_json::from_str::<OldEvent>(&json).unwrap(), OldEvent::Unknown);

    // A proposal without set fields (older versions) still decodes; a new one omits empty fields.
    let old = r#"{"id":"6f1f8a3e-0d4f-4c38-9a59-2d1f6a1c1b2a","title":"X","kind":"decision","status":"open"}"#;
    let p: ActionProposal = serde_json::from_str(old).unwrap();
    assert_eq!(p.set_id, None);
    assert!(!p.reverted);
    let plain = serde_json::to_string(&ActionProposal::new("Y", ProposalKind::Decision, "")).unwrap();
    assert!(!plain.contains("setId") && !plain.contains("taskClass") && !plain.contains("reverted"));
}

#[test]
fn a_2_0_peer_upsert_that_drops_set_id_is_restamped() {
    let mut log = Log::new();
    let (set_id, a, _) = open_set(&mut log);
    // An older peer re-upserts the proposal from its own projection, which
    // never knew the set fields.
    let mut stale = log.state().proposals.into_iter().find(|p| p.id == a).unwrap();
    stale.set_id = None;
    stale.approval_role_floor = WorkspaceRole::Viewer;
    log.push(SessionEvent::ProposalUpserted { proposal: stale });
    let p = log.state().proposals.into_iter().find(|p| p.id == a).unwrap();
    assert_eq!(p.set_id, Some(set_id));
    assert_eq!(p.approval_role_floor, WorkspaceRole::Contributor);
}

#[test]
fn reverting_marks_only_an_applied_proposal() {
    let mut log = Log::new();
    let (_, a, b) = open_set(&mut log);
    let mut applied = log.state().proposals.into_iter().find(|p| p.id == a).unwrap();
    applied.status = hive_core::ProposalStatus::Applied;
    log.push(SessionEvent::ProposalUpserted { proposal: applied });
    log.push(SessionEvent::ProposalReverted { proposal_id: a, at: Timestamp::now() });
    log.push(SessionEvent::ProposalReverted { proposal_id: b, at: Timestamp::now() });
    let s = log.state();
    assert!(s.proposals.iter().find(|p| p.id == a).unwrap().reverted);
    assert!(!s.proposals.iter().find(|p| p.id == b).unwrap().reverted, "an open proposal has nothing to revert");
}

#[test]
fn author_side_authz_enforces_the_set_floor_and_no_self_decision() {
    use hive_core::authorize;
    let mut log = Log::new();
    let (set_id, a, _) = open_set(&mut log);
    let s = log.state();
    let decide = SessionEvent::ProposalSetDecided { set_id, winner: Some(a), at: Timestamp::now() };
    assert!(!authorize(&decide, "v", WorkspaceRole::Viewer, &s).allowed, "viewers can't decide");
    assert!(authorize(&decide, "c", WorkspaceRole::Contributor, &s).allowed);
    assert!(!authorize(&decide, "planner", WorkspaceRole::Owner, &s).allowed, "no deciding for your own alternative");
    let tune = SessionEvent::ProposalSetTuned { set_id, agent_quorum: 1, required_approvals: 1, quality_bar: 10 };
    assert!(!authorize(&tune, "v", WorkspaceRole::Viewer, &s).allowed);
    assert!(authorize(&tune, "c", WorkspaceRole::Contributor, &s).allowed);
}

#[test]
fn a_sets_gate_only_raises_an_alternatives_own_bar() {
    let mut log = Log::new();
    let set_id = Uuid::new_v4();
    // A core path zone already raised this one's bar when it was filed.
    let mut core = ActionProposal::new("Core change", ProposalKind::Decision, "planner");
    core.required_approvals = 3;
    core.approval_role_floor = WorkspaceRole::Admin;
    let docs = ActionProposal::new("Docs change", ProposalKind::Decision, "planner");
    let (c, d) = (core.id, docs.id);
    log.push(SessionEvent::ProposalUpserted { proposal: core });
    log.push(SessionEvent::ProposalUpserted { proposal: docs });
    let mut set = ProposalSet::new("Which?", "planner", vec![c, d]);
    set.id = set_id;
    log.push(SessionEvent::ProposalSetOpened { set: Box::new(set) });
    let gate = |s: &ChatSession, id: Uuid| {
        let p = s.proposals.iter().find(|p| p.id == id).unwrap();
        (p.required_approvals, p.approval_role_floor)
    };
    // Careful: two approvals. The core change keeps its stricter three.
    log.push(SessionEvent::ProposalSetTuned { set_id, agent_quorum: 3, required_approvals: 2, quality_bar: 80 });
    let s = log.state();
    assert_eq!(gate(&s, c), (3, WorkspaceRole::Admin));
    assert_eq!(gate(&s, d), (2, WorkspaceRole::Contributor));
    // Back to fast: the set's gate drops to one, but never below either's own.
    log.push(SessionEvent::ProposalSetTuned { set_id, agent_quorum: 1, required_approvals: 1, quality_bar: 60 });
    let s = log.state();
    assert_eq!(gate(&s, c), (3, WorkspaceRole::Admin));
    assert_eq!(gate(&s, d).0, 1);
}

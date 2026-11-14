//! Adversarial: proposal-set events forged by someone below Contributor, or by
//! a keyholder who isn't a member, are dropped when a chat log is folded
//! against the workspace roster hoisted from the config log. Without the
//! roster (plain `project`) the same forgeries take effect, which is what the
//! check closes.

use hive_core::authorization::HoistedRoster;
use hive_core::proposal_sets::{CheckOutcome, PreflightReport, ProposalSet, ScoutAssessment, SetPhase};
use hive_core::{
    project, project_with_hoist, ActionProposal, ActorIdentity, ActorKind, ActorStamp, ChatSession,
    ProposalApproval, ProposalKind, SessionEvent, SessionEventEnvelope, Timestamp, WorkspaceMember, WorkspaceRole,
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

/// The workspace roster on the config log: `owner` founded it, `mara` is a
/// Contributor, `ada` an Admin and `val` a Viewer. `eve` holds a device key
/// but was never added.
fn workspace_roster() -> Vec<WorkspaceMember> {
    vec![
        member("owner", WorkspaceRole::Owner),
        member("mara", WorkspaceRole::Contributor),
        member("ada", WorkspaceRole::Admin),
        member("val", WorkspaceRole::Viewer),
    ]
}

struct Log {
    sid: Uuid,
    wid: Uuid,
    lamport: u64,
    envs: Vec<SessionEventEnvelope>,
}

impl Log {
    /// A chat created by `owner`: its own roster holds only the creator, as a
    /// real chat log does (members live on the config log).
    fn new() -> Self {
        let mut base = ChatSession::new("Cache", Uuid::new_v4(), "anthropic");
        base.creator_actor_id = "owner".into();
        base.members = vec![member("owner", WorkspaceRole::Owner)];
        let (sid, wid) = (base.id, base.workspace_id);
        let mut log = Self { sid, wid, lamport: 0, envs: Vec::new() };
        log.push("owner", SessionEvent::SessionSnapshot { session: Box::new(base) });
        log
    }
    fn push(&mut self, by: &str, e: SessionEvent) {
        self.lamport += 1;
        let mut env = SessionEventEnvelope::new(self.sid, self.wid, self.lamport as i64, e);
        env.lamport = self.lamport;
        env.actor_stamp =
            Some(ActorStamp { actor: ActorIdentity::new(by, by, ActorKind::Human), recorded_at: Timestamp::epoch() });
        self.envs.push(env);
    }
    fn hoisted(&self, roster: &[WorkspaceMember], dearth_on: bool) -> ChatSession {
        project_with_hoist(&self.envs, None, None, Some(HoistedRoster { members: roster, dearth_on })).unwrap()
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
        tests: if pass { CheckOutcome::passed("ok") } else { CheckOutcome::failed("3 tests failed") },
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
    log.push("mara", SessionEvent::ProposalSetOpened { set: Box::new(set) });
    let (a, b) = (ids[0], ids[1]);
    log.push("mara", SessionEvent::ScoutAssessmentPosted { set_id, assessment: scout(a, "coder", 90) });
    log.push("mara", SessionEvent::ProposalSetQuorumReached { set_id, proposal_id: a, tie_break: false, at: Timestamp::epoch() });
    for who in ["mara", "ada"] {
        log.push(
            who,
            SessionEvent::ProposalVoteCast {
                proposal_id: a,
                approval: ProposalApproval {
                    actor_id: who.into(),
                    role: if who == "ada" { WorkspaceRole::Admin } else { WorkspaceRole::Contributor },
                    approved: true,
                    created_at: Timestamp::epoch(),
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
    let s = log.hoisted(&workspace_roster(), false);
    let set = s.proposal_sets.iter().find(|x| x.id == set_id).expect("mara's set folds");
    assert_eq!(set.phase, SetPhase::Piping);
    assert_eq!(set.front_runner, Some(a));
}

#[test]
fn forged_preflight_tune_and_decide_from_a_viewer_or_stranger_are_dropped() {
    let mut log = Log::new();
    let (set_id, a, b) = piping_set(&mut log, WorkspaceRole::Contributor);
    // val (Viewer) records a failing pre-flight: it would send the set back to
    // scouting and decay every assessment.
    log.push("val", SessionEvent::ProposalSetPreflightRecorded { set_id, proposal_id: a, report: report(false), at: Timestamp::epoch() });
    // eve (no member at all) loosens the dial and stops the front-runner.
    log.push("eve", SessionEvent::ProposalSetTuned { set_id, agent_quorum: 9, required_approvals: 9, quality_bar: 100 });
    log.push("eve", SessionEvent::ProposalSetAlternativeStopped {
        set_id,
        proposal_id: a,
        stopped_by: Some(b),
        reason: hive_core::proposal_sets::StopReason::Quorum,
    });
    log.push("eve", SessionEvent::ProposalSetDecided { set_id, winner: None, at: Timestamp::epoch() });
    log.push("val", SessionEvent::ProposalReverted { proposal_id: a, at: Timestamp::epoch() });

    // Unchecked, the forgeries land: the set is back in scouting, retuned and
    // closed by someone who isn't even a member.
    let raw = log.unchecked();
    let raw_set = raw.proposal_sets.iter().find(|x| x.id == set_id).unwrap();
    assert_eq!(raw_set.phase, SetPhase::Decided, "precondition: without the roster eve's close folds");
    assert_eq!(raw_set.winner, None);
    assert_ne!(raw_set.agent_quorum, 1, "precondition: eve's tune folds unchecked");

    // Against the hoisted roster none of them fold.
    let s = log.hoisted(&workspace_roster(), false);
    let set = s.proposal_sets.iter().find(|x| x.id == set_id).unwrap();
    assert_eq!(set.phase, SetPhase::Piping, "the set is still heading for its decision");
    assert_eq!(set.front_runner, Some(a));
    assert!(!set.is_frozen(a), "eve's stop was dropped");
    assert_eq!(set.agent_quorum, 1, "eve's tune was dropped");
    assert!(set.preflights.is_empty(), "val's pre-flight was dropped");

    // The real members finish it: owner records a passing pre-flight and decides.
    log.push("owner", SessionEvent::ProposalSetPreflightRecorded { set_id, proposal_id: a, report: report(true), at: Timestamp::epoch() });
    log.push("owner", SessionEvent::ProposalSetDecided { set_id, winner: Some(a), at: Timestamp::epoch() });
    let s = log.hoisted(&workspace_roster(), false);
    let set = s.proposal_sets.iter().find(|x| x.id == set_id).unwrap();
    assert_eq!((set.phase, set.winner), (SetPhase::Decided, Some(a)));
}

#[test]
fn a_member_below_the_sets_decision_floor_cannot_decide_or_tune() {
    let mut log = Log::new();
    let (set_id, a, _) = piping_set(&mut log, WorkspaceRole::Admin);
    log.push("owner", SessionEvent::ProposalSetPreflightRecorded { set_id, proposal_id: a, report: report(true), at: Timestamp::epoch() });
    // mara is a Contributor: below this set's Admin floor.
    log.push("mara", SessionEvent::ProposalSetTuned { set_id, agent_quorum: 1, required_approvals: 1, quality_bar: 10 });
    log.push("mara", SessionEvent::ProposalSetDecided { set_id, winner: Some(a), at: Timestamp::epoch() });
    let s = log.hoisted(&workspace_roster(), false);
    let set = s.proposal_sets.iter().find(|x| x.id == set_id).unwrap();
    assert_ne!(set.phase, SetPhase::Decided);
    assert_ne!(set.quality_bar, 10);
    // ada is an Admin: her decision folds.
    log.push("ada", SessionEvent::ProposalSetDecided { set_id, winner: Some(a), at: Timestamp::epoch() });
    let s = log.hoisted(&workspace_roster(), false);
    assert_eq!(s.proposal_sets.iter().find(|x| x.id == set_id).unwrap().winner, Some(a));
}

#[test]
fn a_demotion_on_the_config_log_is_what_counts_not_the_chats_stale_roster() {
    // The chat's own roster still lists mara as a Contributor (a pre-hoist
    // add); the workspace has since demoted her to Viewer.
    let mut log = Log::new();
    log.push("owner", SessionEvent::MemberAdded { member: member("mara", WorkspaceRole::Contributor) });
    let (set_id, _, _) = piping_set(&mut log, WorkspaceRole::Contributor);
    let mut roster = workspace_roster();
    roster.iter_mut().find(|m| m.id == "mara").unwrap().role = WorkspaceRole::Viewer;
    let s = log.hoisted(&roster, false);
    assert!(s.proposal_sets.iter().all(|x| x.id != set_id), "a Viewer can't open a set");
}

#[test]
fn during_dearth_a_chat_only_member_counts_for_nothing() {
    let mut log = Log::new();
    // "temp" was added to this chat only, never admitted on the config log.
    log.push("owner", SessionEvent::MemberAdded { member: member("temp", WorkspaceRole::Contributor) });
    let p = ActionProposal::new("A", ProposalKind::Decision, "planner");
    let set = ProposalSet::new("Q", "planner", vec![p.id]);
    let set_id = set.id;
    log.push("temp", SessionEvent::ProposalUpserted { proposal: p });
    log.push("temp", SessionEvent::ProposalSetOpened { set: Box::new(set) });
    let open = log.hoisted(&workspace_roster(), false);
    assert!(open.proposal_sets.iter().any(|x| x.id == set_id), "outside dearth the chat's own member counts");
    let tight = log.hoisted(&workspace_roster(), true);
    assert!(tight.proposal_sets.iter().all(|x| x.id != set_id), "dearth mode: only config-log members count");
}

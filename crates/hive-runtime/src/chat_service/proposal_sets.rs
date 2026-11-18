//! Proposal-set writes on [`ChatService`]: each one appends a signed,
//! role-checked set event (see `hive_core::proposal_sets` for the process and
//! the fold). A child module so it shares the service's signed-append path.

use hive_core::proposal_sets::{
    PreflightReport, ProposalSet, QuorumKnobs, ScoutAssessment, SetPhase, StopReason,
};
use hive_core::{ActionProposal, SessionEvent, Timestamp};
use uuid::Uuid;

use super::{ChatError, ChatService, Result};

/// What [`ChatService::settle_proposal_set`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuorumOutcome {
    /// The front-runner now piping.
    pub front_runner: Uuid,
    /// It won the deadline tie-break rather than a quorum.
    pub tie_break: bool,
    /// Rivals that got a stop signal, with the scouts that backed each.
    pub stopped: Vec<(Uuid, Vec<String>)>,
}

impl ChatService {
    fn proposal_set(&self, session_id: Uuid, set_id: Uuid) -> Result<(hive_core::ChatSession, ProposalSet)> {
        let session = self
            .load(session_id)?
            .ok_or_else(|| ChatError::NotFound(format!("session {session_id}")))?;
        let set = session
            .proposal_sets
            .iter()
            .find(|s| s.id == set_id)
            .cloned()
            .ok_or_else(|| ChatError::NotFound(format!("proposal set {set_id}")))?;
        Ok((session, set))
    }

    /// File `alternatives` (each stamped with the set id) and open `set` over
    /// them. Returns the projected set.
    pub fn open_proposal_set(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        mut set: ProposalSet,
        alternatives: Vec<ActionProposal>,
    ) -> Result<ProposalSet> {
        set.alternatives = alternatives.iter().map(|p| p.id).collect();
        for mut p in alternatives {
            p.set_id = Some(set.id);
            if p.task_class.is_none() {
                p.task_class = set.task_class.clone();
            }
            self.upsert_proposal(session_id, workspace_id, p)?;
        }
        let set_id = set.id;
        self.append_signed(session_id, workspace_id, SessionEvent::ProposalSetOpened { set: Box::new(set) })?;
        Ok(self.proposal_set(session_id, set_id)?.1)
    }

    /// Group proposals that are already filed into a new set.
    pub fn group_into_proposal_set(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        set: ProposalSet,
    ) -> Result<ProposalSet> {
        let set_id = set.id;
        let session = self
            .load(session_id)?
            .ok_or_else(|| ChatError::NotFound(format!("session {session_id}")))?;
        for id in &set.alternatives {
            if let Some(mut p) = session.proposals.iter().find(|p| p.id == *id).cloned() {
                p.set_id = Some(set_id);
                self.upsert_proposal(session_id, workspace_id, p)?;
            }
        }
        self.append_signed(session_id, workspace_id, SessionEvent::ProposalSetOpened { set: Box::new(set) })?;
        Ok(self.proposal_set(session_id, set_id)?.1)
    }

    /// Add a filed proposal to an open set (a revision of `revises`, or a
    /// competing proposal for the same request). A revision is new evidence,
    /// so the round advances too.
    pub fn add_set_alternative(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        set_id: Uuid,
        proposal_id: Uuid,
        revises: Option<Uuid>,
    ) -> Result<()> {
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::ProposalSetAlternativeAdded { set_id, proposal_id, revises },
        )?;
        if revises.is_some() {
            self.advance_set_round(session_id, workspace_id, set_id, "revision")?;
        }
        Ok(())
    }

    /// Record one scout's assessment. Returns the projected set, or `None` when
    /// the fold refused it (frozen, stopped, unknown alternative).
    pub fn post_scout_assessment(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        set_id: Uuid,
        assessment: ScoutAssessment,
    ) -> Result<Option<ProposalSet>> {
        let (session, set) = self.proposal_set(session_id, set_id)?;
        if set.is_frozen(assessment.proposal_id) || !set.is_active(&session.proposals, assessment.proposal_id) {
            return Ok(None);
        }
        let id = assessment.id;
        self.append_signed(session_id, workspace_id, SessionEvent::ScoutAssessmentPosted { set_id, assessment })?;
        let set = self.proposal_set(session_id, set_id)?.1;
        Ok(set.assessments.iter().any(|a| a.id == id).then_some(set))
    }

    /// Advance the set's evaluation round (new evidence): agent support decays.
    pub fn advance_set_round(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        set_id: Uuid,
        reason: &str,
    ) -> Result<()> {
        let (_, set) = self.proposal_set(session_id, set_id)?;
        if !set.is_open() {
            return Ok(());
        }
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::ProposalSetRoundAdvanced { set_id, round: set.round + 1, reason: reason.to_string() },
        )?;
        Ok(())
    }

    /// If an alternative has reached agent quorum (or the deadline has passed
    /// and one leads), move the set to piping and stop every active rival.
    /// `None` when nothing changed.
    pub fn settle_proposal_set(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        set_id: Uuid,
        now: Timestamp,
    ) -> Result<Option<QuorumOutcome>> {
        let (session, set) = self.proposal_set(session_id, set_id)?;
        let (winner, tie_break) = match set.quorum_candidate(&session.proposals) {
            Some(w) => (w, false),
            None => match set.deadline_leader(&session.proposals, now) {
                Some(w) => (w, true),
                None => return Ok(None),
            },
        };
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::ProposalSetQuorumReached { set_id, proposal_id: winner, tie_break, at: now },
        )?;
        let (session, set) = self.proposal_set(session_id, set_id)?;
        if set.front_runner != Some(winner) {
            return Ok(None); // another device's quorum folded first
        }
        let reason = if tie_break { StopReason::TieDeadline } else { StopReason::Quorum };
        let rivals: Vec<Uuid> = set
            .alternatives
            .iter()
            .copied()
            .filter(|id| *id != winner && set.is_active(&session.proposals, *id))
            .collect();
        let mut stopped = Vec::new();
        for rival in rivals {
            self.append_signed(
                session_id,
                workspace_id,
                SessionEvent::ProposalSetAlternativeStopped {
                    set_id,
                    proposal_id: rival,
                    stopped_by: Some(winner),
                    reason,
                },
            )?;
            stopped.push((rival, set.scouts_of(rival)));
        }
        Ok(Some(QuorumOutcome { front_runner: winner, tie_break, stopped }))
    }

    /// Withdraw an alternative for good (it can't be the front-runner).
    pub fn withdraw_set_alternative(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        set_id: Uuid,
        proposal_id: Uuid,
    ) -> Result<()> {
        let (_, set) = self.proposal_set(session_id, set_id)?;
        if set.front_runner == Some(proposal_id) {
            return Err(ChatError::Unauthorized("the front-runner can't be withdrawn; close the set instead".into()));
        }
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::ProposalSetAlternativeStopped {
                set_id,
                proposal_id,
                stopped_by: None,
                reason: StopReason::Withdrawn,
            },
        )?;
        Ok(())
    }

    /// The "fast ↔ careful" dial.
    pub fn tune_proposal_set(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        set_id: Uuid,
        knobs: QuorumKnobs,
    ) -> Result<ProposalSet> {
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::ProposalSetTuned {
                set_id,
                agent_quorum: knobs.agent_quorum,
                required_approvals: knobs.required_approvals,
                quality_bar: knobs.quality_bar,
            },
        )?;
        Ok(self.proposal_set(session_id, set_id)?.1)
    }

    /// Record a pre-flight report (phase one of the two-phase commit). A failed
    /// report sends the set back to scouting. Returns the projected set.
    pub fn record_set_preflight(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        set_id: Uuid,
        proposal_id: Uuid,
        report: PreflightReport,
    ) -> Result<ProposalSet> {
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::ProposalSetPreflightRecorded { set_id, proposal_id, report, at: Timestamp::now() },
        )?;
        Ok(self.proposal_set(session_id, set_id)?.1)
    }

    /// The human's final call. A winner must have human quorum and a passing
    /// pre-flight in this attempt; `None` closes the set without one.
    pub fn decide_proposal_set(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        set_id: Uuid,
        winner: Option<Uuid>,
    ) -> Result<ProposalSet> {
        let (session, set) = self.proposal_set(session_id, set_id)?;
        if !set.is_open() {
            return Err(ChatError::Unauthorized("this set is already decided".into()));
        }
        if let Some(w) = winner {
            if !set.can_decide_for(&session.proposals, w) {
                return Err(ChatError::Unauthorized(
                    "that alternative needs human approval and a passing pre-flight first".into(),
                ));
            }
        }
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::ProposalSetDecided { set_id, winner, at: Timestamp::now() },
        )?;
        let set = self.proposal_set(session_id, set_id)?.1;
        if set.phase != SetPhase::Decided {
            return Err(ChatError::Unauthorized("the decision didn't take (the set changed under it)".into()));
        }
        Ok(set)
    }

    /// Mark an applied proposal's change as reverted.
    pub fn mark_proposal_reverted(&mut self, session_id: Uuid, workspace_id: Uuid, proposal_id: Uuid) -> Result<()> {
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::ProposalReverted { proposal_id, at: Timestamp::now() },
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use hive_core::proposal_sets::{CheckOutcome, QuorumDial};
    use hive_core::{ActorIdentity, ProposalKind, ProposalStatus, SigningKeypair, WorkspaceRole};

    use super::*;
    use crate::event_store::EventStore;

    fn service() -> ChatService {
        let store = EventStore::open_in_memory().unwrap();
        let author = ActorIdentity::new("u1", "Mara", hive_core::ActorKind::Human);
        ChatService::new(
            store,
            Uuid::new_v4(),
            SigningKeypair::generate().unwrap(),
            SigningKeypair::generate().unwrap(),
            author,
        )
    }

    fn assessment(pid: Uuid, scout: Uuid, name: &str, score: u32) -> ScoutAssessment {
        ScoutAssessment {
            id: Uuid::new_v4(),
            proposal_id: pid,
            scout_agent_id: Some(scout),
            scout_name: name.into(),
            scout_runtime: "rt".into(),
            score,
            rationale: String::new(),
            round: 0,
            attempt: 0,
            created_at: Timestamp::now(),
        }
    }

    fn passing() -> PreflightReport {
        PreflightReport {
            tests: CheckOutcome::passed("ok"),
            base_unchanged: CheckOutcome::passed("ok"),
            hosts_online: CheckOutcome::passed("ok"),
        }
    }

    #[test]
    fn signed_round_trip_through_quorum_stop_preflight_and_decision() {
        let mut svc = service();
        let chat = svc.create_chat("Demo", Uuid::nil(), "anthropic").unwrap();
        svc.ensure_workspace_config(chat.workspace_id).unwrap();
        let (sid, wid) = (chat.id, chat.workspace_id);
        let alts: Vec<ActionProposal> = ["A", "B", "C"]
            .iter()
            .map(|t| ActionProposal::new(*t, ProposalKind::Decision, "planner"))
            .collect();
        let (a, b, c) = (alts[0].id, alts[1].id, alts[2].id);
        let set = svc.open_proposal_set(sid, wid, ProposalSet::new("Q", "planner", vec![]), alts).unwrap();
        assert_eq!(set.alternatives, vec![a, b, c]);
        let set_id = set.id;

        let (s1, s2, s3) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        svc.post_scout_assessment(sid, wid, set_id, assessment(b, s1, "coder", 80)).unwrap().unwrap();
        svc.post_scout_assessment(sid, wid, set_id, assessment(c, s3, "local", 95)).unwrap().unwrap();
        assert!(svc.settle_proposal_set(sid, wid, set_id, Timestamp::now()).unwrap().is_none());
        svc.post_scout_assessment(sid, wid, set_id, assessment(b, s2, "reviewer", 71)).unwrap().unwrap();
        let out = svc.settle_proposal_set(sid, wid, set_id, Timestamp::now()).unwrap().unwrap();
        assert_eq!(out.front_runner, b);
        assert!(!out.tie_break);
        assert_eq!(out.stopped, vec![(a, vec![]), (c, vec!["local".to_string()])]);
        // Rivals are frozen: a late assessment is refused.
        assert!(svc.post_scout_assessment(sid, wid, set_id, assessment(c, s1, "coder", 99)).unwrap().is_none());

        // Decision needs a human approval and a pre-flight.
        assert!(svc.decide_proposal_set(sid, wid, set_id, Some(b)).is_err());
        let r = svc.vote_on_proposal(sid, wid, b, true).unwrap().unwrap();
        assert_eq!(r.status, ProposalStatus::Approved);
        assert!(svc.decide_proposal_set(sid, wid, set_id, Some(b)).is_err(), "no pre-flight yet");
        svc.record_set_preflight(sid, wid, set_id, b, passing()).unwrap();
        let done = svc.decide_proposal_set(sid, wid, set_id, Some(b)).unwrap();
        assert_eq!(done.winner, Some(b));
        assert!(svc.decide_proposal_set(sid, wid, set_id, None).is_err(), "decided is terminal");
    }

    #[test]
    fn failed_preflight_returns_to_scouting_and_revisions_advance_the_round() {
        let mut svc = service();
        let chat = svc.create_chat("Demo", Uuid::nil(), "anthropic").unwrap();
        svc.ensure_workspace_config(chat.workspace_id).unwrap();
        let (sid, wid) = (chat.id, chat.workspace_id);
        let alts: Vec<ActionProposal> =
            ["A", "B"].iter().map(|t| ActionProposal::new(*t, ProposalKind::Decision, "planner")).collect();
        let (a, b) = (alts[0].id, alts[1].id);
        let set_id = svc.open_proposal_set(sid, wid, ProposalSet::new("Q", "planner", vec![]), alts).unwrap().id;
        svc.tune_proposal_set(sid, wid, set_id, QuorumDial::Fast.knobs()).unwrap();
        svc.post_scout_assessment(sid, wid, set_id, assessment(a, Uuid::new_v4(), "coder", 90)).unwrap();
        let out = svc.settle_proposal_set(sid, wid, set_id, Timestamp::now()).unwrap().unwrap();
        assert_eq!(out.front_runner, a, "fast: one scout is enough");
        let mut report = passing();
        report.tests = CheckOutcome::failed("2 tests failed");
        let set = svc.record_set_preflight(sid, wid, set_id, a, report).unwrap();
        assert_eq!(set.phase, SetPhase::Scouting);
        assert!(!set.is_frozen(b), "rivals thaw");

        // A revision of B joins after it and advances the round.
        let mut rev = ActionProposal::new("B v2", ProposalKind::Decision, "planner");
        rev.parent_id = Some(b);
        let rid = rev.id;
        svc.upsert_proposal(sid, wid, rev).unwrap();
        svc.add_set_alternative(sid, wid, set_id, rid, Some(b)).unwrap();
        let set = svc.load(sid).unwrap().unwrap().proposal_sets.remove(0);
        assert_eq!(set.alternatives, vec![a, b, rid]);
        assert_eq!(set.round, 2);
    }

    #[test]
    fn forged_set_events_synced_in_from_a_viewer_or_stranger_are_dropped_on_load() {
        let mut svc = service();
        let chat = svc.create_chat("Demo", Uuid::nil(), "anthropic").unwrap();
        svc.ensure_workspace_config(chat.workspace_id).unwrap();
        let (sid, wid) = (chat.id, chat.workspace_id);
        let val = ActorIdentity::new("val", "Val", hive_core::ActorKind::Human);
        svc.add_member(
            sid,
            wid,
            hive_core::WorkspaceMember {
                id: "val".into(),
                actor: val.clone(),
                role: WorkspaceRole::Viewer,
                title: String::new(),
                index: 0,
                joined_at: Timestamp::now(),
            },
        )
        .unwrap();
        let alts: Vec<ActionProposal> =
            ["A", "B"].iter().map(|t| ActionProposal::new(*t, ProposalKind::Decision, "planner")).collect();
        let a = alts[0].id;
        let set_id = svc.open_proposal_set(sid, wid, ProposalSet::new("Q", "planner", vec![]), alts).unwrap().id;

        // Signed envelopes from peers, as sync would store them: a Viewer's
        // failing pre-flight and tune, and a stranger closing the set.
        let forged = |by: &ActorIdentity, seq: i64, payload: SessionEvent| {
            let mut env = hive_core::SessionEventEnvelope::new(sid, wid, seq, payload);
            env.actor_stamp = Some(hive_core::ActorStamp { actor: by.clone(), recorded_at: Timestamp::now(), cfg_frontier: None, presence: None, runtime_effect: false, prev: None, sig_alg: Default::default(), log_head: None });
            env
        };
        let eve = ActorIdentity::new("eve", "Eve", hive_core::ActorKind::Human);
        let mut failing = passing();
        failing.tests = CheckOutcome::failed("forged");
        for env in [
            forged(&val, 9_001, SessionEvent::ProposalSetPreflightRecorded { set_id, proposal_id: a, report: failing, at: Timestamp::now() }),
            forged(&val, 9_002, SessionEvent::ProposalSetTuned { set_id, agent_quorum: 1, required_approvals: 1, quality_bar: 1 }),
            forged(&eve, 9_003, SessionEvent::ProposalSetDecided { set_id, winner: None, at: Timestamp::now() }),
        ] {
            let mut env = env;
            hive_core::lamport::stamp_next(&mut env, svc.store().lamport_head(sid).unwrap());
            svc.store().append_envelope(&env).unwrap();
        }
        // The raw fold (no workspace roster) would take them.
        let raw = svc.store().load_session(sid).unwrap().unwrap();
        assert_eq!(raw.proposal_sets[0].phase, SetPhase::Decided);
        // The service's load folds against the hoisted roster and drops them.
        let s = svc.load(sid).unwrap().unwrap();
        let set = &s.proposal_sets[0];
        assert!(set.is_open(), "eve's close was dropped");
        assert!(set.preflights.is_empty(), "val's pre-flight was dropped");
        assert_ne!(set.quality_bar, 1, "val's tune was dropped");
        // The owner's own events still fold.
        svc.tune_proposal_set(sid, wid, set_id, QuorumDial::Careful.knobs()).unwrap();
        assert_eq!(svc.load(sid).unwrap().unwrap().proposal_sets[0].agent_quorum, QuorumDial::Careful.knobs().agent_quorum);
    }

    #[test]
    fn a_viewer_cannot_decide_or_tune() {
        let mut svc = service();
        let chat = svc.create_chat("Demo", Uuid::nil(), "anthropic").unwrap();
        svc.ensure_workspace_config(chat.workspace_id).unwrap();
        let (sid, wid) = (chat.id, chat.workspace_id);
        let alts: Vec<ActionProposal> =
            ["A", "B"].iter().map(|t| ActionProposal::new(*t, ProposalKind::Decision, "planner")).collect();
        let set_id = svc.open_proposal_set(sid, wid, ProposalSet::new("Q", "planner", vec![]), alts).unwrap().id;
        // Demote the local actor (the chat creator, an Owner) to Viewer by
        // swapping identity to a non-member.
        svc.author = ActorIdentity::new("stranger", "Stranger", hive_core::ActorKind::Human);
        assert!(matches!(
            svc.tune_proposal_set(sid, wid, set_id, QuorumDial::Fast.knobs()),
            Err(ChatError::Unauthorized(_))
        ));
        assert!(matches!(svc.decide_proposal_set(sid, wid, set_id, None), Err(ChatError::Unauthorized(_))));
    }
}

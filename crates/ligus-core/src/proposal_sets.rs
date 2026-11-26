//! Proposal sets: one question, several competing alternatives, scored by
//! independent agent scouts before a human makes the call.
//!
//! A plain [`ActionProposal`] is judged alone. A **proposal set** groups N
//! alternatives that answer the same question ("explore options for X", or two
//! agents proposing for the same request) and runs a small decision process
//! over them, borrowed from how a honeybee swarm picks a nest site:
//!
//! 1. **Scouting.** Agents (scouts) assess one alternative each, blind: a scout
//!    is shown only the alternative it is rating, never another scout's score,
//!    and its reply's visible text is replaced by a neutral note so later scouts
//!    can't read it in the transcript. Each assessment is a 0–100 score.
//! 2. **Support decays.** An assessment's weight drops by `decay_per_round`
//!    points for every evaluation round since it was made. A round advances on
//!    new evidence (a revised alternative, a human comment, a failed
//!    pre-flight), so an early opinion that is not renewed can't lock the set.
//!    Human votes never decay: they ride the ordinary proposal votes.
//! 3. **Quorum at the option.** An alternative reaches *agent quorum* when at
//!    least `agent_quorum` distinct scouts' current support clears
//!    `quality_bar`. A scout's assessment of an alternative it produced never
//!    counts. Quorum moves the set to **piping**: that alternative is the
//!    front-runner, rivals are frozen (no new agent work), and the human gate
//!    is requested.
//! 4. **Stop signals.** Each rival is stopped, and its scouts are told to stand
//!    down. When nothing reaches quorum by the scouting deadline, the
//!    alternative with the most support wins the tie-break the same way.
//! 5. **Two-phase commit.** The human approves an alternative with the usual
//!    votes (`required_approvals` from members at or above the role floor, no
//!    self-approval, one qualifying rejection rejects). Then a **pre-flight**
//!    (tests in a scratch worktree, the base hasn't moved, the hosts are online)
//!    is recorded; only a passing pre-flight lets the change be applied and the
//!    set decided. A failed pre-flight sends the set back to scouting.
//!
//! Agents never decide: there is no agent path to a vote, a tuning change or a
//! decision, and the fold only accepts a decision for an alternative that has
//! human quorum and a passing pre-flight. Quorum among agents only nominates.
//!
//! Everything here is pure and deterministic. The fold is applied in canonical
//! event order on every device, and every guard reads only the event and the
//! state folded so far, so all devices reach the same set state.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::identity::WorkspaceRole;
use crate::proposals::{ActionProposal, ProposalStatus};
use crate::time_util::Timestamp;

/// Most alternatives a set holds. Keeps scouting cost bounded.
pub const MAX_ALTERNATIVES: usize = 6;
/// Highest assessment score.
pub const MAX_SCORE: u32 = 100;
/// Bounds for the agent quorum `k` and the human approvals.
pub const MAX_AGENT_QUORUM: u32 = 5;
pub const MAX_REQUIRED_APPROVALS: u32 = 5;
/// Default scouting deadline after a set opens, in seconds. Swarms take hours;
/// people expect minutes, so this is short (and only matters for ties).
pub const DEFAULT_DEADLINE_SECS: i64 = 30 * 60;
/// The key a scout or producer is known by when it is the chat's primary runtime.
pub const PRIMARY_KEY: &str = "primary";

/// Where a set is in its decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum SetPhase {
    /// Scouts are assessing alternatives.
    #[default]
    Scouting,
    /// An alternative reached agent quorum (or won the deadline tie-break): it is
    /// the front-runner, rivals are frozen, a human decision is requested.
    Piping,
    /// A human closed the set, with a winner that was applied or with none.
    Decided,
    /// A phase this build doesn't know (written by a newer client).
    #[serde(other)]
    Unknown,
}

/// Why an alternative was stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
    /// A rival reached agent quorum (cross-inhibition).
    Quorum,
    /// Nothing reached quorum by the deadline; a rival had more support.
    TieDeadline,
    /// Withdrawn by a human or its producer. Permanent.
    Withdrawn,
    #[serde(other)]
    Unknown,
}

/// One scout's assessment of one alternative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoutAssessment {
    pub id: Uuid,
    /// The alternative (a proposal id) this scout rated.
    pub proposal_id: Uuid,
    /// The scouting agent; `None` = the chat's primary runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scout_agent_id: Option<Uuid>,
    /// The scout's display name, as the responder that ran the turn (set by the
    /// system from the turn, never taken from the agent's own text).
    pub scout_name: String,
    /// Runtime/model label of the scout, so the view can show how diverse the
    /// scouts were (same-model scouts share biases).
    #[serde(default)]
    pub scout_runtime: String,
    /// 0..=100, clamped on fold.
    pub score: u32,
    #[serde(default)]
    pub rationale: String,
    /// The set's evaluation round this assessment counts from. Stamped by the
    /// fold, not by the author, so a scout can't claim fresher evidence.
    #[serde(default)]
    pub round: u32,
    /// The set's attempt (bumped when a failed pre-flight returns it to scouting).
    #[serde(default)]
    pub attempt: u32,
    #[serde(default)]
    pub created_at: Timestamp,
}

impl ScoutAssessment {
    /// Stable identity of the scout: its agent id, or [`PRIMARY_KEY`].
    pub fn scout_key(&self) -> String {
        self.scout_agent_id.map(|a| a.to_string()).unwrap_or_else(|| PRIMARY_KEY.to_string())
    }
}

/// A stop signal delivered to one alternative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopRecord {
    pub proposal_id: Uuid,
    /// The alternative whose support stopped it, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_by: Option<Uuid>,
    pub reason: StopReason,
    #[serde(default)]
    pub attempt: u32,
}

/// Outcome of one pre-flight check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CheckStatus {
    Passed,
    Failed,
    /// Not applicable or not configured (e.g. no test command set).
    Skipped,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckOutcome {
    pub status: CheckStatus,
    /// Short human note (never file contents).
    #[serde(default)]
    pub note: String,
}

impl CheckOutcome {
    pub fn passed(note: impl Into<String>) -> Self {
        Self { status: CheckStatus::Passed, note: note.into() }
    }
    pub fn failed(note: impl Into<String>) -> Self {
        Self { status: CheckStatus::Failed, note: note.into() }
    }
    pub fn skipped(note: impl Into<String>) -> Self {
        Self { status: CheckStatus::Skipped, note: note.into() }
    }
}

/// The pre-flight ("piping") checks run before an alternative is applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightReport {
    /// The workspace's pre-flight test command, run in a scratch worktree with
    /// the alternative applied.
    pub tests: CheckOutcome,
    /// The base the alternative was cut against hasn't moved, and its patch
    /// still applies.
    pub base_unchanged: CheckOutcome,
    /// The hosts needed to carry it out are online.
    pub hosts_online: CheckOutcome,
}

impl PreflightReport {
    /// Passes when no check failed. A check of unknown status (newer client)
    /// counts as a failure: fail closed.
    pub fn passed(&self) -> bool {
        [&self.tests, &self.base_unchanged, &self.hosts_online]
            .iter()
            .all(|c| matches!(c.status, CheckStatus::Passed | CheckStatus::Skipped))
    }
}

/// A recorded pre-flight for one alternative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightRecord {
    pub proposal_id: Uuid,
    pub attempt: u32,
    pub report: PreflightReport,
    #[serde(default)]
    pub at: Timestamp,
}

/// Preset positions of the "fast ↔ careful" dial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuorumDial {
    Fast,
    Balanced,
    Careful,
}

/// The knobs a dial position sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuorumKnobs {
    pub agent_quorum: u32,
    pub required_approvals: u32,
    pub quality_bar: u32,
}

impl QuorumDial {
    pub fn knobs(self) -> QuorumKnobs {
        match self {
            QuorumDial::Fast => QuorumKnobs { agent_quorum: 1, required_approvals: 1, quality_bar: 60 },
            QuorumDial::Balanced => QuorumKnobs { agent_quorum: 2, required_approvals: 1, quality_bar: 70 },
            QuorumDial::Careful => QuorumKnobs { agent_quorum: 3, required_approvals: 2, quality_bar: 80 },
        }
    }
}

/// How one alternative stands right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Standing {
    /// Sum of current (decayed) support from counting scouts.
    pub support: u32,
    /// Counting scouts whose current support clears the quality bar.
    pub qualifying_scouts: u32,
    /// Counting scouts that assessed it (latest assessment each).
    pub scouts: u32,
    /// Distinct runtimes among those scouts.
    pub runtimes: u32,
    /// Self-assessments ignored (a scout rating an alternative it produced).
    pub self_assessments: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposalSet {
    pub id: Uuid,
    /// The question the alternatives answer.
    pub question: String,
    #[serde(default)]
    pub body: String,
    /// Who opened the set (an agent name, or a member id).
    #[serde(default)]
    pub author_actor_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producing_agent_id: Option<Uuid>,
    /// Short class of the work (`refactor`, `docs`, `triage`…): a grouping label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_class: Option<String>,
    /// The user message the set answers, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_message_id: Option<Uuid>,
    /// Alternative proposal ids, in the order they were proposed.
    pub alternatives: Vec<Uuid>,
    /// `k`: distinct scouts that must clear the bar for agent quorum.
    pub agent_quorum: u32,
    /// Score a scout's current support must reach to count toward quorum.
    pub quality_bar: u32,
    /// Support points an assessment loses per round since it was made.
    pub decay_per_round: u32,
    /// Human approvals the chosen alternative needs (applied to every alternative).
    pub required_approvals: u32,
    pub approval_role_floor: WorkspaceRole,
    /// After this, a tie (nothing at quorum) is broken by support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scouting_deadline: Option<Timestamp>,
    /// The commit the alternatives were cut against, on the device that opened
    /// the set (pre-flight compares when it can resolve it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
    #[serde(default)]
    pub created_at: Timestamp,

    // ---- folded state (reset on open; never taken from the opener) ----
    #[serde(default)]
    pub phase: SetPhase,
    #[serde(default)]
    pub round: u32,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub front_runner: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quorum_at: Option<Timestamp>,
    /// The front-runner won the deadline tie-break rather than a quorum.
    #[serde(default)]
    pub tie_break: bool,
    #[serde(default)]
    pub assessments: Vec<ScoutAssessment>,
    #[serde(default)]
    pub stops: Vec<StopRecord>,
    #[serde(default)]
    pub preflights: Vec<PreflightRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<Timestamp>,
}

impl ProposalSet {
    /// A new set over `alternatives`, at the balanced dial position.
    pub fn new(question: impl Into<String>, author_actor_id: impl Into<String>, alternatives: Vec<Uuid>) -> Self {
        let now = Timestamp::now();
        let knobs = QuorumDial::Balanced.knobs();
        Self {
            id: Uuid::new_v4(),
            question: question.into(),
            body: String::new(),
            author_actor_id: author_actor_id.into(),
            producing_agent_id: None,
            task_class: None,
            trigger_message_id: None,
            alternatives,
            agent_quorum: knobs.agent_quorum,
            quality_bar: knobs.quality_bar,
            decay_per_round: 25,
            required_approvals: knobs.required_approvals,
            approval_role_floor: WorkspaceRole::Contributor,
            scouting_deadline: Some(Timestamp::from_unix_seconds(
                now.inner().unix_timestamp() + DEFAULT_DEADLINE_SECS,
            )),
            base_commit: None,
            created_at: now,
            phase: SetPhase::Scouting,
            round: 0,
            attempt: 0,
            front_runner: None,
            quorum_at: None,
            tie_break: false,
            assessments: Vec::new(),
            stops: Vec::new(),
            preflights: Vec::new(),
            winner: None,
            decided_at: None,
        }
    }

    pub fn is_open(&self) -> bool {
        matches!(self.phase, SetPhase::Scouting | SetPhase::Piping)
    }

    pub fn contains(&self, proposal_id: Uuid) -> bool {
        self.alternatives.contains(&proposal_id)
    }

    /// Clamp knobs into range and reset everything the fold owns, so an opener
    /// can't seed a set that is already decided or pre-scored.
    fn normalized(mut self) -> Self {
        self.agent_quorum = self.agent_quorum.clamp(1, MAX_AGENT_QUORUM);
        self.required_approvals = self.required_approvals.clamp(1, MAX_REQUIRED_APPROVALS);
        self.quality_bar = self.quality_bar.min(MAX_SCORE);
        self.decay_per_round = self.decay_per_round.min(MAX_SCORE);
        let mut seen = Vec::new();
        self.alternatives.retain(|id| {
            let fresh = !seen.contains(id);
            seen.push(*id);
            fresh
        });
        self.alternatives.truncate(MAX_ALTERNATIVES);
        self.phase = SetPhase::Scouting;
        self.round = 0;
        self.attempt = 0;
        self.front_runner = None;
        self.quorum_at = None;
        self.tie_break = false;
        self.assessments.clear();
        self.stops.clear();
        self.preflights.clear();
        self.winner = None;
        self.decided_at = None;
        self
    }

    /// The stop record that currently holds `proposal_id`, if any: a permanent
    /// withdrawal, or a stop from this attempt.
    pub fn stop_of(&self, proposal_id: Uuid) -> Option<&StopRecord> {
        self.stops.iter().find(|s| {
            s.proposal_id == proposal_id && (s.reason == StopReason::Withdrawn || s.attempt == self.attempt)
        })
    }

    /// Whether `proposal_id` is a live alternative: listed, its proposal exists,
    /// not superseded or rejected, and not stopped.
    pub fn is_active(&self, proposals: &[ActionProposal], proposal_id: Uuid) -> bool {
        self.contains(proposal_id)
            && self.stop_of(proposal_id).is_none()
            && proposals.iter().any(|p| {
                p.id == proposal_id && p.superseded_by.is_none() && p.status != ProposalStatus::Rejected
            })
    }

    /// Frozen = no new agent work: a rival while piping, or a stopped alternative.
    pub fn is_frozen(&self, proposal_id: Uuid) -> bool {
        (self.phase == SetPhase::Piping && self.front_runner != Some(proposal_id))
            || self.stop_of(proposal_id).is_some()
            || self.phase == SetPhase::Decided
    }

    /// Current (decayed) support of one assessment.
    pub fn current_support(&self, a: &ScoutAssessment) -> u32 {
        let rounds = self.round.saturating_sub(a.round);
        a.score.saturating_sub(self.decay_per_round.saturating_mul(rounds))
    }

    /// How `proposal_id` stands. Only the latest assessment per scout counts
    /// (later assessments replace earlier ones on fold), and a scout rating an
    /// alternative it produced is ignored.
    pub fn standing(&self, proposals: &[ActionProposal], proposal_id: Uuid) -> Standing {
        let Some(p) = proposals.iter().find(|p| p.id == proposal_id) else {
            return Standing::default();
        };
        let mut st = Standing::default();
        let mut runtimes: Vec<&str> = Vec::new();
        for a in self.assessments.iter().filter(|a| a.proposal_id == proposal_id) {
            if is_self_assessment(p, a) {
                st.self_assessments += 1;
                continue;
            }
            let support = self.current_support(a);
            st.scouts += 1;
            st.support += support;
            if support >= self.quality_bar && support > 0 {
                st.qualifying_scouts += 1;
            }
            if !runtimes.contains(&a.scout_runtime.as_str()) {
                runtimes.push(a.scout_runtime.as_str());
            }
        }
        st.runtimes = runtimes.len() as u32;
        st
    }

    pub fn meets_quorum(&self, proposals: &[ActionProposal], proposal_id: Uuid) -> bool {
        self.is_active(proposals, proposal_id)
            && self.standing(proposals, proposal_id).qualifying_scouts >= self.agent_quorum
    }

    /// The alternative that has reached agent quorum, if the set is scouting.
    /// With several at quorum at once, the most support wins, then the earliest
    /// proposed (first-proposed is only a last tie-break).
    pub fn quorum_candidate(&self, proposals: &[ActionProposal]) -> Option<Uuid> {
        if self.phase != SetPhase::Scouting {
            return None;
        }
        self.ranked(proposals).into_iter().find(|id| self.meets_quorum(proposals, *id))
    }

    /// Past the deadline with nothing at quorum: the alternative with the most
    /// support (then more qualifying scouts, then earliest) — as long as some
    /// scout supports it at all.
    pub fn deadline_leader(&self, proposals: &[ActionProposal], now: Timestamp) -> Option<Uuid> {
        if self.phase != SetPhase::Scouting || !self.deadline_passed(now) {
            return None;
        }
        if self.quorum_candidate(proposals).is_some() {
            return None;
        }
        self.ranked(proposals)
            .into_iter()
            .next()
            .filter(|id| self.standing(proposals, *id).support > 0)
    }

    pub fn deadline_passed(&self, now: Timestamp) -> bool {
        self.scouting_deadline.is_some_and(|d| now >= d)
    }

    /// Active alternatives, best first: support, then qualifying scouts, then
    /// proposal order. Deterministic.
    pub fn ranked(&self, proposals: &[ActionProposal]) -> Vec<Uuid> {
        let mut ids: Vec<(usize, Uuid, Standing)> = self
            .alternatives
            .iter()
            .enumerate()
            .filter(|(_, id)| self.is_active(proposals, **id))
            .map(|(i, id)| (i, *id, self.standing(proposals, *id)))
            .collect();
        ids.sort_by(|a, b| {
            b.2.support
                .cmp(&a.2.support)
                .then(b.2.qualifying_scouts.cmp(&a.2.qualifying_scouts))
                .then(a.0.cmp(&b.0))
        });
        ids.into_iter().map(|(_, id, _)| id).collect()
    }

    /// The latest pre-flight for `proposal_id` in the current attempt.
    pub fn latest_preflight(&self, proposal_id: Uuid) -> Option<&PreflightRecord> {
        self.preflights
            .iter()
            .rev()
            .find(|r| r.proposal_id == proposal_id && r.attempt == self.attempt)
    }

    /// Whether a human may decide for `proposal_id` now: it is active, it has
    /// human quorum (approved, or already applied), and its latest pre-flight
    /// in this attempt passed.
    pub fn can_decide_for(&self, proposals: &[ActionProposal], proposal_id: Uuid) -> bool {
        let human_ok = proposals.iter().any(|p| {
            p.id == proposal_id && (p.ready_to_apply() || p.status == ProposalStatus::Applied)
        });
        self.is_open()
            && self.is_active(proposals, proposal_id)
            && human_ok
            && self.latest_preflight(proposal_id).is_some_and(|r| r.report.passed())
    }

    /// Scouts (by name) whose latest assessment backs `proposal_id`: the ones a
    /// stop signal for it goes to.
    pub fn scouts_of(&self, proposal_id: Uuid) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for a in self.assessments.iter().filter(|a| a.proposal_id == proposal_id) {
            if !names.contains(&a.scout_name) {
                names.push(a.scout_name.clone());
            }
        }
        names
    }
}

/// Whether a scout is rating an alternative it produced. The producer is the
/// proposal's producing agent, else its author: an empty author with no agent
/// is a diff captured from the primary runtime.
pub fn is_self_assessment(p: &ActionProposal, a: &ScoutAssessment) -> bool {
    let producer = match p.producing_agent_id {
        Some(id) => id.to_string(),
        None if p.author_actor_id.is_empty() => PRIMARY_KEY.to_string(),
        None => String::new(),
    };
    (!producer.is_empty() && producer == a.scout_key())
        || (!p.author_actor_id.is_empty() && p.author_actor_id == a.scout_name)
}

/// The set events, as the fold sees them. Built by `events.rs` from the
/// matching `SessionEvent` variants so the fold lives here.
pub enum SetChange<'a> {
    Opened(&'a ProposalSet),
    AlternativeAdded { set_id: Uuid, proposal_id: Uuid, revises: Option<Uuid> },
    Assessed { set_id: Uuid, assessment: &'a ScoutAssessment },
    RoundAdvanced { set_id: Uuid, round: u32 },
    QuorumReached { set_id: Uuid, proposal_id: Uuid, tie_break: bool, at: Timestamp },
    Stopped { set_id: Uuid, proposal_id: Uuid, stopped_by: Option<Uuid>, reason: StopReason },
    Tuned { set_id: Uuid, agent_quorum: u32, required_approvals: u32, quality_bar: u32 },
    Preflight { set_id: Uuid, proposal_id: Uuid, report: &'a PreflightReport, at: Timestamp },
    Decided { set_id: Uuid, winner: Option<Uuid>, at: Timestamp },
}

/// Fold one set event. Every guard is a pure function of the event and the
/// state so far; an event that fails one is ignored (a no-op), the same way on
/// every device.
pub fn apply(sets: &mut Vec<ProposalSet>, proposals: &mut [ActionProposal], change: SetChange<'_>) {
    match change {
        SetChange::Opened(set) => {
            if sets.iter().any(|s| s.id == set.id) {
                return; // first open wins
            }
            let mut set = set.clone().normalized();
            // Only alternatives already filed (they are authored before the set)
            // and not claimed by another set.
            set.alternatives.retain(|id| {
                proposals.iter().any(|p| p.id == *id) && !sets.iter().any(|s| s.contains(*id))
            });
            if set.alternatives.is_empty() {
                return;
            }
            for id in set.alternatives.clone() {
                stamp(&set, proposals, id);
            }
            sets.push(set);
        }
        SetChange::AlternativeAdded { set_id, proposal_id, revises } => {
            if sets.iter().any(|s| s.contains(proposal_id)) || !proposals.iter().any(|p| p.id == proposal_id) {
                return;
            }
            let Some(set) = sets.iter_mut().find(|s| s.id == set_id) else { return };
            if !set.is_open() || set.alternatives.len() >= MAX_ALTERNATIVES && revises.is_none() {
                return;
            }
            // A revision takes its parent's slot in the order.
            match revises.and_then(|r| set.alternatives.iter().position(|a| *a == r)) {
                Some(pos) => set.alternatives.insert(pos + 1, proposal_id),
                None => set.alternatives.push(proposal_id),
            }
            stamp(set, proposals, proposal_id);
        }
        SetChange::Assessed { set_id, assessment } => {
            let Some(set) = sets.iter_mut().find(|s| s.id == set_id) else { return };
            let pid = assessment.proposal_id;
            let accepting = match set.phase {
                SetPhase::Scouting => true,
                // Rivals are frozen while piping; the front-runner can still be
                // re-assessed (fresh evidence for the human).
                SetPhase::Piping => set.front_runner == Some(pid),
                _ => false,
            };
            if !accepting || !set.is_active(proposals, pid) || assessment.scout_name.trim().is_empty() {
                return;
            }
            let mut a = assessment.clone();
            a.score = a.score.min(MAX_SCORE);
            a.round = set.round;
            a.attempt = set.attempt;
            let key = a.scout_key();
            set.assessments.retain(|x| !(x.proposal_id == pid && x.scout_key() == key));
            set.assessments.push(a);
        }
        SetChange::RoundAdvanced { set_id, round } => {
            let Some(set) = sets.iter_mut().find(|s| s.id == set_id) else { return };
            // Exactly the next round: concurrent advances collapse into one, and
            // nobody can jump the clock to wipe out every scout's support.
            if set.is_open() && round == set.round + 1 {
                set.round = round;
            }
        }
        SetChange::QuorumReached { set_id, proposal_id, tie_break, at } => {
            let Some(set) = sets.iter_mut().find(|s| s.id == set_id) else { return };
            let valid = if tie_break {
                set.deadline_leader(proposals, at) == Some(proposal_id)
            } else {
                set.phase == SetPhase::Scouting && set.meets_quorum(proposals, proposal_id)
            };
            if !valid {
                return;
            }
            set.phase = SetPhase::Piping;
            set.front_runner = Some(proposal_id);
            set.quorum_at = Some(at);
            set.tie_break = tie_break;
        }
        SetChange::Stopped { set_id, proposal_id, stopped_by, reason } => {
            let Some(set) = sets.iter_mut().find(|s| s.id == set_id) else { return };
            if !set.is_open() || !set.contains(proposal_id) || set.stop_of(proposal_id).is_some() {
                return;
            }
            let valid = match reason {
                // Cross-inhibition only comes from the front-runner, onto a rival.
                StopReason::Quorum | StopReason::TieDeadline => {
                    set.phase == SetPhase::Piping
                        && stopped_by.is_some()
                        && stopped_by == set.front_runner
                        && Some(proposal_id) != set.front_runner
                }
                StopReason::Withdrawn => Some(proposal_id) != set.front_runner,
                StopReason::Unknown => false,
            };
            if valid {
                let attempt = set.attempt;
                set.stops.push(StopRecord { proposal_id, stopped_by, reason, attempt });
            }
        }
        SetChange::Tuned { set_id, agent_quorum, required_approvals, quality_bar } => {
            let Some(set) = sets.iter_mut().find(|s| s.id == set_id) else { return };
            if !set.is_open() {
                return;
            }
            set.agent_quorum = agent_quorum.clamp(1, MAX_AGENT_QUORUM);
            set.required_approvals = required_approvals.clamp(1, MAX_REQUIRED_APPROVALS);
            set.quality_bar = quality_bar.min(MAX_SCORE);
            for id in set.alternatives.clone() {
                stamp(set, proposals, id);
            }
        }
        SetChange::Preflight { set_id, proposal_id, report, at } => {
            let Some(set) = sets.iter_mut().find(|s| s.id == set_id) else { return };
            if !set.is_open() || !set.is_active(proposals, proposal_id) {
                return;
            }
            set.preflights.push(PreflightRecord {
                proposal_id,
                attempt: set.attempt,
                report: report.clone(),
                at,
            });
            if !report.passed() {
                // Back to scouting: a new attempt (this attempt's stops lift,
                // so rivals can be re-assessed) and a new round (the test
                // results are evidence, so old support decays).
                set.phase = SetPhase::Scouting;
                set.front_runner = None;
                set.quorum_at = None;
                set.tie_break = false;
                set.attempt += 1;
                set.round += 1;
            }
        }
        SetChange::Decided { set_id, winner, at } => {
            let Some(set) = sets.iter().find(|s| s.id == set_id) else { return };
            let valid = match winner {
                None => set.is_open(),
                Some(w) => set.can_decide_for(proposals, w),
            };
            if !valid {
                return;
            }
            let set = sets.iter_mut().find(|s| s.id == set_id).expect("found above");
            set.phase = SetPhase::Decided;
            set.winner = winner;
            set.decided_at = Some(at);
        }
    }
}

/// Mark `proposal_id` as an alternative of `set` and give it the set's human
/// gate (approvals + role floor), recomputing its status.
fn stamp(set: &ProposalSet, proposals: &mut [ActionProposal], proposal_id: Uuid) {
    if let Some(p) = proposals.iter_mut().find(|p| p.id == proposal_id) {
        p.set_id = Some(set.id);
        if p.task_class.is_none() {
            p.task_class = set.task_class.clone();
        }
        p.set_human_gate(set.required_approvals, set.approval_role_floor);
    }
}

/// Re-apply set membership after a full proposal upsert (an older peer's
/// snapshot drops the unknown `set_id`; the set's list is the source of truth).
/// A `set_id` naming a set that doesn't list the proposal is cleared.
pub fn restamp(sets: &[ProposalSet], proposals: &mut [ActionProposal], proposal_id: Uuid) {
    if let Some(set) = sets.iter().find(|s| s.contains(proposal_id)) {
        stamp(set, proposals, proposal_id);
    } else if let Some(p) = proposals.iter_mut().find(|p| p.id == proposal_id) {
        if p.set_id.is_some_and(|sid| sets.iter().any(|s| s.id == sid)) {
            p.set_id = None;
        }
    }
}

/// Author-side checks beyond the role floor for set events. `Some(reason)`
/// denies. Run against the full (hoisted) roster when the event is authored.
pub fn denies(
    event: &crate::events::SessionEvent,
    actor_id: &str,
    actor_role: WorkspaceRole,
    session: &crate::session::ChatSession,
) -> Option<String> {
    use crate::events::SessionEvent as E;
    let set_of = |id: &Uuid| session.proposal_sets.iter().find(|s| s.id == *id);
    match event {
        E::ProposalSetDecided { set_id, winner, .. } => {
            let set = set_of(set_id)?;
            if actor_role.rank() < set.approval_role_floor.rank() {
                return Some(format!("role {actor_role:?} is below this set's decision floor"));
            }
            let w = (*winner)?;
            let p = session.proposals.iter().find(|p| p.id == w)?;
            (!p.author_actor_id.is_empty() && p.author_actor_id == actor_id)
                .then(|| "you can't decide for your own alternative".to_string())
        }
        E::ProposalSetTuned { set_id, .. } => {
            let set = set_of(set_id)?;
            (actor_role.rank() < set.approval_role_floor.rank())
                .then(|| format!("role {actor_role:?} is below this set's decision floor"))
        }
        _ => None,
    }
}

/// Whether a user message asks to compare options, so competing proposals that
/// answer it are grouped into a set. Conservative on purpose: two agents
/// editing for one request are usually doing complementary work.
pub fn wants_options(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    ["option", "alternative", "approaches", "compare", "explore", "which is better", "competing"]
        .iter()
        .any(|k| m.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proposals::{ProposalApproval, ProposalKind};

    fn alt(title: &str, producer: Option<Uuid>) -> ActionProposal {
        let mut p = ActionProposal::new(title, ProposalKind::Decision, "");
        p.producing_agent_id = producer;
        p.author_actor_id = producer.map(|_| String::new()).unwrap_or_else(|| "someone".into());
        p
    }

    struct Fx {
        sets: Vec<ProposalSet>,
        props: Vec<ActionProposal>,
        set_id: Uuid,
        a: Uuid,
        b: Uuid,
        c: Uuid,
        scouts: [Uuid; 4],
    }

    fn fixture() -> Fx {
        let producer = Uuid::new_v4();
        let props = vec![alt("A", Some(producer)), alt("B", None), alt("C", None)];
        let (a, b, c) = (props[0].id, props[1].id, props[2].id);
        let set = ProposalSet::new("Which cache?", "agent", vec![a, b, c]);
        let set_id = set.id;
        let mut fx = Fx {
            sets: Vec::new(),
            props,
            set_id,
            a,
            b,
            c,
            scouts: [producer, Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()],
        };
        apply(&mut fx.sets, &mut fx.props, SetChange::Opened(&set));
        fx
    }

    fn assess(fx: &mut Fx, scout: usize, pid: Uuid, score: u32) {
        let a = ScoutAssessment {
            id: Uuid::new_v4(),
            proposal_id: pid,
            scout_agent_id: Some(fx.scouts[scout]),
            scout_name: format!("scout{scout}"),
            scout_runtime: format!("rt{}", scout % 2),
            score,
            rationale: String::new(),
            round: 999, // ignored: the fold stamps the round
            attempt: 0,
            created_at: Timestamp::epoch(),
        };
        let set_id = fx.set_id;
        apply(&mut fx.sets, &mut fx.props, SetChange::Assessed { set_id, assessment: &a });
    }

    fn set(fx: &Fx) -> &ProposalSet {
        &fx.sets[0]
    }

    fn vote(fx: &mut Fx, pid: Uuid, actor: &str, role: WorkspaceRole, approved: bool) {
        let p = fx.props.iter_mut().find(|p| p.id == pid).unwrap();
        p.cast_vote(ProposalApproval { actor_id: actor.into(), role, approved, created_at: Timestamp::epoch(), diff_digest: None });
    }

    fn passing() -> PreflightReport {
        PreflightReport {
            tests: CheckOutcome::passed("ok"),
            base_unchanged: CheckOutcome::passed("same base"),
            hosts_online: CheckOutcome::skipped("no remote hosts"),
        }
    }

    #[test]
    fn opening_stamps_alternatives_with_the_set_gate_and_resets_folded_state() {
        let props = vec![alt("A", None), alt("B", None)];
        let mut forged = ProposalSet::new("Q", "x", vec![props[0].id, props[1].id, props[0].id, Uuid::new_v4()]);
        forged.phase = SetPhase::Decided;
        forged.winner = Some(props[0].id);
        forged.required_approvals = 99;
        forged.approval_role_floor = WorkspaceRole::Admin;
        let (mut sets, mut props) = (Vec::new(), props);
        apply(&mut sets, &mut props, SetChange::Opened(&forged));
        let s = &sets[0];
        assert_eq!(s.phase, SetPhase::Scouting, "an opener can't seed a decided set");
        assert_eq!(s.winner, None);
        assert_eq!(s.alternatives.len(), 2, "duplicates and unknown proposals are dropped");
        assert_eq!(s.required_approvals, MAX_REQUIRED_APPROVALS);
        for p in &props {
            assert_eq!(p.set_id, Some(s.id));
            assert_eq!(p.required_approvals, MAX_REQUIRED_APPROVALS);
            assert_eq!(p.approval_role_floor, WorkspaceRole::Admin);
        }
        // Re-opening the same id is ignored; a second set can't claim them.
        let again = ProposalSet { id: s.id, ..ProposalSet::new("other", "y", vec![props[0].id]) };
        apply(&mut sets, &mut props, SetChange::Opened(&again));
        let other = ProposalSet::new("other", "y", vec![props[0].id]);
        apply(&mut sets, &mut props, SetChange::Opened(&other));
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].question, "Q");
    }

    #[test]
    fn support_decays_per_round_unless_renewed() {
        let mut fx = fixture();
        { let p = fx.b; assess(&mut fx, 1, p, 90); }
        assert_eq!(set(&fx).assessments[0].round, 0, "round is stamped by the fold");
        assert_eq!(set(&fx).standing(&fx.props, fx.b).support, 90);
        let id = fx.set_id;
        apply(&mut fx.sets, &mut fx.props, SetChange::RoundAdvanced { set_id: id, round: 1 });
        assert_eq!(set(&fx).standing(&fx.props, fx.b).support, 65);
        apply(&mut fx.sets, &mut fx.props, SetChange::RoundAdvanced { set_id: id, round: 2 });
        assert_eq!(set(&fx).standing(&fx.props, fx.b).support, 40);
        assert_eq!(set(&fx).standing(&fx.props, fx.b).qualifying_scouts, 0, "below the bar now");
        // Re-evaluating against the new evidence renews it.
        { let p = fx.b; assess(&mut fx, 1, p, 85); }
        let st = set(&fx).standing(&fx.props, fx.b);
        assert_eq!((st.support, st.scouts, st.qualifying_scouts), (85, 1, 1), "latest per scout replaces");
        // Support floors at zero.
        for r in 3..=10 {
            apply(&mut fx.sets, &mut fx.props, SetChange::RoundAdvanced { set_id: id, round: r });
        }
        assert_eq!(set(&fx).standing(&fx.props, fx.b).support, 0);
    }

    #[test]
    fn rounds_only_advance_one_at_a_time() {
        let mut fx = fixture();
        let id = fx.set_id;
        apply(&mut fx.sets, &mut fx.props, SetChange::RoundAdvanced { set_id: id, round: 50 });
        assert_eq!(set(&fx).round, 0, "a jump that would wipe all support is ignored");
        apply(&mut fx.sets, &mut fx.props, SetChange::RoundAdvanced { set_id: id, round: 1 });
        apply(&mut fx.sets, &mut fx.props, SetChange::RoundAdvanced { set_id: id, round: 1 });
        assert_eq!(set(&fx).round, 1, "concurrent advances to the same round collapse");
    }

    #[test]
    fn human_votes_never_decay() {
        let mut fx = fixture();
        let b = fx.b;
        vote(&mut fx, b, "human", WorkspaceRole::Contributor, true);
        let id = fx.set_id;
        for r in 1..=8 {
            apply(&mut fx.sets, &mut fx.props, SetChange::RoundAdvanced { set_id: id, round: r });
        }
        let p = fx.props.iter().find(|p| p.id == b).unwrap();
        assert_eq!(p.qualifying_approvals(), 1);
        assert!(p.ready_to_apply());
    }

    #[test]
    fn quorum_needs_k_distinct_scouts_over_the_bar_and_ignores_self_assessment() {
        let mut fx = fixture();
        // Scout 0 produced A: its own assessment never counts.
        { let p = fx.a; assess(&mut fx, 0, p, 100); }
        { let p = fx.a; assess(&mut fx, 1, p, 90); }
        let st = set(&fx).standing(&fx.props, fx.a);
        assert_eq!((st.scouts, st.self_assessments, st.qualifying_scouts), (1, 1, 1));
        assert_eq!(set(&fx).quorum_candidate(&fx.props), None, "k=2 not met");
        // The same scout twice is still one scout.
        { let p = fx.a; assess(&mut fx, 1, p, 95); }
        assert_eq!(set(&fx).quorum_candidate(&fx.props), None);
        // A below-bar score doesn't count.
        { let p = fx.a; assess(&mut fx, 2, p, 50); }
        assert_eq!(set(&fx).quorum_candidate(&fx.props), None);
        { let p = fx.a; assess(&mut fx, 3, p, 72); }
        assert_eq!(set(&fx).quorum_candidate(&fx.props), Some(fx.a));
        assert_eq!(set(&fx).standing(&fx.props, fx.a).runtimes, 2);
    }

    #[test]
    fn quorum_moves_to_piping_freezes_rivals_and_accepts_stop_signals_only_from_the_front_runner() {
        let mut fx = fixture();
        { let p = fx.b; assess(&mut fx, 1, p, 80); }
        { let p = fx.c; assess(&mut fx, 2, p, 90); }
        let (id, a, b, c) = (fx.set_id, fx.a, fx.b, fx.c);
        // A forged quorum for an alternative that doesn't have it is ignored.
        apply(&mut fx.sets, &mut fx.props, SetChange::QuorumReached { set_id: id, proposal_id: b, tie_break: false, at: Timestamp::now() });
        assert_eq!(set(&fx).phase, SetPhase::Scouting);
        { let p = fx.b; assess(&mut fx, 3, p, 75); }
        apply(&mut fx.sets, &mut fx.props, SetChange::QuorumReached { set_id: id, proposal_id: b, tie_break: false, at: Timestamp::now() });
        assert_eq!(set(&fx).phase, SetPhase::Piping);
        assert_eq!(set(&fx).front_runner, Some(b));
        assert!(set(&fx).is_frozen(c) && set(&fx).is_frozen(a) && !set(&fx).is_frozen(b));
        // A second quorum (another device, concurrently) loses: first wins.
        apply(&mut fx.sets, &mut fx.props, SetChange::QuorumReached { set_id: id, proposal_id: c, tie_break: false, at: Timestamp::now() });
        assert_eq!(set(&fx).front_runner, Some(b));
        // Frozen rivals take no new assessments; the front-runner still does.
        let before = set(&fx).assessments.len();
        { let p = fx.c; assess(&mut fx, 3, p, 100); }
        assert_eq!(set(&fx).assessments.len(), before);
        { let p = fx.b; assess(&mut fx, 2, p, 88); }
        assert_eq!(set(&fx).assessments.len(), before + 1);
        // Stop signals: only from the front-runner, never onto it.
        apply(&mut fx.sets, &mut fx.props, SetChange::Stopped { set_id: id, proposal_id: b, stopped_by: Some(c), reason: StopReason::Quorum });
        apply(&mut fx.sets, &mut fx.props, SetChange::Stopped { set_id: id, proposal_id: c, stopped_by: Some(a), reason: StopReason::Quorum });
        assert!(set(&fx).stops.is_empty());
        apply(&mut fx.sets, &mut fx.props, SetChange::Stopped { set_id: id, proposal_id: c, stopped_by: Some(b), reason: StopReason::Quorum });
        apply(&mut fx.sets, &mut fx.props, SetChange::Stopped { set_id: id, proposal_id: c, stopped_by: Some(b), reason: StopReason::Quorum });
        assert_eq!(set(&fx).stops.len(), 1, "idempotent");
        assert_eq!(set(&fx).scouts_of(c), vec!["scout2".to_string()]);
    }

    #[test]
    fn tie_past_deadline_picks_the_higher_supported_side_and_stops_the_other() {
        let mut fx = fixture();
        { let p = fx.b; assess(&mut fx, 1, p, 65); } // below the bar: no quorum anywhere
        { let p = fx.c; assess(&mut fx, 2, p, 69); }
        let (id, b, c) = (fx.set_id, fx.b, fx.c);
        let deadline = set(&fx).scouting_deadline.unwrap();
        let before = Timestamp::from_unix_seconds(deadline.inner().unix_timestamp() - 1);
        assert_eq!(set(&fx).deadline_leader(&fx.props, before), None, "not before the deadline");
        // Before the deadline, a tie-break event is ignored.
        apply(&mut fx.sets, &mut fx.props, SetChange::QuorumReached { set_id: id, proposal_id: c, tie_break: true, at: before });
        assert_eq!(set(&fx).phase, SetPhase::Scouting);
        // After it, only the leader can take it.
        assert_eq!(set(&fx).deadline_leader(&fx.props, deadline), Some(c));
        apply(&mut fx.sets, &mut fx.props, SetChange::QuorumReached { set_id: id, proposal_id: b, tie_break: true, at: deadline });
        assert_eq!(set(&fx).phase, SetPhase::Scouting);
        apply(&mut fx.sets, &mut fx.props, SetChange::QuorumReached { set_id: id, proposal_id: c, tie_break: true, at: deadline });
        assert_eq!(set(&fx).front_runner, Some(c));
        assert!(set(&fx).tie_break);
        apply(&mut fx.sets, &mut fx.props, SetChange::Stopped { set_id: id, proposal_id: b, stopped_by: Some(c), reason: StopReason::TieDeadline });
        assert!(set(&fx).stop_of(b).is_some());
    }

    #[test]
    fn exact_tie_is_broken_deterministically_by_proposal_order() {
        let mut fx = fixture();
        { let p = fx.c; assess(&mut fx, 1, p, 60); }
        { let p = fx.b; assess(&mut fx, 2, p, 60); }
        let deadline = set(&fx).scouting_deadline.unwrap();
        assert_eq!(set(&fx).deadline_leader(&fx.props, deadline), Some(fx.b), "earlier-proposed wins a dead heat");
        // No support at all: nothing to pick, stay scouting.
        let empty = fixture();
        assert_eq!(set(&empty).deadline_leader(&empty.props, deadline), None);
    }

    #[test]
    fn two_phase_commit_needs_human_quorum_and_a_passing_preflight() {
        let mut fx = fixture();
        { let p = fx.b; assess(&mut fx, 1, p, 80); }
        { let p = fx.b; assess(&mut fx, 2, p, 80); }
        let (id, b, c) = (fx.set_id, fx.b, fx.c);
        apply(&mut fx.sets, &mut fx.props, SetChange::QuorumReached { set_id: id, proposal_id: b, tie_break: false, at: Timestamp::now() });
        // Agent quorum alone never decides.
        apply(&mut fx.sets, &mut fx.props, SetChange::Decided { set_id: id, winner: Some(b), at: Timestamp::now() });
        assert_eq!(set(&fx).phase, SetPhase::Piping);
        // Human approval, but no pre-flight yet.
        vote(&mut fx, b, "human", WorkspaceRole::Contributor, true);
        apply(&mut fx.sets, &mut fx.props, SetChange::Decided { set_id: id, winner: Some(b), at: Timestamp::now() });
        assert_eq!(set(&fx).phase, SetPhase::Piping);
        // Pre-flight passes, then the decision lands.
        let report = passing();
        apply(&mut fx.sets, &mut fx.props, SetChange::Preflight { set_id: id, proposal_id: b, report: &report, at: Timestamp::now() });
        apply(&mut fx.sets, &mut fx.props, SetChange::Decided { set_id: id, winner: Some(b), at: Timestamp::now() });
        assert_eq!(set(&fx).phase, SetPhase::Decided);
        assert_eq!(set(&fx).winner, Some(b));
        // Decided is terminal.
        apply(&mut fx.sets, &mut fx.props, SetChange::Decided { set_id: id, winner: Some(c), at: Timestamp::now() });
        assert_eq!(set(&fx).winner, Some(b));
    }

    #[test]
    fn a_failed_preflight_returns_the_set_to_scouting() {
        let mut fx = fixture();
        { let p = fx.b; assess(&mut fx, 1, p, 80); }
        { let p = fx.b; assess(&mut fx, 2, p, 80); }
        { let p = fx.c; assess(&mut fx, 3, p, 90); }
        let (id, b, c) = (fx.set_id, fx.b, fx.c);
        apply(&mut fx.sets, &mut fx.props, SetChange::QuorumReached { set_id: id, proposal_id: b, tie_break: false, at: Timestamp::now() });
        apply(&mut fx.sets, &mut fx.props, SetChange::Stopped { set_id: id, proposal_id: c, stopped_by: Some(b), reason: StopReason::Quorum });
        vote(&mut fx, b, "human", WorkspaceRole::Contributor, true);
        let mut report = passing();
        report.base_unchanged = CheckOutcome::failed("the chat branch moved");
        apply(&mut fx.sets, &mut fx.props, SetChange::Preflight { set_id: id, proposal_id: b, report: &report, at: Timestamp::now() });
        let s = set(&fx);
        assert_eq!(s.phase, SetPhase::Scouting);
        assert_eq!(s.front_runner, None);
        assert_eq!((s.attempt, s.round), (1, 1));
        assert!(s.stop_of(c).is_none(), "this attempt's stops lift");
        assert!(!s.is_frozen(c));
        assert_eq!(s.standing(&fx.props, b).support, 110, "old support decayed one round");
        // The failed pre-flight doesn't count for the new attempt.
        apply(&mut fx.sets, &mut fx.props, SetChange::Decided { set_id: id, winner: Some(b), at: Timestamp::now() });
        assert_eq!(set(&fx).phase, SetPhase::Scouting);
        // An unknown check status fails closed.
        let odd = PreflightReport { tests: CheckOutcome { status: CheckStatus::Unknown, note: String::new() }, ..passing() };
        assert!(!odd.passed());
    }

    #[test]
    fn decision_respects_role_floor_and_no_self_approval() {
        let mut fx = fixture();
        let (id, b) = (fx.set_id, fx.b);
        // Floor is Contributor: a viewer's approval doesn't count.
        vote(&mut fx, b, "viewer", WorkspaceRole::Viewer, true);
        // B's author ("someone") approving it doesn't count either.
        vote(&mut fx, b, "someone", WorkspaceRole::Owner, true);
        let report = passing();
        apply(&mut fx.sets, &mut fx.props, SetChange::Preflight { set_id: id, proposal_id: b, report: &report, at: Timestamp::now() });
        apply(&mut fx.sets, &mut fx.props, SetChange::Decided { set_id: id, winner: Some(b), at: Timestamp::now() });
        assert_eq!(set(&fx).phase, SetPhase::Scouting, "no qualifying human approval yet");
        // A qualifying rejection blocks it outright.
        vote(&mut fx, b, "c2", WorkspaceRole::Contributor, false);
        assert!(!set(&fx).is_active(&fx.props, b));
        // Closing without a winner is always possible while open.
        apply(&mut fx.sets, &mut fx.props, SetChange::Decided { set_id: id, winner: None, at: Timestamp::now() });
        assert_eq!(set(&fx).phase, SetPhase::Decided);
        assert_eq!(set(&fx).winner, None);
    }

    #[test]
    fn tuning_the_dial_restamps_the_human_gate() {
        let mut fx = fixture();
        let (id, b) = (fx.set_id, fx.b);
        vote(&mut fx, b, "h1", WorkspaceRole::Contributor, true);
        assert!(fx.props.iter().find(|p| p.id == b).unwrap().ready_to_apply());
        let k = QuorumDial::Careful.knobs();
        apply(&mut fx.sets, &mut fx.props, SetChange::Tuned { set_id: id, agent_quorum: k.agent_quorum, required_approvals: k.required_approvals, quality_bar: k.quality_bar });
        let p = fx.props.iter().find(|p| p.id == b).unwrap();
        assert_eq!(p.required_approvals, 2);
        assert!(!p.ready_to_apply(), "careful needs a second approval");
        assert_eq!(set(&fx).agent_quorum, 3);
        apply(&mut fx.sets, &mut fx.props, SetChange::Tuned { set_id: id, agent_quorum: 0, required_approvals: 0, quality_bar: 500 });
        assert_eq!((set(&fx).agent_quorum, set(&fx).required_approvals, set(&fx).quality_bar), (1, 1, 100));
    }

    #[test]
    fn revisions_join_the_set_after_their_parent_and_withdrawals_are_permanent() {
        let mut fx = fixture();
        let (id, a, b) = (fx.set_id, fx.a, fx.b);
        let mut rev = alt("B v2", None);
        rev.parent_id = Some(b);
        let rid = rev.id;
        fx.props.push(rev);
        fx.props.iter_mut().find(|p| p.id == b).unwrap().superseded_by = Some(rid);
        apply(&mut fx.sets, &mut fx.props, SetChange::AlternativeAdded { set_id: id, proposal_id: rid, revises: Some(b) });
        assert_eq!(set(&fx).alternatives, vec![a, b, rid, fx.c]);
        assert!(!set(&fx).is_active(&fx.props, b), "superseded");
        assert!(set(&fx).is_active(&fx.props, rid));
        assert_eq!(fx.props.iter().find(|p| p.id == rid).unwrap().set_id, Some(id));
        apply(&mut fx.sets, &mut fx.props, SetChange::Stopped { set_id: id, proposal_id: a, stopped_by: None, reason: StopReason::Withdrawn });
        let s = &mut fx.sets[0];
        s.attempt += 1;
        assert!(s.stop_of(a).is_some(), "withdrawal survives a new attempt");
    }

    #[test]
    fn restamp_recovers_membership_after_an_old_peer_upsert() {
        let mut fx = fixture();
        let b = fx.b;
        let p = fx.props.iter_mut().find(|p| p.id == b).unwrap();
        p.set_id = None;
        p.required_approvals = 1;
        p.approval_role_floor = WorkspaceRole::Viewer;
        restamp(&fx.sets, &mut fx.props, b);
        let p = fx.props.iter().find(|p| p.id == b).unwrap();
        assert_eq!(p.set_id, Some(fx.set_id));
        assert_eq!(p.approval_role_floor, WorkspaceRole::Contributor);
    }

    #[test]
    fn options_wording() {
        assert!(wants_options("Explore options for the cache layer"));
        assert!(wants_options("give me two alternatives"));
        assert!(!wants_options("fix the failing test"));
    }
}

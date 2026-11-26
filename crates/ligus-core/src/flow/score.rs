//! Outcome scores ("dance scores"): per agent and task class, a decayed
//! average of outcomes Ligus measured itself — proposal approvals and
//! rejections, revise loops, workflow stages, failed turns and test runs —
//! plus turns spent per success. Never the agent's own claims: a self-report
//! may only become an input once it is calibrated against these outcomes,
//! which this build doesn't do, so none are read.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{agent_key, ms, task_class, AgentOutcomeSummary, TurnTag};
use crate::proposals::ProposalStatus;
use crate::session::ChatSession;
use crate::workflow::{NodeRunStatus, WorkflowNodeKind};

/// Where an outcome came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutcomeSource {
    /// A proposal was approved/applied (1.0) or rejected (0.0).
    Proposal,
    /// A proposal version was superseded by a revision: a revise loop.
    Revision,
    /// A workflow agent stage succeeded or failed.
    Stage,
    /// A turn ended in an error.
    TurnError,
    /// A test command the turn ran passed or failed.
    Tests,
}

/// One measured outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutcomeSample {
    pub agent: String,
    pub task_class: String,
    pub at_ms: i64,
    /// 0.0 (bad) ..= 1.0 (good).
    pub quality: f64,
    /// Counts as a success for turns-per-success.
    pub success: bool,
    pub source: OutcomeSource,
}

/// Quality of a revise loop: the version needed another pass.
pub const REVISION_QUALITY: f64 = 0.3;
/// Quality of a stage that succeeded without a gate judging it.
pub const UNJUDGED_STAGE_QUALITY: f64 = 0.7;

/// Read every measured outcome out of a set of chats (a workspace's chats).
pub fn outcomes(sessions: &[ChatSession]) -> Vec<OutcomeSample> {
    let mut out = Vec::new();
    for s in sessions {
        outcomes_of(s, &mut out);
    }
    out.sort_by_key(|o| o.at_ms);
    out
}

fn proposal_class(s: &ChatSession, p: &crate::proposals::ActionProposal) -> String {
    s.flow
        .turns
        .iter()
        .find(|t| t.proposal_ids.contains(&p.id))
        .map(|t| t.task_class.clone())
        .unwrap_or_else(|| {
            task_class::classify_proposal(&p.title, &p.body, &p.changed_files).to_string()
        })
}

fn outcomes_of(s: &ChatSession, out: &mut Vec<OutcomeSample>) {
    // Proposals produced by an agent turn. Workflow gates (no author) and
    // human-authored proposals measure no agent.
    let agent_names: BTreeMap<&str, uuid::Uuid> = s
        .workspace_agents
        .iter()
        .map(|a| (a.name.as_str(), a.id))
        .collect();
    for p in &s.proposals {
        // Producer: the recorded producing agent, else the turn that filed it
        // (an authorless diff from the primary), else an agent author by name.
        let producer = p
            .producing_agent_id
            .map(|id| agent_key(Some(id)))
            .or_else(|| {
                s.flow
                    .turns
                    .iter()
                    .find(|t| t.proposal_ids.contains(&p.id))
                    .map(|t| t.agent.clone())
            })
            .or_else(|| {
                agent_names
                    .get(p.author_actor_id.as_str())
                    .map(|id| agent_key(Some(*id)))
            });
        let Some(agent) = producer else { continue };
        let class = proposal_class(s, p);
        let decided_at = p
            .approvals
            .iter()
            .map(|a| ms(&a.created_at))
            .max()
            .unwrap_or_else(|| ms(&p.created_at));
        if p.superseded_by.is_some() {
            out.push(OutcomeSample {
                agent,
                task_class: class,
                at_ms: ms(&p.created_at),
                quality: REVISION_QUALITY,
                success: false,
                source: OutcomeSource::Revision,
            });
            continue;
        }
        let (quality, success) = match p.status {
            ProposalStatus::Approved | ProposalStatus::Applied => (1.0, true),
            ProposalStatus::Rejected => (0.0, false),
            ProposalStatus::Open => continue,
        };
        out.push(OutcomeSample {
            agent,
            task_class: class,
            at_ms: decided_at,
            quality,
            success,
            source: OutcomeSource::Proposal,
        });
    }

    // Workflow agent stages.
    for run in &s.workflow_runs {
        for node in &run.definition.nodes {
            let WorkflowNodeKind::Agent {
                agent_id,
                prompt_template,
            } = &node.kind
            else {
                continue;
            };
            let Some(state) = run.node_state(&node.id) else {
                continue;
            };
            let class =
                task_class::classify(&format!("{}\n{}", node.name, prompt_template)).to_string();
            let agent = agent_key(*agent_id);
            let at = ms(&run.updated_at);
            // Extra attempts (a rejecting gate sent the stage back) are revise loops.
            for _ in 1..state.attempts.max(1) {
                out.push(OutcomeSample {
                    agent: agent.clone(),
                    task_class: class.clone(),
                    at_ms: at,
                    quality: REVISION_QUALITY,
                    success: false,
                    source: OutcomeSource::Revision,
                });
            }
            let (quality, success) = match state.status {
                NodeRunStatus::Succeeded => (UNJUDGED_STAGE_QUALITY, true),
                NodeRunStatus::Failed => (0.0, false),
                _ => continue,
            };
            out.push(OutcomeSample {
                agent,
                task_class: class,
                at_ms: at,
                quality,
                success,
                source: OutcomeSource::Stage,
            });
        }
    }

    // Turn-level measurements.
    for t in &s.flow.turns {
        if t.error {
            out.push(OutcomeSample {
                agent: t.agent.clone(),
                task_class: t.task_class.clone(),
                at_ms: t.at_ms,
                quality: 0.0,
                success: false,
                source: OutcomeSource::TurnError,
            });
        }
        if let Some(passed) = t.tests_passed {
            out.push(OutcomeSample {
                agent: t.agent.clone(),
                task_class: t.task_class.clone(),
                at_ms: t.at_ms,
                quality: if passed { 1.0 } else { 0.0 },
                success: passed,
                source: OutcomeSource::Tests,
            });
        }
    }
}

/// Turns each agent spent per task class (the cost side of "cost per
/// successful turn"), counted from the turn tags.
pub fn turns_spent(sessions: &[ChatSession]) -> BTreeMap<(String, String), (f64, f64)> {
    let mut out: BTreeMap<(String, String), (f64, f64)> = BTreeMap::new();
    for s in sessions {
        for t in &s.flow.turns {
            let e = out
                .entry((t.agent.clone(), t.task_class.clone()))
                .or_default();
            e.0 += 1.0;
            e.1 += t.cost_usd;
        }
    }
    out
}

/// Turn-level outcomes from other hosts' daily summaries (P0.5: per-turn
/// tags are device-local facts, so a host sees its own turns as facts and
/// everyone else's as these summaries). `this_host`'s own summaries are
/// skipped: its facts already count.
pub fn summary_outcomes(
    summaries: &BTreeMap<String, Vec<AgentOutcomeSummary>>,
    this_host: &str,
) -> Vec<OutcomeSample> {
    let mut out = Vec::new();
    for sum in summaries
        .values()
        .flatten()
        .filter(|s| s.host_id != this_host)
    {
        // At most MAX_SUMMARY_COUNT samples per summary, whatever it claims.
        let mut budget = super::MAX_SUMMARY_COUNT;
        let mut push =
            |class: &String, n: u32, quality: f64, success: bool, source: OutcomeSource| {
                let n = n.min(budget);
                budget -= n;
                for _ in 0..n {
                    out.push(OutcomeSample {
                        agent: sum.agent_key.clone(),
                        task_class: class.clone(),
                        at_ms: sum.window_start,
                        quality,
                        success,
                        source,
                    });
                }
            };
        for (class, n) in &sum.errors_by_class {
            push(class, *n, 0.0, false, OutcomeSource::TurnError);
        }
        for (class, n) in &sum.tests_passed_by_class {
            push(class, *n, 1.0, true, OutcomeSource::Tests);
        }
        for (class, n) in &sum.tests_failed_by_class {
            push(class, *n, 0.0, false, OutcomeSource::Tests);
        }
    }
    out
}

/// Turns spent per (agent, class) from other hosts' summaries; see
/// [`summary_outcomes`].
pub fn summary_turns_spent(
    summaries: &BTreeMap<String, Vec<AgentOutcomeSummary>>,
    this_host: &str,
) -> BTreeMap<(String, String), (f64, f64)> {
    let mut out: BTreeMap<(String, String), (f64, f64)> = BTreeMap::new();
    for sum in summaries
        .values()
        .flatten()
        .filter(|s| s.host_id != this_host)
    {
        let total: u32 = sum.task_class_counts.values().sum();
        for (class, n) in &sum.task_class_counts {
            let e = out
                .entry((sum.agent_key.clone(), class.clone()))
                .or_default();
            e.0 += *n as f64;
            if total > 0 {
                e.1 += sum.cost_usd * (*n as f64) / total as f64;
            }
        }
    }
    out
}

/// Every outcome a host can score from: the chats (proposals, stages, this
/// host's turn facts) plus the other hosts' summaries.
pub fn outcomes_with_summaries(
    sessions: &[ChatSession],
    summaries: &BTreeMap<String, Vec<AgentOutcomeSummary>>,
    this_host: &str,
) -> Vec<OutcomeSample> {
    let mut out = outcomes(sessions);
    out.extend(summary_outcomes(summaries, this_host));
    out.sort_by_key(|o| o.at_ms);
    out
}

/// [`turns_spent`] plus the other hosts' summaries.
pub fn turns_spent_with_summaries(
    sessions: &[ChatSession],
    summaries: &BTreeMap<String, Vec<AgentOutcomeSummary>>,
    this_host: &str,
) -> BTreeMap<(String, String), (f64, f64)> {
    let mut out = turns_spent(sessions);
    for (k, (t, c)) in summary_turns_spent(summaries, this_host) {
        let e = out.entry(k).or_default();
        e.0 += t;
        e.1 += c;
    }
    out
}

/// A host's daily summary for `agent` built from its own turn tags of that
/// day (and the proposals those turns filed, by status).
pub fn summarize_day(
    agent: &str,
    host_id: &str,
    day_start: i64,
    tags: &[TurnTag],
    proposals: &[&crate::proposals::ActionProposal],
) -> AgentOutcomeSummary {
    let mut sum = AgentOutcomeSummary {
        agent_key: agent.into(),
        host_id: host_id.into(),
        window_start: day_start,
        ..Default::default()
    };
    for t in tags
        .iter()
        .filter(|t| t.agent == agent && super::utc_day(t.at_ms) == day_start)
    {
        sum.turns += 1;
        *sum.task_class_counts
            .entry(t.task_class.clone())
            .or_default() += 1;
        if t.error {
            *sum.errors_by_class.entry(t.task_class.clone()).or_default() += 1;
        }
        match t.tests_passed {
            Some(true) => {
                *sum.tests_passed_by_class
                    .entry(t.task_class.clone())
                    .or_default() += 1
            }
            Some(false) => {
                *sum.tests_failed_by_class
                    .entry(t.task_class.clone())
                    .or_default() += 1
            }
            None => {}
        }
        sum.cost_usd += t.cost_usd;
        for p in proposals.iter().filter(|p| t.proposal_ids.contains(&p.id)) {
            if p.reverted {
                sum.reverted += 1;
            } else {
                match p.status {
                    ProposalStatus::Approved | ProposalStatus::Applied => sum.accepted += 1,
                    ProposalStatus::Rejected => sum.rejected += 1,
                    ProposalStatus::Open => {}
                }
            }
        }
    }
    sum.bounded()
}

/// Scoring knobs.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreParams {
    /// Half-life of an outcome's weight.
    pub half_life_days: f64,
    /// Prior mean every agent starts from, so a newcomer isn't scored zero.
    pub prior_mean: f64,
    /// Weight of the prior, in outcomes.
    pub prior_weight: f64,
}

impl Default for ScoreParams {
    fn default() -> Self {
        Self {
            half_life_days: 14.0,
            prior_mean: 0.5,
            prior_weight: 2.0,
        }
    }
}

/// A dance score: how well an agent has done on a task class lately.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DanceScore {
    pub agent: String,
    pub task_class: String,
    /// Decayed mean quality, shrunk toward the prior (0..=1).
    pub score: f64,
    /// Same, with a short half-life (a quarter of the normal one) — the
    /// recent trend. Below `score` means the agent is getting worse.
    pub recent: f64,
    /// Effective number of outcomes behind it (sum of decayed weights).
    pub weight: f64,
    pub outcomes: u32,
    pub successes: u32,
    /// Turns spent per success (decayed); `None` until the first success.
    pub turns_per_success: Option<f64>,
    pub last_at_ms: i64,
}

fn decay(age_ms: i64, half_life_days: f64) -> f64 {
    if half_life_days <= 0.0 {
        return 1.0;
    }
    let age_days = (age_ms.max(0) as f64) / 86_400_000.0;
    0.5f64.powf(age_days / half_life_days)
}

/// Fold samples into scores per (agent, class). `turns` is from
/// [`turns_spent`] (optional — empty leaves `turns_per_success` unset).
pub fn dance_scores(
    samples: &[OutcomeSample],
    turns: &BTreeMap<(String, String), (f64, f64)>,
    params: ScoreParams,
    now_ms: i64,
) -> Vec<DanceScore> {
    #[derive(Default)]
    struct Acc {
        w: f64,
        wq: f64,
        rw: f64,
        rwq: f64,
        n: u32,
        succ: u32,
        wsucc: f64,
        last: i64,
    }
    let mut acc: BTreeMap<(String, String), Acc> = BTreeMap::new();
    for s in samples {
        let a = acc
            .entry((s.agent.clone(), s.task_class.clone()))
            .or_default();
        let w = decay(now_ms - s.at_ms, params.half_life_days);
        let rw = decay(now_ms - s.at_ms, params.half_life_days / 4.0);
        a.w += w;
        a.wq += w * s.quality;
        a.rw += rw;
        a.rwq += rw * s.quality;
        a.n += 1;
        if s.success {
            a.succ += 1;
            a.wsucc += w;
        }
        a.last = a.last.max(s.at_ms);
    }
    acc.into_iter()
        .map(|((agent, class), a)| {
            let shrink = |w: f64, wq: f64| {
                (wq + params.prior_mean * params.prior_weight) / (w + params.prior_weight)
            };
            let turns_per_success = turns
                .get(&(agent.clone(), class.clone()))
                .and_then(|(t, _)| (a.succ > 0).then(|| (t / a.succ as f64).max(1.0)));
            DanceScore {
                score: shrink(a.w, a.wq),
                recent: shrink(a.rw, a.rwq),
                weight: a.w,
                outcomes: a.n,
                successes: a.succ,
                turns_per_success,
                last_at_ms: a.last,
                agent,
                task_class: class,
            }
        })
        .collect()
}

/// Score lookup for routing: the (agent, class) score, falling back to the
/// agent's score across all classes, then to the prior.
#[derive(Debug, Clone, Default)]
pub struct ScoreBook {
    by_class: BTreeMap<(String, String), DanceScore>,
    by_agent: BTreeMap<String, DanceScore>,
    pub params: ScoreParams,
}

impl ScoreBook {
    pub fn new(
        samples: &[OutcomeSample],
        turns: &BTreeMap<(String, String), (f64, f64)>,
        params: ScoreParams,
        now_ms: i64,
    ) -> Self {
        let by_class = dance_scores(samples, turns, params, now_ms)
            .into_iter()
            .map(|d| ((d.agent.clone(), d.task_class.clone()), d))
            .collect();
        // All-class view: same samples, class erased.
        let any: Vec<OutcomeSample> = samples
            .iter()
            .map(|s| OutcomeSample {
                task_class: ANY.into(),
                ..s.clone()
            })
            .collect();
        let mut turns_any: BTreeMap<(String, String), (f64, f64)> = BTreeMap::new();
        for ((agent, _), (t, c)) in turns {
            let e = turns_any.entry((agent.clone(), ANY.into())).or_default();
            e.0 += t;
            e.1 += c;
        }
        let by_agent = dance_scores(&any, &turns_any, params, now_ms)
            .into_iter()
            .map(|d| (d.agent.clone(), d))
            .collect();
        Self {
            by_class,
            by_agent,
            params,
        }
    }

    /// The score routing uses for `agent` on `class`.
    pub fn get(&self, agent: &str, class: &str) -> Option<&DanceScore> {
        self.by_class
            .get(&(agent.to_string(), class.to_string()))
            .or_else(|| self.by_agent.get(agent))
    }

    /// Score value, or the prior when nothing is known.
    pub fn value(&self, agent: &str, class: &str) -> f64 {
        self.get(agent, class)
            .map(|d| d.score)
            .unwrap_or(self.params.prior_mean)
    }

    pub fn all(&self) -> impl Iterator<Item = &DanceScore> {
        self.by_class.values()
    }
}

/// The class key for an agent's all-class score.
pub const ANY: &str = "any";

/// Score samples from turn tags alone (for tests and callers without chats).
pub fn samples_from_turns(tags: &[TurnTag]) -> Vec<OutcomeSample> {
    let mut s = ChatSession::new("", uuid::Uuid::nil(), "");
    s.flow.turns = tags.to_vec();
    let mut out = Vec::new();
    outcomes_of(&s, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proposals::{ActionProposal, ProposalKind};
    use crate::WorkspaceAgent;

    const DAY: i64 = 86_400_000;

    fn sample(agent: &str, q: f64, at: i64) -> OutcomeSample {
        OutcomeSample {
            agent: agent.into(),
            task_class: "refactor".into(),
            at_ms: at,
            quality: q,
            success: q >= 0.5,
            source: OutcomeSource::Proposal,
        }
    }

    #[test]
    fn newcomer_sits_at_the_prior() {
        let book = ScoreBook::new(&[], &BTreeMap::new(), ScoreParams::default(), 0);
        assert_eq!(book.value("x", "refactor"), 0.5);
    }

    #[test]
    fn good_outcomes_raise_and_bad_lower_the_score() {
        let now = 100 * DAY;
        let good: Vec<_> = (0..6).map(|i| sample("a", 1.0, now - i * DAY)).collect();
        let bad: Vec<_> = (0..6).map(|i| sample("b", 0.0, now - i * DAY)).collect();
        let all = [good, bad].concat();
        let book = ScoreBook::new(&all, &BTreeMap::new(), ScoreParams::default(), now);
        assert!(book.value("a", "refactor") > 0.8);
        assert!(book.value("b", "refactor") < 0.2);
    }

    #[test]
    fn old_outcomes_fade() {
        let now = 200 * DAY;
        // Failures long ago, successes lately.
        let mut s: Vec<_> = (0..10)
            .map(|i| sample("a", 0.0, now - 150 * DAY - i * DAY))
            .collect();
        s.extend((0..3).map(|i| sample("a", 1.0, now - i * DAY)));
        let book = ScoreBook::new(&s, &BTreeMap::new(), ScoreParams::default(), now);
        assert!(
            book.value("a", "refactor") > 0.7,
            "{}",
            book.value("a", "refactor")
        );
    }

    #[test]
    fn recent_tracks_a_fall() {
        let now = 60 * DAY;
        let mut s: Vec<_> = (0..20)
            .map(|i| sample("a", 1.0, now - 20 * DAY - i * DAY))
            .collect();
        s.extend((0..4).map(|i| sample("a", 0.0, now - i * DAY)));
        let d = &dance_scores(&s, &BTreeMap::new(), ScoreParams::default(), now)[0];
        assert!(d.recent < d.score, "recent {} score {}", d.recent, d.score);
    }

    #[test]
    fn falls_back_to_the_agent_score_across_classes() {
        let now = 10 * DAY;
        let s: Vec<_> = (0..5).map(|i| sample("a", 1.0, now - i * DAY)).collect();
        let book = ScoreBook::new(&s, &BTreeMap::new(), ScoreParams::default(), now);
        assert!(book.value("a", "docs") > 0.7);
    }

    #[test]
    fn turns_per_success() {
        let now = DAY;
        let s = vec![sample("a", 1.0, now), sample("a", 1.0, now)];
        let mut turns = BTreeMap::new();
        turns.insert(("a".to_string(), "refactor".to_string()), (6.0, 0.0));
        let d = &dance_scores(&s, &turns, ScoreParams::default(), now)[0];
        assert_eq!(d.turns_per_success, Some(3.0));
    }

    #[test]
    fn reads_outcomes_from_proposals_revisions_stages_and_turns() {
        let mut s = ChatSession::new("t", uuid::Uuid::nil(), "rt");
        let agent = WorkspaceAgent::new("coder", "rt");
        s.workspace_agents.push(agent.clone());

        let mut approved = ActionProposal::file_diff(
            "Rename parser",
            "coder",
            "diff".into(),
            vec!["src/a.rs".into()],
        );
        approved.producing_agent_id = Some(agent.id);
        approved.status = ProposalStatus::Applied;
        let mut revised = ActionProposal::new("First try", ProposalKind::FileDiff, "coder");
        revised.superseded_by = Some(approved.id);
        let mut rejected = ActionProposal::new("Bad idea", ProposalKind::Decision, "coder");
        rejected.status = ProposalStatus::Rejected;
        let open = ActionProposal::new("Pending", ProposalKind::Decision, "coder");
        let mut gate = ActionProposal::new("Gate", ProposalKind::Decision, "");
        gate.status = ProposalStatus::Approved;
        // An authorless diff from the primary, known through its turn tag.
        let mut primary_diff =
            ActionProposal::file_diff("Docs", "", "d".into(), vec!["README.md".into()]);
        primary_diff.status = ProposalStatus::Applied;
        s.flow.turns.push(TurnTag {
            message_id: uuid::Uuid::new_v4(),
            trigger_id: None,
            agent: crate::flow::PRIMARY.into(),
            runtime_id: String::new(),
            host_id: String::new(),
            task_class: "docs".into(),
            role: "proposer".into(),
            arm: String::new(),
            upstream_arm: String::new(),
            error: false,
            error_kind: String::new(),
            tests_passed: None,
            tool_calls: 0,
            duration_ms: 0,
            cost_usd: 0.0,
            proposal_ids: vec![primary_diff.id],
            handoff_parcel: false,
            at_ms: 0,
        });
        s.proposals = vec![approved, revised, rejected, open, gate, primary_diff];

        let out = outcomes(&[s]);
        let key = agent_key(Some(agent.id));
        assert_eq!(out.len(), 4, "{out:?}");
        assert_eq!(out.iter().filter(|o| o.agent == key).count(), 3);
        assert!(out
            .iter()
            .any(|o| o.agent == crate::flow::PRIMARY && o.task_class == "docs" && o.success));
        assert!(out.iter().any(|o| o.source == OutcomeSource::Revision));
        assert!(out.iter().any(|o| o.source == OutcomeSource::Proposal
            && o.quality == 1.0
            && o.task_class == "refactor"));
        assert!(out
            .iter()
            .any(|o| o.source == OutcomeSource::Proposal && o.quality == 0.0));
    }

    #[test]
    fn turn_errors_and_tests_are_outcomes() {
        let tag = |error: bool, tests: Option<bool>| TurnTag {
            message_id: uuid::Uuid::new_v4(),
            trigger_id: None,
            agent: "a".into(),
            runtime_id: String::new(),
            host_id: String::new(),
            task_class: "bugfix".into(),
            role: "conversation".into(),
            arm: String::new(),
            upstream_arm: String::new(),
            error,
            error_kind: String::new(),
            tests_passed: tests,
            tool_calls: 0,
            duration_ms: 0,
            cost_usd: 0.0,
            proposal_ids: vec![],
            handoff_parcel: false,
            at_ms: 1,
        };
        let out = samples_from_turns(&[tag(true, None), tag(false, Some(true)), tag(false, None)]);
        assert_eq!(out.len(), 2);
        assert!(out
            .iter()
            .any(|o| o.source == OutcomeSource::TurnError && o.quality == 0.0));
        assert!(out
            .iter()
            .any(|o| o.source == OutcomeSource::Tests && o.success));
    }

    /// P0.5: a host that only sees another host's daily summaries scores
    /// that host's turns exactly as one that saw the legacy per-turn tags
    /// (fixture turns at the start of their day).
    #[test]
    fn scores_equal_from_summaries_and_from_legacy_tagged() {
        let day = super::super::utc_day(1_760_000_000_000);
        let tag = |agent: &str, class: &str, error: bool, tests: Option<bool>, cost: f64| TurnTag {
            message_id: uuid::Uuid::new_v4(),
            trigger_id: None,
            agent: agent.into(),
            runtime_id: "rt".into(),
            host_id: "worker-1".into(),
            task_class: class.into(),
            role: String::new(),
            arm: String::new(),
            upstream_arm: String::new(),
            error,
            error_kind: String::new(),
            tests_passed: tests,
            tool_calls: 0,
            duration_ms: 0,
            cost_usd: cost,
            proposal_ids: vec![],
            handoff_parcel: false,
            at_ms: day,
        };
        let tags = vec![
            tag("a", "code", false, Some(true), 0.5),
            tag("a", "code", true, None, 0.25),
            tag("a", "docs", false, Some(false), 0.0),
            tag("b", "code", false, None, 1.0),
        ];
        // Legacy: the tags folded into a chat on this host.
        let mut legacy = ChatSession::new("", uuid::Uuid::nil(), "");
        legacy.flow.turns = tags.clone();
        let now = day + 3 * 86_400_000;
        let legacy_book = ScoreBook::new(
            &outcomes(&[legacy.clone()]),
            &turns_spent(&[legacy]),
            ScoreParams::default(),
            now,
        );
        // Summaries: what worker-1 syncs, seen from another host.
        let mut summaries: BTreeMap<String, Vec<AgentOutcomeSummary>> = BTreeMap::new();
        for agent in ["a", "b"] {
            summaries
                .entry(agent.into())
                .or_default()
                .push(summarize_day(agent, "worker-1", day, &tags, &[]));
        }
        let empty = ChatSession::new("", uuid::Uuid::nil(), "");
        let sum_book = ScoreBook::new(
            &outcomes_with_summaries(std::slice::from_ref(&empty), &summaries, "desktop"),
            &turns_spent_with_summaries(&[empty], &summaries, "desktop"),
            ScoreParams::default(),
            now,
        );
        let legacy_all: Vec<_> = legacy_book.all().cloned().collect();
        let sum_all: Vec<_> = sum_book.all().cloned().collect();
        assert_eq!(legacy_all.len(), sum_all.len());
        for (l, s) in legacy_all.iter().zip(&sum_all) {
            assert_eq!(
                (&l.agent, &l.task_class, l.outcomes, l.successes),
                (&s.agent, &s.task_class, s.outcomes, s.successes)
            );
            assert!(
                (l.score - s.score).abs() < 1e-12 && (l.recent - s.recent).abs() < 1e-12,
                "{l:?} vs {s:?}"
            );
            assert_eq!(l.turns_per_success, s.turns_per_success);
        }
        // Its own summaries don't double-count on the host that wrote them.
        assert!(summary_outcomes(&summaries, "worker-1").is_empty());
    }
}

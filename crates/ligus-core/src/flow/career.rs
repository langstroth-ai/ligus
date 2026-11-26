//! Careers: a reversible default path for an agent. A new agent starts on
//! low-risk duties (review comments, docs, tests), **graduates** to the full
//! range — diffs, refactors, features — once its approval record is good
//! enough, and is **pulled back to review** for a while when the workspace is
//! short of reviewers. A person can pin any agent to a stage; the pin wins.
//! This is also the on-ramp for a newly added local model.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::score::{OutcomeSample, OutcomeSource};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Career {
    /// Low-risk work only until it has a track record.
    Probation,
    /// Full range.
    Proposer,
    /// Temporarily on review duty (the workspace is short of reviewers).
    Reviewer,
}

impl Career {
    pub fn as_str(self) -> &'static str {
        match self {
            Career::Probation => "probation",
            Career::Proposer => "proposer",
            Career::Reviewer => "reviewer",
        }
    }
}

/// Per-agent flow settings a person sets (config log).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentFlowProfile {
    pub agent_id: Uuid,
    /// Pin the career stage (`None` = automatic).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub career: Option<Career>,
    /// Persona bias on task-board thresholds: negative = eager, positive =
    /// reluctant. Added to every threshold (in stimulus units).
    #[serde(default)]
    pub threshold_bias: f64,
    /// Leave the agent off the task board entirely.
    #[serde(default)]
    pub board_opt_out: bool,
    #[serde(default)]
    pub at_ms: i64,
}

/// Graduation rule.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CareerPolicy {
    /// Judged proposals needed before graduating.
    pub min_proposals: u32,
    /// Approval rate needed over those.
    pub min_approval_rate: f64,
    /// Open proposals waiting per available reviewer above which the
    /// strongest proposers are pulled back to review.
    pub review_pressure: f64,
}

impl Default for CareerPolicy {
    fn default() -> Self {
        Self { min_proposals: 5, min_approval_rate: 0.6, review_pressure: 4.0 }
    }
}

/// An agent's measured track record: judged proposals and approvals
/// (revisions count as judged and not approved).
pub fn track_record(samples: &[OutcomeSample], agent: &str) -> (u32, u32) {
    let mut judged = 0;
    let mut approved = 0;
    for s in samples.iter().filter(|s| s.agent == agent) {
        match s.source {
            OutcomeSource::Proposal => {
                judged += 1;
                if s.success {
                    approved += 1;
                }
            }
            OutcomeSource::Revision => judged += 1,
            _ => {}
        }
    }
    (judged, approved)
}

/// Whether an agent has graduated from probation.
pub fn graduated(samples: &[OutcomeSample], agent: &str, policy: &CareerPolicy) -> bool {
    let (judged, approved) = track_record(samples, agent);
    judged >= policy.min_proposals && (approved as f64) >= policy.min_approval_rate * judged as f64
}

/// One agent's career inputs.
#[derive(Debug, Clone)]
pub struct CareerInput<'a> {
    pub agent: String,
    pub profile: Option<&'a AgentFlowProfile>,
    /// The agent existed before careers (or is the primary): starts graduated.
    pub grandfathered: bool,
}

/// Career for every agent. `review_queue` is the number of open proposals
/// waiting for a vote; `reviewers` the number of agents already on review
/// duty or probation (who take review work). When the queue per reviewer is
/// above the policy's pressure, the best graduated proposer (by approvals)
/// is pulled back to review until the pressure eases — automatically
/// reversible, since it is recomputed each time.
pub fn careers(
    inputs: &[CareerInput<'_>],
    samples: &[OutcomeSample],
    review_queue: usize,
    policy: &CareerPolicy,
) -> Vec<(String, Career, &'static str)> {
    let mut out: Vec<(String, Career, &'static str)> = inputs
        .iter()
        .map(|i| match i.profile.and_then(|p| p.career) {
            Some(c) => (i.agent.clone(), c, "pinned"),
            None if i.grandfathered || graduated(samples, &i.agent, policy) => (i.agent.clone(), Career::Proposer, "track record"),
            None => (i.agent.clone(), Career::Probation, "new"),
        })
        .collect();
    let reviewers = out.iter().filter(|(_, c, _)| *c != Career::Proposer).count();
    let pressure = review_queue as f64 / (reviewers.max(1) as f64);
    let proposers: Vec<usize> = out
        .iter()
        .enumerate()
        .filter(|(_, (_, c, why))| *c == Career::Proposer && *why != "pinned")
        .map(|(i, _)| i)
        .collect();
    // Keep at least one proposer; pull back one when under review pressure.
    if pressure > policy.review_pressure && proposers.len() > 1 {
        let best = proposers
            .iter()
            .copied()
            .max_by_key(|&i| (track_record(samples, &out[i].0).1, std::cmp::Reverse(out[i].0.clone())))
            .expect("non-empty");
        out[best].1 = Career::Reviewer;
        out[best].2 = "review queue is long";
    }
    out
}

/// Roster diversity: how many distinct runtimes / providers back the agents.
/// Identical agents react to the same stimulus at the same moment; a mixed
/// roster is steadier. A monoculture is two or more agents on one runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diversity {
    pub agents: usize,
    pub runtimes: usize,
    pub providers: usize,
    pub monoculture: bool,
    pub advice: String,
}

/// `pairs` are (runtime id, provider name) per agent.
pub fn diversity(pairs: &[(String, String)]) -> Diversity {
    let mut runtimes: Vec<&str> = pairs.iter().map(|(r, _)| r.as_str()).collect();
    runtimes.sort_unstable();
    runtimes.dedup();
    let mut providers: Vec<&str> = pairs.iter().map(|(_, p)| p.as_str()).collect();
    providers.sort_unstable();
    providers.dedup();
    let monoculture = pairs.len() >= 2 && runtimes.len() == 1;
    let advice = if pairs.len() < 2 {
        "Add a second agent on a different runtime to get a review partner.".to_string()
    } else if monoculture {
        "Every agent runs on the same runtime. Mix in a different model or runtime (a local model, a subscription CLI) so the roster doesn't fail or agree all at once.".to_string()
    } else if providers.len() == 1 {
        "Agents differ in model but share one provider; a second provider keeps work moving through an outage.".to_string()
    } else {
        "The roster mixes runtimes and providers.".to_string()
    };
    Diversity { agents: pairs.len(), runtimes: runtimes.len(), providers: providers.len(), monoculture, advice }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(agent: &str, source: OutcomeSource, success: bool) -> OutcomeSample {
        OutcomeSample { agent: agent.into(), task_class: "x".into(), at_ms: 0, quality: if success { 1.0 } else { 0.0 }, success, source }
    }

    #[test]
    fn graduates_after_a_good_record() {
        let p = CareerPolicy::default();
        let mut samples: Vec<_> = (0..4).map(|_| s("a", OutcomeSource::Proposal, true)).collect();
        assert!(!graduated(&samples, "a", &p));
        samples.push(s("a", OutcomeSource::Revision, false));
        assert!(graduated(&samples, "a", &p), "4 of 5 approved");
        samples.extend((0..5).map(|_| s("a", OutcomeSource::Proposal, false)));
        assert!(!graduated(&samples, "a", &p), "4 of 10 is too few");
    }

    #[test]
    fn pins_win_and_new_agents_start_on_probation() {
        let prof = AgentFlowProfile { agent_id: Uuid::nil(), career: Some(Career::Proposer), threshold_bias: 0.0, board_opt_out: false, at_ms: 0 };
        let inputs = [
            CareerInput { agent: "new".into(), profile: None, grandfathered: false },
            CareerInput { agent: "pinned".into(), profile: Some(&prof), grandfathered: false },
            CareerInput { agent: "old".into(), profile: None, grandfathered: true },
        ];
        let c = careers(&inputs, &[], 0, &CareerPolicy::default());
        assert_eq!(c[0].1, Career::Probation);
        assert_eq!(c[1].1, Career::Proposer);
        assert_eq!(c[1].2, "pinned");
        assert_eq!(c[2].1, Career::Proposer);
    }

    #[test]
    fn review_pressure_pulls_back_the_best_proposer_and_releases_it() {
        let inputs = [
            CareerInput { agent: "a".into(), profile: None, grandfathered: true },
            CareerInput { agent: "b".into(), profile: None, grandfathered: true },
        ];
        let samples = vec![s("b", OutcomeSource::Proposal, true)];
        let busy = careers(&inputs, &samples, 10, &CareerPolicy::default());
        assert_eq!(busy.iter().find(|x| x.0 == "b").unwrap().1, Career::Reviewer);
        assert_eq!(busy.iter().find(|x| x.0 == "a").unwrap().1, Career::Proposer);
        let calm = careers(&inputs, &samples, 1, &CareerPolicy::default());
        assert!(calm.iter().all(|x| x.1 == Career::Proposer));
        // A lone proposer is never pulled back.
        let solo = careers(&inputs[..1], &samples, 50, &CareerPolicy::default());
        assert_eq!(solo[0].1, Career::Proposer);
    }

    #[test]
    fn diversity_flags_a_monoculture() {
        let mono = diversity(&[("cc".into(), "claudeCode".into()), ("cc".into(), "claudeCode".into())]);
        assert!(mono.monoculture);
        let mixed = diversity(&[("cc".into(), "claudeCode".into()), ("llama".into(), "openAI".into())]);
        assert!(!mixed.monoculture);
        assert_eq!(mixed.providers, 2);
        assert!(!diversity(&[("cc".into(), "x".into())]).monoculture);
    }
}

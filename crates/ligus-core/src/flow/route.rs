//! Outcome-weighted routing for un-addressed work.
//!
//! When nobody names an agent, the router picks one from the eligible
//! candidates **by sampling in proportion to its outcome score** for the
//! task class (never argmax, so a weaker agent still gets some traffic and can
//! recover), after three overrides, in order:
//!
//! 1. **Explicit choice** — an `@mention` never reaches the router.
//! 2. **Private memory** — the agent already carrying this thread (the last
//!    live routed pick in the chat, if it didn't fail) or the agent with the
//!    most recent approved change to a file the message names keeps the work.
//! 3. **Route stops** — a route in cool-down after a severe failure is out of
//!    rotation until its stop expires.
//!
//! An adaptive share ε of decisions deliberately goes to an agent other than
//! the incumbent (the best-scoring one): more when the incumbent is weak,
//! uncertain or falling, less when it is strong and steady, within the range
//! colonies are reported to keep as scouts (about 5–30 %).
//!
//! The draw is seeded by the trigger message id, so every device that
//! evaluates the same message with the same log makes the same pick.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::score::ScoreBook;
use super::task_class;

/// Whether the router acts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RouteMode {
    /// No routing: un-addressed messages go to the primary, nothing logged.
    Off,
    /// Log what the router would pick; the primary still answers.
    #[default]
    Shadow,
    /// The router's pick answers.
    Live,
}

impl RouteMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RouteMode::Off => "off",
            RouteMode::Shadow => "shadow",
            RouteMode::Live => "live",
        }
    }
}

/// Why the router chose what it chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RouteReason {
    /// Sampled in proportion to scores.
    Sampled,
    /// The exploration share: deliberately off the incumbent.
    Explore,
    /// The agent already carrying this thread kept it.
    ThreadMemory,
    /// The agent with recent approved work on a named file kept it.
    FileMemory,
    /// Only one eligible candidate.
    Only,
    /// Every candidate was stopped or ineligible; fell back to the primary.
    Fallback,
}

/// One routing decision. Recorded for every un-addressed message while
/// routing is on, in shadow mode too.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteDecision {
    /// The message being routed.
    pub trigger_id: Uuid,
    pub task_class: String,
    /// Agent key that answers (in shadow mode, the primary).
    pub chosen: String,
    /// The incumbent: highest score at decision time.
    pub preferred: String,
    /// The router deliberately chose off the incumbent (the ε share).
    pub explore: bool,
    pub mode: RouteMode,
    /// What the router picked (in live mode equals `chosen`).
    pub shadow_choice: String,
    pub reason: RouteReason,
    /// Exploration share in force.
    pub epsilon: f64,
    /// Candidate scores at decision time (agent key → score).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scores: BTreeMap<String, f64>,
    /// Candidates left out by a route stop or the career gate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<String>,
    pub at_ms: i64,
}

impl RouteDecision {
    #[cfg(test)]
    pub(crate) fn shadow_example(trigger: Uuid) -> Self {
        RouteDecision {
            trigger_id: trigger,
            task_class: "general".into(),
            chosen: super::PRIMARY.into(),
            preferred: super::PRIMARY.into(),
            explore: false,
            mode: RouteMode::Shadow,
            shadow_choice: super::PRIMARY.into(),
            reason: RouteReason::Only,
            epsilon: 0.0,
            scores: BTreeMap::new(),
            excluded: vec![],
            at_ms: 0,
        }
    }
}

/// Classes of failure that pull a route out of rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FailureKind {
    /// 429 / quota / "rate limit".
    RateLimited,
    /// A subscription or plan window at its limit.
    WindowExhausted,
    /// Provider 5xx / overloaded.
    ProviderDown,
    /// The model refused.
    Refusal,
    /// Couldn't reach the endpoint (local server down, connection refused).
    Unreachable,
    /// Timed out.
    Timeout,
    /// Anything else.
    Other,
}

impl FailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FailureKind::RateLimited => "rate_limited",
            FailureKind::WindowExhausted => "window_exhausted",
            FailureKind::ProviderDown => "provider_down",
            FailureKind::Refusal => "refusal",
            FailureKind::Unreachable => "unreachable",
            FailureKind::Timeout => "timeout",
            FailureKind::Other => "other",
        }
    }

    /// Severe failures stop the route on the first occurrence; the rest only
    /// after a repeat (see [`stop_after_failure`]).
    pub fn is_severe(self) -> bool {
        matches!(
            self,
            FailureKind::RateLimited | FailureKind::WindowExhausted | FailureKind::ProviderDown | FailureKind::Unreachable
        )
    }

    /// Failures of the provider rather than the agent: the stop covers every
    /// agent on the same runtime.
    pub fn is_runtime_wide(self) -> bool {
        matches!(
            self,
            FailureKind::RateLimited | FailureKind::WindowExhausted | FailureKind::ProviderDown | FailureKind::Unreachable
        )
    }
}

/// Classify a turn error message.
pub fn classify_failure(error: &str) -> FailureKind {
    let e = error.to_lowercase();
    let any = |words: &[&str]| words.iter().any(|w| e.contains(w));
    if any(&["usage limit", "window", "limit reached", "quota exceeded", "out of credits", "credit balance"]) {
        FailureKind::WindowExhausted
    } else if any(&["429", "rate limit", "rate-limit", "too many requests", "ratelimit"]) {
        FailureKind::RateLimited
    } else if any(&["500", "502", "503", "504", "529", "overloaded", "internal server error", "bad gateway", "service unavailable"]) {
        FailureKind::ProviderDown
    } else if any(&["connection refused", "couldn't connect", "could not connect", "unreachable", "dns", "no route to host", "error sending request"]) {
        FailureKind::Unreachable
    } else if any(&["timed out", "timeout", "deadline"]) {
        FailureKind::Timeout
    } else if any(&["i can't help", "i cannot help", "i can’t help", "refuse", "content policy", "safety"]) {
        FailureKind::Refusal
    } else {
        FailureKind::Other
    }
}

/// A route out of rotation until `until_ms`. `route` is an agent key, or
/// `runtime:<id>` for a provider-wide stop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteStop {
    pub route: String,
    pub reason: FailureKind,
    pub until_ms: i64,
    /// Failures in the window that triggered it.
    #[serde(default)]
    pub failures: u32,
    /// Host the failure happened on, if any.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host_id: String,
    pub at_ms: i64,
}

impl RouteStop {
    pub fn active(&self, now_ms: i64) -> bool {
        now_ms < self.until_ms
    }
}

/// The route key for a runtime-wide stop.
pub fn runtime_route(runtime_id: &str) -> String {
    format!("runtime:{runtime_id}")
}

/// Whether a failure should stop a route, and for how long. A severe failure
/// stops it at once; any other failure stops it when it is the second within
/// `window_ms` (the previous failure time is `last_failure_ms`).
pub fn stop_after_failure(
    route: &str,
    kind: FailureKind,
    now_ms: i64,
    last_failure_ms: Option<i64>,
    cooldown_ms: i64,
    window_ms: i64,
) -> Option<RouteStop> {
    let repeat = last_failure_ms.is_some_and(|t| now_ms - t <= window_ms);
    if !(kind.is_severe() || repeat) || cooldown_ms <= 0 {
        return None;
    }
    Some(RouteStop {
        route: route.to_string(),
        reason: kind,
        until_ms: now_ms + cooldown_ms,
        failures: if repeat { 2 } else { 1 },
        host_id: String::new(),
        at_ms: now_ms,
    })
}

/// A candidate the router may pick.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Agent key ([`super::agent_key`]).
    pub agent: String,
    pub runtime_id: String,
    /// Low-risk classes only (a new agent on probation, see
    /// [`super::career`]).
    pub low_risk_only: bool,
}

/// Exploration share bounds and policy knobs.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutePolicy {
    pub epsilon_min: f64,
    pub epsilon_max: f64,
    /// Outcomes below which the incumbent counts as unproven.
    pub min_weight: f64,
    /// Sharpening exponent: weight = score^k (1 = proportional).
    pub sharpness: f64,
}

impl Default for RoutePolicy {
    fn default() -> Self {
        Self { epsilon_min: 0.05, epsilon_max: 0.30, min_weight: 3.0, sharpness: 1.0 }
    }
}

/// The exploration share for an incumbent with `score`, recent trend
/// `recent` and evidence `weight`: high when the best known option is poor,
/// falling or unproven; low when it is good and steady.
pub fn adaptive_epsilon(score: f64, recent: f64, weight: f64, policy: &RoutePolicy) -> f64 {
    let poor = (1.0 - score).clamp(0.0, 1.0);
    let falling = (score - recent).max(0.0) * 2.0;
    let unproven = if weight < policy.min_weight { 0.5 } else { 0.0 };
    let need = (poor + falling).max(unproven).clamp(0.0, 1.0);
    let lo = policy.epsilon_min.clamp(0.0, 1.0);
    let hi = policy.epsilon_max.clamp(lo, 1.0);
    lo + (hi - lo) * need
}

/// Deterministic uniform draws seeded by a message id (splitmix64).
pub struct Draw(u64);

impl Draw {
    pub fn seeded(id: Uuid) -> Self {
        let (hi, lo) = id.as_u64_pair();
        Draw(hi ^ lo.rotate_left(17) ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub fn next_f64(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Everything one decision reads.
pub struct RouteInput<'a> {
    pub trigger_id: Uuid,
    pub text: &'a str,
    pub candidates: &'a [Candidate],
    pub scores: &'a ScoreBook,
    pub stops: &'a BTreeMap<String, RouteStop>,
    /// The agent carrying this thread, if one should keep it.
    pub thread_memory: Option<&'a str>,
    /// The agent with recent approved work on a file the message names.
    pub file_memory: Option<&'a str>,
    pub mode: RouteMode,
    pub policy: RoutePolicy,
    pub now_ms: i64,
}

/// Decide who takes an un-addressed message. Returns `None` when routing is
/// off or there is nothing to choose between (no candidates).
pub fn decide(input: &RouteInput<'_>) -> Option<RouteDecision> {
    if input.mode == RouteMode::Off || input.candidates.is_empty() {
        return None;
    }
    let class = task_class::classify(input.text).to_string();
    let stopped = |c: &Candidate| {
        [c.agent.clone(), runtime_route(&c.runtime_id)]
            .iter()
            .any(|k| input.stops.get(k).is_some_and(|s| s.active(input.now_ms)))
    };
    let mut excluded = Vec::new();
    let mut eligible: Vec<&Candidate> = Vec::new();
    for c in input.candidates {
        if stopped(c) || (c.low_risk_only && !task_class::is_low_risk(&class)) {
            excluded.push(c.agent.clone());
        } else {
            eligible.push(c);
        }
    }
    let scores: BTreeMap<String, f64> =
        input.candidates.iter().map(|c| (c.agent.clone(), input.scores.value(&c.agent, &class))).collect();
    let primary = super::PRIMARY.to_string();
    let incumbent = eligible
        .iter()
        .max_by(|a, b| scores[&a.agent].total_cmp(&scores[&b.agent]).then_with(|| b.agent.cmp(&a.agent)))
        .map(|c| c.agent.clone());
    let (pick, reason, explore, epsilon) = match &incumbent {
        None => (primary.clone(), RouteReason::Fallback, false, 0.0),
        Some(_) if eligible.len() == 1 => (eligible[0].agent.clone(), RouteReason::Only, false, 0.0),
        Some(best) => {
            let in_play = |a: &str| eligible.iter().any(|c| c.agent == a);
            if let Some(a) = input.thread_memory.filter(|a| in_play(a)) {
                (a.to_string(), RouteReason::ThreadMemory, false, 0.0)
            } else if let Some(a) = input.file_memory.filter(|a| in_play(a)) {
                (a.to_string(), RouteReason::FileMemory, false, 0.0)
            } else {
                let d = input.scores.get(best, &class);
                let (s, r, w) = d.map(|d| (d.score, d.recent, d.weight)).unwrap_or((input.scores.params.prior_mean, input.scores.params.prior_mean, 0.0));
                let eps = adaptive_epsilon(s, r, w, &input.policy);
                let mut draw = Draw::seeded(input.trigger_id);
                if draw.next_f64() < eps {
                    let others: Vec<&&Candidate> = eligible.iter().filter(|c| &c.agent != best).collect();
                    let i = ((draw.next_f64() * others.len() as f64) as usize).min(others.len() - 1);
                    (others[i].agent.clone(), RouteReason::Explore, true, eps)
                } else {
                    let k = input.policy.sharpness.max(0.1);
                    let weights: Vec<f64> = eligible.iter().map(|c| scores[&c.agent].max(0.01).powf(k)).collect();
                    let total: f64 = weights.iter().sum();
                    let mut x = draw.next_f64() * total;
                    let mut chosen = eligible[eligible.len() - 1].agent.clone();
                    for (c, w) in eligible.iter().zip(&weights) {
                        if x < *w {
                            chosen = c.agent.clone();
                            break;
                        }
                        x -= w;
                    }
                    (chosen, RouteReason::Sampled, false, eps)
                }
            }
        }
    };
    let chosen = match input.mode {
        RouteMode::Live => pick.clone(),
        _ => primary.clone(),
    };
    Some(RouteDecision {
        trigger_id: input.trigger_id,
        task_class: class,
        chosen,
        preferred: incumbent.unwrap_or(primary),
        explore,
        mode: input.mode,
        shadow_choice: pick,
        reason,
        epsilon,
        scores,
        excluded,
        at_ms: input.now_ms,
    })
}

/// File paths a message names (tokens with a `/` or a file extension), for
/// file memory.
pub fn named_paths(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || matches!(c, '`' | '(' | ')' | ',' | '"' | '\'' | '<' | '>'))
        .map(|t| t.trim_matches(|c: char| matches!(c, '.' | ':' | ';' | '!' | '?')))
        .filter(|t| !t.is_empty() && !t.starts_with("http") && !t.starts_with('@'))
        .filter(|t| {
            let ext = t.rsplit_once('.').map(|(stem, e)| {
                !stem.is_empty()
                    && (1..=5).contains(&e.len())
                    && e.chars().all(|c| c.is_ascii_alphanumeric())
                    && e.chars().any(|c| c.is_ascii_alphabetic())
            });
            t.contains('/') || ext == Some(true)
        })
        .filter(|t| t.chars().any(|c| c.is_ascii_alphabetic()))
        .map(|t| t.trim_start_matches("./").to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::score::{OutcomeSample, OutcomeSource, ScoreParams};

    const DAY: i64 = 86_400_000;

    fn cand(a: &str) -> Candidate {
        Candidate { agent: a.into(), runtime_id: format!("rt-{a}"), low_risk_only: false }
    }

    fn book(entries: &[(&str, f64, usize)], now: i64) -> ScoreBook {
        let mut s = Vec::new();
        for (agent, q, n) in entries {
            for i in 0..*n {
                s.push(OutcomeSample {
                    agent: agent.to_string(),
                    task_class: "refactor".into(),
                    at_ms: now - i as i64 * DAY / 4,
                    quality: *q,
                    success: *q >= 0.5,
                    source: OutcomeSource::Proposal,
                });
            }
        }
        ScoreBook::new(&s, &BTreeMap::new(), ScoreParams::default(), now)
    }

    fn input<'a>(
        t: Uuid,
        c: &'a [Candidate],
        b: &'a ScoreBook,
        stops: &'a BTreeMap<String, RouteStop>,
        mode: RouteMode,
    ) -> RouteInput<'a> {
        RouteInput {
            trigger_id: t,
            text: "refactor the parser",
            candidates: c,
            scores: b,
            stops,
            thread_memory: None,
            file_memory: None,
            mode,
            policy: RoutePolicy::default(),
            now_ms: 10 * DAY,
        }
    }

    #[test]
    fn off_mode_decides_nothing() {
        let c = [cand("a"), cand("b")];
        let b = book(&[], 0);
        let stops = BTreeMap::new();
        assert!(decide(&input(Uuid::new_v4(), &c, &b, &stops, RouteMode::Off)).is_none());
    }

    #[test]
    fn shadow_logs_the_pick_but_the_primary_answers() {
        let c = [cand("a"), cand("b")];
        let b = book(&[("a", 1.0, 10)], 10 * DAY);
        let stops = BTreeMap::new();
        let d = decide(&input(Uuid::new_v4(), &c, &b, &stops, RouteMode::Shadow)).unwrap();
        assert_eq!(d.chosen, crate::flow::PRIMARY);
        assert_eq!(d.preferred, "a");
        assert_eq!(d.task_class, "refactor");
        assert!(d.shadow_choice == "a" || d.shadow_choice == "b");
    }

    #[test]
    fn same_trigger_same_pick() {
        let c = [cand("a"), cand("b"), cand("c")];
        let b = book(&[("a", 0.9, 5), ("b", 0.5, 5)], 10 * DAY);
        let stops = BTreeMap::new();
        let t = Uuid::new_v4();
        let x = decide(&input(t, &c, &b, &stops, RouteMode::Live)).unwrap();
        let y = decide(&input(t, &c, &b, &stops, RouteMode::Live)).unwrap();
        assert_eq!(x, y);
    }

    #[test]
    fn sampling_is_proportional_not_argmax() {
        let c = [cand("a"), cand("b")];
        let b = book(&[("a", 1.0, 30), ("b", 0.0, 30)], 10 * DAY);
        let stops = BTreeMap::new();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for _ in 0..2000 {
            let d = decide(&input(Uuid::new_v4(), &c, &b, &stops, RouteMode::Live)).unwrap();
            *counts.entry(d.chosen).or_default() += 1;
        }
        let a = counts.get("a").copied().unwrap_or(0);
        let bb = counts.get("b").copied().unwrap_or(0);
        assert!(a > bb * 4, "a {a} b {bb}");
        assert!(bb > 0, "the weaker agent still gets some traffic");
    }

    #[test]
    fn exploration_share_tracks_epsilon() {
        let c = [cand("a"), cand("b"), cand("c")];
        let b = book(&[("a", 1.0, 40), ("b", 0.5, 40), ("c", 0.5, 40)], 10 * DAY);
        let stops = BTreeMap::new();
        let n = 4000;
        let explored = (0..n)
            .filter(|_| decide(&input(Uuid::new_v4(), &c, &b, &stops, RouteMode::Live)).unwrap().explore)
            .count();
        let share = explored as f64 / n as f64;
        // A strong, steady incumbent: near the floor of 5 %.
        assert!((0.03..0.10).contains(&share), "share {share}");
    }

    #[test]
    fn epsilon_adapts_to_the_incumbent() {
        let p = RoutePolicy::default();
        let strong = adaptive_epsilon(0.95, 0.95, 20.0, &p);
        let weak = adaptive_epsilon(0.3, 0.3, 20.0, &p);
        let falling = adaptive_epsilon(0.8, 0.5, 20.0, &p);
        let unproven = adaptive_epsilon(0.9, 0.9, 1.0, &p);
        assert!(strong < 0.07, "{strong}");
        assert!(weak > 0.2, "{weak}");
        assert!(falling > strong);
        assert!(unproven >= 0.17, "{unproven}");
        for e in [strong, weak, falling, unproven] {
            assert!((0.05..=0.30).contains(&e));
        }
    }

    #[test]
    fn stopped_routes_are_skipped() {
        let c = [cand("a"), cand("b")];
        let b = book(&[("a", 1.0, 30)], 10 * DAY);
        let mut stops = BTreeMap::new();
        stops.insert(
            "a".to_string(),
            RouteStop { route: "a".into(), reason: FailureKind::RateLimited, until_ms: 11 * DAY, failures: 1, host_id: String::new(), at_ms: 10 * DAY },
        );
        for _ in 0..50 {
            let d = decide(&input(Uuid::new_v4(), &c, &b, &stops, RouteMode::Live)).unwrap();
            assert_eq!(d.chosen, "b");
            assert_eq!(d.excluded, vec!["a".to_string()]);
        }
        // A runtime-wide stop works the same.
        let mut stops = BTreeMap::new();
        stops.insert(
            runtime_route("rt-b"),
            RouteStop { route: runtime_route("rt-b"), reason: FailureKind::ProviderDown, until_ms: 11 * DAY, failures: 1, host_id: String::new(), at_ms: 0 },
        );
        let d = decide(&input(Uuid::new_v4(), &c, &b, &stops, RouteMode::Live)).unwrap();
        assert_eq!(d.chosen, "a");
        // Expired stops don't count.
        let mut stops = BTreeMap::new();
        stops.insert(
            "a".to_string(),
            RouteStop { route: "a".into(), reason: FailureKind::RateLimited, until_ms: DAY, failures: 1, host_id: String::new(), at_ms: 0 },
        );
        let d = decide(&input(Uuid::new_v4(), &c, &b, &stops, RouteMode::Live)).unwrap();
        assert!(d.excluded.is_empty());
    }

    #[test]
    fn everything_stopped_falls_back_to_the_primary() {
        let c = [cand("a")];
        let b = book(&[], 0);
        let mut stops = BTreeMap::new();
        stops.insert("a".to_string(), RouteStop { route: "a".into(), reason: FailureKind::Refusal, until_ms: i64::MAX, failures: 2, host_id: String::new(), at_ms: 0 });
        let d = decide(&input(Uuid::new_v4(), &c, &b, &stops, RouteMode::Live)).unwrap();
        assert_eq!(d.chosen, crate::flow::PRIMARY);
        assert_eq!(d.reason, RouteReason::Fallback);
    }

    #[test]
    fn private_memory_wins() {
        let c = [cand("a"), cand("b")];
        let b = book(&[("a", 1.0, 30), ("b", 0.0, 30)], 10 * DAY);
        let stops = BTreeMap::new();
        let mut i = input(Uuid::new_v4(), &c, &b, &stops, RouteMode::Live);
        i.thread_memory = Some("b");
        let d = decide(&i).unwrap();
        assert_eq!((d.chosen.as_str(), d.reason), ("b", RouteReason::ThreadMemory));
        i.thread_memory = None;
        i.file_memory = Some("b");
        assert_eq!(decide(&i).unwrap().reason, RouteReason::FileMemory);
        // Memory of a stopped agent doesn't override the stop.
        let mut stops = BTreeMap::new();
        stops.insert("b".to_string(), RouteStop { route: "b".into(), reason: FailureKind::Refusal, until_ms: i64::MAX, failures: 2, host_id: String::new(), at_ms: 0 });
        let mut i = input(Uuid::new_v4(), &c, &b, &stops, RouteMode::Live);
        i.thread_memory = Some("b");
        assert_eq!(decide(&i).unwrap().chosen, "a");
    }

    #[test]
    fn probation_agents_only_take_low_risk_classes() {
        let mut newbie = cand("n");
        newbie.low_risk_only = true;
        let c = [cand("a"), newbie];
        let b = book(&[], 0);
        let stops = BTreeMap::new();
        let d = decide(&input(Uuid::new_v4(), &c, &b, &stops, RouteMode::Live)).unwrap();
        assert_eq!(d.chosen, "a");
        assert_eq!(d.excluded, vec!["n".to_string()]);
        let mut i = input(Uuid::new_v4(), &c, &b, &stops, RouteMode::Live);
        i.text = "update the docs";
        assert!(decide(&i).unwrap().excluded.is_empty());
    }

    #[test]
    fn failures_classify_and_stop() {
        assert_eq!(classify_failure("HTTP 429 Too Many Requests"), FailureKind::RateLimited);
        assert_eq!(classify_failure("Claude usage limit reached"), FailureKind::WindowExhausted);
        assert_eq!(classify_failure("503 Service Unavailable"), FailureKind::ProviderDown);
        assert_eq!(classify_failure("error sending request: connection refused"), FailureKind::Unreachable);
        assert_eq!(classify_failure("stream timed out"), FailureKind::Timeout);
        assert_eq!(classify_failure("weird"), FailureKind::Other);

        let min = 60_000;
        // Severe: stop on the first.
        let s = stop_after_failure("a", FailureKind::RateLimited, 0, None, 15 * min, 10 * min).unwrap();
        assert_eq!(s.until_ms, 15 * min);
        // Mild: only on a repeat inside the window.
        assert!(stop_after_failure("a", FailureKind::Timeout, 0, None, 15 * min, 10 * min).is_none());
        assert!(stop_after_failure("a", FailureKind::Timeout, 20 * min, Some(0), 15 * min, 10 * min).is_none());
        assert!(stop_after_failure("a", FailureKind::Timeout, 5 * min, Some(0), 15 * min, 10 * min).is_some());
        // Cool-down of zero disables stops.
        assert!(stop_after_failure("a", FailureKind::RateLimited, 0, None, 0, 10 * min).is_none());
    }

    #[test]
    fn finds_named_paths() {
        let p = named_paths("look at src/lib.rs and `web/App.tsx`, also README.md. Not v1.2 or @coder or https://x.io/a");
        assert_eq!(p, vec!["src/lib.rs", "web/App.tsx", "README.md"]);
    }
}

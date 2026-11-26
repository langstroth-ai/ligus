//! Staggered throttles near subscription plan-window limits.
//!
//! A colony holds its brood nest within a degree or so with many small
//! actuators at *different* thresholds, not one regulator banging on and off
//! (and a genetically diverse colony, whose workers' thresholds differ, holds
//! it more steadily). Applied to a plan window (Claude's five-hour and weekly
//! limits, a Codex or Copilot quota):
//!
//! * **Protect the brood.** The human's direct, interactive turns are never
//!   throttled: they always get capacity.
//! * **Staggered thresholds.** Background work (scheduled runs) backs off
//!   first, then workflows and agent-to-agent hand-offs, each agent at a
//!   slightly different threshold (a deterministic jitter per agent), so
//!   agents stop one by one as the window fills instead of all at 100 %.
//! * **Slack.** The workflow threshold leaves headroom (15 % by default) that
//!   only interactive turns use. Not more: idle headroom invites busywork.
//!
//! The signal is whatever the runtime reports: Claude Code's rate-limit
//! events (status, utilization when given, reset time), and usage-limit
//! errors from any subscription CLI.

use serde::{Deserialize, Serialize};

/// How a turn ranks for capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnPriority {
    /// A person's direct request: never throttled.
    Interactive,
    /// Workflow stages and agent-to-agent hand-offs.
    Workflow,
    /// Scheduled and exploratory work: first to back off.
    Background,
}

impl TurnPriority {
    pub fn as_str(self) -> &'static str {
        match self {
            TurnPriority::Interactive => "interactive",
            TurnPriority::Workflow => "workflow",
            TurnPriority::Background => "background",
        }
    }
}

/// The priority of an agent turn on the runtime that runs it. A direct
/// request is interactive only when it is the agent owner's own (or the
/// agent is shared): a teammate's direct mention of someone's personal agent
/// spends the owner's plan window, so it ranks as background work and backs
/// off first. Hand-offs and workflow stages rank as workflow work, a
/// teammate's lower still.
pub fn agent_turn_priority(direct: bool, teammate: bool) -> TurnPriority {
    match (direct, teammate) {
        (_, true) => TurnPriority::Background,
        (true, false) => TurnPriority::Interactive,
        (false, false) => TurnPriority::Workflow,
    }
}

/// What a runtime last said about its plan window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WindowStatus {
    Allowed,
    /// The runtime warned the window is nearly used.
    Warning,
    /// The window is exhausted until it resets.
    Rejected,
}

/// One observation of a plan window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanWindow {
    pub status: WindowStatus,
    /// 0–100 when the runtime reports it.
    #[serde(default)]
    pub utilization_pct: Option<u8>,
    /// Unix seconds when the window resets, when known.
    #[serde(default)]
    pub resets_at: Option<i64>,
    /// Which window ("five_hour", "seven_day", ...), when known.
    #[serde(default)]
    pub window: Option<String>,
    /// Unix seconds when observed.
    #[serde(default)]
    pub observed_at: i64,
}

impl PlanWindow {
    /// The utilization to act on: the reported figure, else a floor implied
    /// by the status (a warning means "nearly full"). A rejection, or a
    /// window past its reset time, resolve as below.
    pub fn effective_pct(&self, now: i64) -> u8 {
        if self.resets_at.is_some_and(|r| now >= r) {
            return 0; // the window has reset since we heard
        }
        match self.status {
            WindowStatus::Rejected => 100,
            WindowStatus::Warning => self.utilization_pct.unwrap_or(80).max(80),
            WindowStatus::Allowed => self.utilization_pct.unwrap_or(0),
        }
    }
}

/// Thresholds (percent of the window) at which each priority backs off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThrottlePolicy {
    pub background_pct: u8,
    pub workflow_pct: u8,
    /// Total spread of per-agent thresholds around each base (± half).
    pub stagger_pct: u8,
}

impl Default for ThrottlePolicy {
    fn default() -> Self {
        Self {
            background_pct: 70,
            workflow_pct: 85,
            stagger_pct: 8,
        }
    }
}

impl ThrottlePolicy {
    /// The threshold for `priority` and `agent_key` (an agent id or name), or
    /// `None` when that priority is never throttled.
    pub fn threshold(&self, priority: TurnPriority, agent_key: &str) -> Option<u8> {
        let base = match priority {
            TurnPriority::Interactive => return None,
            TurnPriority::Workflow => self.workflow_pct,
            TurnPriority::Background => self.background_pct,
        } as i32;
        let spread = self.stagger_pct as i32;
        let jitter = if spread == 0 {
            0
        } else {
            (fnv(agent_key) % (spread as u64 + 1)) as i32 - spread / 2
        };
        // Workflows always keep at least the slack interactive turns need.
        let cap = if priority == TurnPriority::Workflow {
            95
        } else {
            99
        };
        Some((base + jitter).clamp(1, cap) as u8)
    }

    /// Whether a turn should wait, and why.
    pub fn decide(
        &self,
        priority: TurnPriority,
        agent_key: &str,
        window: Option<&PlanWindow>,
        now: i64,
    ) -> ThrottleDecision {
        let Some(threshold) = self.threshold(priority, agent_key) else {
            return ThrottleDecision::Run;
        };
        let Some(w) = window else {
            return ThrottleDecision::Run;
        };
        let pct = w.effective_pct(now);
        if pct >= threshold {
            ThrottleDecision::Defer {
                utilization_pct: pct,
                threshold_pct: threshold,
                resets_at: w.resets_at,
            }
        } else {
            ThrottleDecision::Run
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleDecision {
    Run,
    Defer {
        utilization_pct: u8,
        threshold_pct: u8,
        resets_at: Option<i64>,
    },
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(status: WindowStatus, pct: Option<u8>) -> PlanWindow {
        PlanWindow {
            status,
            utilization_pct: pct,
            resets_at: Some(10_000),
            window: Some("five_hour".into()),
            observed_at: 0,
        }
    }

    #[test]
    fn the_brood_always_runs() {
        let p = ThrottlePolicy::default();
        for s in [
            WindowStatus::Allowed,
            WindowStatus::Warning,
            WindowStatus::Rejected,
        ] {
            assert_eq!(
                p.decide(TurnPriority::Interactive, "a", Some(&w(s, Some(100))), 0),
                ThrottleDecision::Run
            );
        }
    }

    #[test]
    fn background_backs_off_before_workflows() {
        let p = ThrottlePolicy {
            stagger_pct: 0,
            ..Default::default()
        };
        let at = |pct| Some(w(WindowStatus::Allowed, Some(pct)));
        assert_eq!(
            p.decide(TurnPriority::Background, "a", at(69).as_ref(), 0),
            ThrottleDecision::Run
        );
        assert!(matches!(
            p.decide(TurnPriority::Background, "a", at(70).as_ref(), 0),
            ThrottleDecision::Defer { .. }
        ));
        assert_eq!(
            p.decide(TurnPriority::Workflow, "a", at(80).as_ref(), 0),
            ThrottleDecision::Run
        );
        assert!(matches!(
            p.decide(TurnPriority::Workflow, "a", at(85).as_ref(), 0),
            ThrottleDecision::Defer { .. }
        ));
        // No signal, no throttle.
        assert_eq!(
            p.decide(TurnPriority::Background, "a", None, 0),
            ThrottleDecision::Run
        );
    }

    #[test]
    fn thresholds_are_staggered_per_agent_and_stable() {
        let p = ThrottlePolicy::default();
        let ts: std::collections::BTreeSet<u8> = (0..40)
            .map(|i| {
                p.threshold(TurnPriority::Background, &format!("agent-{i}"))
                    .unwrap()
            })
            .collect();
        assert!(ts.len() > 3, "agents back off at different points: {ts:?}");
        assert!(ts.iter().all(|t| (66..=74).contains(t)));
        assert_eq!(
            p.threshold(TurnPriority::Workflow, "x"),
            p.threshold(TurnPriority::Workflow, "x")
        );
        // Workflows always leave interactive slack.
        let tight = ThrottlePolicy {
            workflow_pct: 99,
            stagger_pct: 20,
            background_pct: 99,
        };
        assert!((0..50).all(|i| tight
            .threshold(TurnPriority::Workflow, &i.to_string())
            .unwrap()
            <= 95));
    }

    #[test]
    fn status_without_numbers_and_resets() {
        assert_eq!(w(WindowStatus::Warning, None).effective_pct(0), 80);
        assert_eq!(w(WindowStatus::Rejected, None).effective_pct(0), 100);
        // Past the reset time the old signal no longer holds anything back.
        assert_eq!(w(WindowStatus::Rejected, None).effective_pct(10_000), 0);
    }

    #[test]
    fn a_teammates_direct_mention_is_background_on_the_owners_runtime() {
        assert_eq!(
            agent_turn_priority(true, false),
            TurnPriority::Interactive,
            "your own direct turn always runs"
        );
        assert_eq!(agent_turn_priority(true, true), TurnPriority::Background);
        assert_eq!(agent_turn_priority(false, false), TurnPriority::Workflow);
        assert_eq!(agent_turn_priority(false, true), TurnPriority::Background);
        let p = ThrottlePolicy {
            stagger_pct: 0,
            ..Default::default()
        };
        let at75 = w(WindowStatus::Allowed, Some(75));
        assert_eq!(
            p.decide(agent_turn_priority(true, false), "a", Some(&at75), 0),
            ThrottleDecision::Run
        );
        assert!(matches!(
            p.decide(agent_turn_priority(true, true), "a", Some(&at75), 0),
            ThrottleDecision::Defer { .. }
        ));
    }
}

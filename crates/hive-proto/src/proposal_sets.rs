//! IPC DTOs for proposal sets: competing alternatives, blind scout
//! assessments, agent quorum, and the human decision (see
//! `hive_core::proposal_sets`).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// One pre-flight check.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PreflightCheckDto {
    /// "passed" | "failed" | "skipped" | "unknown"
    pub status: String,
    pub note: String,
}

/// The latest pre-flight for an alternative in the current attempt.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PreflightDto {
    pub passed: bool,
    pub tests: PreflightCheckDto,
    pub base_unchanged: PreflightCheckDto,
    pub hosts_online: PreflightCheckDto,
    pub at: String,
}

/// One scout's posted assessment. Only posted assessments exist: a scout's
/// score is never shown (to people or to other scouts) before it posts.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ScoutAssessmentDto {
    pub id: String,
    pub scout_name: String,
    pub scout_runtime: String,
    /// The score as posted (0–100).
    pub score: u32,
    /// The score after decay for the rounds since it was posted.
    pub current_support: u32,
    pub rationale: String,
    pub round: u32,
    /// A scout rating an alternative it produced: shown, never counted.
    pub is_self: bool,
    pub created_at: String,
}

/// One alternative of a set, with its standing.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct SetAlternativeDto {
    pub proposal_id: String,
    /// "A", "B", … in proposal order.
    pub label: String,
    pub title: String,
    pub body: String,
    /// "fileDiff" | "command" | "decision"
    pub kind: String,
    /// The proposal's status: "open" | "approved" | "rejected" | "applied"
    pub status: String,
    #[serde(default)]
    pub changed_files: Vec<String>,
    pub support: u32,
    pub qualifying_scouts: u32,
    pub scouts: u32,
    /// Distinct runtimes among the counting scouts.
    pub runtimes: u32,
    pub meets_quorum: bool,
    /// No new agent work (a rival while piping, or stopped).
    pub frozen: bool,
    /// Live: listed, not superseded/rejected/stopped.
    pub active: bool,
    pub superseded: bool,
    /// "quorum" | "tieDeadline" | "withdrawn", when stopped.
    #[ts(optional = nullable)]
    pub stop_reason: Option<String>,
    #[ts(optional = nullable)]
    pub stopped_by: Option<String>,
    pub assessments: Vec<ScoutAssessmentDto>,
    /// Scouts sent to assess it whose assessment hasn't arrived (this device).
    #[serde(default)]
    pub pending_scouts: Vec<String>,
    /// Human approvals (these never decay).
    pub qualifying_approvals: u32,
    pub required_approvals: u32,
    #[ts(optional = nullable)]
    pub preflight: Option<PreflightDto>,
    pub reverted: bool,
}

/// A proposal set as the desktop shows it.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ProposalSetDto {
    pub id: String,
    pub question: String,
    pub body: String,
    pub author: String,
    #[ts(optional = nullable)]
    pub task_class: Option<String>,
    /// "scouting" | "piping" | "decided" | "unknown"
    pub phase: String,
    pub round: u32,
    pub attempt: u32,
    /// Agent quorum `k`.
    pub agent_quorum: u32,
    pub quality_bar: u32,
    pub decay_per_round: u32,
    pub required_approvals: u32,
    pub approval_role_floor: String,
    /// The dial position the knobs match: "fast" | "balanced" | "careful" | "custom".
    pub dial: String,
    #[ts(optional = nullable)]
    pub scouting_deadline: Option<String>,
    pub deadline_passed: bool,
    #[ts(optional = nullable)]
    pub front_runner: Option<String>,
    pub tie_break: bool,
    #[ts(optional = nullable)]
    pub winner: Option<String>,
    #[ts(optional = nullable)]
    pub decided_at: Option<String>,
    pub alternatives: Vec<SetAlternativeDto>,
    pub created_at: String,
}

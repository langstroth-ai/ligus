//! Guard signals: a device-local, content-free record of what the workspace's
//! guards and coordinators did — lease heartbeats and receipts, boundary
//! decisions, hygiene catches, worktree states, throttles and progress.
//!
//! Signals are **never synced** and never part of the event log. Each device
//! keeps its own bounded table (see `hive_runtime::event_store`), so an
//! observer on the device (the desktop's guard view, `ligus signals`, an
//! external monitor reading the local database) can see the colony's health
//! without any of it reaching other members or the relay.
//!
//! Rules every variant follows:
//!   * no content: ids, enums, counts, durations and opaque keys only — never
//!     message, prompt, proposal or diff text, never a path or repo name;
//!   * describe what happened; nothing reads a signal back to make a decision.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::lease::LeaseScope;

/// Gate a boundary decision was made at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardGate {
    DeviceJoin,
    SyncEvent,
    ToolCall,
    McpTool,
    Egress,
    /// A human-only action (a vote, a lockdown clear) that needs proof a
    /// human was present (P0.6).
    Presence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardVerdict {
    Accept,
    Reject,
}

/// Who the decision was about. Rejecting a `Nestmate` (a current member,
/// enrolled device or approved tool) is a false reject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardSubject {
    Nestmate,
    Stranger,
    Unknown,
}

/// What the workspace envelope caught.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HygieneKind {
    Redaction,
    InjectionDetected,
    EgressBlocked,
    SecretInDiff,
    EnvelopeTightened,
    CiConfigChanged,
    ProposalStale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeState {
    Active,
    Stale,
    Abandoned,
    Removed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressKind {
    TestsPassing,
    DiffConverged,
    FilesWritten,
}

/// One guard signal. Tagged by `kind`; field names are camelCase on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum GuardSignal {
    /// This device renewed a lease it holds (one per beat).
    CoordinatorHeartbeat {
        scope: LeaseScope,
        scope_id: String,
        holder: Uuid,
        lease_ms: u64,
        epoch: u64,
        beat: u64,
    },
    /// This device verified a heartbeat from a lease holder (one per new beat).
    CoordinatorReceived {
        scope: LeaseScope,
        scope_id: String,
        beat: u64,
        holder: Uuid,
        epoch: u64,
        /// The receiving host (this device).
        member: Uuid,
    },
    /// A claim or coordinator write was fenced by a newer epoch (split brain).
    LeaseFenced {
        scope: LeaseScope,
        scope_id: String,
        holder: Uuid,
        epoch: u64,
        current_epoch: u64,
    },
    /// A boundary decision.
    GuardDecision {
        gate: GuardGate,
        verdict: GuardVerdict,
        subject: GuardSubject,
        /// Device id, MCP server name or `relay`.
        source: String,
        /// Short code, e.g. `not_in_roster`, `bad_signature`, `role_floor`.
        reason: String,
    },
    /// Tools offered to a turn from one MCP server, and how many were dropped.
    ToolsSelected {
        server: String,
        offered: u32,
        dropped_cap: u32,
        dropped_collision: u32,
        /// Dropped because the turn's envelope closed egress to this server.
        #[serde(default)]
        dropped_policy: u32,
    },
    /// The workspace envelope caught something.
    Hygiene {
        hygiene_kind: HygieneKind,
        #[serde(default = "one")]
        count: u32,
    },
    /// A chat worktree changed state.
    WorktreeState {
        state: WorktreeState,
        /// Opaque key (HMAC of the chat id under a device secret), never a path.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repo_key: Option<String>,
    },
    /// Measured progress of a long agent loop.
    Progress {
        units: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress_kind: Option<ProgressKind>,
    },
    /// A turn was held back (or released) by a plan-window throttle.
    Throttle {
        runtime_id: String,
        priority: String,
        utilization_pct: u8,
        threshold_pct: u8,
        deferred: bool,
    },
}

fn one() -> u32 {
    1
}

impl GuardSignal {
    /// Stable kind string (the local table's `kind` column).
    pub fn kind_str(&self) -> &'static str {
        match self {
            GuardSignal::CoordinatorHeartbeat { .. } => "coordinatorHeartbeat",
            GuardSignal::CoordinatorReceived { .. } => "coordinatorReceived",
            GuardSignal::LeaseFenced { .. } => "leaseFenced",
            GuardSignal::GuardDecision { .. } => "guardDecision",
            GuardSignal::ToolsSelected { .. } => "toolsSelected",
            GuardSignal::Hygiene { .. } => "hygiene",
            GuardSignal::WorktreeState { .. } => "worktreeState",
            GuardSignal::Progress { .. } => "progress",
            GuardSignal::Throttle { .. } => "throttle",
        }
    }
}

/// A stored signal with where and when it was recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardSignalRecord {
    pub row_id: i64,
    pub workspace_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
    /// Unix milliseconds.
    pub at_ms: i64,
    pub signal: GuardSignal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_shape_is_tagged_camel_case_and_content_free() {
        let s = GuardSignal::CoordinatorHeartbeat {
            scope: LeaseScope::WorkflowRun,
            scope_id: "r".into(),
            holder: Uuid::nil(),
            lease_ms: 60_000,
            epoch: 2,
            beat: 7,
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["kind"], "coordinatorHeartbeat");
        assert_eq!(v["scope"], "workflowRun");
        assert_eq!(v["leaseMs"], 60_000);
        assert_eq!(serde_json::from_value::<GuardSignal>(v).unwrap(), s);

        let h = GuardSignal::Hygiene { hygiene_kind: HygieneKind::SecretInDiff, count: 2 };
        let v = serde_json::to_value(&h).unwrap();
        assert_eq!(v["hygieneKind"], "secret_in_diff");
        assert_eq!(h.kind_str(), "hygiene");
    }
}

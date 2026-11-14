//! IPC DTOs for the workspace guards: coordinator leases, guard signals and
//! (later) dearth mode, chat lockdown, plan-window throttles.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The coordinator lease of a workflow run, as this device sees it.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct RunLeaseDto {
    /// Device id of the holder.
    pub holder: String,
    /// Host label of the holder when known ("" otherwise).
    pub holder_label: String,
    /// Whether this device holds it.
    pub is_me: bool,
    pub epoch: u32,
    pub lease_ms: u32,
    /// "live" | "degraded" | "lapsed" | "released"
    pub health: String,
    /// Milliseconds since the last heartbeat (or the claim), as seen here.
    pub last_beat_age_ms: u32,
    /// False when the holder has no certified device key, so its heartbeats
    /// are trusted only as far as the workspace key goes.
    pub verified: bool,
    /// Device id the holder handed over to, while that handover is pending.
    pub handover_to: Option<String>,
    /// Claims fenced by a newer epoch recently (split brain caught).
    pub fenced_claims: u32,
}

/// One device-local guard signal (content-free), for the guard view.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct GuardSignalDto {
    pub row_id: i64,
    pub at_ms: i64,
    pub kind: String,
    pub session_id: Option<String>,
    /// The signal's fields as JSON (ids, enums and counts only).
    pub attrs_json: String,
}

/// A member waiting for a second Admin's approval (dearth mode).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PendingJoinDto {
    /// Member id or device id: what `approve_join` takes.
    pub subject: String,
    /// "member" | "device"
    pub kind: String,
    /// Display name (the member's, or the device owner's).
    pub name: String,
    /// Who added (member) or owns (device) it.
    pub added_by: String,
    /// Whether this person may approve it (an Admin who isn't the adder/owner).
    pub can_approve: bool,
}

/// One way external content or traffic reaches an agent, and its state.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct EntranceDto {
    pub name: String,
    /// "mcp" | "web" | "shell" | "runtime"
    pub kind: String,
    /// "open" | "allowed" | "closed" | "refused"
    pub state: String,
}

/// Dearth mode as the desktop shows it.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct DearthStateDto {
    pub enabled: bool,
    pub reason: String,
    pub brood_paths: Vec<String>,
    pub allowed_mcp_servers: Vec<String>,
    pub set_by: String,
    /// RFC 3339; empty when never set.
    pub set_at: String,
    /// Whether this person may switch it and approve joins (Admin+).
    pub can_manage: bool,
    pub default_brood_paths: Vec<String>,
    pub pending: Vec<PendingJoinDto>,
    /// The workspace's entrances and what dearth mode does to each.
    pub entrances: Vec<EntranceDto>,
}

/// One infection signal that locked a chat down.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct LockdownSignalDto {
    /// "injectionDetected" | "secretInDiff" | "ciConfigChanged" | "manual"
    pub signal: String,
    /// Content-free detail: a credential kind, a marker name, a file count.
    pub detail: String,
    pub raised_at: String,
    /// The reply whose turn raised it, for a jump to the cause (absent for a
    /// manual lockdown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub message_id: Option<String>,
}

/// A chat's lockdown state (self-medication).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ChatLockdownDto {
    pub active: bool,
    pub signals: Vec<LockdownSignalDto>,
    /// Whether this person may clear it (a human member, Contributor+).
    pub can_clear: bool,
}

/// A proposal the hygiene sweep marked stale.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct StaleProposalDto {
    pub session_id: String,
    pub proposal_id: String,
    pub title: String,
}

/// The last hygiene sweep's results.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct HygieneReportDto {
    /// RFC 3339; empty when no sweep ran yet.
    pub ran_at: String,
    /// False when the workspace has no git repository bound on this device.
    pub repo_bound: bool,
    pub turn_worktrees_removed: u32,
    pub chat_checkouts_removed: u32,
    /// Worktrees parked for reuse by a chat that is gone or archived.
    #[serde(default)]
    pub parked_worktrees_removed: u32,
    pub stale_proposals: Vec<StaleProposalDto>,
}

/// A subscription runtime's plan window, as last reported.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PlanWindowDto {
    /// Runtime kind, e.g. "ClaudeCode".
    pub runtime: String,
    /// "allowed" | "warning" | "rejected"
    pub status: String,
    /// The utilization the throttles act on (0-100).
    pub effective_pct: u8,
    /// Whether the runtime reported the figure (false = implied by status).
    pub reported: bool,
    /// "five_hour", "seven_day", ... or "".
    pub window: String,
    /// RFC 3339, or "" when unknown.
    pub resets_at: String,
    /// Seconds since it was observed.
    pub age_secs: u32,
}

/// Something the throttles are holding back right now.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct DeferredWorkDto {
    /// "workflow" | "background"
    pub priority: String,
    /// What is waiting (a schedule label, an agent, a workflow stage).
    pub what: String,
    pub runtime: String,
    pub utilization_pct: u8,
    pub threshold_pct: u8,
}

/// The throttle state for Settings → Guards.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ThrottleStateDto {
    pub background_pct: u8,
    pub workflow_pct: u8,
    pub stagger_pct: u8,
    pub windows: Vec<PlanWindowDto>,
    pub deferred: Vec<DeferredWorkDto>,
}

/// A suggestion to grow the workspace, made from occupancy signals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct GrowthSuggestionDto {
    /// Stable id, for dismissing it.
    pub id: String,
    /// "agent" | "host" | "worker"
    pub kind: String,
    pub title: String,
    /// The occupancy signal behind it.
    pub reason: String,
}

/// One opt-in egress preset (Settings → Guards → Network presets).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct EgressPresetDto {
    /// The name used in settings and `LIGUS_SANDBOX_EGRESS_PRESETS`.
    pub name: String,
    pub label: String,
    /// The hosts it adds (each on port 443).
    pub hosts: Vec<String>,
}

/// Egress presets on this device: what exists and what is on.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct EgressPresetsDto {
    pub available: Vec<EgressPresetDto>,
    /// On for the active workspace (names).
    pub workspace: Vec<String>,
    /// The asked-about chat's own setting; `None` = follows the workspace.
    pub chat: Option<Vec<String>>,
    /// On for every turn on this device via `LIGUS_SANDBOX_EGRESS_PRESETS`.
    pub host: Vec<String>,
    /// Why presets are ignored for that chat right now: "lockdown",
    /// "dearth", or empty when they apply.
    pub restricted_by: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Part of the `export_bindings` set (the filter CI runs matches this name).
    #[test]
    fn export_bindings_guards() {
        const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/src/bindings");
        let cfg = ts_rs::Config::new().with_out_dir(DIR);
        RunLeaseDto::export_all(&cfg).unwrap();
        GuardSignalDto::export_all(&cfg).unwrap();
        PendingJoinDto::export_all(&cfg).unwrap();
        EntranceDto::export_all(&cfg).unwrap();
        DearthStateDto::export_all(&cfg).unwrap();
        LockdownSignalDto::export_all(&cfg).unwrap();
        ChatLockdownDto::export_all(&cfg).unwrap();
        StaleProposalDto::export_all(&cfg).unwrap();
        HygieneReportDto::export_all(&cfg).unwrap();
        PlanWindowDto::export_all(&cfg).unwrap();
        DeferredWorkDto::export_all(&cfg).unwrap();
        ThrottleStateDto::export_all(&cfg).unwrap();
        GrowthSuggestionDto::export_all(&cfg).unwrap();
        EgressPresetDto::export_all(&cfg).unwrap();
        EgressPresetsDto::export_all(&cfg).unwrap();
    }
}

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
    }
}

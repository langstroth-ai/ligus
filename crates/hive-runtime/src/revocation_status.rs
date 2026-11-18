//! Phone revocation status, shared between the box that does the work
//! (`ligus serve`, the phone's tunnel host) and the desktops that show it.
//!
//! Revoking a phone's workspace device is three parts (Phase 0, item 6):
//! cut its relay tunnel pairing, remove its push registration, and end its
//! langstroth-auth sign-in session. The serve box does all three as soon as
//! it folds the `DeviceRevoked` and keeps a per-step status; it publishes
//! that status on the relay's presence channel, sealed like a host beat, so
//! the desktop that revoked the phone can show "tunnel cut, push removed,
//! session ended" (or what's still pending, and why) and ask for a retry.
//!
//! The presence channel is unsigned (any key holder can write a slot), so a
//! report is display only and a retry request only makes the box try again
//! sooner; neither can undo or skip a revocation.

use std::collections::HashMap;

use hive_core::{open_symmetric, seal_symmetric_kid, SealedEnvelope, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::keyring::KeyringEngine;
use crate::relay_client::{RelayClient, RelayError};

/// The state of one revocation step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    /// Not done yet (being tried, or waiting for a retry).
    #[default]
    Pending,
    /// Done.
    Done,
    /// The last attempt failed; it is retried automatically.
    Failed,
    /// Nothing to do on this box (for example, no langstroth-auth session).
    NotApplicable,
}

impl StepState {
    pub fn is_settled(self) -> bool {
        matches!(self, StepState::Done | StepState::NotApplicable)
    }
}

/// One step's status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StepStatus {
    pub state: StepState,
    /// A short, human-readable explanation (never a secret).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// When the state last changed (unix seconds).
    #[serde(default)]
    pub at: i64,
    /// Attempts so far.
    #[serde(default)]
    pub attempts: u32,
}

/// What one box did about one revoked device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRevocationStatus {
    pub device_id: String,
    /// The paired phone's name on this box.
    #[serde(default)]
    pub name: String,
    /// When this box folded the revocation (unix seconds).
    pub revoked_at: i64,
    /// Relay tunnel pairing cut (live sessions closed, new sessions refused).
    pub tunnel: StepStatus,
    /// Push registration removed (the relay's APNs token; this box stops pushing).
    pub push: StepStatus,
    /// langstroth-auth session ended.
    pub session: StepStatus,
}

impl DeviceRevocationStatus {
    pub fn complete(&self) -> bool {
        self.tunnel.state.is_settled() && self.push.state.is_settled() && self.session.state.is_settled()
    }

    /// "revoked: tunnel cut, push removed, session ended" (or what's left).
    pub fn summary(&self) -> String {
        let part = |s: &StepStatus, done: &str, what: &str| match s.state {
            StepState::Done => done.to_string(),
            StepState::NotApplicable => format!("{what}: not applicable"),
            StepState::Pending => format!("{what}: pending"),
            StepState::Failed => format!("{what}: failed, retrying"),
        };
        format!(
            "revoked: {}, {}, {}",
            part(&self.tunnel, "tunnel cut", "tunnel"),
            part(&self.push, "push removed", "push"),
            part(&self.session, "session ended", "session"),
        )
    }
}

/// A box's report: every device revocation it has acted on recently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RevocationReport {
    pub host_id: String,
    pub at: i64,
    pub devices: Vec<DeviceRevocationStatus>,
}

/// A desktop asking the boxes to retry a device's revocation now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryRequest {
    pub device_id: String,
    pub at: i64,
}

/// Reports carry at most this many devices (newest first).
pub const MAX_REPORTED: usize = 16;
/// A retry request older than this is ignored.
pub const RETRY_TTL_SECS: i64 = 600;

pub fn report_slot(host_id: &str) -> String {
    format!("revocations:{host_id}")
}

pub fn retry_slot(device_id: &str) -> String {
    format!("revocation-retry:{device_id}")
}

fn seal(ring: &KeyringEngine, field: &str, v: &impl Serialize) -> Option<Value> {
    let k = ring.seal_key()?;
    let plain = serde_json::to_vec(v).ok()?;
    let sealed = seal_symmetric_kid(&k.key, k.version, &k.kid, &plain).ok()?;
    Some(serde_json::json!({ field: sealed }))
}

/// Open every `field` blob sealed at (or after) the authorized head.
fn open_all<T: for<'de> Deserialize<'de>>(blobs: &HashMap<String, Value>, ring: &KeyringEngine, field: &str, prefix: &str) -> Vec<(String, T)> {
    let floor = ring.authorized_head();
    let mut out = Vec::new();
    for (slot, blob) in blobs {
        if !slot.starts_with(prefix) {
            continue;
        }
        let Some(sealed) = blob.get(field) else { continue };
        let Ok(sealed) = serde_json::from_value::<SealedEnvelope>(sealed.clone()) else { continue };
        if sealed.version < floor {
            continue;
        }
        let Some(plain) = ring.keys_for(sealed.version, sealed.kid.as_deref()).into_iter().find_map(|k| open_symmetric(k, &sealed).ok()) else {
            continue;
        };
        if let Ok(v) = serde_json::from_slice::<T>(&plain) {
            out.push((slot.clone(), v));
        }
    }
    out
}

pub fn seal_report(ring: &KeyringEngine, report: &RevocationReport) -> Option<Value> {
    seal(ring, "revocationReport", report)
}

/// Every box's report in a presence listing, keyed by host id (a report in
/// another host's slot is ignored).
pub fn open_reports(blobs: &HashMap<String, Value>, ring: &KeyringEngine) -> HashMap<String, RevocationReport> {
    open_all::<RevocationReport>(blobs, ring, "revocationReport", "revocations:")
        .into_iter()
        .filter(|(slot, r)| *slot == report_slot(&r.host_id))
        .map(|(_, r)| (r.host_id.clone(), r))
        .collect()
}

pub fn seal_retry(ring: &KeyringEngine, req: &RetryRequest) -> Option<Value> {
    seal(ring, "revocationRetry", req)
}

/// Retry requests newer than `since` and not older than [`RETRY_TTL_SECS`].
pub fn open_retries(blobs: &HashMap<String, Value>, ring: &KeyringEngine, since: i64, now: i64) -> Vec<RetryRequest> {
    open_all::<RetryRequest>(blobs, ring, "revocationRetry", "revocation-retry:")
        .into_iter()
        .filter(|(slot, r)| *slot == retry_slot(&r.device_id) && r.at > since && r.at >= now - RETRY_TTL_SECS && r.at <= now + 300)
        .map(|(_, r)| r)
        .collect()
}

pub async fn publish_report(relay: &RelayClient, room: &str, ring: &KeyringEngine, report: &RevocationReport) -> Result<(), RelayError> {
    let Some(blob) = seal_report(ring, report) else { return Ok(()) };
    relay.publish_presence(room, &report_slot(&report.host_id), &blob).await
}

pub async fn fetch_reports(relay: &RelayClient, room: &str, ring: &KeyringEngine) -> Result<HashMap<String, RevocationReport>, RelayError> {
    let blobs = relay.list_presence(room).await?;
    Ok(open_reports(&blobs, ring))
}

/// Ask every box to retry `device_id`'s revocation now.
pub async fn request_retry(relay: &RelayClient, room: &str, ring: &KeyringEngine, device_id: &str) -> Result<(), RelayError> {
    let req = RetryRequest { device_id: device_id.to_string(), at: Timestamp::now().inner().unix_timestamp() };
    let Some(blob) = seal_retry(ring, &req) else { return Ok(()) };
    relay.publish_presence(room, &retry_slot(device_id), &blob).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> KeyringEngine {
        KeyringEngine::passphrase(uuid::Uuid::nil(), hive_core::derive_workspace_key(s))
    }

    fn status(device: &str) -> DeviceRevocationStatus {
        DeviceRevocationStatus {
            device_id: device.into(),
            name: "Phone".into(),
            revoked_at: 10,
            tunnel: StepStatus { state: StepState::Done, detail: "relay closed 1 session".into(), at: 11, attempts: 1 },
            push: StepStatus { state: StepState::Done, ..Default::default() },
            session: StepStatus { state: StepState::NotApplicable, ..Default::default() },
        }
    }

    #[test]
    fn reports_round_trip_sealed_and_only_in_their_own_slot() {
        let k = key("room");
        let r = RevocationReport { host_id: "box".into(), at: 12, devices: vec![status("d1")] };
        let mut blobs = HashMap::new();
        blobs.insert(report_slot("box"), seal_report(&k, &r).unwrap());
        blobs.insert(report_slot("other"), seal_report(&k, &RevocationReport { host_id: "box".into(), ..r.clone() }).unwrap());
        blobs.insert(report_slot("stranger"), seal_report(&key("x"), &RevocationReport { host_id: "stranger".into(), ..r.clone() }).unwrap());
        blobs.insert("host:box".into(), serde_json::json!({ "hostBeat": 1 }));
        let got = open_reports(&blobs, &k);
        assert_eq!(got.len(), 1);
        assert_eq!(got["box"], r);
        assert!(!serde_json::to_string(&blobs[&report_slot("box")]).unwrap().contains("d1"), "the relay sees ciphertext only");
        assert!(got["box"].devices[0].complete());
        assert_eq!(got["box"].devices[0].summary(), "revoked: tunnel cut, push removed, session: not applicable");
    }

    #[test]
    fn retries_are_fresh_and_slot_bound() {
        let k = key("room");
        let mut blobs = HashMap::new();
        blobs.insert(retry_slot("d1"), seal_retry(&k, &RetryRequest { device_id: "d1".into(), at: 1_000 }).unwrap());
        blobs.insert(retry_slot("d2"), seal_retry(&k, &RetryRequest { device_id: "d1".into(), at: 1_000 }).unwrap());
        blobs.insert(retry_slot("d3"), seal_retry(&k, &RetryRequest { device_id: "d3".into(), at: 1 }).unwrap());
        let got = open_retries(&blobs, &k, 0, 1_010);
        assert_eq!(got, vec![RetryRequest { device_id: "d1".into(), at: 1_000 }]);
        assert!(open_retries(&blobs, &k, 1_000, 1_010).is_empty(), "already seen");
    }
}

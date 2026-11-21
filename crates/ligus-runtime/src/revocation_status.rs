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
//! Any workspace-key holder can write a presence slot, so both blobs are
//! also **signed by a device key** under their own frozen contexts: a report
//! by the serve box's device (its host id), a retry request by the desktop
//! device that asked. Readers verify against the trust roster (a certified,
//! unrevoked device; for a retry, one of a current member) and drop anything
//! else, so a key holder can neither fake a box's status nor trigger its
//! retries. A report is still display only and a retry only makes the box try
//! again sooner; neither can undo or skip a revocation.

use std::collections::HashMap;

use ligus_core::{open_symmetric, seal_symmetric_kid, SealedEnvelope, SigningKeypair, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::keyring::KeyringEngine;
use crate::relay_client::{RelayClient, RelayError};

/// FROZEN signature context for a box's revocation report.
pub const REPORT_CONTEXT: &[u8] = b"ligus-revocation-report-v1\0";
/// FROZEN signature context for a desktop's retry request.
pub const RETRY_CONTEXT: &[u8] = b"ligus-revocation-retry-v1\0";
/// FROZEN signature context for a box's paired-phones report.
pub const PAIRING_CONTEXT: &[u8] = b"ligus-pairing-report-v1\0";

/// `context ‖ canonical JSON of v without its signature`.
fn preimage(context: &[u8], v: &impl Serialize) -> Vec<u8> {
    let mut out = context.to_vec();
    out.extend_from_slice(&serde_json::to_vec(v).unwrap_or_default());
    out
}

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
    /// The serve box's workspace device id (its host id); it signs.
    pub host_id: String,
    pub at: i64,
    pub devices: Vec<DeviceRevocationStatus>,
    /// Ed25519 by the box's device key over [`REPORT_CONTEXT`] and the rest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signature: Vec<u8>,
}

impl RevocationReport {
    fn unsigned(&self) -> Self {
        Self { signature: Vec::new(), ..self.clone() }
    }

    pub fn sign(mut self, keypair: &SigningKeypair) -> Self {
        self.signature = keypair.sign(&preimage(REPORT_CONTEXT, &self.unsigned())).to_vec();
        self
    }

    pub fn verify(&self, device_public_key: &[u8]) -> bool {
        !self.signature.is_empty() && ligus_core::verify(device_public_key, &preimage(REPORT_CONTEXT, &self.unsigned()), &self.signature).is_ok()
    }
}

/// A desktop asking the boxes to retry a device's revocation now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryRequest {
    pub device_id: String,
    pub at: i64,
    /// The desktop device asking; it signs.
    pub requester: uuid::Uuid,
    /// Ed25519 by the requester's device key over [`RETRY_CONTEXT`] and the rest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signature: Vec<u8>,
}

impl RetryRequest {
    fn unsigned(&self) -> Self {
        Self { signature: Vec::new(), ..self.clone() }
    }

    pub fn sign(mut self, keypair: &SigningKeypair) -> Self {
        self.signature = keypair.sign(&preimage(RETRY_CONTEXT, &self.unsigned())).to_vec();
        self
    }

    pub fn verify(&self, device_public_key: &[u8]) -> bool {
        !self.signature.is_empty() && ligus_core::verify(device_public_key, &preimage(RETRY_CONTEXT, &self.unsigned()), &self.signature).is_ok()
    }
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

pub(crate) fn seal(ring: &KeyringEngine, field: &str, v: &impl Serialize) -> Option<Value> {
    let k = ring.seal_key()?;
    let plain = serde_json::to_vec(v).ok()?;
    let sealed = seal_symmetric_kid(&k.key, k.version, &k.kid, &plain).ok()?;
    Some(serde_json::json!({ field: sealed }))
}

/// Open every `field` blob sealed at (or after) the authorized head.
pub(crate) fn open_all<T: for<'de> Deserialize<'de>>(blobs: &HashMap<String, Value>, ring: &KeyringEngine, field: &str, prefix: &str) -> Vec<(String, T)> {
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

/// Every box's report in a presence listing, keyed by host id. A report in
/// another host's slot, or not signed by that host's device key
/// (`device_key` resolves a certified, unrevoked device), is dropped.
pub fn open_reports(blobs: &HashMap<String, Value>, ring: &KeyringEngine, device_key: impl Fn(uuid::Uuid) -> Option<Vec<u8>>) -> HashMap<String, RevocationReport> {
    open_all::<RevocationReport>(blobs, ring, "revocationReport", "revocations:")
        .into_iter()
        .filter(|(slot, r)| *slot == report_slot(&r.host_id))
        .filter(|(_, r)| uuid::Uuid::parse_str(&r.host_id).ok().and_then(&device_key).is_some_and(|pk| r.verify(&pk)))
        .map(|(_, r)| (r.host_id.clone(), r))
        .collect()
}

pub fn seal_retry(ring: &KeyringEngine, req: &RetryRequest) -> Option<Value> {
    seal(ring, "revocationRetry", req)
}

/// Retry requests newer than `since` and not older than [`RETRY_TTL_SECS`],
/// signed by a requester `requester_key` accepts (it returns the key of a
/// certified, unrevoked device of a current member, or `None`).
pub fn open_retries(blobs: &HashMap<String, Value>, ring: &KeyringEngine, since: i64, now: i64, requester_key: impl Fn(uuid::Uuid) -> Option<Vec<u8>>) -> Vec<RetryRequest> {
    open_all::<RetryRequest>(blobs, ring, "revocationRetry", "revocation-retry:")
        .into_iter()
        .filter(|(slot, r)| *slot == retry_slot(&r.device_id) && r.at > since && r.at >= now - RETRY_TTL_SECS && r.at <= now + 300)
        .filter(|(_, r)| requester_key(r.requester).is_some_and(|pk| r.verify(&pk)))
        .map(|(_, r)| r)
        .collect()
}

pub async fn publish_report(relay: &RelayClient, room: &str, ring: &KeyringEngine, report: &RevocationReport) -> Result<(), RelayError> {
    let Some(blob) = seal_report(ring, report) else { return Ok(()) };
    relay.publish_presence(room, &report_slot(&report.host_id), &blob).await
}

pub async fn fetch_reports(
    relay: &RelayClient,
    room: &str,
    ring: &KeyringEngine,
    device_key: impl Fn(uuid::Uuid) -> Option<Vec<u8>>,
) -> Result<HashMap<String, RevocationReport>, RelayError> {
    let blobs = relay.list_presence(room).await?;
    Ok(open_reports(&blobs, ring, device_key))
}

/// An unsigned retry request for `device_id` from `requester` (sign it with
/// the requester's device key: [`RetryRequest::sign`]).
pub fn retry_request(device_id: &str, requester: uuid::Uuid) -> RetryRequest {
    RetryRequest { device_id: device_id.to_string(), at: Timestamp::now().inner().unix_timestamp(), requester, signature: Vec::new() }
}

/// Ask every box to retry a device's revocation now (`req` signed).
pub async fn request_retry(relay: &RelayClient, room: &str, ring: &KeyringEngine, req: &RetryRequest) -> Result<(), RelayError> {
    let Some(blob) = seal_retry(ring, req) else { return Ok(()) };
    relay.publish_presence(room, &retry_slot(&req.device_id), &blob).await
}

// ── Which phones a box has paired ────────────────────────────────────────────
//
// So My devices can say "paired with home-server, seen 2 min ago" next to a
// phone, and the revoke confirmation can name the phone in the person's hand.
// Sealed and device-signed like a revocation report; display only.

/// One phone a box has paired (an active pairing bound to its workspace device).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PairedPhone {
    pub device_id: String,
    /// The pairing's name on this box.
    #[serde(default)]
    pub name: String,
    /// When the phone last talked to this box (unix seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<i64>,
}

/// A box's report of the phones it has paired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PairingReport {
    /// The serve box's workspace device id (its host id); it signs.
    pub host_id: String,
    /// The workspace it reports for (signed, so a member of two workspaces
    /// can't re-seal one workspace's report into the other).
    pub workspace_id: uuid::Uuid,
    pub at: i64,
    pub phones: Vec<PairedPhone>,
    /// Ed25519 by the box's device key over [`PAIRING_CONTEXT`] and the rest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signature: Vec<u8>,
}

/// Pairing reports carry at most this many phones.
pub const MAX_PAIRED_REPORTED: usize = 32;
/// A pairing's name is at most this long (what a box stores when it pairs).
pub const MAX_PAIRED_NAME_CHARS: usize = 64;
/// A pairing report older than this is not shown (a box republishes it every
/// presence beat while it has phones).
pub const PAIRING_REPORT_TTL_SECS: i64 = 60 * 60;
/// How far ahead of this device's clock a report may be dated.
const PAIRING_REPORT_SKEW_SECS: i64 = 300;

impl PairingReport {
    fn unsigned(&self) -> Self {
        Self { signature: Vec::new(), ..self.clone() }
    }

    pub fn sign(mut self, keypair: &SigningKeypair) -> Self {
        self.signature = keypair.sign(&preimage(PAIRING_CONTEXT, &self.unsigned())).to_vec();
        self
    }

    pub fn verify(&self, device_public_key: &[u8]) -> bool {
        !self.signature.is_empty() && ligus_core::verify(device_public_key, &preimage(PAIRING_CONTEXT, &self.unsigned()), &self.signature).is_ok()
    }
}

pub fn pairing_slot(host_id: &str) -> String {
    format!("pairings:{host_id}")
}

pub fn seal_pairing_report(ring: &KeyringEngine, report: &PairingReport) -> Option<Value> {
    seal(ring, "pairingReport", report)
}

/// Every box's pairing report in a presence listing, keyed by host id. A
/// report in another host's slot, not signed by that host's device key
/// (`device_key` resolves the boxes whose reports this device accepts: on a
/// desktop, its own account's certified, unrevoked devices), made for
/// another workspace than `workspace_id`, or not fresh at `now` is dropped.
/// What is kept is bounded and cleaned for display: at most
/// [`MAX_PAIRED_REPORTED`] phones, names cleaned and at most
/// [`MAX_PAIRED_NAME_CHARS`], no sighting later than the report.
pub fn open_pairing_reports(
    blobs: &HashMap<String, Value>,
    ring: &KeyringEngine,
    workspace_id: uuid::Uuid,
    now: i64,
    device_key: impl Fn(uuid::Uuid) -> Option<Vec<u8>>,
) -> HashMap<String, PairingReport> {
    open_all::<PairingReport>(blobs, ring, "pairingReport", "pairings:")
        .into_iter()
        .filter(|(slot, r)| *slot == pairing_slot(&r.host_id))
        .filter(|(_, r)| r.workspace_id == workspace_id && r.at >= now - PAIRING_REPORT_TTL_SECS && r.at <= now + PAIRING_REPORT_SKEW_SECS)
        .filter(|(_, r)| uuid::Uuid::parse_str(&r.host_id).ok().and_then(&device_key).is_some_and(|pk| r.verify(&pk)))
        .map(|(_, mut r)| {
            r.phones.truncate(MAX_PAIRED_REPORTED);
            for p in &mut r.phones {
                p.name = crate::display::shown(&p.name).chars().take(MAX_PAIRED_NAME_CHARS).collect();
                p.last_seen = p.last_seen.filter(|t| *t <= r.at);
            }
            (r.host_id.clone(), r)
        })
        .collect()
}

pub async fn publish_pairing_report(relay: &RelayClient, room: &str, ring: &KeyringEngine, report: &PairingReport) -> Result<(), RelayError> {
    let Some(blob) = seal_pairing_report(ring, report) else { return Ok(()) };
    relay.publish_presence(room, &pairing_slot(&report.host_id), &blob).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> KeyringEngine {
        KeyringEngine::passphrase(uuid::Uuid::nil(), ligus_core::derive_workspace_key(s))
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

    struct Dev {
        id: uuid::Uuid,
        kp: SigningKeypair,
    }

    fn dev() -> Dev {
        Dev { id: uuid::Uuid::new_v4(), kp: SigningKeypair::generate().unwrap() }
    }

    /// Residual 3: a workspace-key holder can seal a report into a box's
    /// slot, but not sign it with the box's device key.
    #[test]
    fn reports_are_sealed_signed_and_only_in_their_own_slot() {
        let k = key("room");
        let (bx, other, holder) = (dev(), dev(), dev());
        let keys = |d: uuid::Uuid| [&bx, &other].into_iter().find(|x| x.id == d).map(|x| x.kp.public_key_bytes().to_vec());
        let r = RevocationReport { host_id: bx.id.to_string(), at: 12, devices: vec![status("d1")], signature: Vec::new() }.sign(&bx.kp);
        let mut blobs = HashMap::new();
        blobs.insert(report_slot(&bx.id.to_string()), seal_report(&k, &r).unwrap());
        // Another (certified) box's signature over a report naming `bx`, in its own slot.
        blobs.insert(report_slot(&other.id.to_string()), seal_report(&k, &RevocationReport { host_id: bx.id.to_string(), ..r.clone() }.sign(&other.kp)).unwrap());
        blobs.insert(report_slot("stranger"), seal_report(&key("x"), &RevocationReport { host_id: "stranger".into(), ..r.clone() }).unwrap());
        blobs.insert("host:box".into(), serde_json::json!({ "hostBeat": 1 }));
        let got = open_reports(&blobs, &k, keys);
        assert_eq!(got.len(), 1);
        assert_eq!(got[&bx.id.to_string()], r);
        assert!(!serde_json::to_string(&blobs[&report_slot(&bx.id.to_string())]).unwrap().contains("d1"), "the relay sees ciphertext only");
        assert_eq!(got[&bx.id.to_string()].devices[0].summary(), "revoked: tunnel cut, push removed, session: not applicable");

        // A key holder fakes the box's report: unsigned, signed by itself,
        // or the real one with a step changed.
        let slot = report_slot(&bx.id.to_string());
        let fake = RevocationReport { devices: vec![DeviceRevocationStatus { tunnel: StepStatus { state: StepState::Failed, ..Default::default() }, ..status("d1") }], ..r.clone() };
        for forged in [RevocationReport { signature: Vec::new(), ..fake.clone() }, fake.clone().sign(&holder.kp), fake.clone()] {
            let mut b = HashMap::new();
            b.insert(slot.clone(), seal_report(&k, &forged).unwrap());
            assert!(open_reports(&b, &k, keys).is_empty(), "a forged report was shown");
        }
        // A box whose device was revoked (no certified key): dropped too.
        assert!(open_reports(&blobs, &k, |_| None).is_empty());
    }

    /// A pairing report shows only when its own box signed it, in its own
    /// slot: a workspace-key holder can't claim a box paired some phone.
    #[test]
    fn pairing_reports_are_sealed_signed_and_only_in_their_own_slot() {
        let k = key("room");
        let (bx, other, holder) = (dev(), dev(), dev());
        let keys = |d: uuid::Uuid| [&bx, &other].into_iter().find(|x| x.id == d).map(|x| x.kp.public_key_bytes().to_vec());
        let phone = PairedPhone { device_id: "p1".into(), name: "Ana's iPhone".into(), last_seen: Some(40) };
        let r = PairingReport { host_id: bx.id.to_string(), workspace_id: uuid::Uuid::nil(), at: 41, phones: vec![phone.clone()], signature: Vec::new() }.sign(&bx.kp);
        let mut blobs = HashMap::new();
        blobs.insert(pairing_slot(&bx.id.to_string()), seal_pairing_report(&k, &r).unwrap());
        // Another box's signature over a report naming `bx`, in its own slot.
        blobs.insert(pairing_slot(&other.id.to_string()), seal_pairing_report(&k, &PairingReport { host_id: bx.id.to_string(), ..r.clone() }.sign(&other.kp)).unwrap());
        // A revocation report is not a pairing report.
        blobs.insert(report_slot(&bx.id.to_string()), seal_report(&k, &RevocationReport { host_id: bx.id.to_string(), ..Default::default() }.sign(&bx.kp)).unwrap());
        let got = open_pairing_reports(&blobs, &k, uuid::Uuid::nil(), 41, keys);
        assert_eq!(got.len(), 1);
        assert_eq!(got[&bx.id.to_string()], r);
        assert!(!serde_json::to_string(&blobs[&pairing_slot(&bx.id.to_string())]).unwrap().contains("Ana"), "the relay sees ciphertext only");
        let slot = pairing_slot(&bx.id.to_string());
        let fake = PairingReport { phones: vec![PairedPhone { device_id: "p2".into(), ..phone }], ..r.clone() };
        for forged in [PairingReport { signature: Vec::new(), ..fake.clone() }, fake.clone().sign(&holder.kp), fake.clone()] {
            let mut b = HashMap::new();
            b.insert(slot.clone(), seal_pairing_report(&k, &forged).unwrap());
            assert!(open_pairing_reports(&b, &k, uuid::Uuid::nil(), 41, keys).is_empty(), "a forged pairing report was shown");
        }
        // A revocation report's signature doesn't make a pairing report.
        let crossed = PairingReport { signature: RevocationReport { host_id: bx.id.to_string(), at: 41, devices: Vec::new(), signature: Vec::new() }.sign(&bx.kp).signature, ..r.clone() };
        let mut b = HashMap::new();
        b.insert(slot.clone(), seal_pairing_report(&k, &crossed).unwrap());
        assert!(open_pairing_reports(&b, &k, uuid::Uuid::nil(), 41, keys).is_empty());
        assert!(open_pairing_reports(&blobs, &k, uuid::Uuid::nil(), 41, |_| None).is_empty(), "a revoked box's report is dropped");
    }

    /// Security review of the pairing report: a genuine report of a box is
    /// shown only in the workspace it was made for, only while it is fresh,
    /// with no sighting later than the report itself, and within the size
    /// an honest box sends (so a box's key, or a member replaying an old
    /// report, can't keep a phone "seen just now" on the revoke prompt).
    #[test]
    fn a_pairing_report_is_fresh_bounded_and_bound_to_its_workspace() {
        let (wa, wb) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let (a, b) = (KeyringEngine::passphrase(wa, ligus_core::derive_workspace_key("a")), KeyringEngine::passphrase(wb, ligus_core::derive_workspace_key("b")));
        let bx = dev();
        let keys = |d: uuid::Uuid| (d == bx.id).then(|| bx.kp.public_key_bytes().to_vec());
        let now = 1_800_000_000;
        let open = |r: &PairingReport, ring: &KeyringEngine, ws: uuid::Uuid| {
            let mut m = HashMap::new();
            m.insert(pairing_slot(&r.host_id), seal_pairing_report(ring, r).unwrap());
            open_pairing_reports(&m, ring, ws, now, keys).into_values().next()
        };
        let phone = PairedPhone { device_id: "p1".into(), name: "Ana's iPhone".into(), last_seen: Some(now - 60) };
        let fresh = PairingReport { host_id: bx.id.to_string(), workspace_id: wa, at: now, phones: vec![phone.clone()], signature: Vec::new() }.sign(&bx.kp);
        assert_eq!(open(&fresh, &a, wa).as_ref(), Some(&fresh), "control: a fresh report shows");
        // Cross-workspace: a member of both workspaces re-seals the box's
        // report (its device key certified in both) into the other one.
        assert!(open(&fresh, &b, wb).is_none(), "a report made for another workspace was shown");
        // Replay: the box's own genuine report from a month ago; one dated tomorrow.
        let old = PairingReport { at: now - 30 * 86_400, phones: vec![PairedPhone { last_seen: Some(now - 30 * 86_400), ..phone.clone() }], ..fresh.clone() }.sign(&bx.kp);
        assert!(open(&old, &a, wa).is_none(), "a month-old report was shown");
        let ahead = PairingReport { at: now + 86_400, ..fresh.clone() }.sign(&bx.kp);
        assert!(open(&ahead, &a, wa).is_none(), "a report from tomorrow was shown");
        // A sighting after the report itself ("seen just now" for years).
        let later = PairingReport { phones: vec![PairedPhone { last_seen: Some(now + 10 * 365 * 86_400), ..phone.clone() }], ..fresh.clone() }.sign(&bx.kp);
        assert_eq!(open(&later, &a, wa).unwrap().phones[0].last_seen, None, "a sighting after the report was kept");
        // Bounded: no more phones than a box reports, names as long as a pairing's.
        let huge = PairingReport {
            phones: (0..5_000).map(|i| PairedPhone { device_id: format!("p{i}"), name: format!("Ana\u{202E}{}", "x".repeat(5_000)), last_seen: None }).collect(),
            ..fresh.clone()
        }
        .sign(&bx.kp);
        let got = open(&huge, &a, wa).unwrap();
        assert_eq!(got.phones.len(), MAX_PAIRED_REPORTED);
        assert!(got.phones.iter().all(|p| p.name.chars().count() <= MAX_PAIRED_NAME_CHARS && !p.name.contains('\u{202E}')));
    }

    #[test]
    fn pairing_context_is_frozen() {
        assert_eq!(PAIRING_CONTEXT, b"ligus-pairing-report-v1\0");
        assert_eq!(pairing_slot("h"), "pairings:h");
    }

    #[test]
    fn retries_are_fresh_slot_bound_and_signed_by_a_member_device() {
        let k = key("room");
        let (desk, holder) = (dev(), dev());
        let keys = |d: uuid::Uuid| (d == desk.id).then(|| desk.kp.public_key_bytes().to_vec());
        let req = |dev_id: &str, at: i64| RetryRequest { device_id: dev_id.into(), at, requester: desk.id, signature: Vec::new() }.sign(&desk.kp);
        let mut blobs = HashMap::new();
        blobs.insert(retry_slot("d1"), seal_retry(&k, &req("d1", 1_000)).unwrap());
        blobs.insert(retry_slot("d2"), seal_retry(&k, &req("d1", 1_000)).unwrap());
        blobs.insert(retry_slot("d3"), seal_retry(&k, &req("d3", 1)).unwrap());
        let got = open_retries(&blobs, &k, 0, 1_010, keys);
        assert_eq!(got, vec![req("d1", 1_000)]);
        assert!(open_retries(&blobs, &k, 1_000, 1_010, keys).is_empty(), "already seen");
        // A key holder's request: unsigned, self-signed, or naming the
        // desktop as requester without its key.
        for forged in [
            RetryRequest { device_id: "d4".into(), at: 1_005, requester: holder.id, signature: Vec::new() },
            RetryRequest { device_id: "d4".into(), at: 1_005, requester: holder.id, signature: Vec::new() }.sign(&holder.kp),
            RetryRequest { device_id: "d4".into(), at: 1_005, requester: desk.id, signature: Vec::new() }.sign(&holder.kp),
        ] {
            let mut b = HashMap::new();
            b.insert(retry_slot("d4"), seal_retry(&k, &forged).unwrap());
            assert!(open_retries(&b, &k, 0, 1_010, keys).is_empty(), "a forged retry was taken");
        }
    }
}

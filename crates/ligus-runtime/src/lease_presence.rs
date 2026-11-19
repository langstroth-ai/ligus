//! Lease heartbeats over the relay's ephemeral presence channel.
//!
//! A coordinator lease is claimed durably (a signed `CoordinatorLeaseClaimed`
//! on the event log) but renewed ephemerally: the holder publishes a beat to
//! the presence slot `lease:<scope>:<scope_id>` every few seconds, overwritten
//! in place and never part of the log (the same reasoning as host beats in
//! [`crate::host_presence`]). Readers start succession only when no verified
//! beat for the current holder and epoch arrives within the lease.
//!
//! Two layers protect a beat:
//!   * it is sealed under the keyring's seal key (the authorized head, B1),
//!     so the relay sees only an opaque slot name and ciphertext, and readers
//!     take only beats sealed at that head: after a rotation, anyone still
//!     holding an older key (a removed member, an old invite's passphrase)
//!     can neither see which device holds a lease nor replay a beat into it;
//!   * it is **signed by the holder's device key** under its own frozen
//!     context, so another keyholder cannot keep a dead holder's lease alive
//!     (which would block succession). Readers verify it against the holder's
//!     certified device key from the trust roster. A holder with no certified
//!     key (a workspace without account keys) degrades to keyholder trust, as
//!     host beats already do; the reader reports such a lease as unverified.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ligus_core::lease::{lease_key, LeaseScope};
use ligus_core::{open_symmetric, seal_symmetric_kid, SealedEnvelope, SigningKeypair};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::keyring::KeyringEngine;
use crate::relay_client::{RelayClient, RelayError};

/// FROZEN signature context for lease beats. Never rename with the product.
pub const LEASE_BEAT_CONTEXT: &[u8] = b"ligus-lease-beat-v1\0";

/// The presence slot a scope's lease beat lives in.
pub fn lease_presence_slot(scope: LeaseScope, scope_id: &str) -> String {
    format!("lease:{}", lease_key(scope, scope_id))
}

/// A lease heartbeat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseBeat {
    pub scope: LeaseScope,
    pub scope_id: String,
    pub holder: Uuid,
    pub epoch: u64,
    /// Sequence number, increasing per holder and epoch.
    pub beat: u64,
    pub lease_ms: u64,
    /// Unix milliseconds.
    pub at_ms: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signature: Vec<u8>,
}

impl LeaseBeat {
    fn preimage(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(96);
        out.extend_from_slice(LEASE_BEAT_CONTEXT);
        let key = lease_key(self.scope, &self.scope_id);
        out.extend_from_slice(&(key.len() as u32).to_le_bytes());
        out.extend_from_slice(key.as_bytes());
        out.extend_from_slice(self.holder.as_bytes());
        out.extend_from_slice(&self.epoch.to_le_bytes());
        out.extend_from_slice(&self.beat.to_le_bytes());
        out.extend_from_slice(&self.lease_ms.to_le_bytes());
        out.extend_from_slice(&self.at_ms.to_le_bytes());
        out
    }

    /// Sign with the holder device's keypair.
    pub fn sign(mut self, keypair: &SigningKeypair) -> Self {
        self.signature = keypair.sign(&self.preimage()).to_vec();
        self
    }

    /// Verify against the holder device's public key.
    pub fn verify(&self, device_public_key: &[u8]) -> bool {
        !self.signature.is_empty() && ligus_core::verify(device_public_key, &self.preimage(), &self.signature).is_ok()
    }
}

/// Seal a signed beat under `ring`'s seal key, as a presence blob (none when
/// this device may not seal).
pub fn seal_lease_beat(ring: &KeyringEngine, beat: &LeaseBeat) -> Option<Value> {
    let k = ring.seal_key()?;
    let plain = serde_json::to_vec(beat).ok()?;
    let sealed = seal_symmetric_kid(&k.key, k.version, &k.kid, &plain).ok()?;
    Some(serde_json::json!({ "leaseBeat": sealed }))
}

/// How a reader judged a beat's signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BeatTrust {
    /// Signed by the holder's certified device key.
    Verified,
    /// The holder has no certified key; the beat is trusted as far as the
    /// workspace key goes.
    Unverified,
}

/// How far ahead of a reader's clock a beat may be stamped and still count.
/// A beat further ahead is dropped: clamping it to "now" on every read would
/// make one replayed blob look fresh forever.
pub const MAX_BEAT_SKEW_MS: i64 = 120_000;

/// Open every lease beat in a presence listing. `device_key` resolves a
/// holder's certified device key (`None` when the device has none). Skips
/// human/host presence, beats under another key or under an epoch older than
/// the authorized head, beats in the wrong slot, and
/// beats whose signature fails against a certified key. Timestamps up to
/// [`MAX_BEAT_SKEW_MS`] ahead are clamped to `now_ms`; later ones are dropped.
pub fn open_lease_beats(
    blobs: &HashMap<String, Value>,
    ring: &KeyringEngine,
    now_ms: i64,
    device_key: impl Fn(Uuid) -> Option<Vec<u8>>,
) -> Vec<(LeaseBeat, BeatTrust)> {
    let mut out = Vec::new();
    let floor = ring.authorized_head();
    for (slot, blob) in blobs {
        let Some(sealed) = blob.get("leaseBeat") else { continue };
        let Ok(sealed) = serde_json::from_value::<SealedEnvelope>(sealed.clone()) else { continue };
        if sealed.version < floor {
            continue;
        }
        let Some(plain) =
            ring.keys_for(sealed.version, sealed.kid.as_deref()).into_iter().find_map(|k| open_symmetric(k, &sealed).ok())
        else {
            continue;
        };
        let Ok(mut beat) = serde_json::from_slice::<LeaseBeat>(&plain) else { continue };
        if *slot != lease_presence_slot(beat.scope, &beat.scope_id) {
            continue;
        }
        if beat.at_ms > now_ms.saturating_add(MAX_BEAT_SKEW_MS) {
            continue;
        }
        let trust = match device_key(beat.holder) {
            Some(pk) if beat.verify(&pk) => BeatTrust::Verified,
            Some(_) => continue, // a certified holder's beat must verify: forged
            None => BeatTrust::Unverified,
        };
        beat.at_ms = beat.at_ms.min(now_ms);
        out.push((beat, trust));
    }
    out
}

/// Publish a beat to the room's presence channel.
pub async fn publish_lease_beat(relay: &RelayClient, room: &str, ring: &KeyringEngine, beat: &LeaseBeat) -> Result<(), RelayError> {
    let Some(blob) = seal_lease_beat(ring, beat) else { return Ok(()) };
    relay.publish_presence(room, &lease_presence_slot(beat.scope, &beat.scope_id), &blob).await
}

/// The newest verified beat seen per scope, shared between the poller and
/// everything that asks "is this coordinator alive?".
#[derive(Clone, Default)]
pub struct LeaseBeats {
    beats: Arc<Mutex<HashMap<String, (LeaseBeat, BeatTrust)>>>,
}

impl LeaseBeats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a beat; keeps the newest per scope (by epoch, then beat number).
    /// Returns true when it was newer than what was held (a fresh receipt).
    pub fn record(&self, beat: LeaseBeat, trust: BeatTrust) -> bool {
        let mut b = self.beats.lock().unwrap_or_else(|p| p.into_inner());
        let k = lease_key(beat.scope, &beat.scope_id);
        let newer = match b.get(&k) {
            Some((old, _)) => (beat.epoch, beat.beat, beat.at_ms) > (old.epoch, old.beat, old.at_ms),
            None => true,
        };
        if newer {
            b.insert(k, (beat, trust));
        }
        newer
    }

    /// The newest beat for a scope.
    pub fn get(&self, scope: LeaseScope, scope_id: &str) -> Option<(LeaseBeat, BeatTrust)> {
        self.beats.lock().unwrap_or_else(|p| p.into_inner()).get(&lease_key(scope, scope_id)).cloned()
    }

    /// `(holder, epoch, at_ms)` of the newest beat, the shape
    /// `ligus_core::lease::lease_health` takes.
    pub fn last(&self, scope: LeaseScope, scope_id: &str) -> Option<(Uuid, u64, i64)> {
        self.get(scope, scope_id).map(|(b, _)| (b.holder, b.epoch, b.at_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring(pass: &str) -> KeyringEngine {
        KeyringEngine::passphrase(Uuid::nil(), ligus_core::derive_workspace_key(pass))
    }

    fn beat(holder: Uuid, epoch: u64, n: u64) -> LeaseBeat {
        LeaseBeat {
            scope: LeaseScope::WorkflowRun,
            scope_id: "run".into(),
            holder,
            epoch,
            beat: n,
            lease_ms: 60_000,
            at_ms: 1_000,
            signature: Vec::new(),
        }
    }

    #[test]
    fn beat_context_is_frozen() {
        assert_eq!(LEASE_BEAT_CONTEXT, b"ligus-lease-beat-v1\0");
        let b = beat(Uuid::nil(), 1, 1);
        assert!(b.preimage().starts_with(b"ligus-lease-beat-v1\0"));
    }

    #[test]
    fn signed_beats_round_trip_and_forgeries_are_dropped() {
        let k = ring("room secret");
        let holder_kp = SigningKeypair::generate().unwrap();
        let mallory_kp = SigningKeypair::generate().unwrap();
        let holder = Uuid::new_v4();
        let pk = holder_kp.public_key_bytes().to_vec();

        let mut blobs = HashMap::new();
        let good = beat(holder, 2, 5).sign(&holder_kp);
        blobs.insert(lease_presence_slot(LeaseScope::WorkflowRun, "run"), seal_lease_beat(&k, &good).unwrap());
        let opened = open_lease_beats(&blobs, &k, 10_000, |d| (d == holder).then(|| pk.clone()));
        assert_eq!(opened.len(), 1);
        assert_eq!(opened[0].1, BeatTrust::Verified);

        // Another keyholder forges a beat to keep the holder's lease alive.
        let forged = beat(holder, 2, 6).sign(&mallory_kp);
        blobs.insert(lease_presence_slot(LeaseScope::WorkflowRun, "run"), seal_lease_beat(&k, &forged).unwrap());
        assert!(open_lease_beats(&blobs, &k, 10_000, |d| (d == holder).then(|| pk.clone())).is_empty());

        // Unsigned beat for a certified holder: dropped.
        let unsigned = beat(holder, 2, 7);
        blobs.insert(lease_presence_slot(LeaseScope::WorkflowRun, "run"), seal_lease_beat(&k, &unsigned).unwrap());
        assert!(open_lease_beats(&blobs, &k, 10_000, |d| (d == holder).then(|| pk.clone())).is_empty());

        // A tampered field breaks the signature.
        let mut tampered = beat(holder, 2, 8).sign(&holder_kp);
        tampered.epoch = 3;
        blobs.insert(lease_presence_slot(LeaseScope::WorkflowRun, "run"), seal_lease_beat(&k, &tampered).unwrap());
        assert!(open_lease_beats(&blobs, &k, 10_000, |d| (d == holder).then(|| pk.clone())).is_empty());

        // A beat in another scope's slot is ignored; one under another key too.
        let mut blobs2 = HashMap::new();
        blobs2.insert("lease:workflow_run:other".to_string(), seal_lease_beat(&k, &good).unwrap());
        blobs2.insert(
            lease_presence_slot(LeaseScope::WorkflowRun, "run"),
            seal_lease_beat(&ring("other"), &good).unwrap(),
        );
        assert!(open_lease_beats(&blobs2, &k, 10_000, |_| Some(pk.clone())).is_empty());

        // Uncertified holder: accepted as unverified.
        let mut blobs3 = HashMap::new();
        blobs3.insert(lease_presence_slot(LeaseScope::WorkflowRun, "run"), seal_lease_beat(&k, &unsigned).unwrap());
        let o = open_lease_beats(&blobs3, &k, 10_000, |_| None);
        assert_eq!(o[0].1, BeatTrust::Unverified);
        // The relay sees only ciphertext.
        assert!(!serde_json::to_string(&blobs3).unwrap().contains("holder"));
    }

    /// A beat stamped far in the future (a holder whose clock ran ahead, or
    /// one that pre-signed beats) must not keep the lease alive after the
    /// holder dies: anyone holding the sealed blob can republish it, and a
    /// future timestamp was clamped to "now" on every read, so it never aged.
    #[test]
    fn a_future_dated_beat_cannot_keep_a_dead_lease_alive() {
        use ligus_core::lease::{fold_claim, lease_health, CoordinatorLease, LeaseClaimReason, LeaseHealth, LeaseTable};
        let k = ring("room secret");
        let kp = SigningKeypair::generate().unwrap();
        let holder = Uuid::new_v4();
        let pk = kp.public_key_bytes().to_vec();
        let t0: i64 = 1_800_000_000_000;
        let mut table = LeaseTable::new();
        fold_claim(&mut table, &CoordinatorLease {
            scope: LeaseScope::WorkflowRun,
            scope_id: "run".into(),
            holder,
            epoch: 1,
            lease_ms: 60_000,
            reason: LeaseClaimReason::Initial,
            claimed_at: ligus_core::Timestamp::from_unix_seconds(t0 / 1000),
        });
        let state = table.into_values().next().unwrap();
        let mut b = beat(holder, 1, 1);
        b.at_ms = t0 + 365 * 24 * 3600 * 1000;
        let mut blobs = HashMap::new();
        blobs.insert(lease_presence_slot(LeaseScope::WorkflowRun, "run"), seal_lease_beat(&k, &b.sign(&kp)).unwrap());
        // The holder died at t0; a reader looks ten minutes later.
        let now = t0 + 10 * 60_000;
        let last = open_lease_beats(&blobs, &k, now, |_| Some(pk.clone()))
            .into_iter()
            .map(|(b, _)| (b.holder, b.epoch, b.at_ms))
            .next();
        assert_eq!(lease_health(&state, last, now), LeaseHealth::Lapsed, "a replayed future beat kept the lease live");
    }

    #[test]
    fn newest_beat_wins() {
        let s = LeaseBeats::new();
        let h = Uuid::new_v4();
        assert!(s.record(beat(h, 1, 1), BeatTrust::Verified));
        assert!(s.record(beat(h, 1, 2), BeatTrust::Verified));
        assert!(!s.record(beat(h, 1, 1), BeatTrust::Verified), "an older beat is not a fresh receipt");
        assert!(s.record(beat(h, 2, 1), BeatTrust::Verified));
        assert_eq!(s.last(LeaseScope::WorkflowRun, "run").map(|x| x.1), Some(2));
    }

    #[tokio::test]
    async fn publish_and_list_through_the_relay() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, crate::test_relay::router()).await.unwrap() });
        let relay = RelayClient::new(format!("http://{addr}"));
        let k = ring("room secret");
        let kp = SigningKeypair::generate().unwrap();
        let h = Uuid::new_v4();
        publish_lease_beat(&relay, "room-a", &k, &beat(h, 1, 1).sign(&kp)).await.unwrap();
        let blobs = relay.list_presence("room-a").await.unwrap();
        let pk = kp.public_key_bytes().to_vec();
        let got = open_lease_beats(&blobs, &k, i64::MAX, |_| Some(pk.clone()));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0.holder, h);
    }
}

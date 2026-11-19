//! Host liveness over the relay's ephemeral presence channel.
//!
//! A host (a member's desktop, an always-on box) used to prove it was online by
//! re-appending a durable `WorkspaceHostUpserted` with a fresh `last_seen` every
//! minute or so: thousands of signed events a day per host, synced to every
//! member forever. Liveness is transient, so it now rides the relay's presence
//! slots (`/v1/workspaces/{room}/presence`, overwritten in place, never part of
//! the event log). The durable host record is only written when something real
//! changes (label, kind, owner).
//!
//! A beat is sealed under the keyring's seal key (the authorized head, B1), so
//! the relay sees only an opaque slot name and ciphertext. Beats are unsigned,
//! so a reader accepts only those sealed at the authorized head: anyone still
//! holding an older key (a removed member, an old invite's passphrase) can
//! neither read nor forge liveness after a rotation. Readers merge beats with the durable `last_seen`
//! (whichever is newer), so a host still running an older build — which keeps
//! heartbeating durably — shows its true state; an older reader sees the new
//! hosts' durable `last_seen` stop moving and reports them offline/stale.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use hive_core::{open_symmetric, seal_symmetric_kid, SealedEnvelope, Timestamp, WorkspaceHost};
use serde_json::Value;

use crate::keyring::KeyringEngine;
use crate::relay_client::{RelayClient, RelayError};

/// A host counts as online when it beat within this many seconds.
pub const HOST_ONLINE_WINDOW_SECS: i64 = 90;
/// How often a running host publishes its beat (well inside the window, so one
/// lost beat doesn't flip it offline).
pub const HOST_BEAT_EVERY_SECS: i64 = 30;

/// The presence slot a host's beat lives in. Prefixed so a reader of human
/// presence (`actorId` + typing) skips it.
pub fn host_presence_slot(host_id: &str) -> String {
    format!("host:{host_id}")
}

/// A beat's plaintext: [`hive_core::workspace_repo::HostBeat`] (`hostId`,
/// `at`, and, reserved for repo coverage, `seq` and `coverage`, both left
/// out when empty, so a coverage-less beat is byte-identical to before).
type Beat = hive_core::workspace_repo::HostBeat;

/// Seal a beat for `host_id` at `at` (unix seconds) under `ring`'s seal key
/// (none when this device may not seal).
pub fn seal_host_beat(ring: &KeyringEngine, host_id: &str, at: i64) -> Option<Value> {
    seal_beat(ring, &Beat { host_id: host_id.to_string(), at, seq: 0, coverage: Vec::new() })
}

/// Seal a full beat (with this host's repo coverage and its `seq`).
pub fn seal_beat(ring: &KeyringEngine, beat: &Beat) -> Option<Value> {
    let k = ring.seal_key()?;
    let plain = serde_json::to_vec(beat).ok()?;
    let sealed = seal_symmetric_kid(&k.key, k.version, &k.kid, &plain).ok()?;
    Some(serde_json::json!({ "hostBeat": sealed }))
}

/// Open every host beat in a presence listing (`slot` → blob). Skips human
/// presence, beats sealed under another key or under an epoch older than the
/// authorized head, and a beat whose slot doesn't match the host it names. A
/// beat from the future is clamped to `now`.
pub fn open_host_beats(blobs: &HashMap<String, Value>, ring: &KeyringEngine, now: i64) -> HashMap<String, i64> {
    open_beats(blobs, ring, now).into_iter().map(|(h, b)| (h, b.at)).collect()
}

/// How far ahead of this device's clock a beat's `seq` (its sender's clock,
/// in milliseconds) may be.
const SEQ_SKEW_MS: u64 = 10 * 60 * 1000;

/// Whether a beat's `seq` could be its sender's clock now: a beat sealed
/// with a `seq` from the far future (by any workspace key holder, or by a
/// host whose clock jumped ahead) would otherwise win over every genuine
/// later beat and pin that host's coverage until every reader restarts.
fn seq_plausible(seq: u64, now_secs: i64) -> bool {
    let now_ms = u64::try_from(now_secs).unwrap_or(0).saturating_mul(1000);
    seq <= now_ms.saturating_add(SEQ_SKEW_MS)
}

/// Like [`open_host_beats`], with each host's whole beat (coverage
/// included). Of several beats for one host, the highest `seq` wins (ties
/// by `at`); a beat whose coverage breaks the bounds keeps its liveness and
/// loses its coverage.
pub fn open_beats(blobs: &HashMap<String, Value>, ring: &KeyringEngine, now: i64) -> HashMap<String, Beat> {
    let mut out: HashMap<String, Beat> = HashMap::new();
    let floor = ring.authorized_head();
    for (slot, blob) in blobs {
        let Some(sealed) = blob.get("hostBeat") else { continue };
        let Ok(sealed) = serde_json::from_value::<SealedEnvelope>(sealed.clone()) else { continue };
        if sealed.version < floor {
            continue;
        }
        let Some(plain) = ring.keys_for(sealed.version, sealed.kid.as_deref()).into_iter().find_map(|k| open_symmetric(k, &sealed).ok())
        else {
            continue;
        };
        let Ok(beat) = serde_json::from_slice::<Beat>(&plain) else { continue };
        if *slot != host_presence_slot(&beat.host_id) {
            continue;
        }
        let mut beat = beat;
        beat.at = beat.at.min(now);
        if !beat.coverage_ok() || !seq_plausible(beat.seq, now) {
            beat.coverage.clear();
            beat.seq = 0;
        }
        match out.get(&beat.host_id) {
            Some(old) if (old.seq, old.at) >= (beat.seq, beat.at) => {}
            _ => {
                out.insert(beat.host_id.clone(), beat);
            }
        }
    }
    out
}

/// Publish this host's beat to the room's presence channel.
pub async fn publish_host_beat(relay: &RelayClient, room: &str, ring: &KeyringEngine, host_id: &str) -> Result<(), RelayError> {
    let now = Timestamp::now().inner().unix_timestamp();
    let Some(blob) = seal_host_beat(ring, host_id, now) else { return Ok(()) };
    relay.publish_presence(room, &host_presence_slot(host_id), &blob).await
}

/// Publish this host's beat with its repo coverage (`seq` orders beats from
/// one host; coverage replaces the host's previous coverage).
pub async fn publish_beat(relay: &RelayClient, room: &str, ring: &KeyringEngine, host_id: &str, coverage: Vec<hive_core::workspace_repo::CoverageEntry>) -> Result<(), RelayError> {
    let now = Timestamp::now().inner().unix_timestamp();
    let seq = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let Some(blob) = seal_beat(ring, &Beat { host_id: host_id.to_string(), at: now, seq: if coverage.is_empty() { 0 } else { seq }, coverage }) else {
        return Ok(());
    };
    relay.publish_presence(room, &host_presence_slot(host_id), &blob).await
}

/// Read every host's whole latest beat in the room.
pub async fn fetch_beats(relay: &RelayClient, room: &str, ring: &KeyringEngine) -> Result<HashMap<String, Beat>, RelayError> {
    let blobs = relay.list_presence(room).await?;
    Ok(open_beats(&blobs, ring, Timestamp::now().inner().unix_timestamp()))
}

/// Read every host's latest beat in the room (host id → unix seconds).
pub async fn fetch_host_beats(relay: &RelayClient, room: &str, ring: &KeyringEngine) -> Result<HashMap<String, i64>, RelayError> {
    let blobs = relay.list_presence(room).await?;
    Ok(open_host_beats(&blobs, ring, Timestamp::now().inner().unix_timestamp()))
}

/// The latest beat seen per host, shared between the task that polls the relay
/// and everything that asks "is this host online?".
/// A host's coverage and the `seq` of the beat that carried it.
type ReportedCoverage = (u64, Vec<hive_core::workspace_repo::CoverageEntry>);

#[derive(Clone, Default)]
pub struct HostPresence {
    beats: Arc<Mutex<HashMap<String, i64>>>,
    /// Each host's latest reported repo coverage, with its beat's `seq`.
    coverage: Arc<Mutex<HashMap<String, ReportedCoverage>>>,
}

impl HostPresence {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a beat for `host_id` (keeps the newest).
    pub fn record(&self, host_id: &str, at: i64) {
        let mut b = self.beats.lock().unwrap_or_else(|p| p.into_inner());
        let e = b.entry(host_id.to_string()).or_insert(at);
        *e = (*e).max(at);
    }

    /// Merge a fetched listing.
    pub fn merge(&self, beats: HashMap<String, i64>) {
        for (id, at) in beats {
            self.record(&id, at);
        }
    }

    /// Record a whole beat: its liveness, and its coverage when its `seq`
    /// is the highest seen for the host (a coverage-less beat leaves the
    /// last coverage alone).
    pub fn record_beat(&self, beat: &Beat) {
        self.record(&beat.host_id, beat.at);
        if beat.coverage.is_empty() || !beat.coverage_ok() || !seq_plausible(beat.seq, Timestamp::now().inner().unix_timestamp()) {
            return;
        }
        let mut c = self.coverage.lock().unwrap_or_else(|p| p.into_inner());
        match c.get(&beat.host_id) {
            Some((seq, _)) if *seq >= beat.seq => {}
            _ => {
                c.insert(beat.host_id.clone(), (beat.seq, beat.coverage.clone()));
            }
        }
    }

    /// Merge fetched whole beats.
    pub fn merge_beats(&self, beats: HashMap<String, Beat>) {
        for b in beats.values() {
            self.record_beat(b);
        }
    }

    /// The coverage `host_id` last reported (`None`: it reports nothing, so
    /// readers show "unknown", never "missing").
    pub fn coverage_of(&self, host_id: &str) -> Option<Vec<hive_core::workspace_repo::CoverageEntry>> {
        self.coverage.lock().unwrap_or_else(|p| p.into_inner()).get(host_id).map(|(_, c)| c.clone())
    }

    /// The newest beat seen for `host_id`.
    pub fn beat_of(&self, host_id: &str) -> Option<i64> {
        self.beats.lock().unwrap_or_else(|p| p.into_inner()).get(host_id).copied()
    }

    /// When `host` was last seen: its newest presence beat or its durable
    /// `last_seen` (an older build still heartbeats durably), whichever is later.
    pub fn last_seen(&self, host: &WorkspaceHost) -> Timestamp {
        let durable = host.last_seen.inner().unix_timestamp();
        match self.beat_of(&host.id) {
            Some(at) if at > durable => Timestamp::from_unix_seconds(at),
            _ => host.last_seen,
        }
    }

    /// Whether `host` was seen within [`HOST_ONLINE_WINDOW_SECS`] of `now`.
    pub fn is_online(&self, host: &WorkspaceHost, now: &Timestamp) -> bool {
        now.inner().unix_timestamp() - self.last_seen(host).inner().unix_timestamp() <= HOST_ONLINE_WINDOW_SECS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::HostKind;

    fn key(s: &str) -> KeyringEngine {
        KeyringEngine::passphrase(uuid::Uuid::nil(), hive_core::derive_workspace_key(s))
    }

    #[test]
    fn beats_round_trip_and_ignore_everything_else() {
        let k = key("room secret");
        let mut blobs = HashMap::new();
        blobs.insert(host_presence_slot("box"), seal_host_beat(&k, "box", 1_000).unwrap());
        // Human presence (plaintext typing blob) is not a beat.
        blobs.insert("actor-1".into(), serde_json::json!({ "actorId": "actor-1", "ts": 5 }));
        // A beat sealed under another workspace's key doesn't open.
        blobs.insert(host_presence_slot("stranger"), seal_host_beat(&key("other"), "stranger", 1_000).unwrap());
        // A beat that claims another host's slot is ignored.
        blobs.insert(host_presence_slot("desk"), seal_host_beat(&k, "box", 2_000).unwrap());
        // A beat from the future is clamped to now.
        blobs.insert(host_presence_slot("fast"), seal_host_beat(&k, "fast", 9_999).unwrap());

        let beats = open_host_beats(&blobs, &k, 5_000);
        assert_eq!(beats.get("box"), Some(&1_000));
        assert_eq!(beats.get("fast"), Some(&5_000));
        assert_eq!(beats.len(), 2);
        // The relay only ever sees ciphertext.
        let raw = serde_json::to_string(&blobs[&host_presence_slot("box")]).unwrap();
        assert!(!raw.contains("hostId"));
    }

    #[test]
    fn online_uses_the_newer_of_beat_and_durable_last_seen() {
        let now = Timestamp::now();
        let t = now.inner().unix_timestamp();
        let mut host = WorkspaceHost::new("box", HostKind::Worker, "Box");
        host.last_seen = Timestamp::from_unix_seconds(t - 10_000);
        let p = HostPresence::new();
        assert!(!p.is_online(&host, &now), "stale durable record and no beat: offline");
        p.record("box", t - 20);
        assert!(p.is_online(&host, &now), "a fresh beat: online");
        assert_eq!(p.last_seen(&host).inner().unix_timestamp(), t - 20);

        // An older build heartbeating durably, no beats: still online.
        let mut legacy = WorkspaceHost::new("old", HostKind::Device, "Old");
        legacy.last_seen = Timestamp::from_unix_seconds(t - 5);
        assert!(p.is_online(&legacy, &now));
    }

    /// Coverage rides the sealed beat; the highest `seq` wins whatever
    /// order beats arrive in, and an unknown state reads as unknown.
    #[test]
    fn coverage_highest_seq_wins_any_order() {
        use hive_core::workspace_repo::{CoverageEntry, HostBeat};
        let k = key("room secret");
        let entry = |state: &str| CoverageEntry { repo_id: uuid::Uuid::nil(), state: state.into(), code: String::new(), via_fork: false };
        let newer = HostBeat { host_id: "box".into(), at: 100, seq: 7, coverage: vec![entry("present")] };
        let older = HostBeat { host_id: "box".into(), at: 200, seq: 3, coverage: vec![entry("missing")] };
        for order in [[&newer, &older], [&older, &newer]] {
            let p = HostPresence::new();
            for b in order {
                p.record_beat(b);
            }
            assert_eq!(p.coverage_of("box").unwrap()[0].state, "present");
            assert_eq!(p.beat_of("box"), Some(200), "liveness is the newest beat either way");
        }
        let mut blobs = HashMap::new();
        blobs.insert(host_presence_slot("box"), seal_beat(&k, &newer).unwrap());
        let opened = open_beats(&blobs, &k, 1_000);
        assert_eq!(opened["box"].coverage, newer.coverage);
        // Over the bound: liveness kept, coverage dropped.
        let big = HostBeat { host_id: "big".into(), at: 1, seq: 1, coverage: (0..65).map(|_| entry("present")).collect() };
        blobs.insert(host_presence_slot("big"), seal_beat(&k, &big).unwrap());
        let opened = open_beats(&blobs, &k, 1_000);
        assert!(opened["big"].coverage.is_empty());
        assert_eq!(HostPresence::new().coverage_of("nobody"), None);
    }

    /// A beat whose `seq` is from the far future (a forged beat sealed by any
    /// workspace key holder, or a host whose clock jumped ahead) must not pin
    /// a host's coverage forever: the host's genuine later beats still win.
    #[test]
    fn far_future_seq_never_pins_coverage() {
        use hive_core::workspace_repo::{CoverageEntry, HostBeat};
        let entry = |state: &str| CoverageEntry { repo_id: uuid::Uuid::nil(), state: state.into(), code: String::new(), via_fork: false };
        let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        let forged = HostBeat { host_id: "box".into(), at: 1, seq: u64::MAX, coverage: vec![entry("missing")] };
        let real = HostBeat { host_id: "box".into(), at: 2, seq: now_ms, coverage: vec![entry("present")] };
        let p = HostPresence::new();
        p.record_beat(&forged);
        p.record_beat(&real);
        assert_eq!(p.coverage_of("box").unwrap()[0].state, "present");
        // Opened from the relay: the same.
        let k = key("room secret");
        let mut blobs = HashMap::new();
        blobs.insert(host_presence_slot("box"), seal_beat(&k, &forged).unwrap());
        let opened = open_beats(&blobs, &k, (now_ms / 1000) as i64);
        assert!(opened.get("box").is_none_or(|b| b.coverage.is_empty()), "a far-future seq carries no coverage");
    }

    #[tokio::test]
    async fn publish_and_fetch_through_the_relay() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, crate::test_relay::router()).await.unwrap() });
        let relay = RelayClient::new(format!("http://{addr}"));
        let k = key("room secret");
        publish_host_beat(&relay, "room-a", &k, "box").await.unwrap();
        let beats = fetch_host_beats(&relay, "room-a", &k).await.unwrap();
        let now = Timestamp::now().inner().unix_timestamp();
        let at = *beats.get("box").expect("beat published");
        assert!(now - at < 5);
        assert!(fetch_host_beats(&relay, "room-b", &k).await.unwrap().is_empty());
    }
}

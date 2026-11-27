//! Workspace sync engine — the app-level loop that makes the relay actually
//! converge two clients (follow-up #125). Pushes new local envelopes to the
//! relay and ingests remote ones into the local event store, deduped by
//! `event_id`. Ported from the sync coordination in `WorkspaceSyncServices.swift`.
//!
//! This is the relay-forwarding sync path; direct P2P (STUN/hole-punch) and a
//! true CRDT are still tracked follow-ups. Ordering is by the relay's server
//! sequence on fetch + local ingestion order, which the commutative projector
//! (upsert-by-id, idempotent reactions) tolerates.

use std::collections::HashSet;
use std::sync::Arc;

use ligus_core::ids::{RoomName, WorkspaceId};
use ligus_core::{open_symmetric, seal_symmetric_kid, SealedEnvelope, SessionEventEnvelope};
use serde_json::Value;
use uuid::Uuid;

use crate::event_store::EventStore;
use crate::keyring::{KeyringEngine, KeySource};
use crate::envelope_verifier::verdict_for;
use crate::identity_verifier::{gate_ingest, shared_cache, IdentityMode, IngestAction, SharedIdentityCache};
use crate::relay_client::{RelayClient, RelayError, ENVELOPE_PAGE_LIMIT};
use crate::relay_refusal::{human_duration, RefusalClass, RelayRefusal};

/// Most relay pages one [`SyncEngine::pull_round`] fetches and applies. A room
/// with more history than this catches up over several rounds
/// ([`SyncEngine::is_behind`] says another round is due), so one round never
/// holds the store or memory for an unbounded download.
/// The relay's request-body cap (`LANGSTROTH_RELAY_MAX_BODY_BYTES`, 4 MiB by
/// default): a sealed envelope past it is never accepted.
pub const MAX_PUSH_BODY_BYTES: usize = 4 * 1024 * 1024;

pub const PULL_PAGES_PER_ROUND: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Relay(#[from] RelayError),
    #[error(transparent)]
    Store(#[from] crate::event_store::EventStoreError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("crypto error: {0}")]
    Crypto(#[from] ligus_core::CryptoError),
    /// A relay push was attempted with no workspace key. E2EE is mandatory for
    /// relay sync — we refuse to send plaintext to the relay rather than
    /// silently degrading (B4). The caller must configure a key or stay
    /// local-only. No plaintext value is ever produced in this case.
    #[error("refusing to push plaintext to the relay: no workspace key configured (E2EE is mandatory for relay sync)")]
    RelayRequiresEncryption,
    /// The workspace has an authorized key rotation this device can't open:
    /// it can read what it holds keys for, but writing under an older key
    /// would be refused by every peer. An Admin rotates to include it.
    #[error("waiting for the workspace key: this device isn't a recipient of key version {0} yet")]
    NotARecipient(u32),
}

impl SyncError {
    /// The least the caller should wait before the next round, when the
    /// relay said so (a `429`/`507` with `Retry-After`).
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            SyncError::Relay(e) => e.refusal().map(|r| r.retry_after),
            _ => None,
        }
    }
}

/// Something about this room's sync a person should see, while sync itself
/// goes on (reads keep flowing; nothing loops).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncAttention {
    /// The relay refused this device's writes to the room (it's full, the
    /// organisation's quota is reached, the relay is full or rate limiting,
    /// or this client's quota is used up). New events wait in the local
    /// store; one push is tried again at `until` (for a client quota, an
    /// hour at least).
    WritesPaused { refusal: RelayRefusal, until: Option<std::time::Instant> },
    /// A fresh fold (cursor 0) whose first envelope isn't seq 1: the relay
    /// expired the room or trimmed its front, so this device can't rebuild
    /// the workspace from the founder's seed.
    RoomTruncated { first_seq: u64 },
}

impl SyncAttention {
    /// A sentence for the person.
    pub fn message(&self) -> String {
        match self {
            SyncAttention::WritesPaused { refusal, until } => {
                let mut r = refusal.clone();
                if let Some(u) = until {
                    r.retry_after = u.saturating_duration_since(std::time::Instant::now()).max(std::time::Duration::from_secs(1));
                }
                let msg = r.user_message();
                if until.is_none() {
                    msg
                } else if r.class() == RefusalClass::ClientQuota {
                    format!("{msg} (checked again in {})", human_duration(r.retry_after))
                } else {
                    format!("{msg} (sync paused for writes, {} left)", human_duration(r.retry_after))
                }
            }
            SyncAttention::RoomTruncated { first_seq } => format!(
                "room expired or truncated: the relay's history for this workspace starts at envelope {first_seq}, not 1, so this device \
                 can't rebuild the workspace from its founding. Ask a member whose device still holds it to re-invite you, or move \
                 the workspace to a relay that keeps the whole log"
            ),
        }
    }

    /// Whether this pauses this room's writes.
    pub fn is_write_pause(&self) -> bool {
        matches!(self, SyncAttention::WritesPaused { .. })
    }
}

/// Drives one workspace's relay sync. Holds cursors so repeated rounds only
/// move new data.
pub struct SyncEngine {
    relay: RelayClient,
    /// The relay's address for this workspace's logs: builds relay URLs and
    /// names the room in logs, nothing else.
    room: RoomName,
    /// The workspace whose events belong in this room — by default
    /// [`RoomName::workspace_id`]. The store is shared by every workspace on
    /// the device (its local workspace, each joined room), so the push scan
    /// only sends events stamped with this id, and a fetched event stamped
    /// with any other id is dropped rather than ingested. Durable sync state
    /// is kept by it, never by the room string.
    workspace: WorkspaceId,
    /// Test-only: admit every workspace (fixtures that predate room scoping).
    #[cfg(test)]
    any_workspace: bool,
    /// event_ids already pushed (or ingested from the relay) — never re-push.
    /// This is a **fast-path cache** layered over the durable `pushed` table in
    /// the event store: on first use it is seeded from that table (so a restart
    /// resumes dedup), and it grows with this session's activity. The durable
    /// store is the source of truth; this set is bounded by the number of events
    /// in the workspace (same order as the durable set).
    seen: HashSet<Uuid>,
    /// Highest store row the push scan has looked at; 0 = scan everything.
    /// Reset on a failed push so the rolled-back envelopes are found again.
    scanned_row: i64,
    /// highest relay server sequence fetched so far. Loaded from the durable
    /// `sync_cursor` on first use and persisted whenever it advances, so a
    /// restart resumes from the durable cursor instead of 0.
    last_fetched_seq: u64,
    /// Where the next page starts while catching up across pages. It runs ahead
    /// of `last_fetched_seq` when a not-yet-openable body holds the durable
    /// cursor back, so paging still reaches the head; once caught up it resets
    /// and the next round re-fetches from the durable cursor (the retry point).
    page_after: Option<u64>,
    /// The relay said more envelopes remain past the last page fetched.
    behind: bool,
    /// Lowest not-yet-openable seq (no key source to park it) seen on an earlier
    /// page of the current multi-page walk. `apply_fetched` caps the cursor
    /// below it, so a later page that holds nothing can't advance the durable
    /// cursor past a held body on an earlier page (which would skip it).
    walk_hold: Option<u64>,
    /// Envelopes requested per page.
    page_limit: u32,
    /// Whether durable sync state (cursor + pushed set) has been loaded from the
    /// store yet. Guards a one-time seed on the first store-bearing call.
    loaded: bool,
    /// event_ids confirmed on the relay (pushed this session, or ingested from
    /// it) that still need to be written to the durable `pushed` table. Flushed
    /// by any store-bearing call. A **failed** push never lands here, preserving
    /// the partial-push rollback invariant.
    pending: Vec<Uuid>,
    /// The workspace keyring (B1, [`KeyringEngine`]): new events seal under
    /// its one seal key (the authorized head); a fetched body is opened with
    /// the key its `(version, kid)` names. Empty ⇒ no key: relay pushes are
    /// **refused** (E2EE is mandatory on the wire — see [`SyncEngine::encode`]).
    keyring: KeyringEngine,
    /// What this device holds, when the ring is rebuilt from the folded config
    /// log every round (every host but the passphrase-only solo case).
    key_source: Option<KeySource>,
    /// Rotation bodies from the relay keyring route (read-only candidates).
    relay_rows: Vec<Value>,
    /// When the keyring route was last fetched.
    rows_fetched: Option<std::time::Instant>,
    /// The ring parked bodies were last replayed against (A-13).
    parked_ring: Option<Vec<u8>>,
    /// Replaying parked bodies now (no nested replay).
    in_retry: bool,
    /// Replaying bodies an older build parked in the room-keyed table (no
    /// single relay was named for the room, so M1 didn't copy them): their
    /// sequences may be another relay's, so they never move the cursor and
    /// never mark a held event for refetch.
    in_legacy_retry: bool,
    /// Judging held events again now (no nested replay).
    in_held_retry: bool,
    /// When held events were last judged again on the timer.
    held_checked: Option<std::time::Instant>,
    /// Per workspace, the trust rows ([`EventStore::trust_rows_sig`]) held
    /// events were last judged against: a change makes them due again.
    held_sig: std::collections::HashMap<Uuid, (i64, i64)>,
    /// Per workspace, the trust rows dropped held events were last checked
    /// against for fetching again (`event_store::held`).
    refetch_sig: std::collections::HashMap<Uuid, (i64, i64)>,
    /// Applying events fetched again by sequence (`apply_refetched`).
    in_refetch: bool,
    /// A body couldn't be opened (or no seal key is held): fetch the keyring
    /// route on the next round.
    rows_wanted: bool,
    /// What the ring was last built from: (config fingerprint, rotations seen,
    /// relay rows).
    ring_built_from: Option<(u64, usize, usize, bool)>,
    /// Identity-verification mode (security plan S1). `Relay` (Option A, the
    /// default) trusts the roster's account keys as-is — **current behavior**.
    /// `GithubSigningKeys` (Option C) additionally requires each account key to
    /// be verified against GitHub `ssh_signing_keys` before its events project.
    #[cfg(test)]
    identity_mode: IdentityMode,
    /// GitHub-identity verdict cache (Option C). Populated out-of-band by an async
    /// task that hits GitHub; read synchronously on ingest. Unused in `Relay` mode.
    identity_cache: SharedIdentityCache,
    /// The relay refused writes to this room (see [`SyncAttention`]): the
    /// push scan sends nothing until `until` (`None`: until the engine is
    /// rebuilt or [`SyncEngine::resume_writes`]).
    write_pause: Option<(RelayRefusal, Option<std::time::Instant>)>,
    /// A fresh fold's first envelope was past seq 1 (that seq).
    truncated_at: Option<u64>,
    /// A read was refused with a `Retry-After` longer than a request waits
    /// in place: no round before this.
    round_hold: Option<std::time::Instant>,
    /// This engine's secret in its pushes' `Idempotency-Key`s
    /// ([`crate::relay_client::idempotency_key`]): no other writer to the
    /// room can compute them.
    push_secret: [u8; 32],
}

/// Outcome of decoding one fetched relay body. Separating the two failure
/// classes is what keeps a single bad body from halting the stream:
/// - `Malformed` bodies are **provably** un-openable (bad JSON, or an AEAD tag
///   that fails under a key we DO hold = tampered/corrupt/wrong-key). Re-fetching
///   can't help, so the cursor advances past them.
/// - `NotYetOpenable` bodies are well-formed but sealed under an epoch key we
///   don't hold *yet* (a lagging rotation, or a foreign-room post). A later key
///   might open them, so the cursor is held just before them to retry — while
///   later openable events are still applied this pass.
enum Decoded {
    Ready(Box<SessionEventEnvelope>),
    Malformed(String),
    NotYetOpenable(String),
}

/// Dropped held events fetched again per pull round, at most.
const REFETCH_PER_ROUND: usize = 256;

/// Unix milliseconds now (held events' ages).
fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// What ingest made of one decoded event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Judgement {
    /// Accepted (stored, or already held).
    Applied,
    /// Provably bad: refused for good (a lamport witness).
    Refused,
    /// Can't be judged yet: waits in the held table.
    Held(&'static str),
}

/// The outcome of judging a batch: one judgement per event, and how many
/// were newly stored.
struct Judged {
    outcomes: Vec<Judgement>,
    applied: usize,
}

impl SyncEngine {
    pub fn new(relay: RelayClient, room: impl Into<RoomName>) -> Self {
        let room = room.into();
        crate::identity_verifier::warn_if_identity_mode_set();
        let workspace = room.workspace_id();
        Self {
            relay,
            room,
            workspace,
            #[cfg(test)]
            any_workspace: false,
            seen: HashSet::new(),
            scanned_row: 0,
            last_fetched_seq: 0,
            page_after: None,
            behind: false,
            walk_hold: None,
            page_limit: ENVELOPE_PAGE_LIMIT,
            loaded: false,
            pending: Vec::new(),
            keyring: KeyringEngine::default(),
            key_source: None,
            relay_rows: Vec::new(),
            rows_fetched: None,
            parked_ring: None,
            in_retry: false,
            in_legacy_retry: false,
            in_held_retry: false,
            held_checked: None,
            held_sig: std::collections::HashMap::new(),
            refetch_sig: std::collections::HashMap::new(),
            in_refetch: false,
            rows_wanted: false,
            ring_built_from: None,
            #[cfg(test)]
            identity_mode: IdentityMode::Relay,
            identity_cache: shared_cache(),
            write_pause: None,
            truncated_at: None,
            round_hold: None,
            push_secret: {
                let mut b = [0u8; 32];
                getrandom::fill(&mut b).expect("os rng");
                b
            },
        }
    }

    /// How long the relay asked this device to wait before its next round
    /// (a refused read's `Retry-After`), if that time hasn't come.
    pub fn hold_left(&mut self) -> Option<std::time::Duration> {
        let until = self.round_hold?;
        let now = std::time::Instant::now();
        if until <= now {
            self.round_hold = None;
            return None;
        }
        Some(until - now)
    }

    /// What a person should know about this room's sync (writes paused by a
    /// relay refusal, or a truncated room), if anything. A write pause wins.
    pub fn attention(&self) -> Option<SyncAttention> {
        if let Some((refusal, until)) = &self.write_pause {
            return Some(SyncAttention::WritesPaused { refusal: refusal.clone(), until: *until });
        }
        self.truncated_at.map(|first_seq| SyncAttention::RoomTruncated { first_seq })
    }

    /// The `Idempotency-Key` this engine pushes `event_id` under.
    pub fn push_key(&self, event_id: Uuid) -> String {
        crate::relay_client::idempotency_key(&self.push_secret, event_id)
    }

    /// Lift a write pause now (the person freed space, or asked to retry):
    /// the next round pushes again.
    pub fn resume_writes(&mut self) {
        self.write_pause = None;
    }

    /// Whether writes are paused right now (a pause whose time came is
    /// lifted here, so the next scan tries one push again).
    fn writes_paused(&mut self) -> bool {
        match &self.write_pause {
            None => false,
            Some((_, Some(until))) if std::time::Instant::now() >= *until => {
                self.write_pause = None;
                false
            }
            Some(_) => true,
        }
    }

    /// The ingest identity mode: always `Relay` outside tests (P0.8).
    fn identity_mode(&self) -> IdentityMode {
        #[cfg(test)]
        return self.identity_mode;
        #[cfg(not(test))]
        IdentityMode::Relay
    }

    /// Scope this room to `workspace_id` instead of the id derived from the room
    /// string. For a room whose events carry an id that isn't
    /// `room_workspace_id(room)` (tests, older fixtures). Exactly one workspace:
    /// a room never mixes workspaces.
    pub fn with_room_workspace(mut self, workspace_id: impl Into<WorkspaceId>) -> Self {
        self.workspace = workspace_id.into();
        self
    }

    /// Whether an event stamped `workspace_id` belongs in this room.
    pub fn in_scope(&self, workspace_id: Uuid) -> bool {
        #[cfg(test)]
        if self.any_workspace {
            return true;
        }
        self.workspace.uuid() == workspace_id
    }

    /// Test-only: an engine that admits every workspace, for fixtures that
    /// stamp events with arbitrary workspace ids.
    #[cfg(test)]
    pub(crate) fn unscoped(relay: RelayClient, room: impl Into<RoomName>) -> Self {
        let mut e = Self::new(relay, room);
        e.any_workspace = true;
        e
    }

    /// Override the identity-verification mode (tests only; production is
    /// always [`IdentityMode::Relay`], P0.8).
    #[cfg(test)]
    pub fn with_identity_mode(mut self, mode: IdentityMode) -> Self {
        self.identity_mode = mode;
        self
    }

    /// Share a specific GitHub-identity verdict cache — so the app's async
    /// GitHub-verification task and this engine read/write the same cache.
    pub fn with_identity_cache(mut self, cache: SharedIdentityCache) -> Self {
        self.identity_cache = cache;
        self
    }

    /// The shared GitHub-identity verdict cache, for an out-of-band async task to
    /// populate (Option C). In `Relay` mode it is simply never consulted.
    pub fn identity_cache(&self) -> SharedIdentityCache {
        Arc::clone(&self.identity_cache)
    }

    /// What this engine's durable state is kept under: this relay and this
    /// room's workspace (identity model v2, section 9), never the room
    /// string alone.
    pub fn sync_key(&self) -> crate::event_store::SyncKey {
        crate::event_store::SyncKey::new(self.relay.base_url(), self.room_workspace())
    }

    /// The workspace this room's keys belong to (the commitment binds it).
    fn room_workspace(&self) -> Uuid {
        self.workspace.uuid()
    }

    /// E2EE with only the passphrase-derived epoch-0 key: the solo,
    /// local-only case. Hosts that follow rotations use
    /// [`SyncEngine::with_key_source`].
    pub fn with_key(mut self, key: [u8; 32]) -> Self {
        self.keyring = KeyringEngine::passphrase(self.room_workspace(), key);
        self
    }

    /// E2EE with a fixed ring (tests and one-shot callers).
    pub fn with_keyring(mut self, keyring: KeyringEngine) -> Self {
        self.keyring = keyring;
        self
    }

    /// E2EE with the one keyring every host uses (B1): rebuilt each round
    /// from the folded config log, this device's key-agreement key, the
    /// passphrase key, delivered epoch keys and the relay keyring route.
    pub fn with_key_source(mut self, source: KeySource) -> Self {
        self.keyring = KeyringEngine::build(self.room_workspace(), None, &source, &[]);
        self.key_source = Some(source);
        self
    }

    /// The current ring.
    pub fn keyring(&self) -> &KeyringEngine {
        &self.keyring
    }

    /// Rebuild the ring from the store's folded config log (no-op without a
    /// key source, or when nothing it depends on changed).
    pub fn refresh_keyring(&mut self, store: &EventStore) -> Result<(), SyncError> {
        let Some(src) = self.key_source.as_ref() else { return Ok(()) };
        let ws = self.room_workspace();
        // Only a pinned fold decides keys (B2 residual): without a founder pin
        // this device seals under the passphrase epoch and adopts no rotation.
        let pin = store.workspace_pin(ws)?.map(|p| p.founder_account.to_string());
        let (config, timeline) = store.load_config(ws, pin.as_deref(), None)?;
        let from = (timeline.fingerprint(), timeline.rotation_candidates().len(), self.relay_rows.len(), pin.is_some());
        if self.ring_built_from == Some(from) {
            return Ok(());
        }
        let cfg = (config.is_some() && pin.is_some()).then_some(&*timeline);
        self.keyring = KeyringEngine::build(ws, cfg, src, &self.relay_rows);
        self.ring_built_from = Some(from);
        Ok(())
    }

    /// Fetch the relay keyring route's rows when they could help: this device
    /// holds no seal key, or a body it fetched couldn't be opened (at most once
    /// a minute). Failures are soft: the config log alone carries every
    /// authorized rotation; the route only speeds up a lagging device.
    async fn fetch_keyring_rows(&mut self) {
        if self.key_source.is_none() {
            return;
        }
        let wanted = self.rows_wanted || self.keyring.seal_key().is_none();
        let recent = self.rows_fetched.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(60));
        if !wanted || recent {
            return;
        }
        match self.relay.fetch_keyring_rows(&self.room).await {
            Ok(rows) => {
                self.relay_rows = rows;
                self.rows_fetched = Some(std::time::Instant::now());
                self.rows_wanted = false;
            }
            Err(e) => tracing::debug!(error = %e, "keyring route unavailable; using the config log alone"),
        }
    }

    /// Encode an envelope for the relay: sealed ciphertext under the ring's
    /// seal key, stamped with its id. With **no** key this **refuses** with
    /// [`SyncError::RelayRequiresEncryption`] instead of emitting plaintext —
    /// E2EE is mandatory for relay sync, so the relay never sees cleartext
    /// bodies, the member roster, or inline credentials (B4). A device that
    /// isn't a recipient of the authorized head refuses too
    /// ([`SyncError::NotARecipient`]): an older key would be refused by every
    /// peer.
    fn encode(&self, env: &SessionEventEnvelope) -> Result<Value, SyncError> {
        // A rotation is sealed under the epoch it follows, so the devices that
        // hold that epoch can read it and learn the new key.
        let rotation_key = match &env.payload {
            ligus_core::SessionEvent::WorkspaceKeyRotated { rotation } => self.keyring.seal_key_for_rotation(rotation),
            _ => None,
        };
        match rotation_key.as_ref().or(self.keyring.seal_key()) {
            Some(k) => {
                let plain = serde_json::to_vec(env)?;
                Ok(serde_json::to_value(seal_symmetric_kid(&k.key, k.version, &k.kid, &plain)?)?)
            }
            None if self.keyring.is_empty() => Err(SyncError::RelayRequiresEncryption),
            None => Err(SyncError::NotARecipient(self.keyring.authorized_head())),
        }
    }

    /// Decode a relay body back into an envelope, classifying failures so a bad
    /// body can never halt the stream (see [`Decoded`]). A sealed body is opened
    /// with the key for **its** epoch.
    fn decode(&self, body: &Value) -> Decoded {
        if body.get("ciphertext").is_some() {
            // Claims to be sealed. If it doesn't even parse as a SealedEnvelope
            // it's provably garbage (e.g. a hostile `{"ciphertext":1}`) — advance.
            let sealed: SealedEnvelope = match serde_json::from_value(body.clone()) {
                Ok(s) => s,
                Err(e) => return Decoded::Malformed(format!("not a sealed envelope: {e}")),
            };
            // Well-formed, but we don't hold this epoch's key yet (lagging
            // rotation, a tie's other key, or a foreign-room post): a later
            // key might open it → retry.
            let keys = self.keyring.keys_for(sealed.version, sealed.kid.as_deref());
            if keys.is_empty() {
                return Decoded::NotYetOpenable(format!("no key for epoch {}", sealed.version));
            }
            for k in keys {
                if let Ok(plain) = open_symmetric(k, &sealed) {
                    return match serde_json::from_slice::<SessionEventEnvelope>(&plain) {
                        Ok(mut env) => {
                            env.set_sealed_epoch(Some(sealed.version));
                            Decoded::Ready(Box::new(env))
                        }
                        Err(e) => Decoded::Malformed(format!("opened but not an envelope: {e}")),
                    };
                }
            }
            // We hold the key this body names yet the AEAD tag fails to
            // verify: the body is tampered/corrupt (or foreign ciphertext
            // colliding on this epoch, incl. a wrong workspace key).
            // Re-fetching can't help → advance past it.
            Decoded::Malformed(format!(
                "AEAD open failed at epoch {} (tampered/corrupt or wrong key)",
                sealed.version
            ))
        } else if self.is_encrypted() {
            // A keyed workspace only ever pushes ciphertext, and the relay
            // never holds the key: a plaintext body here was written by the
            // relay (or a keyless client), never by a member. Skip it for good.
            Decoded::Malformed("plaintext body in an encrypted workspace".into())
        } else {
            match serde_json::from_value(body.clone()) {
                Ok(env) => Decoded::Ready(Box::new(env)),
                Err(e) => Decoded::Malformed(format!("not a session envelope: {e}")),
            }
        }
    }

    /// Push every local envelope (across all sessions) not yet pushed. Returns
    /// how many were sent.
    pub async fn push_new(&mut self, store: &EventStore) -> Result<usize, SyncError> {
        let to_push = self.take_unpushed(store)?;
        let res = self.push_envelopes(&to_push).await;
        // Persist whatever actually reached the relay — even on a partial
        // failure the delivered prefix is recorded in `pending`, so flushing
        // here durably dedups it. The failed tail was rolled back out of both
        // `seen` and `pending`, so it is never marked pushed.
        self.persist_pending(store)?;
        res?;
        Ok(to_push.len())
    }

    /// Fetch remote envelopes past the cursor and ingest the unseen ones, one
    /// bounded round (see [`SyncEngine::pull_round`]). Returns how many were
    /// newly applied.
    pub async fn pull(&mut self, store: &mut EventStore) -> Result<usize, SyncError> {
        self.pull_round(store).await
    }

    /// Fetch and apply up to [`PULL_PAGES_PER_ROUND`] pages, each applied (and
    /// its cursor persisted) before the next is fetched, so progress survives an
    /// interruption and only one page is in memory at a time. Stops early once
    /// caught up; otherwise [`SyncEngine::is_behind`] stays true and the caller
    /// should run another round soon.
    pub async fn pull_round(&mut self, store: &mut EventStore) -> Result<usize, SyncError> {
        // Load the durable cursor before fetching so a fresh engine resumes from
        // it rather than re-fetching the whole room from 0.
        self.ensure_loaded(store)?;
        self.refresh_keyring(store)?;
        self.fetch_keyring_rows().await;
        self.refresh_keyring(store)?;
        let mut applied = 0;
        for _ in 0..PULL_PAGES_PER_ROUND {
            let fetched = match self.fetch_new().await {
                Ok(f) => f,
                Err(e) => {
                    if let Some(wait) = e.retry_after() {
                        self.round_hold = Some(std::time::Instant::now() + wait);
                    }
                    return Err(e);
                }
            };
            applied += self.apply_fetched(store, &fetched)?;
            if !self.behind {
                break;
            }
        }
        applied += self.refetch_dropped(store).await?;
        Ok(applied)
    }

    /// Fetch again, by relay sequence, held events this device dropped whose
    /// wait is over (`event_store::held`): the relay still has them, so a
    /// flood of the held table doesn't make this device miss them. A bounded
    /// number per round; contiguous sequences share a page.
    pub async fn refetch_dropped(&mut self, store: &mut EventStore) -> Result<usize, SyncError> {
        let due = self.refetch_due(store)?;
        if due.is_empty() {
            return Ok(0);
        }
        let mut rows: Vec<(u64, Value)> = Vec::new();
        let mut i = 0;
        while i < due.len() {
            // A run of sequences within one page.
            let start = due[i];
            let mut j = i;
            while j + 1 < due.len() && due[j + 1] - start < u64::from(self.page_limit) {
                j += 1;
            }
            let span = (due[j] - start + 1) as u32;
            let page = self.relay.fetch_page(&self.room, start.saturating_sub(1), span).await?;
            let want: std::collections::HashSet<u64> = due[i..=j].iter().copied().collect();
            rows.extend(page.rows.into_iter().filter(|(seq, _)| want.contains(seq)));
            i = j + 1;
        }
        self.apply_refetched(store, &due, &rows)
    }

    /// Relay sequences due for fetching again (at most a page's worth).
    pub fn refetch_due(&mut self, store: &EventStore) -> Result<Vec<u64>, SyncError> {
        self.ensure_loaded(store)?;
        Ok(store.held_refetch_due(self.room_workspace(), REFETCH_PER_ROUND)?)
    }

    /// Apply `rows`, the bodies at `tried` fetched again: judged by the one
    /// gate (`apply_fetched`), without moving the cursor or the page walk.
    /// Each sequence tried spends one of its tries; one judged now (applied
    /// or refused) is done; one dropped again waits for the next roster
    /// change, until its tries run out.
    pub fn apply_refetched(&mut self, store: &mut EventStore, tried: &[u64], rows: &[(u64, Value)]) -> Result<usize, SyncError> {
        let ws = self.room_workspace();
        store.held_refetch_tried(ws, tried)?;
        let (walk_hold, behind, page_after) = (self.walk_hold, self.behind, self.page_after);
        self.in_refetch = true;
        let r = self.apply_fetched(store, rows);
        self.in_refetch = false;
        (self.walk_hold, self.behind, self.page_after) = (walk_hold, behind, page_after);
        let applied = r?;
        // Gone from the relay (expired, trimmed): nothing to wait for.
        let got: std::collections::HashSet<u64> = rows.iter().map(|(s, _)| *s).collect();
        for s in tried.iter().filter(|s| !got.contains(s)) {
            store.held_refetch_done(ws, *s)?;
        }
        store.prune_held_refetch(ws)?;
        Ok(applied)
    }

    /// Mark the dropped events of `ws` whose signer now signs for the
    /// account they claimed (what they waited for arrived) due for fetching
    /// again; checked once per change of the workspace's trust rows.
    fn mark_refetch(&mut self, store: &EventStore, ws: Uuid) -> Result<(), SyncError> {
        let sig = store.trust_rows_sig(ws)?;
        if self.refetch_sig.get(&ws) == Some(&sig) {
            return Ok(());
        }
        let rows = store.held_refetch_candidates(ws)?;
        if !rows.is_empty() {
            let pin = store.workspace_pin(ws)?;
            let roster = crate::envelope_verifier::roster_with_batch(store.trust_envelopes(ws)?, &[], pin.as_ref());
            let due: Vec<u64> = rows
                .iter()
                .filter(|r| r.signer.is_some_and(|d| crate::envelope_verifier::may_judge_now(&roster, d, r.claimed)))
                .map(|r| r.seq)
                .collect();
            store.mark_refetch_due(ws, &due)?;
        }
        self.refetch_sig.insert(ws, sig);
        Ok(())
    }

    /// Pull rounds until caught up, or `max_rounds` rounds have run. For
    /// one-shot callers (a CLI sync, a daemon's first sync) that need the whole
    /// room before they act. Returns how many events were newly applied.
    pub async fn catch_up(&mut self, store: &mut EventStore, max_rounds: usize) -> Result<usize, SyncError> {
        let mut applied = self.pull_round(store).await?;
        for _ in 1..max_rounds {
            if !self.behind {
                break;
            }
            applied += self.pull_round(store).await?;
        }
        Ok(applied)
    }

    /// The relay holds envelopes past what this engine has fetched: another
    /// pull round is due.
    pub fn is_behind(&self) -> bool {
        self.behind
    }

    /// Envelopes to request per page (tests use a small value).
    pub fn with_page_limit(mut self, limit: u32) -> Self {
        self.page_limit = limit.max(1);
        self
    }

    /// One-time seed of durable sync state (cursor + pushed set) from the store,
    /// so a freshly-constructed engine resumes rather than restarts. Idempotent;
    /// the cursor is merged with `max` so an in-flight advance is never lost.
    fn ensure_loaded(&mut self, store: &EventStore) -> Result<(), SyncError> {
        if self.loaded {
            return Ok(());
        }
        let key = self.sync_key();
        self.last_fetched_seq = self.last_fetched_seq.max(store.sync_cursor(&key)?);
        for id in store.pushed_ids(&key)? {
            self.seen.insert(id);
        }
        self.loaded = true;
        Ok(())
    }

    /// Flush `pending` (event_ids confirmed on the relay) to the durable `pushed`
    /// table. Called by every store-bearing entry point.
    fn persist_pending(&mut self, store: &EventStore) -> Result<(), SyncError> {
        let key = self.sync_key();
        for id in self.pending.drain(..) {
            store.mark_pushed(&key, id)?;
        }
        Ok(())
    }

    /// One full round: push local, then pull remote.
    pub async fn sync_once(&mut self, store: &mut EventStore) -> Result<(usize, usize), SyncError> {
        let pushed = self.push_new(store).await?;
        let pulled = self.pull(store).await?;
        Ok((pushed, pulled))
    }

    // --- Split storage/network steps, so callers (the background task) can run
    // without holding a (non-Send) store reference across an await. ---

    /// Collect local envelopes not yet pushed, marking them seen (sync; no IO).
    pub fn take_unpushed(
        &mut self,
        store: &EventStore,
    ) -> Result<Vec<SessionEventEnvelope>, SyncError> {
        // Resume dedup from the durable `pushed` set, and flush any pending
        // durable writes from a prior round (e.g. a push whose cycle bailed
        // before persisting).
        self.ensure_loaded(store)?;
        self.persist_pending(store)?;
        self.refresh_keyring(store)?;
        let mut out = Vec::new();
        // A refused room (full, over quota, rate limited): send nothing and
        // scan nothing until the pause ends; what was stored meanwhile is
        // found by the next scan (rows only grow).
        if self.writes_paused() {
            return Ok(out);
        }
        if self.scanned_row > 0 {
            // Incremental: only rows stored since the last scan. Everything at
            // or below `scanned_row` was already collected (or seen), and a
            // failed push resets the mark, so nothing is skipped.
            let (last, envs) = store.envelopes_after_row(self.scanned_row)?;
            self.scanned_row = last;
            for env in envs {
                if self.in_scope(env.workspace_id) && self.seen.insert(env.event_id) {
                    out.push(env);
                }
            }
            return Ok(out);
        }
        // Full scan (first round, or after a failed push). Read the high-water
        // mark first: a row committed during the scan has a larger id, so the
        // next incremental round finds it (and `seen` dedups any overlap).
        let mark = store.max_row_id()?;
        // Each log (workspace, session) of this room's workspace: a session
        // id another workspace also stamped is another log, scanned (or not)
        // on its own.
        let mut logs = Vec::new();
        for session_id in store.list_session_ids()? {
            for ws in store.workspaces_claiming(session_id)? {
                // Another workspace's log (the device-local workspace, a
                // different room) never leaves for this room.
                if self.in_scope(ws) {
                    logs.push((ws, session_id));
                }
            }
        }
        for (ws, session_id) in logs {
            for env in store.list_in(ws, session_id)? {
                // `seen` is seeded from the durable `pushed` table, so this skips
                // events already on the relay even across a restart. Newly
                // collected events are marked seen optimistically; a failed push
                // rolls them back (see `push_envelopes`).
                if self.seen.insert(env.event_id) {
                    out.push(env);
                }
            }
        }
        self.scanned_row = mark;
        Ok(out)
    }

    /// Push pre-collected envelopes to the relay, sealing them if a key is set
    /// (async; no store). `take_unpushed` optimistically marked these seen; if a
    /// push fails mid-batch we **roll back** `seen` for the failed envelope and
    /// every one after it, so the next round re-pushes them instead of silently
    /// dropping the tail.
    pub async fn push_envelopes(&mut self, envelopes: &[SessionEventEnvelope]) -> Result<(), SyncError> {
        for (i, env) in envelopes.iter().enumerate() {
            // `encode` refuses when there's no key (E2EE mandatory); roll back
            // the optimistic `seen` marks just like a network failure so the
            // envelopes are re-attempted once a key is configured.
            let body = match self.encode(env) {
                Ok(body) => body,
                Err(e) => {
                    for unsent in &envelopes[i..] {
                        self.seen.remove(&unsent.event_id);
                    }
                    self.scanned_row = 0;
                    return Err(e);
                }
            };
            // One the relay can never take (past its request-body cap; this
            // device refuses to write one, but an older build or a peer may
            // have) is set aside instead of holding the outbox: every event
            // after it would wait for good.
            let size = serde_json::to_vec(&body).map(|b| b.len()).unwrap_or(usize::MAX);
            if size > MAX_PUSH_BODY_BYTES {
                self.set_aside(env, &format!("{size} bytes sealed, past the relay's {MAX_PUSH_BODY_BYTES}-byte cap"));
                continue;
            }
            // The key makes a re-push (after a refusal or a lost answer) the
            // same write to the relay, not a second copy.
            let key = self.push_key(env.event_id);
            if let Err(e) = self.relay.push_value_keyed(&self.room, &body, Some(&key)).await {
                // `413`: this relay's cap is lower than this envelope; it never
                // will take it. Set aside, as above, and go on.
                if matches!(&e, RelayError::Status(413)) || e.refusal().is_some_and(|r| r.status == 413) {
                    self.set_aside(env, "the relay refused it as too large (413)");
                    continue;
                }
                for unsent in &envelopes[i..] {
                    self.seen.remove(&unsent.event_id);
                }
                self.scanned_row = 0;
                // A refusal of writes pauses this room's writes (the events
                // stay in the outbox) instead of failing the round, so pulls
                // go on and nothing loops against a full room.
                if let RelayError::Refused(r) = &e {
                    if r.class().pauses_writes() {
                        // Every pause ends (a client quota rechecks after an
                        // hour at least), so one refusal can't stop this
                        // room's writes until a restart.
                        let until = Some(std::time::Instant::now() + r.class().write_pause(r.retry_after));
                        tracing::warn!(target: "sync", room = %self.room, code = %r.code, "relay refused writes: {}", r.user_message());
                        self.write_pause = Some((r.clone(), until));
                        return Ok(());
                    }
                }
                return Err(e.into());
            }
            // Delivered: queue it for the durable `pushed` table so a restart
            // won't re-push it. Only reached AFTER a successful relay write, so a
            // failed push (handled above) never marks its events pushed.
            self.pending.push(env.event_id);
            self.write_pause = None;
        }
        Ok(())
    }

    /// Never push `env` (the relay can't take it): it is recorded as done,
    /// so neither this round nor a restart retries it, and the outbox moves
    /// on. It stays in this device's store.
    fn set_aside(&mut self, env: &SessionEventEnvelope, why: &str) {
        tracing::error!(target: "sync", room = %self.room, event_id = %env.event_id, "an event too large for the relay is not sent: {why}");
        self.pending.push(env.event_id);
    }

    /// Whether relay pushes will be sealed (a key is configured). `false` means
    /// every push would be refused — callers should stay local-only or surface
    /// an actionable "no workspace key" error rather than attempting to sync.
    pub fn is_encrypted(&self) -> bool {
        !self.keyring.is_empty()
    }

    /// Whether pushes will go out: a seal key is held.
    pub fn can_seal(&self) -> bool {
        self.keyring.seal_key().is_some()
    }

    /// Fetch the next page of raw remote bodies (async; no store). The durable
    /// cursor is **not** advanced here — it only moves in `apply_fetched`, so a
    /// body fetched before its decryption key is available can be re-fetched and
    /// isn't lost. One page only: [`SyncEngine::is_behind`] says whether more
    /// remain, and the next call continues after this page.
    ///
    /// Callers that construct a fresh engine must load the durable cursor first
    /// (`take_unpushed` / `pull_round` do), or the first page starts at 0.
    pub async fn fetch_new(&mut self) -> Result<Vec<(u64, Value)>, SyncError> {
        let after = match self.page_after {
            Some(p) if self.behind => p.max(self.last_fetched_seq),
            _ => {
                // A fresh walk from the durable cursor: it re-meets any held body.
                self.walk_hold = None;
                self.last_fetched_seq
            }
        };
        let page = self.relay.fetch_page(&self.room, after, self.page_limit).await?;
        // A fresh fold starts at the founder's seed (seq 1). A first envelope
        // past it means the relay expired or trimmed the room: say so.
        if after == 0 {
            if let Some(first) = page.rows.iter().map(|(seq, _)| *seq).min().filter(|s| *s > 1) {
                tracing::warn!(target: "sync", room = %self.room, first, "room expired or truncated: its log starts past seq 1");
                self.truncated_at = Some(first);
            }
        }
        self.behind = page.more;
        self.page_after = page.more.then_some(page.next_after);
        Ok(page.rows)
    }

    /// Decode + ingest fetched bodies into the store (sync; no IO awaits), then
    /// advance the cursor. Returns how many were newly applied.
    ///
    /// **Cursor / retry semantics.** A single bad body must never halt the stream
    /// (the old code either aborted the pull on a decode error or `break`ed the
    /// batch, leaving the cursor before the offender so every later pull re-fetched
    /// it forever and applied nothing after it). Instead we classify each body and
    /// keep the cursor **monotonic**, advancing it to the highest CONTIGUOUS
    /// resolved sequence that sits *below the lowest not-yet-openable body*:
    /// - `Malformed`/quarantined bodies are provably bad and stable — we skip them
    ///   and let the cursor advance past them (re-fetching can't help).
    /// - An event ingest **holds** (a device not certified yet for the account it
    ///   claims, an account not admitted yet, a pending dearth-mode approval, a
    ///   disputed account key) is moved to the device's held table
    ///   ([`crate::event_store::HoldItem`]) and the cursor moves past it: an event
    ///   whose certificate or admission never comes can't stop this device from
    ///   syncing what follows. It is judged again when the roster changes and on
    ///   a slow timer ([`SyncEngine::retry_held`]).
    /// - A `NotYetOpenable` body (well-formed sealed body under an epoch key we
    ///   lack) is parked when the ring is rebuilt from the config log (A-13);
    ///   otherwise (the passphrase-only case) it becomes the **retry point**: the
    ///   cursor is held just before the lowest such sequence so a later pull
    ///   re-fetches it once its key arrives — but every openable event *after* it
    ///   is still applied this pass (deduped, idempotent).
    ///
    /// A genuinely un-openable-forever body under a *held* epoch (foreign/tampered
    /// ciphertext) is `Malformed`, so it advances and cannot permanently wedge.
    pub fn apply_fetched(
        &mut self,
        store: &mut EventStore,
        fetched: &[(u64, Value)],
    ) -> Result<usize, SyncError> {
        // Resume the durable cursor + pushed set (so a fresh engine that only
        // pulls still resumes), and flush any pending durable writes.
        self.ensure_loaded(store)?;
        self.persist_pending(store)?;
        let cursor_before = self.last_fetched_seq;
        // Classify every body; never stop early. `resolved` = sequences the cursor
        // may pass (openable or provably-bad); `retry_at` = lowest sequence we must
        // re-fetch later (a not-yet-openable body), which caps how far the cursor
        // may advance this pass.
        let mut seqs: Vec<u64> = Vec::new();
        let mut decoded: Vec<SessionEventEnvelope> = Vec::new();
        let mut resolved: Vec<u64> = Vec::new();
        let mut retry_at: Option<u64> = None;
        let mut parked_now: Vec<u64> = Vec::new();
        let mut to_park: Vec<(u64, &Value, u8)> = Vec::new();
        let mut foreign = 0usize;
        for (seq, body) in fetched {
            match self.decode(body) {
                // An event of another workspace has no business in this room
                // (an older client pushed everything in its store). Never
                // ingest it, and move past it: it is stable, so re-fetching
                // can't change the verdict.
                Decoded::Ready(env) if !self.in_scope(env.workspace_id) => {
                    foreign += 1;
                    resolved.push(*seq);
                }
                Decoded::Ready(env) => {
                    seqs.push(*seq);
                    decoded.push(*env);
                }
                Decoded::Malformed(reason) => {
                    tracing::warn!(seq = *seq, reason, "skipping a malformed relay body; advancing the cursor past it");
                    resolved.push(*seq);
                }
                // A-13: with the one keyring, a body sealed under a key this
                // device doesn't hold is parked (device-local) and the cursor
                // moves on: a junk epoch nobody announced can't pin it. Parked
                // bodies are retried whenever the ring changes.
                Decoded::NotYetOpenable(reason) if self.key_source.is_some() => {
                    tracing::debug!(seq = *seq, reason, "parking a body sealed under a key this device doesn't hold");
                    to_park.push((*seq, body, self.park_priority(body)));
                    self.rows_wanted = true;
                    parked_now.push(*seq);
                    resolved.push(*seq);
                }
                Decoded::NotYetOpenable(reason) => {
                    // Decrypt-failure log: previously silent, which made a wrong
                    // workspace key an invisible onboarding failure.
                    tracing::warn!(seq = *seq, reason, "cannot open a fetched body yet (missing epoch key); holding the cursor before it to retry");
                    retry_at = Some(retry_at.map_or(*seq, |r| r.min(*seq)));
                }
            }
        }

        // (An old-table body that still can't be opened stays where it is.)
        if !self.in_legacy_retry {
            store.park_bodies(&self.sync_key(), &to_park)?;
        }
        if foreign > 0 {
            tracing::warn!(room = %self.room, foreign, "dropped fetched events stamped with another workspace");
        }
        // A body held on an earlier page of this walk still caps the cursor.
        if let Some(h) = self.walk_hold {
            retry_at = Some(retry_at.map_or(h, |r| r.min(h)));
        }

        // A batch that can change the roster (a trust or config event) is
        // judged together with what this room holds: one roster build, and a
        // held event the batch completes (its certificate, its admission) is
        // released in the same pass, as if it had arrived after it.
        let roster_relevant = decoded.iter().any(|e| {
            crate::envelope_verifier::is_trust_kind(&e.payload) || e.session_id == ligus_core::workspace_config_session_id(e.workspace_id)
        });
        let mut held_in: Vec<(Uuid, crate::event_store::HeldEvent)> = Vec::new();
        if roster_relevant && !self.in_held_retry {
            for ws in store.held_workspaces()?.into_iter().filter(|w| self.in_scope(*w)) {
                held_in.extend(store.held_events(ws)?.into_iter().map(|h| (ws, h)));
            }
        }
        let batch_len = decoded.len();
        decoded.extend(held_in.iter().map(|(_, h)| h.env.clone()));
        let judged = self.judge(store, &decoded, batch_len)?;
        decoded.truncate(batch_len);
        let mut released: std::collections::HashMap<Uuid, Vec<String>> = std::collections::HashMap::new();
        for ((ws, h), outcome) in held_in.iter().zip(&judged.outcomes[batch_len..]) {
            if !matches!(outcome, Judgement::Held(_)) {
                released.entry(*ws).or_default().push(h.digest.clone());
            }
        }
        for (ws, digests) in &released {
            store.release_held(*ws, digests)?;
        }
        let mut applied = judged.applied;
        let mut to_hold: Vec<crate::event_store::HoldItem<'_>> = Vec::new();
        for ((seq, env), outcome) in seqs.iter().zip(&decoded).zip(&judged.outcomes[..batch_len]) {
            // Every judged event lets the cursor pass: a held one waits in the
            // held table, never before the cursor.
            resolved.push(*seq);
            match outcome {
                Judgement::Held(reason) => {
                    to_hold.push(crate::event_store::HoldItem { env, reason, seq: (!self.in_legacy_retry).then_some(*seq) })
                }
                // Fetched again and judged now: nothing more to fetch.
                _ if self.in_refetch => store.held_refetch_done(self.room_workspace_scope(std::slice::from_ref(env)), *seq)?,
                _ => {}
            }
        }
        if !to_hold.is_empty() {
            let ws = self.room_workspace_scope(&decoded);
            let n = store.hold_events(ws, &to_hold, now_ms())?;
            tracing::debug!(room = %self.room, held = n, "holding fetched events until the roster can judge them");
            // Nothing else to do: a held trust or config event was judged
            // together with every other held event just now (a batch that
            // carries one is judged with the held table), and content can't
            // unblock anything.
        }
        // The held events judged with this batch saw the roster this batch
        // makes: not due again until it changes.
        if !held_in.is_empty() {
            let mut seen = std::collections::HashSet::new();
            for (ws, _) in &held_in {
                if seen.insert(*ws) {
                    self.held_sig.insert(*ws, store.trust_rows_sig(*ws)?);
                }
            }
        }
        if roster_relevant && !self.in_held_retry {
            let ws = self.room_workspace_scope(&decoded);
            self.mark_refetch(store, ws)?;
        }

        // B1: a rotation may have arrived on the config log; rebuild the ring
        // so this page's held bodies open on the next pass. A body we couldn't
        // open also makes the relay keyring route due again.
        if applied > 0 {
            self.refresh_keyring(store)?;
        }
        if retry_at.is_some() {
            self.rows_wanted = true;
        }

        // Advance the cursor to the highest resolved sequence, but never to or
        // past the lowest not-yet-openable sequence (so it's re-fetched), and
        // never backwards. Later openable events above `retry_at` were still
        // applied above; the cursor just waits for the gap to fill.
        let settled: Vec<u64> = if self.in_retry { resolved.clone() } else { Vec::new() };
        for seq in resolved.into_iter().filter(|_| !self.in_legacy_retry) {
            if retry_at.is_none_or(|r| seq < r) {
                self.last_fetched_seq = self.last_fetched_seq.max(seq);
            }
        }
        // Carry the hold to the next page of this walk; a walk that reached the
        // head (or a direct call) drops it — the next walk starts at the cursor.
        self.walk_hold = if self.behind { retry_at } else { None };

        // Persist the durable dedup markers, and the cursor iff it advanced —
        // this is what lets a restart resume from here instead of seq 0. The
        // retry semantics above are unchanged: the cursor still sits before the
        // lowest not-yet-openable body, so that value is what gets persisted.
        self.persist_pending(store)?;
        if self.last_fetched_seq != cursor_before {
            store.set_sync_cursor(&self.sync_key(), self.last_fetched_seq)?;
        }
        // A replay of parked bodies: forget the ones that resolved.
        for seq in settled.into_iter().filter(|s| !parked_now.contains(s)) {
            if self.in_legacy_retry {
                store.legacy_unpark(&self.room, seq)?;
            } else {
                store.unpark(&self.sync_key(), seq)?;
            }
        }
        applied += self.retry_parked(store)?;
        applied += self.retry_held(store)?;
        Ok(applied)
    }

    /// The workspace a batch's held events are filed under: the room's own
    /// workspace (every in-scope event carries it), or for an unscoped test
    /// engine the batch's.
    fn room_workspace_scope(&self, decoded: &[SessionEventEnvelope]) -> Uuid {
        #[cfg(test)]
        if self.any_workspace {
            if let Some(e) = decoded.first() {
                return e.workspace_id;
            }
        }
        let _ = decoded;
        self.room_workspace()
    }

    /// Judge `envs` (decoded, in scope) with the ingest gate and ingest the
    /// accepted ones: the one path fetched and held events go through, so an
    /// event applied from the held table is judged exactly as if it had
    /// arrived then.
    ///
    /// `envs[held_from..]` come from the held table: one that arrived over P2P
    /// (unsealed) isn't marked as on the relay when applied, so this device
    /// still pushes it.
    fn judge(&mut self, store: &mut EventStore, envs: &[SessionEventEnvelope], held_from: usize) -> Result<Judged, SyncError> {
        // Verify-on-ingest (S1): build the trust roster from what we already hold
        // plus the trust events in this batch, then classify each envelope.
        // Reject only the *provably bad* (bad signature, revoked device,
        // impersonation); hold what can't be judged yet (a device whose cert we
        // haven't seen for the account it claims, a disputed account key) in the
        // held table until the roster can judge it — never dropped, so two
        // devices that see the same events in a different order converge.
        // P0.4: one roster per workspace, from its own stored trust events plus
        // this batch's trust events only once they verify (batch fixpoint).
        let batch: Vec<&SessionEventEnvelope> = envs.iter().collect();
        let rosters = crate::envelope_verifier::rosters_for_batch(store, &batch)?;
        let empty = crate::envelope_verifier::WorkspaceRoster::default();
        // Recover from a poisoned lock instead of panicking the sync task: the
        // identity cache holds only best-effort verification results (rebuilt/
        // re-fetched), so a panic in a prior holder shouldn't crash every future
        // sync pass. This path is network-input-adjacent — fail soft, not hard.
        let identity = self
            .identity_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // The authorized head this device folds now (B1 ingest rule).
        let head = self.head_roster(store)?;
        let mut out = Judged { outcomes: Vec::with_capacity(envs.len()), applied: 0 };
        // Guard signals (device-local, content-free): every rejection, and one
        // accept per signing device per batch.
        let mut accepted_from: std::collections::HashSet<(Uuid, Option<Uuid>)> = std::collections::HashSet::new();
        for (i, env) in envs.iter().enumerate() {
            let from_relay = i < held_from || env.sealed_epoch().is_some();
            let roster = rosters.get(&env.workspace_id).unwrap_or(&empty);
            let base = verdict_for(roster, env);
            // The account this event is authored as — `verdict_for` has already
            // confirmed the signing device is cert-verified for it (else it's
            // quarantined as impersonation), so it's the right account to run the
            // Option-C GitHub identity gate against. Fall back to the device's
            // account for events that carry no stamp (e.g. some trust events).
            let signer_account = env
                .actor_stamp
                .as_ref()
                .and_then(|s| s.actor.account_id)
                .or_else(|| env.signer_device_id.and_then(|d| roster.account_of(d)));
            let account_key = signer_account.and_then(|a| roster.account_signing_key(a));
            let outcome = match gate_ingest(self.identity_mode(), base, signer_account, account_key, &identity) {
                IngestAction::Quarantine(reason) => {
                    tracing::warn!(?reason, event_id = %env.event_id, "quarantined a fetched event");
                    // Kept as a lamport witness when the refusal depends on
                    // what this device's roster holds when it arrives (a
                    // revocation, a removal, a rebound agent key, a certificate
                    // not here yet): another device may have accepted it and
                    // counted from it. The witness is structural (id, lamport,
                    // log head, chain link), inert, and never pushed
                    // (`ligus_core::lamport`). An unsigned event, or one whose
                    // signature fails under the one key its device id derives
                    // from, is refused by every device whatever it holds: no
                    // one counted from it, and a witness of it would let anyone
                    // with the room's key lengthen a log's lamport chain with
                    // junk no one can attribute.
                    if !matches!(reason, crate::envelope_verifier::QuarantineReason::Unsigned | crate::envelope_verifier::QuarantineReason::BadSignature) {
                        store.note_lamport_witness(env)?;
                    }
                    let _ = store.record_signal(env.workspace_id, None, &sync_decision(roster, env, false, &format!("{reason:?}")));
                    Judgement::Refused
                }
                IngestAction::Hold(reason) => {
                    // Can't judge yet: held aside (never dropped), and judged
                    // again when the roster changes, so a device that sees its
                    // certificate later converges to the same applied state.
                    // A lamport witness meanwhile, as a refused event is:
                    // another device may have applied it and counted from it,
                    // and those later events must not wait on it here.
                    tracing::debug!(reason, event_id = %env.event_id, "holding a fetched event until the roster can judge it");
                    store.note_lamport_witness(env)?;
                    Judgement::Held(reason)
                }
                IngestAction::Accept if late_from_outsider(head.as_deref(), env) => {
                    // B1: sealed under an older key by someone who is no
                    // longer (or not yet) a member: a removed member writing
                    // with the key they kept. Refused for good.
                    tracing::warn!(event_id = %env.event_id, "refused an event sealed under an old key by a non-member");
                    // Refused: a lamport witness (see above).
                    store.note_lamport_witness(env)?;
                    let _ = store.record_signal(env.workspace_id, None, &sync_decision(roster, env, false, "stale_epoch_non_member"));
                    Judgement::Refused
                }
                IngestAction::Accept => {
                    if accepted_from.insert((env.workspace_id, env.signer_device_id)) {
                        let _ = store.record_signal(env.workspace_id, None, &sync_decision(roster, env, true, "accepted"));
                    }
                    // A key registration or rotation the roster refused is
                    // stored (inert: the account-key rules refuse it in every
                    // fold, wherever it sorts) and attributed to its signer.
                    if roster.refused_keys().iter().any(|r| r.event_id == env.event_id) {
                        tracing::warn!(event_id = %env.event_id, signer = ?env.signer_device_id, "an account key event its account's lineage didn't sign: refused, attributed to its signer");
                        let _ = store.record_signal(env.workspace_id, None, &sync_decision(roster, env, false, "account_key_refused"));
                    }
                    if from_relay {
                        self.seen.insert(env.event_id);
                        // This event is now on the relay (we received it from there): queue
                        // it for the durable `pushed` set so a restart never bounces it back.
                        self.pending.push(env.event_id);
                    }
                    if store.ingest_sealed(env, env.sealed_epoch())? {
                        out.applied += 1;
                    }
                    Judgement::Applied
                }
            };
            out.outcomes.push(outcome);
        }
        Ok(out)
    }

    /// Judge the held events again (see [`SyncEngine::apply_fetched`]) when
    /// this device's roster may have changed since the last look (a trust or
    /// config event stored, here or fetched) or [`crate::event_store::HELD_RETRY_EVERY`]
    /// has passed; drops held events older than 30 days. Returns how many
    /// were newly applied.
    pub fn retry_held(&mut self, store: &mut EventStore) -> Result<usize, SyncError> {
        self.retry_held_inner(store, false)
    }

    /// [`SyncEngine::retry_held`] now, whatever changed (`ligus doctor`,
    /// tests).
    pub fn retry_held_now(&mut self, store: &mut EventStore) -> Result<usize, SyncError> {
        self.retry_held_inner(store, true)
    }

    fn retry_held_inner(&mut self, store: &mut EventStore, force: bool) -> Result<usize, SyncError> {
        if self.in_retry || self.in_held_retry {
            return Ok(0);
        }
        let now = std::time::Instant::now();
        let timer = force || self.held_checked.is_none_or(|t| now.duration_since(t) >= crate::event_store::HELD_RETRY_EVERY);
        if timer {
            self.held_checked = Some(now);
            let n = store.expire_held(now_ms())?;
            if n > 0 {
                tracing::warn!(room = %self.room, expired = n, "dropped held events that waited longer than 30 days");
            }
        }
        let workspaces: Vec<Uuid> = store.held_workspaces()?.into_iter().filter(|w| self.in_scope(*w)).collect();
        self.in_held_retry = true;
        let r = self.rejudge_held(store, &workspaces, timer);
        self.in_held_retry = false;
        let applied = r?;
        self.persist_pending(store)?;
        if applied > 0 {
            self.refresh_keyring(store)?;
        }
        Ok(applied)
    }

    /// Judge the held events of `workspaces` again (those whose trust rows
    /// changed since the last look, or all of them when `all`).
    fn rejudge_held(&mut self, store: &mut EventStore, workspaces: &[Uuid], all: bool) -> Result<usize, SyncError> {
        let mut applied = 0;
        for &ws in workspaces {
            let sig = store.trust_rows_sig(ws)?;
            if !all && self.held_sig.get(&ws) == Some(&sig) {
                continue;
            }
            // Releasing one event can let another through (a held
            // certificate, then that device's events): again while something
            // is released, a bounded number of times.
            for _ in 0..8 {
                let held = store.held_events(ws)?;
                if held.is_empty() {
                    break;
                }
                let envs: Vec<SessionEventEnvelope> = held.iter().map(|h| h.env.clone()).collect();
                let judged = self.judge(store, &envs, 0)?;
                let release: Vec<String> = held
                    .iter()
                    .zip(&judged.outcomes)
                    .filter(|(_, o)| !matches!(o, Judgement::Held(_)))
                    .map(|(h, _)| h.digest.clone())
                    .collect();
                store.release_held(ws, &release)?;
                applied += judged.applied;
                if release.is_empty() {
                    break;
                }
            }
            self.held_sig.insert(ws, store.trust_rows_sig(ws)?);
            self.mark_refetch(store, ws)?;
        }
        Ok(applied)
    }

    /// What this room's workspace holds (sync diagnostics).
    pub fn held_stats(&self, store: &EventStore) -> Result<crate::event_store::HeldStats, SyncError> {
        Ok(store.held_stats(self.room_workspace())?)
    }

    /// Replay parked bodies when the keyring changed since the last replay
    /// (A-13). Returns how many were newly applied.
    pub fn retry_parked(&mut self, store: &mut EventStore) -> Result<usize, SyncError> {
        if self.key_source.is_none() || self.in_retry {
            return Ok(0);
        }
        let fp = self.keyring.fingerprint();
        if self.parked_ring.as_ref() == Some(&fp) {
            return Ok(0);
        }
        self.parked_ring = Some(fp);
        let key = self.sync_key();
        let parked = store.parked_bodies(&key)?;
        let legacy = store.legacy_parked_bodies(&self.room)?;
        if parked.is_empty() && legacy.is_empty() {
            return Ok(0);
        }
        // B-11: re-rank what is parked against the new ring, so the cap
        // evicts junk before a body waiting for an authorized rotation.
        let ranks: Vec<(u64, u8)> = parked.iter().map(|(seq, body)| (*seq, self.park_priority(body))).collect();
        store.set_parked_priorities(&key, &ranks)?;
        let (hold, behind) = (self.walk_hold, self.behind);
        self.in_retry = true;
        self.behind = false;
        let mut r = if parked.is_empty() { Ok(0) } else { self.apply_fetched(store, &parked) };
        if r.is_ok() && !legacy.is_empty() {
            self.in_legacy_retry = true;
            let more = self.apply_fetched(store, &legacy);
            self.in_legacy_retry = false;
            r = r.and_then(|n| more.map(|m| n + m));
        }
        self.in_retry = false;
        (self.walk_hold, self.behind) = (hold, behind);
        r
    }

    /// How much a parked body is worth keeping (see
    /// [`KeyringEngine::park_priority`]).
    fn park_priority(&self, body: &Value) -> u8 {
        serde_json::from_value::<SealedEnvelope>(body.clone())
            .map(|s| self.keyring.park_priority(s.version, s.kid.as_deref()))
            .unwrap_or(0)
    }

    /// The current fetch cursor (highest relay sequence durably applied).
    pub fn cursor(&self) -> u64 {
        self.last_fetched_seq
    }

    /// Test seam: replace the ring with only `key` at epoch 0.
    #[cfg(test)]
    pub fn set_key(&mut self, key: [u8; 32]) {
        self.keyring = KeyringEngine::passphrase(self.room_workspace(), key);
    }

    /// Test seam: hold `key` for `version` and seal under it when it is the
    /// highest held (the pre-B1 unsigned behaviour, for transport tests that
    /// predate signed rotations).
    #[cfg(test)]
    pub fn add_key(&mut self, version: u32, key: [u8; 32]) {
        self.keyring.force_key(version, key);
    }
}

impl SyncEngine {
    /// The roster at the head of this room's config log, when this device
    /// follows rotations (a key source) and the log has synced.
    fn head_roster(&self, store: &EventStore) -> Result<Option<Box<ligus_core::authz::RosterState>>, SyncError> {
        if self.key_source.is_none() {
            return Ok(None);
        }
        let ws = self.room_workspace();
        let Some(pin) = store.workspace_pin(ws)?.map(|p| p.founder_account.to_string()) else { return Ok(None) };
        let (config, timeline) = store.load_config(ws, Some(&pin), None)?;
        Ok(config.map(|_| Box::new(timeline.current().clone())))
    }
}

/// B1 ingest rule: a human-stamped event that arrives sealed under an epoch
/// older than the authorized head, from an account that isn't a member at the
/// head. A removed member keeps their old keys but can't write with them once
/// this device knows of the removal and the rotation.
pub(crate) fn late_from_outsider(head: Option<&ligus_core::authz::RosterState>, env: &SessionEventEnvelope) -> bool {
    let (Some(head), Some(sealed)) = (head, env.sealed_epoch()) else { return false };
    if sealed >= head.key_epochs.head_version() {
        return false;
    }
    let Some(account) = env.actor_stamp.as_ref().and_then(|s| s.actor.account_id) else { return false };
    !head.members.values().any(|m| m.account_id == Some(account))
}

/// A content-free guard signal for one ingest decision: whether the signer is
/// a nestmate (a cert-verified device of this workspace) or a stranger.
fn sync_decision(
    roster: &crate::envelope_verifier::WorkspaceRoster,
    env: &SessionEventEnvelope,
    accept: bool,
    reason: &str,
) -> ligus_core::signal::GuardSignal {
    use ligus_core::signal::{GuardGate, GuardSignal, GuardSubject, GuardVerdict};
    let subject = match env.signer_device_id {
        Some(d) if roster.is_certified(d) => GuardSubject::Nestmate,
        Some(_) => GuardSubject::Stranger,
        None => GuardSubject::Unknown,
    };
    GuardSignal::GuardDecision {
        gate: GuardGate::SyncEvent,
        verdict: if accept { GuardVerdict::Accept } else { GuardVerdict::Reject },
        subject,
        source: env.signer_device_id.map(|d| d.to_string()).unwrap_or_else(|| "relay".into()),
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sync_decisions_name_the_subject_without_content() {
        use ligus_core::signal::{GuardSignal, GuardSubject, GuardVerdict};
        let roster = crate::envelope_verifier::build_roster(&[]);
        let mut env = SessionEventEnvelope::new(Uuid::new_v4(), Uuid::new_v4(), 1, ligus_core::SessionEvent::MessageAppended {
            message: ligus_core::ChatMessage::new(ligus_core::MessageRole::User, "x", "secret body"),
        });
        env.signer_device_id = Some(Uuid::new_v4());
        let GuardSignal::GuardDecision { verdict, subject, reason, .. } = super::sync_decision(&roster, &env, false, "BadSignature") else {
            panic!("guard decision expected")
        };
        assert_eq!((verdict, subject, reason.as_str()), (GuardVerdict::Reject, GuardSubject::Stranger, "BadSignature"));
        let json = serde_json::to_string(&super::sync_decision(&roster, &env, true, "accepted")).unwrap();
        assert!(!json.contains("secret body"));
        env.signer_device_id = None;
        assert!(serde_json::to_string(&super::sync_decision(&roster, &env, false, "Unsigned")).unwrap().contains("relay"));
    }

    use super::*;
    use ligus_core::seal_symmetric;
    use ligus_core::{ChatMessage, ChatSession, MessageRole, SessionEvent, SessionEventEnvelope};

    async fn spawn_relay() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, crate::test_relay::router()).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn seed_chat(store: &mut EventStore) -> (Uuid, Uuid) {
        let session = ChatSession::new("Shared", Uuid::new_v4(), "anthropic");
        let sid = session.id;
        let wid = session.workspace_id;
        let snap = SessionEventEnvelope::new(
            sid,
            wid,
            1,
            SessionEvent::SessionSnapshot { session: Box::new(session) },
        );
        store.ingest_next(&snap).unwrap();
        (sid, wid)
    }

    /// A chat (snapshot + one message from Alex) stamped with `wid`.
    fn seed_chat_in(store: &mut EventStore, wid: Uuid, body: &str) -> Uuid {
        let session = ChatSession::new("Chat", wid, "anthropic");
        let sid = session.id;
        store
            .ingest_next(&SessionEventEnvelope::new(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(session) }))
            .unwrap();
        store
            .ingest_next(&SessionEventEnvelope::new(
                sid,
                wid,
                2,
                SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "Alex", body) },
            ))
            .unwrap();
        sid
    }

    /// Regression: a device's store holds every workspace it has (its local
    /// workspace, each room it joined), but a room's sync only ever sends that
    /// room's events. The old push scan sent the whole store to whichever room
    /// was connected.
    #[tokio::test]
    async fn push_sends_only_the_rooms_own_workspace() {
        let base = spawn_relay().await;
        let room = "room-alpha";
        let room_ws = ligus_core::room_workspace_id(room);
        let key = ligus_core::derive_workspace_key("alpha secret");

        let mut store = EventStore::open_in_memory().unwrap();
        seed_chat_in(&mut store, room_ws, "team message");
        seed_chat_in(&mut store, Uuid::new_v4(), "PRIVATE local-only note");
        seed_chat_in(&mut store, ligus_core::room_workspace_id("room-beta"), "PRIVATE other-room message");

        let mut eng = SyncEngine::new(RelayClient::new(&base), room).with_key(key);
        assert_eq!(eng.push_new(&store).await.unwrap(), 2, "only the room's snapshot + message");
        // A later round, incremental or full, still never picks the others up.
        assert_eq!(eng.push_new(&store).await.unwrap(), 0);
        let mut fresh = SyncEngine::new(RelayClient::new(&base), room).with_key(key);
        assert_eq!(fresh.push_new(&store).await.unwrap(), 0);

        let raw = RelayClient::new(&base).fetch_values(room, 0).await.unwrap();
        assert_eq!(raw.len(), 2);
        for (_, body) in &raw {
            let sealed: SealedEnvelope = serde_json::from_value(body.clone()).unwrap();
            let env: SessionEventEnvelope = serde_json::from_slice(&open_symmetric(&key, &sealed).unwrap()).unwrap();
            assert_eq!(env.workspace_id, room_ws);
            assert!(!serde_json::to_string(&env).unwrap().contains("PRIVATE"));
        }
    }

    /// M1d (identity model v2, section 11): two connections name one room
    /// on two relays. A 0.3 device kept one cursor and pushed set for the
    /// room string, so whatever it fetched from relay A moved past events
    /// relay B holds at the same sequences. M1 copies nothing for such a
    /// room: each relay syncs from 0 under its own key, and no event of
    /// either is skipped.
    #[tokio::test]
    async fn m1d_a_room_on_two_relays_resyncs_from_zero_on_both() {
        let (a, b) = (spawn_relay().await, spawn_relay().await);
        let room = "team";
        let ws = ligus_core::room_workspace_id(room);
        let key = ligus_core::derive_workspace_key("team secret");
        let mut writer_a = EventStore::open_in_memory().unwrap();
        let sid_a = seed_chat_in(&mut writer_a, ws, "on relay A");
        let mut writer_b = EventStore::open_in_memory().unwrap();
        let sid_b = seed_chat_in(&mut writer_b, ws, "on relay B");
        assert_eq!(SyncEngine::new(RelayClient::new(&a), room).with_key(key).push_new(&writer_a).await.unwrap(), 2);
        assert_eq!(SyncEngine::new(RelayClient::new(&b), room).with_key(key).push_new(&writer_b).await.unwrap(), 2);

        // The reader synced relay A under 0.3: its room-keyed cursor is at 2.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ligus.db");
        drop(EventStore::open(&db).unwrap());
        let old = rusqlite::Connection::open(&db).unwrap();
        old.execute_batch("INSERT INTO sync_cursor (workspace, last_fetched_seq) VALUES ('team', 2); PRAGMA user_version = 0;").unwrap();
        drop(old);
        let mut store = EventStore::open(&db).unwrap();
        let conns = vec![(a.clone(), room.to_string()), (b.clone(), room.to_string())];
        let migrated = store.migrate_sync_tables(&conns).unwrap();
        assert!(matches!(&migrated, crate::event_store::SyncMigration::Migrated { copied, .. } if copied.is_empty()), "{migrated:?}");

        let mut on_a = SyncEngine::new(RelayClient::new(&a), room).with_key(key);
        let mut on_b = SyncEngine::new(RelayClient::new(&b), room).with_key(key);
        assert_ne!(on_a.sync_key(), on_b.sync_key());
        assert_eq!(on_b.pull(&mut store).await.unwrap(), 2, "relay B's events, from 0");
        assert_eq!(on_a.pull(&mut store).await.unwrap(), 2, "relay A's events, from 0");
        for (writer, sid) in [(&writer_a, sid_a), (&writer_b, sid_b)] {
            for env in writer.list_in(ws, sid).unwrap() {
                assert!(store.has_event_in(ws, env.event_id).unwrap(), "no event skipped");
            }
        }
        assert_eq!(store.sync_cursor(&on_a.sync_key()).unwrap(), 2);
        assert_eq!(store.sync_cursor(&on_b.sync_key()).unwrap(), 2);
    }

    /// A body an older build parked for a room M1 didn't copy (two relays
    /// named it) is drained from the old table by the old path: it applies,
    /// leaves that table, and never moves this relay's cursor (its sequence
    /// may be the other relay's).
    #[tokio::test]
    async fn legacy_parked_bodies_drain_without_moving_the_cursor() {
        let (relay, other) = (spawn_relay().await, spawn_relay().await);
        let room = "team";
        let ws = ligus_core::room_workspace_id(room);
        let source = || {
            crate::keyring::KeySource::for_device(Uuid::new_v4(), ligus_core::e2ee::KeyAgreementKeypair::generate().unwrap(), Some("team pass"), &[])
        };
        let mut writer = EventStore::open_in_memory().unwrap();
        let sid = seed_chat_in(&mut writer, ws, "parked under 0.3");
        assert_eq!(SyncEngine::new(RelayClient::new(&other), room).with_key_source(source()).push_new(&writer).await.unwrap(), 2);
        let bodies = RelayClient::new(&other).fetch_values(room, 0).await.unwrap();

        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ligus.db");
        drop(EventStore::open(&db).unwrap());
        let old = rusqlite::Connection::open(&db).unwrap();
        for (i, (_, body)) in bodies.iter().enumerate() {
            old.execute(
                "INSERT INTO parked_bodies (workspace, seq, body, priority) VALUES ('team', ?1, ?2, 0)",
                rusqlite::params![900 + i as i64, body.to_string()],
            )
            .unwrap();
        }
        old.execute_batch("PRAGMA user_version = 0;").unwrap();
        drop(old);
        let mut store = EventStore::open(&db).unwrap();
        store.migrate_sync_tables(&[(relay.clone(), room.to_string()), (other.clone(), room.to_string())]).unwrap();
        assert_eq!(store.legacy_parked_bodies(room).unwrap().len(), 2);

        let mut eng = SyncEngine::new(RelayClient::new(&relay), room).with_key_source(source());
        assert_eq!(eng.retry_parked(&mut store).unwrap(), 2);
        for env in writer.list_in(ws, sid).unwrap() {
            assert!(store.has_event_in(ws, env.event_id).unwrap());
        }
        assert!(store.legacy_parked_bodies(room).unwrap().is_empty(), "drained");
        assert_eq!(eng.cursor(), 0, "another relay's sequences never move this cursor");
        assert_eq!(store.sync_cursor(&eng.sync_key()).unwrap(), 0);
    }

    /// Defense in depth: events of another workspace that an older client
    /// pushed into the room are never ingested, and don't wedge the cursor.
    #[tokio::test]
    async fn pull_drops_events_of_another_workspace() {
        let base = spawn_relay().await;
        let room = "room-alpha";
        let room_ws = ligus_core::room_workspace_id(room);
        let key = ligus_core::derive_workspace_key("alpha secret");

        // The pre-fix behavior: an unscoped engine pushes its whole store.
        let mut leaky_store = EventStore::open_in_memory().unwrap();
        let foreign = seed_chat_in(&mut leaky_store, Uuid::new_v4(), "PRIVATE local-only note");
        let ours = seed_chat_in(&mut leaky_store, room_ws, "team message");
        let mut leaky = SyncEngine::unscoped(RelayClient::new(&base), room).with_key(key);
        assert_eq!(leaky.push_new(&leaky_store).await.unwrap(), 4);

        let mut store = EventStore::open_in_memory().unwrap();
        let mut eng = SyncEngine::new(RelayClient::new(&base), room).with_key(key);
        assert_eq!(eng.pull(&mut store).await.unwrap(), 2);
        assert!(store.load_session(foreign).unwrap().is_none(), "foreign chat never ingested");
        assert!(store.load_session(ours).unwrap().is_some());
        assert_eq!(eng.cursor(), 4, "cursor moves past the dropped events");
        assert_eq!(store.list_session_ids().unwrap(), vec![ours]);
    }

    #[tokio::test]
    async fn two_devices_converge_through_the_relay() {
        let base = spawn_relay().await;
        let workspace = Uuid::new_v4().to_string();

        // Device A: a chat with a snapshot + a message.
        let mut store_a = EventStore::open_in_memory().unwrap();
        let (sid, wid) = seed_chat(&mut store_a);
        let msg = SessionEventEnvelope::new(
            sid,
            wid,
            2,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "A", "hello from A"),
            },
        );
        store_a.ingest_next(&msg).unwrap();

        // A pushes; B pulls into an empty store. E2EE is mandatory for relay
        // sync, so both carry the shared workspace key.
        let key = ligus_core::derive_workspace_key("converge room secret");
        let mut sync_a = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        assert_eq!(sync_a.push_new(&store_a).await.unwrap(), 2);

        let mut store_b = EventStore::open_in_memory().unwrap();
        let mut sync_b = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        let applied = sync_b.pull(&mut store_b).await.unwrap();
        assert_eq!(applied, 2);

        // B now projects the same conversation.
        let session_b = store_b.load_session(sid).unwrap().expect("session synced to B");
        assert_eq!(session_b.title, "Shared");
        assert_eq!(session_b.messages.len(), 1);
        assert_eq!(session_b.messages[0].body, "hello from A");

        // Idempotent: a second pull applies nothing new.
        assert_eq!(sync_b.pull(&mut store_b).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn e2ee_sync_keeps_ciphertext_on_the_relay() {
        let base = spawn_relay().await;
        let workspace = Uuid::new_v4().to_string();
        let key = ligus_core::derive_workspace_key("shared room secret");

        let mut store_a = EventStore::open_in_memory().unwrap();
        let (sid, wid) = seed_chat(&mut store_a);
        store_a
            .ingest_next(&SessionEventEnvelope::new(
                sid,
                wid,
                2,
                SessionEvent::MessageAppended {
                    message: ChatMessage::new(MessageRole::User, "A", "TOP SECRET PLAN"),
                },
            ))
            .unwrap();

        let mut sync_a = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        sync_a.push_new(&store_a).await.unwrap();

        // The relay's stored bodies must be ciphertext — no plaintext leak.
        let raw = RelayClient::new(&base)
            .fetch_values(&workspace, 0)
            .await
            .unwrap();
        assert_eq!(raw.len(), 2);
        for (_seq, body) in &raw {
            assert!(body.get("ciphertext").is_some(), "body must be sealed");
            let s = serde_json::to_string(body).unwrap();
            assert!(!s.contains("TOP SECRET PLAN"));
            assert!(!s.contains("messageAppended"));
        }

        // B with the same key converges; the message is readable again.
        let mut store_b = EventStore::open_in_memory().unwrap();
        let mut sync_b = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        assert_eq!(sync_b.pull(&mut store_b).await.unwrap(), 2);
        let session_b = store_b.load_session(sid).unwrap().unwrap();
        assert_eq!(session_b.messages[0].body, "TOP SECRET PLAN");

        // B with the WRONG key can't open anything.
        let mut store_c = EventStore::open_in_memory().unwrap();
        let mut sync_c = SyncEngine::unscoped(RelayClient::new(&base), &workspace)
            .with_key(ligus_core::derive_workspace_key("wrong"));
        assert_eq!(sync_c.pull(&mut store_c).await.unwrap(), 0);
    }

    /// An envelope the relay can never take (larger than its request-body
    /// cap: refused with a plain `413`, or too large to send at all) never
    /// blocks the room's outbox: it is set aside and the events after it go
    /// out. Every round after that is quiet.
    #[tokio::test]
    async fn an_envelope_the_relay_cant_take_never_blocks_the_outbox() {
        let base = spawn_relay().await; // the mock's body cap is 2 MiB
        let key = ligus_core::derive_workspace_key("big room");
        let mut store = EventStore::open_in_memory().unwrap();
        let (sid, wid) = seed_chat(&mut store);
        let msg = |seq: i64, body: String| {
            SessionEventEnvelope::new(sid, wid, seq, SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "Alex", body) })
        };
        // Sent and refused (413), then never sent (past the real relay's cap).
        store.ingest_next(&msg(2, "x".repeat(2_500_000))).unwrap();
        store.ingest_next(&msg(3, "y".repeat(5_000_000))).unwrap();
        store.ingest_next(&msg(4, "after the big ones".into())).unwrap();
        let room = Uuid::new_v4().to_string();
        let mut eng = SyncEngine::unscoped(RelayClient::new(&base), &room).with_key(key);
        for _ in 0..3 {
            let _ = eng.push_new(&store).await;
        }
        let raw = RelayClient::new(&base).fetch_values(&room, 0).await.unwrap();
        let bodies: Vec<String> = raw
            .iter()
            .map(|(_, b)| {
                let sealed: SealedEnvelope = serde_json::from_value(b.clone()).unwrap();
                String::from_utf8(open_symmetric(&key, &sealed).unwrap()).unwrap()
            })
            .collect();
        assert!(bodies.iter().any(|b| b.contains("after the big ones")), "the outbox is blocked behind an envelope the relay refuses");
        assert_eq!(eng.push_new(&store).await.unwrap(), 0, "nothing is retried forever");
    }

    /// The local cap leaves room for sealing: an envelope at it seals to a
    /// body the relay's request cap takes.
    #[test]
    fn an_envelope_at_the_local_cap_seals_within_the_relays() {
        let key = ligus_core::derive_workspace_key("cap");
        let plain = vec![b'a'; crate::event_store::MAX_ENVELOPE_BYTES];
        let sealed = ligus_core::e2ee::seal_symmetric_kid(&key, u32::MAX, &"k".repeat(64), &plain).unwrap();
        assert!(serde_json::to_vec(&sealed).unwrap().len() <= MAX_PUSH_BODY_BYTES);
    }

    #[tokio::test]
    async fn failed_push_rolls_back_so_events_retry() {
        // Relay pointed at a closed port → every push errors mid-batch.
        let mut store = EventStore::open_in_memory().unwrap();
        seed_chat(&mut store); // one event (the snapshot)
        let mut eng = SyncEngine::unscoped(RelayClient::new("http://127.0.0.1:9"), Uuid::new_v4().to_string())
            .with_key(ligus_core::derive_workspace_key("rollback room"));

        let batch = eng.take_unpushed(&store).unwrap();
        assert_eq!(batch.len(), 1);
        // The push fails — the optimistically-marked events must roll back to
        // unseen so the next round re-pushes them instead of dropping the tail.
        assert!(eng.push_envelopes(&batch).await.is_err());
        let retry = eng.take_unpushed(&store).unwrap();
        assert_eq!(retry.len(), 1, "unsent events must be retryable after a failed push");
    }

    /// After the first full scan, rounds scan only new rows — and still find
    /// every new event, while a failed push makes the next round rescan.
    #[tokio::test]
    async fn push_scan_is_incremental_and_rescans_after_a_failure() {
        let base = spawn_relay().await;
        let mut store = EventStore::open_in_memory().unwrap();
        let (sid, wid) = seed_chat(&mut store);
        let mut eng = SyncEngine::unscoped(RelayClient::new(&base), Uuid::new_v4().to_string())
            .with_key(ligus_core::derive_workspace_key("incremental room"));
        let first = eng.take_unpushed(&store).unwrap();
        assert_eq!(first.len(), 1);
        eng.push_envelopes(&first).await.unwrap();
        assert!(eng.take_unpushed(&store).unwrap().is_empty());

        let msg = |body: &str| SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "Mara", body) };
        let e = store.append(sid, wid, msg("hi")).unwrap();
        let batch = eng.take_unpushed(&store).unwrap();
        assert_eq!(batch.iter().map(|e| e.event_id).collect::<Vec<_>>(), [e.event_id]);

        // That push fails (relay gone): the next round finds it again.
        let mut offline = SyncEngine::unscoped(RelayClient::new("http://127.0.0.1:9"), Uuid::new_v4().to_string())
            .with_key(ligus_core::derive_workspace_key("incremental room"));
        std::mem::swap(&mut eng.relay, &mut offline.relay);
        assert!(eng.push_envelopes(&batch).await.is_err());
        let retry = eng.take_unpushed(&store).unwrap();
        assert_eq!(retry.iter().map(|e| e.event_id).collect::<Vec<_>>(), [e.event_id]);
    }

    #[tokio::test]
    async fn keyless_relay_push_is_refused_no_plaintext() {
        // B4: a relay-targeted engine with NO key must REFUSE to push rather
        // than send plaintext. Pointed at a LIVE, reachable relay to prove the
        // refusal isn't merely a network failure: the distinct
        // `RelayRequiresEncryption` error is produced and NOTHING is written to
        // the relay. The events roll back to unpushed so a later keyed round
        // re-sends them.
        let base = spawn_relay().await;
        let workspace = Uuid::new_v4().to_string();
        let mut store = EventStore::open_in_memory().unwrap();
        seed_chat(&mut store); // one event (the snapshot)

        let mut eng = SyncEngine::unscoped(RelayClient::new(&base), &workspace);
        assert!(!eng.is_encrypted(), "no key configured");

        let batch = eng.take_unpushed(&store).unwrap();
        assert_eq!(batch.len(), 1);
        let err = eng.push_envelopes(&batch).await.unwrap_err();
        assert!(
            matches!(err, SyncError::RelayRequiresEncryption),
            "keyless push must refuse with RelayRequiresEncryption, got {err:?}"
        );
        // No plaintext value was produced or sent: the relay holds nothing.
        assert!(
            RelayClient::new(&base).fetch_values(&workspace, 0).await.unwrap().is_empty(),
            "a refused keyless push must not write anything to the relay"
        );
        // Rolled back → retryable once a key is configured (not silently dropped).
        let retry = eng.take_unpushed(&store).unwrap();
        assert_eq!(
            retry.len(),
            1,
            "refused envelopes must remain unpushed for a later keyed round"
        );

        // With a key, those same events now seal and land on the relay as
        // ciphertext (a keyed workspace still syncs).
        eng.set_key(ligus_core::derive_workspace_key("now keyed"));
        assert!(eng.is_encrypted());
        eng.push_envelopes(&retry).await.unwrap();
        let raw = RelayClient::new(&base).fetch_values(&workspace, 0).await.unwrap();
        assert_eq!(raw.len(), 1);
        assert!(
            raw[0].1.get("ciphertext").is_some(),
            "a keyed push must seal — the relay must only ever see ciphertext"
        );
    }

    #[tokio::test]
    async fn not_yet_openable_events_do_not_advance_the_cursor() {
        let base = spawn_relay().await;
        let workspace = Uuid::new_v4().to_string();
        // A seals under an epoch (1) that B does not hold yet.
        let k1 = ligus_core::e2ee::generate_workspace_key().unwrap();

        // A publishes two sealed events under epoch 1.
        let mut store_a = EventStore::open_in_memory().unwrap();
        let (sid, wid) = seed_chat(&mut store_a);
        store_a
            .ingest_next(&SessionEventEnvelope::new(
                sid,
                wid,
                2,
                SessionEvent::MessageAppended {
                    message: ChatMessage::new(MessageRole::User, "A", "hi"),
                },
            ))
            .unwrap();
        let mut sync_a = SyncEngine::unscoped(RelayClient::new(&base), &workspace);
        sync_a.add_key(1, k1);
        sync_a.push_new(&store_a).await.unwrap();

        // B holds NO key for epoch 1: the bodies are well-formed but not-yet-
        // openable, so it applies nothing AND does NOT advance its cursor past
        // them (they must be retried once the key arrives).
        let mut store_b = EventStore::open_in_memory().unwrap();
        let mut sync_b = SyncEngine::unscoped(RelayClient::new(&base), &workspace);
        assert_eq!(sync_b.pull(&mut store_b).await.unwrap(), 0);
        assert_eq!(sync_b.cursor(), 0, "not-yet-openable events must not be skipped");

        // The epoch-1 key reaches B (e.g. a rotation catches up). The retained
        // events are re-fetched and now converge — not lost.
        sync_b.add_key(1, k1);
        assert_eq!(sync_b.pull(&mut store_b).await.unwrap(), 2);
        assert_eq!(store_b.load_session(sid).unwrap().unwrap().messages.len(), 1);
    }

    /// A keyed (E2EE) workspace never accepts plaintext from the relay: the
    /// relay sees only ciphertext, so a plaintext body is relay-authored and
    /// must not be applied.
    #[test]
    fn keyed_engine_never_applies_relay_supplied_plaintext() {
        let session = ChatSession::new("Injected", Uuid::new_v4(), "anthropic");
        let (sid, wid) = (session.id, session.workspace_id);
        let snap = SessionEventEnvelope::new(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(session) });
        let msg = SessionEventEnvelope::new(
            sid,
            wid,
            2,
            SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "relay", "injected") },
        );
        let batch: Vec<(u64, Value)> =
            vec![(1, serde_json::to_value(&snap).unwrap()), (2, serde_json::to_value(&msg).unwrap())];
        let mut store = EventStore::open_in_memory().unwrap();
        let mut eng = SyncEngine::unscoped(RelayClient::new("http://127.0.0.1:0"), Uuid::new_v4().to_string());
        eng.set_key(ligus_core::derive_workspace_key("keyed"));
        assert_eq!(eng.apply_fetched(&mut store, &batch).unwrap(), 0, "plaintext applied in a keyed workspace");
        assert!(!store.has_event(snap.event_id).unwrap() && !store.has_event(msg.event_id).unwrap());
        assert_eq!(eng.cursor(), 2, "skipped for good, not retried");
    }

    /// A 0.1 build receives a signed envelope a newer "0.2 encoder" made (an
    /// extra top-level field, a new event kind with nested data): it verifies,
    /// is stored byte for byte, and goes back out byte for byte on re-push.
    #[test]
    fn a_newer_builds_envelope_is_stored_and_re_pushed_byte_identically() {
        let kp = ligus_core::SigningKeypair::from_seed(&[11u8; 32]).unwrap();
        let (sid, wid) = (Uuid::new_v4(), Uuid::new_v4());
        let base = SessionEventEnvelope::new(sid, wid, 1, SessionEvent::MessageRemoved { message_id: Uuid::nil() });
        let mut v = serde_json::to_value(&base).unwrap();
        v["payload"] = serde_json::json!({"kind": "zz21Kind", "nested": {"b": [1, 2.5, "✓"], "a": null}});
        v["zz21Field"] = serde_json::json!({"z": 1, "y": "two"});
        let mut env: SessionEventEnvelope = serde_json::from_str(&v.to_string()).unwrap();
        ligus_core::sign_envelope(&mut env, Uuid::from_u128(3), &kp);
        // The 2.1 encoder's exact bytes, with its own spacing.
        let wire = serde_json::to_string(&env).unwrap().replacen("\"zz21Field\":", "\"zz21Field\" : ", 1);

        let key = ligus_core::derive_workspace_key("newer build");
        let sealed = ligus_core::seal_symmetric(&key, 0, wire.as_bytes()).unwrap();
        let mut store = EventStore::open_in_memory().unwrap();
        let mut eng = SyncEngine::unscoped(RelayClient::new("http://127.0.0.1:0"), Uuid::new_v4().to_string()).with_key(key);
        assert_eq!(eng.apply_fetched(&mut store, &[(1, serde_json::to_value(&sealed).unwrap())]).unwrap(), 1);

        let stored = store.list(sid).unwrap().remove(0);
        assert_eq!(stored.payload, SessionEvent::Unknown, "this build doesn't know the kind");
        assert!(ligus_core::verify_envelope(&stored, &kp.public_key_bytes()).is_ok());
        assert_eq!(serde_json::to_string(&stored).unwrap(), wire, "stored byte for byte");
        // Re-push: what the relay gets opens to the very same bytes.
        let out: ligus_core::SealedEnvelope = serde_json::from_value(eng.encode(&stored).unwrap()).unwrap();
        assert_eq!(ligus_core::open_symmetric(&key, &out).unwrap(), wire.as_bytes());
    }

    #[test]
    fn malformed_body_is_skipped_and_cursor_advances_past_it() {
        // A garbage `{"ciphertext":1}` (hostile or corrupt) sitting between two
        // good plaintext events must not halt the stream: the good events apply
        // and the cursor advances past the bad one, so it's never re-fetched.
        let session = ChatSession::new("Shared", Uuid::new_v4(), "anthropic");
        let sid = session.id;
        let wid = session.workspace_id;
        let snap = SessionEventEnvelope::new(
            sid,
            wid,
            1,
            SessionEvent::SessionSnapshot { session: Box::new(session) },
        );
        let msg = SessionEventEnvelope::new(
            sid,
            wid,
            2,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "A", "still delivered"),
            },
        );
        let snap = crate::event_store::fixture_signed(&snap);
        let msg = crate::event_store::fixture_signed(&msg);
        let batch: Vec<(u64, Value)> = vec![
            (1, serde_json::to_value(&snap).unwrap()),
            (2, serde_json::json!({ "ciphertext": 1 })), // provably malformed
            (3, serde_json::to_value(&msg).unwrap()),
        ];

        let mut store = EventStore::open_in_memory().unwrap();
        let mut eng =
            SyncEngine::unscoped(RelayClient::new("http://127.0.0.1:0"), Uuid::new_v4().to_string());
        assert_eq!(eng.apply_fetched(&mut store, &batch).unwrap(), 2);
        assert!(store.has_event(snap.event_id).unwrap());
        assert!(store.has_event(msg.event_id).unwrap(), "event after the bad body applies");
        assert_eq!(eng.cursor(), 3, "cursor advances past the malformed body");
        // No infinite re-fetch: re-applying the same batch does nothing new.
        assert_eq!(eng.apply_fetched(&mut store, &batch).unwrap(), 0);
        assert_eq!(eng.cursor(), 3);
    }

    #[test]
    fn missing_epoch_key_does_not_strand_later_openable_events() {
        // A body sealed under an epoch the engine LACKS, followed by an event it
        // CAN open: the openable event must still apply (not be stranded), the
        // cursor must stay before the not-yet-openable body (retry point), and
        // the behavior must be deterministic across repeated passes.
        let held = ligus_core::derive_workspace_key("held epoch 0");
        let missing = ligus_core::e2ee::generate_workspace_key().unwrap(); // epoch 7, unheld
        let mut eng = SyncEngine::unscoped(RelayClient::new("http://127.0.0.1:0"), Uuid::new_v4().to_string())
            .with_key(held);

        let session = ChatSession::new("Shared", Uuid::new_v4(), "anthropic");
        let sid = session.id;
        let wid = session.workspace_id;
        let snap = SessionEventEnvelope::new(
            sid,
            wid,
            1,
            SessionEvent::SessionSnapshot { session: Box::new(session) },
        );
        let msg = SessionEventEnvelope::new(
            sid,
            wid,
            2,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "A", "reachable"),
            },
        );
        let snap = crate::event_store::fixture_signed(&snap);
        let msg = crate::event_store::fixture_signed(&msg);
        let seal = |k: &[u8; 32], ver: u32, e: &SessionEventEnvelope| -> Value {
            let plain = serde_json::to_vec(e).unwrap();
            serde_json::to_value(seal_symmetric(k, ver, &plain).unwrap()).unwrap()
        };
        let batch: Vec<(u64, Value)> = vec![
            (1, seal(&missing, 7, &snap)), // not-yet-openable (epoch 7 unheld)
            (2, seal(&held, 0, &msg)),     // openable
        ];

        let mut store = EventStore::open_in_memory().unwrap();
        assert_eq!(eng.apply_fetched(&mut store, &batch).unwrap(), 1);
        assert!(store.has_event(msg.event_id).unwrap(), "later openable event is not stranded");
        assert!(!store.has_event(snap.event_id).unwrap(), "not-yet-openable body is not applied");
        assert_eq!(eng.cursor(), 0, "cursor stays before the not-yet-openable seq (retry point)");

        // Deterministic: another pass over the same batch changes nothing.
        assert_eq!(eng.apply_fetched(&mut store, &batch).unwrap(), 0);
        assert_eq!(eng.cursor(), 0);

        // Once the missing epoch key arrives, the retained body converges too and
        // the cursor jumps forward.
        eng.add_key(7, missing);
        assert_eq!(eng.apply_fetched(&mut store, &batch).unwrap(), 1);
        assert!(store.has_event(snap.event_id).unwrap());
        assert_eq!(eng.cursor(), 2);
    }

    /// The config log's founding seed naming `member` as its sole Owner,
    /// signed and stamped by the member's device (how a workspace begins).
    fn founding_seed(
        member: &ligus_core::WorkspaceMember,
        device_id: Uuid,
        device: &ligus_core::crypto::SigningKeypair,
        lamport: i64,
    ) -> SessionEventEnvelope {
        let ws = Uuid::nil();
        let mut seed = ligus_core::ChatSession::new("", ws, "");
        seed.id = ligus_core::workspace_config_session_id(ws);
        seed.creator_actor_id = member.id.clone();
        seed.members = vec![member.clone()];
        let mut e = SessionEventEnvelope::new(seed.id, ws, lamport, SessionEvent::SessionSnapshot { session: Box::new(seed) });
        e.lamport = lamport as u64;
        e.actor_stamp = Some(ligus_core::ActorStamp {
            actor: member.actor.clone(),
            recorded_at: ligus_core::Timestamp::epoch(),
            cfg_frontier: None,
            presence: None, runtime_effect: false, prev: None, sig_alg: Default::default(), log_head: None });
        ligus_core::sign_envelope(&mut e, device_id, device);
        e
    }

    #[test]
    fn apply_fetched_rejects_provably_bad_keeps_good() {
        use ligus_core::crypto::{DeviceCertificate, SigningKeypair};
        use ligus_core::identity::{ActorIdentity, ActorKind, ActorStamp, WorkspaceMember, WorkspaceRole};
        use ligus_core::Timestamp;

        let account = SigningKeypair::generate().unwrap();
        let account_id = Uuid::new_v4();
        let device = SigningKeypair::generate().unwrap();
        let device_id = ligus_core::crypto::device_id_for(ligus_core::crypto::SigAlg::Ed25519, &device.public_key_bytes());
        let cert = DeviceCertificate::issue(&account, account_id, &device.public_key_bytes(), Timestamp::epoch());
        let member = WorkspaceMember {
            id: account_id.to_string(),
            actor: ActorIdentity {
                id: account_id.to_string(),
                display_name: "A".into(),
                kind: ActorKind::Human,
                account_id: Some(account_id),
                device_id: Some(device_id),
                git_email: None,
                key_agreement_public: None,
                avatar_url: None,
                contact: None,
            },
            role: WorkspaceRole::Owner,
            title: String::new(),
            index: 1,
            joined_at: Timestamp::epoch(),
        };

        // A content event signed by `device`, stamping `claim` as the author.
        let content = |lamport: i64, claim: Uuid| -> SessionEventEnvelope {
            let mut e = SessionEventEnvelope::new(
                Uuid::nil(),
                Uuid::nil(),
                lamport,
                SessionEvent::SessionTitleChanged { title: format!("t{lamport}") },
            );
            e.actor_stamp = Some(ActorStamp {
                actor: ActorIdentity {
                    id: claim.to_string(),
                    display_name: "A".into(),
                    kind: ActorKind::Human,
                    account_id: Some(claim),
                    device_id: Some(device_id),
                    git_email: None,
                    key_agreement_public: None,
                    avatar_url: None,
                    contact: None,
                },
                recorded_at: Timestamp::epoch(), cfg_frontier: None,
                presence: None, runtime_effect: false, prev: None, sig_alg: Default::default(), log_head: None });
            ligus_core::sign_envelope(&mut e, device_id, &device);
            e
        };

        let good = content(100, account_id);
        let mut tampered = content(101, account_id);
        tampered.sequence = 9999; // breaks the signature
        let spoof = content(102, Uuid::new_v4()); // stamps a different account

        let plain = |lamport: i64, payload: SessionEvent| {
            SessionEventEnvelope::new(Uuid::nil(), Uuid::nil(), lamport, payload)
        };
        let all: Vec<SessionEventEnvelope> = vec![
            founding_seed(&member, device_id, &device, 4),
            {
                // Signed and stamped by the account's own device, as
                // `publish_identity` does.
                let mut e = plain(2, SessionEvent::AccountKeyRegistered {
                    account_id,
                    signing_public_key: account.public_key_bytes().to_vec(),
                });
                e.actor_stamp = Some(ActorStamp { actor: member.actor.clone(), recorded_at: Timestamp::epoch(), cfg_frontier: None, presence: None, runtime_effect: false, prev: None, sig_alg: Default::default(), log_head: None });
                ligus_core::sign_envelope(&mut e, device_id, &device);
                e
            },
            {
                // The device publishes its own certificate, as
                // `publish_identity` does.
                let mut e = plain(3, SessionEvent::DeviceCertificateAdded { certificate: cert });
                e.actor_stamp = Some(ActorStamp { actor: member.actor.clone(), recorded_at: Timestamp::epoch(), cfg_frontier: None, presence: None, runtime_effect: false, prev: None, sig_alg: Default::default(), log_head: None });
                ligus_core::sign_envelope(&mut e, device_id, &device);
                e
            },
            good.clone(),
            tampered.clone(),
            spoof.clone(),
        ];
        let batch: Vec<(u64, Value)> = all
            .iter()
            .enumerate()
            .map(|(i, e)| (i as u64 + 1, serde_json::to_value(e).unwrap()))
            .collect();

        let mut store = EventStore::open_in_memory().unwrap();
        let mut eng = SyncEngine::unscoped(RelayClient::new("http://127.0.0.1:0"), Uuid::new_v4().to_string());
        eng.apply_fetched(&mut store, &batch).unwrap();

        assert!(store.has_event(good.event_id).unwrap(), "valid signed event ingested");
        assert!(!store.has_event(tampered.event_id).unwrap(), "tampered event rejected");
        assert!(!store.has_event(spoof.event_id).unwrap(), "impersonating event rejected");
        // A refusal no roster can change (a signature that doesn't verify
        // under the one key its device id derives from, or none at all) is
        // no lamport witness: no device ever accepted it or counted from it,
        // and a witness would let anyone holding the room's key extend a
        // log's lamport chain with junk nobody can attribute. The
        // impersonation (roster-dependent: another device may hold the
        // certificate that makes it valid) stays one.
        let witnesses = store.witness_ids(Uuid::nil()).unwrap();
        assert!(!witnesses.contains(&tampered.event_id), "a bad signature became a lamport witness");
        assert!(witnesses.contains(&spoof.event_id), "a roster-dependent refusal stays a witness");
        let mut unsigned = content(103, account_id);
        unsigned.signature = None;
        unsigned.signer_device_id = None;
        eng.apply_fetched(&mut store, &[(100, serde_json::to_value(&unsigned).unwrap())]).unwrap();
        assert!(!store.witness_ids(Uuid::nil()).unwrap().contains(&unsigned.event_id), "an unsigned event became a lamport witness");
    }

    #[tokio::test]
    async fn rotated_epochs_do_not_strand_history() {
        let base = spawn_relay().await;
        let workspace = Uuid::new_v4().to_string();
        let k0 = ligus_core::derive_workspace_key("epoch 0 base");
        let k1 = ligus_core::e2ee::generate_workspace_key().unwrap();

        // A seals a snapshot under epoch 0, rotates to epoch 1, seals a message.
        let mut store_a = EventStore::open_in_memory().unwrap();
        let (sid, wid) = seed_chat(&mut store_a);
        let mut sync_a = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(k0);
        assert_eq!(sync_a.push_new(&store_a).await.unwrap(), 1); // snapshot @ epoch 0

        store_a
            .ingest_next(&SessionEventEnvelope::new(
                sid,
                wid,
                2,
                SessionEvent::MessageAppended {
                    message: ChatMessage::new(MessageRole::User, "A", "after rotation"),
                },
            ))
            .unwrap();
        sync_a.add_key(1, k1); // epoch 1 is now the highest → new events seal under it
        assert_eq!(sync_a.push_new(&store_a).await.unwrap(), 1); // message @ epoch 1

        // B holds only epoch 0: reads the epoch-0 snapshot; the epoch-1 message is
        // undecodable and retained (not permanently skipped — phase 7).
        let mut store_b = EventStore::open_in_memory().unwrap();
        let mut sync_b = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(k0);
        sync_b.pull(&mut store_b).await.unwrap();
        let sb = store_b.load_session(sid).unwrap().expect("snapshot synced");
        assert_eq!(sb.title, "Shared", "epoch-0 content is readable");
        assert_eq!(sb.messages.len(), 0, "epoch-1 message not yet readable");

        // Epoch 1 reaches B. It reads the message AND still reads epoch-0 history —
        // both keys are retained, so the rotation stranded nothing.
        sync_b.add_key(1, k1);
        sync_b.pull(&mut store_b).await.unwrap();
        let sb = store_b.load_session(sid).unwrap().unwrap();
        assert_eq!(sb.title, "Shared", "epoch-0 history still readable after rotation");
        assert_eq!(sb.messages.len(), 1, "epoch-1 message now readable");
        assert_eq!(sb.messages[0].body, "after rotation");
    }

    #[tokio::test]
    async fn new_engine_resumes_from_persisted_cursor() {
        // #2: durable cursor. After a pull persists the cursor, a NEW engine on
        // the SAME store must resume from it and fetch nothing new — not
        // re-fetch the whole room from seq 0.
        let base = spawn_relay().await;
        let workspace = Uuid::new_v4().to_string();
        let key = ligus_core::derive_workspace_key("resume room");

        let mut store_a = EventStore::open_in_memory().unwrap();
        let (sid, wid) = seed_chat(&mut store_a);
        store_a
            .ingest_next(&SessionEventEnvelope::new(
                sid,
                wid,
                2,
                SessionEvent::MessageAppended {
                    message: ChatMessage::new(MessageRole::User, "A", "hi"),
                },
            ))
            .unwrap();
        let mut sync_a = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        assert_eq!(sync_a.push_new(&store_a).await.unwrap(), 2);

        // First engine on B pulls both, persisting the cursor into store_b.
        let mut store_b = EventStore::open_in_memory().unwrap();
        let mut sync_b1 = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        assert_eq!(sync_b1.pull(&mut store_b).await.unwrap(), 2);
        let cursor = sync_b1.cursor();
        assert!(cursor > 0, "cursor advanced");
        assert_eq!(store_b.sync_cursor(&sync_b1.sync_key()).unwrap(), cursor, "cursor is durable");
        drop(sync_b1);

        // A brand-new engine (simulating a restart) resumes from the durable
        // cursor: it fetches nothing new and starts at the persisted cursor.
        let mut sync_b2 = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        assert_eq!(sync_b2.pull(&mut store_b).await.unwrap(), 0, "resumes; nothing re-fetched");
        assert_eq!(sync_b2.cursor(), cursor, "cursor restored from the durable store");
    }

    #[tokio::test]
    async fn new_engine_does_not_repush_already_pushed() {
        // #2: durable push-dedup. After pushing, a NEW engine on the same store
        // must NOT re-collect or re-push the already-pushed events.
        let base = spawn_relay().await;
        let workspace = Uuid::new_v4().to_string();
        let key = ligus_core::derive_workspace_key("no-repush room");

        let mut store = EventStore::open_in_memory().unwrap();
        let (sid, wid) = seed_chat(&mut store);
        store
            .ingest_next(&SessionEventEnvelope::new(
                sid,
                wid,
                2,
                SessionEvent::MessageAppended {
                    message: ChatMessage::new(MessageRole::User, "A", "hi"),
                },
            ))
            .unwrap();

        let mut sync1 = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        assert_eq!(sync1.push_new(&store).await.unwrap(), 2);
        let sync_key = sync1.sync_key();
        drop(sync1);
        assert_eq!(store.pushed_ids(&sync_key).unwrap().len(), 2, "pushes are durable");

        // Fresh engine, same store: the durable `pushed` set makes take_unpushed
        // collect nothing, so a restart re-push is a no-op and the relay keeps
        // exactly two bodies (no duplicates).
        let mut sync2 = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        assert!(sync2.take_unpushed(&store).unwrap().is_empty(), "already-pushed not re-collected");
        assert_eq!(sync2.push_new(&store).await.unwrap(), 0, "restart re-push is a no-op");
        let raw = RelayClient::new(&base).fetch_values(&workspace, 0).await.unwrap();
        assert_eq!(raw.len(), 2, "no duplicate bodies on the relay after restart");
    }

    #[test]
    fn durable_cursor_preserves_not_yet_openable_retry() {
        // #2 + resilience: the not-yet-openable retry point must survive a
        // restart. A body sealed under an epoch we lack sits before an openable
        // one; the openable event applies, the cursor is held before the missing
        // body, and BOTH the applied event and the held cursor are durable — so a
        // fresh engine on the same store still retries the missing body (doesn't
        // strand it or skip past it).
        let workspace = Uuid::new_v4().to_string();
        let held = ligus_core::derive_workspace_key("held epoch 0");
        let missing = ligus_core::e2ee::generate_workspace_key().unwrap(); // epoch 7, unheld

        let session = ChatSession::new("Shared", Uuid::new_v4(), "anthropic");
        let sid = session.id;
        let wid = session.workspace_id;
        let snap = SessionEventEnvelope::new(
            sid,
            wid,
            1,
            SessionEvent::SessionSnapshot { session: Box::new(session) },
        );
        let msg = SessionEventEnvelope::new(
            sid,
            wid,
            2,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "A", "reachable"),
            },
        );
        let snap = crate::event_store::fixture_signed(&snap);
        let msg = crate::event_store::fixture_signed(&msg);
        let seal = |k: &[u8; 32], ver: u32, e: &SessionEventEnvelope| -> Value {
            let plain = serde_json::to_vec(e).unwrap();
            serde_json::to_value(seal_symmetric(k, ver, &plain).unwrap()).unwrap()
        };
        let batch: Vec<(u64, Value)> = vec![
            (1, seal(&missing, 7, &snap)), // not-yet-openable (epoch 7 unheld)
            (2, seal(&held, 0, &msg)),     // openable
        ];

        let mut store = EventStore::open_in_memory().unwrap();
        let mut eng1 = SyncEngine::unscoped(RelayClient::new("http://127.0.0.1:0"), &workspace)
            .with_key(held);
        assert_eq!(eng1.apply_fetched(&mut store, &batch).unwrap(), 1);
        assert_eq!(eng1.cursor(), 0, "cursor held before the not-yet-openable seq");
        assert_eq!(store.sync_cursor(&eng1.sync_key()).unwrap(), 0, "durable cursor is the retry point");
        drop(eng1);

        // Fresh engine (restart), STILL without epoch 7: resumes cursor 0, the
        // openable event is already ingested so nothing new applies, and the
        // cursor stays before the missing body.
        let mut eng2 = SyncEngine::unscoped(RelayClient::new("http://127.0.0.1:0"), &workspace)
            .with_key(held);
        assert_eq!(eng2.apply_fetched(&mut store, &batch).unwrap(), 0, "openable already applied");
        assert_eq!(eng2.cursor(), 0, "resumed cursor still before the retry point");

        // Epoch 7 arrives: the retained body converges and the cursor jumps,
        // durably.
        eng2.add_key(7, missing);
        assert_eq!(eng2.apply_fetched(&mut store, &batch).unwrap(), 1);
        assert!(store.has_event(snap.event_id).unwrap());
        assert_eq!(eng2.cursor(), 2);
        assert_eq!(store.sync_cursor(&eng2.sync_key()).unwrap(), 2, "durable cursor advanced past the gap");
    }

    #[tokio::test]
    async fn bidirectional_round_trip() {
        let base = spawn_relay().await;
        let workspace = Uuid::new_v4().to_string();

        let key = ligus_core::derive_workspace_key("round trip room");
        let mut store_a = EventStore::open_in_memory().unwrap();
        let (sid, wid) = seed_chat(&mut store_a);
        let mut sync_a = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        sync_a.sync_once(&mut store_a).await.unwrap();

        // B joins, syncs, then replies.
        let mut store_b = EventStore::open_in_memory().unwrap();
        let mut sync_b = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        sync_b.sync_once(&mut store_b).await.unwrap();
        let reply = SessionEventEnvelope::new(
            sid,
            wid,
            99,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::Assistant, "B", "reply from B"),
            },
        );
        store_b.ingest_next(&reply).unwrap();
        sync_b.sync_once(&mut store_b).await.unwrap();

        // A pulls B's reply.
        sync_a.sync_once(&mut store_a).await.unwrap();
        let session_a = store_a.load_session(sid).unwrap().unwrap();
        assert!(session_a.messages.iter().any(|m| m.body == "reply from B"));
    }

    // --- Option C (github-signing-keys identity mode) ------------------------

    use crate::identity_verifier::{shared_cache, IdentityMode, IdentityVerdict, SharedIdentityCache};

    /// A member principal with its trust events (`MemberAdded`, `AccountKeyRegistered`,
    /// `DeviceCertificateAdded`) + an account signing key we can mark verified.
    struct OcPrincipal {
        account_id: Uuid,
        account_key: Vec<u8>,
        device_kp: ligus_core::crypto::SigningKeypair,
        device_id: Uuid,
        trust: Vec<SessionEventEnvelope>,
    }

    fn oc_principal() -> OcPrincipal {
        use ligus_core::crypto::{DeviceCertificate, SigningKeypair};
        use ligus_core::identity::{ActorIdentity, ActorKind, WorkspaceMember, WorkspaceRole};
        use ligus_core::Timestamp;

        let account = SigningKeypair::generate().unwrap();
        let account_id = Uuid::new_v4();
        let device = SigningKeypair::generate().unwrap();
        let device_id = ligus_core::crypto::device_id_for(ligus_core::crypto::SigAlg::Ed25519, &device.public_key_bytes());
        let cert = DeviceCertificate::issue(&account, account_id, &device.public_key_bytes(), Timestamp::epoch());
        let member = WorkspaceMember {
            id: account_id.to_string(),
            actor: ActorIdentity {
                id: account_id.to_string(),
                display_name: "A".into(),
                kind: ActorKind::Human,
                account_id: Some(account_id),
                device_id: Some(device_id),
                git_email: None,
                key_agreement_public: None,
                avatar_url: None,
                contact: None,
            },
            role: WorkspaceRole::Owner,
            title: String::new(),
            index: 1,
            joined_at: Timestamp::epoch(),
        };
        // Trust events are signed by the device (as they are in production), so
        // under Option C they follow the same verified/held path as content.
        let signed = |lamport: i64, payload: SessionEvent| {
            let mut e = SessionEventEnvelope::new(Uuid::nil(), Uuid::nil(), lamport, payload);
            e.actor_stamp = Some(ligus_core::identity::ActorStamp {
                actor: member.actor.clone(),
                recorded_at: Timestamp::epoch(),
                cfg_frontier: None,
                presence: None, runtime_effect: false, prev: None, sig_alg: Default::default(), log_head: None });
            ligus_core::sign_envelope(&mut e, device_id, &device);
            e
        };
        let trust = vec![
            founding_seed(&member, device_id, &device, 4),
            signed(
                2,
                SessionEvent::AccountKeyRegistered {
                    account_id,
                    signing_public_key: account.public_key_bytes().to_vec(),
                },
            ),
            signed(3, SessionEvent::DeviceCertificateAdded { certificate: cert }),
        ];
        OcPrincipal {
            account_id,
            account_key: account.public_key_bytes().to_vec(),
            device_kp: device,
            device_id,
            trust,
        }
    }

    fn oc_content(p: &OcPrincipal, lamport: i64) -> SessionEventEnvelope {
        use ligus_core::identity::{ActorIdentity, ActorKind, ActorStamp};
        use ligus_core::Timestamp;
        let mut e = SessionEventEnvelope::new(
            Uuid::nil(),
            Uuid::nil(),
            lamport,
            SessionEvent::SessionTitleChanged { title: format!("t{lamport}") },
        );
        e.actor_stamp = Some(ActorStamp {
            actor: ActorIdentity {
                id: p.account_id.to_string(),
                display_name: "A".into(),
                kind: ActorKind::Human,
                account_id: Some(p.account_id),
                device_id: Some(p.device_id),
                git_email: None,
                key_agreement_public: None,
                avatar_url: None,
                contact: None,
            },
            recorded_at: Timestamp::epoch(), cfg_frontier: None,
            presence: None, runtime_effect: false, prev: None, sig_alg: Default::default(), log_head: None });
        ligus_core::sign_envelope(&mut e, p.device_id, &p.device_kp);
        e
    }

    fn oc_engine(cache: SharedIdentityCache) -> SyncEngine {
        SyncEngine::unscoped(RelayClient::new("http://127.0.0.1:0"), Uuid::new_v4().to_string())
            .with_identity_mode(IdentityMode::GithubSigningKeys)
            .with_identity_cache(cache)
    }

    fn oc_batch(p: &OcPrincipal, content: &SessionEventEnvelope) -> Vec<(u64, Value)> {
        p.trust
            .iter()
            .chain(std::iter::once(content))
            .enumerate()
            .map(|(i, e)| (i as u64 + 1, serde_json::to_value(e).unwrap()))
            .collect()
    }

    #[test]
    fn option_c_unverified_is_held_not_dropped_then_promoted() {
        let p = oc_principal();
        let content = oc_content(&p, 100);
        let batch = oc_batch(&p, &content);

        let cache = shared_cache();
        let mut eng = oc_engine(Arc::clone(&cache));
        let mut store = EventStore::open_in_memory().unwrap();

        // GitHub not fetched yet: the roster-valid content event is HELD, not
        // dropped. Trust events themselves are unsigned → also held under Option C.
        assert_eq!(eng.apply_fetched(&mut store, &batch).unwrap(), 0);
        assert!(!store.has_event(content.event_id).unwrap(), "unverified content is held, not applied");
        // Held events no longer pin the cursor: they wait in the held table.
        assert_eq!(eng.cursor(), batch.len() as u64, "the cursor moves past held events");
        let held = store.held_events(Uuid::nil()).unwrap();
        assert!(held.iter().any(|h| h.env.event_id == content.event_id), "the content waits in the held table");

        // Deterministic: replaying the same batch changes nothing while unverified.
        assert_eq!(eng.apply_fetched(&mut store, &batch).unwrap(), 0);
        assert!(!store.has_event(content.event_id).unwrap());
        assert_eq!(store.held_events(Uuid::nil()).unwrap().len(), held.len(), "a re-delivery is one held row");

        // GitHub verification completes → the held event is promoted when the
        // held table is judged again (the slow timer, or now).
        cache.lock().unwrap().put(
            p.account_id,
            p.account_key.clone(),
            IdentityVerdict::Verified,
            std::time::Duration::from_secs(600),
        );
        eng.retry_held_now(&mut store).unwrap();
        assert!(store.has_event(content.event_id).unwrap(), "promoted once verified");
        assert!(store.held_events(Uuid::nil()).unwrap().is_empty(), "released from the held table");
    }

    #[test]
    fn option_c_key_not_listed_is_quarantined() {
        let p = oc_principal();
        let content = oc_content(&p, 100);
        let batch = oc_batch(&p, &content);

        let cache = shared_cache();
        // GitHub fetched and the account key is NOT among the login's signing keys.
        cache.lock().unwrap().put(
            p.account_id,
            p.account_key.clone(),
            IdentityVerdict::NotListed,
            std::time::Duration::from_secs(600),
        );
        let mut eng = oc_engine(Arc::clone(&cache));
        let mut store = EventStore::open_in_memory().unwrap();

        eng.apply_fetched(&mut store, &batch).unwrap();
        assert!(!store.has_event(content.event_id).unwrap(), "provably-bad identity is quarantined");
        // Quarantine is stable → the cursor advances past it (re-fetching won't help).
        assert_eq!(eng.cursor() as usize, batch.len(), "cursor advances past a quarantined event");
    }

    #[test]
    fn option_a_default_ignores_github_state_unchanged() {
        // Same batch, same (NotListed) cache — but Relay mode (the default) never
        // consults it: behavior is byte-for-byte the pre-Option-C policy, so the
        // valid signed content event is ingested.
        let p = oc_principal();
        let content = oc_content(&p, 100);
        let batch = oc_batch(&p, &content);

        let cache = shared_cache();
        cache.lock().unwrap().put(
            p.account_id,
            p.account_key.clone(),
            IdentityVerdict::NotListed,
            std::time::Duration::from_secs(600),
        );
        let mut eng = SyncEngine::unscoped(RelayClient::new("http://127.0.0.1:0"), Uuid::new_v4().to_string())
            .with_identity_mode(IdentityMode::Relay)
            .with_identity_cache(cache);
        let mut store = EventStore::open_in_memory().unwrap();

        eng.apply_fetched(&mut store, &batch).unwrap();
        assert!(store.has_event(content.event_id).unwrap(), "Option A ingests regardless of GitHub state");
    }

    #[test]
    fn option_c_devices_at_different_verification_states_converge() {
        // The core convergence property: two devices ingest the SAME batch under
        // Option C but at different GitHub-verification states. The one without
        // verification yet HOLDS the event (doesn't drop it); once it verifies, it
        // reaches the identical applied state. No divergence into applied-vs-dropped.
        let p = oc_principal();
        let content = oc_content(&p, 100);
        let batch = oc_batch(&p, &content);

        // Device A: already GitHub-verified.
        let cache_a = shared_cache();
        cache_a.lock().unwrap().put(
            p.account_id,
            p.account_key.clone(),
            IdentityVerdict::Verified,
            std::time::Duration::from_secs(600),
        );
        let mut eng_a = oc_engine(Arc::clone(&cache_a));
        let mut store_a = EventStore::open_in_memory().unwrap();
        eng_a.apply_fetched(&mut store_a, &batch).unwrap();

        // Device B: no network yet (cache empty) → holds, applies nothing.
        let cache_b = shared_cache();
        let mut eng_b = oc_engine(Arc::clone(&cache_b));
        let mut store_b = EventStore::open_in_memory().unwrap();
        eng_b.apply_fetched(&mut store_b, &batch).unwrap();

        assert!(store_a.has_event(content.event_id).unwrap(), "A applied (verified)");
        assert!(!store_b.has_event(content.event_id).unwrap(), "B held (not yet verified) — NOT dropped");

        // B's GitHub verification catches up (shared authority → same verdict) and
        // it re-fetches the held event → converges to A's applied state.
        cache_b.lock().unwrap().put(
            p.account_id,
            p.account_key.clone(),
            IdentityVerdict::Verified,
            std::time::Duration::from_secs(600),
        );
        eng_b.apply_fetched(&mut store_b, &batch).unwrap();
        assert!(store_b.has_event(content.event_id).unwrap(), "B converges once verified");

        // Both devices now hold the identical event.
        assert_eq!(
            store_a.has_event(content.event_id).unwrap(),
            store_b.has_event(content.event_id).unwrap(),
        );
    }

    #[test]
    fn option_c_content_before_trust_across_pulls_converges_not_wedged() {
        // P2-21 adversarial fetch-split: content arrives in one pull BEFORE its
        // trust events (a hostile/fragmented relay ordering, or trust straddling a
        // cursor boundary). Under Option C the content must be HELD (device
        // unknown) — never dropped — and once a later pull carries the trust it
        // must converge (apply), not wedge behind a permanently-held cursor.
        let p = oc_principal();
        let content = oc_content(&p, 1);
        let cache = shared_cache();
        cache.lock().unwrap().put(
            p.account_id,
            p.account_key.clone(),
            IdentityVerdict::Verified,
            std::time::Duration::from_secs(600),
        );
        let mut eng = oc_engine(Arc::clone(&cache));
        let mut store = EventStore::open_in_memory().unwrap();

        // Pull 1: content alone at seq 1, no trust yet → held (device unknown).
        let pull1 = vec![(1u64, serde_json::to_value(&content).unwrap())];
        eng.apply_fetched(&mut store, &pull1).unwrap();
        assert!(
            !store.has_event(content.event_id).unwrap(),
            "content with no trust must be HELD, not applied and not dropped"
        );

        // Pull 2: re-fetch from the held cursor now carries content + its trust.
        let mut pull2 = pull1.clone();
        for (i, e) in p.trust.iter().enumerate() {
            pull2.push((i as u64 + 2, serde_json::to_value(e).unwrap()));
        }
        eng.apply_fetched(&mut store, &pull2).unwrap();
        assert!(
            store.has_event(content.event_id).unwrap(),
            "content converges once its trust arrives in a later pull — no permanent wedge"
        );
    }

    // --- Paged pull (a room larger than one relay page) ---

    /// A mock relay whose pages hold at most `cap` envelopes.
    async fn spawn_paged_relay(cap: usize) -> (String, crate::test_relay::EnvelopeGets) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (router, gets) = crate::test_relay::router_with_stats(cap);
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (format!("http://{addr}"), gets)
    }

    /// One chat: a snapshot + `n` messages = `n + 1` envelopes.
    fn seed_long_chat(store: &mut EventStore, n: usize) -> Uuid {
        let (sid, wid) = seed_chat(store);
        for i in 0..n {
            store
                .ingest_next(&SessionEventEnvelope::new(
                    sid,
                    wid,
                    2 + i as i64,
                    SessionEvent::MessageAppended {
                        message: ChatMessage::new(MessageRole::User, "A", format!("msg {i}")),
                    },
                ))
                .unwrap();
        }
        sid
    }

    /// A new device joining a room bigger than one relay page gets all of it:
    /// a round is bounded to PULL_PAGES_PER_ROUND pages and says it is behind,
    /// the next round continues, and the durable cursor ends at the head.
    #[tokio::test]
    async fn pull_rounds_page_through_a_room_larger_than_one_page() {
        let (base, gets) = spawn_paged_relay(3).await;
        let workspace = Uuid::new_v4().to_string();
        let key = ligus_core::derive_workspace_key("paged room");

        let mut store_a = EventStore::open_in_memory().unwrap();
        let sid = seed_long_chat(&mut store_a, 40); // 41 envelopes = 14 pages of 3
        let mut a = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        assert_eq!(a.push_new(&store_a).await.unwrap(), 41);

        let mut store_b = EventStore::open_in_memory().unwrap();
        let mut b = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        let before = gets.count();
        let first = b.pull_round(&mut store_b).await.unwrap();
        assert_eq!(first, 3 * PULL_PAGES_PER_ROUND, "one round = a bounded number of pages");
        assert_eq!(gets.count() - before, PULL_PAGES_PER_ROUND);
        assert!(b.is_behind(), "more pages remain");
        assert_eq!(store_b.sync_cursor(&b.sync_key()).unwrap(), first as u64, "progress is durable per page");

        let second = b.pull_round(&mut store_b).await.unwrap();
        assert_eq!(first + second, 41);
        assert!(!b.is_behind(), "caught up");
        assert_eq!(b.cursor(), 41);
        let session = store_b.load_session(sid).unwrap().expect("chat synced");
        assert_eq!(session.messages.len(), 40);

        // Caught up: the next round is a single empty page.
        let before = gets.count();
        assert_eq!(b.pull_round(&mut store_b).await.unwrap(), 0);
        assert_eq!(gets.count() - before, 1);

        // `catch_up` (one-shot callers) gets the whole room in one call.
        let mut store_c = EventStore::open_in_memory().unwrap();
        let mut c = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        assert_eq!(c.catch_up(&mut store_c, 100).await.unwrap(), 41);
        assert!(!c.is_behind());
        assert_eq!(store_c.load_session(sid).unwrap().unwrap().messages.len(), 40);

        // A restarted engine resumes from the durable cursor, not from 0.
        let mut c2 = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        assert_eq!(c2.pull_round(&mut store_c).await.unwrap(), 0);
        assert_eq!(c2.cursor(), 41);
    }

    /// The client's own page size is honored too (smaller than the relay cap).
    #[tokio::test]
    async fn pull_honors_the_client_page_limit() {
        let (base, gets) = spawn_paged_relay(1000).await;
        let workspace = Uuid::new_v4().to_string();
        let key = ligus_core::derive_workspace_key("client limit");
        let mut store_a = EventStore::open_in_memory().unwrap();
        seed_long_chat(&mut store_a, 9); // 10 envelopes
        let mut a = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        a.push_new(&store_a).await.unwrap();

        let mut store_b = EventStore::open_in_memory().unwrap();
        let mut b = SyncEngine::unscoped(RelayClient::new(&base), &workspace)
            .with_key(key)
            .with_page_limit(4);
        let before = gets.count();
        assert_eq!(b.pull_round(&mut store_b).await.unwrap(), 10);
        assert_eq!(gets.count() - before, 3, "4 + 4 + 2");
        assert!(!b.is_behind());
    }

    /// A body that can't be opened yet holds the durable cursor (the retry
    /// point) — paging must still walk past it to the head in the same catch-up,
    /// instead of re-fetching the same first page forever. Once caught up, the
    /// next round re-fetches from the retry point, and the body converges when
    /// its key arrives.
    #[tokio::test]
    async fn paging_walks_past_a_held_body_and_retries_it_later() {
        let (base, _gets) = spawn_paged_relay(2).await;
        let workspace = Uuid::new_v4().to_string();
        let held = ligus_core::derive_workspace_key("held epoch 0");
        let missing = ligus_core::e2ee::generate_workspace_key().unwrap();

        // seq 1: a snapshot sealed under epoch 7 (not held yet).
        let session = ChatSession::new("Shared", Uuid::new_v4(), "anthropic");
        let (sid, wid) = (session.id, session.workspace_id);
        let snap = SessionEventEnvelope::new(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(session) });
        let snap = crate::event_store::fixture_signed(&snap);
        let relay = RelayClient::new(&base);
        let sealed = seal_symmetric(&missing, 7, &serde_json::to_vec(&snap).unwrap()).unwrap();
        relay.push_value(&workspace, &serde_json::to_value(sealed).unwrap()).await.unwrap();
        // seqs 2..=7: openable messages in another chat.
        let mut store_a = EventStore::open_in_memory().unwrap();
        let other = seed_long_chat(&mut store_a, 5);
        let mut a = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(held);
        assert_eq!(a.push_new(&store_a).await.unwrap(), 6);

        let mut store_b = EventStore::open_in_memory().unwrap();
        let mut b = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(held);
        assert_eq!(b.catch_up(&mut store_b, 10).await.unwrap(), 6, "every openable event applied");
        assert!(!b.is_behind(), "reached the head despite the held body");
        assert_eq!(b.cursor(), 0, "durable cursor still at the retry point");
        assert_eq!(store_b.load_session(other).unwrap().unwrap().messages.len(), 5);

        // The key arrives: the next round starts at the retry point again.
        b.add_key(7, missing);
        b.catch_up(&mut store_b, 10).await.unwrap();
        assert!(store_b.has_event(snap.event_id).unwrap(), "held body converges");
        assert_eq!(b.cursor(), 7);
        assert_eq!(store_b.sync_cursor(&b.sync_key()).unwrap(), 7);
    }

    /// The probe asks for one envelope, never the room's history.
    #[tokio::test]
    async fn probe_fetches_one_envelope() {
        let (base, _gets) = spawn_paged_relay(1000).await;
        let workspace = Uuid::new_v4().to_string();
        let key = ligus_core::derive_workspace_key("probe");
        let mut store_a = EventStore::open_in_memory().unwrap();
        seed_long_chat(&mut store_a, 4);
        let mut a = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        a.push_new(&store_a).await.unwrap();
        let page = RelayClient::new(&base).fetch_page(&workspace, 0, 1).await.unwrap();
        assert_eq!(page.rows.len(), 1);
        assert!(page.more);
        assert_eq!(page.next_after, 1);
        assert_eq!(RelayClient::new(&base).probe(&workspace).await, crate::relay_client::RelayProbe::Ok);
    }
}

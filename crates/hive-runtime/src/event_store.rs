//! SQLite-backed event store — the Rust replacement for Swift's NDJSON event
//! log (`<workspaceRoot>/.hive/events/<sessionID>.ndjson`) +
//! `WorkspaceEventEnvelopeStore`.
//!
//! The append-only event-log semantics are unchanged: each workspace has an
//! ordered, per-session-sequenced stream of [`SessionEventEnvelope`]s; current
//! session state is obtained by projecting that stream (`hive_core::project`).
//! Only the storage medium changes (SQLite rows instead of NDJSON files), per
//! the clean-replacement plan.
//!
//! Each envelope's full JSON is stored verbatim in `envelope_json`; the
//! indexed columns (`session_id`, `sequence`, `kind`, `scope`, `timestamp`)
//! exist for efficient ordering/filtering and are derived from the envelope.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

use std::sync::Arc;

use hive_core::authz::ConfigTimeline;
use hive_core::{project_authorized, project_config, ChatSession, SessionEvent, SessionEventEnvelope, WorkspaceAuthz};
use rusqlite::{Connection, OptionalExtension};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum EventStoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, EventStoreError>;

mod repo_tables;
pub use repo_tables::{CheckoutSource, RepoMapping};

/// A chat responder's compaction state as kept on this device: the first
/// kept message (`boundary`), the ids the `summary` covers (everything before
/// the boundary when it was made) and their
/// [`hive_core::context_budget::messages_fingerprint`], which the reader
/// checks against the live transcript before reusing the summary.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredCompaction {
    pub boundary: Option<Uuid>,
    pub covered: Vec<Uuid>,
    pub fingerprint: String,
    pub summary: String,
}

/// An append-only, per-session-sequenced event log over SQLite.
pub struct EventStore {
    conn: Connection,
    /// Projections reused while their session's rows are unchanged. A busy
    /// workspace-config log (host heartbeats) runs to tens of thousands of
    /// events, and every chat load projects it again, so without this one
    /// dispatch or UI refresh spent seconds re-folding the same log.
    projections: Mutex<HashMap<ProjectionKey, ProjectionEntry>>,
    /// Folded config logs with their roster timelines (see [`EventStore::load_config`]).
    configs: Mutex<HashMap<ConfigKey, ConfigEntry>>,
    /// Each log's lamport head ([`EventStore::lamport_head`]), by session,
    /// with the rows it was computed over.
    heads: Mutex<HashMap<Uuid, (RowsSig, Option<hive_core::CfgFrontier>)>>,
}

/// A cached chat projection: its rows, the config events held when it was
/// folded, and the session.
type ProjectionEntry = (RowsSig, usize, Option<ChatSession>);
/// (config session, trusted founder, trusted workspace).
type ConfigKey = (Uuid, Option<String>, Option<Uuid>);
type ConfigEntry = (RowsSig, Option<ChatSession>, Arc<ConfigTimeline>);

/// (session, trusted founder, trusted workspace, config-timeline fingerprint):
/// what a projection depends on besides the session's rows. The fingerprint
/// is 0 for an unchecked projection and 1 for one without a config log.
type ProjectionKey = (Uuid, Option<String>, Option<Uuid>, u64);

/// (row count, max row id) of a session. Any append raises the max row id and
/// any delete lowers the count, so an equal signature means the same rows
/// (envelope rows are never updated in place).
type RowsSig = (i64, i64);

/// Guard signals kept per device (all workspaces); older rows are pruned.
pub const GUARD_SIGNALS_KEEP: i64 = 20_000;
/// Parked relay bodies kept per room (A-13); junk is evicted first (B-11).
pub const PARKED_KEEP: i64 = 5_000;
/// Facts kept on this device (newest).
pub const FACTS_KEEP: i64 = 50_000;

/// Cached projections kept at most; past this the cache starts over.
const PROJECTION_CACHE_MAX: usize = 256;

impl EventStore {
    /// Open (creating if needed) the event store at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// Open an in-memory store (tests).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self> {
        // WAL for concurrent readers (the live UI connection + the background
        // sync connection share the same file); busy_timeout absorbs brief
        // write contention between them.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // NORMAL is safe under WAL (no corruption; at worst the last commit is
        // lost on an OS crash/power loss) and avoids an fsync per commit — a big
        // win for chat workloads. (#2)
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS events (
                row_id        INTEGER PRIMARY KEY AUTOINCREMENT,
                event_id      TEXT NOT NULL UNIQUE,
                session_id    TEXT NOT NULL,
                workspace_id  TEXT NOT NULL,
                sequence      INTEGER NOT NULL,
                kind          TEXT NOT NULL,
                scope         TEXT NOT NULL,
                timestamp     TEXT NOT NULL,
                envelope_json TEXT NOT NULL,
                UNIQUE (session_id, sequence)
            );
            CREATE INDEX IF NOT EXISTS idx_events_session_seq
                ON events (session_id, sequence);
            CREATE INDEX IF NOT EXISTS idx_events_workspace
                ON events (workspace_id, row_id);

            -- Durable sync state (#2), keyed by relay workspace/room. Without
            -- this the sync engine kept its cursor + pushed-set in memory only,
            -- so every process restart re-fetched from seq 0 and re-pushed the
            -- entire local log. `sync_cursor` is the highest relay server
            -- sequence durably applied; `pushed` is the set of event_ids already
            -- on the relay (we pushed them, or ingested them from there) so a
            -- restart never re-sends them.
            -- Device-local, never synced: each workspace's founder as its
            -- invite pinned it (account id, and `fk` = hex of the first 16
            -- bytes of SHA-256 of the founder's account signing key). The
            -- trust roster disputes a founder registration under another key.
            CREATE TABLE IF NOT EXISTS workspace_pins (
                workspace_id TEXT PRIMARY KEY,
                founder      TEXT NOT NULL,
                fk           TEXT NOT NULL
            );

            -- Device-local: the Admin-issued invite this device redeems when it
            -- enrolls itself in a workspace (from the hivews1: invite).
            CREATE TABLE IF NOT EXISTS join_invites (
                workspace_id TEXT PRIMARY KEY,
                invite_id    TEXT NOT NULL
            );

            -- Device-local (Phase B): the redeem key seed of the box
            -- admission this box redeems (from its admission code).
            CREATE TABLE IF NOT EXISTS join_redeem_keys (
                workspace_id TEXT PRIMARY KEY,
                seed         TEXT NOT NULL
            );

            -- Device-local: the issue time (unix ms) of the newest contact
            -- record seen per contact id, so a relay can't serve an older one
            -- (anti-rollback, IDENTITY-AND-FORGES S6/S4).
            CREATE TABLE IF NOT EXISTS contact_seen (
                cid       TEXT PRIMARY KEY,
                issued_ms INTEGER NOT NULL
            );

            -- Device-local (A-13): relay bodies sealed under a key this
            -- device doesn't hold, parked so they never pin the fetch cursor;
            -- retried whenever the keyring changes. Bounded per room.
            CREATE TABLE IF NOT EXISTS parked_bodies (
                workspace TEXT NOT NULL,
                seq       INTEGER NOT NULL,
                body      TEXT NOT NULL,
                PRIMARY KEY (workspace, seq)
            );

            CREATE TABLE IF NOT EXISTS sync_cursor (
                workspace        TEXT PRIMARY KEY,
                last_fetched_seq INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS pushed (
                workspace TEXT NOT NULL,
                event_id  TEXT NOT NULL,
                PRIMARY KEY (workspace, event_id)
            );

            -- Device-local compaction state, never synced: per chat and
            -- responder runtime, the batch boundary plus the summary of
            -- everything before it, the ids it covers and their fingerprint
            -- (see StoredCompaction). Lets an app restart reuse a summary
            -- instead of spending tens of seconds re-summarizing.
            CREATE TABLE IF NOT EXISTS compaction_state (
                session_id   TEXT NOT NULL,
                runtime_id   TEXT NOT NULL,
                boundary     TEXT,
                covered_json TEXT NOT NULL,
                fingerprint  TEXT NOT NULL,
                summary      TEXT NOT NULL,
                PRIMARY KEY (session_id, runtime_id)
            );

            -- Device-local guard signals, never synced (see hive_core::signal):
            -- content-free records of lease heartbeats, boundary decisions,
            -- hygiene catches and throttles. Bounded by `record_signal`.
            CREATE TABLE IF NOT EXISTS guard_signals (
                row_id       INTEGER PRIMARY KEY AUTOINCREMENT,
                workspace_id TEXT NOT NULL,
                session_id   TEXT,
                at_ms        INTEGER NOT NULL,
                kind         TEXT NOT NULL,
                signal_json  TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_guard_signals_ws
                ON guard_signals (workspace_id, row_id);

            -- Device-local facts (P0.5): per-turn measurement records that
            -- no longer ride the signed log. Ids, enums, numbers and
            -- timestamps only (see `crate::facts`). Bounded by `record_fact`.
            CREATE TABLE IF NOT EXISTS facts (
                row_id       INTEGER PRIMARY KEY AUTOINCREMENT,
                workspace_id TEXT NOT NULL,
                session_id   TEXT NOT NULL,
                at_ms        INTEGER NOT NULL,
                kind         TEXT NOT NULL,
                record_json  TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_facts_session ON facts (session_id, row_id);
            CREATE INDEX IF NOT EXISTS idx_facts_ws ON facts (workspace_id, row_id);

            -- Device-local identity-link checks (IDENTITY-AND-FORGES §2.5),
            -- never synced and never folded: what a forge said about a link,
            -- reused for 24 h. Keyed by a digest of the link and workspace.
            CREATE TABLE IF NOT EXISTS identity_link_checks (
                link_key    TEXT PRIMARY KEY,
                checked_ms  INTEGER NOT NULL,
                check_json  TEXT NOT NULL
            );

            -- Another copy under an event id this store already holds, on
            -- the same log, with different signed content. Kept whichever
            -- arrived first and folded with the log: the fold decides which
            -- copies count (hive_core::copies; only the same signer's second
            -- copy is an equivocation, review P-A2c). Never pushed.
            -- Device-local, never synced: the apply requests this host
            -- started (Phase B). Claimed before any git work, so a request is
            -- applied at most once here even across a crash or a restart.
            CREATE TABLE IF NOT EXISTS apply_attempts (
                request_id  TEXT PRIMARY KEY,
                started_ms  INTEGER NOT NULL,
                finished    INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS equivocations (
                row_id        INTEGER PRIMARY KEY AUTOINCREMENT,
                event_id      TEXT NOT NULL,
                session_id    TEXT NOT NULL,
                workspace_id  TEXT NOT NULL,
                kind          TEXT NOT NULL,
                envelope_json TEXT NOT NULL,
                sealed_epoch  INTEGER,
                UNIQUE (event_id, envelope_json)
            );
            CREATE INDEX IF NOT EXISTS idx_equivocations_session ON equivocations (session_id);

            -- Device-local, never synced: an event this device refused at
            -- ingest (for any reason: a revoked or rebound key, a removed
            -- member, an old key, a bad signature) is kept only as a lamport
            -- witness: its id, lamport, log head and chain link
            -- (hive_core::lamport). Another author may have held it and
            -- counted from it; without it, that author's later events would
            -- be held here for good. Kept by that whole structure (a forged
            -- copy of an id never shadows the genuine one), counted by the
            -- lamport rule whether or not the id is stored, and never folded
            -- (no payload). Empty text for an absent head or link.
            CREATE TABLE IF NOT EXISTS lamport_witnesses (
                row_id        INTEGER PRIMARY KEY AUTOINCREMENT,
                event_id      TEXT NOT NULL,
                session_id    TEXT NOT NULL,
                workspace_id  TEXT NOT NULL,
                lamport       INTEGER NOT NULL,
                log_head      TEXT NOT NULL DEFAULT '',
                prev          TEXT NOT NULL DEFAULT '',
                UNIQUE (event_id, lamport, log_head, prev)
            );
            CREATE INDEX IF NOT EXISTS idx_lamport_witnesses_session ON lamport_witnesses (session_id);
            "#,
        )?;
        repo_tables::init(&conn)?;
        // B1: the key epoch each relay body arrived sealed under (device-local,
        // unsigned; NULL for events authored here or received unsealed).
        let has_epoch: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('events') WHERE name = 'sealed_epoch'")?
            .exists([])?;
        if !has_epoch {
            conn.execute_batch("ALTER TABLE events ADD COLUMN sealed_epoch INTEGER")?;
        }
        // B-11: how much a parked body is worth keeping when the room's cap
        // is reached (see [`EventStore::park_bodies`]).
        let has_priority: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('parked_bodies') WHERE name = 'priority'")?
            .exists([])?;
        if !has_priority {
            conn.execute_batch("ALTER TABLE parked_bodies ADD COLUMN priority INTEGER NOT NULL DEFAULT 0")?;
        }
        Ok(Self { conn, projections: Mutex::new(HashMap::new()), configs: Mutex::new(HashMap::new()), heads: Mutex::new(HashMap::new()) })
    }

    /// A cached identity-link check (device-local).
    pub fn link_check(&self, key: &str) -> Option<crate::identity_links::CachedCheck> {
        let json: Option<String> = self
            .conn
            .query_row("SELECT check_json FROM identity_link_checks WHERE link_key = ?1", [key], |r| r.get(0))
            .ok();
        json.and_then(|j| serde_json::from_str(&j).ok())
    }

    /// Record an identity-link check, keeping at most 4096 (the oldest go).
    pub fn put_link_check(&self, key: &str, check: &crate::identity_links::CachedCheck) {
        let Ok(json) = serde_json::to_string(check) else { return };
        let ms = (check.checked_at.0.unix_timestamp_nanos() / 1_000_000) as i64;
        let _ = self.conn.execute(
            "INSERT INTO identity_link_checks (link_key, checked_ms, check_json) VALUES (?1, ?2, ?3) \
             ON CONFLICT(link_key) DO UPDATE SET checked_ms = excluded.checked_ms, check_json = excluded.check_json",
            rusqlite::params![key, ms, json],
        );
        let _ = self.conn.execute(
            "DELETE FROM identity_link_checks WHERE link_key NOT IN \
             (SELECT link_key FROM identity_link_checks ORDER BY checked_ms DESC LIMIT 4096)",
            [],
        );
    }

    /// Highest sequence recorded for a session, if any.
    pub fn max_sequence(&self, session_id: Uuid) -> Result<Option<i64>> {
        let seq = self
            .conn
            .query_row(
                "SELECT MAX(sequence) FROM events WHERE session_id = ?1",
                [session_id.to_string()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten();
        Ok(seq)
    }

    /// The rows a session's projections are computed over (its events and
    /// equivocated copies): a cache key.
    fn rows_sig(&self, session_id: Uuid) -> Result<RowsSig> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) + (SELECT COUNT(*) FROM equivocations WHERE session_id = ?1) \
             + (SELECT COUNT(*) FROM lamport_witnesses WHERE session_id = ?1), COALESCE(MAX(row_id), 0) FROM events WHERE session_id = ?1",
            [session_id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    /// Every envelope of a log this device holds (equivocated copies
    /// included) plus its lamport witnesses as inert events: what a fold of
    /// the log reads.
    fn log_with_witnesses(&self, session_id: Uuid) -> Result<Vec<SessionEventEnvelope>> {
        let mut envelopes = self.query_envelopes_sealed(
            "SELECT envelope_json, sealed_epoch FROM events WHERE session_id = ?1 \
             UNION ALL SELECT envelope_json, sealed_epoch FROM equivocations WHERE session_id = ?1",
            [session_id.to_string()],
        )?;
        envelopes.extend(self.witnesses_of(session_id)?);
        Ok(envelopes)
    }

    /// A log's lamport witnesses (events refused here at ingest, see the
    /// `lamport_witnesses` table), each as an inert envelope: no payload
    /// (`Unknown`), no author or signer, only its id, lamport, log head and
    /// chain link. All of them, whether or not the id is stored: the lamport
    /// rule counts every copy alike on every device, and the fold leaves
    /// witnesses out (`hive_core::lamport::is_witness`).
    fn witnesses_of(&self, session_id: Uuid) -> Result<Vec<SessionEventEnvelope>> {
        let mut stmt = self.conn.prepare(
            "SELECT event_id, workspace_id, lamport, log_head, prev FROM lamport_witnesses WHERE session_id = ?1 ORDER BY row_id ASC",
        )?;
        let rows = stmt.query_map([session_id.to_string()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, ws, lamport, head, prev) = row?;
            let (Ok(id), Ok(ws)) = (Uuid::parse_str(&id), Uuid::parse_str(&ws)) else { continue };
            let mut env = SessionEventEnvelope::new(session_id, ws, 0, SessionEvent::Unknown);
            env.event_id = id;
            env.lamport = lamport as u64;
            env.actor_stamp = Some(hive_core::ActorStamp {
                actor: hive_core::ActorIdentity::new("", "", hive_core::ActorKind::System),
                recorded_at: hive_core::Timestamp::epoch(),
                cfg_frontier: None,
                presence: None,
                runtime_effect: false,
                prev: Uuid::parse_str(&prev).ok(),
                sig_alg: Default::default(),
                log_head: serde_json::from_str(&head).ok(),
            });
            let env = hive_core::lamport::inert_witness(&env);
            out.push(env);
        }
        Ok(out)
    }

    /// Keep `env`, which this device refused at ingest (for whatever reason:
    /// the refusal can depend on this device's roster), as a lamport witness
    /// of its log (see the `lamport_witnesses` table). Only its id, lamport,
    /// log head and chain link are kept; never pushed, never folded as
    /// itself.
    pub fn note_lamport_witness(&self, env: &SessionEventEnvelope) -> Result<()> {
        let stamp = env.actor_stamp.as_ref();
        let head = stamp.and_then(|s| s.log_head).map(|h| serde_json::to_string(&h)).transpose()?.unwrap_or_default();
        let prev = stamp.and_then(|s| s.prev).map(|p| p.to_string()).unwrap_or_default();
        self.conn.execute(
            "INSERT OR IGNORE INTO lamport_witnesses (event_id, session_id, workspace_id, lamport, log_head, prev) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                env.event_id.to_string(),
                env.session_id.to_string(),
                env.workspace_id.to_string(),
                env.lamport.min(i64::MAX as u64) as i64,
                head,
                prev
            ],
        )?;
        Ok(())
    }

    /// The newest event of a session's log whose lamport is within bounds
    /// (`hive_core::lamport`), over every envelope this device holds for it
    /// (authored here or ingested, equivocated copies included; a lamport
    /// witness counts for the bounds but is never the head): what this
    /// device's next event on the log names as its `log_head`, its lamport
    /// one past it. An event past the bounds (a lamport no real event
    /// accounts for) never moves it. Cached per log; an event appended here
    /// after the head moves it without a rescan.
    pub fn lamport_head(&self, session_id: Uuid) -> Result<Option<hive_core::CfgFrontier>> {
        let sig = self.rows_sig(session_id)?;
        if let Some((cached, head)) = self.heads.lock().unwrap_or_else(|e| e.into_inner()).get(&session_id) {
            if *cached == sig {
                return Ok(*head);
            }
        }
        let envelopes = self.log_with_witnesses(session_id)?;
        let head = hive_core::lamport::head(&envelopes);
        let mut cache = self.heads.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= PROJECTION_CACHE_MAX && !cache.contains_key(&session_id) {
            cache.clear();
        }
        cache.insert(session_id, (sig, head));
        Ok(head)
    }

    /// Append a pre-built envelope verbatim. Fails if its
    /// `(session_id, sequence)` already exists (the append-only invariant).
    pub fn append_envelope(&self, env: &SessionEventEnvelope) -> Result<()> {
        // The cached head, if it is current: an event that names it moves it.
        let before = self.rows_sig(env.session_id)?;
        let cached = self.heads.lock().unwrap_or_else(|e| e.into_inner()).get(&env.session_id).copied();
        let json = serde_json::to_string(env)?;
        let timestamp = serde_json::to_value(env.timestamp)?
            .as_str()
            .unwrap_or_default()
            .to_string();
        self.conn.execute(
            r#"
            INSERT INTO events
                (event_id, session_id, workspace_id, sequence, kind, scope, timestamp, envelope_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            rusqlite::params![
                env.event_id.to_string(),
                env.session_id.to_string(),
                env.workspace_id.to_string(),
                env.sequence,
                env.payload.kind_str(),
                serde_json::to_value(env.scope)?.as_str().unwrap_or_default(),
                timestamp,
                json,
            ],
        )?;
        if let Some((sig, head)) = cached {
            let names_head = env.actor_stamp.as_ref().map(|s| s.log_head) == Some(head)
                && env.lamport == hive_core::lamport::next(head);
            let mut cache = self.heads.lock().unwrap_or_else(|e| e.into_inner());
            if sig == before && names_head {
                let key = hive_core::CfgFrontier { lamport: env.lamport, event_id: env.event_id };
                cache.insert(env.session_id, (self.rows_sig(env.session_id)?, Some(key)));
            } else {
                cache.remove(&env.session_id);
            }
        }
        Ok(())
    }

    /// Tests only: append an event for a session as the fixture device
    /// ([`fixture_next`]), assigning the next sequence atomically, and return
    /// the stored envelope. Products author through `ChatService`.
    #[cfg(test)]
    pub fn append(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        payload: SessionEvent,
    ) -> Result<SessionEventEnvelope> {
        // An authorless stamp naming the log's head, so the lamport is one the
        // fold accounts for (`hive_core::lamport`).
        let log_head = self.lamport_head(session_id)?;
        let tx = self.conn.transaction()?;
        let next_seq = tx
            .query_row(
                "SELECT MAX(sequence) FROM events WHERE session_id = ?1",
                [session_id.to_string()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten()
            .map(|s| s + 1)
            .unwrap_or(1);

        let env = fixture_next(&SessionEventEnvelope::new(session_id, workspace_id, next_seq, payload), log_head);
        let json = serde_json::to_string(&env)?;
        let timestamp = serde_json::to_value(env.timestamp)?
            .as_str()
            .unwrap_or_default()
            .to_string();
        tx.execute(
            r#"
            INSERT INTO events
                (event_id, session_id, workspace_id, sequence, kind, scope, timestamp, envelope_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            rusqlite::params![
                env.event_id.to_string(),
                env.session_id.to_string(),
                env.workspace_id.to_string(),
                env.sequence,
                env.payload.kind_str(),
                serde_json::to_value(env.scope)?.as_str().unwrap_or_default(),
                timestamp,
                json,
            ],
        )?;
        tx.commit()?;
        Ok(env)
    }

    /// The envelopes that feed the trust roster (membership, account keys and
    /// their rotations, device certificates and revocations, snapshots, and
    /// the rules and passkeys that gate devices under rules v2), across
    /// all sessions. Cheap — filtered by `kind`. Consumed by
    /// `envelope_verifier::build_roster` to verify signatures on ingest without
    /// any network. Revocations and rotations must be here: a `DeviceRevoked`
    /// pulled in an earlier batch is what keeps a revoked device's later events
    /// out.
    pub fn roster_envelopes(&self) -> Result<Vec<SessionEventEnvelope>> {
        self.query_envelopes(
            "SELECT envelope_json FROM (SELECT row_id, kind, envelope_json FROM events \
             UNION ALL SELECT row_id + 9000000000000, kind, envelope_json FROM equivocations) \
             WHERE kind IN ('accountKeyRegistered','accountKeyRotated','deviceCertificateAdded','deviceRevoked',\
             'memberAdded','memberRemoved','sessionSnapshot','memberRoleChanged','dearthModeChanged','joinApproved',\
             'agentKeyCertified','agentUpserted','agentRosterUpdated','agentRemoved',\
             'workspaceRulesSet','passkeyRegistered','passkeyRemoved') \
             ORDER BY row_id ASC",
            [],
        )
    }

    /// A workspace's whole trust history — account keys (registered/rotated),
    /// device certificates and revocations, membership, and the rules and
    /// passkeys that gate new devices under rules v2 — for folding its
    /// [`crate::envelope_verifier::WorkspaceRoster`] without the rest of the
    /// chats. Its config log comes whole: every event there is a lamport
    /// witness for the ones after it (`hive_core::lamport`), so a subset of
    /// it would hold the trust events that count from the rest.
    pub fn trust_envelopes(&self, workspace_id: Uuid) -> Result<Vec<SessionEventEnvelope>> {
        let mut out = self.query_envelopes(
            "SELECT envelope_json FROM (SELECT row_id, workspace_id, session_id, kind, envelope_json FROM events \
             UNION ALL SELECT row_id + 9000000000000, workspace_id, session_id, kind, envelope_json FROM equivocations) \
             WHERE workspace_id = ?1 \
             AND (session_id = ?2 OR kind IN ('accountKeyRegistered','accountKeyRotated','deviceCertificateAdded','deviceRevoked',\
             'memberAdded','memberRemoved','sessionSnapshot','memberRoleChanged','dearthModeChanged','joinApproved',\
             'agentKeyCertified','agentUpserted','agentRosterUpdated','agentRemoved',\
             'workspaceRulesSet','passkeyRegistered','passkeyRemoved')) \
             ORDER BY row_id ASC",
            [workspace_id.to_string(), hive_core::workspace_config_session_id(workspace_id).to_string()],
        )?;
        out.extend(self.witnesses_of(hive_core::workspace_config_session_id(workspace_id))?);
        Ok(out)
    }

    /// A workspace's account keys and device certificates (registrations,
    /// rotations, certs), for checking a device belongs to an account without
    /// folding the whole trust roster. Cheap: filtered by workspace and kind.
    pub fn identity_envelopes(&self, workspace_id: Uuid) -> Result<Vec<SessionEventEnvelope>> {
        self.query_envelopes(
            "SELECT envelope_json FROM events WHERE workspace_id = ?1 \
             AND kind IN ('accountKeyRegistered','accountKeyRotated','deviceCertificateAdded') \
             ORDER BY row_id ASC",
            [workspace_id.to_string()],
        )
    }

    /// The highest row id stored (0 when empty): a cursor for "what arrived since".
    pub fn max_row_id(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COALESCE(MAX(row_id), 0) FROM events", [], |row| row.get(0))?)
    }

    /// The newest `equivocations` row id (0 when none).
    pub fn max_equivocation_row_id(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT COALESCE(MAX(row_id), 0) FROM equivocations", [], |row| row.get(0))?)
    }

    /// Whether `session_id` gained an event after events row `after_event` or
    /// an equivocation after equivocations row `after_equiv`. Reads only the
    /// newer rows (a row-id range), so it stays cheap on a busy store.
    pub fn session_rows_after(&self, session_id: Uuid, after_event: i64, after_equiv: i64) -> Result<bool> {
        let sid = session_id.to_string();
        let ev: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE row_id > ?1 AND session_id = ?2)",
            rusqlite::params![after_event, sid],
            |r| r.get(0),
        )?;
        if ev {
            return Ok(true);
        }
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM equivocations WHERE row_id > ?1 AND session_id = ?2)",
            rusqlite::params![after_equiv, sid],
            |r| r.get(0),
        )?)
    }

    /// Sessions that gained a message after row `after` (see [`Self::max_row_id`]),
    /// for answering what a sync pull brought in.
    pub fn sessions_with_messages_after(&self, after: i64) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT session_id FROM events WHERE row_id > ?1 AND kind = 'messageAppended'",
        )?;
        let rows = stmt.query_map([after], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for id in rows {
            if let Ok(id) = Uuid::parse_str(&id?) {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// The messages appended after row `after`, as `(session, message, signing
    /// device)` in arrival order: what one sync pull brought in, message by
    /// message. A pull can carry several user turns for one chat (two posts a
    /// few ms apart); dispatch must consider each of them, not only the
    /// chat's last. The signer lets the caller skip its own device's posts.
    pub fn messages_after(&self, after: i64) -> Result<Vec<(Uuid, Uuid, Option<Uuid>)>> {
        Ok(self
            .query_envelopes(
                "SELECT envelope_json FROM events WHERE row_id > ?1 AND kind = 'messageAppended' ORDER BY row_id ASC",
                [after],
            )?
            .into_iter()
            .filter_map(|e| match e.payload {
                SessionEvent::MessageAppended { message } => Some((e.session_id, message.id, e.signer_device_id)),
                _ => None,
            })
            .collect())
    }

    /// Whether this workspace already carries a device certificate binding
    /// `device_id` to `account_id` — so identity publication stays idempotent
    /// (and re-publishes once when the account id changes, e.g. GitHub sign-in).
    /// Workspaces where some device published its key-agreement key (B1).
    pub fn workspaces_with_ka_publications(&self) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT workspace_id FROM events WHERE kind = 'deviceKeyAgreementPublished'")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            if let Ok(id) = Uuid::parse_str(&row?) {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Whether `session_id` itself carries a certificate for (device, account).
    pub fn has_device_certificate_in(&self, session_id: Uuid, device_id: Uuid, account_id: Uuid) -> Result<bool> {
        let envs = self.query_envelopes(
            "SELECT envelope_json FROM events WHERE session_id = ?1 AND kind = 'deviceCertificateAdded'",
            [session_id.to_string()],
        )?;
        Ok(envs.iter().any(|e| {
            matches!(&e.payload, SessionEvent::DeviceCertificateAdded { certificate }
                if certificate.device_id == device_id && certificate.account_id == account_id)
        }))
    }

    pub fn has_device_certificate(
        &self,
        workspace_id: Uuid,
        device_id: Uuid,
        account_id: Uuid,
    ) -> Result<bool> {
        let envs = self.query_envelopes(
            "SELECT envelope_json FROM events WHERE workspace_id = ?1 AND kind = 'deviceCertificateAdded'",
            [workspace_id.to_string()],
        )?;
        Ok(envs.iter().any(|e| {
            matches!(&e.payload, SessionEvent::DeviceCertificateAdded { certificate }
                if certificate.device_id == device_id && certificate.account_id == account_id)
        }))
    }

    /// Keep `env` beside the stored envelope under its id when it is a
    /// different copy on the same log (one that only travelled or was
    /// re-sealed is not). Which copies count is the fold's decision
    /// (`hive_core::copies`): only a verified copy counts against a verified
    /// one, and only the same signer's second copy is an equivocation (review
    /// P-A2c). An unsigned copy of a signed event can never count, so it is
    /// refused here and kept only as a lamport witness (ingest refuses
    /// unsigned envelopes anyway; this covers every other path in).
    fn note_equivocation(&mut self, env: &SessionEventEnvelope, sealed_epoch: Option<u32>) -> Result<()> {
        let stored: Option<String> = self
            .conn
            .query_row("SELECT envelope_json FROM events WHERE event_id = ?1", [env.event_id.to_string()], |r| r.get(0))
            .optional()?;
        let Some(stored) = stored.and_then(|j| serde_json::from_str::<SessionEventEnvelope>(&j).ok()) else { return Ok(()) };
        if !hive_core::events::equivocates(&stored, env) {
            return Ok(());
        }
        if env.signature.is_none() && stored.signature.is_some() {
            return self.note_lamport_witness(env);
        }
        self.conn.execute(
            "INSERT OR IGNORE INTO equivocations (event_id, session_id, workspace_id, kind, envelope_json, sealed_epoch) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                env.event_id.to_string(),
                env.session_id.to_string(),
                env.workspace_id.to_string(),
                env.payload.kind_str(),
                serde_json::to_string(env)?,
                sealed_epoch.map(i64::from),
            ],
        )?;
        tracing::warn!(event_id = %env.event_id, "another copy under a stored event id: kept beside it; the fold decides which counts");
        Ok(())
    }

    /// Claim apply request `request_id` for this host: `true` the first
    /// time, `false` ever after (a restart included).
    pub fn claim_apply_attempt(&self, request_id: Uuid) -> Result<bool> {
        let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO apply_attempts (request_id, started_ms) VALUES (?1, ?2)",
            rusqlite::params![request_id.to_string(), ms],
        )?;
        Ok(n == 1)
    }

    /// Record that this host wrote its outcome for `request_id` (or gave up).
    pub fn finish_apply_attempt(&self, request_id: Uuid) -> Result<()> {
        self.conn.execute("UPDATE apply_attempts SET finished = 1 WHERE request_id = ?1", [request_id.to_string()])?;
        Ok(())
    }

    /// Whether this host finished with `request_id`.
    pub fn apply_attempt_finished(&self, request_id: Uuid) -> Result<bool> {
        let n: Option<i64> = self
            .conn
            .query_row("SELECT finished FROM apply_attempts WHERE request_id = ?1", [request_id.to_string()], |r| r.get(0))
            .optional()?;
        Ok(n == Some(1))
    }

    pub fn has_event(&self, event_id: Uuid) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM events WHERE event_id = ?1",
            [event_id.to_string()],
            |row| row.get(0),
        )?;
        Ok(n > 0)
    }

    /// Ingest a foreign (synced) envelope: if unseen, append it under a fresh
    /// **local** sequence (preserving the original envelope body for signature
    /// verification + provenance). Returns `true` if newly stored. This gives a
    /// per-device local total order = ingestion order; the projector's events
    /// are commutative enough (upsert-by-id, idempotent reactions) for that to
    /// converge across peers.
    pub fn ingest(&mut self, env: &SessionEventEnvelope) -> Result<bool> {
        self.ingest_sealed(env, env.sealed_epoch())
    }

    /// Tests only: [`EventStore::ingest`] an **unsigned** fixture as the next
    /// event of its log here ([`fixture_next`]). A signed envelope is
    /// ingested as is.
    #[cfg(test)]
    pub(crate) fn ingest_next(&mut self, env: &SessionEventEnvelope) -> Result<bool> {
        if env.signature.is_some() {
            return self.ingest(env);
        }
        let env = fixture_next(env, self.lamport_head(env.session_id)?);
        self.ingest(&env)
    }

    /// [`EventStore::ingest`], recording the key epoch the body arrived sealed
    /// under (B1). The first arrival of one copy keeps its epoch: a re-seal
    /// by an old-key holder only repeats it.
    ///
    /// Another copy under an id this store holds is kept beside it (the
    /// `equivocations` table), whichever arrived first: the fold decides
    /// which copies count (`hive_core::copies`), so arrival order never does.
    pub fn ingest_sealed(&mut self, env: &SessionEventEnvelope, sealed_epoch: Option<u32>) -> Result<bool> {
        if self.has_event(env.event_id)? {
            self.note_equivocation(env, sealed_epoch)?;
            return Ok(false);
        }
        // IMMEDIATE: take the write lock up front (waiting out the busy
        // timeout), so the read of the next sequence never has to upgrade a
        // stale snapshot while another connection writes (an instant
        // "database is locked" under WAL).
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let next_seq = tx
            .query_row(
                "SELECT MAX(sequence) FROM events WHERE session_id = ?1",
                [env.session_id.to_string()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten()
            .map(|s| s + 1)
            .unwrap_or(1);
        let json = serde_json::to_string(env)?;
        let timestamp = serde_json::to_value(env.timestamp)?
            .as_str()
            .unwrap_or_default()
            .to_string();
        tx.execute(
            r#"
            INSERT INTO events
                (event_id, session_id, workspace_id, sequence, kind, scope, timestamp, envelope_json, sealed_epoch)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            rusqlite::params![
                env.event_id.to_string(),
                env.session_id.to_string(),
                env.workspace_id.to_string(),
                next_seq,
                env.payload.kind_str(),
                serde_json::to_value(env.scope)?.as_str().unwrap_or_default(),
                timestamp,
                json,
                sealed_epoch,
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Every copy this device holds of a session's events: its log in
    /// sequence order, then the other copies of ids it holds (the
    /// `equivocations` table: copies under an id it already held, kept
    /// whichever arrived first, see `hive_core::copies`). For a reader that
    /// verifies what it relies on and must not be blinded by a forged copy
    /// that happened to arrive first.
    pub fn list_copies(&self, session_id: Uuid) -> Result<Vec<SessionEventEnvelope>> {
        self.query_envelopes(
            "SELECT envelope_json FROM (SELECT 0 AS t, sequence AS o, envelope_json FROM events WHERE session_id = ?1 \
             UNION ALL SELECT 1, row_id, envelope_json FROM equivocations WHERE session_id = ?1) ORDER BY t, o",
            [session_id.to_string()],
        )
    }

    /// All envelopes for a session in sequence order.
    pub fn list(&self, session_id: Uuid) -> Result<Vec<SessionEventEnvelope>> {
        self.query_envelopes(
            "SELECT envelope_json FROM events WHERE session_id = ?1 ORDER BY sequence ASC",
            [session_id.to_string()],
        )
    }

    /// Every envelope stored after row `after`, in row order, with the highest
    /// row id seen (`after` when there are none) — the incremental push scan.
    /// Row ids only grow (AUTOINCREMENT, one writer at a time), so a later call
    /// from the returned row id never misses a row committed since.
    pub fn envelopes_after_row(&self, after: i64) -> Result<(i64, Vec<SessionEventEnvelope>)> {
        let mut stmt = self
            .conn
            .prepare("SELECT row_id, envelope_json FROM events WHERE row_id > ?1 ORDER BY row_id ASC")?;
        let rows = stmt.query_map([after], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?;
        let mut last = after;
        let mut out = Vec::new();
        for row in rows {
            let (row_id, json) = row?;
            last = last.max(row_id);
            match serde_json::from_str::<SessionEventEnvelope>(&json) {
                Ok(env) => out.push(env),
                Err(e) => tracing::warn!(error = %e, "skipping unparseable event row during push scan"),
            }
        }
        Ok((last, out))
    }

    /// Envelopes for a session with `sequence > after`, in order — the sync
    /// fetch path.
    pub fn list_after(&self, session_id: Uuid, after: i64) -> Result<Vec<SessionEventEnvelope>> {
        self.query_envelopes(
            "SELECT envelope_json FROM events WHERE session_id = ?1 AND sequence > ?2 ORDER BY sequence ASC",
            rusqlite::params![session_id.to_string(), after],
        )
    }

    /// Like [`EventStore::query_envelopes`] for `SELECT envelope_json,
    /// sealed_epoch …`: each envelope carries the epoch it arrived sealed
    /// under, which the fold's old-epoch rule reads (B1).
    fn query_envelopes_sealed(&self, sql: &str, params: impl rusqlite::Params) -> Result<Vec<SessionEventEnvelope>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params, |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (json, epoch) = row?;
            match serde_json::from_str::<SessionEventEnvelope>(&json) {
                Ok(mut env) => {
                    env.set_sealed_epoch(epoch.and_then(|e| u32::try_from(e).ok()));
                    out.push(env);
                }
                Err(e) => tracing::warn!(error = %e, "skipping unparseable event row during load"),
            }
        }
        Ok(out)
    }

    /// Park a relay body this device can't open yet (A-13), with priority 0.
    pub fn park_body(&self, workspace: &str, seq: u64, body: &serde_json::Value) -> Result<()> {
        self.park_bodies(workspace, &[(seq, body, 0)])
    }

    /// Park relay bodies this device can't open yet (A-13), each with how
    /// much it is worth keeping (`priority`, higher first; see
    /// `KeyringEngine::park_priority`). Keeps at most [`PARKED_KEEP`] per
    /// room: past that the lowest priority goes first, oldest first within a
    /// priority (B-11: a flood of junk can't evict a body waiting for an
    /// authorized rotation).
    pub fn park_bodies(&self, workspace: &str, bodies: &[(u64, &serde_json::Value, u8)]) -> Result<()> {
        if bodies.is_empty() {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        for (seq, body, priority) in bodies {
            tx.execute(
                "INSERT OR REPLACE INTO parked_bodies (workspace, seq, body, priority) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![workspace, *seq as i64, serde_json::to_string(body)?, *priority as i64],
            )?;
        }
        tx.execute(
            "DELETE FROM parked_bodies WHERE workspace = ?1 AND seq NOT IN \
             (SELECT seq FROM parked_bodies WHERE workspace = ?1 ORDER BY priority DESC, seq DESC LIMIT ?2)",
            rusqlite::params![workspace, PARKED_KEEP],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Re-rank parked bodies (the keyring changed: a rotation they wait for
    /// may now be authorized, or one they claim may have turned out junk).
    pub fn set_parked_priorities(&self, workspace: &str, priorities: &[(u64, u8)]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for (seq, priority) in priorities {
            tx.execute(
                "UPDATE parked_bodies SET priority = ?3 WHERE workspace = ?1 AND seq = ?2",
                rusqlite::params![workspace, *seq as i64, *priority as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Every parked body of a room, by relay sequence.
    pub fn parked_bodies(&self, workspace: &str) -> Result<Vec<(u64, serde_json::Value)>> {
        let mut stmt = self.conn.prepare("SELECT seq, body FROM parked_bodies WHERE workspace = ?1 ORDER BY seq ASC")?;
        let rows = stmt.query_map([workspace], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, body) = row?;
            if let Ok(v) = serde_json::from_str(&body) {
                out.push((seq as u64, v));
            }
        }
        Ok(out)
    }

    /// Forget a parked body (it opened, or proved bad).
    pub fn unpark(&self, workspace: &str, seq: u64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM parked_bodies WHERE workspace = ?1 AND seq = ?2",
            rusqlite::params![workspace, seq as i64],
        )?;
        Ok(())
    }

    /// The key epoch an event arrived sealed under, if it was received sealed.
    pub fn sealed_epoch_of(&self, event_id: Uuid) -> Result<Option<u32>> {
        let v: Option<Option<i64>> = self
            .conn
            .query_row("SELECT sealed_epoch FROM events WHERE event_id = ?1", [event_id.to_string()], |r| r.get(0))
            .optional()?;
        Ok(v.flatten().and_then(|e| u32::try_from(e).ok()))
    }

    fn query_envelopes(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<SessionEventEnvelope>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params, |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for json in rows {
            let json = json?;
            // Resilient decode: a single malformed row must not poison the whole
            // session load. Unknown event *kinds* already deserialize to
            // `SessionEvent::Unknown` (forward-compat); this guards genuinely
            // corrupt/truncated JSON — skip and continue rather than erroring the
            // entire projection.
            match serde_json::from_str::<SessionEventEnvelope>(&json) {
                Ok(env) => out.push(env),
                Err(e) => {
                    tracing::warn!(error = %e, "skipping unparseable event row during load");
                }
            }
        }
        Ok(out)
    }

    // --- Durable sync state (#2): survives process restarts so the sync
    // engine resumes its fetch cursor + push-dedup instead of restarting from
    // scratch. Keyed by the relay `workspace`/room string. ---

    /// The durable fetch cursor for a workspace — the highest relay server
    /// sequence applied so far (0 if the workspace has never synced).
    pub fn sync_cursor(&self, workspace: &str) -> Result<u64> {
        let seq: Option<i64> = self
            .conn
            .query_row(
                "SELECT last_fetched_seq FROM sync_cursor WHERE workspace = ?1",
                [workspace],
                |row| row.get(0),
            )
            .optional()?;
        Ok(seq.unwrap_or(0).max(0) as u64)
    }

    /// Persist the fetch cursor for a workspace (upsert).
    pub fn set_sync_cursor(&self, workspace: &str, last_fetched_seq: u64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_cursor (workspace, last_fetched_seq) VALUES (?1, ?2) \
             ON CONFLICT(workspace) DO UPDATE SET last_fetched_seq = excluded.last_fetched_seq",
            rusqlite::params![workspace, last_fetched_seq as i64],
        )?;
        Ok(())
    }

    /// Mark an event as already on the relay for a workspace (idempotent), so a
    /// later `take_unpushed` — even after a restart — never re-pushes it.
    pub fn mark_pushed(&self, workspace: &str, event_id: Uuid) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO pushed (workspace, event_id) VALUES (?1, ?2)",
            rusqlite::params![workspace, event_id.to_string()],
        )?;
        Ok(())
    }

    /// Every event_id already pushed for a workspace — used to seed the sync
    /// engine's in-memory dedup cache on construction.
    pub fn pushed_ids(&self, workspace: &str) -> Result<Vec<Uuid>> {
        let mut stmt = self
            .conn
            .prepare("SELECT event_id FROM pushed WHERE workspace = ?1")?;
        let rows = stmt.query_map([workspace], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for s in rows {
            if let Ok(id) = Uuid::parse_str(&s?) {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Hard-delete every event for a session (irreversible), and every
    /// other copy held under its ids (left alone, one would fold as the
    /// session). Returns the number of log rows removed.
    pub fn delete_session(&self, session_id: Uuid) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM events WHERE session_id = ?1",
            [session_id.to_string()],
        )?;
        self.conn.execute("DELETE FROM equivocations WHERE session_id = ?1", [session_id.to_string()])?;
        self.delete_compaction(session_id, None)?;
        Ok(n)
    }

    // --- Device-local compaction state (`compaction_state`). Not part of the
    // event log: nothing here is synced or projected. ---

    /// The stored compaction state for a chat's responder runtime, if any. A
    /// row that no longer parses is treated as absent.
    pub fn compaction(&self, session_id: Uuid, runtime_id: &str) -> Result<Option<StoredCompaction>> {
        let row = self
            .conn
            .query_row(
                "SELECT boundary, covered_json, fingerprint, summary FROM compaction_state \
                 WHERE session_id = ?1 AND runtime_id = ?2",
                rusqlite::params![session_id.to_string(), runtime_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((boundary, covered_json, fingerprint, summary)) = row else {
            return Ok(None);
        };
        let boundary = match boundary.as_deref().map(Uuid::parse_str) {
            None => None,
            Some(Ok(id)) => Some(id),
            Some(Err(_)) => return Ok(None),
        };
        let Ok(covered) = serde_json::from_str::<Vec<Uuid>>(&covered_json) else {
            return Ok(None);
        };
        Ok(Some(StoredCompaction { boundary, covered, fingerprint, summary }))
    }

    /// Save (replace) a chat responder's compaction state.
    pub fn save_compaction(&self, session_id: Uuid, runtime_id: &str, state: &StoredCompaction) -> Result<()> {
        self.conn.execute(
            "INSERT INTO compaction_state (session_id, runtime_id, boundary, covered_json, fingerprint, summary) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(session_id, runtime_id) DO UPDATE SET boundary = excluded.boundary, \
             covered_json = excluded.covered_json, fingerprint = excluded.fingerprint, summary = excluded.summary",
            rusqlite::params![
                session_id.to_string(),
                runtime_id,
                state.boundary.map(|b| b.to_string()),
                serde_json::to_string(&state.covered)?,
                state.fingerprint,
                state.summary,
            ],
        )?;
        Ok(())
    }

    /// Forget a chat's compaction state: one responder runtime's, or every
    /// runtime's when `runtime_id` is `None`.
    pub fn delete_compaction(&self, session_id: Uuid, runtime_id: Option<&str>) -> Result<()> {
        match runtime_id {
            Some(rt) => self.conn.execute(
                "DELETE FROM compaction_state WHERE session_id = ?1 AND runtime_id = ?2",
                rusqlite::params![session_id.to_string(), rt],
            )?,
            None => self
                .conn
                .execute("DELETE FROM compaction_state WHERE session_id = ?1", [session_id.to_string()])?,
        };
        Ok(())
    }

    // --- Device-local guard signals (`guard_signals`). Never synced or
    // projected; bounded to the newest [`GUARD_SIGNALS_KEEP`] rows. ---

    /// Record a guard signal. Prunes the oldest rows past the bound.
    pub fn record_signal(
        &self,
        workspace_id: Uuid,
        session_id: Option<Uuid>,
        signal: &hive_core::signal::GuardSignal,
    ) -> Result<i64> {
        let at_ms = (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64;
        self.conn.execute(
            "INSERT INTO guard_signals (workspace_id, session_id, at_ms, kind, signal_json) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                workspace_id.to_string(),
                session_id.map(|s| s.to_string()),
                at_ms,
                signal.kind_str(),
                serde_json::to_string(signal)?,
            ],
        )?;
        let row = self.conn.last_insert_rowid();
        if row % 256 == 0 {
            self.conn.execute(
                "DELETE FROM guard_signals WHERE row_id <= ?1",
                [row - GUARD_SIGNALS_KEEP],
            )?;
        }
        Ok(row)
    }

    /// Signals for a workspace after `after_row` (oldest first), at most `limit`.
    /// `kind` filters by the signal's kind string.
    pub fn list_signals(
        &self,
        workspace_id: Uuid,
        after_row: i64,
        kind: Option<&str>,
        limit: usize,
    ) -> Result<Vec<hive_core::signal::GuardSignalRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT row_id, workspace_id, session_id, at_ms, signal_json FROM guard_signals \
             WHERE workspace_id = ?1 AND row_id > ?2 AND (?3 IS NULL OR kind = ?3) \
             ORDER BY row_id ASC LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![workspace_id.to_string(), after_row, kind, limit as i64],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                ))
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            let (row_id, ws, sid, at_ms, json) = row?;
            let Ok(signal) = serde_json::from_str(&json) else { continue };
            out.push(hive_core::signal::GuardSignalRecord {
                row_id,
                workspace_id: Uuid::parse_str(&ws).unwrap_or_default(),
                session_id: sid.and_then(|s| Uuid::parse_str(&s).ok()),
                at_ms,
                signal,
            });
        }
        Ok(out)
    }

    // --- Device-local facts (`facts`). Never synced; bounded to the newest
    // [`FACTS_KEEP`] rows. ---

    /// Record one fact (a measurement flow record).
    pub fn record_fact(&self, workspace_id: Uuid, session_id: Uuid, at_ms: i64, kind: &str, record_json: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO facts (workspace_id, session_id, at_ms, kind, record_json) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![workspace_id.to_string(), session_id.to_string(), at_ms, kind, record_json],
        )?;
        let row = self.conn.last_insert_rowid();
        if row % 256 == 0 {
            self.conn.execute("DELETE FROM facts WHERE row_id <= ?1", [row - FACTS_KEEP])?;
        }
        Ok(row)
    }

    /// Highest fact row (0 when none): with [`Self::max_row_id`], what a
    /// cached view of folded state plus facts is keyed on.
    pub fn max_fact_row(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT COALESCE(MAX(row_id), 0) FROM facts", [], |row| row.get(0))?)
    }

    /// A session's facts, oldest first: (row, at_ms, kind, record_json).
    pub fn session_facts(&self, session_id: Uuid) -> Result<Vec<(i64, i64, String, String)>> {
        let mut stmt = self.conn.prepare("SELECT row_id, at_ms, kind, record_json FROM facts WHERE session_id = ?1 ORDER BY row_id ASC")?;
        let rows = stmt.query_map([session_id.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Every fact at or after `since_ms` (all workspaces), oldest first:
    /// (row, workspace, session, at_ms, kind, record_json).
    #[allow(clippy::type_complexity)]
    pub fn facts_since(&self, since_ms: i64) -> Result<Vec<(i64, String, String, i64, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT row_id, workspace_id, session_id, at_ms, kind, record_json FROM facts WHERE at_ms >= ?1 ORDER BY row_id ASC",
        )?;
        let rows = stmt.query_map([since_ms], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// The newest event `signer` put on `session_id` here (by local row
    /// order) with a lamport below `below`: what a new event of that signer at
    /// lamport `below` names as `ActorStamp::prev` (a chain link is always
    /// older than the event, `hive_core::lamport`).
    pub fn last_signed_by(&self, session_id: Uuid, signer: Uuid, below: u64) -> Result<Option<Uuid>> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT event_id FROM events WHERE session_id = ?1 AND json_extract(envelope_json, '$.signerDeviceId') = ?2 \
                 AND json_extract(envelope_json, '$.lamport') < ?3 ORDER BY row_id DESC LIMIT 1",
                rusqlite::params![session_id.to_string(), signer.to_string(), below.min(i64::MAX as u64) as i64],
                |r| r.get(0),
            )
            .optional()?;
        Ok(id.and_then(|s| Uuid::parse_str(&s).ok()))
    }

    /// The session ids of one workspace.
    pub fn sessions_of_workspace(&self, workspace_id: Uuid) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT session_id FROM events WHERE workspace_id = ?1")?;
        let rows = stmt.query_map([workspace_id.to_string()], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for s in rows {
            if let Ok(id) = Uuid::parse_str(&s?) {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Facts after row `after_row` and at or after `since_ms` (all
    /// workspaces), oldest first: the facts export's incremental read.
    #[allow(clippy::type_complexity)]
    pub fn facts_after(&self, after_row: i64, since_ms: i64) -> Result<Vec<(i64, String, String, i64, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT row_id, workspace_id, session_id, at_ms, kind, record_json FROM facts \
             WHERE row_id > ?1 AND at_ms >= ?2 ORDER BY row_id ASC",
        )?;
        let rows = stmt.query_map([after_row, since_ms], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Guard signals after row `after_row` and at or after `since_ms` (all
    /// workspaces), oldest first: (row, workspace, session, at_ms, signal_json).
    #[allow(clippy::type_complexity)]
    pub fn signals_after(&self, after_row: i64, since_ms: i64) -> Result<Vec<(i64, String, Option<String>, i64, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT row_id, workspace_id, session_id, at_ms, signal_json FROM guard_signals \
             WHERE row_id > ?1 AND at_ms >= ?2 ORDER BY row_id ASC",
        )?;
        let rows = stmt.query_map([after_row, since_ms], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Highest guard-signal row (0 when none).
    pub fn max_signal_row(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT COALESCE(MAX(row_id), 0) FROM guard_signals", [], |row| row.get(0))?)
    }

    /// Sessions with an event after row `after`, in first-changed order: what
    /// the facts export observes again.
    pub fn sessions_changed_after(&self, after: i64) -> Result<Vec<Uuid>> {
        let mut stmt = self
            .conn
            .prepare("SELECT session_id FROM events WHERE row_id > ?1 GROUP BY session_id ORDER BY MIN(row_id)")?;
        let rows = stmt.query_map([after], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for s in rows {
            if let Ok(id) = Uuid::parse_str(&s?) {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Highest equivocation-evidence row (0 when none).
    pub fn max_equivocation_row(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT COALESCE(MAX(row_id), 0) FROM equivocations", [], |row| row.get(0))?)
    }

    /// Sessions that gained equivocation evidence after row `after`.
    pub fn sessions_equivocated_after(&self, after: i64) -> Result<Vec<Uuid>> {
        let mut stmt = self
            .conn
            .prepare("SELECT session_id FROM equivocations WHERE row_id > ?1 GROUP BY session_id ORDER BY MIN(row_id)")?;
        let rows = stmt.query_map([after], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for s in rows {
            if let Ok(id) = Uuid::parse_str(&s?) {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Workspaces whose config log gained a trust event (the kinds of
    /// [`Self::trust_envelopes`]) after event row `after_event` or
    /// equivocation row `after_equivocation`: every chat of those
    /// workspaces is judged against a changed roster.
    pub fn trust_changed_workspaces_after(&self, after_event: i64, after_equivocation: i64) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare(
            "SELECT workspace_id, session_id FROM events WHERE row_id > ?1 AND kind IN (\
             'accountKeyRegistered','accountKeyRotated','deviceCertificateAdded','deviceRevoked',\
             'memberAdded','memberRemoved','sessionSnapshot','memberRoleChanged','dearthModeChanged','joinApproved',\
             'agentKeyCertified','agentUpserted','agentRosterUpdated','agentRemoved',\
             'workspaceRulesSet','passkeyRegistered','passkeyRemoved') \
             UNION SELECT workspace_id, session_id FROM equivocations WHERE row_id > ?2",
        )?;
        let rows = stmt.query_map([after_event, after_equivocation], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out: Vec<Uuid> = Vec::new();
        for row in rows {
            let (ws, sid) = row?;
            let (Ok(ws), Ok(sid)) = (Uuid::parse_str(&ws), Uuid::parse_str(&sid)) else { continue };
            if sid == hive_core::workspace_config_session_id(ws) && !out.contains(&ws) {
                out.push(ws);
            }
        }
        Ok(out)
    }

    /// Distinct session ids known to the store, in first-seen order.
    pub fn list_session_ids(&self) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id FROM events GROUP BY session_id ORDER BY MIN(row_id)",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for s in rows {
            if let Ok(id) = Uuid::parse_str(&s?) {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Project a session into its current state, replaying only from the latest
    /// `sessionSnapshot` so cost stays bounded on long chats (#3).
    pub fn load_session(&self, session_id: Uuid) -> Result<Option<ChatSession>> {
        self.load_session_with_founder(session_id, None, None)
    }

    /// [`load_session`], but pinned to a `trusted_founder` — the out-of-band
    /// workspace-founder account id from the `hivews1:` invite (B2 residual fix).
    /// Projection then rejects any base seed not authored by that founder, so a
    /// forged low-lamport `SessionSnapshot` from another keyholder cannot displace
    /// the founder's seed. `None` is exactly [`load_session`] (legacy / local /
    /// no-invite). `ChatService::load` supplies its pinned founder here.
    pub fn load_session_with_founder(
        &self,
        session_id: Uuid,
        trusted_founder: Option<&str>,
        // The device-local workspace id, if this session belongs to it — its events
        // skip ingest-time authz (private, single-device; see project_with_options).
        trusted_workspace: Option<Uuid>,
    ) -> Result<Option<ChatSession>> {
        self.load_session_authorized(session_id, trusted_founder, trusted_workspace, WorkspaceAuthz::Unchecked)
    }

    /// [`load_session_with_founder`] for a chat log, folded against its
    /// workspace's config-log roster history (`hive_core::project_authorized`).
    /// `ws` is the workspace authorization mode; for a timeline, cached per
    /// timeline fingerprint, and refolded while any event is held and the
    /// config log has grown since.
    pub fn load_session_authorized(
        &self,
        session_id: Uuid,
        trusted_founder: Option<&str>,
        trusted_workspace: Option<Uuid>,
        ws: WorkspaceAuthz<'_>,
    ) -> Result<Option<ChatSession>> {
        // Project the full stream: the fold orders canonically and picks its
        // base deterministically, so the result is independent of this
        // device's ingestion order.
        let sig: RowsSig = self.rows_sig(session_id)?;
        let (fingerprint, config_events) = match ws {
            WorkspaceAuthz::Unchecked => (0, 0),
            WorkspaceAuthz::NoConfigLog => (1, 0),
            WorkspaceAuthz::Timeline(t) => (t.fingerprint(), t.events_held()),
        };
        let key: ProjectionKey = (session_id, trusted_founder.map(str::to_string), trusted_workspace, fingerprint);
        if let Some((cached, cfg_seen, session)) = self.projections.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            let still_held = session.as_ref().is_some_and(|s| !s.authz.held.is_empty());
            if *cached == sig && (!still_held || *cfg_seen == config_events) {
                return Ok(session.clone());
            }
        }
        let envelopes = self.log_with_witnesses(session_id)?;
        let session = project_authorized(&envelopes, trusted_founder, trusted_workspace, ws);
        let mut cache = self.projections.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= PROJECTION_CACHE_MAX && !cache.contains_key(&key) {
            cache.clear();
        }
        cache.insert(key, (sig, config_events, session.clone()));
        Ok(session)
    }

    /// A workspace's config log folded, with its roster at every position
    /// (`hive_core::project_config`). Cached per config-log rows.
    pub fn load_config(
        &self,
        workspace_id: Uuid,
        trusted_founder: Option<&str>,
        trusted_workspace: Option<Uuid>,
    ) -> Result<(Option<ChatSession>, Arc<ConfigTimeline>)> {
        let config_id = hive_core::workspace_config_session_id(workspace_id);
        let sig: RowsSig = self.rows_sig(config_id)?;
        let key = (config_id, trusted_founder.map(str::to_string), trusted_workspace);
        if let Some((cached, session, timeline)) = self.configs.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            if *cached == sig {
                return Ok((session.clone(), Arc::clone(timeline)));
            }
        }
        let envelopes = self.log_with_witnesses(config_id)?;
        let (session, timeline) = project_config(&envelopes, trusted_founder, trusted_workspace);
        let timeline = Arc::new(timeline);
        let mut cache = self.configs.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= PROJECTION_CACHE_MAX && !cache.contains_key(&key) {
            cache.clear();
        }
        cache.insert(key, (sig, session.clone(), Arc::clone(&timeline)));
        Ok((session, timeline))
    }

    /// Pin a workspace's founder (first pin wins: a later invite can't move it).
    pub fn set_workspace_pin(&self, workspace_id: Uuid, founder: &str, fk: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO workspace_pins (workspace_id, founder, fk) VALUES (?1, ?2, ?3)",
            rusqlite::params![workspace_id.to_string(), founder, fk],
        )?;
        Ok(())
    }

    /// The founder pin of a workspace, if this device recorded one.
    pub fn workspace_pin(&self, workspace_id: Uuid) -> Result<Option<crate::envelope_verifier::FounderPin>> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT founder, fk FROM workspace_pins WHERE workspace_id = ?1",
                [workspace_id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row.and_then(|(f, k)| crate::envelope_verifier::FounderPin::parse(&f, &k)))
    }

    /// This device left `workspace_id`: drop its founder pin and the invite
    /// it redeemed. Its log then vouches for no one (co-members, verified
    /// keys, contact ids and links all come from pinned logs only); a rejoin
    /// pins again from the new invite.
    pub fn forget_workspace(&self, workspace_id: Uuid) -> Result<()> {
        let id = workspace_id.to_string();
        self.conn.execute("DELETE FROM workspace_pins WHERE workspace_id = ?1", [&id])?;
        self.conn.execute("DELETE FROM join_invites WHERE workspace_id = ?1", [&id])?;
        Ok(())
    }

    /// Every workspace this device pinned a founder for, in the order it
    /// pinned them (first joined first: the co-member directory trusts the
    /// earliest-pinned registration of an account's key).
    pub fn pinned_workspaces(&self) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare("SELECT workspace_id FROM workspace_pins ORDER BY rowid ASC")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            if let Ok(id) = Uuid::parse_str(&row?) {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// When a pinned workspace was founded: the timestamp of its founder's
    /// signed seed ([`crate::envelope_verifier::founder_seed`]). `None` without
    /// a pin or before (or without) a verified founding. Owner decision D12
    /// orders DMs by it, so no later event (a joiner's, a denied one, or one
    /// dated earlier by a skewed or hostile clock) can move it, and a forged
    /// DM never has one.
    pub fn config_founded_at(&self, workspace_id: Uuid) -> Result<Option<hive_core::Timestamp>> {
        let Some(pin) = self.workspace_pin(workspace_id)? else { return Ok(None) };
        let envs = self.list_copies(hive_core::workspace_config_session_id(workspace_id))?;
        Ok(crate::envelope_verifier::founder_seed(&envs, &pin).map(|e| e.timestamp))
    }

    /// The contact id (relay v0.3 drop box) `account` most recently stamped
    /// on an event in `workspace_id`, if any, counting only events signed by
    /// a device `signer_key` returns a key for (the caller's certified,
    /// non-revoked devices of `account`): anyone else can stamp any account
    /// on what they sign. A delivery address only: what is dropped there is
    /// sealed to keys verified on the config log.
    pub fn latest_contact_of(
        &self,
        workspace_id: Uuid,
        account: Uuid,
        signer_key: &dyn Fn(Uuid) -> Option<Vec<u8>>,
    ) -> Result<Option<String>> {
        let envs = self.query_envelopes(
            "SELECT envelope_json FROM (SELECT row_id, workspace_id, envelope_json FROM events \
             UNION ALL SELECT row_id + 9000000000000, workspace_id, envelope_json FROM equivocations) \
             WHERE workspace_id = ?1 AND envelope_json LIKE '%\"contact\":\"%' \
             ORDER BY row_id DESC LIMIT 200",
            [workspace_id.to_string()],
        )?;
        Ok(envs.into_iter().find_map(|e| {
            let key = e.signer_device_id.and_then(signer_key)?;
            hive_core::crypto::verify_envelope(&e, &key).ok()?;
            let a = e.actor_stamp?.actor;
            (a.account_id == Some(account)).then_some(a.contact).flatten()
        }))
    }

    /// The issue time of the newest contact record seen for `cid`.
    pub fn contact_newest_seen(&self, cid: &str) -> Result<Option<hive_core::Timestamp>> {
        let ms: Option<i64> = self
            .conn
            .query_row("SELECT issued_ms FROM contact_seen WHERE cid = ?1", [cid], |r| r.get(0))
            .optional()?;
        Ok(ms.and_then(|ms| time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok()).map(hive_core::Timestamp))
    }

    /// Record that a record issued at `issued` was seen for `cid` (only ever
    /// moves forward).
    pub fn note_contact_seen(&self, cid: &str, issued: hive_core::Timestamp) -> Result<()> {
        let ms = (issued.0.unix_timestamp_nanos() / 1_000_000) as i64;
        self.conn.execute(
            "INSERT INTO contact_seen (cid, issued_ms) VALUES (?1, ?2) \
             ON CONFLICT(cid) DO UPDATE SET issued_ms = MAX(issued_ms, excluded.issued_ms)",
            rusqlite::params![cid, ms],
        )?;
        Ok(())
    }

    /// Remember the invite this device redeems to join `workspace_id` (the
    /// latest one wins: a fresh invite replaces a used one).
    pub fn set_join_invite(&self, workspace_id: Uuid, invite_id: Uuid) -> Result<()> {
        self.conn.execute(
            "INSERT INTO join_invites (workspace_id, invite_id) VALUES (?1, ?2) \
             ON CONFLICT(workspace_id) DO UPDATE SET invite_id = excluded.invite_id",
            rusqlite::params![workspace_id.to_string(), invite_id.to_string()],
        )?;
        Ok(())
    }

    /// Keep the redeem key seed of the box admission this box redeems.
    pub fn set_join_redeem_key(&self, workspace_id: Uuid, seed: &[u8; 32]) -> Result<()> {
        let hex: String = seed.iter().map(|b| format!("{b:02x}")).collect();
        self.conn.execute(
            "INSERT INTO join_redeem_keys (workspace_id, seed) VALUES (?1, ?2) \
             ON CONFLICT(workspace_id) DO UPDATE SET seed = excluded.seed",
            rusqlite::params![workspace_id.to_string(), hex],
        )?;
        Ok(())
    }

    /// Forget the redeem key seed once the redemption it proved is written:
    /// it admits no one else, and is kept no longer than needed.
    pub fn clear_join_redeem_key(&self, workspace_id: Uuid) -> Result<()> {
        self.conn.execute("DELETE FROM join_redeem_keys WHERE workspace_id = ?1", [workspace_id.to_string()])?;
        Ok(())
    }

    /// The redeem key seed this box proves its admission with, if any.
    pub fn join_redeem_key(&self, workspace_id: Uuid) -> Result<Option<[u8; 32]>> {
        let v: Option<String> = self
            .conn
            .query_row("SELECT seed FROM join_redeem_keys WHERE workspace_id = ?1", [workspace_id.to_string()], |r| r.get(0))
            .optional()?;
        Ok(v.and_then(|h| crate::box_admission::seed_from_hex(&h)))
    }

    /// Every envelope of `workspace_id`'s config log (equivocations
    /// included), as `load_config` folds them.
    pub fn config_envelopes(&self, workspace_id: Uuid) -> Result<Vec<SessionEventEnvelope>> {
        let config_id = hive_core::workspace_config_session_id(workspace_id);
        self.query_envelopes_sealed(
            "SELECT envelope_json, sealed_epoch FROM events WHERE session_id = ?1 \
             UNION ALL SELECT envelope_json, sealed_epoch FROM equivocations WHERE session_id = ?1",
            [config_id.to_string()],
        )
    }

    /// The invite this device redeems to join `workspace_id`, if it has one.
    pub fn join_invite(&self, workspace_id: Uuid) -> Result<Option<Uuid>> {
        let v: Option<String> = self
            .conn
            .query_row("SELECT invite_id FROM join_invites WHERE workspace_id = ?1", [workspace_id.to_string()], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(v.and_then(|s| Uuid::parse_str(&s).ok()))
    }

    /// The workspace a stored session belongs to, if any of its rows exist.
    pub fn workspace_of(&self, session_id: Uuid) -> Result<Option<Uuid>> {
        let ws: Option<String> = self
            .conn
            .query_row("SELECT workspace_id FROM events WHERE session_id = ?1 LIMIT 1", [session_id.to_string()], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(ws.and_then(|w| Uuid::parse_str(&w).ok()))
    }

    /// Row id of the latest `sessionSnapshot` for a session, if any.
    fn latest_snapshot_row(&self, session_id: Uuid) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT MAX(row_id) FROM events WHERE session_id = ?1 AND kind = 'sessionSnapshot'",
                [session_id.to_string()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten())
    }

    /// How many events a session has accumulated since its latest snapshot —
    /// drives periodic re-snapshotting.
    pub fn rows_since_last_snapshot(&self, session_id: Uuid) -> Result<i64> {
        let n = match self.latest_snapshot_row(session_id)? {
            Some(row) => self.conn.query_row(
                "SELECT COUNT(*) FROM events WHERE session_id = ?1 AND row_id > ?2",
                rusqlite::params![session_id.to_string(), row],
                |r| r.get::<_, i64>(0),
            )?,
            None => self.conn.query_row(
                "SELECT COUNT(*) FROM events WHERE session_id = ?1",
                [session_id.to_string()],
                |r| r.get::<_, i64>(0),
            )?,
        };
        Ok(n)
    }

    /// Workspaces that hold events but aren't `keep` and that `own_device`
    /// never signed an event in, with their event counts — what
    /// [`purge_foreign_workspaces`](Self::purge_foreign_workspaces) would drop.
    /// The nil workspace (legacy local events) is never foreign.
    pub fn foreign_workspaces(&self, keep: &HashSet<Uuid>, own_device: Uuid) -> Result<Vec<(Uuid, usize)>> {
        let counts: Vec<(String, i64)> = {
            let mut stmt = self.conn.prepare("SELECT workspace_id, COUNT(*) FROM events GROUP BY workspace_id")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        // The device id as the envelope JSON spells it (`"signerDeviceId":"…"`).
        let signed_here = format!("\"signerDeviceId\":\"{own_device}\"");
        let mut out = Vec::new();
        for (raw, n) in counts {
            let Ok(ws) = Uuid::parse_str(&raw) else { continue };
            if ws.is_nil() || keep.contains(&ws) {
                continue;
            }
            let own: i64 = self.conn.query_row(
                "SELECT COUNT(*) FROM events WHERE workspace_id = ?1 AND instr(envelope_json, ?2) > 0",
                rusqlite::params![raw, signed_here],
                |r| r.get(0),
            )?;
            if own == 0 {
                out.push((ws, n as usize));
            }
        }
        Ok(out)
    }

    /// One-time cleanup after the room-scoping fix: delete every event of a
    /// workspace this device doesn't belong to (not in `keep`) and never
    /// authored anything in — events an older client pushed into a shared room
    /// regardless of their workspace. A workspace with any event signed by
    /// `own_device` is kept, so nothing this device wrote is ever dropped.
    ///
    /// With `backup`, the whole database is first copied there (`VACUUM INTO`)
    /// and nothing is deleted unless that copy succeeds. The sync bookkeeping
    /// (cursor, pushed set) of a room whose workspace is dropped goes too, so
    /// re-joining that room later pulls it from the start. Returns the dropped
    /// workspaces with their event counts (empty = nothing to do, no backup).
    pub fn purge_foreign_workspaces(
        &mut self,
        keep: &HashSet<Uuid>,
        own_device: Uuid,
        backup: Option<&Path>,
    ) -> Result<Vec<(Uuid, usize)>> {
        let doomed = self.foreign_workspaces(keep, own_device)?;
        if doomed.is_empty() {
            return Ok(doomed);
        }
        if let Some(path) = backup {
            self.conn.execute("VACUUM INTO ?1", [path.to_string_lossy()])?;
        }
        let rooms: Vec<String> = {
            let mut stmt = self.conn.prepare("SELECT workspace FROM sync_cursor UNION SELECT DISTINCT workspace FROM pushed")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let tx = self.conn.transaction()?;
        for (ws, _) in &doomed {
            let ws_s = ws.to_string();
            tx.execute(
                "DELETE FROM compaction_state WHERE session_id IN (SELECT DISTINCT session_id FROM events WHERE workspace_id = ?1)",
                [&ws_s],
            )?;
            tx.execute(
                "DELETE FROM pushed WHERE event_id IN (SELECT event_id FROM events WHERE workspace_id = ?1)",
                [&ws_s],
            )?;
            tx.execute("DELETE FROM events WHERE workspace_id = ?1", [&ws_s])?;
            tx.execute("DELETE FROM equivocations WHERE workspace_id = ?1", [&ws_s])?;
            for room in rooms.iter().filter(|r| hive_core::room_workspace_id(r) == *ws) {
                tx.execute("DELETE FROM sync_cursor WHERE workspace = ?1", [room])?;
                tx.execute("DELETE FROM pushed WHERE workspace = ?1", [room])?;
            }
        }
        tx.commit()?;
        self.projections.lock().unwrap_or_else(|p| p.into_inner()).clear();
        self.configs.lock().unwrap_or_else(|p| p.into_inner()).clear();
        Ok(doomed)
    }

    /// One-time maintenance: delete `messageChunkReceived` rows whose message
    /// already has a `messageCompleted` (the completed body supersedes them).
    /// Shrinks DBs written before per-token persistence was removed; projection
    /// is unaffected. A pruned chunk stays a lamport witness (its id, lamport,
    /// log head and chain link, `hive_core::lamport`): an event here or one
    /// still to arrive from a device that held the chunk may count from it,
    /// and without it that event would be held. Returns rows removed.
    pub fn prune_superseded_chunks(&mut self) -> Result<usize> {
        fn message_id_of(json: &str) -> Option<String> {
            // SessionEvent variant fields keep snake_case (the enum's
            // rename_all only renames variant tags, not their fields).
            serde_json::from_str::<serde_json::Value>(json)
                .ok()?
                .pointer("/payload/message_id")?
                .as_str()
                .map(str::to_owned)
        }

        let mut completed: std::collections::HashSet<String> = std::collections::HashSet::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT envelope_json FROM events WHERE kind = 'messageCompleted'")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            for j in rows {
                if let Some(id) = message_id_of(&j?) {
                    completed.insert(id);
                }
            }
        }
        if completed.is_empty() {
            return Ok(0);
        }

        let mut doomed: Vec<String> = Vec::new(); // event_ids
        let mut witnesses: Vec<SessionEventEnvelope> = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT event_id, envelope_json FROM events WHERE kind = 'messageChunkReceived'",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows {
                let (event_id, json) = row?;
                if message_id_of(&json).map(|m| completed.contains(&m)).unwrap_or(false) {
                    if let Ok(env) = serde_json::from_str::<SessionEventEnvelope>(&json) {
                        witnesses.push(env);
                    }
                    doomed.push(event_id);
                }
            }
        }

        // Every other copy held under a pruned id goes with it (left alone,
        // one would fold in the pruned chunk's place), a witness like it.
        for event_id in &doomed {
            for env in self.query_envelopes("SELECT envelope_json FROM equivocations WHERE event_id = ?1", [event_id])? {
                witnesses.push(env);
            }
        }
        for env in &witnesses {
            self.note_lamport_witness(env)?;
        }
        let tx = self.conn.transaction()?;
        let mut removed = 0;
        for event_id in &doomed {
            removed += tx.execute("DELETE FROM events WHERE event_id = ?1", [event_id])?;
            tx.execute("DELETE FROM equivocations WHERE event_id = ?1", [event_id])?;
        }
        tx.commit()?;
        Ok(removed)
    }
}

/// Tests only: the device id every fixture event is signed by.
#[cfg(test)]
pub(crate) const FIXTURE_DEVICE: Uuid = Uuid::from_u128(0x6669_7874_7572_6500_0000_0000_0000_0001);

/// Tests only: `env` as the next event of its log after `log_head`: an
/// authorless stamp (a `System` actor with an empty id, no account) naming
/// the head, the lamport one past it (`hive_core::lamport`), signed by the
/// fixture device (a genuine event is always signed, and an unsigned one
/// can't carry a stamp past ingest).
#[cfg(test)]
pub(crate) fn fixture_next(env: &SessionEventEnvelope, log_head: Option<hive_core::CfgFrontier>) -> SessionEventEnvelope {
    let mut env = env.clone();
    env.clear_raw();
    if env.actor_stamp.is_none() {
        env.actor_stamp = Some(hive_core::ActorStamp {
            actor: hive_core::ActorIdentity::new("", "", hive_core::ActorKind::System),
            recorded_at: hive_core::Timestamp::epoch(),
            cfg_frontier: None,
            presence: None,
            runtime_effect: false,
            prev: None,
            sig_alg: Default::default(),
            log_head: None,
        });
    }
    hive_core::lamport::stamp_next(&mut env, log_head);
    let kp = hive_core::SigningKeypair::from_seed(&[0x46; 32]).expect("fixture key");
    hive_core::sign_envelope(&mut env, FIXTURE_DEVICE, &kp);
    env
}

/// Tests only: `env` signed as the fixture device, as is (a genuine event is
/// always signed; ingest refuses an unsigned one).
#[cfg(test)]
pub(crate) fn fixture_signed(env: &SessionEventEnvelope) -> SessionEventEnvelope {
    let mut env = env.clone();
    env.clear_raw();
    let kp = hive_core::SigningKeypair::from_seed(&[0x46; 32]).expect("fixture key");
    hive_core::sign_envelope(&mut env, FIXTURE_DEVICE, &kp);
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::{ChatMessage, MessageRole};

    fn seeded_store() -> (EventStore, Uuid, Uuid) {
        let mut store = EventStore::open_in_memory().unwrap();
        let session = ChatSession::new("Demo", Uuid::nil(), "anthropic");
        let sid = session.id;
        let wid = session.workspace_id;
        store
            .append(
                sid,
                wid,
                SessionEvent::SessionSnapshot {
                    session: Box::new(session),
                },
            )
            .unwrap();
        (store, sid, wid)
    }

    #[test]
    fn append_assigns_monotonic_sequences() {
        let (mut store, sid, wid) = seeded_store();
        let e2 = store
            .append(
                sid,
                wid,
                SessionEvent::MessageAppended {
                    message: ChatMessage::new(MessageRole::User, "Mara", "hi"),
                },
            )
            .unwrap();
        let e3 = store
            .append(
                sid,
                wid,
                SessionEvent::MessageAppended {
                    message: ChatMessage::new(MessageRole::Assistant, "Hive", "hello"),
                },
            )
            .unwrap();
        assert_eq!(e2.sequence, 2);
        assert_eq!(e3.sequence, 3);
        assert_eq!(store.max_sequence(sid).unwrap(), Some(3));
    }

    #[test]
    fn messages_after_lists_every_message_a_pull_added_in_order() {
        let (mut store, sid, wid) = seeded_store();
        let before = store.max_row_id().unwrap();
        let a = ChatMessage::new(MessageRole::User, "Alex", "@hive status?");
        let b = ChatMessage::new(MessageRole::User, "Alex", "@coder run the tests");
        let (a_id, b_id) = (a.id, b.id);
        store.append(sid, wid, SessionEvent::MessageAppended { message: a }).unwrap();
        store.append(sid, wid, SessionEvent::SessionTitleChanged { title: "x".into() }).unwrap();
        store.append(sid, wid, SessionEvent::MessageAppended { message: b }).unwrap();
        let fixture = Some(FIXTURE_DEVICE);
        assert_eq!(store.messages_after(before).unwrap(), vec![(sid, a_id, fixture), (sid, b_id, fixture)]);
        assert!(store.messages_after(store.max_row_id().unwrap()).unwrap().is_empty());
    }

    #[test]
    fn the_lamport_head_is_the_newest_event_within_bounds() {
        let (mut store, sid, wid) = seeded_store(); // snapshot @ lamport 1
        let seed = store.list(sid).unwrap()[0].clone();
        let stamped = |payload: SessionEvent, head: Option<&SessionEventEnvelope>| {
            let mut e = SessionEventEnvelope::new(sid, wid, 1, payload);
            e.actor_stamp = seed.actor_stamp.clone();
            hive_core::lamport::stamp_next(&mut e, head.map(|h| hive_core::CfgFrontier { lamport: h.lamport, event_id: h.event_id }));
            e
        };
        let a = stamped(SessionEvent::SessionTitleChanged { title: "a".into() }, Some(&seed));
        let b = stamped(SessionEvent::SessionTitleChanged { title: "b".into() }, Some(&a));
        store.ingest(&b).unwrap();
        assert_eq!(store.lamport_head(sid).unwrap().map(|h| h.event_id), Some(seed.event_id), "b's head isn't here yet");
        store.ingest(&a).unwrap();
        assert_eq!(store.lamport_head(sid).unwrap().map(|h| h.event_id), Some(b.event_id), "ingested in any order");
        // Inflated: a lamport no head accounts for never moves the head.
        let mut big = stamped(SessionEvent::SessionTitleChanged { title: "big".into() }, Some(&b));
        big.lamport = u64::MAX;
        store.ingest(&big).unwrap();
        let mut headless = SessionEventEnvelope::new(sid, wid, 7, SessionEvent::SessionTitleChanged { title: "x".into() });
        headless.lamport = 1_000;
        store.ingest(&headless).unwrap();
        assert_eq!(store.lamport_head(sid).unwrap().map(|h| (h.lamport, h.event_id)), Some((3, b.event_id)));
        // An event appended here moves it.
        let mine = store.append(sid, wid, SessionEvent::SessionTitleChanged { title: "mine".into() }).unwrap();
        assert_eq!(mine.lamport, 4);
        assert_eq!(store.lamport_head(sid).unwrap().map(|h| h.event_id), Some(mine.event_id));
    }

    /// A device never counts its own next event from a witness: a refused
    /// event isn't pushed, so a keyholder who sends one to this device alone
    /// (a peer link, a relay serving it to one device) would otherwise get
    /// this device's honest events held on every device that never met it.
    #[test]
    fn a_device_counts_from_what_it_stored_never_from_a_witness() {
        let (mut a, sid, wid) = seeded_store();
        let seed = a.list(sid).unwrap()[0].clone();
        let mut b = EventStore::open_in_memory().unwrap();
        b.ingest(&seed).unwrap();
        let mut junk = SessionEventEnvelope::new(sid, wid, 1, SessionEvent::SessionTitleChanged { title: "junk".into() });
        junk.actor_stamp = seed.actor_stamp.clone();
        hive_core::lamport::stamp_next(&mut junk, Some(hive_core::CfgFrontier { lamport: seed.lamport, event_id: seed.event_id }));
        a.note_lamport_witness(&junk).unwrap();
        let mine = a.append(sid, wid, SessionEvent::SessionTitleChanged { title: "mine".into() }).unwrap();
        b.ingest(&mine).unwrap();
        assert_eq!(b.lamport_head(sid).unwrap().map(|h| h.event_id), Some(mine.event_id), "not held where the junk never went");
        assert_eq!(b.load_session(sid).unwrap().unwrap().title, "mine");
    }

    /// A witness is kept by its whole structure (id, lamport, head, chain
    /// link), and every witness counts whether or not the id is stored: a
    /// keyholder's refused copies of an event's id (one at its lamport naming
    /// a head nobody holds, met first; one a lamport later) must not make a
    /// device that refused the event judge the log differently from one that
    /// accepted it. A witness never folds, so it never erases the stored
    /// event it shares an id with.
    #[test]
    fn devices_that_refused_different_copies_of_an_id_judge_the_log_alike() {
        let (a0, sid, wid) = seeded_store();
        let seed = a0.list(sid).unwrap()[0].clone();
        let at = |e: &SessionEventEnvelope| Some(hive_core::CfgFrontier { lamport: e.lamport, event_id: e.event_id });
        let stamped = |title: &str, head: Option<hive_core::CfgFrontier>| {
            let mut e = SessionEventEnvelope::new(sid, wid, 1, SessionEvent::SessionTitleChanged { title: title.into() });
            e.actor_stamp = seed.actor_stamp.clone();
            hive_core::lamport::stamp_next(&mut e, head);
            e
        };
        let x = stamped("x", at(&seed));
        let y = stamped("y", at(&seed));
        let mut early = stamped("forged", Some(hive_core::CfgFrontier { lamport: 1, event_id: Uuid::new_v4() }));
        early.event_id = x.event_id;
        let mut late = stamped("forged", at(&y));
        late.event_id = x.event_id;
        let g = stamped("g", Some(hive_core::CfgFrontier { lamport: late.lamport, event_id: x.event_id }));
        let h = stamped("h", at(&g));
        let after_x = stamped("after x", at(&x));

        // A accepted x; it alone: the witness copy doesn't erase it.
        let mut a = a0;
        a.ingest(&x).unwrap();
        a.note_lamport_witness(&late).unwrap();
        assert_eq!(a.load_session(sid).unwrap().unwrap().title, "x", "a witness never folds against the stored event");
        a.ingest(&y).unwrap();
        a.note_lamport_witness(&early).unwrap();
        // B refused x (its roster said so), after meeting the early copy.
        let mut b = EventStore::open_in_memory().unwrap();
        b.ingest(&seed).unwrap();
        b.ingest(&y).unwrap();
        b.note_lamport_witness(&early).unwrap();
        b.note_lamport_witness(&x).unwrap();
        b.note_lamport_witness(&late).unwrap();
        for s in [&mut a, &mut b] {
            for e in [&g, &h, &after_x] {
                s.ingest(e).unwrap();
            }
        }
        assert_eq!(a.lamport_head(sid).unwrap(), b.lamport_head(sid).unwrap(), "one head");
        assert_eq!(a.lamport_head(sid).unwrap().map(|k| k.event_id), Some(h.event_id));
        let (sa, sb) = (a.load_session(sid).unwrap().unwrap(), b.load_session(sid).unwrap().unwrap());
        assert_eq!(sa.title, sb.title, "one fold");
        assert_eq!(sa.authz.held, sb.authz.held, "one set of holds");
    }

    #[test]
    /// Review P-A2c (owner decision): two envelopes under one event id with
    /// different content are both denied on every device, whichever arrived
    /// first; the store keeps the second as evidence instead of dropping it.
    fn an_equivocated_event_id_is_denied_whichever_copy_arrives_first() {
        let base = ChatSession::new("Demo", Uuid::nil(), "anthropic");
        let (sid, wid) = (base.id, base.workspace_id);
        let seed = SessionEventEnvelope::new(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(base) });
        let first = SessionEventEnvelope::new(sid, wid, 2, SessionEvent::SessionTitleChanged { title: "First".into() });
        let mut a_side = SessionEventEnvelope::new(sid, wid, 3, SessionEvent::SessionTitleChanged { title: "Told to A".into() });
        a_side.lamport = 3;
        let [seed, first, a_side]: [SessionEventEnvelope; 3] = hive_core::testkit::honest_chain(&[seed, first, a_side]).try_into().unwrap();
        let mut b_side = a_side.clone();
        b_side.payload = SessionEvent::SessionTitleChanged { title: "Told to B".into() };
        b_side.clear_raw();
        let load = |order: &[&SessionEventEnvelope]| {
            let mut st = EventStore::open_in_memory().unwrap();
            for e in order {
                st.ingest(e).unwrap();
            }
            let s = st.load_session(sid).unwrap().unwrap();
            // A second load hits the cache: it must agree.
            assert_eq!(st.load_session(sid).unwrap().unwrap(), s);
            s
        };
        let x = load(&[&seed, &first, &a_side, &b_side]);
        let y = load(&[&seed, &first, &b_side, &a_side]);
        assert_eq!(x, y, "stores diverged under different arrival order");
        assert_eq!(x.title, "First", "neither side of the equivocation folds");
        // An identical copy is not an equivocation.
        let z = load(&[&seed, &first, &a_side, &a_side]);
        assert_eq!(z.title, "Told to A");
    }

    #[test]
    fn load_session_converges_regardless_of_ingest_order() {
        use hive_core::identity::{ActorIdentity, ActorKind, WorkspaceMember, WorkspaceRole};
        use hive_core::time_util::Timestamp;

        let base = ChatSession::new("Demo", Uuid::nil(), "anthropic");
        let (sid, wid) = (base.id, base.workspace_id);
        let member = WorkspaceMember {
            id: "m1".into(),
            actor: ActorIdentity::new("u1", "U1", ActorKind::Human),
            role: WorkspaceRole::Contributor,
            title: String::new(),
            index: 0,
            joined_at: Timestamp::epoch(),
        };
        let events = vec![
            SessionEventEnvelope::new(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(base) }),
            SessionEventEnvelope::new(sid, wid, 2, SessionEvent::SessionTitleChanged { title: "First".into() }),
            SessionEventEnvelope::new(sid, wid, 3, SessionEvent::MemberAdded { member, invite: None, proof: None }),
            SessionEventEnvelope::new(sid, wid, 4, SessionEvent::SessionTitleChanged { title: "Second".into() }),
            SessionEventEnvelope::new(sid, wid, 5, SessionEvent::MemberRemoved { member_id: "m1".into(), watermark: Vec::new() }),
        ];
        let events = hive_core::testkit::honest_chain(&events);

        // Device A ingests forward; device B ingests in reverse — so B stores the
        // snapshot LAST (highest row_id). The old row_id-sliced load would have
        // projected only the snapshot on B and diverged.
        let mut a = EventStore::open_in_memory().unwrap();
        for e in &events {
            a.ingest(e).unwrap();
        }
        let mut b = EventStore::open_in_memory().unwrap();
        for e in events.iter().rev() {
            b.ingest(e).unwrap();
        }

        let sa = a.load_session(sid).unwrap().expect("a");
        let sb = b.load_session(sid).unwrap().expect("b");
        assert_eq!(sa, sb, "stores diverged under different ingest order");
        assert_eq!(sa.title, "Second", "canonical-latest title wins");
        assert!(sa.members.is_empty(), "m1 added then removed");
    }

    /// The projection cache never serves a stale session: an append (also one
    /// made through another connection to the same file) or a delete re-folds.
    #[test]
    fn cached_projection_follows_appends_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hive.db");
        let mut store = EventStore::open(&path).unwrap();
        let session = ChatSession::new("Demo", Uuid::nil(), "anthropic");
        let (sid, wid) = (session.id, session.workspace_id);
        store.append(sid, wid, SessionEvent::SessionSnapshot { session: Box::new(session) }).unwrap();
        let msg = |body: &str| SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "Mara", body) };
        store.append(sid, wid, msg("one")).unwrap();
        assert_eq!(store.load_session(sid).unwrap().unwrap().messages.len(), 1);
        assert_eq!(store.load_session(sid).unwrap().unwrap().messages.len(), 1, "cache hit");

        let mut other = EventStore::open(&path).unwrap();
        other.append(sid, wid, msg("two")).unwrap();
        let bodies: Vec<_> = store.load_session(sid).unwrap().unwrap().messages.into_iter().map(|m| m.body).collect();
        assert_eq!(bodies, ["one", "two"], "a write through another connection is seen");

        store.delete_session(sid).unwrap();
        assert!(store.load_session(sid).unwrap().is_none(), "a delete is seen");
    }

    #[test]
    fn envelopes_after_row_returns_only_newer_rows() {
        let (mut store, sid, wid) = seeded_store();
        let mark = store.max_row_id().unwrap();
        assert_eq!(store.envelopes_after_row(mark).unwrap(), (mark, vec![]));
        let e = store
            .append(sid, wid, SessionEvent::SessionTitleChanged { title: "t".into() })
            .unwrap();
        let (last, envs) = store.envelopes_after_row(mark).unwrap();
        assert_eq!(last, mark + 1);
        assert_eq!(envs.iter().map(|e| e.event_id).collect::<Vec<_>>(), [e.event_id]);
    }

    #[test]
    fn load_session_projects_appended_messages() {
        let (mut store, sid, wid) = seeded_store();
        store
            .append(
                sid,
                wid,
                SessionEvent::MessageAppended {
                    message: ChatMessage::new(MessageRole::User, "Mara", "hi"),
                },
            )
            .unwrap();
        let session = store.load_session(sid).unwrap().expect("session");
        assert_eq!(session.title, "Demo");
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.messages[0].body, "hi");
    }

    #[test]
    fn list_after_returns_only_newer_events() {
        let (mut store, sid, wid) = seeded_store();
        for i in 0..3 {
            store
                .append(
                    sid,
                    wid,
                    SessionEvent::MessageAppended {
                        message: ChatMessage::new(MessageRole::User, "u", format!("m{i}")),
                    },
                )
                .unwrap();
        }
        // snapshot is seq 1; messages are 2,3,4
        let after_2 = store.list_after(sid, 2).unwrap();
        assert_eq!(after_2.len(), 2);
        assert_eq!(after_2[0].sequence, 3);
    }

    #[test]
    fn duplicate_sequence_is_rejected() {
        let (store, sid, wid) = seeded_store();
        // snapshot already occupies sequence 1
        let dup = SessionEventEnvelope::new(
            sid,
            wid,
            1,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "u", "dup"),
            },
        );
        assert!(store.append_envelope(&dup).is_err());
    }

    /// A newer build's envelope (an unknown top-level key, an unknown kind with
    /// nested data) is stored exactly as received and listed back exactly so,
    /// which is what lets it be re-sent to peers and refolded after an upgrade.
    #[test]
    fn ingest_stores_the_received_bytes() {
        let mut store = EventStore::open_in_memory().unwrap();
        let sid = Uuid::new_v4();
        let base = SessionEventEnvelope::new(sid, Uuid::nil(), 1, SessionEvent::MessageRemoved { message_id: Uuid::nil() });
        let mut v = serde_json::to_value(&base).unwrap();
        v["payload"] = serde_json::json!({"kind": "zzNewKind", "deep": {"x": [1.5, "✓"]}, "order": "kept"});
        v["zzTop"] = serde_json::json!({"b": 1, "a": 2});
        let wire = v.to_string().replace("\"zzTop\"", " \"zzTop\" ");
        let env: SessionEventEnvelope = serde_json::from_str(&wire).unwrap();
        assert_eq!(env.payload, SessionEvent::Unknown);
        assert!(store.ingest(&env).unwrap());
        let stored: String = store
            .conn
            .query_row("SELECT envelope_json FROM events WHERE event_id = ?1", [env.event_id.to_string()], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, wire);
        let listed = store.list(sid).unwrap();
        assert_eq!(serde_json::to_string(&listed[0]).unwrap(), wire);
    }

    #[test]
    fn workspace_pins_persist_and_the_first_pin_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hive.db");
        let ws = Uuid::new_v4();
        let founder = Uuid::new_v4().to_string();
        let fk = crate::envelope_verifier::FounderPin::fingerprint_hex(&[7u8; 32]);
        {
            let store = EventStore::open(&path).unwrap();
            assert!(store.workspace_pin(ws).unwrap().is_none());
            store.set_workspace_pin(ws, &founder, &fk).unwrap();
            // A later invite can't move the pin.
            store.set_workspace_pin(ws, &Uuid::new_v4().to_string(), &"ab".repeat(16)).unwrap();
        }
        let pin = EventStore::open(&path).unwrap().workspace_pin(ws).unwrap().unwrap();
        assert_eq!(pin.founder_account.to_string(), founder);
        assert_eq!(pin.fk, crate::envelope_verifier::FounderPin::fingerprint(&[7u8; 32]));
    }

    #[test]
    fn persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.db");
        let (sid, wid);
        {
            let mut store = EventStore::open(&path).unwrap();
            let session = ChatSession::new("Persisted", Uuid::nil(), "anthropic");
            sid = session.id;
            wid = session.workspace_id;
            store
                .append(
                    sid,
                    wid,
                    SessionEvent::SessionSnapshot {
                        session: Box::new(session),
                    },
                )
                .unwrap();
            store
                .append(
                    sid,
                    wid,
                    SessionEvent::MessageAppended {
                        message: ChatMessage::new(MessageRole::User, "Mara", "persist me"),
                    },
                )
                .unwrap();
        }
        let store = EventStore::open(&path).unwrap();
        let session = store.load_session(sid).unwrap().expect("session");
        assert_eq!(session.title, "Persisted");
        assert_eq!(session.messages[0].body, "persist me");
        let _ = wid;
    }

    #[test]
    fn load_replays_from_latest_snapshot() {
        let (mut store, sid, wid) = seeded_store();
        for i in 0..3 {
            store
                .append(
                    sid,
                    wid,
                    SessionEvent::MessageAppended {
                        message: ChatMessage::new(MessageRole::User, "u", format!("m{i}")),
                    },
                )
                .unwrap();
        }
        // Re-snapshot current state, then add one more message.
        let snap = store.load_session(sid).unwrap().unwrap();
        store
            .append(sid, wid, SessionEvent::SessionSnapshot { session: Box::new(snap) })
            .unwrap();
        store
            .append(
                sid,
                wid,
                SessionEvent::MessageAppended {
                    message: ChatMessage::new(MessageRole::User, "u", "after"),
                },
            )
            .unwrap();

        // One event (the "after" message) has landed since the snapshot, and a
        // full projection still reconstructs the complete state.
        assert_eq!(store.rows_since_last_snapshot(sid).unwrap(), 1);
        let s = store.load_session(sid).unwrap().unwrap();
        assert_eq!(s.messages.len(), 4);
        assert_eq!(s.messages[3].body, "after");
    }

    #[test]
    fn sync_cursor_and_pushed_are_durable_per_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.db");
        let (a, b) = ("room-a", "room-b");
        let e1 = Uuid::new_v4();
        let e2 = Uuid::new_v4();
        {
            let store = EventStore::open(&path).unwrap();
            assert_eq!(store.sync_cursor(a).unwrap(), 0, "unknown workspace starts at 0");
            store.set_sync_cursor(a, 42).unwrap();
            store.set_sync_cursor(a, 99).unwrap(); // upsert
            store.set_sync_cursor(b, 7).unwrap();
            store.mark_pushed(a, e1).unwrap();
            store.mark_pushed(a, e1).unwrap(); // idempotent
            store.mark_pushed(a, e2).unwrap();
        }
        // Reopen: state survived the process boundary.
        let store = EventStore::open(&path).unwrap();
        assert_eq!(store.sync_cursor(a).unwrap(), 99, "cursor persisted + upserted");
        assert_eq!(store.sync_cursor(b).unwrap(), 7, "keyed per workspace");
        let mut ids = store.pushed_ids(a).unwrap();
        ids.sort();
        let mut want = vec![e1, e2];
        want.sort();
        assert_eq!(ids, want, "pushed set persisted, deduped");
        assert!(store.pushed_ids(b).unwrap().is_empty(), "pushed set is per workspace");
    }

    #[test]
    fn compaction_state_survives_reopen_is_keyed_per_runtime_and_stays_out_of_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.db");
        let sid = Uuid::new_v4();
        let other = Uuid::new_v4();
        let state = StoredCompaction {
            boundary: Some(Uuid::new_v4()),
            covered: vec![Uuid::new_v4(), Uuid::new_v4()],
            fingerprint: "abc".into(),
            summary: "They agreed on X.".into(),
        };
        {
            let store = EventStore::open(&path).unwrap();
            assert_eq!(store.compaction(sid, "gateway").unwrap(), None);
            store.save_compaction(sid, "gateway", &state).unwrap();
            let mut first = state.clone();
            first.summary = "older".into();
            store.save_compaction(sid, "hosted", &first).unwrap();
            store.save_compaction(other, "gateway", &first).unwrap();
            // Upsert.
            store.save_compaction(sid, "hosted", &StoredCompaction { boundary: None, ..state.clone() }).unwrap();
        }
        let store = EventStore::open(&path).unwrap();
        assert_eq!(store.compaction(sid, "gateway").unwrap().as_ref(), Some(&state), "survived a restart");
        assert_eq!(store.compaction(sid, "hosted").unwrap().unwrap().boundary, None);
        // Device-local: nothing reached the event log.
        assert!(store.list(sid).unwrap().is_empty());
        assert!(store.list_session_ids().unwrap().is_empty());

        store.delete_compaction(sid, Some("hosted")).unwrap();
        assert!(store.compaction(sid, "hosted").unwrap().is_none());
        assert!(store.compaction(sid, "gateway").unwrap().is_some());
        // Deleting the chat drops its state; other chats keep theirs.
        store.delete_session(sid).unwrap();
        assert!(store.compaction(sid, "gateway").unwrap().is_none());
        assert!(store.compaction(other, "gateway").unwrap().is_some());
    }

    #[test]
    fn prune_drops_chunks_for_completed_messages() {
        let (mut store, sid, wid) = seeded_store();
        let mut placeholder = ChatMessage::new(MessageRole::Assistant, "Hive", "");
        placeholder.is_streaming = true;
        let mid = placeholder.id;
        let placeholder = store
            .append(sid, wid, SessionEvent::MessageAppended { message: placeholder })
            .unwrap();
        // Two chunks streamed concurrently (both after the placeholder): the
        // completion counts from one of them, which stays a witness.
        let at = Some(hive_core::CfgFrontier { lamport: placeholder.lamport, event_id: placeholder.event_id });
        for piece in ["He", "llo"] {
            let chunk = SessionEventEnvelope::new(sid, wid, 9, SessionEvent::MessageChunkReceived { message_id: mid, chunk: piece.into() });
            store.ingest(&fixture_next(&chunk, at)).unwrap();
        }
        store
            .append(
                sid,
                wid,
                SessionEvent::MessageCompleted { message_id: mid, body: "Hello".into() },
            )
            .unwrap();

        let removed = store.prune_superseded_chunks().unwrap();
        assert_eq!(removed, 2, "both chunks are superseded by completed");
        assert!(store.lamport_head(sid).unwrap().is_some_and(|h| h.lamport == 4), "the completion still counts from its chunk");
        // projection unchanged
        let s = store.load_session(sid).unwrap().unwrap();
        assert_eq!(s.messages.iter().find(|m| m.id == mid).unwrap().body, "Hello");
        // idempotent
        assert_eq!(store.prune_superseded_chunks().unwrap(), 0);
    }

    /// Another copy held under a pruned chunk's id goes with it (as a
    /// witness), and a deleted session leaves no copy behind to fold as it.
    #[test]
    fn copies_follow_their_event_out_of_the_store() {
        let (mut store, sid, wid) = seeded_store();
        let mut placeholder = ChatMessage::new(MessageRole::Assistant, "Hive", "");
        placeholder.is_streaming = true;
        let mid = placeholder.id;
        store.append(sid, wid, SessionEvent::MessageAppended { message: placeholder }).unwrap();
        let chunk = store.append(sid, wid, SessionEvent::MessageChunkReceived { message_id: mid, chunk: "Hel".into() }).unwrap();
        // Another signer's copy under the chunk's id, kept beside it.
        let mut other = chunk.clone();
        other.payload = SessionEvent::MessageChunkReceived { message_id: mid, chunk: "lo, forged".into() };
        other.clear_raw();
        let kp = hive_core::SigningKeypair::generate().unwrap();
        hive_core::sign_envelope(&mut other, Uuid::new_v4(), &kp);
        store.ingest(&other).unwrap();
        assert_eq!(store.list_copies(sid).unwrap().iter().filter(|e| e.event_id == chunk.event_id).count(), 2);
        store.append(sid, wid, SessionEvent::MessageCompleted { message_id: mid, body: "Hello".into() }).unwrap();
        assert!(store.prune_superseded_chunks().unwrap() >= 1);
        assert!(store.list_copies(sid).unwrap().iter().all(|e| e.event_id != chunk.event_id), "a copy outlived its pruned chunk");
        let s = store.load_session(sid).unwrap().unwrap();
        assert_eq!(s.messages.iter().find(|m| m.id == mid).unwrap().body, "Hello");

        // A copy of the seed outliving a deleted session would fold as it.
        let seed = store.list(sid).unwrap().remove(0);
        let mut copy = seed.clone();
        copy.clear_raw();
        hive_core::sign_envelope(&mut copy, Uuid::new_v4(), &kp);
        store.ingest(&copy).unwrap();
        store.delete_session(sid).unwrap();
        assert!(store.list_copies(sid).unwrap().is_empty());
        assert!(store.load_session(sid).unwrap().is_none(), "the deleted session came back");
    }

    /// A pruned chunk stays a lamport witness: another device may have held
    /// it and counted from it, and its event can arrive after the prune.
    #[test]
    fn an_event_counting_from_a_pruned_chunk_still_folds() {
        let (mut store, sid, wid) = seeded_store();
        let mut placeholder = ChatMessage::new(MessageRole::Assistant, "Hive", "");
        placeholder.is_streaming = true;
        let mid = placeholder.id;
        let placeholder = store.append(sid, wid, SessionEvent::MessageAppended { message: placeholder }).unwrap();
        let chunk = store.append(sid, wid, SessionEvent::MessageChunkReceived { message_id: mid, chunk: "Hel".into() }).unwrap();
        let key = |e: &SessionEventEnvelope| Some(hive_core::CfgFrontier { lamport: e.lamport, event_id: e.event_id });
        let at_chunk = key(&chunk);
        // This device completes the message counting from something newer it
        // held (two concurrent title changes), so nothing here names the chunk.
        let t1 = fixture_next(&SessionEventEnvelope::new(sid, wid, 9, SessionEvent::SessionTitleChanged { title: "t1".into() }), key(&placeholder));
        let t2 = fixture_next(&SessionEventEnvelope::new(sid, wid, 9, SessionEvent::SessionTitleChanged { title: "t2".into() }), key(&t1));
        store.ingest(&t1).unwrap();
        store.ingest(&t2).unwrap();
        store.append(sid, wid, SessionEvent::MessageCompleted { message_id: mid, body: "Hello".into() }).unwrap();
        // Meanwhile another device held the chunk as its newest event.
        let theirs = SessionEventEnvelope::new(sid, wid, 9, SessionEvent::SessionTitleChanged { title: "theirs".into() });
        let theirs = fixture_next(&theirs, at_chunk);
        assert!(store.prune_superseded_chunks().unwrap() >= 1);
        assert!(!store.has_event(chunk.event_id).unwrap(), "the chunk is pruned");
        store.ingest(&theirs).unwrap();
        let head = store.lamport_head(sid).unwrap().unwrap();
        let s = store.load_session(sid).unwrap().unwrap();
        assert!(s.authz.held.iter().all(|h| h.event_id != theirs.event_id), "not held for the pruned chunk");
        assert!(head.lamport >= theirs.lamport);
    }

    /// The cleanup for events an older client pushed into a room regardless of
    /// their workspace: another workspace's events go (after a backup), this
    /// device's own work never does.
    #[test]
    fn purge_drops_only_foreign_workspaces_after_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = EventStore::open(dir.path().join("hive.db")).unwrap();
        let me = Uuid::new_v4();
        let room = "room-alpha";
        let room_ws = hive_core::room_workspace_id(room);
        let leaked_room = "room-beta";
        let leaked_ws = hive_core::room_workspace_id(leaked_room);
        let other_local = Uuid::new_v4();
        let mine_elsewhere = Uuid::new_v4();
        let msg = |ws: Uuid, signer: Option<Uuid>| {
            let mut e = SessionEventEnvelope::new(
                Uuid::new_v4(),
                ws,
                1,
                SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "Alex", "hi") },
            );
            e.signer_device_id = signer;
            e
        };
        for e in [
            msg(room_ws, Some(Uuid::new_v4())),
            msg(Uuid::nil(), None),
            msg(leaked_ws, Some(Uuid::new_v4())),
            msg(leaked_ws, None),
            msg(other_local, Some(Uuid::new_v4())),
            msg(mine_elsewhere, Some(me)),
        ] {
            store.ingest(&e).unwrap();
        }
        store.set_sync_cursor(leaked_room, 7).unwrap();
        store.set_sync_cursor(room, 9).unwrap();

        let keep = HashSet::from([room_ws]);
        let mut found = store.foreign_workspaces(&keep, me).unwrap();
        found.sort();
        let mut want = vec![(leaked_ws, 2), (other_local, 1)];
        want.sort();
        assert_eq!(found, want);

        let backup = dir.path().join("hive.db.bak");
        let mut dropped = store.purge_foreign_workspaces(&keep, me, Some(&backup)).unwrap();
        dropped.sort();
        assert_eq!(dropped, want);
        assert_eq!(store.max_row_id().unwrap(), 6);
        let left: i64 = store.conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 3, "the room's event, the legacy nil one, and this device's own");
        assert_eq!(store.sync_cursor(leaked_room).unwrap(), 0, "a dropped room re-pulls from the start");
        assert_eq!(store.sync_cursor(room).unwrap(), 9);

        // The backup holds everything, and a second run is a no-op.
        let copy = EventStore::open(&backup).unwrap();
        let all: i64 = copy.conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(all, 6);
        assert!(store.purge_foreign_workspaces(&keep, me, Some(&dir.path().join("again"))).unwrap().is_empty());
        assert!(!dir.path().join("again").exists());
    }
}

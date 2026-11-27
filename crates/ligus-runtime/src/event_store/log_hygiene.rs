//! Log hygiene (workspace model v3, 0.5; design note sections 7.2, 7.3, 13
//! and 16.2, steps M8 and M10): `user_version` 2.
//!
//! - **Tombstones.** An event this device prunes (a streaming chunk of a
//!   finished reply) leaves a row in `pruned_events`. Ingest checks it before
//!   storing: the same copy fetched again (a sync cursor reset, a relay that
//!   hands old pages back) is recorded as a lamport witness only, never
//!   stored again as if it were new. A **different** copy under the id isn't
//!   hidden by the tombstone: it is kept beside it in `equivocations`, and
//!   the fold decides, as for any other copy.
//! - **Dispatch novelty.** `dispatch_processed` holds the message events this
//!   device has handed to dispatch, or authored itself, by event id. A
//!   message is new to dispatch when it isn't there and its signed envelope
//!   time is within [`DISPATCH_AGE_BOUND_MS`]: a message written offline
//!   below the chat's head is still answered, and a very old one fetched
//!   again never is. Not a row or lamport watermark.
//! - **Prune cadence.** The chunk prune runs at most once a day
//!   ([`PRUNE_EVERY_MS`]), off the launch path, and only once the
//!   tombstones exist (version 2).
//!
//! All tables are device-local and never synced; nothing here holds a
//! payload, a body or a secret (ids, digests, lamports and times only).
//! Every step is additive: a 0.4 build on the same data dir ignores the new
//! tables, and once the version is 2 it opens the database read-only
//! (`NEWER_DATA_MESSAGE`).

use std::collections::HashMap;
use std::path::PathBuf;

use ligus_core::{MessageRole, SessionEvent, SessionEventEnvelope};
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

use super::storage_v2::{backup_marked, now_ms, user_version, SCHEMA_VERSION, SYNC_SCHEMA_VERSION};
use super::{remove_backup, write_tx, EventStore, Result};

/// How old (by its signed envelope time) a message may be and still be
/// handed to dispatch: 24 hours. Older ones fetched again are never taken
/// over, and their processed marks are trimmed.
pub const DISPATCH_AGE_BOUND_MS: i64 = 24 * 60 * 60 * 1000;

/// The chunk prune runs at most this often: once a day.
pub const PRUNE_EVERY_MS: i64 = 24 * 60 * 60 * 1000;

/// What the backup before M8 and M10 is named after: `<db>.bak-log-v2-…`.
/// Kept until the person deletes it (`ligus doctor` lists it).
pub(super) const HYGIENE_BACKUP_MARK: &str = ".bak-log-v2-";

/// Why an event was pruned: a streaming chunk of a reply that has its
/// completion (the completed body supersedes it).
pub(super) const SUPERSEDED_CHUNK: &str = "superseded-chunk";
/// A tombstone M10 made from a lamport witness of an event pruned before
/// tombstones existed: its digest is unknown (`''`).
const BACKFILLED: &str = "backfilled";

/// The maintenance task the prune cadence is kept under.
const PRUNE_TASK: &str = "prune-chunks";

pub(super) fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        -- Device-local, never synced: one row per copy of an event this
        -- device pruned (its digest, `ligus_core::copies::copy_digest`;
        -- empty when M10 backfilled it from a lamport witness). Ingest
        -- checks it before storing.
        CREATE TABLE IF NOT EXISTS pruned_events (
            workspace_id    TEXT NOT NULL,
            session_id      TEXT NOT NULL,
            event_id        TEXT NOT NULL,
            envelope_digest TEXT NOT NULL,
            lamport         INTEGER NOT NULL,
            signer          TEXT NOT NULL DEFAULT '',
            reason          TEXT NOT NULL,
            pruned_at       INTEGER NOT NULL,
            PRIMARY KEY (workspace_id, event_id, envelope_digest)
        );
        -- Device-local: the message events this device handed to dispatch
        -- or wrote itself (ms since the epoch). Trimmed once the event is
        -- past the age bound.
        CREATE TABLE IF NOT EXISTS dispatch_processed (
            workspace_id TEXT NOT NULL,
            session_id   TEXT NOT NULL,
            event_id     TEXT NOT NULL,
            processed_at INTEGER NOT NULL,
            PRIMARY KEY (workspace_id, event_id)
        );
        -- Device-local: when each periodic maintenance task last ran.
        CREATE TABLE IF NOT EXISTS log_maintenance (
            task    TEXT PRIMARY KEY,
            last_ms INTEGER NOT NULL
        );
        -- Dispatch reads recent messages by their signed envelope time.
        CREATE INDEX IF NOT EXISTS idx_events_kind_time ON events (kind, julianday(timestamp));
        "#,
    )
}

/// Set version 2 on a database with nothing for M8 or M10 to do (at version
/// 1 with no events and no witnesses: a new one). Inside the schema
/// transaction, after M1's own stamp.
pub(super) fn stamp_if_new(conn: &Connection) -> rusqlite::Result<()> {
    if user_version(conn)? != SYNC_SCHEMA_VERSION {
        return Ok(());
    }
    let empty = !conn.prepare("SELECT 1 FROM events UNION ALL SELECT 1 FROM lamport_witnesses LIMIT 1")?.exists([])?;
    if empty {
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    Ok(())
}

/// RFC 3339 for `ms` since the epoch (what `events.timestamp` holds).
fn rfc3339(ms: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .ok()
        .and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

/// What [`EventStore::migrate_log_hygiene`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HygieneMigration {
    /// Already at version 2.
    AlreadyDone,
    /// Migrated: tombstones backfilled from witnesses, recent message
    /// events marked as handled, and the backup taken first (none when the
    /// store held no events).
    Migrated { tombstones: usize, processed: usize, backup: Option<PathBuf> },
    /// M1 hasn't run yet (it runs first, on this or a later start).
    NotYet,
    /// Another process has the store open: the step waits for a start that
    /// has it alone. Prune waits too; dispatch reads only what each pull
    /// stores meanwhile, as before.
    Busy,
    /// The database was written by a newer Ligus.
    ReadOnly,
}

/// What a tombstone says about a copy about to be stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tombstoned {
    /// No tombstone under the id.
    No,
    /// This very copy was pruned: a witness only.
    Same,
    /// The id was pruned under another copy: this one is kept beside it.
    Other,
}

impl EventStore {
    /// M8 and M10 (workspace model v3, 16.2): bring a version-1 database to
    /// version 2. The tables exist already (every open creates them, so a
    /// start that can't migrate still has them); this backfills a tombstone
    /// for every lamport witness whose event isn't held (chunks pruned before
    /// tombstones), marks the recent message events dispatch must not take
    /// again (answered, not a person's turn, or gone from the fold; recent
    /// unanswered ones stay eligible, and older ones are past the age bound
    /// anyway), and sets the version. Only while this process holds the
    /// store alone; one backup first; one transaction, and a failure removes
    /// the backup and leaves the database as it was. Idempotent.
    pub fn migrate_log_hygiene(&self) -> Result<HygieneMigration> {
        self.migrate_log_hygiene_at(now_ms())
    }

    pub(crate) fn migrate_log_hygiene_at(&self, now: i64) -> Result<HygieneMigration> {
        if self.newer.is_some() {
            return Ok(HygieneMigration::ReadOnly);
        }
        match user_version(&self.conn)? {
            v if v >= SCHEMA_VERSION => return Ok(HygieneMigration::AlreadyDone),
            v if v < SYNC_SCHEMA_VERSION => return Ok(HygieneMigration::NotYet),
            _ => {}
        }
        self.exclusively(|| self.migrate_log_hygiene_locked(now)).unwrap_or(Ok(HygieneMigration::Busy))
    }

    fn migrate_log_hygiene_locked(&self, now: i64) -> Result<HygieneMigration> {
        let _upgrade = super::upgrade_lock().lock().unwrap_or_else(|p| p.into_inner());
        if user_version(&self.conn)? >= SCHEMA_VERSION {
            return Ok(HygieneMigration::AlreadyDone);
        }
        let any: bool = self.conn.prepare("SELECT 1 FROM events LIMIT 1")?.exists([])?;
        let backup = if any { backup_marked(&self.conn, HYGIENE_BACKUP_MARK, "m8-backup")? } else { None };
        let run = || -> Result<(usize, usize)> {
            let tx = write_tx(&self.conn)?;
            let tombstones = self.backfill_tombstones(&tx, now)?;
            #[cfg(test)]
            if super::test_hooks::fails("m10") {
                return Err(rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL), None).into());
            }
            let processed = self.seed_dispatch_processed(&tx, now)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
            Ok((tombstones, processed))
        };
        match run() {
            Ok((tombstones, processed)) => {
                tracing::info!(target: "event_store", tombstones, processed, "pruned events now leave tombstones, and dispatch remembers what it handled");
                Ok(HygieneMigration::Migrated { tombstones, processed, backup })
            }
            Err(e) => {
                if let Some(path) = &backup {
                    remove_backup(path);
                }
                Err(e)
            }
        }
    }

    /// M10, first half: a tombstone (digest unknown) for every lamport
    /// witness whose event this device doesn't hold under its workspace: an
    /// event pruned before tombstones, or one refused at ingest. A backfilled
    /// tombstone only ever stops a copy the prune would remove again (a chunk
    /// of a finished reply, see [`EventStore::tombstoned`]), so a refused
    /// event fetched again is judged again as before.
    fn backfill_tombstones(&self, tx: &Connection, now: i64) -> Result<usize> {
        Ok(tx.execute(
            "INSERT OR IGNORE INTO pruned_events (workspace_id, session_id, event_id, envelope_digest, lamport, signer, reason, pruned_at) \
             SELECT w.workspace_id, w.session_id, w.event_id, '', MAX(w.lamport), '', ?1, ?2 FROM lamport_witnesses w \
             WHERE NOT EXISTS (SELECT 1 FROM events e WHERE e.workspace_id = w.workspace_id AND e.event_id = w.event_id) \
             AND NOT EXISTS (SELECT 1 FROM equivocations q WHERE q.workspace_id = w.workspace_id AND q.event_id = w.event_id) \
             GROUP BY w.workspace_id, w.session_id, w.event_id",
            rusqlite::params![BACKFILLED, now],
        )?)
    }

    /// M10, second half: mark every message event within the age bound that
    /// dispatch must not take again: one the fold doesn't show, one that
    /// isn't a person's turn, and a person's turn that has its answer. A
    /// recent unanswered turn stays eligible (it may have arrived just before
    /// the last quit).
    fn seed_dispatch_processed(&self, tx: &Connection, now: i64) -> Result<usize> {
        let cutoff = rfc3339(now - DISPATCH_AGE_BOUND_MS);
        let recent = self.query_envelopes(
            "SELECT envelope_json FROM events WHERE kind = 'messageAppended' AND julianday(timestamp) >= julianday(?1)",
            [cutoff],
        )?;
        let mut folds: HashMap<(Uuid, Uuid), Option<ligus_core::ChatSession>> = HashMap::new();
        let mut marked = 0;
        for env in recent {
            let SessionEvent::MessageAppended { message } = &env.payload else { continue };
            let log = (env.workspace_id, env.session_id);
            if !folds.contains_key(&log) {
                let fold = self.load_session_in(log.0, log.1).ok().flatten();
                folds.insert(log, fold);
            }
            let eligible = folds.get(&log).and_then(Option::as_ref).is_some_and(|s| {
                s.messages.iter().any(|m| m.id == message.id && m.role == MessageRole::User)
                    && !crate::turn_dispatch::user_turn_answered(s, message.id)
            });
            if !eligible {
                marked += tx.execute(
                    "INSERT OR IGNORE INTO dispatch_processed (workspace_id, session_id, event_id, processed_at) VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![env.workspace_id.to_string(), env.session_id.to_string(), env.event_id.to_string(), now],
                )?;
            }
        }
        Ok(marked)
    }

    /// Whether `env`, about to be stored, was pruned here (see
    /// [`Tombstoned`]). A backfilled tombstone (digest unknown) counts as
    /// this copy only when the copy is what the prune removes: a chunk of a
    /// reply its log holds the completion of.
    pub(super) fn tombstoned(&self, env: &SessionEventEnvelope) -> Result<Tombstoned> {
        let digests: Vec<String> = {
            let mut stmt = self.conn.prepare_cached("SELECT envelope_digest FROM pruned_events WHERE workspace_id = ?1 AND event_id = ?2")?;
            let rows = stmt.query_map([env.workspace_id.to_string(), env.event_id.to_string()], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        if digests.is_empty() {
            return Ok(Tombstoned::No);
        }
        if digests.contains(&ligus_core::copies::copy_digest(env)) {
            return Ok(Tombstoned::Same);
        }
        if digests.iter().any(String::is_empty) && self.superseded_chunk(env)? {
            return Ok(Tombstoned::Same);
        }
        Ok(if digests.iter().any(|d| !d.is_empty()) { Tombstoned::Other } else { Tombstoned::No })
    }

    /// Whether `env` is a streaming chunk of a reply whose completion its
    /// own log holds.
    fn superseded_chunk(&self, env: &SessionEventEnvelope) -> Result<bool> {
        let SessionEvent::MessageChunkReceived { message_id, .. } = &env.payload else { return Ok(false) };
        Ok(self
            .conn
            .prepare_cached(
                "SELECT 1 FROM events WHERE workspace_id = ?1 AND session_id = ?2 AND kind = 'messageCompleted' \
                 AND json_extract(envelope_json, '$.payload.message_id') = ?3",
            )?
            .exists([env.workspace_id.to_string(), env.session_id.to_string(), message_id.to_string()])?)
    }

    /// A copy under a pruned id that isn't the pruned copy: kept beside the
    /// tombstone in `equivocations` (the fold decides which copies count),
    /// never in the log's own place.
    pub(super) fn keep_beside_tombstone(&self, env: &SessionEventEnvelope, sealed_epoch: Option<u32>) -> Result<()> {
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
        tracing::warn!(target: "event_store", event_id = %env.event_id, "another copy under a pruned event id: kept beside its tombstone");
        Ok(())
    }

    /// Leave a tombstone for `env`, a copy about to be pruned.
    pub(super) fn note_pruned(&self, env: &SessionEventEnvelope, reason: &str, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO pruned_events (workspace_id, session_id, event_id, envelope_digest, lamport, signer, reason, pruned_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                env.workspace_id.to_string(),
                env.session_id.to_string(),
                env.event_id.to_string(),
                ligus_core::copies::copy_digest(env),
                env.lamport.min(i64::MAX as u64) as i64,
                env.signer_device_id.map(|d| d.to_string()).unwrap_or_default(),
                reason,
                now,
            ],
        )?;
        Ok(())
    }

    /// Mark a message event this device wrote as handled: dispatch never
    /// takes this device's own posts and replies (the local send answers
    /// them as they're written).
    pub(super) fn mark_authored(&self, env: &SessionEventEnvelope) -> Result<()> {
        if matches!(env.payload, SessionEvent::MessageAppended { .. }) {
            self.conn.execute(
                "INSERT OR IGNORE INTO dispatch_processed (workspace_id, session_id, event_id, processed_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![env.workspace_id.to_string(), env.session_id.to_string(), env.event_id.to_string(), now_ms()],
            )?;
        }
        Ok(())
    }

    /// The message events dispatch hasn't handled, now marked as handled,
    /// as `(workspace, session, message, signing device)` in fold order
    /// `(lamport, event_id)`: every stored message this device didn't write
    /// and hasn't handed to dispatch yet whose signed envelope time is within
    /// [`DISPATCH_AGE_BOUND_MS`], whenever it arrived (one written offline
    /// below the chat's head included). Until M10 has run (another process
    /// held the store), only those stored after row `after_row` (what the
    /// caller's pull stored), as before. Read and marked under the write
    /// lock, so two connections never hand one message to dispatch twice.
    pub fn take_dispatch_candidates(&self, after_row: i64) -> Result<Vec<(Uuid, Uuid, Uuid, Option<Uuid>)>> {
        self.take_dispatch_candidates_at(after_row, now_ms())
    }

    pub(crate) fn take_dispatch_candidates_at(&self, after_row: i64, now: i64) -> Result<Vec<(Uuid, Uuid, Uuid, Option<Uuid>)>> {
        if self.newer.is_some() {
            return Ok(Vec::new());
        }
        let tx = write_tx(&self.conn)?;
        let seeded = user_version(&tx)? >= SCHEMA_VERSION;
        let floor = if seeded { 0 } else { after_row };
        let mut envs = self.query_envelopes(
            "SELECT envelope_json FROM events e WHERE e.kind = 'messageAppended' AND julianday(e.timestamp) >= julianday(?1) AND e.row_id > ?2 \
             AND NOT EXISTS (SELECT 1 FROM dispatch_processed p WHERE p.workspace_id = e.workspace_id AND p.event_id = e.event_id)",
            rusqlite::params![rfc3339(now - DISPATCH_AGE_BOUND_MS), floor],
        )?;
        for env in &envs {
            tx.execute(
                "INSERT OR IGNORE INTO dispatch_processed (workspace_id, session_id, event_id, processed_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![env.workspace_id.to_string(), env.session_id.to_string(), env.event_id.to_string(), now],
            )?;
        }
        tx.commit()?;
        envs.sort_by_key(|e| e.canonical_key());
        Ok(envs
            .into_iter()
            .filter_map(|e| match e.payload {
                SessionEvent::MessageAppended { message } => Some((e.workspace_id, e.session_id, message.id, e.signer_device_id)),
                _ => None,
            })
            .collect())
    }

    /// Prune superseded chunks ([`EventStore::prune_superseded_chunks`]) when
    /// a day has passed since the last prune on this data dir, and trim the
    /// processed marks past the age bound. `None` when it isn't due, or
    /// before version 2 (a pruned event needs its tombstone, and an older
    /// build on the same data dir would store it again).
    pub fn prune_if_due(&mut self) -> Result<Option<usize>> {
        self.prune_if_due_at(now_ms())
    }

    pub(crate) fn prune_if_due_at(&mut self, now: i64) -> Result<Option<usize>> {
        if self.newer.is_some() || user_version(&self.conn)? < SCHEMA_VERSION {
            return Ok(None);
        }
        let last: Option<i64> =
            self.conn.query_row("SELECT last_ms FROM log_maintenance WHERE task = ?1", [PRUNE_TASK], |r| r.get(0)).optional()?;
        // A clock set back far past the last run doesn't stop the prune for
        // good.
        let due = last.is_none_or(|l| now - l >= PRUNE_EVERY_MS || l - now >= PRUNE_EVERY_MS);
        if !due {
            return Ok(None);
        }
        let removed = self.prune_superseded_chunks_at(now)?;
        self.trim_dispatch_processed(now)?;
        self.conn.execute(
            "INSERT INTO log_maintenance (task, last_ms) VALUES (?1, ?2) ON CONFLICT(task) DO UPDATE SET last_ms = excluded.last_ms",
            rusqlite::params![PRUNE_TASK, now],
        )?;
        Ok(Some(removed))
    }

    /// Drop the processed marks dispatch no longer needs: handled more than
    /// the age bound ago, of an event this device no longer holds or whose
    /// envelope time is past the bound (the bound alone keeps those out). A
    /// mark of an event dated in the future stays.
    pub(crate) fn trim_dispatch_processed(&self, now: i64) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM dispatch_processed WHERE processed_at < ?1 AND NOT EXISTS (SELECT 1 FROM events e \
             WHERE e.workspace_id = dispatch_processed.workspace_id AND e.event_id = dispatch_processed.event_id \
             AND julianday(e.timestamp) >= julianday(?2))",
            rusqlite::params![now - DISPATCH_AGE_BOUND_MS, rfc3339(now - DISPATCH_AGE_BOUND_MS)],
        )?)
    }

    /// How many tombstones this device holds (tests and `ligus doctor`).
    pub fn tombstone_count(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM pruned_events", [], |r| r.get(0))?)
    }

    /// Whether dispatch has handled (or this device wrote) the message event
    /// `event_id` of `workspace_id`.
    pub fn dispatch_processed(&self, workspace_id: Uuid, event_id: Uuid) -> Result<bool> {
        Ok(self
            .conn
            .prepare_cached("SELECT 1 FROM dispatch_processed WHERE workspace_id = ?1 AND event_id = ?2")?
            .exists([workspace_id.to_string(), event_id.to_string()])?)
    }
}

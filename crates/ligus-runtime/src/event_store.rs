//! SQLite-backed event store — the Rust replacement for Swift's NDJSON event
//! log (`<workspaceRoot>/.ligus/events/<sessionID>.ndjson`) +
//! `WorkspaceEventEnvelopeStore`.
//!
//! The append-only event-log semantics are unchanged: each workspace has an
//! ordered, per-session-sequenced stream of [`SessionEventEnvelope`]s; current
//! session state is obtained by projecting that stream (`ligus_core::project`).
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

use ligus_core::authz::ConfigTimeline;
use ligus_core::{project_authorized, project_config, ChatSession, SessionEvent, SessionEventEnvelope, WorkspaceAuthz};
use rusqlite::{Connection, OptionalExtension};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum EventStoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// An authored draft whose signer stored another event naming the same
    /// chain link (`ActorStamp::prev`) since it was drafted (another
    /// connection of this device wrote first). Storing it would fork the
    /// signer's chain, so it is refused; draft it again (one confirmed with
    /// a passkey: confirm it again, the confirmation names the draft).
    #[error("this draft is out of date: its signer wrote something else here first; draft it again (confirm it again if it needed a passkey)")]
    StaleDraft,
    /// The cleanup's backup path already holds a file; it is never
    /// overwritten or removed.
    #[error("the backup file {0} already exists; the cleanup didn't run")]
    BackupExists(std::path::PathBuf),
    #[error("this is too large to share with the workspace ({bytes} bytes; the most one change can be is {max} bytes). Split it up, or share a large file as an attachment")]
    TooLarge { bytes: usize, max: usize },
    /// Upgrading the database (a one-time rewrite, with a backup first)
    /// needs more free disk than there is. Nothing was changed.
    #[error("free {} on this disk to upgrade the Ligus database (it needs about {} free, {} is free); nothing was changed", gb(need.saturating_sub(*free)), gb(*need), gb(*free))]
    NeedsSpaceToUpgrade { need: u64, free: u64 },
}

/// Bytes as gigabytes, for a person (`1.4 GB`).
fn gb(bytes: u64) -> String {
    format!("{:.1} GB", (bytes as f64 / 1e9).max(0.1))
}

pub type Result<T> = std::result::Result<T, EventStoreError>;

impl EventStoreError {
    /// SQLite's "database is locked": another connection held the file past
    /// the busy timeout. Worth trying again later; nothing was written.
    pub fn is_busy(&self) -> bool {
        matches!(self, EventStoreError::Sqlite(rusqlite::Error::SqliteFailure(f, _)) if f.code == rusqlite::ErrorCode::DatabaseBusy)
    }
}

/// The largest envelope (its JSON) this device writes to a synced workspace:
/// sealed for the relay (its ciphertext a JSON array of byte values, at most
/// four characters a byte, plus a little), it stays within the relay's
/// request-body cap
/// ([`crate::sync_engine::MAX_PUSH_BODY_BYTES`]).
pub const MAX_ENVELOPE_BYTES: usize = 1024 * 1024 - 16 * 1024;

/// Whether `env` can reach the relay at all ([`MAX_ENVELOPE_BYTES`]). A write
/// to a synced workspace checks it before the envelope is stored: one that
/// can't be sent would otherwise wait in the outbox for good.
pub fn syncable(env: &SessionEventEnvelope) -> Result<()> {
    let bytes = serde_json::to_vec(env)?.len();
    if bytes > MAX_ENVELOPE_BYTES {
        return Err(EventStoreError::TooLarge { bytes, max: MAX_ENVELOPE_BYTES });
    }
    Ok(())
}

mod agent_usage;
mod held;
mod repo_tables;
pub use held::{claimed_account, HeldEvent, HeldStats, HoldItem, RefetchRow, HELD_PER_SIGNER, HELD_PER_WORKSPACE, HELD_REFETCH_PER_WORKSPACE, HELD_REFETCH_TRIES, HELD_RETRY_EVERY, HELD_TOTAL, HELD_TTL_MS};
pub use agent_usage::{parse_day, utc_day, AgentUsageRow};
pub use repo_tables::{CheckoutSource, RepoMapping};

/// A chat responder's compaction state as kept on this device: the first
/// kept message (`boundary`), the ids the `summary` covers (everything before
/// the boundary when it was made) and their
/// [`ligus_core::context_budget::messages_fingerprint`], which the reader
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
    /// Each log's lamport head ([`EventStore::lamport_head`]), by
    /// (workspace, session), with the rows it was computed over.
    heads: Mutex<HashMap<(Uuid, Uuid), (RowsSig, Option<ligus_core::CfgFrontier>)>>,
}

/// A cached chat projection: its rows, the config events held when it was
/// folded, and the session.
type ProjectionEntry = (RowsSig, usize, Option<ChatSession>);
/// (config session, trusted founder, trusted workspace).
type ConfigKey = (Uuid, Option<String>, Option<Uuid>);
type ConfigEntry = (RowsSig, Option<ChatSession>, Arc<ConfigTimeline>);

/// (workspace, session, trusted founder, trusted workspace, config-timeline
/// fingerprint): what a projection depends on besides the log's rows. The
/// fingerprint is 0 for an unchecked projection and 1 for one without a
/// config log.
type ProjectionKey = (Uuid, Uuid, Option<String>, Option<Uuid>, u64);

/// (row count, max row id) of a session. Any append raises the max row id and
/// any delete lowers the count, so an equal signature means the same rows
/// (envelope rows are never updated in place).
type RowsSig = (i64, i64);

/// Guard signals kept per device (all workspaces); older rows are pruned.
pub const GUARD_SIGNALS_KEEP: i64 = 20_000;
/// Parked relay bodies kept per room (A-13); junk is evicted first (B-11).
pub const PARKED_KEEP: i64 = 5_000;
/// Bytes of parked relay bodies kept per room, in the same order as
/// [`PARKED_KEEP`]: a flood of large bodies under an unknown key (anyone who
/// knows the room id on an open relay can write one) can't fill the disk, or
/// memory when they are all replayed after a keyring change.
pub const PARKED_BYTES_KEEP: i64 = 64 << 20;
/// Facts kept on this device (newest).
pub const FACTS_KEEP: i64 = 50_000;

/// Cached projections kept at most; past this the cache starts over.
const PROJECTION_CACHE_MAX: usize = 256;

/// Another stored event of a signer on a log naming a given link (see
/// `EventStore::forks_chain`). The expressions match
/// `idx_events_by_signer` exactly, so it is an index lookup. The log's
/// workspace is checked on the rows it finds (`+workspace_id`: not an index
/// term, so the planner never trades the signer index for the workspace
/// one).
const FORKS_CHAIN_SQL: &str = "SELECT 1 FROM events WHERE session_id = ?1 AND +workspace_id = ?5 AND event_id != ?2 \
     AND json_extract(envelope_json, '$.signerDeviceId') = ?3 \
     AND json_extract(envelope_json, '$.actorStamp') IS NOT NULL \
     AND json_extract(envelope_json, '$.actorStamp.prev') IS ?4 LIMIT 1";

/// A signer's newest event on a log below a lamport (see
/// `EventStore::last_signed_by`), through `idx_events_by_signer` (the
/// workspace checked on its rows, as in [`FORKS_CHAIN_SQL`]).
const LAST_SIGNED_BY_SQL: &str = "SELECT event_id FROM events WHERE session_id = ?1 AND +workspace_id = ?4 AND json_extract(envelope_json, '$.signerDeviceId') = ?2 \
     AND json_extract(envelope_json, '$.lamport') < ?3 ORDER BY row_id DESC LIMIT 1";

/// A fresh name for the foreign-workspace cleanup's backup next to the
/// database in `data_dir`: unique per call (time to the nanosecond and the
/// process id), so two runs never pick the same file.
pub fn purge_backup_path(data_dir: &Path) -> std::path::PathBuf {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    data_dir.join(format!("ligus.db.bak-foreign-purge-{}-{:09}-{}", now.as_secs(), now.subsec_nanos(), std::process::id()))
}

/// How long a connection waits for another connection's lock on the file
/// before it reports "database is locked".
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Put the file in WAL mode (a no-op once it is). SQLite never calls the busy
/// handler for this pragma: switching a new file needs every other
/// connection out of it for a moment, and one that meets another opening
/// the same new file at once (a first start: the service and the sync loop)
/// gets "database is locked" straight away. The switch changes nothing
/// else and is idempotent, so it is tried again, briefly, up to the busy
/// timeout.
fn enable_wal(conn: &Connection) -> Result<()> {
    let deadline = std::time::Instant::now() + BUSY_TIMEOUT;
    let mut pause = std::time::Duration::from_millis(1);
    loop {
        match conn.pragma_update(None, "journal_mode", "WAL") {
            Ok(()) => return Ok(()),
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::DatabaseBusy && std::time::Instant::now() < deadline => {
                std::thread::sleep(pause);
                pause = (pause * 2).min(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Whether `events` still makes an event id unique across every workspace
/// (a database made before ids were unique per workspace): a unique index
/// on `event_id` alone.
fn needs_event_id_migration(conn: &Connection) -> Result<bool> {
    Ok(conn
        .prepare(
            "SELECT 1 FROM pragma_index_list('events') AS il WHERE il.\"unique\" = 1 \
             AND (SELECT group_concat(name) FROM pragma_index_info(il.name)) = 'event_id'",
        )?
        .exists([])?)
}

/// What the migration's backups are named after: `<db>.bak-ws-event-ids-…`.
const MIGRATION_BACKUP_MARK: &str = ".bak-ws-event-ids-";

/// The database file a connection has open (none: in memory).
fn db_path(conn: &Connection) -> Option<std::path::PathBuf> {
    conn.path().filter(|p| !p.is_empty()).map(std::path::PathBuf::from)
}

/// Remove a backup file and SQLite's side files for it.
fn remove_backup(path: &Path) {
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let mut p = path.as_os_str().to_os_string();
        p.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(p));
    }
}

/// Remove every migration backup next to `db` (and its side files). Called
/// at the start of an open, before any new backup: a backup still there is
/// either left by an attempt that failed or crashed (the database itself
/// was never changed: the migration is one transaction), or kept from a
/// migration that succeeded, which this open of the migrated database
/// retires. Either way, no full copy of the store piles up.
fn remove_migration_backups(db: &Path) {
    let (Some(dir), Some(name)) = (db.parent(), db.file_name().and_then(|n| n.to_str())) else { return };
    let prefix = format!("{name}{MIGRATION_BACKUP_MARK}");
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let file = e.file_name();
        let Some(file) = file.to_str() else { continue };
        if file.starts_with(&prefix) {
            let _ = std::fs::remove_file(e.path());
            tracing::info!(target: "event_store", file, "removed an earlier upgrade backup");
        }
    }
}

/// Free bytes on the volume holding `dir` (none when it can't be told).
fn free_bytes(dir: &Path) -> Option<u64> {
    #[cfg(test)]
    if let Some(v) = test_hooks::FREE_BYTES.with(|f| f.get()) {
        return Some(v);
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: `c` is a valid NUL-terminated path and `st` a zeroed
        // struct statvfs fills in.
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            return None;
        }
        #[allow(clippy::unnecessary_cast)]
        return Some(st.f_bavail as u64 * st.f_frsize as u64);
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let mut free: u64 = 0;
        // SAFETY: `wide` is a NUL-terminated UTF-16 path; the out pointer is
        // valid for one u64, the other two may be null.
        let ok = unsafe { windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
        return (ok != 0).then_some(free);
    }
    #[allow(unreachable_code)]
    None
}

/// The database's size on disk, its write-ahead log included.
fn db_bytes(db: &Path) -> u64 {
    let mut wal = db.as_os_str().to_os_string();
    wal.push("-wal");
    [db.to_path_buf(), std::path::PathBuf::from(wal)].iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum()
}

/// Refuse the upgrade, with nothing changed, when the disk can't hold its
/// backup, the rebuilt table and the write-ahead log (about 2.5 times the
/// database).
fn check_upgrade_space(db: &Path) -> Result<()> {
    let need = db_bytes(db).saturating_mul(5) / 2;
    let Some(free) = db.parent().and_then(free_bytes) else { return Ok(()) };
    if free < need {
        return Err(EventStoreError::NeedsSpaceToUpgrade { need, free });
    }
    Ok(())
}

/// Tests only: make one step of the upgrade fail, or report a free-space
/// figure, on this thread.
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::cell::Cell;
    thread_local! {
        pub(crate) static FAIL_STEP: Cell<Option<&'static str>> = const { Cell::new(None) };
        pub(crate) static FREE_BYTES: Cell<Option<u64>> = const { Cell::new(None) };
    }
    pub(crate) fn fails(step: &str) -> bool {
        FAIL_STEP.with(|f| f.get()) == Some(step)
    }
}

/// A copy of the whole database next to it before a migration rewrites a
/// table (`<db>.bak-ws-event-ids-<time>-<pid>`); none for an in-memory or
/// empty store. A copy that fails part way is removed again.
fn migration_backup(conn: &Connection) -> Result<Option<std::path::PathBuf>> {
    let Some(db) = db_path(conn) else { return Ok(None) };
    let rows: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
    if rows == 0 {
        return Ok(None);
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let mut name = db.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(format!("{MIGRATION_BACKUP_MARK}{}-{:09}-{}", now.as_secs(), now.subsec_nanos(), std::process::id()));
    let path = db.with_file_name(name);
    let copied = conn.execute("VACUUM INTO ?1", [path.to_string_lossy()]).map(|_| ());
    #[cfg(test)]
    let copied = if test_hooks::fails("backup") { Err(rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL), None)) } else { copied };
    if let Err(e) = copied {
        remove_backup(&path);
        return Err(e.into());
    }
    Ok(Some(path))
}

/// After a migration: give the rewritten table's old pages back to the disk
/// (`VACUUM`, when there is room for it), so the file doesn't stay twice
/// its size. Best effort: a failure leaves a larger file, nothing else.
fn reclaim_after_migration(conn: &Connection, db: &Path) {
    let room = db.parent().and_then(free_bytes).is_none_or(|free| free > db_bytes(db).saturating_mul(2));
    if !room {
        tracing::info!(target: "event_store", "not enough free disk to compact the upgraded database now");
        return;
    }
    if let Err(e) = conn.execute_batch("VACUUM") {
        tracing::warn!(target: "event_store", "compacting the upgraded database failed: {e}");
    }
    let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
}

/// One upgrade at a time in this process (the service and the sync loop
/// open the store together on a first start): one backup, and no open
/// removes a backup another is still making.
fn upgrade_lock() -> &'static Mutex<()> {
    static L: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    L.get_or_init(Default::default)
}

/// Rebuild `events` with event ids unique per workspace instead of across
/// workspaces (inside the caller's write transaction). Every row, row id and
/// place is kept, and the row-id counter never goes back (the push scan
/// relies on row ids only growing). Then a copy that sits in
/// `equivocations` only because another workspace's row held its id first
/// takes its place in its own workspace's log (the first such copy per
/// workspace and id, at the next place of its session); copies within one
/// workspace stay where they are.
fn migrate_event_ids_per_workspace(tx: &Connection) -> Result<()> {
    let seq: Option<i64> = tx.query_row("SELECT seq FROM sqlite_sequence WHERE name = 'events'", [], |r| r.get(0)).optional()?;
    tx.execute_batch(
        r#"
        CREATE TABLE events_ws_ids (
            row_id        INTEGER PRIMARY KEY AUTOINCREMENT,
            event_id      TEXT NOT NULL,
            session_id    TEXT NOT NULL,
            workspace_id  TEXT NOT NULL,
            sequence      INTEGER NOT NULL,
            kind          TEXT NOT NULL,
            scope         TEXT NOT NULL,
            timestamp     TEXT NOT NULL,
            envelope_json TEXT NOT NULL,
            sealed_epoch  INTEGER,
            UNIQUE (workspace_id, event_id),
            UNIQUE (session_id, sequence)
        );
        INSERT INTO events_ws_ids (row_id, event_id, session_id, workspace_id, sequence, kind, scope, timestamp, envelope_json, sealed_epoch)
            SELECT row_id, event_id, session_id, workspace_id, sequence, kind, scope, timestamp, envelope_json, sealed_epoch FROM events ORDER BY row_id;
        DROP TABLE events;
        ALTER TABLE events_ws_ids RENAME TO events;
        "#,
    )?;
    let max: i64 = tx.query_row("SELECT COALESCE(MAX(row_id), 0) FROM events", [], |r| r.get(0))?;
    let keep = seq.unwrap_or(0).max(max);
    if tx.execute("UPDATE sqlite_sequence SET seq = ?1 WHERE name = 'events'", [keep])? == 0 {
        tx.execute("INSERT INTO sqlite_sequence (name, seq) VALUES ('events', ?1)", [keep])?;
    }
    // Copies kept aside only because another workspace held the id.
    let stranded: Vec<(i64, String, String, String, String, String, Option<i64>)> = {
        let mut stmt = tx.prepare(
            "SELECT q.row_id, q.event_id, q.session_id, q.workspace_id, q.kind, q.envelope_json, q.sealed_epoch FROM equivocations q \
             WHERE NOT EXISTS (SELECT 1 FROM events e WHERE e.workspace_id = q.workspace_id AND e.event_id = q.event_id) ORDER BY q.row_id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (row_id, event_id, session_id, workspace_id, kind, json, sealed_epoch) in stranded {
        let taken: bool = tx
            .prepare("SELECT 1 FROM events WHERE workspace_id = ?1 AND event_id = ?2")?
            .exists([&workspace_id, &event_id])?;
        if taken {
            continue;
        }
        let Ok(env) = serde_json::from_str::<SessionEventEnvelope>(&json) else { continue };
        let timestamp = serde_json::to_value(env.timestamp)?.as_str().unwrap_or_default().to_string();
        let scope = serde_json::to_value(env.scope)?.as_str().unwrap_or_default().to_string();
        let next: i64 = tx.query_row("SELECT COALESCE(MAX(sequence), 0) + 1 FROM events WHERE session_id = ?1", [&session_id], |r| r.get(0))?;
        tx.execute(
            "INSERT INTO events (event_id, session_id, workspace_id, sequence, kind, scope, timestamp, envelope_json, sealed_epoch) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![event_id, session_id, workspace_id, next, kind, scope, timestamp, json, sealed_epoch],
        )?;
        tx.execute("DELETE FROM equivocations WHERE row_id = ?1", [row_id])?;
    }
    tracing::info!(target: "event_store", "event ids are now unique per workspace");
    Ok(())
}

/// A write transaction: `BEGIN IMMEDIATE`, so it takes the file's write lock
/// up front, waiting for it (the busy timeout) while another connection
/// writes. Every transaction here that writes uses it. A deferred one that
/// reads first takes only a read snapshot, and its first write then fails at
/// once with "database is locked" if another connection committed since
/// (SQLite can't wait that out: the snapshot is stale).
///
/// Inside a write this connection already holds ([`EventStore::begin_write`])
/// it is a savepoint: the lock is held already, and its writes still commit
/// or roll back together. Dropped without [`WriteTx::commit`], it rolls back.
pub(crate) fn write_tx(conn: &Connection) -> rusqlite::Result<WriteTx<'_>> {
    let nested = !conn.is_autocommit();
    conn.execute_batch(if nested { "SAVEPOINT ligus_write" } else { "BEGIN IMMEDIATE" })?;
    Ok(WriteTx { conn, nested, done: false })
}

pub(crate) struct WriteTx<'a> {
    conn: &'a Connection,
    nested: bool,
    done: bool,
}

impl WriteTx<'_> {
    /// Commit (release, nested). One that fails is rolled back on drop, so
    /// the connection never stays inside the transaction.
    pub(crate) fn commit(mut self) -> rusqlite::Result<()> {
        self.conn.execute_batch(if self.nested { "RELEASE ligus_write" } else { "COMMIT" })?;
        self.done = true;
        Ok(())
    }
}

impl std::ops::Deref for WriteTx<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn
    }
}

impl Drop for WriteTx<'_> {
    fn drop(&mut self) {
        // (Nothing to undo when SQLite already rolled the transaction back.)
        if !self.done && !self.conn.is_autocommit() {
            let _ = self.conn.execute_batch(if self.nested { "ROLLBACK TO ligus_write; RELEASE ligus_write" } else { "ROLLBACK" });
        }
    }
}

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
        // First, before anything touches the file: a connection that meets
        // another one's lock waits for it (up to the timeout) instead of
        // failing at once with "database is locked". The pragmas below and
        // the schema check already take locks.
        conn.busy_timeout(BUSY_TIMEOUT)?;
        // WAL for concurrent readers (the live UI connection + the background
        // sync connection share the same file).
        enable_wal(&conn)?;
        // NORMAL is safe under WAL (no corruption; at worst the last commit is
        // lost on an OS crash/power loss) and avoids an fsync per commit — a big
        // win for chat workloads. (#2)
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // A database made when event ids were unique across workspaces is
        // copied aside first (`VACUUM INTO`, outside any transaction), then
        // migrated under the write lock. A backup that turns out unneeded
        // (another connection migrated first) is removed again.
        //
        // Any backup already there is from a failed or crashed attempt, or
        // from an upgrade a previous open finished: removed first. Short of
        // disk, the upgrade is refused with a clear error and nothing
        // changed; a failed upgrade removes its own backup.
        let _upgrade = upgrade_lock().lock().unwrap_or_else(|p| p.into_inner());
        let db = db_path(&conn);
        if let Some(db) = &db {
            remove_migration_backups(db);
        }
        let backup = if needs_event_id_migration(&conn)? {
            if let Some(db) = &db {
                check_upgrade_space(db)?;
            }
            migration_backup(&conn)?
        } else {
            None
        };
        // The schema is checked and migrated under the write lock, so two
        // connections opening one file at once never both add a column.
        let migrate = || -> Result<bool> {
            let tx = write_tx(&conn)?;
            Self::init_schema(&tx)?;
            let migrated = needs_event_id_migration(&tx)?;
            if migrated {
                migrate_event_ids_per_workspace(&tx)?;
                Self::init_schema(&tx)?;
                #[cfg(test)]
                if test_hooks::fails("migrate") {
                    return Err(rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL), None).into());
                }
            }
            tx.commit()?;
            Ok(migrated)
        };
        let migrated = match migrate() {
            Ok(m) => m,
            Err(e) => {
                if let Some(path) = &backup {
                    remove_backup(path);
                }
                return Err(e);
            }
        };
        match (&backup, migrated) {
            // Kept until the next open of the upgraded database.
            (Some(_), true) => {}
            (Some(path), false) => remove_backup(path),
            (None, _) => {}
        }
        if migrated {
            if let Some(db) = &db {
                reclaim_after_migration(&conn, db);
            }
        }
        drop(_upgrade);
        Ok(Self { conn, projections: Mutex::new(HashMap::new()), configs: Mutex::new(HashMap::new()), heads: Mutex::new(HashMap::new()) })
    }

    fn init_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r#"
            -- An event id is unique within its workspace: another workspace's
            -- row under the same id (anyone can stamp one) is another log's
            -- and never takes the genuine row's place (see
            -- `migrate_event_ids_per_workspace` for databases made before).
            CREATE TABLE IF NOT EXISTS events (
                row_id        INTEGER PRIMARY KEY AUTOINCREMENT,
                event_id      TEXT NOT NULL,
                session_id    TEXT NOT NULL,
                workspace_id  TEXT NOT NULL,
                sequence      INTEGER NOT NULL,
                kind          TEXT NOT NULL,
                scope         TEXT NOT NULL,
                timestamp     TEXT NOT NULL,
                envelope_json TEXT NOT NULL,
                sealed_epoch  INTEGER,
                UNIQUE (workspace_id, event_id),
                UNIQUE (session_id, sequence)
            );
            CREATE INDEX IF NOT EXISTS idx_events_event_id ON events (event_id);
            CREATE INDEX IF NOT EXISTS idx_events_session_seq
                ON events (session_id, sequence);
            CREATE INDEX IF NOT EXISTS idx_events_workspace
                ON events (workspace_id, row_id);
            -- A log is (workspace, session): session ids are not namespaced
            -- by workspace, so every read of a log names both.
            CREATE INDEX IF NOT EXISTS idx_events_log_seq
                ON events (session_id, workspace_id, sequence);
            -- Each signer's chain on a log (its events and the link each
            -- names): what an authored append checks under the write lock
            -- (`forks_chain`) and what a new event's link is read from
            -- (`last_signed_by`), without a scan of the whole log. The
            -- signer leads, so a query by session alone never picks this
            -- index (and its order) over idx_events_session_seq.
            CREATE INDEX IF NOT EXISTS idx_events_by_signer
                ON events (json_extract(envelope_json, '$.signerDeviceId'), session_id, json_extract(envelope_json, '$.actorStamp.prev'));

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
            -- admission (or person invite) this device redeems (from its
            -- code).
            CREATE TABLE IF NOT EXISTS join_redeem_keys (
                workspace_id TEXT PRIMARY KEY,
                seed         TEXT NOT NULL
            );

            -- Device-local: the redeem key seed of each person invite this
            -- Admin's device issued, until its code is built (a passkey
            -- confirmation can come between issuing and the code).
            CREATE TABLE IF NOT EXISTS issued_invite_keys (
                workspace_id TEXT NOT NULL,
                invite_id    TEXT NOT NULL,
                seed         TEXT NOT NULL,
                PRIMARY KEY (workspace_id, invite_id)
            );

            -- Device-local: the relay subject an Admin's desktop minted for a
            -- box (People > Remote agents) and the box admission it issued in
            -- the same bootstrap. The admission's redemption names the box's
            -- service member, so removing that subject from the relay roster
            -- removes the right workspace member (never matched by name).
            CREATE TABLE IF NOT EXISTS box_relay_subjects (
                workspace_id TEXT NOT NULL,
                subject      TEXT NOT NULL,
                invite_id    TEXT NOT NULL,
                PRIMARY KEY (workspace_id, subject)
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

            -- Device-local guard signals, never synced (see ligus_core::signal):
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
            -- copies count (ligus_core::copies; only the same signer's second
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
            CREATE INDEX IF NOT EXISTS idx_equivocations_log ON equivocations (session_id, workspace_id);

            -- Device-local, never synced: an event this device refused at
            -- ingest (for any reason: a revoked or rebound key, a removed
            -- member, an old key, a bad signature) is kept only as a lamport
            -- witness: its id, lamport, log head and chain link
            -- (ligus_core::lamport). Another author may have held it and
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
            CREATE INDEX IF NOT EXISTS idx_lamport_witnesses_log ON lamport_witnesses (session_id, workspace_id);
            "#,
        )?;
        repo_tables::init(conn)?;
        held::init(conn)?;
        agent_usage::init(conn)?;
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
        Ok(())
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

    /// Highest sequence recorded on a log (`workspace_id`, `session_id`), if
    /// any.
    pub fn max_sequence(&self, workspace_id: Uuid, session_id: Uuid) -> Result<Option<i64>> {
        let seq = self
            .conn
            .query_row(
                "SELECT MAX(sequence) FROM events WHERE session_id = ?1 AND workspace_id = ?2",
                [session_id.to_string(), workspace_id.to_string()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten();
        Ok(seq)
    }

    /// The rows a log's projections are computed over (its events,
    /// equivocated copies and lamport witnesses): a cache key.
    fn rows_sig(&self, workspace_id: Uuid, session_id: Uuid) -> Result<RowsSig> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) + (SELECT COUNT(*) FROM equivocations WHERE session_id = ?1 AND workspace_id = ?2) \
             + (SELECT COUNT(*) FROM lamport_witnesses WHERE session_id = ?1 AND workspace_id = ?2), COALESCE(MAX(row_id), 0) \
             FROM events WHERE session_id = ?1 AND workspace_id = ?2",
            [session_id.to_string(), workspace_id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    /// Every envelope of a log this device holds (equivocated copies
    /// included) plus its lamport witnesses as inert events: what a fold of
    /// the log reads. A log is (`workspace_id`, `session_id`): rows another
    /// workspace stamped under the same session id are another log, never
    /// merged into this one.
    fn log_with_witnesses(&self, workspace_id: Uuid, session_id: Uuid) -> Result<Vec<SessionEventEnvelope>> {
        let mut envelopes = self.query_envelopes_sealed(
            "SELECT envelope_json, sealed_epoch FROM (SELECT 0 AS t, sequence AS o, envelope_json, sealed_epoch FROM events WHERE session_id = ?1 AND workspace_id = ?2 \
             UNION ALL SELECT 1, row_id, envelope_json, sealed_epoch FROM equivocations WHERE session_id = ?1 AND workspace_id = ?2) ORDER BY t, o",
            [session_id.to_string(), workspace_id.to_string()],
        )?;
        envelopes.extend(self.witnesses_of(workspace_id, session_id)?);
        Ok(envelopes)
    }

    /// A log's lamport witnesses (events refused here at ingest, see the
    /// `lamport_witnesses` table), each as an inert envelope: no payload
    /// (`Unknown`), no author or signer, only its id, lamport, log head and
    /// chain link. All of them, whether or not the id is stored: the lamport
    /// rule counts every copy alike on every device, and the fold leaves
    /// witnesses out (`ligus_core::lamport::is_witness`).
    fn witnesses_of(&self, workspace_id: Uuid, session_id: Uuid) -> Result<Vec<SessionEventEnvelope>> {
        let mut stmt = self.conn.prepare(
            "SELECT event_id, workspace_id, lamport, log_head, prev FROM lamport_witnesses WHERE session_id = ?1 AND workspace_id = ?2 ORDER BY row_id ASC",
        )?;
        let rows = stmt.query_map([session_id.to_string(), workspace_id.to_string()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, ws, lamport, head, prev) = row?;
            let (Ok(id), Ok(ws)) = (Uuid::parse_str(&id), Uuid::parse_str(&ws)) else { continue };
            let mut env = SessionEventEnvelope::new(session_id, ws, 0, SessionEvent::Unknown);
            env.event_id = id;
            env.lamport = lamport as u64;
            env.actor_stamp = Some(ligus_core::ActorStamp {
                actor: ligus_core::ActorIdentity::new("", "", ligus_core::ActorKind::System),
                recorded_at: ligus_core::Timestamp::epoch(),
                cfg_frontier: None,
                presence: None,
                runtime_effect: false,
                prev: Uuid::parse_str(&prev).ok(),
                sig_alg: Default::default(),
                log_head: serde_json::from_str(&head).ok(),
            });
            let env = ligus_core::lamport::inert_witness(&env);
            out.push(env);
        }
        Ok(out)
    }

    /// The ids of a log's lamport witnesses (tests).
    #[cfg(test)]
    pub(crate) fn witness_ids(&self, session_id: Uuid) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare("SELECT event_id FROM lamport_witnesses WHERE session_id = ?1 ORDER BY row_id ASC")?;
        let rows = stmt.query_map([session_id.to_string()], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok().and_then(|s| Uuid::parse_str(&s).ok())).collect())
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
    /// (`ligus_core::lamport`), over every envelope this device holds for it
    /// (authored here or ingested, equivocated copies included; a lamport
    /// witness counts for the bounds but is never the head): what this
    /// device's next event on the log names as its `log_head`, its lamport
    /// one past it. An event past the bounds (a lamport no real event
    /// accounts for) never moves it. Cached per log; an event appended here
    /// after the head moves it without a rescan.
    ///
    /// The log is (`workspace_id`, `session_id`): an event another workspace
    /// stamped under the same session id is never this log's head.
    pub fn lamport_head(&self, workspace_id: Uuid, session_id: Uuid) -> Result<Option<ligus_core::CfgFrontier>> {
        let key = (workspace_id, session_id);
        let sig = self.rows_sig(workspace_id, session_id)?;
        if let Some((cached, head)) = self.heads.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            if *cached == sig {
                return Ok(*head);
            }
        }
        let envelopes = self.log_with_witnesses(workspace_id, session_id)?;
        let head = ligus_core::lamport::head(&envelopes);
        let mut cache = self.heads.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= PROJECTION_CACHE_MAX && !cache.contains_key(&key) {
            cache.clear();
        }
        cache.insert(key, (sig, head));
        Ok(head)
    }

    /// Take the file's write lock and hold it until [`EventStore::end_write`]:
    /// no other connection writes meanwhile, so what this one reads stays
    /// current until it writes. For a draft that has to be made again at the
    /// head ([`EventStoreError::StaleDraft`]): drafted and stored under the
    /// lock, it can't go stale again. Writes made meanwhile through this
    /// store commit with `end_write(true)` and roll back otherwise.
    pub(crate) fn begin_write(&self) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        Ok(())
    }

    /// End [`EventStore::begin_write`]: commit, or roll back. Always leaves
    /// the connection out of its transaction (and the file's write lock
    /// released): a commit that fails is rolled back, and a rollback after
    /// SQLite already rolled back on its own (some errors do) is a no-op.
    /// Whatever didn't commit is dropped from the caches too.
    pub(crate) fn end_write(&self, commit: bool) -> Result<()> {
        if commit {
            if let Err(e) = self.conn.execute_batch("COMMIT") {
                self.abandon_write();
                return Err(e.into());
            }
            return Ok(());
        }
        self.abandon_write();
        Ok(())
    }

    /// Roll back whatever transaction this connection holds (none: nothing
    /// to do), and forget what the caches learned inside it.
    fn abandon_write(&self) {
        if !self.conn.is_autocommit() {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
        self.projections.lock().unwrap_or_else(|p| p.into_inner()).clear();
        self.configs.lock().unwrap_or_else(|p| p.into_inner()).clear();
        self.heads.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    /// Whether this connection is outside any transaction (tests).
    #[cfg(test)]
    pub(crate) fn in_autocommit(&self) -> bool {
        self.conn.is_autocommit()
    }

    /// Whether storing `env`, authored here, would fork its signer's chain:
    /// another stored event of the same signer on the log already names the
    /// same link (`ActorStamp::prev`, none for a first event). Only an
    /// authored draft can be stale this way (it was drafted before a write
    /// of the same signer through another connection); a peer's event is
    /// never judged here, its fork is for the fold to see.
    fn forks_chain(&self, env: &SessionEventEnvelope) -> Result<bool> {
        let (Some(signer), Some(stamp), Some(_)) = (env.signer_device_id, env.actor_stamp.as_ref(), env.signature.as_ref()) else {
            return Ok(false);
        };
        Ok(self
            .conn
            .prepare_cached(
                FORKS_CHAIN_SQL,
            )?
            .exists(rusqlite::params![
                env.session_id.to_string(),
                env.event_id.to_string(),
                signer.to_string(),
                stamp.prev.map(|p| p.to_string()),
                env.workspace_id.to_string()
            ])?)
    }

    /// Append an envelope this device authored, verbatim (its JSON, and so
    /// its signed `sequence`, are stored as given). Fails if its event id is
    /// already stored.
    ///
    /// Its place in this device's log (the `sequence` column, what
    /// [`EventStore::list`] orders by) is its own sequence, or the next free
    /// one when the log moved on since that was read: another connection to
    /// this file (the sync loop ingesting a peer's event, another service)
    /// may append between [`EventStore::max_sequence`] and here. Taken
    /// under the write lock, so two appends never get one place and none is
    /// refused for it. Ingested events are placed the same way
    /// ([`EventStore::ingest`]); a stored row is never moved or replaced.
    ///
    /// A draft its own signer has since written past (a write of the same
    /// device through another connection names the same chain link) is
    /// refused with [`EventStoreError::StaleDraft`]: stored, it would fork
    /// the signer's chain. The caller drafts it again.
    pub fn append_envelope(&self, env: &SessionEventEnvelope) -> Result<()> {
        let json = serde_json::to_string(env)?;
        let timestamp = serde_json::to_value(env.timestamp)?
            .as_str()
            .unwrap_or_default()
            .to_string();
        let tx = write_tx(&self.conn)?;
        if self.forks_chain(env)? {
            return Err(EventStoreError::StaleDraft);
        }
        // The cached head, if it is current: an event that names it moves it.
        // Read under the write lock, so no other connection's event lands
        // between this and the insert.
        let log = (env.workspace_id, env.session_id);
        let before = self.rows_sig(env.workspace_id, env.session_id)?;
        let cached = self.heads.lock().unwrap_or_else(|e| e.into_inner()).get(&log).copied();
        let next = self.next_row_sequence(env.session_id)?;
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
                env.sequence.max(next),
                env.payload.kind_str(),
                serde_json::to_value(env.scope)?.as_str().unwrap_or_default(),
                timestamp,
                json,
            ],
        )?;
        let after = self.rows_sig(env.workspace_id, env.session_id)?;
        tx.commit()?;
        if let Some((sig, head)) = cached {
            let names_head = env.actor_stamp.as_ref().map(|s| s.log_head) == Some(head)
                && env.lamport == ligus_core::lamport::next(head);
            let mut cache = self.heads.lock().unwrap_or_else(|e| e.into_inner());
            if sig == before && names_head {
                let key = ligus_core::CfgFrontier { lamport: env.lamport, event_id: env.event_id };
                cache.insert(log, (after, Some(key)));
            } else {
                cache.remove(&log);
            }
        }
        Ok(())
    }

    /// The next free place for a row under `session_id` in this device's
    /// table (`UNIQUE (session_id, sequence)` spans every workspace that
    /// stamped the id, so places are taken across them; a log's own order is
    /// still its rows' sequence order). Not a log read.
    fn next_row_sequence(&self, session_id: Uuid) -> Result<i64> {
        let max: Option<i64> = self
            .conn
            .query_row("SELECT MAX(sequence) FROM events WHERE session_id = ?1", [session_id.to_string()], |row| row.get(0))
            .optional()?
            .flatten();
        Ok(max.unwrap_or(0) + 1)
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
        // fold accounts for (`ligus_core::lamport`).
        let log_head = self.lamport_head(workspace_id, session_id)?;
        let tx = write_tx(&self.conn)?;
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
    /// witness for the ones after it (`ligus_core::lamport`), so a subset of
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
            [workspace_id.to_string(), ligus_core::workspace_config_session_id(workspace_id).to_string()],
        )?;
        out.extend(self.witnesses_of(workspace_id, ligus_core::workspace_config_session_id(workspace_id))?);
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

    /// The messages appended after row `after`, as `(session, message, signing
    /// device)` in arrival order: what one sync pull brought in, message by
    /// message. A pull can carry several user turns for one chat (two posts a
    /// few ms apart); dispatch must consider each of them, not only the
    /// chat's last. The signer lets the caller skip its own device's posts.
    ///
    /// Each as `(workspace, session, message, signing device)`: the log is
    /// (workspace, session), the workspace the one the event carries.
    pub fn messages_after(&self, after: i64) -> Result<Vec<(Uuid, Uuid, Uuid, Option<Uuid>)>> {
        Ok(self
            .query_envelopes(
                "SELECT envelope_json FROM events WHERE row_id > ?1 AND kind = 'messageAppended' ORDER BY row_id ASC",
                [after],
            )?
            .into_iter()
            .filter_map(|e| match e.payload {
                SessionEvent::MessageAppended { message } => Some((e.workspace_id, e.session_id, message.id, e.signer_device_id)),
                _ => None,
            })
            .collect())
    }

    /// Whether this workspace already carries a device certificate binding
    /// `device_id` to `account_id` — so identity publication stays idempotent
    /// (and re-publishes once when the account id changes, e.g. GitHub sign-in).
    /// Workspaces where some device published its key-agreement key (B1).
    pub fn workspaces_with_ka_publications(&self) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT workspace_id FROM events WHERE kind = 'deviceKeyAgreementPublished' ORDER BY workspace_id")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            if let Ok(id) = Uuid::parse_str(&row?) {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Whether the log (`workspace_id`, `session_id`) itself carries a
    /// certificate for (device, account).
    pub fn has_device_certificate_in(&self, workspace_id: Uuid, session_id: Uuid, device_id: Uuid, account_id: Uuid) -> Result<bool> {
        let envs = self.query_envelopes(
            "SELECT envelope_json FROM events WHERE session_id = ?1 AND workspace_id = ?2 AND kind = 'deviceCertificateAdded' ORDER BY row_id",
            [session_id.to_string(), workspace_id.to_string()],
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
            "SELECT envelope_json FROM events WHERE workspace_id = ?1 AND kind = 'deviceCertificateAdded' ORDER BY row_id",
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
    /// (`ligus_core::copies`): only a verified copy counts against a verified
    /// one, and only the same signer's second copy is an equivocation (review
    /// P-A2c). An unsigned copy of a signed event can never count, so it is
    /// refused here and kept only as a lamport witness (ingest refuses
    /// unsigned envelopes anyway; this covers every other path in).
    fn note_equivocation(&self, env: &SessionEventEnvelope, sealed_epoch: Option<u32>) -> Result<()> {
        let stored: Option<String> = self
            .conn
            .query_row(
                "SELECT envelope_json FROM events WHERE workspace_id = ?1 AND event_id = ?2",
                [env.workspace_id.to_string(), env.event_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let Some(stored) = stored.and_then(|j| serde_json::from_str::<SessionEventEnvelope>(&j).ok()) else { return Ok(()) };
        if !ligus_core::events::equivocates(&stored, env) {
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

    /// Whether `workspace_id` holds an event under `event_id`. Ids are
    /// unique per workspace: another workspace's row under the same id is
    /// another log's.
    pub fn has_event_in(&self, workspace_id: Uuid, event_id: Uuid) -> Result<bool> {
        Ok(self
            .conn
            .prepare_cached("SELECT 1 FROM events WHERE workspace_id = ?1 AND event_id = ?2")?
            .exists([workspace_id.to_string(), event_id.to_string()])?)
    }

    /// Whether any workspace holds an event under `event_id`.
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
        let env = fixture_next(env, self.lamport_head(env.workspace_id, env.session_id)?);
        self.ingest(&env)
    }

    /// [`EventStore::ingest`], recording the key epoch the body arrived sealed
    /// under (B1). The first arrival of one copy keeps its epoch: a re-seal
    /// by an old-key holder only repeats it.
    ///
    /// Another copy under an id this store holds is kept beside it (the
    /// `equivocations` table), whichever arrived first: the fold decides
    /// which copies count (`ligus_core::copies`), so arrival order never does.
    pub fn ingest_sealed(&mut self, env: &SessionEventEnvelope, sealed_epoch: Option<u32>) -> Result<bool> {
        self.ingest_inner(env, sealed_epoch, false)
    }

    /// [`EventStore::ingest`] for an event authored for this device to store
    /// that this device didn't sign (a phone's signature over a draft made
    /// here): refused with [`EventStoreError::StaleDraft`] if its signer's
    /// chain moved past the draft's link meanwhile (see
    /// [`EventStore::append_envelope`]).
    pub fn ingest_authored(&mut self, env: &SessionEventEnvelope) -> Result<bool> {
        self.ingest_inner(env, env.sealed_epoch(), true)
    }

    fn ingest_inner(&mut self, env: &SessionEventEnvelope, sealed_epoch: Option<u32>, authored: bool) -> Result<bool> {
        // Under the write lock from the start (see [`write_tx`]): whether the
        // id is stored and the next sequence are read and the row written
        // with no other connection's write in between, so two connections
        // ingesting one event (the serve sync loop and a one-shot sync, say)
        // store it once, and the second sees it stored instead of failing.
        let tx = write_tx(&self.conn)?;
        // Another copy under an id its own workspace holds is kept beside it
        // (the fold decides); another workspace's row under the id is
        // another log's and changes nothing here.
        if self.has_event_in(env.workspace_id, env.event_id)? {
            self.note_equivocation(env, sealed_epoch)?;
            tx.commit()?;
            return Ok(false);
        }
        if authored && self.forks_chain(env)? {
            return Err(EventStoreError::StaleDraft);
        }
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
    /// whichever arrived first, see `ligus_core::copies`). For a reader that
    /// verifies what it relies on and must not be blinded by a forged copy
    /// that happened to arrive first.
    ///
    /// The log is (`workspace_id`, `session_id`): rows another workspace
    /// stamped under the same session id are never among them.
    pub fn list_copies_in(&self, workspace_id: Uuid, session_id: Uuid) -> Result<Vec<SessionEventEnvelope>> {
        self.query_envelopes(
            "SELECT envelope_json FROM (SELECT 0 AS t, sequence AS o, envelope_json FROM events WHERE session_id = ?1 AND workspace_id = ?2 \
             UNION ALL SELECT 1, row_id, envelope_json FROM equivocations WHERE session_id = ?1 AND workspace_id = ?2) ORDER BY t, o",
            [session_id.to_string(), workspace_id.to_string()],
        )
    }

    /// [`Self::list_copies_in`] the workspace [`Self::workspace_of`] names
    /// (nothing when it names none). For a reader that has only a session
    /// id; one that knows the workspace passes it.
    pub fn list_copies(&self, session_id: Uuid) -> Result<Vec<SessionEventEnvelope>> {
        match self.workspace_of(session_id)? {
            Some(ws) => self.list_copies_in(ws, session_id),
            None => Ok(Vec::new()),
        }
    }

    /// A log's envelopes (`workspace_id`, `session_id`) in sequence order.
    pub fn list_in(&self, workspace_id: Uuid, session_id: Uuid) -> Result<Vec<SessionEventEnvelope>> {
        self.query_envelopes(
            "SELECT envelope_json FROM events WHERE session_id = ?1 AND workspace_id = ?2 ORDER BY sequence ASC",
            [session_id.to_string(), workspace_id.to_string()],
        )
    }

    /// [`Self::list_in`] the workspace [`Self::workspace_of`] names (nothing
    /// when it names none).
    pub fn list(&self, session_id: Uuid) -> Result<Vec<SessionEventEnvelope>> {
        match self.workspace_of(session_id)? {
            Some(ws) => self.list_in(ws, session_id),
            None => Ok(Vec::new()),
        }
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

    /// A log's envelopes with `sequence > after`, in order — the sync fetch
    /// path.
    pub fn list_after(&self, workspace_id: Uuid, session_id: Uuid, after: i64) -> Result<Vec<SessionEventEnvelope>> {
        self.query_envelopes(
            "SELECT envelope_json FROM events WHERE session_id = ?1 AND workspace_id = ?3 AND sequence > ?2 ORDER BY sequence ASC",
            rusqlite::params![session_id.to_string(), after, workspace_id.to_string()],
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
        let tx = write_tx(&self.conn)?;
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
        tx.execute(
            "DELETE FROM parked_bodies WHERE workspace = ?1 AND seq IN \
             (SELECT seq FROM (SELECT seq, SUM(length(CAST(body AS BLOB))) OVER (ORDER BY priority DESC, seq DESC ROWS UNBOUNDED PRECEDING) AS kept \
              FROM parked_bodies WHERE workspace = ?1) WHERE kept > ?2)",
            rusqlite::params![workspace, PARKED_BYTES_KEEP],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Re-rank parked bodies (the keyring changed: a rotation they wait for
    /// may now be authorized, or one they claim may have turned out junk).
    pub fn set_parked_priorities(&self, workspace: &str, priorities: &[(u64, u8)]) -> Result<()> {
        let tx = write_tx(&self.conn)?;
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
            .prepare("SELECT event_id FROM pushed WHERE workspace = ?1 ORDER BY event_id")?;
        let rows = stmt.query_map([workspace], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for s in rows {
            if let Ok(id) = Uuid::parse_str(&s?) {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Hard-delete one log (`workspace_id`, `session_id`): its events and
    /// the other copies held under its ids (irreversible). Another
    /// workspace's log under the same session id stays. Returns the number
    /// of log rows removed.
    pub fn delete_session_in(&self, workspace_id: Uuid, session_id: Uuid) -> Result<usize> {
        let (sid, ws) = (session_id.to_string(), workspace_id.to_string());
        let tx = write_tx(&self.conn)?;
        let n = tx.execute("DELETE FROM events WHERE session_id = ?1 AND workspace_id = ?2", [&sid, &ws])?;
        tx.execute("DELETE FROM equivocations WHERE session_id = ?1 AND workspace_id = ?2", [&sid, &ws])?;
        let left: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM events WHERE session_id = ?1)", [&sid], |r| r.get(0))?;
        tx.commit()?;
        if !left {
            self.delete_compaction(session_id, None)?;
        }
        Ok(n)
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
        signal: &ligus_core::signal::GuardSignal,
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
    ) -> Result<Vec<ligus_core::signal::GuardSignalRecord>> {
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
            out.push(ligus_core::signal::GuardSignalRecord {
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
    pub fn session_facts(&self, workspace_id: Uuid, session_id: Uuid) -> Result<Vec<(i64, i64, String, String)>> {
        let mut stmt = self.conn.prepare("SELECT row_id, at_ms, kind, record_json FROM facts WHERE session_id = ?1 AND workspace_id = ?2 ORDER BY row_id ASC")?;
        let rows = stmt.query_map([session_id.to_string(), workspace_id.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
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

    /// Each stored event (any log of `workspace_id`, equivocations
    /// included) whose envelope names `device` as its signer, oldest first,
    /// until `visit` returns `false`. Through `idx_events_by_signer`. Rows
    /// that don't parse are skipped. The signature is **not** checked here:
    /// ingest stores some trust-bootstrap kinds from a signer it can't verify
    /// yet, so a caller that reads who the device acted for checks each one
    /// (`crate::device_enroll::chosen_accounts`).
    pub fn for_each_signed_by(
        &self,
        workspace_id: Uuid,
        device: Uuid,
        mut visit: impl FnMut(SessionEventEnvelope) -> bool,
    ) -> Result<()> {
        for table in ["events", "equivocations"] {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT envelope_json FROM {table} WHERE json_extract(envelope_json, '$.signerDeviceId') = ?1 \
                 AND +workspace_id = ?2 ORDER BY row_id ASC"
            ))?;
            let mut rows = stmt.query(rusqlite::params![device.to_string(), workspace_id.to_string()])?;
            while let Some(row) = rows.next()? {
                let json: String = row.get(0)?;
                let Ok(env) = serde_json::from_str::<SessionEventEnvelope>(&json) else { continue };
                if !visit(env) {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// The newest event `signer` put on `session_id` here (by local row
    /// order) with a lamport below `below`: what a new event of that signer at
    /// lamport `below` names as `ActorStamp::prev` (a chain link is always
    /// older than the event, `ligus_core::lamport`).
    pub fn last_signed_by(&self, workspace_id: Uuid, session_id: Uuid, signer: Uuid, below: u64) -> Result<Option<Uuid>> {
        let id: Option<String> = self
            .conn
            .query_row(
                LAST_SIGNED_BY_SQL,
                rusqlite::params![session_id.to_string(), signer.to_string(), below.min(i64::MAX as u64) as i64, workspace_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(id.and_then(|s| Uuid::parse_str(&s).ok()))
    }

    /// The session ids of one workspace.
    pub fn sessions_of_workspace(&self, workspace_id: Uuid) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT session_id FROM events WHERE workspace_id = ?1 ORDER BY session_id")?;
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

    /// Logs (workspace, session) with an event after row `after`, in
    /// first-changed order: what a sync pull changed, each under the
    /// workspace its rows carry (never one read from another log).
    pub fn logs_changed_after(&self, after: i64) -> Result<Vec<(Uuid, Uuid)>> {
        let mut stmt = self.conn.prepare(
            "SELECT workspace_id, session_id FROM events WHERE row_id > ?1 GROUP BY workspace_id, session_id ORDER BY MIN(row_id)",
        )?;
        let rows = stmt.query_map([after], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (ws, sid) = row?;
            if let (Ok(ws), Ok(sid)) = (Uuid::parse_str(&ws), Uuid::parse_str(&sid)) {
                out.push((ws, sid));
            }
        }
        Ok(out)
    }

    /// Highest equivocation-evidence row (0 when none).
    pub fn max_equivocation_row(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT COALESCE(MAX(row_id), 0) FROM equivocations", [], |row| row.get(0))?)
    }

    /// Logs (workspace, session) that gained equivocation evidence after
    /// row `after`.
    pub fn logs_equivocated_after(&self, after: i64) -> Result<Vec<(Uuid, Uuid)>> {
        let mut stmt = self.conn.prepare(
            "SELECT workspace_id, session_id FROM equivocations WHERE row_id > ?1 GROUP BY workspace_id, session_id ORDER BY MIN(row_id)",
        )?;
        let rows = stmt.query_map([after], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (ws, sid) = row?;
            if let (Ok(ws), Ok(sid)) = (Uuid::parse_str(&ws), Uuid::parse_str(&sid)) {
                out.push((ws, sid));
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
             UNION SELECT workspace_id, session_id FROM equivocations WHERE row_id > ?2 ORDER BY 1, 2",
        )?;
        let rows = stmt.query_map([after_event, after_equivocation], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out: Vec<Uuid> = Vec::new();
        for row in rows {
            let (ws, sid) = row?;
            let (Ok(ws), Ok(sid)) = (Uuid::parse_str(&ws), Uuid::parse_str(&sid)) else { continue };
            if sid == ligus_core::workspace_config_session_id(ws) && !out.contains(&ws) {
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
    ///
    /// The log is the one [`Self::workspace_of`] names (none: `None`); a
    /// reader that knows the workspace uses [`Self::load_session_in`].
    pub fn load_session(&self, session_id: Uuid) -> Result<Option<ChatSession>> {
        self.load_session_with_founder(session_id, None, None)
    }

    /// [`Self::load_session`] of the log (`workspace_id`, `session_id`).
    pub fn load_session_in(&self, workspace_id: Uuid, session_id: Uuid) -> Result<Option<ChatSession>> {
        self.load_session_authorized(workspace_id, session_id, None, None, WorkspaceAuthz::Unchecked)
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
        let Some(workspace_id) = self.workspace_of(session_id)? else { return Ok(None) };
        self.load_session_authorized(workspace_id, session_id, trusted_founder, trusted_workspace, WorkspaceAuthz::Unchecked)
    }

    /// [`load_session_with_founder`] for a chat log, folded against its
    /// workspace's config-log roster history (`ligus_core::project_authorized`).
    /// `ws` is the workspace authorization mode; for a timeline, cached per
    /// timeline fingerprint, and refolded while any event is held and the
    /// config log has grown since.
    ///
    /// The log is (`workspace_id`, `session_id`), the workspace taken from
    /// the reader's context: rows another workspace stamped under the same
    /// session id are another log and never fold here.
    pub fn load_session_authorized(
        &self,
        workspace_id: Uuid,
        session_id: Uuid,
        trusted_founder: Option<&str>,
        trusted_workspace: Option<Uuid>,
        ws: WorkspaceAuthz<'_>,
    ) -> Result<Option<ChatSession>> {
        // Project the full stream: the fold orders canonically and picks its
        // base deterministically, so the result is independent of this
        // device's ingestion order.
        let sig: RowsSig = self.rows_sig(workspace_id, session_id)?;
        let (fingerprint, config_events) = match ws {
            WorkspaceAuthz::Unchecked => (0, 0),
            WorkspaceAuthz::NoConfigLog => (1, 0),
            WorkspaceAuthz::Timeline(t) => (t.fingerprint(), t.events_held()),
        };
        let key: ProjectionKey = (workspace_id, session_id, trusted_founder.map(str::to_string), trusted_workspace, fingerprint);
        if let Some((cached, cfg_seen, session)) = self.projections.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            let still_held = session.as_ref().is_some_and(|s| !s.authz.held.is_empty());
            if *cached == sig && (!still_held || *cfg_seen == config_events) {
                return Ok(session.clone());
            }
        }
        let envelopes = self.log_with_witnesses(workspace_id, session_id)?;
        let session = project_authorized(&envelopes, trusted_founder, trusted_workspace, ws);
        let mut cache = self.projections.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= PROJECTION_CACHE_MAX && !cache.contains_key(&key) {
            cache.clear();
        }
        cache.insert(key, (sig, config_events, session.clone()));
        Ok(session)
    }

    /// A workspace's config log folded, with its roster at every position
    /// (`ligus_core::project_config`). Cached per config-log rows.
    pub fn load_config(
        &self,
        workspace_id: Uuid,
        trusted_founder: Option<&str>,
        trusted_workspace: Option<Uuid>,
    ) -> Result<(Option<ChatSession>, Arc<ConfigTimeline>)> {
        let config_id = ligus_core::workspace_config_session_id(workspace_id);
        let sig: RowsSig = self.rows_sig(workspace_id, config_id)?;
        let key = (config_id, trusted_founder.map(str::to_string), trusted_workspace);
        if let Some((cached, session, timeline)) = self.configs.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            if *cached == sig {
                return Ok((session.clone(), Arc::clone(timeline)));
            }
        }
        let envelopes = self.log_with_witnesses(workspace_id, config_id)?;
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
    pub fn config_founded_at(&self, workspace_id: Uuid) -> Result<Option<ligus_core::Timestamp>> {
        let Some(pin) = self.workspace_pin(workspace_id)? else { return Ok(None) };
        let envs = self.list_copies_in(workspace_id, ligus_core::workspace_config_session_id(workspace_id))?;
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
            ligus_core::crypto::verify_envelope(&e, &key).ok()?;
            let a = e.actor_stamp?.actor;
            (a.account_id == Some(account)).then_some(a.contact).flatten()
        }))
    }

    /// The issue time of the newest contact record seen for `cid`.
    pub fn contact_newest_seen(&self, cid: &str) -> Result<Option<ligus_core::Timestamp>> {
        let ms: Option<i64> = self
            .conn
            .query_row("SELECT issued_ms FROM contact_seen WHERE cid = ?1", [cid], |r| r.get(0))
            .optional()?;
        Ok(ms.and_then(|ms| time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok()).map(ligus_core::Timestamp))
    }

    /// Record that a record issued at `issued` was seen for `cid` (only ever
    /// moves forward).
    pub fn note_contact_seen(&self, cid: &str, issued: ligus_core::Timestamp) -> Result<()> {
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

    /// Keep (or, with `None`, forget) the redeem key seed of the invite this
    /// device redeems to join `workspace_id`: set with the invite, so a seed
    /// from an earlier code never proves a later one.
    pub fn set_join_redeem(&self, workspace_id: Uuid, seed: Option<&[u8; 32]>) -> Result<()> {
        match seed {
            Some(seed) => self.set_join_redeem_key(workspace_id, seed),
            None => self.clear_join_redeem_key(workspace_id),
        }
    }

    /// Keep the redeem key seed of person invite `invite_id` this device
    /// issued, for its code.
    pub fn set_issued_invite_key(&self, workspace_id: Uuid, invite_id: Uuid, seed: &[u8; 32]) -> Result<()> {
        let hex: String = seed.iter().map(|b| format!("{b:02x}")).collect();
        self.conn.execute(
            "INSERT OR REPLACE INTO issued_invite_keys (workspace_id, invite_id, seed) VALUES (?1, ?2, ?3)",
            rusqlite::params![workspace_id.to_string(), invite_id.to_string(), hex],
        )?;
        Ok(())
    }

    /// The redeem key seed of person invite `invite_id` this device issued,
    /// forgotten here: its code carries it from now on.
    pub fn take_issued_invite_key(&self, workspace_id: Uuid, invite_id: Uuid) -> Result<Option<[u8; 32]>> {
        let seed = self.issued_invite_key(workspace_id, invite_id)?;
        self.conn.execute(
            "DELETE FROM issued_invite_keys WHERE workspace_id = ?1 AND invite_id = ?2",
            rusqlite::params![workspace_id.to_string(), invite_id.to_string()],
        )?;
        Ok(seed)
    }

    /// The redeem key seed of person invite `invite_id` this device issued.
    pub fn issued_invite_key(&self, workspace_id: Uuid, invite_id: Uuid) -> Result<Option<[u8; 32]>> {
        let v: Option<String> = self
            .conn
            .query_row(
                "SELECT seed FROM issued_invite_keys WHERE workspace_id = ?1 AND invite_id = ?2",
                rusqlite::params![workspace_id.to_string(), invite_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v.and_then(|h| crate::box_admission::seed_from_hex(&h)))
    }

    /// Link box relay subject `subject` (minted by this Admin's device) to
    /// the box admission `invite_id` issued with it.
    pub fn set_box_relay_admission(&self, workspace_id: Uuid, subject: &str, invite_id: Uuid) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO box_relay_subjects (workspace_id, subject, invite_id) VALUES (?1, ?2, ?3)",
            rusqlite::params![workspace_id.to_string(), subject, invite_id.to_string()],
        )?;
        Ok(())
    }

    /// The box admission linked to relay subject `subject`, if this device
    /// minted that subject together with an admission.
    pub fn box_relay_admission(&self, workspace_id: Uuid, subject: &str) -> Result<Option<Uuid>> {
        let v: Option<String> = self
            .conn
            .query_row(
                "SELECT invite_id FROM box_relay_subjects WHERE workspace_id = ?1 AND subject = ?2",
                rusqlite::params![workspace_id.to_string(), subject],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v.and_then(|s| Uuid::parse_str(&s).ok()))
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
        let config_id = ligus_core::workspace_config_session_id(workspace_id);
        self.query_envelopes_sealed(
            "SELECT envelope_json, sealed_epoch FROM (SELECT 0 AS t, sequence AS o, envelope_json, sealed_epoch FROM events WHERE session_id = ?1 AND workspace_id = ?2 \
             UNION ALL SELECT 1, row_id, envelope_json, sealed_epoch FROM equivocations WHERE session_id = ?1 AND workspace_id = ?2) ORDER BY t, o",
            [config_id.to_string(), workspace_id.to_string()],
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

    /// Every workspace with a stored row under `session_id`. Session ids are
    /// not namespaced by workspace: anyone who knows a chat's id can stamp
    /// an event with it in another workspace, which is another log
    /// (workspace, session), never the chat's.
    pub fn workspaces_claiming(&self, session_id: Uuid) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare_cached("SELECT DISTINCT workspace_id FROM events WHERE session_id = ?1 ORDER BY workspace_id")?;
        let rows = stmt.query_map([session_id.to_string()], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for w in rows {
            if let Ok(w) = Uuid::parse_str(&w?) {
                out.push(w);
            }
        }
        Ok(out)
    }

    /// Whether `device` signed a stored event on the log (`workspace_id`,
    /// `session_id`).
    pub fn has_rows_signed_by(&self, workspace_id: Uuid, session_id: Uuid, device: Uuid) -> Result<bool> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT 1 FROM events WHERE json_extract(envelope_json, '$.signerDeviceId') = ?3 AND session_id = ?1 AND +workspace_id = ?2 LIMIT 1",
            )?
            .exists([session_id.to_string(), workspace_id.to_string(), device.to_string()])?)
    }

    /// The workspace of a stored session, for a reader that has only its id
    /// (one that knows the workspace from its context passes it instead and
    /// never asks). Never read from arrival order or any other data a member
    /// of another workspace controls:
    ///
    /// - a workspace's config log (its id derived from the workspace) is that
    ///   workspace's, whatever else claims the id;
    /// - otherwise the one workspace with rows under the id;
    /// - several: none (fail closed; [`crate::ChatService::workspace_of`]
    ///   prefers the one this device is a member of and wrote in).
    pub fn workspace_of(&self, session_id: Uuid) -> Result<Option<Uuid>> {
        let claims = self.workspaces_claiming(session_id)?;
        if let Some(ws) = claims.iter().find(|w| ligus_core::workspace_config_session_id(**w) == session_id) {
            return Ok(Some(*ws));
        }
        match claims.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(*one)),
            _ => {
                tracing::warn!(target: "event_store", %session_id, workspaces = claims.len(), "a session id is claimed by several workspaces; reading none of them without a workspace");
                Ok(None)
            }
        }
    }

    /// Row id of the latest `sessionSnapshot` on a log, if any.
    fn latest_snapshot_row(&self, workspace_id: Uuid, session_id: Uuid) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT MAX(row_id) FROM events WHERE session_id = ?1 AND workspace_id = ?2 AND kind = 'sessionSnapshot'",
                [session_id.to_string(), workspace_id.to_string()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten())
    }

    /// How many events a session has accumulated since its latest snapshot —
    /// drives periodic re-snapshotting.
    pub fn rows_since_last_snapshot(&self, workspace_id: Uuid, session_id: Uuid) -> Result<i64> {
        let n = match self.latest_snapshot_row(workspace_id, session_id)? {
            Some(row) => self.conn.query_row(
                "SELECT COUNT(*) FROM events WHERE session_id = ?1 AND workspace_id = ?3 AND row_id > ?2",
                rusqlite::params![session_id.to_string(), row, workspace_id.to_string()],
                |r| r.get::<_, i64>(0),
            )?,
            None => self.conn.query_row(
                "SELECT COUNT(*) FROM events WHERE session_id = ?1 AND workspace_id = ?2",
                [session_id.to_string(), workspace_id.to_string()],
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
        self.purge_foreign_workspaces_with(keep, own_device, backup, &mut || {})
    }

    /// [`EventStore::purge_foreign_workspaces`], running `after_backup` once
    /// the backup is taken (tests: another connection's write there).
    fn purge_foreign_workspaces_with(
        &mut self,
        keep: &HashSet<Uuid>,
        own_device: Uuid,
        backup: Option<&Path>,
        after_backup: &mut dyn FnMut(),
    ) -> Result<Vec<(Uuid, usize)>> {
        // The backup comes first (it can't be taken inside a transaction),
        // with each doomed workspace's rows as they stood just before it.
        // Under the write lock the decision is made again, and a workspace
        // is dropped only if its rows are still exactly those: one that
        // gained a row since (a late event of another member, or this
        // device's own first one there) may hold a row the backup lacks, and
        // stays for the next run. Writes elsewhere in the file never stop
        // the cleanup. A backup that ends up covering nothing deleted (or a
        // failure after it was taken) is removed again: no stray full copies
        // of the database pile up.
        let mut before: HashMap<Uuid, (i64, i64, i64, i64)> = HashMap::new();
        if let Some(path) = backup {
            let doomed = self.foreign_workspaces(keep, own_device)?;
            if doomed.is_empty() {
                return Ok(doomed);
            }
            for (ws, _) in &doomed {
                before.insert(*ws, self.workspace_rows(*ws)?);
            }
            // Only a file this call made is ever removed: a path that already
            // holds something (an earlier run's backup, the only copy of what
            // that run deleted) is left alone and the cleanup doesn't run.
            if path.exists() {
                return Err(EventStoreError::BackupExists(path.to_path_buf()));
            }
            if let Err(e) = self.conn.execute("VACUUM INTO ?1", [path.to_string_lossy()]) {
                // (VACUUM INTO refuses a path that exists; one that appeared
                // meanwhile isn't this call's to remove.)
                if !e.to_string().contains("already exists") {
                    let _ = std::fs::remove_file(path);
                }
                return Err(e.into());
            }
            after_backup();
        }
        let dropped = self.purge_locked(keep, own_device, backup.map(|_| &before));
        if let Some(path) = backup {
            if !matches!(&dropped, Ok(d) if !d.is_empty()) {
                let _ = std::fs::remove_file(path);
            }
        }
        dropped
    }

    /// (row count, highest row) of one workspace's events and of the copies
    /// beside them.
    fn workspace_rows(&self, ws: Uuid) -> Result<(i64, i64, i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT (SELECT COUNT(*) FROM events WHERE workspace_id = ?1), (SELECT COALESCE(MAX(row_id), 0) FROM events WHERE workspace_id = ?1), \
             (SELECT COUNT(*) FROM equivocations WHERE workspace_id = ?1), (SELECT COALESCE(MAX(row_id), 0) FROM equivocations WHERE workspace_id = ?1)",
            [ws.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?)
    }

    /// The purge's deletes, decided under the write lock. With `backed_up`,
    /// only workspaces whose rows are unchanged since the backup go.
    fn purge_locked(
        &mut self,
        keep: &HashSet<Uuid>,
        own_device: Uuid,
        backed_up: Option<&HashMap<Uuid, (i64, i64, i64, i64)>>,
    ) -> Result<Vec<(Uuid, usize)>> {
        let tx = write_tx(&self.conn)?;
        let mut doomed = self.foreign_workspaces(keep, own_device)?;
        if let Some(before) = backed_up {
            let mut unchanged = Vec::new();
            for (ws, n) in doomed {
                if before.get(&ws) == Some(&self.workspace_rows(ws)?) {
                    unchanged.push((ws, n));
                }
            }
            doomed = unchanged;
        }
        if doomed.is_empty() {
            return Ok(doomed);
        }
        let rooms: Vec<String> = {
            let mut stmt = self.conn.prepare("SELECT workspace FROM sync_cursor UNION SELECT DISTINCT workspace FROM pushed ORDER BY 1")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
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
            for room in rooms.iter().filter(|r| ligus_core::room_workspace_id(r) == *ws) {
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
    /// log head and chain link, `ligus_core::lamport`): an event here or one
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

        // Decided and done under the write lock: an event or another copy
        // stored meanwhile through another connection is either seen here
        // (and kept as a witness if pruned) or comes after.
        let tx = write_tx(&self.conn)?;
        // A completion counts for its own log's chunks only: another
        // workspace's completion under the same message id prunes nothing.
        let mut completed: std::collections::HashSet<(String, String, String)> = std::collections::HashSet::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT workspace_id, session_id, envelope_json FROM events WHERE kind = 'messageCompleted' ORDER BY row_id")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
            for row in rows {
                let (ws, sid, j) = row?;
                if let Some(id) = message_id_of(&j) {
                    completed.insert((ws, sid, id));
                }
            }
        }
        if completed.is_empty() {
            return Ok(0);
        }

        let mut doomed: Vec<(String, String)> = Vec::new(); // (workspace, event_id)
        let mut witnesses: Vec<SessionEventEnvelope> = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT workspace_id, session_id, event_id, envelope_json FROM events WHERE kind = 'messageChunkReceived' ORDER BY row_id",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))?;
            for row in rows {
                let (ws, sid, event_id, json) = row?;
                if message_id_of(&json).is_some_and(|m| completed.contains(&(ws.clone(), sid, m))) {
                    if let Ok(env) = serde_json::from_str::<SessionEventEnvelope>(&json) {
                        witnesses.push(env);
                    }
                    doomed.push((ws, event_id));
                }
            }
        }

        // Every other copy held under a pruned id goes with it (left alone,
        // one would fold in the pruned chunk's place), a witness like it.
        for (ws, event_id) in &doomed {
            for env in self.query_envelopes("SELECT envelope_json FROM equivocations WHERE workspace_id = ?1 AND event_id = ?2 ORDER BY row_id", [ws, event_id])? {
                witnesses.push(env);
            }
        }
        for env in &witnesses {
            self.note_lamport_witness(env)?;
        }
        let mut removed = 0;
        for (ws, event_id) in &doomed {
            removed += tx.execute("DELETE FROM events WHERE workspace_id = ?1 AND event_id = ?2", [ws, event_id])?;
            tx.execute("DELETE FROM equivocations WHERE workspace_id = ?1 AND event_id = ?2", [ws, event_id])?;
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
/// the head, the lamport one past it (`ligus_core::lamport`), signed by the
/// fixture device (a genuine event is always signed, and an unsigned one
/// can't carry a stamp past ingest).
#[cfg(test)]
pub(crate) fn fixture_next(env: &SessionEventEnvelope, log_head: Option<ligus_core::CfgFrontier>) -> SessionEventEnvelope {
    let mut env = env.clone();
    env.clear_raw();
    if env.actor_stamp.is_none() {
        env.actor_stamp = Some(ligus_core::ActorStamp {
            actor: ligus_core::ActorIdentity::new("", "", ligus_core::ActorKind::System),
            recorded_at: ligus_core::Timestamp::epoch(),
            cfg_frontier: None,
            presence: None,
            runtime_effect: false,
            prev: None,
            sig_alg: Default::default(),
            log_head: None,
        });
    }
    ligus_core::lamport::stamp_next(&mut env, log_head);
    let kp = ligus_core::SigningKeypair::from_seed(&[0x46; 32]).expect("fixture key");
    ligus_core::sign_envelope(&mut env, FIXTURE_DEVICE, &kp);
    env
}

/// Tests only: `env` signed as the fixture device, as is (a genuine event is
/// always signed; ingest refuses an unsigned one).
#[cfg(test)]
pub(crate) fn fixture_signed(env: &SessionEventEnvelope) -> SessionEventEnvelope {
    let mut env = env.clone();
    env.clear_raw();
    let kp = ligus_core::SigningKeypair::from_seed(&[0x46; 32]).expect("fixture key");
    ligus_core::sign_envelope(&mut env, FIXTURE_DEVICE, &kp);
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Final review (area B): parked bodies are capped by bytes too, not
    /// only by count, so large junk under an unknown key can't fill the disk
    /// (or memory, when every parked body is replayed), and the body worth
    /// most stays.
    #[test]
    fn final_review_parked_bodies_are_capped_by_bytes() {
        let store = EventStore::open_in_memory().unwrap();
        let keep = serde_json::json!({"version": 2, "kid": "aaaaaaaaaaaaaaaa", "ciphertext": "pending"});
        store.park_bodies("r", &[(1, &keep, 9)]).unwrap();
        let big = serde_json::json!({"version": 77, "kid": "deadbeefdeadbeef", "ciphertext": "A".repeat(4 << 20)});
        for s in 2..=40u64 {
            store.park_bodies("r", &[(s, &big, 0)]).unwrap();
        }
        let parked = store.parked_bodies("r").unwrap();
        let bytes: usize = parked.iter().map(|(_, b)| b.to_string().len()).sum();
        assert!(bytes as i64 <= PARKED_BYTES_KEEP, "{bytes} bytes parked");
        assert!(parked.iter().any(|(s, _)| *s == 1), "the high-priority body was evicted");
        assert!(parked.len() > 1, "some junk still fits");
    }
    use ligus_core::{ChatMessage, MessageRole};

    /// A database made when event ids were unique across workspaces: the
    /// open migrates it (backup first), keeps every row, row id and the
    /// row-id counter, and moves a copy kept aside only because another
    /// workspace held its id into its own workspace's log.
    #[test]
    fn an_old_database_gets_event_ids_unique_per_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ligus.db");
        let (a, w2, sid) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut session = ChatSession::new("c", a, "rt");
        session.id = sid;
        let seed = fixture_next(&SessionEventEnvelope::new(sid, a, 1, SessionEvent::SessionSnapshot { session: Box::new(session) }), None);
        let genuine = fixture_next(&SessionEventEnvelope::new(sid, a, 2, SessionEvent::SessionTitleChanged { title: "genuine".into() }), Some(ligus_core::CfgFrontier { lamport: seed.lamport, event_id: seed.event_id }));
        let mut planted = SessionEventEnvelope::new(sid, w2, 1, SessionEvent::SessionTitleChanged { title: "planted".into() });
        planted.event_id = genuine.event_id;
        let planted = fixture_next(&planted, None);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE events (row_id INTEGER PRIMARY KEY AUTOINCREMENT, event_id TEXT NOT NULL UNIQUE, session_id TEXT NOT NULL, \
                 workspace_id TEXT NOT NULL, sequence INTEGER NOT NULL, kind TEXT NOT NULL, scope TEXT NOT NULL, timestamp TEXT NOT NULL, \
                 envelope_json TEXT NOT NULL, sealed_epoch INTEGER, UNIQUE (session_id, sequence)); \
                 CREATE TABLE equivocations (row_id INTEGER PRIMARY KEY AUTOINCREMENT, event_id TEXT NOT NULL, session_id TEXT NOT NULL, \
                 workspace_id TEXT NOT NULL, kind TEXT NOT NULL, envelope_json TEXT NOT NULL, sealed_epoch INTEGER, UNIQUE (event_id, envelope_json));",
            )
            .unwrap();
            let put = |row: i64, e: &SessionEventEnvelope, seq: i64| {
                conn.execute(
                    "INSERT INTO events (row_id, event_id, session_id, workspace_id, sequence, kind, scope, timestamp, envelope_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'session', 't', ?7)",
                    rusqlite::params![row, e.event_id.to_string(), e.session_id.to_string(), e.workspace_id.to_string(), seq, e.payload.kind_str(), serde_json::to_string(e).unwrap()],
                )
                .unwrap();
            };
            put(1, &seed, 1);
            // The planted copy reached this device first: the genuine one
            // was kept aside.
            put(2, &planted, 2);
            conn.execute(
                "INSERT INTO equivocations (event_id, session_id, workspace_id, kind, envelope_json) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![genuine.event_id.to_string(), sid.to_string(), a.to_string(), genuine.payload.kind_str(), serde_json::to_string(&genuine).unwrap()],
            )
            .unwrap();
            // A pruned row: the counter is past every row left.
            put(9, &fixture_next(&SessionEventEnvelope::new(Uuid::new_v4(), a, 1, SessionEvent::SessionTitleChanged { title: "gone".into() }), None), 1);
            conn.execute("DELETE FROM events WHERE row_id = 9", []).unwrap();
        }
        let store = EventStore::open(&path).unwrap();
        assert!(!needs_event_id_migration(&store.conn).unwrap());
        let backups: Vec<_> = std::fs::read_dir(dir.path()).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().contains(".bak-ws-event-ids-")).collect();
        assert_eq!(backups.len(), 1, "a backup before the rewrite");
        let a_log: Vec<Uuid> = store.list_in(a, sid).unwrap().iter().map(|e| e.event_id).collect();
        assert_eq!(a_log, vec![seed.event_id, genuine.event_id], "the genuine copy is in its own log now");
        assert_eq!(store.list_in(w2, sid).unwrap().len(), 1);
        assert!(store.list_copies_in(a, sid).unwrap().iter().all(|e| e.workspace_id == a));
        let left: i64 = store.conn.query_row("SELECT COUNT(*) FROM equivocations", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0);
        let rows: Vec<i64> = store.conn.prepare("SELECT row_id FROM events ORDER BY row_id").unwrap().query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(&rows[..2], &[1, 2], "row ids kept");
        assert!(rows[2] > 9, "the row-id counter never goes back: {rows:?}");
        // The next open of the upgraded database retires the backup and
        // changes nothing else.
        drop(store);
        EventStore::open(&path).unwrap();
        assert!(upgrade_leftovers(dir.path()).is_empty());
    }

    /// The upgrade's backups and their side files next to a database.
    fn upgrade_leftovers(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(MIGRATION_BACKUP_MARK))
            .collect();
        v.sort();
        v
    }

    /// A database in the old shape (event ids unique across workspaces)
    /// with `n` events of `pad` bytes each.
    fn old_database(path: &Path, n: usize, pad: usize) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE events (row_id INTEGER PRIMARY KEY AUTOINCREMENT, event_id TEXT NOT NULL UNIQUE, session_id TEXT NOT NULL, \
             workspace_id TEXT NOT NULL, sequence INTEGER NOT NULL, kind TEXT NOT NULL, scope TEXT NOT NULL, timestamp TEXT NOT NULL, \
             envelope_json TEXT NOT NULL, sealed_epoch INTEGER, UNIQUE (session_id, sequence));",
        )
        .unwrap();
        let (ws, sid) = (Uuid::new_v4(), Uuid::new_v4());
        let body = "x".repeat(pad);
        let tx = conn.unchecked_transaction().unwrap();
        for i in 0..n {
            let e = SessionEventEnvelope::new(sid, ws, i as i64 + 1, SessionEvent::SessionTitleChanged { title: body.clone() });
            tx.execute(
                "INSERT INTO events (event_id, session_id, workspace_id, sequence, kind, scope, timestamp, envelope_json) VALUES (?1, ?2, ?3, ?4, ?5, 'session', 't', ?6)",
                rusqlite::params![e.event_id.to_string(), sid.to_string(), ws.to_string(), i as i64 + 1, e.payload.kind_str(), serde_json::to_string(&e).unwrap()],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }

    fn still_old(path: &Path) -> bool {
        let conn = Connection::open(path).unwrap();
        needs_event_id_migration(&conn).unwrap()
    }

    /// Short of disk, the upgrade is refused with a clear error, before any
    /// backup is made, and the database is left as it was.
    #[test]
    fn an_upgrade_short_of_disk_is_refused_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ligus.db");
        old_database(&path, 50, 100);
        test_hooks::FREE_BYTES.with(|f| f.set(Some(1000)));
        let err = EventStore::open(&path).err().expect("refused");
        test_hooks::FREE_BYTES.with(|f| f.set(None));
        assert!(matches!(err, EventStoreError::NeedsSpaceToUpgrade { free: 1000, .. }), "{err}");
        let msg = err.to_string();
        assert!(msg.starts_with("free ") && msg.contains("to upgrade the Ligus database") && msg.contains("nothing was changed"), "{msg}");
        assert!(upgrade_leftovers(dir.path()).is_empty());
        assert!(still_old(&path));
        // With room, the next open upgrades.
        EventStore::open(&path).unwrap();
        assert!(!still_old(&path));
    }

    /// A backup or an upgrade that fails part way (a full disk) leaves no
    /// backup or journal behind and the database as it was, so the next
    /// open with room simply tries again.
    #[test]
    fn a_failed_upgrade_leaves_no_backup_behind() {
        for step in ["backup", "migrate"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("ligus.db");
            old_database(&path, 50, 100);
            test_hooks::FAIL_STEP.with(|f| f.set(Some(step)));
            let r = EventStore::open(&path);
            test_hooks::FAIL_STEP.with(|f| f.set(None));
            assert!(r.is_err(), "{step}");
            assert!(upgrade_leftovers(dir.path()).is_empty(), "{step}: {:?}", upgrade_leftovers(dir.path()));
            assert!(still_old(&path), "{step}: the database is untouched");
            EventStore::open(&path).unwrap();
            assert!(!still_old(&path));
        }
    }

    /// Backups (and journals) a crashed attempt left are removed on the next
    /// open, whether it upgrades or the database is already upgraded.
    #[test]
    fn crash_leftovers_are_removed_on_the_next_open() {
        for upgraded in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("ligus.db");
            old_database(&path, 20, 50);
            if upgraded {
                drop(EventStore::open(&path).unwrap());
            }
            for f in ["ligus.db.bak-ws-event-ids-1-000000001-42", "ligus.db.bak-ws-event-ids-1-000000001-42-journal", "ligus.db.bak-ws-event-ids-2-000000002-43"] {
                std::fs::write(dir.path().join(f), b"partial").unwrap();
            }
            std::fs::write(dir.path().join("unrelated.db.bak"), b"keep").unwrap();
            EventStore::open(&path).unwrap();
            let left = upgrade_leftovers(dir.path());
            if upgraded {
                assert!(left.is_empty(), "{left:?}");
            } else {
                // Only this upgrade's own backup, kept until the next open.
                assert_eq!(left.len(), 1, "{left:?}");
                assert!(!left[0].ends_with("-42") && !left[0].ends_with("-43"));
            }
            assert!(dir.path().join("unrelated.db.bak").exists());
        }
    }

    /// The upgrade gives the rewritten table's pages back: the file doesn't
    /// stay about twice its size.
    #[test]
    fn an_upgraded_database_is_compacted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ligus.db");
        old_database(&path, 4000, 1000);
        let before = db_bytes(&path);
        drop(EventStore::open(&path).unwrap());
        let after = db_bytes(&path);
        assert!(after * 10 <= before * 12, "{before} -> {after}");
    }

    /// Ids are unique per workspace: another workspace's row under an id
    /// never keeps the genuine one out of its own log, and a second copy
    /// within one workspace is still kept aside as before.
    #[test]
    fn an_event_id_is_unique_per_workspace_only() {
        let (mut store, sid, wid) = seeded_store();
        let w2 = Uuid::new_v4();
        let genuine = fixture_next(&SessionEventEnvelope::new(sid, wid, 2, SessionEvent::SessionTitleChanged { title: "genuine".into() }), store.lamport_head(wid, sid).unwrap());
        let mut planted = SessionEventEnvelope::new(sid, w2, 1, SessionEvent::SessionTitleChanged { title: "planted".into() });
        planted.event_id = genuine.event_id;
        let planted = fixture_next(&planted, None);
        let before = store.max_row_id().unwrap();
        assert!(store.ingest(&planted).unwrap());
        assert!(store.ingest(&genuine).unwrap(), "the genuine copy is new in its own workspace");
        assert!(store.has_event_in(wid, genuine.event_id).unwrap() && store.has_event_in(w2, genuine.event_id).unwrap());
        assert!(store.list_in(wid, sid).unwrap().iter().any(|e| e.event_id == genuine.event_id && e.workspace_id == wid));
        assert!(store.logs_changed_after(before).unwrap().contains(&(wid, sid)));
        // Same workspace, other content: kept aside, not stored again.
        let mut twin = genuine.clone();
        twin.payload = SessionEvent::SessionTitleChanged { title: "twin".into() };
        let twin = fixture_signed(&twin);
        assert!(!store.ingest(&twin).unwrap());
        assert_eq!(store.list_copies_in(wid, sid).unwrap().iter().filter(|e| e.event_id == genuine.event_id).count(), 2);
        assert_eq!(store.list_in(wid, sid).unwrap().iter().filter(|e| e.event_id == genuine.event_id).count(), 1);
    }

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
                    message: ChatMessage::new(MessageRole::Assistant, "Ligus", "hello"),
                },
            )
            .unwrap();
        assert_eq!(e2.sequence, 2);
        assert_eq!(e3.sequence, 3);
        assert_eq!(store.max_sequence(wid, sid).unwrap(), Some(3));
    }

    #[test]
    fn messages_after_lists_every_message_a_pull_added_in_order() {
        let (mut store, sid, wid) = seeded_store();
        let before = store.max_row_id().unwrap();
        let a = ChatMessage::new(MessageRole::User, "Alex", "@ligus status?");
        let b = ChatMessage::new(MessageRole::User, "Alex", "@coder run the tests");
        let (a_id, b_id) = (a.id, b.id);
        store.append(sid, wid, SessionEvent::MessageAppended { message: a }).unwrap();
        store.append(sid, wid, SessionEvent::SessionTitleChanged { title: "x".into() }).unwrap();
        store.append(sid, wid, SessionEvent::MessageAppended { message: b }).unwrap();
        let fixture = Some(FIXTURE_DEVICE);
        assert_eq!(store.messages_after(before).unwrap(), vec![(wid, sid, a_id, fixture), (wid, sid, b_id, fixture)]);
        assert!(store.messages_after(store.max_row_id().unwrap()).unwrap().is_empty());
    }

    #[test]
    fn the_lamport_head_is_the_newest_event_within_bounds() {
        let (mut store, sid, wid) = seeded_store(); // snapshot @ lamport 1
        let seed = store.list(sid).unwrap()[0].clone();
        let stamped = |payload: SessionEvent, head: Option<&SessionEventEnvelope>| {
            let mut e = SessionEventEnvelope::new(sid, wid, 1, payload);
            e.actor_stamp = seed.actor_stamp.clone();
            ligus_core::lamport::stamp_next(&mut e, head.map(|h| ligus_core::CfgFrontier { lamport: h.lamport, event_id: h.event_id }));
            e
        };
        let a = stamped(SessionEvent::SessionTitleChanged { title: "a".into() }, Some(&seed));
        let b = stamped(SessionEvent::SessionTitleChanged { title: "b".into() }, Some(&a));
        store.ingest(&b).unwrap();
        assert_eq!(store.lamport_head(wid, sid).unwrap().map(|h| h.event_id), Some(seed.event_id), "b's head isn't here yet");
        store.ingest(&a).unwrap();
        assert_eq!(store.lamport_head(wid, sid).unwrap().map(|h| h.event_id), Some(b.event_id), "ingested in any order");
        // Inflated: a lamport no head accounts for never moves the head.
        let mut big = stamped(SessionEvent::SessionTitleChanged { title: "big".into() }, Some(&b));
        big.lamport = u64::MAX;
        store.ingest(&big).unwrap();
        let mut headless = SessionEventEnvelope::new(sid, wid, 7, SessionEvent::SessionTitleChanged { title: "x".into() });
        headless.lamport = 1_000;
        store.ingest(&headless).unwrap();
        assert_eq!(store.lamport_head(wid, sid).unwrap().map(|h| (h.lamport, h.event_id)), Some((3, b.event_id)));
        // An event appended here moves it.
        let mine = store.append(sid, wid, SessionEvent::SessionTitleChanged { title: "mine".into() }).unwrap();
        assert_eq!(mine.lamport, 4);
        assert_eq!(store.lamport_head(wid, sid).unwrap().map(|h| h.event_id), Some(mine.event_id));
    }

    /// A device never counts its own next event from a witness: a refused
    /// event isn't pushed, so a keyholder who sends one to this device alone
    /// (a peer link, a relay serving it to one device) would otherwise get
    /// this device's honest events held on every device that never met it.
    /// An envelope too large for any relay to take is refused when it is
    /// written, with a clear error, instead of waiting in the outbox.
    #[test]
    fn an_envelope_too_large_to_sync_is_refused_when_written() {
        let store = EventStore::open_in_memory().unwrap();
        let big = SessionEventEnvelope::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            1,
            SessionEvent::MessageAppended { message: ligus_core::ChatMessage::new(ligus_core::MessageRole::User, "Alex", "z".repeat(MAX_ENVELOPE_BYTES)) },
        );
        let err = syncable(&big).unwrap_err().to_string();
        assert!(err.contains("too large"), "{err}");
        let small = SessionEventEnvelope::new(Uuid::new_v4(), Uuid::new_v4(), 1, SessionEvent::SessionTitleChanged { title: "t".into() });
        assert!(syncable(&small).is_ok());
        let _ = store;
    }

    #[test]
    fn a_device_counts_from_what_it_stored_never_from_a_witness() {
        let (mut a, sid, wid) = seeded_store();
        let seed = a.list(sid).unwrap()[0].clone();
        let mut b = EventStore::open_in_memory().unwrap();
        b.ingest(&seed).unwrap();
        let mut junk = SessionEventEnvelope::new(sid, wid, 1, SessionEvent::SessionTitleChanged { title: "junk".into() });
        junk.actor_stamp = seed.actor_stamp.clone();
        ligus_core::lamport::stamp_next(&mut junk, Some(ligus_core::CfgFrontier { lamport: seed.lamport, event_id: seed.event_id }));
        a.note_lamport_witness(&junk).unwrap();
        let mine = a.append(sid, wid, SessionEvent::SessionTitleChanged { title: "mine".into() }).unwrap();
        b.ingest(&mine).unwrap();
        assert_eq!(b.lamport_head(wid, sid).unwrap().map(|h| h.event_id), Some(mine.event_id), "not held where the junk never went");
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
        let at = |e: &SessionEventEnvelope| Some(ligus_core::CfgFrontier { lamport: e.lamport, event_id: e.event_id });
        let stamped = |title: &str, head: Option<ligus_core::CfgFrontier>| {
            let mut e = SessionEventEnvelope::new(sid, wid, 1, SessionEvent::SessionTitleChanged { title: title.into() });
            e.actor_stamp = seed.actor_stamp.clone();
            ligus_core::lamport::stamp_next(&mut e, head);
            e
        };
        let x = stamped("x", at(&seed));
        let y = stamped("y", at(&seed));
        let mut early = stamped("forged", Some(ligus_core::CfgFrontier { lamport: 1, event_id: Uuid::new_v4() }));
        early.event_id = x.event_id;
        let mut late = stamped("forged", at(&y));
        late.event_id = x.event_id;
        let g = stamped("g", Some(ligus_core::CfgFrontier { lamport: late.lamport, event_id: x.event_id }));
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
        assert_eq!(a.lamport_head(wid, sid).unwrap(), b.lamport_head(wid, sid).unwrap(), "one head");
        assert_eq!(a.lamport_head(wid, sid).unwrap().map(|k| k.event_id), Some(h.event_id));
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
        let [seed, first, a_side]: [SessionEventEnvelope; 3] = ligus_core::testkit::honest_chain(&[seed, first, a_side]).try_into().unwrap();
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
        use ligus_core::identity::{ActorIdentity, ActorKind, WorkspaceMember, WorkspaceRole};
        use ligus_core::time_util::Timestamp;

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
        let events = ligus_core::testkit::honest_chain(&events);

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
        let path = dir.path().join("ligus.db");
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
        let after_2 = store.list_after(wid, sid, 2).unwrap();
        assert_eq!(after_2.len(), 2);
        assert_eq!(after_2[0].sequence, 3);
    }

    /// An envelope authored against a sequence another connection took in
    /// the meantime is stored at the next place, as written (its signed
    /// sequence unchanged); the row already there is untouched. The same
    /// event id twice is refused.
    #[test]
    fn a_stale_sequence_takes_the_next_place_and_never_replaces_a_row() {
        let (store, sid, wid) = seeded_store();
        let first = store.list(sid).unwrap();
        // snapshot already occupies sequence 1
        let late = SessionEventEnvelope::new(
            sid,
            wid,
            1,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "u", "late"),
            },
        );
        store.append_envelope(&late).unwrap();
        let stored = store.list(sid).unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].event_id, first[0].event_id, "the row at 1 stays");
        assert_eq!(stored[1].event_id, late.event_id);
        assert_eq!(stored[1].sequence, 1, "stored as written");
        assert_eq!(store.max_sequence(wid, sid).unwrap(), Some(2), "at the next place");
        assert!(store.append_envelope(&late).is_err(), "an event id is stored once");
        assert_eq!(store.list(sid).unwrap().len(), 2);
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
        let path = dir.path().join("ligus.db");
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
        assert_eq!(store.rows_since_last_snapshot(wid, sid).unwrap(), 1);
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
        let mut placeholder = ChatMessage::new(MessageRole::Assistant, "Ligus", "");
        placeholder.is_streaming = true;
        let mid = placeholder.id;
        let placeholder = store
            .append(sid, wid, SessionEvent::MessageAppended { message: placeholder })
            .unwrap();
        // Two chunks streamed concurrently (both after the placeholder): the
        // completion counts from one of them, which stays a witness.
        let at = Some(ligus_core::CfgFrontier { lamport: placeholder.lamport, event_id: placeholder.event_id });
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
        assert!(store.lamport_head(wid, sid).unwrap().is_some_and(|h| h.lamport == 4), "the completion still counts from its chunk");
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
        let mut placeholder = ChatMessage::new(MessageRole::Assistant, "Ligus", "");
        placeholder.is_streaming = true;
        let mid = placeholder.id;
        store.append(sid, wid, SessionEvent::MessageAppended { message: placeholder }).unwrap();
        let chunk = store.append(sid, wid, SessionEvent::MessageChunkReceived { message_id: mid, chunk: "Hel".into() }).unwrap();
        // Another signer's copy under the chunk's id, kept beside it.
        let mut other = chunk.clone();
        other.payload = SessionEvent::MessageChunkReceived { message_id: mid, chunk: "lo, forged".into() };
        other.clear_raw();
        let kp = ligus_core::SigningKeypair::generate().unwrap();
        ligus_core::sign_envelope(&mut other, Uuid::new_v4(), &kp);
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
        ligus_core::sign_envelope(&mut copy, Uuid::new_v4(), &kp);
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
        let mut placeholder = ChatMessage::new(MessageRole::Assistant, "Ligus", "");
        placeholder.is_streaming = true;
        let mid = placeholder.id;
        let placeholder = store.append(sid, wid, SessionEvent::MessageAppended { message: placeholder }).unwrap();
        let chunk = store.append(sid, wid, SessionEvent::MessageChunkReceived { message_id: mid, chunk: "Hel".into() }).unwrap();
        let key = |e: &SessionEventEnvelope| Some(ligus_core::CfgFrontier { lamport: e.lamport, event_id: e.event_id });
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
        let head = store.lamport_head(wid, sid).unwrap().unwrap();
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
        let mut store = EventStore::open(dir.path().join("ligus.db")).unwrap();
        let me = Uuid::new_v4();
        let room = "room-alpha";
        let room_ws = ligus_core::room_workspace_id(room);
        let leaked_room = "room-beta";
        let leaked_ws = ligus_core::room_workspace_id(leaked_room);
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

        let backup = dir.path().join("ligus.db.bak");
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

    /// The chain check on every authored append, and the link a new event
    /// names, are index lookups, not scans of the log.
    #[test]
    fn chain_checks_use_the_signer_chain_index() {
        let store = EventStore::open_in_memory().unwrap();
        let plan = |sql: &str, params: &[&dyn rusqlite::ToSql]| -> Vec<String> {
            let mut stmt = store.conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
            stmt.query_map(params, |r| r.get::<_, String>(3)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
        };
        let forks = plan(FORKS_CHAIN_SQL, &[&"s", &"e", &"d", &"p", &"w"]);
        assert!(forks.iter().any(|p| p.contains("idx_events_by_signer")), "{forks:?}");
        let last = plan(LAST_SIGNED_BY_SQL, &[&"s", &"d", &5i64, &"w"]);
        assert!(last.iter().any(|p| p.contains("idx_events_by_signer")), "{last:?}");
    }

    /// A log is read by (session, workspace) through an index on both, not
    /// a scan of every workspace's rows under the session id.
    #[test]
    fn log_reads_use_the_log_indexes() {
        let store = EventStore::open_in_memory().unwrap();
        let plan = |sql: &str| -> String {
            let mut stmt = store.conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
            let rows: Vec<String> = stmt.query_map(["s", "w"], |r| r.get::<_, String>(3)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
            rows.join(" | ")
        };
        let list = plan("SELECT envelope_json FROM events WHERE session_id = ?1 AND workspace_id = ?2 ORDER BY sequence ASC");
        assert!(list.contains("idx_events_log_seq") && !list.contains("TEMP B-TREE"), "{list}");
        let copies = plan("SELECT envelope_json FROM equivocations WHERE session_id = ?1 AND workspace_id = ?2");
        assert!(copies.contains("idx_equivocations_log"), "{copies}");
        let witnesses = plan("SELECT event_id FROM lamport_witnesses WHERE session_id = ?1 AND workspace_id = ?2 ORDER BY row_id ASC");
        assert!(witnesses.contains("idx_lamport_witnesses_log"), "{witnesses}");
        let max = plan("SELECT MAX(sequence) FROM events WHERE session_id = ?1 AND workspace_id = ?2");
        assert!(max.contains("idx_events_log_seq"), "{max}");
    }

    /// A foreign-looking workspace and an unrelated one, for the purge races.
    fn purge_fixture() -> (tempfile::TempDir, std::path::PathBuf, EventStore, Uuid, Uuid, Uuid) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ligus.db");
        let mut store = EventStore::open(&path).unwrap();
        let (me, foreign, mine) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        store.ingest(&purge_msg(foreign, Uuid::new_v4())).unwrap();
        store.ingest(&purge_msg(mine, me)).unwrap();
        (dir, path, store, me, foreign, mine)
    }

    fn purge_msg(ws: Uuid, signer: Uuid) -> SessionEventEnvelope {
        let mut e = SessionEventEnvelope::new(
            Uuid::new_v4(),
            ws,
            1,
            SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "Alex", "hi") },
        );
        e.signer_device_id = Some(signer);
        e
    }

    fn count_in(store: &EventStore, ws: Uuid) -> i64 {
        store.conn.query_row("SELECT COUNT(*) FROM events WHERE workspace_id = ?1", [ws.to_string()], |r| r.get(0)).unwrap()
    }

    /// A backup path that already holds a file (an earlier run's backup, the
    /// only copy of what it deleted) is never removed or overwritten: the
    /// cleanup doesn't run. Fresh names never collide.
    #[test]
    fn purge_never_removes_a_file_already_at_its_backup_path() {
        let (dir, _path, mut store, me, foreign, mine) = purge_fixture();
        let bak = dir.path().join("ligus.db.bak-foreign-purge-1700000000");
        std::fs::write(&bak, b"the only copy of what an earlier run deleted").unwrap();
        let r = store.purge_foreign_workspaces(&HashSet::from([mine]), me, Some(&bak));
        assert!(matches!(r, Err(EventStoreError::BackupExists(_))), "{r:?}");
        assert_eq!(std::fs::read(&bak).unwrap(), b"the only copy of what an earlier run deleted");
        assert_eq!(count_in(&store, foreign), 1, "nothing deleted");
        let names: HashSet<std::path::PathBuf> = (0..100).map(|_| purge_backup_path(dir.path())).collect();
        assert_eq!(names.len(), 100);
    }

    /// This device's own event, stored in a foreign-looking workspace through
    /// another connection after the cleanup's backup (the sync loop, the
    /// service): that workspace isn't deleted (the backup lacks the row), and
    /// a backup that covers nothing deleted is removed again.
    #[test]
    fn purge_keeps_a_workspace_that_gains_an_own_event_after_its_backup() {
        let (dir, path, mut store, me, foreign, mine) = purge_fixture();
        let keep = HashSet::from([mine]);
        let own = purge_msg(foreign, me);
        let other = EventStore::open(&path).unwrap();
        let bak = dir.path().join("bak");
        let dropped = store.purge_foreign_workspaces_with(&keep, me, Some(&bak), &mut || other.append_envelope(&own).unwrap()).unwrap();
        assert!(dropped.is_empty());
        assert_eq!(count_in(&store, foreign), 2, "nothing deleted");
        assert!(!bak.exists(), "no backup left behind");
        assert!(store.purge_foreign_workspaces(&keep, me, Some(&dir.path().join("bak2"))).unwrap().is_empty(), "now this device's");
    }

    /// Another member's late event in the doomed workspace, after the
    /// backup: it isn't in the backup, so the workspace stays this time.
    #[test]
    fn purge_keeps_a_workspace_that_gains_any_event_after_its_backup() {
        let (dir, path, mut store, me, foreign, mine) = purge_fixture();
        let keep = HashSet::from([mine]);
        let late = purge_msg(foreign, Uuid::new_v4());
        let mut other = EventStore::open(&path).unwrap();
        let bak = dir.path().join("bak");
        let dropped = store.purge_foreign_workspaces_with(&keep, me, Some(&bak), &mut || assert!(other.ingest(&late).unwrap())).unwrap();
        assert!(dropped.is_empty());
        assert_eq!(count_in(&store, foreign), 2);
        assert!(!bak.exists());
    }

    /// Sync writing to an unrelated workspace during the backup doesn't stop
    /// the cleanup: the foreign workspace goes, and the backup stays.
    #[test]
    fn purge_goes_ahead_when_only_other_workspaces_change() {
        let (dir, path, mut store, me, foreign, mine) = purge_fixture();
        let keep = HashSet::from([mine]);
        let elsewhere = purge_msg(mine, Uuid::new_v4());
        let mut other = EventStore::open(&path).unwrap();
        let bak = dir.path().join("bak");
        let dropped = store.purge_foreign_workspaces_with(&keep, me, Some(&bak), &mut || assert!(other.ingest(&elsewhere).unwrap())).unwrap();
        assert_eq!(dropped, vec![(foreign, 1)]);
        assert_eq!(count_in(&store, foreign), 0);
        assert_eq!(count_in(&store, mine), 2);
        assert!(bak.exists(), "the backup of what was deleted stays");
    }
}

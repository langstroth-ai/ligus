//! Identity model v2 storage (0.4; design note sections 9, 10.3, 10.4 and
//! 11): redeem key seeds move to the secret store (migration M2), sync state
//! is keyed by the relay it came from and the workspace instead of the room
//! string (M1), and the database carries a schema version (M4).
//!
//! Every step keeps a 0.3 process on the same data dir working: the old
//! tables are never altered by M1, every write of a seed still lands in its
//! old table too, and a seed's row goes only once its invite or admission is
//! closed or expired on the log.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

use super::{db_path, remove_backup, write_tx, EventStore, Result, BUSY_TIMEOUT};
use crate::secret_store::{SecretId, SecretStore};

/// The schema version this build writes and knows (`PRAGMA user_version`):
/// 0 on a database made before 0.4, 1 once the sync tables are keyed by
/// relay and workspace (M1). A database with a higher one opens read-only.
pub const SCHEMA_VERSION: i64 = 1;

/// What a build says when its database was written by a newer one.
pub const NEWER_DATA_MESSAGE: &str = "this data was written by a newer Ligus; update or use another data dir";

/// How long a person invite's seed stays after this build first put it in
/// the secret store, when nothing on the log closes the invite (only its
/// use does, and the code carries the seed from then on).
pub const PERSON_SEED_KEEP_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// What the sync-table migration's backup is named after:
/// `<db>.bak-sync-v2-…`. Kept until the person deletes it.
pub(super) const SYNC_BACKUP_MARK: &str = ".bak-sync-v2-";

/// Every full copy of the database next to it (`<db>.bak-…`: the event-id
/// upgrade, the sync-table migration, the foreign-workspace cleanup). Each
/// holds the redeem key seeds the database held when it was taken. Nothing
/// deletes one by itself; `ligus doctor` lists them.
pub fn seed_backups(db: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(name)) = (db.parent(), db.file_name().and_then(|n| n.to_str())) else { return Vec::new() };
    let prefix = format!("{name}.bak-");
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| {
            e.file_name().to_str().is_some_and(|f| f.starts_with(&prefix) && !["-wal", "-shm", "-journal"].iter().any(|s| f.ends_with(s)))
        })
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

pub(super) fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        -- Durable sync state keyed by the relay it came from and the
        -- workspace (identity model v2, section 9): two relays can each hold
        -- a room of the same name, and their sequences never mix. The room
        -- string only builds relay URLs. The room-keyed tables (sync_cursor,
        -- pushed, parked_bodies) stay as they are, for an older build on the
        -- same data dir and for a downgrade.
        CREATE TABLE IF NOT EXISTS sync_cursor_v2 (
            relay_origin     TEXT NOT NULL,
            workspace_id     TEXT NOT NULL,
            last_fetched_seq INTEGER NOT NULL,
            PRIMARY KEY (relay_origin, workspace_id)
        );
        CREATE TABLE IF NOT EXISTS pushed_v2 (
            relay_origin TEXT NOT NULL,
            workspace_id TEXT NOT NULL,
            event_id     TEXT NOT NULL,
            PRIMARY KEY (relay_origin, workspace_id, event_id)
        );
        CREATE TABLE IF NOT EXISTS parked_bodies_v2 (
            relay_origin TEXT NOT NULL,
            workspace_id TEXT NOT NULL,
            seq          INTEGER NOT NULL,
            body         TEXT NOT NULL,
            priority     INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (relay_origin, workspace_id, seq)
        );
        -- The rooms M1 copied into the tables above. Their old parked bodies
        -- are drained from the copies; any other room's stay in the old table
        -- and are drained from there.
        CREATE TABLE IF NOT EXISTS sync_v2_rooms (
            room TEXT PRIMARY KEY
        );
        "#,
    )?;
    init_seed_copies(conn)
}

/// Device-local: each redeem key seed this build put in the secret store
/// (its secret id, never the seed) and when it first did.
fn init_seed_copies(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS seed_copies (
            secret       TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL,
            invite_id    TEXT,
            first_ms     INTEGER NOT NULL
        );",
    )
}

/// The database's schema version (`PRAGMA user_version`).
pub(super) fn user_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
}

pub(super) fn now_ms() -> i64 {
    (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis()) as i64
}

// ---------------------------------------------------------------------------
// Sync keys
// ---------------------------------------------------------------------------

/// What durable sync state is kept under: the relay it came from and the
/// workspace. The room string is only an address on that relay.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SyncKey {
    pub relay_origin: String,
    pub workspace_id: Uuid,
}

impl SyncKey {
    /// The key for `workspace_id` on the relay at `relay_url` (any form of
    /// its URL: see [`relay_origin`]).
    pub fn new(relay_url: &str, workspace_id: Uuid) -> Self {
        Self { relay_origin: relay_origin(relay_url), workspace_id }
    }
    fn ws(&self) -> String {
        self.workspace_id.to_string()
    }
}

/// A relay URL in the one form sync state is keyed by: scheme and host in
/// lower case, a default port left out, the path without a trailing slash,
/// and never a user, a query or a fragment. A relay served under a path
/// keeps it, so two relays on one host don't share a key.
pub fn relay_origin(url: &str) -> String {
    let url = url.trim();
    if let Ok(u) = url::Url::parse(url) {
        if let Some(host) = u.host_str() {
            let mut out = format!("{}://{}", u.scheme(), host.to_ascii_lowercase());
            if let Some(p) = u.port() {
                out.push_str(&format!(":{p}"));
            }
            out.push_str(u.path().trim_end_matches('/'));
            return out;
        }
    }
    url.trim_end_matches('/').to_ascii_lowercase()
}

// ---------------------------------------------------------------------------
// The advisory lock (`ligus.db.lock`)
// ---------------------------------------------------------------------------

fn lock_path(db: &Path) -> PathBuf {
    let mut p = db.as_os_str().to_os_string();
    p.push(".lock");
    PathBuf::from(p)
}

/// The lock file next to `db`, held shared: every 0.4 connection holds it
/// while open, and M1 runs only under it exclusive. Waits (up to the busy
/// timeout) for an M1 another process is running; past that the store opens
/// without it (SQLite's own locks still keep the file consistent).
pub(super) fn shared_lock(db: &Path) -> Option<std::fs::File> {
    let f = match std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(lock_path(db)) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(target: "event_store", "can't open the store's lock file: {e}");
            return None;
        }
    };
    let deadline = std::time::Instant::now() + BUSY_TIMEOUT;
    loop {
        match f.try_lock_shared() {
            Ok(()) => return Some(f),
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => {
                tracing::warn!(target: "event_store", "opening the store without its lock file: {e}");
                return None;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// M1: sync tables keyed by (relay origin, workspace)
// ---------------------------------------------------------------------------

/// What [`EventStore::migrate_sync_tables`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncMigration {
    /// The database is already at [`SCHEMA_VERSION`]: nothing to do.
    AlreadyDone,
    /// Migrated: the rooms whose state was copied (exactly one connection
    /// names each) and the rooms that start again from 0, with the backup
    /// taken first (none when the old tables were empty).
    Migrated { copied: Vec<String>, reset: Vec<String>, backup: Option<PathBuf> },
    /// Another process has the store open: M1 waits for a start that has it
    /// alone. Sync state starts from 0 meanwhile, which is safe.
    Busy,
    /// The database was written by a newer Ligus ([`NEWER_DATA_MESSAGE`]).
    ReadOnly,
}

/// Every room the old sync tables hold state for.
fn legacy_rooms(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT workspace FROM sync_cursor UNION SELECT workspace FROM pushed UNION SELECT workspace FROM parked_bodies ORDER BY 1")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// Set the version on a database M1 has nothing to move in (made by 0.4, or
/// one whose old sync tables are empty). Inside the schema transaction.
pub(super) fn stamp_if_nothing_to_migrate(conn: &Connection) -> rusqlite::Result<()> {
    let empty = || -> rusqlite::Result<bool> {
        Ok(!conn.prepare("SELECT 1 FROM sync_cursor UNION ALL SELECT 1 FROM pushed UNION ALL SELECT 1 FROM parked_bodies LIMIT 1")?.exists([])?)
    };
    if user_version(conn)? == 0 && empty()? {
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    Ok(())
}

/// A copy of the whole database next to it before M1 (`VACUUM INTO`); none
/// in memory. One that fails part way is removed again.
fn sync_backup(conn: &Connection) -> Result<Option<PathBuf>> {
    let Some(db) = db_path(conn) else { return Ok(None) };
    super::check_upgrade_space(&db)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let mut name = db.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(format!("{SYNC_BACKUP_MARK}{}-{:09}-{}", now.as_secs(), now.subsec_nanos(), std::process::id()));
    let path = db.with_file_name(name);
    let copied = conn.execute("VACUUM INTO ?1", [path.to_string_lossy()]).map(|_| ());
    #[cfg(test)]
    let copied = if super::test_hooks::fails("m1-backup") { Err(rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL), None)) } else { copied };
    if let Err(e) = copied {
        remove_backup(&path);
        return Err(e.into());
    }
    Ok(Some(path))
}

/// Copy the old tables' state into the `_v2` tables (inside the caller's
/// write transaction) and set the version. A room's cursor, pushed set and
/// parked bodies are copied only when exactly one relay is named for it by
/// `connections`; any other room starts from 0 on every relay (fetching
/// again is safe, and the relay's idempotency key folds a second push).
fn copy_sync_state(tx: &Connection, connections: &[(String, String)]) -> Result<(Vec<String>, Vec<String>)> {
    let (mut copied, mut reset) = (Vec::new(), Vec::new());
    for room in legacy_rooms(tx)? {
        let relays: BTreeSet<String> = connections
            .iter()
            .filter(|(url, r)| r.trim() == room.trim() && !url.trim().is_empty())
            .map(|(url, _)| relay_origin(url))
            .collect();
        if relays.len() != 1 {
            reset.push(room);
            continue;
        }
        let origin = relays.into_iter().next().unwrap_or_default();
        let ws = ligus_core::room_workspace_id(&room).to_string();
        tx.execute(
            "INSERT OR IGNORE INTO sync_cursor_v2 (relay_origin, workspace_id, last_fetched_seq) \
             SELECT ?1, ?2, last_fetched_seq FROM sync_cursor WHERE workspace = ?3",
            rusqlite::params![origin, ws, room],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO pushed_v2 (relay_origin, workspace_id, event_id) SELECT ?1, ?2, event_id FROM pushed WHERE workspace = ?3",
            rusqlite::params![origin, ws, room],
        )?;
        #[cfg(test)]
        if super::test_hooks::fails("m1") {
            return Err(rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL), None).into());
        }
        tx.execute(
            "INSERT OR IGNORE INTO parked_bodies_v2 (relay_origin, workspace_id, seq, body, priority) \
             SELECT ?1, ?2, seq, body, priority FROM parked_bodies WHERE workspace = ?3",
            rusqlite::params![origin, ws, room],
        )?;
        tx.execute("INSERT OR IGNORE INTO sync_v2_rooms (room) VALUES (?1)", [&room])?;
        copied.push(room);
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok((copied, reset))
}

impl EventStore {
    /// The schema version of a database a newer Ligus wrote, when this one
    /// was opened read-only because of it ([`NEWER_DATA_MESSAGE`]).
    pub fn newer_schema(&self) -> Option<i64> {
        self.newer
    }

    /// The database's schema version (`PRAGMA user_version`).
    pub fn schema_version(&self) -> Result<i64> {
        Ok(user_version(&self.conn)?)
    }

    /// M1 (identity model v2, section 11): key the durable sync state by
    /// (relay origin, workspace id). `connections` are this data dir's
    /// connections as (relay URL, room). Runs once (then the version is
    /// [`SCHEMA_VERSION`]); only while this process holds the store alone
    /// (the lock file exclusive, so this connection must be the only one
    /// open in this process too): otherwise [`SyncMigration::Busy`] and it
    /// runs on a later start. One backup first; the copy and the version are
    /// one transaction, and a failure removes the backup and leaves the
    /// database as it was. The old tables are never changed.
    pub fn migrate_sync_tables(&self, connections: &[(String, String)]) -> Result<SyncMigration> {
        if self.newer.is_some() {
            return Ok(SyncMigration::ReadOnly);
        }
        if user_version(&self.conn)? >= SCHEMA_VERSION {
            return Ok(SyncMigration::AlreadyDone);
        }
        // Exclusive: this connection's own shared hold becomes exclusive, or
        // M1 waits for another start.
        let db = db_path(&self.conn);
        let mut fresh = None;
        let lock = match (&self.lock, &db) {
            (Some(l), _) => Some(l),
            (None, Some(db)) => {
                fresh = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(lock_path(db)).ok();
                fresh.as_ref()
            }
            (None, None) => None,
        };
        if let Some(l) = lock {
            let _ = l.unlock();
            if l.try_lock().is_err() {
                let _ = l.lock_shared();
                return Ok(SyncMigration::Busy);
            }
        } else if db.is_some() {
            return Ok(SyncMigration::Busy);
        }
        let result = self.migrate_sync_tables_locked(connections);
        if let Some(l) = lock {
            let _ = l.unlock();
            if fresh.is_none() {
                let _ = l.lock_shared();
            }
        }
        result
    }

    fn migrate_sync_tables_locked(&self, connections: &[(String, String)]) -> Result<SyncMigration> {
        let _upgrade = super::upgrade_lock().lock().unwrap_or_else(|p| p.into_inner());
        if user_version(&self.conn)? >= SCHEMA_VERSION {
            return Ok(SyncMigration::AlreadyDone);
        }
        let backup = if legacy_rooms(&self.conn)?.is_empty() { None } else { sync_backup(&self.conn)? };
        let run = || -> Result<(Vec<String>, Vec<String>)> {
            let tx = write_tx(&self.conn)?;
            let out = copy_sync_state(&tx, connections)?;
            tx.commit()?;
            Ok(out)
        };
        match run() {
            Ok((copied, reset)) => {
                tracing::info!(target: "event_store", copied = copied.len(), reset = reset.len(), "sync state is now kept per relay and workspace");
                Ok(SyncMigration::Migrated { copied, reset, backup })
            }
            Err(e) => {
                if let Some(path) = &backup {
                    remove_backup(path);
                }
                Err(e)
            }
        }
    }

    // --- Durable sync state, keyed by (relay origin, workspace) ---

    /// The durable fetch cursor on one relay for one workspace: the highest
    /// relay sequence applied so far (0 if it never synced there).
    pub fn sync_cursor(&self, key: &SyncKey) -> Result<u64> {
        let seq: Option<i64> = self
            .conn
            .query_row(
                "SELECT last_fetched_seq FROM sync_cursor_v2 WHERE relay_origin = ?1 AND workspace_id = ?2",
                rusqlite::params![key.relay_origin, key.ws()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(seq.unwrap_or(0).max(0) as u64)
    }

    /// Persist the fetch cursor (upsert).
    pub fn set_sync_cursor(&self, key: &SyncKey, last_fetched_seq: u64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_cursor_v2 (relay_origin, workspace_id, last_fetched_seq) VALUES (?1, ?2, ?3) \
             ON CONFLICT(relay_origin, workspace_id) DO UPDATE SET last_fetched_seq = excluded.last_fetched_seq",
            rusqlite::params![key.relay_origin, key.ws(), last_fetched_seq as i64],
        )?;
        Ok(())
    }

    /// Mark an event as already on this relay (idempotent), so a later push
    /// scan, even after a restart, never sends it there again.
    pub fn mark_pushed(&self, key: &SyncKey, event_id: Uuid) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO pushed_v2 (relay_origin, workspace_id, event_id) VALUES (?1, ?2, ?3)",
            rusqlite::params![key.relay_origin, key.ws(), event_id.to_string()],
        )?;
        Ok(())
    }

    /// Every event id already on this relay for this workspace: seeds the
    /// sync engine's in-memory dedup cache.
    pub fn pushed_ids(&self, key: &SyncKey) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare("SELECT event_id FROM pushed_v2 WHERE relay_origin = ?1 AND workspace_id = ?2 ORDER BY event_id")?;
        let rows = stmt.query_map(rusqlite::params![key.relay_origin, key.ws()], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for s in rows {
            if let Ok(id) = Uuid::parse_str(&s?) {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Park a relay body this device can't open yet (A-13), with priority 0.
    pub fn park_body(&self, key: &SyncKey, seq: u64, body: &serde_json::Value) -> Result<()> {
        self.park_bodies(key, &[(seq, body, 0)])
    }

    /// Park relay bodies this device can't open yet (A-13), each with how
    /// much it is worth keeping (`priority`, higher first; see
    /// `KeyringEngine::park_priority`). Keeps at most
    /// [`super::PARKED_KEEP`] bodies and [`super::PARKED_BYTES_KEEP`] bytes
    /// per relay and workspace: past that the lowest priority goes first,
    /// oldest first within a priority (B-11: a flood of junk can't evict a
    /// body waiting for an authorized rotation).
    pub fn park_bodies(&self, key: &SyncKey, bodies: &[(u64, &serde_json::Value, u8)]) -> Result<()> {
        if bodies.is_empty() {
            return Ok(());
        }
        let ws = key.ws();
        let tx = write_tx(&self.conn)?;
        for (seq, body, priority) in bodies {
            tx.execute(
                "INSERT OR REPLACE INTO parked_bodies_v2 (relay_origin, workspace_id, seq, body, priority) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![key.relay_origin, ws, *seq as i64, serde_json::to_string(body)?, *priority as i64],
            )?;
        }
        tx.execute(
            "DELETE FROM parked_bodies_v2 WHERE relay_origin = ?1 AND workspace_id = ?2 AND seq NOT IN \
             (SELECT seq FROM parked_bodies_v2 WHERE relay_origin = ?1 AND workspace_id = ?2 ORDER BY priority DESC, seq DESC LIMIT ?3)",
            rusqlite::params![key.relay_origin, ws, super::PARKED_KEEP],
        )?;
        tx.execute(
            "DELETE FROM parked_bodies_v2 WHERE relay_origin = ?1 AND workspace_id = ?2 AND seq IN \
             (SELECT seq FROM (SELECT seq, SUM(length(CAST(body AS BLOB))) OVER (ORDER BY priority DESC, seq DESC ROWS UNBOUNDED PRECEDING) AS kept \
              FROM parked_bodies_v2 WHERE relay_origin = ?1 AND workspace_id = ?2) WHERE kept > ?3)",
            rusqlite::params![key.relay_origin, ws, super::PARKED_BYTES_KEEP],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Re-rank parked bodies (the keyring changed: a rotation they wait for
    /// may now be authorized, or one they claim may have turned out junk).
    pub fn set_parked_priorities(&self, key: &SyncKey, priorities: &[(u64, u8)]) -> Result<()> {
        let ws = key.ws();
        let tx = write_tx(&self.conn)?;
        for (seq, priority) in priorities {
            tx.execute(
                "UPDATE parked_bodies_v2 SET priority = ?4 WHERE relay_origin = ?1 AND workspace_id = ?2 AND seq = ?3",
                rusqlite::params![key.relay_origin, ws, *seq as i64, *priority as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Every parked body on this relay for this workspace, by sequence.
    pub fn parked_bodies(&self, key: &SyncKey) -> Result<Vec<(u64, serde_json::Value)>> {
        self.parked_rows(
            "SELECT seq, body FROM parked_bodies_v2 WHERE relay_origin = ?1 AND workspace_id = ?2 ORDER BY seq ASC",
            rusqlite::params![key.relay_origin, key.ws()],
        )
    }

    /// Forget a parked body (it opened, or proved bad).
    pub fn unpark(&self, key: &SyncKey, seq: u64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM parked_bodies_v2 WHERE relay_origin = ?1 AND workspace_id = ?2 AND seq = ?3",
            rusqlite::params![key.relay_origin, key.ws(), seq as i64],
        )?;
        Ok(())
    }

    /// The bodies an older build parked for `room` that M1 didn't copy (no
    /// single relay was named for the room): drained from the old table.
    /// Their sequences may be another relay's, so they never move a cursor.
    pub fn legacy_parked_bodies(&self, room: &str) -> Result<Vec<(u64, serde_json::Value)>> {
        self.parked_rows(
            "SELECT seq, body FROM parked_bodies WHERE workspace = ?1 AND NOT EXISTS (SELECT 1 FROM sync_v2_rooms WHERE room = ?1) ORDER BY seq ASC",
            [room],
        )
    }

    /// Forget an old-table parked body ([`EventStore::legacy_parked_bodies`]).
    pub fn legacy_unpark(&self, room: &str, seq: u64) -> Result<()> {
        self.conn.execute("DELETE FROM parked_bodies WHERE workspace = ?1 AND seq = ?2", rusqlite::params![room, seq as i64])?;
        Ok(())
    }

    fn parked_rows(&self, sql: &str, params: impl rusqlite::Params) -> Result<Vec<(u64, serde_json::Value)>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params, |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, body) = row?;
            if let Ok(v) = serde_json::from_str(&body) {
                out.push((seq as u64, v));
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// M2: redeem key seeds to the secret store
// ---------------------------------------------------------------------------

/// The secret id of the redeem key seed this device proves its admission
/// (or person invite) to `ws` with.
pub fn redeem_secret(ws: Uuid) -> SecretId {
    SecretId::new("redeem", ws.to_string())
}

/// The secret id of the redeem key seed of person invite `invite` this
/// device issued in `ws`.
pub fn invite_secret(ws: Uuid, invite: Uuid) -> SecretId {
    SecretId::new("invite-key", format!("{ws}:{invite}"))
}

fn seed_hex(seed: &[u8; 32]) -> String {
    seed.iter().map(|b| format!("{b:02x}")).collect()
}

fn seed32(bytes: &[u8]) -> Option<[u8; 32]> {
    <[u8; 32]>::try_from(bytes).ok()
}

/// A seed row of either table: (secret id, workspace, invite, seed).
type SeedRow = (SecretId, Uuid, Option<Uuid>, [u8; 32]);

fn table_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")?.exists([name])
}

/// Every seed still in the old tables.
fn seed_rows(conn: &Connection) -> rusqlite::Result<Vec<SeedRow>> {
    let mut out = Vec::new();
    if table_exists(conn, "join_redeem_keys")? {
        let mut stmt = conn.prepare("SELECT workspace_id, seed FROM join_redeem_keys")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (ws, hex) = row?;
            if let (Ok(ws), Some(seed)) = (Uuid::parse_str(&ws), crate::box_admission::seed_from_hex(&hex)) {
                out.push((redeem_secret(ws), ws, None, seed));
            }
        }
    }
    if table_exists(conn, "issued_invite_keys")? {
        let mut stmt = conn.prepare("SELECT workspace_id, invite_id, seed FROM issued_invite_keys")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
        for row in rows {
            let (ws, inv, hex) = row?;
            if let (Ok(ws), Ok(inv), Some(seed)) = (Uuid::parse_str(&ws), Uuid::parse_str(&inv), crate::box_admission::seed_from_hex(&hex)) {
                out.push((invite_secret(ws, inv), ws, Some(inv), seed));
            }
        }
    }
    Ok(out)
}

/// Put `seed` in the store under `id` and read it back. With `fresh`, a
/// seed this device didn't hold before: its keep-time starts now.
fn put_and_check(conn: &Connection, store: &dyn SecretStore, id: &SecretId, ws: Uuid, invite: Option<Uuid>, seed: &[u8; 32], now: i64) -> bool {
    let stored = store.get(id).ok().flatten().and_then(|v| seed32(&v));
    let fresh = stored != Some(*seed);
    if fresh {
        if let Err(e) = store.put(id, seed) {
            tracing::warn!(target: "event_store", secret = %id.account(), "couldn't move a redeem key seed to the secret store (it stays in the database): {e}");
            return false;
        }
        if store.get(id).ok().flatten().and_then(|v| seed32(&v)) != Some(*seed) {
            tracing::warn!(target: "event_store", secret = %id.account(), "the secret store didn't give a redeem key seed back (it stays in the database)");
            return false;
        }
    }
    let sql = if fresh || stored.is_none() {
        "INSERT OR REPLACE INTO seed_copies (secret, workspace_id, invite_id, first_ms) VALUES (?1, ?2, ?3, ?4)"
    } else {
        "INSERT OR IGNORE INTO seed_copies (secret, workspace_id, invite_id, first_ms) VALUES (?1, ?2, ?3, ?4)"
    };
    let _ = conn.execute(sql, rusqlite::params![id.account(), ws.to_string(), invite.map(|i| i.to_string()), now]);
    true
}

/// M2, the copy (identity model v2, 10.3; on every start, before any backup
/// is taken): every seed in the old tables goes to the secret store and is
/// read back. A store copy that differs from its row (a 0.3 process wrote
/// the row since) is overwritten. Nothing is deleted here. Returns how many
/// seeds are in the store now.
pub(super) fn copy_seeds(conn: &Connection, store: &dyn SecretStore, now: i64) -> Result<usize> {
    let rows = seed_rows(conn)?;
    if rows.is_empty() {
        return Ok(0);
    }
    init_seed_copies(conn)?;
    Ok(rows.iter().filter(|(id, ws, inv, seed)| put_and_check(conn, store, id, *ws, *inv, seed, now)).count())
}

/// The data dirs whose seeds this process moved already (M2 runs once per
/// start, on the first open).
fn seeds_moved() -> &'static Mutex<std::collections::HashSet<PathBuf>> {
    static S: std::sync::OnceLock<Mutex<std::collections::HashSet<PathBuf>>> = std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

/// Whether this is this process's first open of `db` (M2's turn).
pub(super) fn first_open(db: &Path) -> bool {
    seeds_moved().lock().unwrap_or_else(|p| p.into_inner()).insert(db.to_path_buf())
}

/// Run `f` with `PRAGMA secure_delete` on: content it deletes or overwrites
/// is zeroed in the file, not left in free space.
fn securely<T>(conn: &Connection, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let was: i64 = conn.pragma_query_value(None, "secure_delete", |r| r.get(0)).unwrap_or(0);
    conn.pragma_update(None, "secure_delete", 1)?;
    let out = f();
    let _ = conn.pragma_update(None, "secure_delete", was);
    out
}

/// Fold the write-ahead log into the file and truncate it, so a deleted
/// seed doesn't stay in an old frame. Best effort (a reader elsewhere can
/// hold the log; the next checkpoint finishes it).
fn checkpoint(conn: &Connection) {
    if conn.is_autocommit() {
        let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    }
}

impl EventStore {
    /// The secret store this store moves seeds to: the one the host chose
    /// for the data dir (`secret_store::init_for_data_dir`), or none (the
    /// seeds stay in their tables only). Tests and hosts may set one.
    pub fn set_secret_store(&mut self, store: Option<Arc<dyn SecretStore>>) {
        self.secrets = store;
    }

    /// M2, now (copy, then retire): see [`EventStore::move_seeds_at`].
    pub fn move_seeds(&self) -> Result<(usize, usize)> {
        self.move_seeds_at(now_ms())
    }

    /// M2 at time `now` (unix ms): copy every seed to the secret store and
    /// read it back, then delete the rows (and store copies) whose invite or
    /// admission is closed or expired on the log, with secure delete and a
    /// truncating checkpoint. A person invite nothing closes goes once it was
    /// first copied [`PERSON_SEED_KEEP_MS`] ago. A row whose copy failed is
    /// never deleted. Returns (seeds in the store, rows deleted).
    pub fn move_seeds_at(&self, now: i64) -> Result<(usize, usize)> {
        let Some(store) = self.secrets.clone() else { return Ok((0, 0)) };
        if self.newer.is_some() {
            return Ok((0, 0));
        }
        let copied = copy_seeds(&self.conn, store.as_ref(), now)?;
        let deleted = self.retire_seeds(store.as_ref(), now)?;
        Ok((copied, deleted))
    }

    /// Whether the invite or admission a seed proves is done with: used,
    /// closed or expired on `ws`'s config log, or (a person invite, or one
    /// this device can't see on the log) first copied long enough ago.
    fn seed_closed(&self, ws: Uuid, invite: Option<Uuid>, first_ms: Option<i64>, now: i64) -> Result<bool> {
        if let Some(inv) = invite {
            let founder = self.workspace_pin(ws)?.map(|p| p.founder_account.to_string());
            if let (Some(config), _) = self.load_config(ws, founder.as_deref(), None)? {
                let g = &config.guards;
                if g.used_invites.contains(&inv) || g.closed_admissions.contains_key(&inv) {
                    return Ok(true);
                }
                if let Some(a) = g.service_invites.get(&inv) {
                    return Ok(a.expires_ms <= now);
                }
            }
        }
        Ok(first_ms.is_some_and(|f| now.saturating_sub(f) >= PERSON_SEED_KEEP_MS))
    }

    fn first_copied(&self, id: &SecretId) -> Result<Option<i64>> {
        if !table_exists(&self.conn, "seed_copies")? {
            return Ok(None);
        }
        Ok(self.conn.query_row("SELECT first_ms FROM seed_copies WHERE secret = ?1", [id.account()], |r| r.get(0)).optional()?)
    }

    /// M2, the deletions: rows whose copy reads back and whose invite is
    /// closed, then store copies left behind by a row an older build
    /// deleted.
    fn retire_seeds(&self, store: &dyn SecretStore, now: i64) -> Result<usize> {
        let mut doomed: Vec<SeedRow> = Vec::new();
        for row in seed_rows(&self.conn)? {
            let (id, ws, inv, seed) = &row;
            if store.get(id).ok().flatten().and_then(|v| seed32(&v)) != Some(*seed) {
                continue;
            }
            let invite = match inv {
                Some(i) => Some(*i),
                None => self.join_invite(*ws)?,
            };
            if self.seed_closed(*ws, invite, self.first_copied(id)?, now)? {
                doomed.push(row);
            }
        }
        // Copies whose row is gone (a 0.3 process used it): forgotten once
        // their invite is closed too.
        let mut orphans: Vec<(String, Uuid, Option<Uuid>, i64)> = Vec::new();
        if table_exists(&self.conn, "seed_copies")? {
            let live: BTreeSet<String> = seed_rows(&self.conn)?.iter().map(|(id, ..)| id.account()).collect();
            let mut stmt = self.conn.prepare("SELECT secret, workspace_id, invite_id, first_ms FROM seed_copies")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, i64>(3)?)))?;
            for row in rows {
                let (secret, ws, inv, first) = row?;
                if live.contains(&secret) {
                    continue;
                }
                let Ok(ws) = Uuid::parse_str(&ws) else { continue };
                orphans.push((secret, ws, inv.and_then(|i| Uuid::parse_str(&i).ok()), first));
            }
        }
        let mut gone_copies: Vec<SecretId> = Vec::new();
        for (secret, ws, inv, first) in orphans {
            let id = match (secret.split_once(':'), inv) {
                (Some(("invite-key", _)), Some(i)) => invite_secret(ws, i),
                (Some(("redeem", _)), _) => redeem_secret(ws),
                _ => continue,
            };
            let invite = inv.or(self.join_invite(ws)?);
            if self.seed_closed(ws, invite, Some(first), now)? {
                gone_copies.push(id);
            }
        }
        if doomed.is_empty() && gone_copies.is_empty() {
            return Ok(0);
        }
        securely(&self.conn, || {
            let tx = write_tx(&self.conn)?;
            for (_, ws, inv, seed) in &doomed {
                // Only the row as it was checked: one a 0.3 process rewrote
                // since is a new seed, kept for the next start.
                match inv {
                    None => tx.execute(
                        "DELETE FROM join_redeem_keys WHERE workspace_id = ?1 AND seed = ?2",
                        rusqlite::params![ws.to_string(), seed_hex(seed)],
                    )?,
                    Some(i) => tx.execute(
                        "DELETE FROM issued_invite_keys WHERE workspace_id = ?1 AND invite_id = ?2 AND seed = ?3",
                        rusqlite::params![ws.to_string(), i.to_string(), seed_hex(seed)],
                    )?,
                };
            }
            tx.commit()?;
            Ok(())
        })?;
        checkpoint(&self.conn);
        // The store copies go after the rows; one the store won't delete
        // stays listed and is tried again on the next start.
        for id in doomed.iter().map(|(id, ..)| id).chain(&gone_copies) {
            match store.delete(id) {
                Ok(()) => {
                    self.conn.execute("DELETE FROM seed_copies WHERE secret = ?1", [id.account()])?;
                }
                Err(e) => tracing::warn!(target: "event_store", secret = %id.account(), "couldn't delete a used redeem key seed from the secret store: {e}"),
            }
        }
        tracing::info!(target: "event_store", deleted = doomed.len(), "deleted the redeem key seeds of closed invites");
        Ok(doomed.len())
    }

    /// Put a seed this device now holds in the store too (a fresh seed: its
    /// keep-time starts now).
    fn store_seed(&self, id: &SecretId, ws: Uuid, invite: Option<Uuid>, seed: &[u8; 32]) {
        let Some(store) = &self.secrets else { return };
        if init_seed_copies(&self.conn).is_ok() {
            put_and_check(&self.conn, store.as_ref(), id, ws, invite, seed, now_ms());
        }
    }

    /// A seed: the store's copy, unless its table row says otherwise (a 0.3
    /// process on this data dir writes only the table, so a row that
    /// differs is newer and overwrites the copy); else the row.
    fn read_seed(&self, id: &SecretId, ws: Uuid, invite: Option<Uuid>, row: Option<[u8; 32]>) -> Option<[u8; 32]> {
        let Some(store) = &self.secrets else { return row };
        let stored = store.get(id).ok().flatten().and_then(|v| seed32(&v));
        match (stored, row) {
            (Some(s), None) => Some(s),
            (Some(s), Some(r)) if s == r => Some(s),
            (_, Some(r)) => {
                self.store_seed(id, ws, invite, &r);
                Some(r)
            }
            (None, None) => None,
        }
    }

    /// Forget a seed in both places: the row with secure delete, then the
    /// store copy.
    fn forget_seed(&self, id: &SecretId, delete_row: &str, params: &[&dyn rusqlite::ToSql]) -> Result<()> {
        securely(&self.conn, || {
            self.conn.execute(delete_row, params)?;
            Ok(())
        })?;
        checkpoint(&self.conn);
        if let Some(store) = &self.secrets {
            match store.delete(id) {
                // (One the store won't delete stays listed: tried again on
                // the next start.)
                Ok(()) if table_exists(&self.conn, "seed_copies")? => {
                    self.conn.execute("DELETE FROM seed_copies WHERE secret = ?1", [id.account()])?;
                }
                Ok(()) => {}
                Err(e) => tracing::warn!(target: "event_store", secret = %id.account(), "couldn't delete a redeem key seed from the secret store: {e}"),
            }
        }
        Ok(())
    }

    /// Keep the redeem key seed of the box admission (or person invite) this
    /// device redeems to join `workspace_id`: in the secret store, and in its
    /// table for an older build on this data dir.
    pub fn set_join_redeem_key(&self, workspace_id: Uuid, seed: &[u8; 32]) -> Result<()> {
        securely(&self.conn, || {
            self.conn.execute(
                "INSERT INTO join_redeem_keys (workspace_id, seed) VALUES (?1, ?2) \
                 ON CONFLICT(workspace_id) DO UPDATE SET seed = excluded.seed",
                rusqlite::params![workspace_id.to_string(), seed_hex(seed)],
            )?;
            Ok(())
        })?;
        self.store_seed(&redeem_secret(workspace_id), workspace_id, None, seed);
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

    /// Forget the redeem key seed once the redemption it proved is written:
    /// it admits no one else, and is kept no longer than needed.
    pub fn clear_join_redeem_key(&self, workspace_id: Uuid) -> Result<()> {
        let ws = workspace_id.to_string();
        self.forget_seed(&redeem_secret(workspace_id), "DELETE FROM join_redeem_keys WHERE workspace_id = ?1", &[&ws])
    }

    /// The redeem key seed this device proves its admission with, if any.
    pub fn join_redeem_key(&self, workspace_id: Uuid) -> Result<Option<[u8; 32]>> {
        let row: Option<String> = self
            .conn
            .query_row("SELECT seed FROM join_redeem_keys WHERE workspace_id = ?1", [workspace_id.to_string()], |r| r.get(0))
            .optional()?;
        let row = row.and_then(|h| crate::box_admission::seed_from_hex(&h));
        Ok(self.read_seed(&redeem_secret(workspace_id), workspace_id, None, row))
    }

    /// Keep the redeem key seed of person invite `invite_id` this device
    /// issued, for its code: in the secret store, and in its table for an
    /// older build on this data dir.
    pub fn set_issued_invite_key(&self, workspace_id: Uuid, invite_id: Uuid, seed: &[u8; 32]) -> Result<()> {
        securely(&self.conn, || {
            self.conn.execute(
                "INSERT OR REPLACE INTO issued_invite_keys (workspace_id, invite_id, seed) VALUES (?1, ?2, ?3)",
                rusqlite::params![workspace_id.to_string(), invite_id.to_string(), seed_hex(seed)],
            )?;
            Ok(())
        })?;
        self.store_seed(&invite_secret(workspace_id, invite_id), workspace_id, Some(invite_id), seed);
        Ok(())
    }

    /// The redeem key seed of person invite `invite_id` this device issued,
    /// forgotten here: its code carries it from now on.
    pub fn take_issued_invite_key(&self, workspace_id: Uuid, invite_id: Uuid) -> Result<Option<[u8; 32]>> {
        let seed = self.issued_invite_key(workspace_id, invite_id)?;
        let (ws, inv) = (workspace_id.to_string(), invite_id.to_string());
        self.forget_seed(
            &invite_secret(workspace_id, invite_id),
            "DELETE FROM issued_invite_keys WHERE workspace_id = ?1 AND invite_id = ?2",
            &[&ws, &inv],
        )?;
        Ok(seed)
    }

    /// The redeem key seed of person invite `invite_id` this device issued.
    pub fn issued_invite_key(&self, workspace_id: Uuid, invite_id: Uuid) -> Result<Option<[u8; 32]>> {
        let row: Option<String> = self
            .conn
            .query_row(
                "SELECT seed FROM issued_invite_keys WHERE workspace_id = ?1 AND invite_id = ?2",
                rusqlite::params![workspace_id.to_string(), invite_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let row = row.and_then(|h| crate::box_admission::seed_from_hex(&h));
        Ok(self.read_seed(&invite_secret(workspace_id, invite_id), workspace_id, Some(invite_id), row))
    }
}

#[cfg(test)]
#[path = "storage_v2_tests.rs"]
mod tests;

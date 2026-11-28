//! Device-local tables of `user_version` 2 that replace older blobs and
//! files (workspace model v3, sections 9 and 13; step M9 of 16.2; identity
//! model v2, 10.2).
//!
//! - **Scratch registry.** `scratch_repos` and `scratch_repo_chats` hold
//!   what [`crate::repos::scratch`] used to keep as one JSON blob under
//!   `repo_settings` `scratch:registry`. The blob is imported once and then
//!   left exactly as it was (the backup a downgrade still reads). A blob
//!   that doesn't parse stops the import: nothing is overwritten, the rows
//!   already here stay, and `ligus doctor` warns
//!   ([`EventStore::scratch_import_problem`]).
//! - **Workspace folders.** `workspace_folders` is the one table for "the
//!   folder this device bound to a workspace before any repository is
//!   known": the desktop's `workspace_dirs` (and its older
//!   `workspace_root`), the CLI's `repo-bindings.toml` workspace binding and
//!   lt's `checkouts` are imported into it once. Readers ask the table first
//!   and the old source only as a fallback; writers write the table, and the
//!   old sources stay as they were at the import (read-only through 0.5).
//! - **Imports.** `local_imports` remembers which one-time imports ran, so
//!   each runs once and a later start skips it.
//!
//! The tables are created on every open (additive: a 0.4 build ignores
//! them). The imports run in [`EventStore::migrate_local_tables`], after
//! M8, only while this process holds the store alone, with a backup first
//! (`<db>.bak-local-v2-…`) when they change anything, in one transaction.
//! No secret goes in any of them: ids, names, folders and times only.

use std::path::PathBuf;

use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

use super::storage_v2::{backup_marked, now_ms, user_version, SCHEMA_VERSION};
use super::{remove_backup, write_tx, EventStore, Result};
use crate::repos::scratch::{ScratchRepo, ScratchState};

/// What the backup before the imports is named after: `<db>.bak-local-v2-…`.
pub(super) const LOCAL_BACKUP_MARK: &str = ".bak-local-v2-";

/// The old scratch registry blob (`repo_settings`).
pub(crate) const SCRATCH_BLOB_KEY: &str = "scratch:registry";

/// The `local_imports` name of the scratch registry import.
pub(crate) const SCRATCH_IMPORT: &str = "scratch-registry";

/// The `local_imports` name of a surface's workspace-folder import.
fn folders_import(source: &str) -> String {
    format!("workspace-folders:{source}")
}

pub(super) fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        -- Device-local, never synced: the scratch repositories this host
        -- created (crate::repos::scratch). Only a row here ever lets this
        -- host delete a folder.
        CREATE TABLE IF NOT EXISTS scratch_repos (
            id             TEXT PRIMARY KEY,
            workspace_id   TEXT NOT NULL,
            name           TEXT NOT NULL,
            dir_name       TEXT NOT NULL,
            path           TEXT NOT NULL,
            root           TEXT NOT NULL,
            root_commit    TEXT NOT NULL,
            created_at     INTEGER NOT NULL,
            last_activity  INTEGER NOT NULL,
            created_by     TEXT NOT NULL DEFAULT '',
            repo_id        TEXT,
            state          TEXT NOT NULL,
            noticed_at     INTEGER,
            removed_at     INTEGER,
            published      TEXT,
            retire_pending INTEGER NOT NULL DEFAULT 0,
            -- The list's order (oldest first), as the blob kept it.
            position       INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_scratch_repos_ws ON scratch_repos (workspace_id);
        -- The chats that work in each, in the order they were added.
        CREATE TABLE IF NOT EXISTS scratch_repo_chats (
            scratch_id TEXT NOT NULL,
            chat_id    TEXT NOT NULL,
            position   INTEGER NOT NULL,
            PRIMARY KEY (scratch_id, chat_id)
        );
        -- Device-local: the folder bound to a workspace on this device (v2
        -- 10.2). `source` says where it came from: a person on this device
        -- (`bound`) or an import (`desktop`, `cli`, `lt`).
        CREATE TABLE IF NOT EXISTS workspace_folders (
            workspace_id TEXT PRIMARY KEY,
            path         TEXT NOT NULL,
            source       TEXT NOT NULL,
            updated_at   INTEGER NOT NULL
        );
        -- Device-local: the one-time imports that ran (name, when, a note).
        CREATE TABLE IF NOT EXISTS local_imports (
            name    TEXT PRIMARY KEY,
            done_ms INTEGER NOT NULL,
            detail  TEXT NOT NULL DEFAULT ''
        );
        "#,
    )
}

fn state_str(s: ScratchState) -> &'static str {
    match s {
        ScratchState::Live => "live",
        ScratchState::Kept => "kept",
        ScratchState::Removed => "removed",
    }
}

fn state_parse(s: &str) -> ScratchState {
    match s {
        "kept" => ScratchState::Kept,
        "removed" => ScratchState::Removed,
        _ => ScratchState::Live,
    }
}

fn insert_scratch(tx: &Connection, e: &ScratchRepo, position: i64, replace: bool) -> rusqlite::Result<usize> {
    let verb = if replace { "INSERT OR REPLACE" } else { "INSERT OR IGNORE" };
    let n = tx.execute(
        &format!(
            "{verb} INTO scratch_repos (id, workspace_id, name, dir_name, path, root, root_commit, created_at, last_activity, \
             created_by, repo_id, state, noticed_at, removed_at, published, retire_pending, position) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)"
        ),
        rusqlite::params![
            e.id.to_string(),
            e.workspace_id.to_string(),
            e.name,
            e.dir_name,
            e.path.to_string_lossy(),
            e.root.to_string_lossy(),
            e.root_commit,
            e.created_at,
            e.last_activity,
            e.created_by,
            e.repo_id.map(|r| r.to_string()),
            state_str(e.state),
            e.noticed_at,
            e.removed_at,
            e.published,
            i64::from(e.retire_pending),
            position,
        ],
    )?;
    if n > 0 {
        tx.execute("DELETE FROM scratch_repo_chats WHERE scratch_id = ?1", [e.id.to_string()])?;
        for (i, c) in e.chats.iter().enumerate() {
            tx.execute(
                "INSERT OR IGNORE INTO scratch_repo_chats (scratch_id, chat_id, position) VALUES (?1, ?2, ?3)",
                rusqlite::params![e.id.to_string(), c.to_string(), i as i64],
            )?;
        }
    }
    Ok(n)
}

fn mark(tx: &Connection, name: &str, detail: &str) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO local_imports (name, done_ms, detail) VALUES (?1, ?2, ?3)",
        rusqlite::params![name, now_ms(), detail],
    )?;
    Ok(())
}

/// The old scratch blob as the import sees it.
pub(crate) enum ScratchBlob {
    /// None was ever written.
    Absent,
    Parsed(Vec<ScratchRepo>),
    /// It doesn't parse (the reason).
    Unreadable(String),
}

/// One folder an older source bound to a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyFolder {
    pub workspace_id: Uuid,
    pub path: String,
}

/// What [`EventStore::migrate_local_tables`] imports besides the scratch
/// blob (which is in the store itself): the folders a surface's older
/// source bound, and that surface's name (`desktop`, `cli`, `lt`).
#[derive(Debug, Clone, Default)]
pub struct LegacyLocal {
    pub source: &'static str,
    pub folders: Vec<LegacyFolder>,
}

/// What [`EventStore::migrate_local_tables`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalTablesMigration {
    /// Every import already ran.
    AlreadyDone,
    /// Imported (the backup first when anything changed). `scratch_blocked`:
    /// the old scratch registry doesn't parse, so its import stopped and
    /// waits (`ligus doctor` says so); the rest ran.
    Migrated { scratch: usize, folders: usize, backup: Option<PathBuf>, scratch_blocked: bool },
    /// M8 hasn't run yet (it runs first, on this or a later start).
    NotYet,
    /// Another process has the store open: the imports wait for a start
    /// that has it alone (readers fall back to the old sources meanwhile).
    Busy,
    /// The database was written by a newer Ligus.
    ReadOnly,
}

impl EventStore {
    // ── scratch rows ──────────────────────────────────────────────────────

    /// Every scratch row here, oldest first.
    pub(crate) fn scratch_rows(&self) -> Result<Vec<ScratchRepo>> {
        let mut chats: std::collections::HashMap<String, Vec<Uuid>> = std::collections::HashMap::new();
        {
            let mut st = self.conn.prepare("SELECT scratch_id, chat_id FROM scratch_repo_chats ORDER BY scratch_id, position")?;
            let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows {
                let (s, c) = row?;
                if let Ok(c) = Uuid::parse_str(&c) {
                    chats.entry(s).or_default().push(c);
                }
            }
        }
        let mut st = self.conn.prepare(
            "SELECT id, workspace_id, name, dir_name, path, root, root_commit, created_at, last_activity, created_by, repo_id, \
             state, noticed_at, removed_at, published, retire_pending FROM scratch_repos ORDER BY position, created_at, id",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, String>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, String>(11)?,
                (r.get::<_, Option<i64>>(12)?, r.get::<_, Option<i64>>(13)?, r.get::<_, Option<String>>(14)?, r.get::<_, i64>(15)?),
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, ws, name, dir_name, path, root, root_commit, created_at, last_activity, created_by, repo_id, state, rest) = row?;
            let (noticed_at, removed_at, published, retire) = rest;
            let (Ok(uid), Ok(ws)) = (Uuid::parse_str(&id), Uuid::parse_str(&ws)) else { continue };
            out.push(ScratchRepo {
                id: uid,
                workspace_id: ws,
                name,
                dir_name,
                path: PathBuf::from(path),
                root: PathBuf::from(root),
                root_commit,
                created_at,
                last_activity,
                created_by,
                repo_id: repo_id.and_then(|r| Uuid::parse_str(&r).ok()),
                chats: chats.remove(&id).unwrap_or_default(),
                state: state_parse(&state),
                noticed_at,
                removed_at,
                published,
                retire_pending: retire != 0,
            });
        }
        Ok(out)
    }

    /// Replace every scratch row with `all` (in this order), in one
    /// transaction. `imported`: also note the old blob as imported (the
    /// caller merged it into `all`).
    pub(crate) fn replace_scratch_rows(&self, all: &[ScratchRepo], imported: bool) -> Result<()> {
        let tx = write_tx(&self.conn)?;
        tx.execute("DELETE FROM scratch_repo_chats", [])?;
        tx.execute("DELETE FROM scratch_repos", [])?;
        for (i, e) in all.iter().enumerate() {
            insert_scratch(&tx, e, i as i64, true)?;
        }
        if imported {
            mark(&tx, SCRATCH_IMPORT, "merged on write")?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Whether the one-time import `name` ran.
    pub(crate) fn local_import_done(&self, name: &str) -> Result<bool> {
        Ok(self.conn.query_row("SELECT 1 FROM local_imports WHERE name = ?1", [name], |_| Ok(())).optional()?.is_some())
    }

    /// The old scratch blob, as the import reads it.
    pub(crate) fn scratch_blob(&self) -> Result<ScratchBlob> {
        let text: Option<String> =
            self.conn.query_row("SELECT value FROM repo_settings WHERE key = ?1", [SCRATCH_BLOB_KEY], |r| r.get(0)).optional()?;
        Ok(match text {
            None => ScratchBlob::Absent,
            Some(t) => match serde_json::from_str::<Vec<ScratchRepo>>(&t) {
                Ok(v) => ScratchBlob::Parsed(v),
                Err(e) => ScratchBlob::Unreadable(e.to_string()),
            },
        })
    }

    /// What `ligus doctor` says about the old scratch registry: `Some` while
    /// its blob doesn't parse and so was never imported (it is kept as it
    /// is), or when an older build set unreadable copies aside.
    pub fn scratch_import_problem(&self) -> Option<String> {
        let mut lines = Vec::new();
        if !self.local_import_done(SCRATCH_IMPORT).unwrap_or(true) {
            if let Ok(ScratchBlob::Unreadable(why)) = self.scratch_blob() {
                lines.push(format!(
                    "the old scratch repository list in ligus.db ({SCRATCH_BLOB_KEY}) couldn't be read ({why}); it is kept as it was and wasn't imported"
                ));
            }
        }
        let aside: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM repo_settings WHERE key LIKE ?1", [format!("{SCRATCH_BLOB_KEY}.corrupt-%")], |r| r.get(0))
            .unwrap_or(0);
        if aside > 0 {
            lines.push(format!("{aside} unreadable copy(ies) of the old scratch repository list kept aside in ligus.db ({SCRATCH_BLOB_KEY}.corrupt-*)"));
        }
        (!lines.is_empty()).then(|| lines.join("; "))
    }

    // ── workspace folders ───────────────────────────────────────────────

    /// The folder bound to `workspace_id` on this device, from the table.
    pub fn workspace_folder(&self, workspace_id: Uuid) -> Result<Option<PathBuf>> {
        Ok(self
            .conn
            .query_row("SELECT path FROM workspace_folders WHERE workspace_id = ?1", [workspace_id.to_string()], |r| r.get::<_, String>(0))
            .optional()?
            .map(PathBuf::from))
    }

    /// Every bound folder: (workspace, folder).
    pub fn workspace_folders(&self) -> Result<Vec<(Uuid, PathBuf)>> {
        let mut st = self.conn.prepare("SELECT workspace_id, path FROM workspace_folders ORDER BY workspace_id")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (ws, p) = row?;
            if let Ok(ws) = Uuid::parse_str(&ws) {
                out.push((ws, PathBuf::from(p)));
            }
        }
        Ok(out)
    }

    /// Bind (or with `None` unbind) `workspace_id`'s folder on this device.
    pub fn set_workspace_folder(&self, workspace_id: Uuid, path: Option<&str>) -> Result<()> {
        match path.map(str::trim).filter(|p| !p.is_empty()) {
            Some(p) => self.conn.execute(
                "INSERT INTO workspace_folders (workspace_id, path, source, updated_at) VALUES (?1, ?2, 'bound', ?3) \
                 ON CONFLICT (workspace_id) DO UPDATE SET path = excluded.path, source = excluded.source, updated_at = excluded.updated_at",
                rusqlite::params![workspace_id.to_string(), p, now_ms()],
            )?,
            None => self.conn.execute("DELETE FROM workspace_folders WHERE workspace_id = ?1", [workspace_id.to_string()])?,
        };
        Ok(())
    }

    /// Whether `source`'s workspace-folder import ran (until it does, a
    /// reader falls back to that source's own file).
    pub fn workspace_folders_imported(&self, source: &str) -> bool {
        self.local_import_done(&folders_import(source)).unwrap_or(false)
    }

    // ── the one-time imports (M9) ───────────────────────────────────────

    /// M9 (workspace model v3, 16.2): import the old scratch registry blob
    /// into `scratch_repos` and the folders `legacy` names into
    /// `workspace_folders`, once each. After M8 (version 2); only while this
    /// process holds the store alone; a backup first when anything is
    /// imported; one transaction, and a failure removes the backup and
    /// leaves the tables as they were. Rows already in a table win over an
    /// imported one. The old sources are never changed. Idempotent.
    pub fn migrate_local_tables(&self, legacy: &LegacyLocal) -> Result<LocalTablesMigration> {
        if self.newer.is_some() {
            return Ok(LocalTablesMigration::ReadOnly);
        }
        if user_version(&self.conn)? < SCHEMA_VERSION {
            return Ok(LocalTablesMigration::NotYet);
        }
        if self.local_imports_pending(legacy)?.is_none() {
            return Ok(LocalTablesMigration::AlreadyDone);
        }
        self.exclusively(|| self.migrate_local_tables_locked(legacy)).unwrap_or(Ok(LocalTablesMigration::Busy))
    }

    /// `None` when nothing is left to import; else (scratch pending, folders
    /// pending).
    fn local_imports_pending(&self, legacy: &LegacyLocal) -> Result<Option<(bool, bool)>> {
        let scratch = !self.local_import_done(SCRATCH_IMPORT)?;
        let folders = !legacy.source.is_empty() && !self.local_import_done(&folders_import(legacy.source))?;
        Ok((scratch || folders).then_some((scratch, folders)))
    }

    fn migrate_local_tables_locked(&self, legacy: &LegacyLocal) -> Result<LocalTablesMigration> {
        let _upgrade = super::upgrade_lock().lock().unwrap_or_else(|p| p.into_inner());
        let Some((scratch_pending, folders_pending)) = self.local_imports_pending(legacy)? else {
            return Ok(LocalTablesMigration::AlreadyDone);
        };
        let blob = if scratch_pending { self.scratch_blob()? } else { ScratchBlob::Absent };
        let scratch_blocked = matches!(blob, ScratchBlob::Unreadable(_));
        if let ScratchBlob::Unreadable(why) = &blob {
            tracing::warn!(target: "event_store", "the old scratch repository list doesn't parse ({why}); it is kept as it was and not imported (ligus doctor)");
        }
        let folders: Vec<&LegacyFolder> = if folders_pending {
            legacy.folders.iter().filter(|f| !f.path.trim().is_empty()).collect()
        } else {
            Vec::new()
        };
        let changes = matches!(&blob, ScratchBlob::Parsed(v) if !v.is_empty()) || !folders.is_empty();
        let backup = if changes { backup_marked(&self.conn, LOCAL_BACKUP_MARK, "m9-backup")? } else { None };
        let run = || -> Result<(usize, usize)> {
            let tx = write_tx(&self.conn)?;
            let mut scratch = 0;
            match &blob {
                ScratchBlob::Parsed(all) => {
                    // After the rows already here (they win by id).
                    let base: i64 = tx.query_row("SELECT COALESCE(MAX(position) + 1, 0) FROM scratch_repos", [], |r| r.get(0))?;
                    for (i, e) in all.iter().enumerate() {
                        scratch += insert_scratch(&tx, e, base + i as i64, false)?;
                    }
                    mark(&tx, SCRATCH_IMPORT, &format!("{} imported", scratch))?;
                }
                ScratchBlob::Absent if scratch_pending => mark(&tx, SCRATCH_IMPORT, "none")?,
                _ => {}
            }
            #[cfg(test)]
            if super::test_hooks::fails("m9") {
                return Err(rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL), None).into());
            }
            let mut n = 0;
            if folders_pending {
                for f in &folders {
                    n += tx.execute(
                        "INSERT OR IGNORE INTO workspace_folders (workspace_id, path, source, updated_at) VALUES (?1, ?2, ?3, ?4)",
                        rusqlite::params![f.workspace_id.to_string(), f.path.trim(), legacy.source, now_ms()],
                    )?;
                }
                mark(&tx, &folders_import(legacy.source), &format!("{n} imported"))?;
            }
            tx.commit()?;
            Ok((scratch, n))
        };
        match run() {
            Ok((scratch, folders)) => {
                tracing::info!(target: "event_store", scratch, folders, "the scratch repository list and workspace folders are now tables");
                Ok(LocalTablesMigration::Migrated { scratch, folders, backup, scratch_blocked })
            }
            Err(e) => {
                if let Some(path) = &backup {
                    remove_backup(path);
                }
                Err(e)
            }
        }
    }
}

/// A light read of `workspace_id`'s folder in the database at `db`, for a
/// reader that has no store open (the CLI's per-turn binding lookup).
/// `None`: use the older source (no database, a newer one, or `source`'s
/// import hasn't run, so the table isn't the truth yet); `Some(None)`:
/// nothing is bound.
pub fn peek_workspace_folder(db: &std::path::Path, workspace_id: Uuid, source: &str) -> Option<Option<PathBuf>> {
    if !db.exists() {
        return None;
    }
    let c = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
    let _ = c.busy_timeout(std::time::Duration::from_secs(2));
    if user_version(&c).ok()? > SCHEMA_VERSION {
        return None;
    }
    c.query_row("SELECT 1 FROM local_imports WHERE name = ?1", [folders_import(source)], |_| Ok(())).optional().ok()??;
    Some(
        c.query_row("SELECT path FROM workspace_folders WHERE workspace_id = ?1", [workspace_id.to_string()], |r| r.get::<_, String>(0))
            .optional()
            .ok()?
            .map(PathBuf::from),
    )
}

/// Bind (or unbind) `workspace_id`'s folder in the database at `db`, for a
/// writer with no store open. `Ok(false)`: the table isn't the truth yet
/// (no database, a newer one, or `source`'s import hasn't run): the caller
/// writes its older source as before, and the import takes it later.
pub fn write_workspace_folder(db: &std::path::Path, workspace_id: Uuid, source: &str, path: Option<&str>) -> rusqlite::Result<bool> {
    if !db.exists() {
        return Ok(false);
    }
    let c = Connection::open(db)?;
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    if user_version(&c)? > SCHEMA_VERSION {
        return Ok(false);
    }
    let imported = c
        .query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'local_imports'", [], |_| Ok(()))
        .optional()?
        .is_some()
        && c.query_row("SELECT 1 FROM local_imports WHERE name = ?1", [folders_import(source)], |_| Ok(())).optional()?.is_some();
    if !imported {
        return Ok(false);
    }
    match path.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => c.execute(
            "INSERT INTO workspace_folders (workspace_id, path, source, updated_at) VALUES (?1, ?2, 'bound', ?3) \
             ON CONFLICT (workspace_id) DO UPDATE SET path = excluded.path, source = excluded.source, updated_at = excluded.updated_at",
            rusqlite::params![workspace_id.to_string(), p, now_ms()],
        )?,
        None => c.execute("DELETE FROM workspace_folders WHERE workspace_id = ?1", [workspace_id.to_string()])?,
    };
    Ok(true)
}

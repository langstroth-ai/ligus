//! Device-local repo tables (repos-and-checkouts 2.2): which folder on this
//! device holds which workspace repo, per-chat overrides, scan roots, SSH
//! host aliases and a few per-workspace switches. Never synced, never
//! exported, never in an event, a beat or a phone DTO.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

use super::{EventStore, Result};

pub(super) fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS repo_checkouts (
            workspace_id TEXT NOT NULL,
            repo_id      TEXT NOT NULL,
            path         TEXT NOT NULL,
            source       TEXT NOT NULL,
            is_primary   INTEGER NOT NULL DEFAULT 0,
            confirmed_ms INTEGER,
            via_fork     INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (workspace_id, repo_id, path)
        );
        -- One repo per folder in a workspace (review F13).
        CREATE UNIQUE INDEX IF NOT EXISTS idx_repo_checkouts_path ON repo_checkouts (workspace_id, path);
        CREATE TABLE IF NOT EXISTS chat_checkouts (
            chat_id TEXT NOT NULL,
            repo_id TEXT NOT NULL,
            path    TEXT NOT NULL,
            PRIMARY KEY (chat_id, repo_id)
        );
        CREATE TABLE IF NOT EXISTS repo_scan_roots (
            path     TEXT PRIMARY KEY,
            added_ms INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS ssh_host_aliases (
            alias TEXT PRIMARY KEY,
            host  TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS repo_settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        -- Synced aliases a person on this device accepted (design 12): an
        -- alias joins identity matching here only while the workspace still
        -- names it with the same target.
        CREATE TABLE IF NOT EXISTS accepted_aliases (
            workspace_id TEXT NOT NULL,
            kind         TEXT NOT NULL,
            alias        TEXT NOT NULL,
            target       TEXT NOT NULL,
            accepted_ms  INTEGER NOT NULL,
            PRIMARY KEY (workspace_id, kind, alias, target)
        );
        "#,
    )
}

/// How a mapping came to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckoutSource {
    /// A person picked the folder on this device.
    Bound,
    /// A scan found it; unconfirmed until a person says "use it".
    Detected,
    /// This device cloned it for the repo.
    Cloned,
    /// An older binding (the desktop's folder, the box's `repo-bindings.toml`).
    Migrated,
}

impl CheckoutSource {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckoutSource::Bound => "bound",
            CheckoutSource::Detected => "detected",
            CheckoutSource::Cloned => "cloned",
            CheckoutSource::Migrated => "migrated",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "bound" => CheckoutSource::Bound,
            "cloned" => CheckoutSource::Cloned,
            "migrated" => CheckoutSource::Migrated,
            _ => CheckoutSource::Detected,
        }
    }
}

/// One folder on this device that holds a workspace repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoMapping {
    pub workspace_id: Uuid,
    pub repo_id: Uuid,
    pub path: PathBuf,
    pub source: CheckoutSource,
    pub primary: bool,
    /// A person confirmed it (an explicit bind or clone is confirmed; a
    /// detected match isn't until someone says so).
    pub confirmed: bool,
    /// Covers the repo through a fork (its `upstream` remote).
    pub via_fork: bool,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn path_key(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

impl EventStore {
    /// This device's mappings in `workspace_id` (primary first).
    pub fn repo_mappings(&self, workspace_id: Uuid) -> Result<Vec<RepoMapping>> {
        let mut st = self.conn.prepare(
            "SELECT repo_id, path, source, is_primary, confirmed_ms, via_fork FROM repo_checkouts
             WHERE workspace_id = ?1 ORDER BY is_primary DESC, confirmed_ms IS NULL, path",
        )?;
        let rows = st.query_map([workspace_id.to_string()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get::<_, Option<i64>>(4)?, r.get::<_, i64>(5)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (repo, path, source, primary, confirmed, fork) = row?;
            let Ok(repo_id) = Uuid::parse_str(&repo) else { continue };
            out.push(RepoMapping {
                workspace_id,
                repo_id,
                path: PathBuf::from(path),
                source: CheckoutSource::parse(&source),
                primary: primary != 0,
                confirmed: confirmed.is_some(),
                via_fork: fork != 0,
            });
        }
        Ok(out)
    }

    /// Record a mapping. A folder holds one repo per workspace: mapping a
    /// folder already mapped to another repo returns `false` and changes
    /// nothing. The first confirmed mapping of a repo becomes its primary.
    /// A detected mapping never downgrades a confirmed one.
    pub fn put_repo_mapping(&self, m: &RepoMapping) -> Result<bool> {
        let ws = m.workspace_id.to_string();
        let path = path_key(&m.path);
        let other: Option<String> = self
            .conn
            .query_row("SELECT repo_id FROM repo_checkouts WHERE workspace_id = ?1 AND path = ?2", [&ws, &path], |r| r.get(0))
            .optional()?;
        if other.as_deref().is_some_and(|o| o != m.repo_id.to_string()) {
            return Ok(false);
        }
        let has_primary: bool = self
            .conn
            .prepare("SELECT 1 FROM repo_checkouts WHERE workspace_id = ?1 AND repo_id = ?2 AND is_primary = 1 AND path != ?3")?
            .exists([&ws, &m.repo_id.to_string(), &path])?;
        let primary = m.primary || (m.confirmed && !has_primary);
        if primary {
            self.conn.execute(
                "UPDATE repo_checkouts SET is_primary = 0 WHERE workspace_id = ?1 AND repo_id = ?2",
                [&ws, &m.repo_id.to_string()],
            )?;
        }
        self.conn.execute(
            "INSERT INTO repo_checkouts (workspace_id, repo_id, path, source, is_primary, confirmed_ms, via_fork)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (workspace_id, repo_id, path) DO UPDATE SET
               source = CASE WHEN excluded.source = 'detected' THEN source ELSE excluded.source END,
               is_primary = MAX(is_primary, excluded.is_primary),
               confirmed_ms = COALESCE(confirmed_ms, excluded.confirmed_ms),
               via_fork = excluded.via_fork",
            rusqlite::params![ws, m.repo_id.to_string(), path, m.source.as_str(), primary as i64, m.confirmed.then(now_ms), m.via_fork as i64],
        )?;
        Ok(true)
    }

    /// A person confirmed a detected mapping ("use it"); it becomes primary
    /// when the repo has none.
    pub fn confirm_repo_mapping(&self, workspace_id: Uuid, repo_id: Uuid, path: &Path) -> Result<bool> {
        let Some(mut m) = self.repo_mappings(workspace_id)?.into_iter().find(|m| m.repo_id == repo_id && m.path == path) else {
            return Ok(false);
        };
        m.confirmed = true;
        self.put_repo_mapping(&m)
    }

    /// Forget a mapping (the folder stays). The next confirmed mapping of
    /// the repo becomes primary.
    pub fn remove_repo_mapping(&self, workspace_id: Uuid, repo_id: Uuid, path: &Path) -> Result<()> {
        let ws = workspace_id.to_string();
        self.conn.execute(
            "DELETE FROM repo_checkouts WHERE workspace_id = ?1 AND repo_id = ?2 AND path = ?3",
            [&ws, &repo_id.to_string(), &path_key(path)],
        )?;
        let left: Option<String> = self
            .conn
            .query_row(
                "SELECT path FROM repo_checkouts WHERE workspace_id = ?1 AND repo_id = ?2 AND confirmed_ms IS NOT NULL
                 ORDER BY is_primary DESC, confirmed_ms LIMIT 1",
                [&ws, &repo_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(p) = left {
            self.conn.execute(
                "UPDATE repo_checkouts SET is_primary = (path = ?3) WHERE workspace_id = ?1 AND repo_id = ?2",
                [&ws, &repo_id.to_string(), &p],
            )?;
        }
        Ok(())
    }

    /// The confirmed checkout a turn uses for `repo_id` (the primary).
    pub fn primary_checkout(&self, workspace_id: Uuid, repo_id: Uuid) -> Result<Option<RepoMapping>> {
        Ok(self.repo_mappings(workspace_id)?.into_iter().find(|m| m.repo_id == repo_id && m.confirmed))
    }

    /// The per-chat override: "this chat uses my second clone".
    pub fn chat_checkout(&self, chat_id: Uuid, repo_id: Uuid) -> Result<Option<PathBuf>> {
        Ok(self
            .conn
            .query_row(
                "SELECT path FROM chat_checkouts WHERE chat_id = ?1 AND repo_id = ?2",
                [chat_id.to_string(), repo_id.to_string()],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(PathBuf::from))
    }

    pub fn set_chat_checkout(&self, chat_id: Uuid, repo_id: Uuid, path: Option<&Path>) -> Result<()> {
        match path {
            Some(p) => self.conn.execute(
                "INSERT INTO chat_checkouts (chat_id, repo_id, path) VALUES (?1, ?2, ?3)
                 ON CONFLICT (chat_id, repo_id) DO UPDATE SET path = excluded.path",
                [chat_id.to_string(), repo_id.to_string(), path_key(p)],
            )?,
            None => self
                .conn
                .execute("DELETE FROM chat_checkouts WHERE chat_id = ?1 AND repo_id = ?2", [chat_id.to_string(), repo_id.to_string()])?,
        };
        Ok(())
    }

    /// The folders this device scans for checkouts.
    pub fn scan_roots(&self) -> Result<Vec<PathBuf>> {
        let mut st = self.conn.prepare("SELECT path FROM repo_scan_roots ORDER BY path")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).map(PathBuf::from).collect())
    }

    pub fn add_scan_root(&self, path: &Path) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO repo_scan_roots (path, added_ms) VALUES (?1, ?2)",
            rusqlite::params![path_key(path), now_ms()],
        )?;
        Ok(())
    }

    pub fn remove_scan_root(&self, path: &Path) -> Result<()> {
        self.conn.execute("DELETE FROM repo_scan_roots WHERE path = ?1", [path_key(path)])?;
        Ok(())
    }

    /// This device's SSH host aliases (`github-work` → `github.com`):
    /// identity only, never where git connects. Device-local (review F5d).
    pub fn ssh_host_aliases(&self) -> Result<std::collections::BTreeMap<String, String>> {
        let mut st = self.conn.prepare("SELECT alias, host FROM ssh_host_aliases")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn set_ssh_host_alias(&self, alias: &str, host: Option<&str>) -> Result<()> {
        let alias = alias.trim().to_ascii_lowercase();
        match host {
            Some(h) => self.conn.execute(
                "INSERT INTO ssh_host_aliases (alias, host) VALUES (?1, ?2) ON CONFLICT (alias) DO UPDATE SET host = excluded.host",
                [alias, h.trim().to_ascii_lowercase()],
            )?,
            None => self.conn.execute("DELETE FROM ssh_host_aliases WHERE alias = ?1", [alias])?,
        };
        Ok(())
    }

    /// The synced aliases a person on this device accepted in
    /// `workspace_id`: (kind, alias, target), kind `repo` (target: the repo
    /// id) or `host` (target: the canonical host).
    pub fn accepted_aliases(&self, workspace_id: Uuid) -> Result<Vec<(String, String, String)>> {
        let mut st = self.conn.prepare("SELECT kind, alias, target FROM accepted_aliases WHERE workspace_id = ?1 ORDER BY kind, alias")?;
        let rows = st.query_map([workspace_id.to_string()], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Accept (or, with `accept` false, take back) a synced alias here.
    pub fn set_alias_accepted(&self, workspace_id: Uuid, kind: &str, alias: &str, target: &str, accept: bool) -> Result<()> {
        if accept {
            self.conn.execute(
                "INSERT OR REPLACE INTO accepted_aliases (workspace_id, kind, alias, target, accepted_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![workspace_id.to_string(), kind, alias, target, now_ms()],
            )?;
        } else {
            self.conn.execute(
                "DELETE FROM accepted_aliases WHERE workspace_id = ?1 AND kind = ?2 AND alias = ?3",
                rusqlite::params![workspace_id.to_string(), kind, alias],
            )?;
        }
        Ok(())
    }

    /// A device-local repo switch (`coverage:<workspace>` and the like).
    pub fn repo_setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM repo_settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn set_repo_setting(&self, key: &str, value: Option<&str>) -> Result<()> {
        match value {
            Some(v) => self.conn.execute(
                "INSERT INTO repo_settings (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [key, v],
            )?,
            None => self.conn.execute("DELETE FROM repo_settings WHERE key = ?1", [key])?,
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(ws: Uuid, repo: Uuid, path: &str, source: CheckoutSource, confirmed: bool) -> RepoMapping {
        RepoMapping { workspace_id: ws, repo_id: repo, path: PathBuf::from(path), source, primary: false, confirmed, via_fork: false }
    }

    #[test]
    fn one_repo_per_folder_and_a_primary_per_repo() {
        let s = EventStore::open_in_memory().unwrap();
        let (ws, a, b) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        assert!(s.put_repo_mapping(&m(ws, a, "/x/wefit", CheckoutSource::Bound, true)).unwrap());
        assert!(!s.put_repo_mapping(&m(ws, b, "/x/wefit", CheckoutSource::Bound, true)).unwrap(), "a folder holds one repo");
        assert!(s.put_repo_mapping(&m(ws, a, "/x/wefit2", CheckoutSource::Detected, false)).unwrap());
        let p = s.primary_checkout(ws, a).unwrap().unwrap();
        assert_eq!(p.path, PathBuf::from("/x/wefit"));
        // A rescan never unconfirms or downgrades a bound mapping.
        assert!(s.put_repo_mapping(&m(ws, a, "/x/wefit", CheckoutSource::Detected, false)).unwrap());
        let p = s.primary_checkout(ws, a).unwrap().unwrap();
        assert_eq!((p.source, p.confirmed, p.primary), (CheckoutSource::Bound, true, true));
        // Removing the primary promotes the next confirmed one.
        s.confirm_repo_mapping(ws, a, Path::new("/x/wefit2")).unwrap();
        s.remove_repo_mapping(ws, a, Path::new("/x/wefit")).unwrap();
        assert_eq!(s.primary_checkout(ws, a).unwrap().unwrap().path, PathBuf::from("/x/wefit2"));
        // Another workspace is separate.
        assert!(s.repo_mappings(Uuid::new_v4()).unwrap().is_empty());
    }

    #[test]
    fn chat_overrides_roots_aliases_and_settings() {
        let s = EventStore::open_in_memory().unwrap();
        let (chat, repo) = (Uuid::new_v4(), Uuid::new_v4());
        s.set_chat_checkout(chat, repo, Some(Path::new("/y"))).unwrap();
        assert_eq!(s.chat_checkout(chat, repo).unwrap(), Some(PathBuf::from("/y")));
        s.set_chat_checkout(chat, repo, None).unwrap();
        assert_eq!(s.chat_checkout(chat, repo).unwrap(), None);
        s.add_scan_root(Path::new("/r")).unwrap();
        s.add_scan_root(Path::new("/r")).unwrap();
        assert_eq!(s.scan_roots().unwrap(), vec![PathBuf::from("/r")]);
        s.set_ssh_host_alias("GitHub-Work", Some("github.com")).unwrap();
        assert_eq!(s.ssh_host_aliases().unwrap().get("github-work").map(String::as_str), Some("github.com"));
        s.set_repo_setting("k", Some("v")).unwrap();
        assert_eq!(s.repo_setting("k").unwrap().as_deref(), Some("v"));
    }
}

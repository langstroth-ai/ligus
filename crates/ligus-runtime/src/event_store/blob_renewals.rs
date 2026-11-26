//! Device-local blob renewals (identity model v2, 10.5): per workspace and
//! relay blob, when this device last renewed it on the relay, and whether the
//! relay said it is gone (expired or deleted). Never synced, never in an
//! event: each renewing device keeps its own schedule, and "this file
//! expired on the relay" is what this device saw.

use std::collections::HashMap;

use rusqlite::{params, Connection};
use uuid::Uuid;

use super::{EventStore, Result};

pub(super) fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS blob_renewals (
            workspace_id TEXT NOT NULL,
            blob_id      TEXT NOT NULL,
            renewed_at   INTEGER,
            gone_at      INTEGER,
            PRIMARY KEY (workspace_id, blob_id)
        );
        "#,
    )
}

/// One blob's device-local renewal state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlobRenewal {
    /// Unix seconds of the last renewal attempt the relay answered.
    pub renewed_at: Option<i64>,
    /// Unix seconds this device learned the relay no longer has it.
    pub gone_at: Option<i64>,
}

impl EventStore {
    /// Every blob of `workspace_id` this device has renewed or seen gone.
    pub fn blob_renewals(&self, workspace_id: Uuid) -> Result<HashMap<String, BlobRenewal>> {
        let mut stmt = self.conn.prepare(
            "SELECT blob_id, renewed_at, gone_at FROM blob_renewals WHERE workspace_id = ?1",
        )?;
        let rows = stmt.query_map(params![workspace_id.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                BlobRenewal {
                    renewed_at: r.get(1)?,
                    gone_at: r.get(2)?,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The relay answered a renewal of `blob_id` at `at` (renewed, or a relay
    /// without the route: either way, not again for a day).
    pub fn record_blob_renewed(&self, workspace_id: Uuid, blob_id: &str, at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO blob_renewals (workspace_id, blob_id, renewed_at) VALUES (?1, ?2, ?3) \
             ON CONFLICT (workspace_id, blob_id) DO UPDATE SET renewed_at = excluded.renewed_at",
            params![workspace_id.to_string(), blob_id, at],
        )?;
        Ok(())
    }

    /// The relay no longer has `blob_id` (a download or a renewal found it
    /// gone): it is shown as expired and never renewed again.
    pub fn mark_blob_gone(&self, workspace_id: Uuid, blob_id: &str, at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO blob_renewals (workspace_id, blob_id, gone_at) VALUES (?1, ?2, ?3) \
             ON CONFLICT (workspace_id, blob_id) DO UPDATE SET gone_at = COALESCE(gone_at, excluded.gone_at)",
            params![workspace_id.to_string(), blob_id, at],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renewals_and_gone_are_kept_per_workspace() {
        let store = EventStore::open_in_memory().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        store.record_blob_renewed(a, "x", 100).unwrap();
        store.record_blob_renewed(a, "x", 200).unwrap();
        store.mark_blob_gone(a, "y", 300).unwrap();
        store.mark_blob_gone(a, "y", 400).unwrap();
        let got = store.blob_renewals(a).unwrap();
        assert_eq!(
            got["x"],
            BlobRenewal {
                renewed_at: Some(200),
                gone_at: None
            }
        );
        assert_eq!(
            got["y"],
            BlobRenewal {
                renewed_at: None,
                gone_at: Some(300)
            },
            "the first sighting stands"
        );
        assert!(
            store.blob_renewals(b).unwrap().is_empty(),
            "another workspace's blob of the same id is its own"
        );
    }
}

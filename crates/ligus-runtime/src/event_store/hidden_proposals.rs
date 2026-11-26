//! Proposals a person hid for themselves on this device ("Hide for me").
//! Device-local: there is no per-person channel that syncs between one
//! person's own devices, so a hide never leaves this device, never enters an
//! event, a beat or a phone DTO, and another device of the same person still
//! shows the proposal. Archiving (synced, for everyone) is the
//! `ProposalDismissed` event instead.

use std::collections::BTreeSet;

use rusqlite::Connection;
use uuid::Uuid;

use super::{EventStore, Result};

pub(super) fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS hidden_proposals (
            workspace_id TEXT NOT NULL,
            proposal_id  TEXT NOT NULL,
            hidden_ms    INTEGER NOT NULL,
            PRIMARY KEY (workspace_id, proposal_id)
        );
        "#,
    )
}

impl EventStore {
    /// The proposals hidden on this device in `workspace_id`.
    pub fn hidden_proposals(&self, workspace_id: Uuid) -> Result<BTreeSet<Uuid>> {
        let mut st = self
            .conn
            .prepare("SELECT proposal_id FROM hidden_proposals WHERE workspace_id = ?1")?;
        let rows = st.query_map([workspace_id.to_string()], |r| r.get::<_, String>(0))?;
        Ok(rows
            .filter_map(|r| r.ok())
            .filter_map(|s| Uuid::parse_str(&s).ok())
            .collect())
    }

    /// Hide (or, with `hidden` false, show again) proposals on this device.
    pub fn set_proposals_hidden(
        &self,
        workspace_id: Uuid,
        proposal_ids: &[Uuid],
        hidden: bool,
    ) -> Result<()> {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or_default();
        for id in proposal_ids {
            if hidden {
                self.conn.execute(
                    "INSERT OR IGNORE INTO hidden_proposals (workspace_id, proposal_id, hidden_ms) VALUES (?1, ?2, ?3)",
                    rusqlite::params![workspace_id.to_string(), id.to_string(), ms],
                )?;
            } else {
                self.conn.execute(
                    "DELETE FROM hidden_proposals WHERE workspace_id = ?1 AND proposal_id = ?2",
                    rusqlite::params![workspace_id.to_string(), id.to_string()],
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_per_workspace_and_shows_again() {
        let store = EventStore::open_in_memory().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let p = Uuid::new_v4();
        store.set_proposals_hidden(a, &[p], true).unwrap();
        store.set_proposals_hidden(a, &[p], true).unwrap();
        assert_eq!(store.hidden_proposals(a).unwrap(), BTreeSet::from([p]));
        assert!(
            store.hidden_proposals(b).unwrap().is_empty(),
            "another workspace keeps its own"
        );
        store.set_proposals_hidden(a, &[p], false).unwrap();
        assert!(store.hidden_proposals(a).unwrap().is_empty());
    }
}

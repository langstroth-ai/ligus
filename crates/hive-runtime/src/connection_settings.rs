//! A client's workspace list and connection settings, with their secrets in
//! the OS secret store (Phase 0, B2).
//!
//! What a client keeps to reach a workspace: the relay, the room, the
//! founder pin, and three secrets (the relay access token, the workspace
//! passphrase and the epoch keys an invite delivered). The plain part goes
//! in an owner-only file under the data dir; the secrets go in the
//! [`SecretStore`] under the client's own scope (`cli` for the `ligus` CLI,
//! `term` for the terminal app), never in the file.
//!
//! - [`ConnectionSecrets`]: the three secrets of one connection, named
//!   `<prefix>token`, `<prefix>key` and `<prefix>ek` in a scope. The CLI's
//!   single connection uses an empty prefix.
//! - [`WorkspaceList`]: several workspaces (a client with a workspace
//!   switcher), as `workspaces.json`; each workspace's secrets are prefixed
//!   with its id.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::keyring::DeliveredKey;
use crate::secret_store::{SecretError, SecretId, SecretStore};
use crate::workspace_invite::WorkspaceConn;

/// The three secrets of one connection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectionSecrets {
    /// The relay access token (hrt1/hex).
    pub token: Option<String>,
    /// The workspace E2EE passphrase.
    pub key: Option<String>,
    /// Epoch keys an invite delivered (B1).
    pub ek: Vec<DeliveredKey>,
}

impl ConnectionSecrets {
    /// Read them from `store` (blank values are absent). The token and the
    /// passphrase are registered for redaction.
    pub fn load(store: &dyn SecretStore, scope: &'static str, prefix: &str) -> Self {
        let get = |n: &str| {
            store
                .get_string(&SecretId::new(scope, format!("{prefix}{n}")))
                .ok()
                .flatten()
                .map(|z| z.to_string())
                .filter(|v| !v.is_empty())
        };
        let s = Self {
            token: get("token"),
            key: get("key"),
            ek: get("ek").and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default(),
        };
        if let Some(t) = &s.token {
            crate::secret_redact::register(t);
        }
        if let Some(k) = &s.key {
            crate::secret_redact::register(k);
        }
        s
    }

    /// Write them to `store`; an absent or blank value is deleted.
    pub fn save(&self, store: &dyn SecretStore, scope: &'static str, prefix: &str) -> Result<(), SecretError> {
        let put = |n: &str, v: Option<&str>| -> Result<(), SecretError> {
            let id = SecretId::new(scope, format!("{prefix}{n}"));
            match v.filter(|v| !v.is_empty()) {
                Some(v) => store.put(&id, v.as_bytes()),
                None => store.delete(&id),
            }
        };
        put("token", self.token.as_deref())?;
        put("key", self.key.as_deref())?;
        let ek = (!self.ek.is_empty()).then(|| serde_json::to_string(&self.ek)).transpose().map_err(|e| SecretError::Io(e.into()))?;
        put("ek", ek.as_deref())
    }

    /// Delete them from `store`.
    pub fn delete(store: &dyn SecretStore, scope: &'static str, prefix: &str) -> Result<(), SecretError> {
        Self::default().save(store, scope, prefix)
    }
}

/// One saved workspace: its connection without secrets, plus the secrets
/// (loaded from and saved to the store, never to the file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedWorkspace {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub relay_url: String,
    pub room: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dm_account: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub founder_actor_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fk: Option<String>,
    #[serde(skip)]
    pub secrets: ConnectionSecrets,
}

impl SavedWorkspace {
    /// From a connection (an invite or a new workspace) and a relay token.
    /// The single-use invite and the signature are not kept.
    pub fn from_conn(conn: &WorkspaceConn, token: Option<String>) -> Self {
        Self {
            name: conn.display_name(),
            relay_url: conn.relay_url.trim().to_string(),
            room: conn.room.clone(),
            dm_account: conn.dm_account.clone(),
            founder_actor_id: conn.founder_actor_id.clone(),
            fk: conn.fk.clone(),
            secrets: ConnectionSecrets { token, key: conn.key.clone(), ek: conn.ek.clone() },
        }
    }

    /// The workspace id ([`hive_core::room_workspace_id`]).
    pub fn id(&self) -> Uuid {
        hive_core::room_workspace_id(&self.room)
    }

    /// The connection, with its secrets (for an invite or a sync engine).
    pub fn conn(&self) -> WorkspaceConn {
        WorkspaceConn {
            name: self.name.clone(),
            relay_url: self.relay_url.clone(),
            room: self.room.clone(),
            key: self.secrets.key.clone(),
            icon: None,
            dm_account: self.dm_account.clone(),
            founder_actor_id: self.founder_actor_id.clone(),
            fk: self.fk.clone(),
            inv: None,
            ek: self.secrets.ek.clone(),
            sig: None,
        }
    }

    /// The relay to sync through, when it is a usable http(s) URL.
    pub fn relay(&self) -> Option<&str> {
        let r = self.relay_url.trim();
        crate::workspace_invite::usable_relay_url(r).then_some(r)
    }

    fn secret_prefix(&self) -> String {
        secret_prefix(self.id())
    }
}

fn secret_prefix(id: Uuid) -> String {
    format!("ws-{id}-")
}

/// A client's workspaces, in the order they were added.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceList {
    #[serde(default)]
    pub workspaces: Vec<SavedWorkspace>,
}

/// The file a [`WorkspaceList`] lives in, under the data dir.
pub const WORKSPACE_LIST_FILE: &str = "workspaces.json";

impl WorkspaceList {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(WORKSPACE_LIST_FILE)
    }

    /// The list in `data_dir`, with each workspace's secrets from `store`
    /// under `scope`. A missing or unreadable file is an empty list.
    pub fn load(data_dir: &Path, store: &dyn SecretStore, scope: &'static str) -> Self {
        let mut list: WorkspaceList = std::fs::read(Self::path(data_dir))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        for w in &mut list.workspaces {
            w.secrets = ConnectionSecrets::load(store, scope, &w.secret_prefix());
        }
        list
    }

    /// Write the file (owner-only, no secrets) and every workspace's secrets.
    pub fn save(&self, data_dir: &Path, store: &dyn SecretStore, scope: &'static str) -> Result<(), SecretError> {
        for w in &self.workspaces {
            w.secrets.save(store, scope, &w.secret_prefix())?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(|e| SecretError::Io(e.into()))?;
        crate::private_file::write_private_atomic(&Self::path(data_dir), &json)?;
        Ok(())
    }

    pub fn get(&self, id: Uuid) -> Option<&SavedWorkspace> {
        self.workspaces.iter().find(|w| w.id() == id)
    }

    /// Add `w`, or replace the saved workspace with the same room (a
    /// re-join refreshes the relay and keys).
    pub fn upsert(&mut self, w: SavedWorkspace) {
        match self.workspaces.iter_mut().find(|x| x.room == w.room) {
            Some(x) => *x = w,
            None => self.workspaces.push(w),
        }
    }

    /// Remove workspace `id` and delete its secrets. Returns whether it was
    /// there.
    pub fn remove(&mut self, id: Uuid, store: &dyn SecretStore, scope: &'static str) -> Result<bool, SecretError> {
        let before = self.workspaces.len();
        self.workspaces.retain(|w| w.id() != id);
        if self.workspaces.len() != before {
            ConnectionSecrets::delete(store, scope, &secret_prefix(id))?;
            return Ok(true);
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret_store::File0600;

    fn conn(room: &str) -> WorkspaceConn {
        WorkspaceConn {
            name: "Team".into(),
            relay_url: " https://relay.example ".into(),
            room: room.into(),
            key: Some("passphrase-1".into()),
            icon: Some("data:image/png;base64,AA".into()),
            dm_account: None,
            founder_actor_id: Some(Uuid::new_v4().to_string()),
            fk: Some("00".repeat(16)),
            inv: Some(Uuid::new_v4().to_string()),
            ek: vec![DeliveredKey::new(2, &[7u8; 32])],
            sig: None,
        }
    }

    #[test]
    fn secrets_round_trip_and_blank_deletes() {
        let tmp = tempfile::tempdir().unwrap();
        let store = File0600::new(tmp.path());
        let s = ConnectionSecrets { token: Some("hrt1-x".into()), key: Some("k".into()), ek: vec![DeliveredKey::new(1, &[1u8; 32])] };
        s.save(&store, "cli", "").unwrap();
        assert_eq!(ConnectionSecrets::load(&store, "cli", ""), s);
        assert_eq!(ConnectionSecrets::load(&store, "term", ""), ConnectionSecrets::default(), "scopes are separate");
        ConnectionSecrets { token: Some(String::new()), ..s.clone() }.save(&store, "cli", "").unwrap();
        assert_eq!(ConnectionSecrets::load(&store, "cli", "").token, None);
    }

    #[test]
    fn the_list_file_never_holds_a_secret() {
        let tmp = tempfile::tempdir().unwrap();
        let store = File0600::new(tmp.path());
        let c = conn("room-a");
        let mut list = WorkspaceList::default();
        list.upsert(SavedWorkspace::from_conn(&c, Some("hrt1-token-abc".into())));
        list.save(tmp.path(), &store, "term").unwrap();
        let text = std::fs::read_to_string(WorkspaceList::path(tmp.path())).unwrap();
        for secret in ["passphrase-1", "hrt1-token-abc", &DeliveredKey::new(2, &[7u8; 32]).k, "data:image", c.inv.as_deref().unwrap()] {
            assert!(!text.contains(secret), "{secret} leaked into {text}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(WorkspaceList::path(tmp.path())).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let back = WorkspaceList::load(tmp.path(), &store, "term");
        assert_eq!(back, list);
        let w = back.get(c.id()).unwrap();
        assert_eq!(w.relay(), Some("https://relay.example"));
        let again = w.conn();
        assert_eq!((again.key, again.ek, again.inv, again.icon), (c.key.clone(), c.ek.clone(), None, None));
    }

    #[test]
    fn upsert_replaces_by_room_and_remove_deletes_secrets() {
        let tmp = tempfile::tempdir().unwrap();
        let store = File0600::new(tmp.path());
        let mut list = WorkspaceList::default();
        list.upsert(SavedWorkspace::from_conn(&conn("a"), None));
        list.upsert(SavedWorkspace::from_conn(&conn("b"), None));
        let mut c = conn("a");
        c.relay_url = "https://other.example".into();
        list.upsert(SavedWorkspace::from_conn(&c, None));
        assert_eq!(list.workspaces.len(), 2);
        assert_eq!(list.workspaces[0].relay_url, "https://other.example");
        list.save(tmp.path(), &store, "term").unwrap();
        assert!(list.remove(c.id(), &store, "term").unwrap());
        assert!(!list.remove(c.id(), &store, "term").unwrap());
        assert_eq!(ConnectionSecrets::load(&store, "term", &secret_prefix(c.id())), ConnectionSecrets::default());
    }

    #[test]
    fn a_relayless_workspace_has_no_relay() {
        let mut c = conn("x");
        c.relay_url = String::new();
        assert_eq!(SavedWorkspace::from_conn(&c, None).relay(), None);
    }
}

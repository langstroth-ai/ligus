//! The desktop's share of the 0.5 local tables (workspace model v3, 13 and
//! 16.2 M9; identity model v2, 10.1 and 10.2).
//!
//! - **Workspace folders.** The folder bound to each workspace on this
//!   device lives in `ligus.db`'s `workspace_folders` table. `settings.json`'s
//!   `workspace_dirs` (with the older `workspace_root` already folded into it
//!   for the local workspace) is imported once; from then on the table is
//!   read first and the map only fills in what the table lacks. A new
//!   binding is written to the table (and to the map, which a 0.4 build on
//!   this data dir still reads).
//! - **One connection list.** `workspaces.json` (the format lt and the CLI
//!   read) mirrors `settings.json`'s `workspaces[]` at every launch: each
//!   connection's secrets are copied to its own ids under the `desktop`
//!   scope and read back before the file names it. The desktop itself keeps
//!   reading `settings.json` in 0.5: that list carries fields the shared
//!   format doesn't (the workspace icon, the invite a join still redeems),
//!   and its secrets already live in the secret store. Nothing in
//!   `settings.json` changes.

use std::collections::HashMap;
use std::path::Path;

use ligus_runtime::connection_settings::{SavedWorkspace, WorkspaceList};
use ligus_runtime::event_store::{LegacyFolder, LegacyLocal, LocalTablesMigration};
use ligus_runtime::ChatService;
use uuid::Uuid;

use crate::LiveSettings;

/// The name the desktop's rows and import carry.
pub(crate) const SOURCE: &str = "desktop";

/// The secret-store scope of the desktop's `workspaces.json` entries.
pub(crate) const SCOPE: &str = "desktop";

/// M9 for the desktop: import `dirs` (workspace id → folder) into the
/// table once, then fill `dirs` from the table (the table wins). Returns
/// whether `dirs` changed.
pub(crate) fn import_and_overlay(service: &ChatService, dirs: &mut HashMap<String, String>) -> bool {
    let folders = dirs
        .iter()
        .filter_map(|(k, v)| Some(LegacyFolder { workspace_id: Uuid::parse_str(k).ok()?, path: v.clone() }))
        .collect();
    match service.store().migrate_local_tables(&LegacyLocal { source: SOURCE, folders }) {
        Ok(LocalTablesMigration::Migrated { scratch, folders, backup, scratch_blocked }) => {
            tracing::info!(target: "settings", scratch, folders, backup = ?backup, scratch_blocked, "the scratch list and the workspace folders are now tables");
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(target: "settings", "moving the local lists into tables failed; it runs again on the next launch: {e}"),
    }
    overlay(service, dirs)
}

/// Fill `dirs` from the table (the table wins).
fn overlay(service: &ChatService, dirs: &mut HashMap<String, String>) -> bool {
    let mut changed = false;
    for (ws, path) in service.store().workspace_folders().unwrap_or_default() {
        let path = path.to_string_lossy().into_owned();
        if dirs.get(&ws.to_string()) != Some(&path) {
            dirs.insert(ws.to_string(), path);
            changed = true;
        }
    }
    changed
}

/// Record `path` as `workspace_id`'s folder on this device, in the
/// database under `data_dir` (its own short connection: a command must
/// never wait on the service lock). Before the import ran, the map in
/// settings.json is the record and the import takes it later.
pub(crate) fn bind(data_dir: &Path, workspace_id: &str, path: &str) {
    let Ok(ws) = Uuid::parse_str(workspace_id) else { return };
    if let Err(e) = ligus_runtime::event_store::write_workspace_folder(&data_dir.join("ligus.db"), ws, SOURCE, Some(path)) {
        tracing::warn!(target: "settings", "couldn't record the workspace's folder: {e}");
    }
}

/// `settings`'s connections as `workspaces.json` entries. The relay token
/// goes only with the workspaces on the relay it was saved for.
fn saved_workspaces(settings: &LiveSettings) -> Vec<SavedWorkspace> {
    let origin = |u: &str| ligus_runtime::event_store::relay_origin(u.trim());
    let token_relay = settings.relay_url.as_deref().filter(|u| !u.trim().is_empty()).map(origin);
    settings
        .workspaces
        .iter()
        .map(|w| {
            let token = settings.relay_access_token.clone().filter(|t| !t.is_empty() && token_relay.as_deref() == Some(origin(&w.relay_url).as_str()));
            SavedWorkspace::from_conn(w, token)
        })
        .collect()
}

/// Mirror `settings`'s connections into `workspaces.json` in `data_dir`.
pub(crate) fn mirror_connections(data_dir: &Path, settings: &LiveSettings) {
    let store = ligus_runtime::secret_store::for_data_dir(data_dir);
    match WorkspaceList::mirror(data_dir, store.as_ref(), SCOPE, saved_workspaces(settings)) {
        Ok(out) if !out.unverified.is_empty() => {
            tracing::warn!(target: "settings", rooms = out.unverified.len(), "some connections' secrets didn't read back; workspaces.json leaves them out for now")
        }
        Ok(out) if out.written + out.removed > 0 => tracing::info!(target: "settings", written = out.written, removed = out.removed, "workspaces.json updated"),
        Ok(_) => {}
        Err(e) => tracing::warn!(target: "settings", "couldn't update workspaces.json: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(room: &str, relay: &str) -> ligus_runtime::workspace_invite::WorkspaceConn {
        ligus_runtime::workspace_invite::WorkspaceConn {
            name: room.into(),
            relay_url: relay.into(),
            room: room.into(),
            key: Some(format!("decoy-key-{room}")),
            icon: Some("data:image/png;base64,AA".into()),
            dm_account: None,
            founder_actor_id: None,
            fk: None,
            inv: Some("decoy-invite".into()),
            ek: Vec::new(),
            sig: None,
        }
    }

    /// `workspaces.json` mirrors settings.json's list: the secrets are
    /// copied to the entry's own ids, the token only for its own relay, and
    /// neither file gets a secret; a left workspace drops out.
    #[test]
    fn connections_mirror_into_workspaces_json() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = LiveSettings {
            relay_url: Some("https://relay.example".into()),
            relay_access_token: Some("hrt1-decoy-token".into()),
            workspaces: vec![conn("team", "https://relay.example/"), conn("other", "https://elsewhere.example")],
            ..Default::default()
        };
        mirror_connections(tmp.path(), &s);
        let store = ligus_runtime::secret_store::for_data_dir(tmp.path());
        let list = WorkspaceList::load(tmp.path(), store.as_ref(), SCOPE);
        assert_eq!(list.workspaces.iter().map(|w| w.room.as_str()).collect::<Vec<_>>(), vec!["team", "other"]);
        assert_eq!(list.workspaces[0].secrets.token.as_deref(), Some("hrt1-decoy-token"));
        assert_eq!(list.workspaces[1].secrets.token, None, "the token stays with its relay");
        assert_eq!(list.workspaces[1].secrets.key.as_deref(), Some("decoy-key-other"));
        let text = std::fs::read_to_string(WorkspaceList::path(tmp.path())).unwrap();
        assert!(!text.contains("decoy") && !text.contains("data:image"), "{text}");
        s.workspaces.remove(1);
        mirror_connections(tmp.path(), &s);
        assert_eq!(WorkspaceList::load(tmp.path(), store.as_ref(), SCOPE).workspaces.len(), 1);
    }

    /// The desktop's folders go into the table once; the table wins over
    /// the map afterwards, and a new binding is written there.
    #[test]
    fn folders_import_once_and_the_table_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = ChatService::new(
            ligus_runtime::EventStore::open(tmp.path().join("ligus.db")).unwrap(),
            ligus_core::crypto::SigningKeypair::generate().unwrap(),
            ligus_core::crypto::SigningKeypair::generate().unwrap(),
            ligus_core::ActorIdentity::new("me", "Me", ligus_core::ActorKind::Human),
        );
        let (a, b) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
        let mut dirs = HashMap::from([(a.clone(), "/src/a".to_string()), ("not-a-uuid".into(), "/x".into())]);
        import_and_overlay(&svc, &mut dirs);
        assert_eq!(svc.store().workspace_folders().unwrap().len(), 1);
        bind(tmp.path(), &b, "/src/b");
        bind(tmp.path(), &a, "/src/a2");
        let mut stale = HashMap::from([(a.clone(), "/src/a".to_string())]);
        assert!(import_and_overlay(&svc, &mut stale));
        assert_eq!(stale.get(&a).map(String::as_str), Some("/src/a2"));
        assert_eq!(stale.get(&b).map(String::as_str), Some("/src/b"));
    }
}

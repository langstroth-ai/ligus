//! "Set up Ligus" on the desktop (Settings > Set up, the empty Home and the
//! command palette): the shared checklist rows
//! (`ligus_runtime::setup::checklist`) for the active workspace, Clean up's
//! "Remove from view" and the one Clean up removal that had no command yet
//! (a former member's host). Every other removal goes through the existing
//! commands (Remove member, Remove device, Remove agent), each its own
//! passkey ceremony after the pane's confirm.

use serde_json::Value;
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

use ligus_runtime::setup::checklist::{self, hidden, Live, Viewer};

use crate::AppState;

/// How this computer is called in the rows.
fn this_device_word() -> &'static str {
    if cfg!(target_os = "macos") {
        "this Mac"
    } else {
        "this computer"
    }
}

/// The checklist for the active workspace (cheap: the fold, this store and
/// the beats already seen; no network).
pub(crate) fn setup_checklist_sync(state: State<AppState>) -> Result<Value, String> {
    let ws = state.active_workspace_id();
    let name = {
        let s = state.settings.lock().unwrap();
        s.workspaces.iter().find(|w| w.id() == ws).map(|w| w.display_name()).unwrap_or_default()
    };
    let latest = crate::latest_release_tag();
    let hidden = hidden::load(&state.data_dir);
    let svc = state.service.lock().unwrap();
    let here = svc.device_id().to_string();
    let now = ligus_core::Timestamp::now();
    let online = |h: &ligus_core::WorkspaceHost| h.id == here || state.host_presence.is_online(h, &now);
    let status = |id: &str| if id == here { ligus_runtime::host_offers::local_status() } else { state.host_presence.status_of(id) };
    let no_push = |_: Uuid| None;
    let live = Live {
        online: &online,
        status: &status,
        latest_release: latest,
        this_device_word: this_device_word().into(),
        workspace_name: name,
        push: &no_push,
        hidden,
        now,
    };
    let facts = checklist::gather(&svc, ws, &Viewer::author(&svc), &live);
    serde_json::to_value(checklist::checklist(&facts)).map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn setup_checklist(ligus_app: AppHandle) -> Result<Value, String> {
    crate::off_main(ligus_app, move |app| setup_checklist_sync(app.state::<AppState>())).await
}

/// Remove a Clean up item from view on this device (`show`: bring it back).
/// Nothing in the workspace changes.
#[tauri::command]
pub(crate) fn setup_hide(state: State<'_, AppState>, key: String, show: Option<bool>) -> Result<(), String> {
    hidden::set(&state.data_dir, &key, show.unwrap_or(false)).map(|_| ()).map_err(|e| e.to_string())
}

/// Remove a host record of a former member, a revoked device or a box that
/// left (an Admin's, with a passkey; your own host needs none).
#[tauri::command]
pub(crate) async fn setup_remove_host(ligus_app: AppHandle, host_id: String) -> Result<(), String> {
    crate::off_main(ligus_app, move |app| {
        let state = app.state::<AppState>();
        let ws = state.active_workspace_id();
        let mut svc = state.service.lock().unwrap();
        let cfg = svc.load_in(ws, ligus_core::workspace_config_session_id(ws)).map_err(crate::map_err)?;
        if !cfg.is_some_and(|c| c.workspace_hosts.iter().any(|h| h.id == host_id)) {
            return Err("That host isn't in this workspace any more.".into());
        }
        svc.remove_host(ws, &host_id).map_err(crate::map_err)
    })
    .await
}

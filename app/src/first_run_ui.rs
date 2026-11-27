//! The first run's question ("Is this your first device with Ligus?") and
//! what follows from the answer, on the desktop.
//!
//! * [`set_first_run_path`] records the answer on this data dir: `first`
//!   (start here) or `link` (this device belongs to someone who already uses
//!   Ligus). The link branch never creates or joins anything.
//! * [`first_run_workspace`] makes the person's first workspace for the
//!   Secure step, once: running the first run again (a relaunch half-way
//!   through Secure) picks the same workspace up instead of founding another.
//! * [`invite_link_hint`] is the invite guard: one line on the join screen
//!   when an invite looks like it is for someone who already uses Ligus on
//!   another device. A hint only, never a refusal: invites stay for other
//!   people, and joining still works.

use ligus_proto::WorkspaceInfoDto;
use ligus_runtime::workspace_invite::{decode_workspace_invite, WorkspaceConn};
use tauri::State;
use uuid::Uuid;

use crate::{AppState, LiveSettings};

/// The two answers to the first run's question.
pub(crate) const PATH_FIRST: &str = "first";
pub(crate) const PATH_LINK: &str = "link";

/// Record the first run's answer on this data dir.
#[tauri::command]
pub(crate) fn set_first_run_path(state: State<AppState>, path: String) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    record_path(&mut s, &path)?;
    crate::save_settings(&state.data_dir, &s);
    Ok(())
}

pub(crate) fn record_path(s: &mut LiveSettings, path: &str) -> Result<(), String> {
    match path {
        PATH_FIRST | PATH_LINK => {
            s.first_run_path = Some(path.to_string());
            Ok(())
        }
        other => Err(format!("unknown first-run answer {other:?}: use first or link")),
    }
}

/// The person's first workspace, made once by the first run (and made
/// active). `name` names it when it is made; a workspace the first run made
/// before, still on this device, is reused as it is.
#[tauri::command]
pub(crate) async fn first_run_workspace(ligus_app: tauri::AppHandle, name: String) -> Result<WorkspaceInfoDto, String> {
    crate::off_main(ligus_app, move |ligus_app| first_run_workspace_sync(tauri::Manager::state::<AppState>(ligus_app), name)).await
}

pub(crate) fn first_run_workspace_sync(state: State<AppState>, name: String) -> Result<WorkspaceInfoDto, String> {
    let reuse = {
        let s = state.settings.lock().unwrap();
        if s.first_run_path.as_deref() == Some(PATH_LINK) {
            return Err("This device is set to link to your account, so it doesn't make a workspace.".into());
        }
        reusable_workspace(&s).cloned()
    };
    if let Some(conn) = reuse {
        crate::set_active_workspace_sync(state.clone(), conn.id().to_string())?;
        return Ok(WorkspaceInfoDto {
            id: conn.id().to_string(),
            name: crate::shown(&conn.display_name()),
            kind: "room".to_string(),
            active: true,
            icon_url: conn.icon.clone(),
        });
    }
    // No relay is asked here: the workspace syncs once one is added (the
    // first invite asks for it).
    let ws = crate::create_workspace_sync(state.clone(), name, None)?;
    let mut s = state.settings.lock().unwrap();
    s.first_run_workspace = Some(ws.id.clone());
    crate::save_settings(&state.data_dir, &s);
    Ok(ws)
}

/// The workspace an earlier first run made, while it is still on this device.
pub(crate) fn reusable_workspace(s: &LiveSettings) -> Option<&WorkspaceConn> {
    let id = s.first_run_workspace.as_deref().and_then(|i| Uuid::parse_str(i).ok())?;
    s.workspaces.iter().find(|w| w.id() == id && w.dm_account.is_none())
}

/// Whether the join screen should say "Looks like you already use Ligus.
/// Link this device instead." for `code` (an invite or a short code).
#[tauri::command]
pub(crate) async fn invite_link_hint(ligus_app: tauri::AppHandle, code: String) -> Result<bool, String> {
    crate::off_main(ligus_app, move |ligus_app| {
        let state = tauri::Manager::state::<AppState>(ligus_app);
        let me = state.service.lock().unwrap().author().id.clone();
        let linked = ligus_runtime::computer_link::LinkState::load(&state.data_dir).map(|l| l.is_linked()).unwrap_or(false);
        let s = state.settings.lock().unwrap();
        Ok(link_hint(&s, &code, &me, linked))
    })
    .await
}

/// The invite guard's signals, any of which shows the hint:
///
/// * this install's first run was answered "I already use Ligus";
/// * this install is already linked to someone's account;
/// * the invite is for a workspace this account founded, but which isn't on
///   this device: the person founded it on another of their devices.
///
/// An invite for a workspace already on this device shows no hint (joining
/// it again changes nothing), and nor does a direct-message invite.
pub(crate) fn link_hint(s: &LiveSettings, code: &str, me: &str, linked: bool) -> bool {
    let conn = decode_workspace_invite(code.trim()).ok();
    if let Some(c) = &conn {
        if c.dm_account.is_some() || s.workspaces.iter().any(|w| w.room == c.room) {
            return false;
        }
    }
    if linked || s.first_run_path.as_deref() == Some(PATH_LINK) {
        return true;
    }
    conn.and_then(|c| c.founder_actor_id).is_some_and(|f| !me.is_empty() && f == me)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ligus_runtime::workspace_invite::encode_workspace_invite;

    fn conn(room: &str, founder: Option<&str>) -> WorkspaceConn {
        WorkspaceConn {
            name: "Team".into(),
            relay_url: "https://relay.example".into(),
            room: room.into(),
            key: Some("k".into()),
            founder_actor_id: founder.map(Into::into),
            fk: None,
            icon: None,
            dm_account: None,
            inv: None,
            ek: Vec::new(),
            sig: None,
        }
    }

    fn settings() -> LiveSettings {
        LiveSettings::default()
    }

    #[test]
    fn only_the_two_answers_are_recorded() {
        let mut s = settings();
        assert_eq!(s.first_run_path, None);
        record_path(&mut s, "link").unwrap();
        assert_eq!(s.first_run_path.as_deref(), Some("link"));
        record_path(&mut s, "first").unwrap();
        assert_eq!(s.first_run_path.as_deref(), Some("first"), "changing your mind replaces the answer");
        assert!(record_path(&mut s, "join").is_err());
        assert_eq!(s.first_run_path.as_deref(), Some("first"));
    }

    #[test]
    fn the_first_workspace_is_reused_while_it_is_on_this_device() {
        let mut s = settings();
        assert!(reusable_workspace(&s).is_none(), "nothing made yet");
        let made = conn("room-1", Some("me"));
        s.workspaces.push(made.clone());
        s.first_run_workspace = Some(made.id().to_string());
        assert_eq!(reusable_workspace(&s).map(|w| w.room.as_str()), Some("room-1"));
        // Left since: the next first run makes a new one.
        s.workspaces.clear();
        assert!(reusable_workspace(&s).is_none());
        // A garbled record reuses nothing.
        s.workspaces.push(made);
        s.first_run_workspace = Some("not-a-uuid".into());
        assert!(reusable_workspace(&s).is_none());
    }

    #[test]
    fn the_invite_guard_hints_and_never_for_a_workspace_already_here() {
        let mut s = settings();
        let theirs = encode_workspace_invite(&conn("room-a", Some("ana")));
        let mine_elsewhere = encode_workspace_invite(&conn("room-b", Some("me")));
        // A fresh install answering "first device": no hint for someone else's invite.
        record_path(&mut s, "first").unwrap();
        assert!(!link_hint(&s, &theirs, "me", false));
        assert!(!link_hint(&s, "K7P2QX", "me", false), "a short code alone says nothing");
        // An invite to a workspace this account founded elsewhere.
        assert!(link_hint(&s, &mine_elsewhere, "me", false));
        assert!(!link_hint(&s, &mine_elsewhere, "", false), "no author yet: no match");
        // Linked already, or answered "I already use Ligus": any invite hints.
        assert!(link_hint(&s, &theirs, "me", true));
        assert!(link_hint(&s, "K7P2QX", "me", true));
        record_path(&mut s, "link").unwrap();
        assert!(link_hint(&s, &theirs, "me", false));
        assert!(link_hint(&s, "K7P2QX", "me", false));
        // Already on this device: joining again changes nothing, so no hint.
        s.workspaces.push(conn("room-a", Some("ana")));
        assert!(!link_hint(&s, &theirs, "me", true));
        // A direct-message invite is never a self-invite.
        let mut dm = conn("room-dm", Some("me"));
        dm.dm_account = Some("someone".into());
        assert!(!link_hint(&s, &encode_workspace_invite(&dm), "me", true));
    }
}

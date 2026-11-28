//! **Leave workspace** (workspace model v3, 10.4): write this person's own
//! `MemberLeft` (with their passkey when the account holds one), push it,
//! then forget the connection on this device. Under rules 3 the person stays
//! listed as "asked to leave" until an Admin removes them; under rules 4 the
//! leave removes them. Leaving isn't
//! revoking: other devices keep their keys. Where the workspace refuses the
//! leave (the founder, the last Owner, a linked computer), the desktop
//! offers what it did before: remove the workspace from this device only.

use ligus_runtime::chat_service::{LeaveCheck, NOT_REVOKING};
use tauri::{AppHandle, Manager};
use uuid::Uuid;

use crate::AppState;

/// What leaving would do, for the confirmation.
#[derive(serde::Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LeaveCheckDto {
    /// "ready" | "passkey" | "left" | "refused"
    state: &'static str,
    /// Why the workspace refuses it ("refused" only).
    refusal: Option<String>,
    /// The one-line consequence (on the workspace's rules).
    line: String,
    /// Leaving isn't revoking.
    reminder: &'static str,
}

fn dto(check: LeaveCheck, line: String) -> LeaveCheckDto {
    let (state, refusal) = match check {
        LeaveCheck::Ready => ("ready", None),
        LeaveCheck::NeedsPasskey => ("passkey", None),
        LeaveCheck::AlreadyLeft => ("left", None),
        LeaveCheck::Refused(why) => ("refused", Some(crate::shown(&why))),
    };
    LeaveCheckDto { state, refusal, line, reminder: NOT_REVOKING }
}

fn room_id(state: &AppState, workspace_id: &str) -> Result<Uuid, String> {
    let id = Uuid::parse_str(workspace_id).map_err(|e| e.to_string())?;
    if id == state.local_workspace_id {
        return Err("Your personal workspace can't be left.".into());
    }
    Ok(id)
}

/// Whether, and how, this person can leave `workspace_id` (nothing written).
#[tauri::command(async)]
pub(crate) async fn leave_workspace_check(ligus_app: AppHandle, workspace_id: String) -> Result<LeaveCheckDto, String> {
    crate::off_main(ligus_app, move |app| {
        let state = app.state::<AppState>();
        let ws = room_id(&state, &workspace_id)?;
        let name = {
            let s = state.settings.lock().unwrap();
            s.workspaces.iter().find(|w| w.id() == ws).map(|w| crate::shown(&w.display_name())).unwrap_or_else(|| "this workspace".into())
        };
        let mut svc = state.service.lock().unwrap();
        let check = svc.leave_check(ws).map_err(|e| e.to_string())?;
        let line = svc.leave_line(ws, &name).map_err(|e| e.to_string())?;
        Ok(dto(check, line))
    })
    .await
}

/// Leave `workspace_id`: write the leave, push it, then remove the workspace
/// from this device. A leave the workspace refuses changes nothing (the
/// caller offers "Remove from this device" instead). When the relay can't be
/// reached the leave stays written here and the workspace stays, so a retry
/// sends it.
#[tauri::command(async)]
pub(crate) async fn leave_workspace(ligus_app: AppHandle, workspace_id: String) -> Result<(), String> {
    let app = ligus_app.clone();
    let wid = workspace_id.clone();
    crate::off_main(ligus_app, move |app| {
        let state = app.state::<AppState>();
        let ws = room_id(&state, &wid)?;
        state.service.lock().unwrap().leave_workspace(ws).map_err(|e| crate::shown(&e.to_string()))?;
        Ok::<(), String>(())
    })
    .await?;
    let conn = {
        let state = app.state::<AppState>();
        let ws = room_id(&state, &workspace_id)?;
        let s = state.settings.lock().unwrap();
        s.workspaces.iter().find(|w| w.id() == ws).cloned()
    };
    if let Some(conn) = conn {
        let push = { crate::link_request_ui::sync_conn(&app.state::<AppState>(), &conn) };
        push.await.map_err(|e| format!("Your leave is saved on this device but didn't reach the relay ({e}). Try again when you're online."))?;
    }
    crate::off_main(app, move |app| crate::remove_workspace_sync(app.state::<AppState>(), workspace_id)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    use ligus_runtime::chat_service::{leave_consequence, LEAVE_CONSEQUENCE};

    #[test]
    fn every_check_carries_the_line_and_the_reminder() {
        for c in [LeaveCheck::Ready, LeaveCheck::NeedsPasskey, LeaveCheck::AlreadyLeft, LeaveCheck::Refused("Couldn't leave: you're the last Owner".into())] {
            let d = dto(c, LEAVE_CONSEQUENCE.into());
            assert_eq!(d.line, LEAVE_CONSEQUENCE);
            assert!(d.reminder.starts_with("Leaving isn't revoking"));
        }
        let line = || LEAVE_CONSEQUENCE.to_string();
        assert_eq!(dto(LeaveCheck::Refused("the founder stays".into()), line()).refusal.as_deref(), Some("the founder stays"));
        assert_eq!(dto(LeaveCheck::NeedsPasskey, line()).state, "passkey");
    }

    #[test]
    fn under_rules_4_the_confirm_says_you_will_be_removed() {
        let d = dto(LeaveCheck::Ready, leave_consequence(ligus_core::authz::RULES_V4, "Garden"));
        assert_eq!(d.line, "You'll be removed from Garden, and this device stops syncing it.");
        let d = dto(LeaveCheck::Ready, leave_consequence(ligus_core::authz::RULES_V3, "Garden"));
        assert_eq!(d.line, LEAVE_CONSEQUENCE);
    }
}

//! "Add this phone" on the desktop: certify the phone's P-256 Secure Enclave
//! key as one of your devices, from the code its app shows
//! (`hive_runtime::phone_enroll`).
//!
//! The person pastes (or scans) the phone's `ligusphone1:` code, compares the
//! fingerprint with the phone's screen, and confirms with their passkey: the
//! certificate is a P-256 one (the phone tier, capped by their role), and the
//! rules require their presence for every phone enrollment, so the desktop's
//! presence sheet opens while `certify_phone_code` runs.

use hive_runtime::phone_enroll::{PhoneCode, PhoneStanding};
use tauri::State;

use crate::AppState;

/// What a phone code asks for, shown before the person confirms.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PhoneCodeDto {
    /// The phone's workspace device id, as the phone presents it.
    device_id: String,
    label: String,
    /// Compare with the phone's screen (Settings > This phone).
    fingerprint: String,
    /// `new`, `alreadyMine`, `revoked` or `conflict`.
    standing: String,
    /// Why it can't be certified (`conflict`, `revoked`).
    problem: Option<String>,
    /// The workspace it would join as your phone.
    workspace_id: String,
}

fn decode(code: &str) -> Result<PhoneCode, String> {
    PhoneCode::decode(code).map_err(|e| e.to_string())
}

fn standing_dto(s: PhoneStanding) -> (String, Option<String>) {
    match s {
        PhoneStanding::New => ("new".into(), None),
        PhoneStanding::AlreadyMine => ("alreadyMine".into(), None),
        PhoneStanding::Revoked => (
            "revoked".into(),
            Some("This phone was revoked here. In the app, reset its device key and use the new code.".into()),
        ),
        PhoneStanding::Conflict(why) => ("conflict".into(), Some(why)),
    }
}

/// Check a phone code (signature, key, where the device stands) without
/// writing anything.
#[tauri::command]
pub(crate) fn inspect_phone_code(state: State<AppState>, code: String) -> Result<PhoneCodeDto, String> {
    let c = decode(&code)?;
    let ws = state.active_workspace_id();
    let svc = state.service.lock().unwrap();
    let (standing, problem) = standing_dto(svc.phone_standing(ws, &c).map_err(|e| e.to_string())?);
    Ok(PhoneCodeDto {
        device_id: c.device_id.to_string(),
        label: c.label.clone(),
        fingerprint: c.fingerprint(),
        standing,
        problem,
        workspace_id: ws.to_string(),
    })
}

/// Certify the phone in the active workspace. Blocks on the person's passkey
/// (the presence sheet); refused without it.
#[tauri::command(async)]
pub(crate) fn certify_phone_code(state: State<AppState>, code: String, fingerprint: String) -> Result<String, String> {
    let c = decode(&code)?;
    // The sheet showed a fingerprint; certify only the key it showed.
    if !fingerprint.trim().eq_ignore_ascii_case(&c.fingerprint()) {
        return Err("The code changed since you compared its fingerprint. Check it again.".into());
    }
    let ws = state.active_workspace_id();
    let mut svc = state.service.lock().unwrap();
    let id = svc.certify_phone_code(ws, &c).map_err(|e| e.to_string())?;
    tracing::info!(target: "identity", device = %id, label = %c.label, "certified a phone as this account's");
    Ok(id.to_string())
}

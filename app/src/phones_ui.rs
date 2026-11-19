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

/// Refuse to certify anything but what the sheet showed: the key whose
/// fingerprint the person compared, into the workspace the preview was for
/// (the active one can change between the preview and the passkey).
fn confirm_matches_preview(
    c: &PhoneCode,
    fingerprint: &str,
    previewed_ws: &str,
    active_ws: uuid::Uuid,
) -> Result<(), String> {
    if !fingerprint.trim().eq_ignore_ascii_case(&c.fingerprint()) {
        return Err("The code changed since you compared its fingerprint. Check it again.".into());
    }
    if uuid::Uuid::parse_str(previewed_ws.trim()).ok() != Some(active_ws) {
        return Err("The workspace changed since you checked this code. Check it again in the workspace you want the phone in.".into());
    }
    Ok(())
}

/// Certify the phone in the workspace its preview was for, which must still
/// be the active one. Blocks on the person's passkey (the presence sheet);
/// refused without it.
#[tauri::command(async)]
pub(crate) fn certify_phone_code(
    state: State<AppState>,
    code: String,
    fingerprint: String,
    workspace_id: String,
) -> Result<String, String> {
    let c = decode(&code)?;
    let ws = state.active_workspace_id();
    confirm_matches_preview(&c, &fingerprint, &workspace_id, ws)?;
    let mut svc = state.service.lock().unwrap();
    let id = svc.certify_phone_code(ws, &c).map_err(|e| e.to_string())?;
    tracing::info!(target: "identity", device = %id, label = %c.label, "certified a phone as this account's");
    Ok(id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Only the key's fingerprint matters here, not the signature.
    fn code() -> PhoneCode {
        let key: Vec<u8> = std::iter::once(4u8).chain((0..64).map(|i| i as u8)).collect();
        PhoneCode::new_signed(uuid::Uuid::new_v4(), key, "Ana's iPhone", |_| [0u8; 64])
    }

    #[test]
    fn certify_refuses_another_workspace_than_the_preview() {
        let c = code();
        let (shown, active) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        assert!(confirm_matches_preview(&c, &c.fingerprint(), &shown.to_string(), shown).is_ok());
        let err = confirm_matches_preview(&c, &c.fingerprint(), &shown.to_string(), active).unwrap_err();
        assert!(err.contains("workspace changed"), "{err}");
        assert!(confirm_matches_preview(&c, &c.fingerprint(), "not-a-uuid", active).is_err());
    }

    #[test]
    fn certify_refuses_another_fingerprint() {
        let c = code();
        let ws = uuid::Uuid::new_v4();
        assert!(confirm_matches_preview(&c, &c.fingerprint().to_lowercase(), &ws.to_string(), ws).is_ok());
        assert!(confirm_matches_preview(&c, "0000-0000-0000-0000-0000", &ws.to_string(), ws).is_err());
    }
}

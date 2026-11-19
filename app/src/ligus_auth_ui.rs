//! Desktop side of langstroth-auth: device-code sign-in from Settings, and a
//! background refresher that keeps the relay access token fresh.
//!
//! The session (with its device-bound refresh token) is saved as
//! `<app data>/ligus-auth.json` (0600). While it is valid, its short-lived
//! access token is used for every relay call instead of the static
//! "Relay access token" setting. Signing in grants relay access only; a
//! workspace Admin still has to add the person as a member.

use std::sync::{Arc, LazyLock, Mutex, Once};

use ligus_runtime::ligus_auth::{self, DeviceKey, DeviceSigner, Poll, TokenCell};
use tauri::State;

use crate::AppState;

const CLIENT_ID: &str = "ligus-desktop";

static CELL: LazyLock<TokenCell> = LazyLock::new(TokenCell::default);
static PENDING: Mutex<Option<Pending>> = Mutex::new(None);
static STARTED: Once = Once::new();

/// A device-code sign-in in progress. The device code stays in the backend.
struct Pending {
    auth_url: String,
    device_code: String,
}

/// The relay bearer to use: the langstroth-auth access token when signed in,
/// otherwise the static token from settings.
/// The langstroth-auth token goes only to the relay its session was signed in
/// for (E-5); any other relay gets the static token.
pub(crate) fn effective(relay_url: Option<&str>, static_token: Option<String>) -> Option<String> {
    relay_url.and_then(|u| CELL.get_for(u)).filter(|t| !t.is_empty()).or(static_token)
}

fn signer(state: &AppState) -> Result<Arc<dyn DeviceSigner>, String> {
    let device_id = state.service.lock().unwrap().device_id();
    let kp = state
        .identity
        .device_keypair(device_id)
        .map_err(|e| e.to_string())?;
    Ok(Arc::new(DeviceKey::new(device_id, kp)))
}

/// Start the background refresher (once per process). It idles cheaply while
/// there is no session and picks one up as soon as the person signs in.
pub(crate) fn start(state: &AppState) {
    STARTED.call_once(|| match signer(state) {
        Ok(s) => {
            let dir = state.data_dir.clone();
            tauri::async_runtime::spawn(ligus_auth::run_refresher(dir, s, CELL.clone()));
        }
        Err(e) => tracing::warn!("langstroth-auth: no device key for the refresher: {e}"),
    });
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LigusAuthStatusDto {
    auth_url: String,
    tenant: String,
    sub: String,
    workspaces: Vec<String>,
    access_expires_at: i64,
    /// The access token is loaded and in use for relay calls.
    active: bool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LigusAuthStartDto {
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    interval: u64,
    expires_in: u64,
}

#[tauri::command]
pub fn ligus_auth_status(state: State<'_, AppState>) -> Option<LigusAuthStatusDto> {
    let s = ligus_auth::load_session(&state.data_dir)?;
    Some(LigusAuthStatusDto {
        auth_url: s.auth_url,
        tenant: s.tenant,
        sub: s.sub,
        workspaces: s.workspaces,
        access_expires_at: s.access_expires_at,
        active: CELL.get().is_some(),
    })
}

#[tauri::command]
pub async fn ligus_auth_start(
    state: State<'_, AppState>,
    url: String,
) -> Result<LigusAuthStartDto, String> {
    start(&state);
    let (base, _) = ligus_auth::parse_auth_url(&url).map_err(|e| e.to_string())?;
    let label = state.device_name.clone();
    let ds = ligus_auth::start_device(&ligus_auth::http_client(), &base, CLIENT_ID, &label)
        .await
        .map_err(|e| e.to_string())?;
    *PENDING.lock().unwrap() = Some(Pending {
        auth_url: base,
        device_code: ds.device_code.clone(),
    });
    Ok(LigusAuthStartDto {
        user_code: ds.user_code,
        verification_uri: ds.verification_uri,
        verification_uri_complete: ds.verification_uri_complete,
        interval: ds.interval,
        expires_in: ds.expires_in,
    })
}

/// One poll: "pending" | "slowDown" | "denied" | "expired" | "success" | "none".
#[tauri::command]
pub async fn ligus_auth_poll(state: State<'_, AppState>) -> Result<String, String> {
    let Some((url, code)) = PENDING
        .lock()
        .unwrap()
        .as_ref()
        .map(|p| (p.auth_url.clone(), p.device_code.clone()))
    else {
        return Ok("none".into());
    };
    let signer = signer(&state)?;
    let outcome = ligus_auth::poll_device(
        &ligus_auth::http_client(),
        &url,
        CLIENT_ID,
        &code,
        signer.as_ref(),
    )
    .await
    .map_err(|e| e.to_string())?;
    let status = match outcome {
        Poll::Pending => "pending",
        Poll::SlowDown => "slowDown",
        Poll::Denied => "denied",
        Poll::Expired => "expired",
        Poll::SignedIn(s) => {
            // Bound to the relay configured now (E-5).
            let mut s = *s;
            s.relay_origin = state.settings.lock().unwrap().relay_url.as_deref().and_then(ligus_auth::origin_of);
            ligus_auth::save_session(&state.data_dir, &s).map_err(|e| e.to_string())?;
            CELL.set_session(&s);
            "success"
        }
    };
    if status != "pending" && status != "slowDown" {
        *PENDING.lock().unwrap() = None;
    }
    Ok(status.into())
}

#[tauri::command]
pub async fn ligus_auth_sign_out(state: State<'_, AppState>) -> Result<(), String> {
    *PENDING.lock().unwrap() = None;
    ligus_auth::sign_out(&ligus_auth::http_client(), &state.data_dir).await;
    CELL.set(None);
    Ok(())
}

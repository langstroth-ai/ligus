//! Desktop passkeys (Phase 0, B3): the system-browser hand-off.
//!
//! WebAuthn in the Tauri webview is unreliable, so every OS uses one path:
//! the desktop listens on `127.0.0.1:<random>` with a one-time nonce and opens
//! `https://ligus.ai/presence#c=<challenge>&p=<port>&n=<nonce>&op=get|create…`.
//! That static page (strict CSP, no third-party script) runs
//! `navigator.credentials.get/create` with `rpId: "ligus.ai"` and
//! `userVerification: "required"`, then submits the result to the loopback
//! (a top-level form POST, so no CORS or mixed-content rules apply). The
//! desktop checks the returned challenge is its own before using it, and the
//! fold verifies the assertion again on every peer.
//!
//! The confirmation sheet (`presence://requested`) shows exactly what is
//! being authorized (kind, summary, digest short id); Cancel stops the wait.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hive_core::presence::{self, b64url, unb64url, PasskeyCredential, PresenceAssertion, RecoveryKey};
use hive_runtime::chat_service::{PresenceProvider, PresenceRequest};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::AppState;

/// Where the hand-off page lives (the ligus.ai site repo serves it).
pub const PRESENCE_PAGE: &str = "https://ligus.ai/presence";
/// How long the desktop waits for the browser.
const WAIT: Duration = Duration::from_secs(180);

static CANCEL: AtomicBool = AtomicBool::new(false);
/// A recovery key the person entered for the pending `get` (its 24 words),
/// used once instead of the browser and dropped right after.
static RECOVERY: std::sync::Mutex<Option<RecoveryKey>> = std::sync::Mutex::new(None);

/// What answered a ceremony.
enum Outcome {
    Browser(HandOff),
    Recovery(RecoveryKey),
}

fn take_recovery() -> Option<RecoveryKey> {
    RECOVERY.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// What the confirmation sheet shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresencePrompt {
    pub kind: String,
    pub summary: String,
    pub digest_short: String,
    pub op: String,
}

/// (The page shows the digest short id it computes from `c` itself, never
/// one from the link, so it is not passed.)
///
/// The browser's result, as the page submits it.
#[derive(Debug, Default)]
struct HandOff {
    credential_id: String,
    authenticator_data: String,
    client_data_json: String,
    signature: String,
    attestation_object: String,
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn urldecode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    out.push(v);
                    i += 2;
                } else {
                    out.push(b'%');
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Run one ceremony in the system browser and wait for its result.
fn hand_off(app: &AppHandle, prompt: PresencePrompt, fragment: String) -> Result<Outcome, String> {
    let recovery_ok = prompt.op == "get";
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    // A v4 UUID is 122 random bits from the OS generator.
    let nonce = b64url(uuid::Uuid::new_v4().as_bytes());
    let url = format!("{PRESENCE_PAGE}#{fragment}&p={port}&n={nonce}");
    CANCEL.store(false, Ordering::SeqCst);
    let _ = take_recovery();
    let _ = app.emit("presence://requested", &prompt);
    crate::open_url_in_browser(&url).map_err(|e| format!("couldn't open the browser: {e}"))?;
    let deadline = Instant::now() + WAIT;
    let result = loop {
        if CANCEL.load(Ordering::SeqCst) {
            break Err("cancelled".to_string());
        }
        if Instant::now() > deadline {
            break Err("timed out waiting for the passkey".to_string());
        }
        if recovery_ok {
            if let Some(k) = take_recovery() {
                break Ok(Outcome::Recovery(k));
            }
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(r) = read_submission(stream, &nonce) {
                    break Ok(Outcome::Browser(r));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => break Err(e.to_string()),
        }
    };
    let _ = app.emit("presence://done", result.is_ok());
    result
}

/// One request to the loopback: `POST /<nonce>` with a form body. Anything
/// else (a wrong nonce, a GET) is answered 404 and ignored.
fn read_submission(mut stream: std::net::TcpStream, nonce: &str) -> Option<HandOff> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let ok_path = format!("POST /{nonce} ");
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).ok()? == 0 || h == "\r\n" || h == "\n" {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    if !line.starts_with(&ok_path) || len == 0 || len > 64 * 1024 {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return None;
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).ok()?;
    let page = "<!doctype html><meta charset=utf-8><title>Ligus</title><p style=\"font-family:system-ui\">Done. You can close this tab and return to Ligus.</p>";
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
    let mut r = HandOff::default();
    for pair in String::from_utf8_lossy(&body).split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = urldecode(v);
        match k {
            "credentialId" => r.credential_id = v,
            "authenticatorData" => r.authenticator_data = v,
            "clientDataJSON" => r.client_data_json = v,
            "signature" => r.signature = v,
            "attestationObject" => r.attestation_object = v,
            _ => {}
        }
    }
    Some(r)
}

/// The challenge a returned `clientDataJSON` names.
fn returned_challenge(client_data_b64: &str) -> Option<Vec<u8>> {
    let raw = unb64url(client_data_b64).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    unb64url(v.get("challenge")?.as_str()?).ok()
}

/// `navigator.credentials.get` over `challenge` in the browser.
pub fn browser_get(app: &AppHandle, req: &PresenceRequest) -> Result<PresenceAssertion, String> {
    let fragment = format!(
        "op=get&c={}&cred={}&k={}&s={}",
        b64url(&req.challenge),
        urlencode(&req.credential_ids.join(",")),
        urlencode(&req.kind),
        urlencode(&req.summary),
    );
    let prompt = PresencePrompt { kind: req.kind.clone(), summary: req.summary.clone(), digest_short: req.digest_short.clone(), op: "get".into() };
    let r = match hand_off(app, prompt, fragment)? {
        Outcome::Browser(r) => r,
        // The recovery key signs the very challenge requested, here.
        Outcome::Recovery(k) => return Ok(k.assert(&req.challenge)),
    };
    if returned_challenge(&r.client_data_json).as_deref() != Some(&req.challenge[..]) {
        return Err("the browser returned an assertion for a different challenge".into());
    }
    Ok(PresenceAssertion {
        credential_id: r.credential_id,
        authenticator_data: r.authenticator_data,
        client_data_json: r.client_data_json,
        signature: r.signature,
    })
}

/// `navigator.credentials.create` over `challenge`: the new credential and
/// its proof (the attestation object rides in `authenticator_data`).
fn browser_create(app: &AppHandle, req: &PresenceRequest, user_name: &str) -> Result<PresenceAssertion, String> {
    let fragment = format!(
        "op=create&c={}&u={}&un={}&x={}",
        b64url(&req.challenge),
        b64url(req.account_id.as_bytes()),
        urlencode(user_name),
        urlencode(&req.credential_ids.join(",")),
    );
    let prompt = PresencePrompt {
        kind: "passkeyRegistered".into(),
        summary: "Create a Ligus passkey".into(),
        digest_short: req.digest_short.clone(),
        op: "create".into(),
    };
    let Outcome::Browser(r) = hand_off(app, prompt, fragment)? else {
        return Err("a recovery key doesn't create a passkey".into());
    };
    if returned_challenge(&r.client_data_json).as_deref() != Some(&req.challenge[..]) {
        return Err("the browser returned a credential for a different challenge".into());
    }
    Ok(PresenceAssertion {
        credential_id: r.credential_id,
        authenticator_data: r.attestation_object,
        client_data_json: r.client_data_json,
        signature: String::new(),
    })
}

/// The desktop's presence provider: the browser hand-off.
pub struct DesktopPresence(pub AppHandle);

impl PresenceProvider for DesktopPresence {
    fn assert(&self, req: &PresenceRequest) -> Result<PresenceAssertion, String> {
        browser_get(&self.0, req)
    }
}

// ── Commands ────────────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyDto {
    credential_id: String,
    recovery: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecurityStatusDto {
    passkeys: Vec<PasskeyDto>,
    rules: u32,
    key_version: u32,
    secret_backend: String,
}

/// Passkeys of this account on the active workspace, the rules version and
/// the key version.
#[tauri::command]
pub fn security_status(state: State<'_, AppState>) -> Result<SecurityStatusDto, String> {
    let ws = state.active_workspace_id();
    let svc = state.service.lock().unwrap();
    let account = svc.author().account_id;
    let passkeys = account
        .map(|a| svc.passkeys_of(ws, a).unwrap_or_default())
        .unwrap_or_default()
        .into_iter()
        .map(|(credential_id, recovery)| PasskeyDto { credential_id, recovery })
        .collect();
    let cfg = svc.load(hive_core::workspace_config_session_id(ws)).ok().flatten();
    let rules = cfg.as_ref().map(|c| c.authz.rules).unwrap_or(1);
    let (_, timeline) = svc.store().load_config(ws, None, None).map_err(|e| e.to_string())?;
    Ok(SecurityStatusDto {
        passkeys,
        rules,
        key_version: timeline.current().key_epochs.head_version(),
        secret_backend: hive_runtime::secret_store::for_data_dir(&state.data_dir).backend().as_str().into(),
    })
}

/// Register a passkey on the active workspace: one browser ceremony creates
/// the key (`navigator.credentials.create`, attestation "none"), a second one
/// signs the registration's digest (with the new key for the account's
/// first, with an existing one after that).
#[tauri::command]
pub async fn passkey_register(app: AppHandle, label: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let ws = state.active_workspace_id();
        let (account, name, existing) = {
            let svc = state.service.lock().unwrap();
            let account = svc.author().account_id.ok_or("this device has no account")?;
            let existing: Vec<String> = svc.passkeys_of(ws, account).map_err(|e| e.to_string())?.into_iter().map(|(id, _)| id).collect();
            (account, svc.author().display_name.clone(), existing)
        };
        // 1. Create the key (its challenge only binds this ceremony).
        let mut challenge = [0u8; 32];
        challenge[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        challenge[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        let create_req = PresenceRequest {
            challenge,
            kind: "passkeyRegistered".into(),
            account_id: account,
            credential_ids: existing.clone(),
            digest_short: hive_core::keys::hex(&challenge[..8]),
            summary: "Create a Ligus passkey".into(),
        };
        let att = browser_create(&app, &create_req, &name)?;
        let (id, alg, pk) = presence::verify_create(&att, &challenge, &presence::Policy::ligus()).map_err(|e| e.0)?;
        let new_id = b64url(&id);
        let credential = PasskeyCredential {
            account_id: account,
            credential_id: new_id.clone(),
            alg,
            public_key: pk,
            label: label.trim().chars().take(64).collect(),
            recovery: false,
            registered_at: hive_core::Timestamp::now(),
        };
        // 2. Sign the registration itself.
        let first = existing.is_empty();
        let app2 = app.clone();
        let mut svc = state.service.lock().unwrap();
        svc.register_passkey(ws, credential, &move |req| {
            let mut req = req.clone();
            if first {
                req.credential_ids = vec![new_id.clone()];
            }
            browser_get(&app2, &req)
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Create a recovery key on the active workspace: the 24 words are returned
/// once and never stored. Needs a confirmation with an existing passkey.
#[tauri::command]
pub async fn recovery_key_create(app: AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let ws = state.active_workspace_id();
        let mut svc = state.service.lock().unwrap();
        let account = svc.author().account_id.ok_or("this device has no account")?;
        if svc.passkeys_of(ws, account).map_err(|e| e.to_string())?.is_empty() {
            return Err("register a passkey first".to_string());
        }
        let key = RecoveryKey::generate().map_err(|e| e.0)?;
        let app2 = app.clone();
        svc.register_passkey(ws, key.credential(account, "Recovery key"), &move |req| browser_get(&app2, req))
            .map_err(|e| e.to_string())?;
        Ok(key.words())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Turn on (or set) the workspace rules version; v2 needs a passkey and a
/// recovery key first, and the switch itself is confirmed with a passkey.
#[tauri::command]
pub async fn workspace_rules_set(app: AppHandle, version: u32) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let ws = state.active_workspace_id();
        let mut svc = state.service.lock().unwrap();
        svc.set_workspace_rules(ws, version).map(|_| ()).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Remove one of this account's passkeys (or the recovery key) on the
/// active workspace.
#[tauri::command]
pub async fn passkey_remove(app: AppHandle, credential_id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let ws = state.active_workspace_id();
        let mut svc = state.service.lock().unwrap();
        let account = svc.author().account_id.ok_or("this device has no account")?;
        svc.remove_passkey(ws, account, &credential_id).map(|_| ()).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Let member `member_id` of the active workspace register a first passkey
/// again (rules v2): an Admin's re-grant of their role, confirmed with this
/// account's passkey.
#[tauri::command]
pub async fn passkey_regrant(app: AppHandle, member_id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let ws = state.active_workspace_id();
        let mut svc = state.service.lock().unwrap();
        svc.regrant_passkey(ws, &member_id).map(|_| ()).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Stop waiting for the browser.
#[tauri::command]
pub fn presence_cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

/// Confirm the pending request with the recovery key's 24 words instead of
/// a passkey (lost every passkey). The words are parsed here (checksum
/// verified), used for that one assertion and never stored; the fold accepts
/// it only if that recovery key is registered to this account.
#[tauri::command]
pub fn presence_use_recovery(words: String) -> Result<(), String> {
    let key = RecoveryKey::from_words(&words).map_err(|e| e.0)?;
    *RECOVERY.lock().unwrap_or_else(|e| e.into_inner()) = Some(key);
    Ok(())
}

/// Install the browser hand-off as the desktop's presence provider.
pub fn install(state: &AppState, app: &AppHandle) {
    state.service.lock().unwrap().set_presence_provider(Some(Arc::new(DesktopPresence(app.clone()))));
}

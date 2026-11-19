//! Desktop passkeys (Phase 0, B3; native ceremonies Phase D).
//!
//! On macOS and Windows a ceremony first runs natively
//! ([`crate::passkey_native`]: AuthenticationServices, `webauthn.dll`) when
//! the platform serves the workspace's RP. Everywhere else, and as the
//! fallback, the system-browser hand-off runs (WebAuthn in the Tauri webview
//! is unreliable). For the hand-off the desktop listens on
//! `127.0.0.1:<random>` with a one-time nonce and opens
//! `<workspace origin>/presence#rp=<rp id>&c=<challenge>&p=<port>&n=<nonce>&op=get|create…`
//! (`https://ligus.ai/presence` for the owner's deployment), through a
//! private redirect page under the data dir, so the nonce is never in a
//! command line.
//! That static page (strict CSP, no third-party script) runs
//! `navigator.credentials.get/create` with the workspace's `rpId` and
//! `userVerification: "required"`, then submits the result to the loopback
//! (a top-level form POST, so no CORS or mixed-content rules apply). The
//! desktop checks the returned challenge is its own before using it, and the
//! fold verifies the assertion again on every peer.
//!
//! The confirmation sheet (`presence://requested`) shows exactly what is
//! being authorized (kind, summary, digest short id); Cancel stops the wait.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ligus_core::presence::{self, b64url, returned_challenge, PasskeyCredential, PresenceAssertion, RecoveryKey};
use ligus_runtime::chat_service::{PresenceProvider, PresenceRequest};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::AppState;

/// How long the desktop waits for the browser.
const WAIT: Duration = Duration::from_secs(180);

/// The confirmation sheet's choices for the pending ceremony (Cancel, the
/// browser instead of the system dialog, and a recovery key the person
/// entered for a `get`: its 24 words, used once and dropped right after).
static SIGNALS: crate::passkey_native::Signals = crate::passkey_native::Signals::new();

/// What answered a ceremony.
enum Outcome {
    Browser(HandOff),
    Recovery(RecoveryKey),
}

/// What the confirmation sheet shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresencePrompt {
    pub kind: String,
    pub summary: String,
    pub digest_short: String,
    pub op: String,
    /// `browser` (the hand-off page is open), `native` (the system passkey
    /// dialog is up) or `dismissed` (the person closed it; the sheet offers
    /// the browser, the recovery key and Cancel).
    pub via: String,
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
fn hand_off(app: &AppHandle, page_url: &str, prompt: PresencePrompt, fragment: String) -> Result<Outcome, String> {
    // A `get` can be answered by the recovery key the person enters for
    // this request (bound to the digest the sheet shows).
    let recovery_for = (prompt.op == "get").then(|| prompt.digest_short.clone());
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    // A v4 UUID is 122 random bits from the OS generator.
    let nonce = b64url(uuid::Uuid::new_v4().as_bytes());
    let url = format!("{page_url}#{fragment}&p={port}&n={nonce}");
    SIGNALS.reset();
    let _ = app.emit("presence://requested", &PresencePrompt { via: "browser".into(), ..prompt });
    // The nonce goes through a private redirect page, never the command line.
    let dir = app.state::<AppState>().data_dir.join("presence");
    let page = redirect_page(&dir, &url).map_err(|e| format!("couldn't prepare the passkey page: {e}"))?;
    let opened = crate::open_url_in_browser(&page.to_string_lossy());
    if let Err(e) = opened {
        let _ = std::fs::remove_file(&page);
        return Err(format!("couldn't open the browser: {e}"));
    }
    let deadline = Instant::now() + WAIT;
    let result = loop {
        if SIGNALS.cancelled() {
            break Err("cancelled".to_string());
        }
        if Instant::now() > deadline {
            break Err("timed out waiting for the passkey".to_string());
        }
        if let Some(digest) = recovery_for.as_deref() {
            if let Some(k) = SIGNALS.take_recovery(digest) {
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
    let _ = std::fs::remove_file(&page);
    let _ = app.emit("presence://done", result.is_ok());
    result
}

/// One request to the loopback: `POST /<nonce>` with a form body. Anything
/// else (a wrong nonce, a GET) is answered 404 and ignored.
fn read_submission(mut stream: std::net::TcpStream, nonce: &str) -> Option<HandOff> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let body = match parse_submission(stream.try_clone().ok()?, nonce) {
        Ok(body) => body,
        Err(()) => {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            return None;
        }
    };
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

/// Caps on what the loopback reads from one connection (B-12): a line (the
/// request line or one header), the number of headers, the whole header
/// block, and the form body.
const MAX_LINE: u64 = 4 * 1024;
const MAX_HEADERS: usize = 64;
const MAX_HEAD: u64 = 16 * 1024;
const MAX_BODY: usize = 64 * 1024;

/// One line of at most [`MAX_LINE`] bytes, ending in `\n`.
fn capped_line(reader: &mut impl BufRead) -> Result<String, ()> {
    let mut buf = Vec::new();
    reader.by_ref().take(MAX_LINE).read_until(b'\n', &mut buf).map_err(|_| ())?;
    if buf.last() != Some(&b'\n') {
        return Err(()); // too long, or the connection ended mid-line
    }
    String::from_utf8(buf).map_err(|_| ())
}

/// The form body of a `POST /<nonce>` request read from `stream`, reading at
/// most [`MAX_HEAD`] + [`MAX_BODY`] bytes whatever the client sends.
fn parse_submission(stream: impl Read, nonce: &str) -> Result<Vec<u8>, ()> {
    let mut reader = BufReader::new(stream.take(MAX_HEAD + MAX_BODY as u64));
    let line = capped_line(&mut reader)?;
    let ok_path = format!("POST /{nonce} ");
    let mut len = 0usize;
    let mut head = line.len() as u64;
    let mut ended = false;
    for _ in 0..MAX_HEADERS {
        let h = capped_line(&mut reader)?;
        head += h.len() as u64;
        if head > MAX_HEAD {
            return Err(());
        }
        if h == "\r\n" || h == "\n" {
            ended = true;
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    if !ended || !line.starts_with(&ok_path) || len == 0 || len > MAX_BODY {
        return Err(());
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).map_err(|_| ())?;
    Ok(body)
}

/// Where the browser is pointed (B-12): a one-off redirect page in a private
/// directory under the data dir (which agent sandboxes can't read), so the
/// loopback nonce never appears in the `open` command's arguments, which
/// every local process can list. The page forwards to `url` (fragment and
/// all) and is deleted when the ceremony ends.
fn redirect_page(dir: &std::path::Path, url: &str) -> std::io::Result<std::path::PathBuf> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let target = serde_json::to_string(url).map_err(std::io::Error::other)?;
    let attr = url.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;");
    let page = format!(
        "<!doctype html><meta charset=utf-8><meta name=referrer content=no-referrer>\
         <meta http-equiv=refresh content=\"0;url={attr}\"><title>Ligus</title>\
         <script>location.replace({target})</script>"
    );
    let path = dir.join(format!("presence-{}.html", uuid::Uuid::new_v4().simple()));
    ligus_runtime::secret_store::write_private_atomic(&path, page.as_bytes())?;
    Ok(path)
}

/// The sheet's view of a native ceremony.
struct SheetEvents<'a> {
    app: &'a AppHandle,
    prompt: PresencePrompt,
}

impl crate::passkey_native::Events for SheetEvents<'_> {
    fn stage(&self, stage: crate::passkey_native::Stage) {
        let via = match stage {
            crate::passkey_native::Stage::Native => "native",
            crate::passkey_native::Stage::Dismissed => "dismissed",
        };
        let _ = self.app.emit("presence://requested", &PresencePrompt { via: via.into(), ..self.prompt.clone() });
    }
    fn done(&self, ok: bool) {
        let _ = self.app.emit("presence://done", ok);
    }
}

/// A presence `get` over `req.challenge`: natively when this OS serves the
/// workspace's RP, else (and on any fallback) through the browser.
pub fn presence_get(app: &AppHandle, req: &PresenceRequest) -> Result<PresenceAssertion, String> {
    let prompt = PresencePrompt { kind: req.kind.clone(), summary: req.summary.clone(), digest_short: req.digest_short.clone(), op: "get".into(), via: String::new() };
    let events = SheetEvents { app, prompt };
    crate::passkey_native::run_get(crate::passkey_native::platform(app), req, &SIGNALS, WAIT, &events, || browser_get(app, req))
}

/// A `create` for a new passkey, natively when this OS serves the
/// workspace's RP, else through the browser.
fn presence_create(app: &AppHandle, req: &PresenceRequest, user_id: &[u8], user_name: &str) -> Result<PresenceAssertion, String> {
    let prompt = PresencePrompt {
        kind: "passkeyRegistered".into(),
        summary: "Create a Ligus passkey".into(),
        digest_short: req.digest_short.clone(),
        op: "create".into(),
        via: String::new(),
    };
    let events = SheetEvents { app, prompt };
    crate::passkey_native::run_create(crate::passkey_native::platform(app), req, user_id, user_name, &SIGNALS, WAIT, &events, || browser_create(app, req, user_id, user_name))
}

/// `navigator.credentials.get` over `challenge` in the browser.
fn browser_get(app: &AppHandle, req: &PresenceRequest) -> Result<PresenceAssertion, String> {
    let fragment = format!(
        "op=get&rp={}&c={}&cred={}&k={}&s={}",
        urlencode(&req.policy.rp_id),
        b64url(&req.challenge),
        urlencode(&req.credential_ids.join(",")),
        urlencode(&req.kind),
        urlencode(&req.summary),
    );
    let prompt = PresencePrompt { kind: req.kind.clone(), summary: req.summary.clone(), digest_short: req.digest_short.clone(), op: "get".into(), via: "browser".into() };
    let r = match hand_off(app, &req.policy.page_url(), prompt, fragment)? {
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
fn browser_create(app: &AppHandle, req: &PresenceRequest, user_id: &[u8], user_name: &str) -> Result<PresenceAssertion, String> {
    let fragment = format!(
        "op=create&rp={}&c={}&u={}&un={}&x={}",
        urlencode(&req.policy.rp_id),
        b64url(&req.challenge),
        b64url(user_id),
        urlencode(user_name),
        urlencode(&req.credential_ids.join(",")),
    );
    let prompt = PresencePrompt {
        kind: "passkeyRegistered".into(),
        summary: "Create a Ligus passkey".into(),
        digest_short: req.digest_short.clone(),
        op: "create".into(),
        via: "browser".into(),
    };
    let Outcome::Browser(r) = hand_off(app, &req.policy.page_url(), prompt, fragment)? else {
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

/// The desktop's presence provider: native passkeys where the OS serves the
/// workspace's RP, the browser hand-off otherwise.
pub struct DesktopPresence(pub AppHandle);

impl PresenceProvider for DesktopPresence {
    fn assert(&self, req: &PresenceRequest) -> Result<PresenceAssertion, String> {
        presence_get(&self.0, req)
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
    /// This device's certificate is in force on the workspace config log
    /// (under rules v2 a new device of an account with a passkey needs
    /// **Confirm this device**).
    device_confirmed: bool,
}

/// Passkeys of this account on the active workspace, the rules version and
/// the key version.
#[tauri::command]
pub async fn security_status(ligus_app: tauri::AppHandle) -> Result<SecurityStatusDto, String> {
    crate::off_main(ligus_app, move |ligus_app| security_status_sync(tauri::Manager::state::<AppState>(ligus_app))).await
}

pub fn security_status_sync(state: State<'_, AppState>) -> Result<SecurityStatusDto, String> {
    let ws = state.active_workspace_id();
    let svc = state.service.lock().unwrap();
    let account = svc.author().account_id;
    let passkeys = account
        .map(|a| svc.passkeys_of(ws, a).unwrap_or_default())
        .unwrap_or_default()
        .into_iter()
        .map(|(credential_id, recovery)| PasskeyDto { credential_id, recovery })
        .collect();
    let cfg = svc.load(ligus_core::workspace_config_session_id(ws)).ok().flatten();
    let rules = cfg.as_ref().map(|c| c.authz.rules).unwrap_or(ligus_core::authz::RULES_GENESIS);
    let (_, timeline) = svc.store().load_config(ws, None, None).map_err(|e| e.to_string())?;
    Ok(SecurityStatusDto {
        passkeys,
        rules,
        key_version: timeline.current().key_epochs.head_version(),
        secret_backend: ligus_runtime::secret_store::for_data_dir(&state.data_dir).backend().as_str().into(),
        device_confirmed: svc.device_confirmed(ws).unwrap_or(false),
    })
}

/// Confirm this device on the active workspace (rules v2: a new device of an
/// account with a passkey counts only once that passkey confirms it).
#[tauri::command]
pub async fn device_confirm(app: AppHandle) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let ws = state.active_workspace_id();
        let mut svc = state.service.lock().unwrap();
        svc.confirm_device(ws).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
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
        let (account, name, existing, policy) = {
            let svc = state.service.lock().unwrap();
            let account = svc.author().account_id.ok_or("this device has no account")?;
            let existing: Vec<String> = svc.passkeys_of(ws, account).map_err(|e| e.to_string())?.into_iter().map(|(id, _)| id).collect();
            let policy = svc.workspace_presence_policy(ws).map_err(|e| e.to_string())?;
            (account, svc.author().display_name.clone(), existing, policy)
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
            digest_short: ligus_core::keys::hex(&challenge[..8]),
            summary: "Create a Ligus passkey".into(),
            policy: policy.clone(),
        };
        let att = presence_create(&app, &create_req, &crate::passkey_native::user_handle(account, ws), &name)?;
        let (id, alg, pk) = policy.with(|p| presence::verify_create(&att, &challenge, p)).map_err(|e| e.0)?;
        let new_id = b64url(&id);
        let credential = PasskeyCredential {
            account_id: account,
            credential_id: new_id.clone(),
            alg,
            public_key: pk,
            label: label.trim().chars().take(64).collect(),
            recovery: false,
            registered_at: ligus_core::Timestamp::now(),
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
            presence_get(&app2, &req)
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
        svc.register_passkey(ws, key.credential(account, "Recovery key"), &move |req| presence_get(&app2, req))
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

/// Stop waiting for the passkey (the system dialog or the browser).
#[tauri::command]
pub fn presence_cancel() {
    SIGNALS.cancel();
}

/// Use the browser hand-off instead of the system passkey dialog (a
/// security key or another device the dialog doesn't offer, or after
/// closing it).
#[tauri::command]
pub fn presence_use_browser() {
    SIGNALS.use_browser();
}

/// Confirm the pending request with the recovery key's 24 words instead of
/// a passkey (lost every passkey). The words are parsed here (checksum
/// verified), used for that one assertion and never stored; the fold accepts
/// it only if that recovery key is registered to this account. `digest` is
/// the short digest the sheet showed: the key answers only that request.
#[tauri::command]
pub fn presence_use_recovery(words: String, digest: String) -> Result<(), String> {
    let key = RecoveryKey::from_words(&words).map_err(|e| e.0)?;
    SIGNALS.set_recovery(key, &digest);
    Ok(())
}

/// Install the desktop's presence provider (native, else the hand-off).
pub fn install(state: &AppState, app: &AppHandle) {
    crate::passkey_native::remember_window(app);
    state.service.lock().unwrap().set_presence_provider(Some(Arc::new(DesktopPresence(app.clone()))));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream that never ends: a request line (or header block) with no
    /// end. Fails the test if the loopback reads more than 1 MiB of it.
    struct Endless {
        prefix: Vec<u8>,
        fill: &'static [u8],
        read: usize,
    }

    impl Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            assert!(self.read < 1 << 20, "the loopback kept reading an endless request");
            let n = buf.len().min(4096);
            for (i, b) in buf[..n].iter_mut().enumerate() {
                let at = self.read + i;
                *b = if at < self.prefix.len() { self.prefix[at] } else { self.fill[(at - self.prefix.len()) % self.fill.len()] };
            }
            self.read += n;
            Ok(n)
        }
    }

    /// B-12: one connection to the loopback can't make the desktop read
    /// without bound: the request line, each header, the header block and
    /// the body are capped.
    #[test]
    fn loopback_reads_are_capped() {
        let endless_line = Endless { prefix: b"POST /n".to_vec(), fill: b"AAAAAAAA", read: 0 };
        assert!(parse_submission(endless_line, "n").is_err());
        let endless_headers = Endless { prefix: b"POST /n HTTP/1.1\r\n".to_vec(), fill: b"X-A: b\r\n", read: 0 };
        assert!(parse_submission(endless_headers, "n").is_err());
        let endless_header = Endless { prefix: b"POST /n HTTP/1.1\r\nX-A: ".to_vec(), fill: b"bbbbbbbb", read: 0 };
        assert!(parse_submission(endless_header, "n").is_err());
        let huge_body = b"POST /n HTTP/1.1\r\nContent-Length: 99999999\r\n\r\nab".to_vec();
        assert!(parse_submission(&huge_body[..], "n").is_err());
        let many_headers = [b"POST /n HTTP/1.1\r\n".to_vec(), b"X: y\r\n".repeat(MAX_HEADERS + 1), b"\r\n".to_vec()].concat();
        assert!(parse_submission(&many_headers[..], "n").is_err());
        // A well-formed submission still goes through; a wrong nonce doesn't.
        let ok = b"POST /n HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 5\r\n\r\nab=cd".to_vec();
        assert_eq!(parse_submission(&ok[..], "n").unwrap(), b"ab=cd");
        assert!(parse_submission(&ok[..], "m").is_err());
    }

    /// B-12: the browser is opened on a private redirect page, so the
    /// loopback nonce isn't in the `open` command's arguments.
    #[test]
    fn the_nonce_stays_off_the_command_line() {
        let dir = std::env::temp_dir().join(format!("ligus-presence-test-{}", uuid::Uuid::new_v4().simple()));
        let url = format!("{}#op=get&c=abc&p=4242&n=secret-nonce-123", ligus_core::presence::PresencePolicy::ligus().page_url());
        let page = redirect_page(&dir.join("presence"), &url).unwrap();
        assert!(!page.to_string_lossy().contains("secret-nonce-123"), "the nonce is in the path");
        let html = std::fs::read_to_string(&page).unwrap();
        assert!(html.contains(&serde_json::to_string(&url).unwrap()), "{html}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&page).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(page.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

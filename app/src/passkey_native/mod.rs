//! Native desktop passkeys (Phase D).
//!
//! Where the OS has a platform WebAuthn API, the desktop runs the ceremony
//! natively instead of handing it to the system browser:
//!
//! * **macOS**: AuthenticationServices
//!   (`ASAuthorizationPlatformPublicKeyCredentialProvider`, [`macos`]). The
//!   system only serves an RP the app is associated with: the
//!   `webcredentials:` domains of `app/Ligus.entitlements`, each confirmed by
//!   the domain's `apple-app-site-association`. A workspace whose RP isn't one
//!   of them (a self-hosted RP) keeps the browser hand-off. Apple writes the
//!   `clientDataJSON` itself, with origin `https://<rp id>`.
//! * **Windows**: `webauthn.dll` ([`windows`]), loaded at run time. The app
//!   builds the `clientDataJSON` itself
//!   ([`hive_core::presence::client_data_json`], origin: the workspace's
//!   hand-off page origin), so any well-formed workspace RP works.
//! * **Linux**: no platform API; the browser hand-off as before.
//!
//! The hand-off ([`crate::passkey_ui`]) stays the fallback everywhere: when
//! the native API is missing, refuses the RP (an unsigned or unentitled
//! build, an RP without the association) or fails, and when the person
//! picks the browser on the confirmation sheet.
//!
//! The result is the same [`PresenceAssertion`] the hand-off returns (mapped
//! by [`hive_core::presence::native`]); the desktop checks its challenge with
//! the same [`returned_challenge`], and the fold verifies it with the same
//! verifiers. Nothing about the digest or its binding differs by path.
//!
//! Tests drive [`run_get`] and [`run_create`] with a mock authenticator; no
//! test ever reaches a platform API ([`platform`] is `None` under test, in
//! the e2e build, and when `LIGUS_NATIVE_PASSKEYS=0`).

// Linux and Windows don't use every item (the entitlements reader, some
// error kinds) outside tests.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use hive_core::presence::native::{RawAssertion, RawRegistration};
use hive_core::presence::{returned_challenge, unb64url, PresenceAssertion, PresencePolicy, RecoveryKey};
use hive_runtime::chat_service::PresenceRequest;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

/// The env var that turns the native path off (`0`): every ceremony then
/// uses the browser hand-off.
pub const DISABLE_ENV: &str = "LIGUS_NATIVE_PASSKEYS";

/// A `webauthn.get` for a native authenticator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetRequest {
    pub rp_id: String,
    /// The origin a self-built `clientDataJSON` names (Windows).
    pub origin: String,
    pub challenge: [u8; 32],
    /// Allowed credential ids (decoded); empty = any of this RP's.
    pub allow: Vec<Vec<u8>>,
}

/// A `webauthn.create` for a native authenticator (`attestation: "none"`,
/// user verification required, ES256 or EdDSA).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateRequest {
    pub rp_id: String,
    pub origin: String,
    pub challenge: [u8; 32],
    /// The WebAuthn user handle: the account id's 16 bytes, as on the page.
    pub user_id: Vec<u8>,
    pub user_name: String,
    /// The account's existing credentials (decoded), never to be replaced.
    pub exclude: Vec<Vec<u8>>,
}

/// Why a native ceremony gave no result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeError {
    /// The API can't serve this request here (missing, old OS, the app isn't
    /// associated with the RP, an option it doesn't support): use the browser.
    Unavailable(String),
    /// The person dismissed the system dialog.
    Cancelled,
    /// The ceremony failed (use the browser).
    Failed(String),
    /// The authenticator refused for a reason the browser would hit too
    /// (it already holds one of the account's passkeys): an error, no
    /// fallback.
    Refused(String),
}

/// A platform WebAuthn API. Calls block until the ceremony ends; `cancel`
/// may be called from another thread and ends a pending call.
pub trait NativeAuthenticator: Send + Sync {
    /// The origin this authenticator's `clientDataJSON` names for a
    /// workspace with `policy`, or `None` when it can't serve that RP (the
    /// hand-off is used then).
    fn origin(&self, policy: &PresencePolicy) -> Option<String>;
    fn get(&self, req: &GetRequest) -> Result<RawAssertion, NativeError>;
    fn create(&self, req: &CreateRequest) -> Result<RawRegistration, NativeError>;
    fn cancel(&self);
}

/// This OS's native authenticator, if it has one and it isn't turned off.
pub fn platform(app: &tauri::AppHandle) -> Option<Arc<dyn NativeAuthenticator>> {
    if cfg!(any(test, feature = "e2e")) || std::env::var(DISABLE_ENV).is_ok_and(|v| v.trim() == "0") {
        return None;
    }
    platform_impl(app)
}

#[cfg(target_os = "macos")]
fn platform_impl(app: &tauri::AppHandle) -> Option<Arc<dyn NativeAuthenticator>> {
    macos::MacAuthenticator::new(app.clone()).map(|a| Arc::new(a) as Arc<dyn NativeAuthenticator>)
}

#[cfg(windows)]
fn platform_impl(_app: &tauri::AppHandle) -> Option<Arc<dyn NativeAuthenticator>> {
    windows::WinAuthenticator::load().map(|a| Arc::new(a) as Arc<dyn NativeAuthenticator>)
}

/// At startup, on the main thread: what the native path needs from the main
/// window later without asking the (possibly blocked) main thread.
pub fn remember_window(_app: &tauri::AppHandle) {
    #[cfg(windows)]
    windows::remember_window(_app);
}

#[cfg(not(any(target_os = "macos", windows)))]
fn platform_impl(_app: &tauri::AppHandle) -> Option<Arc<dyn NativeAuthenticator>> {
    None
}

/// The `webcredentials:` domains in an entitlements plist: the RPs the macOS
/// app may run native ceremonies for.
pub fn webcredential_domains(entitlements: &str) -> Vec<String> {
    entitlements
        .split("<string>")
        .skip(1)
        .filter_map(|s| s.split("</string>").next())
        .filter_map(|s| s.trim().strip_prefix("webcredentials:"))
        .map(|d| d.split('?').next().unwrap_or(d).trim().to_ascii_lowercase())
        .filter(|d| !d.is_empty())
        .collect()
}

/// What the person (through the confirmation sheet) asked for while a
/// ceremony waits: shared by the native path and the browser hand-off.
pub struct Signals {
    cancel: AtomicBool,
    browser: AtomicBool,
    recovery: Mutex<Option<RecoveryKey>>,
}

impl Default for Signals {
    fn default() -> Self {
        Self::new()
    }
}

impl Signals {
    pub const fn new() -> Self {
        Self { cancel: AtomicBool::new(false), browser: AtomicBool::new(false), recovery: Mutex::new(None) }
    }
    /// A new ceremony starts: forget earlier choices.
    pub fn reset(&self) {
        self.cancel.store(false, Ordering::SeqCst);
        self.browser.store(false, Ordering::SeqCst);
        let _ = self.take_recovery();
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
    /// The person chose the browser instead of the system dialog.
    pub fn use_browser(&self) {
        self.browser.store(true, Ordering::SeqCst);
    }
    fn browser_chosen(&self) -> bool {
        self.browser.load(Ordering::SeqCst)
    }
    pub fn set_recovery(&self, k: RecoveryKey) {
        *self.recovery.lock().unwrap_or_else(|e| e.into_inner()) = Some(k);
    }
    pub fn take_recovery(&self) -> Option<RecoveryKey> {
        self.recovery.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// Where the confirmation sheet is in a native ceremony.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The system passkey dialog is up.
    Native,
    /// The person closed it: the sheet offers the browser, the recovery key
    /// (for a `get`) and Cancel.
    Dismissed,
}

/// What the ceremony tells the confirmation sheet.
pub trait Events {
    fn stage(&self, stage: Stage);
    fn done(&self, ok: bool);
}

/// How one native attempt ended.
pub enum Attempt<T> {
    Done(T),
    Recovery(RecoveryKey),
    /// Use the browser hand-off (why, for the log).
    Browser(String),
    Cancelled,
    Failed(String),
}

/// Run `call` on its own thread and wait for it or for the person's choice
/// on the sheet. A pending call is cancelled whenever the wait ends without
/// its result, and never waited for after that (a late result is dropped).
pub fn attempt<T: Send + 'static>(
    auth: Arc<dyn NativeAuthenticator>,
    sig: &Signals,
    recovery_ok: bool,
    wait: Duration,
    events: &dyn Events,
    call: impl FnOnce(&dyn NativeAuthenticator) -> Result<T, NativeError> + Send + 'static,
) -> Attempt<T> {
    let (tx, rx) = mpsc::channel();
    let worker = auth.clone();
    std::thread::spawn(move || {
        let _ = tx.send(call(&*worker));
    });
    let deadline = Instant::now() + wait;
    let mut pending = true;
    let stop = |pending: bool| {
        if pending {
            auth.cancel();
        }
    };
    loop {
        if sig.cancelled() {
            stop(pending);
            return Attempt::Cancelled;
        }
        if sig.browser_chosen() {
            stop(pending);
            return Attempt::Browser("the person chose the browser".into());
        }
        if recovery_ok {
            if let Some(k) = sig.take_recovery() {
                stop(pending);
                return Attempt::Recovery(k);
            }
        }
        if Instant::now() > deadline {
            stop(pending);
            return Attempt::Failed("timed out waiting for the passkey".into());
        }
        if !pending {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Ok(v)) => return Attempt::Done(v),
            Ok(Err(NativeError::Cancelled)) => {
                // The person closed the system dialog: wait for their choice.
                pending = false;
                events.stage(Stage::Dismissed);
            }
            Ok(Err(NativeError::Unavailable(why) | NativeError::Failed(why))) => return Attempt::Browser(why),
            Ok(Err(NativeError::Refused(why))) => return Attempt::Failed(why),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Attempt::Browser("the native authenticator stopped".into()),
        }
    }
}

/// A presence `get` over `req.challenge`: native when this OS's
/// authenticator serves the workspace's RP, else (or on fallback) `hand_off`.
pub fn run_get(
    native: Option<Arc<dyn NativeAuthenticator>>,
    req: &PresenceRequest,
    sig: &Signals,
    wait: Duration,
    events: &dyn Events,
    hand_off: impl FnOnce() -> Result<PresenceAssertion, String>,
) -> Result<PresenceAssertion, String> {
    let Some((auth, origin)) = native.and_then(|a| a.origin(&req.policy).map(|o| (a, o))) else {
        return hand_off();
    };
    sig.reset();
    events.stage(Stage::Native);
    let g = GetRequest {
        rp_id: req.policy.rp_id.clone(),
        origin,
        challenge: req.challenge,
        allow: req.credential_ids.iter().filter_map(|c| unb64url(c).ok()).collect(),
    };
    let out = match attempt(auth, sig, true, wait, events, move |a| a.get(&g)) {
        Attempt::Done(raw) => {
            let a = raw.into_assertion();
            if returned_challenge(&a.client_data_json).as_deref() == Some(&req.challenge[..]) {
                Ok(a)
            } else {
                Err("the passkey returned an assertion for a different challenge".to_string())
            }
        }
        // The recovery key signs the very challenge requested, here.
        Attempt::Recovery(k) => Ok(k.assert(&req.challenge)),
        Attempt::Cancelled => Err("cancelled".to_string()),
        Attempt::Failed(e) => Err(e),
        Attempt::Browser(why) => {
            tracing::info!(target: "presence", "native passkey: using the browser ({why})");
            return hand_off();
        }
    };
    events.done(out.is_ok());
    out
}

/// A `create` for a new passkey: native when this OS's authenticator serves
/// the workspace's RP, else (or on fallback) `hand_off`. The result is in
/// the hand-off's form (the attestation object in `authenticator_data`).
pub fn run_create(
    native: Option<Arc<dyn NativeAuthenticator>>,
    req: &PresenceRequest,
    user_name: &str,
    sig: &Signals,
    wait: Duration,
    events: &dyn Events,
    hand_off: impl FnOnce() -> Result<PresenceAssertion, String>,
) -> Result<PresenceAssertion, String> {
    let Some((auth, origin)) = native.and_then(|a| a.origin(&req.policy).map(|o| (a, o))) else {
        return hand_off();
    };
    sig.reset();
    events.stage(Stage::Native);
    let c = CreateRequest {
        rp_id: req.policy.rp_id.clone(),
        origin,
        challenge: req.challenge,
        user_id: req.account_id.as_bytes().to_vec(),
        user_name: user_name.to_string(),
        exclude: req.credential_ids.iter().filter_map(|c| unb64url(c).ok()).collect(),
    };
    let out = match attempt(auth, sig, false, wait, events, move |a| a.create(&c)) {
        Attempt::Done(raw) => {
            let a = raw.into_assertion();
            if returned_challenge(&a.client_data_json).as_deref() == Some(&req.challenge[..]) {
                Ok(a)
            } else {
                Err("the passkey returned a credential for a different challenge".to_string())
            }
        }
        Attempt::Recovery(_) => Err("a recovery key doesn't create a passkey".to_string()),
        Attempt::Cancelled => Err("cancelled".to_string()),
        Attempt::Failed(e) => Err(e),
        Attempt::Browser(why) => {
            tracing::info!(target: "presence", "native passkey: using the browser ({why})");
            return hand_off();
        }
    };
    events.done(out.is_ok());
    out
}

/// A ceremony that needs the app's main thread (macOS: AppKit presents the
/// dialog and delivers its result there). The main thread runs Tauri's
/// synchronous commands, and many of those wait for the service lock that
/// the thread asking for presence holds for the whole ceremony. If one is
/// waiting, the dialog can't appear or answer until the ceremony ends: a
/// deadlock until the timeout. So the waiter checks the main thread answers
/// (it must start the request within `start_wait`, and answer a heartbeat
/// every `beat_every` within `stall`) and otherwise gives up with
/// [`NativeError::Unavailable`], which falls back to the browser hand-off
/// (it doesn't need the main thread).
pub(crate) struct MainThreadWatch {
    pub start_wait: Duration,
    pub stall: Duration,
    pub beat_every: Duration,
    pub total: Duration,
}

/// The request was queued for the main thread.
pub(crate) const QUEUED: u8 = 0;
/// The main thread took it and presented it.
pub(crate) const STARTED: u8 = 1;
/// The waiter gave up first: the main thread must not present it.
pub(crate) const ABANDONED: u8 = 2;

/// Wait for a main-thread ceremony's reply on `rx`. `state` moves from
/// [`QUEUED`] to [`STARTED`] (the main thread, before presenting) or to
/// [`ABANDONED`] (here), whichever comes first. `on_main` queues a closure
/// on the main thread (false if it can't); `cancel` cancels the presented
/// request.
pub(crate) fn await_main_thread<T>(
    rx: &mpsc::Receiver<Result<T, NativeError>>,
    state: &std::sync::atomic::AtomicU8,
    w: &MainThreadWatch,
    on_main: &dyn Fn(Box<dyn FnOnce() + Send>) -> bool,
    cancel: &dyn Fn(),
) -> Result<T, NativeError> {
    use std::sync::atomic::AtomicU64;
    let busy = || NativeError::Unavailable("the app's main thread is busy".into());
    let t0 = Instant::now();
    loop {
        if let Ok(r) = rx.try_recv() {
            return r;
        }
        if state.load(Ordering::SeqCst) == STARTED {
            break;
        }
        if t0.elapsed() > w.start_wait && state.compare_exchange(QUEUED, ABANDONED, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
            return Err(busy());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let answered = Arc::new(AtomicU64::new(0));
    let mut sent = 0u64;
    let mut sent_at = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(r) => return r,
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(NativeError::Failed("the passkey dialog went away".into())),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if t0.elapsed() > w.total {
            cancel();
            return Err(NativeError::Failed("no answer from the passkey dialog".into()));
        }
        let got = answered.load(Ordering::SeqCst);
        if got < sent && sent_at.elapsed() > w.stall {
            cancel();
            return Err(busy());
        }
        if got == sent && sent_at.elapsed() >= w.beat_every {
            sent += 1;
            sent_at = Instant::now();
            let (a, n) = (answered.clone(), sent);
            if !on_main(Box::new(move || a.store(n, Ordering::SeqCst))) {
                cancel();
                return Err(busy());
            }
        }
    }
}

#[cfg(test)]
mod tests;

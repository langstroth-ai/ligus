//! ligus-auth client: sign in at an organization's identity provider through
//! ligus-auth and keep a short-lived relay access token fresh.
//!
//! ligus-auth (the `cmd/ligus-auth` service in the relay repository) runs the
//! OIDC sign-in and mints `hrt1` relay tokens that live at most an hour. This
//! module is the desktop and CLI side:
//!
//! - **Device-code sign-in** (RFC 8628): [`start_device`] returns a code the
//!   person enters in a browser; [`poll_device`] waits for approval.
//! - **Device proof**: every token request is signed with this device's
//!   Ed25519 key over a server nonce ([`device_proof_message`]). Refresh tokens
//!   are bound to that key, so a copied session file is useless elsewhere.
//! - **Refresh**: [`fresh_access_token`] rotates the refresh token shortly
//!   before the access token expires; [`run_refresher`] keeps a [`TokenCell`]
//!   current for long-running hosts, and `RelayClient::with_auth_cell` reads it.
//!
//! A ligus-auth token only gates relay *transport*. It never makes anyone a
//! workspace member: that still takes an Admin's signed `MemberAdded`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use base64::Engine;
use hive_core::crypto::SigningKeypair;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Domain-separation prefix of the device proof (PHASE0-SPECS section 0.1).
/// Frozen: ligus-auth verifies exactly these bytes.
pub const DEVICE_PROOF_CONTEXT: &[u8] = b"ligus-auth-device-proof-v1\0";
/// The session file in the data directory (mode 0600). Holds the refresh token.
pub const SESSION_FILE: &str = "ligus-auth.json";
const LOCK_FILE: &str = "ligus-auth.lock";
/// RFC 8628 grant type.
pub const GRANT_DEVICE: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// Refresh this long before the access token expires.
const REFRESH_MARGIN_SECS: i64 = 120;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("could not reach ligus-auth: {0}")]
    Http(String),
    /// The session is over (revoked, expired, deprovisioned, or no workspace
    /// is granted). Sign in again.
    #[error("sign-in needed: {0}")]
    SignInNeeded(String),
    #[error("ligus-auth error: {0}")]
    Protocol(String),
    #[error("invalid ligus-auth URL: {0}")]
    BadUrl(String),
    #[error("session file: {0}")]
    Io(String),
}

/// Signs token requests with this device's key.
pub trait DeviceSigner: Send + Sync {
    fn device_id(&self) -> String;
    fn public_key(&self) -> [u8; 32];
    fn sign(&self, msg: &[u8]) -> [u8; 64];
}

/// The device's own signing key (the same key that signs its envelopes).
pub struct DeviceKey {
    id: String,
    kp: SigningKeypair,
}

impl DeviceKey {
    pub fn new(device_id: uuid::Uuid, kp: SigningKeypair) -> Self {
        Self {
            id: device_id.to_string(),
            kp,
        }
    }
}

impl DeviceSigner for DeviceKey {
    fn device_id(&self) -> String {
        self.id.clone()
    }
    fn public_key(&self) -> [u8; 32] {
        self.kp.public_key_bytes()
    }
    fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.kp.sign(msg)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn b64u(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The exact bytes a device signs: the context, then tenant, grant type,
/// nonce, device id and the hex SHA-256 of the grant credential (the device
/// code or refresh token), NUL-separated.
pub fn device_proof_message(
    tenant: &str,
    grant_type: &str,
    nonce: &str,
    device_id: &str,
    credential: &str,
) -> Vec<u8> {
    let mut out = DEVICE_PROOF_CONTEXT.to_vec();
    let cred = hex(&Sha256::digest(credential.as_bytes()));
    for (i, part) in [tenant, grant_type, nonce, device_id, cred.as_str()]
        .iter()
        .enumerate()
    {
        if i > 0 {
            out.push(0);
        }
        out.extend_from_slice(part.as_bytes());
    }
    out
}

/// Validate a ligus-auth tenant URL (`https://auth.example.com/t/<tenant>`)
/// and return it normalized, with the tenant id. Plain http is accepted only
/// for a loopback host (local testing).
pub fn parse_auth_url(raw: &str) -> Result<(String, String), AuthError> {
    let s = raw.trim().trim_end_matches('/');
    let u = reqwest::Url::parse(s).map_err(|e| AuthError::BadUrl(e.to_string()))?;
    let host = u.host_str().unwrap_or_default().to_string();
    let loopback = host == "127.0.0.1" || host == "[::1]" || host == "::1" || host == "localhost";
    match u.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => return Err(AuthError::BadUrl("use https://".into())),
    }
    if u.query().is_some() || u.fragment().is_some() || !u.username().is_empty() {
        return Err(AuthError::BadUrl(
            "no query, fragment or credentials".into(),
        ));
    }
    let segs: Vec<&str> = u.path().trim_matches('/').split('/').collect();
    let n = segs.len();
    if n < 2 || segs[n - 2] != "t" || segs[n - 1].is_empty() {
        return Err(AuthError::BadUrl(
            "expected the tenant URL, like https://auth.example.com/t/<tenant>".into(),
        ));
    }
    let tenant = segs[n - 1].to_string();
    if !tenant
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(AuthError::BadUrl(
            "tenant id must be lowercase letters, digits and -".into(),
        ));
    }
    Ok((s.to_string(), tenant))
}

/// A signed-in ligus-auth session. Saved as [`SESSION_FILE`] (0600).
#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    pub auth_url: String,
    pub tenant: String,
    pub client_id: String,
    pub device_id: String,
    pub refresh_token: String,
    /// Nonce the server handed out for the next refresh (saves a round trip).
    #[serde(default)]
    pub refresh_nonce: Option<String>,
    pub access_token: String,
    /// Unix seconds.
    pub access_expires_at: i64,
    #[serde(default)]
    pub sub: String,
    #[serde(default)]
    pub workspaces: Vec<String>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the tokens.
        f.debug_struct("Session")
            .field("auth_url", &self.auth_url)
            .field("tenant", &self.tenant)
            .field("sub", &self.sub)
            .field("workspaces", &self.workspaces)
            .field("access_expires_at", &self.access_expires_at)
            .finish_non_exhaustive()
    }
}

impl Session {
    pub fn access_fresh(&self, now: i64) -> bool {
        self.access_expires_at - REFRESH_MARGIN_SECS > now
    }
}

pub fn session_path(dir: &Path) -> PathBuf {
    dir.join(SESSION_FILE)
}

/// Where a session keeps its tokens (B2): the OS secret store (scope
/// `auth`); the session file holds `{"set": true}` in their place.
const SESSION_SECRETS: crate::secret_store::SecretPaths = &["refresh_token", "access_token"];

pub fn load_session(dir: &Path) -> Option<Session> {
    let store = crate::secret_store::for_data_dir(dir);
    let v = crate::secret_store::load_settings_json(&session_path(dir), &*store, "auth", SESSION_SECRETS)?;
    let s: Session = serde_json::from_value(v).ok()?;
    if s.refresh_token.is_empty() {
        return None;
    }
    register_secrets(&s);
    Some(s)
}

/// Mask the session's refresh token in replies and proposals. It is opaque;
/// the access token is an `hrt1` token, which redaction masks by its shape.
fn register_secrets(s: &Session) {
    crate::secret_redact::register(&s.refresh_token);
}

/// Write the session: its tokens to the OS secret store, the rest to the
/// session file (atomic, owner-only).
pub fn save_session(dir: &Path, s: &Session) -> Result<(), AuthError> {
    register_secrets(s);
    std::fs::create_dir_all(dir).map_err(|e| AuthError::Io(e.to_string()))?;
    let store = crate::secret_store::for_data_dir(dir);
    let v = serde_json::to_value(s).map_err(|e| AuthError::Io(e.to_string()))?;
    crate::secret_store::save_settings_json(&session_path(dir), &*store, "auth", v, SESSION_SECRETS)
        .map_err(|e| AuthError::Io(e.to_string()))
}

pub fn delete_session(dir: &Path) {
    let store = crate::secret_store::for_data_dir(dir);
    let _ = crate::secret_store::persist(&*store, "auth", &Default::default());
    let _ = std::fs::remove_file(session_path(dir));
}

/// The current relay access token, shared with `RelayClient::with_auth_cell`.
#[derive(Clone, Default)]
pub struct TokenCell(Arc<RwLock<Option<String>>>);

impl std::fmt::Debug for TokenCell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenCell(..)")
    }
}

impl TokenCell {
    pub fn get(&self) -> Option<String> {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn set(&self, v: Option<String>) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = v;
    }
}

/// HTTP client for ligus-auth: bounded, and never follows redirects (a
/// redirect must not carry a refresh token or device code elsewhere).
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("ligus-app")
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// The code and link the person uses to approve this device.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStart {
    #[serde(alias = "device_code")]
    pub device_code: String,
    #[serde(alias = "user_code")]
    pub user_code: String,
    #[serde(alias = "verification_uri")]
    pub verification_uri: String,
    #[serde(default, alias = "verification_uri_complete")]
    pub verification_uri_complete: Option<String>,
    #[serde(default = "default_interval")]
    pub interval: u64,
    #[serde(default, alias = "expires_in")]
    pub expires_in: u64,
}

fn default_interval() -> u64 {
    5
}

/// Step 1 of the device flow.
pub async fn start_device(
    http: &reqwest::Client,
    auth_url: &str,
    client_id: &str,
    label: &str,
) -> Result<DeviceStart, AuthError> {
    let (base, _) = parse_auth_url(auth_url)?;
    let resp = http
        .post(format!("{base}/device_authorization"))
        .form(&[("client_id", client_id), ("device_label", label)])
        .send()
        .await
        .map_err(|e| AuthError::Http(e.to_string()))?;
    let status = resp.status();
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AuthError::Http(e.to_string()))?;
    if !status.is_success() {
        return Err(AuthError::Protocol(oauth_error(&v)));
    }
    let ds: DeviceStart =
        serde_json::from_value(v).map_err(|e| AuthError::Protocol(e.to_string()))?;
    if ds.device_code.is_empty() || ds.user_code.is_empty() {
        return Err(AuthError::Protocol("malformed device authorization".into()));
    }
    // The person opens this link: keep it on ligus-auth's own origin.
    let origin = |u: &str| {
        reqwest::Url::parse(u)
            .ok()
            .map(|u| u.origin().ascii_serialization())
    };
    if origin(&ds.verification_uri) != origin(&base) {
        return Err(AuthError::Protocol(
            "verification link points off the ligus-auth origin".into(),
        ));
    }
    if ds
        .verification_uri_complete
        .as_deref()
        .is_some_and(|c| origin(c) != origin(&base))
    {
        return Err(AuthError::Protocol(
            "verification link points off the ligus-auth origin".into(),
        ));
    }
    Ok(ds)
}

/// Outcome of one poll.
#[derive(Debug)]
pub enum Poll {
    Pending,
    SlowDown,
    Denied,
    Expired,
    SignedIn(Box<Session>),
}

fn oauth_error(v: &serde_json::Value) -> String {
    let e = v.get("error").and_then(|x| x.as_str()).unwrap_or("error");
    match v.get("error_description").and_then(|x| x.as_str()) {
        Some(d) => format!("{e}: {d}"),
        None => e.to_string(),
    }
}

#[derive(Deserialize)]
struct TokenResp {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
    #[serde(default)]
    device_nonce: Option<String>,
    #[serde(default)]
    sub: String,
    #[serde(default)]
    workspaces: Vec<String>,
}

/// Post a token request, answering a `use_device_nonce` challenge once.
/// `nonce` is a nonce the client already holds (skips the challenge).
async fn token_request(
    http: &reqwest::Client,
    (base, tenant): (&str, &str),
    mut form: Vec<(&'static str, String)>,
    grant_type: &str,
    credential: &str,
    signer: &dyn DeviceSigner,
    mut nonce: Option<String>,
) -> Result<Result<TokenResp, serde_json::Value>, AuthError> {
    form.push(("grant_type", grant_type.to_string()));
    for _ in 0..2 {
        let mut f = form.clone();
        if let Some(n) = &nonce {
            let id = signer.device_id();
            let sig = signer.sign(&device_proof_message(
                tenant, grant_type, n, &id, credential,
            ));
            f.push(("device_id", id));
            f.push(("device_key", b64u(&signer.public_key())));
            f.push(("device_nonce", n.clone()));
            f.push(("device_sig", b64u(&sig)));
        }
        let resp = http
            .post(format!("{base}/token"))
            .form(&f)
            .send()
            .await
            .map_err(|e| AuthError::Http(e.to_string()))?;
        let status = resp.status();
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AuthError::Http(e.to_string()))?;
        if status.is_success() {
            return serde_json::from_value(v)
                .map(Ok)
                .map_err(|e| AuthError::Protocol(e.to_string()));
        }
        if v.get("error").and_then(|x| x.as_str()) == Some("use_device_nonce") {
            nonce = v
                .get("device_nonce")
                .and_then(|x| x.as_str())
                .map(str::to_string);
            if nonce.is_some() {
                continue;
            }
        }
        return Ok(Err(v));
    }
    Err(AuthError::Protocol("device proof refused".into()))
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn session_from(
    base: &str,
    tenant: &str,
    client_id: &str,
    device_id: String,
    t: TokenResp,
) -> Session {
    Session {
        auth_url: base.to_string(),
        tenant: tenant.to_string(),
        client_id: client_id.to_string(),
        device_id,
        refresh_token: t.refresh_token,
        refresh_nonce: t.device_nonce,
        access_token: t.access_token,
        access_expires_at: now_unix() + t.expires_in.clamp(0, 3600),
        sub: t.sub,
        workspaces: t.workspaces,
    }
}

/// Step 2 of the device flow: poll once.
pub async fn poll_device(
    http: &reqwest::Client,
    auth_url: &str,
    client_id: &str,
    device_code: &str,
    signer: &dyn DeviceSigner,
) -> Result<Poll, AuthError> {
    let (base, tenant) = parse_auth_url(auth_url)?;
    let form = vec![
        ("device_code", device_code.to_string()),
        ("client_id", client_id.to_string()),
    ];
    match token_request(
        http,
        (&base, &tenant),
        form,
        GRANT_DEVICE,
        device_code,
        signer,
        None,
    )
    .await?
    {
        Ok(t) => Ok(Poll::SignedIn(Box::new(session_from(
            &base,
            &tenant,
            client_id,
            signer.device_id(),
            t,
        )))),
        Err(v) => match v.get("error").and_then(|x| x.as_str()).unwrap_or("") {
            "authorization_pending" => Ok(Poll::Pending),
            "slow_down" => Ok(Poll::SlowDown),
            "access_denied" => Ok(Poll::Denied),
            "expired_token" | "invalid_grant" => Ok(Poll::Expired),
            _ => Err(AuthError::Protocol(oauth_error(&v))),
        },
    }
}

/// Rotate the refresh token and get a new access token.
pub async fn refresh(
    http: &reqwest::Client,
    s: &Session,
    signer: &dyn DeviceSigner,
) -> Result<Session, AuthError> {
    if signer.device_id() != s.device_id {
        return Err(AuthError::SignInNeeded(
            "this session belongs to another device".into(),
        ));
    }
    let (base, tenant) = parse_auth_url(&s.auth_url)?;
    let form = vec![("refresh_token", s.refresh_token.clone())];
    match token_request(
        http,
        (&base, &tenant),
        form,
        "refresh_token",
        &s.refresh_token,
        signer,
        s.refresh_nonce.clone(),
    )
    .await?
    {
        Ok(t) => Ok(session_from(
            &base,
            &tenant,
            &s.client_id,
            s.device_id.clone(),
            t,
        )),
        Err(v) => match v.get("error").and_then(|x| x.as_str()).unwrap_or("") {
            "invalid_grant" | "access_denied" => Err(AuthError::SignInNeeded(oauth_error(&v))),
            _ => Err(AuthError::Protocol(oauth_error(&v))),
        },
    }
}

/// Revoke the session at ligus-auth (best effort) and delete the local file.
pub async fn sign_out(http: &reqwest::Client, dir: &Path) {
    if let Some(s) = load_session(dir) {
        if let Ok((base, _)) = parse_auth_url(&s.auth_url) {
            let _ = http
                .post(format!("{base}/revoke"))
                .form(&[
                    ("token", s.refresh_token.as_str()),
                    ("token_type_hint", "refresh_token"),
                ])
                .send()
                .await;
        }
    }
    delete_session(dir);
}

/// Cross-process lock around a refresh: two processes on one data directory
/// must not both spend the same refresh token (ligus-auth would treat the
/// second use as theft and end the session).
///
/// On Unix this is an advisory `flock` on the lock file: the kernel drops it
/// when the holder exits or crashes, so there is no stale-lock guessing and a
/// lock that is still held is never taken over, however long the refresh
/// takes.
#[cfg(unix)]
struct RefreshLock(#[allow(dead_code)] std::fs::File);

#[cfg(unix)]
impl RefreshLock {
    async fn acquire(dir: &Path) -> Result<Self, AuthError> {
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::AsRawFd;
        std::fs::create_dir_all(dir).map_err(|e| AuthError::Io(e.to_string()))?;
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(dir.join(LOCK_FILE))
            .map_err(|e| AuthError::Io(e.to_string()))?;
        // Up to 90 s: longer than one refresh can hold the lock.
        for _ in 0..900 {
            // SAFETY: flock on a file descriptor this function owns.
            if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self(f));
            }
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::WouldBlock
                && err.raw_os_error() != Some(libc::EINTR)
            {
                return Err(AuthError::Io(err.to_string()));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(AuthError::Io(
            "timed out waiting for the refresh lock".into(),
        ))
    }
}

/// Elsewhere: an exclusive-create lock file, taken over only when older than
/// the longest a refresh can hold it.
#[cfg(not(unix))]
struct RefreshLock(PathBuf);

#[cfg(not(unix))]
impl RefreshLock {
    async fn acquire(dir: &Path) -> Result<Self, AuthError> {
        let p = dir.join(LOCK_FILE);
        for _ in 0..900 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&p)
            {
                Ok(_) => return Ok(Self(p)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&p)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > Duration::from_secs(300));
                    if stale {
                        let _ = std::fs::remove_file(&p);
                        continue;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => return Err(AuthError::Io(e.to_string())),
            }
        }
        Err(AuthError::Io(
            "timed out waiting for the refresh lock".into(),
        ))
    }
}

#[cfg(not(unix))]
impl Drop for RefreshLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The session's access token, refreshed first if it expires within two
/// minutes. `Ok(None)` when there is no session.
pub async fn fresh_access_token(
    http: &reqwest::Client,
    dir: &Path,
    signer: &dyn DeviceSigner,
) -> Result<Option<Session>, AuthError> {
    let Some(s) = load_session(dir) else {
        return Ok(None);
    };
    if s.access_fresh(now_unix()) {
        return Ok(Some(s));
    }
    let _lock = RefreshLock::acquire(dir).await?;
    // Another process may have refreshed while we waited.
    let Some(s) = load_session(dir) else {
        return Ok(None);
    };
    if s.access_fresh(now_unix()) {
        return Ok(Some(s));
    }
    let next = refresh(http, &s, signer).await?;
    save_session(dir, &next)?;
    Ok(Some(next))
}

/// Keep `cell` holding a fresh access token for as long as the session
/// lasts. When the session ends (revoked, deprovisioned) the cell is cleared
/// and the loop waits for a new sign-in (a new session file).
pub async fn run_refresher(dir: PathBuf, signer: Arc<dyn DeviceSigner>, cell: TokenCell) {
    let http = http_client();
    let mut backoff = 30u64;
    loop {
        let wait = match fresh_access_token(&http, &dir, signer.as_ref()).await {
            Ok(Some(s)) => {
                cell.set(Some(s.access_token.clone()));
                backoff = 30;
                (s.access_expires_at - REFRESH_MARGIN_SECS - now_unix()).clamp(15, 3600) as u64
            }
            Ok(None) => {
                cell.set(None);
                30
            }
            Err(AuthError::SignInNeeded(why)) => {
                tracing::warn!("ligus-auth: sign-in needed ({why})");
                cell.set(None);
                // Keep the file so status can explain; poll for a new sign-in.
                60
            }
            Err(e) => {
                tracing::warn!("ligus-auth: refresh failed, retrying: {e}");
                backoff = (backoff * 2).min(300);
                backoff
            }
        };
        tokio::time::sleep(Duration::from_secs(wait)).await;
    }
}

/// For synchronous callers (the CLI): fill `cell` once now, then keep it
/// fresh from a background thread with its own small runtime.
pub fn start_background_refresh(
    dir: PathBuf,
    signer: Arc<dyn DeviceSigner>,
    cell: TokenCell,
) -> Result<(), AuthError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| AuthError::Io(e.to_string()))?;
    let first = rt.block_on(fresh_access_token(&http_client(), &dir, signer.as_ref()));
    match first {
        Ok(Some(s)) => cell.set(Some(s.access_token)),
        Ok(None) => return Ok(()),
        Err(e) => return Err(e),
    }
    std::thread::Builder::new()
        .name("ligus-auth-refresh".into())
        .spawn(move || rt.block_on(run_refresher(dir, signer, cell)))
        .map_err(|e| AuthError::Io(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Cross-language vector: the same inputs, signed by the same seed, as
    /// `TestDeviceProofVectorFrozen` in the relay repository's ligusauth
    /// package. If either side drifts, sign-in breaks; this catches it.
    #[test]
    fn device_proof_matches_ligus_auth() {
        let msg = device_proof_message(
            "home",
            "refresh_token",
            "bm9uY2U",
            "5d0b7c1e-2f43-4a8e-9a51-0c6f3d2e1b07",
            "rt-123",
        );
        assert_eq!(hex(&msg), "6c696775732d617574682d6465766963652d70726f6f662d763100686f6d6500726566726573685f746f6b656e00626d39755932550035643062376331652d326634332d346138652d396135312d3063366633643265316230370032383165366439643761623363653837376635653361633862343334363461316130353564323533313564623364313839313466666334333133303736396539");
        let kp = SigningKeypair::from_seed(&[7u8; 32]).unwrap();
        assert_eq!(
            b64u(&kp.public_key_bytes()),
            "6kpsY-KcUgq-9VB7Ey7F-ZVHdq6-vnuSQh7qaRRG0iw"
        );
        assert_eq!(kp.sign(&msg).to_vec(), unhex("fa484b92be85148eda71fa2535fa96ec6455f6ea271a802639c134a2600100dcf7191b061e7a28aff546558d293e24a6906b4d25c7dc713d7a155f00d50a830c"));
        // Frozen context bytes.
        assert_eq!(
            hex(DEVICE_PROOF_CONTEXT),
            "6c696775732d617574682d6465766963652d70726f6f662d763100"
        );
    }

    #[test]
    fn auth_url_rules() {
        assert_eq!(
            parse_auth_url("https://auth.example.com/t/home/").unwrap(),
            ("https://auth.example.com/t/home".into(), "home".into())
        );
        assert_eq!(
            parse_auth_url("https://ex.com/ligus/t/acme-2").unwrap().1,
            "acme-2"
        );
        assert!(
            parse_auth_url("http://auth.example.com/t/home").is_err(),
            "plain http off loopback"
        );
        assert!(parse_auth_url("http://127.0.0.1:8090/t/home").is_ok());
        assert!(
            parse_auth_url("https://auth.example.com").is_err(),
            "needs /t/<tenant>"
        );
        assert!(parse_auth_url("https://auth.example.com/t/Home").is_err());
        assert!(parse_auth_url("https://u:p@auth.example.com/t/home").is_err());
        assert!(parse_auth_url("https://auth.example.com/t/home?x=1").is_err());
    }

    /// A saved or loaded session's refresh token is masked in agent output
    /// (Track C's redaction), like Ligus's other secrets.
    #[test]
    fn session_refresh_token_is_redacted_in_replies() {
        let dir = tempfile::tempdir().unwrap();
        let refresh = "rt-7Qm2xVb9Lk4pZr8sWc1N";
        let s = Session {
            auth_url: "https://a/t/h".into(),
            tenant: "h".into(),
            client_id: "ligus-desktop".into(),
            device_id: "d".into(),
            refresh_token: refresh.into(),
            refresh_nonce: None,
            access_token: "hrt1.eyJzdWIiOiJ4In0.c2lnbmF0dXJlMTIz".into(),
            access_expires_at: 10,
            sub: "h:u".into(),
            workspaces: vec![],
        };
        save_session(dir.path(), &s).unwrap();
        let out = crate::secret_redact::redacted(&format!("refresh={refresh} access={}", s.access_token));
        assert!(!out.contains(refresh) && !out.contains("hrt1."), "{out}");
    }

    #[test]
    fn session_file_is_private_and_debug_redacts() {
        let dir = tempfile::tempdir().unwrap();
        let s = Session {
            auth_url: "https://a/t/h".into(),
            tenant: "h".into(),
            client_id: "ligus-desktop".into(),
            device_id: "d".into(),
            refresh_token: "secret-refresh".into(),
            refresh_nonce: None,
            access_token: "hrt1.secret".into(),
            access_expires_at: 10,
            sub: "h:u".into(),
            workspaces: vec!["w".into()],
        };
        save_session(dir.path(), &s).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(session_path(dir.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let dbg = format!("{s:?}");
        assert!(!dbg.contains("secret"), "{dbg}");
        assert_eq!(
            load_session(dir.path()).unwrap().refresh_token,
            "secret-refresh"
        );
        delete_session(dir.path());
        assert!(load_session(dir.path()).is_none());
    }

    /// E-4: a refresh lock that is still held is never taken over, however
    /// old its file looks. The old lock treated any lock file older than a
    /// minute as left over from a crash and deleted it, but a refresh can hold
    /// the lock that long (two token requests, each up to 40 s), so a second
    /// process then spent the same refresh token and ligus-auth revoked the
    /// whole session as stolen.
    #[tokio::test]
    async fn held_refresh_lock_is_never_stolen() {
        let dir = tempfile::tempdir().unwrap();
        let held = RefreshLock::acquire(dir.path()).await.unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(600);
        std::fs::OpenOptions::new()
            .write(true)
            .open(dir.path().join(LOCK_FILE))
            .unwrap()
            .set_modified(old)
            .unwrap();
        let second =
            tokio::time::timeout(Duration::from_millis(800), RefreshLock::acquire(dir.path())).await;
        assert!(
            !matches!(second, Ok(Ok(_))),
            "a held refresh lock was taken over"
        );
        drop(held);
        let third =
            tokio::time::timeout(Duration::from_secs(2), RefreshLock::acquire(dir.path())).await;
        assert!(matches!(third, Ok(Ok(_))), "released lock is acquirable");
    }

    // ── mock ligus-auth: device flow + device proof + refresh rotation ──────

    use axum::{extract::State, routing::post, Form, Json, Router};
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Mock {
        base: String,
        approved: bool,
        polls: u32,
        refresh: String,
        issued: u32,
        bad_proofs: u32,
    }
    type Shared = Arc<Mutex<Mock>>;

    async fn mock_device(
        State(m): State<Shared>,
        Form(_f): Form<HashMap<String, String>>,
    ) -> Json<serde_json::Value> {
        let base = m.lock().unwrap().base.clone();
        Json(serde_json::json!({
            "device_code": "dc-1", "user_code": "BCDF-GHJK",
            "verification_uri": format!("{base}/device"),
            "verification_uri_complete": format!("{base}/device?user_code=BCDF-GHJK"),
            "expires_in": 600, "interval": 5
        }))
    }

    fn proof_ok(f: &HashMap<String, String>, grant: &str, cred: &str) -> bool {
        let (Some(id), Some(key), Some(n), Some(sig)) = (
            f.get("device_id"),
            f.get("device_key"),
            f.get("device_nonce"),
            f.get("device_sig"),
        ) else {
            return false;
        };
        let dec = |s: &str| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(s)
                .unwrap_or_default()
        };
        hive_core::crypto::verify(
            &dec(key),
            &device_proof_message("home", grant, n, id, cred),
            &dec(sig),
        )
        .is_ok()
    }

    async fn mock_token(
        State(m): State<Shared>,
        Form(f): Form<HashMap<String, String>>,
    ) -> (axum::http::StatusCode, Json<serde_json::Value>) {
        use axum::http::StatusCode;
        let mut m = m.lock().unwrap();
        let err = |e: &str| {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e, "device_nonce": "nonce-x"})),
            )
        };
        let grant = f.get("grant_type").cloned().unwrap_or_default();
        let cred = match grant.as_str() {
            GRANT_DEVICE => {
                m.polls += 1;
                if !m.approved {
                    return err("authorization_pending");
                }
                f.get("device_code").cloned().unwrap_or_default()
            }
            "refresh_token" => {
                let rt = f.get("refresh_token").cloned().unwrap_or_default();
                if rt != m.refresh {
                    return err("invalid_grant");
                }
                rt
            }
            _ => return err("unsupported_grant_type"),
        };
        if !f.contains_key("device_nonce") {
            return err("use_device_nonce");
        }
        if !proof_ok(&f, &grant, &cred) {
            m.bad_proofs += 1;
            return err("invalid_grant");
        }
        m.issued += 1;
        m.refresh = format!("rt-{}", m.issued);
        (
            StatusCode::OK,
            Json(serde_json::json!({
                "access_token": format!("hrt1.access-{}", m.issued), "token_type": "Bearer", "expires_in": 60,
                "refresh_token": m.refresh, "device_nonce": "nonce-next", "sub": "home:u1", "tid": "home", "workspaces": ["ws-home"]
            })),
        )
    }

    async fn mock_server() -> (String, Shared) {
        let m: Shared = Arc::default();
        let app = Router::new()
            .route("/t/home/device_authorization", post(mock_device))
            .route("/t/home/token", post(mock_token))
            .with_state(m.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/t/home", listener.local_addr().unwrap());
        m.lock().unwrap().base = base.clone();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, m)
    }

    #[tokio::test]
    async fn device_sign_in_then_refresh_with_device_proof() {
        let (base, m) = mock_server().await;
        let http = http_client();
        let dev = DeviceKey::new(uuid::Uuid::new_v4(), SigningKeypair::generate().unwrap());
        let ds = start_device(&http, &base, "ligus-desktop", "Mac")
            .await
            .unwrap();
        assert_eq!(ds.user_code, "BCDF-GHJK");
        assert!(matches!(
            poll_device(&http, &base, "ligus-desktop", &ds.device_code, &dev)
                .await
                .unwrap(),
            Poll::Pending
        ));
        m.lock().unwrap().approved = true;
        let Poll::SignedIn(s) = poll_device(&http, &base, "ligus-desktop", &ds.device_code, &dev)
            .await
            .unwrap()
        else {
            panic!("expected sign-in");
        };
        assert_eq!(s.workspaces, vec!["ws-home".to_string()]);
        assert_eq!(s.refresh_nonce.as_deref(), Some("nonce-next"));
        let dir = tempfile::tempdir().unwrap();
        save_session(dir.path(), &s).unwrap();
        // expires_in 60 is inside the 2-minute margin: the next read refreshes.
        let next = fresh_access_token(&http, dir.path(), &dev)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(next.access_token, "hrt1.access-2");
        assert_eq!(
            load_session(dir.path()).unwrap().refresh_token,
            "rt-2",
            "rotated token is saved"
        );
        assert_eq!(m.lock().unwrap().bad_proofs, 0);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), RefreshLock::acquire(dir.path()))
                .await
                .is_ok_and(|r| r.is_ok()),
            "lock released"
        );

        // Another device's key cannot use this session.
        let thief = DeviceKey::new(uuid::Uuid::new_v4(), SigningKeypair::generate().unwrap());
        assert!(matches!(
            refresh(&http, &next, &thief).await,
            Err(AuthError::SignInNeeded(_))
        ));
        // A session the server no longer honours asks for a new sign-in.
        m.lock().unwrap().refresh = "something-else".into();
        assert!(matches!(
            refresh(&http, &next, &dev).await,
            Err(AuthError::SignInNeeded(_))
        ));
    }

    #[tokio::test]
    async fn relay_client_reads_the_cell() {
        let cell = TokenCell::default();
        let rc = crate::RelayClient::new("http://127.0.0.1:9")
            .with_auth(Some("static".into()))
            .with_auth_cell(Some(cell.clone()));
        let req = |rc: &crate::RelayClient| {
            rc.authed(reqwest::Client::new().get("http://127.0.0.1:9/x"))
                .build()
                .unwrap()
        };
        let bearer = |r: reqwest::Request| {
            r.headers()
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(bearer(req(&rc)), "Bearer static", "empty cell falls back");
        cell.set(Some("hrt1.fresh".into()));
        assert_eq!(bearer(req(&rc)), "Bearer hrt1.fresh");
        cell.set(Some("hrt1.fresher".into()));
        assert_eq!(
            bearer(req(&rc)),
            "Bearer hrt1.fresher",
            "a refresh is picked up without rebuilding the client"
        );
        assert!(!format!("{cell:?}").contains("fresher"));
    }

    #[tokio::test]
    async fn verification_link_must_stay_on_origin() {
        let app = Router::new().route(
            "/t/home/device_authorization",
            post(|| async {
                Json(serde_json::json!({"device_code": "d", "user_code": "U", "verification_uri": "https://evil.example/device", "interval": 5, "expires_in": 600}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/t/home", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        assert!(start_device(&http_client(), &base, "ligus-cli", "box")
            .await
            .is_err());
    }
}

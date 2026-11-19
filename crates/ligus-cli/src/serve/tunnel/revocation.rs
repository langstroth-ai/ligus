//! Phone revocation, all three parts, with per-step status (Phase B).
//!
//! When this box folds a `DeviceRevoked` of a phone it has paired (by any
//! path: a sync round, a local write, another process on the same data dir,
//! or the phone revoking itself), [`TunnelHost::revoke_device`] does at once,
//! locally:
//!
//! - marks the device's pairings revoked in `phones.json`: live tunnel
//!   sessions end (GOAWAY revoked), a new session or a request on an open one
//!   is refused, and this box sends it no more pushes;
//!
//! and then, in the background, with retries ([`run`]):
//!
//! 1. **tunnel**: tells the relay to drop the pairing, so it refuses the
//!    phone's credential (4003) and closes what it still splices
//!    (`POST /v1/tunnel/revoke`, signed with the host key; a relay without
//!    that route confirms through the control connection's `revoke` ack);
//! 2. **push**: the same relay call deletes the phone's APNs token;
//! 3. **session**: ends the device's langstroth-auth session through the
//!    operator API when this box is configured for it
//!    (`LIGUS_AUTH_ADMIN_URL`, `LIGUS_AUTH_ADMIN_TENANT`,
//!    `LIGUS_AUTH_ADMIN_TOKEN_FILE`: a 0600 file with the tenant's
//!    device-revocation token); otherwise "not applicable".
//!
//! The status lives in `tunnel/revocations.json` and is shown by
//! `GET /v1/revocations`, `ligus phones list`, and (through the presence
//! channel) the desktop.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use ligus_runtime::revocation_status::{DeviceRevocationStatus, RevocationReport, StepState, StepStatus, MAX_REPORTED};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::{b64u, ctx, now, random_bytes, read_json, write_json, HostKeys, RelaySettings, TunnelHost};

/// How long finished entries are kept.
const KEEP_SECS: u64 = 30 * 86_400;
/// Entries kept at most.
const MAX_ENTRIES: usize = 64;
/// How often the executor looks for due work.
pub const TICK: Duration = Duration::from_millis(500);

/// Which step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Tunnel,
    Push,
    Session,
}

/// One revoked device on this box.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    #[serde(flatten)]
    pub status: DeviceRevocationStatus,
    /// This box's pairings of the device.
    pub pids: Vec<String>,
    /// Pairings the relay has confirmed dropping.
    #[serde(default)]
    pub relay_done: BTreeSet<String>,
    /// Sessions the relay reported closing, summed over pairings.
    #[serde(default)]
    pub relay_closed: u64,
    /// Whether the relay reported removing a push token.
    #[serde(default)]
    pub push_removed: bool,
    /// When the next attempt is due (unix seconds; 0 = now).
    #[serde(default)]
    pub next_try: u64,
}

impl Entry {
    pub fn device(&self) -> Option<Uuid> {
        Uuid::parse_str(&self.status.device_id).ok()
    }

    fn step(&mut self, s: Step) -> &mut StepStatus {
        match s {
            Step::Tunnel => &mut self.status.tunnel,
            Step::Push => &mut self.status.push,
            Step::Session => &mut self.status.session,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct LedgerFile {
    #[serde(default)]
    v: u32,
    #[serde(default)]
    devices: Vec<Entry>,
}

/// `tunnel/revocations.json`.
pub struct Ledger {
    path: PathBuf,
    inner: Mutex<Vec<Entry>>,
}

/// Retry backoff after `attempts` failures: 2, 4, 8 … 60 s.
fn backoff(attempts: u32) -> u64 {
    2u64.saturating_pow(attempts.clamp(1, 6)).min(60)
}

impl Ledger {
    pub fn file(dir: &Path) -> PathBuf {
        dir.join("revocations.json")
    }

    pub fn load(dir: &Path) -> Self {
        let path = Self::file(dir);
        let devices = read_json::<LedgerFile>(&path).ok().flatten().map(|f| f.devices).unwrap_or_default();
        Self { path, inner: Mutex::new(devices) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Entry>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, devices: &[Entry]) {
        if let Err(e) = write_json(&self.path, &LedgerFile { v: 1, devices: devices.to_vec() }) {
            eprintln!("remote: saving the revocation status failed: {e:#}");
        }
    }

    fn update<T>(&self, f: impl FnOnce(&mut Vec<Entry>) -> T) -> T {
        let mut d = self.lock();
        let out = f(&mut d);
        let t = now();
        d.retain(|e| !e.status.complete() || t.saturating_sub(e.status.revoked_at.max(0) as u64) < KEEP_SECS);
        if d.len() > MAX_ENTRIES {
            d.sort_by_key(|e| std::cmp::Reverse(e.status.revoked_at));
            d.truncate(MAX_ENTRIES);
        }
        self.save(&d);
        out
    }

    pub fn list(&self) -> Vec<Entry> {
        let mut v = self.lock().clone();
        v.sort_by_key(|e| std::cmp::Reverse(e.status.revoked_at));
        v
    }

    /// Start (or extend, with new pairings) the revocation of `device`.
    pub fn begin(&self, device: Uuid, name: &str, pids: &[String], session: Option<(StepState, &str)>) {
        let t = now() as i64;
        self.update(|d| {
            let id = device.to_string();
            let pending = |detail: &str| StepStatus { state: StepState::Pending, detail: detail.into(), at: t, attempts: 0 };
            match d.iter_mut().find(|e| e.status.device_id == id) {
                Some(e) => {
                    let mut added = false;
                    for p in pids {
                        if !e.pids.contains(p) {
                            e.pids.push(p.clone());
                            added = true;
                        }
                    }
                    if added {
                        e.status.tunnel = pending("closed on this box; telling the relay");
                        e.status.push = pending("this box stopped pushing; removing the relay's push token");
                        e.next_try = 0;
                    }
                }
                None => {
                    let session = match session {
                        Some((state, detail)) => StepStatus { state, detail: detail.into(), at: t, attempts: 0 },
                        None => pending("ending the langstroth-auth session"),
                    };
                    d.push(Entry {
                        status: DeviceRevocationStatus {
                            device_id: id,
                            name: name.to_string(),
                            revoked_at: t,
                            tunnel: pending("closed on this box; telling the relay"),
                            push: pending("this box stopped pushing; removing the relay's push token"),
                            session,
                        },
                        pids: pids.to_vec(),
                        relay_done: BTreeSet::new(),
                        relay_closed: 0,
                        push_removed: false,
                        next_try: 0,
                    });
                }
            }
        });
    }

    /// The relay confirmed dropping `pid` (`closed`/`removed`: what it
    /// reported, when it said). When every pairing of the device is
    /// confirmed, the tunnel and push steps are done.
    pub fn relay_confirmed(&self, pid: &str, closed: Option<u64>, removed: Option<bool>) -> bool {
        let t = now() as i64;
        self.update(|d| {
            let Some(e) = d.iter_mut().find(|e| e.pids.iter().any(|p| p == pid)) else { return false };
            e.relay_done.insert(pid.to_string());
            e.relay_closed += closed.unwrap_or(0);
            e.push_removed |= removed.unwrap_or(false);
            if e.pids.iter().all(|p| e.relay_done.contains(p)) {
                let n = e.relay_closed;
                e.status.tunnel = StepStatus {
                    state: StepState::Done,
                    detail: format!("the relay refuses this phone and closed {n} live session{}", if n == 1 { "" } else { "s" }),
                    at: t,
                    attempts: e.status.tunnel.attempts,
                };
                e.status.push = StepStatus {
                    state: StepState::Done,
                    detail: if e.push_removed { "the relay deleted its push token".into() } else { "the relay holds no push token for it".into() },
                    at: t,
                    attempts: e.status.push.attempts,
                };
            }
            true
        })
    }

    /// Record an attempt's outcome for one step of `device`.
    pub fn set(&self, device: Uuid, step: Step, state: StepState, detail: &str) {
        let t = now();
        self.update(|d| {
            let id = device.to_string();
            let Some(e) = d.iter_mut().find(|e| e.status.device_id == id) else { return };
            let s = e.step(step);
            if s.state == StepState::Done && state != StepState::Done {
                return;
            }
            s.attempts += u32::from(state != StepState::NotApplicable);
            s.state = state;
            s.detail = detail.to_string();
            s.at = t as i64;
            let attempts = s.attempts;
            if matches!(state, StepState::Failed | StepState::Pending) {
                e.next_try = e.next_try.max(t + backoff(attempts));
            }
        });
    }

    /// Count an attempt without changing the state (an unacked relay call).
    pub fn attempted(&self, device: Uuid, step: Step) {
        self.update(|d| {
            let id = device.to_string();
            if let Some(e) = d.iter_mut().find(|e| e.status.device_id == id) {
                e.step(step).attempts += 1;
            }
        });
    }

    /// Unsettled entries whose next attempt is due.
    pub fn due(&self) -> Vec<Entry> {
        let t = now();
        self.lock().iter().filter(|e| !e.status.complete() && e.next_try <= t).cloned().collect()
    }

    /// Schedule `device` for its next attempt now. False when unknown.
    pub fn retry(&self, device: Uuid) -> bool {
        self.update(|d| {
            let id = device.to_string();
            match d.iter_mut().find(|e| e.status.device_id == id) {
                Some(e) => {
                    e.next_try = 0;
                    true
                }
                None => false,
            }
        })
    }

    /// Hold every unsettled entry until `at` (used after a pass).
    fn defer(&self, device: Uuid, at: u64) {
        self.update(|d| {
            let id = device.to_string();
            if let Some(e) = d.iter_mut().find(|e| e.status.device_id == id) {
                e.next_try = e.next_try.max(at);
            }
        });
    }

    pub fn report(&self, host_id: &str) -> RevocationReport {
        RevocationReport {
            host_id: host_id.to_string(),
            at: now() as i64,
            devices: self.list().into_iter().take(MAX_REPORTED).map(|e| e.status).collect(),
            signature: Vec::new(),
        }
    }
}

// ── The relay call (POST /v1/tunnel/revoke) ──────────────────────────────────

/// The signed preimage, byte-identical to the relay's `hostRevokeMessage`:
/// `ctx("host-revoke", hostId, pid, u64be(exp), u64be(ts), nonce)`.
pub fn host_revoke_message(host_id: &str, pid: &str, exp: u64, ts: u64, nonce: &[u8; 16]) -> Vec<u8> {
    ctx("host-revoke", &[host_id.as_bytes(), pid.as_bytes(), &exp.to_be_bytes(), &ts.to_be_bytes(), nonce])
}

/// What the relay did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayRevoked {
    pub sessions_closed: u64,
    pub push_token_removed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayRevokeError {
    /// The relay predates the route (404/405): the control `revoke` ack stands in.
    Unsupported,
    /// The relay refused (status, code).
    Refused(u16, String),
    /// Couldn't reach it.
    Network(String),
    /// The relay's clock is this many seconds ahead of ours (signed).
    ClockSkew(i64),
}

impl std::fmt::Display for RelayRevokeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => write!(f, "the relay predates POST /v1/tunnel/revoke"),
            Self::Refused(s, c) => write!(f, "the relay refused it ({s} {c})"),
            Self::Network(e) => write!(f, "couldn't reach the relay: {e}"),
            Self::ClockSkew(d) => write!(f, "this box's clock is {d} s off the relay's"),
        }
    }
}

/// Ask the relay to drop `pid` (until `exp`), signed with the host key.
pub async fn relay_revoke(http: &reqwest::Client, settings: &RelaySettings, keys: &HostKeys, pid: &str, exp: u64) -> Result<RelayRevoked, RelayRevokeError> {
    relay_revoke_at(http, settings, keys, pid, exp, now()).await
}

/// [`relay_revoke`] with `ts` (the relay's time as this box best knows it).
pub async fn relay_revoke_at(http: &reqwest::Client, settings: &RelaySettings, keys: &HostKeys, pid: &str, exp: u64, ts: u64) -> Result<RelayRevoked, RelayRevokeError> {
    let nonce: [u8; 16] = random_bytes();
    let sig = keys.sign(&host_revoke_message(keys.host_id(), pid, exp, ts, &nonce));
    let body = json!({ "v": 1, "hostId": keys.host_id(), "pid": pid, "exp": exp, "ts": ts, "nonce": b64u(nonce), "sig": b64u(sig) });
    let url = format!("{}/v1/tunnel/revoke", settings.relay);
    let resp = http.post(url).json(&body).timeout(Duration::from_secs(10)).send().await.map_err(|e| RelayRevokeError::Network(e.without_url().to_string()))?;
    let status = resp.status().as_u16();
    let v: serde_json::Value = resp.json().await.unwrap_or_default();
    match status {
        200 if v["ok"] == json!(true) => Ok(RelayRevoked {
            sessions_closed: v["sessionsClosed"].as_u64().unwrap_or(0),
            push_token_removed: v["pushTokenRemoved"].as_bool().unwrap_or(false),
        }),
        404 | 405 => Err(RelayRevokeError::Unsupported),
        401 if v["error"] == json!("clock_skew") => match v["now"].as_u64() {
            Some(relay_now) => Err(RelayRevokeError::ClockSkew(relay_now as i64 - now() as i64)),
            None => Err(RelayRevokeError::Refused(401, "clock_skew".into())),
        },
        s => Err(RelayRevokeError::Refused(s, v["error"].as_str().unwrap_or("?").chars().take(40).collect())),
    }
}

// ── langstroth-auth (the session step) ───────────────────────────────────────

/// The operator API this box may use to end a device's sign-in session.
#[derive(Clone)]
pub struct AuthAdmin {
    /// The admin listener's base URL (no trailing `/`).
    pub url: String,
    pub tenant: String,
    pub token: String,
}

impl std::fmt::Debug for AuthAdmin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthAdmin").field("url", &self.url).field("tenant", &self.tenant).field("token", &"<redacted>").finish()
    }
}

impl AuthAdmin {
    /// `LIGUS_AUTH_ADMIN_URL` + `LIGUS_AUTH_ADMIN_TENANT` +
    /// `LIGUS_AUTH_ADMIN_TOKEN_FILE` (a 0600 file holding that tenant's
    /// device-revocation token, `device_revoke_token_*` in langstroth-auth's
    /// config; never the operator token, never an env var). `None` when any
    /// is unset.
    pub fn from_env() -> Option<Self> {
        let url = crate::env_opt("LIGUS_AUTH_ADMIN_URL")?.trim().trim_end_matches('/').to_string();
        let tenant = crate::env_opt("LIGUS_AUTH_ADMIN_TENANT")?.trim().to_string();
        let file = crate::env_opt("LIGUS_AUTH_ADMIN_TOKEN_FILE")?;
        Self::from_parts(&url, &tenant, std::path::Path::new(file.trim()))
    }

    /// The token comes only from a file, readable by its owner alone.
    pub fn from_parts(url: &str, tenant: &str, file: &Path) -> Option<Self> {
        let (url, tenant) = (url.to_string(), tenant.to_string());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::metadata(file).is_ok_and(|m| m.permissions().mode() & 0o077 != 0) {
                eprintln!("remote: LIGUS_AUTH_ADMIN_TOKEN_FILE is readable by others (chmod 600 it); phone sign-in sessions won't be ended on revocation");
                return None;
            }
        }
        let token = match std::fs::read_to_string(file) {
            Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ => {
                eprintln!("remote: LIGUS_AUTH_ADMIN_TOKEN_FILE is unreadable or empty; phone sign-in sessions won't be ended on revocation");
                return None;
            }
        };
        ligus_runtime::secret_redact::register(&token);
        // https, or plain http to loopback only ("http://localhost.example"
        // is not loopback); a tenant id is one path segment.
        let loopback = reqwest::Url::parse(&url).is_ok_and(|u| u.scheme() == "http" && matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")));
        let https = reqwest::Url::parse(&url).is_ok_and(|u| u.scheme() == "https" && u.host_str().is_some());
        if tenant.is_empty() || !tenant.chars().all(|c| c.is_ascii_alphanumeric() || "._:-".contains(c)) {
            eprintln!("remote: LIGUS_AUTH_ADMIN_TENANT is not a tenant id; phone sign-in sessions won't be ended on revocation");
            return None;
        }
        if !(https || loopback) {
            eprintln!("remote: LIGUS_AUTH_ADMIN_URL must be https (or loopback); phone sign-in sessions won't be ended on revocation");
            return None;
        }
        Some(Self { url, tenant, token })
    }

    /// End `device`'s session; returns how many refresh tokens it revoked.
    pub async fn revoke_device(&self, http: &reqwest::Client, device: Uuid) -> Result<u64, String> {
        let url = format!("{}/admin/v1/tenants/{}/devices/{device}/revoke", self.url, self.tenant);
        let resp = http
            .post(url)
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| format!("couldn't reach langstroth-auth: {}", e.without_url()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("langstroth-auth answered {}", status.as_u16()));
        }
        let v: serde_json::Value = resp.json().await.map_err(|_| "langstroth-auth sent an unreadable answer".to_string())?;
        Ok(v["revoked"].as_u64().unwrap_or(0))
    }
}

// ── The executor ─────────────────────────────────────────────────────────────

/// Do what's due for one entry.
async fn work(host: &TunnelHost, e: Entry) {
    let Some(device) = e.device() else { return };
    // Tunnel + push: one relay call per unconfirmed pairing.
    if !(e.status.tunnel.state.is_settled() && e.status.push.state.is_settled()) {
        for pid in e.pids.iter().filter(|p| !e.relay_done.contains(*p)) {
            let exp = host.revocation_exp(pid);
            let at = |off: i64| (now() as i64).saturating_add(off).max(0) as u64;
            let mut res = relay_revoke_at(&host.http, &host.settings, &host.keys, pid, exp, at(host.relay_clock_offset())).await;
            if let Err(RelayRevokeError::ClockSkew(off)) = res {
                // The relay told us its time: sign with it (once per attempt).
                eprintln!("remote: this box's clock is {off} s off the relay's; signing revocations with the relay's time");
                host.set_relay_clock_offset(off);
                res = relay_revoke_at(&host.http, &host.settings, &host.keys, pid, exp, at(off)).await;
            }
            match res {
                Ok(r) => {
                    host.ledger.relay_confirmed(pid, Some(r.sessions_closed), Some(r.push_token_removed));
                }
                Err(RelayRevokeError::Unsupported) => {
                    // An older relay: its control `revoke` ack confirms (on_ack).
                    host.ledger.attempted(device, Step::Tunnel);
                    host.ledger.defer(device, now() + 30);
                    host.send_revocations();
                }
                Err(err) => {
                    let detail = format!("closed on this box; {err}; retrying");
                    host.ledger.set(device, Step::Tunnel, StepState::Failed, &detail);
                    host.ledger.set(device, Step::Push, StepState::Failed, &format!("this box stopped pushing; {err}; retrying"));
                }
            }
        }
    }
    if !e.status.session.state.is_settled() {
        match host.auth_admin() {
            None => host.ledger.set(device, Step::Session, StepState::NotApplicable, NO_AUTH_ADMIN),
            Some(a) => match a.revoke_device(&host.http, device).await {
                Ok(0) => host.ledger.set(device, Step::Session, StepState::Done, "langstroth-auth had no session for this device"),
                Ok(n) => host.ledger.set(device, Step::Session, StepState::Done, &format!("langstroth-auth ended its session ({n} refresh token{})", if n == 1 { "" } else { "s" })),
                Err(err) => host.ledger.set(device, Step::Session, StepState::Failed, &format!("{err}; retrying")),
            },
        }
    }
}

pub const NO_AUTH_ADMIN: &str = "this box has no langstroth-auth operator access configured (a phone signs in through its paired box, not langstroth-auth)";

/// Background loop: work through due revocations (on a kick, or every [`TICK`]).
pub async fn run(host: Arc<TunnelHost>, mut stop: tokio::sync::watch::Receiver<bool>) {
    loop {
        for e in host.ledger.due() {
            work(&host, e).await;
        }
        tokio::select! {
            _ = tokio::time::sleep(TICK) => {}
            _ = host.revocation_kick.notified() => {}
            _ = super::stopped(&mut stop) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Byte-identical to the relay's TestTunnelRevoke_MessageLayout.
    #[test]
    fn host_revoke_preimage_matches_the_relay() {
        let (hid, pid) = ("AAAAAAAAAAAAAAAAAAAAAA", "BBBBBBBBBBBBBBBBBBBBBB");
        let got = host_revoke_message(hid, pid, 0x0102030405060708, 0x1112131415161718, &[7u8; 16]);
        let mut want = b"hive-tunnel/1 host-revoke\0".to_vec();
        want.extend_from_slice(hid.as_bytes());
        want.extend_from_slice(pid.as_bytes());
        want.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18]);
        want.extend_from_slice(&[7u8; 16]);
        assert_eq!(got, want);
    }

    #[cfg(unix)]
    #[test]
    fn the_auth_token_file_must_be_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("tok");
        std::fs::write(&f, "home-device-revoke-token-0123456789abcdef\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(AuthAdmin::from_parts("https://auth.example", "home", &f).is_none(), "a world-readable token file was used");
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
        let a = AuthAdmin::from_parts("https://auth.example", "home", &f).unwrap();
        assert_eq!(a.token, "home-device-revoke-token-0123456789abcdef");
        assert!(!format!("{a:?}").contains("0123456789"), "the token is redacted from Debug");
        // Plain http only to loopback; a tenant id is one path segment.
        assert!(AuthAdmin::from_parts("http://localhost.attacker.example", "home", &f).is_none());
        assert!(AuthAdmin::from_parts("http://127.0.0.1:9000", "home", &f).is_some());
        assert!(AuthAdmin::from_parts("https://auth.example", "home/../other", &f).is_none());
    }

    #[test]
    fn ledger_tracks_each_step_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let l = Ledger::load(dir.path());
        let d = Uuid::new_v4();
        l.begin(d, "Phone", &["p1".into(), "p2".into()], Some((StepState::NotApplicable, NO_AUTH_ADMIN)));
        assert_eq!(l.due().len(), 1);
        // One of two pairings confirmed: still pending.
        assert!(l.relay_confirmed("p1", Some(1), Some(true)));
        assert_eq!(l.list()[0].status.tunnel.state, StepState::Pending);
        l.set(d, Step::Tunnel, StepState::Failed, "relay down");
        assert_eq!(l.list()[0].status.tunnel.state, StepState::Failed);
        assert!(l.due().is_empty(), "backed off");
        assert!(l.retry(d));
        assert_eq!(l.due().len(), 1);
        assert!(l.relay_confirmed("p2", Some(0), Some(false)));
        let e = &l.list()[0];
        assert!(e.status.complete(), "{e:?}");
        assert_eq!(e.status.summary(), "revoked: tunnel cut, push removed, session: not applicable");
        assert!(e.status.tunnel.detail.contains("1 live session"));
        // A late failure never undoes a done step.
        l.set(d, Step::Tunnel, StepState::Failed, "late");
        assert_eq!(l.list()[0].status.tunnel.state, StepState::Done);
        // Survives a restart; an unknown pid or device changes nothing.
        let again = Ledger::load(dir.path());
        assert_eq!(again.list(), l.list());
        assert!(!again.relay_confirmed("nope", None, None));
        assert!(!again.retry(Uuid::new_v4()));
        // A new pairing of the same device reopens the relay steps.
        again.begin(d, "Phone", &["p3".into()], None);
        assert_eq!(again.list()[0].status.tunnel.state, StepState::Pending);
        assert_eq!(again.list()[0].status.session.state, StepState::NotApplicable);
    }
}

//! OAuth 2.0 device authorization (RFC 8628) for GitHub (the Ligus GitHub
//! App) and GitLab (an OAuth application per instance).
//!
//! The host starts the flow only when the person asks (never from a code
//! typed in from elsewhere: `device_flow_only_user_initiated`), shows the
//! user code and verification URI over its authenticated channel (the
//! desktop window, or the phone through the end-to-end encrypted tunnel),
//! and polls. The resulting tokens go straight into the secret store.
//!
//! - GitHub: `POST {web}/login/device/code`, poll and refresh at
//!   `POST {web}/login/oauth/access_token`. A GitHub App ignores `scope`
//!   (its permissions are the app's); refreshing a device-flow token needs
//!   no client secret. <https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-user-access-token-for-a-github-app>
//! - GitLab (17.9+; 17.3+ on by default): `POST {base}/oauth/authorize_device`,
//!   poll and refresh at `POST {base}/oauth/token`.
//!   <https://docs.gitlab.com/api/oauth2/>

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::conn::Connection;
use crate::tokens::Secret;
use crate::{ForgeError, ForgeKind, Result};

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

const GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// The scopes GitLab's device sign-in asks for: the API (merge requests,
/// reads) and git over HTTPS.
pub const GITLAB_SCOPES: &str = "api read_repository write_repository";

/// The endpoints and client of one connection's device flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceEndpoints {
    pub device_url: String,
    pub token_url: String,
    pub client_id: String,
    pub scope: Option<String>,
}

/// The Ligus GitHub App's client id: the connection's own (GHES), else
/// `LIGUS_GITHUB_APP_CLIENT_ID` at run time, else the one baked in at build
/// time. No client id is committed: the owner registers the app.
pub fn github_client_id(conn: &Connection) -> Option<String> {
    if let Some(c) = conn.client_id.clone().filter(|c| !c.trim().is_empty()) {
        return Some(c);
    }
    conn_is_github_com(conn)
        .then(|| std::env::var("LIGUS_GITHUB_APP_CLIENT_ID").ok().or_else(|| option_env!("LIGUS_GITHUB_APP_CLIENT_ID").map(str::to_string)))
        .flatten()
        .filter(|c| !c.trim().is_empty())
}

fn conn_is_github_com(conn: &Connection) -> bool {
    crate::spec::host_of(&conn.base_url).is_some_and(|h| h == "github.com")
}

/// The gitlab.com OAuth application's id (`LIGUS_GITLAB_CLIENT_ID` at run
/// time or build time); a self-managed instance needs its own application,
/// registered by its admin or the user, set on the connection.
pub fn gitlab_client_id(conn: &Connection) -> Option<String> {
    if let Some(c) = conn.client_id.clone().filter(|c| !c.trim().is_empty()) {
        return Some(c);
    }
    let is_com = crate::spec::host_of(&conn.base_url).is_some_and(|h| h == "gitlab.com");
    is_com
        .then(|| std::env::var("LIGUS_GITLAB_CLIENT_ID").ok().or_else(|| option_env!("LIGUS_GITLAB_CLIENT_ID").map(str::to_string)))
        .flatten()
        .filter(|c| !c.trim().is_empty())
}

/// Where a connection's device flow runs, or why it can't.
pub fn endpoints(conn: &Connection) -> Result<DeviceEndpoints> {
    let base = conn.base_url.trim_end_matches('/');
    match conn.kind {
        ForgeKind::GitHub => {
            let client_id = github_client_id(conn).ok_or_else(|| {
                ForgeError::Unsupported("device sign-in for GitHub isn't configured in this build (no Ligus GitHub App client id): use the gh login on this device or paste a fine-grained token".into())
            })?;
            Ok(DeviceEndpoints { device_url: format!("{base}/login/device/code"), token_url: format!("{base}/login/oauth/access_token"), client_id, scope: None })
        }
        ForgeKind::GitLab => {
            let client_id = gitlab_client_id(conn).ok_or_else(|| {
                ForgeError::Unsupported("device sign-in needs an OAuth application on this GitLab instance: ask its admin to register one (or register your own), set its application id on the connection, or paste a token".into())
            })?;
            Ok(DeviceEndpoints {
                device_url: format!("{base}/oauth/authorize_device"),
                token_url: format!("{base}/oauth/token"),
                client_id,
                scope: Some(GITLAB_SCOPES.into()),
            })
        }
        ForgeKind::Gitea => Err(ForgeError::Unsupported("Forgejo, Gitea and Codeberg have no device sign-in: paste a token".into())),
    }
}

/// A started device flow. The device code is a secret (it redeems the
/// token) and never leaves the host.
#[derive(Debug, Clone)]
pub struct DeviceStart {
    pub device_code: Secret,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// Tokens from a finished flow or a refresh.
#[derive(Debug, Clone)]
pub struct TokenSet {
    pub access: Secret,
    pub refresh: Option<Secret>,
    pub expires_in: Option<u64>,
    pub scope: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Poll {
    Pending,
    /// Slow down: poll at this interval from now on.
    SlowDown(u64),
    Done(TokenSet),
    Denied,
    Expired,
}

async fn post_form(url: &str, form: &[(&str, &str)]) -> Result<(reqwest::StatusCode, Value)> {
    crate::http::guard_url(url)?;
    let resp = crate::http::client()
        .post(url)
        .header("Accept", "application/json")
        .form(form)
        .send()
        .await
        .map_err(|e| ForgeError::Transport(format!("couldn't reach the code host: {}", e.without_url())))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| ForgeError::Transport(e.without_url().to_string()))?;
    if text.len() > 64 * 1024 {
        return Err(ForgeError::Transport("the code host's answer was too large".into()));
    }
    Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
}

fn token_set(v: &Value) -> Option<TokenSet> {
    let access = v["access_token"].as_str().filter(|t| !t.is_empty())?;
    Some(TokenSet {
        access: Secret::new(access),
        refresh: v["refresh_token"].as_str().filter(|t| !t.is_empty()).map(Secret::new),
        expires_in: v["expires_in"].as_u64(),
        scope: v["scope"].as_str().map(str::to_string),
    })
}

/// A verification address is https on the code host's own host (that of the
/// device endpoint), or plain http only when that host is a real loopback
/// host. Parsed, never matched by prefix.
fn verification_uri_ok(uri: &str, device_url: &str) -> bool {
    let (Ok(u), Ok(d)) = (url::Url::parse(uri), url::Url::parse(device_url)) else { return false };
    let (Some(host), Some(forge_host)) = (u.host_str(), d.host_str()) else { return false };
    if !u.username().is_empty() || u.password().is_some() || !crate::spec::same_host(host, forge_host) {
        return false;
    }
    let loopback = match u.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    u.scheme() == "https" || (u.scheme() == "http" && loopback)
}

/// Start a device flow (only ever on the person's own request).
pub async fn start(ep: &DeviceEndpoints) -> Result<DeviceStart> {
    let mut form = vec![("client_id", ep.client_id.as_str())];
    if let Some(s) = &ep.scope {
        form.push(("scope", s.as_str()));
    }
    let (status, v) = post_form(&ep.device_url, &form).await?;
    let code = v["device_code"].as_str().filter(|_| status.is_success());
    let Some(code) = code else {
        let why = v["error_description"].as_str().or(v["error"].as_str()).unwrap_or("no device code");
        return Err(ForgeError::Rejected(format!("the code host refused to start a device sign-in: {why}")));
    };
    let uri = v["verification_uri"].as_str().or(v["verification_url"].as_str()).unwrap_or("").to_string();
    if !verification_uri_ok(&uri, &ep.device_url) {
        return Err(ForgeError::Rejected("the code host gave a verification address that isn't https on the code host itself".into()));
    }
    Ok(DeviceStart {
        device_code: Secret::new(code),
        user_code: v["user_code"].as_str().unwrap_or("").chars().take(32).collect(),
        verification_uri: uri,
        expires_in: v["expires_in"].as_u64().unwrap_or(900).min(3600),
        interval: v["interval"].as_u64().unwrap_or(5).clamp(1, 60),
    })
}

/// Poll once.
pub async fn poll(ep: &DeviceEndpoints, device_code: &Secret) -> Result<Poll> {
    let (_, v) = post_form(&ep.token_url, &[("client_id", ep.client_id.as_str()), ("device_code", device_code.expose()), ("grant_type", GRANT)]).await?;
    if let Some(t) = token_set(&v) {
        return Ok(Poll::Done(t));
    }
    match v["error"].as_str().unwrap_or("") {
        "authorization_pending" => Ok(Poll::Pending),
        "slow_down" => Ok(Poll::SlowDown(v["interval"].as_u64().unwrap_or(10).clamp(1, 120))),
        "access_denied" => Ok(Poll::Denied),
        // GitHub's docs spell it both ways.
        "expired_token" | "token_expired" => Ok(Poll::Expired),
        other => Err(ForgeError::Rejected(format!(
            "device sign-in failed: {}",
            v["error_description"].as_str().unwrap_or(if other.is_empty() { "no token" } else { other })
        ))),
    }
}

/// Exchange a refresh token for a new access token.
pub async fn refresh(token_url: &str, client_id: &str, refresh_token: &Secret) -> Result<TokenSet> {
    let (_, v) = post_form(token_url, &[("client_id", client_id), ("grant_type", "refresh_token"), ("refresh_token", refresh_token.expose())]).await?;
    token_set(&v).ok_or_else(|| ForgeError::NotSignedIn("the device sign-in for this code host expired: sign in again".into()))
}

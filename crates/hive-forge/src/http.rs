//! A small JSON client over one forge API: base URL, auth header style,
//! error mapping, response size cap, same-host redirects only.

use std::sync::Arc;
use std::time::Duration;

use reqwest::{Method, StatusCode};
use serde_json::Value;

use crate::tokens::{CredentialSource, Purpose, Secret};
use crate::{ForgeError, Result};

/// How the token goes on the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthStyle {
    /// `Authorization: Bearer <t>` (GitHub; GitLab OAuth tokens and PATs).
    Bearer,
    /// `Authorization: token <t>` (Forgejo, Gitea).
    Token,
}

/// Largest response body read (JSON or a file), in bytes.
pub(crate) const MAX_BODY: usize = 8 * 1024 * 1024;

pub(crate) struct Api {
    pub client: reqwest::Client,
    pub base: String,
    pub style: AuthStyle,
    pub creds: Arc<dyn CredentialSource>,
    pub headers: Vec<(&'static str, &'static str)>,
}

/// Whether this process is a test build that must never reach a real code
/// host nor read the host's `gh` login: `cfg(test)`, any cargo test binary
/// (they run from `target/<profile>/deps/`), or `LIGUS_FORGE_DENY_REAL=1`
/// (for harnesses that spawn the real binary). Only loopback hosts are
/// reachable then, and `gh` (from `PATH`) is never run.
pub fn real_hosts_denied() -> bool {
    if cfg!(test) || std::env::var_os("LIGUS_FORGE_DENY_REAL").is_some_and(|v| v == "1") {
        return true;
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().and_then(|d| d.file_name()).map(|n| n == "deps"))
        .unwrap_or(false)
}

/// Install rustls's ring provider as the process default, once.
///
/// reqwest 0.13 is built with `rustls-no-provider` (iroh already pulls it that
/// way, and the default `rustls` feature would add aws-lc-rs next to ring), so
/// it panics on `Client::build` unless a process-level provider is installed.
/// Every Ligus HTTP client goes through [`client_builder`] / [`new_client`],
/// which call this first. A provider installed earlier by someone else wins.
pub fn install_tls_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// `reqwest::Client::builder()` with the TLS provider installed. Use this
/// (not `reqwest::Client::builder()`) everywhere in Ligus.
pub fn client_builder() -> reqwest::ClientBuilder {
    install_tls_provider();
    reqwest::Client::builder()
}

/// `reqwest::Client::new()` with the TLS provider installed. Use this (not
/// `reqwest::Client::new()`) everywhere in Ligus.
pub fn new_client() -> reqwest::Client {
    install_tls_provider();
    reqwest::Client::new()
}

/// Refuse a request to a non-loopback host from a test build.
pub(crate) fn guard_url(raw: &str) -> Result<()> {
    if !real_hosts_denied() {
        return Ok(());
    }
    let loopback = match url::Url::parse(raw).ok().and_then(|u| u.host().map(|h| h.to_owned())) {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    if loopback {
        Ok(())
    } else {
        Err(ForgeError::Transport("test builds never reach a real code host".into()))
    }
}

pub(crate) fn client() -> reqwest::Client {
    client_builder()
        .user_agent(concat!("ligus/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        // A token must never follow a redirect to another host (or down to
        // plain http): same scheme and host only.
        .redirect(reqwest::redirect::Policy::custom(|a| {
            let first = a.previous().first().cloned();
            match first {
                Some(f) if a.previous().len() < 5 && f.scheme() == a.url().scheme() && f.host_str() == a.url().host_str() && f.port() == a.url().port() => a.follow(),
                _ => a.stop(),
            }
        }))
        .build()
        .unwrap_or_default()
}

/// A forge's error message from its JSON body (`message`, `error`,
/// `errors[]`), bounded and on one line.
fn forge_message(body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let mut parts: Vec<String> = Vec::new();
    for k in ["message", "error", "error_description"] {
        match &v[k] {
            Value::String(s) if !s.is_empty() => parts.push(s.clone()),
            Value::Array(a) => parts.extend(a.iter().filter_map(|x| x.as_str().map(str::to_string))),
            Value::Object(o) => parts.extend(o.iter().map(|(k, v)| format!("{k}: {}", v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())))),
            _ => {}
        }
    }
    if let Some(a) = v["errors"].as_array() {
        for e in a {
            if let Some(m) = e["message"].as_str().or(e.as_str()) {
                parts.push(m.to_string());
            }
        }
    }
    let s = if parts.is_empty() { body.chars().take(200).collect::<String>() } else { parts.join("; ") };
    s.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(300).collect()
}

impl Api {
    async fn token(&self, purpose: Purpose) -> Result<Option<Secret>> {
        self.creds.token(purpose).await
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base.trim_end_matches('/'), path.trim_start_matches('/'))
    }

    /// Send a request; `Ok((status, body))` for any 2xx, 409 or 422, else
    /// the mapped error. Writes require a write token.
    pub async fn send(&self, method: Method, path: &str, query: &[(&str, String)], body: Option<&Value>, purpose: Purpose, accept: Option<&str>) -> Result<(StatusCode, String)> {
        let token = self.token(purpose).await?;
        if purpose == Purpose::Write && token.is_none() {
            return Err(ForgeError::NotSignedIn("this needs a write token for the code host on this device".into()));
        }
        guard_url(&self.url(path))?;
        let mut req = self.client.request(method, self.url(path)).query(query);
        for (k, v) in &self.headers {
            if accept.is_some() && k.eq_ignore_ascii_case("accept") {
                continue;
            }
            req = req.header(*k, *v);
        }
        if let Some(a) = accept {
            req = req.header("Accept", a);
        }
        if let Some(t) = &token {
            let v = match self.style {
                AuthStyle::Bearer => format!("Bearer {}", t.expose()),
                AuthStyle::Token => format!("token {}", t.expose()),
            };
            req = req.header("Authorization", v);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        // Errors from reqwest can include the URL, never headers.
        let mut resp = req.send().await.map_err(|e| ForgeError::Transport(format!("couldn't reach the code host: {}", e.without_url())))?;
        let status = resp.status();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| ForgeError::Transport(e.without_url().to_string()))? {
            if buf.len() + chunk.len() > MAX_BODY {
                return Err(ForgeError::Transport("the code host's answer was too large".into()));
            }
            buf.extend_from_slice(&chunk);
        }
        let mut text = String::from_utf8_lossy(&buf).into_owned();
        // Some forges echo the presented token in their error bodies
        // (Gitea/Forgejo: "access token does not exist [sha: …]"): never
        // carry it into a message.
        if let Some(t) = token.as_ref().filter(|t| !status.is_success() && !t.is_empty()) {
            if text.contains(t.expose()) {
                text = text.replace(t.expose(), "***");
            }
        }
        if status.is_success() || status == StatusCode::CONFLICT || status == StatusCode::UNPROCESSABLE_ENTITY {
            return Ok((status, text));
        }
        let msg = forge_message(&text);
        Err(match status {
            StatusCode::UNAUTHORIZED => ForgeError::Unauthorized(format!("the code host refused the credentials ({msg})")),
            StatusCode::FORBIDDEN => ForgeError::Unauthorized(format!("forbidden by the code host ({msg})")),
            StatusCode::NOT_FOUND => ForgeError::NotFound(path.to_string()),
            s if s.is_server_error() => ForgeError::Transport(format!("the code host failed ({s}): {msg}")),
            s => ForgeError::Rejected(format!("{s}: {msg}")),
        })
    }

    pub async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        let (_, body) = self.send(Method::GET, path, query, None, Purpose::Read, None).await?;
        serde_json::from_str(&body).map_err(|e| ForgeError::Transport(format!("the code host answered with something that isn't JSON: {e}")))
    }

    /// `Ok(None)` on 404.
    pub async fn get_opt(&self, path: &str, query: &[(&str, String)]) -> Result<Option<Value>> {
        match self.get(path, query).await {
            Ok(v) => Ok(Some(v)),
            Err(ForgeError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub async fn get_text(&self, path: &str, query: &[(&str, String)], accept: Option<&str>) -> Result<String> {
        Ok(self.send(Method::GET, path, query, None, Purpose::Read, accept).await?.1)
    }

    /// A write: `(status, json)` for 2xx, 409 and 422.
    pub async fn write(&self, method: Method, path: &str, body: &Value) -> Result<(StatusCode, Value)> {
        let (status, text) = self.send(method, path, &[], Some(body), Purpose::Write, None).await?;
        let v = serde_json::from_str(&text).unwrap_or(Value::Null);
        if status == StatusCode::CONFLICT || status == StatusCode::UNPROCESSABLE_ENTITY {
            return Ok((status, Value::String(forge_message(&text))));
        }
        Ok((status, v))
    }
}

pub(crate) fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_string()
}

pub(crate) fn id_str(v: &Value) -> String {
    match v {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn forge_messages_are_one_bounded_line() {
        assert_eq!(super::forge_message(r#"{"message":"Validation Failed","errors":[{"message":"A pull request already exists for a:b."}]}"#), "Validation Failed; A pull request already exists for a:b.");
        assert_eq!(super::forge_message(r#"{"message":["Another open merge request already exists for this source branch: !7"]}"#), "Another open merge request already exists for this source branch: !7");
        assert!(super::forge_message(&"x\n".repeat(500)).len() <= 300);
    }
}

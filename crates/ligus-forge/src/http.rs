//! A small JSON client over one forge API: base URL, auth header style,
//! error mapping, response size cap, same-host redirects only.

use std::sync::Arc;
use std::time::Duration;

use hyper_util::client::proxy::matcher::Matcher;
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
    /// The connection is marked internal on this device: loopback, private
    /// and link-local addresses are allowed.
    pub internal: bool,
    pub base: String,
    pub style: AuthStyle,
    pub creds: Arc<dyn CredentialSource>,
    pub headers: Vec<(&'static str, &'static str)>,
}

/// Whether this process is a test build that must never reach a real code
/// host nor read the host's `gh` login: `cfg(test)`, any cargo test binary
/// (see [`is_cargo_test_binary`]), or `LIGUS_FORGE_DENY_REAL=1` (for
/// harnesses that spawn the real binary). Only loopback hosts are reachable
/// then, and `gh` (from `PATH`) is never run.
pub fn real_hosts_denied() -> bool {
    if cfg!(test) || std::env::var_os("LIGUS_FORGE_DENY_REAL").is_some_and(|v| v == "1") {
        return true;
    }
    std::env::current_exe().ok().is_some_and(|p| is_cargo_test_binary(&p))
}

/// A binary cargo built for a test run: it lives in `target/<profile>/deps/`
/// (the classic layout), or its name ends in cargo's 16-hex-digit metadata
/// hash (`no_real_hosts-8e27a82306d4d313`), which also covers the newer
/// build-dir layout (`target/<profile>/build/<pkg>/<hash>/out/`). The
/// binaries cargo uplifts for people to run (`target/<profile>/ligus`) have
/// neither.
pub fn is_cargo_test_binary(exe: &std::path::Path) -> bool {
    if exe.parent().and_then(|d| d.file_name()).is_some_and(|n| n == "deps") {
        return true;
    }
    let Some(stem) = exe.file_stem().and_then(|s| s.to_str()) else { return false };
    stem.rsplit_once('-').is_some_and(|(name, hash)| {
        !name.is_empty() && hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
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

/// The process's rustls provider (ring unless someone installed another).
fn tls_provider() -> std::sync::Arc<rustls::crypto::CryptoProvider> {
    install_tls_provider();
    rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| std::sync::Arc::new(rustls::crypto::ring::default_provider()))
}

/// Whether the OS trust store can be used here: rustls-platform-verifier
/// (what reqwest verifies with by default) builds. On Linux it refuses to
/// build when the system has no CA certificates (a minimal container, an
/// empty `SSL_CERT_FILE`), which would make every client build fail and
/// `Client::new()` panic, even for loopback-only use. Probed once; the one
/// answer picks the roots for HTTP ([`client_builder`]) and for every other
/// TLS connection ([`tls_client_config`]) alike.
pub fn uses_platform_roots() -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| {
        let ok = rustls_platform_verifier::Verifier::new(tls_provider()).is_ok();
        if !ok {
            tracing::warn!("no usable system CA certificates; TLS verifies against Ligus's bundled Mozilla roots");
        }
        ok
    })
}

/// A verifying rustls config over the bundled Mozilla roots (what reqwest
/// 0.12 used before the OS trust store), for hosts without a CA store.
fn bundled_roots_tls() -> rustls::ClientConfig {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    rustls::ClientConfig::builder_with_provider(tls_provider())
        .with_safe_default_protocol_versions()
        .expect("ring supports the default TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth()
}

/// The rustls client config for a TLS connection Ligus makes outside
/// reqwest (the relay tunnel's WebSockets). It trusts exactly what the HTTP
/// clients trust: the OS trust store, or the bundled Mozilla roots when the
/// OS has none. No ALPN is set; callers add their own.
pub fn tls_client_config() -> rustls::ClientConfig {
    let provider = tls_provider();
    if uses_platform_roots() {
        if let Ok(v) = rustls_platform_verifier::Verifier::new(provider.clone()) {
            return rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .expect("ring supports the default TLS versions")
                .dangerous()
                .with_custom_certificate_verifier(std::sync::Arc::new(v))
                .with_no_client_auth();
        }
    }
    bundled_roots_tls()
}

/// `reqwest::Client::builder()` with the TLS provider installed. Use this
/// (not `reqwest::Client::builder()`) everywhere in Ligus. Certificates are
/// verified against the OS trust store, or the bundled Mozilla roots when the
/// OS has none (the same choice as [`tls_client_config`]).
pub fn client_builder() -> reqwest::ClientBuilder {
    client_builder_with_roots(uses_platform_roots())
}

/// [`client_builder`] for a host with (`platform_roots`) or without a usable
/// OS trust store. Without one the builder carries a preconfigured rustls
/// config (the bundled roots), which overrides every later TLS setting on
/// the builder (`tls_certs_only`, `identity`): a caller that pins a CA or
/// presents a client certificate must not start from it.
pub fn client_builder_with_roots(platform_roots: bool) -> reqwest::ClientBuilder {
    install_tls_provider();
    let b = reqwest::Client::builder();
    if platform_roots {
        b
    } else {
        b.tls_backend_preconfigured(bundled_roots_tls())
    }
}

/// `reqwest::Client::new()` with the TLS provider installed. Use this (not
/// `reqwest::Client::new()`) everywhere in Ligus.
pub fn new_client() -> reqwest::Client {
    client_builder().build().expect("an HTTP client with default settings builds")
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

/// Whether `ip` is an ordinary public unicast address. Loopback, private
/// (RFC 1918, unique local), link-local (cloud metadata lives there), shared
/// CGNAT and tailnet space, unspecified, multicast, broadcast, documentation,
/// benchmarking and reserved ranges are not, and neither are IPv4-mapped,
/// IPv4-compatible or NAT64 forms of those.
pub fn is_public_ip(ip: std::net::IpAddr) -> bool {
    use std::net::{IpAddr, Ipv4Addr};
    fn v4(a: Ipv4Addr) -> bool {
        let o = a.octets();
        !(a.is_unspecified()
            || a.is_loopback()
            || a.is_private()
            || a.is_link_local()
            || a.is_broadcast()
            || a.is_multicast()
            || a.is_documentation()
            || o[0] == 0
            || (o[0] == 100 && (o[1] & 0xc0) == 64)
            || (o[0] == 192 && o[1] == 0 && o[2] == 0)
            || (o[0] == 198 && (o[1] & 0xfe) == 18)
            || o[0] >= 240)
    }
    match ip {
        IpAddr::V4(a) => v4(a),
        IpAddr::V6(a) => {
            if let Some(m) = a.to_ipv4_mapped() {
                return v4(m);
            }
            let s = a.segments();
            if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
                return v4(Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8));
            }
            !(s[0..6] == [0, 0, 0, 0, 0, 0]
                || a.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] & 0xffc0) == 0xfec0
                || (s[0] == 0x2001 && s[1] == 0x0db8))
        }
    }
}

/// Why a connection may not reach an address.
pub(crate) fn internal_refused(what: &str) -> ForgeError {
    ForgeError::Invalid(format!(
        "{what} is an internal address (loopback, private or link-local): mark the connection internal on this device to use it"
    ))
}

/// Refuse a URL whose host is an internal IP literal (literals never go
/// through the resolver).
pub(crate) fn check_url_address(raw: &str, internal: bool) -> Result<()> {
    check_url_address_with(raw, internal, &proxy_matcher())
}

/// [`check_url_address`] against the proxy settings in `proxies`. Besides
/// internal IP literals, it refuses a target whose name is a proxy's host but
/// that would go direct (excluded by `NO_PROXY`): the resolver answers proxy
/// hosts as is, so that target would reach the proxy's (possibly internal)
/// address without the guard.
fn check_url_address_with(raw: &str, internal: bool, proxies: &Matcher) -> Result<()> {
    if internal {
        return Ok(());
    }
    let Ok(u) = url::Url::parse(raw) else { return Ok(()) };
    let ip = match u.host().map(|h| h.to_owned()) {
        Some(url::Host::Ipv4(ip)) => std::net::IpAddr::V4(ip),
        Some(url::Host::Ipv6(ip)) => std::net::IpAddr::V6(ip),
        Some(url::Host::Domain(d)) => {
            let d = d.to_ascii_lowercase();
            let direct = raw.parse::<http::Uri>().map(|uri| proxies.intercept(&uri).is_none()).unwrap_or(true);
            if direct && proxy_hosts_of(proxies).contains(&d) {
                return Err(internal_refused(&d));
            }
            return Ok(());
        }
        None => return Ok(()),
    };
    if is_public_ip(ip) {
        Ok(())
    } else {
        Err(internal_refused(&ip.to_string()))
    }
}

/// The proxy settings reqwest applies: hyper-util's `Matcher::from_system`,
/// which reqwest's own system proxy calls. That is the environment
/// (`HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY`, `NO_PROXY`) and, where it sets
/// nothing, the OS proxy settings on macOS and Windows. Read per check, as
/// reqwest reads it per client.
fn proxy_matcher() -> Matcher {
    Matcher::from_system()
}

/// The hosts of the configured proxies (env or OS): the resolver answers for
/// them as is (a local proxy is the person's own setup; through a proxy the
/// target is resolved by the proxy, see the docs' caveat).
fn proxy_hosts() -> Vec<String> {
    proxy_hosts_of(&proxy_matcher())
}

/// The proxy hosts `m` sends http and https requests to.
fn proxy_hosts_of(m: &Matcher) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for probe in ["http://ligus-proxy-probe.invalid/", "https://ligus-proxy-probe.invalid/"] {
        let Ok(uri) = probe.parse::<http::Uri>() else { continue };
        if let Some(h) = m.intercept(&uri).and_then(|i| i.uri().host().map(|h| h.trim_matches(['[', ']']).to_ascii_lowercase())) {
            if !hosts.contains(&h) {
                hosts.push(h);
            }
        }
    }
    hosts
}

/// Whether `raw` would be reached through a configured proxy (env or OS)
/// and, if so, whether its name resolves here to public addresses only.
///
/// Through a proxy, the proxy resolves the target: the public-only resolver
/// of [`public_only_client_builder`] never sees it. For a server someone
/// else named (a relay in a DM invite), call this before each connection so
/// a name that resolves (or was re-pointed) to a loopback, private or
/// link-local address is refused before the request is handed to the proxy.
/// A direct request is left to the resolver; a name this machine can't
/// resolve is left to the proxy (a proxy with its own DNS view can still
/// reach what it resolves: the documented limitation). Test builds resolve
/// no names but `localhost`.
pub async fn check_proxied_target(raw: &str) -> Result<()> {
    check_proxied_target_with(raw, &proxy_matcher()).await
}

async fn check_proxied_target_with(raw: &str, proxies: &Matcher) -> Result<()> {
    let Ok(uri) = raw.parse::<http::Uri>() else { return Ok(()) };
    if proxies.intercept(&uri).is_none() {
        return Ok(());
    }
    let Ok(u) = url::Url::parse(raw) else { return Ok(()) };
    let host = match u.host().map(|h| h.to_owned()) {
        Some(url::Host::Ipv4(ip)) => return if is_public_ip(ip.into()) { Ok(()) } else { Err(internal_refused(&ip.to_string())) },
        Some(url::Host::Ipv6(ip)) => return if is_public_ip(ip.into()) { Ok(()) } else { Err(internal_refused(&ip.to_string())) },
        Some(url::Host::Domain(d)) => d.trim_end_matches('.').to_ascii_lowercase(),
        None => return Ok(()),
    };
    if real_hosts_denied() && host != "localhost" {
        return Ok(());
    }
    let port = u.port_or_known_default().unwrap_or(443);
    let lookup = tokio::time::timeout(Duration::from_secs(5), tokio::net::lookup_host((host.as_str(), port))).await;
    if let Ok(Ok(addrs)) = lookup {
        if addrs.into_iter().any(|a| !is_public_ip(a.ip())) {
            return Err(internal_refused(&host));
        }
    }
    Ok(())
}

/// A resolver that answers with public addresses only, so a name that
/// resolves (or is rebound) to an internal address is never connected to.
struct PublicOnly {
    proxies: Vec<String>,
}

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_ascii_lowercase();
        let proxy = self.proxies.contains(&host);
        Box::pin(async move {
            let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let ok: Vec<std::net::SocketAddr> = addrs.into_iter().filter(|a| proxy || is_public_ip(a.ip())).collect();
            if ok.is_empty() {
                return Err(Box::new(internal_refused(&host)) as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(ok.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// [`client_builder`] for a server someone else named (a relay in an
/// invite): every name resolves to public addresses only, checked when the
/// connection is made, so a name re-pointed at a loopback, private or
/// link-local address after it was checked is never connected to. Redirects
/// are not followed (an IP literal in a redirect skips the resolver). The
/// caller still refuses internal IP literals in the URL itself.
pub fn public_only_client_builder() -> reqwest::ClientBuilder {
    client_builder()
        .dns_resolver(std::sync::Arc::new(PublicOnly { proxies: proxy_hosts() }))
        .redirect(reqwest::redirect::Policy::none())
}

/// The client for a connection: [`client`], resolving to public addresses
/// only unless the connection is marked internal on this device.
pub(crate) fn client_for(internal: bool) -> reqwest::Client {
    let b = builder();
    let b = if internal { b } else { b.dns_resolver(std::sync::Arc::new(PublicOnly { proxies: proxy_hosts() })) };
    b.build().unwrap_or_default()
}

fn builder() -> reqwest::ClientBuilder {
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
        check_url_address(&self.url(path), self.internal)?;
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
    /// Test binaries are recognised in both cargo layouts; the binaries
    /// people run are not.
    #[test]
    fn cargo_test_binaries_are_recognised_in_both_layouts() {
        use std::path::Path;
        assert!(super::is_cargo_test_binary(Path::new("/w/target/debug/deps/no_real_hosts-8e27a82306d4d313")));
        assert!(super::is_cargo_test_binary(Path::new("/w/target/debug/build/ligus-forge/8e27a82306d4d313/out/no_real_hosts-8e27a82306d4d313")));
        assert!(super::is_cargo_test_binary(Path::new("C:/t/debug/build/ligus-forge/8e27a82306d4d313/out/contract-8e27a82306d4d313.exe")));
        assert!(!super::is_cargo_test_binary(Path::new("/w/target/debug/ligus")));
        assert!(!super::is_cargo_test_binary(Path::new("C:/t/release/ligus.exe")));
        assert!(!super::is_cargo_test_binary(Path::new("/usr/local/bin/ligus-serve")));
        assert!(!super::is_cargo_test_binary(Path::new("/opt/x/tool-0123456789ABCDEF")));
    }

    /// Non-reqwest TLS trusts the same roots as reqwest: the OS store's
    /// verifier, or the bundled roots when the OS has none.
    #[test]
    fn tls_client_config_follows_the_root_policy() {
        let dbg = format!("{:?}", super::tls_client_config());
        if super::uses_platform_roots() {
            assert!(dbg.contains("verifier: Verifier"), "{dbg}");
        } else {
            assert!(dbg.contains("WebPkiServerVerifier"), "{dbg}");
        }
        assert!(super::tls_client_config().alpn_protocols.is_empty());
    }

    /// What a macOS or Windows system proxy setting looks like to the
    /// matcher (`host:port`, no scheme), and an env-style one.
    fn proxied(p: &str, no: &str) -> super::Matcher {
        super::Matcher::builder().http(p).https(p).no(no).build()
    }

    /// A system proxy on localhost is answered by the public-only resolver
    /// exactly like an env proxy on localhost (it used to be refused, so the
    /// forge client failed wherever the OS proxy was a local one).
    #[tokio::test]
    async fn a_system_proxy_on_localhost_is_trusted_like_an_env_one() {
        use reqwest::dns::Resolve;
        for p in ["localhost:3128", "http://localhost:3128", "http://user:pw@LocalHost:3128"] {
            let hosts = super::proxy_hosts_of(&proxied(p, ""));
            assert_eq!(hosts, vec!["localhost".to_string()], "{p}");
            let r = super::PublicOnly { proxies: hosts };
            assert!(r.resolve("localhost".parse().unwrap()).await.is_ok(), "{p}");
        }
        assert!(super::proxy_hosts_of(&super::Matcher::builder().build()).is_empty());
        let r = super::PublicOnly { proxies: vec![] };
        assert!(r.resolve("localhost".parse().unwrap()).await.is_err());
    }

    /// A target named like the proxy but excluded from it (NO_PROXY) would be
    /// resolved here, with the proxy's exemption: refused unless the
    /// connection is internal. Proxied targets and other names are fine.
    #[test]
    fn a_direct_target_named_like_the_proxy_is_refused() {
        let m = proxied("localhost:3128", "localhost");
        assert!(super::check_url_address_with("https://localhost:8443/api", false, &m).is_err());
        assert!(super::check_url_address_with("https://localhost:8443/api", true, &m).is_ok());
        assert!(super::check_url_address_with("https://git.example.com/api", false, &m).is_ok());
        let m = proxied("localhost:3128", "");
        assert!(super::check_url_address_with("https://localhost:8443/api", false, &m).is_ok());
    }

    /// Behind a proxy the proxy resolves the target, so the public-only
    /// resolver never sees it: a server someone else named (a DM relay) is
    /// resolved here first and refused when it lands on a non-public address.
    /// Without a proxy (or excluded by NO_PROXY) the resolver applies, and a
    /// name this machine can't resolve is left to the proxy.
    #[tokio::test]
    async fn a_proxied_target_is_resolved_here_first() {
        let m = proxied("proxy.example:3128", "");
        assert!(super::check_proxied_target_with("https://localhost:8443/", &m).await.is_err());
        assert!(super::check_proxied_target_with("https://127.0.0.1/", &m).await.is_err());
        assert!(super::check_proxied_target_with("https://[::1]/", &m).await.is_err());
        assert!(super::check_proxied_target_with("https://relay.example.com/", &m).await.is_ok());
        let direct = super::Matcher::builder().build();
        assert!(super::check_proxied_target_with("https://localhost:8443/", &direct).await.is_ok());
        let excluded = proxied("proxy.example:3128", "localhost");
        assert!(super::check_proxied_target_with("https://localhost:8443/", &excluded).await.is_ok());
    }

    #[test]
    fn forge_messages_are_one_bounded_line() {
        assert_eq!(super::forge_message(r#"{"message":"Validation Failed","errors":[{"message":"A pull request already exists for a:b."}]}"#), "Validation Failed; A pull request already exists for a:b.");
        assert_eq!(super::forge_message(r#"{"message":["Another open merge request already exists for this source branch: !7"]}"#), "Another open merge request already exists for this source branch: !7");
        assert!(super::forge_message(&"x\n".repeat(500)).len() <= 300);
    }
}

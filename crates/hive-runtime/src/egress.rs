//! `ligus-egress`: the per-turn egress proxy.
//!
//! An agent process runs with the network denied at the OS layer (Seatbelt
//! on macOS, a fresh network namespace on Linux) except for one loopback
//! port: this proxy. It is an HTTP proxy speaking `CONNECT` (and plain
//! absolute-form HTTP for allow-listed `http://` endpoints), with:
//!
//! - **an allow-list** per turn: the runtime's provider endpoints plus the
//!   hosts of the MCP servers that chat may reach ([`HostPort`] list from the
//!   [`crate::executor::EnforcementPlan`]);
//! - **exact matching**: host (case-insensitive) and port;
//! - **SNI pinning**: after `CONNECT`, the TLS ClientHello's server name must
//!   equal the `CONNECT` host (an IP-literal target may send none). There is
//!   no TLS interception; bytes are spliced through untouched;
//! - **auth**: a random token, carried as `Proxy-Authorization` (the
//!   `http://ligus:<token>@127.0.0.1:<port>` URL in `HTTPS_PROXY`), so another
//!   local process can't borrow the turn's allow-list;
//! - **denials** recorded (host and port only) for the caller to surface as a
//!   content-free decision.
//!
//! Loopback entries (a local Ollama for `pi`) skip the SNI check: they are
//! plain HTTP on this machine.
//!
//! **Gateway routes** (docs/design/v2/model-gateways.md §7): a turn whose
//! runtime uses an operator-listed model gateway reaches it only as plain
//! HTTP to this proxy, `http://127.0.0.1:<port>/ligus-gw/<token>/<name>/...`.
//! The proxy strips every credential the client sent, adds the gateway's own
//! ([`crate::model_gateway::Gateway::injected_headers`], the same headers a
//! hosted-API turn sends) and forwards over the gateway's TLS client. The
//! child never holds the credential.
//!
//! The proxy runs on its own thread with a current-thread Tokio runtime, so
//! synchronous callers (pre-flight) and async ones share it; dropping the
//! [`EgressProxy`] stops it.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::model_gateway::ResolvedGateway;

/// One allowed endpoint.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct HostPort {
    pub host: String,
    pub port: u16,
}

impl HostPort {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self { host: host.into().trim().trim_end_matches('.').to_ascii_lowercase(), port }
    }

    /// `host`, `host:port` (443 by default) or `[v6]:port`.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        if let Some(rest) = s.strip_prefix('[') {
            let (h, tail) = rest.split_once(']')?;
            let port = match tail.strip_prefix(':') {
                Some(p) => p.parse().ok()?,
                None => 443,
            };
            return Some(Self::new(h, port));
        }
        match s.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') => Some(Self::new(h, p.parse().ok()?)),
            _ => Some(Self::new(s, 443)),
        }
    }

    /// The host and port of a URL (`https://api.example.com/v1` →
    /// `api.example.com:443`).
    pub fn from_url(url: &str) -> Option<Self> {
        let u = url::Url::parse(url.trim()).ok()?;
        let host = u.host_str()?.trim_start_matches('[').trim_end_matches(']').to_string();
        let port = u.port_or_known_default()?;
        Some(Self::new(host, port))
    }

    pub fn is_loopback(&self) -> bool {
        self.host == "localhost" || self.host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    }

    fn is_ip_literal(&self) -> bool {
        self.host.parse::<IpAddr>().is_ok()
    }
}

impl std::fmt::Display for HostPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

/// A named, opt-in group of endpoints a chat's egress allow-list can add
/// (off by default). A preset only widens an **open** envelope: under dearth
/// mode or a chat lockdown it is ignored, like every other device-local
/// widening. Hosts are matched exactly, on port 443, with the proxy's SNI pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EgressPreset {
    /// npm, crates.io, PyPI and GitHub: what `npm install`, `cargo build`,
    /// `pip install` and a git dependency fetch.
    PackageRegistries,
    /// `mcp-proxy.anthropic.com`, for Claude Code turns only: the proxy
    /// behind the user's claude.ai MCP connectors (Drive, Gmail, ...). It
    /// lets the agent reach whatever services the user connected there.
    /// Claude Code's telemetry hosts stay blocked.
    ClaudeConnectors,
}

impl EgressPreset {
    /// Every preset, in display order.
    pub const ALL: &'static [EgressPreset] = &[EgressPreset::PackageRegistries, EgressPreset::ClaudeConnectors];

    /// The preset's name in settings and `LIGUS_SANDBOX_EGRESS_PRESETS`.
    pub fn name(self) -> &'static str {
        match self {
            EgressPreset::PackageRegistries => "package-registries",
            EgressPreset::ClaudeConnectors => "claude-connectors",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            EgressPreset::PackageRegistries => "Package registries",
            EgressPreset::ClaudeConnectors => "Claude connectors",
        }
    }

    /// A preset by name (case-insensitive; `_` and `-` alike).
    pub fn parse(s: &str) -> Option<Self> {
        let n = s.trim().to_ascii_lowercase().replace('_', "-");
        Self::ALL.iter().copied().find(|p| p.name() == n)
    }

    /// Known presets among `names`, deduplicated and sorted (unknown names
    /// are dropped).
    pub fn parse_list<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<Self> {
        let mut v: Vec<Self> = names.into_iter().filter_map(Self::parse).collect();
        v.sort();
        v.dedup();
        v
    }

    /// The host names the preset adds.
    pub fn host_names(self) -> &'static [&'static str] {
        match self {
            EgressPreset::PackageRegistries => &[
                // npm
                "registry.npmjs.org",
                // crates.io
                "crates.io",
                "static.crates.io",
                "index.crates.io",
                // PyPI
                "pypi.org",
                "files.pythonhosted.org",
                // GitHub
                "github.com",
                "api.github.com",
                "codeload.github.com",
                "objects.githubusercontent.com",
                "raw.githubusercontent.com",
            ],
            EgressPreset::ClaudeConnectors => &["mcp-proxy.anthropic.com"],
        }
    }

    /// The one runtime a preset is for, if it is runtime-specific (it then
    /// widens only that runtime's agent children, never `run_command` or
    /// pre-flight).
    pub fn only_for(self) -> Option<hive_core::ModelProviderKind> {
        match self {
            EgressPreset::PackageRegistries => None,
            EgressPreset::ClaudeConnectors => Some(hive_core::ModelProviderKind::ClaudeCode),
        }
    }

    /// The endpoints the preset adds (each host on 443).
    pub fn hosts(self) -> Vec<HostPort> {
        self.host_names().iter().map(|h| HostPort::new(*h, 443)).collect()
    }
}

impl std::fmt::Display for EgressPreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A running proxy. Dropping it stops the listener.
pub struct EgressProxy {
    port: u16,
    token: String,
    unix_socket: Option<PathBuf>,
    denials: Arc<Mutex<Vec<HostPort>>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl EgressProxy {
    /// Start a proxy for `allow` on `127.0.0.1:<random>`, and also on the
    /// unix socket `unix_socket` when given (the Linux sandbox reaches the
    /// host only through a bind-mounted socket).
    pub fn start(allow: Vec<HostPort>, unix_socket: Option<&Path>) -> std::io::Result<Self> {
        Self::start_with(allow, unix_socket, Vec::new())
    }

    /// [`Self::start`] with gateway routes (one per gateway the turn may use).
    pub fn start_with(allow: Vec<HostPort>, unix_socket: Option<&Path>, routes: Vec<ResolvedGateway>) -> std::io::Result<Self> {
        let std_listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        std_listener.set_nonblocking(true)?;
        let port = std_listener.local_addr()?.port();
        #[cfg(unix)]
        let unix_listener = match unix_socket {
            Some(p) => {
                let _ = std::fs::remove_file(p);
                let l = std::os::unix::net::UnixListener::bind(p)?;
                l.set_nonblocking(true)?;
                Some(l)
            }
            None => None,
        };
        let mut raw = [0u8; 24];
        getrandom::fill(&mut raw).map_err(|e| std::io::Error::other(e.to_string()))?;
        let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let denials = Arc::new(Mutex::new(Vec::new()));
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let shared = Arc::new(Shared { allow, token: token.clone(), denials: denials.clone(), routes });
        let thread = std::thread::Builder::new().name("ligus-egress".into()).spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
            rt.block_on(async move {
                let Ok(tcp) = tokio::net::TcpListener::from_std(std_listener) else { return };
                #[cfg(unix)]
                let unix = unix_listener.and_then(|l| tokio::net::UnixListener::from_std(l).ok());
                let mut stop_rx = stop_rx;
                loop {
                    #[cfg(unix)]
                    let next_unix = async {
                        match &unix {
                            Some(u) => u.accept().await.ok().map(|(s, _)| s),
                            None => std::future::pending().await,
                        }
                    };
                    #[cfg(not(unix))]
                    let next_unix = std::future::pending::<Option<()>>();
                    tokio::select! {
                        _ = &mut stop_rx => break,
                        Ok((s, _)) = tcp.accept() => {
                            let sh = shared.clone();
                            tokio::spawn(async move { let _ = handle(s, &sh).await; });
                        }
                        Some(s) = next_unix => {
                            let sh = shared.clone();
                            #[cfg(unix)]
                            tokio::spawn(async move { let _ = handle(s, &sh).await; });
                            #[cfg(not(unix))]
                            let _ = (s, sh);
                        }
                    }
                }
            });
        })?;
        Ok(Self { port, token, unix_socket: unix_socket.map(Path::to_path_buf), denials, stop: Some(stop_tx), thread: Some(thread) })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// `http://ligus:<token>@127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        proxy_url(self.port, &self.token)
    }

    /// The loopback base URL of gateway `name`'s route (what a CLI is given
    /// as its base URL).
    pub fn gateway_url(&self, name: &str) -> String {
        gateway_route_url(self.port, &self.token, name)
    }

    /// Endpoints refused so far.
    pub fn denials(&self) -> Vec<HostPort> {
        self.denials.lock().map(|d| d.clone()).unwrap_or_default()
    }

    pub fn unix_socket(&self) -> Option<&Path> {
        self.unix_socket.as_deref()
    }
}

/// The proxy URL a child is given.
pub fn proxy_url(port: u16, token: &str) -> String {
    format!("http://ligus:{token}@127.0.0.1:{port}")
}

/// `http://127.0.0.1:<port>/ligus-gw/<token>/<name>`.
pub fn gateway_route_url(port: u16, token: &str, name: &str) -> String {
    format!("http://127.0.0.1:{port}{}{token}/{name}", crate::model_gateway::ROUTE_PREFIX)
}

/// The proxy env a child gets: both spellings of `HTTPS_PROXY`/`HTTP_PROXY`,
/// and `NO_PROXY` for loopback only.
pub fn proxy_env(url: &str) -> Vec<(String, String)> {
    let mut v = Vec::new();
    for k in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
        v.push((k.to_string(), url.to_string()));
    }
    for k in ["NO_PROXY", "no_proxy"] {
        v.push((k.to_string(), "localhost,127.0.0.1,::1".to_string()));
    }
    v
}

impl Drop for EgressProxy {
    fn drop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        if let Some(p) = &self.unix_socket {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Addresses a non-loopback allow-list name must never resolve to: this
/// machine, link-local (cloud metadata at 169.254.169.254), unspecified and
/// multicast. A name that resolves there (DNS rebinding, or a numeric form
/// such as `127.1`) would turn the allow-list into a tunnel to local
/// services. Private LAN ranges stay reachable (a self-hosted MCP server).
fn ip_forbidden(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    };
    match ip {
        IpAddr::V4(a) => a.is_loopback() || a.is_link_local() || a.is_unspecified() || a.is_multicast() || a.is_broadcast() || a.octets()[0] == 0,
        IpAddr::V6(a) => a.is_loopback() || a.is_unspecified() || a.is_multicast() || (a.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// Connect to an allow-listed endpoint. A loopback entry goes to this
/// machine; any other is resolved here and refused when an address it
/// resolves to is [`ip_forbidden`] (the vetted address is the one dialed,
/// so a second lookup can't rebind it).
async fn connect_upstream(hp: &HostPort) -> std::io::Result<tokio::net::TcpStream> {
    if hp.is_loopback() {
        return tokio::net::TcpStream::connect((hp.host.as_str(), hp.port)).await;
    }
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((hp.host.as_str(), hp.port)).await?.collect();
    if addrs.is_empty() || addrs.iter().any(|a| ip_forbidden(a.ip())) {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "endpoint resolves to a local address"));
    }
    tokio::net::TcpStream::connect(&addrs[..]).await
}

struct Shared {
    allow: Vec<HostPort>,
    token: String,
    denials: Arc<Mutex<Vec<HostPort>>>,
    routes: Vec<ResolvedGateway>,
}

impl Shared {
    fn allowed(&self, hp: &HostPort) -> bool {
        self.allow.iter().any(|a| a == hp || (a.is_loopback() && hp.is_loopback() && a.port == hp.port))
    }
    fn deny(&self, hp: HostPort) {
        if let Ok(mut d) = self.denials.lock() {
            if d.len() < 256 && !d.contains(&hp) {
                d.push(hp);
            }
        }
    }
}

const MAX_HEAD: usize = 16 * 1024;

async fn read_head<S: AsyncRead + Unpin>(s: &mut S) -> std::io::Result<(Vec<u8>, Vec<u8>)> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = buf.split_off(i + 4);
            return Ok((buf, rest));
        }
        if buf.len() > MAX_HEAD {
            return Err(std::io::Error::other("request head too large"));
        }
        let n = s.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::Error::other("eof before request head"));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn authorized(head: &str, token: &str) -> bool {
    use base64::Engine;
    head.lines().skip(1).any(|l| {
        let Some((name, value)) = l.split_once(':') else { return false };
        if !name.trim().eq_ignore_ascii_case("proxy-authorization") {
            return false;
        }
        let value = value.trim();
        let Some((scheme, cred)) = value.split_once(' ') else { return false };
        match scheme.to_ascii_lowercase().as_str() {
            "basic" => base64::engine::general_purpose::STANDARD
                .decode(cred.trim())
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .and_then(|s| s.split_once(':').map(|(_, p)| constant_eq(p.as_bytes(), token.as_bytes())))
                .unwrap_or(false),
            "bearer" => constant_eq(cred.trim().as_bytes(), token.as_bytes()),
            _ => false,
        }
    })
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn respond<S: AsyncWrite + Unpin>(s: &mut S, status: &str) {
    let _ = s.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await;
    let _ = s.flush().await;
}

async fn handle<S: AsyncRead + AsyncWrite + Unpin>(mut client: S, sh: &Shared) -> std::io::Result<()> {
    let (head, rest) = read_head(&mut client).await?;
    let head_str = String::from_utf8_lossy(&head).into_owned();
    let first = head_str.lines().next().unwrap_or_default().to_string();
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    // A gateway route authenticates by the token in its path (the CLI talks
    // to it directly, not as a proxy).
    if target.starts_with(crate::model_gateway::ROUTE_PREFIX) {
        return gateway_route(client, &method, &target, &head_str, rest, sh).await;
    }
    if !authorized(&head_str, &sh.token) {
        respond(&mut client, "407 Proxy Authentication Required").await;
        return Ok(());
    }
    if method.eq_ignore_ascii_case("CONNECT") {
        let Some(hp) = HostPort::parse(&target) else {
            respond(&mut client, "400 Bad Request").await;
            return Ok(());
        };
        if !sh.allowed(&hp) {
            sh.deny(hp);
            respond(&mut client, "403 Forbidden").await;
            return Ok(());
        }
        client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
        client.flush().await?;
        let mut first_bytes = rest;
        if !hp.is_loopback() {
            // Pin the TLS server name to the CONNECT host.
            let hello = read_client_hello(&mut client, &mut first_bytes).await;
            let ok = match hello {
                Some(Some(sni)) => sni.eq_ignore_ascii_case(&hp.host),
                Some(None) => hp.is_ip_literal(),
                None => false,
            };
            if !ok {
                sh.deny(hp);
                return Ok(());
            }
        }
        let mut upstream = match connect_upstream(&hp).await {
            Ok(u) => u,
            Err(e) => {
                sh.deny(hp);
                return Err(e);
            }
        };
        upstream.write_all(&first_bytes).await?;
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        return Ok(());
    }
    // Absolute-form plain HTTP (`GET http://host:port/path`).
    let Some(hp) = url::Url::parse(&target).ok().filter(|u| u.scheme() == "http").and_then(|_| HostPort::from_url(&target)) else {
        respond(&mut client, "400 Bad Request").await;
        return Ok(());
    };
    if !sh.allowed(&hp) {
        sh.deny(hp);
        respond(&mut client, "403 Forbidden").await;
        return Ok(());
    }
    let mut upstream = match connect_upstream(&hp).await {
        Ok(u) => u,
        Err(e) => {
            sh.deny(hp);
            respond(&mut client, "403 Forbidden").await;
            return Err(e);
        }
    };
    // Forward the head without the proxy credential.
    let mut out = String::new();
    for (i, l) in head_str.split("\r\n").enumerate() {
        if i > 0 && l.split_once(':').is_some_and(|(n, _)| n.trim().eq_ignore_ascii_case("proxy-authorization")) {
            continue;
        }
        out.push_str(l);
        out.push_str("\r\n");
    }
    let out = out.trim_end_matches("\r\n").to_string() + "\r\n\r\n";
    upstream.write_all(out.as_bytes()).await?;
    upstream.write_all(&rest).await?;
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Gateway routes
// ---------------------------------------------------------------------------

/// The most a gateway route request body may be.
const MAX_ROUTE_BODY: usize = 32 * 1024 * 1024;

/// Hop-by-hop headers, never forwarded either way.
fn hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection" | "keep-alive" | "proxy-connection" | "te" | "trailer" | "transfer-encoding" | "upgrade" | "host" | "content-length" | "expect"
    )
}

async fn respond_text<S: AsyncWrite + Unpin>(s: &mut S, status: &str, msg: &str) {
    let body = format!("{{\"error\":{{\"message\":{}}}}}", serde_json::Value::String(msg.to_string()));
    let _ = s
        .write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes())
        .await;
    let _ = s.flush().await;
}

/// Read a request body: `Content-Length`, or chunked; `buf` holds what was
/// read past the head.
async fn read_body<S: AsyncRead + Unpin>(s: &mut S, headers: &[(String, String)], mut buf: Vec<u8>) -> std::io::Result<Vec<u8>> {
    let get = |n: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.trim().to_string());
    let chunked = get("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    let mut chunk = [0u8; 16 * 1024];
    if !chunked {
        let len: usize = match get("content-length") {
            Some(v) => v.parse().map_err(|_| std::io::Error::other("bad content-length"))?,
            None => 0,
        };
        if len > MAX_ROUTE_BODY {
            return Err(std::io::Error::other("request body too large"));
        }
        while buf.len() < len {
            let n = s.read(&mut chunk).await?;
            if n == 0 {
                return Err(std::io::Error::other("eof in request body"));
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        buf.truncate(len);
        return Ok(buf);
    }
    // Chunked: decode as it arrives.
    let mut out = Vec::new();
    let mut pos = 0usize;
    loop {
        // Need a full size line.
        let line_end = loop {
            if let Some(i) = buf[pos..].windows(2).position(|w| w == b"\r\n") {
                break pos + i;
            }
            if buf.len() - pos > 1024 {
                return Err(std::io::Error::other("bad chunk header"));
            }
            let n = s.read(&mut chunk).await?;
            if n == 0 {
                return Err(std::io::Error::other("eof in chunked body"));
            }
            buf.extend_from_slice(&chunk[..n]);
        };
        let size_str = String::from_utf8_lossy(&buf[pos..line_end]).to_string();
        let size = usize::from_str_radix(size_str.split(';').next().unwrap_or("").trim(), 16).map_err(|_| std::io::Error::other("bad chunk size"))?;
        pos = line_end + 2;
        if size == 0 {
            // Trailers end with an empty line; don't wait for them.
            return Ok(out);
        }
        if out.len() + size > MAX_ROUTE_BODY {
            return Err(std::io::Error::other("request body too large"));
        }
        while buf.len() < pos + size + 2 {
            let n = s.read(&mut chunk).await?;
            if n == 0 {
                return Err(std::io::Error::other("eof in chunk"));
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        out.extend_from_slice(&buf[pos..pos + size]);
        pos += size + 2;
    }
}

/// Serve one request on a gateway route: check the path token, strip the
/// client's credentials, add the gateway's, forward, stream the answer back.
async fn gateway_route<S: AsyncRead + AsyncWrite + Unpin>(
    mut client: S,
    method: &str,
    target: &str,
    head_str: &str,
    rest: Vec<u8>,
    sh: &Shared,
) -> std::io::Result<()> {
    let tail = &target[crate::model_gateway::ROUTE_PREFIX.len()..];
    let (tok, tail) = tail.split_once('/').unwrap_or((tail, ""));
    if !constant_eq(tok.as_bytes(), sh.token.as_bytes()) {
        respond_text(&mut client, "403 Forbidden", "bad route token").await;
        return Ok(());
    }
    let (name, path) = tail.split_once('/').map(|(n, p)| (n, format!("/{p}"))).unwrap_or((tail, "/".to_string()));
    let name = name.split('?').next().unwrap_or_default();
    let Some(route) = sh.routes.iter().find(|r| r.gateway.name() == name) else {
        respond_text(&mut client, "404 Not Found", "no such model gateway for this turn").await;
        return Ok(());
    };
    if !crate::model_gateway::route_path_allowed(route.gateway.api(), method, &path) {
        respond_text(&mut client, "403 Forbidden", "only the model API is reachable through a gateway route").await;
        return Ok(());
    }
    let Ok(m) = reqwest::Method::from_bytes(method.as_bytes()) else {
        respond_text(&mut client, "405 Method Not Allowed", "bad method").await;
        return Ok(());
    };
    let headers: Vec<(String, String)> = head_str
        .split("\r\n")
        .skip(1)
        .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .filter(|(k, _)| !k.is_empty())
        .collect();
    if headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("expect") && v.eq_ignore_ascii_case("100-continue")) {
        client.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").await?;
    }
    let body = match read_body(&mut client, &headers, rest).await {
        Ok(b) => b,
        Err(e) => {
            respond_text(&mut client, "400 Bad Request", &e.to_string()).await;
            return Ok(());
        }
    };
    let gw = &route.gateway;
    let http = match gw.client().await {
        Ok(c) => c,
        Err(e) => {
            respond_text(&mut client, "502 Bad Gateway", &e.to_string()).await;
            return Ok(());
        }
    };
    let injected = match gw.injected_headers(route.attribution.as_ref()) {
        Ok(h) => h,
        Err(e) => {
            respond_text(&mut client, "502 Bad Gateway", &e.to_string()).await;
            return Ok(());
        }
    };
    let mut req = http.request(m, gw.join(&path));
    for (k, v) in &headers {
        if hop_by_hop(k) || crate::model_gateway::is_client_credential_header(k) {
            continue;
        }
        req = req.header(k.as_str(), v.as_str());
    }
    for (k, v) in &injected {
        req = req.header(k.as_str(), v.as_str());
    }
    let resp = match req.body(body).send().await {
        Ok(r) => r,
        Err(e) => {
            respond_text(&mut client, "502 Bad Gateway", &format!("model gateway '{}' unreachable: {}", gw.name(), crate::provider::http::root_cause(&e))).await;
            return Ok(());
        }
    };
    let status = resp.status();
    let mut out = format!("HTTP/1.1 {} {}\r\n", status.as_u16(), status.canonical_reason().unwrap_or(""));
    for (k, v) in resp.headers() {
        if hop_by_hop(k.as_str()) {
            continue;
        }
        if let Ok(v) = v.to_str() {
            out.push_str(&format!("{}: {v}\r\n", k.as_str()));
        }
    }
    out.push_str("Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n");
    client.write_all(out.as_bytes()).await?;
    use futures_util::StreamExt;
    let mut stream = resp.bytes_stream();
    while let Some(next) = stream.next().await {
        let Ok(bytes) = next else { break };
        if bytes.is_empty() {
            continue;
        }
        client.write_all(format!("{:x}\r\n", bytes.len()).as_bytes()).await?;
        client.write_all(&bytes).await?;
        client.write_all(b"\r\n").await?;
        client.flush().await?;
    }
    client.write_all(b"0\r\n\r\n").await?;
    client.flush().await?;
    Ok(())
}

/// Read the client's TLS ClientHello (appending every byte read to `buf`).
/// `None` = not TLS or malformed; `Some(None)` = a ClientHello without SNI.
async fn read_client_hello<S: AsyncRead + Unpin>(s: &mut S, buf: &mut Vec<u8>) -> Option<Option<String>> {
    let mut chunk = [0u8; 4096];
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match parse_client_hello(buf) {
            Parse::Done(sni) => return Some(sni),
            Parse::Invalid => return None,
            Parse::NeedMore => {}
        }
        if buf.len() > 64 * 1024 {
            return None;
        }
        let n = tokio::time::timeout_at(deadline, s.read(&mut chunk)).await.ok()?.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

#[derive(Debug, PartialEq)]
enum Parse {
    Done(Option<String>),
    NeedMore,
    Invalid,
}

/// Parse a ClientHello from TLS records in `buf` (it may span records).
fn parse_client_hello(buf: &[u8]) -> Parse {
    // Reassemble the handshake payload from consecutive handshake records.
    let mut hs = Vec::new();
    let mut i = 0;
    loop {
        if buf.len() < i + 5 {
            return Parse::NeedMore;
        }
        if buf[i] != 0x16 || buf[i + 1] != 0x03 {
            return Parse::Invalid;
        }
        let len = u16::from_be_bytes([buf[i + 3], buf[i + 4]]) as usize;
        if buf.len() < i + 5 + len {
            return Parse::NeedMore;
        }
        hs.extend_from_slice(&buf[i + 5..i + 5 + len]);
        i += 5 + len;
        if hs.len() >= 4 {
            if hs[0] != 0x01 {
                return Parse::Invalid;
            }
            let hlen = ((hs[1] as usize) << 16) | ((hs[2] as usize) << 8) | hs[3] as usize;
            if hs.len() >= 4 + hlen {
                return match sni_from_hello(&hs[4..4 + hlen]) {
                    Some(s) => Parse::Done(s),
                    None => Parse::Invalid,
                };
            }
        }
    }
}

/// The SNI host name in a ClientHello body (`Some(None)` when absent).
fn sni_from_hello(b: &[u8]) -> Option<Option<String>> {
    let mut p = 2 + 32; // client_version, random
    let sid = *b.get(p)? as usize;
    p += 1 + sid;
    let cs = u16::from_be_bytes([*b.get(p)?, *b.get(p + 1)?]) as usize;
    p += 2 + cs;
    let comp = *b.get(p)? as usize;
    p += 1 + comp;
    if p == b.len() {
        return Some(None);
    }
    let ext_len = u16::from_be_bytes([*b.get(p)?, *b.get(p + 1)?]) as usize;
    p += 2;
    let end = p.checked_add(ext_len)?;
    if end > b.len() {
        return None;
    }
    while p + 4 <= end {
        let ty = u16::from_be_bytes([b[p], b[p + 1]]);
        let len = u16::from_be_bytes([b[p + 2], b[p + 3]]) as usize;
        p += 4;
        let data = b.get(p..p + len)?;
        if ty == 0 {
            // server_name_list
            let mut q = 2;
            while q + 3 <= data.len() {
                let name_type = data[q];
                let nlen = u16::from_be_bytes([data[q + 1], data[q + 2]]) as usize;
                let name = data.get(q + 3..q + 3 + nlen)?;
                if name_type == 0 {
                    return Some(Some(String::from_utf8(name.to_vec()).ok()?.trim_end_matches('.').to_string()));
                }
                q += 3 + nlen;
            }
            return Some(None);
        }
        p += len;
    }
    Some(None)
}

/// What a refused endpoint is, for telling the user whether it mattered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RefusedKind {
    /// A CLI's own telemetry or error reporting (Claude Code's Datadog log
    /// intake, Sentry, ...). Never needed for a turn; blocked on purpose.
    Telemetry,
    /// Claude Code's claude.ai connectors proxy: the user's claude.ai MCP
    /// connectors, off unless the `claude-connectors` preset is on. The turn
    /// and the Ligus tools work without it.
    ClaudeConnectors,
    /// Anything else: may be something the agent needed.
    Other,
}

/// Classify a refused endpoint (`RefusedKind`).
pub fn classify_refused(hp: &HostPort) -> RefusedKind {
    let h = hp.host.trim_end_matches('.').to_ascii_lowercase();
    let under = |d: &str| h == d || h.ends_with(&format!(".{d}"));
    if EgressPreset::ClaudeConnectors.host_names().iter().any(|c| h == *c) {
        return RefusedKind::ClaudeConnectors;
    }
    const TELEMETRY: &[&str] = &["datadoghq.com", "datadoghq.eu", "sentry.io", "statsigapi.net", "featuregates.org", "featureassets.org", "prodregistryv2.org"];
    if TELEMETRY.iter().any(|d| under(d)) {
        return RefusedKind::Telemetry;
    }
    RefusedKind::Other
}

/// The chat notice for a turn's refused endpoints: `None` when every one is
/// a known optional host (telemetry, the claude.ai connectors proxy), which
/// are blocked on purpose and don't affect the turn. Otherwise it names the
/// hosts that may have mattered, and the optional ones apart.
pub fn refused_notice(denied: &[HostPort]) -> Option<String> {
    let mut by: std::collections::BTreeMap<RefusedKind, Vec<String>> = Default::default();
    for hp in denied {
        let name = if hp.port == 443 { hp.host.clone() } else { hp.to_string() };
        let v = by.entry(classify_refused(hp)).or_default();
        if !v.contains(&name) {
            v.push(name);
        }
    }
    let other = by.get(&RefusedKind::Other)?;
    let mut s = format!(
        "The agent's sandbox blocked {} — not on this chat's allow-list (its model provider, allowed MCP servers and any egress presets). If the agent needed {}, switch on a network preset for this chat in Settings → Guards → Network presets.",
        other.join(", "),
        if other.len() == 1 { "it" } else { "them" }
    );
    let mut optional = Vec::new();
    if let Some(t) = by.get(&RefusedKind::Telemetry) {
        optional.push(format!("telemetry ({})", t.join(", ")));
    }
    if let Some(c) = by.get(&RefusedKind::ClaudeConnectors) {
        optional.push(format!("your claude.ai connectors ({}; the Claude connectors preset allows it)", c.join(", ")));
    }
    if !optional.is_empty() {
        s.push_str(&format!(" Also blocked, which doesn't affect the turn: {}.", optional.join("; ")));
    }
    Some(s)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A minimal TLS 1.2-shaped ClientHello record carrying `sni`.
    pub(crate) fn client_hello(sni: Option<&str>) -> Vec<u8> {
        let mut body = vec![0x03, 0x03];
        body.extend([0u8; 32]);
        body.push(0); // session id
        body.extend([0x00, 0x02, 0x13, 0x01]); // one cipher suite
        body.extend([0x01, 0x00]); // compression
        let mut exts = Vec::new();
        if let Some(name) = sni {
            let n = name.as_bytes();
            let mut list = vec![0u8];
            list.extend((n.len() as u16).to_be_bytes());
            list.extend(n);
            let mut data = (list.len() as u16).to_be_bytes().to_vec();
            data.extend(list);
            exts.extend([0x00, 0x00]);
            exts.extend((data.len() as u16).to_be_bytes());
            exts.extend(data);
        }
        body.extend((exts.len() as u16).to_be_bytes());
        body.extend(exts);
        let mut hs = vec![0x01, 0, 0, 0];
        let l = body.len();
        hs[1] = (l >> 16) as u8;
        hs[2] = (l >> 8) as u8;
        hs[3] = l as u8;
        hs.extend(body);
        let mut rec = vec![0x16, 0x03, 0x01];
        rec.extend((hs.len() as u16).to_be_bytes());
        rec.extend(hs);
        rec
    }

    #[test]
    fn host_port_parsing() {
        assert_eq!(HostPort::parse("API.Anthropic.com"), Some(HostPort::new("api.anthropic.com", 443)));
        assert_eq!(HostPort::parse("example.com:8443"), Some(HostPort::new("example.com", 8443)));
        assert_eq!(HostPort::parse("[::1]:11434"), Some(HostPort::new("::1", 11434)));
        assert_eq!(HostPort::from_url("http://localhost:11434/v1/chat"), Some(HostPort::new("localhost", 11434)));
        assert_eq!(HostPort::from_url("https://openrouter.ai/api/v1"), Some(HostPort::new("openrouter.ai", 443)));
        assert!(HostPort::new("127.0.0.1", 1).is_loopback() && HostPort::new("localhost", 1).is_loopback());
        assert!(!HostPort::new("example.com", 443).is_loopback());
    }

    #[test]
    fn sni_is_parsed_across_records() {
        let rec = client_hello(Some("api.anthropic.com"));
        assert_eq!(parse_client_hello(&rec), Parse::Done(Some("api.anthropic.com".into())));
        assert_eq!(parse_client_hello(&rec[..10]), Parse::NeedMore);
        assert_eq!(parse_client_hello(&client_hello(None)), Parse::Done(None));
        assert_eq!(parse_client_hello(b"GET / HTTP/1.1\r\n\r\n"), Parse::Invalid);
        // The same handshake split over two records.
        let hs = &rec[5..];
        let (a, b) = hs.split_at(20);
        let mut two = vec![0x16, 0x03, 0x01];
        two.extend((a.len() as u16).to_be_bytes());
        two.extend(a);
        two.extend([0x16, 0x03, 0x01]);
        two.extend((b.len() as u16).to_be_bytes());
        two.extend(b);
        assert_eq!(parse_client_hello(&two), Parse::Done(Some("api.anthropic.com".into())));
    }

    async fn connect_through(p: &EgressProxy, target: &str, auth: bool) -> (tokio::net::TcpStream, String) {
        use base64::Engine;
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", p.port())).await.unwrap();
        let cred = base64::engine::general_purpose::STANDARD.encode(format!("ligus:{}", p.token()));
        let auth = if auth { format!("Proxy-Authorization: Basic {cred}\r\n") } else { String::new() };
        s.write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n{auth}\r\n").as_bytes()).await.unwrap();
        let mut buf = vec![0u8; 256];
        let n = s.read(&mut buf).await.unwrap();
        (s, String::from_utf8_lossy(&buf[..n]).into_owned())
    }

    /// An "upstream" that echoes what it receives, standing in for a TLS
    /// server (the proxy never terminates TLS, so bytes are all it sees).
    async fn echo_server() -> u16 {
        let l = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let (mut r, mut w) = s.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn proxy_allows_listed_hosts_and_refuses_others() {
        let up = echo_server().await;
        // "localhost" is loopback: no SNI needed (plain local service).
        let p = EgressProxy::start(vec![HostPort::new("localhost", up)], None).unwrap();
        let (_s, resp) = connect_through(&p, "example.com:443", true).await;
        assert!(resp.starts_with("HTTP/1.1 403"), "{resp}");
        let (_s, resp) = connect_through(&p, &format!("localhost:{up}"), false).await;
        assert!(resp.starts_with("HTTP/1.1 407"), "{resp}");
        let (mut s, resp) = connect_through(&p, &format!("localhost:{up}"), true).await;
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        s.write_all(b"ping").await.unwrap();
        let mut b = [0u8; 4];
        s.read_exact(&mut b).await.unwrap();
        assert_eq!(&b, b"ping");
        assert_eq!(p.denials(), vec![HostPort::new("example.com", 443)]);
    }

    /// DNS rebinding: an allow-listed *name* (an MCP server's host) that
    /// resolves to this machine or a link-local address (cloud metadata) must
    /// not become a tunnel to local services. "127.1" stands in for such a
    /// name: it isn't a loopback literal, yet resolves to 127.0.0.1.
    #[tokio::test]
    async fn proxy_refuses_names_that_resolve_to_loopback_or_link_local() {
        let up = echo_server().await;
        let name = "127.1";
        let p = EgressProxy::start(vec![HostPort::new(name, up)], None).unwrap();
        let (mut s, resp) = connect_through(&p, &format!("{name}:{up}"), true).await;
        if resp.starts_with("HTTP/1.1 200") {
            let hello = client_hello(Some(name));
            s.write_all(&hello).await.unwrap();
            let mut b = vec![0u8; hello.len()];
            let got = tokio::time::timeout(std::time::Duration::from_secs(5), s.read_exact(&mut b)).await;
            assert!(!matches!(got, Ok(Ok(_))), "the proxy spliced an allow-listed name through to a loopback service");
        }
        // Plain HTTP (absolute form) the same.
        let mut c = tokio::net::TcpStream::connect(("127.0.0.1", p.port())).await.unwrap();
        let cred = { use base64::Engine; base64::engine::general_purpose::STANDARD.encode(format!("ligus:{}", p.token())) };
        c.write_all(format!("GET http://{name}:{up}/ HTTP/1.1\r\nHost: {name}:{up}\r\nProxy-Authorization: Basic {cred}\r\n\r\n").as_bytes()).await.unwrap();
        let mut b = vec![0u8; 64];
        let n = tokio::time::timeout(std::time::Duration::from_secs(5), c.read(&mut b)).await.unwrap().unwrap_or(0);
        let text = String::from_utf8_lossy(&b[..n]);
        assert!(!text.starts_with("GET "), "plain HTTP reached the loopback service: {text}");
        assert!(ip_forbidden("169.254.169.254".parse().unwrap()) && ip_forbidden("::ffff:127.0.0.1".parse().unwrap()));
        assert!(!ip_forbidden("192.168.1.10".parse().unwrap()) && !ip_forbidden("1.1.1.1".parse().unwrap()));
    }

    /// C.4.6 `proxy_rejects_sni_mismatch`: an allow-listed CONNECT host with
    /// a ClientHello naming another server is cut, and a matching one passes.
    #[tokio::test]
    async fn proxy_rejects_sni_mismatch() {
        let up = echo_server().await;
        // A non-loopback name that resolves to this machine, so the matching
        // case can reach the echo server.
        let name = "ligus-sni-test.localhost";
        let resolves = tokio::net::lookup_host((name, up)).await.is_ok_and(|mut a| a.next().is_some());
        let p = EgressProxy::start(vec![HostPort::new(name, up)], None).unwrap();
        let (mut s, resp) = connect_through(&p, &format!("{name}:{up}"), true).await;
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        s.write_all(&client_hello(Some("evil.example"))).await.unwrap();
        let mut b = [0u8; 16];
        let n = tokio::time::timeout(std::time::Duration::from_secs(5), s.read(&mut b)).await.unwrap().unwrap_or(0);
        assert_eq!(n, 0, "a mismatched SNI is cut");
        assert_eq!(p.denials(), vec![HostPort::new(name, up)]);
        // Plain bytes (not TLS) through CONNECT to a non-loopback host: cut too.
        let (mut s, _) = connect_through(&p, &format!("{name}:{up}"), true).await;
        s.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
        let n = tokio::time::timeout(std::time::Duration::from_secs(5), s.read(&mut b)).await.unwrap().unwrap_or(0);
        assert_eq!(n, 0);
        if resolves {
            // A matching SNI passes the pin, but this name resolves to this
            // machine, so the connection is still refused (no rebinding to
            // local services; `proxy_refuses_names_that_resolve_to_loopback_or_link_local`).
            let (mut s, _) = connect_through(&p, &format!("{name}:{up}"), true).await;
            let hello = client_hello(Some(name));
            s.write_all(&hello).await.unwrap();
            let mut echoed = vec![0u8; hello.len()];
            let got = tokio::time::timeout(std::time::Duration::from_secs(5), s.read_exact(&mut echoed)).await;
            assert!(!matches!(got, Ok(Ok(_))), "a name resolving to loopback is not spliced");
        }
    }

    /// A plain Claude Code turn hits exactly these two (live check, Claude
    /// Code 2.1.x): neither matters, so no chat notice.
    #[test]
    fn known_optional_refusals_stay_quiet() {
        let dd = HostPort::new("http-intake.logs.us5.datadoghq.com", 443);
        let mcp = HostPort::new("mcp-proxy.anthropic.com", 443);
        assert_eq!(classify_refused(&dd), RefusedKind::Telemetry);
        assert_eq!(classify_refused(&mcp), RefusedKind::ClaudeConnectors);
        assert_eq!(classify_refused(&HostPort::new("evil-datadoghq.com", 443)), RefusedKind::Other);
        assert_eq!(refused_notice(&[dd.clone(), mcp.clone()]), None);
        let n = refused_notice(&[dd, mcp, HostPort::new("example.com", 443), HostPort::new("example.com", 443)]).unwrap();
        assert!(n.contains("blocked example.com —"), "{n}");
        assert!(!n.contains("example.com, example.com"), "{n}");
        assert!(n.contains("telemetry (http-intake.logs.us5.datadoghq.com)"), "{n}");
        assert!(n.contains("claude.ai connectors (mcp-proxy.anthropic.com"), "{n}");
    }
}

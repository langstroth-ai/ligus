//! Operator-listed model gateways (docs/design/v2/model-gateways.md).
//!
//! A host's operator lists the LLM gateways its agents may use (LiteLLM,
//! kgateway, Envoy AI Gateway, ... anything speaking the OpenAI or Anthropic
//! wire) in a mounted file, `LIGUS_MODEL_GATEWAYS_FILE`. Each entry names a
//! base URL and how to authenticate to it; the credential is always a file
//! (a projected ServiceAccount token, an API key, a client certificate),
//! read by this process on every request and never handed to an agent child.
//!
//! - [`ModelGateways`] is the host's list ([`host`]), loaded once.
//! - [`Gateway`] is one entry: its TLS client ([`Gateway::client`]: the pinned
//!   CA, the mTLS identity, a vetted address) and its credential headers
//!   ([`Gateway::auth_headers`]). The egress proxy's gateway route and the
//!   HTTP model clients both go through [`Gateway::apply`], so a CLI turn and
//!   a hosted-API turn send exactly the same credential.
//! - [`ModelGateways::route`] decides a turn's model route: a named gateway,
//!   an endpoint that is a listed gateway, direct, or refused (a gateway host
//!   is exclusive by default).
//! - [`attribution_headers`]: the opt-in pseudonymous workspace and agent ids.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use ligus_core::{HostModelGateway, ModelProviderKind};
use serde::Deserialize;
use uuid::Uuid;
use zeroize::Zeroizing;

/// The env var naming the gateways file.
pub const GATEWAYS_FILE_ENV: &str = "LIGUS_MODEL_GATEWAYS_FILE";
/// `0` turns exclusivity off: the host then also runs turns that don't use a
/// listed gateway.
pub const EXCLUSIVE_ENV: &str = "LIGUS_MODEL_GATEWAYS_EXCLUSIVE";
/// An operator-provided attribution key file (32+ bytes, the same on every
/// host of a workspace so the pseudonymous ids agree).
pub const ATTRIBUTION_KEY_ENV: &str = "LIGUS_MODEL_GATEWAY_ATTRIBUTION_KEY_FILE";
/// The host-generated attribution key, in the data dir, when none is given.
pub const ATTRIBUTION_KEY_FILE: &str = "model-gateway-attribution.key";
/// The HMAC message prefix of the attribution ids (phase0-specs §0.1).
pub const ATTRIBUTION_CONTEXT: &[u8] = b"ligus-gateway-attribution-v1\0";
/// Attribution header names.
pub const HEADER_WORKSPACE: &str = "x-ligus-workspace";
pub const HEADER_AGENT: &str = "x-ligus-agent";
/// The key a CLI is given when it insists on one: never a credential.
pub const DUMMY_KEY: &str = "ligus-gateway";
/// The egress proxy's path prefix for gateway routes.
pub const ROUTE_PREFIX: &str = "/ligus-gw/";

/// The wire a gateway speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Api {
    #[default]
    #[serde(alias = "openAI", alias = "open-ai")]
    Openai,
    Anthropic,
}

impl Api {
    pub fn as_str(self) -> &'static str {
        match self {
            Api::Openai => "openai",
            Api::Anthropic => "anthropic",
        }
    }
}

/// How the host authenticates to a gateway.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase", deny_unknown_fields)]
pub enum AuthConfig {
    /// A projected, audience-bound ServiceAccount token, sent as a bearer.
    #[serde(rename_all = "camelCase")]
    ServiceAccountToken { token_file: PathBuf },
    /// An API key file; `header` is `authorization` (bearer, default),
    /// `x-api-key` or `api-key`.
    #[serde(rename_all = "camelCase")]
    ApiKey {
        key_file: PathBuf,
        #[serde(default)]
        header: Option<String>,
    },
    /// A client certificate and key (PEM); the CA is `caFile`.
    #[serde(rename_all = "camelCase")]
    Mtls { cert_file: PathBuf, client_key_file: PathBuf },
    /// No credential (a mesh identity or the network position authenticates).
    None,
}

/// One gateway entry in the operator's file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayConfig {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub api: Api,
    #[serde(default)]
    pub allow_plaintext: bool,
    #[serde(default)]
    pub attribution: bool,
    pub auth: AuthConfig,
    #[serde(default)]
    pub ca_file: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GatewaysFile {
    #[serde(default)]
    gateways: Vec<GatewayConfig>,
}

/// Why a gateway or the file was refused, or a request couldn't be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GatewayError {
    #[error("model gateways file: {0}")]
    Config(String),
    #[error("model gateway '{0}': {1}")]
    Gateway(String, String),
}

fn gw_err(name: &str, why: impl Into<String>) -> GatewayError {
    GatewayError::Gateway(name.to_string(), why.into())
}

/// One operator-listed gateway.
#[derive(Debug)]
pub struct Gateway {
    cfg: GatewayConfig,
    base: url::Url,
}

impl Gateway {
    /// Validate one entry.
    pub fn new(cfg: GatewayConfig) -> Result<Self, GatewayError> {
        let name = cfg.name.clone();
        if !HostModelGateway::valid_name(&name) {
            return Err(gw_err(&name, "name must be 1-63 of a-z, 0-9 and '-', not starting or ending with '-'"));
        }
        let base = url::Url::parse(cfg.url.trim()).map_err(|e| gw_err(&name, format!("url: {e}")))?;
        if !base.username().is_empty() || base.password().is_some() {
            return Err(gw_err(&name, "url must not carry credentials"));
        }
        if base.query().is_some() || base.fragment().is_some() {
            return Err(gw_err(&name, "url must not carry a query or fragment"));
        }
        match base.scheme() {
            "https" => {}
            "http" if cfg.allow_plaintext => {}
            "http" => return Err(gw_err(&name, "http:// needs allowPlaintext (only inside a mesh or an isolated path)")),
            s => return Err(gw_err(&name, format!("unsupported scheme {s}"))),
        }
        if base.host_str().is_none_or(str::is_empty) {
            return Err(gw_err(&name, "url has no host"));
        }
        if matches!(cfg.auth, AuthConfig::Mtls { .. }) && base.scheme() != "https" {
            return Err(gw_err(&name, "mtls needs an https:// url"));
        }
        if let Some(h) = base.host_str() {
            if let Ok(ip) = h.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>() {
                if addr_forbidden(ip, ip.is_loopback()) {
                    return Err(gw_err(&name, "url points at a link-local, metadata or unspecified address"));
                }
            }
        }
        Ok(Self { cfg, base })
    }

    pub fn name(&self) -> &str {
        &self.cfg.name
    }
    pub fn api(&self) -> Api {
        self.cfg.api
    }
    pub fn attribution(&self) -> bool {
        self.cfg.attribution
    }
    pub fn config(&self) -> &GatewayConfig {
        &self.cfg
    }

    /// The base URL without a trailing slash.
    pub fn base_url(&self) -> String {
        self.base.as_str().trim_end_matches('/').to_string()
    }

    /// `host:port` of the gateway (`[v6]:port` for IPv6).
    pub fn host_port(&self) -> String {
        let host = self.base.host_str().unwrap_or_default();
        format!("{host}:{}", self.base.port_or_known_default().unwrap_or(443))
    }

    /// `<base>/<path>` (`path` may start with `/` and carry a query).
    pub fn join(&self, path: &str) -> String {
        format!("{}/{}", self.base_url(), path.trim_start_matches('/'))
    }

    /// The endpoint a hosted-API turn posts to.
    pub fn chat_endpoint(&self) -> String {
        match self.cfg.api {
            Api::Openai => self.join("v1/chat/completions"),
            Api::Anthropic => self.join("v1/messages"),
        }
    }

    /// Whether `endpoint` is on this gateway (same origin, under its path).
    pub fn covers(&self, endpoint: &str) -> bool {
        let Ok(u) = url::Url::parse(endpoint.trim()) else { return false };
        if u.scheme() != self.base.scheme()
            || u.host_str().map(str::to_ascii_lowercase) != self.base.host_str().map(str::to_ascii_lowercase)
            || u.port_or_known_default() != self.base.port_or_known_default()
            || !u.username().is_empty()
        {
            return false;
        }
        let prefix = self.base.path().trim_end_matches('/');
        let p = u.path();
        prefix.is_empty() || p == prefix || p.starts_with(&format!("{prefix}/"))
    }

    /// The path (and query) of `endpoint` below this gateway's base path,
    /// starting with `/`, when [`Self::covers`] it. Taken from the raw string
    /// (the URL parser would resolve dot segments and hide them).
    pub fn relative_path(&self, endpoint: &str) -> Option<String> {
        if !self.covers(endpoint) {
            return None;
        }
        let e = endpoint.trim();
        let after_scheme = &e[e.find("://")? + 3..];
        let raw_path = after_scheme.find(['/', '?']).map(|i| &after_scheme[i..]).unwrap_or("");
        let raw_path = raw_path.split('#').next().unwrap_or_default();
        let prefix = self.base.path().trim_end_matches('/');
        let rest = raw_path.strip_prefix(prefix).unwrap_or(raw_path);
        Some(if rest.starts_with('/') { rest.to_string() } else { format!("/{rest}") })
    }

    /// Every file holding this gateway's credential material (for the
    /// sandbox's deny list).
    pub fn secret_files(&self) -> Vec<PathBuf> {
        let mut v = match &self.cfg.auth {
            AuthConfig::ServiceAccountToken { token_file } => vec![token_file.clone()],
            AuthConfig::ApiKey { key_file, .. } => vec![key_file.clone()],
            AuthConfig::Mtls { cert_file, client_key_file } => vec![cert_file.clone(), client_key_file.clone()],
            AuthConfig::None => Vec::new(),
        };
        v.extend(self.cfg.ca_file.clone());
        v
    }

    /// The credential headers, read from their files now (a rotated token
    /// is picked up on the next request).
    pub fn auth_headers(&self) -> Result<Vec<(&'static str, Zeroizing<String>)>, GatewayError> {
        let read = |p: &Path, what: &str| -> Result<Zeroizing<String>, GatewayError> {
            let raw = Zeroizing::new(std::fs::read(p).map_err(|e| gw_err(self.name(), format!("can't read its {what} ({e})")))?);
            let s = std::str::from_utf8(&raw).map_err(|_| gw_err(self.name(), format!("its {what} is not UTF-8")))?.trim();
            if s.is_empty() {
                return Err(gw_err(self.name(), format!("its {what} is empty")));
            }
            if s.bytes().any(|b| b < 0x21 || b == 0x7f) {
                return Err(gw_err(self.name(), format!("its {what} has whitespace or control bytes")));
            }
            Ok(Zeroizing::new(s.to_string()))
        };
        Ok(match &self.cfg.auth {
            AuthConfig::ServiceAccountToken { token_file } => {
                let t = read(token_file, "ServiceAccount token")?;
                vec![("authorization", Zeroizing::new(format!("Bearer {}", t.as_str())))]
            }
            AuthConfig::ApiKey { key_file, header } => {
                let k = read(key_file, "API key")?;
                match header.as_deref().map(str::to_ascii_lowercase).as_deref() {
                    None | Some("") | Some("authorization") => vec![("authorization", Zeroizing::new(format!("Bearer {}", k.as_str())))],
                    Some("x-api-key") => vec![("x-api-key", k)],
                    Some("api-key") => vec![("api-key", k)],
                    Some(h) => return Err(gw_err(self.name(), format!("unsupported api key header {h}"))),
                }
            }
            AuthConfig::Mtls { .. } | AuthConfig::None => Vec::new(),
        })
    }

    /// Resolve the gateway's host and vet every address (never link-local,
    /// metadata, unspecified or multicast; loopback only when the URL names
    /// loopback itself). The vetted addresses are the ones dialed.
    pub async fn vetted_addrs(&self) -> Result<Vec<SocketAddr>, GatewayError> {
        let host = self.base.host_str().unwrap_or_default().trim_start_matches('[').trim_end_matches(']').to_string();
        let port = self.base.port_or_known_default().unwrap_or(443);
        let loopback_ok = host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|e| gw_err(self.name(), format!("can't resolve {host} ({e})")))?
            .collect();
        if addrs.is_empty() {
            return Err(gw_err(self.name(), format!("{host} resolves to nothing")));
        }
        if let Some(bad) = addrs.iter().find(|a| addr_forbidden(a.ip(), loopback_ok)) {
            return Err(gw_err(self.name(), format!("{host} resolves to a forbidden address ({})", bad.ip())));
        }
        Ok(addrs)
    }

    /// A TLS client for this gateway: trusts only `caFile` when set (else the
    /// platform roots), presents the mTLS identity, and dials the vetted
    /// addresses, and never follows a redirect. Built per turn (or per
    /// proxied request), so a rotated certificate is picked up.
    pub async fn client(&self) -> Result<reqwest::Client, GatewayError> {
        self.client_with_roots(ligus_forge::uses_platform_roots()).await
    }

    /// [`Self::client`], for a host with (`platform_roots`) or without
    /// system CA certificates.
    pub(crate) async fn client_with_roots(&self, platform_roots: bool) -> Result<reqwest::Client, GatewayError> {
        let addrs = self.vetted_addrs().await?;
        // A pinned CA replaces the roots, so the builder must not carry the
        // preconfigured bundled-roots TLS a host without system CAs gets: it
        // would silently override `tls_certs_only` and `identity`. An mTLS
        // identity without a pinned CA there can't be presented: refused.
        let mtls = matches!(self.cfg.auth, AuthConfig::Mtls { .. });
        if mtls && self.cfg.ca_file.is_none() && !platform_roots {
            return Err(gw_err(self.name(), "mtls needs caFile on a host without system CA certificates"));
        }
        let platform_roots = platform_roots || self.cfg.ca_file.is_some();
        // Never follow a redirect: the credential would ride along (reqwest
        // keeps `x-api-key`/`api-key` across origins and `Authorization` on
        // the same host), to an address that was never vetted. A 3xx goes
        // back to the caller as is.
        let mut b = ligus_forge::http_client_builder_with_roots(platform_roots)
            .connect_timeout(crate::provider::HTTP_CONNECT_TIMEOUT)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none());
        if let Some(host) = self.base.host_str() {
            let host = host.trim_start_matches('[').trim_end_matches(']');
            if host.parse::<IpAddr>().is_err() {
                b = b.resolve_to_addrs(host, &addrs);
            }
        }
        if let Some(ca) = &self.cfg.ca_file {
            let pem = std::fs::read(ca).map_err(|e| gw_err(self.name(), format!("can't read its CA file ({e})")))?;
            let certs = reqwest::Certificate::from_pem_bundle(&pem).map_err(|e| gw_err(self.name(), format!("CA file: {e}")))?;
            if certs.is_empty() {
                return Err(gw_err(self.name(), "CA file holds no certificate"));
            }
            b = b.tls_certs_only(certs);
        }
        if let AuthConfig::Mtls { cert_file, client_key_file } = &self.cfg.auth {
            let mut pem = Zeroizing::new(std::fs::read(cert_file).map_err(|e| gw_err(self.name(), format!("can't read its client certificate ({e})")))?);
            pem.push(b'\n');
            let key = Zeroizing::new(std::fs::read(client_key_file).map_err(|e| gw_err(self.name(), format!("can't read its client key ({e})")))?);
            pem.extend_from_slice(&key);
            let id = reqwest::Identity::from_pem(&pem).map_err(|e| gw_err(self.name(), format!("client identity: {e}")))?;
            b = b.identity(id);
        }
        b.build().map_err(|e| gw_err(self.name(), format!("TLS client: {e}")))
    }

    /// The one place a gateway request gets its credential: drop every
    /// credential the caller set (the builder's own headers are replaced
    /// here), then add this gateway's and, when on, the attribution ids.
    pub fn apply(&self, mut req: reqwest::RequestBuilder, attr: Option<&Attribution>) -> Result<reqwest::RequestBuilder, GatewayError> {
        for (k, v) in self.auth_headers()? {
            req = req.header(k, v.as_str());
        }
        if self.cfg.attribution {
            if let Some(a) = attr {
                for (k, v) in a.headers() {
                    req = req.header(k, v);
                }
            }
        }
        Ok(req)
    }

    /// [`Self::apply`], logging (never the value) and sending without a
    /// credential when it can't be read: the gateway then refuses the
    /// request, and dispatch already checked the credential before the turn.
    pub fn apply_or_warn(&self, req: reqwest::RequestBuilder, attr: Option<&Attribution>) -> reqwest::RequestBuilder {
        match self.injected_headers(attr) {
            Ok(h) => h.iter().fold(req, |r, (k, v)| r.header(k.as_str(), v.as_str())),
            Err(e) => {
                tracing::warn!(target: "dispatch", "{e}");
                req
            }
        }
    }

    /// The headers [`Self::apply`] adds, as plain pairs (the egress proxy's
    /// route, which writes them itself).
    pub fn injected_headers(&self, attr: Option<&Attribution>) -> Result<Vec<(String, Zeroizing<String>)>, GatewayError> {
        let mut v: Vec<(String, Zeroizing<String>)> = self.auth_headers()?.into_iter().map(|(k, val)| (k.to_string(), val)).collect();
        if self.cfg.attribution {
            if let Some(a) = attr {
                v.extend(a.headers().into_iter().map(|(k, val)| (k.to_string(), Zeroizing::new(val))));
            }
        }
        Ok(v)
    }
}

/// Whether a CLI child may send `method path` to a gateway through the
/// proxy's route: only the model API of the gateway's wire (chat,
/// completions, responses, embeddings, messages, token counts, the model
/// list). Anything else on the gateway host (LiteLLM's key and user admin
/// routes, a health or metrics page) never gets the injected credential.
/// Dot segments, encoded dots or slashes, backslashes and empty segments are
/// refused outright, so a path can't climb out of an allowed prefix.
pub fn route_path_allowed(api: Api, method: &str, path_and_query: &str) -> bool {
    let path = path_and_query.split('?').next().unwrap_or_default();
    let lower = path.to_ascii_lowercase();
    if !path.starts_with('/')
        || path.contains("//")
        || path.contains('\\')
        || lower.contains("%2e")
        || lower.contains("%2f")
        || lower.contains("%5c")
        || path.split('/').any(|seg| seg == "." || seg == "..")
    {
        return false;
    }
    let p = path.trim_end_matches('/');
    let get = method.eq_ignore_ascii_case("GET");
    let post = method.eq_ignore_ascii_case("POST");
    let models = p == "/v1/models" || p.starts_with("/v1/models/");
    match api {
        Api::Openai => {
            (post && matches!(p, "/v1/chat/completions" | "/v1/completions" | "/v1/responses" | "/v1/embeddings")) || (get && models)
        }
        Api::Anthropic => (post && matches!(p, "/v1/messages" | "/v1/messages/count_tokens")) || (get && models),
    }
}

/// Request headers every wire's model API needs: content negotiation, the
/// client's user agent, and an idempotency key for retried requests.
const COMMON_REQUEST_HEADERS: &[&str] = &["content-type", "accept", "accept-encoding", "user-agent", "idempotency-key"];
/// The Anthropic wire's version and beta-feature headers.
const ANTHROPIC_REQUEST_HEADERS: &[&str] = &["anthropic-version", "anthropic-beta"];
/// The OpenAI wire's beta-feature header.
const OPENAI_REQUEST_HEADERS: &[&str] = &["openai-beta"];

/// The request headers the egress proxy's gateway route forwards for `api`
/// (lowercase).
pub fn request_header_allow_list_of(api: Api) -> impl Iterator<Item = &'static str> {
    let wire = match api {
        Api::Openai => OPENAI_REQUEST_HEADERS,
        Api::Anthropic => ANTHROPIC_REQUEST_HEADERS,
    };
    COMMON_REQUEST_HEADERS.iter().chain(wire).copied()
}

/// Whether the gateway route forwards a client's request header `name` to a
/// gateway speaking `api`: only the allow-list
/// ([`request_header_allow_list_of`]). Everything else a sandboxed agent
/// sends is dropped: gateway identity, budget and routing headers
/// (`x-litellm-*`, `x-envoy-*`, `x-forwarded-*`, `forwarded`, ...), its own
/// credentials, and Ligus's attribution ids, which only Ligus sets
/// ([`Gateway::injected_headers`]).
pub fn forwardable_request_header(api: Api, name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    !is_client_credential_header(&n) && request_header_allow_list_of(api).any(|h| h == n)
}

/// Request headers a client may never pass to a gateway: credentials, and
/// the attribution ids (only Ligus sets those).
pub fn is_client_credential_header(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    matches!(n.as_str(), "authorization" | "proxy-authorization" | "x-api-key" | "api-key" | "cookie") || n.starts_with("x-ligus-")
}

/// Addresses a gateway may never be dialed at: link-local (cloud metadata
/// 169.254.169.254), the Alibaba and AWS IPv6 metadata addresses,
/// unspecified, multicast, broadcast; loopback unless `loopback_ok`.
pub fn addr_forbidden(ip: IpAddr, loopback_ok: bool) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    };
    match ip {
        IpAddr::V4(a) => {
            (a.is_loopback() && !loopback_ok)
                || a.is_link_local()
                || a.is_unspecified()
                || a.is_multicast()
                || a.is_broadcast()
                || a.octets()[0] == 0
                || a.octets() == [100, 100, 100, 200]
        }
        IpAddr::V6(a) => {
            (a.is_loopback() && !loopback_ok)
                || a.is_unspecified()
                || a.is_multicast()
                || (a.segments()[0] & 0xffc0) == 0xfe80
                || a == "fd00:ec2::254".parse::<std::net::Ipv6Addr>().expect("literal")
        }
    }
}

/// The pseudonymous ids a gateway with attribution on receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    pub workspace: Option<String>,
    pub agent: Option<String>,
}

impl Attribution {
    /// Keyed hashes of `workspace` and `agent` under `key`.
    pub fn new(key: &[u8], workspace: Option<Uuid>, agent: Option<Uuid>) -> Self {
        Self { workspace: workspace.map(|w| pseudonym(key, b"w", w)), agent: agent.map(|a| pseudonym(key, b"a", a)) }
    }
    pub fn headers(&self) -> Vec<(&'static str, String)> {
        let mut v = Vec::new();
        if let Some(w) = &self.workspace {
            v.push((HEADER_WORKSPACE, w.clone()));
        }
        if let Some(a) = &self.agent {
            v.push((HEADER_AGENT, a.clone()));
        }
        v
    }
}

/// `hex(HMAC-SHA256(key, ATTRIBUTION_CONTEXT || kind || 0 || id)[..16])`.
pub fn pseudonym(key: &[u8], kind: &[u8], id: Uuid) -> String {
    use hmac::{KeyInit, Mac};
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(key).expect("any key length");
    mac.update(ATTRIBUTION_CONTEXT);
    mac.update(kind);
    mac.update(&[0]);
    mac.update(id.as_bytes());
    let out = mac.finalize().into_bytes();
    out[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// Which gateway a turn uses, with the ids its attribution hashes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GatewaySelection {
    pub name: String,
    pub workspace_id: Option<Uuid>,
    pub agent_id: Option<Uuid>,
}

impl GatewaySelection {
    pub fn named(name: impl Into<String>) -> Self {
        Self { name: name.into(), ..Default::default() }
    }
    pub fn with_ids(mut self, workspace: Option<Uuid>, agent: Option<Uuid>) -> Self {
        self.workspace_id = workspace;
        self.agent_id = agent;
        self
    }
}

/// A gateway resolved for a turn, with its attribution.
#[derive(Debug, Clone)]
pub struct ResolvedGateway {
    pub gateway: Arc<Gateway>,
    pub attribution: Option<Attribution>,
}

/// The host's gateway list.
#[derive(Debug, Default)]
pub struct ModelGateways {
    gateways: Vec<Arc<Gateway>>,
    exclusive: bool,
    attribution_key: Option<Zeroizing<Vec<u8>>>,
}

impl ModelGateways {
    /// Parse and validate a gateways file's contents.
    pub fn parse(text: &str) -> Result<Vec<Gateway>, GatewayError> {
        let f: GatewaysFile = serde_json::from_str(text).map_err(|e| GatewayError::Config(e.to_string()))?;
        let mut out: Vec<Gateway> = Vec::new();
        for c in f.gateways {
            let g = Gateway::new(c)?;
            if out.iter().any(|o| o.name() == g.name()) {
                return Err(GatewayError::Config(format!("gateway '{}' is listed twice", g.name())));
            }
            out.push(g);
        }
        if out.len() > ligus_core::workspace_host::MAX_HOST_GATEWAYS {
            return Err(GatewayError::Config(format!("at most {} gateways", ligus_core::workspace_host::MAX_HOST_GATEWAYS)));
        }
        Ok(out)
    }

    pub fn new(gateways: Vec<Gateway>, exclusive: bool, attribution_key: Option<Vec<u8>>) -> Self {
        Self { gateways: gateways.into_iter().map(Arc::new).collect(), exclusive, attribution_key: attribution_key.map(Zeroizing::new) }
    }

    /// Load from the environment (`LIGUS_MODEL_GATEWAYS_FILE`, ...). No
    /// variable: no gateways. A listed file that can't be read or is invalid
    /// is an error (the host must not run without the gateways its operator
    /// listed). `data_dir` holds the generated attribution key.
    pub fn from_env(data_dir: Option<&Path>) -> Result<Self, GatewayError> {
        let Some(path) = ligus_core::compat::var(GATEWAYS_FILE_ENV).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
            return Ok(Self::default());
        };
        let text = std::fs::read_to_string(&path).map_err(|e| GatewayError::Config(format!("{path}: {e}")))?;
        let gateways = Self::parse(&text)?;
        let exclusive = !gateways.is_empty() && !ligus_core::compat::var(EXCLUSIVE_ENV).is_ok_and(|v| v.trim() == "0" || v.trim().eq_ignore_ascii_case("false"));
        let key = if gateways.iter().any(|g| g.attribution()) { Some(attribution_key(data_dir)?) } else { None };
        Ok(Self::new(gateways, exclusive, key))
    }

    pub fn is_empty(&self) -> bool {
        self.gateways.is_empty()
    }
    pub fn exclusive(&self) -> bool {
        self.exclusive
    }
    pub fn get(&self, name: &str) -> Option<Arc<Gateway>> {
        self.gateways.iter().find(|g| g.name() == name).cloned()
    }
    pub fn all(&self) -> &[Arc<Gateway>] {
        &self.gateways
    }

    /// What the host advertises: names and wires, sorted.
    pub fn offers(&self) -> Vec<HostModelGateway> {
        let mut v: Vec<HostModelGateway> =
            self.gateways.iter().map(|g| HostModelGateway { name: g.name().to_string(), api: g.api().as_str().to_string() }).collect();
        v.sort();
        v
    }

    /// Every credential file (for the sandbox deny list).
    pub fn secret_files(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self.gateways.iter().flat_map(|g| g.secret_files()).collect();
        if let Some(p) = ligus_core::compat::var(ATTRIBUTION_KEY_ENV).ok().filter(|s| !s.trim().is_empty()) {
            v.push(PathBuf::from(p.trim()));
        }
        v
    }

    fn attribution_for(&self, g: &Gateway, sel: &GatewaySelection) -> Option<Attribution> {
        let key = self.attribution_key.as_ref().filter(|_| g.attribution())?;
        Some(Attribution::new(key, sel.workspace_id, sel.agent_id))
    }

    /// The gateway a turn uses.
    ///
    /// - `selected` names a gateway: it must be listed here.
    /// - Otherwise an HTTP runtime whose `endpoint` is on a listed gateway
    ///   goes through it (credential injected).
    /// - Otherwise the turn is direct, unless this host is exclusive.
    pub fn route(
        &self,
        provider: ModelProviderKind,
        endpoint: &str,
        selected: Option<&GatewaySelection>,
    ) -> Result<Option<ResolvedGateway>, String> {
        if let Some(sel) = selected.filter(|s| !s.name.is_empty()) {
            let Some(g) = self.get(&sel.name) else {
                return Err(format!(
                    "this runtime uses the model gateway '{}', which this host doesn't offer; bind the agent to a host that offers it (the hosts list shows which)",
                    sel.name
                ));
            };
            if let Some(why) = cli_unsupported(provider, g.api()) {
                return Err(why);
            }
            let attribution = self.attribution_for(&g, sel);
            return Ok(Some(ResolvedGateway { gateway: g, attribution }));
        }
        if !provider.is_cli_agent() && !endpoint.trim().is_empty() {
            if let Some(g) = self.gateways.iter().find(|g| g.covers(endpoint)) {
                // The credential goes to the gateway wire's model API only
                // (as on the proxy route): an endpoint on its admin routes,
                // its root or the other wire is refused, not sent.
                if let Some(why) = cli_unsupported(provider, g.api()) {
                    return Err(why);
                }
                let model_api = g.relative_path(endpoint).is_some_and(|p| route_path_allowed(g.api(), "POST", &p));
                if !model_api {
                    return Err(format!(
                        "this runtime's endpoint is on the model gateway '{}' but isn't its model API; set the runtime's gateway to '{}' instead of an endpoint",
                        g.name(),
                        g.name()
                    ));
                }
                let attribution = self.attribution_for(g, &GatewaySelection::default());
                return Ok(Some(ResolvedGateway { gateway: g.clone(), attribution }));
            }
        }
        // The one exemption, a Ligus daemon on this host, holds only for a
        // loopback endpoint: a `ligus-daemon` runtime pointed anywhere else
        // is model traffic leaving the host like any other.
        let local_daemon = provider == ModelProviderKind::LigusDaemon && endpoint_is_loopback(endpoint);
        if self.exclusive && !local_daemon {
            let names: Vec<&str> = self.gateways.iter().map(|g| g.name()).collect();
            return Err(format!(
                "this host sends model traffic only through its operator's model gateways ({}); set the runtime's gateway to one of them",
                names.join(", ")
            ));
        }
        Ok(None)
    }
}

/// Whether `endpoint` names this host: `localhost` or a loopback address.
fn endpoint_is_loopback(endpoint: &str) -> bool {
    match url::Url::parse(endpoint.trim()).ok().and_then(|u| u.host().map(|h| h.to_owned())) {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Why a CLI can't use a gateway of wire `api` (no base-URL setting, or a
/// wire it doesn't speak).
pub fn cli_unsupported(provider: ModelProviderKind, api: Api) -> Option<String> {
    let ok = match provider {
        ModelProviderKind::ClaudeCode => api == Api::Anthropic,
        ModelProviderKind::Codex => api == Api::Openai,
        ModelProviderKind::Aider | ModelProviderKind::Hermes => true,
        // pi reads its base URL from a provider config Ligus writes before
        // the proxy (and so the route) exists; Antigravity and Copilot have
        // no base-URL setting at all.
        ModelProviderKind::Pi | ModelProviderKind::Antigravity | ModelProviderKind::Copilot => false,
        // HTTP runtimes: the client speaks the gateway's wire.
        ModelProviderKind::Anthropic => api == Api::Anthropic,
        ModelProviderKind::OpenAI
        | ModelProviderKind::OpenRouter
        | ModelProviderKind::Azure
        | ModelProviderKind::Custom
        | ModelProviderKind::Ollama
        | ModelProviderKind::LigusDaemon => api == Api::Openai,
    };
    if matches!(provider, ModelProviderKind::Pi | ModelProviderKind::Antigravity | ModelProviderKind::Copilot) {
        return Some(format!("the {provider:?} CLI has no base-URL setting Ligus can point at a model gateway; use another CLI or an HTTP model runtime"));
    }
    (!ok).then(|| format!("a {provider:?} runtime can't use a model gateway that speaks the {} wire", api.as_str()))
}

/// The env a CLI child gets for a gateway route at `base` (the proxy's
/// loopback route URL): its base URL variable(s) and a dummy key.
pub fn cli_env(provider: ModelProviderKind, api: Api, base: &str) -> Vec<(String, String)> {
    let base = base.trim_end_matches('/');
    let openai = || {
        vec![
            ("OPENAI_BASE_URL".to_string(), format!("{base}/v1")),
            ("OPENAI_API_BASE".to_string(), format!("{base}/v1")),
            ("OPENAI_API_KEY".to_string(), DUMMY_KEY.to_string()),
        ]
    };
    let anthropic = || {
        vec![
            ("ANTHROPIC_BASE_URL".to_string(), base.to_string()),
            ("ANTHROPIC_API_KEY".to_string(), DUMMY_KEY.to_string()),
        ]
    };
    match (provider, api) {
        (ModelProviderKind::Antigravity | ModelProviderKind::Copilot, _) => Vec::new(),
        (_, Api::Anthropic) => anthropic(),
        (_, Api::Openai) => openai(),
    }
}

/// The env variables a gateway route sets (so the plan can drop any the
/// caller passed: the child sees only the route).
pub const CLI_ENV_KEYS: &[&str] = &[
    "OPENAI_BASE_URL",
    "OPENAI_API_BASE",
    "OPENAI_API_KEY",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "OPENROUTER_API_KEY",
];

/// The operator's attribution key, else this host's generated one.
fn attribution_key(data_dir: Option<&Path>) -> Result<Vec<u8>, GatewayError> {
    if let Some(p) = ligus_core::compat::var(ATTRIBUTION_KEY_ENV).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        let k = std::fs::read(&p).map_err(|e| GatewayError::Config(format!("attribution key {p}: {e}")))?;
        if k.len() < 16 {
            return Err(GatewayError::Config("attribution key must be at least 16 bytes".into()));
        }
        return Ok(k);
    }
    let Some(dir) = data_dir else {
        let mut k = vec![0u8; 32];
        getrandom::fill(&mut k).map_err(|e| GatewayError::Config(e.to_string()))?;
        return Ok(k);
    };
    let path = dir.join(ATTRIBUTION_KEY_FILE);
    if let Ok(k) = std::fs::read(&path) {
        if k.len() >= 16 {
            return Ok(k);
        }
    }
    let mut k = vec![0u8; 32];
    getrandom::fill(&mut k).map_err(|e| GatewayError::Config(e.to_string()))?;
    crate::private_file::write_private_atomic(&path, &k).map_err(|e| GatewayError::Config(format!("{}: {e}", path.display())))?;
    Ok(k)
}

fn slot() -> &'static RwLock<Arc<ModelGateways>> {
    static S: OnceLock<RwLock<Arc<ModelGateways>>> = OnceLock::new();
    S.get_or_init(|| RwLock::new(Arc::new(ModelGateways::default())))
}

/// This host's gateways (empty until [`init_host`]).
pub fn host() -> Arc<ModelGateways> {
    slot().read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Load this host's gateways from the environment, deny their credential
/// files to agent children, and install them. serve and workers call it at
/// startup; an error must stop the host.
pub fn init_host(data_dir: &Path) -> Result<Arc<ModelGateways>, GatewayError> {
    let g = Arc::new(ModelGateways::from_env(Some(data_dir))?);
    install(g.clone());
    Ok(g)
}

/// Install `g` as this host's list (also tests), adding its credential files
/// and their directories to the sandbox deny list.
pub fn install(g: Arc<ModelGateways>) {
    let mut deny: Vec<PathBuf> = Vec::new();
    for f in g.secret_files() {
        if let Some(d) = f.parent().filter(|d| d.as_os_str().len() > 1) {
            deny.push(d.to_path_buf());
        }
        deny.push(f);
    }
    if !deny.is_empty() {
        crate::executor::configure_secret_files(&deny);
    }
    *slot().write().unwrap_or_else(|e| e.into_inner()) = g;
}

#[cfg(test)]
mod tests;

//! Operator-listed model gateways (docs/design/v2/model-gateways.md).
//!
//! A host's operator lists the LLM gateways its agents may use (LiteLLM,
//! kgateway, Envoy AI Gateway, ... anything speaking the OpenAI or Anthropic
//! wire) in a file: `LIGUS_MODEL_GATEWAYS_FILE`, else `model-gateways.json` in
//! the data dir (what `ligus gateway add` writes). Each entry names a
//! base URL and how to authenticate to it; the credential is always a file
//! (a projected ServiceAccount token, an API key, a client certificate),
//! read by this process on every request and never handed to an agent child.
//!
//! - [`ModelGateways`] is the host's list ([`host`]), loaded at startup and,
//!   on serve and workers, again whenever the file changes ([`watch_host`]).
//! - [`Gateway`] is one entry: its TLS client ([`Gateway::client`]: the pinned
//!   CA, the mTLS identity, a vetted address) and its credential headers
//!   ([`Gateway::auth_headers`]). The egress proxy's gateway route and the
//!   HTTP model clients both go through [`Gateway::apply`], so a CLI turn and
//!   a hosted-API turn send exactly the same credential.
//! - [`ModelGateways::route`] decides a turn's model route: a named gateway,
//!   an endpoint that is a listed gateway, direct, or refused (a gateway host
//!   is exclusive by default).
//! - [`attribution_headers`]: the opt-in pseudonymous workspace, agent and
//!   requesting-member ids.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use ligus_core::{HostModelGateway, ModelProviderKind};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroizing;

/// The env var naming the gateways file.
pub const GATEWAYS_FILE_ENV: &str = "LIGUS_MODEL_GATEWAYS_FILE";
/// The gateways file in the data dir, read when [`GATEWAYS_FILE_ENV`] is unset.
pub const DEFAULT_GATEWAYS_FILE: &str = "model-gateways.json";
/// `0` turns exclusivity off: the host then also runs turns that don't use a
/// listed gateway. Set, it wins over the file's `exclusive`; any other value
/// keeps the host exclusive.
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
/// The member a turn runs for (the person nearest its trigger, agent access
/// controls), so a gateway's own budgets can count per member.
pub const HEADER_MEMBER: &str = "x-ligus-member";
/// The key a CLI is given when it insists on one: never a credential.
pub const DUMMY_KEY: &str = "ligus-gateway";
/// The egress proxy's path prefix for gateway routes.
pub const ROUTE_PREFIX: &str = "/ligus-gw/";

/// The wire a gateway speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
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
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        header: Option<String>,
    },
    /// A client certificate and key (PEM); the CA is `caFile`.
    #[serde(rename_all = "camelCase")]
    Mtls { cert_file: PathBuf, client_key_file: PathBuf },
    /// No credential (a mesh identity or the network position authenticates).
    None,
}

/// One gateway entry in the operator's file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayConfig {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub api: Api,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_plaintext: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub attribution: bool,
    pub auth: AuthConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_file: Option<PathBuf>,
}

/// The gateways file: the list, and whether the host is exclusive. A file
/// without `exclusive` is exclusive once it lists a gateway (the default
/// before `ligus gateway add` wrote the choice down).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GatewaysFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive: Option<bool>,
    #[serde(default)]
    pub gateways: Vec<GatewayConfig>,
}

/// Where a host's gateways file comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSource {
    /// `LIGUS_MODEL_GATEWAYS_FILE`.
    Env,
    /// `<data dir>/model-gateways.json`.
    DataDir,
}

impl FileSource {
    pub fn as_str(self) -> &'static str {
        match self {
            FileSource::Env => "env",
            FileSource::DataDir => "data-dir",
        }
    }
}

/// The gateways file this host reads: `LIGUS_MODEL_GATEWAYS_FILE` when set,
/// else `model-gateways.json` in `data_dir` (which may not exist yet). None
/// without either.
pub fn gateways_file(data_dir: Option<&Path>) -> Option<(PathBuf, FileSource)> {
    if let Some(p) = ligus_core::names::var(GATEWAYS_FILE_ENV).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        return Some((PathBuf::from(p), FileSource::Env));
    }
    data_dir.map(|d| (d.join(DEFAULT_GATEWAYS_FILE), FileSource::DataDir))
}

/// What decided whether a host is exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExclusiveSource {
    /// `LIGUS_MODEL_GATEWAYS_EXCLUSIVE`.
    Env,
    /// The file's `exclusive`.
    File,
    /// Neither: exclusive once a gateway is listed.
    Default,
}

impl ExclusiveSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ExclusiveSource::Env => "env",
            ExclusiveSource::File => "file",
            ExclusiveSource::Default => "default",
        }
    }
}

/// Whether a host listing `listed` gateways is exclusive, given the file's
/// `exclusive` and `env` (`LIGUS_MODEL_GATEWAYS_EXCLUSIVE`'s value). Only an
/// explicit `0`/`false` turns it off: anything unclear stays exclusive.
pub fn exclusive_of(listed: usize, file: Option<bool>, env: Option<&str>) -> (bool, ExclusiveSource) {
    let (on, src) = match env.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => (!(v == "0" || v.eq_ignore_ascii_case("false")), ExclusiveSource::Env),
        None => match file {
            Some(b) => (b, ExclusiveSource::File),
            None => (true, ExclusiveSource::Default),
        },
    };
    (on && listed > 0, src)
}

impl GatewaysFile {
    /// Parse (no validation of the entries: [`ModelGateways::parse`] does).
    pub fn from_json(text: &str) -> Result<Self, GatewayError> {
        serde_json::from_str(text).map_err(|e| GatewayError::Config(e.to_string()))
    }
    /// The file's text, as `ligus gateway add` writes it.
    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into());
        s.push('\n');
        s
    }
    /// Validate every entry with the rules the host loads it with.
    pub fn validate(&self) -> Result<Vec<Gateway>, GatewayError> {
        ModelGateways::validate(self.gateways.clone())
    }
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
    /// The requesting member's pseudonym (never a name or an email).
    pub member: Option<String>,
}

impl Attribution {
    /// Keyed hashes of `workspace` and `agent` under `key`.
    pub fn new(key: &[u8], workspace: Option<Uuid>, agent: Option<Uuid>) -> Self {
        Self { workspace: workspace.map(|w| pseudonym(key, b"w", w)), agent: agent.map(|a| pseudonym(key, b"a", a)), member: None }
    }

    /// With the requesting member's keyed hash (`member`: its member id),
    /// scoped to `workspace` ([`member_pseudonym`]); none without one.
    pub fn with_member(mut self, key: &[u8], workspace: Option<Uuid>, member: Option<&str>) -> Self {
        self.member = match (workspace, member.filter(|m| !m.is_empty())) {
            (Some(w), Some(m)) => Some(member_pseudonym(key, w, m)),
            _ => None,
        };
        self
    }
    pub fn headers(&self) -> Vec<(&'static str, String)> {
        let mut v = Vec::new();
        if let Some(w) = &self.workspace {
            v.push((HEADER_WORKSPACE, w.clone()));
        }
        if let Some(a) = &self.agent {
            v.push((HEADER_AGENT, a.clone()));
        }
        if let Some(m) = &self.member {
            v.push((HEADER_MEMBER, m.clone()));
        }
        v
    }
}

/// `hex(HMAC-SHA256(key, ATTRIBUTION_CONTEXT || kind || 0 || id)[..16])`.
pub fn pseudonym(key: &[u8], kind: &[u8], id: Uuid) -> String {
    pseudonym_bytes(key, kind, id.as_bytes())
}

/// The `x-ligus-member` pseudonym: [`pseudonym_bytes`] with kind `m` over
/// the workspace id's 16 bytes then the member id's UTF-8 bytes. A member
/// id is an account id, the same in every workspace, so the pseudonym is
/// scoped to the workspace: the same person's turns in two workspaces don't
/// share one at the gateway.
pub fn member_pseudonym(key: &[u8], workspace: Uuid, member: &str) -> String {
    let mut id = workspace.as_bytes().to_vec();
    id.extend_from_slice(member.as_bytes());
    pseudonym_bytes(key, b"m", &id)
}

/// [`pseudonym`] over arbitrary id bytes (a member id is a string): kind
/// `m`, the member id's UTF-8 bytes.
pub fn pseudonym_bytes(key: &[u8], kind: &[u8], id: &[u8]) -> String {
    use hmac::{KeyInit, Mac};
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(key).expect("any key length");
    mac.update(ATTRIBUTION_CONTEXT);
    mac.update(kind);
    mac.update(&[0]);
    mac.update(id);
    let out = mac.finalize().into_bytes();
    out[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// Which gateway a turn uses, with the ids its attribution hashes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GatewaySelection {
    pub name: String,
    pub workspace_id: Option<Uuid>,
    pub agent_id: Option<Uuid>,
    /// The member the turn runs for (set by the turn engine from the
    /// agent-access decision).
    pub member_id: Option<String>,
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
    /// The file became invalid while the host ran: every model turn is
    /// refused until it is fixed (as an invalid file stops a starting host).
    broken: Option<String>,
}

impl ModelGateways {
    /// Parse and validate a gateways file's contents.
    pub fn parse(text: &str) -> Result<Vec<Gateway>, GatewayError> {
        Self::validate(GatewaysFile::from_json(text)?.gateways)
    }

    fn validate(entries: Vec<GatewayConfig>) -> Result<Vec<Gateway>, GatewayError> {
        let mut out: Vec<Gateway> = Vec::new();
        for c in entries {
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
        Self { gateways: gateways.into_iter().map(Arc::new).collect(), exclusive, attribution_key: attribution_key.map(Zeroizing::new), broken: None }
    }

    /// A host whose gateways file broke while it ran: refuses every model
    /// turn (except a Ligus daemon on loopback, as an exclusive host does).
    pub fn refusing(why: impl Into<String>) -> Self {
        Self { exclusive: true, broken: Some(why.into()), ..Self::default() }
    }

    /// Load from the environment: the file [`gateways_file`] names
    /// (`LIGUS_MODEL_GATEWAYS_FILE`, else `model-gateways.json` in
    /// `data_dir`). No file: no gateways. A named file that can't be read,
    /// or any file that is invalid, is an error (the host must not run
    /// without the gateways its operator listed). `data_dir` also holds the
    /// generated attribution key.
    pub fn from_env(data_dir: Option<&Path>) -> Result<Self, GatewayError> {
        let env = ligus_core::names::var(EXCLUSIVE_ENV).ok();
        Self::load_from(gateways_file(data_dir), env.as_deref(), data_dir)
    }

    /// [`from_env`](Self::from_env) with the file and
    /// `LIGUS_MODEL_GATEWAYS_EXCLUSIVE` given.
    pub fn load_from(file: Option<(PathBuf, FileSource)>, exclusive_env: Option<&str>, data_dir: Option<&Path>) -> Result<Self, GatewayError> {
        let Some((path, source)) = file else {
            return Ok(Self::default());
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if source == FileSource::DataDir && e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(GatewayError::Config(format!("{}: {e}", path.display()))),
        };
        let file = GatewaysFile::from_json(&text).map_err(|e| GatewayError::Config(format!("{}: {e}", path.display())))?;
        let gateways = file.validate()?;
        let (exclusive, _) = exclusive_of(gateways.len(), file.exclusive, exclusive_env);
        let key = if gateways.iter().any(|g| g.attribution()) { Some(attribution_key(data_dir)?) } else { None };
        Ok(Self::new(gateways, exclusive, key))
    }

    /// Why every model turn is refused (the file broke while the host ran).
    pub fn broken(&self) -> Option<&str> {
        self.broken.as_deref()
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
        if let Some(p) = ligus_core::names::var(ATTRIBUTION_KEY_ENV).ok().filter(|s| !s.trim().is_empty()) {
            v.push(PathBuf::from(p.trim()));
        }
        v
    }

    fn attribution_for(&self, g: &Gateway, sel: &GatewaySelection) -> Option<Attribution> {
        let key = self.attribution_key.as_ref().filter(|_| g.attribution())?;
        Some(Attribution::new(key, sel.workspace_id, sel.agent_id).with_member(key, sel.workspace_id, sel.member_id.as_deref()))
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
        if let Some(why) = &self.broken {
            let local_daemon = provider == ModelProviderKind::LigusDaemon && endpoint_is_loopback(endpoint) && selected.is_none_or(|s| s.name.is_empty());
            if !local_daemon {
                return Err(format!(
                    "this host's model gateways file is invalid, so every model turn is refused until it is fixed ({why}); ligus gateway list shows the problem"
                ));
            }
        }
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
    if let Some(p) = ligus_core::names::var(ATTRIBUTION_KEY_ENV).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
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
    // The list itself is hidden from agent children too (they can't write
    // outside their turn's paths anyway): a running host reads it again when
    // it changes ([`watch_host`]), so it steers the host's model routes.
    if let Some((path, _)) = gateways_file(Some(data_dir)) {
        crate::executor::configure_secret_files(&[path]);
    }
    let g = Arc::new(ModelGateways::from_env(Some(data_dir))?);
    install(g.clone());
    Ok(g)
}

/// How often [`watch_host`] looks at the gateways file.
pub const WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// What the watcher last saw of the gateways file: absent, its bytes, or
/// why it couldn't be read.
pub type FileSnapshot = Option<Result<Vec<u8>, String>>;

/// The gateways file as it is now.
pub fn snapshot(path: Option<&Path>) -> FileSnapshot {
    match std::fs::read(path?) {
        Ok(b) => Some(Ok(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(Err(e.kind().to_string())),
    }
}

/// Whether the file at `path` changed since `last` (and remember it).
pub fn changed(path: Option<&Path>, last: &mut FileSnapshot) -> bool {
    let now = snapshot(path);
    if now == *last {
        return false;
    }
    *last = now;
    true
}

/// Load this host's gateways again and install them. A file that became
/// invalid installs [`ModelGateways::refusing`]: every model turn is refused
/// until it is fixed, never run with a list the operator didn't write.
pub fn reload_host(data_dir: &Path) -> Result<Arc<ModelGateways>, GatewayError> {
    match ModelGateways::from_env(Some(data_dir)) {
        Ok(g) => {
            let g = Arc::new(g);
            install(g.clone());
            Ok(g)
        }
        Err(e) => {
            install(Arc::new(ModelGateways::refusing(e.to_string())));
            Err(e)
        }
    }
}

/// The gateways file of `data_dir` as it is now: take it before
/// [`init_host`] and hand it to [`watch_host`], so a change between the two
/// is still seen.
pub fn file_snapshot(data_dir: &Path) -> FileSnapshot {
    snapshot(gateways_file(Some(data_dir)).map(|(p, _)| p).as_deref())
}

/// What a running host does when its gateways file changed from `before`
/// to `now`: a file that existed and is gone refuses every model turn (a
/// host that started without one runs with none; one that loses it while
/// running must not quietly stop being exclusive). None: load it again.
pub fn vanished(before: &FileSnapshot, now: &FileSnapshot) -> Option<String> {
    (before.is_some() && now.is_none()).then(|| "the gateways file was removed while the host ran; recreate it (ligus gateway add) or restart the host to run without gateways".to_string())
}

/// serve and workers: reload the gateways whenever the file changes
/// (checked every [`WATCH_INTERVAL`]), so `ligus gateway add` and `remove`
/// apply without a restart. `last` is the file as [`init_host`] read it
/// ([`file_snapshot`] before it). What the host advertises on its host
/// record (the gateway names the desktop lists) still changes at the next
/// start, and a turn already running keeps the route it started with.
pub fn watch_host(data_dir: PathBuf, mut last: FileSnapshot) {
    let path = gateways_file(Some(&data_dir)).map(|(p, _)| p);
    let _ = std::thread::Builder::new().name("ligus-gateways-watch".into()).spawn(move || loop {
        std::thread::sleep(WATCH_INTERVAL);
        let before = last.clone();
        if !changed(path.as_deref(), &mut last) {
            continue;
        }
        if let Some(why) = vanished(&before, &last) {
            tracing::error!(target: "dispatch", "model gateways: {why}; every model turn on this host is refused");
            install(Arc::new(ModelGateways::refusing(why)));
            continue;
        }
        match reload_host(&data_dir) {
            Ok(g) => tracing::info!(
                target: "dispatch",
                "model gateways file changed: {} gateway(s), {}",
                g.all().len(),
                if g.exclusive() { "exclusive" } else { "not exclusive" }
            ),
            Err(e) => tracing::error!(target: "dispatch", "{e}; every model turn on this host is refused until the file is fixed"),
        }
    });
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

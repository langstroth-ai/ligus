//! Verifying linked identities on this device (IDENTITY-AND-FORGES §2.3-2.5).
//!
//! A link is a claim on a workspace config log ("this Ligus account is also
//! `<subject>` on `<forge>`"); the fold only checks its shape, its author and
//! that its statement names the account, the account key and the workspace
//! (`hive_core::authz::links`). This module is what a device does on top, and
//! keeps for itself: never folded, never synced.
//!
//! 1. **Offline:** the link hasn't expired, its statement still names the
//!    account's current key, and the SSHSIG over the statement verifies under
//!    the namespace [`LINK_NAMESPACE`] (RustCrypto `ssh-key`; Ed25519,
//!    `sk-ssh-ed25519@openssh.com`, ECDSA P-256, `sk-ecdsa-sha2-nistp256@openssh.com`
//!    and `rsa-sha2-512` of at least 3072 bits, hash `sha512`).
//! 2. **Online, directly against the forge** (never through the relay, never
//!    with a token): the forge's public key list for the *stable* subject id
//!    must still contain the signing key (or, for `KeyPublished`, the Ligus
//!    account key on the signing list). Where the listing is by handle, the
//!    id is resolved to the handle before and after the fetch, and both must
//!    agree (a rename between the two calls is not a verification).
//! 3. **Device-local status** with a 24-hour cache ([`CHECK_TTL`]).
//!
//! Requests go only to the forge hosts this build knows or the user added on
//! this device ([`ForgeConfig`]), with no redirects, short timeouts and a
//! size cap. The one dynamic host, an atproto PDS (or a `did:web` host), must
//! be HTTPS and resolve to public addresses only.
//!
//! A verified link proves "this forge says so", nothing more: it never grants
//! membership, a role or trust for sealing keys.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use hive_core::identity_link::{render_statement, IdentityLink, LinkMethod};
use hive_core::Timestamp;
use serde::{Deserialize, Serialize};
use ssh_key::{Algorithm, EcdsaCurve, HashAlg, PublicKey, SshSig};
use uuid::Uuid;

pub use hive_core::identity_link::{LINK_NAMESPACE, STATEMENT_TAG};
pub use ssh_key::PublicKey as SshPublicKey;

/// How long a forge check is reused before the forge is asked again (owner
/// decision D13). Bounds how long a removed forge key keeps a link verified.
pub const CHECK_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// The largest forge response read.
pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;

/// Per-request timeout.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The smallest RSA modulus accepted.
pub const MIN_RSA_BITS: usize = 3072;

// ---------------------------------------------------------------------------
// SSHSIG

/// A signature that verified: the key that made it.
#[derive(Debug, Clone)]
pub struct SigOk {
    pub public_key: PublicKey,
    /// `SHA256:...`, as `ssh-keygen -l` prints it.
    pub fingerprint: String,
    pub algorithm: String,
}

/// Verify an armored SSHSIG (`-----BEGIN SSH SIGNATURE-----`) over `message`
/// under exactly `namespace`. Only the algorithms of §2.5 are accepted, the
/// hash must be `sha512`, an RSA key must be at least [`MIN_RSA_BITS`] and
/// sign with `rsa-sha2-512`, and a security key's signature must carry the
/// user-presence flag.
pub fn verify_sshsig(armored: &str, namespace: &str, message: &[u8]) -> Result<SigOk, String> {
    if armored.len() > 16 * 1024 {
        return Err("signature too large".into());
    }
    let sig = SshSig::from_pem(armored.trim().as_bytes()).map_err(|e| format!("not an SSH signature: {e}"))?;
    if namespace.is_empty() || sig.namespace() != namespace {
        return Err(format!("signed for namespace {:?}, not {namespace:?}", sig.namespace()));
    }
    if sig.hash_alg() != HashAlg::Sha512 {
        return Err("the signature must use sha512".into());
    }
    let key = PublicKey::from(sig.public_key().clone());
    let algorithm = key.algorithm();
    match (&algorithm, sig.public_key()) {
        (Algorithm::Ed25519, _) | (Algorithm::SkEd25519, _) | (Algorithm::SkEcdsaSha2NistP256, _) => {}
        (Algorithm::Ecdsa { curve: EcdsaCurve::NistP256 }, _) => {}
        (Algorithm::Rsa { .. }, ssh_key::public::KeyData::Rsa(rsa)) => {
            let n = rsa.n.as_positive_bytes().unwrap_or_default();
            let lead = n.first().map(|b| 8 - b.leading_zeros() as usize).unwrap_or(0);
            let bits = n.len().saturating_sub(1) * 8 + lead;
            if bits < MIN_RSA_BITS {
                return Err(format!("RSA keys need at least {MIN_RSA_BITS} bits (this one has {bits})"));
            }
            if sig.signature().algorithm() != (Algorithm::Rsa { hash: Some(HashAlg::Sha512) }) {
                return Err("RSA signatures must be rsa-sha2-512".into());
            }
        }
        _ => return Err(format!("{} keys aren't accepted for identity links", algorithm.as_str())),
    }
    if matches!(algorithm, Algorithm::SkEd25519 | Algorithm::SkEcdsaSha2NistP256) {
        // PROTOCOL.u2f: the signature ends with flags (1 byte) and a counter
        // (4 bytes); bit 0 is user presence.
        let b = sig.signature().as_bytes();
        let flags = b.len().checked_sub(5).and_then(|i| b.get(i)).copied().unwrap_or(0);
        if flags & 0x01 == 0 {
            return Err("the security key signed without user presence".into());
        }
    }
    key.verify(namespace, message, &sig).map_err(|e| format!("signature doesn't verify: {e}"))?;
    Ok(SigOk {
        fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
        algorithm: algorithm.as_str().to_string(),
        public_key: key,
    })
}

/// Parse one OpenSSH public key line (`<algo> <base64> [comment]`).
pub fn parse_openssh(line: &str) -> Option<PublicKey> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') || line.len() > 16 * 1024 {
        return None;
    }
    PublicKey::from_openssh(line).ok()
}

/// The `ssh-ed25519 ...` line for a Ligus account key (the `KeyPublished`
/// fallback: what the user pastes into the forge's signing keys).
pub fn account_key_openssh(account_key: &[u8]) -> Option<String> {
    let k: [u8; 32] = account_key.try_into().ok()?;
    PublicKey::from(ssh_key::public::KeyData::Ed25519(ssh_key::public::Ed25519PublicKey(k)))
        .to_openssh()
        .ok()
}

/// The `ssh-keygen` command that signs `statement_path` for a link.
pub fn sign_command(key_path: &str, statement_path: &str) -> String {
    format!("ssh-keygen -Y sign -n {LINK_NAMESPACE} -f {key_path} {statement_path}")
}

// ---------------------------------------------------------------------------
// Status

/// A link's device-local status (§2.5 step 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum LinkStatus {
    /// The forge lists the key now (or did at `checked_at`, within 24 h).
    /// `name_bound`: the forge has no public stable id (SourceHut), so the
    /// link is bound to a name that could be reused.
    #[serde(rename_all = "camelCase")]
    Verified { checked_at: Timestamp, handle: Option<String>, name_bound: bool },
    /// The signature is valid; forge checks are turned off on this device.
    SignatureOnly,
    /// The forge no longer lists the key (removed, rotated, account gone), or
    /// the account key the statement names was rotated.
    #[serde(rename_all = "camelCase")]
    Stale { checked_at: Timestamp, reason: String },
    /// The forge couldn't be asked (or answered inconsistently).
    #[serde(rename_all = "camelCase")]
    Unreachable { last_verified_at: Option<Timestamp>, reason: String },
    Expired,
    /// The offline checks failed: never shown as linked.
    Invalid { reason: String },
    /// No public key list exists for this provider: a claim only.
    SelfAsserted,
    /// This device can't check it (a host not configured here, a method this
    /// build doesn't verify).
    NotChecked { reason: String },
}

impl LinkStatus {
    pub fn is_verified(&self) -> bool {
        matches!(self, LinkStatus::Verified { .. })
    }
    /// A result worth caching for [`CHECK_TTL`] (the forge answered).
    fn definitive(&self) -> bool {
        matches!(self, LinkStatus::Verified { .. } | LinkStatus::Stale { .. })
    }
}

/// What a device knows about the link's account at the head of the
/// workspace's config log.
#[derive(Debug, Clone, Copy)]
pub struct LinkContext<'a> {
    pub workspace_id: Uuid,
    /// The account's current (undisputed) signing key here.
    pub account_key: Option<&'a [u8]>,
}

/// The offline half: expiry, the account key, the statement and the SSHSIG.
/// `Ok(Some(sig))` for a valid SSH-signed link, `Ok(None)` for a
/// `KeyPublished` link, `Err(status)` when nothing more is to be checked.
pub fn offline_check(link: &IdentityLink, ctx: LinkContext<'_>, now: Timestamp) -> Result<Option<SigOk>, LinkStatus> {
    if let Err(why) = hive_core::identity_link::check_shape(link) {
        return Err(LinkStatus::Invalid { reason: why });
    }
    if link.expires_at <= now {
        return Err(LinkStatus::Expired);
    }
    match link.method {
        LinkMethod::SshSig => {}
        LinkMethod::KeyPublished => {
            return match ctx.account_key {
                Some(_) => Ok(None),
                None => Err(LinkStatus::Invalid { reason: "the account's key isn't known here".into() }),
            };
        }
        LinkMethod::SelfAsserted => return Err(LinkStatus::SelfAsserted),
        _ => return Err(LinkStatus::NotChecked { reason: "this version of Ligus can't check this kind of link".into() }),
    }
    let Some(key) = ctx.account_key else {
        return Err(LinkStatus::Invalid { reason: "the account's key isn't known here".into() });
    };
    if link.statement != render_statement(link, ctx.workspace_id, key) {
        // The fold checked it at its position; a mismatch now means the
        // account key was rotated since (§2.6): re-sign.
        let at_other_key = link.statement.contains(&format!("\nworkspace: {}\n", ctx.workspace_id.hyphenated()));
        return Err(if at_other_key {
            LinkStatus::Stale { checked_at: now, reason: "the account key changed since this link was signed".into() }
        } else {
            LinkStatus::Invalid { reason: "the statement doesn't match this workspace".into() }
        });
    }
    let sig = verify_sshsig(&link.proof, LINK_NAMESPACE, link.statement.as_bytes())
        .map_err(|reason| LinkStatus::Invalid { reason })?;
    if sig.fingerprint != link.key_fingerprint {
        return Err(LinkStatus::Invalid { reason: "the signing key isn't the one the link names".into() });
    }
    Ok(Some(sig))
}

// ---------------------------------------------------------------------------
// Forges

/// A forge instance the user added on this device (self-managed GitLab,
/// Forgejo, Gitea, GitHub Enterprise). Device-local; never synced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelfHostedForge {
    pub provider: String,
    /// The host a link names (lowercase, optional port).
    pub host: String,
    /// Its HTTPS base URL (`https://gitlab.example.com`).
    pub base_url: String,
}

/// Which forges this device asks, and whether it asks at all.
#[derive(Debug, Clone, Default)]
pub struct ForgeConfig {
    /// "Contact forges to check linked identities" (on by default, D13).
    pub check_forges: bool,
    pub self_hosted: Vec<SelfHostedForge>,
    /// Ask hosts the link's owner chose (a PDS other than the big hosted
    /// ones, a `did:web` host, a handle's domain). Set only for a check the
    /// person asked for ("Check now", linking their own account): an
    /// automatic check must not tell a server the link's owner runs who
    /// looked, and from where.
    pub ask_owner_chosen_hosts: bool,
    /// Tests only: API roots by `(provider, host)` (fake servers on
    /// loopback), the PLC directory root, and plain HTTP to loopback for a
    /// PDS. Never set from settings.
    #[doc(hidden)]
    pub test_api: BTreeMap<(String, String), String>,
    #[doc(hidden)]
    pub test_plc: Option<String>,
    #[doc(hidden)]
    pub test_allow_loopback: bool,
    /// Tests only: the server standing in for `https://{host}` (a handle's
    /// domain, a `did:web` host).
    #[doc(hidden)]
    pub test_https: BTreeMap<String, String>,
}

/// The built-in forges (anonymous key listings documented in §2.3).
fn builtin_api(provider: &str, host: &str) -> Option<&'static str> {
    Some(match (provider, host) {
        ("github", "github.com") => "https://api.github.com",
        ("gitlab", "gitlab.com") => "https://gitlab.com/api/v4",
        ("forgejo" | "gitea", "codeberg.org") => "https://codeberg.org",
        ("gitea" | "forgejo", "gitea.com") => "https://gitea.com",
        ("sourcehut", "meta.sr.ht") => "https://meta.sr.ht",
        _ => return None,
    })
}

/// The hosted PDSes an automatic check may ask (Bluesky's): a host the
/// link's owner can't run.
pub fn well_known_pds(host: &str) -> bool {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    h == "bsky.social" || (h.ends_with(".bsky.network") && h.len() > ".bsky.network".len())
}

/// The PLC directory (did:plc resolution).
const PLC_DIRECTORY: &str = "https://plc.directory";

impl ForgeConfig {
    /// The API root for a forge, if this device may ask it.
    pub fn api_root(&self, provider: &str, host: &str) -> Option<String> {
        if let Some(t) = self.test_api.get(&(provider.to_string(), host.to_string())) {
            return Some(t.trim_end_matches('/').to_string());
        }
        if let Some(b) = builtin_api(provider, host) {
            return Some(b.to_string());
        }
        let s = self.self_hosted.iter().find(|s| s.provider == provider && s.host == host)?;
        let base = url::Url::parse(&s.base_url).ok()?;
        if base.scheme() != "https" || base.host_str().is_none() {
            return None;
        }
        let base = s.base_url.trim_end_matches('/');
        Some(match provider {
            "github" => format!("{base}/api/v3"),
            "gitlab" => format!("{base}/api/v4"),
            "forgejo" | "gitea" | "sourcehut" => base.to_string(),
            _ => return None,
        })
    }

    fn plc(&self) -> String {
        self.test_plc.clone().unwrap_or_else(|| PLC_DIRECTORY.to_string())
    }
}

/// One key a forge lists for the subject.
#[derive(Debug, Clone)]
pub struct ForgeKey {
    pub key: PublicKey,
    /// On the forge's *signing* list (where the account key grants no push).
    pub signing: bool,
}

/// What a forge says about a subject now.
#[derive(Debug, Clone)]
pub struct KeyList {
    /// The current handle, when the forge says (display only).
    pub handle: Option<String>,
    pub keys: Vec<ForgeKey>,
    /// Bound to a name, not a stable id (SourceHut).
    pub name_bound: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// This device doesn't ask that host (not built in, not added here).
    NotConfigured(String),
    /// The subject doesn't exist (any more).
    Gone,
    /// The forge answered inconsistently (a rename during the check).
    Inconsistent(String),
    Transport(String),
}

/// Resolves only to public addresses: the guard for hosts a link (or a DID
/// document) names rather than the user (an atproto PDS, a `did:web` host).
struct PublicOnlyResolver;

/// Whether an address is one a forge lookup may reach: not loopback,
/// private, link-local, CGNAT, unique-local, multicast or unspecified.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                || o[0] == 0
                || (o[0] == 100 && (64..128).contains(&o[1]))
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || o[0] >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(v4));
            }
            let s = v6.segments();
            let v4_at = |hi: u16, lo: u16| std::net::Ipv4Addr::from(((hi as u32) << 16) | lo as u32);
            // NAT64 (well-known prefix), the IPv4-compatible form and 6to4
            // reach the IPv4 address they carry.
            if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
                return is_public_ip(IpAddr::V4(v4_at(s[6], s[7])));
            }
            if s[0..6] == [0, 0, 0, 0, 0, 0] && !v6.is_loopback() && !v6.is_unspecified() {
                return is_public_ip(IpAddr::V4(v4_at(s[6], s[7])));
            }
            if s[0] == 0x2002 {
                return is_public_ip(IpAddr::V4(v4_at(s[1], s[2])));
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] == 0x2001 && s[1] == 0x0db8)
                // Teredo, local-use NAT64, site-local, discard-only.
                || (s[0] == 0x2001 && s[1] == 0)
                || (s[0] == 0x64 && s[1] == 0xff9b)
                || (s[0] & 0xffc0) == 0xfec0
                || (s[0] == 0x100 && s[1..4] == [0, 0, 0]))
        }
    }
}

impl reqwest::dns::Resolve for PublicOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| is_public_ip(a.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(format!("{host} doesn't resolve to a public address").into());
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Fetches public key listings. Never sends a token or cookie, never follows
/// a redirect.
#[derive(Clone)]
pub struct LinkFetcher {
    cfg: ForgeConfig,
    configured: reqwest::Client,
    dynamic: reqwest::Client,
}

fn client(public_only: bool) -> reqwest::Client {
    let mut b = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(Duration::from_secs(5))
        .user_agent(concat!("Ligus/", env!("CARGO_PKG_VERSION"), " identity-links"))
        .no_proxy();
    if public_only {
        b = b.dns_resolver(Arc::new(PublicOnlyResolver));
    }
    b.build().unwrap_or_default()
}

/// A path segment a forge handle or id may be (no traversal, no query).
fn safe_segment(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s != "."
        && s != ".."
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn numeric(s: &str) -> bool {
    !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit())
}

/// A `did:plc:` or `did:web:` id this module resolves.
pub fn valid_did(s: &str) -> bool {
    if let Some(rest) = s.strip_prefix("did:plc:") {
        return rest.len() == 24 && rest.bytes().all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b));
    }
    if let Some(host) = s.strip_prefix("did:web:") {
        return !host.is_empty()
            && host.len() <= 253
            && host.contains('.')
            && host.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-'));
    }
    false
}

impl LinkFetcher {
    pub fn new(cfg: ForgeConfig) -> Self {
        let dynamic = client(!cfg.test_allow_loopback);
        Self { cfg, configured: client(false), dynamic }
    }

    pub fn config(&self) -> &ForgeConfig {
        &self.cfg
    }

    async fn get(&self, url: &str, dynamic: bool) -> Result<(u16, Vec<u8>), FetchError> {
        let parsed = url::Url::parse(url).map_err(|e| FetchError::Transport(e.to_string()))?;
        if dynamic {
            let loopback_ok = self.cfg.test_allow_loopback
                && parsed.host_str().is_some_and(|h| h == "127.0.0.1" || h == "localhost");
            if parsed.scheme() != "https" && !loopback_ok {
                return Err(FetchError::NotConfigured("only HTTPS for a host named by a link".into()));
            }
            if !parsed.username().is_empty() || parsed.password().is_some() {
                return Err(FetchError::NotConfigured("a URL with credentials".into()));
            }
            let chosen_by_owner = !parsed.host_str().is_some_and(well_known_pds);
            if chosen_by_owner && !self.cfg.ask_owner_chosen_hosts {
                return Err(FetchError::NotConfigured(format!(
                    "checking this link asks {}, a server its owner chose; check it by hand to ask it",
                    parsed.host_str().unwrap_or("a server")
                )));
            }
            if let Some(url::Host::Ipv4(ip)) = parsed.host() {
                if !is_public_ip(IpAddr::V4(ip)) && !loopback_ok {
                    return Err(FetchError::NotConfigured("a private address".into()));
                }
            }
            if let Some(url::Host::Ipv6(ip)) = parsed.host() {
                if !is_public_ip(IpAddr::V6(ip)) {
                    return Err(FetchError::NotConfigured("a private address".into()));
                }
            }
        }
        let c = if dynamic { &self.dynamic } else { &self.configured };
        let resp = c
            .get(parsed)
            .header(reqwest::header::ACCEPT, "application/json, text/plain")
            .send()
            .await
            .map_err(|e| FetchError::Transport(e.without_url().to_string()))?;
        let status = resp.status().as_u16();
        if (300..400).contains(&status) {
            return Err(FetchError::Transport(format!("the forge redirected (HTTP {status}); redirects aren't followed")));
        }
        if resp.content_length().is_some_and(|n| n as usize > MAX_RESPONSE_BYTES) {
            return Err(FetchError::Transport("the forge's answer is too large".into()));
        }
        let mut body = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| FetchError::Transport(e.without_url().to_string()))?;
            if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(FetchError::Transport("the forge's answer is too large".into()));
            }
            body.extend_from_slice(&chunk);
        }
        Ok((status, body))
    }

    async fn get_ok(&self, url: &str, dynamic: bool) -> Result<Vec<u8>, FetchError> {
        match self.get(url, dynamic).await? {
            (200, b) => Ok(b),
            (404 | 410, _) => Err(FetchError::Gone),
            (s, _) => Err(FetchError::Transport(format!("HTTP {s}"))),
        }
    }

    async fn get_json(&self, url: &str, dynamic: bool) -> Result<serde_json::Value, FetchError> {
        let b = self.get_ok(url, dynamic).await?;
        serde_json::from_slice(&b).map_err(|e| FetchError::Transport(format!("malformed forge answer: {e}")))
    }

    fn root(&self, provider: &str, host: &str) -> Result<String, FetchError> {
        self.cfg
            .api_root(provider, host)
            .ok_or_else(|| FetchError::NotConfigured(format!("{host} isn't a forge this device checks (add it under Linked accounts)")))
    }

    /// Linking time: the stable id and canonical handle for a handle the user
    /// typed (or a DID / `~user` given directly).
    pub async fn resolve_handle(&self, provider: &str, host: &str, handle: &str) -> Result<(String, String), FetchError> {
        let handle = handle.trim().trim_start_matches('@');
        match provider {
            "github" => {
                if !safe_segment(handle) {
                    return Err(FetchError::Gone);
                }
                let root = self.root(provider, host)?;
                let v = self.get_json(&format!("{root}/users/{handle}"), false).await?;
                let id = v["id"].as_u64().ok_or(FetchError::Gone)?;
                Ok((id.to_string(), v["login"].as_str().and_then(clean_handle).unwrap_or_else(|| handle.to_string())))
            }
            "gitlab" => {
                if !safe_segment(handle) {
                    return Err(FetchError::Gone);
                }
                let root = self.root(provider, host)?;
                let v = self.get_json(&format!("{root}/users?username={handle}"), false).await?;
                let u = v.as_array().and_then(|a| a.first()).ok_or(FetchError::Gone)?;
                let id = u["id"].as_u64().ok_or(FetchError::Gone)?;
                Ok((id.to_string(), u["username"].as_str().and_then(clean_handle).unwrap_or_else(|| handle.to_string())))
            }
            "forgejo" | "gitea" => {
                if !safe_segment(handle) {
                    return Err(FetchError::Gone);
                }
                let root = self.root(provider, host)?;
                let v = self.get_json(&format!("{root}/api/v1/users/search?q={handle}&limit=50"), false).await?;
                let u = v["data"]
                    .as_array()
                    .and_then(|a| a.iter().find(|u| u["login"].as_str().is_some_and(|l| l.eq_ignore_ascii_case(handle))))
                    .ok_or(FetchError::Gone)?;
                let id = u["id"].as_u64().ok_or(FetchError::Gone)?;
                Ok((id.to_string(), u["login"].as_str().and_then(clean_handle).unwrap_or_else(|| handle.to_string())))
            }
            "sourcehut" => {
                let user = format!("~{}", handle.trim_start_matches('~'));
                if !safe_segment(&user[1..]) {
                    return Err(FetchError::Gone);
                }
                self.root(provider, host)?;
                Ok((user.clone(), user))
            }
            "tangled" => {
                if valid_did(handle) {
                    let doc = self.did_document(handle).await?;
                    let shown = self.verified_handle(handle, &doc).await.unwrap_or_else(|| handle.to_string());
                    return Ok((handle.to_string(), shown));
                }
                // A handle: its domain's well-known DID (a host the user
                // named, so public addresses only).
                let h = handle.to_ascii_lowercase();
                if !h.contains('.') || !h.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-')) {
                    return Err(FetchError::Gone);
                }
                let b = self.get_ok(&self.https_url(&h, "/.well-known/atproto-did"), true).await?;
                let did = String::from_utf8_lossy(&b).trim().to_string();
                if !valid_did(&did) {
                    return Err(FetchError::Gone);
                }
                let doc = self.did_document(&did).await?;
                if handle_of(&doc).as_deref() != Some(h.as_str()) {
                    return Err(FetchError::Inconsistent("the DID document doesn't claim that handle".into()));
                }
                Ok((did, h))
            }
            _ => Err(FetchError::NotConfigured(format!("{provider} links can't be checked against a forge"))),
        }
    }

    /// `https://{host}{path}` (a host a link or DID document names).
    fn https_url(&self, host: &str, path: &str) -> String {
        match self.cfg.test_https.get(host) {
            Some(base) => format!("{}{path}", base.trim_end_matches('/')),
            None => format!("https://{host}{path}"),
        }
    }

    /// The handle a DID document claims (`alsoKnownAs`), only when the
    /// handle's domain names the DID back (atproto's bidirectional check):
    /// anyone can write any handle into their own DID document.
    async fn verified_handle(&self, did: &str, doc: &serde_json::Value) -> Option<String> {
        let h = handle_of(doc)?.to_ascii_lowercase();
        if !h.contains('.') || !h.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-')) {
            return None;
        }
        let b = self.get_ok(&self.https_url(&h, "/.well-known/atproto-did"), true).await.ok()?;
        (String::from_utf8_lossy(&b).trim() == did).then_some(h)
    }

    async fn did_document(&self, did: &str) -> Result<serde_json::Value, FetchError> {
        if !valid_did(did) {
            return Err(FetchError::Gone);
        }
        let doc = if let Some(host) = did.strip_prefix("did:web:") {
            self.get_json(&self.https_url(host, "/.well-known/did.json"), true).await?
        } else {
            self.get_json(&format!("{}/{did}", self.cfg.plc()), false).await?
        };
        if doc["id"].as_str() != Some(did) {
            return Err(FetchError::Inconsistent("the DID document names another DID".into()));
        }
        Ok(doc)
    }

    /// GitHub: id → login (`GET /user/{id}`).
    async fn github_login(&self, root: &str, id: &str) -> Result<String, FetchError> {
        let v = self.get_json(&format!("{root}/user/{id}"), false).await?;
        if v["id"].as_u64().map(|n| n.to_string()).as_deref() != Some(id) {
            return Err(FetchError::Inconsistent("the forge answered for another id".into()));
        }
        v["login"].as_str().filter(|l| safe_segment(l)).map(str::to_string).ok_or(FetchError::Gone)
    }

    /// Forgejo/Gitea: id → login (`GET /api/v1/users/search?uid=`).
    async fn gitea_login(&self, root: &str, id: &str) -> Result<String, FetchError> {
        let v = self.get_json(&format!("{root}/api/v1/users/search?uid={id}"), false).await?;
        let u = v["data"]
            .as_array()
            .and_then(|a| a.iter().find(|u| u["id"].as_u64().map(|n| n.to_string()).as_deref() == Some(id)))
            .ok_or(FetchError::Gone)?;
        u["login"].as_str().filter(|l| safe_segment(l)).map(str::to_string).ok_or(FetchError::Gone)
    }

    /// The keys a forge lists for the stable subject id now (§2.5 step 3).
    pub async fn key_list(&self, provider: &str, host: &str, subject_id: &str) -> Result<KeyList, FetchError> {
        match provider {
            "github" => {
                if !numeric(subject_id) {
                    return Err(FetchError::Gone);
                }
                let root = self.root(provider, host)?;
                let login = self.github_login(&root, subject_id).await?;
                let auth = self.get_json(&format!("{root}/users/{login}/keys"), false).await?;
                let signing = self.get_json(&format!("{root}/users/{login}/ssh_signing_keys"), false).await?;
                // A rename (and re-registration of the old name) between the
                // calls would hand us someone else's keys.
                if self.github_login(&root, subject_id).await? != login {
                    return Err(FetchError::Inconsistent("the account was renamed during the check".into()));
                }
                let mut keys = json_keys(&auth, "key", |_| false);
                keys.extend(json_keys(&signing, "key", |_| true));
                Ok(KeyList { handle: Some(login), keys, name_bound: false })
            }
            "gitlab" => {
                if !numeric(subject_id) {
                    return Err(FetchError::Gone);
                }
                let root = self.root(provider, host)?;
                // By id: no rename window.
                let v = self.get_json(&format!("{root}/users/{subject_id}/keys"), false).await?;
                // `signing` marks a key that grants no push access (D15):
                // "auth_and_signing", GitLab's default, grants push.
                let keys = json_keys(&v, "key", |k| k["usage_type"].as_str() == Some("signing"));
                // The handle is display only; gitlab.com doesn't answer
                // `/users/{id}` anonymously.
                let handle = match self.get_json(&format!("{root}/users/{subject_id}"), false).await {
                    Ok(u) => u["username"].as_str().and_then(clean_handle),
                    Err(_) => None,
                };
                Ok(KeyList { handle, keys, name_bound: false })
            }
            "forgejo" | "gitea" => {
                if !numeric(subject_id) {
                    return Err(FetchError::Gone);
                }
                let root = self.root(provider, host)?;
                let login = self.gitea_login(&root, subject_id).await?;
                // The JSON key API needs a token on Forgejo and Gitea; the
                // `.keys` listing is anonymous.
                let text = self.get_ok(&format!("{root}/{login}.keys"), false).await?;
                if self.gitea_login(&root, subject_id).await? != login {
                    return Err(FetchError::Inconsistent("the account was renamed during the check".into()));
                }
                Ok(KeyList { handle: Some(login), keys: text_keys(&text), name_bound: false })
            }
            "sourcehut" => {
                let name = subject_id.strip_prefix('~').filter(|n| safe_segment(n)).ok_or(FetchError::Gone)?;
                let root = self.root(provider, host)?;
                let text = self.get_ok(&format!("{root}/~{name}.keys"), false).await?;
                Ok(KeyList { handle: Some(subject_id.to_string()), keys: text_keys(&text), name_bound: true })
            }
            "tangled" => {
                if host != "atproto" {
                    return Err(FetchError::NotConfigured("a Tangled link names host \"atproto\"".into()));
                }
                let doc = self.did_document(subject_id).await?;
                let pds = pds_of(&doc).ok_or(FetchError::Inconsistent("the DID document names no PDS".into()))?;
                let mut keys = Vec::new();
                // The current lexicon (org.tangled.key.sshKey, `publicKey`),
                // then the legacy one (sh.tangled.publicKey, `key`), best
                // effort: a PDS that doesn't have it answers 400.
                for (collection, field) in [("org.tangled.key.sshKey", "publicKey"), ("sh.tangled.publicKey", "key")] {
                    let url = format!(
                        "{pds}/xrpc/com.atproto.repo.listRecords?repo={subject_id}&collection={collection}&limit=100"
                    );
                    match self.get_json(&url, true).await {
                        Ok(v) => {
                            if let Some(recs) = v["records"].as_array() {
                                for r in recs {
                                    if let Some(k) = r["value"][field].as_str().and_then(parse_openssh) {
                                        keys.push(ForgeKey { key: k, signing: true });
                                    }
                                }
                            }
                        }
                        Err(e) if collection == "org.tangled.key.sshKey" => return Err(e),
                        Err(_) => {}
                    }
                }
                Ok(KeyList { handle: self.verified_handle(subject_id, &doc).await, keys, name_bound: false })
            }
            _ => Err(FetchError::NotConfigured(format!("{provider} has no public key list"))),
        }
    }
}

fn json_keys(v: &serde_json::Value, field: &str, signing: impl Fn(&serde_json::Value) -> bool) -> Vec<ForgeKey> {
    v.as_array()
        .map(|a| {
            a.iter()
                .take(500)
                .filter_map(|k| Some(ForgeKey { key: parse_openssh(k[field].as_str()?)?, signing: signing(k) }))
                .collect()
        })
        .unwrap_or_default()
}

fn text_keys(body: &[u8]) -> Vec<ForgeKey> {
    String::from_utf8_lossy(body)
        .lines()
        .take(500)
        .filter_map(parse_openssh)
        .map(|key| ForgeKey { key, signing: false })
        .collect()
}

fn handle_of(doc: &serde_json::Value) -> Option<String> {
    doc["alsoKnownAs"].as_array()?.iter().filter_map(|a| a.as_str()?.strip_prefix("at://")).find_map(clean_handle)
}

/// A name a forge or DID document supplied, as it may be shown: without
/// control, bidi or zero-width characters, trimmed, at most 256 bytes.
fn clean_handle(s: &str) -> Option<String> {
    let c = hive_core::identity_link::strip_bidi_controls(s);
    let c = c.trim();
    (!c.is_empty() && c.len() <= 256 && !c.chars().any(char::is_whitespace)).then(|| c.to_string())
}

fn pds_of(doc: &serde_json::Value) -> Option<String> {
    doc["service"].as_array()?.iter().find_map(|s| {
        let id = s["id"].as_str()?;
        (id == "#atproto_pds" || id.ends_with("#atproto_pds"))
            .then(|| s["serviceEndpoint"].as_str().map(|e| e.trim_end_matches('/').to_string()))
            .flatten()
    })
}

/// Check one link (offline, then against its forge unless forge checks are
/// off). `last_verified_at` comes from the device-local cache.
pub async fn check_link(
    link: &IdentityLink,
    ctx: LinkContext<'_>,
    fetcher: &LinkFetcher,
    now: Timestamp,
    last_verified_at: Option<Timestamp>,
) -> LinkStatus {
    let sig = match offline_check(link, ctx, now) {
        Ok(s) => s,
        Err(status) => return status,
    };
    if !fetcher.cfg.check_forges {
        return LinkStatus::SignatureOnly;
    }
    let list = match fetcher.key_list(&link.provider, &link.host, &link.subject_id).await {
        Ok(l) => l,
        Err(FetchError::Gone) => {
            return LinkStatus::Stale { checked_at: now, reason: "the forge has no such account any more".into() }
        }
        Err(FetchError::NotConfigured(reason)) => return LinkStatus::NotChecked { reason },
        Err(FetchError::Inconsistent(reason)) | Err(FetchError::Transport(reason)) => {
            return LinkStatus::Unreachable { last_verified_at, reason }
        }
    };
    let listed = match &sig {
        Some(sig) => list.keys.iter().any(|k| k.key.key_data() == sig.public_key.key_data()),
        None => {
            // KeyPublished: the account key on the *signing* list.
            let mine = ctx.account_key.and_then(account_key_openssh).and_then(|l| parse_openssh(&l));
            mine.is_some_and(|m| list.keys.iter().any(|k| k.signing && k.key.key_data() == m.key_data()))
        }
    };
    if listed {
        LinkStatus::Verified { checked_at: now, handle: list.handle, name_bound: list.name_bound }
    } else {
        LinkStatus::Stale {
            checked_at: now,
            reason: match sig {
                Some(_) => "the forge no longer lists the key that signed this link".into(),
                None => "the Ligus key isn't on the forge's signing keys".into(),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Device-local cache

/// A cached check (the `identity_link_checks` table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedCheck {
    pub status: LinkStatus,
    pub checked_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verified_at: Option<Timestamp>,
}

/// Where cached checks live (the event store's `identity_link_checks`).
pub trait LinkCheckCache {
    fn get_check(&self, key: &str) -> Option<CachedCheck>;
    fn put_check(&self, key: &str, check: &CachedCheck);
}

/// The cache key: the link's content, the workspace it was published in and
/// the account key it is checked under (any change to the claim, or to the
/// key a published-key link stands for, is a new check).
pub fn cache_key(link: &IdentityLink, workspace_id: Uuid, account_key: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(workspace_id.as_bytes());
    h.update((account_key.len() as u64).to_be_bytes());
    h.update(account_key);
    h.update(serde_json::to_vec(link).unwrap_or_default());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A cached result still within [`CHECK_TTL`] (a definitive one only: an
/// unreachable forge is asked again next time).
pub fn fresh(c: &CachedCheck, now: Timestamp) -> bool {
    c.status.definitive() && now.0 - c.checked_at.0 < time::Duration::try_from(CHECK_TTL).unwrap_or(time::Duration::DAY)
}

/// [`check_link`] behind the cache: reuse a fresh result unless `force`;
/// otherwise ask and record. Offline outcomes (invalid, expired) are never
/// served from the cache.
pub async fn check_cached(
    cache: &dyn LinkCheckCacheSync,
    link: &IdentityLink,
    ctx: LinkContext<'_>,
    fetcher: &LinkFetcher,
    now: Timestamp,
    force: bool,
) -> LinkStatus {
    let key = cache_key(link, ctx.workspace_id, ctx.account_key.unwrap_or_default());
    let cached = cache.get_check(&key);
    if let Err(status) = offline_check(link, ctx, now) {
        return status;
    }
    if !force && fetcher.cfg.check_forges {
        if let Some(c) = cached.as_ref().filter(|c| fresh(c, now)) {
            return c.status.clone();
        }
    }
    let last = cached.as_ref().and_then(|c| match &c.status {
        LinkStatus::Verified { checked_at, .. } => Some(*checked_at),
        _ => c.last_verified_at,
    });
    let status = check_link(link, ctx, fetcher, now, last).await;
    if status.definitive() || matches!(status, LinkStatus::Unreachable { .. }) {
        let last_verified_at = if status.is_verified() { Some(now) } else { last };
        cache.put_check(&key, &CachedCheck { status: status.clone(), checked_at: now, last_verified_at });
    }
    status
}

/// A [`LinkCheckCache`] that can be shared across an await (`Sync`).
pub trait LinkCheckCacheSync: LinkCheckCache + Sync {}
impl<T: LinkCheckCache + Sync> LinkCheckCacheSync for T {}

/// An in-memory cache (tests, and the CLI's one-shot checks).
#[derive(Default)]
pub struct MemoryCache(pub std::sync::Mutex<BTreeMap<String, CachedCheck>>);

impl LinkCheckCache for MemoryCache {
    fn get_check(&self, key: &str) -> Option<CachedCheck> {
        self.0.lock().unwrap().get(key).cloned()
    }
    fn put_check(&self, key: &str, check: &CachedCheck) {
        self.0.lock().unwrap().insert(key.to_string(), check.clone());
    }
}

impl LinkCheckCache for crate::EventStore {
    fn get_check(&self, key: &str) -> Option<CachedCheck> {
        self.link_check(key)
    }
    fn put_check(&self, key: &str, check: &CachedCheck) {
        self.put_link_check(key, check)
    }
}

#[cfg(test)]
#[path = "identity_links_tests.rs"]
mod tests;

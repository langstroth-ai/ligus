//! Credentials: where a connection's tokens come from, and how git gets
//! them without ever seeing them on a command line.
//!
//! - [`TokenStore`] is the small seam to the OS secret store (the host
//!   implements it over `ligus_runtime::secret_store`, scope `forge`, name
//!   `<forge id>:<purpose>`). Nothing else in Ligus holds a forge token:
//!   not `settings.json`, not `ligus.db`, not synced events, not the relay.
//! - [`CredentialSource`] is what a forge asks for a token: the store
//!   ([`StoreCredentials`]), the host's existing `gh` login
//!   ([`GhCredentials`]), or nothing ([`Anonymous`]).
//! - Git never gets a token in argv or env: [`store_helper_args`] points
//!   `credential.<base>.helper` at `ligus forge git-credential <id>`, which
//!   answers git's `get` from the store ([`answer_git_credential`]).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{ForgeError, ForgeId, ForgeKind, Result};

/// A token in memory: zeroed on drop, never printed.
#[derive(Clone)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(Zeroizing::new(s.into()))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// What a token is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Purpose {
    /// Listing, and the agents' read-only tools.
    Read,
    /// Push, fork, open a change request.
    Write,
    /// An OAuth refresh token (device sign-in).
    Refresh,
}

impl Purpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Purpose::Read => "read",
            Purpose::Write => "write",
            Purpose::Refresh => "refresh",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "read" => Some(Purpose::Read),
            "write" => Some(Purpose::Write),
            _ => None,
        }
    }
}

/// The secret-store name of a token: `<forge id>:<purpose>` (scope `forge`).
pub fn secret_name(forge: &ForgeId, purpose: Purpose) -> String {
    format!("{}:{}", forge.as_str(), purpose.as_str())
}

/// The OS secret store, as far as forges need it.
pub trait TokenStore: Send + Sync {
    fn get(&self, forge: &ForgeId, purpose: Purpose) -> Result<Option<Secret>>;
    fn put(&self, forge: &ForgeId, purpose: Purpose, token: &Secret) -> Result<()>;
    fn delete(&self, forge: &ForgeId, purpose: Purpose) -> Result<()>;
    /// Which purposes hold a token (never the tokens).
    fn present(&self, forge: &ForgeId) -> Vec<Purpose> {
        [Purpose::Read, Purpose::Write, Purpose::Refresh].into_iter().filter(|p| self.get(forge, *p).ok().flatten().is_some()).collect()
    }
    /// Remove every token of a connection.
    fn delete_all(&self, forge: &ForgeId) -> Result<()> {
        for p in [Purpose::Read, Purpose::Write, Purpose::Refresh] {
            self.delete(forge, p)?;
        }
        Ok(())
    }
}

/// An in-memory store (tests, and a box with no store at all).
#[derive(Default)]
pub struct MemoryTokens {
    map: Mutex<BTreeMap<String, Secret>>,
}

impl TokenStore for MemoryTokens {
    fn get(&self, forge: &ForgeId, purpose: Purpose) -> Result<Option<Secret>> {
        Ok(self.map.lock().unwrap_or_else(|e| e.into_inner()).get(&secret_name(forge, purpose)).cloned())
    }
    fn put(&self, forge: &ForgeId, purpose: Purpose, token: &Secret) -> Result<()> {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).insert(secret_name(forge, purpose), token.clone());
        Ok(())
    }
    fn delete(&self, forge: &ForgeId, purpose: Purpose) -> Result<()> {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).remove(&secret_name(forge, purpose));
        Ok(())
    }
}

/// Where a connection's credential comes from (shown in Settings; never the
/// token).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthSource {
    /// The host's `gh` login.
    Gh,
    /// A pasted token.
    Token,
    /// A device sign-in (OAuth).
    Oauth,
    /// Nothing: anonymous reads of public repositories only.
    None,
}

impl AuthSource {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthSource::Gh => "gh",
            AuthSource::Token => "token",
            AuthSource::Oauth => "oauth",
            AuthSource::None => "none",
        }
    }
}

/// What a forge asks for credentials.
#[async_trait]
pub trait CredentialSource: Send + Sync {
    fn source(&self) -> AuthSource;
    /// A token for `purpose`. Reads fall back to the write token (on the
    /// host only; agents never see either); writes never use a read token.
    async fn token(&self, purpose: Purpose) -> Result<Option<Secret>>;
    /// Whether a write is possible at all (without fetching the token).
    fn has_write(&self) -> bool;
    /// Why nothing can be signed in with, for the UI.
    fn signed_out_reason(&self) -> Option<String> {
        None
    }
    /// The git credential helper command (after `!`) for HTTPS pushes.
    fn git_helper(&self) -> Option<String>;
}

/// No credentials.
pub struct Anonymous;

#[async_trait]
impl CredentialSource for Anonymous {
    fn source(&self) -> AuthSource {
        AuthSource::None
    }
    async fn token(&self, _: Purpose) -> Result<Option<Secret>> {
        Ok(None)
    }
    fn has_write(&self) -> bool {
        false
    }
    fn signed_out_reason(&self) -> Option<String> {
        Some("no credentials for this code host on this device".into())
    }
    fn git_helper(&self) -> Option<String> {
        None
    }
}

/// How to refresh an expiring OAuth access token (device sign-in).
#[derive(Debug, Clone)]
pub struct RefreshConfig {
    /// `https://github.com/login/oauth/access_token`, `{base}/oauth/token`.
    pub token_url: String,
    pub client_id: String,
    /// The connection is marked internal on this device.
    pub internal: bool,
}

/// Tokens in the OS secret store.
pub struct StoreCredentials {
    pub store: Arc<dyn TokenStore>,
    pub forge: ForgeId,
    pub source: AuthSource,
    /// The command after `!` that answers git from the store
    /// (`'/path/ligus' forge git-credential '<id>'`).
    pub helper: Option<String>,
    /// Unix seconds the stored OAuth access token expires (device sign-in).
    pub expires_at: Mutex<Option<u64>>,
    pub refresh: Option<RefreshConfig>,
    /// Told the new expiry after a refresh, once, while the refresh still
    /// holds the data dir's lock (the host saves it to `forges.toml`).
    pub on_refresh: Option<Arc<dyn Fn(Option<u64>) + Send + Sync>>,
    /// The data dir whose `forges.toml` holds this connection: refreshes
    /// take its [`crate::conn::ForgesLock`] and re-read the saved expiry
    /// first, so only one process ever spends a (rotating) refresh token.
    pub data_dir: Option<std::path::PathBuf>,
    /// One refresh at a time within this instance.
    refreshing: tokio::sync::Mutex<()>,
}

impl StoreCredentials {
    pub fn new(store: Arc<dyn TokenStore>, forge: ForgeId, source: AuthSource, helper: Option<String>) -> Self {
        Self {
            store,
            forge,
            source,
            helper,
            expires_at: Mutex::new(None),
            refresh: None,
            on_refresh: None,
            data_dir: None,
            refreshing: tokio::sync::Mutex::new(()),
        }
    }

    fn fresh(exp: Option<u64>, now: u64) -> bool {
        exp.is_none_or(|e| e > now + 60)
    }

    async fn refresh_if_expired(&self) -> Result<()> {
        let Some(cfg) = &self.refresh else { return Ok(()) };
        let exp = || *self.expires_at.lock().unwrap_or_else(|e| e.into_inner());
        let now = crate::device::now_unix();
        if Self::fresh(exp(), now) {
            return Ok(());
        }
        // GitLab rotates refresh tokens: a second refresh with the one just
        // spent fails and signs the device out. So one refresh at a time,
        // here and across every process using this data dir, and whoever
        // comes second takes the expiry the first one saved.
        let _here = self.refreshing.lock().await;
        if Self::fresh(exp(), now) {
            return Ok(());
        }
        let _lock = match &self.data_dir {
            Some(d) => Some(crate::conn::ForgesLock::acquire_async(d).await?),
            None => None,
        };
        if let Some(d) = &self.data_dir {
            let saved = crate::conn::Connections::load(d).get(&self.forge).and_then(|c| c.token_expires_at);
            if saved.is_some_and(|e| e > now + 60) {
                *self.expires_at.lock().unwrap_or_else(|e| e.into_inner()) = saved;
                return Ok(());
            }
        }
        let Some(rt) = self.store.get(&self.forge, Purpose::Refresh)? else {
            return Err(ForgeError::NotSignedIn("the device sign-in for this code host expired: sign in again".into()));
        };
        let set = crate::device::refresh(&cfg.token_url, &cfg.client_id, &rt, cfg.internal).await?;
        self.store.put(&self.forge, Purpose::Write, &set.access)?;
        if let Some(r) = &set.refresh {
            self.store.put(&self.forge, Purpose::Refresh, r)?;
        }
        let new_exp = set.expires_in.map(|s| now + s);
        *self.expires_at.lock().unwrap_or_else(|e| e.into_inner()) = new_exp;
        if let Some(f) = &self.on_refresh {
            f(new_exp);
        }
        Ok(())
    }
}

#[async_trait]
impl CredentialSource for StoreCredentials {
    fn source(&self) -> AuthSource {
        self.source
    }
    async fn token(&self, purpose: Purpose) -> Result<Option<Secret>> {
        self.refresh_if_expired().await?;
        match purpose {
            Purpose::Read => Ok(self.store.get(&self.forge, Purpose::Read)?.or(self.store.get(&self.forge, Purpose::Write)?)),
            Purpose::Write => self.store.get(&self.forge, Purpose::Write),
            Purpose::Refresh => Err(ForgeError::Invalid("a refresh token is never handed out".into())),
        }
    }
    fn has_write(&self) -> bool {
        self.store.get(&self.forge, Purpose::Write).ok().flatten().is_some()
    }
    fn signed_out_reason(&self) -> Option<String> {
        let any = self.store.get(&self.forge, Purpose::Read).ok().flatten().is_some() || self.has_write();
        (!any).then(|| "no token for this code host on this device: add one in Settings > Code hosts".into())
    }
    fn git_helper(&self) -> Option<String> {
        self.helper.clone().filter(|_| self.has_write())
    }
}

/// The host's own `gh` login (GitHub only). Ligus never stores this token:
/// it asks `gh auth token --hostname <host>` when a request needs one, and
/// git uses `gh auth git-credential` directly.
/// When `gh` was last asked, and what it said.
type GhProbe = (Instant, std::result::Result<Secret, String>);

#[derive(Clone)]
pub struct GhCredentials {
    pub host: String,
    pub gh: String,
    cache: Arc<Mutex<Option<GhProbe>>>,
}

/// Why `gh` can't be used: not installed.
pub const GH_MISSING: &str = "Install the GitHub CLI (gh) on the host, then run gh auth login on the host";
/// Why `gh` can't be used: not signed in.
pub const GH_SIGNED_OUT: &str = "Run gh auth login on the host";

impl GhCredentials {
    pub fn new(host: impl Into<String>) -> Self {
        Self { host: host.into(), gh: "gh".into(), cache: Arc::new(Mutex::new(None)) }
    }

    /// Ask `gh` (cached for a minute, both ways).
    pub fn probe(&self) -> std::result::Result<Secret, String> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, r)) = cache.as_ref() {
            if at.elapsed() < Duration::from_secs(60) {
                return r.clone();
            }
        }
        // A test build never asks the host's own gh (a fake one by path is fine).
        if self.gh == "gh" && crate::http::real_hosts_denied() {
            let r: std::result::Result<Secret, String> = Err(GH_SIGNED_OUT.to_string());
            *cache = Some((Instant::now(), r.clone()));
            return r;
        }
        let mut c = std::process::Command::new(&self.gh);
        c.args(["auth", "token", "--hostname", &self.host])
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .stdin(std::process::Stdio::null());
        let r = match c.output() {
            Err(_) => Err(GH_MISSING.to_string()),
            Ok(o) if !o.status.success() => Err(GH_SIGNED_OUT.to_string()),
            Ok(o) => {
                let t = Secret::new(String::from_utf8_lossy(&o.stdout).trim().to_string());
                if t.is_empty() {
                    Err(GH_SIGNED_OUT.to_string())
                } else {
                    Ok(t)
                }
            }
        };
        *cache = Some((Instant::now(), r.clone()));
        r
    }
}

#[async_trait]
impl CredentialSource for GhCredentials {
    fn source(&self) -> AuthSource {
        AuthSource::Gh
    }
    async fn token(&self, _purpose: Purpose) -> Result<Option<Secret>> {
        let me = self.clone();
        let r = tokio::task::spawn_blocking(move || me.probe()).await.map_err(|e| ForgeError::Transport(e.to_string()))?;
        Ok(r.ok())
    }
    fn has_write(&self) -> bool {
        self.probe().is_ok()
    }
    fn signed_out_reason(&self) -> Option<String> {
        self.probe().err()
    }
    fn git_helper(&self) -> Option<String> {
        self.probe().ok().map(|_| format!("{} auth git-credential", self.gh))
    }
}

// ── Git credential helper ──────────────────────────────────────────────────

/// Single-quote for `sh` (the `!` helper runs through the shell).
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The helper command for a connection: `'<ligus>' forge git-credential '<id>'`.
pub fn store_helper_command(program: &std::path::Path, forge: &ForgeId) -> String {
    format!("{} forge git-credential {}", sh_quote(&program.to_string_lossy()), sh_quote(forge.as_str()))
}

/// `git -c` args for `base` (`https://host[:port]`) with `helper` (the part
/// after `!`): reset any inherited helper for that URL, then use ours. The
/// token is not in them.
pub fn helper_args(base: &str, helper: &str) -> Vec<String> {
    let base = base.trim_end_matches('/');
    vec!["-c".into(), format!("credential.{base}.helper="), "-c".into(), format!("credential.{base}.helper=!{helper}")]
}

/// The username git sends with a token. GitHub ignores it for tokens
/// (`x-access-token` is what it documents for app tokens); GitLab wants
/// `oauth2` for OAuth tokens and accepts any non-empty name with a PAT;
/// Forgejo/Gitea take the account's login with the token as password.
pub fn git_username(kind: ForgeKind, source: AuthSource, login: Option<&str>) -> String {
    match kind {
        ForgeKind::GitHub => "x-access-token".into(),
        ForgeKind::GitLab if source == AuthSource::Oauth => "oauth2".into(),
        ForgeKind::GitLab => login.filter(|l| !l.is_empty()).unwrap_or("oauth2").into(),
        ForgeKind::Gitea => login.filter(|l| !l.is_empty()).unwrap_or("ligus").into(),
    }
}

/// Answer one git credential helper invocation. `action` is git's last
/// argument (`get`, `store`, `erase`); `input` is what git wrote on stdin.
/// Only `get` answers, and only for `https` to this connection's own host
/// (a helper scoped by URL can still be asked about another host through
/// redirects or config mistakes: the token never goes anywhere else).
/// `store`/`erase` are ignored: git never writes into Ligus's store.
pub fn answer_git_credential(action: &str, input: &str, host: &str, username: &str, token: Option<&Secret>) -> Option<String> {
    if action != "get" {
        return None;
    }
    let mut kv = BTreeMap::new();
    for line in input.lines() {
        if let Some((k, v)) = line.split_once('=') {
            kv.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    if kv.get("protocol").map(String::as_str) != Some("https") {
        return None;
    }
    let asked = kv.get("host")?;
    if !asked.eq_ignore_ascii_case(host) {
        return None;
    }
    let token = token?;
    if token.is_empty() || token.expose().contains(['\n', '\r', '\0']) {
        return None;
    }
    // The login is the code host's (cached at the last check): it never adds
    // a key to the credential protocol.
    if username.chars().any(char::is_control) {
        return None;
    }
    Some(format!("protocol=https\nhost={asked}\nusername={username}\npassword={}\n", token.expose()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_never_print() {
        let s = Secret::new("ghp_supersecret");
        assert_eq!(format!("{s:?}"), "Secret(***)");
        assert!(!format!("{:?}", Some(s)).contains("supersecret"));
    }

    #[test]
    fn reads_fall_back_to_write_but_writes_never_use_read() {
        let store: Arc<dyn TokenStore> = Arc::new(MemoryTokens::default());
        let id = ForgeId::github_com();
        let c = StoreCredentials::new(store.clone(), id.clone(), AuthSource::Token, Some("h".into()));
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        store.put(&id, Purpose::Read, &Secret::new("r")).unwrap();
        assert_eq!(rt.block_on(c.token(Purpose::Read)).unwrap().unwrap().expose(), "r");
        assert!(rt.block_on(c.token(Purpose::Write)).unwrap().is_none(), "a read token never writes");
        assert!(!c.has_write() && c.git_helper().is_none(), "no write token, no git helper");
        store.put(&id, Purpose::Write, &Secret::new("w")).unwrap();
        assert_eq!(rt.block_on(c.token(Purpose::Write)).unwrap().unwrap().expose(), "w");
        store.delete(&id, Purpose::Read).unwrap();
        assert_eq!(rt.block_on(c.token(Purpose::Read)).unwrap().unwrap().expose(), "w", "the host reads with the write token when it is the only one");
        assert!(rt.block_on(c.token(Purpose::Refresh)).is_err(), "a refresh token is never handed out");
        assert_eq!(store.present(&id), vec![Purpose::Write]);
    }

    #[test]
    fn git_helper_answers_get_for_its_own_https_host_only() {
        let t = Secret::new("tok");
        let ok = answer_git_credential("get", "protocol=https\nhost=github.com\npath=a/b.git\n", "github.com", "x-access-token", Some(&t)).unwrap();
        assert_eq!(ok, "protocol=https\nhost=github.com\nusername=x-access-token\npassword=tok\n");
        assert!(answer_git_credential("get", "protocol=https\nhost=evil.example\n", "github.com", "u", Some(&t)).is_none());
        assert!(answer_git_credential("get", "protocol=http\nhost=github.com\n", "github.com", "u", Some(&t)).is_none(), "never over plain http");
        assert!(answer_git_credential("store", "protocol=https\nhost=github.com\npassword=x\n", "github.com", "u", Some(&t)).is_none());
        assert!(answer_git_credential("get", "protocol=https\nhost=github.com\n", "github.com", "u", None).is_none());
        assert!(answer_git_credential("get", "protocol=https\nhost=github.com\n", "github.com", "u", Some(&Secret::new("a\nb"))).is_none());
        // The login comes from the code host (cached by the last check): a
        // newline in it must not add credential-protocol keys.
        let inj = answer_git_credential("get", "protocol=https\nhost=codeberg.org\n", "codeberg.org", "ana\nhost=evil.example\nquit=1", Some(&t));
        assert!(inj.clone().is_none_or(|o| o.lines().count() == 4 && !o.contains("evil.example")), "{inj:?}");
    }

    #[test]
    fn helper_args_never_carry_the_token_and_quote_paths() {
        let cmd = store_helper_command(std::path::Path::new("/Applications/Ligus it's.app/ligus"), &ForgeId::parse("gitlab:gitlab.example.com").unwrap());
        assert_eq!(cmd, "'/Applications/Ligus it'\\''s.app/ligus' forge git-credential 'gitlab:gitlab.example.com'");
        let args = helper_args("https://gitlab.example.com/", &cmd);
        assert_eq!(args[1], "credential.https://gitlab.example.com.helper=");
        assert!(args[3].starts_with("credential.https://gitlab.example.com.helper=!'/Applications"));
        assert_eq!(git_username(ForgeKind::GitLab, AuthSource::Oauth, Some("ana")), "oauth2");
        assert_eq!(git_username(ForgeKind::Gitea, AuthSource::Token, Some("ana")), "ana");
    }
}

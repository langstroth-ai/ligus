//! The device-local list of forge connections (`<data dir>/forges.toml`).
//!
//! It holds what a connection *is* (kind, base URL, where its credential
//! comes from, the cached login) and never a token: tokens are in the OS
//! secret store under `forge:<id>:<purpose>`. The file is never synced.
//! With no file, a box has one implicit connection: github.com through the
//! host's `gh` login (what `ligus serve` did before forges existed).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::tokens::{Anonymous, AuthSource, CredentialSource, GhCredentials, RefreshConfig, StoreCredentials, TokenStore};
use crate::{spec, Forge, ForgeError, ForgeId, ForgeKind, Result};

pub const FILE: &str = "forges.toml";

/// The lock file that serialises token refreshes and edits of
/// `forges.toml` across this device's processes (`ligus serve`, the agents'
/// read tools, the git credential helper, the desktop).
pub const LOCK_FILE: &str = ".forges.lock";

/// An exclusive hold on [`LOCK_FILE`] (released on drop). One holder per
/// data dir at a time, across processes and threads: each acquire opens its
/// own file description, so `flock` serialises threads of one process too.
/// Not reentrant: never acquire it again while holding it.
pub struct ForgesLock {
    _file: std::fs::File,
}

impl ForgesLock {
    /// Block until this process holds the lock.
    pub fn acquire(data_dir: &Path) -> Result<Self> {
        let io = |e: std::io::Error| ForgeError::Store(format!("couldn't lock the code host connections: {e}"));
        std::fs::create_dir_all(data_dir).map_err(io)?;
        let file = std::fs::OpenOptions::new().create(true).read(true).write(true).truncate(false).open(data_dir.join(LOCK_FILE)).map_err(io)?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            loop {
                // SAFETY: flock on a file descriptor this function owns; it
                // stays open (and the lock held) for the life of `ForgesLock`.
                if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                    break;
                }
                let e = std::io::Error::last_os_error();
                if e.kind() != std::io::ErrorKind::Interrupted {
                    return Err(io(e));
                }
            }
        }
        // Closing the file releases the lock.
        Ok(Self { _file: file })
    }

    /// [`ForgesLock::acquire`] off the async runtime.
    pub async fn acquire_async(data_dir: &Path) -> Result<Self> {
        let d = data_dir.to_path_buf();
        tokio::task::spawn_blocking(move || Self::acquire(&d)).await.map_err(|e| ForgeError::Store(e.to_string()))?
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub id: ForgeId,
    pub kind: ForgeKind,
    /// The web base: `https://github.com`, `https://gitlab.example.com`,
    /// `https://codeberg.org` (a path prefix is allowed for an instance
    /// under a sub-path).
    pub base_url: String,
    /// The API base, when it isn't derived from `base_url` (tests, unusual
    /// proxies).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
    /// `forgejo`, `gitea`, `codeberg` or `ghes`, for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flavor: Option<String>,
    pub auth: AuthSource,
    /// A public OAuth client id for this instance's device sign-in
    /// (self-managed GitLab, GHES). Not a secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// The signed-in account, cached at the last check (display only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// When the stored OAuth access token expires (unix seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_expires_at: Option<u64>,
    /// The person marked this connection internal on this device: its host
    /// may be (or resolve to) a loopback, private or link-local address.
    /// Without the mark such addresses are refused (when added, and on every
    /// request), so a code-host URL can't be used to reach this device's own
    /// network. Device-local, like the rest of this file.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub internal: bool,
}

/// A base URL Ligus will send a token to: `https://host[:port][/prefix]`, or
/// `http://` only to a loopback host. No credentials, query or fragment.
pub fn validate_base_url(raw: &str) -> Result<String> {
    let raw = raw.trim().trim_end_matches('/');
    // Only plain printable ASCII: a backslash (a path separator to the URL
    // parser), percent-escapes in the authority, spaces or control bytes would
    // let the host shown differ from the host requests reach.
    if !raw.bytes().all(|b| b.is_ascii_graphic()) || raw.contains('\\') {
        return Err(ForgeError::Invalid(format!("'{raw}' has characters a code host URL can't have")));
    }
    let u = url::Url::parse(raw).map_err(|_| ForgeError::Invalid(format!("'{raw}' isn't a URL")))?;
    let host = u.host_str().ok_or_else(|| ForgeError::Invalid("the URL has no host".into()))?.to_ascii_lowercase();
    let loopback = host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    match u.scheme() {
        "https" => {}
        "http" if loopback => {}
        "http" => return Err(ForgeError::Invalid("a code host must be https (tokens never travel over plain http)".into())),
        _ => return Err(ForgeError::Invalid("a code host URL is https://…".into())),
    }
    if !u.username().is_empty() || u.password().is_some() || u.query().is_some() || u.fragment().is_some() {
        return Err(ForgeError::Invalid("a code host URL can't carry credentials, a query or a fragment".into()));
    }
    if raw.split('/').any(|s| s == ".." || s == ".") {
        return Err(ForgeError::Invalid("bad path in the code host URL".into()));
    }
    let parsed_host = match u.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.clone(),
    };
    if spec::host_of(raw).as_deref() != Some(parsed_host.as_str()) {
        return Err(ForgeError::Invalid(format!("'{raw}' doesn't name one clear host")));
    }
    Ok(raw.to_string())
}

/// Save a finished device sign-in's tokens and mark the connection OAuth,
/// under [`ForgesLock`]. Nothing is stored (and `Ok(false)` returned) when
/// the connection is gone or `still_current` says the sign-in that produced
/// these tokens is no longer the pending one (the connection was removed,
/// perhaps added again, or another sign-in started since): a background
/// poll must never leave a stray token behind, nor attach one to a
/// connection nobody signed in on. Not for use while holding the lock.
pub fn save_device_sign_in(data_dir: &Path, store: &dyn TokenStore, id: &ForgeId, t: &crate::device::TokenSet, still_current: impl FnOnce() -> bool) -> Result<bool> {
    let _lock = ForgesLock::acquire(data_dir)?;
    let mut all = Connections::load(data_dir);
    if all.get(id).is_none() || !still_current() {
        return Ok(false);
    }
    store.put(id, crate::tokens::Purpose::Write, &t.access)?;
    match &t.refresh {
        Some(r) => store.put(id, crate::tokens::Purpose::Refresh, r)?,
        None => store.delete(id, crate::tokens::Purpose::Refresh)?,
    }
    if let Some(c) = all.get_mut(id) {
        c.auth = AuthSource::Oauth;
        c.token_expires_at = t.expires_in.map(|s| crate::device::now_unix() + s);
    }
    all.save(data_dir)?;
    Ok(true)
}

/// Refuse a connection whose host is, or resolves to, an internal address
/// (see [`crate::http::is_public_ip`]) unless it is marked internal. Test
/// builds resolve no names but `localhost` (the request-time resolver still
/// applies); a name that doesn't resolve here is left to that resolver.
pub async fn check_address(c: &Connection) -> Result<()> {
    if c.internal {
        return Ok(());
    }
    for raw in std::iter::once(c.base_url.as_str()).chain(c.api_url.as_deref()) {
        crate::http::check_url_address(raw, false)?;
        let Ok(u) = url::Url::parse(raw) else { continue };
        let Some(url::Host::Domain(host)) = u.host() else { continue };
        let host = host.to_ascii_lowercase();
        if crate::http::real_hosts_denied() && host != "localhost" {
            continue;
        }
        let port = u.port_or_known_default().unwrap_or(443);
        let lookup = tokio::time::timeout(std::time::Duration::from_secs(5), tokio::net::lookup_host((host.as_str(), port))).await;
        if let Ok(Ok(addrs)) = lookup {
            let addrs: Vec<_> = addrs.collect();
            if !addrs.is_empty() && addrs.iter().all(|a| !crate::http::is_public_ip(a.ip())) {
                return Err(crate::http::internal_refused(&host));
            }
        }
    }
    Ok(())
}

/// The forge a well-known host runs.
pub fn known_kind(host: &str) -> Option<(ForgeKind, Option<&'static str>)> {
    match host.trim().to_ascii_lowercase().as_str() {
        "github.com" | "www.github.com" => Some((ForgeKind::GitHub, None)),
        "gitlab.com" | "www.gitlab.com" => Some((ForgeKind::GitLab, None)),
        "codeberg.org" => Some((ForgeKind::Gitea, Some("codeberg"))),
        "gitea.com" => Some((ForgeKind::Gitea, Some("gitea"))),
        _ => None,
    }
}

impl Connection {
    /// A new connection (validated).
    pub fn new(kind: ForgeKind, base_url: &str, auth: AuthSource) -> Result<Self> {
        let base_url = validate_base_url(base_url)?;
        let host = spec::host_of(&base_url).ok_or_else(|| ForgeError::Invalid("the URL has no valid host".into()))?;
        if auth == AuthSource::Gh && kind != ForgeKind::GitHub {
            return Err(ForgeError::Invalid("the gh login works only for GitHub".into()));
        }
        let flavor = known_kind(&host).and_then(|(_, f)| f).map(str::to_string).or_else(|| (kind == ForgeKind::GitHub && host != "github.com").then(|| "ghes".into()));
        Ok(Self { id: ForgeId::new(kind, &host), kind, base_url, api_url: None, flavor, auth, client_id: None, login: None, user_id: None, token_expires_at: None, internal: false })
    }

    /// The default connection: github.com through the host's `gh` login.
    pub fn github_gh() -> Self {
        Self::new(ForgeKind::GitHub, "https://github.com", AuthSource::Gh).expect("valid")
    }

    pub fn host(&self) -> String {
        spec::host_of(&self.base_url).unwrap_or_default()
    }

    /// Where the REST API is.
    pub fn api_base(&self) -> String {
        if let Some(a) = &self.api_url {
            return a.trim_end_matches('/').to_string();
        }
        let base = self.base_url.trim_end_matches('/');
        match self.kind {
            ForgeKind::GitHub if self.host() == "github.com" => "https://api.github.com".into(),
            ForgeKind::GitHub => format!("{base}/api/v3"),
            ForgeKind::GitLab => format!("{base}/api/v4"),
            ForgeKind::Gitea => format!("{base}/api/v1"),
        }
    }

    /// Display name: `GitHub`, `GitLab (gitlab.example.com)`, `Codeberg`.
    pub fn display(&self) -> String {
        let host = self.host();
        match (self.kind, self.flavor.as_deref()) {
            (ForgeKind::GitHub, _) if host == "github.com" => "GitHub".into(),
            (ForgeKind::GitLab, _) if host == "gitlab.com" => "GitLab".into(),
            (_, Some("codeberg")) => "Codeberg".into(),
            (ForgeKind::GitHub, _) => format!("GitHub Enterprise ({host})"),
            (ForgeKind::GitLab, _) => format!("GitLab ({host})"),
            (ForgeKind::Gitea, Some("forgejo")) => format!("Forgejo ({host})"),
            (ForgeKind::Gitea, _) => format!("Gitea ({host})"),
        }
    }

    /// The credential source for this connection.
    pub fn credentials(&self, store: Arc<dyn TokenStore>, helper_program: Option<&Path>, data_dir: Option<&Path>) -> Arc<dyn CredentialSource> {
        self.credentials_with_helper(store, helper_program.map(|p| crate::tokens::store_helper_command(p, &self.id)), data_dir)
    }

    /// [`Connection::credentials`] with a ready-made helper command (the
    /// part after `!`), for hosts whose helper takes extra arguments.
    ///
    /// `data_dir` is where `forges.toml` lives: a refreshed device sign-in's
    /// expiry is saved there (once, under [`ForgesLock`]), and refreshes are
    /// serialised with every other process using that data dir.
    pub fn credentials_with_helper(&self, store: Arc<dyn TokenStore>, helper: Option<String>, data_dir: Option<&Path>) -> Arc<dyn CredentialSource> {
        match self.auth {
            AuthSource::Gh => Arc::new(GhCredentials::new(self.host())),
            AuthSource::None => Arc::new(Anonymous),
            AuthSource::Token | AuthSource::Oauth => {
                let mut c = StoreCredentials::new(store, self.id.clone(), self.auth, helper);
                if self.auth == AuthSource::Oauth {
                    *c.expires_at.get_mut().unwrap_or_else(|e| e.into_inner()) = self.token_expires_at;
                    if let Ok(ep) = crate::device::endpoints(self) {
                        c.refresh = Some(RefreshConfig { token_url: ep.token_url, client_id: ep.client_id, internal: self.internal });
                    }
                    if let Some(dir) = data_dir {
                        c.data_dir = Some(dir.to_path_buf());
                        let (dir, id) = (dir.to_path_buf(), self.id.clone());
                        // Runs under the lock the refresh holds: no second
                        // acquire here.
                        c.on_refresh = Some(Arc::new(move |exp| {
                            let mut all = Connections::load(&dir);
                            if let Some(c) = all.get_mut(&id) {
                                c.token_expires_at = exp;
                                if let Err(e) = all.save(&dir) {
                                    tracing::warn!("couldn't save the refreshed sign-in expiry for {id}: {e}");
                                }
                            }
                        }));
                    }
                }
                Arc::new(c)
            }
        }
    }

    /// The forge for this connection with `creds`.
    pub fn build(&self, creds: Arc<dyn CredentialSource>) -> Arc<dyn Forge> {
        match self.kind {
            ForgeKind::GitHub => Arc::new(crate::github::GitHub::new(self, creds)),
            ForgeKind::GitLab => Arc::new(crate::gitlab::GitLab::new(self, creds)),
            ForgeKind::Gitea => Arc::new(crate::gitea::Gitea::new(self, creds)),
        }
    }
}

/// The connections on this device.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connections {
    #[serde(default, rename = "forge")]
    pub forges: Vec<Connection>,
}

impl Connections {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(FILE)
    }

    /// The saved list, or just the implicit `gh` GitHub connection when
    /// nothing was ever saved.
    pub fn load(data_dir: &Path) -> Self {
        match std::fs::read_to_string(Self::path(data_dir)) {
            Ok(t) => {
                let mut cs: Self = toml::from_str(&t).unwrap_or_default();
                // A hand-edited entry whose URLs don't validate, or whose id
                // names another host than its URL, is dropped: a token is only
                // ever sent to the host the connection is shown as.
                cs.forges.retain(|c| {
                    validate_base_url(&c.base_url).is_ok_and(|b| spec::host_of(&b).is_some_and(|h| c.id == ForgeId::new(c.kind, &h)))
                        && c.api_url.as_deref().is_none_or(|a| validate_base_url(a).is_ok())
                });
                cs
            }
            Err(_) => Self { forges: vec![Connection::github_gh()] },
        }
    }

    /// Write atomically (temp file, then rename), owner-only.
    pub fn save(&self, data_dir: &Path) -> Result<()> {
        let io = |e: std::io::Error| ForgeError::Store(e.to_string());
        std::fs::create_dir_all(data_dir).map_err(io)?;
        let text = toml::to_string_pretty(self).map_err(|e| ForgeError::Store(e.to_string()))?;
        let tmp = data_dir.join(format!(".{FILE}.{}.tmp", uuid::Uuid::new_v4().simple()));
        std::fs::write(&tmp, text).map_err(io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, Self::path(data_dir)).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            io(e)
        })
    }

    pub fn get(&self, id: &ForgeId) -> Option<&Connection> {
        self.forges.iter().find(|c| &c.id == id)
    }

    pub fn get_mut(&mut self, id: &ForgeId) -> Option<&mut Connection> {
        self.forges.iter_mut().find(|c| &c.id == id)
    }

    /// The connection for a host (`github.com`, `codeberg.org`).
    pub fn for_host(&self, host: &str) -> Option<&Connection> {
        self.forges.iter().find(|c| spec::same_host(&c.host(), host))
    }

    /// Add or replace (by id).
    pub fn upsert(&mut self, c: Connection) {
        match self.get_mut(&c.id) {
            Some(x) => *x = c,
            None => self.forges.push(c),
        }
    }

    /// Load, change and save `forges.toml` under [`ForgesLock`], so a
    /// refresh saving its expiry and another edit never lose each other's
    /// change. Not for use while holding the lock.
    pub fn update<T>(data_dir: &Path, f: impl FnOnce(&mut Connections) -> Result<T>) -> Result<T> {
        let _lock = ForgesLock::acquire(data_dir)?;
        let mut all = Self::load(data_dir);
        let out = f(&mut all)?;
        all.save(data_dir)?;
        Ok(out)
    }

    pub fn remove(&mut self, id: &ForgeId) -> bool {
        let n = self.forges.len();
        self.forges.retain(|c| &c.id != id);
        n != self.forges.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_urls_must_be_tls_unless_loopback() {
        assert!(validate_base_url("https://gitlab.example.com/").is_ok());
        assert!(validate_base_url("https://git.example.com/gitlab").is_ok());
        assert!(validate_base_url("http://127.0.0.1:3000").is_ok());
        for bad in ["http://gitlab.example.com", "ftp://x", "https://u:p@x.com", "https://x.com/?a=1", "https://x.com/#f", "nope", "https://x.com/a/../b"] {
            assert!(validate_base_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn ids_api_bases_and_names() {
        let gh = Connection::github_gh();
        assert_eq!((gh.id.as_str(), gh.api_base().as_str()), ("github:github.com", "https://api.github.com"));
        let ghes = Connection::new(ForgeKind::GitHub, "https://ghe.corp.example", AuthSource::Token).unwrap();
        assert_eq!((ghes.api_base().as_str(), ghes.flavor.as_deref()), ("https://ghe.corp.example/api/v3", Some("ghes")));
        let gl = Connection::new(ForgeKind::GitLab, "https://gitlab.example.com", AuthSource::Token).unwrap();
        assert_eq!((gl.id.as_str(), gl.api_base().as_str()), ("gitlab:gitlab.example.com", "https://gitlab.example.com/api/v4"));
        let cb = Connection::new(ForgeKind::Gitea, "https://codeberg.org", AuthSource::Token).unwrap();
        assert_eq!((cb.display().as_str(), cb.api_base().as_str()), ("Codeberg", "https://codeberg.org/api/v1"));
        assert!(Connection::new(ForgeKind::GitLab, "https://gitlab.com", AuthSource::Gh).is_err());
        assert_eq!(ForgeId::parse("github.com").unwrap(), ForgeId::github_com());
        assert!(ForgeId::parse("svn:x.com").is_err() && ForgeId::parse("gitea:bad host").is_err());
    }

    #[test]
    fn the_file_holds_no_tokens_and_defaults_to_gh() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(Connections::load(d.path()).forges, vec![Connection::github_gh()]);
        let mut cs = Connections::default();
        cs.upsert(Connection::new(ForgeKind::Gitea, "https://codeberg.org", AuthSource::Token).unwrap());
        cs.save(d.path()).unwrap();
        let text = std::fs::read_to_string(Connections::path(d.path())).unwrap();
        assert!(!text.to_lowercase().contains("token =") && text.contains("codeberg.org"), "{text}");
        assert_eq!(Connections::load(d.path()), cs, "a saved empty-of-GitHub list stays that way");
        assert!(cs.for_host("www.codeberg.org").is_some());
    }

    /// The host Ligus shows and keys a connection by must be the host its
    /// requests reach. A backslash is a path separator to the URL parser
    /// (WHATWG), so `https://evil.example\\@gitlab.com` parses to host
    /// `evil.example` while a naive split reads `gitlab.com`.
    #[test]
    fn base_url_host_is_the_host_requests_reach() {
        for raw in ["https://evil.example\\@gitlab.com", "https://evil.example\\@codeberg.org/", "https://evil.example%5C@gitlab.com"] {
            match Connection::new(ForgeKind::GitLab, raw, AuthSource::Token) {
                Err(_) => {}
                Ok(c) => {
                    let reached = url::Url::parse(&c.api_base()).unwrap().host_str().unwrap().to_string();
                    assert_eq!(c.host(), reached, "{raw}: shown as {} but requests go to {reached}", c.display());
                }
            }
        }
        // Loaded from a hand-edited forges.toml, too.
        let toml = "[[forge]]\nid = \"gitlab:gitlab.com\"\nkind = \"gitlab\"\nbaseUrl = \"https://evil.example\\\\@gitlab.com\"\nauth = \"token\"\n";
        let d = tempfile::tempdir().unwrap();
        std::fs::write(Connections::path(d.path()), toml).unwrap();
        for c in Connections::load(d.path()).forges {
            let reached = url::Url::parse(&c.api_base()).unwrap().host_str().unwrap().to_string();
            assert_eq!(c.host(), reached);
        }
    }
}

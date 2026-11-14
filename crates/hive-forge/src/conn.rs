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
}

/// A base URL Ligus will send a token to: `https://host[:port][/prefix]`, or
/// `http://` only to a loopback host. No credentials, query or fragment.
pub fn validate_base_url(raw: &str) -> Result<String> {
    let raw = raw.trim().trim_end_matches('/');
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
    Ok(raw.to_string())
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
        Ok(Self { id: ForgeId::new(kind, &host), kind, base_url, api_url: None, flavor, auth, client_id: None, login: None, user_id: None, token_expires_at: None })
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
    pub fn credentials(&self, store: Arc<dyn TokenStore>, helper_program: Option<&Path>) -> Arc<dyn CredentialSource> {
        self.credentials_with_helper(store, helper_program.map(|p| crate::tokens::store_helper_command(p, &self.id)))
    }

    /// [`Connection::credentials`] with a ready-made helper command (the
    /// part after `!`), for hosts whose helper takes extra arguments.
    pub fn credentials_with_helper(&self, store: Arc<dyn TokenStore>, helper: Option<String>) -> Arc<dyn CredentialSource> {
        match self.auth {
            AuthSource::Gh => Arc::new(GhCredentials::new(self.host())),
            AuthSource::None => Arc::new(Anonymous),
            AuthSource::Token | AuthSource::Oauth => {
                let mut c = StoreCredentials::new(store, self.id.clone(), self.auth, helper);
                if self.auth == AuthSource::Oauth {
                    *c.expires_at.get_mut().unwrap_or_else(|e| e.into_inner()) = self.token_expires_at;
                    if let Ok(ep) = crate::device::endpoints(self) {
                        c.refresh = Some(RefreshConfig { token_url: ep.token_url, client_id: ep.client_id });
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
            Ok(t) => toml::from_str(&t).unwrap_or_default(),
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
}

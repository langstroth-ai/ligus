//! `host.toml`: what this host declares about itself (workspace model v3,
//! 5.1 and 13). Device-local, never synced.
//!
//! ```toml
//! # Don't list this computer's tools (a person's own computer only;
//! # workers and boxes always list theirs).
//! list_tools = false
//!
//! # Local model servers: the address stays here, the workspace sees the name.
//! [[local]]
//! name = "ollama"
//! harness = "ollama"          # ollama | openai
//! url = "http://127.0.0.1:11434"
//!
//! # The secret names this host resolves for one workspace (its id), and the
//! # hosted providers they back.
//! [workspaces."6f1c0c4e-…"]
//! secrets = ["team-claude"]
//! [[workspaces."6f1c0c4e-…".providers]]
//! name = "team-claude"
//! harness = "anthropic"       # anthropic | openAI | openRouter
//! secret = "team-claude"
//! ```
//!
//! **A host resolves only secret names it declared** for the workspace a
//! turn is in ([`HostConfig::declares_secret`]). A string on the log never
//! builds an environment variable or secret id by itself: before, any Admin
//! could point a runtime at a secret meant for another workspace on a shared
//! worker. The gateway allow-list stays in its own file
//! (`crate::model_gateway`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The file's name in the data dir.
pub const HOST_CONFIG_FILE: &str = "host.toml";

fn default_true() -> bool {
    true
}

/// The parsed `host.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    /// List this computer's tools on its host record. Only a person's own
    /// computers honour `false`; workers and boxes always list.
    #[serde(default = "default_true")]
    pub list_tools: bool,
    /// Local model servers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local: Vec<LocalSource>,
    /// Per-workspace declarations, keyed by workspace id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub workspaces: BTreeMap<String, WorkspaceDecl>,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self { list_tools: true, local: Vec::new(), workspaces: BTreeMap::new() }
    }
}

/// A local model server this host runs or reaches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalSource {
    pub name: String,
    /// `ollama` or `openai` (an OpenAI-compatible server).
    #[serde(default = "default_ollama")]
    pub harness: String,
    pub url: String,
}

fn default_ollama() -> String {
    "ollama".into()
}

/// What this host declares for one workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceDecl {
    /// Secret names this host resolves for the workspace.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,
    /// Hosted providers backed by those secrets, offered by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<ProviderDecl>,
}

/// A hosted provider offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderDecl {
    pub name: String,
    /// `anthropic`, `openAI` or `openRouter`.
    pub harness: String,
    /// The secret it uses (declared for the workspace, or it isn't offered).
    pub secret: String,
}

/// Why `host.toml` couldn't be used.
#[derive(Debug, thiserror::Error)]
pub enum HostConfigError {
    #[error("{path}: {why}")]
    Invalid { path: String, why: String },
    #[error("{path}: {err}")]
    Io { path: String, err: std::io::Error },
}

/// Where `host.toml` lives for `data_dir`.
pub fn path_in(data_dir: &Path) -> PathBuf {
    data_dir.join(HOST_CONFIG_FILE)
}

impl HostConfig {
    /// Parse `text` (no file: [`HostConfig::default`]).
    pub fn parse(text: &str) -> Result<Self, String> {
        let cfg: HostConfig = toml::from_str(text).map_err(|e| e.to_string())?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), String> {
        let name_ok = ligus_core::HostModelGateway::valid_name;
        for l in &self.local {
            if !name_ok(&l.name) {
                return Err(format!("local source name '{}' must be lowercase letters, digits and '-'", l.name));
            }
            if !matches!(l.harness.as_str(), "ollama" | "openai") {
                return Err(format!("local source '{}': harness must be ollama or openai", l.name));
            }
            if l.url.trim().is_empty() {
                return Err(format!("local source '{}' needs a url", l.name));
            }
        }
        for (ws, d) in &self.workspaces {
            if Uuid::parse_str(ws).is_err() {
                return Err(format!("[workspaces.\"{ws}\"]: the key must be a workspace id (ligus status shows it)"));
            }
            for p in &d.providers {
                if !name_ok(&p.name) {
                    return Err(format!("provider name '{}' must be lowercase letters, digits and '-'", p.name));
                }
                if !matches!(p.harness.as_str(), "anthropic" | "openAI" | "openRouter") {
                    return Err(format!("provider '{}': harness must be anthropic, openAI or openRouter", p.name));
                }
            }
        }
        Ok(())
    }

    /// Read `host.toml` from `data_dir` (missing: the default).
    pub fn load(data_dir: &Path) -> Result<Self, HostConfigError> {
        let path = path_in(data_dir);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).map_err(|why| HostConfigError::Invalid { path: path.display().to_string(), why }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(HostConfigError::Io { path: path.display().to_string(), err }),
        }
    }

    /// Write this config to `data_dir/host.toml` atomically, owner-only.
    pub fn save(&self, data_dir: &Path) -> Result<(), HostConfigError> {
        let path = path_in(data_dir);
        let io = |err| HostConfigError::Io { path: path.display().to_string(), err };
        let body = toml::to_string_pretty(self).map_err(|e| HostConfigError::Invalid { path: path.display().to_string(), why: e.to_string() })?;
        let text = format!("# This host's own declarations (never synced). See `ligus help hosts`.\n{body}");
        std::fs::create_dir_all(data_dir).map_err(io)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, &path).map_err(io)
    }

    /// Whether this host resolves secret `name` for `workspace_id`.
    pub fn declares_secret(&self, workspace_id: Uuid, name: &str) -> bool {
        !name.is_empty()
            && self
                .workspaces
                .get(&workspace_id.to_string())
                .is_some_and(|d| d.secrets.iter().any(|s| s == name))
    }

    /// The secret a runtime's secret reference `name` resolves to here: the
    /// name itself when declared for `workspace_id`, or, for the name of a
    /// hosted provider this host offers there, that provider's declared
    /// secret (an agent picked "team-claude on box-1" by the offer's name).
    /// `None` for anything this host didn't declare.
    pub fn secret_for<'a>(&'a self, workspace_id: Uuid, name: &'a str) -> Option<&'a str> {
        if self.declares_secret(workspace_id, name) {
            return Some(name);
        }
        self.providers_in(workspace_id).into_iter().find(|p| p.name == name).map(|p| p.secret.as_str())
    }

    /// The hosted providers offered in `workspace_id`: those whose secret is
    /// declared there.
    pub fn providers_in(&self, workspace_id: Uuid) -> Vec<&ProviderDecl> {
        let Some(d) = self.workspaces.get(&workspace_id.to_string()) else { return Vec::new() };
        d.providers.iter().filter(|p| d.secrets.contains(&p.secret)).collect()
    }

    /// A local source by name.
    pub fn local_source(&self, name: &str) -> Option<&LocalSource> {
        self.local.iter().find(|l| l.name == name)
    }
}

/// The secret names provisioned in this process's environment
/// (`LIGUS_WS_SECRET_<name>`, including files from `LIGUS_WS_SECRET_DIR`
/// once loaded). Names only.
pub fn env_secret_names() -> Vec<String> {
    let mut v: Vec<String> = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .filter_map(|k| k.strip_prefix("LIGUS_WS_SECRET_").map(str::to_string))
        .filter(|n| !n.is_empty() && n != "DIR")
        .collect();
    v.sort();
    v.dedup();
    v
}

/// The one-time move to declared secrets for a box or worker that serves
/// `workspace_id`: when it has no `host.toml` yet, write one declaring the
/// secret names already provisioned in its environment for that workspace,
/// so an upgrade keeps working. Later secrets are declared by hand. Returns
/// whether it wrote.
pub fn seed_declarations(data_dir: &Path, workspace_id: Uuid, names: &[String]) -> Result<bool, HostConfigError> {
    if path_in(data_dir).exists() || names.is_empty() {
        return Ok(false);
    }
    let mut cfg = HostConfig::default();
    cfg.workspaces.insert(workspace_id.to_string(), WorkspaceDecl { secrets: names.to_vec(), providers: Vec::new() });
    cfg.save(data_dir)?;
    Ok(true)
}

/// Set "Don't list this computer's tools" (`list_tools = !hide`) in
/// `data_dir/host.toml`, keeping the rest.
pub fn set_list_tools(data_dir: &Path, list: bool) -> Result<HostConfig, HostConfigError> {
    let mut cfg = HostConfig::load(data_dir)?;
    cfg.list_tools = list;
    cfg.save(data_dir)?;
    Ok(cfg)
}

fn slot() -> &'static RwLock<Arc<HostConfig>> {
    static SLOT: OnceLock<RwLock<Arc<HostConfig>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(Arc::new(HostConfig::default())))
}

/// This host's `host.toml` (the default until [`init_host`]).
pub fn host() -> Arc<HostConfig> {
    slot().read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Install `cfg` as this host's (also tests).
pub fn install(cfg: HostConfig) {
    *slot().write().unwrap_or_else(|e| e.into_inner()) = Arc::new(cfg);
}

/// Load and install `data_dir/host.toml`. An invalid file is an error: the
/// host must not guess what its operator declared.
pub fn init_host(data_dir: &Path) -> Result<Arc<HostConfig>, HostConfigError> {
    let cfg = HostConfig::load(data_dir)?;
    install(cfg);
    Ok(host())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_resolves_only_declared_names() {
        let ws = Uuid::new_v4();
        let other = Uuid::new_v4();
        let text = format!(
            "list_tools = false\n[[local]]\nname = \"ollama\"\nurl = \"http://127.0.0.1:11434\"\n\
             [workspaces.\"{ws}\"]\nsecrets = [\"team-claude\"]\n\
             [[workspaces.\"{ws}\".providers]]\nname = \"team-claude\"\nharness = \"anthropic\"\nsecret = \"team-claude\"\n\
             [[workspaces.\"{ws}\".providers]]\nname = \"undeclared\"\nharness = \"anthropic\"\nsecret = \"nope\"\n"
        );
        let cfg = HostConfig::parse(&text).unwrap();
        assert!(!cfg.list_tools);
        assert_eq!(cfg.local[0].harness, "ollama");
        assert!(cfg.declares_secret(ws, "team-claude"));
        assert!(!cfg.declares_secret(other, "team-claude"), "declared for one workspace only");
        assert!(!cfg.declares_secret(ws, "nope"));
        assert!(!cfg.declares_secret(ws, ""));
        // A provider offer's name resolves to its declared secret; nothing
        // else does.
        assert_eq!(cfg.secret_for(ws, "team-claude"), Some("team-claude"));
        assert_eq!(cfg.secret_for(ws, "nope"), None);
        assert_eq!(cfg.secret_for(other, "team-claude"), None);
        assert_eq!(cfg.secret_for(ws, "undeclared"), None, "a provider whose secret isn't declared");
        let text = format!(
            "[workspaces.\"{ws}\"]\nsecrets = [\"anthropic-key\"]\n\
             [[workspaces.\"{ws}\".providers]]\nname = \"team\"\nharness = \"anthropic\"\nsecret = \"anthropic-key\"\n"
        );
        let named = HostConfig::parse(&text).unwrap();
        assert_eq!(named.secret_for(ws, "team"), Some("anthropic-key"), "the offer's name names its secret");
        assert_eq!(named.secret_for(ws, "anthropic-key"), Some("anthropic-key"));
        let names: Vec<_> = cfg.providers_in(ws).iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["team-claude"], "a provider whose secret isn't declared isn't offered");
        assert!(HostConfig::default().list_tools);
    }

    #[test]
    fn refuses_bad_files() {
        assert!(HostConfig::parse("bogus = 1").is_err());
        assert!(HostConfig::parse("[[local]]\nname = \"Big Name\"\nurl = \"x\"").is_err());
        assert!(HostConfig::parse("[workspaces.\"not-a-uuid\"]\nsecrets = []").is_err());
    }

    #[test]
    fn seeds_once_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Uuid::new_v4();
        assert!(seed_declarations(dir.path(), ws, &["acme".into()]).unwrap());
        assert!(!seed_declarations(dir.path(), ws, &["other".into()]).unwrap(), "never over an existing file");
        let cfg = HostConfig::load(dir.path()).unwrap();
        assert!(cfg.declares_secret(ws, "acme"));
        assert!(!cfg.declares_secret(ws, "other"));
        let cfg = set_list_tools(dir.path(), false).unwrap();
        assert!(!cfg.list_tools);
        assert!(HostConfig::load(dir.path()).unwrap().declares_secret(ws, "acme"), "the rest is kept");
        let empty = tempfile::tempdir().unwrap();
        assert!(!seed_declarations(empty.path(), ws, &[]).unwrap(), "nothing to declare: no file");
        assert_eq!(HostConfig::load(empty.path()).unwrap(), HostConfig::default());
    }
}

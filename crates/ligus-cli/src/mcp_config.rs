//! Headless MCP config — the CLI's equivalent of the app's device-local
//! `mcp_servers` table.
//!
//! The desktop app configures MCP servers through a UI that writes an
//! `mcp_servers` table into `ligus.config.toml` and gates each one behind an
//! explicit *enable* toggle. A headless worker has no UI, so it reads the same
//! table shape from a file named by `LIGUS_MCP_CONFIG`. Presence in that file
//! **is** the opt-in (an operator wrote and provisioned it), so servers default
//! to `enabled = true` here — set `enabled = false` to keep one inert.
//!
//! Secrets never live in the TOML: an HTTP/SSE server's bearer token is named
//! by `token_env` and resolved from the worker's environment (the same
//! out-of-band convention as `LIGUS_WS_SECRET_*`). A stdio server's child process
//! inherits the worker's environment, so its provider token (e.g.
//! `GITHUB_PERSONAL_ACCESS_TOKEN`) is provisioned there.
//!
//! `ligus setup tool` keeps its servers in its own file, `<data dir>/`
//! [`MANAGED_FILE`] (owner-only, written atomically, never the operator's
//! file), read alongside the operator's; on an id in both the operator's
//! wins. A token it saved lives in the secret store (`token_secret = true`
//! names it), and which agents may reach a server is device-local
//! ([`ligus_runtime::McpReach`]): a malformed reach file fails the load
//! instead of widening it.

use anyhow::{Context, Result};
use ligus_runtime::{McpServerSpec, McpTransport};
use serde::Deserialize;

/// The env var naming the headless MCP config TOML file.
pub const CONFIG_ENV: &str = "LIGUS_MCP_CONFIG";

/// The file `ligus setup tool` writes, in the data dir.
pub const MANAGED_FILE: &str = "mcp-servers.toml";

/// The secret-store scope and name of a server's saved token.
pub const TOKEN_SCOPE: &str = "cli";
pub fn token_secret_id(server: &str) -> ligus_runtime::secret_store::SecretId {
    ligus_runtime::secret_store::SecretId::new(TOKEN_SCOPE, format!("mcp-token:{server}"))
}

/// Where a saved token may be sent: the origin it was saved for, kept in
/// the secret store beside it (so editing the managed file's URL can't
/// redirect it).
pub fn token_origin_id(server: &str) -> ligus_runtime::secret_store::SecretId {
    ligus_runtime::secret_store::SecretId::new(TOKEN_SCOPE, format!("mcp-token-origin:{server}"))
}

/// Save `server`'s bearer token, bound to `url`'s origin.
pub fn save_token(
    store: &dyn ligus_runtime::secret_store::SecretStore,
    server: &str,
    url: &str,
    token: &str,
) -> Result<()> {
    let origin = ligus_runtime::setup::validate::origin(url);
    store
        .put(&token_origin_id(server), origin.as_bytes())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    store
        .put(&token_secret_id(server), token.as_bytes())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

/// Forget `server`'s token and its origin.
pub fn delete_token(
    store: &dyn ligus_runtime::secret_store::SecretStore,
    server: &str,
) -> Result<()> {
    store
        .delete(&token_secret_id(server))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    store
        .delete(&token_origin_id(server))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

/// `server`'s saved token, only when `url` has the origin it was saved for.
pub fn token_for(
    store: &dyn ligus_runtime::secret_store::SecretStore,
    server: &str,
    url: &str,
) -> Option<String> {
    let saved = store.get_string(&token_origin_id(server)).ok().flatten()?;
    if saved.as_str() != ligus_runtime::setup::validate::origin(url) {
        return None;
    }
    store
        .get_string(&token_secret_id(server))
        .ok()
        .flatten()
        .map(|z| z.as_str().to_string())
}

/// This process's data dir and workspace (set once at start-up), for the
/// managed file, saved tokens and reach.
static CONTEXT: std::sync::OnceLock<(std::path::PathBuf, uuid::Uuid)> = std::sync::OnceLock::new();

pub fn set_context(data_dir: &std::path::Path, workspace: uuid::Uuid) {
    let _ = CONTEXT.set((data_dir.to_path_buf(), workspace));
}

/// The managed file's path in `data_dir`.
pub fn managed_path(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join(MANAGED_FILE)
}

#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    #[serde(default)]
    mcp_servers: Vec<RawMcpServer>,
}

/// Mirrors the app's `mcp_servers` entry (`id` + `transport` + stdio
/// `command`/`args` or http `url`), plus a headless-only `token_env` for
/// resolving a bearer token from the environment.
#[derive(Debug, Default, Deserialize)]
struct RawMcpServer {
    id: String,
    #[serde(default = "default_transport_stdio")]
    transport: String,
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    url: Option<String>,
    /// Name of the env var holding this server's bearer token (http/sse). The
    /// token itself is never written to the TOML.
    token_env: Option<String>,
    /// Servers default to enabled in a headless config — the file is the opt-in.
    #[serde(default = "default_true")]
    enabled: bool,
    /// The token is in this device's secret store ([`token_secret_id`]).
    #[serde(default)]
    token_secret: bool,
}

fn default_transport_stdio() -> String {
    "stdio".to_string()
}

fn default_true() -> bool {
    true
}

/// Load the MCP servers named by `LIGUS_MCP_CONFIG`. Unset or empty → no servers
/// (an empty vec), so the caller behaves exactly as it did before MCP wiring.
pub fn load_from_env() -> Result<Vec<McpServerSpec>> {
    let mut specs = match ligus_core::names::var(CONFIG_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        Some(path) => {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {CONFIG_ENV}={path}"))?;
            specs_from_str(&text).with_context(|| format!("parsing {CONFIG_ENV}={path}"))?
        }
        None => Vec::new(),
    };
    if let Some((dir, ws)) = CONTEXT.get() {
        for m in load_managed(dir)? {
            if !specs.iter().any(|s| s.id == m.id) {
                specs.push(m);
            }
        }
        let reach =
            ligus_runtime::McpReach::load(dir).map_err(|e| anyhow::anyhow!("MCP reach: {e}"))?;
        reach.apply(*ws, &mut specs);
    }
    Ok(specs)
}

/// The servers `ligus setup tool` saved in `data_dir`, tokens resolved.
pub fn load_managed(data_dir: &std::path::Path) -> Result<Vec<McpServerSpec>> {
    let path = managed_path(data_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let raw: RawConfig =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let store = ligus_runtime::secret_store::for_data_dir(data_dir);
    Ok(raw
        .mcp_servers
        .into_iter()
        .map(|m| {
            // Only to the origin the token was saved for.
            let saved = match (&m.url, m.token_secret) {
                (Some(url), true) => token_for(store.as_ref(), &m.id, url),
                _ => None,
            };
            let mut spec = spec_from_raw(m);
            if saved.is_some() {
                spec.auth = saved;
            }
            spec
        })
        .collect())
}

/// One entry of the managed file, as `ligus setup tool` writes it.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct ManagedEntry {
    pub id: String,
    pub transport: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub token_secret: bool,
}

#[derive(Debug, Default, serde::Serialize, Deserialize)]
struct ManagedFile {
    #[serde(default)]
    mcp_servers: Vec<ManagedEntry>,
}

/// The managed file's entries (no tokens: none are in it).
pub fn managed_entries(data_dir: &std::path::Path) -> Result<Vec<ManagedEntry>> {
    let path = managed_path(data_dir);
    match std::fs::read_to_string(&path) {
        Ok(t) => Ok(toml::from_str::<ManagedFile>(&t)
            .with_context(|| format!("parsing {}", path.display()))?
            .mcp_servers),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Replace the managed file's entries (owner-only, atomically).
pub fn save_managed(data_dir: &std::path::Path, entries: Vec<ManagedEntry>) -> Result<()> {
    let text = toml::to_string_pretty(&ManagedFile {
        mcp_servers: entries,
    })?;
    ligus_runtime::private_file::write_private_atomic(&managed_path(data_dir), text.as_bytes())?;
    Ok(())
}

/// Parse an MCP config TOML into [`McpServerSpec`]s, resolving each http/sse
/// server's bearer token from its `token_env` environment variable.
pub fn specs_from_str(text: &str) -> Result<Vec<McpServerSpec>> {
    let raw: RawConfig = toml::from_str(text)?;
    Ok(raw.mcp_servers.into_iter().map(spec_from_raw).collect())
}

fn spec_from_raw(m: RawMcpServer) -> McpServerSpec {
    let is_http = matches!(m.transport.to_lowercase().as_str(), "http" | "sse");
    let transport = if is_http {
        McpTransport::Http {
            url: m.url.unwrap_or_default(),
        }
    } else {
        McpTransport::Stdio {
            command: m.command.unwrap_or_default(),
            args: m.args,
        }
    };
    // Resolve a bearer token from the environment (never from the file).
    let auth = m.token_env.as_deref().and_then(|var| {
        match std::env::var(var).ok().filter(|s| !s.trim().is_empty()) {
            Some(tok) => Some(tok),
            None => {
                eprintln!(
                    "mcp: server '{}' token_env '{var}' is not set — connecting without auth",
                    m.id
                );
                None
            }
        }
    });
    McpServerSpec {
        id: m.id,
        transport,
        enabled: m.enabled,
        auth,
        agents: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A saved token goes only to the origin it was saved for: editing the
    /// managed file's URL, or a new server reusing the id, gets nothing.
    #[test]
    fn a_token_is_bound_to_its_origin() {
        let d = tempfile::tempdir().unwrap();
        let store = ligus_runtime::secret_store::File0600::new(d.path());
        save_token(&store, "gh", "https://mcp.example.com/v1", "tok-canary").unwrap();
        assert_eq!(
            token_for(&store, "gh", "https://mcp.example.com/other").as_deref(),
            Some("tok-canary")
        );
        assert_eq!(token_for(&store, "gh", "https://evil.example.com/v1"), None);
        assert_eq!(
            token_for(&store, "gh", "http://mcp.example.com/v1"),
            None,
            "scheme is part of the origin"
        );
        delete_token(&store, "gh").unwrap();
        assert_eq!(token_for(&store, "gh", "https://mcp.example.com/v1"), None);
    }

    #[test]
    fn empty_or_missing_config_yields_no_servers() {
        // No `mcp_servers` table at all → empty.
        assert!(specs_from_str("").unwrap().is_empty());
        assert!(specs_from_str("# just a comment\n").unwrap().is_empty());
    }

    #[test]
    fn parses_stdio_and_http_servers() {
        std::env::set_var("LIGUS_MCP_TEST_TOKEN", "secret-abc");
        let toml = r#"
[[mcp_servers]]
id = "fs"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/data"]

[[mcp_servers]]
id = "linear"
transport = "http"
url = "https://mcp.linear.app/mcp"
token_env = "LIGUS_MCP_TEST_TOKEN"
"#;
        let specs = specs_from_str(toml).unwrap();
        assert_eq!(specs.len(), 2);

        // stdio server: command + args, no auth, enabled by default (headless).
        assert_eq!(specs[0].id, "fs");
        assert!(specs[0].enabled, "headless servers default to enabled");
        assert_eq!(specs[0].auth, None);
        assert_eq!(
            specs[0].transport,
            McpTransport::Stdio {
                command: "npx".into(),
                args: vec![
                    "-y".into(),
                    "@modelcontextprotocol/server-filesystem".into(),
                    "/data".into(),
                ],
            }
        );

        // http server: url + bearer token resolved from token_env.
        assert_eq!(specs[1].id, "linear");
        assert_eq!(
            specs[1].transport,
            McpTransport::Http {
                url: "https://mcp.linear.app/mcp".into()
            }
        );
        assert_eq!(specs[1].auth.as_deref(), Some("secret-abc"));

        std::env::remove_var("LIGUS_MCP_TEST_TOKEN");
    }

    #[test]
    fn sse_maps_to_http_and_disabled_is_respected() {
        let toml = r#"
[[mcp_servers]]
id = "remote"
transport = "sse"
url = "https://example.com/sse"
enabled = false
"#;
        let specs = specs_from_str(toml).unwrap();
        assert_eq!(specs.len(), 1);
        assert!(!specs[0].enabled, "explicit enabled=false is honored");
        assert!(matches!(specs[0].transport, McpTransport::Http { .. }));
    }
}

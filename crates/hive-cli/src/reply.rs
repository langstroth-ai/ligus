//! `ligus agent`'s reply, and the headless hosts' tool settings.
//!
//! `ligus agent` turns a chat transcript into one reply. When
//! `LIGUS_MCP_CONFIG` names one or more enabled MCP servers *and* the runtime
//! is Anthropic with a key, the reply is produced by the agentic tool loop
//! (the turn engine's adapter and MCP executor); otherwise it falls back to a
//! single streamed completion.
//!
//! `ligus worker` / `ligus serve` run their turns through
//! `hive_runtime::turn_engine`, the same engine as the desktop; this module
//! only reads their [`ToolSettings`] from the environment.

use anyhow::{anyhow, Result};
use hive_core::{ModelProviderKind, WorkspaceRole};
use hive_runtime::tool_loop;
use hive_runtime::{dispatch, AnthropicClient, ChatTurn, ResolvedRuntime};

use crate::mcp_config;

const MAX_TOKENS: u32 = 1024;
const MAX_TOOL_ITERS: usize = 6;

/// The configured minimum author role for a turn of this tier
/// (`LIGUS_MCP_MIN_ROLE`, else Admin when write-capable and Contributor when
/// read-only; see [`hive_runtime::author_gate`]).
pub fn mcp_min_role(write_capable: bool) -> WorkspaceRole {
    hive_runtime::author_gate::min_role(write_capable)
}

/// Produce one reply for `turns` under `system`. Runs the MCP tool loop when an
/// Anthropic runtime has a key, `LIGUS_MCP_CONFIG` yields ≥1 enabled tool, **and**
/// `allow_mcp_tools` is set (the triggering author cleared the role gate);
/// otherwise streams a single completion (the original, tool-free behavior).
pub async fn generate_reply(
    rt: &ResolvedRuntime,
    system: &str,
    turns: &[ChatTurn],
    allow_mcp_tools: bool,
    working_dir: Option<&str>,
) -> Result<String> {
    if allow_mcp_tools {
        if let Some(text) = try_tool_loop(rt, system, turns, &hive_core::sandbox::EgressPolicy::Open).await? {
            return Ok(text);
        }
    }
    // working_dir is the subprocess agent's cwd — an isolated worktree when the
    // worker is isolating this turn, else the plain workspace root.
    // The CLI is headless — it prints the final reply, so tool/thinking activity
    // (the second callback) is dropped; only the desktop UI renders it live.
    dispatch::stream(rt, Some(system), turns, working_dir, &[], MAX_TOKENS, |_| {}, |_| {})
        .await
        .map_err(|e| anyhow!("provider: {e}"))
}

/// What a headless turn's tools may do — the CLI's equivalent of the app's
/// tool settings, read from the environment with the app's defaults:
///
/// - `LIGUS_BUILTIN_TOOLS` (default on): offer Hive's built-in file tools to
///   runtimes that run the agent tool loop (Custom / HiveDaemon / Ollama with
///   tools on).
/// - `LIGUS_BUILTIN_RUN_COMMAND` (default off): also offer `run_command`
///   (only ever inside the turn's isolated worktree).
/// - `LIGUS_ALLOW_REMOTE_FILE_TOOLS` (default off): offer the file tools even
///   when the model endpoint isn't on this machine / a private network /
///   the tailnet — they send file contents to it.
/// - `LIGUS_MCP_AFTER_READS` (default off): turn off the exfiltration guard
///   (MCP calls refused once a turn has read project files).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSettings {
    pub builtin_tools: bool,
    pub run_command: bool,
    pub allow_remote_file_tools: bool,
    pub mcp_after_reads: bool,
    /// The turn's sandbox-envelope egress policy: which MCP servers it may
    /// reach (open by default; narrowed per turn by dearth mode or a lockdown).
    pub mcp_egress: hive_core::sandbox::EgressPolicy,
    /// The envelope's protected directories: writes never land in them.
    pub protected_dirs: Vec<String>,
}

impl Default for ToolSettings {
    fn default() -> Self {
        Self {
            builtin_tools: true,
            run_command: false,
            allow_remote_file_tools: false,
            mcp_after_reads: false,
            mcp_egress: hive_core::sandbox::EgressPolicy::Open,
            protected_dirs: hive_core::sandbox::PROTECTED_DIRS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

pub fn env_flag(name: &str, default: bool) -> bool {
    match hive_core::compat::var(name).ok().map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if matches!(v.as_str(), "1" | "true" | "on" | "yes") => true,
        Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no") => false,
        _ => default,
    }
}

impl ToolSettings {
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            builtin_tools: env_flag("LIGUS_BUILTIN_TOOLS", d.builtin_tools),
            run_command: env_flag("LIGUS_BUILTIN_RUN_COMMAND", d.run_command),
            allow_remote_file_tools: env_flag("LIGUS_ALLOW_REMOTE_FILE_TOOLS", d.allow_remote_file_tools),
            mcp_after_reads: env_flag("LIGUS_MCP_AFTER_READS", d.mcp_after_reads),
            mcp_egress: d.mcp_egress,
            protected_dirs: d.protected_dirs,
        }
    }
}

/// Whether a headless HTTP model runtime gets the agent tool loop at all.
/// Workspace runtimes carry no "Supports tools" switch (it is device-local in
/// the app), so a worker opts its local-capable runtimes in unless
/// `LIGUS_WORKER_TOOLS=0`. Hosted APIs never get it (see
/// [`ResolvedRuntime::runs_agent_tools`]).
pub fn worker_tools_enabled() -> bool {
    env_flag("LIGUS_WORKER_TOOLS", true)
}

/// The app's `wants_builtin_tools`: the setting is on, the runtime runs the
/// agent tool loop, and the endpoint is private (loopback, RFC 1918,
/// Tailscale, …) unless the operator opted into remote file tools.
#[cfg(test)]
pub fn wants_builtin_tools(rt: &ResolvedRuntime, settings: &ToolSettings) -> bool {
    let tools = hive_runtime::turn_engine::ToolOptions {
        builtin: settings.builtin_tools,
        remote_file_tools: settings.allow_remote_file_tools,
        ..Default::default()
    };
    hive_runtime::turn_engine::wants_builtin_tools(rt, &tools)
}

/// Attempt the MCP tool loop. `Ok(None)` = no tools / unsupported runtime →
/// caller falls back to streaming. The turn engine's pieces, without a chat.
async fn try_tool_loop(
    rt: &ResolvedRuntime,
    system: &str,
    turns: &[ChatTurn],
    egress: &hive_core::sandbox::EgressPolicy,
) -> Result<Option<String>> {
    use hive_runtime::turn_engine::mcp;
    // The tool-loop adapter is Anthropic-only (as in the app), and needs a key.
    if rt.provider != ModelProviderKind::Anthropic {
        return Ok(None);
    }
    let Some(api_key) = rt.api_key.clone() else {
        return Ok(None);
    };
    // No config / no enabled servers → fall back to streaming. Servers
    // outside the envelope's egress policy are never offered, and without
    // the shell no tool that runs shell commands is.
    let specs: Vec<_> = mcp_config::load_from_env()?.into_iter().filter(|s| egress.allows_mcp(&s.id)).collect();
    let shell = hive_runtime::executor::turn_shell_allowed();
    let Some((defs, executor)) = mcp::tool_set(specs, shell, None, |_| {}).await else {
        return Ok(None);
    };
    let model = mcp::AnthropicMessages {
        client: AnthropicClient::new(),
        api_key,
        model: rt.model.clone(),
        system: Some(system.to_string()),
        max_tokens: MAX_TOKENS,
    };
    let text = tool_loop::run_with_messages(&model, &executor, mcp::turns_to_messages(turns), defs, MAX_TOOL_ITERS)
        .await
        .map_err(|e| anyhow!("tool loop: {e}"))?;
    Ok(Some(text))
}

#[cfg(test)]
mod tests {
    use hive_core::WorkspaceRole::{self, *};

    fn parse_min_role(v: Option<&str>, write_capable: bool) -> WorkspaceRole {
        hive_runtime::author_gate::min_role_from(v, write_capable)
    }
    fn mcp_allowed_for(author_role: Option<WorkspaceRole>, min_role: WorkspaceRole) -> bool {
        hive_runtime::author_gate::gate(hive_core::ModelProviderKind::Ollama, author_role, min_role).allows_tools()
    }

    #[test]
    fn viewer_denied_at_default() {
        let min = parse_min_role(None, false);
        assert_eq!(min, Contributor);
        assert!(!mcp_allowed_for(Some(Viewer), min));
    }

    #[test]
    fn contributor_allowed_at_the_read_only_default() {
        assert!(mcp_allowed_for(Some(Contributor), parse_min_role(None, false)));
        assert!(mcp_allowed_for(Some(Admin), parse_min_role(None, false)));
        assert!(mcp_allowed_for(Some(Owner), parse_min_role(None, false)));
    }

    /// S3: a write-capable worker defaults to an Admin floor.
    #[test]
    fn worker_default_floor_is_admin_on_write_tiers() {
        let min = parse_min_role(None, true);
        assert_eq!(min, Admin);
        assert!(!mcp_allowed_for(Some(Contributor), min));
        assert!(mcp_allowed_for(Some(Admin), min));
        assert_eq!(parse_min_role(Some("superuser"), true), Admin);
    }

    #[test]
    fn raised_min_role_denies_contributor() {
        let min = parse_min_role(Some("admin"), false);
        assert_eq!(min, Admin);
        assert!(!mcp_allowed_for(Some(Contributor), min));
        assert!(mcp_allowed_for(Some(Admin), min));
    }

    #[test]
    fn env_parse_is_case_insensitive_and_trims() {
        assert_eq!(parse_min_role(Some("  Owner "), false), Owner);
        assert_eq!(parse_min_role(Some("VIEWER"), true), Viewer);
        assert_eq!(parse_min_role(Some("Contributor"), true), Contributor);
    }

    #[test]
    fn unknown_or_absent_env_defaults_to_the_tier_default() {
        assert_eq!(parse_min_role(None, false), Contributor);
        assert_eq!(parse_min_role(Some(""), false), Contributor);
        assert_eq!(parse_min_role(Some(""), true), Admin);
    }

    #[test]
    fn unresolved_author_is_denied() {
        assert!(!mcp_allowed_for(None, parse_min_role(None, false)));
        assert!(!mcp_allowed_for(None, parse_min_role(Some("viewer"), false)));
    }
}

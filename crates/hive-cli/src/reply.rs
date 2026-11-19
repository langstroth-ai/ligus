//! Generating an agent's reply — with MCP tools when configured.
//!
//! Both `ligus agent` and `ligus worker` turn a chat transcript into one reply.
//! When `LIGUS_MCP_CONFIG` names one or more enabled MCP servers *and* the
//! runtime is Anthropic with a key, the reply is produced by the agentic tool
//! loop (model ↔ tool_use ↔ tool_result), so the agent can reach external
//! boards (Linear/GitHub) via MCP tools. Otherwise it falls back to a single
//! streamed completion — identical to the pre-MCP behavior — so nothing
//! regresses when no MCP config is present.
//!
//! `ligus worker` / `ligus serve` use [`generate_reply_streaming`], which adds the
//! offline harness's loop for local model runtimes (OpenAI wire / Ollama):
//! built-in file tools + MCP tools behind `CompositeTools`, under the same
//! policy as the desktop (writes only in a per-turn worktree, `run_command`
//! opt-in, private-endpoint gating, exfiltration guard).
//!
//! Mirrors the app's `try_tool_loop`: an Anthropic `MessagesApi` adapter + an
//! `McpRegistry`-backed `ToolExecutor`, driven by `run_with_messages`. Runs
//! entirely within the caller's task (the CLI's current-thread runtime), so the
//! non-`Send` registry/executor never cross tasks.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use hive_core::{ModelProviderKind, WorkspaceRole};
use hive_runtime::provider::anthropic::{content_value, AnthropicResponse};
use hive_runtime::tool_loop::{self, MessagesApi, ToolExecutor};
use hive_runtime::{dispatch, AnthropicClient, ChatTurn, McpRegistry, ProviderError, ResolvedRuntime};
use serde_json::{json, Value};

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

/// Where a tool-using turn may read and write files.
#[derive(Debug, Clone)]
pub struct TurnFiles {
    /// The bound checkout (see `repos.rs`); `None` = no file tools.
    pub repo_root: Option<std::path::PathBuf>,
    pub chat_id: String,
    /// Unique per turn (the reply's message id) — names the worktree.
    pub turn_key: String,
}

/// The endpoint a runtime actually talks to (native Ollama defaults local).
fn effective_endpoint(rt: &ResolvedRuntime) -> String {
    if rt.endpoint.trim().is_empty() && rt.provider == ModelProviderKind::Ollama {
        "http://localhost:11434".into()
    } else {
        rt.endpoint.clone()
    }
}

/// The app's `wants_builtin_tools`: the setting is on, the runtime runs the
/// agent tool loop, and the endpoint is private (loopback, RFC 1918,
/// Tailscale, …) unless the operator opted into remote file tools.
pub fn wants_builtin_tools(rt: &ResolvedRuntime, settings: &ToolSettings) -> bool {
    settings.builtin_tools
        && rt.runs_agent_tools()
        && (settings.allow_remote_file_tools
            || rt.allow_remote_file_tools
            || hive_core::endpoint::endpoint_is_private(&effective_endpoint(rt)))
}

/// The result of one streamed, possibly tool-using turn.
pub struct TurnReply {
    pub text: String,
    /// The per-turn worktree a file-writing turn cut (lazily, on its first
    /// write). The caller captures its diff as a proposal and removes it.
    pub worktree: Option<hive_runtime::git_worktree::Worktree>,
}

/// Produce one reply, streaming fragments to `on_delta`.
///
/// - Anthropic with tools allowed: the (non-streaming) Anthropic MCP tool
///   loop; its whole answer is delivered as one fragment.
/// - An HTTP runtime that runs the agent tool loop (Custom / HiveDaemon /
///   Ollama with tools on): `dispatch::stream_with_tools` with Hive's built-in
///   file tools (when a checkout is bound and the endpoint is private) plus the
///   MCP tools, behind `CompositeTools` — the same pieces and the same policy
///   as the desktop: reads at the chat's base worktree, writes only in a
///   per-turn git worktree cut on the first write, `run_command` only when
///   enabled, the exfiltration guard on.
/// - Anything else: a plain streamed completion.
///
/// `allow_tools` is the H1 role gate (the triggering author's role): when
/// false no tool of either kind is offered.
#[allow(clippy::too_many_arguments)]
pub async fn generate_reply_streaming(
    rt: &ResolvedRuntime,
    system: &str,
    turns: &[ChatTurn],
    allow_tools: bool,
    working_dir: Option<&str>,
    files: Option<&TurnFiles>,
    settings: &ToolSettings,
    mut on_delta: impl FnMut(String),
) -> Result<TurnReply> {
    if allow_tools && rt.provider == ModelProviderKind::Anthropic {
        if let Some(text) = try_tool_loop(rt, system, turns, &settings.mcp_egress).await? {
            on_delta(text.clone());
            return Ok(TurnReply { text, worktree: None });
        }
    }
    if allow_tools && rt.runs_agent_tools() && rt.is_openai_wire() {
        return tool_loop_turn(rt, system, turns, files, settings, on_delta).await;
    }
    let text = dispatch::stream(rt, Some(system), turns, working_dir, &[], MAX_TOKENS, on_delta, |_| {})
        .await
        .map_err(|e| anyhow!("provider: {e}"))?;
    Ok(TurnReply { text, worktree: None })
}

async fn tool_loop_turn(
    rt: &ResolvedRuntime,
    system: &str,
    turns: &[ChatTurn],
    files: Option<&TurnFiles>,
    settings: &ToolSettings,
    on_delta: impl FnMut(String),
) -> Result<TurnReply> {
    use hive_runtime::agent_tools::{builtin_policy, tool_hints, tool_round_budget, CompositeTools, MAX_OFFERED_TOOLS};
    use hive_runtime::git_worktree::{chat_worktree, is_git_repo, LazyTurnWorktree};
    use std::sync::Arc;

    let mcp = mcp_tool_set(&settings.mcp_egress).await?;
    let repo = files
        .and_then(|f| f.repo_root.as_deref().map(|r| (f, r)))
        .filter(|(_, r)| is_git_repo(r));
    let mut lazy: Option<Arc<LazyTurnWorktree>> = None;
    let builtin = match repo {
        // Built-in file tools (and run_command) need this host's OS sandbox:
        // without one an HTTP model gets no local tools (chat only).
        Some((f, root)) if wants_builtin_tools(rt, settings) && hive_runtime::executor::local_tools_allowed() => {
            let wt = Arc::new(LazyTurnWorktree::new(root, f.chat_id.clone(), f.turn_key.clone()));
            // Read the chat's own base worktree (the tree its turn worktree is
            // cut from), so the model edits what it read.
            let read_root = chat_worktree(root, &f.chat_id).unwrap_or_else(|e| {
                eprintln!("worker: chat base worktree unavailable, reading the checkout: {e}");
                root.to_path_buf()
            });
            // isolated = true: writes go to the lazily cut turn worktree, never
            // the checkout (no in-place fallback on a headless host).
            // `run_command` is a shell: the Owner's, or granted (Phase C;
            // the host set it on the turn's policy).
            let shell = hive_runtime::executor::turn_shell_allowed();
            let policy = builtin_policy(&read_root, true, false, settings.run_command && shell).with_protected_dirs(&settings.protected_dirs);
            match hive_runtime::BuiltinTools::lazy(policy, wt.clone(), false) {
                Ok(b) => {
                    lazy = Some(wt);
                    Some(b)
                }
                Err(e) => {
                    eprintln!("worker: built-in tools unavailable: {e}");
                    None
                }
            }
        }
        _ => None,
    };
    // The folded Ligus tools, when the host started a Ligus turn (D.3.3).
    let ligus = hive_runtime::agent_turn::current().and_then(|s| s.ligus.clone());
    let cap = MAX_OFFERED_TOOLS - if ligus.is_some() { hive_runtime::ligus_mcp::tools::FOLDED.len() } else { 0 };
    let tools = CompositeTools::with_hints(builtin, mcp, cap, &tool_hints(system, turns)).allow_mcp_after_reads(settings.mcp_after_reads);
    let tools = match ligus {
        Some(l) => tools.with_ligus_tools(l),
        None => tools,
    };
    dispatch::warm_context_probe(rt).await;
    let rounds = tool_round_budget(rt.provider, rt.max_tool_rounds);
    let set = (!tools.is_empty()).then(|| dispatch::ToolSet { defs: tools.defs(), executor: &tools, max_rounds: rounds });
    let working_dir = repo.map(|(_, r)| r.to_string_lossy().into_owned());
    let result = dispatch::stream_with_tools(
        rt,
        Some(system),
        turns,
        working_dir.as_deref(),
        &[],
        MAX_TOKENS,
        set,
        on_delta,
        |_| {},
    )
    .await;
    let worktree = lazy.as_ref().and_then(|l| l.finish());
    match result {
        Ok(text) => Ok(TurnReply { text, worktree }),
        Err(e) => {
            // A failed turn's partial edits are discarded, like the desktop.
            if let Some(wt) = worktree {
                let _ = wt.remove();
            }
            Err(anyhow!("provider: {e}"))
        }
    }
}

/// The enabled MCP servers' tools (from `LIGUS_MCP_CONFIG`), as neutral defs
/// plus the executor that routes a call back to its server. `None` when no
/// server is configured or none lists a tool.
async fn mcp_tool_set(egress: &hive_core::sandbox::EgressPolicy) -> Result<Option<(Vec<Value>, McpToolExecutor)>> {
    // The sandbox envelope decides which servers this turn may reach at all.
    let specs: Vec<_> = mcp_config::load_from_env()?.into_iter().filter(|s| egress.allows_mcp(&s.id)).collect();
    let registry = McpRegistry::new(specs);
    if registry.enabled().next().is_none() {
        return Ok(None);
    }
    let tagged = registry.list_all_tools().await;
    if tagged.is_empty() {
        return Ok(None);
    }
    let mut defs = Vec::with_capacity(tagged.len());
    let mut names = HashMap::new();
    for (server, tool) in &tagged {
        let tname = sanitize_tool_name(&format!("{server}__{}", tool.name));
        defs.push(json!({ "name": tname, "description": tool.description, "input_schema": tool.input_schema }));
        names.insert(tname, (server.clone(), tool.name.clone()));
    }
    Ok(Some((defs, McpToolExecutor { registry, names })))
}

/// Attempt the MCP tool loop. `Ok(None)` = no tools / unsupported runtime →
/// caller falls back to streaming. Mirrors the app's `try_tool_loop`.
async fn try_tool_loop(
    rt: &ResolvedRuntime,
    system: &str,
    turns: &[ChatTurn],
    egress: &hive_core::sandbox::EgressPolicy,
) -> Result<Option<String>> {
    // The tool-loop adapter is Anthropic-only (as in the app), and needs a key.
    if rt.provider != ModelProviderKind::Anthropic {
        return Ok(None);
    }
    let Some(api_key) = rt.api_key.clone() else {
        return Ok(None);
    };
    // No config / no enabled servers → empty registry → fall back to streaming,
    // unless the host started a Ligus turn (its tools ride this loop).
    // Servers outside the envelope's egress policy are never offered.
    let ligus = hive_runtime::agent_turn::current().and_then(|s| s.ligus.clone());
    let specs: Vec<_> = mcp_config::load_from_env()?.into_iter().filter(|s| egress.allows_mcp(&s.id)).collect();
    let registry = McpRegistry::new(specs);
    let tagged = if registry.enabled().next().is_some() { registry.list_all_tools().await } else { Vec::new() };
    if tagged.is_empty() && ligus.is_none() {
        return Ok(None);
    }

    let mut tool_defs = Vec::with_capacity(tagged.len());
    let mut names = HashMap::new();
    for (server, tool) in &tagged {
        let tname = sanitize_tool_name(&format!("{server}__{}", tool.name));
        tool_defs.push(json!({
            "name": tname,
            "description": tool.description,
            "input_schema": tool.input_schema,
        }));
        names.insert(tname, (server.clone(), tool.name.clone()));
    }

    let model = AnthropicMessages {
        client: AnthropicClient::new(),
        api_key,
        model: rt.model.clone(),
        system: Some(system.to_string()),
    };
    let executor = McpToolExecutor { registry, names };
    if ligus.is_some() {
        let mut defs = hive_runtime::ligus_mcp::tools::tool_loop_defs();
        defs.extend(tool_defs);
        tool_defs = defs;
    }
    let routed = hive_runtime::ligus_mcp::WithLigus { turn: ligus.as_deref(), inner: Some(&executor) };
    let initial = turns_to_messages(turns);
    let text = tool_loop::run_with_messages(&model, &routed, initial, tool_defs, MAX_TOOL_ITERS)
        .await
        .map_err(|e| anyhow!("tool loop: {e}"))?;
    Ok(Some(text))
}

/// Adapter: one non-streaming Anthropic turn for the tool loop.
struct AnthropicMessages {
    client: AnthropicClient,
    api_key: String,
    model: String,
    system: Option<String>,
}

impl MessagesApi for AnthropicMessages {
    async fn run(&self, messages: &[Value], tools: &[Value]) -> Result<AnthropicResponse, ProviderError> {
        self.client
            .run_messages(&self.api_key, &self.model, self.system.as_deref(), messages, tools, MAX_TOKENS)
            .await
    }
}

/// Routes a (sanitized) tool name back to its MCP server, honoring the gate.
struct McpToolExecutor {
    registry: McpRegistry,
    /// sanitized anthropic tool name → (server id, real tool name)
    names: HashMap<String, (String, String)>,
}

impl ToolExecutor for McpToolExecutor {
    async fn call(&self, name: &str, input: &Value) -> (String, bool) {
        match self.names.get(name) {
            Some((server, tool)) => match self.registry.call_tool(server, tool, input).await {
                Ok(text) => (text, false),
                Err(e) => (format!("tool error: {e}"), true),
            },
            None => (format!("unknown tool {name}"), true),
        }
    }
}

/// Anthropic tool names must match `^[a-zA-Z0-9_-]{1,64}$`.
fn sanitize_tool_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    cleaned.chars().take(64).collect()
}

/// Convert provider turns into the Anthropic message array the loop seeds.
fn turns_to_messages(turns: &[ChatTurn]) -> Vec<Value> {
    turns
        .iter()
        .map(|t| json!({ "role": t.role, "content": content_value(&t.content) }))
        .collect()
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

//! Agent Client Protocol (ACP) driver (Track D1): drive a subscription CLI
//! as an ACP agent over JSON-RPC 2.0 on its stdio, spawned through the turn's
//! [`crate::executor`] sandbox like every other agent child.
//!
//! Client → agent: `initialize` (protocol version 1, fs read/write, no
//! terminal), `authenticate` (only for a non-interactive method, once, when
//! `session/new` reports that auth is required), `session/new` (the worktree
//! and the MCP servers, the Ligus server first), `session/prompt`, and
//! `session/cancel`.
//!
//! Agent → client:
//!
//! | Message | Handling |
//! |---|---|
//! | `session/update` `agent_message_chunk` | text delta |
//! | `session/update` `agent_thought_chunk` | thinking activity |
//! | `session/update` `tool_call` / `tool_call_update` / `plan` | activity |
//! | `session/update` `usage_update` | spend (for `maxSpendUsd`) |
//! | `session/request_permission` | the permission tier: allow, deny, or escalate to ask-the-human ([`judge_permission`]) |
//! | `fs/read_text_file`, `fs/write_text_file` | served by Ligus inside the worktree only, never in protected paths ([`FsPolicy`]) |
//!
//! Adapters are pinned in [`PINS`] (checked against the ACP registry,
//! `cdn.agentclientprotocol.com/registry/v1/latest/registry.json`, on
//! 2026-11-14). Ligus never downloads an adapter at turn time: Copilot and
//! Antigravity speak ACP natively (`copilot --acp`, `agy acp`); the Claude
//! and Codex adapters must be installed on the host (`claude-agent-acp`,
//! `codex-acp` on PATH).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use hive_core::{Driver, ModelProviderKind};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use super::anthropic::{ChatTurn, ProviderError};
use super::dispatch::{ResolvedRuntime, StreamActivity};

/// The ACP protocol version Ligus speaks.
pub const ACP_PROTOCOL_VERSION: u64 = 1;

/// One pinned ACP entry point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpPin {
    pub runtime: ModelProviderKind,
    /// The ACP registry id.
    pub registry_id: &'static str,
    /// The registry's distribution (for the install hint and the release
    /// matrix), pinned.
    pub package: &'static str,
    pub version: &'static str,
    /// Program to run: `None` = the runtime's own CLI binary.
    pub program: Option<&'static str>,
    pub args: &'static [&'static str],
    /// The minimum CLI version with the ACP entry point, when native.
    pub min_cli_version: Option<&'static str>,
    /// How to install the adapter.
    pub install: &'static str,
}

/// The pinned ACP entry points (registry snapshot of 2026-11-14).
pub const PINS: [AcpPin; 4] = [
    AcpPin {
        runtime: ModelProviderKind::Copilot,
        registry_id: "github-copilot-cli",
        package: "@github/copilot",
        version: "1.0.112",
        program: None,
        args: &["--acp"],
        min_cli_version: Some("1.0.112"),
        install: "brew upgrade copilot-cli (or: npm install -g @github/copilot@1.0.112)",
    },
    AcpPin {
        runtime: ModelProviderKind::Antigravity,
        registry_id: "antigravity-acp",
        package: "agy acp",
        version: "1.5.0",
        program: None,
        args: &["acp"],
        min_cli_version: Some("1.5.0"),
        install: "update the Antigravity CLI to 1.5.0 or later (agy acp)",
    },
    AcpPin {
        runtime: ModelProviderKind::ClaudeCode,
        registry_id: "claude-acp",
        package: "@agentclientprotocol/claude-agent-acp",
        version: "0.107.0",
        program: Some("claude-agent-acp"),
        args: &[],
        min_cli_version: None,
        install: "npm install -g @agentclientprotocol/claude-agent-acp@0.107.0",
    },
    AcpPin {
        runtime: ModelProviderKind::Codex,
        registry_id: "codex-acp",
        package: "@agentclientprotocol/codex-acp",
        version: "2.6.0",
        program: Some("codex-acp"),
        args: &[],
        min_cli_version: None,
        install: "npm install -g @agentclientprotocol/codex-acp@2.6.0",
    },
];

/// The pin for `kind`, if it has an ACP entry point.
pub fn pin_for(kind: ModelProviderKind) -> Option<&'static AcpPin> {
    PINS.iter().find(|p| p.runtime == kind)
}

/// A resolved ACP command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpCommand {
    pub program: String,
    pub args: Vec<String>,
}

/// Why a runtime runs on its legacy adapter instead of ACP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyReason {
    /// The runtime's driver setting is `legacy`.
    Configured,
    /// No ACP entry point for this runtime.
    NoAcp,
    /// The adapter isn't installed on this host.
    NotInstalled(String),
    /// Claude Code over ACP needs an API key configured in Ligus (the
    /// claude.ai login is not offered through the SDK adapter: Anthropic's
    /// Agent SDK terms).
    ClaudeSubscription,
    /// A restricted envelope, and the Claude adapter can't be held to the
    /// legacy adapter's restricted flags (settings sources, hooks, strict MCP).
    RestrictedClaude,
}

fn on_path(program: &str) -> Option<PathBuf> {
    let p = Path::new(program);
    if p.is_absolute() {
        return p.is_file().then(|| p.to_path_buf());
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).map(|d| d.join(program)).find(|c| c.is_file())
}

/// Which driver a turn uses: `Ok(cmd)` = ACP with `cmd`, `Err(why)` =
/// legacy. `restricted` = the turn's egress isn't open. Pure apart from the
/// PATH lookup of the adapter.
pub fn resolve(rt: &ResolvedRuntime, restricted: bool) -> Result<AcpCommand, LegacyReason> {
    if rt.driver == Driver::Legacy {
        return Err(LegacyReason::Configured);
    }
    let pin = pin_for(rt.provider).ok_or(LegacyReason::NoAcp)?;
    if rt.provider == ModelProviderKind::ClaudeCode {
        // Until Anthropic confirms (V2) that launching the user's own Claude
        // login through the SDK adapter is allowed, ACP is used for Claude
        // only with an API key configured in Ligus; subscriptions stay on
        // `claude -p`, which runs Claude Code itself.
        if rt.api_key.as_deref().is_none_or(str::is_empty) {
            return Err(LegacyReason::ClaudeSubscription);
        }
        if restricted {
            return Err(LegacyReason::RestrictedClaude);
        }
    }
    let program = match pin.program {
        Some(p) => p.to_string(),
        None if rt.endpoint.trim().is_empty() => super::cli_agent::default_program_for(rt.provider).to_string(),
        None => rt.endpoint.trim().to_string(),
    };
    if on_path(&program).is_none() {
        return Err(LegacyReason::NotInstalled(pin.install.to_string()));
    }
    Ok(AcpCommand { program, args: pin.args.iter().map(|s| s.to_string()).collect() })
}

// ---------------------------------------------------------------------------
// Permissions and the filesystem
// ---------------------------------------------------------------------------

/// What Ligus does with a `session/request_permission`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermDecision {
    Allow,
    Deny,
    /// Ask the chat's humans (ask-the-human), deny when no one answers.
    Escalate,
}

/// The permission tier (Ligus's CLI permission setting) applied to an ACP
/// tool kind (`read`, `edit`, `delete`, `move`, `search`, `execute`,
/// `think`, `fetch`, `switch_mode`, `other`).
///
/// - Reads, searches and thinking are always allowed.
/// - Edits need `acceptEdits` or higher; commands need `runCommands` or
///   higher; a request above the tier is escalated to a human.
/// - When the author gate allows no writes, edits and commands are denied
///   outright (a human can't widen the gate from a permission prompt).
/// - Network fetches are allowed only under `bypassPermissions` with open
///   egress; otherwise escalated (open egress) or denied (restricted).
pub fn judge_permission(tool_kind: &str, mode: &str, writes_allowed: bool, egress_open: bool) -> PermDecision {
    let rank = match mode {
        "acceptEdits" => 1,
        "runCommands" => 2,
        "bypassPermissions" => 3,
        _ => 0,
    };
    match tool_kind {
        "read" | "search" | "think" => PermDecision::Allow,
        "edit" | "delete" | "move" => {
            if !writes_allowed {
                PermDecision::Deny
            } else if rank >= 1 {
                PermDecision::Allow
            } else {
                PermDecision::Escalate
            }
        }
        "execute" => {
            if !writes_allowed {
                PermDecision::Deny
            } else if rank >= 2 {
                PermDecision::Allow
            } else {
                PermDecision::Escalate
            }
        }
        "fetch" => {
            if !egress_open {
                PermDecision::Deny
            } else if rank >= 3 {
                PermDecision::Allow
            } else {
                PermDecision::Escalate
            }
        }
        // switch_mode, other, unknown kinds: a person decides.
        _ => PermDecision::Escalate,
    }
}

/// Pick the option id for a decision: `allow_once` (else `allow_always`) to
/// allow, `reject_once` (else `reject_always`) to deny. `None` = cancel.
pub fn option_for(options: &[Value], allow: bool) -> Option<String> {
    let kinds: &[&str] = if allow { &["allow_once", "allow_always"] } else { &["reject_once", "reject_always"] };
    kinds.iter().find_map(|k| {
        options
            .iter()
            .find(|o| o.get("kind").and_then(Value::as_str) == Some(*k))
            .and_then(|o| o.get("optionId").and_then(Value::as_str))
            .map(str::to_owned)
    })
}

/// Where the agent's `fs/*` requests may go: inside the turn's directory,
/// never into its read-only paths for writes, and writes only when the plan
/// and the tier allow them.
#[derive(Debug, Clone)]
pub struct FsPolicy {
    pub root: PathBuf,
    pub writable: bool,
    pub read_only: Vec<PathBuf>,
    pub deny_read: Vec<PathBuf>,
}

impl FsPolicy {
    pub fn from_plan(plan: &crate::executor::EnforcementPlan, tier_writes: bool) -> Self {
        Self {
            root: crate::executor::canonical(&plan.worktree),
            writable: plan.worktree_writable && tier_writes,
            read_only: plan.read_only.iter().map(|p| crate::executor::canonical(p)).collect(),
            deny_read: plan.deny_read.iter().map(|p| crate::executor::canonical(p)).collect(),
        }
    }

    fn inside(&self, path: &str) -> Result<PathBuf, String> {
        let p = Path::new(path);
        if !p.is_absolute() {
            return Err("path must be absolute".into());
        }
        if p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err("path may not contain ..".into());
        }
        let c = crate::executor::canonical(p);
        if !c.starts_with(&self.root) {
            return Err("outside the turn's directory".into());
        }
        if self.deny_read.iter().any(|d| c.starts_with(d)) && !c.starts_with(&self.root) {
            return Err("denied".into());
        }
        Ok(c)
    }

    /// `fs/read_text_file`: `line` is 1-based, `limit` a line count.
    pub fn read(&self, path: &str, line: Option<u64>, limit: Option<u64>) -> Result<String, String> {
        let c = self.inside(path)?;
        let text = std::fs::read_to_string(&c).map_err(|e| e.to_string())?;
        if line.is_none() && limit.is_none() {
            return Ok(text);
        }
        let start = line.unwrap_or(1).max(1) as usize - 1;
        let take = limit.map(|l| l as usize).unwrap_or(usize::MAX);
        Ok(text.split_inclusive('\n').skip(start).take(take).collect())
    }

    /// `fs/write_text_file`.
    pub fn write(&self, path: &str, content: &str) -> Result<(), String> {
        if !self.writable {
            return Err("this turn may not write files".into());
        }
        let c = self.inside(path)?;
        if self.read_only.iter().any(|r| c.starts_with(r)) {
            return Err("that path is protected".into());
        }
        let parent = c.parent().ok_or("no parent directory")?;
        if !parent.starts_with(&self.root) {
            return Err("outside the turn's directory".into());
        }
        // The host writes unsandboxed, so it never follows a link the agent
        // planted: a dangling symlink resolves nowhere, which left the check
        // above looking at the link's own (inside) path.
        if std::fs::symlink_metadata(&c).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("that path is a symbolic link".into());
        }
        let name = c.file_name().ok_or("no file name")?.to_owned();
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        // Re-check where the directory really is now that it exists.
        let real_parent = std::fs::canonicalize(parent).map_err(|e| e.to_string())?;
        if !real_parent.starts_with(&self.root) {
            return Err("outside the turn's directory".into());
        }
        let target = real_parent.join(name);
        if self.read_only.iter().any(|r| target.starts_with(r)) {
            return Err("that path is protected".into());
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(libc::O_NOFOLLOW);
        }
        let mut f = opts.open(&target).map_err(|e| e.to_string())?;
        std::io::Write::write_all(&mut f, content.as_bytes()).map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------
// The client
// ---------------------------------------------------------------------------

/// A failed ACP turn: `Init` failures (the agent didn't start or didn't
/// answer `initialize`) let `Driver::Auto` fall back to the legacy adapter;
/// `Turn` failures are the turn's error.
#[derive(Debug)]
pub enum AcpFailure {
    Init(String),
    Turn(ProviderError),
}

impl std::fmt::Display for AcpFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcpFailure::Init(e) => write!(f, "ACP initialize failed: {e}"),
            AcpFailure::Turn(e) => write!(f, "{e}"),
        }
    }
}

/// One ACP turn's inputs.
pub struct AcpTurn<'a> {
    pub kind: ModelProviderKind,
    pub command: &'a AcpCommand,
    pub model: &'a str,
    pub working_dir: Option<&'a str>,
    pub extra_env: &'a [(String, String)],
    pub system: Option<&'a str>,
    pub turns: &'a [ChatTurn],
    /// MCP servers beyond the Ligus one (ACP stdio entries).
    pub extra_mcp: Vec<Value>,
    /// The runtime's Ligus-configured key (passed to the adapter's env as
    /// the runtime's key variable, through the plan).
    pub api_key: Option<&'a str>,
    /// The runtime's legacy permission-tier args (already narrowed by the
    /// author gate): the tier ACP permission requests are judged by.
    pub tier_args: &'a [String],
}

/// The Ligus permission tier a CLI runtime's tier args express: `default`
/// (read-only), `acceptEdits`, `runCommands` or `bypassPermissions`.
pub fn tier_from_args(kind: ModelProviderKind, args: &[String]) -> &'static str {
    let has = |x: &str| args.iter().any(|a| a == x);
    let pair = |flag: &str, val: &str| args.windows(2).any(|w| w[0] == flag && w[1] == val);
    match kind {
        ModelProviderKind::ClaudeCode => {
            if pair("--permission-mode", "bypassPermissions") || has("--dangerously-skip-permissions") {
                "bypassPermissions"
            } else if pair("--allowedTools", "Bash") {
                "runCommands"
            } else if pair("--permission-mode", "acceptEdits") {
                "acceptEdits"
            } else {
                "default"
            }
        }
        ModelProviderKind::Antigravity => {
            if has("--dangerously-skip-permissions") {
                "bypassPermissions"
            } else if pair("--mode", "auto") {
                "runCommands"
            } else if pair("--mode", "accept-edits") {
                "acceptEdits"
            } else {
                "default"
            }
        }
        ModelProviderKind::Copilot => {
            if has("--allow-all-tools") {
                "bypassPermissions"
            } else if has("--allow-tool=shell") {
                "runCommands"
            } else if has("--allow-tool=write") {
                "acceptEdits"
            } else {
                "default"
            }
        }
        // Codex always runs with file and shell tools (its legacy adapter's
        // workspace-write sandbox); the author gate refuses it outright below
        // the floor.
        ModelProviderKind::Codex => "runCommands",
        _ => "default",
    }
}

struct Conn {
    stdin: tokio::process::ChildStdin,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    next_id: u64,
}

impl Conn {
    async fn send(&mut self, v: &Value) -> Result<(), String> {
        let mut line = serde_json::to_vec(v).map_err(|e| e.to_string())?;
        line.push(b'\n');
        self.stdin.write_all(&line).await.map_err(|e| format!("write: {e}"))?;
        self.stdin.flush().await.map_err(|e| format!("write: {e}"))
    }
}

/// The per-turn handling state.
struct Handler<'a, D: FnMut(String), A: FnMut(StreamActivity)> {
    on_delta: D,
    on_activity: A,
    assembled: String,
    last_message: Option<String>,
    fs: Option<FsPolicy>,
    scope: Option<std::sync::Arc<crate::agent_turn::AgentTurnScope>>,
    mode: &'static str,
    writes_allowed: bool,
    egress_open: bool,
    last_cost: f64,
    session_id: Option<String>,
    agent_label: &'a str,
}

impl<D: FnMut(String), A: FnMut(StreamActivity)> Handler<'_, D, A> {
    fn text(&mut self, message_id: Option<&str>, text: &str) {
        if text.is_empty() {
            return;
        }
        let new_segment = message_id.is_some() && self.last_message.as_deref() != message_id;
        if message_id.is_some() {
            self.last_message = message_id.map(str::to_owned);
        }
        if new_segment && !self.assembled.is_empty() && !self.assembled.ends_with('\n') {
            self.assembled.push('\n');
            (self.on_delta)("\n".into());
        }
        self.assembled.push_str(text);
        (self.on_delta)(text.to_string());
    }

    fn update(&mut self, u: &Value) {
        let kind = u.get("sessionUpdate").and_then(Value::as_str).unwrap_or("");
        let content_text = |v: &Value| -> String {
            match v.get("content") {
                Some(Value::Object(o)) => o.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
                Some(Value::Array(a)) => a
                    .iter()
                    .filter_map(|c| c.get("content").and_then(|c| c.get("text")).or_else(|| c.get("text")).and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            }
        };
        match kind {
            "agent_message_chunk" => {
                let t = content_text(u);
                let mid = u.get("messageId").and_then(Value::as_str).map(str::to_owned);
                self.text(mid.as_deref(), &t);
            }
            "agent_thought_chunk" => (self.on_activity)(StreamActivity::Thinking { text: content_text(u) }),
            "tool_call" => (self.on_activity)(StreamActivity::Tool {
                id: u.get("toolCallId").and_then(Value::as_str).unwrap_or("").to_string(),
                name: u.get("title").and_then(Value::as_str).unwrap_or("tool").to_string(),
                input_json: u.get("rawInput").map(Value::to_string).unwrap_or_else(|| "{}".into()),
            }),
            "tool_call_update" => {
                let status = u.get("status").and_then(Value::as_str).unwrap_or("");
                if matches!(status, "completed" | "failed") {
                    let content = content_text(u);
                    (self.on_activity)(StreamActivity::ToolResult {
                        call_id: u.get("toolCallId").and_then(Value::as_str).unwrap_or("").to_string(),
                        is_error: status == "failed",
                        content: super::cli_agent::cap_preview(content),
                    })
                }
            }
            "plan" => (self.on_activity)(StreamActivity::Tool {
                id: "plan".into(),
                name: "plan".into(),
                input_json: u.get("entries").map(Value::to_string).unwrap_or_else(|| "[]".into()),
            }),
            "usage_update" => {
                let cost = u.get("cost");
                let usd = cost
                    .filter(|c| c.get("currency").and_then(Value::as_str).is_none_or(|c| c.eq_ignore_ascii_case("usd")))
                    .and_then(|c| c.get("amount").and_then(Value::as_f64));
                if let Some(total) = usd {
                    // Cumulative for the session: report the delta.
                    let delta = (total - self.last_cost).max(0.0);
                    self.last_cost = total.max(self.last_cost);
                    if delta > 0.0 {
                        if let Some(spend) = self.scope.as_ref().and_then(|s| s.spend.clone()) {
                            spend(delta);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Answer an agent → client request. `Ok(result)` or `Err((code, msg))`.
    async fn request(&mut self, method: &str, params: &Value) -> Result<Value, (i64, String)> {
        match method {
            "session/request_permission" => {
                let options: Vec<Value> = params.get("options").and_then(Value::as_array).cloned().unwrap_or_default();
                let tool = params.get("toolCall").cloned().unwrap_or(Value::Null);
                let tool_kind = tool.get("kind").and_then(Value::as_str).unwrap_or("other");
                let decision = judge_permission(tool_kind, self.mode, self.writes_allowed, self.egress_open);
                let allow = match decision {
                    PermDecision::Allow => true,
                    PermDecision::Deny => false,
                    PermDecision::Escalate => self.escalate(&tool, &options).await,
                };
                let outcome = match option_for(&options, allow) {
                    Some(id) => json!({"outcome": "selected", "optionId": id}),
                    None => json!({"outcome": "cancelled"}),
                };
                Ok(json!({ "outcome": outcome }))
            }
            "fs/read_text_file" => {
                let fs = self.fs.as_ref().ok_or((-32603, "no filesystem for this turn".to_string()))?;
                let path = params.get("path").and_then(Value::as_str).unwrap_or("");
                let line = params.get("line").and_then(Value::as_u64);
                let limit = params.get("limit").and_then(Value::as_u64);
                fs.read(path, line, limit).map(|content| json!({ "content": content })).map_err(|e| (-32602, e))
            }
            "fs/write_text_file" => {
                let fs = self.fs.as_ref().ok_or((-32603, "no filesystem for this turn".to_string()))?;
                let path = params.get("path").and_then(Value::as_str).unwrap_or("");
                let content = params.get("content").and_then(Value::as_str).unwrap_or("");
                fs.write(path, content).map(|_| json!({})).map_err(|e| (-32602, e))
            }
            other => Err((-32601, format!("method not supported by Ligus: {other}"))),
        }
    }

    /// Put an out-of-tier permission request to the chat's humans. The turn
    /// is paused (idle timers off) while it waits.
    async fn escalate(&mut self, tool: &Value, options: &[Value]) -> bool {
        let Some(scope) = self.scope.clone() else { return false };
        let Some(asker) = scope.asker.clone() else { return false };
        let title = tool.get("title").and_then(Value::as_str).unwrap_or("a tool");
        let kind = tool.get("kind").and_then(Value::as_str).unwrap_or("other");
        let question = format!("{} asks permission to run {title} ({kind}). Allow it once?", self.agent_label);
        let choices = vec!["Allow once".to_string(), "Deny".to_string()];
        let _pause = scope.pause();
        let answer = asker.ask(question, choices, ask_timeout()).await;
        let _ = options;
        answer.is_some_and(|a| a.option_index == Some(0))
    }
}

/// How long a permission escalation waits for a person.
pub fn ask_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(
        hive_core::compat::var("LIGUS_ACP_ASK_TIMEOUT_SECS").ok().and_then(|v| v.parse().ok()).filter(|s: &u64| *s > 0).unwrap_or(600),
    )
}

async fn read_line(conn: &mut Conn, idle: std::time::Duration, label: &str) -> Result<Option<String>, String> {
    match crate::agent_turn::idle_timeout(idle, conn.lines.next_line()).await {
        Ok(Ok(l)) => Ok(l),
        Ok(Err(e)) => Err(format!("read: {e}")),
        Err(()) => Err(format!(
            "{label} produced no output for {}s; raise LIGUS_TURN_IDLE_TIMEOUT_SECS for long silent steps",
            idle.as_secs()
        )),
    }
}

/// Send `method` and process incoming messages until its response.
async fn call<D: FnMut(String), A: FnMut(StreamActivity)>(
    conn: &mut Conn,
    h: &mut Handler<'_, D, A>,
    method: &str,
    params: Value,
    idle: std::time::Duration,
) -> Result<Result<Value, Value>, String> {
    let id = conn.next_id;
    conn.next_id += 1;
    conn.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).await?;
    loop {
        let Some(line) = read_line(conn, idle, h.agent_label).await? else {
            return Err(format!("the agent exited before answering {method}"));
        };
        let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else { continue };
        let m = msg.get("method").and_then(Value::as_str).map(str::to_owned);
        match (m, msg.get("id").cloned()) {
            // A request from the agent.
            (Some(m), Some(rid)) => {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                let reply = match h.request(&m, &params).await {
                    Ok(result) => json!({"jsonrpc": "2.0", "id": rid, "result": result}),
                    Err((code, message)) => json!({"jsonrpc": "2.0", "id": rid, "error": {"code": code, "message": message}}),
                };
                conn.send(&reply).await?;
            }
            // A notification.
            (Some(m), None) => {
                if m == "session/update" {
                    if let Some(u) = msg.get("params").and_then(|p| p.get("update")) {
                        h.update(u);
                    }
                }
            }
            // A response.
            (None, Some(rid)) if rid.as_u64() == Some(id) => {
                if let Some(e) = msg.get("error") {
                    return Ok(Err(e.clone()));
                }
                return Ok(Ok(msg.get("result").cloned().unwrap_or(Value::Null)));
            }
            _ => {}
        }
    }
}

fn is_auth_error(e: &Value) -> bool {
    let code = e.get("code").and_then(Value::as_i64).unwrap_or(0);
    let msg = e.get("message").and_then(Value::as_str).unwrap_or("").to_ascii_lowercase();
    code == -32000 || msg.contains("auth")
}

/// A non-interactive auth method: one the agent completes from its own
/// configuration (an API key in its env), never a terminal or browser login.
pub fn non_interactive_method(methods: &[Value]) -> Option<String> {
    methods.iter().find_map(|m| {
        let id = m.get("id").and_then(Value::as_str)?;
        let ty = m.get("type").and_then(Value::as_str).unwrap_or("agent");
        let lid = id.to_ascii_lowercase();
        let keyish = lid.contains("api-key") || lid.contains("api_key") || lid.contains("apikey") || lid.contains("env");
        (ty != "terminal" && keyish && !lid.contains("login")).then(|| id.to_string())
    })
}

/// Run one turn over ACP. See the module docs.
pub async fn stream_reply(
    t: AcpTurn<'_>,
    on_delta: impl FnMut(String),
    on_activity: impl FnMut(StreamActivity),
) -> Result<String, AcpFailure> {
    let label = super::cli_agent::display_for(t.kind);
    let mode = tier_from_args(t.kind, t.tier_args);
    let gate_writes = crate::executor::current_turn().is_none_or(|p| p.gate.allows_writes());
    let tier_writes = gate_writes && mode != "default";
    let spec = crate::executor::SpawnSpec::new(crate::executor::Purpose::Agent(t.kind), t.command.program.as_str(), &t.command.args)
        .cwd(t.working_dir.map(Path::new))
        .env(t.extra_env);
    let spec = crate::executor::SpawnSpec { provider_key: t.api_key, writes: Some(tier_writes), ..spec };
    let (mut cmd, guard) = crate::executor::tokio_command(spec).map_err(|e| {
        // A refused sandbox is the turn's error, not an ACP start failure.
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            AcpFailure::Turn(ProviderError::Subprocess(format!("sandbox {}: {e}", t.command.program)))
        } else {
            AcpFailure::Init(format!("sandbox {}: {e}", t.command.program))
        }
    })?;
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    super::subprocess::suppress_console_window(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| AcpFailure::Init(format!("spawn {}: {e}", t.command.program)))?;
    let stdin = child.stdin.take().ok_or_else(|| AcpFailure::Init("no stdin".into()))?;
    let stdout = child.stdout.take().ok_or_else(|| AcpFailure::Init("no stdout".into()))?;
    let stderr_task = child.stderr.take().map(|mut e| {
        tokio::spawn(async move {
            let mut buf = String::new();
            let _ = e.read_to_string(&mut buf).await;
            buf
        })
    });
    let mut conn = Conn { stdin, lines: BufReader::new(stdout).lines(), next_id: 1 };
    let scope = crate::agent_turn::current();
    let egress_open = matches!(guard.plan().egress, crate::executor::EgressPlan::Open)
        || crate::executor::current_turn().is_none_or(|p| p.envelope.egress.is_open());
    let mut h = Handler {
        on_delta,
        on_activity,
        assembled: String::new(),
        last_message: None,
        fs: Some(FsPolicy::from_plan(guard.plan(), tier_writes)),
        scope: scope.clone(),
        mode,
        writes_allowed: gate_writes,
        egress_open,
        last_cost: 0.0,
        session_id: None,
        agent_label: label,
    };
    let idle = super::turn_idle_timeout();
    // A short window for initialize: an agent that doesn't speak ACP (an old
    // CLI without the flag) fails fast and the turn falls back.
    let init_idle = std::time::Duration::from_secs(
        hive_core::compat::var("LIGUS_ACP_INIT_TIMEOUT_SECS").ok().and_then(|v| v.parse().ok()).filter(|s: &u64| *s > 0).unwrap_or(60),
    );
    let stderr_of = |task: Option<tokio::task::JoinHandle<String>>| async move {
        match task {
            Some(t) => t.await.unwrap_or_default(),
            None => String::new(),
        }
    };

    let init = call(
        &mut conn,
        &mut h,
        "initialize",
        json!({
            "protocolVersion": ACP_PROTOCOL_VERSION,
            "clientCapabilities": { "fs": { "readTextFile": true, "writeTextFile": true }, "terminal": false },
            "clientInfo": { "name": "ligus", "version": env!("CARGO_PKG_VERSION") },
        }),
        init_idle,
    )
    .await;
    let init = match init {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return Err(AcpFailure::Init(e.to_string())),
        Err(e) => {
            drop(conn);
            let _ = child.start_kill();
            let err = stderr_of(stderr_task).await;
            return Err(AcpFailure::Init(format!("{e} {}", err.trim()).trim().to_string()));
        }
    };
    let version = init.get("protocolVersion").and_then(Value::as_u64).unwrap_or(0);
    if version != ACP_PROTOCOL_VERSION {
        return Err(AcpFailure::Init(format!("the agent speaks ACP version {version}, Ligus speaks {ACP_PROTOCOL_VERSION}")));
    }
    let auth_methods: Vec<Value> = init.get("authMethods").and_then(Value::as_array).cloned().unwrap_or_default();

    // MCP servers: the Ligus server first (it always holds its slot), then
    // the chat's allowed servers.
    let mut mcp: Vec<Value> = Vec::new();
    if let Some(attach) = scope.as_ref().and_then(|s| s.mcp.as_ref()) {
        mcp.push(attach.acp_entry());
    }
    mcp.extend(t.extra_mcp.iter().cloned());
    let cwd = crate::executor::canonical(&guard.plan().worktree).to_string_lossy().into_owned();
    let new_params = json!({ "cwd": cwd, "mcpServers": mcp });
    let turn_err = |e: String| AcpFailure::Turn(super::cli_agent::failure_error_for(t.kind, &e));
    let mut session = call(&mut conn, &mut h, "session/new", new_params.clone(), idle).await.map_err(turn_err)?;
    if let Err(e) = &session {
        if is_auth_error(e) {
            if let Some(method) = non_interactive_method(&auth_methods) {
                let _ = call(&mut conn, &mut h, "authenticate", json!({ "methodId": method }), idle).await.map_err(turn_err)?;
                session = call(&mut conn, &mut h, "session/new", new_params, idle).await.map_err(turn_err)?;
            }
        }
    }
    let session = session.map_err(|e| turn_err(e.get("message").and_then(Value::as_str).unwrap_or("session/new failed").to_string()))?;
    let session_id = session.get("sessionId").and_then(Value::as_str).unwrap_or("").to_string();
    if session_id.is_empty() {
        return Err(turn_err("the agent returned no session id".into()));
    }
    h.session_id = Some(session_id.clone());
    if !t.model.trim().is_empty() {
        // Best effort: not every agent supports picking a model per session.
        let _ = call(&mut conn, &mut h, "session/set_model", json!({ "sessionId": session_id, "modelId": t.model.trim() }), idle).await;
    }
    let prompt = super::subprocess::render_prompt(t.system, t.turns);
    let res = call(
        &mut conn,
        &mut h,
        "session/prompt",
        json!({ "sessionId": session_id, "prompt": [{ "type": "text", "text": prompt }] }),
        idle,
    )
    .await;
    let res = match res {
        Ok(r) => r,
        Err(e) => {
            // Tell the agent to stop, then let kill_on_drop reap it.
            let _ = conn.send(&json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session_id}})).await;
            let err = stderr_of(stderr_task).await;
            return Err(turn_err(format!("{e} {}", err.trim()).trim().to_string()));
        }
    };
    let stop = match res {
        Ok(v) => v.get("stopReason").and_then(Value::as_str).unwrap_or("end_turn").to_string(),
        Err(e) => return Err(turn_err(e.get("message").and_then(Value::as_str).unwrap_or("session/prompt failed").to_string())),
    };
    drop(conn);
    let _ = child.start_kill();
    let _ = child.wait().await;
    if stop == "cancelled" {
        return Err(AcpFailure::Turn(ProviderError::Subprocess(format!("{label}: the turn was cancelled"))));
    }
    if stop == "refusal" && h.assembled.trim().is_empty() {
        return Err(AcpFailure::Turn(ProviderError::Subprocess(format!("{label} refused the request"))));
    }
    drop(guard);
    Ok(h.assembled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_tier_mapping() {
        use PermDecision::*;
        // Reads always.
        for mode in ["default", "acceptEdits", "runCommands", "bypassPermissions"] {
            assert_eq!(judge_permission("read", mode, false, false), Allow);
            assert_eq!(judge_permission("search", mode, true, true), Allow);
        }
        // Edits: escalated below the tier, allowed at it, denied without a write gate.
        assert_eq!(judge_permission("edit", "default", true, true), Escalate);
        assert_eq!(judge_permission("edit", "acceptEdits", true, true), Allow);
        assert_eq!(judge_permission("edit", "bypassPermissions", false, true), Deny);
        // Commands.
        assert_eq!(judge_permission("execute", "acceptEdits", true, true), Escalate);
        assert_eq!(judge_permission("execute", "runCommands", true, true), Allow);
        assert_eq!(judge_permission("execute", "runCommands", false, true), Deny);
        // Network.
        assert_eq!(judge_permission("fetch", "bypassPermissions", true, false), Deny);
        assert_eq!(judge_permission("fetch", "runCommands", true, true), Escalate);
        assert_eq!(judge_permission("fetch", "bypassPermissions", true, true), Allow);
        // Unknown kinds go to a person.
        assert_eq!(judge_permission("switch_mode", "bypassPermissions", true, true), Escalate);
        assert_eq!(judge_permission("other", "default", true, true), Escalate);
    }

    #[test]
    fn options_are_picked_by_kind() {
        let opts = vec![
            json!({"optionId": "a1", "name": "Always", "kind": "allow_always"}),
            json!({"optionId": "a0", "name": "Once", "kind": "allow_once"}),
            json!({"optionId": "r0", "name": "No", "kind": "reject_once"}),
        ];
        assert_eq!(option_for(&opts, true).as_deref(), Some("a0"));
        assert_eq!(option_for(&opts, false).as_deref(), Some("r0"));
        assert_eq!(option_for(&opts[..1], false), None);
    }

    #[test]
    fn auth_methods_never_pick_interactive_logins() {
        let m = vec![
            json!({"id": "claude-login", "name": "Log in"}),
            json!({"type": "terminal", "id": "env-setup", "name": "Terminal"}),
        ];
        assert_eq!(non_interactive_method(&m), None);
        let m = vec![json!({"id": "anthropic-api-key", "name": "API key"})];
        assert_eq!(non_interactive_method(&m).as_deref(), Some("anthropic-api-key"));
    }

    fn rt(kind: ModelProviderKind, key: Option<&str>, driver: Driver, endpoint: &str) -> ResolvedRuntime {
        ResolvedRuntime {
            provider: kind,
            model: String::new(),
            endpoint: endpoint.into(),
            api_key: key.map(str::to_owned),
            args: vec![],
            model_provider_id: None,
            model_base_url: None,
            context_window_tokens: None,
            keep_alive: None,
            think: None,
            tools: false,
            max_tool_rounds: None,
            allow_remote_file_tools: false,
            driver,
        }
    }

    #[test]
    fn claude_uses_acp_only_with_an_api_key_and_open_egress() {
        // The adapter binary is "installed" via an absolute path stand-in:
        // resolution for Claude checks the key first, so the policy holds
        // whether or not the adapter is present.
        let sub = rt(ModelProviderKind::ClaudeCode, None, Driver::Acp, "claude");
        assert_eq!(resolve(&sub, false), Err(LegacyReason::ClaudeSubscription));
        let keyed = rt(ModelProviderKind::ClaudeCode, Some("sk-test"), Driver::Auto, "claude");
        assert_eq!(resolve(&keyed, true), Err(LegacyReason::RestrictedClaude));
        assert_eq!(resolve(&rt(ModelProviderKind::Copilot, None, Driver::Legacy, "copilot"), false), Err(LegacyReason::Configured));
        assert_eq!(resolve(&rt(ModelProviderKind::Aider, None, Driver::Acp, "aider"), false), Err(LegacyReason::NoAcp));
    }

    #[test]
    fn native_cli_acp_uses_the_configured_binary() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("copilot");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        let r = resolve(&rt(ModelProviderKind::Copilot, None, Driver::Auto, bin.to_str().unwrap()), false).unwrap();
        assert_eq!(r.program, bin.to_str().unwrap());
        assert_eq!(r.args, vec!["--acp"]);
        let agy = dir.path().join("agy");
        std::fs::write(&agy, "#!/bin/sh\n").unwrap();
        let r = resolve(&rt(ModelProviderKind::Antigravity, None, Driver::Auto, agy.to_str().unwrap()), false).unwrap();
        assert_eq!(r.args, vec!["acp"]);
        // Missing binary: legacy, with the install hint.
        assert!(matches!(
            resolve(&rt(ModelProviderKind::Copilot, None, Driver::Auto, "/nonexistent/copilot"), false),
            Err(LegacyReason::NotInstalled(_))
        ));
    }

    #[test]
    fn fs_policy_keeps_the_agent_in_its_worktree() {
        let wt = tempfile::tempdir().unwrap();
        let root = crate::executor::canonical(wt.path());
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("a.txt"), "1\n2\n3\n").unwrap();
        let fs = FsPolicy { root: root.clone(), writable: true, read_only: vec![root.join(".git")], deny_read: vec![] };
        assert_eq!(fs.read(root.join("a.txt").to_str().unwrap(), Some(2), Some(1)).unwrap(), "2\n");
        assert!(fs.read("/etc/hosts", None, None).is_err());
        assert!(fs.read(&format!("{}/../x", root.display()), None, None).is_err());
        assert!(fs.write(root.join("b.txt").to_str().unwrap(), "ok").is_ok());
        assert!(fs.write(root.join(".git/config").to_str().unwrap(), "x").is_err());
        let ro = FsPolicy { writable: false, ..fs };
        assert!(ro.write(root.join("c.txt").to_str().unwrap(), "x").is_err());
    }

    /// A dangling symlink inside the worktree resolves nowhere, so the path
    /// check saw only the link's own (inside) path; the host's write then
    /// followed it and created a file outside the worktree.
    #[cfg(unix)]
    #[test]
    fn fs_write_never_follows_a_dangling_symlink_out_of_the_worktree() {
        let wt = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = crate::executor::canonical(wt.path());
        let target = outside.path().join("planted.plist");
        std::os::unix::fs::symlink(&target, root.join("evil")).unwrap();
        // And one pointing at a protected path that doesn't exist yet.
        std::fs::create_dir_all(root.join(".git/hooks")).unwrap();
        std::os::unix::fs::symlink(root.join(".git/hooks/pre-commit"), root.join("hook")).unwrap();
        let fs = FsPolicy { root: root.clone(), writable: true, read_only: vec![root.join(".git")], deny_read: vec![] };
        assert!(fs.write(root.join("evil").to_str().unwrap(), "x").is_err());
        assert!(!target.exists(), "wrote outside the worktree through a dangling symlink");
        assert!(fs.write(root.join("hook").to_str().unwrap(), "#!/bin/sh").is_err());
        assert!(!root.join(".git/hooks/pre-commit").exists(), "wrote a git hook through a dangling symlink");
    }
}

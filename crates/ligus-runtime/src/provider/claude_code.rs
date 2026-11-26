//! Claude Code CLI bridge — the bring-your-own-subscription path (no API key).
//! Ported from `ClaudeCodeBridge.swift`. Runs the official `claude` CLI in
//! non-interactive **stream-json** mode and surfaces token-level deltas, so a
//! Claude subscription streams just like the Anthropic API backend.
//!
//! Invocation:
//! `claude -p --output-format stream-json --verbose --include-partial-messages
//!  [--add-dir <workspace>] <extra args>` with the rendered prompt on stdin.
//!
//! The CLI emits newline-delimited JSON. With `--include-partial-messages` it
//! produces `stream_event` lines wrapping the Anthropic streaming events
//! (`content_block_delta` / `text_delta`) — token streaming. A final `result`
//! line carries the complete text (used as a fallback if no deltas arrived,
//! e.g. an older CLI without partial messages).

use std::process::Stdio;

use serde_json::Value;
use tokio::io::{AsyncWriteExt, BufReader};

use super::anthropic::{ChatTurn, ProviderError};
use super::dispatch::StreamActivity;

/// Cap on a tool result's text carried in a live activity event — the raw output
/// (a full file read, a long build log) can be huge, and it's only a preview.
const RESULT_PREVIEW_CAP: usize = 4000;

/// Render the conversation for the CLI prompt (stdin). Unlike the generic
/// bridge there's no `assistant:` trailer — `claude -p` treats the whole text
/// as the user prompt.
pub fn render_prompt(system: Option<&str>, turns: &[ChatTurn]) -> String {
    let mut out = String::new();
    if let Some(s) = system {
        out.push_str(s);
        out.push_str("\n\n");
    }
    for t in turns {
        out.push_str(&format!("{}: {}\n", t.role, t.content));
    }
    out
}

/// Extract an incremental text delta from one stream-json line, if it carries a
/// partial-message `content_block_delta` / `text_delta`.
pub fn extract_text_delta(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("type")?.as_str()? != "stream_event" {
        return None;
    }
    let event = v.get("event")?;
    if event.get("type")?.as_str()? != "content_block_delta" {
        return None;
    }
    let delta = event.get("delta")?;
    if delta.get("type")?.as_str()? != "text_delta" {
        return None;
    }
    Some(delta.get("text")?.as_str()?.to_string())
}

/// True if `line` is a `content_block_start` for a TEXT block — the boundary
/// between narration segments around tool calls. Used to re-insert the separator
/// the block split otherwise drops.
pub fn is_text_block_start(line: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
        return false;
    };
    if v.get("type").and_then(Value::as_str) != Some("stream_event") {
        return false;
    }
    let Some(event) = v.get("event") else { return false };
    if event.get("type").and_then(Value::as_str) != Some("content_block_start") {
        return false;
    }
    event
        .get("content_block")
        .and_then(|b| b.get("type"))
        .and_then(Value::as_str)
        == Some("text")
}

/// Extract the final assembled text from a `result` line (fallback path).
pub fn extract_result(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("type")?.as_str()? != "result" {
        return None;
    }
    v.get("result")?.as_str().map(str::to_owned)
}

/// A plan-window observation from a `rate_limit_event` line, if this is one.
/// Parsed defensively: every field is optional, unknown statuses are ignored.
/// Shape (checked against the schema bundled in the Claude Code CLI, 2.1.x):
/// `{"type":"rate_limit_event","rate_limit_info":{"status":"allowed" |
/// "allowed_warning" | "rejected","resetsAt":<unix s>,"rateLimitType":
/// "five_hour" | "seven_day" | "seven_day_opus" | ...,"utilization":<number>,
/// "unifiedWindows":{"five_hour":{"utilization":..,"resetsAt":..},...}},
/// "uuid":..,"session_id":..}`. Only `status` is required there; when the
/// top-level `utilization` is missing, the busiest of `unifiedWindows` is used.
pub fn extract_rate_limit(line: &str) -> Option<ligus_core::throttle::PlanWindow> {
    use ligus_core::throttle::{PlanWindow, WindowStatus};
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("type")?.as_str()? != "rate_limit_event" {
        return None;
    }
    let info = v.get("rate_limit_info").or_else(|| v.get("rateLimitInfo"))?;
    let status = match info.get("status")?.as_str()? {
        "allowed" => WindowStatus::Allowed,
        "allowed_warning" | "warning" => WindowStatus::Warning,
        "rejected" => WindowStatus::Rejected,
        _ => return None,
    };
    let pct_of = |u: f64| {
        let pct = if u <= 1.0 { u * 100.0 } else { u };
        pct.clamp(0.0, 100.0).round() as u8
    };
    let secs = |t: i64| if t > 10_000_000_000 { t / 1000 } else { t };
    // The busiest per-window entry, for when the top-level figures are absent.
    let busiest = info.get("unifiedWindows").and_then(Value::as_object).and_then(|w| {
        w.iter()
            .filter_map(|(name, v)| Some((name.clone(), v.get("utilization")?.as_f64()?, v.get("resetsAt").and_then(Value::as_i64))))
            .max_by(|a, b| a.1.total_cmp(&b.1))
    });
    let utilization_pct =
        info.get("utilization").and_then(Value::as_f64).or(busiest.as_ref().map(|b| b.1)).map(pct_of);
    let resets_at = info
        .get("resetsAt")
        .or_else(|| info.get("resets_at"))
        .and_then(Value::as_i64)
        .or(busiest.as_ref().and_then(|b| b.2))
        .map(secs);
    let window = info
        .get("rateLimitType")
        .or_else(|| info.get("rate_limit_type"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or(busiest.map(|b| b.0));
    let observed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some(PlanWindow { status, utilization_pct, resets_at, window, observed_at })
}

/// Background-activity signals from one non-text stream-json line: `tool_use`
/// blocks on an `assistant` line, `tool_result` blocks on a `user` line, and a
/// `thinking` block (either). Returns empty for text/delta/result/system lines.
/// The CLI re-emits an assistant snapshot as blocks fill in, so the same
/// `tool_use` id can appear more than once — the UI dedups by id.
pub fn extract_activity(line: &str) -> Vec<StreamActivity> {
    let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
    let content = v
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array);
    let Some(blocks) = content else {
        return out;
    };
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("tool_use") if ty == "assistant" => out.push(StreamActivity::Tool {
                id: b.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                name: b.get("name").and_then(Value::as_str).unwrap_or("tool").to_string(),
                input_json: b.get("input").map(Value::to_string).unwrap_or_default(),
            }),
            Some("tool_result") if ty == "user" => {
                let mut content = tool_result_text(b.get("content"));
                if content.len() > RESULT_PREVIEW_CAP {
                    content.truncate(RESULT_PREVIEW_CAP);
                    content.push('…');
                }
                out.push(StreamActivity::ToolResult {
                    call_id: b
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                    content,
                });
            }
            Some("thinking") => out.push(StreamActivity::Thinking { text: String::new() }),
            _ => {}
        }
    }
    out
}

/// A `tool_result` `content` is either a plain string or an array of blocks
/// (`{type:"text", text:…}`); flatten either to text.
fn tool_result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|x| x.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// The Ligus MCP server's status in Claude Code's stream-json `init` line
/// (`connected`, `failed`, `pending`, ...); `None` for any other line or
/// when the server isn't attached.
fn ligus_mcp_status(line: &str) -> Option<String> {
    if !line.contains("\"init\"") || !line.contains("mcp_servers") {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("subtype").and_then(Value::as_str) != Some("init") {
        return None;
    }
    v.get("mcp_servers")?
        .as_array()?
        .iter()
        .find(|s| s.get("name").and_then(Value::as_str) == Some(crate::executor::LIGUS_MCP_ID))
        .and_then(|s| s.get("status").and_then(Value::as_str))
        .map(str::to_string)
}

/// Built-in Claude Code tools a Ligus turn may use: file and shell tools,
/// in-process sub-agents (they inherit this same tool set, sandbox and
/// flags) and the deferred-tool loader. An allow-list (`--tools`), so a tool
/// a newer Claude Code adds is off until Ligus reviews it.
pub const TURN_TOOLS: &[&str] = &["Task", "Agent", "Bash", "Read", "Edit", "Write", "Glob", "Grep", "NotebookEdit", "TodoWrite", "ToolSearch", "LSP"];

/// Web tools, offered only while the turn's egress is open. `WebSearch` is
/// a server-side tool (Anthropic runs the search), so the turn's egress
/// proxy never sees it; it follows the egress policy here instead.
pub const WEB_TOOLS: &[&str] = &["WebFetch", "WebSearch"];

/// Tools that schedule or run work outside the turn, or reach other
/// sessions: cloud routines (`RemoteTrigger`, which run on Anthropic's side
/// under the user's account and connectors), cron and wake-ups, background
/// monitors and tasks, push notifications, messaging other Claude Code
/// sessions, multi-agent workflows, design sync to claude.ai, and worktree
/// switching (Ligus owns the turn's directory). Denied in every turn, even
/// when the workspace opted into the user's own setup.
pub const OUT_OF_TURN_TOOLS: &[&str] = &[
    "RemoteTrigger",
    "CronCreate",
    "CronDelete",
    "CronList",
    "ScheduleWakeup",
    "Monitor",
    "TaskStop",
    "PushNotification",
    "SendMessage",
    "ListAgents",
    "Workflow",
    "DesignSync",
    "EnterWorktree",
    "ExitWorktree",
];

/// Env for every Ligus-run `claude`, inherited by anything it starts (a
/// nested `claude` from the shell too): no cron, no background tasks, no
/// workflows.
pub const CONFINE_ENV: &[(&str, &str)] = &[
    ("CLAUDE_CODE_DISABLE_CRON", "1"),
    ("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1"),
    ("CLAUDE_CODE_DISABLE_WORKFLOWS", "1"),
];

/// What a turn's confinement depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Confine {
    /// The turn's egress is open.
    pub egress_open: bool,
    /// The workspace opted into the user's own Claude Code setup.
    pub user_config: bool,
    /// The author gate allows writes (else only Ligus's own MCP server is
    /// pre-approved).
    pub writes_allowed: bool,
    /// The chat turned the `claude-connectors` egress preset on (open egress
    /// only): the user's claude.ai connectors load, which Claude Code offers
    /// only without `--strict-mcp-config`.
    pub connectors: bool,
    /// The turn may use a shell (Phase C: the Owner's, or granted to the
    /// agent). Without it Bash is off the tool list and denied.
    pub shell: bool,
}

impl Confine {
    /// From the turn in scope (`TurnPolicy::outside_turn` when there is
    /// none: read-only, no shell, only the model).
    pub fn current() -> Self {
        let p = crate::executor::agent_policy();
        let open = p.envelope.egress.is_open();
        Self {
            egress_open: open,
            user_config: p.cli_user_config,
            writes_allowed: p.gate.allows_writes(),
            connectors: open && {
                let c = crate::egress::EgressPreset::ClaudeConnectors;
                p.egress_presets.contains(&c) || crate::executor::host_paths().egress_presets.contains(&c)
            },
            shell: p.shell_allowed,
        }
    }
}

/// MCP server ids named in `--mcp-config` values that are inline JSON (the
/// envelope's allow-list); config files are Ligus's own attach file.
fn inline_mcp_servers(args: &[String]) -> Vec<String> {
    let mut ids = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--mcp-config" {
            i += 1;
            while i < args.len() && !args[i].starts_with("--") {
                if let Ok(v) = serde_json::from_str::<Value>(&args[i]) {
                    if let Some(m) = v.get("mcpServers").and_then(Value::as_object) {
                        ids.extend(m.keys().cloned());
                    }
                }
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    ids
}

/// Confine a Ligus-run `claude -p` to the turn: `args` (the runtime's tier
/// and model, the envelope's flags, the Ligus MCP attach) plus the flags
/// below, and the env to add.
///
/// - `--strict-mcp-config` (and `ENABLE_CLAUDEAI_MCP_SERVERS=false`): only
///   the servers Ligus passed (its own and the chat's allow-listed ones),
///   never `~/.claude.json`, project `.mcp.json`, plugin servers or claude.ai
///   connectors. Left off only when the chat turned the `claude-connectors`
///   preset on (open egress): Claude Code loads connectors only without it,
///   so the user's own MCP servers load with them.
/// - `--setting-sources ""`, `disableAllHooks`, `--disable-slash-commands`:
///   no user, project or local settings (their hooks, permission rules,
///   enabled plugins, env), no skills or commands. With the workspace's
///   opt-in and open egress: the user's own settings (`user` only, never the
///   repo's), hooks, plugins and skills.
/// - `--tools`: the built-in allow-list ([`TURN_TOOLS`], plus [`WEB_TOOLS`]
///   while egress is open, plus `Skill` with the opt-in).
/// - `--disallowedTools`: [`OUT_OF_TURN_TOOLS`] always; the web tools and
///   the shell under restricted egress.
/// - `--allowedTools mcp__<id>`: pre-approves Ligus's own server (its tools
///   are authorized by Ligus per call) and, when the author gate allows
///   writes, the chat's allow-listed servers. Without it every MCP call is
///   refused in `-p` mode.
pub fn confine(args: &[String], c: Confine) -> (Vec<String>, Vec<(String, String)>) {
    let mut out = args.to_vec();
    let user = c.user_config && c.egress_open;
    let connectors = c.connectors && c.egress_open;
    let mut env: Vec<(String, String)> = CONFINE_ENV.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    if connectors {
        out.retain(|a| a != "--strict-mcp-config");
    } else {
        if !out.iter().any(|a| a == "--strict-mcp-config") {
            out.push("--strict-mcp-config".into());
        }
        // Also for a `claude` the agent starts from its shell.
        env.push(("ENABLE_CLAUDEAI_MCP_SERVERS".into(), "false".into()));
    }
    out.push("--setting-sources".into());
    out.push(if user { "user".into() } else { String::new() });
    if !user {
        out.push("--settings".into());
        out.push(serde_json::json!({ "disableAllHooks": true }).to_string());
        out.push("--disable-slash-commands".into());
    }
    let shell = c.egress_open && c.shell;
    let mut tools: Vec<&str> = TURN_TOOLS.iter().copied().filter(|t| shell || *t != "Bash").collect();
    if c.egress_open {
        tools.extend(WEB_TOOLS);
    }
    if user {
        tools.push("Skill");
    }
    out.push("--tools".into());
    out.push(tools.join(","));
    out.push("--disallowedTools".into());
    out.extend(OUT_OF_TURN_TOOLS.iter().map(|s| s.to_string()));
    if !c.egress_open {
        out.extend(WEB_TOOLS.iter().map(|s| s.to_string()));
    }
    if !shell {
        out.push("Bash".into());
    }
    let mut approve = vec![crate::executor::LIGUS_MCP_ID.to_string()];
    if c.writes_allowed {
        for id in inline_mcp_servers(args) {
            if !approve.contains(&id) {
                approve.push(id);
            }
        }
    }
    out.push("--allowedTools".into());
    out.extend(approve.iter().map(|id| format!("mcp__{id}")));
    (out, env)
}

/// Stream a reply from the Claude Code CLI. `program` is the binary (default
/// `claude`); `extra_args` are appended; `working_dir` is added via `--add-dir`
/// and used as the process cwd.
pub async fn stream_reply(
    program: &str,
    extra_args: &[String],
    working_dir: Option<&str>,
    extra_env: &[(String, String)],
    system: Option<&str>,
    turns: &[ChatTurn],
    mut on_delta: impl FnMut(String),
    // Live tool/thinking activity parsed from the same stream (Read/Bash/Edit,
    // their results, thinking). Ephemeral — surfaced under the generating bubble.
    mut on_activity: impl FnMut(StreamActivity),
) -> Result<String, ProviderError> {
    let program = if program.is_empty() { "claude" } else { program };

    let mut args: Vec<String> = vec![
        "-p".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--include-partial-messages".into(),
    ];
    if let Some(dir) = working_dir {
        args.push("--add-dir".into());
        args.push(dir.to_string());
    }
    let (confined, confine_env) = confine(extra_args, Confine::current());
    args.extend(confined);
    let mut env: Vec<(String, String)> = extra_env.to_vec();
    for (k, v) in confine_env {
        env.retain(|(x, _)| *x != k);
        env.push((k, v));
    }
    let extra_env = &env[..];

    // Confined to the turn's sandbox plan (see `executor`): its worktree,
    // the egress proxy, an allow-listed env (extra_env carries GIT_AUTHOR_*
    // so commits are credited to the requester). With no working dir it runs
    // in the turn's own scratch dir, never the app's cwd.
    let spec = crate::executor::SpawnSpec::new(crate::executor::Purpose::Agent(ligus_core::ModelProviderKind::ClaudeCode), program, &args)
        .cwd(working_dir.map(std::path::Path::new))
        .env(extra_env);
    let (mut cmd, _sandbox) = crate::executor::tokio_command(spec).map_err(|e| ProviderError::Subprocess(format!("sandbox {program}: {e}")))?;
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Kill the CLI if this future is dropped (e.g. the user hits Stop and the
        // caller's `select!` aborts the turn) — otherwise the subprocess would
        // keep generating, orphaned, after we stopped reading it.
        .kill_on_drop(true);
    super::subprocess::suppress_console_window(&mut cmd);

    tracing::debug!(target: "claude", %program, ?args, ?working_dir, "spawning claude");
    let mut child = cmd
        .spawn()
        .map_err(|e| ProviderError::Subprocess(format!("spawn {program}: {e}")))?;
    tracing::debug!(target: "claude", "claude spawned; streaming");

    if let Some(mut stdin) = child.stdin.take() {
        let prompt = render_prompt(system, turns);
        stdin
            .write_all(prompt.as_bytes())
            .await
            .map_err(|e| ProviderError::Subprocess(format!("write stdin: {e}")))?;
        // drop closes stdin → EOF
    }

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ProviderError::Subprocess("no stdout".into()))?;
    // Drain stderr concurrently. Otherwise a chatty CLI — `--verbose` can emit a
    // lot — fills the OS stderr pipe buffer (~64KB), which blocks the child's
    // next stderr write, which stops it writing stdout, which hangs our read
    // loop forever: the UI sits on "thinking" with no error and no output. We
    // read it in parallel and use the captured text for the failure path below.
    let stderr_task = child.stderr.take().map(|mut e| {
        tokio::spawn(async move {
            super::bounded::read_capped(&mut e, super::bounded::MAX_STDERR_BYTES).await
        })
    });
    let mut reader = super::bounded::BoundedLines::new(BufReader::new(stdout));

    // Idle timeout, not wall-clock. The cap exists to catch a runtime that streams
    // *nothing* (wedged on auth/input/a blocked tool) — not to bound how long real
    // work takes. So we time out only when NO line arrives for the idle window;
    // ANY stream-json line (a text delta OR a tool_use/tool_result/system event)
    // resets it, so a long-but-active turn — e.g. a multi-minute `cargo test` tool
    // run that emits no text — won't trip it, while a truly silent CLI still does.
    // Override with LIGUS_TURN_IDLE_TIMEOUT_SECS (shared with the HTTP
    // providers via `provider::turn_idle_timeout`). (#100)
    let idle = super::turn_idle_timeout();
    let mut assembled = String::new();
    let mut result_fallback: Option<String> = None;
    loop {
        let line = match tokio::time::timeout(idle, reader.next_line()).await {
            Ok(Ok(Some(line))) => line,
            Ok(Ok(None)) => break, // clean EOF
            Ok(Err(e)) => return Err(ProviderError::Subprocess(format!("claude: {e}"))),
            Err(_) => {
                // No output at all for the idle window — dropping the future kills
                // the CLI (kill_on_drop) once we return.
                return Err(ProviderError::Subprocess(format!(
                    "claude produced no output for {}s — it may be waiting on login/auth, input, or a blocked tool. Any activity (text or a tool call) resets this; a long but active build won't trip it. Verify with Settings \u{2192} Models \u{2192} Test, or raise LIGUS_TURN_IDLE_TIMEOUT_SECS.",
                    idle.as_secs()
                )));
            }
        };
        // A turn that interleaves narration with tool calls produces SEPARATE text
        // content blocks ("…download." before a tool, "Reading…" after its result).
        // Their deltas would otherwise concatenate with no separator, running the
        // two sentences together ("download.Reading"). Insert a newline at each new
        // text block so the narration segments stay on their own lines.
        if is_text_block_start(&line) && !assembled.is_empty() && !assembled.ends_with('\n') {
            assembled.push('\n');
            on_delta("\n".to_string());
        }
        if let Some(status) = ligus_mcp_status(&line).filter(|s| s != "connected") {
            // The prompt taught the Ligus tools; say in the log why the agent
            // doesn't have them (the bridge's own stderr goes to the CLI).
            tracing::warn!(target: "ligus_mcp", %status, "Claude Code did not connect the Ligus MCP server this turn");
            crate::agent_turn::note(format!("ligus_mcp_{status}:claude-code"));
        }
        if let Some(text) = extract_text_delta(&line) {
            assembled.push_str(&text);
            on_delta(text);
        } else if let Some(result) = extract_result(&line) {
            result_fallback = Some(result);
        } else {
            // Non-text line: surface any tool calls / results / thinking so the UI
            // can show what the agent is doing while it works.
            for act in extract_activity(&line) {
                on_activity(act);
            }
            if let Some(window) = extract_rate_limit(&line) {
                on_activity(StreamActivity::RateLimit { window });
            }
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| ProviderError::Subprocess(format!("wait: {e}")))?;
    let stderr = match stderr_task {
        Some(task) => task.await.unwrap_or_default(),
        None => String::new(),
    };
    if !status.success() {
        return Err(ProviderError::Subprocess(format!(
            "{program} exited with {status}: {}",
            stderr.trim()
        )));
    }

    // Prefer streamed text; fall back to the result line (older CLI / no
    // partial messages), emitting it once so the UI shows something.
    if assembled.is_empty() {
        if let Some(result) = result_fallback {
            if !result.is_empty() {
                on_delta(result.clone());
                return Ok(result);
            }
        }
    }
    Ok(assembled)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The command line a Ligus turn launches `claude` with, end to end
    /// through `stream_reply` (a fake `claude` that records its argv).
    #[cfg(unix)]
    async fn launched_argv(policy: crate::executor::TurnPolicy, args: &[String]) -> (Vec<String>, String) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("out")).unwrap();
        let out = dir.path().join("out/argv.txt");
        let envf = dir.path().join("out/env.txt");
        let fake = dir.path().join("claude");
        std::fs::write(&fake, format!("#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\nenv > '{}'\n", out.display(), envf.display())).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut a = vec!["--permission-mode".to_string(), "acceptEdits".to_string()];
        a.extend(args.iter().cloned());
        let policy = crate::executor::TurnPolicy { worktree_is_isolated: true, ..policy };
        let wd = dir.path().to_string_lossy().into_owned();
        let _ = crate::executor::with_turn(policy, stream_reply(fake.to_str().unwrap(), &a, Some(&wd), &[], None, &[], |_| {}, |_| {})).await;
        let argv = std::fs::read_to_string(&out).unwrap_or_default().lines().map(str::to_string).collect();
        (argv, std::fs::read_to_string(&envf).unwrap_or_default())
    }

    fn values_after(argv: &[String], flag: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < argv.len() {
            if argv[i] == flag {
                i += 1;
                while i < argv.len() && !argv[i].starts_with("--") {
                    out.extend(argv[i].split(',').map(str::to_string));
                    i += 1;
                }
            } else {
                i += 1;
            }
        }
        out
    }

    /// A Ligus turn gets only what the turn should have: no user or repo
    /// settings, hooks, plugins or skills, no MCP server Ligus didn't pass,
    /// and none of the tools that schedule or run work outside the turn
    /// (cloud routines, cron, background monitors, push, cross-session
    /// messaging, workflows).
    #[cfg(unix)]
    #[tokio::test]
    async fn default_turn_is_confined_to_the_turn() {
        if crate::executor::skip_without_sandbox("default_turn_is_confined_to_the_turn") {
            return;
        }
        let owner = crate::executor::TurnPolicy { shell_allowed: true, ..crate::executor::TurnPolicy::baseline(crate::executor::HostKind::Desktop) };
        let (argv, env) = launched_argv(owner, &[]).await;
        assert!(argv.contains(&"--strict-mcp-config".to_string()), "{argv:?}");
        assert!(argv.windows(2).any(|w| w[0] == "--setting-sources" && w[1].is_empty()), "{argv:?}");
        let i = argv.iter().rposition(|a| a == "--settings").expect("--settings");
        let v: Value = serde_json::from_str(&argv[i + 1]).unwrap();
        assert_eq!(v["disableAllHooks"], serde_json::json!(true));
        assert!(argv.contains(&"--disable-slash-commands".to_string()), "{argv:?}");
        let tools = values_after(&argv, "--tools");
        for t in ["Bash", "Read", "Edit", "Write", "Task", "WebSearch", "WebFetch"] {
            assert!(tools.contains(&t.to_string()), "{t} missing from {tools:?}");
        }
        let denied = values_after(&argv, "--disallowedTools");
        for t in ["RemoteTrigger", "CronCreate", "ScheduleWakeup", "Monitor", "PushNotification", "SendMessage", "Workflow", "DesignSync"] {
            assert!(!tools.contains(&t.to_string()), "{t} offered: {tools:?}");
            assert!(denied.contains(&t.to_string()), "{t} not denied: {denied:?}");
        }
        assert!(!tools.contains(&"Skill".to_string()));
        for var in ["CLAUDE_CODE_DISABLE_CRON=1", "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1", "CLAUDE_CODE_DISABLE_WORKFLOWS=1"] {
            assert!(env.lines().any(|l| l == var), "{var} not set");
        }
    }

    /// A workspace that opted in gets the user's own settings, hooks,
    /// plugins and skills back, never the out-of-turn tools; and only while
    /// egress is open.
    #[cfg(unix)]
    #[tokio::test]
    async fn opt_in_restores_user_setup_but_not_out_of_turn_tools() {
        if crate::executor::skip_without_sandbox("opt_in_restores_user_setup_but_not_out_of_turn_tools") {
            return;
        }
        let base = crate::executor::TurnPolicy::baseline(crate::executor::HostKind::Desktop);
        let (argv, _) = launched_argv(crate::executor::TurnPolicy { cli_user_config: true, ..base.clone() }, &[]).await;
        assert!(argv.windows(2).any(|w| w == ["--setting-sources", "user"]), "{argv:?}");
        assert!(!argv.contains(&"--disable-slash-commands".to_string()) && !argv.contains(&"--settings".to_string()), "{argv:?}");
        assert!(argv.contains(&"--strict-mcp-config".to_string()));
        assert!(values_after(&argv, "--tools").contains(&"Skill".to_string()));
        assert!(values_after(&argv, "--disallowedTools").contains(&"RemoteTrigger".to_string()));

        // Restricted egress: the opt-in is ignored and the web tools and the
        // shell go too.
        let env = ligus_core::sandbox::SandboxEnvelope::workspace_default().tighten(ligus_core::sandbox::TightenedBy::Dearth, ligus_core::sandbox::EgressPolicy::Closed, false);
        let (argv, _) = launched_argv(crate::executor::TurnPolicy { envelope: env, cli_user_config: true, ..base }, &[]).await;
        assert!(argv.windows(2).any(|w| w[0] == "--setting-sources" && w[1].is_empty()), "{argv:?}");
        let tools = values_after(&argv, "--tools");
        let denied = values_after(&argv, "--disallowedTools");
        for t in ["WebSearch", "WebFetch", "Bash", "Skill"] {
            assert!(!tools.contains(&t.to_string()), "{t} offered under restriction: {tools:?}");
        }
        for t in ["WebSearch", "WebFetch", "Bash"] {
            assert!(denied.contains(&t.to_string()), "{t} not denied under restriction");
        }
    }

    /// The `claude-connectors` preset is the one way claude.ai connectors
    /// reach a turn, and only under open egress.
    #[test]
    fn connectors_preset_lifts_strict_mcp_only_when_open() {
        let strict = vec!["--strict-mcp-config".to_string(), "--mcp-config".into(), r#"{"mcpServers":{}}"#.into()];
        let c = Confine { egress_open: true, user_config: false, writes_allowed: true, connectors: false, shell: true };
        let (a, env) = confine(&strict, c);
        assert_eq!(a.iter().filter(|x| *x == "--strict-mcp-config").count(), 1);
        assert!(env.contains(&("ENABLE_CLAUDEAI_MCP_SERVERS".to_string(), "false".to_string())));
        let (a, env) = confine(&strict, Confine { connectors: true, ..c });
        assert!(!a.contains(&"--strict-mcp-config".to_string()));
        assert!(!env.iter().any(|(k, _)| k == "ENABLE_CLAUDEAI_MCP_SERVERS"));
        assert!(values_after(&a, "--disallowedTools").contains(&"RemoteTrigger".to_string()));
        let (a, _) = confine(&strict, Confine { connectors: true, egress_open: false, ..c });
        assert!(a.contains(&"--strict-mcp-config".to_string()));
    }

    /// Review: a `claude` spawned with no turn in scope (the chat summarizer
    /// and compaction, a chat title, a runtime ping) reads prompts other
    /// members wrote, so it gets no shell: the shell is decided per turn, and
    /// with no turn there is no Owner behind it.
    #[test]
    fn no_turn_in_scope_means_no_shell() {
        assert!(crate::executor::current_turn().is_none());
        assert!(!Confine::current().shell, "a claude outside any turn keeps Bash");
        assert!(!crate::executor::turn_shell_allowed());
        assert!(!crate::executor::TurnPolicy::baseline(crate::executor::HostKind::Desktop).shell_allowed);
        // Nor writes, the web tools or MCP: it only has to answer in text.
        let c = Confine::current();
        assert!(!c.writes_allowed && !c.egress_open && !c.connectors, "{c:?}");
        let (a, _) = confine(&[], Confine::current());
        assert!(!values_after(&a, "--tools").iter().any(|t| t == "Bash"), "{a:?}");
        assert!(values_after(&a, "--disallowedTools").contains(&"Bash".to_string()), "{a:?}");
    }

    /// Phase C: without the shell (a turn the Owner didn't start, no grant)
    /// Bash is off the tool list and denied, under open egress too.
    #[test]
    fn no_shell_takes_bash_away() {
        let c = Confine { egress_open: true, user_config: false, writes_allowed: true, connectors: false, shell: true };
        let (a, _) = confine(&[], c);
        assert!(values_after(&a, "--tools").iter().any(|t| t.split(',').any(|x| x == "Bash")), "{a:?}");
        assert!(!values_after(&a, "--disallowedTools").contains(&"Bash".to_string()));
        let (a, _) = confine(&[], Confine { shell: false, ..c });
        assert!(!values_after(&a, "--tools").iter().any(|t| t.split(',').any(|x| x == "Bash")), "{a:?}");
        assert!(values_after(&a, "--disallowedTools").contains(&"Bash".to_string()), "{a:?}");
        // Restricted egress already took it; it's denied once.
        let (a, _) = confine(&[], Confine { shell: false, egress_open: false, ..c });
        assert_eq!(values_after(&a, "--disallowedTools").iter().filter(|t| *t == "Bash").count(), 1);
    }

    /// The owner's live test: the Ligus MCP server connected, but every
    /// call was refused in `-p` mode ("Claude requested permissions to use
    /// mcp__ligus__ligus_list_chats, but you haven't granted it yet").
    /// Ligus pre-approves its own server (its tools are gated by Ligus
    /// authz), and the chat's allow-listed servers, and nothing else.
    #[cfg(unix)]
    #[tokio::test]
    async fn ligus_and_chat_mcp_servers_are_pre_approved() {
        if crate::executor::skip_without_sandbox("ligus_and_chat_mcp_servers_are_pre_approved") {
            return;
        }
        let strict = vec!["--strict-mcp-config".to_string(), "--mcp-config".into(), r#"{"mcpServers":{"docs":{"type":"http","url":"https://docs.example"}}}"#.into()];
        let (argv, _) = launched_argv(crate::executor::TurnPolicy::baseline(crate::executor::HostKind::Desktop), &strict).await;
        let allowed = values_after(&argv, "--allowedTools");
        assert!(allowed.contains(&"mcp__ligus".to_string()), "{argv:?}");
        assert!(allowed.contains(&"mcp__docs".to_string()), "{argv:?}");
        assert!(!allowed.iter().any(|t| t == "mcp__*" || t == "*"), "{argv:?}");
        // A read-only author gate (its worktree is read-only, so checked on
        // the constructed args): only Ligus's own server, whose tools Ligus
        // authorizes per call.
        let (argv, _) = confine(&strict, Confine { egress_open: true, user_config: false, writes_allowed: false, connectors: false, shell: true });
        let allowed = values_after(&argv, "--allowedTools");
        assert_eq!(allowed, vec!["mcp__ligus".to_string()], "{argv:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn claude_code_child_never_inherits_ligus_secrets() {
        use std::os::unix::fs::PermissionsExt;
        if crate::executor::skip_without_sandbox("claude_code_child_never_inherits_ligus_secrets") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // In a subdirectory: the Landlock tier can't add entries to the
        // worktree's top level.
        std::fs::create_dir_all(dir.path().join("out")).unwrap();
        let out = dir.path().join("out/env.txt");
        let fake = dir.path().join("claude");
        std::fs::write(&fake, format!("#!/bin/sh\nenv > '{}'\n", out.display())).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("LIGUS_SCRUBTEST_WORKSPACE_KEY", "e2ee-passphrase-value");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-parent-env-key");
        // A write tier, so the (sandboxed) fake may write its env file into
        // its working dir.
        let args = ["--permission-mode".to_string(), "acceptEdits".to_string()];
        // In a write turn (outside one, a CLI is read-only).
        let turn = crate::executor::TurnPolicy::baseline(crate::executor::HostKind::Desktop);
        let _ = crate::executor::with_turn(turn, stream_reply(fake.to_str().unwrap(), &args, dir.path().to_str(), &[], None, &[], |_| {}, |_| {})).await;
        std::env::remove_var("LIGUS_SCRUBTEST_WORKSPACE_KEY");
        std::env::remove_var("ANTHROPIC_API_KEY");
        let env = std::fs::read_to_string(&out).unwrap();
        assert!(!env.contains("e2ee-passphrase-value"), "workspace key reached claude");
        assert!(!env.contains("sk-ant-parent-env-key"), "the parent's provider key reached claude");
    }

    /// End to end through the real provider path and this host's sandbox: a
    /// stub `claude` that tries to read Ligus's keys, settings and serve
    /// token, write outside its worktree and reach the network gets none of
    /// it, while it still writes its own worktree.
    #[cfg(unix)]
    #[tokio::test]
    async fn claude_turn_cannot_read_ligus_data_or_escape() {
        use std::os::unix::fs::PermissionsExt;
        if crate::executor::skip_without_sandbox("claude_turn_cannot_read_ligus_data_or_escape") {
            return;
        }
        let root = crate::executor::canonical(&tempfile::tempdir().unwrap().keep());
        let data = root.join("data");
        std::fs::create_dir_all(data.join("keys")).unwrap();
        std::fs::write(data.join("keys/device.seed"), "SEED-SECRET").unwrap();
        std::fs::write(data.join("settings.json"), "SETTINGS-SECRET").unwrap();
        std::fs::write(data.join("serve.token"), "SERVE-TOKEN-SECRET").unwrap();
        crate::executor::configure_host(&data, &[data.join("serve.token")], &[]);
        let wt = root.join("wt");
        std::fs::create_dir_all(wt.join("out")).unwrap();
        let fake = root.join("claude");
        let script = format!(
            "#!/bin/sh\n\
             cat '{d}/keys/device.seed' '{d}/settings.json' '{d}/serve.token' > out/report.txt 2>&1\n\
             echo escaped > '{r}/escape.txt' 2>> out/report.txt\n\
             (curl -s -m 3 --noproxy '*' https://example.com || echo no-network) >> out/report.txt 2>&1\n\
             echo done >> out/report.txt\n",
            d = data.display(),
            r = root.display()
        );
        std::fs::write(&fake, script).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let args = ["--permission-mode".to_string(), "acceptEdits".to_string()];
        let turn = crate::executor::TurnPolicy::baseline(crate::executor::HostKind::Desktop);
        let _ = crate::executor::with_turn(turn, stream_reply(fake.to_str().unwrap(), &args, wt.to_str(), &[], None, &[], |_| {}, |_| {})).await;
        let report = std::fs::read_to_string(wt.join("out/report.txt")).expect("the stub ran and wrote its worktree");
        assert!(report.contains("done"), "{report}");
        for secret in ["SEED-SECRET", "SETTINGS-SECRET", "SERVE-TOKEN-SECRET"] {
            assert!(!report.contains(secret), "{secret} reached the agent: {report}");
        }
        assert!(!root.join("escape.txt").exists(), "wrote outside its worktree");
        assert!(!report.contains("<html"), "reached the network directly: {report}");
    }

    /// The legacy adapter reads the CLI's stdout line by line: a 100 MiB
    /// line ends the turn at the line cap instead of growing the host's
    /// buffer.
    #[cfg(unix)]
    #[tokio::test]
    async fn oversized_output_line_ends_the_turn() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("claude");
        std::fs::write(
            &fake,
            "#!/bin/sh\nhead -c 104857600 /dev/zero | tr '\\0' x\necho\necho '{\"type\":\"result\",\"result\":\"ok\"}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let r = stream_reply(fake.to_str().unwrap(), &[], dir.path().to_str(), &[], None, &[], |_| {}, |_| {}).await;
        let err = r.expect_err("a 100 MiB line was accepted").to_string();
        assert!(err.contains("longer than"), "{err}");
    }

    #[test]
    fn parses_partial_message_text_delta() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}},"session_id":"x"}"#;
        assert_eq!(extract_text_delta(line).as_deref(), Some("Hel"));
        assert!(extract_result(line).is_none());
    }

    #[test]
    fn ignores_non_delta_events() {
        assert!(extract_text_delta(r#"{"type":"system","subtype":"init"}"#).is_none());
        assert_eq!(
            ligus_mcp_status(r#"{"type":"system","subtype":"init","mcp_servers":[{"name":"ligus","status":"failed","source":"dynamic"}]}"#).as_deref(),
            Some("failed")
        );
        assert_eq!(ligus_mcp_status(r#"{"type":"system","subtype":"init","mcp_servers":[]}"#), None);
        assert!(extract_text_delta(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#
        )
        .is_none());
        assert!(extract_text_delta("not json").is_none());
    }

    #[test]
    fn parses_result_fallback() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"Hello there","session_id":"x"}"#;
        assert_eq!(extract_result(line).as_deref(), Some("Hello there"));
        assert!(extract_text_delta(line).is_none());
    }

    #[test]
    fn extracts_tool_use_from_assistant_line() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Let me look"},{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"src/lib.rs"}}]}}"#;
        let acts = extract_activity(line);
        assert_eq!(acts.len(), 1, "text block ignored, tool_use captured");
        match &acts[0] {
            StreamActivity::Tool { id, name, input_json } => {
                assert_eq!(id, "toolu_1");
                assert_eq!(name, "Read");
                assert!(input_json.contains("src/lib.rs"));
            }
            other => panic!("expected Tool, got {other:?}"),
        }
    }

    #[test]
    fn extracts_tool_result_from_user_line_string_and_array() {
        let s = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","is_error":false,"content":"file contents"}]}}"#;
        match &extract_activity(s)[0] {
            StreamActivity::ToolResult { call_id, is_error, content } => {
                assert_eq!(call_id, "toolu_1");
                assert!(!is_error);
                assert_eq!(content, "file contents");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
        let arr = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","is_error":true,"content":[{"type":"text","text":"boom"}]}]}}"#;
        match &extract_activity(arr)[0] {
            StreamActivity::ToolResult { is_error, content, .. } => {
                assert!(is_error);
                assert_eq!(content, "boom");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn thinking_and_non_activity_lines() {
        let think = r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hmm"}]}}"#;
        assert!(matches!(extract_activity(think).as_slice(), [StreamActivity::Thinking { .. }]));
        // Text deltas, result lines, and junk carry no activity.
        assert!(extract_activity(r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"hi"}}}"#).is_empty());
        assert!(extract_activity(r#"{"type":"result","result":"done"}"#).is_empty());
        assert!(extract_activity("not json").is_empty());
    }

    #[test]
    fn parses_rate_limit_events() {
        use ligus_core::throttle::WindowStatus;
        let w = extract_rate_limit(r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","resetsAt":1900000000,"rateLimitType":"five_hour","utilization":0.86}}"#).unwrap();
        assert_eq!(w.status, WindowStatus::Warning);
        assert_eq!(w.utilization_pct, Some(86));
        assert_eq!(w.resets_at, Some(1_900_000_000));
        assert_eq!(w.window.as_deref(), Some("five_hour"));
        let r = extract_rate_limit(r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":1900000000000}}"#).unwrap();
        assert_eq!(r.status, WindowStatus::Rejected);
        assert_eq!(r.resets_at, Some(1_900_000_000), "milliseconds are normalized");
        assert!(extract_rate_limit(r#"{"type":"rate_limit_event","rate_limit_info":{"status":"mystery"}}"#).is_none());
        assert!(extract_rate_limit(r#"{"type":"result","result":"x"}"#).is_none());
    }

    #[test]
    fn parses_the_cli_rate_limit_event_shape() {
        use ligus_core::throttle::WindowStatus;
        // The full shape the CLI emits (uuid, session_id, per-window figures).
        let line = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1900000000,"rateLimitType":"seven_day","utilization":0.42,"isUsingOverage":false,"unifiedWindows":{"five_hour":{"utilization":0.9,"resetsAt":1800000000},"seven_day":{"utilization":0.42,"resetsAt":1900000000}}},"uuid":"u","session_id":"s"}"#;
        let w = extract_rate_limit(line).unwrap();
        assert_eq!((w.status, w.utilization_pct, w.resets_at, w.window.as_deref()), (WindowStatus::Allowed, Some(42), Some(1_900_000_000), Some("seven_day")));
        // Only status and the per-window figures: the busiest window is used.
        let line = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","unifiedWindows":{"five_hour":{"utilization":0.9,"resetsAt":1800000000},"seven_day":{"utilization":0.42,"resetsAt":1900000000}}},"uuid":"u","session_id":"s"}"#;
        let w = extract_rate_limit(line).unwrap();
        assert_eq!((w.status, w.utilization_pct, w.resets_at, w.window.as_deref()), (WindowStatus::Warning, Some(90), Some(1_800_000_000), Some("five_hour")));
        // Status alone is enough.
        let w = extract_rate_limit(r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected"},"uuid":"u","session_id":"s"}"#).unwrap();
        assert_eq!((w.status, w.utilization_pct, w.resets_at), (WindowStatus::Rejected, None, None));
    }

    #[test]
    fn detects_text_block_start_and_separates_narration() {
        let start = r#"{"type":"stream_event","event":{"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}}"#;
        assert!(is_text_block_start(start));
        // A tool_use block start is NOT a text boundary.
        let tool = r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t","name":"Read"}}}"#;
        assert!(!is_text_block_start(tool));
        // A delta / result / junk line is not a block start.
        assert!(!is_text_block_start(r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"x"}}}"#));
        assert!(!is_text_block_start("not json"));

        // Simulate the interleaved sequence: text "download." → tool → NEW text
        // block "Reading". The boundary re-inserts the dropped newline.
        let lines = [
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"…download."}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t","name":"Read"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Reading…"}}}"#,
        ];
        let mut assembled = String::new();
        for l in lines {
            if is_text_block_start(l) && !assembled.is_empty() && !assembled.ends_with('\n') {
                assembled.push('\n');
            }
            if let Some(t) = extract_text_delta(l) {
                assembled.push_str(&t);
            }
        }
        assert_eq!(assembled, "…download.\nReading…", "boundary newline re-inserted");
    }

    #[test]
    fn assembles_deltas_in_order() {
        let lines = [
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hel"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"lo"}}}"#,
            r#"{"type":"result","result":"Hello"}"#,
        ];
        let s: String = lines.iter().filter_map(|l| extract_text_delta(l)).collect();
        assert_eq!(s, "Hello");
    }
}

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
pub fn extract_rate_limit(line: &str) -> Option<hive_core::throttle::PlanWindow> {
    use hive_core::throttle::{PlanWindow, WindowStatus};
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
    args.extend(extra_args.iter().cloned());

    // Confined to the turn's sandbox plan (see `executor`): its worktree,
    // the egress proxy, an allow-listed env (extra_env carries GIT_AUTHOR_*
    // so commits are credited to the requester). With no working dir it runs
    // in the turn's own scratch dir, never the app's cwd.
    let spec = crate::executor::SpawnSpec::new(crate::executor::Purpose::Agent(hive_core::ModelProviderKind::ClaudeCode), program, &args)
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
        let _ = stream_reply(fake.to_str().unwrap(), &args, dir.path().to_str(), &[], None, &[], |_| {}, |_| {}).await;
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
        let _ = stream_reply(fake.to_str().unwrap(), &args, wt.to_str(), &[], None, &[], |_| {}, |_| {}).await;
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
        use hive_core::throttle::WindowStatus;
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
        use hive_core::throttle::WindowStatus;
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

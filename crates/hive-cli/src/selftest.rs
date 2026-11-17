//! `ligus doctor --turn-test`: run one real headless agent turn and check
//! what its sandbox lets it do.
//!
//! The turn is the same path `ligus worker` and `ligus serve` take for an
//! HTTP model with local tools ([`crate::reply::generate_reply_streaming`]):
//! a Custom (OpenAI-wire) runtime, Ligus's built-in tools with `run_command`
//! on, a per-turn git worktree, and every command spawned through this host's
//! agent sandbox. The model is a stub served in process on loopback: its first
//! answer calls `run_command` with a probe script, its second ends the turn.
//! The probe's output (the tool result the stub receives back) must show
//!
//! - that the command ran at all (a sandbox is present and works);
//! - that it could not read `/proc/1/environ` (the container's init, whose
//!   environment is the pod's) nor `/proc/<this process>/environ`;
//! - that it could not list this host's data dir, nor read its secret files.
//!
//! Exit codes are in `cmd_doctor`. Used by the Helm chart's `helm test` and
//! by the kind end-to-end workflow, which also asserts the tier.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context, Result};
use axum::body::Body;
use axum::extract::State;
use axum::routing::post;
use axum::Router;
use hive_core::ModelProviderKind;
use hive_runtime::{ChatTurn, ResolvedRuntime};
use serde_json::{json, Value};

use crate::reply::{self, ToolSettings, TurnFiles};

const RAN: &str = "LIGUS_PROBE_RAN";
const LEAK: &str = "LIGUS_PROBE_LEAK";

/// The result of one probe turn.
pub struct TurnReport {
    pub tier: &'static str,
    /// Human-readable findings, one per check.
    pub lines: Vec<String>,
    pub ran: bool,
    pub leaks: Vec<String>,
}

impl TurnReport {
    pub fn passed(&self) -> bool {
        self.ran && self.leaks.is_empty()
    }
}

/// The shell script the stub model asks `run_command` to run. Each check
/// prints a marker; `LEAK_*` markers mean the sandbox let something through.
fn probe_script(parent_pid: u32, data_dir: &Path, secrets: &[PathBuf]) -> String {
    let q = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
    let mut s = String::new();
    s.push_str(&format!("echo {RAN}\n"));
    for pid in ["1".to_string(), parent_pid.to_string()] {
        s.push_str(&format!(
            "n=$(cat /proc/{pid}/environ 2>/dev/null | wc -c); if [ \"$n\" -gt 0 ]; then echo {LEAK}_ENVIRON_{pid}; else echo DENIED_ENVIRON_{pid}; fi\n"
        ));
    }
    s.push_str(&format!(
        "if ls {} >/dev/null 2>&1; then echo {LEAK}_DATA_DIR; else echo DENIED_DATA_DIR; fi\n",
        q(data_dir)
    ));
    for (i, f) in secrets.iter().enumerate() {
        s.push_str(&format!(
            "if [ -e {p} ] && cat {p} >/dev/null 2>&1; then echo {LEAK}_SECRET_{i}; else echo DENIED_SECRET_{i}; fi\n",
            p = q(f)
        ));
    }
    s
}

#[derive(Clone)]
struct Stub {
    script: Arc<String>,
    bodies: Arc<Mutex<Vec<Value>>>,
}

fn sse(d: Value) -> String {
    format!("data: {}\n\n", json!({"choices": [{"index": 0, "delta": d}]}))
}

/// OpenAI-wire chat completions, streamed: round one calls `run_command`,
/// every later round answers with text.
async fn stub_chat(State(s): State<Stub>, body: String) -> axum::response::Response {
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let n = {
        let mut b = s.bodies.lock().unwrap_or_else(|e| e.into_inner());
        b.push(v);
        b.len() - 1
    };
    let mut out = String::new();
    if n == 0 {
        let args = json!({ "command": s.script.as_str(), "timeout_secs": 30 }).to_string();
        out.push_str(&sse(json!({"tool_calls": [{"index": 0, "id": "probe-1", "type": "function", "function": {"name": "run_command", "arguments": args}}]})));
    } else {
        out.push_str(&sse(json!({"content": "probe done"})));
    }
    out.push_str("data: [DONE]\n\n");
    axum::response::Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from(out))
        .unwrap_or_default()
}

fn git(dir: &Path, args: &[&str]) -> Result<()> {
    let out = hive_core::process_util::command("git").arg("-C").arg(dir).args(args).output().context("running git")?;
    if !out.status.success() {
        bail!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

/// The text of the tool result the stub received back.
fn tool_output(bodies: &[Value]) -> Option<String> {
    bodies.iter().flat_map(|b| b["messages"].as_array().cloned().unwrap_or_default()).find_map(|m| {
        (m["role"] == "tool").then(|| match &m["content"] {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
    })
}

/// The secret files this process loaded (`*_FILE`, `LIGUS_WS_SECRET_DIR`),
/// which the turn must not read.
fn secret_files() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = crate::hardening::FILE_SECRETS
        .iter()
        .filter_map(|n| crate::env_opt(&format!("{n}_FILE")))
        .map(PathBuf::from)
        .collect();
    if let Some(d) = crate::env_opt(crate::hardening::WS_SECRET_DIR) {
        if let Ok(rd) = std::fs::read_dir(&d) {
            v.extend(rd.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| !n.to_string_lossy().starts_with('.'))));
        }
    }
    v
}

pub async fn turn_test(data_dir: &Path) -> Result<TurnReport> {
    let tier = hive_runtime::executor::sandbox_status().tier();
    let secrets = secret_files();
    let script = probe_script(std::process::id(), data_dir, &secrets);
    let stub = Stub { script: Arc::new(script), bodies: Default::default() };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let app = Router::new().route("/v1/chat/completions", post(stub_chat)).with_state(stub.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    // A throwaway repository for the turn's worktree, outside the data dir.
    let tmp = std::env::temp_dir().join(format!("ligus-turn-test-{}", uuid::Uuid::new_v4()));
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo)?;
    let result = async {
        git(&repo, &["init", "-q"])?;
        std::fs::write(repo.join("README.md"), "turn test\n")?;
        git(&repo, &["add", "-A"])?;
        git(&repo, &["-c", "user.email=turn-test@ligus.invalid", "-c", "user.name=turn-test", "commit", "-qm", "init"])?;

        let rt = ResolvedRuntime {
            provider: ModelProviderKind::Custom,
            model: "turn-test-stub".into(),
            endpoint: format!("http://{addr}/v1/chat/completions"),
            api_key: None,
            args: Vec::new(),
            model_provider_id: None,
            model_base_url: None,
            context_window_tokens: None,
            keep_alive: None,
            think: None,
            tools: true,
            max_tool_rounds: Some(3),
            allow_remote_file_tools: false,
            driver: Default::default(),
        };
        let settings = ToolSettings { run_command: true, ..ToolSettings::default() };
        let files = TurnFiles {
            repo_root: Some(repo.clone()),
            chat_id: uuid::Uuid::new_v4().to_string(),
            turn_key: uuid::Uuid::new_v4().to_string(),
        };
        let reply = reply::generate_reply_streaming(
            &rt,
            "You are the Ligus sandbox turn test.",
            &[ChatTurn::user("Run the probe.")],
            true,
            None,
            Some(&files),
            &settings,
            |_| {},
        )
        .await?;
        if let Some(wt) = reply.worktree {
            let _ = wt.remove();
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    server.abort();
    let _ = std::fs::remove_dir_all(&tmp);
    result?;

    let bodies = stub.bodies.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if bodies.is_empty() {
        return Err(anyhow!("the turn never called the stub model"));
    }
    let offered = bodies[0]["tools"].as_array().is_some_and(|t| t.iter().any(|t| t["function"]["name"] == "run_command"));
    let out = tool_output(&bodies).unwrap_or_default();
    let ran = out.lines().any(|l| l.trim() == RAN);
    let leaks: Vec<String> = out.lines().filter(|l| l.trim().starts_with(LEAK)).map(|l| l.trim().to_string()).collect();
    let mut lines = vec![format!("agent sandbox: {tier}")];
    lines.push(format!("run_command offered: {}", if offered { "yes" } else { "no" }));
    lines.push(format!("run_command ran: {}", if ran { "yes" } else { "no" }));
    for l in out.lines().map(str::trim).filter(|l| l.starts_with("DENIED_") || l.starts_with(LEAK)) {
        lines.push(l.to_string());
    }
    if !ran {
        lines.push(format!("tool output: {}", out.chars().take(600).collect::<String>()));
    }
    Ok(TurnReport { tier, lines, ran, leaks })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_script_quotes_paths_and_marks_each_check() {
        let s = probe_script(42, Path::new("/var/lib/li'gus"), &[PathBuf::from("/etc/ligus/secrets/key")]);
        assert!(s.contains("/proc/1/environ") && s.contains("/proc/42/environ"));
        assert!(s.contains("'/var/lib/li'\\''gus'"));
        assert!(s.contains("LIGUS_PROBE_LEAK_SECRET_0"));
    }

    #[test]
    fn tool_output_reads_only_the_tool_message() {
        let bodies = vec![
            json!({"messages": [{"role": "user", "content": "x"}]}),
            json!({"messages": [
                {"role": "assistant", "tool_calls": [{"function": {"arguments": "echo LIGUS_PROBE_LEAK_X"}}]},
                {"role": "tool", "content": "LIGUS_PROBE_RAN\nDENIED_DATA_DIR"}
            ]}),
        ];
        assert_eq!(tool_output(&bodies).unwrap(), "LIGUS_PROBE_RAN\nDENIED_DATA_DIR");
    }

    /// The whole probe on this machine's own sandbox (Seatbelt on macOS,
    /// bwrap or Landlock on Linux). Skips where there is none.
    #[tokio::test]
    async fn a_real_turn_runs_confined() {
        if hive_runtime::executor::sandbox_status().kind == hive_runtime::executor::ExecutorKind::Unconfined {
            eprintln!("SKIPPED: no agent sandbox on this host");
            return;
        }
        let data = tempfile::tempdir().unwrap();
        std::fs::write(data.path().join("hive.db"), "secret").unwrap();
        hive_runtime::executor::configure_host(data.path(), &[], &[]);
        let r = turn_test(data.path()).await.unwrap();
        assert!(r.ran, "{:?}", r.lines);
        assert!(r.leaks.is_empty(), "{:?}", r.lines);
    }
}

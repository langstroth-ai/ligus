//! `ligus doctor --turn-test`: run one real headless agent turn and check
//! what its sandbox lets it do.
//!
//! The turn is the same path `ligus worker` and `ligus serve` take for an
//! HTTP model with local tools (the turn engine, `hive_runtime::turn_engine`):
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
fn probe_script(parent_pid: u32, host_pid_ns: &str, data_dir: &Path, secrets: &[PathBuf]) -> String {
    let q = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
    let mut s = String::new();
    s.push_str(&format!("echo {RAN}\n"));
    // A pid in the host's pid namespace: under bubblewrap's own namespace
    // pid 1 is bubblewrap (whose environment is the turn's), and the host's
    // pids name nothing, so a readable environ there is no leak.
    s.push_str(&format!("hostns={}; myns=$(readlink /proc/self/ns/pid 2>/dev/null)\n", q(Path::new(host_pid_ns))));
    for pid in ["1".to_string(), parent_pid.to_string()] {
        s.push_str(&format!(
            "n=$(cat /proc/{pid}/environ 2>/dev/null | wc -c); if [ \"$n\" -gt 0 ] && [ \"$myns\" = \"$hostns\" ]; then echo {LEAK}_ENVIRON_{pid}; else echo DENIED_ENVIRON_{pid}; fi\n"
        ));
    }
    // Listing the data dir: an empty tmpfs mounted over it (bubblewrap) lists
    // fine and shows nothing; any entry it shows is a leak.
    s.push_str(&format!(
        "if [ -n \"$(ls -A {} 2>/dev/null)\" ]; then echo {LEAK}_DATA_DIR; else echo DENIED_DATA_DIR; fi\n",
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

/// The secret files this process loaded (`*_FILE`, `LIGUS_WS_SECRET_DIR`,
/// the secret-store mount `LIGUS_SECRETS_DIR`),
/// which the turn must not read.
fn secret_files() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = crate::hardening::FILE_SECRETS
        .iter()
        .filter_map(|n| crate::env_opt(&format!("{n}_FILE")))
        .map(PathBuf::from)
        .collect();
    let k8s_default = crate::env_opt("LIGUS_SECRET_BACKEND")
        .filter(|b| b.trim().eq_ignore_ascii_case("k8s"))
        .map(|_| hive_runtime::secret_store::K8S_SECRETS_DIR_DEFAULT.to_string());
    let dirs = [
        crate::env_opt(crate::hardening::WS_SECRET_DIR),
        crate::env_opt(crate::hardening::SECRETS_DIR).or(k8s_default),
    ];
    for d in dirs.into_iter().flatten() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            v.extend(rd.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| !n.to_string_lossy().starts_with('.'))));
        }
    }
    v
}

pub async fn turn_test(data_dir: &Path) -> Result<TurnReport> {
    let tier = hive_runtime::executor::sandbox_status().tier();
    let secrets = secret_files();
    let host_pid_ns = std::fs::read_link("/proc/self/ns/pid").map(|p| p.display().to_string()).unwrap_or_default();
    let script = probe_script(std::process::id(), &host_pid_ns, data_dir, &secrets);
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
            gateway: None,
        };
        // The device owner's own probe, with no other member's words in it:
        // a turn whose shell is on (outside a turn there is none). It runs
        // through the turn engine, in a throwaway chat.
        let policy = hive_runtime::executor::TurnPolicy { shell_allowed: true, ..hive_runtime::executor::TurnPolicy::baseline(hive_runtime::executor::HostKind::Desktop) };
        let (svc, ws, chat) = probe_chat()?;
        let session = svc.lock().unwrap_or_else(|e| e.into_inner()).load(chat)?.ok_or_else(|| anyhow!("probe chat missing"))?;
        let host = ProbeHost { svc, repo: repo.clone() };
        let input = hive_runtime::turn_engine::TurnInput {
            workspace_id: ws,
            chat_id: chat,
            session,
            reply_to: None,
            responder: hive_runtime::turn_engine::Responder {
                author: "turn-test".into(),
                effect_author: "turn-test".into(),
                runtime_id: "turn-test".into(),
                agent_id: None,
                runtime: rt,
            },
            system: "You are the Ligus sandbox turn test.".into(),
            turns: vec![ChatTurn::user("Run the probe.")],
            gate: hive_runtime::author_gate::GatePlan::Full,
            envelope: policy.envelope.clone(),
            shell_allowed: true,
            policy,
            executor: hive_runtime::executor::default_executor(),
            tools: hive_runtime::turn_engine::ToolOptions { builtin: true, run_command: true, ..Default::default() },
            write_capable: false,
            require_isolation: true,
            stop: None,
            timeout: hive_runtime::turn_engine::turn_timeout(),
            options: Default::default(),
        };
        hive_runtime::turn_engine::run(&host, input).await.map_err(|e| anyhow!("{e}"))?;
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

/// A throwaway chat service and chat for the probe turn.
fn probe_chat() -> Result<(Mutex<hive_runtime::ChatService>, uuid::Uuid, uuid::Uuid)> {
    let author = hive_core::ActorIdentity::new("turn-test".to_string(), "turn-test".to_string(), hive_core::ActorKind::Human);
    let kp = hive_core::SigningKeypair::generate()?;
    let account = hive_core::SigningKeypair::generate()?;
    let mut svc = hive_runtime::ChatService::new(hive_runtime::EventStore::open_in_memory()?, uuid::Uuid::new_v4(), kp, account, author);
    let ws = uuid::Uuid::new_v4();
    svc.ensure_workspace_config(ws)?;
    let chat = svc.create_chat("turn test", ws, "")?.id;
    Ok((Mutex::new(svc), ws, chat))
}

/// The probe's turn host: the throwaway chat, the probe repo.
struct ProbeHost {
    svc: Mutex<hive_runtime::ChatService>,
    repo: PathBuf,
}

impl hive_runtime::turn_engine::TurnHost for ProbeHost {
    fn service(&self) -> &Mutex<hive_runtime::ChatService> {
        &self.svc
    }
    fn checkout(&self, _s: &hive_core::ChatSession, _m: uuid::Uuid) -> impl std::future::Future<Output = std::result::Result<hive_runtime::turn_engine::Checkout, String>> + Send {
        let c = hive_runtime::turn_engine::Checkout { repo_root: Some(self.repo.clone()), ..Default::default() };
        async move { Ok(c) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_script_quotes_paths_and_marks_each_check() {
        let s = probe_script(42, "pid:[4026531836]", Path::new("/var/lib/li'gus"), &[PathBuf::from("/etc/ligus/secrets/key")]);
        assert!(s.contains("/proc/1/environ") && s.contains("/proc/42/environ"));
        assert!(s.contains("'/var/lib/li'\\''gus'"));
        assert!(s.contains("LIGUS_PROBE_LEAK_SECRET_0"));
    }

    /// Review: on the bubblewrap tier the probe reported leaks that weren't:
    /// pid 1 in the turn's own pid namespace is bubblewrap (its environ is
    /// the turn's), and `ls` of the data dir lists the empty tmpfs over it.
    /// Run the script itself: a data dir with nothing in it and a different
    /// pid namespace are no leak; entries, or the host's namespace with a
    /// readable environ, are.
    #[cfg(unix)]
    #[test]
    fn probe_script_reports_only_real_leaks() {
        let run = |s: &str| String::from_utf8_lossy(&std::process::Command::new("/bin/sh").arg("-c").arg(s).output().unwrap().stdout).into_owned();
        let empty = tempfile::tempdir().unwrap();
        let out = run(&probe_script(1, "pid:[not-this-namespace]", empty.path(), &[]));
        assert!(out.contains("LIGUS_PROBE_RAN") && !out.contains("LIGUS_PROBE_LEAK"), "{out}");
        let full = tempfile::tempdir().unwrap();
        std::fs::write(full.path().join("keys"), "x").unwrap();
        let out = run(&probe_script(1, "pid:[not-this-namespace]", full.path(), &[]));
        assert!(out.contains("LIGUS_PROBE_LEAK_DATA_DIR"), "{out}");
        #[cfg(target_os = "linux")]
        {
            // A child of ours, not this process: another test in this binary
            // (`hardening::not_dumpable_after_hardening`) may already have
            // made this process not dumpable, which hides its environ.
            let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
            let ns = std::fs::read_link("/proc/self/ns/pid").unwrap().display().to_string();
            let out = run(&probe_script(child.id(), &ns, empty.path(), &[]));
            let _ = child.kill();
            let _ = child.wait();
            assert!(out.contains(&format!("LIGUS_PROBE_LEAK_ENVIRON_{}", child.id())), "{out}");
        }
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
        // Linux: the sandbox re-executes a Ligus binary as its pre-exec
        // helper, and this test harness isn't one. Use the `ligus` or
        // `ligus-sandbox-helper` binary cargo built for this run (a workspace
        // test run builds the latter); without either, skip, or fail under
        // LIGUS_REQUIRE_SANDBOX=1.
        #[cfg(target_os = "linux")]
        {
            let exe = std::env::current_exe().ok();
            let Some(helper) = exe.as_deref().and_then(|e| hive_runtime::executor::linux::find_built_helper(e, &["ligus", "ligus-sandbox-helper"])) else {
                let why = "no ligus or ligus-sandbox-helper binary built near the test harness (run the workspace tests)";
                assert!(std::env::var("LIGUS_REQUIRE_SANDBOX").map_or(true, |v| v.trim() != "1"), "LIGUS_REQUIRE_SANDBOX=1 but {why}");
                eprintln!("SKIPPED: {why}");
                return;
            };
            hive_runtime::executor::set_helper_exe(helper);
        }
        let data = tempfile::tempdir().unwrap();
        std::fs::write(data.path().join("hive.db"), "secret").unwrap();
        hive_runtime::executor::configure_host(data.path(), &[], &[]);
        let r = turn_test(data.path()).await.unwrap();
        assert!(r.ran, "{:?}", r.lines);
        assert!(r.leaks.is_empty(), "{:?}", r.lines);
    }
}

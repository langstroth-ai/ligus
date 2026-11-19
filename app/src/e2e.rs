//! In-app live test: drives the app's own turn path end to end, with no
//! window. Built only with `--features e2e` and run by `tests/app_live.rs`
//! (a `harness = false` test, so the Tauri app is built on the main thread
//! as macOS requires).
//!
//! It builds a real Tauri `App` (from the app's own `tauri.conf.json`, with
//! the window list cleared, never run), a real [`AppState`] over a temp data
//! dir via [`build_state_in`], and a temp git repo as the workspace. Every
//! step calls the same command functions the UI invokes over IPC —
//! `add_runtime`, `create_chat`, `set_chat_runtime`, `add_remote_mcp_server`,
//! `set_mcp_bearer_token`, `add_vault`, `set_retrieval_settings`,
//! `set_utility_runtime`, `send_message`, `list_proposals`, `get_chat` — so
//! a turn goes through `send_message` → `run_turn` → `windowed_context` +
//! `run_prepared_turn` exactly as when the user presses Send.
//!
//! The chat runtimes point at a local recording proxy in front of the
//! gateway, so each scenario can assert what went over the wire (system
//! prompt bytes, passages on the latest user turn, summary and title
//! requests). The proxy forwards byte-for-byte and streams responses back.
//!
//! ```text
//! LIGUS_APP_LIVE_ENDPOINT=http://host:8080 \
//! LIGUS_APP_LIVE_MCP_URL=http://mcp-host:8090/mcp \
//! LIGUS_APP_LIVE_MCP_TOKEN_FILE=/path/to/0600-token-file \
//!   cargo test -p Hive --features e2e --test app_live -- --nocapture
//! ```
//!
//! Optional: `LIGUS_APP_LIVE_SCENARIOS` (default `a,b,c,d,e,f`),
//! `LIGUS_APP_LIVE_MODEL` (default `gemma4:12b`), `LIGUS_APP_LIVE_MODEL_B`
//! (`gpt-oss:20b`), `LIGUS_APP_LIVE_QUICK_MODEL` (`lfm2:8b`),
//! `LIGUS_APP_LIVE_EMBED_MODEL` (`embeddinggemma-300m`),
//! `LIGUS_APP_LIVE_RERANK_MODEL` (`bge-reranker-v2-m3`),
//! `LIGUS_APP_LIVE_KEEP=1` (keep the temp dirs).
//!
//! Without `LIGUS_APP_LIVE_ENDPOINT` only the offline checks run (no network):
//! the context meter's first poll on a fresh chat, which once deadlocked the
//! app on the `service` mutex.
//!
//! Scenarios: (a) a planted bug fixed by `gemma4` with the built-in tools,
//! captured as a fileDiff proposal that applies and passes the test, with no
//! worktree left behind; (b) the same with `gpt-oss`; (c) an rdnamcp
//! `search_docs` call through an HTTP MCP server holding a static bearer
//! token; (d) vault retrieval — passages on the latest user turn and a
//! byte-identical system prompt across turns; (e) a chat title from the
//! Quick tasks runtime; (f) compaction — one summary call, none on the next
//! turn, and the compacted facts recalled.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use uuid::Uuid;
use tauri::{Listener, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{AgentToolSettings, AppState, RetrievalSettings};

const TITLE_SYSTEM_PREFIX: &str = "Write a short, specific title";

/// Live-test configuration from the environment; `None` skips the run.
struct Config {
    upstream: String,
    model: String,
    model_b: String,
    quick_model: String,
    embed_model: String,
    rerank_model: String,
    mcp_url: Option<String>,
    mcp_token_file: Option<PathBuf>,
    scenarios: Vec<String>,
}

fn env(name: &str) -> Option<String> {
    hive_core::compat::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

impl Config {
    fn from_env() -> Option<Self> {
        let raw = env("LIGUS_APP_LIVE_ENDPOINT")?;
        // The proxy forwards paths as is, so the upstream is the server root.
        let mut upstream = raw.trim_end_matches('/').to_string();
        for suffix in ["/chat/completions", "/v1"] {
            if let Some(s) = upstream.strip_suffix(suffix) {
                upstream = s.to_string();
            }
        }
        Some(Self {
            upstream,
            model: env("LIGUS_APP_LIVE_MODEL").unwrap_or_else(|| "gemma4:12b".into()),
            model_b: env("LIGUS_APP_LIVE_MODEL_B").unwrap_or_else(|| "gpt-oss:20b".into()),
            quick_model: env("LIGUS_APP_LIVE_QUICK_MODEL").unwrap_or_else(|| "lfm2:8b".into()),
            embed_model: env("LIGUS_APP_LIVE_EMBED_MODEL").unwrap_or_else(|| "embeddinggemma-300m".into()),
            rerank_model: env("LIGUS_APP_LIVE_RERANK_MODEL").unwrap_or_else(|| "bge-reranker-v2-m3".into()),
            mcp_url: env("LIGUS_APP_LIVE_MCP_URL"),
            mcp_token_file: env("LIGUS_APP_LIVE_MCP_TOKEN_FILE").map(PathBuf::from),
            scenarios: env("LIGUS_APP_LIVE_SCENARIOS")
                .unwrap_or_else(|| "a,b,c,d,e,f".into())
                .split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect(),
        })
    }
}

// ---------------------------------------------------------------------------
// Recording proxy
// ---------------------------------------------------------------------------

/// One request that went through the proxy.
#[derive(Clone, Debug)]
struct Recorded {
    method: String,
    path: String,
    body: Value,
    status: u16,
    /// Raw response body (SSE for streamed requests).
    response: String,
    secs: f64,
}

impl Recorded {
    fn model(&self) -> &str {
        self.body["model"].as_str().unwrap_or("")
    }
    fn messages(&self) -> &[Value] {
        self.body["messages"].as_array().map(Vec::as_slice).unwrap_or(&[])
    }
    fn system(&self) -> Option<&str> {
        self.messages().first().filter(|m| m["role"] == "system").and_then(|m| m["content"].as_str())
    }
    fn is_chat(&self) -> bool {
        self.method == "POST" && self.path.ends_with("/chat/completions")
    }
    /// A chat-turn request (not a title or summary job): carries a system
    /// prompt that is neither the title nor the summary instruction.
    fn is_turn(&self) -> bool {
        self.is_chat()
            && self
                .system()
                .is_some_and(|s| !s.starts_with(TITLE_SYSTEM_PREFIX) && s != crate::DEFAULT_CONTEXT_SUMMARY_PROMPT)
    }
    fn is_summary(&self) -> bool {
        self.is_chat() && self.system() == Some(crate::DEFAULT_CONTEXT_SUMMARY_PROMPT)
    }
    fn is_title(&self) -> bool {
        self.is_chat() && self.system().is_some_and(|s| s.starts_with(TITLE_SYSTEM_PREFIX))
    }
    fn last_user(&self) -> Option<&str> {
        self.messages().iter().rev().find(|m| m["role"] == "user").and_then(|m| m["content"].as_str())
    }

    /// One line on what the model sent back: finish reason, text and
    /// reasoning lengths, tool calls — from a streamed (SSE) or plain body.
    fn summary(&self) -> String {
        let (mut text, mut reasoning, mut finish, mut tools) = (0usize, 0usize, String::new(), Vec::<String>::new());
        let mut usage = String::new();
        let bodies: Vec<Value> = if self.response.trim_start().starts_with('{') {
            serde_json::from_str(&self.response).into_iter().collect()
        } else {
            self.response
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .filter_map(|d| serde_json::from_str(d.trim()).ok())
                .collect()
        };
        for b in &bodies {
            if let Some(u) = b.get("usage").filter(|u| !u.is_null()) {
                usage = format!(" usage={u}");
            }
            for ch in b["choices"].as_array().into_iter().flatten() {
                let d = if ch.get("delta").is_some() { &ch["delta"] } else { &ch["message"] };
                text += d["content"].as_str().map_or(0, str::len);
                reasoning += d["reasoning_content"].as_str().map_or(0, str::len);
                for tc in d["tool_calls"].as_array().into_iter().flatten() {
                    if let Some(n) = tc["function"]["name"].as_str() {
                        tools.push(n.to_string());
                    }
                }
                if let Some(f) = ch["finish_reason"].as_str() {
                    finish = f.to_string();
                }
            }
        }
        format!(
            "{:>6.1}s HTTP {} msgs={} finish={finish:?} text={text}B reasoning={reasoning}B tools={tools:?}{usage}",
            self.secs,
            self.status,
            self.messages().len()
        )
    }
}

#[derive(Clone)]
struct Proxy {
    base: String,
    log: Arc<Mutex<Vec<Recorded>>>,
}

impl Proxy {
    fn mark(&self) -> usize {
        self.log.lock().unwrap().len()
    }
    fn since(&self, mark: usize) -> Vec<Recorded> {
        self.log.lock().unwrap()[mark..].to_vec()
    }
    fn chat_endpoint(&self) -> String {
        format!("{}/v1/chat/completions", self.base)
    }
}

/// Start the proxy on a loopback port. `GET /vault/*` is served locally
/// from `vault_doc`; everything else is forwarded to `upstream`.
async fn start_proxy(upstream: String, vault_doc: Arc<String>) -> Proxy {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
    let base = format!("http://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let client = hive_forge::http_client_builder().build().unwrap();
    let proxy = Proxy { base, log: log.clone() };
    tauri::async_runtime::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { continue };
            let (upstream, client, log, vault) = (upstream.clone(), client.clone(), log.clone(), vault_doc.clone());
            tauri::async_runtime::spawn(async move {
                if let Err(e) = proxy_one(sock, &upstream, &client, &log, &vault).await {
                    eprintln!("[proxy] connection error: {e}");
                }
            });
        }
    });
    proxy
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Serve one request, then close the connection (`connection: close`).
async fn proxy_one(
    mut sock: tokio::net::TcpStream,
    upstream: &str,
    client: &reqwest::Client,
    log: &Mutex<Vec<Recorded>>,
    vault: &str,
) -> std::io::Result<()> {
    let started = Instant::now();
    let mut buf = Vec::new();
    let mut tmp = [0u8; 16384];
    let head_end = loop {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(i) = find_subslice(&buf, b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let method = first.next().unwrap_or("GET").to_string();
    let path = first.next().unwrap_or("/").to_string();
    let mut content_length = 0usize;
    let mut forward_headers = Vec::new();
    for line in lines {
        let Some((k, v)) = line.split_once(':') else { continue };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_string());
        if k == "content-length" {
            content_length = v.parse().unwrap_or(0);
        }
        if matches!(k.as_str(), "content-type" | "authorization" | "accept") {
            forward_headers.push((k, v));
        }
    }
    while buf.len() < head_end + content_length {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = buf[head_end..(head_end + content_length).min(buf.len())].to_vec();

    if method == "GET" && path.starts_with("/vault/") {
        let reply = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/markdown\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            vault.len(),
            vault
        );
        sock.write_all(reply.as_bytes()).await?;
        log.lock().unwrap().push(Recorded {
            method,
            path,
            body: Value::Null,
            status: 200,
            response: String::new(),
            secs: started.elapsed().as_secs_f64(),
        });
        return Ok(());
    }

    let mut req = client.request(
        reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET),
        format!("{upstream}{path}"),
    );
    for (k, v) in &forward_headers {
        req = req.header(k.as_str(), v.as_str());
    }
    let record_body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut resp = match req.body(body).send().await {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("proxy upstream error: {e}");
            let reply = format!(
                "HTTP/1.1 502 Bad Gateway\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{msg}",
                msg.len()
            );
            sock.write_all(reply.as_bytes()).await?;
            log.lock().unwrap().push(Recorded {
                method,
                path,
                body: record_body,
                status: 502,
                response: msg,
                secs: started.elapsed().as_secs_f64(),
            });
            return Ok(());
        }
    };
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let head = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: {content_type}\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    );
    sock.write_all(head.as_bytes()).await?;
    let mut response = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                if response.len() < 400_000 {
                    response.extend_from_slice(&chunk);
                }
                sock.write_all(format!("{:x}\r\n", chunk.len()).as_bytes()).await?;
                sock.write_all(&chunk).await?;
                sock.write_all(b"\r\n").await?;
                sock.flush().await?;
            }
            Ok(None) => break,
            Err(e) => {
                eprintln!("[proxy] upstream body error: {e}");
                break;
            }
        }
    }
    sock.write_all(b"0\r\n\r\n").await?;
    sock.flush().await?;
    log.lock().unwrap().push(Recorded {
        method,
        path,
        body: record_body,
        status: status.as_u16(),
        response: String::from_utf8_lossy(&response).to_string(),
        secs: started.elapsed().as_secs_f64(),
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Ctx {
    app: tauri::AppHandle,
    proxy: Proxy,
    repo: PathBuf,
    cfg: Config,
    activity: Arc<Mutex<Vec<Value>>>,
}

/// One scenario's result line.
struct Outcome {
    name: &'static str,
    ok: bool,
    secs: f64,
    notes: Vec<String>,
}

/// Collects a scenario's checks: a failed check is noted, not panicked on,
/// so one scenario's failure doesn't hide the others' results.
struct Checks {
    notes: Vec<String>,
    failed: bool,
}

impl Checks {
    fn new() -> Self {
        Self { notes: Vec::new(), failed: false }
    }
    fn check(&mut self, ok: bool, what: impl Into<String>) {
        let what = what.into();
        eprintln!("    [{}] {what}", if ok { "ok" } else { "FAIL" });
        if !ok {
            self.failed = true;
        }
        self.notes.push(format!("{}{what}", if ok { "" } else { "FAIL: " }));
    }
    fn note(&mut self, what: impl Into<String>) {
        let what = what.into();
        eprintln!("    {what}");
        self.notes.push(what);
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().expect("git");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

const BUGGY_PRICING: &str = "\"\"\"Price helpers.\"\"\"\n\n\ndef apply_discount(price, percent):\n    \"\"\"Return price reduced by percent (0-100).\"\"\"\n    return price * percent / 100\n\n\ndef add_tax(price, rate):\n    return price * (1 + rate)\n";

/// A repo with a planted bug: `apply_discount` multiplies by the percentage
/// instead of taking it off. The test is right; the code is wrong.
fn planted_repo(dir: &Path) {
    std::fs::create_dir_all(dir.join("shop")).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "e2e@example.invalid"]);
    git(dir, &["config", "user.name", "e2e"]);
    std::fs::write(dir.join("README.md"), "# shop\n\nPricing helpers. Run `python3 test_pricing.py` to test.\n").unwrap();
    std::fs::write(dir.join("shop/__init__.py"), "").unwrap();
    std::fs::write(dir.join("shop/pricing.py"), BUGGY_PRICING).unwrap();
    std::fs::write(
        dir.join("test_pricing.py"),
        "from shop.pricing import apply_discount\nassert apply_discount(200, 25) == 150, apply_discount(200, 25)\nprint(\"ok\")\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "init"]);
}

/// Leftovers of a turn: registered worktrees other than the repo itself and
/// the chats' persistent base checkouts (`.hive/worktrees/chat-<id>`, kept
/// by design), plus any `hive/turn-*` branch.
fn turn_leftovers(repo: &Path) -> Vec<String> {
    let canon = |p: &Path| dunce::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let root = canon(repo);
    let mut left: Vec<String> = git(repo, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(|p| canon(Path::new(p)))
        .filter(|p| *p != root)
        .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("chat-")))
        .map(|p| p.display().to_string())
        .collect();
    left.extend(
        git(repo, &["branch", "--list", "hive/turn-*", "--format=%(refname:short)"])
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_string),
    );
    left
}

impl Ctx {
    fn state(&self) -> tauri::State<'_, AppState> {
        self.app.state::<AppState>()
    }

    #[allow(clippy::too_many_arguments)]
    fn add_runtime(&self, id: &str, model: &str, tools: bool, window: Option<u32>) {
        crate::add_runtime(
            self.state(),
            id.into(),
            format!("Gateway {model}"),
            "custom".into(),
            "local".into(),
            self.proxy.chat_endpoint(),
            model.into(),
            tools,
            false,
            None,
            None,
            window,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("add_runtime");
    }

    fn new_chat(&self, runtime_id: &str) -> String {
        let chat = crate::create_chat(self.state(), String::new()).expect("create_chat");
        crate::set_chat_runtime(self.state(), chat.id.clone(), runtime_id.into()).expect("set_chat_runtime");
        chat.id
    }

    /// Send a message exactly as the composer does; returns (result, secs).
    async fn send(&self, sid: &str, body: &str) -> (Result<(), String>, f64) {
        eprintln!("  > {}", body.chars().take(120).collect::<String>());
        let started = Instant::now();
        let r = crate::send_message(self.app.clone(), self.state(), sid.into(), body.into(), None).await;
        (r, started.elapsed().as_secs_f64())
    }

    fn chat(&self, sid: &str) -> hive_proto::ChatSessionDto {
        crate::get_chat(self.state(), sid.into()).expect("get_chat").expect("chat exists")
    }

    fn last_reply(&self, sid: &str) -> String {
        self.chat(sid)
            .messages
            .iter()
            .rev()
            .find(|m| m.role != "user" && m.role != "system")
            .map(|m| m.body.clone())
            .unwrap_or_default()
    }

    /// The context meter's command, bounded: it once deadlocked on the
    /// `service` mutex, which must fail this test rather than hang it.
    fn telemetry(&self, sid: &str) -> hive_proto::ContextTelemetryDto {
        let (tx, rx) = std::sync::mpsc::channel();
        let (app, sid) = (self.app.clone(), sid.to_string());
        std::thread::spawn(move || {
            let _ = tx.send(crate::get_context_telemetry(app.state::<AppState>(), sid));
        });
        rx.recv_timeout(Duration::from_secs(20))
            .expect("get_context_telemetry did not return within 20s (deadlock?)")
            .expect("get_context_telemetry")
            .expect("chat exists")
    }

    fn activity_for(&self, sid: &str) -> Vec<Value> {
        self.activity.lock().unwrap().iter().filter(|e| e["sessionId"] == sid).cloned().collect()
    }

    fn tool_calls(&self, sid: &str) -> Vec<(String, String)> {
        self.activity_for(sid)
            .iter()
            .filter(|e| e["kind"] == "tool")
            .map(|e| (e["name"].as_str().unwrap_or("").to_string(), e["inputJson"].as_str().unwrap_or("").to_string()))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// (a)/(b): a planted bug, fixed through the built-in tools, lands as a
/// reviewable fileDiff proposal; the main tree is untouched and no
/// worktree is left behind.
async fn scenario_fix_bug(ctx: &Ctx, runtime_id: &str, model: &str) -> Checks {
    let mut c = Checks::new();
    ctx.add_runtime(runtime_id, model, true, None);
    let sid = ctx.new_chat(runtime_id);
    let before = turn_leftovers(&ctx.repo);
    let mark = ctx.proxy.mark();
    let (r, secs) = ctx
        .send(
            &sid,
            "test_pricing.py fails: apply_discount in shop/pricing.py returns the wrong value. \
Find the bug and fix it in the code (do not change the test). Keep the fix minimal.",
        )
        .await;
    c.note(format!("turn took {secs:.1}s"));
    c.check(r.is_ok(), format!("send_message returned Ok ({:?})", r.err()));
    let calls = ctx.tool_calls(&sid);
    c.note(format!(
        "tool calls: {}",
        calls.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")
    ));
    for (n, req) in ctx.proxy.since(mark).iter().filter(|r| r.is_turn()).enumerate() {
        c.note(format!("round {}: {}", n + 1, req.summary()));
    }
    let reply = ctx.last_reply(&sid);
    c.note(format!("reply: {}", reply.chars().take(200).collect::<String>().replace('\n', " ")));
    c.check(!reply.starts_with("[error]"), "reply is not an error");
    c.check(!reply.trim().is_empty(), "reply is not empty");

    let proposals = crate::list_proposals(ctx.state(), sid.clone()).expect("list_proposals");
    let diffs: Vec<_> = proposals.iter().filter(|p| p.kind == "fileDiff").collect();
    c.check(
        diffs.len() == 1,
        format!(
            "exactly one fileDiff proposal (got {}: {:?})",
            diffs.len(),
            diffs.iter().map(|p| (&p.title, p.diff.is_some())).collect::<Vec<_>>()
        ),
    );
    let others: Vec<_> = proposals.iter().filter(|p| p.kind != "fileDiff").map(|p| (&p.kind, &p.title)).collect();
    if !others.is_empty() {
        c.note(format!("other proposals: {others:?}"));
    }
    if let Some(p) = diffs.first() {
        c.note(format!("proposal: \"{}\" status={} files={:?}", p.title, p.status, p.changed_files));
        c.check(p.status == "open", "proposal is open for review");
        c.check(
            p.changed_files.iter().any(|f| f == "shop/pricing.py"),
            "proposal touches shop/pricing.py",
        );
        c.check(
            !p.changed_files.iter().any(|f| f == "test_pricing.py"),
            "proposal leaves the test alone",
        );
        // Implement = apply the stored diff. Do it on a scratch clone and
        // run the test there, so the proposal is proven to be the fix.
        let diff = p.diff.clone().unwrap_or_default();
        let scratch = ctx.repo.with_file_name(format!("{}-apply-{runtime_id}", ctx.repo.file_name().unwrap().to_string_lossy()));
        let _ = std::fs::remove_dir_all(&scratch);
        git(ctx.repo.parent().unwrap(), &["clone", "-q", ctx.repo.to_str().unwrap(), scratch.to_str().unwrap()]);
        let patch = scratch.join("proposal.patch");
        std::fs::write(&patch, &diff).unwrap();
        let applied = std::process::Command::new("git")
            .arg("-C")
            .arg(&scratch)
            .args(["apply", "proposal.patch"])
            .output()
            .unwrap();
        c.check(applied.status.success(), format!("diff applies cleanly ({})", String::from_utf8_lossy(&applied.stderr).trim()));
        let test = std::process::Command::new("python3").arg("test_pricing.py").current_dir(&scratch).output().unwrap();
        c.check(
            test.status.success(),
            format!(
                "test passes with the proposal applied ({})",
                format!("{}{}", String::from_utf8_lossy(&test.stdout), String::from_utf8_lossy(&test.stderr)).trim()
            ),
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }
    let main_file = std::fs::read_to_string(ctx.repo.join("shop/pricing.py")).unwrap_or_default();
    c.check(main_file == BUGGY_PRICING, "main tree untouched (the change waits in review)");
    let after = turn_leftovers(&ctx.repo);
    c.check(
        before.is_empty() && after.is_empty(),
        format!("no turn worktree or branch left behind (before {before:?}, after {after:?})"),
    );
    let chat_dir = ctx.repo.join(".ligus/worktrees").join(format!("chat-{sid}"));
    c.note(format!("chat base checkout (persistent by design) present: {}", chat_dir.exists()));
    c
}

/// (c): rdnamcp as an HTTP MCP server with a static bearer token; the turn
/// calls `search_docs` and answers from it.
async fn scenario_mcp(ctx: &Ctx) -> Checks {
    let mut c = Checks::new();
    let (Some(url), Some(token_file)) = (ctx.cfg.mcp_url.clone(), ctx.cfg.mcp_token_file.clone()) else {
        c.check(false, "LIGUS_APP_LIVE_MCP_URL / LIGUS_APP_LIVE_MCP_TOKEN_FILE unset");
        return c;
    };
    let token = std::fs::read_to_string(&token_file).map(|t| t.trim().to_string()).unwrap_or_default();
    c.check(!token.is_empty(), "token file readable (value never printed)");
    crate::add_remote_mcp_server(ctx.state(), "rdnamcp".into(), url, None).expect("add_remote_mcp_server");
    let set = crate::set_mcp_bearer_token(ctx.state(), "rdnamcp".into(), token.clone()).expect("set_mcp_bearer_token");
    c.check(set, "bearer token stored via set_mcp_bearer_token");
    crate::set_mcp_enabled(ctx.state(), "rdnamcp".into(), true).expect("set_mcp_enabled");
    let listed = crate::list_mcp_servers(ctx.state()).expect("list_mcp_servers");
    let entry = listed.iter().find(|s| s.id == "rdnamcp");
    c.check(entry.is_some_and(|s| s.has_bearer_token && s.enabled), "list_mcp_servers: enabled, hasBearerToken");
    c.check(
        !serde_json::to_string(&listed).unwrap_or_default().contains(&token),
        "token never returned to the UI",
    );

    ctx.add_runtime("gw-main", &ctx.cfg.model, true, None);
    let sid = ctx.new_chat("gw-main");
    let (r, secs) = ctx
        .send(
            &sid,
            "Use the rdnamcp search_docs tool on the \"rdna-docs\" collection to look up what the RDNA \
documentation says about wave32 versus wave64 execution. Then answer in two or three sentences based on what \
the search returned.",
        )
        .await;
    c.note(format!("turn took {secs:.1}s"));
    c.check(r.is_ok(), format!("send_message returned Ok ({:?})", r.err()));
    let activity = ctx.activity_for(&sid);
    let calls = ctx.tool_calls(&sid);
    c.note(format!("tool calls: {}", calls.iter().map(|(n, i)| format!("{n} {i}")).collect::<Vec<_>>().join(" | ")));
    let search = calls.iter().find(|(n, _)| n.contains("search_docs"));
    c.check(search.is_some(), "search_docs was called");
    if let Some((_, input)) = search {
        c.check(input.contains("rdna-docs"), "on the rdna-docs collection");
    }
    let results: Vec<&Value> = activity.iter().filter(|e| e["kind"] == "result").collect();
    let ok_result = results
        .iter()
        .any(|e| e["isError"] == false && e["content"].as_str().is_some_and(|s| s.trim().len() > 40));
    c.check(ok_result, "a tool returned a non-error, non-empty result");
    let reply = ctx.last_reply(&sid);
    c.note(format!("reply: {}", reply.chars().take(300).collect::<String>().replace('\n', " ")));
    c.check(!reply.trim().is_empty() && !reply.starts_with("[error]"), "answered");
    c.check(
        reply.to_ascii_lowercase().contains("wave"),
        "answer is about waves",
    );
    // Leave MCP off for the later scenarios.
    crate::set_mcp_enabled(ctx.state(), "rdnamcp".into(), false).expect("disable mcp");
    c
}

const RUNBOOK: &str = "# Hive Lab Runbook\n\n\
## Nightly backups\n\
The nightly backup job runs at 02:30 UTC every day. It snapshots the Postgres cluster and the artifact store, \
then uploads the snapshots to the object-storage bucket named lab-snapshots-7781 in the eu-central region. \
Snapshots are kept for 21 days. If the job fails twice in a row, the on-call engineer is paged and must \
re-run it by hand with the backup-now script. Restores are tested on the first Monday of each month.\n\n\
## On-call rotation\n\
Weekday on-call follows the team calendar. Weekend on-call is always covered by the infrastructure pair, \
currently Odette Vasquez and Tomasz Lindqvist, who swap every other weekend. Pages go to the #lab-pager \
channel first; anything unacknowledged for 15 minutes escalates to a phone call.\n\n\
## GPU nodes\n\
The desktop node carries the RX 6900 XT and serves the 12B chat model with four slots. The small-form-factor \
node carries the RX 6700 XT and runs the 20B reasoning model with one slot. Thermal limits are set to 85 C; \
above that the gateway drains the node and stops routing new requests to it.\n\n\
## Network\n\
All lab services are reachable only over the tailnet. The gateway listens on port 8080; the MCP server on \
port 8090. Public ingress is disabled, and SSH requires a hardware key.\n";

/// (d): vault retrieval through the gateway's embedder + reranker: passages
/// ride on the latest user turn; the system prompt stays byte-identical.
async fn scenario_vault(ctx: &Ctx) -> Checks {
    let mut c = Checks::new();
    let v1 = format!("{}/v1", ctx.cfg.upstream);
    crate::set_retrieval_settings(
        ctx.state(),
        RetrievalSettings {
            embed_url: v1.clone(),
            embed_model: ctx.cfg.embed_model.clone(),
            rerank_url: v1,
            rerank_model: ctx.cfg.rerank_model.clone(),
            top_k: 3,
            candidates: 12,
        },
    )
    .expect("set_retrieval_settings");
    ctx.add_runtime("gw-main", &ctx.cfg.model, true, None);
    let sid = ctx.new_chat("gw-main");
    crate::add_vault(ctx.state(), sid.clone(), "https".into(), format!("{}/vault/runbook.md", ctx.proxy.base), None)
        .expect("add_vault");

    let questions = [
        ("What time does the nightly backup run, and which bucket are the snapshots uploaded to? Answer from the runbook.", ["02:30", "lab-snapshots-7781"]),
        ("Who covers weekend on-call? Answer from the runbook.", ["Odette", "Tomasz"]),
    ];
    let mut systems: Vec<String> = Vec::new();
    for (i, (q, facts)) in questions.iter().enumerate() {
        let mark = ctx.proxy.mark();
        let (r, secs) = ctx.send(&sid, q).await;
        c.note(format!("turn {} took {secs:.1}s", i + 1));
        c.check(r.is_ok(), format!("turn {}: send_message Ok ({:?})", i + 1, r.err()));
        let reqs = ctx.proxy.since(mark);
        let fetched = reqs.iter().any(|r| r.path.starts_with("/vault/"));
        if i == 0 {
            c.check(fetched, "vault fetched from its URL");
        }
        let Some(first) = reqs.iter().find(|r| r.is_turn()) else {
            c.check(false, format!("turn {}: a chat request went out", i + 1));
            continue;
        };
        let system = first.system().unwrap_or("").to_string();
        let last_user = first.last_user().unwrap_or("");
        c.check(
            last_user.contains(hive_runtime::retrieval::PASSAGES_HEADER),
            format!("turn {}: passages ride on the latest user turn", i + 1),
        );
        c.check(
            last_user.contains(facts[0]) && last_user.contains(facts[1]),
            format!("turn {}: the retrieved passages hold the answer ({} / {})", i + 1, facts[0], facts[1]),
        );
        c.check(
            !system.contains(hive_runtime::retrieval::PASSAGES_HEADER) && !system.contains("Reference vaults") && !system.contains("lab-snapshots-7781"),
            format!("turn {}: nothing from the vault in the system prompt", i + 1),
        );
        let earlier_users_clean = first
            .messages()
            .iter()
            .filter(|m| m["role"] == "user")
            .rev()
            .skip(1)
            .all(|m| !m["content"].as_str().unwrap_or("").contains(hive_runtime::retrieval::PASSAGES_HEADER));
        c.check(earlier_users_clean, format!("turn {}: earlier user turns carry no passages", i + 1));
        let reply = ctx.last_reply(&sid);
        c.note(format!("reply {}: {}", i + 1, reply.chars().take(200).collect::<String>().replace('\n', " ")));
        let hits = facts.iter().filter(|f| reply.contains(**f)).count();
        c.check(hits == 2, format!("turn {}: answer states both facts ({hits}/2)", i + 1));
        systems.push(system);
        if i == 0 {
            // A user reads the reply before typing again; by then the chat's
            // auto-title has landed. Wait for it so the next turn is planned
            // against the renamed chat, as in real use.
            let waited = Instant::now();
            while crate::is_default_title(&ctx.chat(&sid).title) && waited.elapsed() < Duration::from_secs(60) {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            c.note(format!("chat titled \"{}\" before turn 2", ctx.chat(&sid).title));
        }
    }
    if systems.len() == 2 {
        let same = systems[0] == systems[1];
        if !same {
            let diverge = systems[0].chars().zip(systems[1].chars()).take_while(|(a, b)| a == b).count();
            let window = |s: &str| s.chars().skip(diverge.saturating_sub(60)).take(140).collect::<String>().replace('\n', "\\n");
            c.note(format!("system prompts diverge at char {diverge}: 1st …{}… / 2nd …{}…", window(&systems[0]), window(&systems[1])));
        }
        c.check(same, format!("system prompt byte-identical across the two turns ({} bytes)", systems[0].len()));
    }
    // Leave retrieval off for the later scenarios.
    crate::set_retrieval_settings(
        ctx.state(),
        RetrievalSettings {
            embed_url: String::new(),
            embed_model: String::new(),
            rerank_url: String::new(),
            rerank_model: String::new(),
            top_k: 0,
            candidates: 0,
        },
    )
    .expect("reset retrieval");
    c
}

/// (e): a new chat's title comes from the Quick tasks runtime.
async fn scenario_title(ctx: &Ctx) -> Checks {
    let mut c = Checks::new();
    ctx.add_runtime("gw-main", &ctx.cfg.model, true, None);
    ctx.add_runtime("gw-quick", &ctx.cfg.quick_model, false, None);
    crate::set_utility_runtime(ctx.state(), Some("gw-quick".into())).expect("set_utility_runtime");
    let sid = ctx.new_chat("gw-main");
    c.check(crate::is_default_title(&ctx.chat(&sid).title), "new chat starts untitled");
    let mark = ctx.proxy.mark();
    let (r, secs) = ctx
        .send(&sid, "What is a good, short name for a Rust crate that parses TOML config files? One line, no tools needed.")
        .await;
    c.note(format!("turn took {secs:.1}s"));
    c.check(r.is_ok(), format!("send_message Ok ({:?})", r.err()));
    // The title is generated off the critical path; wait for it.
    let waited = Instant::now();
    let mut title = ctx.chat(&sid).title;
    while crate::is_default_title(&title) && waited.elapsed() < Duration::from_secs(60) {
        tokio::time::sleep(Duration::from_millis(250)).await;
        title = ctx.chat(&sid).title;
    }
    c.note(format!("title \"{title}\" landed {:.1}s after the turn", waited.elapsed().as_secs_f64()));
    c.check(!crate::is_default_title(&title), "chat got a title");
    let title_reqs: Vec<Recorded> = ctx.proxy.since(mark).into_iter().filter(|r| r.is_title()).collect();
    let quick: Vec<&Recorded> = title_reqs.iter().filter(|r| r.model() == ctx.cfg.quick_model).collect();
    c.check(
        quick.len() == 1 && quick[0].status == 200,
        format!("one title request to {} (HTTP {:?}, {:.2}s)", ctx.cfg.quick_model, quick.first().map(|r| r.status), quick.first().map_or(0.0, |r| r.secs)),
    );
    c.check(
        title_reqs.iter().all(|r| r.model() == ctx.cfg.quick_model),
        "no fallback title request to the chat model",
    );
    c.check(
        quick.first().is_some_and(|r| r.body.get("tools").is_none()),
        "title job carries no tools",
    );
    crate::set_utility_runtime(ctx.state(), None).expect("reset utility runtime");
    c
}

const FACTS: [(&str, &str); 3] = [
    ("Note for later: the deploy codename for the v3 launch is AMBER-FALCON-17. Please remember it.", "AMBER-FALCON-17"),
    ("Also: the staging database listens on port 6543, not the default.", "6543"),
    ("One more: Priya Raman is the release manager for v3; she signs off on every deploy.", "Priya"),
];

/// Filler discussion, long enough to push the facts out of the window.
fn filler(i: usize) -> (String, String) {
    let topics = [
        "the CSV importer's handling of quoted newlines",
        "whether the cache layer should use LRU or LFU eviction",
        "flaky integration tests in the payments module",
        "renaming the config keys to snake_case",
        "the logging format for the background workers",
        "pagination on the admin search endpoint",
        "the retry policy for outbound webhooks",
        "splitting the monolith's auth code into its own crate",
    ];
    let t = topics[i % topics.len()];
    (
        format!(
            "Item {i}: let's talk about {t}. I looked at it this morning and there are a few rough edges: the current \
behaviour is inconsistent between environments, the tests only cover the happy path, and the docs describe an older \
version. What would you do first, and what can wait until next sprint?"
        ),
        format!(
            "On {t} (item {i}): I'd start by pinning the behaviour with a couple of focused tests for the edge cases, then \
make the environments consistent behind one code path, and update the docs last once the behaviour is settled. The \
cosmetic cleanups and any refactor of the surrounding module can wait until next sprint; none of them block users."
        ),
    )
}

/// (f): a long chat past the window compacts once (one summary call); the
/// next turn reuses the summary (no call) and recalls the compacted facts.
async fn scenario_compaction(ctx: &Ctx) -> Checks {
    let mut c = Checks::new();
    let window = 8192;
    ctx.add_runtime("gw-small", &ctx.cfg.model, true, Some(window));
    let sid = ctx.new_chat("gw-small");
    let uuid = uuid::Uuid::parse_str(&sid).unwrap();
    // Seed the history the way turns land in the log: user messages from the
    // local actor, completed Hive replies.
    let seed = |user: &str, reply: &str| {
        let state = ctx.state();
        let mut svc = state.service.lock().unwrap();
        let ws = svc.load(uuid).unwrap().unwrap().workspace_id;
        let posted = svc.post_user_message(uuid, ws, user).unwrap();
        let mid = svc.begin_assistant_message(uuid, ws, "Hive", "gw-small", None, Some(posted.id)).unwrap();
        svc.complete_assistant_message(uuid, ws, mid, reply).unwrap();
    };
    for (fact, _) in FACTS {
        seed(fact, "Noted — I'll keep that in mind.");
    }
    let telemetry = || ctx.telemetry(&sid);
    let mut i = 0;
    // Stop as soon as the plan overflows: the next turn compacts, and the
    // overflow (just past the high watermark) fits one summary request.
    while telemetry().overflow_message_count == 0 && i < 200 {
        let (u, a) = filler(i);
        seed(&u, &a);
        i += 1;
    }
    let t = telemetry();
    c.note(format!(
        "seeded {} messages: history {} tok, budget {} tok, kept {} / overflow {} msgs (window {window})",
        t.message_count, t.history_tokens, t.history_budget_tokens, t.kept_message_count, t.overflow_message_count
    ));
    c.check(t.overflow_message_count > 0, "history overflows the window");

    let mark = ctx.proxy.mark();
    let (r, secs) = ctx.send(&sid, "Thanks. Of everything above, which item should we tackle first tomorrow? One sentence.").await;
    c.note(format!("turn 1 took {secs:.1}s"));
    c.check(r.is_ok(), format!("turn 1: send_message Ok ({:?})", r.err()));
    let reqs1 = ctx.proxy.since(mark);
    let summaries1 = reqs1.iter().filter(|r| r.is_summary()).count();
    c.check(summaries1 == 1, format!("turn 1 compacts with one summary call (got {summaries1})"));
    if let Some(s) = reqs1.iter().find(|r| r.is_summary()) {
        c.note(format!("summary request took {:.1}s", s.secs));
    }
    let first1 = reqs1.iter().find(|r| r.is_turn()).cloned();
    let after1 = telemetry();
    c.note(format!("after turn 1: strategy={}, kept {} / overflow {}", after1.summary_strategy, after1.kept_message_count, after1.overflow_message_count));

    let mark = ctx.proxy.mark();
    let (r, secs) = ctx
        .send(
            &sid,
            "Quick check from memory, no tools: what is the v3 deploy codename, which port does the staging database \
listen on, and who is the release manager?",
        )
        .await;
    c.note(format!("turn 2 took {secs:.1}s"));
    c.check(r.is_ok(), format!("turn 2: send_message Ok ({:?})", r.err()));
    let reqs2 = ctx.proxy.since(mark);
    let summaries2 = reqs2.iter().filter(|r| r.is_summary()).count();
    c.check(summaries2 == 0, format!("turn 2 does not re-summarize (got {summaries2})"));
    let first2 = reqs2.iter().find(|r| r.is_turn()).cloned();
    if let (Some(a), Some(b)) = (&first1, &first2) {
        // The summary rides as the first turn after the system prompt; the
        // prefix up to the end of turn 1's request should be unchanged.
        let (ma, mb) = (a.messages(), b.messages());
        c.check(
            ma.len() >= 2 && mb.len() >= 2 && ma[1] == mb[1],
            "summary turn byte-identical across turns",
        );
        let shared = ma.iter().zip(mb.iter()).take_while(|(x, y)| x == y).count();
        c.note(format!("turn 2's request shares its first {shared} of {} messages with turn 1's", ma.len()));
        c.check(a.system() == b.system(), "system prompt byte-identical across turns");
        // Facts must have left the kept window: recall can only come from the summary.
        let in_kept = mb.iter().skip(2).any(|m| m["content"].as_str().is_some_and(|s| s.contains("AMBER-FALCON-17") && s.starts_with("Note for later")));
        c.check(!in_kept, "the fact messages themselves were compacted away");
        let summary_has = mb.get(1).and_then(|m| m["content"].as_str()).map_or(0, |s| FACTS.iter().filter(|(_, k)| s.contains(k)).count());
        c.note(format!("summary turn carries {summary_has}/3 facts"));
    }
    let reply = ctx.last_reply(&sid);
    c.note(format!("reply: {}", reply.chars().take(300).collect::<String>().replace('\n', " ")));
    let recalled = FACTS.iter().filter(|(_, k)| reply.contains(k)).count();
    c.check(recalled == 3, format!("recalled {recalled}/3 compacted facts"));
    c
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// The app's own config and context, minus its window; the app is built
/// (managers, plugins, event system) but its event loop never runs. A real
/// [`AppState`] over `data_dir` is managed on it, with `repo` as the
/// workspace. Must be called on the main thread.
fn headless_app(data_dir: &Path, repo: &Path) -> tauri::App {
    let mut context = crate::app_context();
    context.config_mut().app.windows.clear();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .build(context)
        .expect("build headless app");
    let handle = app.handle().clone();
    let state = crate::build_state_in(&handle, data_dir.to_path_buf()).expect("build_state_in");
    handle.manage(state);
    crate::set_workspace_root(handle.state::<AppState>(), repo.to_string_lossy().into_owned())
        .expect("set_workspace_root");
    app
}

/// Offline checks of the real app state, run even without a gateway: the
/// context meter's first poll on a fresh chat (an empty summary cache) once
/// deadlocked the whole app on the `service` mutex. Returns failures.
fn run_offline() -> usize {
    let root = std::env::temp_dir().join(format!("hive-app-offline-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    planted_repo(&repo);
    std::env::set_current_dir(&repo).unwrap();
    let app = headless_app(&root.join("data"), &repo);
    let handle = app.handle().clone();
    let mut failed = 0;

    let sid = crate::create_chat(handle.state::<AppState>(), String::new()).expect("create_chat").id;
    // Bounded: a deadlock must fail the run, not hang it.
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let (handle, sid) = (handle.clone(), sid.clone());
        std::thread::spawn(move || {
            let _ = tx.send(crate::get_context_telemetry(handle.state::<AppState>(), sid));
        });
    }
    match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(Ok(Some(t))) => eprintln!("PASS context meter on a fresh chat ({} messages)", t.message_count),
        Ok(other) => {
            eprintln!("FAIL context meter on a fresh chat: {other:?}");
            failed += 1;
        }
        Err(_) => {
            // The command thread is stuck holding the mutex; exit hard.
            eprintln!("FAIL get_context_telemetry did not return within 20s (deadlock on the service mutex)");
            std::process::exit(1);
        }
    }
    failed += tauri::async_runtime::block_on(repos_checks(&handle, &root));
    drop(app);
    let _ = std::fs::remove_dir_all(&root);
    failed
}

// ---------------------------------------------------------------------------
// Workspace repos through the command layer (offline, local temp repos)
// ---------------------------------------------------------------------------

/// A local HTTP server that answers every request with a redirect to a
/// second local port, and whether anything reached that second port; call
/// the returned closure to stop both. Loopback only.
fn redirecting_server() -> (String, Arc<std::sync::atomic::AtomicBool>, impl FnOnce()) {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    let elsewhere = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let b = elsewhere.local_addr().unwrap().port();
    elsewhere.set_nonblocking(true).unwrap();
    let pinned = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let a = pinned.local_addr().unwrap().port();
    pinned.set_nonblocking(true).unwrap();
    let hit = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let (hit2, stop2) = (hit.clone(), stop.clone());
    let t = std::thread::spawn(move || {
        while !stop2.load(Ordering::SeqCst) {
            if let Ok((mut c, _)) = elsewhere.accept() {
                hit2.store(true, Ordering::SeqCst);
                let _ = c.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            }
            if let Ok((mut c, _)) = pinned.accept() {
                let _ = c.set_nonblocking(false);
                let mut buf = [0u8; 4096];
                let _ = c.read(&mut buf);
                let resp = format!("HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{b}/e2e/gamma.git/info/refs?service=git-upload-pack\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                let _ = c.write_all(resp.as_bytes());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    let stopper = move || {
        stop.store(true, Ordering::SeqCst);
        let _ = t.join();
    };
    (format!("http://127.0.0.1:{a}/e2e/gamma.git"), hit, stopper)
}

const CONFIG_MCP: &str = "[[mcp_servers]]\nid = \"{id}\"\ntransport = \"stdio\"\ncommand = \"{cmd}\"\nenabled = true\n";

fn mcp_entry(id: &str, cmd: &str) -> String {
    CONFIG_MCP.replace("{id}", id).replace("{cmd}", cmd)
}

/// A local "forge" (a bare repo) for `canonical` with `config` committed,
/// and, unless `checkout` is `None`, a checkout of it whose origin URL names
/// the canonical (fetches go through a `forge` remote).
fn forge_repo(root: &Path, slug: &str, canonical: &str, config: &str, checkout: Option<&Path>) -> PathBuf {
    let seed = root.join(format!("seed-{slug}"));
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", "main"]);
    git(&seed, &["config", "user.email", "e2e@example.invalid"]);
    git(&seed, &["config", "user.name", "e2e"]);
    std::fs::write(seed.join("README.md"), format!("# {slug}\n")).unwrap();
    std::fs::write(seed.join("ligus.config.toml"), config).unwrap();
    git(&seed, &["add", "-A"]);
    git(&seed, &["commit", "-qm", "init"]);
    let bare = root.join(format!("{slug}.git"));
    git(root, &["clone", "-q", "--bare", &seed.to_string_lossy(), &bare.to_string_lossy()]);
    if let Some(co) = checkout {
        git(root, &["clone", "-q", &bare.to_string_lossy(), &co.to_string_lossy()]);
        git(co, &["config", "user.email", "e2e@example.invalid"]);
        git(co, &["config", "user.name", "e2e"]);
        git(co, &["remote", "rename", "origin", "forge"]);
        git(co, &["remote", "add", "origin", &format!("https://{canonical}.git")]);
        git(co, &["fetch", "-q", "forge"]);
        git(co, &["update-ref", "refs/remotes/origin/main", "refs/remotes/forge/main"]);
    }
    bare
}

/// Trust, on this device, the repo-supplied items of `checkout` named in `ids`.
fn trust(data_dir: &Path, checkout: &Path, ids: &[&str]) {
    use hive_runtime::workspace_trust as wt;
    let store = wt::TrustStore::open(data_dir).unwrap();
    let root = wt::root_id(checkout);
    for item in wt::repo_items(checkout).into_iter().filter(|i| ids.contains(&i.id.as_str())) {
        store.enable(&root, item.kind, &item.digest, "").unwrap();
    }
}

fn mcp_ids(servers: &[hive_runtime::McpServerSpec]) -> Vec<String> {
    let mut v: Vec<String> = servers.iter().map(|s| s.id.clone()).collect();
    v.sort();
    v
}

/// Repos and checkouts through the Tauri command layer: the per-turn
/// catalog and trust (review F14), the turn gate, repo-addressed apply and
/// its conflict actions, and the desktop's clone by id. Returns failures.
async fn repos_checks(handle: &tauri::AppHandle, root: &Path) -> usize {
    let mut c = Checks::new();
    let state = || handle.state::<AppState>();
    let base = root.join("repos");
    std::fs::create_dir_all(&base).unwrap();
    let data_dir = state().data_dir.clone();

    // Two workspace repos checked out here, a third that isn't; each
    // carries repo-supplied MCP servers. "shared" is the same server in A
    // and B, trusted only for B's repo.
    let alpha = base.join("Alpha");
    let beta = base.join("Beta");
    forge_repo(&base, "alpha", "github.com/e2e/alpha", &format!("{}{}{}", mcp_entry("a-mcp", "mcp-a"), mcp_entry("a-untrusted", "mcp-evil"), mcp_entry("shared", "mcp-shared")), Some(&alpha));
    forge_repo(&base, "beta", "github.com/e2e/beta", &format!("{}{}", mcp_entry("b-mcp", "mcp-b"), mcp_entry("shared", "mcp-shared")), Some(&beta));
    let gamma_forge = forge_repo(&base, "gamma", "git.e2e.invalid/e2e/gamma", "", None);
    trust(&data_dir, &alpha, &["a-mcp"]);
    trust(&data_dir, &beta, &["b-mcp", "shared"]);
    // The workspace's bound folder gets a server of its own.
    let bound = PathBuf::from(state().workspace_repo_root());
    std::fs::write(bound.join("ligus.config.toml"), mcp_entry("bound-mcp", "mcp-bound")).unwrap();
    git(&bound, &["add", "ligus.config.toml"]);
    git(&bound, &["commit", "-qm", "config"]);
    trust(&data_dir, &bound, &["bound-mcp"]);

    let add = |input: &str, name: &str, path: Option<&Path>| {
        crate::repos_ui::add_workspace_repo(state(), input.into(), Some(name.into()), Some("github".into()), Some("main".into()), path.map(|p| p.to_string_lossy().into_owned()))
    };
    let (a_id, b_id, g_id) = match (
        add("github.com/e2e/alpha", "Alpha", Some(&alpha)),
        add("github.com/e2e/beta", "Beta", Some(&beta)),
        crate::repos_ui::add_workspace_repo(state(), "git.e2e.invalid/e2e/gamma".into(), Some("Gamma".into()), None, Some("main".into()), None),
    ) {
        (Ok(a), Ok(b), Ok(g)) => (a["id"].as_str().unwrap().to_string(), b["id"].as_str().unwrap().to_string(), g["id"].as_str().unwrap().to_string()),
        other => {
            c.check(false, format!("add workspace repos: {other:?}"));
            return 1;
        }
    };
    let chat = |title: &str, repo: Option<&str>| {
        let id = crate::create_chat(state(), title.into()).expect("create_chat").id;
        if let Some(r) = repo {
            crate::repos_ui::set_chat_workspace_repo(state(), id.clone(), Some(r.into()), None).expect("set_chat_workspace_repo");
        }
        Uuid::parse_str(&id).unwrap()
    };
    let chat_a = chat("on alpha", Some(&a_id));
    let chat_b = chat("on beta", Some(&b_id));
    let chat_c = chat("no repo", None);
    let chat_g = chat("on gamma", Some(&g_id));
    let load = |sid: Uuid| state().service.lock().unwrap().load(sid).unwrap().unwrap();

    // Paths as Ligus compares them (links resolved, no Windows verbatim prefix).
    let canon = |p: &Path| hive_runtime::repos::git::canonical(p);

    // ---- Per-turn catalogs and trust (F14) ----
    // The window views chat B: the global catalogs follow it.
    crate::set_active_chat(state(), Some(chat_b.to_string())).unwrap();
    let viewed = state().workspace_root.lock().unwrap().clone();
    c.check(mcp_ids(&state().combined_mcp_servers()) == ["b-mcp", "shared"], format!("the window's catalog follows chat B: {:?}", mcp_ids(&state().combined_mcp_servers())));
    // A turn in chat A sees only A's trusted servers: not B's, not the
    // bound folder's, and not "shared", which is trusted for B's repo only.
    match crate::turn_scope(&state(), &load(chat_a)) {
        Ok(scope) => {
            c.check(mcp_ids(&scope.catalog.mcp) == ["a-mcp"], format!("chat A's turn sees only A's trusted servers: {:?}", mcp_ids(&scope.catalog.mcp)));
            c.check(canon(Path::new(&scope.dir)).starts_with(canon(&alpha)), format!("chat A's turn runs in Alpha's chat worktree: {}", scope.dir));
            c.check(scope.repo.as_ref().is_some_and(|r| canon(&r.checkout) == canon(&alpha)), "chat A's turn is gated on Alpha's checkout");
        }
        Err(e) => c.check(false, format!("chat A's turn scope: {e}")),
    }
    // A chat without a repo: the bound folder's catalog, never the viewed repo's.
    match crate::turn_scope(&state(), &load(chat_c)) {
        Ok(scope) => c.check(mcp_ids(&scope.catalog.mcp) == ["bound-mcp"], format!("a chat without a repo sees the bound folder's servers: {:?}", mcp_ids(&scope.catalog.mcp))),
        Err(e) => c.check(false, format!("chat C's turn scope: {e}")),
    }
    // Resolving a turn changes nothing the window shows (root, catalogs).
    c.check(*state().workspace_root.lock().unwrap() == viewed, "a turn leaves the window's folder alone");
    c.check(mcp_ids(&state().combined_mcp_servers()) == ["b-mcp", "shared"], "a turn leaves the window's catalog alone");
    // Viewing chat A swaps the window (and its watcher) to A's folder.
    crate::set_active_chat(state(), Some(chat_a.to_string())).unwrap();
    c.check(mcp_ids(&state().combined_mcp_servers()) == ["a-mcp"], format!("viewing chat A shows A's catalog: {:?}", mcp_ids(&state().combined_mcp_servers())));
    c.check(state().workspace_root.lock().unwrap().contains("Alpha"), "the window's folder (and watcher) follow the viewed chat's checkout");

    // ---- The turn gate (5.3) ----
    let gated = crate::turn_scope(&state(), &load(chat_g));
    c.check(gated.as_ref().is_err(), format!("a chat whose repo isn't here never runs a turn: {:?}", gated.as_ref().err()));
    let chip = crate::repos_ui::chat_workspace_repo(state(), chat_g.to_string()).unwrap();
    c.check(chip["here"]["state"] != "present", format!("the chip says Gamma isn't here: {chip}"));
    let pinned = load(chat_a).repo.and_then(|r| r.base_commit).is_some();
    c.check(pinned, "the first turn pins chat A's base");

    // ---- Repo-addressed apply and its conflict actions ----
    let ws = load(chat_a).workspace_id;
    let file_diff = |title: &str, diff: &str| {
        // Made by an agent (an author's own vote never counts toward quorum).
        let mut p = hive_core::ActionProposal::new(title, hive_core::ProposalKind::FileDiff, "coder");
        p.diff = Some(diff.into());
        p.required_approvals = 1;
        let id = p.id;
        state().service.lock().unwrap().upsert_proposal(chat_a, ws, p).expect("upsert_proposal");
        crate::vote_proposal(state(), chat_a.to_string(), id.to_string(), true).expect("vote");
        id
    };
    let add_file = "diff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hello\n";
    let p1 = file_diff("add new.txt", add_file);
    let r = crate::implement_proposal(handle.clone(), state(), chat_a.to_string(), p1.to_string(), None).await;
    let chat_wt = hive_runtime::git_worktree::chat_worktree_path_of(&alpha, &chat_a.to_string()).unwrap();
    c.check(r.is_ok(), format!("an approved repo proposal applies: {:?}", r.err()));
    c.check(chat_wt.join("new.txt").exists(), "it landed on chat A's branch, in Alpha's checkout");
    c.check(!bound.join("new.txt").exists() && !alpha.join("new.txt").exists(), "never in the bound folder or Alpha's own worktree");
    c.check(load(chat_a).proposals.iter().find(|p| p.id == p1).is_some_and(|p| p.status == hive_core::ProposalStatus::Applied), "the proposal is applied");
    // The apply passes the turn's checkout gate: never into a checkout whose
    // remote moved, or one another host on this machine manages.
    let p_gate = file_diff("add gate.txt", "diff --git a/gate.txt b/gate.txt\nnew file mode 100644\n--- /dev/null\n+++ b/gate.txt\n@@ -0,0 +1 @@\n+g\n");
    git(&alpha, &["remote", "set-url", "origin", "https://github.com/e2e/evil.git"]);
    let r = crate::implement_proposal(handle.clone(), state(), chat_a.to_string(), p_gate.to_string(), None).await;
    c.check(r.as_ref().is_err_and(|e| e.contains("no longer points")), format!("an apply into a checkout whose remote moved is refused: {r:?}"));
    git(&alpha, &["remote", "set-url", "origin", "https://github.com/e2e/alpha.git"]);
    let me = state().service.lock().unwrap().device_id();
    hive_runtime::repos::marker::take_over(&alpha, Uuid::new_v4(), "e2e-box").unwrap();
    let r = crate::implement_proposal(handle.clone(), state(), chat_a.to_string(), p_gate.to_string(), None).await;
    c.check(r.as_ref().is_err_and(|e| e.contains("managed by e2e-box")), format!("an apply into a checkout another host manages is refused: {r:?}"));
    // Hygiene and legacy turns leave such a checkout alone too.
    let bound_before = git(&bound, &["worktree", "list"]);
    hive_runtime::repos::marker::take_over(&bound, Uuid::new_v4(), "e2e-box").unwrap();
    c.check(crate::turn_scope(&state(), &load(chat_c)).is_err_and(|e| e.contains("managed by e2e-box")), "a chat without a repo never runs in a bound folder another host manages");
    let swept = crate::guards::run_hygiene_sweep(&state());
    c.check(git(&bound, &["worktree", "list"]) == bound_before && swept.chat_checkouts_removed == 0, "hygiene never sweeps a checkout another host manages");
    hive_runtime::repos::marker::take_over(&bound, me, "e2e").unwrap();
    hive_runtime::repos::marker::take_over(&alpha, me, "e2e").unwrap();
    c.check(!chat_wt.join("gate.txt").exists() && !alpha.join("gate.txt").exists(), "nothing landed while refused");

    // Something else lands on the branch; a proposal made before it conflicts.
    std::fs::write(chat_wt.join("README.md"), "# mine\n").unwrap();
    git(&chat_wt, &["commit", "-qam", "mine"]);
    let tip = git(&chat_wt, &["rev-parse", "HEAD"]);
    let conflicting = "diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-# alpha\n+# theirs\n";
    let p2 = file_diff("retitle", conflicting);
    let r = crate::implement_proposal(handle.clone(), state(), chat_a.to_string(), p2.to_string(), None).await;
    c.check(r.as_ref().is_err_and(|e| e.starts_with("Not applied")), format!("a conflicting apply is refused: {r:?}"));
    c.check(git(&chat_wt, &["rev-parse", "HEAD"]) == tip && git(&chat_wt, &["status", "--porcelain"]).is_empty(), "the conflict changed nothing");
    match crate::rebase_request(&state(), chat_a, p2) {
        Ok(req) => c.check(req.instruction.contains("\"retitle\"") && req.instruction.contains("README.md") && req.instruction.contains(&tip.trim()[..12]), format!("the rebase turn carries the conflict: {}", req.instruction)),
        Err(e) => c.check(false, format!("rebase request: {e}")),
    }
    c.check(crate::rebase_request(&state(), chat_a, p1).is_err(), "an applied proposal isn't sent to rebase");
    c.check(git(&chat_wt, &["rev-parse", "HEAD"]) == tip, "the probe changed nothing");
    // Open in editor: the chat's folder, with the OS handler (recorded here).
    let opened = Arc::new(Mutex::new(Vec::new()));
    {
        let o = opened.clone();
        crate::repos_ui::set_open_hook(Some(Box::new(move |p: &Path| o.lock().unwrap().push(p.to_path_buf()))));
    }
    let r = crate::repos_ui::open_chat_checkout(state(), chat_a.to_string());
    crate::repos_ui::set_open_hook(None);
    let seen: Vec<PathBuf> = opened.lock().unwrap().iter().map(|p| canon(p)).collect();
    c.check(r.is_ok() && seen == [canon(&chat_wt)], format!("open in editor opens chat A's folder: {r:?} {seen:?}"));
    c.check(crate::repos_ui::open_chat_checkout(state(), chat_c.to_string()).is_err(), "a chat without a repo checkout has nothing to open");

    // ---- Re-pinning the chat's base (review residual 3) ----
    // Upstream moves on; the chat's creator re-pins. The chat branch carries
    // someone's own commit ("mine"), so this host refuses to follow and
    // says why; without it, the branch is rebuilt on the new base.
    let pin_before = load(chat_a).repo.and_then(|r| r.base_commit).unwrap_or_default();
    std::fs::write(alpha.join("upstream.txt"), "u\n").unwrap();
    git(&alpha, &["add", "upstream.txt"]);
    git(&alpha, &["commit", "-qm", "upstream"]);
    let upstream = git(&alpha, &["rev-parse", "HEAD"]).trim().to_string();
    let r = crate::repos_ui::repin_chat_base(state(), chat_a.to_string(), Some(upstream.clone()));
    c.check(r.as_deref() == Ok(upstream.as_str()), format!("the chat's creator re-pins its base: {r:?}"));
    let folded = load(chat_a);
    c.check(
        folded.repo.as_ref().and_then(|r| r.base_commit.clone()) == Some(upstream.clone())
            && folded.repo_bases.values().next().is_some_and(|h| h.first() == Some(&pin_before)),
        "the fold keeps the first pin and moves to the new one",
    );
    let r = crate::repos_ui::repin_chat_base(state(), chat_a.to_string(), Some(upstream.clone()));
    c.check(r.is_err(), "re-pinning onto the pin in force is refused");
    let refused = crate::turn_scope(&state(), &load(chat_a));
    c.check(refused.as_ref().is_err_and(|e| e.contains("re-pinned") && e.contains("isn't from the chat's ledger")), format!("a branch with someone's own commit refuses to follow, visibly: {:?}", refused.as_ref().err()));
    c.check(git(&chat_wt, &["rev-parse", "HEAD"]) == tip, "and changes nothing");
    git(&chat_wt, &["reset", "-q", "--hard", "HEAD~1"]);
    match crate::turn_scope(&state(), &load(chat_a)) {
        Ok(_) => c.check(chat_wt.join("upstream.txt").exists() && chat_wt.join("new.txt").exists(), "the branch is rebuilt on the new base with the chat's ledger"),
        Err(e) => c.check(false, format!("following the re-pin: {e}")),
    }

    // ---- Clone into… (4.3) ----
    let parent = base.join("clones");
    std::fs::create_dir_all(&parent).unwrap();
    let r = crate::repos_ui::clone_workspace_repo(state(), g_id.clone(), parent.to_string_lossy().into_owned()).await;
    c.check(r.as_ref().is_err_and(|e| e.contains("isn't a code host connected")), format!("no connection to the repo's host: refused: {r:?}"));
    let r = crate::repos_ui::clone_workspace_repo(state(), Uuid::new_v4().to_string(), parent.to_string_lossy().into_owned()).await;
    c.check(r.is_err(), "an unknown repo id is refused");
    let gamma = state().service.lock().unwrap().workspace_roster(ws).unwrap().repos.get(&Uuid::parse_str(&g_id).unwrap()).unwrap().repo.clone();
    let r = crate::repos_ui::clone_repo_into(&state(), ws, &gamma, &gamma_forge.to_string_lossy(), Path::new("relative"), None, &[]).await;
    c.check(r.is_err(), "a relative parent is refused");
    // Never a redirect (review F12), with or without a DNS pin: a server
    // at the repo's address that answers 302 to another one is not followed.
    let (redirect_url, elsewhere_hit, stop) = redirecting_server();
    let r = crate::repos_ui::clone_repo_into(&state(), ws, &gamma, &redirect_url, &parent, None, &[]).await;
    stop();
    c.check(r.is_err() && !elsewhere_hit.load(std::sync::atomic::Ordering::SeqCst), format!("a clone by id never follows a redirect: {r:?}"));
    c.check(std::fs::read_dir(&parent).map(|d| d.count()).unwrap_or(0) == 0, "and leaves nothing behind");
    let r = crate::repos_ui::clone_repo_into(&state(), ws, &gamma, &gamma_forge.to_string_lossy(), &parent, None, &[]).await;
    c.check(r.as_ref().is_ok_and(|v| v["path"].as_str().is_some_and(|p| p.contains("gamma-"))), format!("Gamma clones into the chosen folder: {r:?}"));
    c.check(
        r.as_ref().ok().and_then(|v| v["path"].as_str().map(PathBuf::from)).is_some_and(|p| canon(&p) == p && p.parent() == Some(canon(&parent).as_path())),
        "the bound clone path is the normalised one every later match compares",
    );
    let chip = crate::repos_ui::chat_workspace_repo(state(), chat_g.to_string()).unwrap();
    c.check(chip["here"]["state"] == "present", format!("the clone is bound: chat G can run here now: {chip}"));
    c.check(crate::turn_scope(&state(), &load(chat_g)).is_ok(), "and its turn gate passes");

    eprintln!("{} repos through the command layer", if c.failed { "FAIL" } else { "PASS" });
    usize::from(c.failed)
}

/// Run the offline checks, then the live scenarios. Returns the number of
/// failures; the live part is skipped without `LIGUS_APP_LIVE_ENDPOINT`.
/// Must be called on the main thread.
pub fn run_live() -> usize {
    let offline_failed = run_offline();
    let Some(cfg) = Config::from_env() else {
        eprintln!("LIGUS_APP_LIVE_ENDPOINT unset; skipping the in-app live scenarios");
        return offline_failed;
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            hive_core::compat::var("LIGUS_LOG").map_err(|_| ()).and_then(|v| tracing_subscriber::EnvFilter::try_new(v).map_err(|_| ()))
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,dispatch=info,compaction=info,worktree=info")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let root = std::env::temp_dir().join(format!("hive-app-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let data_dir = root.join("data");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    planted_repo(&repo);
    // Launch-time workspace adoption looks at the cwd; make it the repo, as
    // when the app is started from a project.
    std::env::set_current_dir(&repo).unwrap();

    let app = headless_app(&data_dir, &repo);
    let handle = app.handle().clone();
    crate::set_agent_tool_settings(
        handle.state::<AppState>(),
        AgentToolSettings { enabled: true, write_in_place: false, run_command: false },
    )
    .expect("set_agent_tool_settings");

    let activity = Arc::new(Mutex::new(Vec::new()));
    {
        let activity = activity.clone();
        handle.listen_any(hive_proto::ChatActivityEvent::EVENT, move |ev| {
            if let Ok(v) = serde_json::from_str::<Value>(ev.payload()) {
                activity.lock().unwrap().push(v);
            }
        });
    }

    let started = Instant::now();
    let proxy_log: Arc<Mutex<Option<Vec<Recorded>>>> = Arc::new(Mutex::new(None));
    let outcomes = tauri::async_runtime::block_on(async {
        let proxy = start_proxy(cfg.upstream.clone(), Arc::new(RUNBOOK.to_string())).await;
        let log_handle = proxy.log.clone();
        eprintln!(
            "in-app live test: upstream {} via proxy {} (gateway private: {}), repo {}",
            cfg.upstream,
            proxy.base,
            hive_core::endpoint::endpoint_is_private(&cfg.upstream),
            repo.display()
        );
        let ctx = Ctx { app: handle.clone(), proxy, repo: repo.clone(), cfg, activity };
        let mut outcomes = Vec::new();
        let wanted = ctx.cfg.scenarios.clone();
        for key in wanted {
            let started = Instant::now();
            let (name, checks) = match key.as_str() {
                "a" => {
                    eprintln!("\n== (a) fix a planted bug — {}", ctx.cfg.model);
                    ("a: bug fix via built-in tools (model A)", scenario_fix_bug(&ctx, "gw-a", &ctx.cfg.model.clone()).await)
                }
                "b" => {
                    eprintln!("\n== (b) fix a planted bug — {}", ctx.cfg.model_b);
                    ("b: bug fix via built-in tools (model B)", scenario_fix_bug(&ctx, "gw-b", &ctx.cfg.model_b.clone()).await)
                }
                "c" => {
                    eprintln!("\n== (c) MCP search_docs with a static bearer token");
                    ("c: MCP search_docs (bearer token)", scenario_mcp(&ctx).await)
                }
                "d" => {
                    eprintln!("\n== (d) vault retrieval");
                    ("d: vault retrieval + stable system prompt", scenario_vault(&ctx).await)
                }
                "e" => {
                    eprintln!("\n== (e) Quick tasks title");
                    ("e: title via Quick tasks runtime", scenario_title(&ctx).await)
                }
                "f" => {
                    eprintln!("\n== (f) compaction");
                    ("f: compaction once + recall", scenario_compaction(&ctx).await)
                }
                other => {
                    eprintln!("unknown scenario {other}");
                    continue;
                }
            };
            outcomes.push(Outcome { name, ok: !checks.failed, secs: started.elapsed().as_secs_f64(), notes: checks.notes });
        }
        // Let a still-running title task finish before tearing down.
        tokio::time::sleep(Duration::from_millis(500)).await;
        *proxy_log.lock().unwrap() = Some(log_handle.lock().unwrap().clone());
        outcomes
    });

    eprintln!("\n================ in-app live results ({:.0}s) ================", started.elapsed().as_secs_f64());
    for o in &outcomes {
        eprintln!("{} {:<45} {:>7.1}s", if o.ok { "PASS" } else { "FAIL" }, o.name, o.secs);
        for n in o.notes.iter().filter(|n| n.starts_with("FAIL")) {
            eprintln!("       {n}");
        }
    }
    let failed = outcomes.iter().filter(|o| !o.ok).count();
    // Everything that crossed the proxy (prompts and raw responses; no
    // credentials go through it), for post-mortems of a slow or odd round.
    if let Some(proxy) = proxy_log.lock().unwrap().take() {
        let path = std::env::temp_dir().join(format!("hive-app-live-proxy-{}.jsonl", std::process::id()));
        let lines: Vec<String> = proxy
            .iter()
            .map(|r| {
                serde_json::json!({
                    "method": r.method, "path": r.path, "status": r.status, "secs": r.secs,
                    "summary": r.summary(), "request": r.body, "response": r.response,
                })
                .to_string()
            })
            .collect();
        if std::fs::write(&path, lines.join("\n")).is_ok() {
            eprintln!("proxy log: {}", path.display());
        }
    }
    drop(app);
    if cfg_keep() {
        eprintln!("kept {}", root.display());
    } else {
        // Holds settings.json (with the MCP token) — always removed.
        let _ = std::fs::remove_dir_all(&root);
    }
    failed + offline_failed
}

fn cfg_keep() -> bool {
    env("LIGUS_APP_LIVE_KEEP").as_deref() == Some("1")
}

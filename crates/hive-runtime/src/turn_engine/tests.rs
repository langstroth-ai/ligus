//! Engine-level tests: one claimed turn end to end against a stub model (an
//! OpenAI-wire server scripted per request), a temp git repo and a real
//! store, with the Ligus MCP server and a recording executor.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::routing::post;
use axum::Router;
use hive_core::crypto::SigningKeypair;
use hive_core::sandbox::{EgressPolicy, SandboxEnvelope};
use hive_core::{ActorIdentity, ActorKind, ChatSession, ModelProviderKind, ProposalKind, WorkspaceAgent};
use serde_json::{json, Value};
use uuid::Uuid;

use super::*;
use crate::egress::EgressProxy;
use crate::executor::{EnforcementPlan, ExecutorKind, HostKind, TurnExecutor, Unconfined};
use crate::ligus_mcp::{LigusHost, LigusMcp};
use crate::mcp::McpTransport;
use crate::{EventStore, McpServerSpec};

// ── The stub model ─────────────────────────────────────────────────────────

/// A line that makes the stub stall (cancellation tests).
const HANG: &str = "HANG";

#[derive(Clone, Default)]
struct Model {
    bodies: Arc<Mutex<Vec<Value>>>,
    rounds: Arc<Vec<Vec<String>>>,
}

fn delta(d: Value) -> String {
    format!("data: {}", json!({"choices": [{"index": 0, "delta": d}]}))
}

fn text(s: &str) -> Vec<String> {
    vec![delta(json!({"content": s}))]
}

fn call(id: &str, name: &str, args: Value) -> Vec<String> {
    let args = args.to_string();
    vec![delta(json!({"tool_calls": [{"index": 0, "id": id, "type": "function", "function": {"name": name, "arguments": args}}]}))]
}

async fn chat(State(m): State<Model>, body: String) -> axum::response::Response {
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let n = {
        let mut b = m.bodies.lock().unwrap();
        b.push(v);
        b.len() - 1
    };
    let lines = m.rounds.get(n).or_else(|| m.rounds.last()).cloned().unwrap_or_default();
    if lines.first().map(String::as_str) == Some(HANG) {
        tokio::time::sleep(Duration::from_secs(120)).await;
    }
    let mut out: String = lines.iter().map(|l| format!("{l}\n\n")).collect();
    out.push_str("data: [DONE]\n\n");
    axum::response::Response::builder().header("content-type", "text/event-stream").body(Body::from(out)).unwrap()
}

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

async fn spawn_model(rounds: Vec<Vec<String>>) -> (String, Model) {
    let m = Model { bodies: Default::default(), rounds: Arc::new(rounds) };
    let base = serve(Router::new().route("/v1/chat/completions", post(chat)).with_state(m.clone())).await;
    (format!("{base}/v1/chat/completions"), m)
}

/// A stub MCP server over HTTP with one tool, `echo`.
async fn spawn_mcp() -> String {
    async fn rpc(body: String) -> axum::Json<Value> {
        let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let id = v["id"].clone();
        let result = match v["method"].as_str().unwrap_or("") {
            "initialize" => json!({"protocolVersion": "2025-03-26", "capabilities": {"tools": {}}, "serverInfo": {"name": "stub", "version": "1"}}),
            "tools/list" => json!({"tools": [{"name": "echo", "description": "Echo the text back.", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}}]}),
            "tools/call" => json!({"content": [{"type": "text", "text": format!("echo: {}", v["params"]["arguments"]["text"].as_str().unwrap_or(""))}]}),
            _ => json!({}),
        };
        axum::Json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }
    format!("{}/mcp", serve(Router::new().route("/mcp", post(rpc))).await)
}

// ── The recording executor ─────────────────────────────────────────────────

/// Reports a confining kind, records every plan it is asked to confine, and
/// spawns like [`Unconfined`] (these tests check the plumbing, not the OS).
#[derive(Default)]
struct Recording(Mutex<Vec<EnforcementPlan>>);

impl TurnExecutor for Recording {
    fn kind(&self) -> ExecutorKind {
        ExecutorKind::Seatbelt
    }
    fn confine(&self, program: &OsStr, args: &[OsString], cwd: &Path, plan: &EnforcementPlan, proxy: Option<&EgressProxy>) -> std::io::Result<std::process::Command> {
        self.0.lock().unwrap().push(plan.clone());
        Unconfined.confine(program, args, cwd, plan, proxy)
    }
}

// ── The world and the host ─────────────────────────────────────────────────

struct Inner {
    svc: Mutex<ChatService>,
    started: Mutex<Vec<(Uuid, Uuid, Uuid)>>,
}

impl LigusHost for Inner {
    fn with_service(&self, f: &mut dyn FnMut(&mut ChatService)) {
        f(&mut self.svc.lock().unwrap())
    }
    fn start_turn(&self, chat: Uuid, agent: Uuid, trigger: Uuid) {
        self.started.lock().unwrap().push((chat, agent, trigger));
    }
}

struct Host {
    inner: Arc<Inner>,
    mcp: Arc<LigusMcp>,
    repo: Option<PathBuf>,
    servers: Vec<McpServerSpec>,
    deltas: Mutex<String>,
    activity: Mutex<Vec<StreamActivity>>,
    phases: Mutex<Vec<Phase>>,
    /// Finalize fails (the error path's placeholder handling).
    fail_finalize: bool,
}

impl TurnHost for Host {
    fn service(&self) -> &Mutex<ChatService> {
        &self.inner.svc
    }
    fn mcp_servers(&self, _egress: &EgressPolicy, _shell: bool) -> impl Future<Output = Vec<McpServerSpec>> + Send {
        let s = self.servers.clone();
        async move { s }
    }
    fn ligus(&self) -> Option<Arc<LigusMcp>> {
        Some(self.mcp.clone())
    }
    fn checkout(&self, _s: &ChatSession, _m: Uuid) -> impl Future<Output = Result<Checkout, String>> + Send {
        let c = Checkout { repo_root: self.repo.clone(), ..Default::default() };
        async move { Ok(c) }
    }
    fn delta(&self, _m: Uuid, text: &str) {
        self.deltas.lock().unwrap().push_str(text);
    }
    fn activity(&self, _m: Uuid, a: &StreamActivity) {
        self.activity.lock().unwrap().push(a.clone());
    }
    fn phase(&self, _m: Uuid, p: Phase, _t: &str) {
        self.phases.lock().unwrap().push(p);
    }
    fn finalize(&self, svc: &mut ChatService, a: FinalizeArgs<'_>) -> Result<String, String> {
        if self.fail_finalize {
            return Err("finalize failed".into());
        }
        let (sid, wid, mid) = (a.ctx.session_id, a.ctx.workspace_id, a.ctx.message_id);
        let input = crate::finalize::FinalizeInput { ctx: a.ctx, workspace_root: "", full: a.full, edits_in_review: a.edits_in_review, mcp: a.mcp, base_commit: None, apply_flow: true };
        match crate::finalize::finalize_reply(svc, input) {
            Ok(f) => Ok(f.cleaned),
            Err(e) => {
                if let Some(mid) = mid {
                    let _ = svc.complete_assistant_message(sid, wid, mid, a.full.to_string());
                }
                Err(e.to_string())
            }
        }
    }
}

struct World {
    host: Host,
    ws: Uuid,
    chat: Uuid,
    coder: Uuid,
    reviewer: Uuid,
    trigger: Uuid,
    _dir: tempfile::TempDir,
}

fn git(dir: &Path, args: &[&str]) {
    let out = hive_core::process_util::git().arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A workspace with a chat, an agent `coder` bound to this device (its own
/// certified key), a second agent `reviewer`, the person's request, and a
/// git repo with one commit.
fn world(servers: Vec<McpServerSpec>) -> World {
    let dir = tempfile::tempdir().unwrap();
    let store = EventStore::open(dir.path().join("hive.db")).unwrap();
    let mut svc = ChatService::new(store, Uuid::new_v4(), SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), ActorIdentity::new("x", "Alex", ActorKind::Human));
    let account = Uuid::new_v4().to_string();
    svc.set_author_account(account.clone(), "Alex", None);
    let ws = Uuid::new_v4();
    let chat = svc.create_chat("A", ws, "rt").unwrap();
    svc.set_agent_vault(Box::new(crate::identity_store::FileKeyVault::new(dir.path())));
    let mut agent = WorkspaceAgent::new("coder", "rt");
    agent.owner_actor_id = account.clone();
    let coder = agent.id;
    svc.add_agent(chat.id, ws, agent).unwrap();
    svc.set_agent_host(ws, coder, &svc.device_id().to_string()).unwrap();
    let mut rev = WorkspaceAgent::new("reviewer", "rt");
    rev.owner_actor_id = account;
    let reviewer = rev.id;
    svc.add_agent(chat.id, ws, rev).unwrap();
    let trigger = svc.post_user_message(chat.id, ws, "@coder please add a greeting").unwrap().id;

    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "# demo\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["-c", "user.email=t@example.com", "-c", "user.name=t", "commit", "-qm", "init"]);

    let inner = Arc::new(Inner { svc: Mutex::new(svc), started: Mutex::new(Vec::new()) });
    let mcp = LigusMcp::new(inner.clone());
    let host = Host {
        inner,
        mcp,
        repo: Some(repo),
        servers,
        deltas: Default::default(),
        activity: Default::default(),
        phases: Default::default(),
        fail_finalize: false,
    };
    World { host, ws, chat: chat.id, coder, reviewer, trigger, _dir: dir }
}

fn runtime(endpoint: &str) -> ResolvedRuntime {
    ResolvedRuntime {
        provider: ModelProviderKind::Custom,
        model: "stub".into(),
        endpoint: endpoint.into(),
        api_key: None,
        args: Vec::new(),
        model_provider_id: None,
        model_base_url: None,
        context_window_tokens: Some(32_000),
        keep_alive: None,
        think: None,
        tools: true,
        max_tool_rounds: Some(6),
        allow_remote_file_tools: false,
        driver: Default::default(),
        gateway: None,
    }
}

fn input(w: &World, rt: ResolvedRuntime, shell: bool, executor: Arc<dyn TurnExecutor>) -> TurnInput {
    let session = w.host.inner.svc.lock().unwrap().load(w.chat).unwrap().unwrap();
    let envelope = SandboxEnvelope::default();
    let mut policy = TurnPolicy::new(HostKind::Desktop, envelope.clone(), GatePlan::Full);
    policy.shell_allowed = shell;
    TurnInput {
        workspace_id: w.ws,
        chat_id: w.chat,
        session,
        reply_to: Some(w.trigger),
        responder: Responder { author: "coder".into(), effect_author: "coder".into(), runtime_id: "rt".into(), agent_id: Some(w.coder), runtime: rt },
        system: "You are @coder.".into(),
        turns: vec![ChatTurn::user("please add a greeting")],
        gate: GatePlan::Full,
        envelope,
        shell_allowed: shell,
        policy,
        executor,
        tools: ToolOptions { builtin: true, run_command: true, ..Default::default() },
        write_capable: false,
        require_isolation: true,
        stop: None,
        timeout: Duration::from_secs(60),
        options: EngineOptions { scan_injections: true, ..Default::default() },
    }
}

fn offered(m: &Model, round: usize) -> Vec<String> {
    m.bodies.lock().unwrap()[round]["tools"].as_array().map(|t| t.iter().filter_map(|t| t["function"]["name"].as_str().map(str::to_string)).collect()).unwrap_or_default()
}

fn load(w: &World) -> ChatSession {
    w.host.inner.svc.lock().unwrap().load(w.chat).unwrap().unwrap()
}

// ── Tests ──────────────────────────────────────────────────────────────────

/// The tool loop: the model calls a built-in tool, gets its result, and
/// answers. Its edit lands in a per-turn worktree (never the checkout), and
/// the diff is filed as the agent's FileDiff proposal. Without the shell,
/// `run_command` is never offered, even with the setting on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_loop_edits_in_a_turn_worktree_and_files_a_proposal() {
    let (endpoint, model) =
        spawn_model(vec![call("c1", "write_file", json!({"path": "hello.txt", "content": "hello\n"})), text("Added hello.txt.")]).await;
    let w = world(Vec::new());
    let out = run(&w.host, input(&w, runtime(&endpoint), false, Arc::new(Recording::default()))).await.expect("the turn runs");
    assert_eq!(out.body, "Added hello.txt.");
    assert_eq!(model.bodies.lock().unwrap().len(), 2, "one tool round, then the answer");
    let tools = offered(&model, 0);
    assert!(tools.iter().any(|t| t == "write_file") && tools.iter().any(|t| t == "read_file"), "{tools:?}");
    assert!(!tools.iter().any(|t| t == "run_command"), "no shell without the Owner's rule: {tools:?}");
    assert!(tools.iter().any(|t| t == "ligus_propose"), "the folded Ligus tools ride along: {tools:?}");
    // The checkout is untouched; the change is a proposal.
    assert!(!w.host.repo.as_ref().unwrap().join("hello.txt").exists(), "never written in place");
    let s = load(&w);
    let p = s.proposals.iter().find(|p| Some(p.id) == out.proposal).expect("the proposal was filed");
    assert_eq!(p.kind, ProposalKind::FileDiff);
    assert!(p.diff.as_deref().unwrap_or("").contains("+hello"), "{:?}", p.diff);
    assert_eq!(p.changed_files, vec!["hello.txt".to_string()]);
    assert_eq!(p.producing_agent_id, Some(w.coder));
    let reply = s.messages.iter().find(|m| m.id == out.message_id).unwrap();
    assert!(!reply.is_streaming && reply.body == "Added hello.txt.");
    assert_eq!(reply.reply_to_message_id, Some(w.trigger));
    // The host saw the stream, the tool activity and the end.
    assert_eq!(*w.host.deltas.lock().unwrap(), "Added hello.txt.");
    assert!(w.host.activity.lock().unwrap().iter().any(|a| matches!(a, StreamActivity::Tool { name, .. } if name == "write_file")));
    assert_eq!(*w.host.phases.lock().unwrap(), vec![Phase::Completed]);
}

/// With the shell, `run_command` is offered, and the command it runs is
/// confined by the executor the host passed in (never the host's default),
/// under a plan whose home dir is denied.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_command_goes_through_the_given_executor_with_home_denied() {
    let (endpoint, model) = spawn_model(vec![call("c1", "run_command", json!({"command": "echo ran > ran.txt"})), text("Ran it.")]).await;
    let w = world(Vec::new());
    let exec = Arc::new(Recording::default());
    let out = run(&w.host, input(&w, runtime(&endpoint), true, exec.clone())).await.expect("the turn runs");
    assert_eq!(out.body, "Ran it.");
    assert!(offered(&model, 0).iter().any(|t| t == "run_command"));
    let plans = exec.0.lock().unwrap();
    assert_eq!(plans.len(), 1, "the command was confined by the given executor");
    let plan = &plans[0];
    if let Some(home) = crate::executor::home_dir() {
        assert_eq!(plan.home.as_deref(), Some(home.as_path()), "the home dir is default-deny: {plan:?}");
        assert!(!plan.home_read.contains(&home), "the home dir itself is never readable: {plan:?}");
    }
    // The command ran in the turn's worktree: its file came back as a diff.
    let s = load(&w);
    let p = s.proposals.iter().find(|p| Some(p.id) == out.proposal).expect("the command's file came back as a proposal");
    assert!(p.changed_files.contains(&"ran.txt".to_string()), "{:?}", p.changed_files);
}

/// An MCP tool call through a stub HTTP MCP server: offered under its
/// server-qualified name, called, and its result handed back to the model.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_tool_call_through_a_stub_server() {
    let url = spawn_mcp().await;
    let (endpoint, model) = spawn_model(vec![call("c1", "stub__echo", json!({"text": "ping"})), text("It said pong.")]).await;
    let server = McpServerSpec { id: "stub".into(), transport: McpTransport::Http { url }, enabled: true, auth: None };
    let mut w = world(vec![server]);
    w.host.repo = None;
    let out = run(&w.host, input(&w, runtime(&endpoint), false, Arc::new(Recording::default()))).await.expect("the turn runs");
    assert_eq!(out.body, "It said pong.");
    assert!(offered(&model, 0).iter().any(|t| t == "stub__echo"), "{:?}", offered(&model, 0));
    assert!(model.bodies.lock().unwrap()[1].to_string().contains("echo: ping"), "the tool's result reached the model");
    assert!(out.proposal.is_none(), "nothing to review");
}

/// A hand-off through the folded Ligus tools: the receiver's turn is started
/// through the host, and the chat records the card; a proposal the agent
/// files through the tools is its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_handoff_starts_the_receiver_through_the_host() {
    let (endpoint, _model) = spawn_model(vec![
        call("c1", "ligus_flow", json!({"op": "handoff", "to": "reviewer", "keyFiles": ["README.md"], "notes": "check it"})),
        call("c2", "ligus_propose", json!({"kind": "decision", "title": "Greet in English", "body": "simplest"})),
        text("Handed off."),
    ])
    .await;
    let w = world(Vec::new());
    let out = run(&w.host, input(&w, runtime(&endpoint), false, Arc::new(Recording::default()))).await.expect("the turn runs");
    assert_eq!(out.body, "Handed off.");
    let started = w.host.inner.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1, "{started:?}");
    assert_eq!((started[0].0, started[0].1), (w.chat, w.reviewer));
    let s = load(&w);
    assert!(s.flow.handoffs.iter().any(|c| c.to_name == "reviewer" && c.via == "tool"));
    let decision = s.proposals.iter().find(|p| p.title == "Greet in English").expect("the agent's proposal");
    assert_eq!(decision.producing_agent_id, Some(w.coder));
}

/// The stop signal cancels a running turn: the placeholder is completed
/// empty, and the turn reports it was stopped (not failed).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_stop_signal_cancels_a_running_turn() {
    let (endpoint, _model) = spawn_model(vec![vec![HANG.to_string()]]).await;
    let w = world(Vec::new());
    let stop = Arc::new(tokio::sync::Notify::new());
    let mut i = input(&w, runtime(&endpoint), false, Arc::new(Recording::default()));
    i.stop = Some(stop.clone());
    let s2 = stop.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        s2.notify_waiters();
    });
    let started = std::time::Instant::now();
    let err = run(&w.host, i).await.expect_err("stopped");
    assert!(err.stopped, "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(30), "stopped promptly");
    let s = load(&w);
    let reply = s.messages.iter().find(|m| Some(m.id) == err.message_id).unwrap();
    assert!(!reply.is_streaming && reply.body.is_empty(), "{reply:?}");
    assert_eq!(*w.host.phases.lock().unwrap(), vec![Phase::Completed]);
}

/// A writing CLI agent never runs in place: with no checkout to cut a
/// worktree from, the turn is refused before anything is spawned.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_writing_cli_without_a_worktree_is_refused() {
    let mut w = world(Vec::new());
    w.host.repo = None;
    let exec = Arc::new(Recording::default());
    let mut rt = runtime("");
    rt.provider = ModelProviderKind::Aider;
    let mut i = input(&w, rt, false, exec.clone());
    i.write_capable = true;
    let err = run(&w.host, i).await.expect_err("refused");
    assert!(err.message.contains("isolated git worktree"), "{err:?}");
    assert!(exec.0.lock().unwrap().is_empty(), "nothing was spawned");
    let reply = load(&w).messages.into_iter().find(|m| Some(m.id) == err.message_id).unwrap();
    assert!(!reply.is_streaming && reply.body.contains("isolated git worktree"));
    // The host hears it, so no live placeholder is left in its UI.
    assert_eq!(*w.host.phases.lock().unwrap(), vec![Phase::Error]);
}

/// Below the author floor no tools are offered at all: the model streams
/// plain chat.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn below_the_floor_no_tools_are_offered() {
    let (endpoint, model) = spawn_model(vec![text("Just text.")]).await;
    let w = world(Vec::new());
    let mut i = input(&w, runtime(&endpoint), true, Arc::new(Recording::default()));
    i.gate = GatePlan::ReadOnly;
    i.policy.gate = GatePlan::ReadOnly;
    let out = run(&w.host, i).await.expect("the turn runs");
    assert_eq!(out.body, "Just text.");
    assert!(offered(&model, 0).is_empty(), "{:?}", offered(&model, 0));
}

/// The shared gate: an author the log can't vouch for fails closed
/// (read-only), and such a turn has no shell.
#[test]
fn decide_gate_fails_closed_for_an_unknown_author() {
    let w = world(Vec::new());
    let svc = w.host.inner.svc.lock().unwrap();
    let session = svc.load(w.chat).unwrap().unwrap();
    let g = decide_gate(&svc, &session, Uuid::new_v4(), Some(w.coder), runtime("http://127.0.0.1:9/v1/chat/completions"), None).expect("an HTTP model always may");
    assert_eq!(g.gate, GatePlan::ReadOnly);
    assert!(!g.shell_allowed);
}

/// Worktrees Ligus keeps for `repo` (besides the checkout itself).
fn turn_worktrees(repo: &Path) -> Vec<String> {
    let out = hive_core::process_util::git().arg("-C").arg(repo).args(["worktree", "list", "--porcelain"]).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .filter(|p| Path::new(p).canonicalize().ok() != repo.canonicalize().ok())
        .filter(|p| !p.contains("/chat-") && !p.contains("chat-"))
        .map(str::to_string)
        .collect()
}

/// A turn stopped after it wrote: nothing partial is filed, the checkout is
/// untouched, and the turn's worktree is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_turn_files_nothing_partial_and_drops_its_worktree() {
    let (endpoint, model) = spawn_model(vec![call("c1", "write_file", json!({"path": "half.txt", "content": "half\n"})), vec![HANG.to_string()]]).await;
    let w = world(Vec::new());
    let stop = Arc::new(tokio::sync::Notify::new());
    let mut i = input(&w, runtime(&endpoint), false, Arc::new(Recording::default()));
    i.stop = Some(stop.clone());
    let (s2, m2) = (stop.clone(), model.clone());
    tokio::spawn(async move {
        // Stop once the model was asked again (the write is done).
        for _ in 0..200 {
            if m2.bodies.lock().unwrap().len() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        s2.notify_waiters();
    });
    let err = run(&w.host, i).await.expect_err("stopped");
    assert!(err.stopped, "{err:?}");
    let repo = w.host.repo.clone().unwrap();
    assert!(!repo.join("half.txt").exists(), "never written in place");
    let s = load(&w);
    assert!(!s.proposals.iter().any(|p| p.kind == ProposalKind::FileDiff), "a stopped turn files no diff: {:?}", s.proposals);
    assert!(turn_worktrees(&repo).is_empty(), "the turn's worktree is gone: {:?}", turn_worktrees(&repo));
}

/// The streaming backstop works the same: a timed-out turn fails, files
/// nothing and leaves no live placeholder.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_timed_out_turn_files_nothing_and_completes_its_placeholder() {
    let (endpoint, _model) = spawn_model(vec![call("c1", "write_file", json!({"path": "late.txt", "content": "late\n"})), vec![HANG.to_string()]]).await;
    let w = world(Vec::new());
    let mut i = input(&w, runtime(&endpoint), false, Arc::new(Recording::default()));
    i.timeout = Duration::from_secs(3);
    let err = run(&w.host, i).await.expect_err("timed out");
    assert!(!err.stopped && err.message.contains("timed out"), "{err:?}");
    let s = load(&w);
    let reply = s.messages.iter().find(|m| Some(m.id) == err.message_id).unwrap();
    assert!(!reply.is_streaming && reply.body.starts_with("[error]"), "{reply:?}");
    assert!(!s.proposals.iter().any(|p| p.kind == ProposalKind::FileDiff));
    assert!(turn_worktrees(w.host.repo.as_ref().unwrap()).is_empty());
    assert_eq!(*w.host.phases.lock().unwrap(), vec![Phase::Error]);
}

/// A finalize failure never leaves a live placeholder: the reply is
/// completed and the host hears the error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_finalize_failure_completes_the_placeholder() {
    let (endpoint, _model) = spawn_model(vec![text("Done.")]).await;
    let mut w = world(Vec::new());
    w.host.fail_finalize = true;
    let err = run(&w.host, input(&w, runtime(&endpoint), false, Arc::new(Recording::default()))).await.expect_err("finalize failed");
    let reply = load(&w).messages.into_iter().find(|m| Some(m.id) == err.message_id).unwrap();
    assert!(!reply.is_streaming && reply.body == "Done.", "{reply:?}");
    assert_eq!(*w.host.phases.lock().unwrap(), vec![Phase::Error]);
}

/// Listing a turn's MCP tools starts its stdio servers through the turn's
/// executor (never the host's default), under the turn's policy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_listing_spawns_stdio_servers_through_the_turns_executor() {
    let (endpoint, _model) = spawn_model(vec![text("No tools needed.")]).await;
    let server = McpServerSpec { id: "local".into(), transport: McpTransport::Stdio { command: "/bin/sh".into(), args: vec!["-c".into(), "exit 0".into()] }, enabled: true, auth: None };
    let mut w = world(vec![server]);
    w.host.repo = None;
    let exec = Arc::new(Recording::default());
    let out = run(&w.host, input(&w, runtime(&endpoint), false, exec.clone())).await.expect("the turn runs");
    assert_eq!(out.body, "No tools needed.");
    assert!(!exec.0.lock().unwrap().is_empty(), "the stdio server was confined by the turn's executor");
}

/// Concurrent turns (each with Ligus tool calls, a write and a proposal)
/// while the UI keeps reading the store: no lock-order inversion between
/// the engine's callbacks, the Ligus server and the service lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_turns_and_ui_reads_never_deadlock() {
    let w = Arc::new(world(Vec::new()));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // The UI: keep loading the chat (a blocking reader, like a Tauri command).
    let reader = {
        let (w, done) = (w.clone(), done.clone());
        std::thread::spawn(move || {
            let mut n = 0u64;
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                let s = w.host.inner.svc.lock().unwrap().load(w.chat).unwrap().unwrap();
                n += s.messages.len() as u64;
                std::thread::yield_now();
            }
            n
        })
    };
    let mut turns = Vec::new();
    for k in 0..4 {
        let (endpoint, _model) = spawn_model(vec![
            call("c1", "ligus_propose", json!({"kind": "decision", "title": format!("Option {k}"), "body": "b"})),
            call("c2", "write_file", json!({"path": format!("f{k}.txt"), "content": "x\n"})),
            text(&format!("Turn {k} done.")),
        ])
        .await;
        let w = w.clone();
        turns.push(tokio::spawn(async move {
            let i = input(&w, runtime(&endpoint), false, Arc::new(Recording::default()));
            run(&w.host, i).await
        }));
    }
    let all = tokio::time::timeout(Duration::from_secs(120), futures_util::future::join_all(turns)).await.expect("no deadlock: every turn finished");
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    reader.join().unwrap();
    for r in all {
        let out = r.unwrap().expect("the turn runs");
        assert!(out.body.ends_with("done."), "{}", out.body);
        assert!(out.proposal.is_some(), "each turn filed its diff");
    }
    let s = load(&w);
    assert_eq!(s.proposals.iter().filter(|p| p.kind == ProposalKind::FileDiff).count(), 4);
    assert!(!s.messages.iter().any(|m| m.is_streaming), "no live placeholder left");
}

//! Integration tests for `ligus serve`: a real server against the in-process
//! test relay (`hive-runtime/src/test_relay.rs`), a fake OpenAI-compatible
//! model server and a fake ntfy, driven over HTTP + SSE like the phone.
//!
//! The env-gated live test runs the same flow against the lab gateway:
//!
//! ```text
//! LIGUS_SERVE_LIVE_ENDPOINT=http://host:8080/v1/chat/completions \
//! LIGUS_SERVE_LIVE_MODELS=gpt-oss:20b,gemma4:12b \
//!   cargo test -p hive-cli serve::tests::live -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::Router;
use futures_util::StreamExt;
use hive_core::{workspace_config_session_id, ModelProviderKind, WorkspaceAgent, WorkspaceRuntime};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::*;
use crate::repos::RepoBindings;

#[path = "../../../hive-runtime/src/test_relay.rs"]
#[allow(dead_code)]
mod test_relay;

async fn spawn(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

// ── Fake model server ──────────────────────────────────────────────────────

#[derive(Clone, Default)]
struct FakeModel {
    bodies: Arc<Mutex<Vec<Value>>>,
    rounds: Arc<Vec<Vec<String>>>,
}

fn delta(d: Value) -> String {
    format!("data: {}", json!({"choices": [{"index": 0, "delta": d}]}))
}

fn streamed_call(id: &str, name: &str, args: &str) -> Vec<String> {
    let (a, b) = args.split_at(args.len() / 2);
    vec![
        delta(json!({"tool_calls": [{"index": 0, "id": id, "type": "function", "function": {"name": name, "arguments": ""}}]})),
        delta(json!({"tool_calls": [{"index": 0, "function": {"arguments": a}}]})),
        delta(json!({"tool_calls": [{"index": 0, "function": {"arguments": b}}]})),
    ]
}

async fn fake_chat(State(f): State<FakeModel>, body: String) -> axum::response::Response {
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let n = {
        let mut b = f.bodies.lock().unwrap();
        b.push(v);
        b.len() - 1
    };
    let lines = f.rounds.get(n).or_else(|| f.rounds.last()).cloned().unwrap_or_default();
    let mut out: String = lines.iter().map(|l| format!("{l}\n\n")).collect();
    out.push_str("data: [DONE]\n\n");
    axum::response::Response::builder().header("content-type", "text/event-stream").body(Body::from(out)).unwrap()
}

async fn spawn_model(rounds: Vec<Vec<String>>) -> (String, FakeModel) {
    let f = FakeModel { bodies: Default::default(), rounds: Arc::new(rounds) };
    let base = spawn(Router::new().route("/v1/chat/completions", post(fake_chat)).with_state(f.clone())).await;
    (format!("{base}/v1/chat/completions"), f)
}

// ── Fake ntfy ──────────────────────────────────────────────────────────────

type Pushes = Arc<Mutex<Vec<(HeaderMap, String)>>>;

async fn spawn_ntfy() -> (String, Pushes) {
    let pushes: Pushes = Default::default();
    let p = pushes.clone();
    let router = Router::new().route(
        "/:topic",
        post(move |headers: HeaderMap, body: String| {
            let p = p.clone();
            async move {
                p.lock().unwrap().push((headers, body));
                "{}"
            }
        }),
    );
    (spawn(router).await, pushes)
}

// ── Harness ────────────────────────────────────────────────────────────────

fn git(dir: &Path, args: &[&str]) {
    let out = hive_core::process_util::command("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn make_repo(dir: &Path, files: &[(&str, &str)]) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    for (name, body) in files {
        std::fs::write(dir.join(name), body).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "init"]);
}

fn config(dir: &Path, relay: &str, room: &str) -> Config {
    Config {
        data_dir: dir.to_path_buf(),
        relay_url: Some(relay.to_string()),
        token: None,
        github_token: None,
        room: room.to_string(),
        key: Some("serve-test-passphrase".into()),
    }
}

struct Harness {
    _tmp: tempfile::TempDir,
    desktop: Config,
    handle: ServeHandle,
    base: String,
    token: String,
    client: reqwest::Client,
    agent_id: Uuid,
    ntfy: Pushes,
}

/// A desktop that owns the workspace (founder) with one agent `@coder` on a
/// Custom runtime at `endpoint` / `model`, synced through the relay; then a
/// serve host joined to it with a repo bound.
async fn harness(endpoint: &str, model: &str, repo_files: &[(&str, &str)]) -> Harness {
    harness_with(endpoint, model, repo_files, |_| {}).await
}

async fn harness_with(
    endpoint: &str,
    model: &str,
    repo_files: &[(&str, &str)],
    tweak: impl FnOnce(&mut ServeOptions),
) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let relay = spawn(test_relay::router()).await;
    let room = format!("serve-test-{}", Uuid::new_v4());
    let ws = crate::uuid_of_room(&room);

    let desktop = config(&tmp.path().join("desktop"), &relay, &room);
    let agent_id = {
        let mut svc = open_service(&desktop).unwrap();
        svc.ensure_self_member(workspace_config_session_id(ws), ws).unwrap();
        let mut wr = WorkspaceRuntime::new("gw", "Lab gateway", ModelProviderKind::Custom, model);
        wr.endpoint = endpoint.to_string();
        svc.add_workspace_runtime(ws, wr).unwrap();
        let mut agent = WorkspaceAgent::new("coder", "gw");
        agent.owner_actor_id = svc.author().id.clone();
        let id = agent.id;
        svc.add_agent(ws, ws, agent).unwrap();
        (id, svc.issue_invite(ws).unwrap())
    };
    let (agent_id, invite) = agent_id;
    sync_once(&desktop).await.unwrap();

    let serve_dir = tmp.path().join("serve");
    // The box joins with the founder's invite (redeemed by its self-enrollment).
    std::fs::create_dir_all(&serve_dir).unwrap();
    hive_runtime::event_store::EventStore::open(serve_dir.join("hive.db"))
        .unwrap()
        .set_join_invite(ws, invite)
        .unwrap();
    let serve_cfg = config(&serve_dir, &relay, &room);
    let repo = tmp.path().join("repo");
    make_repo(&repo, repo_files);
    RepoBindings { workspace: Some(repo.to_string_lossy().into_owned()), ..Default::default() }.save(&serve_dir).unwrap();

    let (ntfy_url, ntfy) = spawn_ntfy().await;
    let dir_for_dashboard = serve_dir.clone();
    let opts = ServeOptions {
        listen: "127.0.0.1:0".parse().unwrap(),
        token_file: serve_dir.join("serve.token"),
        ntfy: Some(ntfy::Ntfy { url: ntfy_url, topic: "hive-test".into(), token: None }),
        public_url: None,
        label: "sff".into(),
        name: Some("Phone".into()),
        pair: false,
        prometheus_url: "http://127.0.0.1:9".into(),
        prometheus_configured: false,
        dashboard_file: dir_for_dashboard.join("dashboard.toml"),
        default_agent: None,
        owner: None,
        apply_on_approve: false,
        sync_every: Duration::from_millis(300),
        remote: None,
        appearance: Default::default(),
    };
    let mut opts = opts;
    tweak(&mut opts);
    let handle = start(serve_cfg, opts).await.unwrap();
    let base = format!("http://{}", handle.addr);
    let token = handle.shared.token();
    Harness { _tmp: tmp, desktop, handle, base, token, client: reqwest::Client::new(), agent_id, ntfy }
}

impl Harness {
    async fn get(&self, path: &str) -> Value {
        let r = self.client.get(format!("{}{path}", self.base)).bearer_auth(&self.token).send().await.unwrap();
        assert!(r.status().is_success(), "GET {path}: {}", r.status());
        r.json().await.unwrap()
    }

    async fn send(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Value {
        let mut req = self.client.request(method.clone(), format!("{}{path}", self.base)).bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await.unwrap();
        let status = r.status();
        let v: Value = r.json().await.unwrap_or(Value::Null);
        assert!(status.is_success(), "{method} {path}: {status} {v}");
        v
    }

    /// Open the SSE stream; events arrive as (name, data, id) on the channel.
    async fn events(&self, last_id: Option<u64>) -> mpsc::UnboundedReceiver<(String, Value, u64)> {
        let mut req = self.client.get(format!("{}/v1/events", self.base)).bearer_auth(&self.token);
        if let Some(id) = last_id {
            req = req.header("Last-Event-ID", id.to_string());
        }
        let resp = req.send().await.unwrap();
        assert!(resp.status().is_success());
        assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut buf = String::new();
            let mut stream = resp.bytes_stream();
            while let Some(Ok(chunk)) = stream.next().await {
                buf.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(end) = buf.find("\n\n") {
                    let frame: String = buf.drain(..end + 2).collect();
                    let (mut name, mut data, mut id) = (String::new(), String::new(), 0u64);
                    for line in frame.lines() {
                        if let Some(v) = line.strip_prefix("event:") {
                            name = v.trim().to_string();
                        } else if let Some(v) = line.strip_prefix("data:") {
                            data.push_str(v.trim_start());
                        } else if let Some(v) = line.strip_prefix("id:") {
                            id = v.trim().parse().unwrap_or(0);
                        }
                    }
                    if !name.is_empty() && tx.send((name, serde_json::from_str(&data).unwrap_or(Value::Null), id)).is_err() {
                        return;
                    }
                }
            }
        });
        rx
    }
}

/// Collect events until `done` says so (or time runs out).
async fn collect_until(
    rx: &mut mpsc::UnboundedReceiver<(String, Value, u64)>,
    secs: u64,
    mut done: impl FnMut(&[(String, Value, u64)]) -> bool,
) -> Vec<(String, Value, u64)> {
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while !done(&seen) {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(ev)) => seen.push(ev),
            _ => break,
        }
    }
    seen
}

async fn this_host(h: &Harness) -> String {
    let hosts = h.get("/v1/hosts").await;
    let me = hosts["hosts"].as_array().unwrap().iter().find(|x| x["isThis"] == json!(true)).expect("this host listed");
    assert_eq!(me["label"], "sff");
    assert_eq!(me["online"], json!(true));
    me["id"].as_str().unwrap().to_string()
}

/// Binding an agent to a host is an Admin action (S2): the desktop founder
/// promotes this box's identity to Admin, and the box syncs the change.
async fn make_serve_admin(h: &Harness) {
    let ws = h.handle.shared.ws;
    let serve_actor = h.handle.shared.lock().author().id.clone();
    {
        let mut desk = open_service(&h.desktop).unwrap();
        // The box's join must have reached the desktop first.
        for _ in 0..100 {
            sync_once(&h.desktop).await.unwrap();
            let cfg = desk.load(workspace_config_session_id(ws)).unwrap().unwrap();
            if cfg.members.iter().any(|m| m.id == serve_actor) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            desk = open_service(&h.desktop).unwrap();
        }
        desk.set_member_role(ws, ws, serve_actor.clone(), hive_core::WorkspaceRole::Admin).unwrap();
    }
    sync_once(&h.desktop).await.unwrap();
    for _ in 0..200 {
        let cfg = h.handle.shared.lock().load(workspace_config_session_id(ws)).unwrap();
        if cfg.is_some_and(|c| c.role_of(&serve_actor) == hive_core::WorkspaceRole::Admin) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the box never saw its promotion");
}

// ── Tests ──────────────────────────────────────────────────────────────────

/// The phone's pairing check calls `GET /v1/agents`; a box whose workspace has
/// no agents or config yet must answer with empty lists, not 404.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn empty_workspace_lists_are_empty_not_404() {
    let tmp = tempfile::tempdir().unwrap();
    let relay = spawn(test_relay::router()).await;
    let room = format!("serve-empty-{}", Uuid::new_v4());
    let dir = tmp.path().join("serve");
    let dir_for_dashboard = dir.clone();
    let opts = ServeOptions {
        listen: "127.0.0.1:0".parse().unwrap(),
        token_file: dir.join("serve.token"),
        ntfy: None,
        public_url: None,
        label: "sff".into(),
        name: Some("Phone".into()),
        pair: false,
        prometheus_url: "http://127.0.0.1:9".into(),
        prometheus_configured: false,
        dashboard_file: dir_for_dashboard.join("dashboard.toml"),
        default_agent: None,
        owner: None,
        apply_on_approve: false,
        sync_every: Duration::from_millis(300),
        remote: None,
        appearance: Default::default(),
    };
    let handle = start(config(&dir, &relay, &room), opts).await.unwrap();
    let base = format!("http://{}", handle.addr);
    let client = reqwest::Client::new();
    for (path, key) in [("/v1/agents", "agents"), ("/v1/hosts", "hosts"), ("/v1/chats", "chats"), ("/v1/proposals", "proposals")] {
        let r = client.get(format!("{base}{path}")).bearer_auth(handle.shared.token()).send().await.unwrap();
        assert_eq!(r.status(), 200, "{path}");
        let v: Value = r.json().await.unwrap();
        assert!(v[key].is_array(), "{path}: {v}");
    }
    assert_eq!(
        client
            .get(format!("{base}/v1/agents"))
            .bearer_auth(handle.shared.token())
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["agents"],
        json!([])
    );
    handle.shutdown();
}

/// People, channels and chat details: avatars and host kinds ride the roster,
/// chats carry their channel and recent speakers, and details mirror the
/// desktop's right-hand panel (agents, people, default responder, context,
/// workflow runs, skills, vaults).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn people_channels_and_chat_details() {
    let (endpoint, _model) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "gpt-4o", &[("README.md", "# demo\n")]).await;
    let ws = crate::uuid_of_room(&h.handle.shared.cfg.room);

    // The desktop files a channel, gives @coder a picture, loads a skill and
    // runs a workflow in the channel's chat.
    let avatar = "data:image/png;base64,iVBORw0KGgo=";
    let (channel_id, chat_id) = {
        let mut svc = open_service(&h.desktop).unwrap();
        let (ch, chat) = svc.create_channel(ws, "design", Some("UI and UX".into()), "gw").unwrap();
        svc.set_agent_avatar(chat.id, ws, h.agent_id, Some(avatar.into()), Some("#336699".into())).unwrap();
        let mut skill = hive_core::SkillProfile::new("Reviewer", "# Review code\nLook for bugs.");
        skill.source_url = Some("https://example.com/skill.md".into());
        svc.add_skill(chat.id, ws, skill).unwrap();
        let def = hive_core::WorkflowDefinition {
            id: Uuid::new_v4(),
            name: "Ship it".into(),
            description: String::new(),
            input_label: None,
            nodes: vec![hive_core::workflow::WorkflowNode {
                id: "draft".into(),
                name: "Draft".into(),
                depends_on: vec![],
                kind: hive_core::WorkflowNodeKind::Agent { agent_id: None, prompt_template: "go".into() },
                x: None,
                y: None,
            }],
            created_at: hive_core::Timestamp::now(),
            disabled: false,
        };
        let me = svc.author().id.clone();
        svc.upsert_workflow_run(chat.id, ws, hive_core::workflow::new_run(&def, "now", me)).unwrap();
        svc.post_user_message(chat.id, ws, "kicking off the design pass").unwrap();
        (ch.id, chat.id)
    };
    sync_once(&h.desktop).await.unwrap();

    // The serve box syncs on its own loop; wait for the channel to arrive.
    let mut channels = Value::Null;
    for _ in 0..50 {
        channels = h.get("/v1/channels").await;
        if channels["channels"].as_array().is_some_and(|c| c.iter().any(|c| c["id"] == json!(channel_id))) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let design = channels["channels"].as_array().unwrap().iter().find(|c| c["id"] == json!(channel_id)).expect("channel synced");
    assert_eq!(design["name"], "design");
    assert_eq!(design["purpose"], "UI and UX");
    assert_eq!(design["archived"], json!(false));
    assert_eq!(design["defaultChatId"], json!(chat_id.to_string()));

    // Wait for the avatar edit and the message too.
    let mut chat = Value::Null;
    for _ in 0..50 {
        let chats = h.get("/v1/chats").await;
        chat = chats["chats"].as_array().unwrap().iter().find(|c| c["id"] == json!(chat_id)).cloned().unwrap_or(Value::Null);
        if chat["participants"].as_array().is_some_and(|p| !p.is_empty()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(chat["channelId"], json!(channel_id));
    let speaker = &chat["participants"][0];
    assert_eq!(speaker["kind"], "person");
    assert!(speaker.get("avatarUrl").is_none(), "chat rows never carry avatars");
    let desktop_actor = speaker["id"].as_str().unwrap().to_string();

    // Messages name a person's actor.
    let msgs = h.get(&format!("/v1/chats/{chat_id}/messages")).await;
    let m = msgs["messages"].as_array().unwrap().iter().find(|m| m["role"] == "user").unwrap();
    assert_eq!(m["actorId"], json!(desktop_actor));

    // Agents carry the avatar, color and where they run.
    let agents = h.get("/v1/agents").await;
    let coder = agents["agents"].as_array().unwrap().iter().find(|a| a["name"] == "coder").unwrap().clone();
    assert_eq!(coder["avatarUrl"], avatar);
    assert_eq!(coder["avatarColorHex"], "#336699");
    assert_eq!(coder["hostKind"], "ownerDevice");
    let host = this_host(&h).await;
    make_serve_admin(&h).await;
    let moved = h
        .send(reqwest::Method::PUT, &format!("/v1/agents/{}/host", h.agent_id), Some(json!({ "hostId": host })))
        .await;
    assert_eq!(moved["agent"]["hostKind"], "thisHost");
    assert_eq!(moved["agent"]["hostOnline"], json!(true));

    // People: the desktop founder (with role) and this box.
    let people = h.get("/v1/people").await;
    let list = people["people"].as_array().unwrap();
    let founder = list.iter().find(|p| p["actorId"] == json!(desktop_actor)).expect("founder listed");
    assert_eq!(founder["role"], "owner");
    assert_eq!(founder["isSelf"], json!(false));

    // Details.
    let d = h.get(&format!("/v1/chats/{chat_id}/details")).await;
    assert_eq!(d["chat"]["id"], json!(chat_id));
    assert_eq!(d["channel"]["name"], "design");
    let coder = d["agents"].as_array().unwrap().iter().find(|a| a["name"] == "coder").unwrap();
    assert_eq!(coder["inChat"], json!(false));
    assert_eq!(coder["avatarUrl"], avatar);
    let founder = d["people"].as_array().unwrap().iter().find(|p| p["actorId"] == json!(desktop_actor)).unwrap();
    assert_eq!(founder["inChat"], json!(true));
    assert_eq!(d["defaultResponder"]["kind"], "primary");
    assert_eq!(d["defaultResponder"]["name"], "hive");
    assert_eq!(d["defaultResponder"]["runtimeLabel"], "Lab gateway");
    assert_eq!(d["defaultResponder"]["model"], "gpt-4o");
    assert_eq!(d["context"]["estimated"], json!(true));
    assert_eq!(d["context"]["model"], "gpt-4o");
    assert!(d["context"]["windowTokens"].as_u64().unwrap() >= 8192);
    assert!(d["context"]["usedTokens"].as_u64().unwrap() > 0);
    assert!(d["context"]["skillTokens"].as_u64().unwrap() > 0);
    let run = &d["workflowRuns"][0];
    assert_eq!(run["name"], "Ship it");
    assert_eq!(run["status"], "running");
    assert_eq!(run["stepsDone"], json!(0));
    assert_eq!(run["stepsTotal"], json!(1));
    let skill = &d["skills"][0];
    assert_eq!(skill["name"], "Reviewer");
    assert_eq!(skill["summary"], "Review code");
    assert_eq!(skill["sourceUrl"], "https://example.com/skill.md");
    assert_eq!(d["vaults"], json!([]));

    // An unknown chat is a 404.
    let r = h.client.get(format!("{}/v1/chats/{}/details", h.base, Uuid::new_v4())).bearer_auth(&h.token).send().await.unwrap();
    assert_eq!(r.status(), 404);

    // A phone-made chat lands in the first channel (sidebar order).
    let made = h.send(reqwest::Method::POST, "/v1/chats", Some(json!({ "title": "From the phone" }))).await;
    assert_eq!(made["chat"]["channelId"], json!(channel_id));
    h.handle.shutdown();
}

/// The phone's URLSession only returns the response once body bytes arrive, so
/// a quiet event stream must send something at once — not wait for the 15 s
/// heartbeat (the app showed "Connecting" that long after every connect).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn event_stream_sends_bytes_immediately() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let resp = h.client.get(format!("{}/v1/events", h.base)).bearer_auth(&h.token).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let mut body = resp.bytes_stream();
    let first = tokio::time::timeout(Duration::from_secs(2), body.next())
        .await
        .expect("a first frame within 2 s")
        .expect("stream open")
        .unwrap();
    assert!(String::from_utf8_lossy(&first).starts_with(':'), "an SSE comment: {first:?}");
    h.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auth_is_required_except_for_bare_health() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;

    let anon: Value = h.client.get(format!("{}/v1/health", h.base)).send().await.unwrap().json().await.unwrap();
    assert_eq!(anon["ok"], json!(true));
    assert!(anon["version"].is_string());
    assert!(anon.get("workspace").is_none() && anon.get("deviceLabel").is_none(), "{anon}");

    let authed = h.get("/v1/health").await;
    assert!(authed["workspace"].as_str().unwrap().starts_with("serve-test-"));
    assert_eq!(authed["deviceLabel"], "sff");

    for path in ["/v1/chats", "/v1/agents", "/v1/hosts", "/v1/proposals", "/v1/lab/status", "/v1/dashboard", "/v1/appearance", "/v1/events"] {
        let none = h.client.get(format!("{}{path}", h.base)).send().await.unwrap();
        assert_eq!(none.status(), 401, "{path} without a token");
        let wrong = h.client.get(format!("{}{path}", h.base)).bearer_auth("nope").send().await.unwrap();
        assert_eq!(wrong.status(), 401, "{path} with a wrong token");
    }
    let post = h.client.post(format!("{}/v1/chats", h.base)).json(&json!({})).send().await.unwrap();
    assert_eq!(post.status(), 401);

    // Lab status is best-effort: an unreachable Prometheus is an empty snapshot.
    let lab = h.get("/v1/lab/status").await;
    assert_eq!(lab["workers"], json!([]));
    assert!(lab["updatedAt"].is_string());
    let r = h.client.get(format!("{}/v1/lab/status", h.base)).bearer_auth(&h.token).send().await.unwrap();
    assert_eq!(r.headers()["deprecation"], "true", "lab status is deprecated");

    // No dashboard.toml and no LIGUS_PROMETHEUS_URL: the built-in Ligus
    // section alone.
    let d = h.get("/v1/dashboard").await;
    let titles: Vec<&str> = d["sections"].as_array().unwrap().iter().map(|s| s["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["Ligus"], "{d}");
    assert_eq!(d["title"], "Dashboard");
    assert_eq!(d["source"], "default");
    let host = d["sections"][0]["panels"].as_array().unwrap().iter().find(|p| p["id"] == "hive-host").unwrap().clone();
    let labels: Vec<&str> = host["rows"].as_array().unwrap().iter().map(|r| r["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["Ligus serve", "Relay sync", "Push"]);
    assert_eq!(host["rows"][1]["up"], true, "the start-up sync counts: {host}");
    assert_eq!(host["rows"][2]["text"], "ntfy");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dashboard_serves_the_hosts_config_with_per_panel_errors() {
    use axum::routing::get;
    let json_src = spawn(Router::new().route("/status", get(|| async { axum::Json(json!({"gpu": {"temp": 61.5}, "state": "ok"})) }))).await;
    let (endpoint, _model) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let dashboard_toml = format!(
        r#"
title = "Home"
[[section]]
title = "Box"
[[section.panel]]
id = "gpu-temp"
title = "GPU temp"
kind = "gauge"
unit = "°C"
url = "{json_src}/status"
path = "/gpu/temp"
max = 110
thresholds = [{{ value = 0, level = "ok" }}, {{ value = 80, level = "critical" }}]
[[section.panel]]
title = "Gateway"
kind = "status"
url = "{json_src}/status"
path = "/state"
[[section.panel]]
title = "Power"
query = "sum(power_watts)"
"#
    );
    let h = harness_with(&endpoint, "gemma4:12b", &[("README.md", "# demo\n")], |o| {
        std::fs::create_dir_all(o.dashboard_file.parent().unwrap()).unwrap();
        std::fs::write(&o.dashboard_file, &dashboard_toml).unwrap();
    })
    .await;
    let d = h.get("/v1/dashboard").await;
    assert_eq!(d["title"], "Home");
    assert_eq!(d["source"], "config");
    // The built-in Ligus section comes first (the file has no `builtin` key),
    // read from this box's own workspace.
    assert_eq!(d["sections"][0]["title"], "Ligus");
    let hive = d["sections"][0]["panels"].as_array().unwrap();
    assert!(hive.iter().all(|p| p.get("error").is_none()), "{}", d["sections"][0]);
    let agents_online = hive.iter().find(|p| p["id"] == "hive-agents-online").unwrap();
    // The harness's one agent runs on its owner's desktop, which never
    // registered a host here: not counted as online.
    assert_eq!(agents_online["value"], json!(0.0), "{agents_online}");
    let open = hive.iter().find(|p| p["id"] == "hive-proposals-open").unwrap();
    assert_eq!(open["value"], json!(0.0));
    let host = hive.iter().find(|p| p["id"] == "hive-host").unwrap();
    assert_eq!(host["rows"][0], json!({"label": "Ligus serve", "up": true, "text": "Online"}));
    let panels = d["sections"][1]["panels"].as_array().unwrap();
    assert_eq!(panels[0]["id"], "gpu-temp");
    assert_eq!(panels[0]["value"], json!(61.5));
    assert_eq!(panels[0]["max"], json!(110.0));
    assert!(panels[0].get("error").is_none(), "{}", panels[0]);
    assert_eq!(panels[1]["rows"], json!([{"label": "Gateway", "up": true, "text": "ok"}]));
    // Prometheus at 127.0.0.1:9 is unreachable: that panel errors, the rest stand.
    assert!(panels[2]["error"].as_str().unwrap().contains("Prometheus"), "{}", panels[2]);
    assert!(panels[2].get("value").is_none());
}

/// "Match host" on the phone: `/v1/appearance` reports the desktop app's saved
/// theme on this machine, live (no restart), and serve config pins a field.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn appearance_follows_the_desktop_file_and_config() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let desk = tempfile::tempdir().unwrap();
    let dir = desk.path().to_path_buf();
    let h = harness_with(&endpoint, "m", &[("README.md", "x\n")], |o| {
        o.appearance = appearance::AppearanceConfig { desktop_dir: Some(dir), ..Default::default() };
    })
    .await;
    assert_eq!(h.get("/v1/appearance").await, json!({"palette": null, "mode": null, "source": "none"}));

    std::fs::write(desk.path().join(appearance::FILE_NAME), r#"{"palette":"harbor","mode":"dark"}"#).unwrap();
    assert_eq!(h.get("/v1/appearance").await, json!({"palette": "harbor", "mode": "dark", "source": "desktop"}));
    h.handle.shutdown();

    let pinned = harness_with(&endpoint, "m", &[("README.md", "x\n")], |o| {
        o.appearance = appearance::AppearanceConfig { palette: Some("obsidian".into()), mode: None, desktop_dir: None };
    })
    .await;
    assert_eq!(pinned.get("/v1/appearance").await, json!({"palette": "obsidian", "mode": null, "source": "config"}));
    pinned.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn phone_message_to_hosted_agent_streams_reply_and_files_a_proposal() {
    let mut rounds = vec![streamed_call("call_1", "write_file", r#"{"path":"hello.txt","content":"hello from the phone\n"}"#)];
    rounds.push(vec![delta(json!({"content": "Done: "})), delta(json!({"content": "added hello.txt"}))]);
    let (endpoint, model) = spawn_model(rounds).await;
    let h = harness(&endpoint, "gemma4:12b", &[("README.md", "# demo\n")]).await;

    // The desktop's agent is visible, on its owner's device until sent here.
    let agents = h.get("/v1/agents").await;
    let coder = agents["agents"].as_array().unwrap().iter().find(|a| a["name"] == "coder").expect("coder listed").clone();
    assert_eq!(coder["id"], json!(h.agent_id));
    assert_eq!(coder["runtimeLabel"], "Lab gateway");
    assert_eq!(coder["model"], "gemma4:12b");
    assert!(coder.get("hostId").is_none());

    let host = this_host(&h).await;
    make_serve_admin(&h).await;
    let moved = h
        .send(reqwest::Method::PUT, &format!("/v1/agents/{}/host", h.agent_id), Some(json!({ "hostId": host })))
        .await;
    assert_eq!(moved["agent"]["hostId"], json!(host));
    assert_eq!(moved["agent"]["hostLabel"], "sff (this host)");
    // An unknown host is refused.
    let bad = h
        .client
        .put(format!("{}/v1/agents/{}/host", h.base, h.agent_id))
        .bearer_auth(&h.token)
        .json(&json!({ "hostId": "not-a-host" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);

    let chat = h.send(reqwest::Method::POST, "/v1/chats", Some(json!({ "title": "From the phone" }))).await;
    let chat_id = chat["chat"]["id"].as_str().unwrap().to_string();
    assert_eq!(chat["chat"]["title"], "From the phone");

    let mut rx = h.events(None).await;
    let posted = h
        .send(
            reqwest::Method::POST,
            &format!("/v1/chats/{chat_id}/messages"),
            Some(json!({ "body": "@coder please add a hello file" })),
        )
        .await;
    assert_eq!(posted["message"]["role"], "user");
    assert_eq!(posted["message"]["author"], "Phone");
    assert_eq!(posted["message"]["chatId"], json!(chat_id));

    let seen = collect_until(&mut rx, 30, |evs| {
        evs.iter().any(|(n, _, _)| n == "proposal.created")
            && evs.iter().any(|(n, d, _)| n == "turn.status" && d["state"] == "done")
            && evs.iter().any(|(n, d, _)| n == "message.done" && d["message"]["role"] == "agent")
    })
    .await;
    let names: Vec<&str> = seen.iter().map(|(n, _, _)| n.as_str()).collect();
    let find = |name: &str| seen.iter().find(|(n, _, _)| n == name).map(|(_, d, _)| d.clone());

    let created = seen
        .iter()
        .find(|(n, d, _)| n == "message.created" && d["message"]["role"] == "agent")
        .map(|(_, d, _)| d.clone())
        .unwrap_or_else(|| panic!("agent message.created missing: {names:?}"));
    assert_eq!(created["message"]["streaming"], json!(true));
    assert_eq!(created["message"]["agentId"], json!(h.agent_id));
    let reply_id = created["message"]["id"].clone();

    let deltas: String = seen
        .iter()
        .filter(|(n, d, _)| n == "message.delta" && d["messageId"] == reply_id)
        .map(|(_, d, _)| d["text"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(deltas.contains("added hello.txt"), "deltas: {deltas:?} / {names:?}");
    let delta_ev = find("message.delta").unwrap();
    assert_eq!(delta_ev["chatId"], json!(chat_id));

    let done = seen
        .iter()
        .find(|(n, d, _)| n == "message.done" && d["message"]["id"] == reply_id)
        .map(|(_, d, _)| d.clone())
        .unwrap_or_else(|| panic!("message.done missing: {names:?}"));
    assert_eq!(done["message"]["streaming"], json!(false));
    assert_eq!(done["message"]["body"], "Done: added hello.txt");
    assert!(seen.iter().any(|(n, d, _)| n == "turn.status" && d["state"] == "running" && d["agentId"] == json!(h.agent_id)));
    assert!(names.contains(&"chat.updated"));
    // Event ids increase.
    assert!(seen.windows(2).all(|w| w[0].2 < w[1].2));

    let proposal = find("proposal.created").unwrap()["proposal"].clone();
    assert_eq!(proposal["kind"], "fileDiff");
    assert_eq!(proposal["status"], "open");
    assert_eq!(proposal["chatId"], json!(chat_id));
    assert_eq!(proposal["author"], "@coder");
    assert!(proposal["diff"].as_str().unwrap().contains("+hello from the phone"));

    // The model was offered the built-in tools on the first round.
    {
        let bodies = model.bodies.lock().unwrap();
        let tools: Vec<&str> = bodies[0]["tools"]
            .as_array()
            .expect("tools offered")
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        assert!(tools.contains(&"write_file") && tools.contains(&"read_file"), "{tools:?}");
        assert!(!tools.contains(&"run_command"), "run_command is off by default");
    }

    // REST views agree.
    let chats = h.get("/v1/chats").await;
    let listed = chats["chats"].as_array().unwrap().iter().find(|c| c["id"] == json!(chat_id)).unwrap().clone();
    assert_eq!(listed["preview"], "Done: added hello.txt");
    assert_eq!(listed["unread"], json!(1), "the agent's reply is unread until fetched");
    let msgs = h.get(&format!("/v1/chats/{chat_id}/messages")).await;
    let roles: Vec<&str> = msgs["messages"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, vec!["user", "agent"]);
    let after_user = msgs["messages"][0]["id"].as_str().unwrap();
    let page = h.get(&format!("/v1/chats/{chat_id}/messages?after={after_user}&limit=5")).await;
    assert_eq!(page["messages"].as_array().unwrap().len(), 1);
    let chats = h.get("/v1/chats").await;
    assert_eq!(chats["chats"][0]["unread"], json!(0), "fetching marked it read");

    // The edit lives only in the proposal: the checkout is untouched.
    let open = h.get("/v1/proposals").await;
    assert_eq!(open["proposals"].as_array().unwrap().len(), 1);
    let pid = open["proposals"][0]["id"].as_str().unwrap().to_string();
    let approved = h.send(reqwest::Method::POST, &format!("/v1/proposals/{pid}/approve"), None).await;
    assert_eq!(approved["proposal"]["status"], "approved");
    assert!(h.get("/v1/proposals?status=open").await["proposals"].as_array().unwrap().is_empty());
    assert_eq!(h.get("/v1/proposals?status=all").await["proposals"][0]["status"], "approved");

    // ntfy heard about the reply and the proposal, with a deep link and no content.
    tokio::time::sleep(Duration::from_millis(500)).await;
    {
        let pushes = h.ntfy.lock().unwrap();
        assert!(pushes.len() >= 2, "ntfy pushes: {}", pushes.len());
        for (headers, body) in pushes.iter() {
            assert_eq!(headers["click"].to_str().unwrap(), format!("hive://chat/{chat_id}"));
            assert!(!body.contains("hello from the phone"));
        }
        assert!(pushes.iter().any(|(hd, _)| hd["title"].to_str().unwrap() == "coder replied"));
        assert!(pushes.iter().any(|(hd, _)| hd["title"].to_str().unwrap() == "Change ready for review"));
    }

    // The desktop sees the phone's chat, the agent's reply and the approval
    // through the relay (E2EE, verified on ingest).
    let chat_uuid = Uuid::parse_str(&chat_id).unwrap();
    let mut synced = None;
    for _ in 0..40 {
        sync_once(&h.desktop).await.unwrap();
        let svc = open_service(&h.desktop).unwrap();
        if let Some(s) = svc.load(chat_uuid).unwrap() {
            let replied = s.messages.iter().any(|m| m.body == "Done: added hello.txt" && !m.is_streaming);
            let approved = s.proposals.iter().any(|p| p.status == hive_core::ProposalStatus::Approved);
            if replied && approved {
                synced = Some(s);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(synced.is_some(), "desktop never saw the reply + approval");
    h.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reject_with_reason_and_last_event_id_replay() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let chat = h.send(reqwest::Method::POST, "/v1/chats", None).await;
    let chat_id = Uuid::parse_str(chat["chat"]["id"].as_str().unwrap()).unwrap();
    assert_eq!(chat["chat"]["title"], "Untitled");

    // A proposal filed the way the worker files one.
    let pid = {
        let mut svc = h.handle.shared.lock();
        let ws = h.handle.shared.ws;
        let p = hive_core::ActionProposal::file_diff("@coder proposed changes to 1 file", "", "diff --git a/x b/x\n".into(), vec!["x".into()]);
        let id = p.id;
        svc.upsert_proposal(chat_id, ws, p).unwrap();
        id
    };
    h.handle.shared.after_write();

    let rejected = h
        .send(reqwest::Method::POST, &format!("/v1/proposals/{pid}/reject"), Some(json!({ "reason": "not like this" })))
        .await;
    assert_eq!(rejected["proposal"]["status"], "rejected");
    {
        let svc = h.handle.shared.lock();
        let s = svc.load(chat_id).unwrap().unwrap();
        let p = s.proposals.iter().find(|p| p.id == pid).unwrap();
        assert_eq!(p.refinements.len(), 1);
        assert_eq!(p.refinements[0].text, "not like this");
    }
    let missing = h
        .client
        .post(format!("{}/v1/proposals/{}/approve", h.base, Uuid::new_v4()))
        .bearer_auth(&h.token)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);

    // A reconnecting client replays what it missed after Last-Event-ID 0.
    let mut rx = h.events(Some(0)).await;
    let seen = collect_until(&mut rx, 5, |evs| evs.iter().any(|(n, _, _)| n == "proposal.created")).await;
    let names: Vec<&str> = seen.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(names.contains(&"chat.updated"), "{names:?}");
    let p = seen.iter().find(|(n, _, _)| n == "proposal.created").expect("replayed proposal.created");
    assert_eq!(p.1["proposal"]["id"], json!(pid));
    assert!(seen[0].2 >= 1);
    h.handle.shutdown();
}

/// File a proposal on a fresh chat the way the worker does (authorless, from
/// `@coder`), returning (chat, proposal).
async fn filed_proposal(h: &Harness, producing: Option<Uuid>) -> (Uuid, Uuid) {
    let chat = h.send(reqwest::Method::POST, "/v1/chats", None).await;
    let chat_id = Uuid::parse_str(chat["chat"]["id"].as_str().unwrap()).unwrap();
    let pid = {
        let mut svc = h.handle.shared.lock();
        let mut p = hive_core::ActionProposal::file_diff(
            "@coder proposed changes to 1 file",
            "",
            "diff --git a/x b/x\n".into(),
            vec!["x".into()],
        );
        p.producing_agent_id = producing;
        let id = p.id;
        svc.upsert_proposal(chat_id, h.handle.shared.ws, p).unwrap();
        id
    };
    h.handle.shared.after_write();
    (chat_id, pid)
}

async fn status_of(h: &Harness, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
    let mut req = h.client.request(method, format!("{}{path}", h.base)).bearer_auth(&h.token);
    if let Some(b) = body {
        req = req.json(&b);
    }
    let r = req.send().await.unwrap();
    let code = r.status().as_u16();
    (code, r.json().await.unwrap_or(Value::Null))
}

/// The desktop's Review card, from the phone: comments and change requests
/// land on the proposal's refinement thread, a vote is tagged with the voter's
/// name and role, and each change reaches the stream as `proposal.updated`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn comments_and_votes_show_who_and_stream_updates() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let (_, pid) = filed_proposal(&h, Some(h.agent_id)).await;
    let mut rx = h.events(None).await;

    let listed = h.get("/v1/proposals").await["proposals"][0].clone();
    assert_eq!(listed["commentCount"], 0);
    assert_eq!(listed["requiredApprovals"], 1);
    assert_eq!(listed["qualifyingApprovals"], 0);
    assert_eq!(listed["votes"], json!([]));
    assert_eq!(listed["myVote"], Value::Null);
    assert_eq!(listed["canVote"], true);

    let empty = status_of(&h, reqwest::Method::POST, &format!("/v1/proposals/{pid}/comments"), Some(json!({ "text": "  " }))).await;
    assert_eq!(empty.0, 400);

    let c = h
        .send(reqwest::Method::POST, &format!("/v1/proposals/{pid}/comments"), Some(json!({ "text": "Looks close" })))
        .await;
    assert_eq!(c["comment"]["text"], "Looks close");
    assert_eq!(c["comment"]["requestChanges"], false);
    assert_eq!(c["comment"]["author"], "Phone");
    assert_eq!(c["proposal"]["commentCount"], 1);
    let cr = h
        .send(
            reqwest::Method::POST,
            &format!("/v1/proposals/{pid}/comments"),
            Some(json!({ "text": "Rename the helper", "requestChanges": true })),
        )
        .await;
    assert_eq!(cr["proposal"]["changeRequestCount"], 1);
    let thread = h.get(&format!("/v1/proposals/{pid}/comments")).await;
    let texts: Vec<&str> = thread["comments"].as_array().unwrap().iter().map(|c| c["text"].as_str().unwrap()).collect();
    assert_eq!(texts, vec!["Looks close", "Rename the helper"]);

    let approved = h.send(reqwest::Method::POST, &format!("/v1/proposals/{pid}/approve"), None).await;
    let p = &approved["proposal"];
    assert_eq!(p["status"], "approved");
    assert_eq!(p["myVote"], "approve");
    assert_eq!(p["qualifyingApprovals"], 1);
    assert_eq!(p["quorumMet"], true);
    assert_eq!(p["votes"][0]["voter"], "Phone");
    assert_eq!(p["votes"][0]["approved"], true);
    assert!(["owner", "admin", "contributor"].contains(&p["votes"][0]["role"].as_str().unwrap()), "{p}");
    // A later reject replaces this voter's vote (latest wins), as on the desktop.
    let rejected = h.send(reqwest::Method::POST, &format!("/v1/proposals/{pid}/reject"), None).await;
    assert_eq!(rejected["proposal"]["myVote"], "reject");
    assert_eq!(rejected["proposal"]["votes"].as_array().unwrap().len(), 1);

    let one = h.get(&format!("/v1/proposals/{pid}")).await;
    assert_eq!(one["proposal"]["status"], "rejected");
    assert_eq!(one["proposal"]["comments"].as_array().unwrap().len(), 2);

    let seen = collect_until(&mut rx, 5, |evs| {
        evs.iter().filter(|(n, d, _)| n == "proposal.updated" && d["proposal"]["id"] == json!(pid)).count() >= 4
    })
    .await;
    let updates: Vec<&Value> = seen.iter().filter(|(n, _, _)| n == "proposal.updated").map(|(_, d, _)| &d["proposal"]).collect();
    assert!(updates.len() >= 4, "{seen:?}");
    assert_eq!(updates.last().unwrap()["status"], "rejected");

    // And it synced: the desktop sees the thread and the vote.
    sync_once(&h.desktop).await.unwrap();
    let mut desktop_seen = false;
    for _ in 0..30 {
        sync_once(&h.desktop).await.unwrap();
        let svc = open_service(&h.desktop).unwrap();
        let found = svc
            .store()
            .list_session_ids()
            .unwrap()
            .into_iter()
            .filter_map(|id| svc.load(id).ok().flatten())
            .flat_map(|s| s.proposals)
            .find(|p| p.id == pid);
        if found.as_ref().is_some_and(|p| p.refinements.len() == 2 && p.approvals.len() == 1) {
            desktop_seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(desktop_seen, "the desktop never saw the phone's notes and vote");
    h.handle.shutdown();
}

/// "Send to agent to revise" from the phone: the change request rides the
/// thread, the instruction is posted to the producing agent as a mention, and
/// the revision that agent files comes back linked to the original.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn request_changes_asks_the_agent_and_links_the_revision() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let (chat_id, pid) = filed_proposal(&h, Some(h.agent_id)).await;

    // Nothing on the thread yet: nothing to forward.
    let bare = status_of(&h, reqwest::Method::POST, &format!("/v1/proposals/{pid}/request-changes"), None).await;
    assert_eq!(bare.0, 400, "{}", bare.1);

    let res = h
        .send(
            reqwest::Method::POST,
            &format!("/v1/proposals/{pid}/request-changes"),
            Some(json!({ "note": "Use a constant, not a literal" })),
        )
        .await;
    let body = res["message"]["body"].as_str().unwrap();
    assert!(body.starts_with("@coder Please revise your proposal"), "{body}");
    assert!(body.contains("- Use a constant, not a literal"), "{body}");
    let thread: Vec<&str> =
        res["proposal"]["comments"].as_array().unwrap().iter().map(|c| c["text"].as_str().unwrap()).collect();
    assert_eq!(thread, vec!["Use a constant, not a literal", "Sent to the agent to revise."]);
    assert!(res["proposal"]["comments"].as_array().unwrap().iter().all(|c| c["requestChanges"] == true));
    {
        let svc = h.handle.shared.lock();
        let s = svc.load(chat_id).unwrap().unwrap();
        let pending = hive_runtime::chat_service::pending_mentions(&s);
        assert!(pending.iter().any(|m| m.agent_id == h.agent_id), "the revision request is queued work for @coder");
    }

    // The agent's turn files a new version (wherever it ran).
    let mut rx = h.events(None).await;
    let child = {
        let mut svc = h.handle.shared.lock();
        let mut p = hive_core::ActionProposal::file_diff(
            "@coder proposed changes to 1 file",
            "",
            "diff --git a/y b/y\n".into(),
            vec!["y".into()],
        );
        p.producing_agent_id = Some(h.agent_id);
        let id = p.id;
        svc.upsert_proposal(chat_id, h.handle.shared.ws, p).unwrap();
        id
    };
    h.handle.shared.after_write();
    let all = h.get("/v1/proposals?status=all").await;
    let find = |id: Uuid| all["proposals"].as_array().unwrap().iter().find(|p| p["id"] == json!(id)).unwrap().clone();
    assert_eq!(find(child)["parentId"], json!(pid));
    assert_eq!(find(pid)["supersededBy"], json!(child));
    let open: Vec<Value> = h.get("/v1/proposals").await["proposals"].as_array().unwrap().clone();
    assert_eq!(open.len(), 1, "the superseded version left the inbox");
    assert_eq!(open[0]["id"], json!(child));
    let seen = collect_until(&mut rx, 5, |evs| {
        evs.iter().any(|(n, d, _)| n == "proposal.updated" && d["proposal"]["supersededBy"] == json!(child))
    })
    .await;
    assert!(seen.iter().any(|(n, _, _)| n == "proposal.created"), "{seen:?}");

    // A revised proposal can't be sent back again.
    let again = status_of(&h, reqwest::Method::POST, &format!("/v1/proposals/{pid}/request-changes"), Some(json!({ "note": "x" }))).await;
    assert_eq!(again.0, 409);

    // One from the chat's primary runtime has no agent the phone can address.
    let (_, primary) = filed_proposal(&h, None).await;
    let res = status_of(&h, reqwest::Method::POST, &format!("/v1/proposals/{primary}/request-changes"), Some(json!({ "note": "x" }))).await;
    assert_eq!(res.0, 409, "{}", res.1);
    h.handle.shutdown();
}

/// Role gates hold exactly as on the desktop: a viewer can't vote, comment or
/// send a proposal back — the core's authz refuses the event and the phone
/// gets 403 — and the proposal JSON says so up front (`canVote: false`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_viewer_cannot_vote_comment_or_request_changes() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let (chat_id, pid) = filed_proposal(&h, Some(h.agent_id)).await;
    let me = h.handle.shared.my_actor_id();
    let ws = h.handle.shared.ws;

    // Wait for the box's own membership to reach the desktop, then the owner
    // demotes it to viewer.
    let mut demoted = false;
    for _ in 0..50 {
        sync_once(&h.desktop).await.unwrap();
        let mut svc = open_service(&h.desktop).unwrap();
        let cfg = workspace_config_session_id(ws);
        let known = svc.load(cfg).unwrap().is_some_and(|s| s.members.iter().any(|m| m.id == me));
        if known {
            svc.set_member_role(cfg, ws, me.clone(), hive_core::WorkspaceRole::Viewer).unwrap();
            drop(svc);
            sync_once(&h.desktop).await.unwrap();
            demoted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(demoted, "the box never joined the desktop's roster");
    let mut is_viewer = false;
    for _ in 0..50 {
        let s = h.handle.shared.lock().load(chat_id).unwrap().unwrap();
        if s.role_of(&me) == hive_core::WorkspaceRole::Viewer {
            is_viewer = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(is_viewer, "the demotion never synced to the box");

    let p = h.get(&format!("/v1/proposals/{pid}")).await;
    assert_eq!(p["proposal"]["canVote"], false);
    assert_eq!(p["proposal"]["myRole"], "viewer");
    for (path, body) in [
        (format!("/v1/proposals/{pid}/approve"), None),
        (format!("/v1/proposals/{pid}/reject"), None),
        (format!("/v1/proposals/{pid}/comments"), Some(json!({ "text": "hi" }))),
        (format!("/v1/proposals/{pid}/request-changes"), Some(json!({ "note": "redo" }))),
    ] {
        let (code, v) = status_of(&h, reqwest::Method::POST, &path, body).await;
        assert_eq!(code, 403, "{path}: {v}");
    }
    let s = h.handle.shared.lock().load(chat_id).unwrap().unwrap();
    let p = s.proposals.iter().find(|p| p.id == pid).unwrap();
    assert!(p.approvals.is_empty() && p.refinements.is_empty(), "a denied write left a trace");
    assert_eq!(p.status, hive_core::ProposalStatus::Open);
    drop(s);
    h.handle.shutdown();
}

/// Approving from the phone must not apply a proposal a qualifying reviewer
/// already rejected: quorum stays met after a reject, but the folded status is
/// Rejected, and Applied would be terminal on every peer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn apply_on_approve_never_overrides_a_rejection() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness_with(&endpoint, "m", &[("README.md", "x\n")], |o| o.apply_on_approve = true).await;
    let chat = h.send(reqwest::Method::POST, "/v1/chats", None).await;
    let chat_id = Uuid::parse_str(chat["chat"]["id"].as_str().unwrap()).unwrap();
    let ws = h.handle.shared.ws;
    let diff = |name: &str| {
        format!("diff --git a/{name} b/{name}\nnew file mode 100644\n--- /dev/null\n+++ b/{name}\n@@ -0,0 +1 @@\n+hi\n")
    };
    let file = |name: &str| {
        let mut p = hive_core::ActionProposal::file_diff(format!("add {name}"), "", diff(name), vec![name.into()]);
        p.producing_agent_id = Some(h.agent_id);
        p
    };
    let (rejected, control) = (file("rejected.txt"), file("approved.txt"));
    let (rid, cid) = (rejected.id, control.id);
    {
        let mut svc = h.handle.shared.lock();
        svc.upsert_proposal(chat_id, ws, rejected).unwrap();
        svc.upsert_proposal(chat_id, ws, control).unwrap();
    }
    h.handle.shared.after_write();
    // The desktop founder (Owner) rejected it, and that vote synced in.
    for _ in 0..100 {
        sync_once(&h.desktop).await.unwrap();
        if open_service(&h.desktop).unwrap().load(chat_id).unwrap().is_some_and(|c| c.proposals.iter().any(|p| p.id == rid)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    open_service(&h.desktop).unwrap().vote_on_proposal(chat_id, ws, rid, false).unwrap().expect("desktop sees the proposal");
    sync_once(&h.desktop).await.unwrap();
    for _ in 0..200 {
        let seen = h.handle.shared.lock().load(chat_id).unwrap().unwrap();
        if seen.proposals.iter().find(|p| p.id == rid).is_some_and(|p| p.status == hive_core::ProposalStatus::Rejected) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let res = h.send(reqwest::Method::POST, &format!("/v1/proposals/{rid}/approve"), None).await;
    assert_eq!(res["proposal"]["status"], "rejected", "{res}");
    let wt = h._tmp.path().join("repo").join(".ligus").join("worktrees").join(format!("chat-{chat_id}"));
    assert!(!wt.join("rejected.txt").exists(), "a rejected proposal was applied");
    {
        let svc = h.handle.shared.lock();
        let s = svc.load(chat_id).unwrap().unwrap();
        let p = s.proposals.iter().find(|p| p.id == rid).unwrap();
        assert_eq!(p.status, hive_core::ProposalStatus::Rejected);
    }

    // Control: the same flow without a rejection applies and commits.
    let res = h.send(reqwest::Method::POST, &format!("/v1/proposals/{cid}/approve"), None).await;
    assert_eq!(res["proposal"]["status"], "applied", "{res}");
    assert!(wt.join("approved.txt").exists(), "the approved proposal was applied in the chat worktree");
    assert!(!wt.join("rejected.txt").exists());
    h.handle.shutdown();
}

/// An alternative in an open proposal set is never applied on approval: the
/// desktop applies it only after the set's pre-flight. The phone sees which
/// set it belongs to.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn apply_on_approve_skips_proposal_set_alternatives() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness_with(&endpoint, "m", &[("README.md", "x\n")], |o| o.apply_on_approve = true).await;
    let chat = h.send(reqwest::Method::POST, "/v1/chats", None).await;
    let chat_id = Uuid::parse_str(chat["chat"]["id"].as_str().unwrap()).unwrap();
    let ws = h.handle.shared.ws;
    let diff = "diff --git a/alt.txt b/alt.txt\nnew file mode 100644\n--- /dev/null\n+++ b/alt.txt\n@@ -0,0 +1 @@\n+hi\n";
    let mut a = hive_core::ActionProposal::file_diff("alt A", "", diff.into(), vec!["alt.txt".into()]);
    a.producing_agent_id = Some(h.agent_id);
    let b = hive_core::ActionProposal::new("alt B", hive_core::ProposalKind::Decision, "planner");
    let aid = a.id;
    let set_id = {
        let mut svc = h.handle.shared.lock();
        let set = hive_core::ProposalSet::new("Which?", "planner", vec![]);
        svc.open_proposal_set(chat_id, ws, set, vec![a, b]).unwrap().id
    };
    h.handle.shared.after_write();
    let res = h.send(reqwest::Method::POST, &format!("/v1/proposals/{aid}/approve"), None).await;
    assert_eq!(res["proposal"]["status"], "approved", "{res}");
    assert_eq!(res["proposal"]["setId"], json!(set_id));
    let wt = h._tmp.path().join("repo").join(".ligus").join("worktrees").join(format!("chat-{chat_id}"));
    assert!(!wt.join("alt.txt").exists(), "a set alternative was applied without its pre-flight");
    h.handle.shutdown();
}

/// A turn cut off by a restart (placeholder still streaming, stamped as the
/// mention's reply) is finished as interrupted on the next start instead of
/// staying "streaming" forever and never being answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_finishes_a_reply_cut_off_mid_stream() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let chat = h.send(reqwest::Method::POST, "/v1/chats", None).await;
    let chat_id = Uuid::parse_str(chat["chat"]["id"].as_str().unwrap()).unwrap();
    let posted = h.send(reqwest::Method::POST, &format!("/v1/chats/{chat_id}/messages"), Some(json!({ "body": "@coder hi" }))).await;
    let mention = Uuid::parse_str(posted["message"]["id"].as_str().unwrap()).unwrap();
    let ws = h.handle.shared.ws;
    let reply = {
        let mut svc = h.handle.shared.lock();
        let mid = svc
            .begin_assistant_message(chat_id, ws, "coder".to_string(), "gw".to_string(), Some(h.agent_id), Some(mention))
            .unwrap();
        svc.append_chunk(chat_id, ws, mid, "partial answer".to_string()).unwrap();
        mid
    };
    assert!(h.handle.stop(Duration::from_secs(5)).await, "an idle worker stops at once");

    // Same box, same data dir: start again.
    let serve_cfg = config(&h._tmp.path().join("serve"), h.desktop.relay_url.as_deref().unwrap(), &h.desktop.room);
    let mut opts = ServeOptions::from_flags(&serve_cfg, Some("127.0.0.1:0".into()), None, None, None, None, Some("sff".into()), None, false)
        .unwrap();
    opts.prometheus_url = "http://127.0.0.1:9".into();
    let again = start(serve_cfg, opts).await.unwrap();
    {
        let svc = again.shared.lock();
        let s = svc.load(chat_id).unwrap().unwrap();
        let m = s.messages.iter().find(|m| m.id == reply).unwrap();
        assert!(!m.is_streaming, "still streaming after a restart");
        assert!(m.body.starts_with("partial answer"), "{}", m.body);
        assert!(m.body.ends_with(crate::worker::INTERRUPTED_NOTE), "{}", m.body);
    }
    let base = format!("http://{}", again.addr);
    let msgs: Value = h
        .client
        .get(format!("{base}/v1/chats/{chat_id}/messages"))
        .bearer_auth(again.shared.token())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let m = msgs["messages"].as_array().unwrap().iter().find(|m| m["id"] == json!(reply)).unwrap().clone();
    assert_eq!(m["streaming"], json!(false));
    again.shutdown();
}

/// Regression (an untagged "Hello" in a chat): the desktop's @hive greeting
/// listed the box's agents by handle, and each of them answered it with an
/// empty reply. Exactly one agent is woken, the greeting reaches its model as
/// the user's turn, and a model that returns nothing posts a notice instead
/// of an empty message.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_greeting_listing_agents_wakes_one_and_an_empty_model_posts_a_notice() {
    let (endpoint, model) = spawn_model(vec![vec![]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let ws = h.handle.shared.ws;
    let host = this_host(&h).await;
    let chat_agent = {
        let mut svc = open_service(&h.desktop).unwrap();
        let mut agent = WorkspaceAgent::new("chat", "gw");
        agent.owner_actor_id = svc.author().id.clone();
        let id = agent.id;
        svc.add_agent(ws, ws, agent).unwrap();
        id
    };
    sync_once(&h.desktop).await.unwrap();
    for _ in 0..50 {
        if h.get("/v1/agents").await["agents"].as_array().unwrap().len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    make_serve_admin(&h).await;
    for id in [h.agent_id, chat_agent] {
        h.send(reqwest::Method::PUT, &format!("/v1/agents/{id}/host"), Some(json!({ "hostId": host }))).await;
    }
    h.handle.shared.sync_kick.notify_one();
    tokio::time::sleep(Duration::from_millis(500)).await;
    sync_once(&h.desktop).await.unwrap();

    // On the desktop: Alex says hello; @hive greets and lists the agents.
    let chat_id = {
        let mut svc = open_service(&h.desktop).unwrap();
        let chat = svc.create_chat("general", ws, "gw").unwrap();
        svc.post_user_message(chat.id, ws, "Hello").unwrap();
        let m = svc.begin_assistant_message(chat.id, ws, "Hive", "gw", None, None).unwrap();
        svc.complete_assistant_message(chat.id, ws, m, "Hi Alex! I can take something myself, or hand it to @chat or @coder.")
            .unwrap();
        chat.id
    };
    sync_once(&h.desktop).await.unwrap();

    let replies = || {
        let svc = h.handle.shared.lock();
        svc.load(chat_id)
            .unwrap()
            .map(|s| s.messages.into_iter().filter(|m| m.responder_id.is_some()).collect::<Vec<_>>())
            .unwrap_or_default()
    };
    for _ in 0..100 {
        if replies().iter().any(|m| !m.is_streaming) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    // Give a second (wrong) responder time to show up, then check.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let got = replies();
    assert_eq!(got.len(), 1, "exactly one agent answers: {got:?}");
    assert_eq!(got[0].author, "chat", "the agent named first");
    assert!(!got[0].is_streaming);
    assert!(got[0].body.starts_with("[error]") && got[0].body.contains("empty reply"), "{}", got[0].body);

    // The model saw the greeting as the user's turn, not an assistant turn to continue.
    let bodies = model.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 1, "one model call");
    let last = bodies[0]["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["role"], "user", "{last}");
    assert!(last["content"].as_str().unwrap().contains("Hive: Hi Alex!"), "{last}");
    h.handle.shutdown();
}

/// refresh() runs at once from the sync loop, the worker and API handlers. A
/// refresh that loaded its projection before a newer one was diffed must not
/// roll the differ back, or the next refresh re-emits message.done (and a
/// second ntfy push). Deterministic: the test holds the differ lock while a
/// refresh is in flight and advances the differ itself in between.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_refresh_never_rolls_the_differ_back() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let chat = h.send(reqwest::Method::POST, "/v1/chats", None).await;
    let chat_id = Uuid::parse_str(chat["chat"]["id"].as_str().unwrap()).unwrap();
    let shared = h.handle.shared.clone();
    let ws = shared.ws;
    let mid = shared
        .lock()
        .begin_assistant_message(chat_id, ws, "coder".to_string(), "gw".to_string(), Some(h.agent_id), None)
        .unwrap();
    shared.refresh(); // the differ knows the reply is streaming

    let (_, mut rx) = shared.hub.subscribe(None);
    let guard = shared.differ.lock().unwrap();
    // A: a refresh that starts while the reply is still streaming.
    let slow = {
        let shared = shared.clone();
        std::thread::spawn(move || shared.refresh())
    };
    std::thread::sleep(Duration::from_millis(300));
    // The reply completes and a newer refresh diffs it first.
    shared.lock().complete_assistant_message(chat_id, ws, mid, "done").unwrap();
    let mut differ = guard;
    let changes = differ.diff(&shared.workspace_chats().unwrap());
    assert!(changes.contains(&hub::Change::MessageDone { chat: chat_id, message: mid }));
    drop(differ);
    slow.join().unwrap();
    shared.refresh();

    let mut repeats = 0;
    while let Ok(ev) = rx.try_recv() {
        if ev.name == "message.done" && ev.data["message"]["id"] == json!(mid) {
            repeats += 1;
        }
    }
    assert_eq!(repeats, 0, "message.done was emitted again after the differ had seen it");
    h.handle.shutdown();
}

/// A fresh box whose first sync fails must not seed its own workspace config
/// log (naming itself Owner); it refuses to start until the relay answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fresh_box_with_unreachable_relay_refuses_to_start() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("serve");
    let room = format!("serve-offline-{}", Uuid::new_v4());
    let cfg = config(&dir, "http://127.0.0.1:9", &room);
    let mut opts = ServeOptions::from_flags(&cfg, Some("127.0.0.1:0".into()), None, None, None, None, Some("sff".into()), None, false)
        .unwrap();
    opts.prometheus_url = "http://127.0.0.1:9".into();
    let err = start(cfg.clone(), opts).await.err().expect("must not start");
    assert!(err.to_string().contains("no copy of the workspace"), "{err}");
    let ws = crate::uuid_of_room(&room);
    assert!(open_service(&cfg).unwrap().load(workspace_config_session_id(ws)).unwrap().is_none(), "no config log seeded");
}

#[test]
fn serve_options_refuse_public_binds_and_need_both_ntfy_parts() {
    let cfg = Config {
        data_dir: PathBuf::from("/tmp/x"),
        relay_url: None,
        token: None,
        github_token: None,
        room: "r".into(),
        key: None,
    };
    let opts = |listen: &str, url: Option<&str>, topic: Option<&str>| {
        ServeOptions::from_flags(
            &cfg,
            Some(listen.into()),
            None,
            url.map(Into::into),
            topic.map(Into::into),
            None,
            Some("sff".into()),
            None,
            false,
        )
    };
    assert!(opts("0.0.0.0:8787", None, None).is_err());
    assert!(opts("100.101.102.104:8787", Some("http://n"), None).is_err());
    let ok = opts("100.101.102.104:8787", Some("http://n:8095"), Some("t")).unwrap();
    assert_eq!(ok.server_url(), "http://100.101.102.104:8787");
    assert_eq!(ok.token_file, PathBuf::from("/tmp/x/serve.token"));
    assert_eq!(ok.ntfy.unwrap().endpoint(), "http://n:8095/t");
}

/// Live: one agent turn end to end against the lab gateway with a built-in
/// tool call (`read_file` of a planted note). Skipped unless
/// `LIGUS_SERVE_LIVE_ENDPOINT` is set.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_gateway_turn_reads_a_file() {
    let Some(endpoint) = hive_core::compat::var("LIGUS_SERVE_LIVE_ENDPOINT").ok().filter(|s| !s.trim().is_empty()) else {
        eprintln!("skipping: LIGUS_SERVE_LIVE_ENDPOINT unset");
        return;
    };
    let models = hive_core::compat::var("LIGUS_SERVE_LIVE_MODELS").unwrap_or_else(|_| "gpt-oss:20b,gemma4:12b".into());
    for model in models.split(',').map(str::trim).filter(|m| !m.is_empty()) {
        let word = format!("lantern{}", &Uuid::new_v4().simple().to_string()[..6]);
        let note = format!("The secret word is {word}.\n");
        let h = harness(&endpoint, model, &[("NOTE.txt", note.as_str()), ("README.md", "# notes\n")]).await;
        let host = this_host(&h).await;
        make_serve_admin(&h).await;
        h.send(reqwest::Method::PUT, &format!("/v1/agents/{}/host", h.agent_id), Some(json!({ "hostId": host }))).await;
        let chat = h.send(reqwest::Method::POST, "/v1/chats", Some(json!({ "title": "live" }))).await;
        let chat_id = chat["chat"]["id"].as_str().unwrap().to_string();
        let mut rx = h.events(None).await;
        let started = std::time::Instant::now();
        h.send(
            reqwest::Method::POST,
            &format!("/v1/chats/{chat_id}/messages"),
            Some(json!({ "body": "@coder Use the read_file tool to read NOTE.txt, then reply with only the secret word it contains." })),
        )
        .await;
        let seen = collect_until(&mut rx, 300, |evs| {
            evs.iter().any(|(n, d, _)| n == "message.done" && d["message"]["role"] == "agent")
        })
        .await;
        let done = seen
            .iter()
            .find(|(n, d, _)| n == "message.done" && d["message"]["role"] == "agent")
            .unwrap_or_else(|| panic!("{model}: no reply within the timeout"));
        let body = done.1["message"]["body"].as_str().unwrap_or("");
        let deltas = seen.iter().filter(|(n, _, _)| n == "message.delta").count();
        eprintln!("{model}: {:.1}s, {deltas} deltas, reply: {body:?}", started.elapsed().as_secs_f64());
        assert!(body.contains(&word), "{model}: the reply should carry the word read from the file: {body:?}");
        h.handle.shutdown();
    }
}

#[path = "tunnel/e2e_tests.rs"]
mod tunnel_e2e;

/// Poll `GET /v1/health` until `identity.mode` is `mode`.
async fn wait_identity(h: &Harness, mode: &str) -> Value {
    let mut v = Value::Null;
    for _ in 0..100 {
        v = h.get("/v1/health").await["identity"].clone();
        if v["mode"] == mode {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("identity never became {mode}: {v}");
}

/// The owner approves a box's enrollment code on their desktop (here the
/// headless client standing in for it: the same `certify_device`).
async fn approve_on_desktop(desktop: &Config, code: &str) {
    let request = hive_runtime::device_enroll::DeviceEnrollment::decode(code).unwrap();
    open_service(desktop).unwrap().certify_device(&request).unwrap();
    sync_once(desktop).await.unwrap();
}

/// `LIGUS_SERVE_OWNER` + the owner's approval make this box one of the owner's
/// own devices: everything the phone does is authored as the owner and accepted
/// by the owner's desktop; one person, one vote; `@hive` in a chat the phone
/// creates belongs to the owner's desktop; revoking the box stops it, and a new
/// device key re-enrolls it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn phone_box_enrolled_as_the_owners_device() {
    let (endpoint, _model) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    // The desktop's identity is named "hive-cli" (the CLI test identity).
    let h = harness_with(&endpoint, "gpt-4o", &[("README.md", "# demo\n")], |o| o.owner = Some("hive-cli".into())).await;
    let ws = crate::uuid_of_room(&h.handle.shared.cfg.room);
    let cfg_id = workspace_config_session_id(ws);
    let desktop_actor = open_service(&h.desktop).unwrap().author().id.clone();

    // Before approval: the box waits — it neither joined the roster nor writes.
    let id = wait_identity(&h, "pending").await;
    assert_ne!(id["actorId"], json!(desktop_actor));
    let r = h.client.post(format!("{}/v1/chats", h.base)).bearer_auth(&h.token).json(&json!({"title": "x"})).send().await.unwrap();
    assert_eq!(r.status(), 403);
    let box_actor = h.handle.shared.my_actor_id();
    let roster = open_service(&h.desktop).unwrap().load(cfg_id).unwrap().unwrap().members;
    assert!(!roster.iter().any(|m| m.actor.id == box_actor), "a pending box doesn't join as its own member");

    // The owner approves the code `ligus serve` printed; the box switches live.
    let code = h.handle.shared.lock().enrollment_request(ws, "sff").encode();
    approve_on_desktop(&h.desktop, &code).await;
    let id = wait_identity(&h, "owner").await;
    assert_eq!(id["actorId"], json!(desktop_actor));
    assert_eq!(id["displayName"], "hive-cli");

    // The phone's chat, message, vote and comment are the owner's.
    let chat = h.send(reqwest::Method::POST, "/v1/chats", Some(json!({ "title": "from the phone" }))).await;
    let chat_id = chat["chat"]["id"].as_str().unwrap().to_string();
    let cid = Uuid::parse_str(&chat_id).unwrap();
    h.send(reqwest::Method::POST, &format!("/v1/chats/{chat_id}/messages"), Some(json!({ "body": "@hive what changed?" }))).await;
    let proposal = hive_core::ActionProposal::new("ship it", hive_core::ProposalKind::Decision, desktop_actor.clone());
    let pid = proposal.id;
    // Wait for the phone's chat to reach the desktop, then file a proposal in it.
    for _ in 0..50 {
        sync_once(&h.desktop).await.unwrap();
        if open_service(&h.desktop).unwrap().load(cid).unwrap().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    open_service(&h.desktop).unwrap().upsert_proposal(cid, ws, proposal).unwrap();
    sync_once(&h.desktop).await.unwrap();
    let mut found = false;
    for _ in 0..50 {
        if h.get("/v1/proposals").await["proposals"].as_array().unwrap().iter().any(|p| p["id"] == json!(pid)) {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(found, "the desktop's proposal reaches the phone");
    h.send(reqwest::Method::POST, &format!("/v1/proposals/{pid}/approve"), None).await;
    h.send(reqwest::Method::POST, &format!("/v1/proposals/{pid}/comments"), Some(json!({ "text": "looks right" }))).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    sync_once(&h.desktop).await.unwrap();
    let svc = open_service(&h.desktop).unwrap();
    let s = svc.load(cid).unwrap().expect("the phone's chat reached the desktop");
    assert_eq!(s.creator_actor_id, desktop_actor, "the chat is the owner's: their desktop runs its @hive");
    let m = s.messages.iter().find(|m| m.body == "@hive what changed?").expect("message accepted");
    assert_eq!(m.actor_identity.as_ref().unwrap().id, desktop_actor);
    let p = s.proposals.iter().find(|p| p.id == pid).unwrap();
    assert_eq!(p.approvals.len(), 1);
    assert_eq!(p.approvals[0].actor_id, desktop_actor);
    assert_eq!(p.refinements.last().unwrap().actor_id, desktop_actor);
    drop(svc);
    // One person, one vote: the desktop's vote replaces the phone's.
    open_service(&h.desktop).unwrap().vote_on_proposal(cid, ws, pid, false).unwrap();
    sync_once(&h.desktop).await.unwrap();
    let mut votes = Value::Null;
    for _ in 0..50 {
        votes = h.get(&format!("/v1/proposals/{pid}")).await["proposal"].clone();
        if votes["myVote"] == "reject" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(votes["myVote"], "reject", "{votes}");
    let s = open_service(&h.desktop).unwrap().load(cid).unwrap().unwrap();
    assert_eq!(s.proposals.iter().find(|p| p.id == pid).unwrap().approvals.len(), 1);

    // People: the phone's "you" is the owner.
    let people = h.get("/v1/people").await;
    let me: Vec<&Value> = people["people"].as_array().unwrap().iter().filter(|p| p["isSelf"] == json!(true)).collect();
    assert_eq!(me.len(), 1);
    assert_eq!(me[0]["actorId"], json!(desktop_actor));

    // @hive availability follows the owner's desktop heartbeat.
    {
        let mut svc = open_service(&h.desktop).unwrap();
        svc.register_host(ws, hive_core::WorkspaceHost::new(svc.device_id().to_string(), hive_core::HostKind::Device, "Studio Mac"))
            .unwrap();
    }
    sync_once(&h.desktop).await.unwrap();
    let mut r = Value::Null;
    for _ in 0..50 {
        r = h.get(&format!("/v1/chats/{chat_id}/details")).await["defaultResponder"].clone();
        if r["available"] == json!(true) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(r["availableVia"], "Studio Mac", "{r}");

    // Revoke: the box goes read-only, and what it already sent stays.
    let old_device = Uuid::parse_str(&h.handle.shared.host_id).unwrap();
    open_service(&h.desktop).unwrap().revoke_device(cfg_id, ws, old_device).unwrap();
    sync_once(&h.desktop).await.unwrap();
    wait_identity(&h, "revoked").await;
    let r = h
        .client
        .post(format!("{}/v1/chats/{chat_id}/messages", h.base))
        .bearer_auth(&h.token)
        .json(&json!({ "body": "after revocation" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);

    // Re-enroll: a new device key, approved again.
    let serve_cfg = h.handle.shared.cfg.clone();
    let opts = h.handle.shared.opts.clone();
    assert!(h.handle.stop(Duration::from_secs(5)).await);
    assert!(cmd_enroll(&serve_cfg, &opts, false).is_err(), "a revoked box needs --new-device");
    cmd_enroll(&serve_cfg, &opts, true).unwrap();
    let handle = start(serve_cfg.clone(), opts.clone()).await.unwrap();
    assert_ne!(handle.shared.host_id, old_device.to_string());
    let code = handle.shared.lock().enrollment_request(ws, "sff").encode();
    approve_on_desktop(&h.desktop, &code).await;
    let h2 = Harness { base: format!("http://{}", handle.addr), token: handle.shared.token(), handle, ..h };
    wait_identity(&h2, "owner").await;
    h2.send(reqwest::Method::POST, &format!("/v1/chats/{chat_id}/messages"), Some(json!({ "body": "back again" }))).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    sync_once(&h2.desktop).await.unwrap();
    let s = open_service(&h2.desktop).unwrap().load(cid).unwrap().unwrap();
    let bodies: Vec<(&str, &str)> =
        s.messages.iter().map(|m| (m.body.as_str(), m.actor_identity.as_ref().map(|a| a.id.as_str()).unwrap_or(""))).collect();
    assert!(bodies.contains(&("back again", desktop_actor.as_str())), "{bodies:?}");
    assert!(!bodies.iter().any(|(b, _)| *b == "after revocation"));
    h2.handle.shutdown();
}

/// A box that was its own member before `LIGUS_SERVE_OWNER` keeps the dispatch
/// stopgap until its owner approves it: the owner's desktop answers `@hive` in
/// chats the phone creates once the box is an Admin, and details say whether
/// that desktop is online (`available` / `availableVia`) from its heartbeat.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn phone_chat_primary_availability_follows_the_owners_desktop() {
    let (endpoint, _model) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    // An existing box: joined as its own member before the owner was set.
    let h0 = harness(&endpoint, "gpt-4o", &[("README.md", "# demo\n")]).await;
    let serve_cfg = h0.handle.shared.cfg.clone();
    let mut opts = h0.handle.shared.opts.clone();
    assert!(h0.handle.stop(Duration::from_secs(5)).await);
    opts.owner = Some("hive-cli".into());
    let handle = start(serve_cfg, opts).await.unwrap();
    let h = Harness { base: format!("http://{}", handle.addr), token: handle.shared.token(), handle, ..h0 };
    assert_eq!(h.get("/v1/health").await["identity"]["mode"], "pending");
    let ws = crate::uuid_of_room(&h.handle.shared.cfg.room);
    let cfg_id = workspace_config_session_id(ws);
    let (desktop_actor, box_actor, box_host) = {
        let svc = open_service(&h.desktop).unwrap();
        let hosts = h.handle.shared.lock().list_hosts(ws).unwrap();
        let mine = hosts.iter().find(|x| x.id == h.handle.shared.host_id).unwrap().clone();
        (svc.author().id.clone(), h.handle.shared.my_actor_id(), mine)
    };
    // The box registered its host as the owner's.
    assert_eq!(box_host.owner_actor_id, desktop_actor);

    let chat = h.send(reqwest::Method::POST, "/v1/chats", Some(json!({ "title": "phone @hive test" }))).await;
    let chat_id = chat["chat"]["id"].as_str().unwrap().to_string();
    let details = || async { h.get(&format!("/v1/chats/{chat_id}/details")).await["defaultResponder"].clone() };

    // Still a Contributor: the claim isn't honored, so the phone's chat is the
    // box's own, which can't run @hive — and nothing pretends otherwise.
    let r = details().await;
    assert_eq!(r["kind"], "primary");
    assert_eq!(r["available"], json!(false));
    assert_eq!(r["unavailableReason"], "noDesktop");
    assert!(r.get("availableVia").is_none());

    // The owner makes the box an Admin and opens the desktop (Device heartbeat).
    sync_once(&h.desktop).await.unwrap();
    {
        let mut svc = open_service(&h.desktop).unwrap();
        svc.set_member_role(cfg_id, ws, &box_actor, hive_core::WorkspaceRole::Admin).unwrap();
        svc.register_host(ws, hive_core::WorkspaceHost::new(svc.device_id().to_string(), hive_core::HostKind::Device, "Studio Mac"))
            .unwrap();
    }
    sync_once(&h.desktop).await.unwrap();
    let mut r = Value::Null;
    for _ in 0..50 {
        r = details().await;
        if r["available"] == json!(true) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(r["available"], json!(true), "{r}");
    assert_eq!(r["availableVia"], "Studio Mac");
    assert_eq!(h.handle.shared.lock().device_links(ws).unwrap().person(&box_actor), desktop_actor);

    // The desktop closes: its last heartbeat ages out.
    {
        let mut svc = open_service(&h.desktop).unwrap();
        let mut stale = hive_core::WorkspaceHost::new(svc.device_id().to_string(), hive_core::HostKind::Device, "Studio Mac");
        stale.last_seen = hive_core::Timestamp::from_unix_seconds(0);
        svc.register_host(ws, stale).unwrap();
    }
    sync_once(&h.desktop).await.unwrap();
    for _ in 0..50 {
        r = details().await;
        if r["available"] == json!(false) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(r["available"], json!(false), "{r}");
    assert_eq!(r["unavailableReason"], "desktopOffline");
    assert!(r.get("availableVia").is_none());

    // The desktop opens again, now beating on the relay's presence channel
    // instead of re-writing its durable host record: available, with no new
    // host event in the log.
    let desktop_host = open_service(&h.desktop).unwrap().device_id().to_string();
    let rows_before = h.handle.shared.lock().store().list(cfg_id).unwrap().len();
    h.handle.shared.presence.record(&desktop_host, hive_core::Timestamp::now().inner().unix_timestamp());
    let r = details().await;
    assert_eq!(r["available"], json!(true), "{r}");
    assert_eq!(r["availableVia"], "Studio Mac");
    assert_eq!(h.handle.shared.lock().store().list(cfg_id).unwrap().len(), rows_before);
    h.handle.shutdown();
}

/// A message the box wrote as its own (since retired) member reads on the
/// phone as the owner's, via the box.
#[test]
fn retired_box_messages_read_as_the_owner_via_the_box() {
    use hive_core::{ActorIdentity, ActorKind, ChatMessage, ChatSession, MessageRole, WorkspaceMember, WorkspaceRole};
    let owner = ActorIdentity::new("owner-1", "Alex", ActorKind::Human);
    let mut chat = ChatSession::new("c", Uuid::new_v4(), "rt");
    chat.members = vec![WorkspaceMember {
        id: owner.id.clone(),
        actor: owner.clone(),
        role: WorkspaceRole::Owner,
        title: String::new(),
        index: 1,
        joined_at: Default::default(),
    }];
    let mut links = hive_core::DeviceLinks::default();
    assert!(links.link_retired(&chat, "box-1", "owner-1"));
    let mut old = ChatMessage::new(MessageRole::User, "Phone", "from the box");
    old.actor_identity = Some(ActorIdentity::new("box-1", "Phone", ActorKind::Human));
    let v = api::linked_message_dto(&chat, &old, &links);
    assert_eq!((v["author"].as_str(), v["via"].as_str(), v["actorId"].as_str()), (Some("Alex via Phone"), Some("Phone"), Some("owner-1")));
    let mut mine = ChatMessage::new(MessageRole::User, "Alex", "from the desktop");
    mine.actor_identity = Some(owner);
    let v = api::linked_message_dto(&chat, &mine, &links);
    assert_eq!(v["author"], "Alex");
    assert!(v.get("via").is_none());
}

/// `sync_once` (a daemon's first sync, `ligus sync`, a worker tick) pages
/// through a room bigger than one relay page until it is caught up, instead
/// of stopping after the first page.
#[tokio::test]
async fn sync_once_pages_through_a_large_room() {
    let tmp = tempfile::tempdir().unwrap();
    let relay = spawn(test_relay::router_with_page_cap(2)).await;
    let room = format!("serve-test-{}", Uuid::new_v4());
    let ws = crate::uuid_of_room(&room);

    let desktop = config(&tmp.path().join("desktop"), &relay, &room);
    let chat_id = {
        let mut svc = open_service(&desktop).unwrap();
        svc.ensure_self_member(workspace_config_session_id(ws), ws).unwrap();
        let chat = svc.create_chat("general", ws, "gw").unwrap();
        for i in 0..60 {
            svc.post_user_message(chat.id, ws, format!("message {i}")).unwrap();
        }
        chat.id
    };
    let (pushed, _) = sync_once(&desktop).await.unwrap();
    assert!(pushed > 60, "pushed {pushed}");

    // Far more envelopes than one round (PULL_PAGES_PER_ROUND pages of 2).
    let fresh = config(&tmp.path().join("fresh"), &relay, &room);
    std::fs::create_dir_all(&fresh.data_dir).unwrap();
    let (_, pulled) = sync_once(&fresh).await.unwrap();
    assert_eq!(pulled, pushed, "the whole room in one sync_once");
    let svc = open_service(&fresh).unwrap();
    let chat = svc.load(chat_id).unwrap().expect("chat synced");
    assert_eq!(chat.messages.len(), 60);
}

// ── Repos from the phone (`repos_api.rs`) ──────────────────────────────────

/// GitHub stand-in: "repos" are bare repos under `remotes/<owner>/<name>.git`,
/// pull requests are recorded instead of opened.
struct FakeGitHub {
    remotes: PathBuf,
    available: bool,
    prs: Mutex<Vec<super::repos_api::PrRequest>>,
    /// "owner/name" of a fork → its parent (owner, name, default branch).
    parents: std::collections::HashMap<String, (String, String, String)>,
}

impl FakeGitHub {
    fn new(remotes: &Path, available: bool) -> Self {
        Self { remotes: remotes.to_path_buf(), available, prs: Mutex::new(Vec::new()), parents: Default::default() }
    }
}

impl super::repos_api::GitHub for FakeGitHub {
    fn status(&self) -> super::repos_api::GhStatus {
        if self.available {
            super::repos_api::GhStatus { available: true, login: Some("acme".into()), reason: None }
        } else {
            super::repos_api::GhStatus { available: false, login: None, reason: Some("Run gh auth login on the host".into()) }
        }
    }
    fn list(&self, q: &super::repos_api::RepoQuery, _login: &str) -> Result<Vec<super::repos_api::GhRepo>, String> {
        let mut out = Vec::new();
        for owner in std::fs::read_dir(&self.remotes).unwrap().flatten() {
            for repo in std::fs::read_dir(owner.path()).unwrap().flatten() {
                let name = repo.file_name().to_string_lossy().trim_end_matches(".git").to_string();
                if q.q.as_deref().is_some_and(|t| !name.contains(t)) {
                    continue;
                }
                out.push(super::repos_api::GhRepo {
                    owner: owner.file_name().to_string_lossy().into_owned(),
                    name,
                    description: String::new(),
                    private: false,
                    default_branch: "main".into(),
                    updated_at: String::new(),
                    url: String::new(),
                    fork: false,
                });
            }
        }
        Ok(out)
    }
    fn owners(&self, login: &str) -> Result<Vec<String>, String> {
        Ok(vec![login.to_string(), "acme-org".into()])
    }
    fn clone_url(&self, owner: &str, name: &str) -> String {
        format!("file://{}/{owner}/{name}.git", self.remotes.display())
    }
    fn git_auth_args(&self) -> Vec<String> {
        Vec::new()
    }
    fn repo_of_remote(&self, url: &str) -> Option<crate::repos::RemoteRef> {
        url.starts_with("file://").then(|| crate::repos::parse_remote(url)).flatten()
    }
    fn create_pr(&self, _dir: &Path, pr: &super::repos_api::PrRequest) -> Result<(String, bool), String> {
        let mut prs = self.prs.lock().unwrap();
        let existing = prs.iter().any(|p| p.repo == pr.repo && p.head == pr.head);
        prs.push(pr.clone());
        Ok((format!("https://github.com/{}/pull/1", pr.repo), existing))
    }
    fn repo_meta(&self, owner: &str, name: &str) -> Result<Option<super::repos_api::RepoMeta>, String> {
        if !self.remotes.join(owner).join(format!("{name}.git")).exists() {
            return Ok(None);
        }
        Ok(Some(super::repos_api::RepoMeta {
            owner: owner.into(),
            name: name.into(),
            default_branch: "main".into(),
            parent: self.parents.get(&format!("{owner}/{name}")).cloned(),
        }))
    }
}

/// A bare "GitHub" repo `owner/name` with one commit on `main`; returns a
/// working clone to push more commits from.
fn fake_remote(remotes: &Path, owner: &str, name: &str) -> PathBuf {
    let bare = remotes.join(owner).join(format!("{name}.git"));
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "--bare", "-b", "main"]);
    let work = remotes.parent().unwrap().join(format!("work-{owner}-{name}"));
    make_repo(&work, &[("README.md", "hello\n")]);
    git(&work, &["branch", "-M", "main"]);
    git(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(&work, &["push", "-q", "origin", "main"]);
    work
}

async fn wait_job(h: &Harness, id: &str) -> Value {
    for _ in 0..100 {
        let j = h.get(&format!("/v1/repos/clone/{id}")).await["job"].clone();
        if j["state"] != "running" {
            return j;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("clone job {id} never finished");
}

/// The phone lists repos, clones one, switches a chat onto it (and back),
/// pulls, and opens a PR from the chat's branch; the agent's turn is told
/// which repo and branch it works in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repos_clone_switch_pull_and_pr() {
    let (endpoint, model) = spawn_model(vec![vec![delta(json!({"content": "ok"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let remotes = h._tmp.path().join("remotes");
    std::fs::create_dir_all(&remotes).unwrap();
    let work = fake_remote(&remotes, "acme", "widget");
    let fake = Arc::new(FakeGitHub::new(&remotes, true));
    h.handle.shared.repos.set_github(fake.clone());

    // Listing: no local checkouts yet; GitHub lists the fake remote.
    let v = h.get("/v1/repos").await;
    assert_eq!(v["local"], json!([]));
    assert_eq!(v["workspaceDefault"], "repo", "an absolute workspace binding shows only its last component");
    assert!(!v.to_string().contains(h._tmp.path().to_str().unwrap()), "no host paths: {v}");
    assert_eq!(v["github"]["available"], true);
    assert_eq!(v["github"]["owners"], json!(["acme", "acme-org"]));
    assert_eq!(v["github"]["repos"][0]["fullName"], "acme/widget");
    assert_eq!(v["github"]["repos"][0]["cloned"], false);

    // Strict input.
    for bad in ["../etc", "a/b/c", "https://gitlab.com/a/b", "file:///etc", "git@github.com:a/b.git"] {
        let (code, _) = status_of(&h, reqwest::Method::POST, "/v1/repos/clone", Some(json!({ "repo": bad }))).await;
        assert_eq!(code, 400, "{bad}");
    }

    // Clone (a job), then again = fetch.
    let job = h.send(reqwest::Method::POST, "/v1/repos/clone", Some(json!({ "repo": "https://github.com/acme/widget" }))).await;
    assert_eq!(job["job"]["action"], "clone");
    let done = wait_job(&h, job["job"]["id"].as_str().unwrap()).await;
    assert_eq!(done["state"], "done", "{done}");
    let checkout = h.handle.shared.cfg.data_dir.join("repos").join("acme__widget");
    assert!(checkout.join("README.md").exists());
    let again = h.send(reqwest::Method::POST, "/v1/repos/clone", Some(json!({ "repo": "acme/widget" }))).await;
    assert_eq!(again["job"]["action"], "fetch");
    assert_eq!(wait_job(&h, again["job"]["id"].as_str().unwrap()).await["state"], "done");
    let v = h.get("/v1/repos?q=widg").await;
    assert_eq!(v["local"][0]["name"], "acme__widget");
    assert_eq!(v["local"][0]["defaultBranch"], "main");
    assert_eq!(v["local"][0]["dirty"], false);
    assert_eq!(v["github"]["repos"][0]["cloned"], true);
    assert_eq!(v["github"]["repos"][0]["checkout"], "acme__widget");

    // Switch a chat onto it.
    let chat = h.send(reqwest::Method::POST, "/v1/chats", Some(json!({ "title": "repo picker" }))).await;
    let chat_id = chat["chat"]["id"].as_str().unwrap().to_string();
    let r = h.get(&format!("/v1/chats/{chat_id}/repo")).await;
    assert_eq!(r["repo"]["source"], "workspace");
    for (bad, code) in [("../etc", 400), ("nope", 404), ("/abs", 400)] {
        let (c, _) = status_of(&h, reqwest::Method::PUT, &format!("/v1/chats/{chat_id}/repo"), Some(json!({ "repo": bad }))).await;
        assert_eq!(c, code, "{bad}");
    }
    let r = h.send(reqwest::Method::PUT, &format!("/v1/chats/{chat_id}/repo"), Some(json!({ "repo": "acme/widget" }))).await;
    assert_eq!(r["repo"]["name"], "acme__widget");
    assert_eq!(r["repo"]["source"], "chat");
    assert_eq!(r["chatBranch"], format!("hive/chat/{chat_id}"));
    let b = RepoBindings::load(&h.handle.shared.cfg.data_dir);
    assert_eq!(b.chats.get(&chat_id).map(String::as_str), Some("acme__widget"));
    let d = h.get(&format!("/v1/chats/{chat_id}/details")).await;
    assert_eq!(d["repo"]["repo"]["name"], "acme__widget");
    let msgs = h.get(&format!("/v1/chats/{chat_id}/messages")).await;
    assert!(msgs.to_string().contains("now works in acme__widget"), "{msgs}");

    // The agent's turn (hosted here) is told the repo and branch.
    let host = this_host(&h).await;
    make_serve_admin(&h).await;
    h.send(reqwest::Method::PUT, &format!("/v1/agents/{}/host", h.agent_id), Some(json!({ "hostId": host }))).await;
    h.send(reqwest::Method::POST, &format!("/v1/chats/{chat_id}/messages"), Some(json!({ "body": "@coder hi" }))).await;
    let mut system = String::new();
    for _ in 0..300 {
        if let Some(b) = model.bodies.lock().unwrap().first() {
            system = b["messages"][0]["content"].as_str().unwrap_or("").to_string();
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(system.contains("`acme__widget`") && system.contains(&format!("hive/chat/{chat_id}")), "{system}");

    // Pull fast-forwards a clean checkout.
    std::fs::write(work.join("NEW.md"), "new\n").unwrap();
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-qm", "upstream"]);
    git(&work, &["push", "-q", "origin", "main"]);
    let p = h.send(reqwest::Method::POST, &format!("/v1/chats/{chat_id}/repo/pull"), None).await;
    assert_eq!(p["updated"], true, "{p}");
    assert!(checkout.join("NEW.md").exists());
    std::fs::write(checkout.join("README.md"), "local edit\n").unwrap();
    let (c, v) = status_of(&h, reqwest::Method::POST, &format!("/v1/chats/{chat_id}/repo/pull"), None).await;
    assert_eq!(c, 409, "dirty checkout: {v}");
    git(&checkout, &["checkout", "--", "README.md"]);

    // PR: nothing on the chat branch yet → 409; then a commit there → pushed + PR.
    let pr_path = format!("/v1/chats/{chat_id}/pr");
    let (c, _) = status_of(&h, reqwest::Method::POST, &pr_path, Some(json!({ "title": "Add thing" }))).await;
    assert_eq!(c, 409);
    let wt = hive_runtime::git_worktree::chat_worktree(&checkout, &chat_id).unwrap();
    std::fs::write(wt.join("THING.md"), "thing\n").unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "thing"]);
    let r = h.get(&format!("/v1/chats/{chat_id}/repo")).await;
    assert_eq!((r["chatBranchExists"].clone(), r["ahead"].clone()), (json!(true), json!(1)));
    let (c, _) = status_of(&h, reqwest::Method::POST, &pr_path, Some(json!({ "title": "  " }))).await;
    assert_eq!(c, 400);
    let pr = h.send(reqwest::Method::POST, &pr_path, Some(json!({ "title": "Add thing", "body": "from the phone", "draft": true }))).await;
    assert_eq!(pr["url"], "https://github.com/acme/widget/pull/1");
    assert_eq!(pr["base"], "main");
    let rec = fake.prs.lock().unwrap().clone();
    assert_eq!(rec.len(), 1);
    assert_eq!((rec[0].head.as_str(), rec[0].base.as_str(), rec[0].draft), (format!("hive/chat/{chat_id}").as_str(), "main", true));
    let bare = remotes.join("acme").join("widget.git");
    git(&bare, &["rev-parse", "--verify", &format!("refs/heads/hive/chat/{chat_id}")]);
    let msgs = h.get(&format!("/v1/chats/{chat_id}/messages")).await;
    assert!(msgs.to_string().contains("Opened pull request: https://github.com/acme/widget/pull/1"));

    // Switch back to the workspace default; the chat branch in acme__widget stays.
    let r = h.send(reqwest::Method::PUT, &format!("/v1/chats/{chat_id}/repo"), Some(json!({ "repo": null }))).await;
    assert_eq!(r["repo"]["source"], "workspace");
    git(&checkout, &["rev-parse", "--verify", &format!("refs/heads/hive/chat/{chat_id}")]);
    let r = h.send(reqwest::Method::PUT, &format!("/v1/chats/{chat_id}/repo"), Some(json!({ "repo": "acme__widget" }))).await;
    assert_eq!(r["ahead"], 1, "switching back resumes the chat's branch");

    // Signed out: listing says so, PRs are refused clearly, public clones still work.
    let signed_out = Arc::new(FakeGitHub::new(&remotes, false));
    h.handle.shared.repos.set_github(signed_out);
    let v = h.get("/v1/repos").await;
    assert_eq!(v["github"], json!({ "available": false, "reason": "Run gh auth login on the host" }));
    let (c, v) = status_of(&h, reqwest::Method::POST, &pr_path, Some(json!({ "title": "x" }))).await;
    assert_eq!(c, 412, "{v}");
    assert!(v["error"].as_str().unwrap().contains("gh auth login"));
    fake_remote(&remotes, "pub", "lib");
    let job = h.send(reqwest::Method::POST, "/v1/repos/clone", Some(json!({ "repo": "pub/lib" }))).await;
    assert_eq!(wait_job(&h, job["job"]["id"].as_str().unwrap()).await["state"], "done");

    // A clone that fails cleans up after itself.
    let job = h.send(reqwest::Method::POST, "/v1/repos/clone", Some(json!({ "repo": "ghost/missing" }))).await;
    let j = wait_job(&h, job["job"]["id"].as_str().unwrap()).await;
    assert_eq!(j["state"], "error");
    let leftovers: Vec<_> = std::fs::read_dir(h.handle.shared.cfg.data_dir.join("repos"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(leftovers.iter().all(|n| !n.starts_with(".clone-") && n != "ghost__missing"), "{leftovers:?}");
}

fn rev(dir: &Path, r: &str) -> String {
    let out = hive_core::process_util::command("git").arg("-C").arg(dir).args(["rev-parse", r]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn git_ok(dir: &Path, args: &[&str]) -> bool {
    hive_core::process_util::command("git").arg("-C").arg(dir).args(args).output().unwrap().status.success()
}

/// A fork, end to end with bare repos: clone the parent "via my fork" (gets
/// `upstream`, fetch-only, base tracking upstream), chat branches start from
/// upstream, pull fast-forwards from upstream, and a PR pushes a readable
/// branch to the fork (same branch on later pushes, never forced) and opens
/// upstream as a draft. Nothing is ever pushed to the upstream repo.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fork_checkouts_base_on_upstream_and_open_prs_there() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "ok"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let remotes = h._tmp.path().join("remotes");
    std::fs::create_dir_all(&remotes).unwrap();
    // Upstream project, and the owner's fork of it (a copy that then falls behind).
    let up_work = fake_remote(&remotes, "kubeflow", "pipelines");
    fake_remote(&remotes, "kubeflow", "other");
    let fork_bare = remotes.join("acme").join("pipelines.git");
    std::fs::create_dir_all(fork_bare.parent().unwrap()).unwrap();
    git(h._tmp.path(), &["clone", "-q", "--bare", remotes.join("kubeflow/pipelines.git").to_str().unwrap(), fork_bare.to_str().unwrap()]);
    let commit = |work: &Path, file: &str| {
        std::fs::write(work.join(file), format!("{file}\n")).unwrap();
        git(work, &["add", "-A"]);
        git(work, &["commit", "-qm", file]);
        git(work, &["push", "-q", "origin", "main"]);
    };
    commit(&up_work, "UPSTREAM1.md");
    let up_bare = remotes.join("kubeflow/pipelines.git");
    let mut fake = FakeGitHub::new(&remotes, true);
    fake.parents.insert("acme/pipelines".into(), ("kubeflow".into(), "pipelines".into(), "main".into()));
    let fake = Arc::new(fake);
    h.handle.shared.repos.set_github(fake.clone());

    // Does the account have a fork? Yes for pipelines, no for other.
    let f = h.get("/v1/repos/fork?repo=kubeflow/pipelines").await;
    assert_eq!(f["fork"]["fullName"], "acme/pipelines", "{f}");
    assert_eq!(f["fork"]["cloned"], false);
    assert_eq!(h.get("/v1/repos/fork?repo=kubeflow/other").await["fork"], Value::Null);
    // viaFork without a fork: 404, and nothing is created.
    let (c, v) = status_of(&h, reqwest::Method::POST, "/v1/repos/clone", Some(json!({ "repo": "kubeflow/other", "viaFork": true }))).await;
    assert_eq!(c, 404, "{v}");
    assert!(v["error"].as_str().unwrap().contains("doesn't create forks"), "{v}");
    assert!(!remotes.join("acme/other.git").exists());

    // Clone the parent via the fork.
    let job = h.send(reqwest::Method::POST, "/v1/repos/clone", Some(json!({ "repo": "kubeflow/pipelines", "viaFork": true }))).await;
    assert_eq!(job["job"]["checkout"], "acme__pipelines");
    let done = wait_job(&h, job["job"]["id"].as_str().unwrap()).await;
    assert_eq!(done["state"], "done", "{done}");
    let co = h.handle.shared.cfg.data_dir.join("repos").join("acme__pipelines");
    assert!(co.join("UPSTREAM1.md").exists(), "base follows upstream, not the stale fork");
    assert_eq!(rev(&co, "main"), rev(&up_bare, "main"));
    let out = |args: &[&str]| {
        let o = hive_core::process_util::command("git").arg("-C").arg(&co).args(args).output().unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    assert_eq!(out(&["rev-parse", "--abbrev-ref", "main@{upstream}"]), "upstream/main");
    assert_eq!(out(&["remote", "get-url", "--push", "upstream"]), "DISABLED-push-to-fork-only");
    assert_eq!(out(&["config", "hive.chatBase"]), "refs/remotes/upstream/main");

    // Listing shows the fork layout; the parent's GitHub row points at it.
    let v = h.get("/v1/repos").await;
    let local = v["local"].as_array().unwrap().iter().find(|r| r["name"] == "acme__pipelines").unwrap().clone();
    assert_eq!(local["fork"]["upstream"], "kubeflow/pipelines", "{local}");
    assert_eq!(local["fork"]["baseBranch"], "main");
    assert_eq!(local["fork"]["upstreamPushDisabled"], true);
    assert_eq!((local["fork"]["behind"].clone(), local["fork"]["ahead"].clone()), (json!(0), json!(0)));
    assert_eq!(local["defaultBranch"], "main");
    let parent_row = v["github"]["repos"].as_array().unwrap().iter().find(|r| r["fullName"] == "kubeflow/pipelines").unwrap().clone();
    assert_eq!(parent_row["viaForkCheckout"], "acme__pipelines", "{parent_row}");
    assert_eq!(h.get("/v1/repos/fork?repo=kubeflow/pipelines").await["fork"]["cloned"], true);

    // A chat bound to it: its branch starts from upstream.
    let chat = h.send(reqwest::Method::POST, "/v1/chats", Some(json!({ "title": "Fix: the v2 compiler (again)!" }))).await;
    let chat_id = chat["chat"]["id"].as_str().unwrap().to_string();
    let r = h.send(reqwest::Method::PUT, &format!("/v1/chats/{chat_id}/repo"), Some(json!({ "repo": "acme__pipelines" }))).await;
    assert_eq!(r["repo"]["fork"]["upstream"], "kubeflow/pipelines", "{r}");
    let ctx = crate::repos::turn_context(&h.handle.shared.cfg.data_dir, Uuid::parse_str(&chat_id).unwrap());
    assert!(ctx.contains("a fork of `kubeflow/pipelines`") && ctx.contains("`upstream/main`"), "{ctx}");

    // Upstream moves; pull fast-forwards the base from upstream.
    commit(&up_work, "UPSTREAM2.md");
    let p = h.send(reqwest::Method::POST, &format!("/v1/chats/{chat_id}/repo/pull"), None).await;
    assert_eq!(p["updated"], true, "{p}");
    assert_eq!(rev(&co, "main"), rev(&up_bare, "main"));
    let wt = hive_runtime::git_worktree::chat_worktree(&co, &chat_id).unwrap();
    assert_eq!(rev(&wt, "HEAD"), rev(&up_bare, "main"), "the chat branch starts from upstream's tip");

    // PR: readable branch on the fork, a draft upstream.
    std::fs::write(wt.join("FIX.md"), "fix\n").unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "fix"]);
    let pr_path = format!("/v1/chats/{chat_id}/pr");
    let pr = h.send(reqwest::Method::POST, &pr_path, Some(json!({ "title": "Fix the compiler" }))).await;
    let want = format!("hive/fix-the-v2-compiler-again-{}", &chat_id.replace('-', "")[..8]);
    assert_eq!(pr["pushedBranch"], want.as_str(), "{pr}");
    assert_eq!((pr["repo"].clone(), pr["base"].clone(), pr["draft"].clone()), (json!("kubeflow/pipelines"), json!("main"), json!(true)));
    let rec = fake.prs.lock().unwrap().last().cloned().unwrap();
    assert_eq!((rec.repo.as_str(), rec.head.as_str(), rec.base.as_str(), rec.draft), ("kubeflow/pipelines", format!("acme:{want}").as_str(), "main", true));
    assert_eq!(rev(&fork_bare, &format!("refs/heads/{want}")), rev(&wt, "HEAD"));
    assert!(!git_ok(&up_bare, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{want}")]), "never pushed upstream");
    assert_eq!(h.get(&format!("/v1/chats/{chat_id}/repo")).await["prBranch"], want.as_str());

    // A later push updates the same branch (from the recorded mapping).
    let map = crate::repos::PrBranches::load(&h.handle.shared.cfg.data_dir);
    assert_eq!(map.get("acme__pipelines", Uuid::parse_str(&chat_id).unwrap()), Some(want.as_str()));
    std::fs::write(wt.join("FIX2.md"), "more\n").unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "more"]);
    let pr = h.send(reqwest::Method::POST, &pr_path, Some(json!({ "title": "Fix the compiler", "draft": false }))).await;
    assert_eq!((pr["pushedBranch"].as_str(), pr["existing"].clone()), (Some(want.as_str()), json!(true)), "{pr}");
    assert_eq!(rev(&fork_bare, &format!("refs/heads/{want}")), rev(&wt, "HEAD"));

    // Never forced: a branch that moved on the fork is refused.
    let other = h._tmp.path().join("other-clone");
    git(h._tmp.path(), &["clone", "-q", fork_bare.to_str().unwrap(), other.to_str().unwrap()]);
    git(&other, &["checkout", "-q", &want]);
    std::fs::write(other.join("THEIRS.md"), "x\n").unwrap();
    git(&other, &["add", "-A"]);
    git(&other, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "theirs"]);
    git(&other, &["push", "-q", "origin", &want]);
    std::fs::write(wt.join("FIX3.md"), "x\n").unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "mine"]);
    let (c, v) = status_of(&h, reqwest::Method::POST, &pr_path, Some(json!({ "title": "x" }))).await;
    assert_eq!(c, 409, "{v}");
    assert_eq!(rev(&fork_bare, &format!("refs/heads/{want}")), rev(&other, "HEAD"), "the fork's branch was not overwritten");

    // An origin whose push URL resolves to upstream is refused outright.
    git(&co, &["remote", "set-url", "--push", "origin", up_bare.to_str().unwrap()]);
    let (c, v) = status_of(&h, reqwest::Method::POST, &pr_path, Some(json!({ "title": "x" }))).await;
    assert_eq!(c, 409, "{v}");
    assert!(v["error"].as_str().unwrap().contains("refusing to push"), "{v}");
    let heads = hive_core::process_util::command("git").arg("-C").arg(&up_bare).args(["for-each-ref", "refs/heads"]).output().unwrap();
    assert!(!String::from_utf8_lossy(&heads.stdout).contains("hive/"), "upstream got no hive branches");
}

/// A plain fetch of an existing fork checkout without `upstream` adds it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fetching_a_fork_checkout_adds_upstream() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "ok"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let remotes = h._tmp.path().join("remotes");
    std::fs::create_dir_all(&remotes).unwrap();
    fake_remote(&remotes, "kubeflow", "pipelines");
    let fork_bare = remotes.join("acme").join("pipelines.git");
    std::fs::create_dir_all(fork_bare.parent().unwrap()).unwrap();
    git(h._tmp.path(), &["clone", "-q", "--bare", remotes.join("kubeflow/pipelines.git").to_str().unwrap(), fork_bare.to_str().unwrap()]);
    // Plain clone first (GitHub signed out: no fork info).
    h.handle.shared.repos.set_github(Arc::new(FakeGitHub::new(&remotes, false)));
    let job = h.send(reqwest::Method::POST, "/v1/repos/clone", Some(json!({ "repo": "acme/pipelines" }))).await;
    assert_eq!(wait_job(&h, job["job"]["id"].as_str().unwrap()).await["state"], "done");
    let co = h.handle.shared.cfg.data_dir.join("repos").join("acme__pipelines");
    assert!(!git_ok(&co, &["remote", "get-url", "upstream"]));
    let local = h.get("/v1/repos").await["local"][0].clone();
    assert!(local.get("fork").is_none(), "{local}");
    // Signed in: a fetch notices it's a fork and sets up upstream.
    let mut fake = FakeGitHub::new(&remotes, true);
    fake.parents.insert("acme/pipelines".into(), ("kubeflow".into(), "pipelines".into(), "main".into()));
    h.handle.shared.repos.set_github(Arc::new(fake));
    let job = h.send(reqwest::Method::POST, "/v1/repos/clone", Some(json!({ "repo": "acme/pipelines" }))).await;
    assert_eq!(job["job"]["action"], "fetch");
    assert_eq!(wait_job(&h, job["job"]["id"].as_str().unwrap()).await["state"], "done");
    let local = h.get("/v1/repos").await["local"][0].clone();
    assert_eq!(local["fork"]["upstream"], "kubeflow/pipelines", "{local}");
    assert_eq!(local["fork"]["upstreamPushDisabled"], true);
}

/// Repo writes follow the box's standing: a write-blocked box (revoked or
/// pending) gets 403, reads still work.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repo_writes_need_a_box_that_can_write() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let chat = h.send(reqwest::Method::POST, "/v1/chats", Some(json!({ "title": "t" }))).await;
    let chat_id = chat["chat"]["id"].as_str().unwrap().to_string();
    h.handle.shared.lock().set_write_block(Some("this box was revoked".into()));
    for (m, path, body) in [
        (reqwest::Method::POST, "/v1/repos/clone".to_string(), Some(json!({ "repo": "a/b" }))),
        (reqwest::Method::PUT, format!("/v1/chats/{chat_id}/repo"), Some(json!({ "repo": null }))),
        (reqwest::Method::POST, format!("/v1/chats/{chat_id}/repo/pull"), None),
        (reqwest::Method::POST, format!("/v1/chats/{chat_id}/pr"), Some(json!({ "title": "x" }))),
    ] {
        let (c, v) = status_of(&h, m, &path, body).await;
        assert_eq!(c, 403, "{path}: {v}");
    }
    assert_eq!(h.get(&format!("/v1/chats/{chat_id}/repo")).await["repo"]["source"], "workspace");
    let r = h.client.get(format!("{}/v1/repos", h.base)).send().await.unwrap();
    assert_eq!(r.status(), 401, "bearer required");
}

/// `GET /v1/proposals/:id/file`: the whole file before or after a proposal,
/// built from the chat's checkout without touching it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proposal_file_serves_both_sides_from_the_bound_checkout() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let readme = "# Hive\n\nIntro.\n\n- one\n- two\n";
    let h = harness(&endpoint, "m", &[("README.md", readme), ("gone.md", "bye\n")]).await;
    let chat = h.send(reqwest::Method::POST, "/v1/chats", None).await;
    let chat_id = Uuid::parse_str(chat["chat"]["id"].as_str().unwrap()).unwrap();
    let ws = h.handle.shared.ws;
    let diff = "diff --git a/README.md b/README.md\nindex 0000000..1111111 100644\n--- a/README.md\n+++ b/README.md\n\
@@ -3,4 +3,5 @@\n Intro.\n \n - one\n-- two\n+- two, revised\n+- three\n\
diff --git a/new.md b/new.md\nnew file mode 100644\n--- /dev/null\n+++ b/new.md\n@@ -0,0 +1,2 @@\n+# New\n+hi\n\
diff --git a/gone.md b/gone.md\ndeleted file mode 100644\n--- a/gone.md\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n";
    let stale = "diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1,2 +1,2 @@\n-# Not this\n+# Other\n \n";
    let mk = |title: &str, d: &str| hive_core::ActionProposal::file_diff(title, "", d.into(), vec!["README.md".into()]);
    let (good, bad) = (mk("docs", diff), mk("stale", stale));
    let (pid, sid) = (good.id, bad.id);
    {
        let mut svc = h.handle.shared.lock();
        svc.upsert_proposal(chat_id, ws, good).unwrap();
        svc.upsert_proposal(chat_id, ws, bad).unwrap();
    }
    h.handle.shared.after_write();
    let file = |id: Uuid, path: &str, side: &str| format!("/v1/proposals/{id}/file?path={path}&side={side}");
    let new_readme = "# Hive\n\nIntro.\n\n- one\n- two, revised\n- three\n";

    // Modified: both sides, the checkout untouched.
    let (code, v) = status_of(&h, reqwest::Method::GET, &file(pid, "README.md", "new"), None).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["content"], new_readme);
    assert_eq!(v["applied"], json!(false));
    assert_eq!(v["side"], "new");
    let (code, v) = status_of(&h, reqwest::Method::GET, &file(pid, "README.md", "old"), None).await;
    assert_eq!((code, v["content"].as_str()), (200, Some(readme)), "{v}");
    let repo = h._tmp.path().join("repo");
    assert_eq!(std::fs::read_to_string(repo.join("README.md")).unwrap(), readme);
    assert!(!repo.join("new.md").exists());
    // `side` defaults to new.
    let (code, v) = status_of(&h, reqwest::Method::GET, &format!("/v1/proposals/{pid}/file?path=README.md"), None).await;
    assert_eq!((code, v["content"].as_str()), (200, Some(new_readme)));

    // New file: a new side only.
    let (code, v) = status_of(&h, reqwest::Method::GET, &file(pid, "new.md", "new"), None).await;
    assert_eq!((code, v["content"].as_str()), (200, Some("# New\nhi\n")), "{v}");
    assert_eq!(status_of(&h, reqwest::Method::GET, &file(pid, "new.md", "old"), None).await.0, 404);
    // Deleted file: an old side only.
    let (code, v) = status_of(&h, reqwest::Method::GET, &file(pid, "gone.md", "old"), None).await;
    assert_eq!((code, v["content"].as_str()), (200, Some("bye\n")), "{v}");
    assert_eq!(status_of(&h, reqwest::Method::GET, &file(pid, "gone.md", "new"), None).await.0, 404);

    // Bad requests: not in the diff, traversal, absolute, .git, bad side.
    for bad in [
        file(pid, "other.md", "new"),
        file(pid, "../README.md", "new"),
        file(pid, "docs/../../README.md", "old"),
        file(pid, "/etc/passwd", "new"),
        file(pid, ".git/config", "new"),
        file(pid, "README.md", "both"),
        format!("/v1/proposals/{pid}/file"),
    ] {
        let (code, v) = status_of(&h, reqwest::Method::GET, &bad, None).await;
        assert_eq!(code, 400, "{bad}: {v}");
    }
    // A diff that fits neither base: 409.
    let (code, v) = status_of(&h, reqwest::Method::GET, &file(sid, "README.md", "new"), None).await;
    assert_eq!(code, 409, "{v}");
    assert_eq!(status_of(&h, reqwest::Method::GET, &file(Uuid::new_v4(), "README.md", "new"), None).await.0, 404);
    // Unauthenticated: 401 like every other read.
    let anon = h.client.get(format!("{}{}", h.base, file(pid, "README.md", "new"))).send().await.unwrap();
    assert_eq!(anon.status().as_u16(), 401);

    // Applied on the chat branch: the base carries the change, the old side
    // comes from reversing it.
    git(&repo, &["branch", &crate::repos::chat_branch(chat_id)]);
    let wt = h._tmp.path().join("wt");
    git(&repo, &["worktree", "add", "-q", wt.to_str().unwrap(), &crate::repos::chat_branch(chat_id)]);
    std::fs::write(wt.join("README.md"), new_readme).unwrap();
    git(&wt, &["commit", "-qam", "apply"]);
    let (code, v) = status_of(&h, reqwest::Method::GET, &file(pid, "README.md", "old"), None).await;
    assert_eq!((code, v["content"].as_str()), (200, Some(readme)), "{v}");
    assert_eq!(v["applied"], json!(true));
    assert_eq!(v["base"], crate::repos::chat_branch(chat_id));

    // No checkout bound for the chat on this host: 404 with the reason.
    RepoBindings::default().save(&h._tmp.path().join("serve")).unwrap();
    let (code, v) = status_of(&h, reqwest::Method::GET, &file(pid, "README.md", "new"), None).await;
    assert_eq!(code, 404, "{v}");
    assert!(v.to_string().contains("no repository"), "{v}");
    h.handle.shutdown();
}

/// The phone's read-only view of the workspace guards: dearth mode, locked
/// chats and the workspace coordinator lease this box holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn guards_endpoint_shows_dearth_lockdowns_and_the_coordinator() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let g = h.get("/v1/guards").await;
    assert!(g["dearth"].is_null(), "{g}");
    assert_eq!(g["lockedChats"], json!([]));

    let chat = h.send(reqwest::Method::POST, "/v1/chats", None).await;
    let chat_id = Uuid::parse_str(chat["chat"]["id"].as_str().unwrap()).unwrap();
    let ws = h.handle.shared.ws;
    let cause = Uuid::new_v4();
    {
        let mut svc = h.handle.shared.lock();
        svc.self_medicate(ws, chat_id, Some(cause), &[(hive_core::guard::LockdownSignal::SecretInDiff, "aws_access_key".into())]).unwrap();
        let d = hive_core::guard::DearthMode {
            enabled: true,
            reason: "leaked invite".into(),
            brood_paths: vec![".github/**".into()],
            allowed_mcp_servers: vec![],
            admitted_devices: vec![],
            set_by: String::new(),
            set_at: Timestamp::now(),
        };
        // The box may not be an Admin; switching dearth mode is then refused,
        // which is itself the rule under test. Seed it as the desktop would.
        let _ = svc.set_dearth_mode(ws, d);
    }
    let g = h.get("/v1/guards").await;
    assert_eq!(g["lockedChats"][0]["chatId"], json!(chat_id), "{g}");
    assert_eq!(g["lockedChats"][0]["signals"][0]["signal"], "secretInDiff");
    assert_eq!(g["lockedChats"][0]["signals"][0]["messageId"], json!(cause), "names the reply that tripped it");
    assert!(!g.to_string().contains("AKIA"), "only kinds, never values");

    // The box claims the workspace coordinator lease on its first presence tick.
    let mut coordinator = Value::Null;
    for _ in 0..50 {
        coordinator = h.get("/v1/guards").await["coordinator"].clone();
        if !coordinator.is_null() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(coordinator["isThis"], true, "{coordinator}");
    assert_eq!(coordinator["epoch"], 1);
    h.handle.shutdown();
}

// ── Phone 2.0: proposal sets, lockdown clear, Flow, reviewer recruits ──────

/// Demote this box's identity to Viewer on the desktop and wait until the box
/// has synced the demotion into `chat_id`'s roster.
async fn demote_box_to_viewer(h: &Harness, chat_id: Uuid) {
    let me = h.handle.shared.my_actor_id();
    let ws = h.handle.shared.ws;
    let mut demoted = false;
    for _ in 0..50 {
        sync_once(&h.desktop).await.unwrap();
        let mut svc = open_service(&h.desktop).unwrap();
        let cfg = workspace_config_session_id(ws);
        if svc.load(cfg).unwrap().is_some_and(|s| s.members.iter().any(|m| m.id == me)) {
            svc.set_member_role(cfg, ws, me.clone(), hive_core::WorkspaceRole::Viewer).unwrap();
            drop(svc);
            sync_once(&h.desktop).await.unwrap();
            demoted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(demoted, "the box never joined the desktop's roster");
    for _ in 0..50 {
        let viewer_here = h.handle.shared.lock().load(chat_id).unwrap().unwrap().role_of(&me) == hive_core::WorkspaceRole::Viewer;
        let (_, config) = api::hosts_and_agents(&h.handle.shared).ok().unwrap();
        if viewer_here && config.role_of(&me) == hive_core::WorkspaceRole::Viewer {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("the demotion never synced to the box");
}

/// A chat with a two-alternative proposal set; alternative A has one posted
/// scout assessment. Returns (chat, set, A, B).
async fn chat_with_set(h: &Harness) -> (Uuid, Uuid, Uuid, Uuid) {
    let chat = h.send(reqwest::Method::POST, "/v1/chats", None).await;
    let chat_id = Uuid::parse_str(chat["chat"]["id"].as_str().unwrap()).unwrap();
    let ws = h.handle.shared.ws;
    let mut a = hive_core::ActionProposal::new("Cache in memory", hive_core::ProposalKind::Decision, "");
    a.producing_agent_id = Some(h.agent_id);
    let b = hive_core::ActionProposal::new("Cache in Redis", hive_core::ProposalKind::Decision, "planner");
    let (aid, bid) = (a.id, b.id);
    let set_id = {
        let mut svc = h.handle.shared.lock();
        let set = hive_core::ProposalSet::new("Where should the cache live?", "planner", vec![]);
        let set = svc.open_proposal_set(chat_id, ws, set, vec![a, b]).unwrap();
        let assessment = hive_core::proposal_sets::ScoutAssessment {
            id: Uuid::new_v4(),
            proposal_id: aid,
            scout_agent_id: None,
            scout_name: "reviewer".into(),
            scout_runtime: "rt-2".into(),
            score: 80,
            rationale: "Simple and fast enough".into(),
            round: 0,
            attempt: 0,
            created_at: hive_core::Timestamp::now(),
        };
        svc.post_scout_assessment(chat_id, ws, set.id, assessment).unwrap().expect("assessment folded");
        set.id
    };
    h.handle.shared.after_write();
    (chat_id, set_id, aid, bid)
}

/// The phone reads proposal sets (standings, posted assessments, quorum and
/// pre-flight state) and votes on an alternative through the ordinary vote
/// route; nothing on the set routes writes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proposal_sets_are_listed_with_standing_and_votable_alternatives() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let (chat_id, set_id, aid, bid) = chat_with_set(&h).await;

    let sets = h.get("/v1/proposal-sets").await;
    let set = &sets["sets"][0];
    assert_eq!(set["id"], json!(set_id), "{sets}");
    assert_eq!(set["chatId"], json!(chat_id));
    assert_eq!(set["question"], "Where should the cache live?");
    assert_eq!(set["phase"], "scouting");
    assert_eq!(set["open"], true);
    assert_eq!(set["dial"], "balanced");
    assert_eq!(set["decideOn"], "desktop");
    let alts = set["alternatives"].as_array().unwrap();
    assert_eq!(alts.len(), 2);
    let (a, b) = (&alts[0], &alts[1]);
    assert_eq!((a["label"].as_str(), a["proposalId"].clone()), (Some("A"), json!(aid)));
    assert_eq!((b["label"].as_str(), b["proposalId"].clone()), (Some("B"), json!(bid)));
    assert_eq!(a["assessments"][0]["scoutName"], "reviewer");
    assert_eq!(a["assessments"][0]["score"], 80);
    assert_eq!(a["assessments"][0]["currentSupport"], 80);
    assert_eq!(a["assessments"][0]["isSelf"], false);
    assert_eq!(a["support"], 80);
    assert_eq!(a["scouts"], 1);
    assert_eq!(b["assessments"], json!([]), "B has no posted assessment");
    assert_eq!(b["support"], 0);
    assert_eq!(a["frozen"], false);
    assert_eq!(a["isFrontRunner"], false);
    assert!(a.get("preflight").is_none() || a["preflight"].is_null());
    assert_eq!(a["canVote"], true);
    assert_eq!(a["myVote"], Value::Null);

    // The chat's and the single-set views agree.
    let per_chat = h.get(&format!("/v1/chats/{chat_id}/proposal-sets")).await;
    assert_eq!(per_chat["sets"][0]["id"], json!(set_id));
    let one = h.get(&format!("/v1/proposal-sets/{set_id}")).await;
    assert_eq!(one["set"]["alternatives"][1]["title"], "Cache in Redis");
    let (code, _) = status_of(&h, reqwest::Method::GET, &format!("/v1/proposal-sets/{}", Uuid::new_v4()), None).await;
    assert_eq!(code, 404);
    let (code, _) = status_of(&h, reqwest::Method::GET, "/v1/proposal-sets?status=bogus", None).await;
    assert_eq!(code, 400);
    // Read-only: no route writes a set.
    let (code, _) = status_of(&h, reqwest::Method::POST, &format!("/v1/proposal-sets/{set_id}"), Some(json!({}))).await;
    assert_eq!(code, 405);

    // Voting on an alternative is the ordinary proposal vote.
    let voted = h.send(reqwest::Method::POST, &format!("/v1/proposals/{bid}/approve"), None).await;
    assert_eq!(voted["proposal"]["setId"], json!(set_id));
    let one = h.get(&format!("/v1/proposal-sets/{set_id}")).await;
    assert_eq!(one["set"]["alternatives"][1]["myVote"], "approve", "{one}");
    assert_eq!(one["set"]["alternatives"][1]["qualifyingApprovals"], 1);
    // An unauthenticated caller sees nothing.
    let r = h.client.get(format!("{}/v1/proposal-sets", h.base)).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 401);
    h.handle.shutdown();
}

/// Clearing a lockdown from the phone writes the same signed
/// `ChatLockdownCleared` the desktop does, only for a human Contributor+; a
/// Viewer is refused and the lockdown stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lockdown_clear_needs_a_human_contributor() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let ws = h.handle.shared.ws;
    let lock = |chat: Uuid| {
        h.handle
            .shared
            .lock()
            .self_medicate(ws, chat, None, &[(hive_core::guard::LockdownSignal::InjectionDetected, "ignore-previous".into())])
            .unwrap();
    };
    let chat = h.send(reqwest::Method::POST, "/v1/chats", Some(json!({ "title": "Triage" }))).await;
    let chat_id = Uuid::parse_str(chat["chat"]["id"].as_str().unwrap()).unwrap();
    lock(chat_id);

    let g = h.get("/v1/guards").await;
    assert_eq!(g["lockedChats"][0]["chatId"], json!(chat_id), "{g}");
    assert_eq!(g["lockedChats"][0]["chatTitle"], "Triage");
    assert_eq!(g["lockedChats"][0]["signals"][0]["signal"], "injectionDetected");
    assert!(g["lockedChats"][0]["signals"][0]["raisedAt"].is_string());
    assert_eq!(g["canClearLockdown"], true, "{g}");

    let res = h.send(reqwest::Method::POST, &format!("/v1/chats/{chat_id}/lockdown/clear"), None).await;
    assert_eq!(res["cleared"], true);
    let g = h.get("/v1/guards").await;
    assert_eq!(g["lockedChats"], json!([]), "{g}");
    let s = h.handle.shared.lock().load(chat_id).unwrap().unwrap();
    assert!(!s.guards.lockdown_active());
    assert!(s.messages.iter().any(|m| m.body.contains("cleared this chat's lockdown")), "the desktop's system note");
    drop(s);
    // Not locked any more: nothing to clear.
    let (code, _) = status_of(&h, reqwest::Method::POST, &format!("/v1/chats/{chat_id}/lockdown/clear"), None).await;
    assert_eq!(code, 409);

    // As a Viewer: refused, and the lockdown stays.
    lock(chat_id);
    demote_box_to_viewer(&h, chat_id).await;
    let g = h.get("/v1/guards").await;
    assert_eq!(g["canClearLockdown"], false, "{g}");
    assert_eq!(g["myRole"], "viewer");
    let (code, v) = status_of(&h, reqwest::Method::POST, &format!("/v1/chats/{chat_id}/lockdown/clear"), None).await;
    assert_eq!(code, 403, "{v}");
    let g = h.get("/v1/guards").await;
    assert_eq!(g["lockedChats"][0]["chatId"], json!(chat_id), "a refused clear left the lockdown in place: {g}");
    h.handle.shutdown();
}

/// `GET /v1/flow` and `GET /v1/chats/:id/handoffs` read the folded flow
/// records: the board (an unreviewed proposal), the queue, marks, the routing
/// shadow summary and hand-off cards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_view_shows_board_queue_marks_routing_and_handoffs() {
    use hive_core::flow::{self as hflow, FlowRecord};
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let ws = h.handle.shared.ws;
    let chat = h.send(reqwest::Method::POST, "/v1/chats", Some(json!({ "title": "Auth" }))).await;
    let chat_id = Uuid::parse_str(chat["chat"]["id"].as_str().unwrap()).unwrap();
    let now = hflow::now_ms();
    let reply = Uuid::new_v4();
    {
        let mut svc = h.handle.shared.lock();
        let mut p = hive_core::ActionProposal::file_diff("Add login", "", "diff --git a/x b/x\n".into(), vec!["src/login.rs".into()]);
        p.author_actor_id = "someone-else".into();
        svc.upsert_proposal(chat_id, ws, p).unwrap();
        svc.record_flow(chat_id, ws, FlowRecord::Mark(hflow::WorkspaceMark {
            id: Uuid::new_v4(),
            kind: hflow::MarkKind::UnderConstruction,
            path: "src/auth.rs".into(),
            lines: Some((10, 20)),
            task_id: String::new(),
            note: "refactoring".into(),
            agent: hflow::agent_key(Some(h.agent_id)),
            by: "coder".into(),
            session_id: Some(chat_id),
            at_ms: now,
            expires_ms: Some(now + 3_600_000),
        }))
        .unwrap();
        let trigger = Uuid::new_v4();
        svc.record_flow(chat_id, ws, FlowRecord::RouteDecision(hflow::RouteDecision {
            trigger_id: trigger,
            task_class: "feature".into(),
            chosen: hflow::PRIMARY.into(),
            preferred: hflow::agent_key(Some(h.agent_id)),
            explore: false,
            mode: hflow::RouteMode::Shadow,
            shadow_choice: hflow::agent_key(Some(h.agent_id)),
            reason: hflow::route::RouteReason::Sampled,
            epsilon: 0.1,
            scores: Default::default(),
            excluded: vec![],
            at_ms: now,
        }))
        .unwrap();
        svc.record_flow(chat_id, ws, FlowRecord::Handoff(hflow::HandoffCard {
            id: Uuid::new_v4(),
            from: hflow::agent_key(Some(h.agent_id)),
            to: hflow::PRIMARY.into(),
            from_name: "coder".into(),
            to_name: String::new(),
            message_id: Some(reply),
            task: "Review the login flow".into(),
            task_class: "review".into(),
            branch: "ligus/chat/x".into(),
            files: vec!["src/login.rs".into()],
            quality: hflow::HandoffQuality { tests_passed: Some(true), ..Default::default() },
            via: "mention".into(),
            parcel: Some(hflow::ContextParcel { decisions: vec!["kept the API".into()], ..Default::default() }),
            at_ms: now,
        }))
        .unwrap();
    }
    h.handle.shared.after_write();

    let f = h.get("/v1/flow").await;
    let board = f["board"].as_array().unwrap();
    let item = board.iter().find(|i| i["kind"] == "unreviewedProposal").unwrap_or_else(|| panic!("{f}"));
    assert_eq!(item["chatId"], json!(chat_id));
    assert_eq!(item["chatTitle"], "Auth");
    assert!(item["proposalId"].is_string());
    assert_eq!(f["queue"]["gate"]["open"], 1, "{f}");
    assert_eq!(f["queue"]["queueLimit"], 5);
    assert!(f["queue"]["alert"].is_null(), "one waiting proposal isn't a bottleneck");
    let mark = &f["marks"][0];
    assert_eq!(mark["kind"], "underConstruction");
    assert_eq!(mark["path"], "src/auth.rs");
    assert_eq!(mark["lines"], json!([10, 20]));
    assert_eq!(mark["chatTitle"], "Auth");
    assert_eq!(f["routing"]["decisions"], 1);
    assert_eq!(f["routing"]["offPrimaryShare"], 1.0);
    assert_eq!(f["routing"]["topPicks"][0]["name"], "coder");

    let cards = h.get(&format!("/v1/chats/{chat_id}/handoffs")).await;
    let c = &cards["handoffs"][0];
    assert_eq!(c["fromName"], "coder");
    assert_eq!(c["toName"], "Ligus (primary)", "an empty name falls back to the agent's");
    assert_eq!(c["messageId"], json!(reply));
    assert!(c.get("replyMessageId").is_none(), "the receiver hasn't answered yet: {c}");
    assert_eq!(c["quality"]["testsPassed"], true);
    assert_eq!(c["parcel"]["decisions"][0], "kept the API");
    // Once the receiver answers, the card names that answer.
    let answer = Uuid::new_v4();
    {
        let mut svc = h.handle.shared.lock();
        svc.record_flow(chat_id, ws, FlowRecord::TurnTagged(hflow::TurnTag {
            message_id: answer,
            trigger_id: Some(reply),
            agent: hflow::PRIMARY.into(),
            runtime_id: String::new(),
            host_id: String::new(),
            task_class: "review".into(),
            role: hflow::roles::CONVERSATION.into(),
            arm: String::new(),
            upstream_arm: String::new(),
            error: false,
            error_kind: String::new(),
            tests_passed: None,
            tool_calls: 0,
            duration_ms: 0,
            cost_usd: 0.0,
            proposal_ids: vec![],
            handoff_parcel: false,
            at_ms: now + 1,
        }))
        .unwrap();
    }
    h.handle.shared.after_write();
    let cards = h.get(&format!("/v1/chats/{chat_id}/handoffs")).await;
    assert_eq!(cards["handoffs"][0]["replyMessageId"], json!(answer), "{cards}");
    let (code, _) = status_of(&h, reqwest::Method::GET, &format!("/v1/chats/{}/handoffs", Uuid::new_v4()), None).await;
    assert_eq!(code, 404);
    h.handle.shutdown();
}

/// A reviewer recruit naming this box's user pushes `review.recruit` (here
/// through ntfy), pointing at the proposal waiting longest for them; one that
/// names someone else doesn't.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reviewer_recruit_naming_you_pushes() {
    use hive_core::flow::{self as hflow, FlowRecord};
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let ws = h.handle.shared.ws;
    let me = h.handle.shared.my_actor_id();
    let chat = h.send(reqwest::Method::POST, "/v1/chats", Some(json!({ "title": "Queue" }))).await;
    let chat_id = Uuid::parse_str(chat["chat"]["id"].as_str().unwrap()).unwrap();
    let recruit = |members: Vec<String>| {
        FlowRecord::Recruit(hflow::RecruitNotice {
            id: Uuid::new_v4(),
            action: "approvers".into(),
            queue_len: 6,
            oldest_wait_ms: 3 * 3_600_000,
            member_ids: members,
            to_host: String::new(),
            from_host: String::new(),
            reference: String::new(),
            at_ms: hflow::now_ms(),
        })
    };
    {
        let mut svc = h.handle.shared.lock();
        let mut p = hive_core::ActionProposal::new("Pick a queue", hive_core::ProposalKind::Decision, "someone-else");
        p.author_actor_id = "someone-else".into();
        svc.upsert_proposal(chat_id, ws, p).unwrap();
    }
    h.handle.shared.after_write();
    let before = h.ntfy.lock().unwrap().len();
    {
        let mut svc = h.handle.shared.lock();
        svc.record_flow(chat_id, ws, recruit(vec!["not-me".into()])).unwrap();
    }
    h.handle.shared.after_write();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let titles = |pushes: &Pushes| -> Vec<String> {
        pushes.lock().unwrap().iter().filter_map(|(hd, _)| hd.get("title").and_then(|t| t.to_str().ok()).map(str::to_string)).collect()
    };
    assert!(!titles(&h.ntfy).iter().any(|t| t.contains("review queue")), "a recruit for someone else pushed");
    {
        let mut svc = h.handle.shared.lock();
        svc.record_flow(chat_id, ws, recruit(vec![me.clone()])).unwrap();
    }
    h.handle.shared.after_write();
    let mut ok = false;
    for _ in 0..30 {
        if h.ntfy.lock().unwrap().len() > before
            && h.ntfy.lock().unwrap().iter().any(|(_, body)| body.contains("6 proposals waiting"))
        {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ok, "no recruit push: {:?}", h.ntfy.lock().unwrap());
    h.handle.shutdown();
}

/// Chat details carry each workflow run's coordinator (the run lease's holder).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workflow_runs_show_their_coordinator() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let ws = h.handle.shared.ws;
    let chat = h.send(reqwest::Method::POST, "/v1/chats", None).await;
    let chat_id = Uuid::parse_str(chat["chat"]["id"].as_str().unwrap()).unwrap();
    {
        let mut svc = h.handle.shared.lock();
        let def = hive_core::workflow::preset_review_gate();
        let me = svc.author().id.clone();
        let run = hive_core::workflow::new_run(&def, "go", &me);
        let run_id = run.id;
        svc.upsert_workflow_run(chat_id, ws, run).unwrap();
        svc.claim_lease(
            chat_id,
            ws,
            hive_core::lease::LeaseScope::WorkflowRun,
            &run_id.to_string(),
            1,
            90_000,
            hive_core::lease::LeaseClaimReason::Initial,
        )
        .unwrap();
    }
    h.handle.shared.after_write();
    let d = h.get(&format!("/v1/chats/{chat_id}/details")).await;
    let c = &d["workflowRuns"][0]["coordinator"];
    assert_eq!(c["isThis"], true, "{d}");
    assert_eq!(c["epoch"], 1);
    assert_eq!(c["released"], false);
    assert_eq!(c["online"], true);
    assert!(c["holderLabel"].as_str().unwrap().contains("this host"));
    h.handle.shutdown();
}

//! Live check of a desktop Claude Code turn with the Ligus MCP server, the
//! way the desktop runs one: the host's OS sandbox (Seatbelt on macOS) with
//! the workspace's default envelope, the per-turn socket and credential, the
//! legacy `claude -p` adapter, and **no bound code folder** (working dir
//! `None`) or, with `LIGUS_LIVE_BOUND=1`, a bound temp folder.
//!
//! Ignored unless `LIGUS_LIVE_CLAUDE=1` (needs `claude` installed and signed
//! in; spends one tiny prompt). Uses a throwaway data dir and chat store:
//!
//! ```text
//! LIGUS_LIVE_CLAUDE=1 cargo test -p ligus-runtime --test ligus_mcp_claude_live -- --nocapture
//! ```
//!
//! It asks the model to call `ligus_list_chats` (asserting the call is
//! pre-approved and returns the throwaway chat) and to list its tools
//! (asserting none of the out-of-turn tools: cloud routines, cron, monitors,
//! push, cross-session messaging, workflows), and prints every endpoint the
//! turn's egress proxy refused. `LIGUS_LIVE_USER_CONFIG=1` runs it with the
//! workspace's opt-in into the user's own Claude Code setup.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ligus_core::crypto::SigningKeypair;
use ligus_core::sandbox::SandboxEnvelope;
use ligus_core::{ActorIdentity, ActorKind, ModelProviderKind};
use ligus_runtime::author_gate::GatePlan;
use ligus_runtime::event_store::EventStore;
use ligus_runtime::executor::{self, HostKind, TurnPolicy};
use ligus_runtime::ligus_mcp::{Budgets, LigusHost, LigusMcp, TurnSpec};
use ligus_runtime::provider::dispatch::{self, ResolvedRuntime};
use ligus_runtime::provider::ChatTurn;
use ligus_runtime::ChatService;
use uuid::Uuid;

struct Host(Mutex<ChatService>);
impl LigusHost for Host {
    fn with_service(&self, f: &mut dyn FnMut(&mut ChatService)) {
        f(&mut self.0.lock().unwrap())
    }
}

#[tokio::test]
async fn claude_code_turn_gets_ligus_tools_in_seatbelt() {
    if std::env::var("LIGUS_LIVE_CLAUDE").ok().as_deref() != Some("1") {
        return;
    }
    if executor::skip_without_sandbox("claude_code_turn_gets_ligus_tools_in_seatbelt") {
        return;
    }
    let data = tempfile::tempdir().unwrap();
    executor::configure_host(data.path(), &[], &[]);
    std::env::set_var("LIGUS_MCP_BRIDGE", env!("CARGO_BIN_EXE_ligus-mcp-bridge"));
    let store = EventStore::open(data.path().join("ligus.db")).unwrap();
    let mut svc = ChatService::new(store, SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), ActorIdentity::new("u", "U", ActorKind::Human));
    let wid = Uuid::new_v4();
    let chat = svc.create_chat("Ligustica-7341", wid, "rt").unwrap();
    let reply = svc.begin_assistant_message(chat.id, wid, "ligus", "rt", None, None).unwrap();
    let mcp = LigusMcp::new(Arc::new(Host(Mutex::new(svc))));
    let turn = mcp.begin_turn(TurnSpec {
        workspace_id: wid,
        chat_id: chat.id,
        agent_id: None,
        author: "ligus".into(),
        message_id: reply,
        worktree: None,
        budgets: Budgets::default(),
        ttl: Duration::from_secs(600),
    });
    let socket = turn.listen().unwrap();
    let mut policy = TurnPolicy::new(HostKind::Desktop, SandboxEnvelope::workspace_default(), GatePlan::ReadOnly);
    policy.ligus_mcp_socket = Some(socket);
    policy.cli_user_config = std::env::var("LIGUS_LIVE_USER_CONFIG").ok().as_deref() == Some("1");
    let denials = policy.denials.clone();
    let rt = ResolvedRuntime {
        provider: ModelProviderKind::ClaudeCode,
        model: std::env::var("LIGUS_LIVE_MODEL").unwrap_or_default(),
        endpoint: "claude".into(),
        api_key: None,
        args: vec![],
        model_provider_id: None,
        model_base_url: None,
        context_window_tokens: None,
        keep_alive: None,
        think: None,
        tools: false,
        max_tool_rounds: None,
        allow_remote_file_tools: false,
        driver: ligus_core::Driver::parse(&std::env::var("LIGUS_LIVE_DRIVER").unwrap_or_default()),
        gateway: None,
    };
    let bound = tempfile::tempdir().unwrap();
    let working_dir = (std::env::var("LIGUS_LIVE_BOUND").ok().as_deref() == Some("1")).then(|| bound.path().to_string_lossy().into_owned());
    let turns = vec![ChatTurn {
        role: "user".into(),
        content: "First call the ligus_list_chats tool and write the exact title of a chat it returns on the first line (or FAILED and the error if the call failed). \
                  Then, on the second line, list the exact names of every tool you have, comma separated, and nothing else."
            .into(),
    }];
    let scope = turn.agent_scope();
    let extra_env: Vec<(String, String)> = std::env::var("LIGUS_LIVE_ENV").unwrap_or_default().split(',').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect();
    if std::env::var("LIGUS_LIVE_PAUSE").is_ok() {
        let a = scope.mcp.clone().unwrap();
        let f = a.write_config_file("debug-mcp.json", &Default::default()).unwrap();
        println!("PAUSE config={}", f.display());
        tokio::time::sleep(Duration::from_secs(90)).await;
    }
    let mut acts = Vec::new();
    let out = executor::with_turn(
        policy,
        ligus_runtime::agent_turn::with_agent_turn(
            scope,
            dispatch::stream(&rt, None, &turns, working_dir.as_deref(), &extra_env, 512, |_| {}, |a| acts.push(format!("{a:?}"))),
        ),
    )
    .await;
    let refused = denials.lock().unwrap().clone();
    println!("LIVE bound={} reply={out:?}", working_dir.is_some());
    println!("LIVE refused endpoints: {refused:?}");
    let reply = out.expect("the turn ran");
    assert!(reply.contains("ligus_"), "the Ligus tools reached the model: {reply}");
    // The call itself went through (pre-approved; in `-p` mode an
    // unapproved MCP tool is refused) and returned the chat store's data.
    assert!(reply.contains("Ligustica-7341"), "ligus_list_chats returned data: {reply}\n{acts:?}");
    // None of the tools that run work outside the turn.
    for t in ["RemoteTrigger", "CronCreate", "ScheduleWakeup", "Monitor", "PushNotification", "SendMessage", "Workflow"] {
        assert!(!reply.contains(t), "{t} offered to a Ligus turn: {reply}");
    }
}

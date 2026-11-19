//! `mcp_bridge_reachable_inside_seatbelt_and_bwrap`: an agent child confined
//! by the host's executor (Seatbelt on macOS, bubblewrap or the Landlock
//! tier on Linux) under a
//! **closed** egress envelope reaches the turn's Ligus MCP socket through
//! `ligus-mcp-bridge`, and nothing else. On macOS a control run without the
//! socket in the plan is refused by the profile.

use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hive_core::crypto::SigningKeypair;
use hive_core::sandbox::{EgressPolicy, SandboxEnvelope, TightenedBy};
use hive_core::{ActorIdentity, ActorKind, ModelProviderKind};
use hive_runtime::author_gate::GatePlan;
use hive_runtime::event_store::EventStore;
use hive_runtime::executor::{self, HostKind, Purpose, SpawnSpec, TurnPolicy};
use hive_runtime::ligus_mcp::{Budgets, LigusHost, LigusMcp, TurnSpec};
use hive_runtime::ChatService;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use uuid::Uuid;

const BRIDGE: &str = env!("CARGO_BIN_EXE_ligus-mcp-bridge");

struct Host(Mutex<ChatService>);
impl LigusHost for Host {
    fn with_service(&self, f: &mut dyn FnMut(&mut ChatService)) {
        f(&mut self.0.lock().unwrap())
    }
}

async fn run_bridge(socket_in_plan: bool) -> Result<Vec<Value>, String> {
    let dir = tempfile::tempdir().unwrap();
    let store = EventStore::open(dir.path().join("hive.db")).unwrap();
    let mut svc = ChatService::new(store, SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), ActorIdentity::new("u", "U", ActorKind::Human));
    let wid = Uuid::new_v4();
    let chat = svc.create_chat("A", wid, "rt").unwrap();
    let reply = svc.begin_assistant_message(chat.id, wid, "hive", "rt", None, None).unwrap();
    let mcp = LigusMcp::new(Arc::new(Host(Mutex::new(svc))));
    let turn = mcp.begin_turn(TurnSpec {
        workspace_id: wid,
        chat_id: chat.id,
        agent_id: None,
        author: "hive".into(),
        message_id: reply,
        worktree: None,
        budgets: Budgets::default(),
        ttl: Duration::from_secs(60),
    });
    let socket = turn.listen().unwrap();
    let envelope = SandboxEnvelope::workspace_default().tighten(TightenedBy::Lockdown, EgressPolicy::Closed, false);
    let mut policy = TurnPolicy::new(HostKind::Desktop, envelope, GatePlan::ReadOnly);
    if socket_in_plan {
        policy.ligus_mcp_socket = Some(socket.clone());
    }
    let mut env = vec![
        ("LIGUS_MCP_SOCKET".to_string(), socket.to_string_lossy().into_owned()),
        ("LIGUS_TURN_CRED".to_string(), turn.credential().to_string()),
    ];
    // Under bubblewrap the bridge goes through the helper's forwarder, under
    // the Landlock tier through the endpoint's loopback listener (C-10).
    env.extend(hive_runtime::ligus_mcp::bridge::sandbox_tcp_env_for(&socket));
    let work = tempfile::tempdir().unwrap();
    let out = executor::with_turn(policy, async {
        let spec = SpawnSpec::new(Purpose::Agent(ModelProviderKind::ClaudeCode), BRIDGE, Vec::<String>::new()).cwd(Some(work.path())).env(&env);
        let (mut cmd, guard) = executor::tokio_command(spec).map_err(|e| e.to_string())?;
        assert_ne!(guard.kind(), executor::ExecutorKind::Unconfined);
        if socket_in_plan {
            assert!(guard.plan().mcp.first().is_some_and(|m| m == executor::LIGUS_MCP_ID), "the Ligus server holds its slot under closed egress");
            assert!(!matches!(guard.plan().egress, executor::EgressPlan::Open), "only the model endpoints, through the proxy");
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        let mut stdin = child.stdin.take().unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let msgs = [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "ligus_read_board", "arguments": {}}}),
        ];
        for m in msgs {
            let _ = stdin.write_all(format!("{m}\n").as_bytes()).await;
        }
        let mut got = Vec::new();
        while got.len() < 3 {
            match tokio::time::timeout(Duration::from_secs(10), lines.next_line()).await {
                Ok(Ok(Some(l))) => got.push(serde_json::from_str::<Value>(&l).map_err(|e| e.to_string())?),
                _ => break,
            }
        }
        drop(stdin);
        let status = child.wait().await.map_err(|e| e.to_string())?;
        drop(guard);
        if got.is_empty() {
            return Err(format!("bridge exited {status} without a reply"));
        }
        Ok(got)
    })
    .await;
    drop(turn);
    out
}

#[tokio::test]
async fn mcp_bridge_reachable_inside_seatbelt_and_bwrap() {
    if executor::skip_without_sandbox("mcp_bridge_reachable_inside_seatbelt_and_bwrap") {
        return;
    }
    let got = run_bridge(true).await.expect("bridge reaches the socket");
    assert_eq!(got[0]["result"]["serverInfo"]["name"], "ligus");
    let tools = got[1]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), hive_runtime::ligus_mcp::tools::TOOLS.len(), "the full set, code-host reads included");
    assert_eq!(got[2]["result"]["isError"], false);

    // macOS: without the socket in the plan, the profile denies the connect.
    #[cfg(target_os = "macos")]
    assert!(run_bridge(false).await.is_err(), "Seatbelt only allows the plan's socket");
}

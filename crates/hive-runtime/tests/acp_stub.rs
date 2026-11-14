//! The ACP driver end to end against a stub ACP agent (`acp_stub_agent`),
//! in the flavors of Copilot CLI (`copilot --acp`), Antigravity (`agy acp`),
//! Codex (`codex-acp`, API-key auth) and the Claude adapter. No real CLI or
//! subscription is involved. Children run through the host's executor
//! (Seatbelt on macOS, bubblewrap on Linux), as in production.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hive_core::sandbox::SandboxEnvelope;
use hive_core::ModelProviderKind;
use hive_runtime::agent_turn::{AgentTurnScope, HumanAnswer, HumanAsker, McpAttach};
use hive_runtime::author_gate::GatePlan;
use hive_runtime::executor::{self, HostKind, TurnPolicy};
use hive_runtime::provider::acp::{self, AcpCommand, AcpFailure, AcpTurn};
use hive_runtime::provider::dispatch::StreamActivity;
use hive_runtime::ChatTurn;

const STUB: &str = env!("CARGO_BIN_EXE_acp_stub_agent");

/// A person who answers every ask with `option`, and records the questions.
struct Answers {
    option: Option<usize>,
    asked: Mutex<Vec<String>>,
}

impl HumanAsker for Answers {
    fn ask(&self, question: String, _options: Vec<String>, _timeout: Duration) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<HumanAnswer>> + Send + '_>> {
        self.asked.lock().unwrap().push(question);
        let opt = self.option;
        Box::pin(async move { opt.map(|i| HumanAnswer { answer: String::new(), option_index: Some(i) }) })
    }
}

struct Out {
    text: Result<String, String>,
    activity: Vec<String>,
    spend: f64,
}

async fn run(mode: &str, kind: ModelProviderKind, tier_args: &[&str], gate: GatePlan, asker: Option<Arc<Answers>>, wt: &std::path::Path) -> Out {
    let cmd = AcpCommand { program: STUB.to_string(), args: vec!["--mode".into(), mode.into()] };
    let tier: Vec<String> = tier_args.iter().map(|s| s.to_string()).collect();
    let turns = vec![ChatTurn { role: "user".into(), content: "hi".into() }];
    let wt_s = wt.to_string_lossy().into_owned();
    let spent = Arc::new(Mutex::new(0.0f64));
    let s2 = spent.clone();
    let mut scope = AgentTurnScope { spend: Some(Arc::new(move |d| *s2.lock().unwrap() += d)), ..Default::default() };
    scope.asker = asker.map(|a| a as Arc<dyn HumanAsker>);
    let dir = tempfile::tempdir().unwrap();
    scope.mcp = Some(McpAttach {
        name: "ligus".into(),
        command: "/bin/true".into(),
        args: vec!["mcp-bridge".into()],
        env: vec![],
        socket: dir.path().join("s"),
        config_dir: dir.path().to_path_buf(),
    });
    let mut policy = TurnPolicy::new(HostKind::Desktop, SandboxEnvelope::workspace_default(), gate);
    policy.worktree_is_isolated = true;
    let mut activity = Vec::new();
    let text = executor::with_turn(
        policy,
        hive_runtime::agent_turn::with_agent_turn(scope, async {
            acp::stream_reply(
                AcpTurn {
                    kind,
                    command: &cmd,
                    model: "",
                    working_dir: Some(&wt_s),
                    extra_env: &[],
                    system: Some("You are a test."),
                    turns: &turns,
                    extra_mcp: vec![],
                    api_key: None,
                    tier_args: &tier,
                },
                |_| {},
                |a| {
                    activity.push(match a {
                        StreamActivity::Tool { name, .. } => format!("tool:{name}"),
                        StreamActivity::ToolResult { is_error, .. } => format!("result:{is_error}"),
                        StreamActivity::Thinking { .. } => "thinking".into(),
                        StreamActivity::RateLimit { .. } => "rate".into(),
                    })
                },
            )
            .await
        }),
    )
    .await;
    let spend = *spent.lock().unwrap();
    Out { text: text.map_err(|e| e.to_string()), activity, spend }
}

fn sandbox_ok() -> bool {
    !executor::refuses_writes()
}

#[tokio::test]
async fn copilot_flavor_streams_and_auto_allows_edits_at_its_tier() {
    if !sandbox_ok() {
        eprintln!("skipping: no OS sandbox on this host");
        return;
    }
    let wt = tempfile::tempdir().unwrap();
    let out = run("copilot", ModelProviderKind::Copilot, &["--allow-tool=read", "--allow-tool=write", "--deny-tool=shell"], GatePlan::Full, None, wt.path()).await;
    let text = out.text.unwrap();
    assert!(text.contains("hello from copilot"), "{text}");
    assert!(text.contains("permission=allow-once"), "{text}");
    assert!(text.contains("wrote=yes"), "{text}");
    assert!(text.contains("mcp=ligus"), "the Ligus MCP server is offered first: {text}");
    assert_eq!(std::fs::read_to_string(wt.path().join("notes.txt")).unwrap(), "written over ACP\n");
    assert!(out.activity.contains(&"thinking".to_string()));
    assert!(out.activity.iter().any(|a| a == "tool:Edit notes.txt"));
    assert!((out.spend - 0.25).abs() < 1e-9, "usage_update cost reaches the spend budget");
}

#[tokio::test]
async fn agy_flavor_below_tier_escalates_to_a_human() {
    if !sandbox_ok() {
        return;
    }
    let wt = tempfile::tempdir().unwrap();
    // Read-only tier (no tier args): an edit request goes to a person, who allows it.
    let asker = Arc::new(Answers { option: Some(0), asked: Mutex::new(Vec::new()) });
    let out = run("agy", ModelProviderKind::Antigravity, &[], GatePlan::Full, Some(asker.clone()), wt.path()).await;
    let text = out.text.unwrap();
    assert!(text.contains("permission=allow-once"), "{text}");
    assert_eq!(asker.asked.lock().unwrap().len(), 1, "one escalation");
    // The person allowed the edit, but the read-only tier's sandbox and fs
    // policy still refuse the write itself.
    assert!(text.contains("wrote=refused"), "{text}");

    // Denied by the person.
    let asker = Arc::new(Answers { option: Some(1), asked: Mutex::new(Vec::new()) });
    let out = run("agy", ModelProviderKind::Antigravity, &[], GatePlan::Full, Some(asker), wt.path()).await;
    assert!(out.text.unwrap().contains("permission=reject-once"));

    // No one to ask: denied.
    let out = run("agy", ModelProviderKind::Antigravity, &[], GatePlan::Full, None, wt.path()).await;
    assert!(out.text.unwrap().contains("permission=reject-once"));
}

#[tokio::test]
async fn read_only_gate_denies_edits_without_asking() {
    if !sandbox_ok() {
        return;
    }
    let wt = tempfile::tempdir().unwrap();
    let asker = Arc::new(Answers { option: Some(0), asked: Mutex::new(Vec::new()) });
    let out = run("claude", ModelProviderKind::ClaudeCode, &["--permission-mode", "acceptEdits"], GatePlan::ReadOnly, Some(asker.clone()), wt.path()).await;
    let text = out.text.unwrap();
    assert!(text.contains("permission=reject-once"), "{text}");
    assert!(asker.asked.lock().unwrap().is_empty(), "the author gate isn't widened from a prompt");
    assert!(!wt.path().join("notes.txt").exists());
}

#[tokio::test]
async fn codex_flavor_authenticates_with_the_api_key_method() {
    if !sandbox_ok() {
        return;
    }
    let wt = tempfile::tempdir().unwrap();
    let out = run("codex", ModelProviderKind::Codex, &[], GatePlan::Full, None, wt.path()).await;
    let text = out.text.unwrap();
    assert!(text.contains("hello from codex"), "{text}");
    assert!(text.contains("wrote=yes"), "codex runs at its command tier: {text}");
}

#[tokio::test]
async fn an_agent_without_acp_is_an_init_failure() {
    if !sandbox_ok() {
        return;
    }
    let wt = tempfile::tempdir().unwrap();
    let cmd = AcpCommand { program: STUB.to_string(), args: vec!["--mode".into(), "broken".into()] };
    let wt_s = wt.path().to_string_lossy().into_owned();
    let mut policy = TurnPolicy::new(HostKind::Desktop, SandboxEnvelope::workspace_default(), GatePlan::Full);
    policy.worktree_is_isolated = true;
    let r = executor::with_turn(
        policy,
        acp::stream_reply(
            AcpTurn {
                kind: ModelProviderKind::Copilot,
                command: &cmd,
                model: "",
                working_dir: Some(&wt_s),
                extra_env: &[],
                system: None,
                turns: &[],
                extra_mcp: vec![],
                api_key: None,
                tier_args: &[],
            },
            |_| {},
            |_| {},
        ),
    )
    .await;
    assert!(matches!(r, Err(AcpFailure::Init(_))), "{:?}", r.err().map(|e| e.to_string()));
}

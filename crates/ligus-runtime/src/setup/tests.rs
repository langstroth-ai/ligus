use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use ligus_core::WorkspaceRole;
use zeroize::Zeroizing;

use super::fake::Fake;
use super::*;
use crate::checks::Status;

const CANARY: &str = "canary-SECRET-7f3a9c0e1d";

fn run_(flow: FlowId, ops: &mut Fake, inputs: &[(&str, &str)]) -> Outcome {
    run_s(flow, ops, inputs, &[])
}

fn run_s(flow: FlowId, ops: &mut Fake, inputs: &[(&str, &str)], secrets: &[(&str, &str)]) -> Outcome {
    run_o(flow, ops, inputs, secrets, &RunOpts::default())
}

fn run_o(flow: FlowId, ops: &mut Fake, inputs: &[(&str, &str)], secrets: &[(&str, &str)], opts: &RunOpts) -> Outcome {
    let mut i = Inputs::new();
    for (k, v) in inputs {
        i.set(k, *v);
    }
    let mut s = Secrets::new();
    for (k, v) in secrets {
        s.set(k, Zeroizing::new(v.to_string()));
    }
    run(flow, ops, &i, &s, opts)
}

fn ids(o: &Outcome) -> Vec<String> {
    o.report.checks.iter().map(|c| format!("{} {}", c.id, c.status.as_str())).collect()
}

fn has(o: &Outcome, id: &str, status: Status) -> bool {
    o.report.checks.iter().any(|c| c.id == id && c.status == status)
}

/// The JSON and the text form of an outcome, for the secret canary.
fn printed(o: &Outcome) -> String {
    let mut s = o.to_json(false).to_string();
    for c in &o.report.checks {
        s.push_str(&crate::checks::Report::check_text(c));
    }
    s.push_str(&o.report.summary());
    s.push_str(&format!("{:?}", o.results));
    s
}

const AGENT: &[(&str, &str)] = &[("harness", "claude-code"), ("name", "builder"), ("model", "sonnet")];

// ── the contract ────────────────────────────────────────────────────────────

#[test]
fn seven_flows_in_order_with_cli_ids() {
    let ids: Vec<&str> = flows().iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, ["agent", "tool", "folder", "box", "phone", "invite", "relay"]);
    for f in flows() {
        assert_eq!(flow(f.id.as_str()).map(|d| d.title), Some(f.title));
        assert!(!f.why.is_empty() && !f.summary.is_empty());
    }
    assert!(flow("nope").is_none());
}

#[test]
fn every_fail_has_a_fix_and_one_line_details() {
    let mut f = Fake { sandbox: SandboxFact { tier: "unconfined".into(), summary: String::new() }, ..Fake::default() };
    for flow in FlowId::ALL {
        let o = run_(flow, &mut f, &[]);
        let d = o.to_json(false);
        assert_eq!(d["schema"], "ligus.checks.v1");
        assert_eq!(d["command"], format!("setup {}", flow.as_str()));
        assert_eq!(d["exitCode"], o.exit);
        for c in d["checks"].as_array().unwrap() {
            assert!(c["id"].as_str().unwrap().starts_with(flow.as_str()), "{c}");
            if c["status"] == "fail" {
                assert!(c["fix"].is_string(), "{c}");
            }
        }
    }
}

#[test]
fn the_setup_ops_trait_cannot_issue_an_admission() {
    // A setup flow observes a person's approval; it never makes one.
    let src = include_str!("ops.rs").to_lowercase();
    for word in ["fn issue", "recovery", "presence", "approve", "fn admit", "certify"] {
        assert!(!src.contains(word), "SetupOps must not offer '{word}'");
    }
}

#[test]
fn an_agent_driver_is_refused_before_any_side_effect() {
    for flow in FlowId::ALL {
        let mut f = Fake::default();
        let o = run_o(flow, &mut f, AGENT, &[], &RunOpts { driver: Driver::Agent, ..Default::default() });
        assert_eq!(o.exit, exit::REFUSED, "{flow:?}");
        assert!(f.writes.is_empty(), "{flow:?}: {:?}", f.writes);
    }
    let mut f = Fake::default();
    let o = run_o(FlowId::Tool, &mut f, &[("id", "x"), ("kind", "url"), ("url", "https://m.example.com")], &[("token", CANARY)], &RunOpts { driver: Driver::Agent, ..Default::default() });
    assert_eq!(o.exit, exit::REFUSED);
    assert!(f.secrets_seen.is_empty());
}

// ── gates and validation ────────────────────────────────────────────────────

#[test]
fn viewer_refused_bind_and_hosted_tool() {
    for flow in [FlowId::Folder, FlowId::Tool, FlowId::Agent, FlowId::Invite] {
        let mut f = Fake { role: Some(WorkspaceRole::Viewer), ..Fake::default() };
        let o = run_(flow, &mut f, &[]);
        assert_eq!(o.exit, exit::REFUSED, "{flow:?} {:?}", ids(&o));
        assert!(f.writes.is_empty());
    }
    let mut f = Fake { role: Some(WorkspaceRole::Contributor), ..Fake::default() };
    assert_eq!(run_(FlowId::Invite, &mut f, &[]).exit, exit::REFUSED, "inviting is an Admin's");
}

#[test]
fn unknown_and_misplaced_inputs_are_usage_errors() {
    let mut f = Fake::default();
    let o = run_(FlowId::Agent, &mut f, &[("harness", "claude-code"), ("name", "x"), ("colour", "blue")]);
    assert_eq!(o.exit, exit::USAGE);
    assert!(has(&o, "agent.input.colour", Status::Fail));
    // A secret given as a plain input is refused, and never echoed.
    let o = run_(FlowId::Relay, &mut f, &[("token", CANARY)]);
    assert_eq!(o.exit, exit::USAGE);
    assert!(!printed(&o).contains(CANARY));
    // A missing required field names its flag.
    let o = run_(FlowId::Agent, &mut f, &[("harness", "claude-code")]);
    assert_eq!(o.exit, exit::USAGE);
    assert!(o.report.checks.iter().any(|c| c.fix.as_deref().is_some_and(|x| x.contains("--name"))), "{:?}", ids(&o));
    assert!(f.writes.is_empty());
}

// ── agent ───────────────────────────────────────────────────────────────────

#[test]
fn agent_flow_creates_runtime_then_agent_and_reruns_as_a_no_op() {
    let mut f = Fake::default();
    let o = run_(FlowId::Agent, &mut f, AGENT);
    assert_eq!(o.exit, 0, "{:?}", ids(&o));
    assert!(o.done);
    assert_eq!(f.writes, ["add_runtime claude-code-sonnet", "add_agent builder"]);
    assert!(o.results.contains_key("agentId"));
    let again = run_(FlowId::Agent, &mut f, AGENT);
    assert_eq!(again.exit, 0);
    assert!(again.done);
    assert_eq!(f.writes.len(), 2, "a rerun writes nothing: {:?}", f.writes);
    assert!(has(&again, "agent.agent", Status::Ok));
}

#[test]
fn local_servers_are_offered_for_agy_only_and_stay_off_unless_asked() {
    let f = Fake::default();
    let field = |h: &str| {
        let mut i = Inputs::new();
        i.set("harness", h);
        agent::steps(&f, &i).into_iter().flat_map(|s| s.fields).find(|x| x.id == "local-servers").map(|x| (x.applies(&i), x.default.clone()))
    };
    assert_eq!(field("antigravity"), Some((true, None)), "shown for agy, off by default");
    assert_eq!(field("claude-code"), Some((false, None)), "hidden for other harnesses");
    // Not asked: nothing written but the runtime and the agent.
    let mut f = Fake::default();
    f.harnesses.push(super::fake::harness("antigravity", true, crate::cli_detect::LoginState::LoggedIn));
    let o = run_(FlowId::Agent, &mut f, &[("harness", "antigravity"), ("name", "gem")]);
    assert_eq!(o.exit, 0, "{:?}", ids(&o));
    assert!(f.local_servers.is_empty() && !f.writes.iter().any(|w| w.starts_with("set_local_servers")), "{:?}", f.writes);
    // Asked: on for that agent only, after it exists; a rerun is a no-op.
    let o = run_(FlowId::Agent, &mut f, &[("harness", "antigravity"), ("name", "gem2"), ("local-servers", "on")]);
    assert_eq!(o.exit, 0, "{:?}", ids(&o));
    assert_eq!(f.writes.last().map(String::as_str), Some("set_local_servers gem2 true"), "{:?}", f.writes);
    let gem2 = f.agents.iter().find(|a| a.name == "gem2").unwrap().id;
    assert_eq!(f.local_servers, vec![gem2]);
    assert!(has(&o, "agent.local-servers.macos", Status::Warn), "a Mac says it has no effect here: {:?}", ids(&o));
    let n = f.writes.len();
    let again = run_(FlowId::Agent, &mut f, &[("harness", "antigravity"), ("name", "gem2"), ("local-servers", "on")]);
    assert_eq!(again.exit, 0);
    assert_eq!(f.writes.len(), n, "{:?}", f.writes);
    // A test turn of an agent with the grant runs with it.
    let o = run_(FlowId::Agent, &mut f, &[("harness", "antigravity"), ("name", "gem2"), ("test-turn", "on")]);
    assert_eq!(o.exit, 0, "{:?}", ids(&o));
    assert!(f.writes.iter().any(|w| w.starts_with("test_turn_with_local_servers")), "{:?}", f.writes);
    // The existing agent turned on by a rerun with the flag.
    let o = run_(FlowId::Agent, &mut f, &[("harness", "antigravity"), ("name", "gem"), ("local-servers", "on")]);
    assert_eq!(o.exit, 0, "{:?}", ids(&o));
    assert_eq!(f.local_servers.len(), 2);
}

#[test]
fn local_servers_are_the_owners_and_dry_run_writes_nothing() {
    let mut f = Fake { role: Some(WorkspaceRole::Admin), ..Fake::default() };
    let o = run_(FlowId::Agent, &mut f, &[("harness", "claude-code"), ("name", "builder"), ("model", "sonnet"), ("local-servers", "on")]);
    assert_eq!(o.exit, exit::REFUSED, "{:?}", ids(&o));
    assert!(!f.writes.iter().any(|w| w.starts_with("add_agent")), "refused before the agent is written: {:?}", f.writes);
    let mut f = Fake::default();
    let o = run_o(FlowId::Agent, &mut f, &[("harness", "claude-code"), ("name", "builder"), ("model", "sonnet"), ("local-servers", "on")], &[], &RunOpts { dry_run: true, ..RunOpts::default() });
    assert!(f.writes.is_empty() && has(&o, "agent.local-servers", Status::Skip), "{:?}", ids(&o));
}

#[test]
fn agent_flow_reuses_a_matching_runtime() {
    let mut f = Fake { runtimes: vec![RuntimeFact { id: "cc".into(), harness: "claude-code".into(), model: "sonnet".into(), endpoint: String::new() }], ..Fake::default() };
    run_(FlowId::Agent, &mut f, AGENT);
    assert_eq!(f.writes, ["add_agent builder"]);
}

#[test]
fn missing_harness_or_sign_in_is_fixable_with_the_command() {
    let mut f = Fake::default();
    let o = run_(FlowId::Agent, &mut f, &[("harness", "copilot"), ("name", "x")]);
    assert_eq!(o.exit, exit::FIXABLE);
    assert!(o.report.checks.iter().any(|c| c.fix.as_deref() == Some("install copilot")));
    let o = run_(FlowId::Agent, &mut f, &[("harness", "codex"), ("name", "x")]);
    assert_eq!(o.exit, exit::FIXABLE);
    assert!(o.report.checks.iter().any(|c| c.fix.as_deref().is_some_and(|x| x.contains("`codex login`"))));
    assert!(f.writes.is_empty());
}

#[test]
fn an_unconfined_host_refuses_cli_agents_but_never_offers_to_unsandbox() {
    let mut f = Fake { sandbox: SandboxFact { tier: "unconfined".into(), summary: String::new() }, ..Fake::default() };
    let o = run_(FlowId::Agent, &mut f, AGENT);
    assert_eq!(o.exit, exit::FIXABLE);
    assert!(has(&o, "agent.sandbox", Status::Warn));
    assert!(has(&o, "agent.harness.sandbox", Status::Fail));
    assert!(!printed(&o).to_lowercase().contains("disable"));
}

#[test]
fn agent_names_follow_the_rules() {
    for bad in ["ligus", "Hive", "ana", "two words"] {
        let mut f = Fake::default();
        let o = run_(FlowId::Agent, &mut f, &[("harness", "claude-code"), ("name", bad)]);
        assert_eq!(o.exit, exit::USAGE, "{bad}");
        assert!(f.writes.is_empty());
    }
}

#[test]
fn a_passkey_refusal_is_a_persons_step_with_where_to_do_it() {
    // The CLI has no passkey: the rules hold the runtime for its author's
    // confirmation. That is a person's step (exit 4), not a refusal (6), and
    // the fix says where to confirm it, not "ligus doctor".
    let mut f = Fake { needs_presence: vec!["add_runtime"], ..Fake::default() };
    let o = run_(FlowId::Agent, &mut f, AGENT);
    assert_eq!(o.exit, exit::PERSON);
    let c = o.report.checks.iter().find(|c| c.id == "agent.runtime").expect("runtime check");
    assert_eq!(c.status, Status::Warn);
    let fix = c.fix.clone().unwrap_or_default();
    assert!(fix.contains("desktop app") && fix.contains("--recovery-words-file"), "{fix}");
    assert!(!fix.contains("doctor"), "{fix}");
    assert!(o.report.next.iter().any(|n| n.contains("desktop app")), "{:?}", o.report.next);
    assert!(f.writes.is_empty());

    // On a surface with a passkey (a cancelled ceremony), the line says to
    // confirm it, and never names a command-line flag.
    let mut f = Fake { surface: Surface::Desktop, needs_presence: vec!["add_agent"], ..Fake::default() };
    let o = run_(FlowId::Agent, &mut f, AGENT);
    assert_eq!(o.exit, exit::PERSON);
    let c = o.report.checks.iter().find(|c| c.id == "agent.agent").expect("agent check");
    assert!(!c.fix.clone().unwrap_or_default().contains("--recovery-words-file"));
    // A person's step keeps what came before it: the rerun reuses the runtime.
    assert_eq!(f.writes, ["add_runtime claude-code-sonnet"]);
}

#[test]
fn a_failed_agent_write_rolls_the_runtime_back() {
    let mut f = Fake { fail: vec!["add_agent"], ..Fake::default() };
    let o = run_(FlowId::Agent, &mut f, AGENT);
    assert_eq!(o.exit, exit::FAILED);
    assert!(has(&o, "agent.rolled-back", Status::Ok), "{:?}", ids(&o));
    assert_eq!(f.writes, ["add_runtime claude-code-sonnet", "remove_runtime claude-code-sonnet"]);
    assert!(f.runtimes.is_empty());
}

#[test]
fn workspace_runtimes_need_an_admin() {
    let mut f = Fake { role: Some(WorkspaceRole::Contributor), runtimes_need_admin: true, ..Fake::default() };
    let o = run_(FlowId::Agent, &mut f, AGENT);
    assert_eq!(o.exit, exit::REFUSED);
    assert!(f.writes.is_empty());
}

#[test]
fn dry_run_writes_nothing_in_any_flow() {
    let opts = RunOpts { dry_run: true, ..Default::default() };
    let cases: Vec<(FlowId, Vec<(&str, &str)>, Vec<(&str, &str)>)> = vec![
        (FlowId::Agent, AGENT.to_vec(), vec![]),
        (FlowId::Tool, vec![("id", "files"), ("command", "npx -y server ~/notes")], vec![]),
        (FlowId::Folder, vec![("path", "/w")], vec![]),
        (FlowId::Box, vec![], vec![("code", CANARY)]),
        (FlowId::Relay, vec![("kind", "own"), ("url", "https://r2.example.com")], vec![]),
    ];
    for (flow, inputs, secrets) in cases {
        let mut f = Fake { folder: checkout("github.com/o/r"), ..Fake::default() };
        let o = run_o(flow, &mut f, &inputs, &secrets, &opts);
        assert_eq!(o.exit, 0, "{flow:?} {:?}", ids(&o));
        assert!(f.writes.iter().all(|w| !w.contains("add_") && !w.contains("switch") && !w.contains("redeem") && !w.contains("bind") && !w.starts_with("handshake")), "{flow:?}: {:?}", f.writes);
        assert!(o.report.checks.iter().any(|c| c.status == Status::Skip && c.detail.starts_with("would")), "{flow:?}");
        assert!(f.secrets_seen.is_empty());
    }
}

#[test]
fn cancel_stops_between_steps_and_undoes() {
    let cancel = Arc::new(AtomicBool::new(true));
    let mut f = Fake::default();
    let o = run_o(FlowId::Agent, &mut f, AGENT, &[], &RunOpts { cancel: Some(cancel), ..Default::default() });
    assert_eq!(o.exit, exit::FAILED);
    assert!(has(&o, "agent.cancelled", Status::Warn));
    assert!(f.writes.is_empty());
}

#[test]
fn the_test_turn_is_optional_and_reported() {
    let mut f = Fake::default();
    let mut inputs = AGENT.to_vec();
    inputs.push(("test-turn", "true"));
    let o = run_(FlowId::Agent, &mut f, &inputs);
    assert!(has(&o, "agent.test-turn", Status::Ok), "{:?}", ids(&o));
    let mut f = Fake { test_turn: Ok(TestTurn { ok: false, latency_ms: 0, detail: "401 from the model".into() }), ..Fake::default() };
    let o = run_(FlowId::Agent, &mut f, &inputs);
    assert_eq!(o.exit, exit::FAILED);
    assert_eq!(f.agents.len(), 1, "a failed test turn keeps the agent (a verify failure, not a half config)");
}

// ── tool ────────────────────────────────────────────────────────────────────

#[test]
fn tool_flow_never_persists_enabled_without_reach() {
    let mut f = Fake::default();
    let a = f.add_agent("builder", "cc").unwrap();
    f.writes.clear();
    let o = run_(FlowId::Tool, &mut f, &[("id", "files"), ("command", "npx -y server ~/notes"), ("reach", "builder")]);
    assert_eq!(o.exit, 0, "{:?}", ids(&o));
    // Handshake first, on the unsaved spec; then one write with its reach.
    assert_eq!(f.writes, ["handshake files", "add_mcp_server files enabled=true reach=1"]);
    assert_eq!(f.mcp[0].agents, vec![a]);
    // Rerun: nothing.
    run_(FlowId::Tool, &mut f, &[("id", "files"), ("command", "npx -y server ~/notes"), ("reach", "builder")]);
    assert_eq!(f.writes.len(), 2);
}

#[test]
fn tool_secret_goes_to_the_store_only_and_rolls_back() {
    let mut f = Fake { fail: vec!["add_mcp_server"], ..Fake::default() };
    let o = run_s(FlowId::Tool, &mut f, &[("id", "gh"), ("kind", "url"), ("url", "https://mcp.example.com/v1")], &[("token", CANARY)]);
    assert_eq!(o.exit, exit::FAILED);
    assert!(f.mcp_secrets.is_empty(), "the token is removed with the failed write");
    assert!(f.writes.contains(&"delete_mcp_secret gh".to_string()));
    assert!(!printed(&o).contains(CANARY));
    let mut f = Fake::default();
    let o = run_s(FlowId::Tool, &mut f, &[("id", "gh"), ("kind", "url"), ("url", "https://mcp.example.com/v1")], &[("token", CANARY)]);
    assert_eq!(o.exit, 0, "{:?}", ids(&o));
    assert_eq!(f.mcp_secrets.get("gh").map(String::as_str), Some(CANARY));
    assert!(!printed(&o).contains(CANARY));
    assert!(!printed(&o).contains("/v1"), "only the origin is shown");
}

#[test]
fn tool_urls_and_args_carrying_secrets_are_refused() {
    for (inputs, why) in [
        (vec![("id", "x"), ("kind", "url"), ("url", "https://u:pw@m.example.com")], "userinfo"),
        (vec![("id", "x"), ("kind", "url"), ("url", "https://m.example.com/?token=abc")], "query"),
        (vec![("id", "x"), ("kind", "url"), ("url", "http://m.example.com")], "plain http"),
        (vec![("id", "x"), ("command", "server --api-key sk-abcdefghijklmnopqrstuvwx")], "arg"),
        (vec![("id", "Bad Id"), ("command", "server")], "id"),
    ] {
        let mut f = Fake::default();
        let o = run_(FlowId::Tool, &mut f, &inputs);
        assert_eq!(o.exit, exit::USAGE, "{why}: {:?}", ids(&o));
        assert!(f.writes.is_empty(), "{why}");
    }
    // A token is only for a URL server.
    let mut f = Fake::default();
    assert_eq!(run_s(FlowId::Tool, &mut f, &[("id", "x"), ("command", "server")], &[("token", CANARY)]).exit, exit::USAGE);
}

#[test]
fn lockdown_wins_over_a_new_tool() {
    let mut f = Fake { lockdown: true, ..Fake::default() };
    let o = run_(FlowId::Tool, &mut f, &[("id", "files"), ("command", "server")]);
    assert_eq!(o.exit, exit::REFUSED);
    assert!(f.writes.is_empty());
    let mut f = Fake { lockdown: true, lockdown_allowed: vec!["files".into()], ..Fake::default() };
    assert_eq!(run_(FlowId::Tool, &mut f, &[("id", "files"), ("command", "server")]).exit, 0);
}

#[test]
fn a_failed_handshake_saves_nothing() {
    let mut f = Fake { handshake: Err(OpsError::Offline("connection refused".into())), ..Fake::default() };
    let o = run_(FlowId::Tool, &mut f, &[("id", "r"), ("kind", "url"), ("url", "https://m.example.com")]);
    assert_eq!(o.exit, exit::OFFLINE);
    assert!(f.mcp.is_empty());
}

#[test]
fn shell_tools_are_flagged() {
    let mut f = Fake { handshake: Ok(vec!["run_command".into()]), ..Fake::default() };
    let o = run_(FlowId::Tool, &mut f, &[("id", "sh"), ("command", "server")]);
    assert!(has(&o, "tool.handshake.shell", Status::Warn));
}

// ── folder ──────────────────────────────────────────────────────────────────

fn checkout(canonical: &str) -> FolderFact {
    FolderFact {
        path: "/w".into(),
        exists: true,
        is_checkout: true,
        canonical: Some(canonical.into()),
        default_branch: Some("main".into()),
        root_commit: Some("abc".into()),
        ..Default::default()
    }
}

#[test]
fn folder_adds_then_binds_and_reruns_clean() {
    let mut f = Fake { folder: checkout("github.com/o/r"), ..Fake::default() };
    let o = run_(FlowId::Folder, &mut f, &[("path", "/w")]);
    assert_eq!(o.exit, 0, "{:?}", ids(&o));
    assert_eq!(f.writes, ["add_repo github.com/o/r  main", "bind_folder /w"]);
    f.folder.bound_to = Some(f.repos[0].id.clone());
    let o = run_(FlowId::Folder, &mut f, &[("path", "/w")]);
    assert!(o.done);
    assert_eq!(f.writes.len(), 2);
}

#[test]
fn folder_bind_failure_writes_no_repo_event() {
    for (folder, why) in [
        (FolderFact { exists: false, ..checkout("github.com/o/r") }, "missing"),
        (FolderFact { is_checkout: false, ..checkout("github.com/o/r") }, "not git"),
        (FolderFact { linked_worktree: true, ..checkout("github.com/o/r") }, "worktree"),
        (FolderFact { managed_elsewhere: Some("serve".into()), ..checkout("github.com/o/r") }, "managed"),
        (FolderFact { bound_to: Some("other".into()), ..checkout("github.com/o/r") }, "taken"),
    ] {
        let mut f = Fake { folder, ..Fake::default() };
        let o = run_(FlowId::Folder, &mut f, &[("path", "/w")]);
        assert_ne!(o.exit, 0, "{why}");
        assert!(f.writes.is_empty(), "{why}: {:?}", f.writes);
    }
}

#[test]
fn binding_an_existing_repo_needs_no_admin_but_adding_does() {
    let repo = RepoFact { id: "r1".into(), canonical: "github.com/o/r".into(), display_name: "r".into(), default_branch: "main".into(), bound_here: None };
    let mut f = Fake { role: Some(WorkspaceRole::Contributor), folder: checkout("github.com/o/r"), repos: vec![repo], ..Fake::default() };
    assert_eq!(run_(FlowId::Folder, &mut f, &[("path", "/w")]).exit, 0);
    assert_eq!(f.writes, ["bind_folder /w"]);
    let mut f = Fake { role: Some(WorkspaceRole::Contributor), folder: checkout("github.com/o/new"), ..Fake::default() };
    assert_eq!(run_(FlowId::Folder, &mut f, &[("path", "/w")]).exit, exit::REFUSED);
    assert!(f.writes.is_empty());
}

#[test]
fn folder_can_start_with_a_scratch_repo() {
    let mut f = Fake::default();
    let o = run_(FlowId::Folder, &mut f, &[("scratch", "true"), ("name", "spike")]);
    assert_eq!(o.exit, 0, "{:?}", ids(&o));
    assert!(o.done);
    assert_eq!(f.writes, ["create_scratch spike"]);
    assert_eq!(o.results.get("scratch").map(String::as_str), Some("true"));
    // No path needed; a path and a scratch repo at once is a usage error.
    let mut f = Fake::default();
    assert_eq!(run_(FlowId::Folder, &mut f, &[("scratch", "true"), ("path", "/w")]).exit, exit::USAGE);
    assert!(f.writes.is_empty());
    // An Admin's, like adding any repository.
    let mut f = Fake { role: Some(WorkspaceRole::Contributor), ..Fake::default() };
    assert_eq!(run_(FlowId::Folder, &mut f, &[("scratch", "true")]).exit, exit::REFUSED);
    assert!(f.writes.is_empty());
    // A host that can't make one says so.
    let mut f = Fake { fail: vec!["create_scratch"], ..Fake::default() };
    assert_ne!(run_(FlowId::Folder, &mut f, &[("scratch", "true")]).exit, 0);
    // Without --scratch, a folder is still needed.
    let mut f = Fake::default();
    assert_eq!(run_(FlowId::Folder, &mut f, &[]).exit, exit::USAGE);
}

// ── box, phone, invite ──────────────────────────────────────────────────────

#[test]
fn box_flow_stops_for_the_person_then_redeems() {
    let mut f = Fake::default();
    let o = run_(FlowId::Box, &mut f, &[("label", "home")]);
    assert_eq!(o.exit, exit::PERSON);
    assert_eq!(o.results.get("joinRequest").map(String::as_str), Some("ligusjoin1:PUBLIC"));
    assert!(o.report.next.iter().any(|n| n.contains("--code -")));
    let o = run_s(FlowId::Box, &mut f, &[], &[("code", CANARY)]);
    assert_eq!(o.exit, 0, "{:?}", ids(&o));
    assert!(o.done);
    assert!(!printed(&o).contains(CANARY));
    // Joined: a rerun only verifies.
    let before = f.writes.len();
    assert_eq!(run_(FlowId::Box, &mut f, &[]).exit, 0);
    assert_eq!(f.writes.len(), before);
}

#[test]
fn setup_box_refuses_the_code_on_argv_and_the_desktop_data_dir() {
    let mut f = Fake::default();
    let o = run_(FlowId::Box, &mut f, &[("code", "ligusadmit1:xyz")]);
    assert_eq!(o.exit, exit::USAGE);
    assert!(!printed(&o).contains("ligusadmit1:xyz"));
    let mut f = Fake { box_state: Ok(BoxFact { desktop_data_dir: true, ..Default::default() }), ..Fake::default() };
    assert_eq!(run_(FlowId::Box, &mut f, &[]).exit, exit::FIXABLE);
    assert!(f.writes.is_empty());
}

#[test]
fn desktop_box_flow_is_an_admin_handoff() {
    let mut f = Fake { surface: Surface::Desktop, role: Some(WorkspaceRole::Contributor), ..Fake::default() };
    assert_eq!(run_(FlowId::Box, &mut f, &[]).exit, exit::REFUSED);
    let mut f = Fake { surface: Surface::Desktop, ..Fake::default() };
    let o = run_(FlowId::Box, &mut f, &[]);
    assert_eq!(o.exit, exit::PERSON);
    assert!(f.writes.is_empty());
    let mut f = Fake { surface: Surface::Desktop, relay: None, ..Fake::default() };
    assert_eq!(run_(FlowId::Box, &mut f, &[]).exit, exit::FIXABLE);
}

#[test]
fn phone_and_invite_never_carry_their_secret() {
    for surface in [Surface::Cli, Surface::Desktop] {
        let mut f = Fake { surface, ..Fake::default() };
        let o = run_(FlowId::Phone, &mut f, &[]);
        assert_eq!(o.exit, exit::PERSON);
        let o = run_(FlowId::Invite, &mut f, &[]);
        assert_eq!(o.exit, exit::PERSON, "{:?}", ids(&o));
        assert!(has(&o, "invite.role", Status::Ok));
        assert!(f.writes.is_empty());
    }
}

#[test]
fn invites_are_tracked() {
    let inv = |used| InviteFact { id: uuid::Uuid::new_v4(), role: "contributor".into(), created_ms: 0, used, closed: false };
    let f = Fake { invites: vec![inv(true), inv(false)], ..Fake::default() };
    let s = status(&f);
    let i = s.iter().find(|s| s.flow.id == FlowId::Invite).unwrap();
    assert_eq!((i.progress, i.detail.as_str()), (Progress::Waiting, "1 waiting, 1 accepted"));
}

#[test]
fn phone_precheck_failures_block() {
    let mut f = Fake {
        surface: Surface::Desktop,
        precheck: Ok(vec![crate::checks::Check::fail("phone.precheck.relay", "relay", "no relay").fix("set one")]),
        ..Fake::default()
    };
    assert_eq!(run_(FlowId::Phone, &mut f, &[]).exit, exit::FIXABLE);
}

// ── relay ───────────────────────────────────────────────────────────────────

#[test]
fn relay_flow_checks_switches_and_reruns_clean() {
    let mut f = Fake { relay: None, role: None, ..Fake::default() };
    let o = run_(FlowId::Relay, &mut f, &[]);
    assert_eq!(o.exit, 0, "{:?}", ids(&o));
    assert_eq!(f.relay.as_deref(), Some(ligus_core::COMMUNITY_RELAY_URL));
    let o = run_(FlowId::Relay, &mut f, &[]);
    assert!(has(&o, "relay.switch", Status::Ok));
    assert_eq!(f.writes.len(), 1);
}

#[test]
fn relay_failures_map_to_the_table_and_leave_the_old_url() {
    for (probe, code) in [
        (ProbeFact::Unreachable("connection refused".into()), exit::OFFLINE),
        (ProbeFact::Unauthorized, exit::FIXABLE),
        (ProbeFact::HttpStatus(500), exit::FAILED),
    ] {
        let mut f = Fake { probe, ..Fake::default() };
        let o = run_(FlowId::Relay, &mut f, &[("kind", "own"), ("url", "https://r2.example.com")]);
        assert_eq!(o.exit, code, "{:?}", ids(&o));
        assert_eq!(f.relay.as_deref(), Some("https://relay.example.com"));
        assert!(o.report.checks.iter().any(|c| c.fix.is_some()));
    }
    let mut f = Fake { probe: ProbeFact::HttpStatus(404), ..Fake::default() };
    assert_eq!(run_(FlowId::Relay, &mut f, &[("kind", "own"), ("url", "https://r2.example.com")]).exit, 0, "a new relay passes with a warning");
    let mut f = Fake::default();
    for bad in ["http://r.example.com", "http://localhost.example.com", "https://u:p@r.example.com"] {
        assert_eq!(run_(FlowId::Relay, &mut f, &[("kind", "own"), ("url", bad)]).exit, exit::USAGE, "{bad}");
    }
    assert!(f.writes.is_empty());
    let mut f = Fake { fail: vec!["switch_relay"], ..Fake::default() };
    run_(FlowId::Relay, &mut f, &[("kind", "own"), ("url", "https://r2.example.com")]);
    assert_eq!(f.relay.as_deref(), Some("https://relay.example.com"), "relay_switch_failure_leaves_old_url");
}

#[test]
fn relay_token_canary() {
    let mut f = Fake::default();
    let o = run_s(FlowId::Relay, &mut f, &[("kind", "own"), ("url", "https://r2.example.com")], &[("token", CANARY)]);
    assert_eq!(o.exit, 0);
    assert!(!printed(&o).contains(CANARY));
    assert!(!format!("{:?}", Secrets::new()).contains(CANARY));
    let mut s = Secrets::new();
    s.set("token", Zeroizing::new(CANARY.into()));
    assert!(!format!("{s:?}").contains(CANARY));
}

// ── status and forms ────────────────────────────────────────────────────────

#[test]
fn status_lists_all_seven_without_side_effects() {
    let mut f = Fake { role: Some(WorkspaceRole::Viewer), ..Fake::default() };
    f.add_agent("a", "r").unwrap();
    let w = f.writes.len();
    let s = status(&f);
    assert_eq!(s.len(), 7);
    assert_eq!(s[0].progress, Progress::Unavailable, "a Viewer can't add an agent");
    assert_eq!(f.writes.len(), w);
}

#[test]
fn forms_offer_choices_and_flag_names() {
    let f = Fake::default();
    let form = form(FlowId::Agent, &f, &Inputs::new().with("harness", "claude-code"));
    assert!(!form.blocked);
    let h = form.fields().find(|f| f.id == "harness").unwrap();
    let FieldKind::Choice { options } = &h.kind else { panic!() };
    assert_eq!(options.iter().find(|o| o.value == "codex").and_then(|o| o.hint.as_deref()), Some("not signed in"));
    assert_eq!(options.iter().find(|o| o.value == "copilot").and_then(|o| o.hint.as_deref()), Some("not installed"));
    let m = form.fields().find(|f| f.id == "model").unwrap();
    assert!(matches!(&m.kind, FieldKind::Suggest { options } if options.iter().any(|o| o.value == "sonnet")));
    let json = serde_json::to_value(&form).unwrap();
    assert_eq!(json["flow"]["id"], "agent");
    assert!(json["steps"].as_array().unwrap().len() >= 4);
    // Secret fields say so, and carry no default.
    let t = form_fields(FlowId::Tool, &f, &Inputs::new());
    assert!(t.iter().any(|f| f.id == "token" && f.kind == FieldKind::Secret && f.default.is_none()));
    let blocked = super::form(FlowId::Invite, &Fake { role: Some(WorkspaceRole::Viewer), ..Fake::default() }, &Inputs::new());
    assert!(blocked.blocked);
}

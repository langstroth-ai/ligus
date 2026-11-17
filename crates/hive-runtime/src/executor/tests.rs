//! Plan properties and the sandbox integration tests. The `sandbox_*` tests
//! spawn `sh -c` under this host's real executor (Seatbelt on macOS,
//! bubblewrap on Linux) and assert the operation fails; on a host without one
//! they are skipped with a note (and `unconfined_refuses_write_tiers` covers
//! that case).

use super::*;
use hive_core::sandbox::TightenedBy;

/// Deterministic generator for property loops (256 cases; set
/// `LIGUS_PROP_CASES=4096` for the nightly run).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[(self.next() % xs.len() as u64) as usize]
    }
    fn coin(&mut self) -> bool {
        self.next() % 2 == 0
    }
}

fn cases() -> u64 {
    std::env::var("LIGUS_PROP_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(256)
}

const KINDS: &[ModelProviderKind] = &[
    ModelProviderKind::ClaudeCode,
    ModelProviderKind::Codex,
    ModelProviderKind::Copilot,
    ModelProviderKind::Antigravity,
    ModelProviderKind::Aider,
    ModelProviderKind::Pi,
    ModelProviderKind::Hermes,
];

fn random_envelope(r: &mut Rng) -> SandboxEnvelope {
    let mut e = SandboxEnvelope::workspace_default();
    if r.coin() {
        let allow: Vec<String> = ["docs", "linear", "gh"].iter().filter(|_| r.coin()).map(|s| s.to_string()).collect();
        e = e.tighten(TightenedBy::Dearth, EgressPolicy::AllowList { mcp_servers: allow }, false);
    }
    if r.coin() {
        e = e.tighten(TightenedBy::Lockdown, EgressPolicy::Closed, true);
    }
    if r.coin() {
        e.protected_dirs.push(".github".into());
    }
    e
}

fn host_fixture() -> HostPaths {
    HostPaths {
        data_dirs: vec![PathBuf::from("/data/ligus")],
        secret_files: vec![PathBuf::from("/etc/ligus/serve.token")],
        allow_read: vec![PathBuf::from("/data/ligus/repos")],
        extra_egress: vec![HostPort::new("crates.io", 443)],
        ..Default::default()
    }
}

/// `prop_enforcement_plan_host_independent`: the desktop, worker, serve and
/// `ligus agent` adapters give the same plan for the same turn.
#[test]
fn prop_enforcement_plan_host_independent() {
    let mut r = Rng(0x5eed_0001);
    let host = host_fixture();
    for _ in 0..cases() {
        let envelope = random_envelope(&mut r);
        let kind = *r.pick(KINDS);
        let purpose = *r.pick(&[Purpose::Agent(kind), Purpose::McpServer, Purpose::Tool, Purpose::Preflight]);
        let gate = r.pick(&[GatePlan::Full, GatePlan::ReadOnly]).clone();
        let mcp_hosts = vec![HostPort::new("mcp.example", 443)];
        let mcp_ids = vec!["docs".to_string()];
        let (iso, writes) = (r.coin(), r.coin());
        let presets: Vec<EgressPreset> = if r.coin() { vec![EgressPreset::PackageRegistries] } else { Vec::new() };
        let plan_for = |h: HostKind| {
            EnvelopeEnforcer::plan(&PlanInputs {
                host: h,
                envelope: &envelope,
                purpose,
                gate: &gate,
                worktree: Path::new("/repo/.ligus/worktrees/t1"),
                worktree_is_isolated: iso,
                write_capable: writes,
                tmp: Path::new("/tmp/ligus-turn-1"),
                home: Path::new("/home/u"),
                host_paths: &host,
                mcp_hosts: &mcp_hosts,
                mcp_ids: &mcp_ids,
                model_base_url: None,
                extra_env: &[],
                provider_key: None,
                model_provider_id: None,
                envelope_args: &[],
                ligus_mcp_socket: None,
                egress_presets: &presets,
                cli_home: None,
            })
        };
        let desktop = plan_for(HostKind::Desktop);
        for h in [HostKind::Worker, HostKind::Serve, HostKind::Agent] {
            assert_eq!(plan_for(h), desktop, "{h:?} differs for {envelope:?} {purpose:?} {gate:?}");
        }
        // Invariants that hold for every plan.
        assert!(desktop.deny_read.contains(&PathBuf::from("/data/ligus")));
        assert!(desktop.deny_read.contains(&PathBuf::from("/etc/ligus/serve.token")));
        assert!(desktop.deny_read.contains(&PathBuf::from("/home/u/.ssh")));
        assert!(desktop.read_only.contains(&PathBuf::from("/repo/.ligus/worktrees/t1/.git")));
        if !gate.allows_writes() || matches!(purpose, Purpose::McpServer) {
            assert!(!desktop.worktree_writable);
        }
        if let Purpose::Agent(_) = purpose {
            assert!(!matches!(desktop.egress, EgressPlan::Open), "an agent never gets open egress");
            if let EgressPlan::Proxy { allow } = &desktop.egress {
                if !envelope.egress.is_open() {
                    assert!(!allow.contains(&HostPort::new("crates.io", 443)), "device extras only under an open envelope");
                    assert!(!allow.contains(&HostPort::new("registry.npmjs.org", 443)), "presets only under an open envelope");
                }
                if envelope.egress == EgressPolicy::Closed {
                    assert!(!allow.contains(&HostPort::new("mcp.example", 443)), "lockdown cuts MCP hosts");
                }
            }
        }
        assert!(!desktop.env.iter().any(|(k, _)| k.to_string_lossy().contains("API_KEY")));
    }
}

/// `prop_every_envelope_field_consumed`: the enforcer destructures the
/// envelope exhaustively (a new field fails to compile there), and each
/// field that shapes the process changes the plan.
#[test]
fn prop_every_envelope_field_consumed() {
    // Compile-time half: the same exhaustive pattern as the enforcer.
    let SandboxEnvelope { isolate_writes: _, protected_dirs: _, egress: _, redact_secrets: _, human_gate: _, tightened_by: _ } =
        SandboxEnvelope::workspace_default();
    let host = host_fixture();
    let gate = GatePlan::Full;
    let plan = |e: &SandboxEnvelope, iso: bool| {
        EnvelopeEnforcer::plan(&PlanInputs {
            host: HostKind::Desktop,
            envelope: e,
            purpose: Purpose::Agent(ModelProviderKind::ClaudeCode),
            gate: &gate,
            worktree: Path::new("/repo"),
            worktree_is_isolated: iso,
            write_capable: true,
            tmp: Path::new("/tmp/t"),
            home: Path::new("/home/u"),
            host_paths: &host,
            mcp_hosts: &[HostPort::new("mcp.example", 443)],
            mcp_ids: &["docs".to_string()],
            model_base_url: None,
            extra_env: &[],
            provider_key: None,
            model_provider_id: None,
            envelope_args: &[],
            ligus_mcp_socket: None,
            egress_presets: &[],
                cli_home: None,
        })
    };
    let base = SandboxEnvelope::workspace_default();
    // isolate_writes: an in-place directory isn't writable under it.
    let iso = SandboxEnvelope { isolate_writes: true, ..base.clone() };
    assert!(plan(&base, false).worktree_writable && !plan(&iso, false).worktree_writable && plan(&iso, true).worktree_writable);
    // protected_dirs: each becomes a read-only path.
    let mut more = base.clone();
    more.protected_dirs.push(".github".into());
    assert!(plan(&more, true).read_only.contains(&PathBuf::from("/repo/.github")));
    assert!(!plan(&base, true).read_only.contains(&PathBuf::from("/repo/.github")));
    // egress: open → provider + MCP + extras; allow-list → provider + MCP; closed → provider only.
    let allow = |e: &SandboxEnvelope| match plan(e, true).egress {
        EgressPlan::Proxy { allow } => allow,
        other => panic!("{other:?}"),
    };
    let open = allow(&base);
    assert!(open.contains(&HostPort::new("api.anthropic.com", 443)) && open.contains(&HostPort::new("mcp.example", 443)) && open.contains(&HostPort::new("crates.io", 443)));
    let listed = allow(&SandboxEnvelope { egress: EgressPolicy::AllowList { mcp_servers: vec!["docs".into()] }, ..base.clone() });
    assert!(listed.contains(&HostPort::new("mcp.example", 443)) && !listed.contains(&HostPort::new("crates.io", 443)));
    let closed = SandboxEnvelope { egress: EgressPolicy::Closed, ..base.clone() };
    assert_eq!(allow(&closed), provider_endpoints(ModelProviderKind::ClaudeCode).into_iter().collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>());
    assert!(plan(&closed, true).mcp.is_empty());
    // redact_secrets / human_gate / tightened_by act outside the process:
    // they leave the plan unchanged.
    let other = SandboxEnvelope { redact_secrets: false, human_gate: true, tightened_by: vec![TightenedBy::Dearth], ..base.clone() };
    assert_eq!(plan(&other, true), plan(&base, true));
}

#[test]
fn provider_key_only_when_configured_in_ligus() {
    let host = host_fixture();
    let env_for = |key: Option<&str>, kind, mp: Option<&str>| {
        EnvelopeEnforcer::plan(&PlanInputs {
            host: HostKind::Worker,
            envelope: &SandboxEnvelope::workspace_default(),
            purpose: Purpose::Agent(kind),
            gate: &GatePlan::Full,
            worktree: Path::new("/repo"),
            worktree_is_isolated: true,
            write_capable: true,
            tmp: Path::new("/tmp/t"),
            home: Path::new("/home/u"),
            host_paths: &host,
            mcp_hosts: &[],
            mcp_ids: &[],
            model_base_url: Some("http://localhost:11434/v1"),
            extra_env: &[],
            provider_key: key,
            model_provider_id: mp,
            envelope_args: &[],
            ligus_mcp_socket: None,
            egress_presets: &[],
                cli_home: None,
        })
    };
    let p = env_for(Some("sk-configured"), ModelProviderKind::Aider, Some("anthropic"));
    assert!(p.env.iter().any(|(k, v)| k == "ANTHROPIC_API_KEY" && v == "sk-configured"));
    assert!(matches!(&p.egress, EgressPlan::Proxy { allow } if allow.contains(&HostPort::new("localhost", 11434))));
    let p = env_for(None, ModelProviderKind::ClaudeCode, None);
    assert!(!p.env.iter().any(|(k, _)| k.to_string_lossy().ends_with("_API_KEY")));
}

#[test]
fn cwd_is_never_home_root_or_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    let host = HostPaths { data_dirs: vec![dir.path().to_path_buf()], allow_read: vec![dir.path().join("repos")], ..Default::default() };
    assert!(check_cwd(Path::new("/"), &host).is_err());
    assert!(check_cwd(dir.path(), &host).is_err());
    std::fs::create_dir_all(dir.path().join("keys")).unwrap();
    assert!(check_cwd(&dir.path().join("keys"), &host).is_err());
    std::fs::create_dir_all(dir.path().join("repos/r")).unwrap();
    assert!(check_cwd(&dir.path().join("repos/r"), &host).is_ok());
    if let Some(h) = home_dir() {
        assert!(check_cwd(&h, &host).is_err());
    }
    let other = tempfile::tempdir().unwrap();
    assert!(check_cwd(other.path(), &host).is_ok());
}

/// `unconfined_refuses_write_tiers`: without an OS sandbox a write-capable
/// agent turn is refused.
#[test]
fn unconfined_refuses_write_tiers() {
    let wt = tempfile::tempdir().unwrap();
    let write = SpawnSpec::new(Purpose::Agent(ModelProviderKind::ClaudeCode), "true", ["--permission-mode", "acceptEdits"]).cwd(Some(wt.path()));
    let err = prepare_with(&Unconfined, write).err().expect("refused");
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    let codex = SpawnSpec::new(Purpose::Agent(ModelProviderKind::Codex), "true", Vec::<String>::new()).cwd(Some(wt.path()));
    assert!(prepare_with(&Unconfined, codex).is_err());
}

/// Without an OS sandbox a read-only CLI turn is refused too: its read
/// tools reach everything the user can read, `hive.db` (no at-rest
/// encryption) and keys included. The refusal names the platform and how
/// to get a sandbox.
#[test]
fn unconfined_refuses_read_only_cli_turns() {
    let wt = tempfile::tempdir().unwrap();
    for kind in [ModelProviderKind::ClaudeCode, ModelProviderKind::Copilot, ModelProviderKind::Antigravity] {
        let ro = crate::author_gate::read_only_args(kind, &[]);
        let mut spec = SpawnSpec::new(Purpose::Agent(kind), "true", ro).cwd(Some(wt.path()));
        spec.writes = Some(false);
        let err = prepare_with(&Unconfined, spec).err().unwrap_or_else(|| panic!("{kind:?} read-only ran unconfined"));
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{kind:?}");
        let msg = err.to_string();
        assert!(msg.contains(no_sandbox_platform()), "names the platform: {msg}");
    }
    #[cfg(target_os = "linux")]
    {
        let msg = no_sandbox_refusal("a CLI agent turn");
        assert!(msg.contains("bubblewrap") && msg.contains("Landlock"), "{msg}");
    }
    #[cfg(windows)]
    assert!(no_sandbox_refusal("a CLI agent turn").contains("not supported"));
}

/// `run_command` (`Purpose::Tool`), a proposal's pre-flight test and a
/// stdio MCP server are refused without an OS sandbox, at any tier: each
/// runs a model's (or an agent-written) command on this host.
#[test]
fn unconfined_refuses_tool_purpose() {
    let wt = tempfile::tempdir().unwrap();
    for purpose in [Purpose::Tool, Purpose::Preflight, Purpose::McpServer] {
        let policy = TurnPolicy { gate: GatePlan::ReadOnly, ..TurnPolicy::baseline(HostKind::Desktop) };
        let err = with_turn_sync(policy, || prepare_with(&Unconfined, SpawnSpec::new(purpose, "/bin/sh", ["-c", "cat ~/.ssh/id_ed25519"]).cwd(Some(wt.path()))).err())
            .unwrap_or_else(|| panic!("{purpose:?} ran unconfined"));
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{purpose:?}");
    }
}

/// A tool-less HTTP/API turn (pure model chat) still runs without an OS
/// sandbox: it spawns nothing and gets no local tools. An HTTP turn that
/// would get local tools (built-in file tools, `run_command`, a stdio MCP
/// server) and every CLI turn need the sandbox.
#[test]
fn unconfined_allows_toolless_api_chat() {
    for p in [ModelProviderKind::Anthropic, ModelProviderKind::Ollama, ModelProviderKind::OpenAI] {
        assert!(turn_allowed_on(ExecutorKind::Unconfined, p, false).is_ok(), "{p:?} chat");
        assert!(turn_allowed_on(ExecutorKind::Unconfined, p, true).is_err(), "{p:?} with local tools");
        assert!(turn_allowed_on(ExecutorKind::Seatbelt, p, true).is_ok());
    }
    for p in [ModelProviderKind::ClaudeCode, ModelProviderKind::Codex, ModelProviderKind::Copilot] {
        assert!(turn_allowed_on(ExecutorKind::Unconfined, p, false).is_err(), "{p:?}");
        assert!(turn_allowed_on(ExecutorKind::Bwrap, p, false).is_ok());
    }
    assert!(!local_tools_allowed_on(ExecutorKind::Unconfined));
    assert!(local_tools_allowed_on(ExecutorKind::Landlock));
}

// ---------------------------------------------------------------------------
// Integration: the real sandbox on this host
// ---------------------------------------------------------------------------

/// A fake data dir with keys, settings and a serve token, a home dir with
/// `.ssh`, and a worktree, all under one temp root.
pub(crate) struct Fixture {
    pub data: PathBuf,
    pub worktree: PathBuf,
    pub outside: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        // Never removed: the host's deny list is process-wide, and a data dir
        // that vanished mid-run would race another test's bwrap mounts.
        let base = canonical(&tempfile::tempdir().unwrap().keep());
        let data = base.join("data");
        std::fs::create_dir_all(data.join("keys")).unwrap();
        std::fs::write(data.join("keys/device.seed"), "SEED-SECRET").unwrap();
        std::fs::write(data.join("settings.json"), "{\"apiKey\":\"SETTINGS-SECRET\"}").unwrap();
        std::fs::write(data.join("serve.token"), "SERVE-TOKEN-SECRET").unwrap();
        let worktree = base.join("repo/.ligus/worktrees/t1");
        std::fs::create_dir_all(worktree.join(".git")).unwrap();
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        std::fs::write(worktree.join("README.md"), "hello").unwrap();
        let outside = base.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        configure_host(&data, &[data.join("serve.token")], &[]);
        let f = Fixture { data, worktree, outside };
        f.assert_runs();
        f
    }

    /// The sandbox runs a command at all (a deny test that passes because
    /// nothing runs would prove nothing).
    pub fn assert_runs(&self) {
        if let Some(e) = default_executor_for_tests() {
            let (ok, out) = run_sh(&*e, self, "echo sandbox-ok", isolated());
            assert!(ok && out.contains("sandbox-ok"), "the sandbox doesn't run commands: {out}");
        }
    }
}

fn default_executor_for_tests() -> Option<Arc<dyn TurnExecutor>> {
    let e = default_executor();
    (e.kind() != ExecutorKind::Unconfined).then_some(e)
}


/// The real executor for this host, or `None` (test skipped) when it has
/// none.
pub(crate) fn real_executor() -> Option<Arc<dyn TurnExecutor>> {
    // The test harness isn't a Ligus binary: point the Linux helper at the
    // `ligus-sandbox-helper` bin cargo built for this run (`helper_exe` looks
    // for it too, and fails a confined spawn, saying so, without one).
    #[cfg(target_os = "linux")]
    if let Some(exe) = std::env::current_exe().ok().and_then(|e| super::linux::find_built_helper(&e, &["ligus-sandbox-helper"])) {
        set_helper_exe(exe);
    }
    let e = default_executor();
    // Linux: bubblewrap, else the Landlock tier; neither (or macOS without
    // sandbox-exec) prints SKIPPED, or fails with LIGUS_REQUIRE_SANDBOX=1.
    if skip_without_sandbox("executor sandbox test") {
        return None;
    }
    Some(e)
}

/// Whether this host runs the Landlock tier (no namespaces).
fn landlock_tier() -> bool {
    default_executor().kind() == ExecutorKind::Landlock
}

/// Run `script` under the plan for a write-capable claude turn in the
/// fixture's worktree. Returns (exit ok, stdout+stderr).
pub(crate) fn run_sh(e: &dyn TurnExecutor, f: &Fixture, script: &str, policy: TurnPolicy) -> (bool, String) {
    with_turn_sync(policy, || {
        let spec = SpawnSpec::new(
            Purpose::Agent(ModelProviderKind::ClaudeCode),
            "/bin/sh",
            ["-c", script, "--permission-mode", "acceptEdits"],
        )
        .cwd(Some(&f.worktree));
        // `sh -c <script> <$0> <$1>…`: the tier flags ride along as
        // positional args so the plan sees a write-capable turn.
        let mut p = prepare_with(e, spec).expect("prepare");
        let out = p.command.output().expect("spawn");
        drop(p.guard);
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        (out.status.success(), text)
    })
}

fn isolated() -> TurnPolicy {
    TurnPolicy { worktree_is_isolated: true, ..TurnPolicy::baseline(HostKind::Desktop) }
}

#[test]
fn sandbox_denies_data_dir_read() {
    let Some(e) = real_executor() else { return };
    let f = Fixture::new();
    for target in ["keys/device.seed", "settings.json"] {
        let (ok, out) = run_sh(&*e, &f, &format!("cat '{}'", f.data.join(target).display()), isolated());
        assert!(!ok, "read {target}: {out}");
        assert!(!out.contains("SECRET"), "{out}");
    }
    let (ok, out) = run_sh(&*e, &f, &format!("ls '{}'", f.data.display()), isolated());
    assert!(!ok || !out.contains("keys"), "listing the data dir: {out}");
    // Control: the same read outside the sandbox succeeds, so the denial
    // above is the sandbox's doing.
    let control = {
        let out = std::process::Command::new("/bin/sh")
            .args(["-c", "cat \"$0\"", f.data.join("settings.json").to_str().unwrap()])
            .current_dir(&f.worktree)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    assert!(control.contains("SETTINGS-SECRET"), "control: {control}");
    // The worktree itself is readable.
    let (ok, out) = run_sh(&*e, &f, "cat README.md", isolated());
    assert!(ok && out.contains("hello"), "{out}");
}

#[test]
fn serve_token_unreadable_by_agent() {
    let Some(e) = real_executor() else { return };
    let f = Fixture::new();
    // Also when the token file lives outside the data dir (`--token-file`).
    let token = f.outside.join("serve.token");
    std::fs::write(&token, "SERVE-TOKEN-SECRET").unwrap();
    configure_host(&f.data, std::slice::from_ref(&token), &[]);
    for t in [f.data.join("serve.token"), token] {
        let (ok, out) = run_sh(&*e, &f, &format!("cat '{}'", t.display()), isolated());
        assert!(!ok && !out.contains("SECRET"), "{}: {out}", t.display());
    }
}

#[test]
fn sandbox_denies_write_outside_worktree() {
    let Some(e) = real_executor() else { return };
    let f = Fixture::new();
    let target = f.outside.join("x");
    let (ok, out) = run_sh(&*e, &f, &format!("echo x > '{}'", target.display()), isolated());
    assert!(!ok && !target.exists(), "{out}");
    if let Some(h) = home_dir() {
        let probe = h.join(format!(".ligus-sandbox-probe-{}", std::process::id()));
        let (ok, _) = run_sh(&*e, &f, &format!("echo x > '{}'", probe.display()), isolated());
        let existed = probe.exists();
        let _ = std::fs::remove_file(&probe);
        assert!(!ok && !existed, "wrote to $HOME");
    }
    // Inside the worktree, writes work: a new file in a subdirectory, an
    // existing top-level file.
    let (ok, out) = run_sh(&*e, &f, "echo ok > src/new.txt && cat src/new.txt && echo more >> README.md", isolated());
    assert!(ok && out.contains("ok"), "{out}");
    // A new top-level entry: allowed under bubblewrap and Seatbelt; the
    // Landlock tier can't grant it without granting `.git` too.
    let (ok, out) = run_sh(&*e, &f, "echo ok > new.txt && cat new.txt", isolated());
    if landlock_tier() {
        assert!(!ok && !f.worktree.join("new.txt").exists(), "{out}");
    } else {
        assert!(ok && out.contains("ok"), "{out}");
    }
    // A read-only turn can't write even its worktree.
    let ro = TurnPolicy { gate: GatePlan::ReadOnly, ..isolated() };
    let (ok, _) = run_sh(&*e, &f, "echo no > ro.txt", ro);
    assert!(!ok && !f.worktree.join("ro.txt").exists());
}

#[test]
fn sandbox_denies_git_dir_write() {
    let Some(e) = real_executor() else { return };
    let f = Fixture::new();
    std::fs::create_dir_all(f.worktree.join(".ligus")).unwrap();
    for target in [".git/hooks-pre-commit", ".ligus/x", "ligus.config.toml"] {
        let (ok, out) = run_sh(&*e, &f, &format!("echo x > {target}"), isolated());
        assert!(!ok && !f.worktree.join(target).exists(), "{target}: {out}");
    }
}

/// macOS: the protected names are refused in any case and at any depth.
/// APFS ignores case, so a `.CLAUDE/settings.json` made while `.claude/`
/// doesn't exist yet is the `.claude/settings.json` Claude Code loads; and
/// a nested `sub/.claude/` is the project config of a session started in
/// `sub/`.
#[cfg(target_os = "macos")]
#[test]
fn sandbox_protected_names_ignore_case_and_depth() {
    let Some(e) = real_executor() else { return };
    let f = Fixture::new();
    for (script, made) in [
        ("mkdir .CLAUDE && touch .CLAUDE/settings.json", ".CLAUDE/settings.json"),
        ("mkdir -p sub/.claude && touch sub/.claude/settings.json", "sub/.claude/settings.json"),
        ("mkdir -p sub/.VSCode && touch sub/.VSCode/tasks.json", "sub/.VSCode/tasks.json"),
        ("mkdir -p sub && echo x > sub/Ligus.Config.Toml", "sub/Ligus.Config.Toml"),
        ("echo x > .ENVRC", ".ENVRC"),
    ] {
        let (ok, out) = run_sh(&*e, &f, script, isolated());
        assert!(!ok && !f.worktree.join(made).exists(), "{script}: {out}");
    }
    // Names that only contain a protected one stay writable.
    let (ok, out) = run_sh(&*e, &f, "mkdir -p sub/my.claude-notes && echo ok > sub/my.claude-notes/a && echo ok > envrc.md", isolated());
    assert!(ok, "{out}");
}

#[test]
fn sandbox_denies_direct_egress() {
    let Some(e) = real_executor() else { return };
    let f = Fixture::new();
    // A listener on this machine stands in for "the internet": a direct
    // connect to it must fail, and nothing must arrive.
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    l.set_nonblocking(true).unwrap();
    let script = format!(
        "if command -v curl >/dev/null; then curl -s --noproxy '*' -m 3 http://127.0.0.1:{port}/; \
         else exec 3<>/dev/tcp/127.0.0.1/{port}; fi"
    );
    let (ok, out) = run_sh(&*e, &f, &format!("/bin/bash -c '{script}' || exit 7; exit 0"), isolated());
    let got = l.accept().is_ok();
    assert!(!got, "a direct connection reached the host: {out}");
    let _ = ok;
    // And through the proxy, a host that isn't allowed is refused (403).
    let script = "curl -s -o /dev/null -w '%{http_code}' -m 5 https://example.com/ ; echo";
    if which("curl") {
        let (_, out) = run_sh(&*e, &f, script, isolated());
        assert!(!out.contains("200"), "{out}");
    }
}

fn which(p: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|d| d.join(p).is_file()))
}

#[test]
fn sandbox_allows_proxy_allowlisted_host() {
    let Some(e) = real_executor() else { return };
    if !which("curl") {
        eprintln!("skipped: no curl");
        return;
    }
    let f = Fixture::new();
    // A local HTTP server, allow-listed as a loopback model endpoint (the
    // pi-to-Ollama case): reachable through the proxy; a non-listed port isn't.
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for s in l.incoming().take(4).flatten() {
            let mut s = s;
            let mut b = [0u8; 1024];
            let _ = s.read(&mut b);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        }
    });
    let blocked = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let bport = blocked.local_addr().unwrap().port();
    let base = format!("http://localhost:{port}/v1");
    let spec_run = |url: &str| {
        with_turn_sync(isolated(), || {
            let mut spec = SpawnSpec::new(
                Purpose::Agent(ModelProviderKind::Pi),
                "/bin/sh",
                vec!["-c".to_string(), format!("curl -s -m 5 --proxy \"$HTTP_PROXY\" {url}")],
            )
            .cwd(Some(&f.worktree));
            spec.model_base_url = Some(&base);
            let mut p = prepare_with(&*e, spec).expect("prepare");
            let out = p.command.output().unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        })
    };
    assert_eq!(spec_run(&format!("http://localhost:{port}/")), "ok");
    assert_ne!(spec_run(&format!("http://localhost:{bport}/")), "ok");
}

/// Linux only: seccomp stops a new user namespace (and with it a nested
/// sandbox escape) and ptrace; Landlock or the read-only root stops writes
/// to `/usr`.
#[cfg(target_os = "linux")]
#[test]
fn sandbox_linux_seccomp_and_landlock() {
    let Some(e) = real_executor() else { return };
    let f = Fixture::new();
    if which("unshare") {
        let (ok, out) = run_sh(&*e, &f, "unshare --user true", isolated());
        assert!(!ok, "unshare --user succeeded: {out}");
    }
    let (ok, _) = run_sh(&*e, &f, "echo x > /usr/ligus-probe", isolated());
    assert!(!ok);
    // The D-Bus session bus dir is hidden.
    if let Some(run) = std::env::var_os("XDG_RUNTIME_DIR") {
        let (_, out) = run_sh(&*e, &f, &format!("ls -A '{}'", PathBuf::from(run).display()), isolated());
        assert!(!out.contains("bus"), "{out}");
    }
}

/// Linux, Landlock tier: what the missing namespaces would otherwise have
/// covered. No UDP (so no DNS or QUIC of its own), no raw sockets, no TCP
/// bind or listen, no Fast Open connect; no reading another process's
/// environment; no signals out of the sandbox (Landlock ABI 6+); no new
/// namespaces of any kind; no `io_uring`.
#[cfg(target_os = "linux")]
#[test]
fn sandbox_landlock_tier_closes_namespace_gaps() {
    let Some(e) = real_executor() else { return };
    if !landlock_tier() {
        let _ = std::io::Write::write_all(&mut std::io::stderr().lock(), b"SKIPPED sandbox_landlock_tier_closes_namespace_gaps: this host uses bubblewrap (run with LIGUS_SANDBOX=landlock)\n");
        return;
    }
    if !which("python3") {
        eprintln!("skipped: python3 is needed for the socket probes");
        return;
    }
    let f = Fixture::new();
    let py = |code: &str| format!("python3 -I -c '{code}'");
    for (what, code) in [
        ("udp socket", "import socket; socket.socket(socket.AF_INET, socket.SOCK_DGRAM); print(\"OPENED\")"),
        ("udp6 socket", "import socket; socket.socket(socket.AF_INET6, socket.SOCK_DGRAM); print(\"OPENED\")"),
        ("raw socket", "import socket; socket.socket(socket.AF_INET, socket.SOCK_RAW, 1); print(\"OPENED\")"),
        ("sctp socket", "import socket; socket.socket(socket.AF_INET, socket.SOCK_STREAM, 132); print(\"OPENED\")"),
        ("packet socket", "import socket; socket.socket(17, socket.SOCK_RAW, 0); print(\"OPENED\")"),
        ("tcp bind", "import socket; s=socket.socket(); s.bind((\"127.0.0.1\", 0)); print(\"OPENED\")"),
        ("tcp listen", "import socket; s=socket.socket(); s.listen(1); print(\"OPENED\")"),
        ("fast open", "import socket; s=socket.socket(); s.sendto(b\"x\", 0x20000000, (\"1.1.1.1\", 80)); print(\"OPENED\")"),
        ("dns", "import socket; socket.getaddrinfo(\"example.com\", 443); print(\"OPENED\")"),
        ("unshare net", "import os; os.unshare(os.CLONE_NEWNET); print(\"OPENED\")"),
        ("io_uring", "import ctypes; r=ctypes.CDLL(None).syscall(425, 8, ctypes.create_string_buffer(120)); print(\"OPENED\" if r >= 0 else \"refused\")"),
    ] {
        let (_, out) = run_sh(&*e, &f, &py(code), isolated());
        assert!(!out.contains("OPENED"), "{what} was allowed: {out}");
    }
    // A plain TCP socket still opens (the egress proxy is TCP).
    let (ok, out) = run_sh(&*e, &f, &py("import socket; socket.socket(); print(\"TCP-OK\")"), isolated());
    assert!(ok && out.contains("TCP-OK"), "{out}");
    // Another process's environment (here: this test's) is out of reach.
    std::env::set_var("LIGUS_LANDLOCK_PROBE_SECRET", "ENVIRON-SECRET");
    let me = std::process::id();
    let (_, out) = run_sh(&*e, &f, &format!("cat /proc/{me}/environ | tr '\\0' '\\n'"), isolated());
    assert!(!out.contains("ENVIRON-SECRET"), "read another process's environment: {out}");
    // Signals out of the sandbox (Landlock scoping, ABI 6+).
    if super::linux::kernel_landlock_abi() >= 6 {
        let (ok, out) = run_sh(&*e, &f, &format!("kill -0 {me} && echo SIGNALED"), isolated());
        assert!(!ok && !out.contains("SIGNALED"), "signaled a process outside the sandbox: {out}");
    }
}

/// Linux (C-10): an agent can't reach the host's unix sockets
/// (`docker.sock`, an ssh-agent, a desktop service): bwrap's read-only `/`
/// still lets a process `connect()` to any socket file it can see. Seccomp
/// refuses new `AF_UNIX` sockets inside the sandbox, so no socket path is
/// reachable, while the turn's Ligus MCP server stays reachable through
/// the helper's loopback forwarder (`mcp_bridge_reachable_inside_seatbelt_and_bwrap`).
///
/// Linux only: run it on a host with bubblewrap and user namespaces (the
/// owner's SFF box, or Linux CI); it can't run on macOS.
#[cfg(target_os = "linux")]
#[test]
fn sandbox_denies_unix_socket_connect() {
    let Some(e) = real_executor() else { return };
    let f = Fixture::new();
    // A socket the agent's mount namespace can see, as docker.sock would be.
    let path = f.outside.join("host-service.sock");
    let _ = std::fs::remove_file(&path);
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    if !which("python3") {
        eprintln!("skipped: python3 is needed for the connect probe");
        return;
    }
    let probe = format!(
        "python3 -c 'import socket,sys\ns=socket.socket(socket.AF_UNIX)\ns.connect(sys.argv[1])\nprint(\"CONNECTED\")' '{}'",
        path.display()
    );
    let (ok, out) = run_sh(&*e, &f, &probe, isolated());
    assert!(!ok && !out.contains("CONNECTED"), "an agent reached a host unix socket: {out}");
    assert!(listener.accept().is_err(), "the host socket saw a connection");
    // socketpair (local IPC between the agent's own processes) still works.
    let (ok, out) = run_sh(&*e, &f, "python3 -c 'import socket; a,b=socket.socketpair(); a.send(b\"x\"); print(b.recv(1))'", isolated());
    assert!(ok && out.contains("x"), "{out}");
}

/// macOS: a confined agent must not escape through LaunchServices. Writing
/// an app bundle into a writable dir and `open`ing it hands the launch to
/// launchd, so the app would run outside the profile (reading the data dir,
/// with unrestricted network).
///
/// Denying launchservicesd is not enough on its own: `LSOpen*` asks
/// RunningBoard to spawn the app (suspended, under launchd) before it checks
/// in with launchservicesd, so the open "fails" but the process exists, and a
/// `kill -CONT` from inside the turn resumes it unconfined. The probe tries
/// that whole chain, for a background-only and a UI-element bundle, and
/// fails if any process from the bundle starts at all, not only if it
/// manages to write.
#[cfg(target_os = "macos")]
#[test]
fn sandbox_denies_launchservices_escape() {
    let Some(e) = real_executor() else { return };
    let f = Fixture::new();
    for (n, (open, key)) in [("/usr/bin/open -g -j", "LSBackgroundOnly"), ("/usr/bin/open", "LSUIElement")].into_iter().enumerate() {
        let loot = f.outside.join(format!("escaped-{n}"));
        let id = format!("ai.ligus.sandbox-probe.{}.{n}.{}", std::process::id(), f.outside.display().to_string().len());
        let app = f.worktree.join(format!("Probe{n}.app"));
        // If launchd does start the probe it outlives this test (often
        // stopped, never exiting): reap it on every exit path, panics included.
        let reaper = ProbeReaper(app.clone());
        let script = format!(
            "mkdir -p '{app}/Contents/MacOS' && \
             printf '%s' '<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleExecutable</key><string>run</string><key>CFBundleIdentifier</key><string>{id}</string><key>{key}</key><true/></dict></plist>' > '{app}/Contents/Info.plist' && \
             printf '#!/bin/sh\\ncat \"%s\" > \"%s\"\\n' '{secret}' '{loot}' > '{app}/Contents/MacOS/run' && \
             chmod +x '{app}/Contents/MacOS/run'; {open} '{app}'; sleep 1; \
             for p in $(/usr/bin/pgrep -f '{app}/Contents/MacOS'); do kill -CONT $p; done; true",
            app = app.display(),
            secret = f.data.join("settings.json").display(),
            loot = loot.display(),
        );
        let (_ok, out) = run_sh(&*e, &f, &script, isolated());
        assert!(app.join("Contents/MacOS/run").exists(), "the probe bundle was written: {out}");
        let started = reaper.watch(std::time::Duration::from_secs(3), &loot);
        let leaked = std::fs::read_to_string(&loot).unwrap_or_default();
        assert!(!leaked.contains("SETTINGS-SECRET") && !loot.exists(), "an app launched via LaunchServices ran outside the sandbox ({open}): {out}");
        assert!(started.is_empty(), "LaunchServices started the probe outside the sandbox ({open}, pids {started:?}): {out}");
    }
}

/// macOS: an XPC service embedded in a bundle the agent wrote is spawned by
/// launchd, not by the agent, so it would run outside the profile. The
/// probe (compiled here, outside the sandbox, and run inside it) connects to
/// its own bundled service; the service must never start.
#[cfg(target_os = "macos")]
#[test]
fn sandbox_denies_bundled_xpc_service_escape() {
    let Some(e) = real_executor() else { return };
    if !Path::new("/usr/bin/clang").exists() || !Path::new("/usr/bin/codesign").exists() {
        eprintln!("skipped: clang and codesign are needed to build the XPC probe");
        return;
    }
    let f = Fixture::new();
    let loot = f.outside.join("xpc-escaped");
    let app = f.worktree.join("XpcProbe.app");
    let reaper = ProbeReaper(app.clone());
    let name = format!("ai.ligus.sandbox-xpc-probe.{}", std::process::id());
    let svc = app.join(format!("Contents/XPCServices/{name}.xpc"));
    std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    std::fs::create_dir_all(svc.join("Contents/MacOS")).unwrap();
    let src = f.outside.join("xpc-src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("main.c"),
        format!(
            "#include <xpc/xpc.h>\n#include <stdio.h>\nint main(void){{xpc_connection_t c=xpc_connection_create(\"{name}\",NULL);\
             xpc_connection_set_event_handler(c,^(xpc_object_t e){{(void)e;}});xpc_connection_resume(c);\
             xpc_object_t r=xpc_connection_send_message_with_reply_sync(c,xpc_dictionary_create(NULL,NULL,0));\
             printf(\"reply:%s\\n\",xpc_copy_description(r));return 0;}}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        src.join("svc.c"),
        "#include <stdio.h>\nint main(int c,char**v){(void)c;(void)v;FILE*s=fopen(SECRET,\"r\"),*o=fopen(LOOT,\"w\");\
         if(o){int ch;fputs(\"svc-ran\\n\",o);while(s&&(ch=fgetc(s))!=EOF)fputc(ch,o);fclose(o);}return 0;}\n",
    )
    .unwrap();
    let clang = |out: &Path, file: &str, defs: &[String]| {
        let st = std::process::Command::new("/usr/bin/clang").args(defs).arg("-o").arg(out).arg(src.join(file)).status().unwrap();
        assert!(st.success(), "clang {file}");
    };
    clang(&app.join("Contents/MacOS/main"), "main.c", &[]);
    clang(
        &svc.join("Contents/MacOS/svc"),
        "svc.c",
        &[format!("-DSECRET=\"{}\"", f.data.join("settings.json").display()), format!("-DLOOT=\"{}\"", loot.display())],
    );
    let plist = |exe: &str, id: &str, kind: &str, extra: &str| {
        format!("<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleExecutable</key><string>{exe}</string><key>CFBundleIdentifier</key><string>{id}</string><key>CFBundlePackageType</key><string>{kind}</string>{extra}</dict></plist>")
    };
    std::fs::write(app.join("Contents/Info.plist"), plist("main", &format!("{name}.app"), "APPL", "")).unwrap();
    std::fs::write(svc.join("Contents/Info.plist"), plist("svc", &name, "XPC!", "<key>XPCService</key><dict><key>ServiceType</key><string>Application</string></dict>")).unwrap();
    for b in [&svc, &app] {
        let st = std::process::Command::new("/usr/bin/codesign").args(["-s", "-", "--force"]).arg(b).stderr(std::process::Stdio::null()).status().unwrap();
        assert!(st.success(), "codesign {}", b.display());
    }
    let (_ok, out) = run_sh(&*e, &f, &format!("'{}'", app.join("Contents/MacOS/main").display()), isolated());
    assert!(out.contains("reply:"), "the probe ran: {out}");
    let started = reaper.watch(std::time::Duration::from_secs(2), &loot);
    assert!(!loot.exists(), "a bundled XPC service ran outside the sandbox: {out}");
    assert!(started.is_empty(), "launchd started the bundled XPC service (pids {started:?}): {out}");
}

/// macOS: other ways a confined turn could get work done outside its
/// profile. Signals to processes outside the sandbox (resuming a suspended
/// launch, or killing the user's processes), preference writes through
/// cfprefsd (which writes `~/Library/Preferences` on the agent's behalf),
/// Shortcuts (run by a helper outside the profile) and launchd job
/// submission. Each is checked for denial; a regression that let one through
/// is cleaned up (the preference domain and the launchd job are removed).
#[cfg(target_os = "macos")]
#[test]
fn sandbox_denies_out_of_profile_helpers() {
    let Some(e) = real_executor() else { return };
    let f = Fixture::new();
    // Signals: the test process is outside the sandbox.
    let me = std::process::id();
    let (_, out) = run_sh(&*e, &f, &format!("kill -0 {me} && echo SIGNALED"), isolated());
    assert!(!out.contains("SIGNALED"), "signaled a process outside the sandbox: {out}");
    // ...while the turn's own children can still be signaled.
    let (ok, out) = run_sh(&*e, &f, "sleep 5 & kill $! && wait $!; echo rc=$?", isolated());
    assert!(ok && out.contains("rc=143"), "couldn't signal its own child: {out}");
    // Preference writes through cfprefsd.
    let domain = format!("ai.ligus.sandbox-prefs-probe.{me}");
    struct PrefGuard(String);
    impl Drop for PrefGuard {
        fn drop(&mut self) {
            let _ = std::process::Command::new("/usr/bin/defaults").args(["delete", &self.0]).stderr(std::process::Stdio::null()).status();
            if let Some(home) = std::env::var_os("HOME") {
                let _ = std::fs::remove_file(PathBuf::from(home).join(format!("Library/Preferences/{}.plist", self.0)));
            }
        }
    }
    let _pref = PrefGuard(domain.clone());
    let (_, out) = run_sh(&*e, &f, &format!("/usr/bin/defaults write {domain} k v 2>&1; /usr/bin/defaults read {domain} k 2>&1"), isolated());
    let read = std::process::Command::new("/usr/bin/defaults").args(["read", &domain, "k"]).output().unwrap();
    assert!(!read.status.success(), "cfprefsd wrote a preference for the agent: {out}");
    // Preference reads still work (CLIs read their defaults).
    let (ok, out) = run_sh(&*e, &f, "/usr/bin/defaults domains >/dev/null && echo READ-OK", isolated());
    assert!(ok && out.contains("READ-OK"), "preference reads broke: {out}");
    // Shortcuts: listing needs the same service running one does.
    if Path::new("/usr/bin/shortcuts").exists() {
        let (ok, out) = run_sh(&*e, &f, "/usr/bin/shortcuts list >/dev/null 2>&1 && echo REACHED", isolated());
        assert!(!ok || !out.contains("REACHED"), "Shortcuts was reachable: {out}");
    }
    // launchd job submission (launchd itself refuses sandboxed callers).
    let label = format!("ai.ligus.sandbox-launchd-probe.{me}");
    struct JobGuard(String);
    impl Drop for JobGuard {
        fn drop(&mut self) {
            let _ = std::process::Command::new("/bin/launchctl").args(["remove", &self.0]).stderr(std::process::Stdio::null()).status();
        }
    }
    let _job = JobGuard(label.clone());
    let loot = f.outside.join("launchd-escaped");
    let (_, out) = run_sh(&*e, &f, &format!("/bin/launchctl submit -l {label} -- /bin/sh -c 'echo x > \"{}\"'", loot.display()), isolated());
    std::thread::sleep(std::time::Duration::from_millis(500));
    let listed = std::process::Command::new("/bin/launchctl").args(["list", &label]).output().unwrap();
    assert!(!listed.status.success() && !loot.exists(), "launchd accepted a job from the sandbox: {out}");
}

/// Run `script` under `purpose` in the fixture's worktree (`provider_key`:
/// the turn's configured API key, as a hosted-key turn has).
#[cfg(target_os = "macos")]
fn run_purpose(e: &dyn TurnExecutor, f: &Fixture, purpose: Purpose, script: &str, provider_key: Option<&str>) -> (bool, String) {
    with_turn_sync(isolated(), || {
        let mut spec = SpawnSpec::new(purpose, "/bin/sh", ["-c", script]).cwd(Some(&f.worktree));
        spec.provider_key = provider_key;
        let mut p = prepare_with(e, spec).expect("prepare");
        let out = p.command.output().expect("spawn");
        drop(p.guard);
        (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    })
}

/// macOS: an agent whose sign-in doesn't live in the login keychain can't
/// search it. Items stored with `/usr/bin/security` in their ACL (the gh
/// token, git's osxkeychain credentials, other CLIs' logins) are readable
/// without a prompt by anything that can run `security`, so a turn that
/// reaches the keychain reads them although `~/.config/gh` is denied. The
/// probe only opens the keychain's settings, never an item.
#[cfg(target_os = "macos")]
#[test]
fn sandbox_denies_login_keychain_search() {
    let Some(e) = real_executor() else { return };
    let f = Fixture::new();
    let Some(home) = home_dir() else { return };
    let login = home.join("Library/Keychains/login.keychain-db");
    // `show-keychain-info` opens the keychain (the client reads the file);
    // it doesn't unlock it or read any item.
    let probe = format!("/usr/bin/security show-keychain-info '{}' 2>&1; echo rc=$?", login.display());
    let reached = |out: &str| out.contains("no-timeout") || out.contains("lock-on-sleep") || out.contains("rc=0");
    // Control: outside the sandbox the login keychain opens.
    let control = std::process::Command::new("/bin/sh").args(["-c", &probe]).output().unwrap();
    let control = String::from_utf8_lossy(&control.stdout).into_owned();
    if !reached(&control) {
        eprintln!("skipped: no login keychain to open ({control})");
        return;
    }
    // A hosted-key turn (no subscription sign-in) and a tool child never
    // need the keychain.
    for (purpose, key) in [(Purpose::Agent(ModelProviderKind::ClaudeCode), Some("sk-ant-test")), (Purpose::Agent(ModelProviderKind::Codex), Some("sk-test")), (Purpose::Tool, None), (Purpose::McpServer, None)] {
        let (_, out) = run_purpose(&*e, &f, purpose, &probe, key);
        assert!(!reached(&out), "{purpose:?} opened the login keychain: {out}");
    }
    // A Claude Code subscription sign-in is bridged into the turn home as
    // its credentials file, and the turn then runs without the keychain.
    let has_item = std::process::Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", "Claude Code-credentials"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if has_item && std::env::var_os("CLAUDE_CONFIG_DIR").is_none() && !home.join(".claude/.credentials.json").exists() {
        let (_, out) = run_purpose(&*e, &f, Purpose::Agent(ModelProviderKind::ClaudeCode), &format!("test -s \"$HOME/.claude/.credentials.json\" && echo BRIDGED; {probe}"), None);
        assert!(out.contains("BRIDGED") && !reached(&out), "the subscription sign-in wasn't bridged, or the keychain stayed open: {out}");
    }
}

/// macOS: under open egress (a stdio MCP server) a confined child can't
/// connect to the user's unix sockets. A Docker or OrbStack socket is root
/// on the VM with the user's home mounted (a full escape), and ssh-agent or
/// gpg-agent sign with the user's keys.
#[cfg(target_os = "macos")]
#[test]
fn sandbox_denies_unix_sockets_under_open_egress() {
    let Some(e) = real_executor() else { return };
    let f = Fixture::new();
    let dir = tempfile::Builder::new().prefix("lsk").tempdir_in("/tmp").unwrap();
    let path = dir.path().join("svc.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let probe = format!(
        "/usr/bin/python3 -I -c 'import socket,sys\ns=socket.socket(socket.AF_UNIX)\ns.connect(sys.argv[1])\nprint(\"CONNECTED\")' '{}'",
        canonical(&path).display()
    );
    let plan = EnvelopeEnforcer::plan(&PlanInputs {
        host: HostKind::Desktop,
        envelope: &SandboxEnvelope::workspace_default(),
        purpose: Purpose::McpServer,
        gate: &GatePlan::Full,
        worktree: Path::new("/repo"),
        worktree_is_isolated: true,
        write_capable: false,
        tmp: Path::new("/tmp/t"),
        home: Path::new("/home/u"),
        host_paths: &HostPaths::default(),
        mcp_hosts: &[],
        mcp_ids: &[],
        model_base_url: None,
        extra_env: &[],
        provider_key: None,
        model_provider_id: None,
        envelope_args: &[],
        ligus_mcp_socket: None,
        egress_presets: &[],
        cli_home: None,
    });
    assert_eq!(plan.egress, EgressPlan::Open, "the probe needs an open-egress child");
    let (_, out) = run_purpose(&*e, &f, Purpose::McpServer, &probe, None);
    assert!(!out.contains("CONNECTED"), "an open-egress child reached a host unix socket: {out}");
    assert!(listener.accept().is_err(), "the host socket saw a connection");
    // Name resolution (mDNSResponder's socket) still works.
    let (ok, out) = run_purpose(&*e, &f, Purpose::McpServer, "/usr/bin/python3 -I -c 'import socket; socket.getaddrinfo(\"localhost\", 80); print(\"RESOLVED\")'", None);
    assert!(ok && out.contains("RESOLVED"), "{out}");
}

/// macOS: mach-lookup is default-deny with an allow-list. The default-allow
/// profile left hundreds of launchd services reachable (the Dock, login
/// items, the window server, TCC, ...), each one a way to get work done
/// outside the profile. A probe (compiled here, run inside) looks services
/// up by name: the ones a CLI needs answer, the rest don't.
#[cfg(target_os = "macos")]
#[test]
fn sandbox_mach_lookup_is_allow_listed() {
    let Some(e) = real_executor() else { return };
    if !Path::new("/usr/bin/clang").exists() {
        eprintln!("skipped: clang is needed to build the mach probe");
        return;
    }
    let f = Fixture::new();
    let src = f.outside.join("mp.c");
    std::fs::write(
        &src,
        "#include <servers/bootstrap.h>\n#include <stdio.h>\nint main(int c,char**v){for(int i=1;i<c;i++){mach_port_t p=MACH_PORT_NULL;\
         printf(\"%s %s\\n\",bootstrap_look_up(bootstrap_port,v[i],&p)==KERN_SUCCESS?\"OPEN\":\"SHUT\",v[i]);}return 0;}\n",
    )
    .unwrap();
    let exe = f.worktree.join("mp");
    let st = std::process::Command::new("/usr/bin/clang").arg("-o").arg(&exe).arg(&src).status().unwrap();
    assert!(st.success(), "clang mp.c");
    let shut = [
        "com.apple.windowserver.active",
        "com.apple.tccd",
        "com.apple.backgroundtaskmanagementagent",
        "com.apple.dock.server",
        "com.apple.replayd",
        "com.apple.SecurityServer",
    ];
    let open = ["com.apple.system.opendirectoryd.libinfo", "com.apple.trustd", "com.apple.system.notification_center", "com.apple.cfprefsd.agent"];
    // Control: all of them answer outside the sandbox.
    let control = std::process::Command::new(&exe).args(shut).args(open).output().unwrap();
    let control = String::from_utf8_lossy(&control.stdout).into_owned();
    let names: Vec<&str> = shut.iter().chain(open.iter()).copied().filter(|n| control.contains(&format!("OPEN {n}\n"))).collect();
    let (_, out) = run_purpose(&*e, &f, Purpose::Agent(ModelProviderKind::ClaudeCode), &format!("'{}' {}", exe.display(), names.join(" ")), Some("sk-ant-test"));
    for n in names {
        let want = if shut.contains(&n) { "SHUT" } else { "OPEN" };
        assert!(out.contains(&format!("{want} {n}\n")), "{n} should be {want}: {out}");
    }
    // User lookups (libinfo) work: `whoami` prints a name, not a uid.
    let (ok, out) = run_purpose(&*e, &f, Purpose::Tool, "/usr/bin/whoami", None);
    assert!(ok && out.trim().parse::<u32>().is_err(), "{out}");
}

/// Kills whatever runs out of a probe app bundle (matched by its unique
/// path) and removes the bundle, so a LaunchServices or XPC launch can't
/// leak a process past the test. Bounded: gives up after a few seconds.
#[cfg(target_os = "macos")]
struct ProbeReaper(PathBuf);

#[cfg(target_os = "macos")]
impl ProbeReaper {
    fn pids(&self) -> Vec<i32> {
        let Ok(out) = std::process::Command::new("/usr/bin/pgrep").arg("-f").arg(self.0.join("Contents/").as_os_str()).output() else {
            return Vec::new();
        };
        String::from_utf8_lossy(&out.stdout).split_whitespace().filter_map(|p| p.parse().ok()).collect()
    }

    /// Every pid seen running from the bundle over `for` (stopping early
    /// once `loot` appears). Call it after the sandboxed run returned, so
    /// the only matches are processes something else started.
    fn watch(&self, r#for: std::time::Duration, loot: &Path) -> Vec<i32> {
        let mut seen = Vec::new();
        let deadline = std::time::Instant::now() + r#for;
        while std::time::Instant::now() < deadline {
            for p in self.pids() {
                if !seen.contains(&p) {
                    seen.push(p);
                }
            }
            if loot.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        seen
    }
}

#[cfg(target_os = "macos")]
impl Drop for ProbeReaper {
    fn drop(&mut self) {
        // Unregister and remove the bundle first, so a launch still in
        // flight has nothing to exec and LaunchServices keeps no record.
        let _ = std::process::Command::new("/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister")
            .arg("-u")
            .arg(&self.0)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let _ = std::fs::remove_dir_all(&self.0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        // Keep sweeping briefly after the last kill: launchd may start it late.
        let mut quiet_since = std::time::Instant::now();
        while std::time::Instant::now() < deadline {
            let pids = self.pids();
            if pids.is_empty() {
                if quiet_since.elapsed() >= std::time::Duration::from_millis(500) {
                    break;
                }
            } else {
                for pid in pids {
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
                quiet_since = std::time::Instant::now();
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

/// A confined child runs in a session of its own, without the parent's
/// controlling terminal: with it, an agent could open `/dev/tty` and push
/// keystrokes into the user's shell with `TIOCSTI` (Seatbelt allows tty
/// writes), and they would run unconfined once `ligus agent`/`serve`
/// returns to the prompt.
#[cfg(target_os = "macos")]
#[test]
fn confined_child_runs_in_its_own_session() {
    let Some(e) = real_executor() else { return };
    if !Path::new("/usr/bin/python3").exists() {
        eprintln!("skipped: no python3");
        return;
    }
    let f = Fixture::new();
    let (ok, out) = run_sh(&*e, &f, "/usr/bin/python3 -I -c 'import os; print(os.getsid(0), os.getpid())'", isolated());
    assert!(ok, "{out}");
    let mine = unsafe { libc::getsid(0) };
    let child_sid: i32 = out.split_whitespace().next().and_then(|s| s.parse().ok()).expect("sid");
    assert_ne!(child_sid, mine, "the agent shares the parent's session (and its controlling terminal)");
}

/// C-13: a confined child can't drive other apps with Apple Events, or read
/// the pasteboard; ordinary commands still run.
#[cfg(target_os = "macos")]
#[test]
fn sandbox_denies_apple_events_and_pasteboard() {
    let Some(e) = real_executor() else { return };
    let f = Fixture::new();
    // `count windows` needs a real event to Finder (an application's `name`
    // is answered locally by AppleScript).
    let (ok, out) = run_sh(&*e, &f, "/usr/bin/osascript -e 'tell application id \"com.apple.finder\" to count windows' 2>&1", isolated());
    assert!(!ok || out.trim().parse::<i64>().is_err(), "Apple Events reached Finder: {out}");
    let (ok, out) = run_sh(&*e, &f, "/usr/bin/pbpaste >/dev/null 2>&1 && echo READ", isolated());
    assert!(!ok || !out.contains("READ"), "the pasteboard was readable: {out}");
    let (ok, out) = run_sh(&*e, &f, "echo still-fine", isolated());
    assert!(ok && out.contains("still-fine"), "{out}");
    let prof = seatbelt::profile(&EnvelopeEnforcer::plan(&PlanInputs {
        host: HostKind::Desktop,
        envelope: &SandboxEnvelope::workspace_default(),
        purpose: Purpose::Agent(ModelProviderKind::ClaudeCode),
        gate: &GatePlan::Full,
        worktree: Path::new("/repo"),
        worktree_is_isolated: true,
        write_capable: true,
        tmp: Path::new("/tmp/t"),
        home: Path::new("/home/u"),
        host_paths: &HostPaths::default(),
        mcp_hosts: &[],
        mcp_ids: &[],
        model_base_url: None,
        extra_env: &[],
        provider_key: None,
        model_provider_id: None,
        envelope_args: &[],
        ligus_mcp_socket: None,
        egress_presets: &[],
        cli_home: None,
    }), None);
    assert!(prof.contains("(deny appleevent-send)") && prof.contains("com.apple.pasteboard"));
}

/// `ligus_server_present_under_closed_egress`: the Ligus MCP server holds
/// its slot (first) under every egress policy, `Closed` included, and its
/// socket is the profile's one allowed unix-socket connection.
#[test]
fn ligus_server_present_under_closed_egress() {
    use hive_core::sandbox::{EgressPolicy, TightenedBy};
    let sock = Path::new("/tmp/ligus-mcp-x/s");
    for egress in [EgressPolicy::Closed, EgressPolicy::AllowList { mcp_servers: vec![] }, EgressPolicy::Open] {
        let env = SandboxEnvelope::workspace_default().tighten(TightenedBy::Lockdown, egress.clone(), false);
        let plan = EnvelopeEnforcer::plan(&PlanInputs {
            host: HostKind::Worker,
            envelope: &env,
            purpose: Purpose::Agent(ModelProviderKind::Copilot),
            gate: &GatePlan::ReadOnly,
            worktree: Path::new("/repo"),
            worktree_is_isolated: false,
            write_capable: false,
            tmp: Path::new("/tmp/t"),
            home: Path::new("/home/u"),
            host_paths: &HostPaths::default(),
            mcp_hosts: &[],
            mcp_ids: &["docs".to_string()],
            model_base_url: None,
            extra_env: &[],
            provider_key: None,
            model_provider_id: None,
            envelope_args: &[],
            ligus_mcp_socket: Some(sock),
            egress_presets: &[],
            cli_home: None,
        });
        assert_eq!(plan.mcp.first().map(String::as_str), Some(LIGUS_MCP_ID), "{egress:?}");
        assert_eq!(plan.ligus_mcp_socket.as_deref(), Some(sock));
        assert!(seatbelt::profile(&plan, Some(1)).contains("unix-socket (path-literal"), "{egress:?}");
    }
}

/// B2 / C-11: a subscription-CLI agent keeps Keychain service access (its
/// login lives in the login keychain), but Ligus's own secret store sits in
/// the data dir, which the sandbox denies: neither the owner-only files nor
/// Ligus's keychain file can be read. With `LIGUS_TEST_KEYCHAIN=1` the real
/// keychain backend is exercised (a throwaway keychain and service name,
/// removed afterwards): `security` inside the sandbox can't open it.
#[test]
fn ligus_secrets_unreachable_by_subscription_cli_agent() {
    use crate::secret_store::{File0600, SecretId, SecretStore};
    let Some(e) = real_executor() else { return };
    let f = Fixture::new();
    File0600::new(&f.data).put(&SecretId::new("settings", "api_key"), b"STORE-SECRET").unwrap();
    std::fs::write(f.data.join("secrets/ligus.keychain-db"), "KEYCHAIN-FILE").unwrap();
    for target in ["secrets/settings/api_key", "secrets/ligus.keychain-db"] {
        let (ok, out) = run_sh(&*e, &f, &format!("cat '{}'", f.data.join(target).display()), isolated());
        assert!(!ok && !out.contains("SECRET") && !out.contains("KEYCHAIN-FILE"), "read {target}: {out}");
    }
    #[cfg(target_os = "macos")]
    if std::env::var("LIGUS_TEST_KEYCHAIN").ok().as_deref() == Some("1") {
        let service = format!("ai.ligus.secrets.test-{}", uuid::Uuid::new_v4());
        let path = f.data.join("secrets/ligus-sandbox-test.keychain-db");
        let kc = crate::secret_store::MacKeychain::open_with(path.clone(), &service).unwrap();
        kc.put(&SecretId::new("settings", "probe"), b"REAL-KEYCHAIN-SECRET").unwrap();
        let script = format!(
            "/usr/bin/security find-generic-password -s '{service}' -a settings:probe -w '{}'",
            path.display()
        );
        let (ok, out) = run_sh(&*e, &f, &script, isolated());
        let cleaned = kc.destroy();
        assert!(!ok && !out.contains("REAL-KEYCHAIN-SECRET"), "the agent read Ligus's keychain: {out}");
        cleaned.unwrap();
    }
}

/// B2: the Secret Service is a D-Bus session service; an agent child never
/// gets the session bus address (it isn't on the env allow-list), and the
/// bubblewrap profile doesn't bind the bus socket.
#[test]
fn agent_env_has_no_session_bus() {
    let host = host_fixture();
    let envelope = SandboxEnvelope::workspace_default();
    let plan = EnvelopeEnforcer::plan(&PlanInputs {
        host: HostKind::Desktop,
        envelope: &envelope,
        purpose: Purpose::Agent(ModelProviderKind::ClaudeCode),
        gate: &GatePlan::Full,
        worktree: Path::new("/repo/.ligus/worktrees/t1"),
        worktree_is_isolated: true,
        write_capable: true,
        tmp: Path::new("/tmp/ligus-turn-1"),
        home: Path::new("/home/u"),
        host_paths: &host,
        mcp_hosts: &[],
        mcp_ids: &[],
        model_base_url: None,
        extra_env: &[("DBUS_SESSION_BUS_ADDRESS".into(), "unix:path=/run/user/1/bus".into())],
        provider_key: None,
        model_provider_id: None,
        envelope_args: &[],
        ligus_mcp_socket: None,
        egress_presets: &[],
        cli_home: None,
    });
    assert!(plan.env.iter().all(|(k, _)| !k.to_string_lossy().starts_with("DBUS")), "{:?}", plan.env);
}

// ---------------------------------------------------------------------------
// Egress presets
// ---------------------------------------------------------------------------

fn preset_plan(envelope: &SandboxEnvelope, purpose: Purpose, host: &HostPaths, presets: &[EgressPreset]) -> EnforcementPlan {
    EnvelopeEnforcer::plan(&PlanInputs {
        host: HostKind::Desktop,
        envelope,
        purpose,
        gate: &GatePlan::Full,
        worktree: Path::new("/repo"),
        worktree_is_isolated: true,
        write_capable: true,
        tmp: Path::new("/tmp/t"),
        home: Path::new("/home/u"),
        host_paths: host,
        mcp_hosts: &[],
        mcp_ids: &[],
        model_base_url: None,
        extra_env: &[],
        provider_key: None,
        model_provider_id: None,
        envelope_args: &[],
        ligus_mcp_socket: None,
        egress_presets: presets,
        cli_home: None,
    })
}

fn allow_of(p: &EnforcementPlan) -> Vec<HostPort> {
    match &p.egress {
        EgressPlan::Proxy { allow } => allow.clone(),
        _ => Vec::new(),
    }
}

const REGISTRY_HOSTS: &[&str] = &[
    "registry.npmjs.org",
    "crates.io",
    "static.crates.io",
    "index.crates.io",
    "pypi.org",
    "files.pythonhosted.org",
    "github.com",
    "api.github.com",
    "codeload.github.com",
    "objects.githubusercontent.com",
    "raw.githubusercontent.com",
];

/// The preset is off by default: no turn policy, host or plan carries it
/// unless something opts in, and an open workspace's agent reaches only its
/// provider.
#[test]
fn egress_preset_off_by_default() {
    assert!(TurnPolicy::baseline(HostKind::Desktop).egress_presets.is_empty());
    assert!(TurnPolicy::new(HostKind::Worker, SandboxEnvelope::workspace_default(), GatePlan::Full).egress_presets.is_empty());
    assert!(HostPaths::default().egress_presets.is_empty());
    let open = SandboxEnvelope::workspace_default();
    let agent = preset_plan(&open, Purpose::Agent(ModelProviderKind::ClaudeCode), &HostPaths::default(), &[]);
    let allow = allow_of(&agent);
    for h in REGISTRY_HOSTS {
        // github.com is not a Claude Code endpoint; nothing from the preset leaks in.
        assert!(!allow.contains(&HostPort::new(*h, 443)), "{h} allowed without the preset");
    }
    // run_command and pre-flight stay offline.
    for p in [Purpose::Tool, Purpose::Preflight] {
        assert_eq!(preset_plan(&open, p, &HostPaths::default(), &[]).egress, EgressPlan::Closed);
    }
}

#[test]
fn egress_preset_names_parse() {
    assert_eq!(EgressPreset::parse("package-registries"), Some(EgressPreset::PackageRegistries));
    assert_eq!(EgressPreset::parse(" Package_Registries "), Some(EgressPreset::PackageRegistries));
    assert_eq!(EgressPreset::parse("everything"), None);
    assert_eq!(EgressPreset::parse_list(["package-registries", "nope", "package-registries"]), vec![EgressPreset::PackageRegistries]);
    let hosts: Vec<String> = EgressPreset::PackageRegistries.hosts().into_iter().map(|h| h.to_string()).collect();
    assert_eq!(hosts, REGISTRY_HOSTS.iter().map(|h| format!("{h}:443")).collect::<Vec<_>>());
}

/// Enabling the preset adds exactly its hosts (port 443) to the plan, and
/// nothing else; it works from the chat/workspace setting and from the
/// host's `LIGUS_SANDBOX_EGRESS_PRESETS` alike.
#[test]
fn egress_preset_adds_exactly_its_hosts() {
    let open = SandboxEnvelope::workspace_default();
    let preset = [EgressPreset::PackageRegistries];
    let want: std::collections::BTreeSet<HostPort> = REGISTRY_HOSTS.iter().map(|h| HostPort::new(*h, 443)).collect();
    for kind in [ModelProviderKind::ClaudeCode, ModelProviderKind::Codex, ModelProviderKind::Aider] {
        let base: std::collections::BTreeSet<HostPort> = allow_of(&preset_plan(&open, Purpose::Agent(kind), &HostPaths::default(), &[])).into_iter().collect();
        let with: std::collections::BTreeSet<HostPort> = allow_of(&preset_plan(&open, Purpose::Agent(kind), &HostPaths::default(), &preset)).into_iter().collect();
        let expected: std::collections::BTreeSet<HostPort> = base.union(&want).cloned().collect();
        assert_eq!(with, expected, "{kind:?}");
        let host = HostPaths { egress_presets: preset.to_vec(), ..Default::default() };
        let via_env: std::collections::BTreeSet<HostPort> = allow_of(&preset_plan(&open, Purpose::Agent(kind), &host, &[])).into_iter().collect();
        assert_eq!(via_env, expected, "{kind:?} via the host setting");
    }
    for p in [Purpose::Tool, Purpose::Preflight] {
        let plan = preset_plan(&open, p, &HostPaths::default(), &preset);
        let got: std::collections::BTreeSet<HostPort> = allow_of(&plan).into_iter().collect();
        assert_eq!(got, want, "{p:?} gets exactly the preset");
    }
    // A stdio MCP server's egress isn't the agent's allow-list; unchanged.
    assert_eq!(preset_plan(&open, Purpose::McpServer, &HostPaths::default(), &preset).egress, EgressPlan::Open);
}

/// Lockdown and dearth mode override the preset: a chat whose egress a
/// guard restricted never gets the registries, whoever opted in.
#[test]
fn egress_preset_ignored_under_lockdown_and_dearth() {
    let preset = [EgressPreset::PackageRegistries];
    let host = HostPaths { egress_presets: preset.to_vec(), ..Default::default() };
    let base = SandboxEnvelope::workspace_default();
    let lockdown = base.clone().tighten(TightenedBy::Lockdown, EgressPolicy::Closed, true);
    let dearth = base.clone().tighten(TightenedBy::Dearth, EgressPolicy::AllowList { mcp_servers: vec![] }, false);
    let both = dearth.clone().tighten(TightenedBy::Lockdown, EgressPolicy::Closed, true);
    for env in [&lockdown, &dearth, &both] {
        for h in [&HostPaths::default(), &host] {
            let agent = preset_plan(env, Purpose::Agent(ModelProviderKind::ClaudeCode), h, &preset);
            assert_eq!(
                allow_of(&agent).into_iter().collect::<std::collections::BTreeSet<_>>(),
                provider_endpoints(ModelProviderKind::ClaudeCode).into_iter().collect(),
                "{:?}: the agent keeps only its provider",
                env.tightened_by
            );
            for p in [Purpose::Tool, Purpose::Preflight] {
                assert_eq!(preset_plan(env, p, h, &preset).egress, EgressPlan::Closed, "{:?} {p:?}", env.tightened_by);
            }
        }
    }
}

async fn connect_via(p: &EgressProxy, target: &str) -> (tokio::net::TcpStream, String) {
    use base64::Engine;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", p.port())).await.unwrap();
    let cred = base64::engine::general_purpose::STANDARD.encode(format!("ligus:{}", p.token()));
    s.write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\nProxy-Authorization: Basic {cred}\r\n\r\n").as_bytes()).await.unwrap();
    let mut buf = vec![0u8; 256];
    let n = s.read(&mut buf).await.unwrap();
    (s, String::from_utf8_lossy(&buf[..n]).into_owned())
}

/// The per-turn proxy started from a plan with the preset lets each preset
/// host through (on 443 only), refuses everything else, and still pins the
/// TLS server name: a ClientHello for another host is cut. No network is
/// needed: the allow decision and the SNI pin come before any upstream dial.
#[tokio::test]
async fn egress_preset_hosts_pass_the_proxy_with_sni_pinning() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let open = SandboxEnvelope::workspace_default();
    let plan = preset_plan(&open, Purpose::Tool, &HostPaths::default(), &[EgressPreset::PackageRegistries]);
    let proxy = EgressProxy::start(allow_of(&plan), None).unwrap();
    for h in REGISTRY_HOSTS {
        let (_s, resp) = connect_via(&proxy, &format!("{h}:443")).await;
        assert!(resp.starts_with("HTTP/1.1 200"), "{h}: {resp}");
    }
    for refused in ["example.com:443", "registry.npmjs.org:80", "evil.github.com:443", "gist.github.com:443", "registry.yarnpkg.com:443"] {
        let (_s, resp) = connect_via(&proxy, refused).await;
        assert!(resp.starts_with("HTTP/1.1 403"), "{refused}: {resp}");
    }
    // SNI pinning: CONNECT to an allowed registry, then a ClientHello for
    // another server, is cut.
    let (mut s, resp) = connect_via(&proxy, "registry.npmjs.org:443").await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    s.write_all(&crate::egress::tests::client_hello(Some("evil.example"))).await.unwrap();
    let mut b = [0u8; 16];
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), s.read(&mut b)).await.unwrap().unwrap_or(0);
    assert_eq!(n, 0, "a mismatched SNI is cut");
    let denied = proxy.denials();
    assert!(denied.contains(&HostPort::new("registry.npmjs.org", 443)) && denied.contains(&HostPort::new("example.com", 443)), "{denied:?}");
}

/// `claude-connectors` adds only `mcp-proxy.anthropic.com:443`, only to
/// Claude Code turns, never to tools or other runtimes; it's off by default,
/// ignored under lockdown/dearth, and telemetry hosts stay off the list.
#[test]
fn claude_connectors_preset_is_claude_code_only() {
    let open = SandboxEnvelope::workspace_default();
    let preset = [EgressPreset::ClaudeConnectors];
    let mcp_proxy = HostPort::new("mcp-proxy.anthropic.com", 443);
    let cc = Purpose::Agent(ModelProviderKind::ClaudeCode);
    assert!(!allow_of(&preset_plan(&open, cc, &HostPaths::default(), &[])).contains(&mcp_proxy), "off by default");
    let base: std::collections::BTreeSet<HostPort> = allow_of(&preset_plan(&open, cc, &HostPaths::default(), &[])).into_iter().collect();
    let with: std::collections::BTreeSet<HostPort> = allow_of(&preset_plan(&open, cc, &HostPaths::default(), &preset)).into_iter().collect();
    assert_eq!(with.difference(&base).cloned().collect::<Vec<_>>(), vec![mcp_proxy.clone()]);
    for h in &with {
        assert!(!h.host.contains("statsig") || base.contains(h), "the preset adds no telemetry host");
    }
    assert!(!with.iter().any(|h| h.host.contains("sentry") || h.host.contains("datadog")));
    for other in [ModelProviderKind::Codex, ModelProviderKind::Copilot, ModelProviderKind::Aider] {
        assert!(!allow_of(&preset_plan(&open, Purpose::Agent(other), &HostPaths::default(), &preset)).contains(&mcp_proxy), "{other:?}");
    }
    for p in [Purpose::Tool, Purpose::Preflight] {
        assert_eq!(preset_plan(&open, p, &HostPaths::default(), &preset).egress, EgressPlan::Closed, "{p:?}");
    }
    let lockdown = open.clone().tighten(TightenedBy::Lockdown, EgressPolicy::Closed, true);
    let dearth = open.clone().tighten(TightenedBy::Dearth, EgressPolicy::AllowList { mcp_servers: vec![] }, false);
    for env in [&lockdown, &dearth] {
        assert!(!allow_of(&preset_plan(env, cc, &HostPaths::default(), &preset)).contains(&mcp_proxy), "{:?}", env.tightened_by);
    }
    assert_eq!(EgressPreset::parse("claude-connectors"), Some(EgressPreset::ClaudeConnectors));
}

/// Claude Code's Bash tool mkdirs `/tmp/claude-<uid>`, which the sandbox
/// refuses: its plan points `CLAUDE_CODE_TMPDIR` at the turn's scratch dir,
/// with or without a per-turn home, and the variable reaches the child
/// through the executor (Seatbelt or bwrap) unchanged.
#[test]
fn claude_code_tmpdir_is_the_turn_scratch_dir() {
    let open = SandboxEnvelope::workspace_default();
    let get = |p: &EnforcementPlan| p.env.iter().find(|(k, _)| k == "CLAUDE_CODE_TMPDIR").map(|(_, v)| PathBuf::from(v));
    let plan = preset_plan(&open, Purpose::Agent(ModelProviderKind::ClaudeCode), &HostPaths::default(), &[]);
    assert_eq!(get(&plan), Some(plan.tmp.clone()));
    let with_home = EnvelopeEnforcer::plan(&PlanInputs {
        host: HostKind::Worker,
        envelope: &open,
        purpose: Purpose::Agent(ModelProviderKind::ClaudeCode),
        gate: &GatePlan::ReadOnly,
        worktree: Path::new("/repo"),
        worktree_is_isolated: false,
        write_capable: false,
        tmp: Path::new("/tmp/ligus-turn-cc"),
        home: Path::new("/home/u"),
        host_paths: &HostPaths::default(),
        mcp_hosts: &[],
        mcp_ids: &[],
        model_base_url: None,
        extra_env: &[("CLAUDE_CODE_TMPDIR".to_string(), "/tmp/elsewhere".to_string())],
        provider_key: None,
        model_provider_id: None,
        envelope_args: &[],
        ligus_mcp_socket: None,
        egress_presets: &[],
        cli_home: Some(Path::new("/tmp/ligus-turn-cc/home")),
    });
    assert_eq!(get(&with_home), Some(PathBuf::from("/tmp/ligus-turn-cc")));
    assert_eq!(with_home.env.iter().filter(|(k, _)| k == "CLAUDE_CODE_TMPDIR").count(), 1);
    // Other runtimes don't get it.
    assert_eq!(get(&preset_plan(&open, Purpose::Agent(ModelProviderKind::Codex), &HostPaths::default(), &[])), None);
    // The executors pass the plan's env through: Seatbelt's and bwrap's
    // child env is `plan_env`, so the variable arrives as planned.
    let env = plan_env(&plan, None);
    assert!(env.iter().any(|(k, v)| k == "CLAUDE_CODE_TMPDIR" && Path::new(v) == plan.tmp));
    #[cfg(target_os = "macos")]
    {
        // confine writes the profile into the turn scratch dir, which a real
        // turn creates first; on a clean machine nothing else has made it.
        std::fs::create_dir_all(&plan.tmp).unwrap();
        let cmd = seatbelt::Seatbelt.confine(OsStr::new("/usr/bin/true"), &[], Path::new("/tmp"), &plan, None).unwrap();
        assert!(cmd.get_envs().any(|(k, v)| k == "CLAUDE_CODE_TMPDIR" && v.map(Path::new) == Some(plan.tmp.as_path())));
    }
    #[cfg(target_os = "linux")]
    if let Some(cmd) = linux::Bwrap::probe().ok().and_then(|b| b.confine(OsStr::new("/bin/true"), &[], Path::new("/tmp"), &plan, None).ok()) {
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let via_env = cmd.get_envs().any(|(k, v)| k == "CLAUDE_CODE_TMPDIR" && v.map(Path::new) == Some(plan.tmp.as_path()));
        let via_args = args.windows(3).any(|w| w[0] == "--setenv" && w[1] == "CLAUDE_CODE_TMPDIR" && Path::new(&w[2]) == plan.tmp);
        assert!(via_env || via_args, "bwrap child env lacks CLAUDE_CODE_TMPDIR: {args:?}");
    }
}

/// The scratch dir goes away when the turn's guard drops (a cancelled turn
/// drops it with its future), and the sweep removes a leaked dir that no
/// live process owns once it's older than the longest turn, but never a dir
/// in use here, a young one, or one another running process owns.
#[test]
fn stale_turn_dirs_are_swept_and_live_ones_kept() {
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let parent = root.path();
    // A live dir of this process: kept, then removed on drop.
    let live = make_turn_tmp_in(parent).unwrap();
    let live_path = live.path().to_path_buf();
    assert!(live_path.join(".ligus-owner").exists());
    // A leaked dir whose owner is gone (a dead pid), with a copied config.
    let dead = parent.join("ligus-turn-dead");
    std::fs::create_dir_all(dead.join("home/.copilot")).unwrap();
    std::fs::write(dead.join("home/.copilot/config.json"), "{}").unwrap();
    // Find a pid that isn't running.
    let dead_pid = (400_000u32..500_000).find(|p| !pid_alive(*p)).unwrap();
    std::fs::write(dead.join(".ligus-owner"), dead_pid.to_string()).unwrap();
    // No owner file at all (a dir from an older Ligus).
    let ownerless = parent.join("ligus-turn-old");
    std::fs::create_dir_all(&ownerless).unwrap();
    // Owned by a running process (pid 1; on Windows pid 4, the System process).
    let other = parent.join("ligus-turn-other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join(".ligus-owner"), if cfg!(windows) { "4" } else { "1" }).unwrap();
    // Not a turn dir.
    let unrelated = parent.join("something-else");
    std::fs::create_dir_all(&unrelated).unwrap();

    // Young: nothing goes.
    assert_eq!(sweep_stale_turn_dirs_in(parent, Duration::from_secs(3600)), 0);
    assert!(dead.exists() && ownerless.exists());
    // Older than the longest turn: the dead and ownerless dirs go.
    assert_eq!(sweep_stale_turn_dirs_in(parent, Duration::ZERO), 2);
    assert!(!dead.exists() && !ownerless.exists());
    assert!(live_path.exists() && other.exists() && unrelated.exists());
    drop(live);
    assert!(!live_path.exists(), "dropping the turn's scratch dir removes it");

    // A whole prepared turn: its guard removes the dir, including when the
    // turn's future is cancelled before the child finishes.
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let dir = rt.block_on(async {
        let (tx, rx) = tokio::sync::oneshot::channel::<PathBuf>();
        let turn = async move {
            let p = prepare_with(&ScratchOnly, SpawnSpec::new(Purpose::McpServer, "/bin/sh", ["-c", "sleep 30"])).unwrap();
            let tmp = p.guard.plan().tmp.clone();
            let _ = tx.send(tmp);
            let _guard = p.guard;
            std::future::pending::<()>().await;
        };
        let _ = tokio::time::timeout(Duration::from_millis(50), turn).await;
        rx.await.unwrap()
    });
    assert!(!dir.exists(), "a cancelled turn's scratch dir is removed");
}

// ---------------------------------------------------------------------------
// Sandbox defaults (owner-approved; docs: concepts/security.md "Sandbox
// defaults"). Each test pins one default.
// ---------------------------------------------------------------------------

/// Default 1: an agent can't commit inside a turn. `.git` is read-only in
/// every plan, and under the real sandbox `git commit` fails while `git
/// status` still works, so the turn's changes stay a diff Ligus captures as
/// a proposal.
#[test]
fn sandbox_default_agents_cannot_commit() {
    let host = HostPaths::default();
    for kind in KINDS {
        for gate in [GatePlan::Full, GatePlan::ReadOnly] {
            let plan = EnvelopeEnforcer::plan(&PlanInputs {
                host: HostKind::Desktop,
                envelope: &SandboxEnvelope::workspace_default(),
                purpose: Purpose::Agent(*kind),
                gate: &gate,
                worktree: Path::new("/repo"),
                worktree_is_isolated: true,
                write_capable: true,
                tmp: Path::new("/tmp/t"),
                home: Path::new("/home/u"),
                host_paths: &host,
                mcp_hosts: &[],
                mcp_ids: &[],
                model_base_url: None,
                extra_env: &[],
                provider_key: None,
                model_provider_id: None,
                envelope_args: &[],
                ligus_mcp_socket: None,
                egress_presets: &[],
                cli_home: None,
            });
            assert!(plan.read_only.contains(&PathBuf::from("/repo/.git")), "{kind:?} {gate:?}");
        }
    }
    let Some(e) = real_executor() else { return };
    if std::process::Command::new("git").arg("--version").output().map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("skipping: no git");
        return;
    }
    let f = Fixture::new();
    let wt = &f.worktree;
    let _ = std::fs::remove_dir_all(wt.join(".git"));
    let git = |args: &[&str]| {
        let o = std::process::Command::new("git").args(args).current_dir(wt).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    git(&["init", "-q"]);
    git(&["-c", "user.name=t", "-c", "user.email=t@example.com", "add", "-A"]);
    git(&["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-qm", "base"]);
    let head = git(&["rev-parse", "HEAD"]);
    let (ok, out) = run_sh(&*e, &f, "git status --porcelain >/dev/null && echo READS", isolated());
    if !ok || !out.contains("READS") {
        eprintln!("skipping the commit half: git can't run under this host's sandbox ({out})");
        return;
    }
    let (ok, out) = run_sh(
        &*e,
        &f,
        "echo changed > README.md && git -c user.name=a -c user.email=a@example.com commit -qam agent-commit",
        isolated(),
    );
    assert!(!ok, "an agent committed inside a turn: {out}");
    assert_eq!(git(&["rev-parse", "HEAD"]), head, "HEAD moved");
    // The edit itself landed in the worktree: Ligus captures it as a diff.
    assert_eq!(std::fs::read_to_string(wt.join("README.md")).unwrap().trim(), "changed");
}

/// Default 2: where there's no OS sandbox (`LIGUS_SANDBOX=off`, Windows,
/// Linux without bubblewrap or Landlock) every agent turn is refused before
/// it starts, write-capable or read-only.
#[test]
fn sandbox_default_write_turns_refused_without_os_sandbox() {
    let wt = tempfile::tempdir().unwrap();
    let write_specs: Vec<(ModelProviderKind, Vec<&str>)> = vec![
        (ModelProviderKind::ClaudeCode, vec!["--permission-mode", "acceptEdits"]),
        (ModelProviderKind::ClaudeCode, vec!["--dangerously-skip-permissions"]),
        (ModelProviderKind::Copilot, vec!["--allow-all-tools"]),
        (ModelProviderKind::Antigravity, vec!["--mode", "auto"]),
        (ModelProviderKind::Codex, vec![]),
        (ModelProviderKind::Aider, vec![]),
        (ModelProviderKind::Pi, vec![]),
        (ModelProviderKind::Hermes, vec![]),
    ];
    for (kind, args) in write_specs {
        let spec = SpawnSpec::new(Purpose::Agent(kind), "true", args.clone()).cwd(Some(wt.path()));
        let err = prepare_with(&Unconfined, spec).err().unwrap_or_else(|| panic!("{kind:?} {args:?} ran unconfined"));
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{kind:?}");
    }
    for kind in [ModelProviderKind::ClaudeCode, ModelProviderKind::Copilot, ModelProviderKind::Antigravity] {
        // The CLI at its read-only tier (Copilot's is explicit deny rules)
        // is refused too: unconfined, its reads reach the Ligus data dir.
        let ro = crate::author_gate::read_only_args(kind, &[]);
        let spec = SpawnSpec::new(Purpose::Agent(kind), "true", ro).cwd(Some(wt.path()));
        assert!(prepare_with(&Unconfined, spec).is_err(), "{kind:?} read-only is refused");
    }
    // No Seatbelt and no bubblewrap backend on this OS (Windows): the host
    // refuses write turns.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    assert!(refuses_writes());
}

/// Default 3: on a worker (and every other host) a CLI that always writes
/// (codex, aider, pi, hermes) runs only for a message whose author was an
/// Admin or Owner when they posted it; below that it is refused, not
/// downgraded, because it has no read-only tier.
#[test]
fn sandbox_default_always_writing_clis_need_admin() {
    use hive_core::WorkspaceRole::*;
    use crate::author_gate::{always_writes, gate, gate_turn, min_role_from};
    for kind in [ModelProviderKind::Codex, ModelProviderKind::Aider, ModelProviderKind::Pi, ModelProviderKind::Hermes] {
        assert!(always_writes(kind));
        // The default floor for such a turn (no LIGUS_MCP_MIN_ROLE) is Admin.
        assert_eq!(min_role_from(None, true), Admin);
        for role in [None, Some(Viewer), Some(Contributor)] {
            assert!(matches!(gate(kind, role, Admin), GatePlan::Refuse(_)), "{kind:?} {role:?}");
        }
        for role in [Admin, Owner] {
            assert_eq!(gate(kind, Some(role), Admin), GatePlan::Full, "{kind:?} {role:?}");
        }
        if std::env::var_os("LIGUS_MCP_MIN_ROLE").is_none() && std::env::var_os("HIVE_MCP_MIN_ROLE").is_none() {
            let mut args = Vec::new();
            assert!(matches!(gate_turn(kind, &mut args, Some(Contributor)), GatePlan::Refuse(_)), "{kind:?}");
            assert_eq!(gate_turn(kind, &mut args, Some(Admin)), GatePlan::Full);
        }
    }
    for kind in [ModelProviderKind::ClaudeCode, ModelProviderKind::Copilot, ModelProviderKind::Antigravity] {
        assert!(!always_writes(kind), "{kind:?} has a read-only tier");
    }
}

/// S11: forge CLIs keep their logins in plain files on a box without a secret
/// service (gh's hosts.yml holds a write-capable token). An agent never reads
/// them; the forge tools reach the code host for it.
#[test]
fn home_secret_dirs_cover_forge_cli_logins() {
    let home = Path::new("/home/u");
    for purpose in [Purpose::Agent(ModelProviderKind::ClaudeCode), Purpose::Agent(ModelProviderKind::Aider), Purpose::Agent(ModelProviderKind::Codex)] {
        let dirs = home_secret_dirs(home, purpose);
        for rel in [".config/gh", ".config/glab-cli", ".config/tea", ".config/hub"] {
            assert!(dirs.contains(&home.join(rel)), "{rel} readable by {purpose:?}");
        }
    }
}

/// A stand-in OS sandbox for tests of the turn plumbing (scratch dirs,
/// guards) that don't exercise confinement: it reports a confining kind
/// and spawns like [`Unconfined`].
struct ScratchOnly;

impl TurnExecutor for ScratchOnly {
    fn kind(&self) -> ExecutorKind {
        ExecutorKind::Seatbelt
    }
    fn confine(&self, program: &OsStr, args: &[OsString], cwd: &Path, plan: &EnforcementPlan, proxy: Option<&EgressProxy>) -> io::Result<std::process::Command> {
        Unconfined.confine(program, args, cwd, plan, proxy)
    }
}

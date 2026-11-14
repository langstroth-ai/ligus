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
/// agent turn is refused, and a read-only one still runs.
#[test]
fn unconfined_refuses_write_tiers() {
    let wt = tempfile::tempdir().unwrap();
    let write = SpawnSpec::new(Purpose::Agent(ModelProviderKind::ClaudeCode), "true", ["--permission-mode", "acceptEdits"]).cwd(Some(wt.path()));
    let err = prepare_with(&Unconfined, write).err().expect("refused");
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    let codex = SpawnSpec::new(Purpose::Agent(ModelProviderKind::Codex), "true", Vec::<String>::new()).cwd(Some(wt.path()));
    assert!(prepare_with(&Unconfined, codex).is_err());
    let read = SpawnSpec::new(Purpose::Agent(ModelProviderKind::ClaudeCode), "true", ["--model", "x"]).cwd(Some(wt.path()));
    let p = prepare_with(&Unconfined, read).expect("a read-only turn runs");
    assert_eq!(p.guard.kind(), ExecutorKind::Unconfined);
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
    // `ligus-sandbox-helper` bin cargo built next to it.
    #[cfg(target_os = "linux")]
    if let Some(exe) = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.parent()?.join("ligus-sandbox-helper"))) {
        if exe.exists() {
            set_helper_exe(exe);
        }
    }
    let e = default_executor();
    if e.kind() == ExecutorKind::Unconfined {
        eprintln!("skipped: no agent sandbox on this host (see docs: the Linux sandbox tests must run where bubblewrap and user namespaces are available)");
        return None;
    }
    Some(e)
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
    let control = with_turn_sync(TurnPolicy { gate: GatePlan::ReadOnly, ..isolated() }, || {
        let spec = SpawnSpec::new(Purpose::Agent(ModelProviderKind::ClaudeCode), "/bin/sh", ["-c", "cat \"$0\"", f.data.join("settings.json").to_str().unwrap()])
            .cwd(Some(&f.worktree));
        let mut p = prepare_with(&Unconfined, spec).expect("prepare");
        String::from_utf8_lossy(&p.command.output().unwrap().stdout).into_owned()
    });
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
    configure_host(&f.data, &[token.clone()], &[]);
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
    // Inside the worktree, writes work.
    let (ok, out) = run_sh(&*e, &f, "echo ok > new.txt && cat new.txt", isolated());
    assert!(ok && out.contains("ok"), "{out}");
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

//! An agent's local-servers grant (`localServers`): off by default, only for
//! that agent's own CLI child, held on macOS (no effect), and nothing else in
//! the plan changes. The `sandbox_local_servers_*` tests run a
//! probe under this host's real executor.

use super::home_tests::plan_for;
use super::tests::{real_executor, Fixture};
use super::*;
use ligus_core::sandbox::{AgentGrant, PathGrants, SandboxGrants};

fn on() -> PathGrants {
    PathGrants { local_servers: true, ..Default::default() }
}

fn plan_with(purpose: Purpose, grants: &PathGrants) -> EnforcementPlan {
    plan_for(Path::new("/home/u"), &HostPaths::default(), purpose, grants)
}

#[test]
fn local_servers_are_off_by_default_and_only_for_the_agent_child() {
    let agent = Purpose::Agent(ModelProviderKind::Antigravity);
    assert!(!plan_with(agent, &PathGrants::default()).local_servers, "off by default");
    assert!(plan_with(agent, &on()).local_servers);
    for p in [Purpose::Tool, Purpose::Preflight, Purpose::McpServer] {
        assert!(!plan_with(p, &on()).local_servers, "{p:?} never gets it");
    }
    // One agent's grant is that agent's: the grants hand it to no other.
    let (a, b) = (uuid::Uuid::from_u128(1), uuid::Uuid::from_u128(2));
    let mut g = SandboxGrants::default();
    g.agents.insert(a, AgentGrant { local_servers: true, ..Default::default() });
    assert!(plan_with(agent, &g.paths_for(Some(a))).local_servers);
    assert!(!plan_with(agent, &g.paths_for(Some(b))).local_servers);
    assert!(!plan_with(agent, &g.paths_for(None)).local_servers);
}

#[test]
fn local_servers_change_nothing_else_in_the_plan() {
    for k in [ModelProviderKind::Antigravity, ModelProviderKind::ClaudeCode, ModelProviderKind::Codex, ModelProviderKind::Copilot] {
        let off = plan_with(Purpose::Agent(k), &PathGrants::default());
        let mut onp = plan_with(Purpose::Agent(k), &on());
        assert!(onp.local_servers);
        onp.local_servers = false;
        assert_eq!(onp, off, "{k:?}: files, env, egress and MCP are the same with it on");
    }
}

fn plan_in(envelope: &SandboxEnvelope, grants: &PathGrants) -> EnforcementPlan {
    let host = HostPaths::default();
    EnvelopeEnforcer::plan(&PlanInputs {
        host: HostKind::Desktop,
        envelope,
        purpose: Purpose::Agent(ModelProviderKind::Antigravity),
        gate: &GatePlan::Full,
        worktree: Path::new("/home/u/projects/ws"),
        worktree_is_isolated: true,
        write_capable: true,
        tmp: Path::new("/tmp/ligus-turn-x"),
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
        gateway: None,
        turn_home: Some(Path::new("/tmp/ligus-turn-x/home")),
        grants,
        extra_home_read: &[],
        user_config: false,
    })
}

#[test]
fn a_tightened_envelope_drops_local_servers() {
    use ligus_core::sandbox::TightenedBy;
    for by in [TightenedBy::Lockdown, TightenedBy::Dearth] {
        let envelope = SandboxEnvelope::workspace_default().tighten(by, EgressPolicy::Closed, true);
        assert!(!plan_in(&envelope, &on()).local_servers, "{by:?}: runs its agents without local servers");
    }
    // An agent CLI run outside a turn (a runtime's test turn) keeps a grant
    // its caller passed: its envelope is closed, not tightened.
    assert!(plan_in(&TurnPolicy::outside_turn(HostKind::Desktop).envelope, &on()).local_servers);
    assert!(!plan_in(&TurnPolicy::outside_turn(HostKind::Desktop).envelope, &PathGrants::default()).local_servers);
}

/// macOS is held: the Seatbelt profile with the grant is byte-identical to
/// the profile without it (Seatbelt can't keep a listener loopback-only).
#[test]
fn seatbelt_ignores_the_grant() {
    let off = plan_with(Purpose::Agent(ModelProviderKind::Antigravity), &PathGrants::default());
    let onp = plan_with(Purpose::Agent(ModelProviderKind::Antigravity), &on());
    assert!(onp.local_servers, "the grant is still planned (and stored)");
    for port in [None, Some(4123)] {
        let (a, b) = (seatbelt::profile(&off, port), seatbelt::profile(&onp, port));
        assert_eq!(b, a, "the profile doesn't change with the grant");
        assert!(!b.contains("network-bind") && !b.contains("network-inbound"), "no listening rule: {b}");
    }
}

/// A probe that listens on 127.0.0.1:0 (as agy's language server does),
/// connects to itself and echoes a byte: `LISTEN_OK`, else `LISTEN_DENIED`.
const LISTEN_PROBE: &str = concat!(
    "import socket\n",
    "try:\n",
    "    s = socket.socket(); s.bind(('127.0.0.1', 0)); s.listen(1)\n",
    "    c = socket.socket(); c.settimeout(3); c.connect(s.getsockname()); a, _ = s.accept(); a.send(b'x')\n",
    "    print('LISTEN_OK' if c.recv(1) == b'x' else 'LISTEN_DENIED')\n",
    "except OSError as e:\n",
    "    print('LISTEN_DENIED', e)\n",
);

fn python() -> Option<&'static str> {
    ["/usr/bin/python3", "/usr/local/bin/python3", "/bin/python3"].into_iter().find(|p| Path::new(p).exists())
}

fn agy_policy(local_servers: bool) -> TurnPolicy {
    TurnPolicy {
        worktree_is_isolated: true,
        grants: PathGrants { local_servers, ..Default::default() },
        ..TurnPolicy::baseline(HostKind::Desktop)
    }
}

fn run_py(e: &dyn TurnExecutor, f: &Fixture, py: &str, code: &str, args: &[String], local_servers: bool) -> String {
    with_turn_sync(agy_policy(local_servers), || {
        let mut a: Vec<String> = vec!["-I".into(), "-c".into(), code.into()];
        a.extend(args.iter().cloned());
        let spec = SpawnSpec::new(Purpose::Agent(ModelProviderKind::Antigravity), py, a).cwd(Some(&f.worktree));
        let mut p = prepare_with(e, spec).expect("prepare");
        let out = p.command.output().expect("spawn");
        drop(p.guard);
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    })
}

#[test]
fn sandbox_local_servers_listen_only_inside_a_private_namespace() {
    let Some(e) = real_executor() else { return };
    let Some(py) = python() else {
        eprintln!("skipped: no python3");
        return;
    };
    let f = Fixture::new();
    let off = run_py(&*e, &f, py, LISTEN_PROBE, &[], false);
    let on = run_py(&*e, &f, py, LISTEN_PROBE, &[], true);
    match e.kind() {
        // Held on macOS: refused with or without the grant.
        ExecutorKind::Seatbelt => {
            assert!(off.contains("LISTEN_DENIED") && on.contains("LISTEN_DENIED"), "off: {off} on: {on}");
        }
        // Its own network namespace: loopback is the turn's, either way.
        ExecutorKind::Bwrap => {
            assert!(off.contains("LISTEN_OK") && on.contains("LISTEN_OK"), "off: {off} on: {on}");
        }
        // No namespace to contain it: refused with or without the grant.
        ExecutorKind::Landlock => {
            assert!(off.contains("LISTEN_DENIED") && on.contains("LISTEN_DENIED"), "off: {off} on: {on}");
        }
        ExecutorKind::Unconfined => {}
    }
}

#[test]
fn sandbox_local_servers_leave_files_unix_sockets_and_egress_denied() {
    let Some(e) = real_executor() else { return };
    let Some(py) = python() else {
        eprintln!("skipped: no python3");
        return;
    };
    let f = Fixture::new();
    // Ligus's data stays unreadable.
    let read = "import sys\ntry:\n    print(open(sys.argv[1]).read())\nexcept OSError as e:\n    print('READ_DENIED', e)\n";
    let out = run_py(&*e, &f, py, read, &[f.data.join("keys/device.seed").display().to_string()], true);
    assert!(out.contains("READ_DENIED") && !out.contains("SEED-SECRET"), "{out}");
    // A host unix socket stays unreachable.
    let dir = tempfile::Builder::new().prefix("lls").tempdir_in("/tmp").unwrap();
    let path = dir.path().join("svc.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let unix = "import socket,sys\ntry:\n    s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); print('UNIX_CONNECTED')\nexcept OSError as e:\n    print('UNIX_DENIED', e)\n";
    let out = run_py(&*e, &f, py, unix, &[canonical(&path).display().to_string()], true);
    assert!(out.contains("UNIX_DENIED"), "{out}");
    assert!(listener.accept().is_err(), "the host socket saw a connection");
    // A direct connection off this machine stays refused (only the proxy
    // goes out).
    let egress = "import socket\ntry:\n    socket.create_connection(('1.1.1.1', 80), timeout=3); print('EGRESS_CONNECTED')\nexcept OSError as e:\n    print('EGRESS_DENIED', e)\n";
    let out = run_py(&*e, &f, py, egress, &[], true);
    assert!(out.contains("EGRESS_DENIED"), "{out}");
}

/// The grant never reaches this machine's other loopback servers: macOS
/// ignores it, bubblewrap's namespace has its own loopback, the Landlock
/// tier refuses.
#[test]
fn sandbox_local_servers_and_this_machines_other_servers() {
    let Some(e) = real_executor() else { return };
    let Some(py) = python() else {
        eprintln!("skipped: no python3");
        return;
    };
    let f = Fixture::new();
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    l.set_nonblocking(true).unwrap();
    let probe = "import socket,sys\ntry:\n    socket.create_connection(('127.0.0.1', int(sys.argv[1])), timeout=3); print('HOST_CONNECTED')\nexcept OSError as e:\n    print('HOST_DENIED', e)\n";
    let off = run_py(&*e, &f, py, probe, &[port.to_string()], false);
    assert!(off.contains("HOST_DENIED"), "without the grant nothing on the host is reached: {off}");
    let on = run_py(&*e, &f, py, probe, &[port.to_string()], true);
    assert!(on.contains("HOST_DENIED"), "{on}");
}

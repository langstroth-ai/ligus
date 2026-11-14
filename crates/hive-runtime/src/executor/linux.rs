//! Linux: bubblewrap + Landlock + seccomp.
//!
//! `bwrap` gives the child a read-only view of `/`, tmpfs over Ligus's data
//! dir, `~/.ssh` and the other denied paths (and over `$XDG_RUNTIME_DIR`, so
//! the D-Bus session bus and Secret Service are unreachable), the worktree
//! (read-write, with `.git` and the protected dirs bound read-only on top),
//! the turn's scratch dir, the runtime's state dirs, and fresh PID, IPC, UTS
//! and **network** namespaces.
//!
//! Inside, a tiny pre-exec helper (the same binary, hidden argument
//! [`HELPER_ARG`]) then:
//! 1. when the turn has egress, listens on `127.0.0.1:<proxy port>` in the
//!    new namespace and forwards each connection to the host's egress proxy
//!    through a bind-mounted unix socket (loopback allow-list entries get a
//!    listener of their own that tunnels through the proxy with `CONNECT`);
//! 2. applies **Landlock** (best effort for the kernel's ABI): writes only
//!    beneath the writable paths;
//! 3. applies **seccomp**: `ptrace`, `process_vm_*`, `mount`, `umount2`,
//!    `pivot_root`, `bpf`, `perf_event_open`, `keyctl`, `add_key`,
//!    `request_key`, `kexec_*`, `init_module`, `finit_module` fail with
//!    EPERM, and so do `unshare`/`clone` with `CLONE_NEWUSER`; `clone3`
//!    fails with ENOSYS so libc falls back to `clone`; and (C-10) for every
//!    child with a network namespace, `socket(AF_UNIX, …)` fails with EPERM,
//!    so no host unix socket (`docker.sock`, an ssh-agent, a desktop
//!    service) is reachable through bwrap's read-only `/`. `socketpair`
//!    still works. The turn's Ligus MCP server is reached through a
//!    loopback forwarder the helper runs on [`SANDBOX_TCP_PORT`] inside the
//!    namespace, and the egress proxy through the forwarders above (both
//!    started before the filter, which binds only the command's thread);
//! 4. runs the command.
//!
//! Known host sockets (`docker.sock`, `podman.sock`, `containerd.sock`,
//! `$SSH_AUTH_SOCK`) are also masked with `/dev/null`, a second layer that
//! still holds where the unix-socket filter is turned off
//! (`LIGUS_SANDBOX_UNIX_SOCKETS=allow`, device-local, for an agent whose
//! tools need their own unix sockets).
//!
//! Deviation from the spec: the network is always a fresh namespace (not
//! Landlock's TCP port filter on ABI ≥ 4), so UDP and every other protocol
//! are cut too, on every kernel.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

use super::{canonical, host_paths, plan_env, EgressPlan, EnforcementPlan, ExecutorKind, TurnExecutor};
use crate::egress::EgressProxy;
use crate::ligus_mcp::bridge::SANDBOX_TCP_PORT;

/// Whether this device lets confined children create unix sockets
/// (`LIGUS_SANDBOX_UNIX_SOCKETS=allow`; off by default, C-10).
fn unix_sockets_allowed() -> bool {
    hive_core::compat::var("LIGUS_SANDBOX_UNIX_SOCKETS").is_ok_and(|v| v.trim().eq_ignore_ascii_case("allow"))
}

/// Host sockets an agent must never reach, masked by path as a second layer.
fn known_host_sockets() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = ["/var/run/docker.sock", "/run/docker.sock", "/run/podman/podman.sock", "/run/containerd/containerd.sock"].iter().map(PathBuf::from).collect();
    v.extend(std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from));
    v
}

/// The hidden first argument that turns a Ligus binary into the helper.
pub const HELPER_ARG: &str = "__ligus-sandbox-exec-linux";

/// The env var that carries the egress proxy's token to the helper (C-12:
/// never on a command line, which every user on the host can read with
/// `ps`). The helper takes it out of its env before running the command.
pub const PROXY_TOKEN_ENV: &str = "LIGUS_SANDBOX_PROXY_TOKEN";

pub struct Bwrap {
    bwrap: PathBuf,
}

impl Bwrap {
    /// `bwrap` on PATH, and able to create a user namespace here.
    pub fn probe() -> Result<Self, String> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let bwrap = std::env::split_paths(&path)
            .map(|d| d.join("bwrap"))
            .find(|p| p.is_file())
            .ok_or_else(|| "bwrap is not installed".to_string())?;
        let ok = std::process::Command::new(&bwrap)
            .args(["--ro-bind", "/", "/", "--dev", "/dev", "--unshare-net", "--unshare-pid", "--die-with-parent", "--", "true"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return Err("bwrap can't create namespaces here (user namespaces disabled?)".into());
        }
        Ok(Self { bwrap })
    }
}

/// The binary that runs the helper: an explicit one, else a
/// `ligus-sandbox-helper` installed next to this executable (or one level
/// up, where cargo puts bins beside test harnesses), else this executable
/// (the `ligus` CLI and the desktop app both carry it).
fn helper_exe() -> io::Result<PathBuf> {
    if let Some(p) = host_paths().helper_exe {
        return Ok(p);
    }
    let me = std::env::current_exe()?;
    for dir in [me.parent(), me.parent().and_then(Path::parent)].into_iter().flatten() {
        let sibling = dir.join("ligus-sandbox-helper");
        if sibling.is_file() {
            return Ok(sibling);
        }
    }
    Ok(me)
}

/// The bwrap arguments for `plan` (everything before the helper).
pub fn bwrap_args(plan: &EnforcementPlan, cwd: &Path) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    let mut push = |xs: &[&OsStr]| a.extend(xs.iter().map(|x| x.to_os_string()));
    push(&["--die-with-parent".as_ref(), "--new-session".as_ref(), "--unshare-pid".as_ref(), "--unshare-ipc".as_ref(), "--unshare-uts".as_ref()]);
    if !matches!(plan.egress, EgressPlan::Open) {
        push(&["--unshare-net".as_ref()]);
    }
    push(&["--ro-bind".as_ref(), "/".as_ref(), "/".as_ref(), "--dev".as_ref(), "/dev".as_ref(), "--proc".as_ref(), "/proc".as_ref()]);
    let mut denied: Vec<PathBuf> = plan.deny_read.iter().map(|p| canonical(p)).collect();
    if let Some(run) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        denied.push(canonical(&run));
    }
    // Shallow first, so a nested deny isn't hidden under a parent's tmpfs.
    denied.sort_by_key(|p| p.components().count());
    for d in &denied {
        match std::fs::metadata(d) {
            Ok(m) if m.is_dir() => push(&["--tmpfs".as_ref(), d.as_os_str()]),
            Ok(_) => push(&["--ro-bind".as_ref(), "/dev/null".as_ref(), d.as_os_str()]),
            Err(_) => {}
        }
    }
    // C-10: known host sockets read as /dev/null (a second layer to the
    // seccomp unix-socket filter). Ones under a denied dir are gone already.
    {
        use std::os::unix::fs::FileTypeExt;
        let mut masked: Vec<PathBuf> = Vec::new();
        for sock in known_host_sockets() {
            let c = canonical(&sock);
            if masked.contains(&c) || denied.iter().any(|d| c.starts_with(d)) {
                continue;
            }
            if std::fs::metadata(&c).is_ok_and(|m| m.file_type().is_socket()) {
                push(&["--ro-bind".as_ref(), "/dev/null".as_ref(), c.as_os_str()]);
                masked.push(c);
            }
        }
    }
    for r in &plan.allow_read {
        let r = canonical(r);
        if r.exists() && denied.iter().any(|d| r.starts_with(d)) && r != canonical(&plan.worktree) {
            push(&["--ro-bind".as_ref(), r.as_os_str(), r.as_os_str()]);
        }
    }
    let wt = canonical(&plan.worktree);
    if plan.worktree_writable {
        push(&["--bind".as_ref(), wt.as_os_str(), wt.as_os_str()]);
        // A protected path that doesn't exist yet is pinned by an empty
        // read-only directory at that name, so the agent can't create it
        // (git ignores the empty dir; the guard removes it afterwards).
        let empty = placeholder_dir(plan);
        for r in &plan.read_only {
            let r = canonical(r);
            if r.exists() {
                push(&["--ro-bind".as_ref(), r.as_os_str(), r.as_os_str()]);
            } else {
                push(&["--ro-bind".as_ref(), empty.as_os_str(), r.as_os_str()]);
            }
        }
    } else if wt.exists() {
        push(&["--ro-bind".as_ref(), wt.as_os_str(), wt.as_os_str()]);
    }
    for w in &plan.write_extra {
        let w = canonical(w);
        if w.exists() {
            push(&["--bind".as_ref(), w.as_os_str(), w.as_os_str()]);
        }
    }
    // The turn's Ligus MCP socket (D.3.2): its 0700 directory is bound in, so
    // the socket is reachable even when a denied path's tmpfs covers it.
    // Unix sockets work across the fresh network namespace.
    if let Some(dir) = plan.ligus_mcp_socket.as_ref().and_then(|s| s.parent()) {
        let d = canonical(dir);
        if d.exists() {
            push(&["--bind".as_ref(), d.as_os_str(), d.as_os_str()]);
        }
    }
    push(&["--chdir".as_ref(), cwd.as_os_str()]);
    a
}

/// The empty directory (in the turn's scratch dir) that pins missing
/// protected paths.
pub fn placeholder_dir(plan: &EnforcementPlan) -> PathBuf {
    canonical(&plan.tmp).join(".ligus-empty")
}

/// Remove the empty placeholder dirs bwrap left for missing protected paths.
pub fn remove_placeholders(plan: &EnforcementPlan) {
    for r in &plan.read_only {
        if std::fs::read_dir(r).is_ok_and(|mut d| d.next().is_none()) {
            let _ = std::fs::remove_dir(r);
        }
    }
}

/// The helper's arguments (after [`HELPER_ARG`], before `--`).
pub fn helper_args(plan: &EnforcementPlan, proxy: Option<&EgressProxy>) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    let mut rw: Vec<PathBuf> = plan.write_extra.iter().map(|p| canonical(p)).filter(|p| p.exists()).collect();
    if plan.worktree_writable {
        rw.push(canonical(&plan.worktree));
    }
    for p in rw {
        a.push("--rw".into());
        a.push(p.into_os_string());
    }
    // C-10: children with a network namespace (everything but a user's
    // open-egress MCP server) get no unix sockets; the Ligus MCP server is
    // reached through the helper's forwarder.
    let netns = !matches!(plan.egress, EgressPlan::Open);
    if netns && !unix_sockets_allowed() {
        a.push("--deny-unix".into());
    }
    if let Some(sock) = plan.ligus_mcp_socket.as_ref().filter(|_| netns) {
        a.extend(["--mcp-sock".into(), canonical(sock).into_os_string()]);
    }
    if let (Some(p), EgressPlan::Proxy { allow }) = (proxy, &plan.egress) {
        if let Some(sock) = p.unix_socket() {
            a.extend(["--proxy-sock".into(), sock.as_os_str().to_os_string()]);
            a.extend(["--proxy-port".into(), p.port().to_string().into()]);
            for hp in allow.iter().filter(|h| h.is_loopback()) {
                a.extend(["--loop-port".into(), hp.port.to_string().into()]);
            }
        }
    }
    a
}

impl TurnExecutor for Bwrap {
    fn kind(&self) -> ExecutorKind {
        ExecutorKind::Bwrap
    }

    fn confine(&self, program: &OsStr, args: &[OsString], cwd: &Path, plan: &EnforcementPlan, proxy: Option<&EgressProxy>) -> io::Result<std::process::Command> {
        let helper = helper_exe()?;
        std::fs::create_dir_all(placeholder_dir(plan))?;
        let mut c = hive_core::process_util::command(&self.bwrap);
        c.args(bwrap_args(plan, cwd))
            .arg("--")
            .arg(helper)
            .arg(HELPER_ARG)
            .args(helper_args(plan, proxy))
            .arg("--")
            .arg(program)
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .envs(plan_env(plan, proxy));
        if let (Some(p), EgressPlan::Proxy { .. }) = (proxy, &plan.egress) {
            c.env(PROXY_TOKEN_ENV, p.token());
        }
        // bwrap itself leads a new session/process group too (the inner
        // command gets `--new-session`), so callers can kill the group.
        super::new_session(&mut c);
        Ok(c)
    }
}

// ---------------------------------------------------------------------------
// The helper (runs inside bwrap)
// ---------------------------------------------------------------------------

/// If this process was started as the sandbox helper, run it (never
/// returns). Call first thing in `main` of every binary that spawns turns.
pub fn maybe_run_helper() {
    let mut args = std::env::args_os();
    let _ = args.next();
    if args.next().as_deref() != Some(OsStr::new(HELPER_ARG)) {
        return;
    }
    let rest: Vec<OsString> = args.collect();
    match run_helper(rest) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("ligus sandbox: {e}");
            std::process::exit(126);
        }
    }
}

struct HelperOpts {
    rw: Vec<PathBuf>,
    /// The turn's Ligus MCP socket, forwarded from 127.0.0.1:SANDBOX_TCP_PORT.
    mcp_sock: Option<PathBuf>,
    /// Refuse new AF_UNIX sockets (C-10).
    deny_unix: bool,
    proxy_sock: Option<PathBuf>,
    proxy_port: Option<u16>,
    token: String,
    loop_ports: Vec<u16>,
    cmd: Vec<OsString>,
}

fn parse_helper(args: Vec<OsString>) -> io::Result<HelperOpts> {
    let mut o = HelperOpts { rw: Vec::new(), mcp_sock: None, deny_unix: false, proxy_sock: None, proxy_port: None, token: String::new(), loop_ports: Vec::new(), cmd: Vec::new() };
    let mut it = args.into_iter();
    let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidInput, m.to_string());
    while let Some(a) = it.next() {
        let s = a.to_string_lossy().into_owned();
        let mut val = || it.next().ok_or_else(|| bad(&format!("{s} needs a value")));
        match s.as_str() {
            "--rw" => o.rw.push(PathBuf::from(val()?)),
            "--proxy-sock" => o.proxy_sock = Some(PathBuf::from(val()?)),
            "--mcp-sock" => o.mcp_sock = Some(PathBuf::from(val()?)),
            "--deny-unix" => o.deny_unix = true,
            "--proxy-port" => o.proxy_port = Some(val()?.to_string_lossy().parse().map_err(|_| bad("port"))?),
            "--loop-port" => o.loop_ports.push(val()?.to_string_lossy().parse().map_err(|_| bad("port"))?),
            "--" => {
                o.cmd = it.collect();
                break;
            }
            other => return Err(bad(&format!("unknown helper option {other}"))),
        }
    }
    if o.cmd.is_empty() {
        return Err(bad("no command"));
    }
    o.token = take_proxy_token();
    Ok(o)
}

/// The proxy token from the helper's env, removed from it so the command
/// (which inherits the env) never sees this copy.
fn take_proxy_token() -> String {
    let t = std::env::var(PROXY_TOKEN_ENV).unwrap_or_default();
    // SAFETY (edition 2021: safe fn): the helper is single-threaded here,
    // before it starts its forwarders or the command.
    std::env::remove_var(PROXY_TOKEN_ENV);
    t
}

fn splice(a: std::os::unix::net::UnixStream, b: std::net::TcpStream) {
    let (mut a2, mut b2) = match (a.try_clone(), b.try_clone()) {
        (Ok(x), Ok(y)) => (x, y),
        _ => return,
    };
    let (mut a, mut b) = (a, b);
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut b2, &mut a2);
        let _ = a2.shutdown(std::net::Shutdown::Write);
    });
    let _ = std::io::copy(&mut a, &mut b);
    let _ = b.shutdown(std::net::Shutdown::Write);
}

/// The Ligus MCP forwarder: 127.0.0.1:SANDBOX_TCP_PORT (in the turn's
/// network namespace) to the turn's MCP socket. Its threads start before
/// the seccomp filter, which applies only to the thread that installs it
/// (and so to the command), so they can still open unix sockets.
fn start_mcp_forwarder(o: &HelperOpts) -> io::Result<bool> {
    let Some(sock) = o.mcp_sock.clone() else { return Ok(false) };
    let l = std::net::TcpListener::bind(("127.0.0.1", SANDBOX_TCP_PORT))?;
    std::thread::spawn(move || {
        for c in l.incoming().flatten() {
            let sock = sock.clone();
            std::thread::spawn(move || {
                if let Ok(u) = std::os::unix::net::UnixStream::connect(&sock) {
                    splice(u, c);
                }
            });
        }
    });
    Ok(true)
}

fn start_forwarders(o: &HelperOpts) -> io::Result<bool> {
    use std::io::{Read, Write};
    let mcp = start_mcp_forwarder(o)?;
    let (Some(sock), Some(port)) = (o.proxy_sock.clone(), o.proxy_port) else { return Ok(mcp) };
    let l = std::net::TcpListener::bind(("127.0.0.1", port))?;
    let s2 = sock.clone();
    std::thread::spawn(move || {
        for c in l.incoming().flatten() {
            let sock = s2.clone();
            std::thread::spawn(move || {
                if let Ok(u) = std::os::unix::net::UnixStream::connect(&sock) {
                    splice(u, c);
                }
            });
        }
    });
    for lp in &o.loop_ports {
        let lp = *lp;
        if lp == port || lp == SANDBOX_TCP_PORT {
            continue;
        }
        let l = std::net::TcpListener::bind(("127.0.0.1", lp))?;
        let (sock, token) = (sock.clone(), o.token.clone());
        std::thread::spawn(move || {
            for c in l.incoming().flatten() {
                let (sock, token) = (sock.clone(), token.clone());
                std::thread::spawn(move || {
                    let Ok(mut u) = std::os::unix::net::UnixStream::connect(&sock) else { return };
                    let pre = format!("CONNECT 127.0.0.1:{lp} HTTP/1.1\r\nHost: 127.0.0.1:{lp}\r\nProxy-Authorization: Bearer {token}\r\n\r\n");
                    if u.write_all(pre.as_bytes()).is_err() {
                        return;
                    }
                    // Swallow the proxy's response head.
                    let mut head = Vec::new();
                    let mut b = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") && head.len() < 4096 {
                        match u.read(&mut b) {
                            Ok(1) => head.push(b[0]),
                            _ => return,
                        }
                    }
                    if !head.starts_with(b"HTTP/1.1 200") {
                        return;
                    }
                    splice(u, c);
                });
            }
        });
    }
    Ok(true)
}

fn apply_landlock(rw: &[PathBuf]) -> io::Result<()> {
    use landlock::{path_beneath_rules, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr, ABI};
    let abi = ABI::V5;
    let mut writable: Vec<PathBuf> = rw.iter().filter(|p| p.exists()).cloned().collect();
    writable.push(PathBuf::from("/dev"));
    let err = |e: landlock::RulesetError| io::Error::other(e.to_string());
    let status = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .map_err(err)?
        .create()
        .map_err(err)?
        .add_rules(path_beneath_rules(["/"], AccessFs::from_read(abi)))
        .map_err(err)?
        .add_rules(path_beneath_rules(&writable, AccessFs::from_all(abi)))
        .map_err(err)?
        .restrict_self()
        .map_err(err)?;
    if status.ruleset == landlock::RulesetStatus::NotEnforced {
        // bwrap's read-only mounts still apply; Landlock is the second layer.
        eprintln!("ligus sandbox: Landlock is not available on this kernel (bwrap mounts still apply)");
    }
    Ok(())
}

fn apply_seccomp(deny_unix: bool) -> io::Result<()> {
    use seccompiler::{BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter, SeccompRule, TargetArch};
    use std::collections::BTreeMap;
    let arch: TargetArch = std::env::consts::ARCH.try_into().map_err(|e: seccompiler::BackendError| io::Error::other(e.to_string()))?;
    let e = |x: seccompiler::Error| io::Error::other(x.to_string());
    let be = |x: seccompiler::BackendError| io::Error::other(x.to_string());
    let mut deny: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    for nr in [
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_bpf,
        libc::SYS_perf_event_open,
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_kexec_load,
        libc::SYS_kexec_file_load,
        libc::SYS_init_module,
        libc::SYS_finit_module,
    ] {
        deny.insert(nr, vec![]);
    }
    let newuser = libc::CLONE_NEWUSER as u64;
    let flag_rule = |idx: u8| -> io::Result<SeccompRule> {
        SeccompRule::new(vec![SeccompCondition::new(idx, SeccompCmpArgLen::Qword, SeccompCmpOp::MaskedEq(newuser), newuser).map_err(be)?]).map_err(be)
    };
    deny.insert(libc::SYS_unshare, vec![flag_rule(0)?]);
    deny.insert(libc::SYS_clone, vec![flag_rule(0)?]);
    if deny_unix {
        // C-10: socket(AF_UNIX, …). socketpair (local IPC) stays allowed.
        let unix = SeccompRule::new(vec![SeccompCondition::new(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, libc::AF_UNIX as u64).map_err(be)?]).map_err(be)?;
        deny.insert(libc::SYS_socket, vec![unix]);
    }
    let f1 = SeccompFilter::new(deny, SeccompAction::Allow, SeccompAction::Errno(libc::EPERM as u32), arch).map_err(be)?;
    let mut enosys: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    enosys.insert(libc::SYS_clone3, vec![]);
    let f2 = SeccompFilter::new(enosys, SeccompAction::Allow, SeccompAction::Errno(libc::ENOSYS as u32), arch).map_err(be)?;
    let p1: BpfProgram = f1.try_into().map_err(be)?;
    let p2: BpfProgram = f2.try_into().map_err(be)?;
    seccompiler::apply_filter(&p1).map_err(e)?;
    seccompiler::apply_filter(&p2).map_err(e)?;
    Ok(())
}

fn run_helper(args: Vec<OsString>) -> io::Result<i32> {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    let o = parse_helper(args)?;
    let forwarding = start_forwarders(&o)?;
    apply_landlock(&o.rw)?;
    apply_seccomp(o.deny_unix)?;
    let mut cmd = std::process::Command::new(&o.cmd[0]);
    cmd.args(&o.cmd[1..]);
    if !forwarding {
        return Err(cmd.exec());
    }
    // The forwarders live in this process: run the command as a child and
    // pass its status on. It dies with us.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    let status = cmd.spawn()?.wait()?;
    Ok(status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(9)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::egress::HostPort;

    fn plan(tmp: &Path) -> EnforcementPlan {
        EnforcementPlan {
            worktree: tmp.join("wt"),
            worktree_writable: false,
            read_only: vec![],
            deny_read: vec![],
            allow_read: vec![],
            write_extra: vec![],
            tmp: tmp.to_path_buf(),
            egress: EgressPlan::Proxy { allow: vec![HostPort::new("api.anthropic.com", 443), HostPort::new("127.0.0.1", 11434)] },
            env: vec![],
            args: vec![],
            mcp: vec![],
            ligus_mcp_socket: None,
        }
    }

    /// C-12: the egress proxy's token used to be a helper argument, readable
    /// by every user on the host in `ps`. It now reaches the helper in its
    /// env, which the helper clears before running the command.
    #[test]
    fn proxy_token_stays_off_the_helper_command_line() {
        let t = tempfile::tempdir().unwrap();
        let p = plan(t.path());
        let proxy = EgressProxy::start(vec![HostPort::new("api.anthropic.com", 443)], Some(&t.path().join("e.sock"))).unwrap();
        let token = proxy.token().to_string();
        assert!(!token.is_empty());
        assert!(!helper_args(&p, Some(&proxy)).iter().any(|a| a.to_string_lossy().contains(&token)), "token in the helper's args");
        let c = Bwrap { bwrap: PathBuf::from("/usr/bin/bwrap") }.confine(OsStr::new("/bin/true"), &[], t.path(), &p, Some(&proxy)).unwrap();
        assert!(!c.get_args().any(|a| a.to_string_lossy().contains(&token)), "token on bwrap's command line");
        assert!(c.get_envs().any(|(k, v)| k == PROXY_TOKEN_ENV && v.is_some_and(|v| v == token.as_str())), "the helper gets the token in its env");
        // The helper takes it out of its env.
        std::env::set_var(PROXY_TOKEN_ENV, "tok-123");
        let o = parse_helper(vec!["--proxy-port".into(), "1".into(), "--".into(), "true".into()]).unwrap();
        assert_eq!(o.token, "tok-123");
        assert!(std::env::var_os(PROXY_TOKEN_ENV).is_none(), "the command would inherit the token");
        // The old argument is gone.
        assert!(parse_helper(vec!["--token".into(), "x".into(), "--".into(), "true".into()]).is_err());
    }

    /// C-10: a child with a network namespace gets the unix-socket filter
    /// and the MCP forwarder; a user's open-egress MCP server gets neither.
    #[test]
    fn helper_denies_unix_sockets_and_forwards_mcp() {
        let t = tempfile::tempdir().unwrap();
        let mut p = plan(t.path());
        p.ligus_mcp_socket = Some(t.path().join("mcp/s"));
        let a: Vec<String> = helper_args(&p, None).iter().map(|x| x.to_string_lossy().into_owned()).collect();
        assert!(a.contains(&"--deny-unix".to_string()), "{a:?}");
        assert!(a.windows(2).any(|w| w[0] == "--mcp-sock" && w[1].ends_with("mcp/s")), "{a:?}");
        let o = parse_helper(helper_args(&p, None).into_iter().chain(["--".into(), "true".into()]).collect()).unwrap();
        assert!(o.deny_unix && o.mcp_sock.is_some());
        p.egress = EgressPlan::Open;
        let a: Vec<String> = helper_args(&p, None).iter().map(|x| x.to_string_lossy().into_owned()).collect();
        assert!(!a.contains(&"--deny-unix".to_string()) && !a.contains(&"--mcp-sock".to_string()), "{a:?}");
    }
}

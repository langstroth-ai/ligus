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
//!
//! ## The Landlock tier
//!
//! Where bubblewrap can't create a user namespace (Ubuntu 24.04 restricts
//! unprivileged user namespaces through AppArmor and ships no profile for
//! `bwrap`; a kernel with them disabled), [`LandlockTier`] runs the same
//! helper **without namespaces**, as a weaker but real sandbox (see
//! [`super::landlock_rules`] for the filesystem and TCP rules):
//!
//! - Landlock filesystem rules at the highest ABI the kernel has: reads
//!   carved around the denied paths, writes only beneath the worktree
//!   (minus `.git` and the protected paths), the scratch dir and the turn
//!   home;
//! - Landlock network rules (ABI ≥ 4, required for this tier): TCP connect
//!   only to the egress proxy, the Ligus MCP listener and allow-listed
//!   loopback endpoints; no TCP bind;
//! - Landlock scoping (ABI ≥ 6): no signals to, and no abstract unix
//!   sockets of, processes outside the sandbox;
//! - seccomp: everything the bubblewrap tier denies, plus every namespace
//!   flag on `unshare`/`clone`, `setns`, and, for any child whose network is
//!   restricted, every socket but TCP over IPv4/IPv6 and route netlink (no
//!   UDP, so no DNS or QUIC of its own: names resolve at the proxy; no raw,
//!   packet, SCTP or MPTCP sockets), `listen`, and TCP Fast Open (which
//!   would connect without `connect()`); `io_uring` (whose operations
//!   bypass seccomp) is unavailable in both tiers;
//! - `no_new_privs`, a new session, the env allow-list, and death with the
//!   parent.
//!
//! What it can't do that bubblewrap does: hide other processes (no PID
//! namespace; Landlock still refuses `ptrace`-class access to them, which
//! covers `/proc/<pid>/environ`, `mem` and `fd`), restrict TCP by address
//! (Landlock matches ports only), give the agent its own ptys, or let it
//! create new entries in the worktree's top-level directory.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

use super::{canonical, host_paths, plan_env, EgressPlan, EnforcementPlan, ExecutorKind, TurnExecutor};
use crate::egress::EgressProxy;
use crate::ligus_mcp::bridge::SANDBOX_TCP_PORT;

/// Whether this device lets confined children create unix sockets
/// (`LIGUS_SANDBOX_UNIX_SOCKETS=allow`; off by default, C-10).
fn unix_sockets_allowed() -> bool {
    ligus_core::names::var("LIGUS_SANDBOX_UNIX_SOCKETS").is_ok_and(|v| v.trim().eq_ignore_ascii_case("allow"))
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

/// Where the docs explain the optional AppArmor profile for bubblewrap.
pub const APPARMOR_DOCS: &str = "https://docs.langstroth.ai/operate/ops/linux-sandbox/#apparmor-profile";

pub struct Bwrap {
    bwrap: PathBuf,
}

/// Why bubblewrap couldn't start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BwrapUnavailable {
    pub reason: String,
    /// AppArmor's unprivileged-user-namespace restriction is the cause (an
    /// AppArmor profile for `bwrap` fixes it).
    pub apparmor: bool,
}

fn read_sysctl(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Explain a failed bubblewrap probe from its stderr and the host's
/// user-namespace sysctls.
pub fn bwrap_failure(stderr: &str, apparmor_restrict: Option<&str>, userns_clone: Option<&str>, max_userns: Option<&str>) -> BwrapUnavailable {
    let denied = stderr.contains("Permission denied") || stderr.contains("Operation not permitted");
    if apparmor_restrict == Some("1") && denied {
        return BwrapUnavailable {
            reason: "bubblewrap can't create a user namespace: AppArmor restricts unprivileged user namespaces on this host \
                     (kernel.apparmor_restrict_unprivileged_userns=1) and there is no AppArmor profile for bwrap \
                     (Ubuntu 24.04 ships none; 24.10 and later do)"
                .into(),
            apparmor: true,
        };
    }
    if userns_clone == Some("0") {
        return BwrapUnavailable { reason: "unprivileged user namespaces are disabled (kernel.unprivileged_userns_clone=0)".into(), apparmor: false };
    }
    if max_userns == Some("0") {
        return BwrapUnavailable { reason: "user namespaces are disabled (user.max_user_namespaces=0)".into(), apparmor: false };
    }
    let first = stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("no error output");
    BwrapUnavailable { reason: format!("bwrap can't create namespaces here ({first})"), apparmor: false }
}

impl Bwrap {
    /// `bwrap` on PATH, and able to create a user namespace here.
    pub fn probe() -> Result<Self, BwrapUnavailable> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let bwrap = std::env::split_paths(&path)
            .map(|d| d.join("bwrap"))
            .find(|p| p.is_file())
            .ok_or_else(|| BwrapUnavailable { reason: "bubblewrap (bwrap) is not installed".into(), apparmor: false })?;
        let out = std::process::Command::new(&bwrap)
            .args(["--ro-bind", "/", "/", "--dev", "/dev", "--unshare-net", "--unshare-pid", "--die-with-parent", "--", "true"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output();
        match out {
            Ok(o) if o.status.success() => Ok(Self { bwrap }),
            Ok(o) => Err(bwrap_failure(
                &String::from_utf8_lossy(&o.stderr),
                read_sysctl("/proc/sys/kernel/apparmor_restrict_unprivileged_userns").as_deref(),
                read_sysctl("/proc/sys/kernel/unprivileged_userns_clone").as_deref(),
                read_sysctl("/proc/sys/user/max_user_namespaces").as_deref(),
            )),
            Err(e) => Err(BwrapUnavailable { reason: format!("bwrap failed to start: {e}"), apparmor: false }),
        }
    }
}

/// The kernel's Landlock ABI version (0: no Landlock).
pub fn kernel_landlock_abi() -> i32 {
    // landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)
    // SAFETY: a version query; no pointers are read or written.
    let r = unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, std::ptr::null::<u8>(), 0usize, 1u32) };
    if r < 0 {
        0
    } else {
        r as i32
    }
}

/// Seccomp filters are available (`PR_GET_SECCOMP` answers).
fn seccomp_available() -> bool {
    // SAFETY: a read-only prctl.
    unsafe { libc::prctl(libc::PR_GET_SECCOMP, 0, 0, 0, 0) >= 0 }
}

/// The Landlock tier: the helper with Landlock and seccomp, no namespaces.
pub struct LandlockTier {
    abi: i32,
}

/// The first Landlock ABI with network rules (Linux 6.7), required for the
/// Landlock tier: without them nothing would stop an agent's own TCP.
pub const LANDLOCK_MIN_ABI: i32 = 4;

impl LandlockTier {
    pub fn probe() -> Result<Self, String> {
        let abi = kernel_landlock_abi();
        if abi < 1 {
            return Err("Landlock is not enabled in this kernel (needs Linux 6.7+ with lsm=…,landlock)".into());
        }
        if abi < LANDLOCK_MIN_ABI {
            return Err(format!("this kernel's Landlock (ABI {abi}) has no network rules; the Landlock tier needs ABI {LANDLOCK_MIN_ABI} (Linux 6.7+)"));
        }
        if !seccomp_available() {
            return Err("seccomp filters are not available in this kernel".into());
        }
        Ok(Self { abi })
    }

    pub fn abi(&self) -> i32 {
        self.abi
    }

    /// What this tier leaves open on this kernel.
    pub fn notes(&self) -> Vec<String> {
        let mut n = vec![
            "no namespaces: the agent sees the host's processes and their command lines (Landlock still blocks ptrace-class access: their environment, memory and open files)".to_string(),
            "TCP is limited by port, not address: an agent can reach any host on the egress proxy's or the Ligus MCP listener's port number".to_string(),
            "UDP, raw and other non-TCP sockets are refused (no DNS of its own: names resolve at the egress proxy)".to_string(),
            "nothing can be created, removed or renamed directly in the worktree's top-level directory (subdirectories and existing files are writable)".to_string(),
            "no terminals (/dev/pts) for the agent".to_string(),
        ];
        if self.abi < 6 {
            n.push(format!("Landlock ABI {}: no signal or abstract-socket scoping (Linux 6.12+), so an agent can signal your other processes", self.abi));
        }
        n
    }
}

impl TurnExecutor for LandlockTier {
    fn kind(&self) -> ExecutorKind {
        ExecutorKind::Landlock
    }

    fn confine(&self, program: &OsStr, args: &[OsString], cwd: &Path, plan: &EnforcementPlan, proxy: Option<&EgressProxy>) -> io::Result<std::process::Command> {
        let helper = helper_exe()?;
        let mcp_port = plan.ligus_mcp_socket.as_deref().and_then(crate::ligus_mcp::socket::tcp_port_for);
        let home = super::home_dir();
        let rules = super::landlock_rules::rules(plan, home.as_deref(), proxy.map(EgressProxy::port), mcp_port);
        let file = rules.write_file(&canonical(&plan.tmp))?;
        let mut c = ligus_core::process_util::command(&helper);
        c.arg(HELPER_ARG).arg("--landlock-rules").arg(file);
        if !matches!(plan.egress, EgressPlan::Open) && !unix_sockets_allowed() {
            c.arg("--deny-unix");
        }
        c.arg("--").arg(program).args(args).current_dir(cwd).env_clear().envs(plan_env(plan, proxy));
        super::new_session(&mut c);
        Ok(c)
    }
}

/// Pick this host's Linux tier: bubblewrap, else Landlock, else none.
/// `LIGUS_SANDBOX=landlock` skips bubblewrap; `LIGUS_SANDBOX=bwrap` refuses
/// the Landlock fallback.
pub fn select_executor() -> (std::sync::Arc<dyn TurnExecutor>, super::SandboxStatus) {
    use super::SandboxStatus;
    use std::sync::Arc;
    let mode = ligus_core::names::var("LIGUS_SANDBOX").map(|v| v.trim().to_ascii_lowercase()).unwrap_or_default();
    let bwrap_err = if mode == "landlock" {
        BwrapUnavailable { reason: "LIGUS_SANDBOX=landlock skips bubblewrap".into(), apparmor: false }
    } else {
        match Bwrap::probe() {
            Ok(b) => {
                let s = SandboxStatus { kind: ExecutorKind::Bwrap, summary: "bubblewrap + Landlock + seccomp (namespaces)".into(), notes: Vec::new() };
                return (Arc::new(b), s);
            }
            Err(e) => e,
        }
    };
    let mut notes = Vec::new();
    if bwrap_err.apparmor {
        notes.push(format!("to restore the bubblewrap tier, install the optional AppArmor profile for bwrap: {APPARMOR_DOCS}"));
    }
    if mode == "bwrap" {
        return (Arc::new(super::Unconfined), SandboxStatus::unconfined(format!("{}; LIGUS_SANDBOX=bwrap refuses the Landlock fallback", bwrap_err.reason), notes));
    }
    match LandlockTier::probe() {
        Ok(l) => {
            notes.extend(l.notes());
            let s = SandboxStatus {
                kind: ExecutorKind::Landlock,
                summary: format!("Landlock (ABI {}) + seccomp, without namespaces, because {}", l.abi(), bwrap_err.reason),
                notes,
            };
            (Arc::new(l), s)
        }
        Err(le) => (Arc::new(super::Unconfined), SandboxStatus::unconfined(format!("{}; and {le}", bwrap_err.reason), notes)),
    }
}

/// Set once `maybe_run_helper` has run in this process and returned: this
/// executable's `main` dispatches the helper, so it can be re-executed as one.
/// A libtest harness (unit or integration tests) never sets it.
static DISPATCHES_HELPER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The helper binary's name, for a host that ships it on its own.
pub const HELPER_BIN: &str = "ligus-sandbox-helper";

/// The binary that runs the helper. See [`resolve_helper_exe`].
fn helper_exe() -> io::Result<PathBuf> {
    let me = std::env::current_exe()?;
    resolve_helper_exe(HelperLookup {
        explicit: host_paths().helper_exe,
        me: &me,
        dispatches_helper: DISPATCHES_HELPER.load(std::sync::atomic::Ordering::Relaxed),
        env: &|k| std::env::var_os(k),
    })
}

/// What [`resolve_helper_exe`] looks at (split out so it is testable).
pub struct HelperLookup<'a> {
    /// `set_helper_exe`.
    pub explicit: Option<PathBuf>,
    /// This process's executable.
    pub me: &'a Path,
    /// Whether this process's `main` ran [`maybe_run_helper`].
    pub dispatches_helper: bool,
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
}

/// The binary the sandbox re-executes as its pre-exec helper:
///
/// 1. an explicit one (`set_helper_exe`);
/// 2. a `ligus-sandbox-helper` installed next to this executable (or one
///    level up);
/// 3. this executable, but only when it can be one: its `main` ran
///    [`maybe_run_helper`] (the `ligus` CLI, so `serve`, `worker` and the
///    container's `/usr/local/bin/ligus`, and the desktop app), or it is
///    `ligus-sandbox-helper` itself;
/// 4. otherwise this process is a test harness (or a host that doesn't carry
///    the helper): the `ligus-sandbox-helper` or `ligus` binary cargo built
///    for the run, from nextest's / cargo's `*_BIN_EXE_*` variables or found
///    near the harness ([`find_built_helper`]);
/// 5. else an error saying so. A test harness is never run as the helper: it
///    would take the helper's options as its own and every confined spawn
///    would fail with "Unrecognized option".
pub fn resolve_helper_exe(l: HelperLookup<'_>) -> io::Result<PathBuf> {
    if let Some(p) = l.explicit {
        return Ok(p);
    }
    let me = l.me;
    for dir in [me.parent(), me.parent().and_then(Path::parent)].into_iter().flatten() {
        let sibling = dir.join(HELPER_BIN);
        if sibling.is_file() && !same_file(&sibling, me) {
            return Ok(sibling);
        }
    }
    if l.dispatches_helper || me.file_stem().is_some_and(|s| s == HELPER_BIN) {
        return Ok(me.to_path_buf());
    }
    let names = [HELPER_BIN, "ligus"];
    let from_env = names.iter().find_map(|n| {
        let vars = [format!("NEXTEST_BIN_EXE_{n}"), format!("NEXTEST_BIN_EXE_{}", n.replace('-', "_")), format!("CARGO_BIN_EXE_{n}")];
        vars.iter().filter_map(|v| (l.env)(v)).map(PathBuf::from).find(|p| p.is_file() && !same_file(p, me))
    });
    if let Some(p) = from_env.or_else(|| find_built_helper(me, &names)) {
        return Ok(p);
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "{} doesn't carry Ligus's Linux sandbox helper and there is no {HELPER_BIN} or ligus binary near it; \
             a test run needs cargo to build the {HELPER_BIN} bin (a ligus-runtime or workspace test run does), \
             and a host needs {HELPER_BIN} installed next to its executable",
            me.display()
        ),
    ))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// For tests: the first of `names` among the binaries cargo built for the
/// test harness `exe`. Cargo puts bins in the profile's artifact dir
/// (`target/debug`), and the harness either in `target/debug/deps/` or, with
/// cargo's build-dir layout (cargo 1.9x and later),
/// `target/debug/build/<package>/<hash>/out/` (or `.../<hash>/deps/`); so
/// look up to five levels above the harness. Never returns `exe` itself.
pub fn find_built_helper(exe: &Path, names: &[&str]) -> Option<PathBuf> {
    exe.ancestors().skip(1).take(5).find_map(|dir| names.iter().map(|n| dir.join(n)).find(|p| p.is_file() && !same_file(p, exe)))
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
    // Other turns' scratch dirs (their homes hold their CLIs' sign-ins) are
    // an empty tmpfs, this turn's own bound back in below. A turn that starts
    // later makes its dir on the host, which this mount doesn't show.
    let turns_root = super::sibling_turns_root(&plan.tmp);
    if let Some(root) = &turns_root {
        let tmp = canonical(&plan.tmp);
        push(&["--tmpfs".as_ref(), root.as_os_str(), "--bind".as_ref(), tmp.as_os_str(), tmp.as_os_str()]);
    }
    let wt = canonical(&plan.worktree);
    let empty = placeholder_dir(plan);
    // The worktree: read-write with its protected paths read-only on top (a
    // missing one pinned by an empty read-only dir, so the agent can't create
    // it; git ignores the empty dir and the guard removes it afterwards), or
    // read-only.
    let worktree = |a: &mut Vec<OsString>| {
        let mut push = |xs: &[&OsStr]| a.extend(xs.iter().map(|x| x.to_os_string()));
        if plan.worktree_writable {
            push(&["--bind".as_ref(), wt.as_os_str(), wt.as_os_str()]);
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
    };
    // Phase C: the home dir is an empty tmpfs; what the plan lets back in is
    // bound over it (read-only, shallow first), then the worktree and the
    // writable extras. The hard denies come after all of these, so a grant
    // of a parent dir never re-exposes a denied path inside it.
    let home = plan.home.as_ref().map(|h| canonical(h)).filter(|h| h.is_dir());
    if let Some(h) = &home {
        push(&["--tmpfs".as_ref(), h.as_os_str()]);
        let mut ro: Vec<PathBuf> = plan.home_read.iter().filter(|p| super::unmoved(p)).map(|p| canonical(p)).filter(|p| p.starts_with(h) && p.exists()).collect();
        ro.sort_by_key(|p| p.components().count());
        ro.dedup();
        for r in &ro {
            push(&["--ro-bind".as_ref(), r.as_os_str(), r.as_os_str()]);
        }
    }
    worktree(&mut a);
    let mut push = |xs: &[&OsStr]| a.extend(xs.iter().map(|x| x.to_os_string()));
    for w in plan.write_extra.iter().filter(|w| super::unmoved(w)) {
        let w = canonical(w);
        if w.exists() {
            push(&["--bind".as_ref(), w.as_os_str(), w.as_os_str()]);
        }
    }
    let mut denied: Vec<PathBuf> = plan.deny_read.iter().map(|p| canonical(p)).collect();
    if let Some(run) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        denied.push(canonical(&run));
    }
    // Shallow first, so a nested deny isn't hidden under a parent's tmpfs.
    denied.sort_by_key(|p| p.components().count());
    // A denied path under the home is already gone unless something bound
    // above brought its parent back.
    let visible = |d: &Path| match &home {
        Some(h) if d.starts_with(h) => {
            plan.home_read.iter().chain(plan.write_extra.iter()).chain(std::iter::once(&plan.worktree)).any(|b| d.starts_with(canonical(b)))
        }
        _ => true,
    };
    for d in &denied {
        if !visible(d) {
            continue;
        }
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
            if masked.contains(&c) || denied.iter().any(|d| c.starts_with(d)) || home.as_ref().is_some_and(|h| c.starts_with(h)) {
                continue;
            }
            if std::fs::metadata(&c).is_ok_and(|m| m.file_type().is_socket()) {
                push(&["--ro-bind".as_ref(), "/dev/null".as_ref(), c.as_os_str()]);
                masked.push(c);
            }
        }
    }
    // Readable inside a denied dir (a worker's checkouts under its data dir),
    // and the worktree again when it sits inside one.
    for r in &plan.allow_read {
        let r = canonical(r);
        if r.exists() && denied.iter().any(|d| r.starts_with(d)) && r != wt {
            push(&["--ro-bind".as_ref(), r.as_os_str(), r.as_os_str()]);
        }
    }
    if denied.iter().any(|d| wt.starts_with(d)) {
        worktree(&mut a);
    }
    let mut push = |xs: &[&OsStr]| a.extend(xs.iter().map(|x| x.to_os_string()));
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
        let mut c = ligus_core::process_util::command(&self.bwrap);
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
        DISPATCHES_HELPER.store(true, std::sync::atomic::Ordering::Relaxed);
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
    /// The Landlock tier: its rules file (no bubblewrap around the helper).
    landlock_rules: Option<PathBuf>,
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
    let mut o = HelperOpts { rw: Vec::new(), landlock_rules: None, mcp_sock: None, deny_unix: false, proxy_sock: None, proxy_port: None, token: String::new(), loop_ports: Vec::new(), cmd: Vec::new() };
    let mut it = args.into_iter();
    let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidInput, m.to_string());
    while let Some(a) = it.next() {
        let s = a.to_string_lossy().into_owned();
        let mut val = || it.next().ok_or_else(|| bad(&format!("{s} needs a value")));
        match s.as_str() {
            "--rw" => o.rw.push(PathBuf::from(val()?)),
            "--landlock-rules" => o.landlock_rules = Some(PathBuf::from(val()?)),
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

/// What the seccomp filter refuses beyond the base list.
#[derive(Debug, Clone, Copy, Default)]
struct SeccompOpts {
    /// `socket(AF_UNIX, …)` (C-10).
    deny_unix: bool,
    /// The Landlock tier with a restricted network: only TCP (and route
    /// netlink) sockets, no `listen`, no TCP Fast Open.
    restrict_net: bool,
    /// The Landlock tier: every namespace flag, not just `CLONE_NEWUSER`.
    all_namespaces: bool,
}

/// `kexec_file_load`. The `libc` crate doesn't define it for aarch64 musl, so
/// use the kernel's number there (asm-generic `__NR_kexec_file_load`).
#[cfg(all(target_arch = "aarch64", target_env = "musl"))]
const SYS_KEXEC_FILE_LOAD: libc::c_long = 294;
#[cfg(not(all(target_arch = "aarch64", target_env = "musl")))]
const SYS_KEXEC_FILE_LOAD: libc::c_long = libc::SYS_kexec_file_load;

fn apply_seccomp(o: SeccompOpts) -> io::Result<()> {
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
        SYS_KEXEC_FILE_LOAD,
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_setns,
    ] {
        deny.insert(nr, vec![]);
    }
    let cond = |idx: u8, len: SeccompCmpArgLen, op: SeccompCmpOp, v: u64| SeccompCondition::new(idx, len, op, v).map_err(be);
    let rule = |c: Vec<SeccompCondition>| SeccompRule::new(c).map_err(be);
    let ns_flags: Vec<u64> = if o.all_namespaces {
        [libc::CLONE_NEWUSER, libc::CLONE_NEWNS, libc::CLONE_NEWPID, libc::CLONE_NEWNET, libc::CLONE_NEWIPC, libc::CLONE_NEWUTS, libc::CLONE_NEWCGROUP]
            .iter()
            .map(|f| *f as u64)
            .collect()
    } else {
        vec![libc::CLONE_NEWUSER as u64]
    };
    let flag_rules = |extra: &[u64]| -> io::Result<Vec<SeccompRule>> {
        ns_flags.iter().chain(extra).map(|f| rule(vec![cond(0, SeccompCmpArgLen::Qword, SeccompCmpOp::MaskedEq(*f), *f)?])).collect()
    };
    // CLONE_NEWTIME (0x80) shares clone()'s exit-signal byte, so it is
    // matched on unshare only.
    let newtime: &[u64] = if o.all_namespaces { &[0x80] } else { &[] };
    deny.insert(libc::SYS_unshare, flag_rules(newtime)?);
    deny.insert(libc::SYS_clone, flag_rules(&[])?);
    let mut socket_rules: Vec<SeccompRule> = Vec::new();
    if o.deny_unix {
        // C-10: socket(AF_UNIX, …). socketpair (local IPC) stays allowed.
        socket_rules.push(rule(vec![cond(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, libc::AF_UNIX as u64)?])?);
    }
    if o.restrict_net {
        let (inet, inet6, unix, netlink) = (libc::AF_INET as u64, libc::AF_INET6 as u64, libc::AF_UNIX as u64, libc::AF_NETLINK as u64);
        // Any family but IPv4, IPv6, unix (decided above) and netlink.
        socket_rules.push(rule(vec![
            cond(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Ne, inet)?,
            cond(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Ne, inet6)?,
            cond(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Ne, unix)?,
            cond(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Ne, netlink)?,
        ])?);
        // Netlink: route only (interface lists; changing them needs
        // CAP_NET_ADMIN).
        socket_rules.push(rule(vec![
            cond(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, netlink)?,
            cond(2, SeccompCmpArgLen::Dword, SeccompCmpOp::Ne, libc::NETLINK_ROUTE as u64)?,
        ])?);
        for fam in [inet, inet6] {
            // Not a stream socket: SOCK_DGRAM (UDP, ICMP), SOCK_RAW, SOCK_RDM,
            // SOCK_SEQPACKET, SOCK_DCCP (6), SOCK_PACKET (10). The type's
            // low four bits; the high ones are SOCK_NONBLOCK/SOCK_CLOEXEC.
            for ty in [libc::SOCK_DGRAM as u64, libc::SOCK_RAW as u64, libc::SOCK_RDM as u64, libc::SOCK_SEQPACKET as u64, 6, 10] {
                socket_rules.push(rule(vec![cond(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, fam)?, cond(1, SeccompCmpArgLen::Dword, SeccompCmpOp::MaskedEq(0xf), ty)?])?);
            }
            // A stream socket that isn't TCP (SCTP, MPTCP): Landlock's
            // port rules cover TCP only.
            socket_rules.push(rule(vec![
                cond(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, fam)?,
                cond(2, SeccompCmpArgLen::Dword, SeccompCmpOp::Ne, 0)?,
                cond(2, SeccompCmpArgLen::Dword, SeccompCmpOp::Ne, libc::IPPROTO_TCP as u64)?,
            ])?);
        }
        // No inbound connections (an unbound socket's listen() would pick
        // a port without the bind Landlock refuses).
        deny.insert(libc::SYS_listen, vec![]);
        // TCP Fast Open connects from sendto/sendmsg, past Landlock's
        // connect hook.
        let fastopen = libc::MSG_FASTOPEN as u64;
        deny.insert(libc::SYS_sendto, vec![rule(vec![cond(3, SeccompCmpArgLen::Dword, SeccompCmpOp::MaskedEq(fastopen), fastopen)?])?]);
        deny.insert(libc::SYS_sendmsg, vec![rule(vec![cond(2, SeccompCmpArgLen::Dword, SeccompCmpOp::MaskedEq(fastopen), fastopen)?])?]);
        deny.insert(libc::SYS_sendmmsg, vec![rule(vec![cond(3, SeccompCmpArgLen::Dword, SeccompCmpOp::MaskedEq(fastopen), fastopen)?])?]);
    }
    if !socket_rules.is_empty() {
        deny.insert(libc::SYS_socket, socket_rules);
    }
    let f1 = SeccompFilter::new(deny, SeccompAction::Allow, SeccompAction::Errno(libc::EPERM as u32), arch).map_err(be)?;
    let mut enosys: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    // clone3 takes its flags in memory seccomp can't read: ENOSYS makes libc
    // fall back to clone. io_uring operations (socket, connect, open) run
    // without the syscalls this filter sees; ENOSYS makes libuv & co. fall
    // back to plain syscalls.
    for nr in [libc::SYS_clone3, libc::SYS_io_uring_setup, libc::SYS_io_uring_enter, libc::SYS_io_uring_register] {
        enosys.insert(nr, vec![]);
    }
    let f2 = SeccompFilter::new(enosys, SeccompAction::Allow, SeccompAction::Errno(libc::ENOSYS as u32), arch).map_err(be)?;
    let p1: BpfProgram = f1.try_into().map_err(be)?;
    let p2: BpfProgram = f2.try_into().map_err(be)?;
    #[cfg(target_arch = "x86_64")]
    seccompiler::apply_filter(&x32_filter()).map_err(e)?;
    seccompiler::apply_filter(&p1).map_err(e)?;
    seccompiler::apply_filter(&p2).map_err(e)?;
    Ok(())
}

/// x86-64 only: refuse the x32 ABI (syscall numbers with bit 30 set), whose
/// numbers the lists above don't name. 32-bit (i386) calls fail the
/// filters' architecture check and kill the process.
#[cfg(target_arch = "x86_64")]
fn x32_filter() -> seccompiler::BpfProgram {
    use seccompiler::sock_filter;
    const AUDIT_ARCH_X86_64: u32 = 62 | 0x8000_0000 | 0x4000_0000;
    let f = |code: u16, jt: u8, jf: u8, k: u32| sock_filter { code, jt, jf, k };
    vec![
        f(0x20, 0, 0, 4),                    // ld [arch]
        f(0x15, 0, 3, AUDIT_ARCH_X86_64),    // jeq x86_64 ? next : allow
        f(0x20, 0, 0, 0),                    // ld [nr]
        f(0x45, 0, 1, 0x4000_0000),          // jset x32 bit ? deny : allow
        f(0x06, 0, 0, 0x0005_0000 | libc::EPERM as u32), // ret ERRNO(EPERM)
        f(0x06, 0, 0, 0x7fff_0000),          // ret ALLOW
    ]
}

/// The Landlock tier's filesystem, network and scope rules. Fails closed:
/// a kernel without Landlock, or without network rules for a turn whose
/// network must be restricted, runs nothing.
fn apply_landlock_tier(r: &super::landlock_rules::LandlockRules) -> io::Result<()> {
    use landlock::{path_beneath_rules, Access, AccessFs, AccessNet, NetPort, Ruleset, RulesetAttr, RulesetCreatedAttr, Scope, ABI};
    let kernel = kernel_landlock_abi();
    if kernel < 1 {
        return Err(io::Error::other("Landlock is not available on this kernel"));
    }
    if r.restrict_net && kernel < LANDLOCK_MIN_ABI {
        return Err(io::Error::other(format!("this kernel's Landlock (ABI {kernel}) can't restrict the network")));
    }
    // The newest ABI this crate knows, best effort: rights the kernel lacks
    // are dropped (`sandbox_status` names what that leaves open).
    let abi = ABI::V9;
    let err = |e: landlock::RulesetError| io::Error::other(e.to_string());
    let mut rs = Ruleset::default().handle_access(AccessFs::from_all(abi)).map_err(err)?;
    if r.restrict_net {
        rs = rs.handle_access(AccessNet::BindTcp | AccessNet::ConnectTcp).map_err(err)?;
    }
    let mut c = rs
        .scope(Scope::AbstractUnixSocket | Scope::Signal)
        .map_err(err)?
        .create()
        .map_err(err)?
        .add_rules(verified_rules(&r.read, AccessFs::from_read(abi), abi))
        .map_err(err)?
        .add_rules(verified_rules(&r.write, AccessFs::from_all(abi), abi))
        .map_err(err)?
        .add_rules(path_beneath_rules(&r.dev, AccessFs::ReadFile | AccessFs::WriteFile | AccessFs::Truncate | AccessFs::IoctlDev))
        .map_err(err)?;
    if r.restrict_net {
        for p in &r.connect {
            c = c.add_rule(NetPort::new(*p, AccessNet::ConnectTcp)).map_err(err)?;
        }
    }
    let status = c.restrict_self().map_err(err)?;
    if status.ruleset == landlock::RulesetStatus::NotEnforced || !status.no_new_privs {
        return Err(io::Error::other("Landlock could not be enforced"));
    }
    Ok(())
}

/// Landlock rules on exactly the paths the parent judged. Each is opened
/// without following a final symlink and kept only when the descriptor
/// still resolves to that path (`/proc/self/fd`): a path swapped for a link,
/// or under a parent swapped for one, between the rules and here (a
/// concurrent turn with write access beside it) opens nothing rather than
/// the link's target. Without `/proc` the paths are opened as given. Lazy,
/// like `path_beneath_rules`: one descriptor open at a time (a carved rule
/// set runs to thousands of paths, past the descriptor limit).
fn verified_rules(
    paths: &[PathBuf],
    access: landlock::BitFlags<landlock::AccessFs>,
    abi: landlock::ABI,
) -> impl Iterator<Item = Result<landlock::PathBeneath<std::os::fd::OwnedFd>, landlock::RulesetError>> + '_ {
    use landlock::{AccessFs, PathBeneath};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let proc_ok = Path::new("/proc/self/fd").is_dir();
    let file_access = access & AccessFs::from_file(abi);
    paths.iter().filter_map(move |p| {
        let flags = libc::O_PATH | libc::O_CLOEXEC | if proc_ok { libc::O_NOFOLLOW } else { 0 };
        let f = std::fs::OpenOptions::new().read(true).custom_flags(flags).open(p).ok()?;
        let meta = f.metadata().ok()?;
        if meta.file_type().is_symlink() {
            return None;
        }
        if proc_ok && std::fs::read_link(format!("/proc/self/fd/{}", f.as_raw_fd())).ok()? != *p {
            return None;
        }
        let a = if meta.is_dir() { access } else { file_access };
        Some(Ok(PathBeneath::new(std::os::fd::OwnedFd::from(f), a)))
    })
}

/// The Landlock tier: no bubblewrap around us. Apply Landlock and seccomp
/// to this process and become the command.
fn run_landlock_tier(o: &HelperOpts, rules_file: &Path) -> io::Result<i32> {
    use std::os::unix::process::CommandExt;
    let rules = super::landlock_rules::LandlockRules::decode(&std::fs::read(rules_file)?)?;
    // Die with the parent, as bwrap's --die-with-parent does.
    // SAFETY: a prctl on this process.
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
    }
    apply_landlock_tier(&rules)?;
    apply_seccomp(SeccompOpts { deny_unix: o.deny_unix, restrict_net: rules.restrict_net, all_namespaces: true })?;
    let mut cmd = std::process::Command::new(&o.cmd[0]);
    cmd.args(&o.cmd[1..]);
    Err(cmd.exec())
}

fn run_helper(args: Vec<OsString>) -> io::Result<i32> {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    let o = parse_helper(args)?;
    if let Some(f) = o.landlock_rules.clone() {
        return run_landlock_tier(&o, &f);
    }
    let forwarding = start_forwarders(&o)?;
    apply_landlock(&o.rw)?;
    apply_seccomp(SeccompOpts { deny_unix: o.deny_unix, ..Default::default() })?;
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
            home: None,
            home_read: vec![],
            write_extra: vec![],
            tmp: tmp.to_path_buf(),
            egress: EgressPlan::Proxy { allow: vec![HostPort::new("api.anthropic.com", 443), HostPort::new("127.0.0.1", 11434)] },
            env: vec![],
            args: vec![],
            mcp: vec![],
            ligus_mcp_socket: None,
            gateway: None,
            local_servers: false,
        }
    }

    fn touch(p: &Path) -> PathBuf {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"").unwrap();
        p.to_path_buf()
    }

    fn lookup(me: &Path, dispatches_helper: bool, env: &dyn Fn(&str) -> Option<OsString>) -> io::Result<PathBuf> {
        resolve_helper_exe(HelperLookup { explicit: None, me, dispatches_helper, env })
    }

    const NO_ENV: &dyn Fn(&str) -> Option<OsString> = &|_| None;

    /// The shipped hosts re-execute themselves: the `ligus` CLI (which is
    /// also `ligus serve` and `ligus worker`), the container image's
    /// `/usr/local/bin/ligus`, and the desktop app's binary all run
    /// `maybe_run_helper` first thing in `main`. A helper installed next to
    /// them wins.
    #[test]
    fn product_binaries_carry_the_helper() {
        let t = tempfile::tempdir().unwrap();
        for exe in ["usr/local/bin/ligus", "home/u/.local/bin/ligus", "usr/bin/ligus-desktop", "opt/Ligus/ligus-app"] {
            let me = touch(&t.path().join(exe));
            assert_eq!(lookup(&me, true, NO_ENV).unwrap(), me, "{exe}");
        }
        let me = touch(&t.path().join("opt/x/bin/ligus"));
        let helper = touch(&t.path().join("opt/x/bin").join(HELPER_BIN));
        assert_eq!(lookup(&me, true, NO_ENV).unwrap(), helper);
        // `ligus-sandbox-helper` itself.
        let solo = touch(&t.path().join("solo").join(HELPER_BIN));
        assert_eq!(lookup(&solo, false, NO_ENV).unwrap(), solo);
        // An explicit helper always wins.
        let explicit = t.path().join("explicit");
        let got = resolve_helper_exe(HelperLookup { explicit: Some(explicit.clone()), me: &me, dispatches_helper: true, env: NO_ENV }).unwrap();
        assert_eq!(got, explicit);
    }

    /// A libtest harness (unit or integration tests, either cargo layout)
    /// is never the helper: it gets the helper bin cargo built, or an error.
    #[test]
    fn a_test_harness_is_never_the_helper() {
        let t = tempfile::tempdir().unwrap();
        let debug = t.path().join("target/debug");
        let harnesses = [
            debug.join("deps/ligus_runtime-0123456789abcdef"),
            debug.join("deps/acp_stub-0123456789abcdef"),
            debug.join("build/ligus-runtime/0123456789abcdef/out/acp_stub-0123456789abcdef"),
            debug.join("build/ligus-runtime/0123456789abcdef/deps/acp_stub-0123456789abcdef"),
            debug.join("build/ligus-cli/0123456789abcdef/out/ligus-0123456789abcdef"),
        ];
        for h in &harnesses {
            touch(h);
            let err = lookup(h, false, NO_ENV).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::NotFound);
            assert!(err.to_string().contains(HELPER_BIN), "{err}");
        }
        // Only `ligus` built (a ligus-cli test run): that, since it carries the helper.
        let ligus = touch(&debug.join("ligus"));
        for h in &harnesses {
            assert_eq!(lookup(h, false, NO_ENV).unwrap(), ligus, "{}", h.display());
        }
        // The helper bin, once built, is preferred.
        let helper = touch(&debug.join(HELPER_BIN));
        for h in &harnesses {
            assert_eq!(lookup(h, false, NO_ENV).unwrap(), helper, "{}", h.display());
        }
    }

    /// A separate build dir (`CARGO_BUILD_BUILD_DIR`) puts the harness far
    /// from the bins: nextest's `NEXTEST_BIN_EXE_*` (or cargo's
    /// `CARGO_BIN_EXE_*`) names them.
    #[test]
    fn bin_exe_variables_name_the_helper() {
        let t = tempfile::tempdir().unwrap();
        let harness = touch(&t.path().join("build-dir/debug/build/ligus-runtime/0123456789abcdef/out/mcp_stdio-0123456789abcdef"));
        let helper = touch(&t.path().join("artifacts/debug").join(HELPER_BIN));
        assert!(lookup(&harness, false, NO_ENV).is_err());
        for var in ["NEXTEST_BIN_EXE_ligus-sandbox-helper", "NEXTEST_BIN_EXE_ligus_sandbox_helper", "CARGO_BIN_EXE_ligus-sandbox-helper"] {
            let h = helper.clone();
            let env = move |k: &str| (k == var).then(|| h.clone().into_os_string());
            assert_eq!(lookup(&harness, false, &env).unwrap(), helper, "{var}");
        }
        // A variable naming the harness itself is ignored.
        let me = harness.clone();
        let env = move |k: &str| (k == "NEXTEST_BIN_EXE_ligus").then(|| me.clone().into_os_string());
        assert!(lookup(&harness, false, &env).is_err());
    }

    /// This very process is a test harness: whatever the lookup returns, it
    /// isn't this executable.
    #[test]
    fn this_harness_is_not_its_own_helper() {
        let me = std::env::current_exe().unwrap();
        assert!(!DISPATCHES_HELPER.load(std::sync::atomic::Ordering::Relaxed));
        if let Ok(p) = helper_exe() {
            assert!(!same_file(&p, &me), "{} would run the test harness as the helper", p.display());
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

    /// The probe names AppArmor's user-namespace restriction (Ubuntu 24.04)
    /// and points at the profile; other causes are named too.
    #[test]
    fn bwrap_probe_explains_apparmor_userns_restriction() {
        let e = bwrap_failure("bwrap: setting up uid map: Permission denied\n", Some("1"), None, Some("63000"));
        assert!(e.apparmor && e.reason.contains("AppArmor") && e.reason.contains("apparmor_restrict_unprivileged_userns=1"), "{e:?}");
        let e = bwrap_failure("bwrap: No permissions to creating new namespace", Some("0"), Some("0"), None);
        assert!(!e.apparmor && e.reason.contains("unprivileged_userns_clone=0"), "{e:?}");
        let e = bwrap_failure("bwrap: Creating new namespace failed: No space left on device", None, None, Some("0"));
        assert!(e.reason.contains("max_user_namespaces=0"), "{e:?}");
        let e = bwrap_failure("bwrap: something else\n", None, None, None);
        assert!(e.reason.contains("something else") && !e.apparmor, "{e:?}");
    }

    /// The Landlock tier hands the helper a rules file in the turn's 0700
    /// scratch dir, never paths or the proxy token on its command line.
    #[test]
    fn landlock_tier_rules_stay_off_the_command_line() {
        let t = tempfile::tempdir().unwrap();
        let mut p = plan(t.path());
        p.worktree_writable = true;
        std::fs::create_dir_all(&p.worktree).unwrap();
        let proxy = EgressProxy::start(vec![HostPort::new("api.anthropic.com", 443)], None).unwrap();
        let c = LandlockTier { abi: 6 }.confine(OsStr::new("/bin/true"), &[], t.path(), &p, Some(&proxy)).unwrap();
        let args: Vec<String> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args[0], HELPER_ARG);
        assert!(args.windows(2).any(|w| w[0] == "--landlock-rules" && w[1].starts_with(&*canonical(t.path()).to_string_lossy())), "{args:?}");
        assert!(args.contains(&"--deny-unix".to_string()), "{args:?}");
        assert!(!args.iter().any(|a| a.contains(proxy.token())), "{args:?}");
        assert!(!c.get_envs().any(|(k, _)| k == PROXY_TOKEN_ENV), "the Landlock tier needs no token in the helper");
        let rules = crate::executor::landlock_rules::LandlockRules::decode(&std::fs::read(t.path().join(".ligus-landlock-rules")).unwrap()).unwrap();
        assert!(rules.restrict_net && rules.connect.contains(&proxy.port()) && rules.connect.contains(&11434), "{rules:?}");
        let o = parse_helper(c.get_args().skip(1).map(|a| a.to_os_string()).collect()).unwrap();
        assert!(o.landlock_rules.is_some() && o.deny_unix && o.cmd == vec![OsString::from("/bin/true")]);
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

    /// Phase C: the home is an empty tmpfs, what the plan lets back in is
    /// bound over it, and the hard denies come after every bind, so a grant
    /// of a parent dir never re-exposes a denied path inside it.
    #[test]
    fn bwrap_home_is_a_tmpfs_and_hard_denies_come_last() {
        let t = tempfile::tempdir().unwrap();
        let root = canonical(t.path());
        let home = root.join("home");
        for d in [".cargo/bin", "granted/sub", "granted/sub/.ssh", "projects/ws", "data"] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        let mut p = plan(&root);
        p.worktree = home.join("projects/ws");
        p.worktree_writable = true;
        p.home = Some(home.clone());
        p.home_read = vec![home.join(".cargo/bin")];
        p.write_extra = vec![home.join("granted")];
        p.deny_read = vec![home.join("granted/sub/.ssh"), home.join("data")];
        let a: Vec<String> = bwrap_args(&p, &p.worktree).iter().map(|x| x.to_string_lossy().into_owned()).collect();
        let at = |flag: &str, path: &Path| a.windows(2).position(|w| w[0] == flag && w[1] == path.to_string_lossy()).unwrap_or_else(|| panic!("{flag} {}: {a:?}", path.display()));
        let tmpfs_home = at("--tmpfs", &home);
        let ro = a.windows(3).position(|w| w[0] == "--ro-bind" && w[1] == home.join(".cargo/bin").to_string_lossy()).unwrap();
        let wt = a.windows(3).position(|w| w[0] == "--bind" && w[1] == home.join("projects/ws").to_string_lossy()).unwrap();
        let extra = a.windows(3).position(|w| w[0] == "--bind" && w[1] == home.join("granted").to_string_lossy()).unwrap();
        let ssh = at("--tmpfs", &home.join("granted/sub/.ssh"));
        assert!(tmpfs_home < ro && ro < wt && wt < extra && extra < ssh, "{a:?}");
        // A denied dir the home's tmpfs already hides (nothing bound above it)
        // isn't mounted again.
        assert!(!a.windows(2).any(|w| w[0] == "--tmpfs" && w[1] == home.join("data").to_string_lossy()), "{a:?}");
        // Without a home to deny, no home tmpfs.
        p.home = None;
        let a: Vec<String> = bwrap_args(&p, &p.worktree).iter().map(|x| x.to_string_lossy().into_owned()).collect();
        assert!(!a.windows(2).any(|w| w[0] == "--tmpfs" && w[1] == home.to_string_lossy()), "{a:?}");
    }
}

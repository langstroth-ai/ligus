//! macOS: Seatbelt (`sandbox-exec -p <profile>`).
//!
//! The profile is generated per child from the [`EnforcementPlan`] and is
//! never read from the repository. `sandbox-exec` is deprecated but still
//! shipped, and Claude Code and Codex use the same mechanism; the
//! `sandbox_*` integration tests pin its behaviour on the macOS runner.
//!
//! Shape (later rules win in SBPL):
//!
//! ```scheme
//! (version 1)(allow default)
//! (deny file-write*)
//! (allow file-write* (subpath "<worktree>") (subpath "<turn tmp>") <state dirs> <ttys, /dev/null>)
//! (deny file-read* file-write* (subpath "<home>"))   ; Phase C: the home is default-deny
//! (allow file-read* <worktree, turn dirs, write extras, home_read under the home>)
//! (allow file-write* <worktree and write extras under the home>)
//! (allow file-read-metadata (literal "<home>") <their ancestors>)  ; stat/realpath, no listing
//! (deny file-write* (subpath "<worktree>/.git") <protected dirs and files>
//!   (regex #"^<worktree>/(.*/)?[.][gG][iI][tT](/.*)?$") …)  ; each name, any depth, any case
//! (deny file-read* file-write* (subpath "<data dir>") (subpath "~/.ssh") (literal "<serve.token>") …)
//! (allow file-read* (subpath "<worktree>") <readable dirs inside a denied one>)
//! (deny mach-lookup) (allow mach-lookup <MACH_ALLOW> [keychain services])  ; default-deny lookups
//! (deny mach-lookup (global-name-prefix "com.apple.coreservices.") (global-name "com.apple.runningboard"))
//!                                                    ; no LaunchServices launches (not even suspended)
//! (deny mach-lookup (xpc-service-name-regex #"."))   ; no bundled XPC services (launchd spawns them)
//! (deny signal) (allow signal (target same-sandbox)) ; can't resume or kill outside processes
//! (deny user-preference-write)                       ; no cfprefsd writes on the agent's behalf
//! (deny mach-lookup (global-name-prefix "com.apple.siri."))  ; no `shortcuts run`
//! (deny appleevent-send) (deny mach-lookup … "com.apple.pasteboard…")  ; no Apple Events, no pasteboard
//! (deny network*)                                   ; proxied/closed egress
//! (deny network-outbound (remote unix-socket)) …     ; open egress: no host unix sockets (but DNS)
//! (allow network-outbound (remote ip "localhost:<proxy port>"))
//! (allow network-outbound (remote unix-socket (path-literal "<ligus mcp socket>")))
//! ```
//!
//! Keychain: the login keychain's files are unreadable (so it can't be
//! opened) except to a subscription CLI whose sign-in still lives there
//! (Copilot, Antigravity, Claude Code with a custom config dir). Claude
//! Code's keychain sign-in is bridged into the turn home as a credentials
//! file (`cli_home`), and Codex signs in from `auth.json`, so their turns run
//! without it: any process that can run `/usr/bin/security` reads every item
//! that trusts it (the gh token, git's osxkeychain credentials) with no
//! prompt. Ligus's own items trust only the Ligus binaries.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;

use super::{canonical, plan_env, EgressPlan, EnforcementPlan, ExecutorKind, TurnExecutor};
use crate::egress::EgressProxy;

pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

pub struct Seatbelt;

/// Whether `sandbox-exec` is present.
pub fn available() -> bool {
    Path::new(SANDBOX_EXEC).exists()
}

fn q(p: &Path) -> String {
    let s = canonical(p).to_string_lossy().into_owned();
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// An SBPL regex for `s`, matched without regard to ASCII case (macOS file
/// systems are case-insensitive by default, so `.CLAUDE/` is `.claude/`).
fn ci_regex(s: &str) -> String {
    let mut r = String::new();
    for c in s.chars() {
        match c {
            c if c.is_ascii_alphabetic() => {
                r.push('[');
                r.push(c.to_ascii_lowercase());
                r.push(c.to_ascii_uppercase());
                r.push(']');
            }
            c if c.is_ascii_digit() || matches!(c, '/' | '-' | '_' | ' ') => r.push(c),
            ']' | '\\' | '^' => {
                r.push('\\');
                r.push(c);
            }
            c => {
                r.push('[');
                r.push(c);
                r.push(']');
            }
        }
    }
    r
}

/// The deny-write filters for the plan's read-only paths: each as given
/// (`subpath`), and each top-level protected name (`.git`, `.claude`,
/// `ligus.config.toml`, …) at any depth below the worktree, in any case.
fn protected_filters(plan: &EnforcementPlan) -> String {
    let mut p = String::new();
    for r in &plan.read_only {
        p.push_str(&format!(" (subpath {})", q(r)));
    }
    let wt = canonical(&plan.worktree);
    let wt_s = wt.to_string_lossy();
    if wt_s.contains('"') {
        // Not expressible in an SBPL regex literal; the subpaths still hold.
        return p;
    }
    for r in &plan.read_only {
        let r = canonical(r);
        if r.parent() != Some(wt.as_path()) {
            continue;
        }
        let Some(name) = r.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        if name.contains('"') {
            continue;
        }
        p.push_str(&format!(" (regex #\"^{}/(.*/)?{}(/.*)?$\")", ci_regex(&wt_s), ci_regex(&name)));
    }
    p
}

/// The launchd services a confined child may look up: user and group
/// lookups, logging and notifications, preference reads (writes are denied
/// below), TLS trust, DNS and network configuration, file events, the
/// per-user temp dir, and power assertions.
const MACH_ALLOW: &[&str] = &[
    "com.apple.system.opendirectoryd.libinfo",
    "com.apple.system.opendirectoryd.membership",
    "com.apple.system.DirectoryService.libinfo_v1",
    "com.apple.system.DirectoryService.membership_v1",
    "com.apple.system.notification_center",
    "com.apple.system.logger",
    "com.apple.logd",
    "com.apple.logd.events",
    "com.apple.diagnosticd",
    "com.apple.cfprefsd.agent",
    "com.apple.cfprefsd.daemon",
    "com.apple.trustd",
    "com.apple.trustd.agent",
    "com.apple.ocspd",
    "com.apple.SystemConfiguration.configd",
    "com.apple.SystemConfiguration.DNSConfiguration",
    "com.apple.dnssd.service",
    "com.apple.networkd",
    "com.apple.FSEvents",
    "com.apple.bsd.dirhelper",
    "com.apple.distributed_notifications@Uv3",
    "com.apple.distributed_notifications@1v3",
    "com.apple.PowerManagement.control",
];

/// The keychain services, for a child that keeps the login keychain.
const MACH_ALLOW_KEYCHAIN: &[&str] = &["com.apple.SecurityServer", "com.apple.securityd.xpc"];

/// Whether `plan` leaves the login keychain readable (a subscription CLI
/// whose sign-in lives there). With the home denied (Phase C), only when the
/// runtime's allow-list names it and no deny takes it back.
fn keychain_reachable(plan: &EnforcementPlan) -> bool {
    let denied = plan.deny_read.iter().any(|d| d.ends_with("Library/Keychains"));
    match plan.home {
        None => !denied,
        Some(_) => !denied && plan.home_read.iter().any(|d| d.ends_with("Library/Keychains")),
    }
}

/// The directories strictly between `home` and `p` (and `home` itself):
/// their metadata stays readable so `realpath`, `getcwd` and `stat` along
/// the way to an allowed path work, without listing or reading them.
fn ancestors_within(home: &Path, p: &Path) -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    if !p.starts_with(home) {
        return v;
    }
    let mut cur = p.parent();
    while let Some(c) = cur {
        if !c.starts_with(home) {
            break;
        }
        v.push(c.to_path_buf());
        if c == home {
            break;
        }
        cur = c.parent();
    }
    v
}

/// The home-deny rules (Phase C): nothing under the home is readable but the
/// worktree, the turn's dirs, the writable extras and `home_read`; their
/// ancestors keep their metadata.
fn home_rules(plan: &EnforcementPlan) -> String {
    let Some(home) = plan.home.as_ref().map(|h| canonical(h)) else { return String::new() };
    let mut p = format!("(deny file-read* file-write* (subpath {}))\n", q(&home));
    let mut open: Vec<std::path::PathBuf> = vec![canonical(&plan.worktree), canonical(&plan.tmp)];
    open.extend(plan.write_extra.iter().map(|w| canonical(w)));
    open.extend(plan.allow_read.iter().map(|a| canonical(a)));
    open.extend(plan.home_read.iter().map(|r| canonical(r)));
    if let Some(s) = &plan.ligus_mcp_socket {
        open.push(canonical(s));
    }
    let under: Vec<&std::path::PathBuf> = open.iter().filter(|o| o.starts_with(&home)).collect();
    if !under.is_empty() {
        p.push_str("(allow file-read*");
        for o in &under {
            p.push_str(&format!(" (subpath {})", q(o)));
        }
        p.push_str(")\n");
    }
    // Writes the plan grants under the home (the worktree, its extras) come
    // back; reads above are read-only otherwise.
    let mut writes: Vec<std::path::PathBuf> = plan.write_extra.iter().map(|w| canonical(w)).filter(|w| w.starts_with(&home)).collect();
    if plan.worktree_writable && canonical(&plan.worktree).starts_with(&home) {
        writes.push(canonical(&plan.worktree));
    }
    if !writes.is_empty() {
        p.push_str("(allow file-write*");
        for w in &writes {
            p.push_str(&format!(" (subpath {})", q(w)));
        }
        p.push_str(")\n");
    }
    let mut meta: Vec<std::path::PathBuf> = vec![home.clone()];
    for o in &under {
        meta.extend(ancestors_within(&home, o));
    }
    meta.sort();
    meta.dedup();
    p.push_str("(allow file-read-metadata");
    for m in &meta {
        p.push_str(&format!(" (literal {})", q(m)));
    }
    p.push_str(")\n");
    p
}

/// The SBPL profile for `plan` (`proxy_port`: the egress proxy's port).
pub fn profile(plan: &EnforcementPlan, proxy_port: Option<u16>) -> String {
    let mut p = String::from("(version 1)\n(allow default)\n");
    // Writes: nothing, then the turn's own paths.
    p.push_str("(deny file-write*)\n(allow file-write*");
    if plan.worktree_writable {
        p.push_str(&format!(" (subpath {})", q(&plan.worktree)));
    }
    for w in &plan.write_extra {
        p.push_str(&format!(" (subpath {})", q(w)));
    }
    p.push_str(" (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/dtracehelper\") (regex #\"^/dev/tty\") (regex #\"^/dev/fd/\") (regex #\"^/dev/ptmx\") (regex #\"^/dev/ttys\"))\n");
    // Phase C: the home dir, default-deny (before the protected paths and the
    // hard denies, which win over what it lets back in).
    p.push_str(&home_rules(plan));
    // Other turns' scratch dirs (their homes hold their CLIs' sign-ins):
    // the dir that holds them is denied, this turn's own dir excepted
    // (before the protected paths and the hard denies, which still win).
    if let Some(root) = super::sibling_turns_root(&plan.tmp) {
        p.push_str(&format!(
            "(deny file-read* file-write* (subpath {root}))\n(allow file-read* file-write* (subpath {tmp}))\n(allow file-read-metadata (literal {root}))\n",
            root = q(&root),
            tmp = q(&plan.tmp)
        ));
    }
    if plan.worktree_writable && !plan.read_only.is_empty() {
        p.push_str(&format!("(deny file-write*{})\n", protected_filters(plan)));
    }
    // Reads (and writes) of Ligus's data, keys and the user's credentials.
    if !plan.deny_read.is_empty() {
        p.push_str("(deny file-read* file-write*");
        for d in &plan.deny_read {
            p.push_str(&format!(" (subpath {})", q(d)));
        }
        p.push_str(")\n");
    }
    // Readable again: the turn's directory and explicitly allowed paths
    // (a checkout under the data dir). Metadata of the denied dirs' parents
    // stays readable so `cd`/`stat` along the path work.
    p.push_str(&format!("(allow file-read* (subpath {})", q(&plan.worktree)));
    for a in &plan.allow_read {
        p.push_str(&format!(" (subpath {})", q(a)));
    }
    p.push_str(")\n");
    if plan.worktree_writable {
        // Re-allow writes in the worktree if it sits inside a denied dir,
        // then re-deny its protected parts.
        let inside_denied = plan.deny_read.iter().any(|d| canonical(&plan.worktree).starts_with(canonical(d)));
        if inside_denied {
            p.push_str(&format!("(allow file-write* (subpath {}))\n", q(&plan.worktree)));
            if !plan.read_only.is_empty() {
                p.push_str(&format!("(deny file-write*{})\n", protected_filters(plan)));
            }
        }
    }
    // No launches through LaunchServices: `open`/`LSOpen*` hand the launch to
    // launchd, and the launched app runs outside this profile (an agent could
    // write an app bundle into its worktree and `open` it to read the data
    // dir; `sandbox_denies_launchservices_escape`).
    //
    // Denying launchservicesd alone is not enough: `LSOpen*` asks RunningBoard
    // to spawn the app *before* it checks in with launchservicesd, so the
    // app is created (suspended, under launchd) and the open then fails; a
    // `kill -CONT` from inside the turn would resume it unconfined. So:
    // no RunningBoard (no LaunchServices spawn at all), no signals to
    // processes outside this sandbox (nothing suspended out there can be
    // resumed, and the user's own processes can't be killed), and no XPC
    // service lookups (an XPC service embedded in a bundle the agent wrote
    // is spawned by launchd, outside this profile).
    //
    // Beyond those, mach-lookup is default-deny: a default-allow profile
    // leaves hundreds of launchd services reachable (the Dock, login items,
    // the window server, TCC, …), each a way to get work done outside the
    // profile. CLIs, compilers and runtimes need only [`MACH_ALLOW`]
    // (`sandbox_mach_lookup_is_allow_listed`; measured with claude, codex,
    // git, cargo/clang, swift, node and python).
    p.push_str("(deny mach-lookup)\n(allow mach-lookup");
    for s in MACH_ALLOW {
        p.push_str(&format!(" (global-name \"{s}\")"));
    }
    if keychain_reachable(plan) {
        for s in MACH_ALLOW_KEYCHAIN {
            p.push_str(&format!(" (global-name \"{s}\")"));
        }
    }
    p.push_str(")\n");
    p.push_str("(deny mach-lookup (global-name-prefix \"com.apple.coreservices.\") (global-name \"com.apple.runningboard\"))\n");
    p.push_str("(deny mach-lookup (xpc-service-name-regex #\".\"))\n");
    p.push_str("(deny signal)\n(allow signal (target same-sandbox))\n");
    // No preference writes through cfprefsd: it writes the plist outside
    // this profile (e.g. a Terminal profile's start-up command, run unconfined
    // the next time the user opens Terminal). Reads still work.
    p.push_str("(deny user-preference-write)\n");
    // No Shortcuts (`shortcuts run` executes the user's shortcuts, which may
    // run shell scripts, in a helper outside this profile).
    p.push_str("(deny mach-lookup (global-name-prefix \"com.apple.siri.\"))\n");
    // C-13: no Apple Events to other apps (AppleScript driving Terminal or
    // Finder runs outside this profile), and no pasteboard (it can hold a
    // password or token the user just copied).
    p.push_str("(deny appleevent-send)\n");
    p.push_str("(deny mach-lookup (global-name \"com.apple.coreservices.appleevents\") (global-name-prefix \"com.apple.pasteboard\"))\n");
    match (&plan.egress, proxy_port) {
        // Open egress is the network, not the user's unix sockets: a
        // Docker/OrbStack socket is root on a VM with the home dir mounted,
        // and ssh-agent signs with the user's keys
        // (`sandbox_denies_unix_sockets_under_open_egress`). Name resolution
        // (mDNSResponder) and syslog stay reachable.
        (EgressPlan::Open, _) => {
            p.push_str("(deny network-outbound (remote unix-socket))\n");
            p.push_str("(allow network-outbound (remote unix-socket (path-literal \"/private/var/run/mDNSResponder\") (path-literal \"/private/var/run/syslog\")))\n");
        }
        (EgressPlan::Proxy { .. }, Some(port)) => {
            p.push_str("(deny network*)\n");
            p.push_str(&format!("(allow network-outbound (remote ip \"localhost:{port}\"))\n"));
            // Loopback allow-list entries (a local model server) are direct.
            if let EgressPlan::Proxy { allow } = &plan.egress {
                for hp in allow.iter().filter(|h| h.is_loopback()) {
                    p.push_str(&format!("(allow network-outbound (remote ip \"localhost:{}\"))\n", hp.port));
                }
            }
        }
        _ => p.push_str("(deny network*)\n"),
    }
    // The turn's Ligus MCP socket (D.3.2): the one extra connection allowed.
    if let Some(sock) = &plan.ligus_mcp_socket {
        p.push_str(&format!("(allow network-outbound (remote unix-socket (path-literal {})))\n", q(sock)));
    }
    p
}

impl TurnExecutor for Seatbelt {
    fn kind(&self) -> ExecutorKind {
        ExecutorKind::Seatbelt
    }

    fn confine(&self, program: &OsStr, args: &[OsString], cwd: &Path, plan: &EnforcementPlan, proxy: Option<&EgressProxy>) -> io::Result<std::process::Command> {
        let prof = profile(plan, proxy.map(EgressProxy::port));
        // The profile goes in the turn's 0700 scratch dir (never the repo).
        let path = plan.tmp.join(".ligus-sandbox.sb");
        crate::private_file::write_private_atomic(&path, prof.as_bytes())?;
        let mut c = hive_core::process_util::command(SANDBOX_EXEC);
        c.arg("-f").arg(&path).arg("--").arg(program).args(args).current_dir(cwd).env_clear().envs(plan_env(plan, proxy));
        super::new_session(&mut c);
        Ok(c)
    }
}

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
//! (deny file-write* (subpath "<worktree>/.git") <protected dirs and files>)
//! (deny file-read* file-write* (subpath "<data dir>") (subpath "~/.ssh") (literal "<serve.token>") …)
//! (allow file-read* (subpath "<worktree>") <readable dirs inside a denied one>)
//! (deny mach-lookup (global-name-prefix "com.apple.coreservices."))   ; no LaunchServices launches
//! (deny appleevent-send) (deny mach-lookup … "com.apple.pasteboard…")  ; no Apple Events, no pasteboard
//! (deny network*)
//! (allow network-outbound (remote ip "localhost:<proxy port>"))
//! (allow network-outbound (remote unix-socket (path-literal "<ligus mcp socket>")))
//! ```
//!
//! Keychain: agent CLIs keep Keychain *service* access (mach lookups to
//! `securityd`), because Claude Code's subscription login lives there; the
//! keychain *files* are unreadable. Ligus's own items are protected by their
//! item ACLs, not by this profile.

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
    if plan.worktree_writable && !plan.read_only.is_empty() {
        p.push_str("(deny file-write*");
        for r in &plan.read_only {
            p.push_str(&format!(" (subpath {})", q(r)));
        }
        p.push_str(")\n");
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
                p.push_str("(deny file-write*");
                for r in &plan.read_only {
                    p.push_str(&format!(" (subpath {})", q(r)));
                }
                p.push_str(")\n");
            }
        }
    }
    // No launches through LaunchServices: `open`/`LSOpen*` hand the launch to
    // launchd, and the launched app runs outside this profile (an agent could
    // write an app bundle into its worktree and `open` it to read the data
    // dir; `sandbox_denies_launchservices_escape`).
    p.push_str("(deny mach-lookup (global-name-prefix \"com.apple.coreservices.\"))\n");
    // C-13: no Apple Events to other apps (AppleScript driving Terminal or
    // Finder runs outside this profile), and no pasteboard (it can hold a
    // password or token the user just copied).
    p.push_str("(deny appleevent-send)\n");
    p.push_str("(deny mach-lookup (global-name \"com.apple.coreservices.appleevents\") (global-name-prefix \"com.apple.pasteboard\"))\n");
    match (&plan.egress, proxy_port) {
        (EgressPlan::Open, _) => {}
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

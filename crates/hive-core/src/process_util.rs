//! Spawning helpers shared by the app and runtime.
//!
//! Release builds set `windows_subsystem = "windows"` (see `app/src/main.rs`),
//! so Hive.exe owns no console. On Windows, when a process without a console
//! spawns a console application, the OS allocates a **brand new console** for
//! the child — a black window that pops up on screen and vanishes when the
//! child exits. An agent turn shells out to `git` a dozen times (worktree
//! isolation, diff capture, attribution), so the user sees a terminal flash on
//! every single response.
//!
//! `CREATE_NO_WINDOW` (`0x08000000`) tells Windows to give the child a console
//! that has no window. The child still gets a working console handle — and,
//! importantly, its own grandchildren inherit that windowless console — so a
//! CLI agent's nested tool calls stay invisible too, without any of them
//! needing the flag themselves.
//!
//! Use [`command`] instead of `std::process::Command::new` for anything Hive
//! spawns in the background. Both entry points are no-ops off Windows.

use std::ffi::OsStr;
use std::process::Command;

/// `CREATE_NO_WINDOW` — a console for the child, but no window for it.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Drop-in replacement for `std::process::Command::new` that never flashes a
/// console window on Windows. Returns an owned `Command`, so the usual builder
/// chain (`.args(..).output()`) works unchanged.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    no_console_window(&mut cmd);
    cmd
}

/// Apply the no-window creation flag to an already-built `Command`, for callers
/// that can't swap their constructor.
pub fn no_console_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// Names of the variables in this process's environment that hold Ligus's
/// own secrets: any `LIGUS_*`/`HIVE_*` variable whose name says key, token,
/// secret or password (the workspace E2EE key, relay access/GitHub/issuer
/// tokens, the tunnel registration token, ntfy, `LIGUS_WS_SECRET_*`
/// workspace-runtime secrets, `LIGUS_PROVIDER_API_KEY`). Agent CLIs, MCP
/// servers, pre-flight tests and git never need them; a key a runtime does
/// need is passed to it explicitly.
pub fn ligus_secret_env_names() -> Vec<std::ffi::OsString> {
    std::env::vars_os()
        .map(|(k, _)| k)
        .filter(|k| {
            let n = k.to_string_lossy().to_ascii_uppercase();
            (n.starts_with("LIGUS_") || n.starts_with("HIVE_"))
                && ["KEY", "TOKEN", "SECRET", "PASSWORD", "PASSPHRASE"].iter().any(|w| n.contains(w))
        })
        .collect()
}

/// Remove [`ligus_secret_env_names`] from a child's environment.
pub fn scrub_ligus_secrets(cmd: &mut Command) {
    for k in ligus_secret_env_names() {
        cmd.env_remove(k);
    }
}

/// `git` for Hive's own operations on repositories whose content (and, in an
/// agent's turn tree, whose files) may be attacker-written. Repo hooks and
/// fsmonitor never run: a husky-style `core.hooksPath` points into the work
/// tree, so an agent that can only write files could otherwise get code run
/// by Hive, outside any agent sandbox. Passed as `GIT_CONFIG_*` so they
/// override repo-local config and work before or after `-C`.
pub fn git() -> Command {
    let mut cmd = command("git");
    harden_git(&mut cmd);
    scrub_ligus_secrets(&mut cmd);
    cmd
}

/// Apply [`git`]'s overrides to an already-built `git` `Command`.
pub fn harden_git(cmd: &mut Command) {
    const OVERRIDES: [(&str, &str); 3] = [
        ("core.hooksPath", "/dev/null"),
        ("core.fsmonitor", "false"),
        ("protocol.ext.allow", "never"),
    ];
    cmd.env("GIT_CONFIG_COUNT", OVERRIDES.len().to_string());
    for (i, (k, v)) in OVERRIDES.iter().enumerate() {
        cmd.env(format!("GIT_CONFIG_KEY_{i}"), k).env(format!("GIT_CONFIG_VALUE_{i}"), v);
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn hardened_git_ignores_repo_hooks_and_fsmonitor() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("hive-hardgit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".hooks")).unwrap();
        let run = |c: &mut std::process::Command| assert!(c.output().unwrap().status.success());
        run(super::command("git").arg("-C").arg(&dir).args(["init", "-q"]));
        let marker = dir.with_extension("ran");
        let _ = std::fs::remove_file(&marker);
        let hook = dir.join(".hooks/pre-commit");
        std::fs::write(&hook, format!("#!/bin/sh\necho hook >> '{}'\n", marker.display())).unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let fsm = dir.join("fsm.sh");
        std::fs::write(&fsm, format!("#!/bin/sh\necho fsmonitor >> '{}'\n", marker.display())).unwrap();
        std::fs::set_permissions(&fsm, std::fs::Permissions::from_mode(0o755)).unwrap();
        for (k, v) in [("core.hooksPath", ".hooks"), ("core.fsmonitor", fsm.to_str().unwrap()), ("user.email", "t@t"), ("user.name", "t")] {
            run(super::command("git").arg("-C").arg(&dir).args(["config", k, v]));
        }
        std::fs::write(dir.join("a"), "a").unwrap();
        run(super::git().arg("-C").arg(&dir).args(["add", "-A"]));
        run(super::git().arg("-C").arg(&dir).args(["status", "--porcelain"]));
        run(super::git().arg("-C").arg(&dir).args(["commit", "-qm", "x"]));
        let ran = std::fs::read_to_string(&marker).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&marker);
        assert!(ran.is_empty(), "ran: {ran}");
    }
}

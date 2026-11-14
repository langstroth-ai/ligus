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

use std::ffi::{OsStr, OsString};
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

/// What a child's environment is built from (see [`child_env`]).
#[derive(Debug, Clone, Default)]
pub struct ChildEnvSpec {
    /// `TMPDIR` for the child (its per-turn scratch dir).
    pub tmpdir: Option<std::path::PathBuf>,
    /// Proxy variables set by Ligus (`HTTPS_PROXY`, `NO_PROXY`, …).
    pub proxy: Vec<(String, String)>,
    /// Extra variables from the caller. Only allow-listed names pass:
    /// `GIT_AUTHOR_*`, `GIT_COMMITTER_*`, `PI_CODING_AGENT_DIR`,
    /// `LIGUS_TURN_CRED`, `LIGUS_MCP_SOCKET`.
    pub extra: Vec<(String, String)>,
    /// The one provider key this runtime was configured with **in Ligus**,
    /// as (env var name, value). Never inherited from the parent.
    pub provider_key: Option<(String, String)>,
}

/// Parent variables every child keeps (names, or `PREFIX*`).
const ALWAYS: &[&str] = &[
    "PATH", "HOME", "USER", "LOGNAME", "SHELL", "LANG", "LC_*", "TZ", "TERM", "TMPDIR",
    "SSL_CERT_FILE", "SSL_CERT_DIR", "NODE_EXTRA_CA_CERTS",
    // The CLIs' own config dirs, when the user set them.
    "CLAUDE_CONFIG_DIR", "CODEX_HOME", "COPILOT_HOME", "COPILOT_CONFIG_DIR",
];
#[cfg(target_os = "linux")]
const PLATFORM: &[&str] = &["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME"];
#[cfg(windows)]
const PLATFORM: &[&str] = &["SYSTEMROOT", "SYSTEMDRIVE", "WINDIR", "TEMP", "TMP", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "PATHEXT", "COMSPEC"];
#[cfg(not(any(target_os = "linux", windows)))]
const PLATFORM: &[&str] = &[];
/// Caller-supplied names that may pass.
const EXTRA_ALLOWED: &[&str] = &["GIT_AUTHOR_*", "GIT_COMMITTER_*", "PI_CODING_AGENT_DIR", "LIGUS_TURN_CRED", "LIGUS_MCP_SOCKET"];

fn name_matches(name: &str, pats: &[&str]) -> bool {
    pats.iter().any(|p| match p.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => name == *p,
    })
}

/// The whole environment of a child Ligus spawns for an agent turn (CLI
/// agents, stdio MCP servers, `run_command`, pre-flight). Callers
/// `env_clear()` and set exactly this: an allow-list, not a scrub, so a
/// provider key or token in Ligus's own environment never reaches a child
/// that wasn't explicitly given it.
pub fn child_env(spec: &ChildEnvSpec) -> Vec<(OsString, OsString)> {
    child_env_from(std::env::vars_os(), spec)
}

/// [`child_env`] over an explicit parent environment (for tests).
pub fn child_env_from(parent: impl IntoIterator<Item = (OsString, OsString)>, spec: &ChildEnvSpec) -> Vec<(OsString, OsString)> {
    let mut out: Vec<(OsString, OsString)> = parent
        .into_iter()
        .filter(|(k, _)| {
            let k = k.to_string_lossy();
            name_matches(&k, ALWAYS) || name_matches(&k, PLATFORM)
        })
        .collect();
    let mut set = |k: &str, v: &OsStr| {
        out.retain(|(x, _)| x != k);
        out.push((k.into(), v.to_os_string()));
    };
    if let Some(t) = &spec.tmpdir {
        set("TMPDIR", t.as_os_str());
    }
    for (k, v) in &spec.proxy {
        set(k, OsStr::new(v));
    }
    for (k, v) in &spec.extra {
        if name_matches(k, EXTRA_ALLOWED) {
            set(k, OsStr::new(v));
        }
    }
    if let Some((k, v)) = &spec.provider_key {
        set(k, OsStr::new(v));
    }
    out.sort();
    out
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
    use std::ffi::OsString;

    /// C.4.6 `child_env_has_no_parent_provider_keys`: provider keys and
    /// Ligus secrets in the parent never reach a child; the one configured
    /// key does, and only allow-listed extras pass.
    #[test]
    fn child_env_has_no_parent_provider_keys() {
        let parent: Vec<(OsString, OsString)> = [
            ("PATH", "/usr/bin"),
            ("HOME", "/home/u"),
            ("LC_ALL", "C"),
            ("ANTHROPIC_API_KEY", "sk-ant-parent"),
            ("OPENAI_API_KEY", "sk-parent"),
            ("OPENROUTER_API_KEY", "or-parent"),
            ("AWS_SECRET_ACCESS_KEY", "aws"),
            ("GITHUB_TOKEN", "ghp_x"),
            ("LIGUS_RELAY_ACCESS_TOKEN", "hrt1.a.b"),
            ("LIGUS_WORKSPACE_KEY", "k"),
            ("HIVE_TOKEN", "t"),
            ("HTTPS_PROXY", "http://corp:3128"),
        ]
        .iter()
        .map(|(k, v)| (OsString::from(k), OsString::from(v)))
        .collect();
        let spec = super::ChildEnvSpec {
            tmpdir: Some("/tmp/ligus-turn-x".into()),
            proxy: vec![("HTTPS_PROXY".into(), "http://ligus:t@127.0.0.1:9".into())],
            extra: vec![("GIT_AUTHOR_NAME".into(), "Ana".into()), ("OPENAI_API_KEY".into(), "smuggled".into())],
            provider_key: Some(("ANTHROPIC_API_KEY".into(), "sk-ant-configured".into())),
        };
        let env = super::child_env_from(parent, &spec);
        let get = |k: &str| env.iter().find(|(x, _)| x == k).map(|(_, v)| v.to_string_lossy().into_owned());
        assert_eq!(get("PATH").as_deref(), Some("/usr/bin"));
        assert_eq!(get("LC_ALL").as_deref(), Some("C"));
        assert_eq!(get("TMPDIR").as_deref(), Some("/tmp/ligus-turn-x"));
        assert_eq!(get("HTTPS_PROXY").as_deref(), Some("http://ligus:t@127.0.0.1:9"));
        assert_eq!(get("GIT_AUTHOR_NAME").as_deref(), Some("Ana"));
        assert_eq!(get("ANTHROPIC_API_KEY").as_deref(), Some("sk-ant-configured"));
        for k in ["OPENAI_API_KEY", "OPENROUTER_API_KEY", "AWS_SECRET_ACCESS_KEY", "GITHUB_TOKEN", "LIGUS_RELAY_ACCESS_TOKEN", "LIGUS_WORKSPACE_KEY", "HIVE_TOKEN"] {
            assert_eq!(get(k), None, "{k} leaked");
        }
        // No provider key configured: none at all.
        let env = super::child_env_from(vec![(OsString::from("ANTHROPIC_API_KEY"), OsString::from("x"))], &Default::default());
        assert!(env.is_empty(), "{env:?}");
    }

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

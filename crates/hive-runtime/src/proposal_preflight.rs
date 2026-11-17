//! Pre-flight for a proposal-set alternative: phase one of the two-phase
//! commit (see `hive_core::proposal_sets`). Before an alternative is applied:
//!
//! - **tests**: the workspace's pre-flight test command runs in a scratch
//!   worktree cut from the chat's base, with the alternative's diff applied;
//! - **base unchanged**: the chat branch is still at the commit the set was
//!   opened against (when this device can resolve it), and the diff still
//!   applies cleanly to it;
//! - **hosts online**: the hosts needed to carry it out answer presence.
//!
//! The test command is device-local on purpose: it comes from
//! `LIGUS_PREFLIGHT_TEST` only, never from the synced log and never from the
//! repository (not even its `.git/config`, which a clone can carry), so
//! neither a teammate nor a repo can make your machine run a command. It
//! runs in the agent sandbox like any agent child. Notes recorded in the report are short status lines, never test
//! output or file contents (the report is synced).

use std::path::Path;
use std::time::{Duration, Instant};

use hive_core::process_util::command;
use hive_core::proposal_sets::{CheckOutcome, PreflightReport};

use crate::git_worktree::{self, Worktree};

/// The pre-flight test command (device-local env).
pub const PREFLIGHT_TEST_ENV: &str = "LIGUS_PREFLIGHT_TEST";
/// Default bound on the test run.
pub const DEFAULT_TEST_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// What the pre-flight needs, gathered by the caller.
#[derive(Debug, Clone, Default)]
pub struct PreflightInputs<'a> {
    /// The workspace's git repo root on this device, if it is one.
    pub repo_root: Option<&'a Path>,
    /// The chat whose base branch the change lands on.
    pub chat_id: &'a str,
    /// The alternative's unified diff (`None` for a command or decision).
    pub diff: Option<&'a str>,
    /// The commit recorded when the set opened.
    pub base_commit: Option<&'a str>,
    /// The test command to run (see [`test_command`]); `None` skips the check.
    pub test_command: Option<String>,
    pub test_timeout: Option<Duration>,
    /// Hosts the change needs, with whether each is online now.
    pub hosts: Vec<(String, bool)>,
}

/// The configured pre-flight test command, if any: `LIGUS_PREFLIGHT_TEST`
/// on this device. The repository is never consulted.
pub fn test_command() -> Option<String> {
    hive_core::compat::var(PREFLIGHT_TEST_ENV).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn git_out(dir: &Path, args: &[&str]) -> Option<String> {
    let out = hive_core::process_util::git().arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The commit the chat's changes currently land on: its base branch if it
/// exists, else the base a fresh chat branch would be cut from.
pub fn current_base(repo_root: &Path, chat_id: &str) -> Option<String> {
    let branch = format!("refs/heads/hive/chat/{chat_id}");
    git_out(repo_root, &["rev-parse", "--verify", "--quiet", &branch]).or_else(|| {
        let base = git_worktree::chat_base(repo_root);
        git_out(repo_root, &["rev-parse", "--verify", "--quiet", &format!("{base}^{{commit}}")])
    })
}

fn patch_applies(dir: &Path, diff: &str) -> Result<(), String> {
    run_git_apply(dir, diff, true)
}

fn run_git_apply(dir: &Path, diff: &str, check_only: bool) -> Result<(), String> {
    use std::io::Write;
    use std::process::Stdio;
    let mut cmd = hive_core::process_util::git();
    cmd.arg("-C").arg(dir).args(["apply", "--whitespace=nowarn"]);
    if check_only {
        cmd.arg("--check");
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git apply: {e}"))?;
    child
        .stdin
        .take()
        .ok_or("git apply: no stdin")?
        .write_all(diff.as_bytes())
        .map_err(|e| format!("git apply: {e}"))?;
    let out = child.wait_with_output().map_err(|e| format!("git apply: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err("the change no longer applies to the chat's code".into())
    }
}

fn check_base(inputs: &PreflightInputs<'_>) -> CheckOutcome {
    let Some(root) = inputs.repo_root else {
        return CheckOutcome::skipped("not a git workspace on this device");
    };
    let now = current_base(root, inputs.chat_id);
    let mut notes = Vec::new();
    if let (Some(base), Some(now)) = (inputs.base_commit, now.as_deref()) {
        let known = git_out(root, &["cat-file", "-e", &format!("{base}^{{commit}}")]).is_some();
        if known && base != now {
            return CheckOutcome::failed(format!(
                "the chat's base moved since scouting began ({} → {})",
                short(base),
                short(now)
            ));
        }
        notes.push(if known { "base unchanged" } else { "base recorded on another device" });
    }
    if let Some(diff) = inputs.diff {
        let target = git_worktree::chat_worktree(root, inputs.chat_id).ok();
        let dir = target.as_deref().unwrap_or(root);
        if let Err(e) = patch_applies(dir, diff) {
            return CheckOutcome::failed(e);
        }
        notes.push("patch applies");
    }
    if notes.is_empty() {
        CheckOutcome::skipped("nothing to compare")
    } else {
        CheckOutcome::passed(notes.join("; "))
    }
}

fn short(sha: &str) -> &str {
    sha.get(..8).unwrap_or(sha)
}

/// The shell that runs `cmdline`. On Windows the command line itself is
/// appended raw after the sandbox has prepared the command (see
/// [`run_bounded`]): `cmd` parses its own command line, and the standard
/// argument quoting (`\"`) would mangle any quoted argument in it.
fn shell(cmdline: &str) -> std::process::Command {
    #[cfg(windows)]
    {
        let _ = cmdline;
        let mut c = command("cmd");
        c.args(["/D", "/S", "/C"]);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = command("sh");
        c.args(["-c", cmdline]);
        c
    }
}

/// Run `cmdline` in `dir`, bounded by `timeout`. Ok(true) = exit 0.
fn run_bounded(dir: &Path, cmdline: &str, timeout: Duration) -> Result<bool, String> {
    use std::process::Stdio;
    // It runs the alternative's code (teammate or agent content): confined
    // like an agent's child (writes only in the scratch worktree, no Ligus
    // data or keys, allow-listed env, no network unless device-allowed).
    let sh = shell(cmdline);
    let program = sh.get_program().to_os_string();
    let args: Vec<std::ffi::OsString> = sh.get_args().map(|a| a.to_os_string()).collect();
    let spec = crate::executor::SpawnSpec::new(crate::executor::Purpose::Preflight, &program, args).cwd(Some(dir));
    let crate::executor::Prepared { command: mut sh, guard: _sandbox } =
        crate::executor::prepare(spec).map_err(|e| format!("couldn't start the test command: {e}"))?;
    // `cmd /S /C "<line>"` runs <line> verbatim (Windows has no agent
    // sandbox, so the prepared command is `cmd` itself and this lands last).
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        sh.raw_arg(format!("\"{cmdline}\""));
    }
    let mut child = sh
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("couldn't start the test command: {e}"))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.success()),
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("tests timed out after {}s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(format!("test command: {e}")),
        }
    }
}

fn check_tests(inputs: &PreflightInputs<'_>, scratch_id: &str) -> CheckOutcome {
    let Some(cmd) = inputs.test_command.as_deref() else {
        return CheckOutcome::skipped(format!("no pre-flight test command set ({PREFLIGHT_TEST_ENV})"));
    };
    let (Some(root), Some(diff)) = (inputs.repo_root, inputs.diff) else {
        return CheckOutcome::skipped("not a code change");
    };
    // Cut the scratch tree from the chat's base branch when it exists.
    let _ = git_worktree::chat_worktree(root, inputs.chat_id);
    let base = format!("hive/chat/{}", inputs.chat_id);
    let wt = match Worktree::create_from(root, scratch_id, &base).or_else(|_| Worktree::create(root, scratch_id)) {
        Ok(wt) => wt,
        Err(e) => return CheckOutcome::failed(format!("couldn't make a scratch worktree: {e}")),
    };
    let outcome = match run_git_apply(&wt.path, diff, false) {
        Err(e) => CheckOutcome::failed(e),
        Ok(()) => match run_bounded(&wt.path, cmd, inputs.test_timeout.unwrap_or(DEFAULT_TEST_TIMEOUT)) {
            Ok(true) => CheckOutcome::passed("tests passed"),
            Ok(false) => CheckOutcome::failed("tests failed"),
            Err(e) => CheckOutcome::failed(e),
        },
    };
    let _ = wt.remove();
    outcome
}

fn check_hosts(hosts: &[(String, bool)]) -> CheckOutcome {
    if hosts.is_empty() {
        return CheckOutcome::skipped("no other host involved");
    }
    let offline: Vec<&str> = hosts.iter().filter(|(_, up)| !up).map(|(h, _)| h.as_str()).collect();
    if offline.is_empty() {
        CheckOutcome::passed(format!("{} host(s) online", hosts.len()))
    } else {
        CheckOutcome::failed(format!("offline: {}", offline.join(", ")))
    }
}

/// Run every check. Blocking (git + the test command): call it off the UI
/// thread. `scratch_id` names the throwaway worktree (unique per run).
pub fn run(inputs: &PreflightInputs<'_>, scratch_id: &str) -> PreflightReport {
    let base_unchanged = check_base(inputs);
    // Tests only run on a base that still holds: a stale patch would fail for
    // the wrong reason.
    let tests = if base_unchanged.status == hive_core::proposal_sets::CheckStatus::Failed {
        CheckOutcome::skipped("not run: the base check failed")
    } else {
        check_tests(inputs, scratch_id)
    };
    PreflightReport { tests, base_unchanged, hosts_online: check_hosts(&inputs.hosts) }
}

#[cfg(test)]
mod tests {

    /// S1: a repository's `.git/config` can't name a command for this device
    /// to run (`ligus.preflightTest` was a repo-controlled source).
    #[test]
    fn preflight_ignores_repo_git_config() {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| assert!(hive_core::process_util::git().arg("-C").arg(dir.path()).args(args).status().unwrap().success());
        run(&["init", "-q"]);
        run(&["config", "ligus.preflightTest", "touch /tmp/ligus-preflight-pwned"]);
        let cmd = test_command();
        assert_ne!(cmd.as_deref(), Some("touch /tmp/ligus-preflight-pwned"));
    }

    use super::*;
    use hive_core::proposal_sets::CheckStatus;
    use std::path::PathBuf;

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = command("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn temp_repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ligus-preflight-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        sh(&dir, &["init", "-q"]);
        sh(&dir, &["config", "user.email", "t@t"]);
        sh(&dir, &["config", "user.name", "t"]);
        sh(&dir, &["config", "core.autocrlf", "false"]);
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        sh(&dir, &["add", "-A"]);
        sh(&dir, &["commit", "-qm", "init"]);
        dir
    }

    const DIFF: &str = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-hello\n+hello world\n";

    #[test]
    fn passes_on_an_unmoved_base_with_passing_tests() {
        let repo = temp_repo("pass");
        let base = current_base(&repo, "c1").unwrap();
        let inputs = PreflightInputs {
            repo_root: Some(&repo),
            chat_id: "c1",
            diff: Some(DIFF),
            base_commit: Some(&base),
            // `cmd /C` on Windows, `sh -c` elsewhere.
            test_command: Some(if cfg!(windows) { r#"findstr /C:"hello world" a.txt"# } else { "grep -q 'hello world' a.txt" }.into()),
            test_timeout: Some(Duration::from_secs(30)),
            hosts: vec![("this device".into(), true)],
        };
        let r = run(&inputs, "pf-pass");
        assert_eq!(r.base_unchanged.status, CheckStatus::Passed, "{r:?}");
        // The test command runs in the agent sandbox. A host without one
        // (Windows) refuses it, fail closed, and says why.
        if crate::executor::skip_without_sandbox("preflight test command") {
            assert_eq!(r.tests.status, CheckStatus::Failed, "{r:?}");
            assert!(r.tests.note.contains("no agent sandbox"), "{r:?}");
            assert!(!r.passed());
            let _ = std::fs::remove_dir_all(&repo);
            return;
        }
        assert_eq!(r.tests.status, CheckStatus::Passed, "the diff is applied in the scratch tree: {r:?}");
        assert!(r.passed());
        // The scratch tree is gone and the chat tree is untouched.
        let chat = git_worktree::chat_worktree(&repo, "c1").unwrap();
        assert_eq!(std::fs::read_to_string(chat.join("a.txt")).unwrap(), "hello\n");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn fails_when_the_base_moved_or_tests_fail_or_a_host_is_down() {
        let repo = temp_repo("fail");
        let base = current_base(&repo, "c2").unwrap();
        // Tests fail.
        let mut inputs = PreflightInputs {
            repo_root: Some(&repo),
            chat_id: "c2",
            diff: Some(DIFF),
            base_commit: Some(&base),
            test_command: Some("exit 3".into()),
            test_timeout: Some(Duration::from_secs(30)),
            hosts: vec![("worker".into(), false)],
        };
        let r = run(&inputs, "pf-fail-1");
        assert_eq!(r.tests.status, CheckStatus::Failed);
        assert_eq!(r.hosts_online.status, CheckStatus::Failed);
        assert!(r.hosts_online.note.contains("worker"));
        assert!(!r.passed());

        // The chat branch moves: the base check fails and tests don't run.
        let chat = git_worktree::chat_worktree(&repo, "c2").unwrap();
        std::fs::write(chat.join("b.txt"), "b\n").unwrap();
        sh(&chat, &["add", "-A"]);
        sh(&chat, &["commit", "-qm", "moved"]);
        inputs.test_command = Some("true".into());
        inputs.hosts.clear();
        let r = run(&inputs, "pf-fail-2");
        assert_eq!(r.base_unchanged.status, CheckStatus::Failed);
        assert!(r.base_unchanged.note.contains("moved"), "{r:?}");
        assert_eq!(r.tests.status, CheckStatus::Skipped);
        assert_eq!(r.hosts_online.status, CheckStatus::Skipped);

        // A base from another device (unknown commit) falls back to "patch applies".
        inputs.base_commit = Some("0123456789abcdef0123456789abcdef01234567");
        let r = run(&inputs, "pf-fail-3");
        assert_eq!(r.base_unchanged.status, CheckStatus::Passed, "{r:?}");
        assert!(r.base_unchanged.note.contains("another device"));
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn a_conflicting_patch_fails_the_base_check_and_no_command_skips_tests() {
        let repo = temp_repo("conflict");
        let chat = git_worktree::chat_worktree(&repo, "c3").unwrap();
        std::fs::write(chat.join("a.txt"), "changed\n").unwrap();
        sh(&chat, &["commit", "-qam", "edit"]);
        let inputs = PreflightInputs {
            repo_root: Some(&repo),
            chat_id: "c3",
            diff: Some(DIFF),
            base_commit: None,
            test_command: None,
            ..Default::default()
        };
        let r = run(&inputs, "pf-conflict");
        assert_eq!(r.base_unchanged.status, CheckStatus::Failed);
        assert_eq!(r.tests.status, CheckStatus::Skipped);
        // Not a repo / not a code change: everything skips, which passes.
        let r = run(&PreflightInputs { chat_id: "x", test_command: Some("true".into()), ..Default::default() }, "pf-none");
        assert!(r.passed());
        assert_eq!(r.tests.status, CheckStatus::Skipped);
        let _ = std::fs::remove_dir_all(&repo);
    }
}

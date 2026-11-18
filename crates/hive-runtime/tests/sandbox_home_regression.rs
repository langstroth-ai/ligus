//! Phase C regression: with the home dir default-deny, the workflows agents
//! run every day still work under this host's sandbox: git, `cargo build` in
//! the worktree, node, python and `pip install` into a venv in the worktree.
//! Each runs as a turn's command (`run_command`), in a temp worktree, with
//! no network, against the real toolchains (read-only through the
//! toolchain allow-list). A tool this host lacks is skipped with a note.
//!
//! The CLI agents themselves (Claude Code `-p`, `codex --version` and
//! `codex login status`, and the other CLIs' `--version`) run with
//! `LIGUS_LIVE_CLI=1` (`--ignored`): they use the real sign-in and, for
//! Claude Code, a model call.

use std::process::Stdio;

use hive_core::sandbox::SandboxEnvelope;
use hive_core::ModelProviderKind;
use hive_runtime::author_gate::GatePlan;
use hive_runtime::executor::{self, HostKind, Purpose, SpawnSpec, TurnPolicy};

fn have(tool: &str) -> bool {
    std::process::Command::new("/bin/sh")
        .args(["-c", &format!("command -v {tool}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn policy() -> TurnPolicy {
    let mut p = TurnPolicy::new(HostKind::Desktop, SandboxEnvelope::workspace_default(), GatePlan::Full);
    p.worktree_is_isolated = true;
    p
}

/// Run `script` under `purpose` (`sh -c`) in `dir`; (ok, stdout+stderr).
fn run(purpose: Purpose, dir: &std::path::Path, script: &str) -> (bool, String) {
    executor::with_turn_sync(policy(), || {
        let mut spec = SpawnSpec::new(purpose, "/bin/sh", ["-c", script]).cwd(Some(dir));
        spec.writes = Some(true);
        let executor::Prepared { mut command, guard } = executor::prepare(spec).expect("prepare");
        assert_ne!(guard.kind(), executor::ExecutorKind::Unconfined);
        let out = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).output().expect("spawn");
        drop(guard);
        (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    })
}

fn sandboxed(test: &str) -> bool {
    !executor::skip_without_sandbox(test)
}

fn worktree() -> std::path::PathBuf {
    executor::canonical(&tempfile::tempdir().unwrap().keep())
}

/// The Landlock tier can't let a turn create entries directly in the
/// worktree's top-level directory (any right there would reach `.git`; a
/// documented limit of that tier). A repository that was built before has
/// them already: make them, as such a checkout would have.
fn landlock_tier_prepare(wt: &std::path::Path, entries: &[(&str, Option<&str>)]) {
    if executor::sandbox_status().kind != executor::ExecutorKind::Landlock {
        return;
    }
    for (name, body) in entries {
        match body {
            Some(b) => std::fs::write(wt.join(name), b).unwrap(),
            None => std::fs::create_dir_all(wt.join(name)).unwrap(),
        }
    }
}

#[test]
fn regression_git_commit_works_in_a_turn_repo() {
    if !sandboxed("regression_git_commit_works_in_a_turn_repo") || !have("git") {
        return;
    }
    let wt = worktree();
    let (ok, out) = run(
        Purpose::Tool,
        &wt,
        // A nested `.git` in the worktree is a protected name (Ligus captures
        // the worktree's changes as a diff), so the repo is in the turn's
        // scratch dir.
        "cd \"$TMPDIR\" && git init -q demo && cd demo && echo a > a && git add a && \
         git -c user.name=t -c user.email=t@example.com commit -qm first && echo COMMITS=$(git log --oneline | wc -l | tr -d ' ')",
    );
    assert!(ok && out.contains("COMMITS=1"), "{out}");
}

#[test]
fn regression_cargo_build_in_the_worktree() {
    if !sandboxed("regression_cargo_build_in_the_worktree") || !have("cargo") {
        eprintln!("SKIPPED regression_cargo_build_in_the_worktree: no cargo or sandbox");
        return;
    }
    let wt = worktree();
    std::fs::create_dir_all(wt.join("src")).unwrap();
    std::fs::write(wt.join("Cargo.toml"), "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n").unwrap();
    std::fs::write(wt.join("src/main.rs"), "fn main() { println!(\"built-{}\", 6 * 7); }\n").unwrap();
    landlock_tier_prepare(&wt, &[("Cargo.lock", Some("version = 3\n\n[[package]]\nname = \"demo\"\nversion = \"0.1.0\"\n")), ("target", None)]);
    let (ok, out) = run(Purpose::Tool, &wt, "cargo build --offline -q && ./target/debug/demo");
    assert!(ok && out.contains("built-42"), "{out}");
    // As a CLI agent's own shell (its per-turn home) too.
    let (ok, out) = run(Purpose::Agent(ModelProviderKind::ClaudeCode), &wt, "cargo build --offline -q && ./target/debug/demo");
    assert!(ok && out.contains("built-42"), "{out}");
}

#[test]
fn regression_node_runs() {
    if !sandboxed("regression_node_runs") || !have("node") {
        eprintln!("SKIPPED regression_node_runs: no node or sandbox");
        return;
    }
    let wt = worktree();
    landlock_tier_prepare(&wt, &[("out.txt", Some(""))]);
    let (ok, out) = run(Purpose::Tool, &wt, "node -e 'require(\"fs\").writeFileSync(\"out.txt\", String(6*7)); console.log(\"node-\" + require(\"fs\").readFileSync(\"out.txt\"))'");
    assert!(ok && out.contains("node-42"), "{out}");
}

/// A minimal wheel, built in the sandbox with the standard library.
const MAKE_WHEEL: &str = r#"
import zipfile
with zipfile.ZipFile("demo-0.1-py3-none-any.whl", "w") as z:
    z.writestr("demo/__init__.py", "X = 42\n")
    z.writestr("demo-0.1.dist-info/METADATA", "Metadata-Version: 2.1\nName: demo\nVersion: 0.1\n")
    z.writestr("demo-0.1.dist-info/WHEEL", "Wheel-Version: 1.0\nGenerator: ligus-test\nRoot-Is-Purelib: true\nTag: py3-none-any\n")
    z.writestr("demo-0.1.dist-info/RECORD", "demo/__init__.py,,\ndemo-0.1.dist-info/METADATA,,\ndemo-0.1.dist-info/WHEEL,,\ndemo-0.1.dist-info/RECORD,,\n")
"#;

#[test]
fn regression_python_and_pip_into_a_venv_in_the_worktree() {
    if !sandboxed("regression_python_and_pip_into_a_venv_in_the_worktree") || !have("python3") {
        eprintln!("SKIPPED regression_python_and_pip_into_a_venv_in_the_worktree: no python3 or sandbox");
        return;
    }
    let wt = worktree();
    std::fs::write(wt.join("make_wheel.py"), MAKE_WHEEL).unwrap();
    landlock_tier_prepare(&wt, &[("demo-0.1-py3-none-any.whl", Some("")), (".venv", None)]);
    let (ok, out) = run(
        Purpose::Tool,
        &wt,
        "python3 make_wheel.py && python3 -m venv .venv && \
         .venv/bin/python -m pip install -q --no-index --disable-pip-version-check ./demo-0.1-py3-none-any.whl && \
         .venv/bin/python -c 'import demo; print(\"py-%d\" % demo.X)'",
    );
    assert!(ok && out.contains("py-42"), "{out}");
}

// ---------------------------------------------------------------------------
// Live CLIs (LIGUS_LIVE_CLI=1, --ignored)
// ---------------------------------------------------------------------------

fn live() -> bool {
    std::env::var("LIGUS_LIVE_CLI").is_ok_and(|v| v == "1")
}

/// Run `program args` as a CLI agent turn of `kind` in a temp worktree.
fn run_cli(kind: ModelProviderKind, program: &str, args: &[&str]) -> (bool, String) {
    let wt = worktree();
    executor::with_turn_sync(policy(), || {
        let mut spec = SpawnSpec::new(Purpose::Agent(kind), program, args.iter().copied()).cwd(Some(&wt));
        spec.writes = Some(false);
        let executor::Prepared { mut command, guard } = executor::prepare(spec).expect("prepare");
        let out = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).output().expect("spawn");
        drop(guard);
        (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    })
}

#[test]
#[ignore]
fn live_claude_code_print_answers() {
    if !live() || !have("claude") {
        eprintln!("SKIPPED live_claude_code_print_answers (LIGUS_LIVE_CLI=1 and claude needed)");
        return;
    }
    let (ok, out) = run_cli(ModelProviderKind::ClaudeCode, "claude", &["-p", "Reply with exactly the word PONG and nothing else.", "--output-format", "text"]);
    assert!(ok && out.contains("PONG"), "{out}");
}

#[test]
#[ignore]
fn live_codex_version_and_login_status() {
    if !live() || !have("codex") {
        eprintln!("SKIPPED live_codex_version_and_login_status (LIGUS_LIVE_CLI=1 and codex needed)");
        return;
    }
    let (ok, out) = run_cli(ModelProviderKind::Codex, "codex", &["--version"]);
    assert!(ok && out.to_ascii_lowercase().contains("codex"), "{out}");
    let (ok, out) = run_cli(ModelProviderKind::Codex, "codex", &["login", "status"]);
    assert!(ok && out.to_ascii_lowercase().contains("logged in"), "{out}");
}

#[test]
#[ignore]
fn live_other_clis_start() {
    if !live() {
        return;
    }
    for (kind, program) in [
        (ModelProviderKind::Copilot, "copilot"),
        (ModelProviderKind::Antigravity, "agy"),
        (ModelProviderKind::Pi, "pi"),
        (ModelProviderKind::Aider, "aider"),
    ] {
        if !have(program) {
            eprintln!("SKIPPED {program}: not installed");
            continue;
        }
        let (ok, out) = run_cli(kind, program, &["--version"]);
        assert!(ok && out.chars().any(|c| c.is_ascii_digit()), "{program}: {out}");
    }
}

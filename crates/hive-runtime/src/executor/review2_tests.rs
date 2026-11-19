//! Second adversarial review of the Phase C sandbox (after the first
//! review's fixes): each probe was written to fail first, then fixed. Decoy
//! homes in temp dirs only; nothing here reads the user's own files.

use super::home_tests::{plan_for, run_in, DecoyHome};
use super::*;
use hive_core::sandbox::PathGrants;

#[cfg(unix)]
const PURPOSES: &[Purpose] = &[Purpose::Agent(ModelProviderKind::ClaudeCode), Purpose::Agent(ModelProviderKind::Codex), Purpose::Tool];

/// `script` in the decoy's worktree; `None` when the spawn is refused.
#[cfg(unix)]
fn try_run(e: &dyn TurnExecutor, d: &DecoyHome, purpose: Purpose, script: &str) -> Option<(bool, String)> {
    let policy = TurnPolicy { worktree_is_isolated: true, ..TurnPolicy::baseline(HostKind::Desktop) };
    with_turn_sync(policy, || {
        let spec = SpawnSpec::new(purpose, "/bin/sh", ["-c", script, "sh", "--permission-mode", "acceptEdits"]).cwd(Some(&d.worktree));
        let mut p = prepare_in(e, spec, &d.home, d.host()).ok()?;
        let out = p.command.output().expect("spawn");
        drop(p.guard);
        Some((out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))))
    })
}

/// A protected path in the worktree (`.envrc`, `ligus.config.toml`) that is
/// a symlink out of it (committed by a teammate, or in a repo the user
/// cloned) never brings what it points to into the turn: bubblewrap bound
/// the resolved target read-only *after* the home's tmpfs, so
/// `.envrc -> ~/.ssh` exposed the keys. Nor can the turn replace it (the
/// Landlock tier left a protected symlink writable).
#[cfg(unix)]
#[test]
fn review2_protected_symlink_out_of_the_worktree_opens_nothing_live() {
    let Some(e) = super::tests::real_executor() else { return };
    let d = DecoyHome::new();
    std::os::unix::fs::symlink(d.home.join(".ssh"), d.worktree.join(".envrc")).unwrap();
    std::os::unix::fs::symlink(d.home.join("Documents"), d.worktree.join("ligus.config.toml")).unwrap();
    let h = d.home.display().to_string();
    for purpose in PURPOSES {
        let script = format!(
            "cat .envrc/id_ed25519 '{h}/.ssh/id_ed25519' ligus.config.toml/taxes.txt '{h}/Documents/taxes.txt'; ls .envrc/; rm -f .envrc && echo EVIL > .envrc && echo REPLACED"
        );
        if let Some((_, out)) = try_run(&*e, &d, *purpose, &script) {
            assert!(!out.contains("DECOY") && !out.contains("id_ed25519"), "{purpose:?} read through a protected symlink: {out}");
            assert!(!out.contains("REPLACED"), "{purpose:?} replaced a protected symlink: {out}");
        }
        assert!(std::fs::symlink_metadata(d.worktree.join(".envrc")).unwrap().file_type().is_symlink(), "{purpose:?}");
    }
}

/// A turn never reads another turn's scratch dir: a concurrent turn's home
/// holds its CLI's sign-in (a Copilot `hosts.json` is a GitHub token, a
/// Codex `auth.json` an OpenAI one), and every turn dir sits side by side in
/// the system temp dir, readable by the same user.
#[cfg(unix)]
#[test]
fn review2_a_turn_never_reads_another_turns_scratch_live() {
    let Some(e) = super::tests::real_executor() else { return };
    let other = make_turn_tmp().unwrap();
    let cred = other.path().join("home/.config/github-copilot/hosts.json");
    std::fs::create_dir_all(cred.parent().unwrap()).unwrap();
    std::fs::write(&cred, "DECOY-OTHER-TURN-TOKEN").unwrap();
    let d = DecoyHome::new();
    for purpose in PURPOSES {
        let (_, out) = run_in(&*e, &d, *purpose, &format!("cat '{}'; ls '{}'", cred.display(), other.path().display()), PathGrants::default());
        assert!(!out.contains("DECOY") && !out.contains(".ligus-owner"), "{purpose:?} read another turn's scratch dir: {out}");
        // Its own scratch dir still works.
        let (ok, out) = run_in(&*e, &d, *purpose, "echo mine > \"$TMPDIR/x\" && cat \"$TMPDIR/x\"", PathGrants::default());
        assert!(ok && out.contains("mine"), "{purpose:?}: {out}");
    }
}

/// APFS is case-insensitive: a read-write grant spelled in another case
/// (`~/Library/launchagents`, `~/.SSH`) for a path that doesn't exist yet
/// was compared case-sensitively, so it was neither a code path nor a
/// denied one, and the turn could create the real `~/Library/LaunchAgents`
/// (a login item runs unconfined) or `~/.ssh` (`authorized_keys`, `rc`).
#[cfg(target_os = "macos")]
#[test]
fn review2_rw_grant_case_variants_never_reach_code_or_denied_paths_live() {
    let Some(e) = super::tests::real_executor() else { return };
    let d = DecoyHome::new();
    std::fs::remove_dir_all(d.home.join(".ssh")).unwrap();
    let grants = PathGrants { read_only: vec![], read_write: vec!["~/Library/launchagents".into(), "~/.SSH".into(), "~/.Zshrc.d".into()] };
    let h = d.home.display().to_string();
    let script = format!(
        "mkdir -p '{h}/Library/launchagents' && echo x > '{h}/Library/launchagents/evil.plist' && echo PLANTED-AGENT; mkdir -p '{h}/.SSH' && echo k > '{h}/.SSH/authorized_keys' && echo PLANTED-SSH"
    );
    for purpose in PURPOSES {
        let (_, out) = run_in(&*e, &d, *purpose, &script, grants.clone());
        assert!(!out.contains("PLANTED"), "{purpose:?}: {out}");
        assert!(!d.home.join("Library/LaunchAgents/evil.plist").exists() && !d.home.join(".ssh/authorized_keys").exists(), "{purpose:?}");
    }
}

/// More paths whose contents run as the user outside the sandbox: a Python
/// user site (`.pth` files run on every interpreter start), package caches a
/// build compiles from (`~/.cargo/registry/src/*/build.rs`), editor settings
/// and extensions (VS Code and Cursor tasks), `~/bin`, tool plugins
/// (`~/.docker/cli-plugins`, gh extensions, Terraform providers), and the
/// shell's extra startup files. A read-write grant of one is read-only.
#[cfg(unix)]
#[test]
fn review2_rw_grants_never_open_more_code_paths() {
    let d = DecoyHome::new();
    let rw: Vec<&str> = vec![
        ".local/lib",
        ".local/lib/python3.12/site-packages",
        "Library/Python",
        ".cargo/registry",
        ".cargo/git",
        ".cargo/env",
        "bin",
        ".bin",
        ".vscode",
        ".vscode/extensions",
        ".cursor",
        ".config/Code/User",
        ".config/Cursor/User",
        "Library/Application Support/Code/User",
        "Library/Application Support/Cursor/User",
        ".docker/cli-plugins",
        ".local/share/gh",
        ".terraform.d",
        ".ipython",
        ".gradle",
        ".m2",
        "go/pkg/mod",
        ".bash_aliases",
        ".bashrc.d",
        ".oh-my-zsh",
        ".zsh",
        ".config/zsh",
        ".config/starship.toml",
        ".config/tmux",
        ".config/kitty",
        ".config/wezterm",
        ".wezterm.lua",
        ".config/zed",
        ".hgrc",
        ".pythonrc",
        ".pdbrc",
        ".irbrc",
        ".gemrc",
        ".gem",
        ".bundle",
        ".yarnrc",
        ".yarnrc.yml",
        ".pip",
        ".pydistutils.cfg",
        ".hammerspoon",
        "Library/Services",
        "Library/Scripts",
        "Library/Application Scripts",
        ".config/npm",
        ".config/pnpm",
        ".config/yarn",
        ".deno",
        ".bun",
    ];
    let grants = PathGrants { read_only: vec![], read_write: rw.iter().map(|r| format!("~/{r}")).collect() };
    let p = plan_for(&d.home, &d.host(), Purpose::Tool, &grants);
    let open: Vec<&&str> = rw.iter().filter(|r| p.write_extra.contains(&d.home.join(r))).collect();
    assert!(open.is_empty(), "granted read-write: {open:?}");
    // Absolute prefixes of user-writable installs on `PATH` (Homebrew).
    let abs = PathGrants { read_only: vec![], read_write: vec!["/opt/homebrew".into(), "/usr/local".into(), "/usr/local/bin".into()] };
    let p = plan_for(&d.home, &d.host(), Purpose::Tool, &abs);
    assert!(p.write_extra.iter().all(|w| !w.starts_with("/opt/homebrew") && !w.starts_with("/usr/local")), "{:?}", p.write_extra);
    // A data grant beside them still opens read-write.
    let ok = PathGrants { read_only: vec![], read_write: vec!["~/granted/rw".into(), "~/datasets".into()] };
    let p = plan_for(&d.home, &d.host(), Purpose::Tool, &ok);
    assert!(p.write_extra.contains(&d.home.join("granted/rw")) && p.write_extra.contains(&d.home.join("datasets")), "{:?}", p.write_extra);
}

/// A path is judged where it resolves when the plan is made; the Linux
/// executors resolved it again when they built the bind mounts and the
/// Landlock rules. A grant swapped for a symlink into `~/.ssh` in between
/// (an agent with write access to the grant's parent, in a concurrent turn)
/// opened the keys.
#[cfg(target_os = "linux")]
#[test]
fn review2_a_path_swapped_after_the_plan_opens_nothing() {
    let d = DecoyHome::new();
    std::fs::create_dir_all(d.home.join("shared/cache")).unwrap();
    let grants = PathGrants { read_only: vec!["~/shared/cache".into()], read_write: vec!["~/granted/rw".into()] };
    let p = plan_for(&d.home, &d.host(), Purpose::Tool, &grants);
    assert!(p.home_read.iter().any(|r| canonical(r) == canonical(&d.home.join("shared/cache"))), "{:?}", p.home_read);
    // The swap, after the plan.
    for g in ["shared/cache", "granted/rw"] {
        std::fs::rename(d.home.join(g), d.home.join(format!("{g}-old"))).unwrap();
        std::os::unix::fs::symlink(d.home.join(".ssh"), d.home.join(g)).unwrap();
    }
    let ssh = canonical(&d.home.join(".ssh"));
    let a: Vec<String> = super::linux::bwrap_args(&p, &d.worktree).iter().map(|x| x.to_string_lossy().into_owned()).collect();
    let bound = a.windows(2).any(|w| (w[0] == "--ro-bind" || w[0] == "--bind") && Path::new(&w[1]).starts_with(&ssh));
    assert!(!bound, "bubblewrap binds ~/.ssh: {a:?}");
    let r = super::landlock_rules::rules(&p, Some(&d.home), None, None);
    assert!(!r.read.iter().chain(r.write.iter()).any(|x| x.starts_with(&ssh)), "Landlock opens ~/.ssh: {:?}", r.read);
}

/// `HOME` that isn't the user's home (unset, or pointing elsewhere) left the
/// real home open: the plan denied only the dir `HOME` named. The password
/// database's home is denied too.
#[cfg(unix)]
#[test]
fn review2_a_wrong_home_still_denies_the_real_one_live() {
    assert!(passwd_home().is_some(), "no home in the password database");
    let wrong = canonical(&tempfile::tempdir().unwrap().keep());
    assert_eq!(other_homes(Some(&wrong), Some(Path::new("/home/u"))), vec![PathBuf::from("/home/u")]);
    assert_eq!(other_homes(None, Some(Path::new("/home/u"))), vec![PathBuf::from("/home/u")]);
    assert!(other_homes(Some(Path::new("/home/u")), Some(Path::new("/home/u"))).is_empty());
    let Some(e) = super::tests::real_executor() else { return };
    let d = DecoyHome::new();
    let host = HostPaths { other_homes: vec![d.home.clone()], ..d.host() };
    let p = plan_for(&wrong, &host, Purpose::Tool, &PathGrants::default());
    assert!(p.deny_read.contains(&d.home) && p.deny_read.contains(&d.home.join(".ssh")), "{:?}", p.deny_read);
    for purpose in PURPOSES {
        let policy = TurnPolicy { worktree_is_isolated: true, ..TurnPolicy::baseline(HostKind::Desktop) };
        let out = with_turn_sync(policy, || {
            let script = format!("cat '{}' '{}'; cat README.md", d.home.join("Documents/taxes.txt").display(), d.home.join(".ssh/id_ed25519").display());
            let spec = SpawnSpec::new(*purpose, "/bin/sh", ["-c", script.as_str(), "sh", "--permission-mode", "acceptEdits"]).cwd(Some(&d.worktree));
            let mut p = prepare_in(&*e, spec, &wrong, host.clone()).expect("prepare");
            let out = p.command.output().expect("spawn");
            drop(p.guard);
            format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
        });
        assert!(!out.contains("DECOY"), "{purpose:?} read the real home under a wrong HOME: {out}");
        assert!(out.contains("hello"), "{purpose:?}: the worktree inside it stays readable: {out}");
    }
}

/// A grant of a tool's dir (`~/.cargo`, `~/.m2`) or of a config file
/// (`~/.npmrc`) opened the registry tokens kept in it: only `~/.ssh`-style
/// credential dirs were hard denies.
#[cfg(unix)]
#[test]
fn review2_a_grant_of_a_tool_dir_never_opens_its_tokens_live() {
    let Some(e) = super::tests::real_executor() else { return };
    let d = DecoyHome::new();
    for (f, body) in [(".m2/settings.xml", "DECOY-MAVEN"), (".pypirc", "DECOY-PYPI"), (".gradle/gradle.properties", "DECOY-GRADLE")] {
        std::fs::create_dir_all(d.home.join(f).parent().unwrap()).unwrap();
        std::fs::write(d.home.join(f), body).unwrap();
    }
    let grants = PathGrants {
        read_only: vec!["~/.cargo".into(), "~/.m2".into(), "~/.npmrc".into(), "~/.pypirc".into()],
        read_write: vec!["~/.gradle".into()],
    };
    let h = d.home.display().to_string();
    for purpose in PURPOSES {
        let (_, out) = run_in(
            &*e,
            &d,
            *purpose,
            &format!("cat '{h}/.cargo/credentials.toml' '{h}/.m2/settings.xml' '{h}/.npmrc' '{h}/.pypirc' '{h}/.gradle/gradle.properties'; cat '{h}/.cargo/bin/decoy-tool'"),
            grants.clone(),
        );
        assert!(!out.contains("DECOY"), "{purpose:?}: {out}");
        // The rest of a granted dir reads (the Landlock tier can't list a dir
        // that holds a denied file, but reads what is in it).
        assert!(out.contains("TOOLCHAIN-OK"), "{purpose:?}: the rest of a granted dir reads: {out}");
    }
}

/// A CLI started outside any turn on the legacy path (no ACP adapter) kept
/// the shell its runtime args name (Copilot's `--allow-tool=shell`,
/// Antigravity's `--mode auto`, pi's bash): only Claude Code and ACP
/// sessions read the turn's shell rule.
#[test]
fn review2_outside_a_turn_a_cli_has_no_shell() {
    use crate::provider::dispatch::outside_turn_args;
    let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let copilot = outside_turn_args(ModelProviderKind::Copilot, a(&["--allow-tool=read", "--allow-tool=write", "--allow-tool=shell", "--allow-all-tools"]));
    assert!(!copilot.iter().any(|x| x == "--allow-tool=shell" || x == "--allow-all-tools") && copilot.contains(&"--deny-tool=shell".to_string()), "{copilot:?}");
    let agy = outside_turn_args(ModelProviderKind::Antigravity, a(&["--mode", "auto"]));
    assert!(!agy.contains(&"auto".to_string()), "{agy:?}");
    let pi = outside_turn_args(ModelProviderKind::Pi, a(&[]));
    assert!(pi.windows(2).any(|w| w == ["--exclude-tools", "bash"]), "{pi:?}");
    // One with no switch keeps its args (and runs read-only, model only).
    assert_eq!(outside_turn_args(ModelProviderKind::Codex, a(&["-m", "x"])), a(&["-m", "x"]));
}

/// Tests never copy the real user's sign-ins into a turn home, nor read the
/// login keychain to bridge Claude Code's, unless asked to
/// (`LIGUS_TEST_KEYCHAIN=1`): almost every executor test spawns a CLI turn
/// against the real home.
#[test]
fn review2_tests_never_carry_real_sign_ins() {
    if std::env::var("LIGUS_TEST_KEYCHAIN").is_ok_and(|v| v.trim() == "1") {
        return;
    }
    assert!(cli_home::in_test_binary());
    assert!(!cli_home::real_sign_ins_allowed());
}

/// Git's global hooks dir runs as the user in every repository: a
/// read-write grant of it (`core.hooksPath = ~/.githooks`) is read-only.
#[cfg(unix)]
#[test]
fn review2_rw_grant_of_the_git_hooks_dir_is_read_only() {
    let d = DecoyHome::new();
    std::fs::write(d.home.join(".gitconfig"), "[core]\n\thooksPath = ~/.githooks\n").unwrap();
    std::fs::create_dir_all(d.home.join(".githooks")).unwrap();
    let dirs = cli_home::git_exec_dirs(&d.home);
    assert!(dirs.contains(&d.home.join(".githooks")), "{dirs:?}");
    let host = HostPaths { exec_dirs: dirs, ..d.host() };
    let grants = PathGrants { read_only: vec![], read_write: vec!["~/.githooks".into(), "~/datasets".into()] };
    let p = plan_for(&d.home, &host, Purpose::Tool, &grants);
    assert!(!p.write_extra.iter().any(|w| w.ends_with(".githooks")), "{:?}", p.write_extra);
    assert!(p.write_extra.contains(&d.home.join("datasets")), "{:?}", p.write_extra);
}

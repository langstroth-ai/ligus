//! Phase C: the home dir is default-deny for every agent turn. The probes
//! run against a decoy home in a temp dir (decoy keys, documents, a decoy
//! desktop data dir, another workspace's worktree), never the user's own
//! files, and write only inside temp dirs.

use super::*;
use hive_core::sandbox::PathGrants;

/// A decoy home: secrets and documents an agent must never read, a decoy
/// toolchain it may run, and a turn worktree inside it.
pub(crate) struct DecoyHome {
    pub home: PathBuf,
    pub worktree: PathBuf,
}

/// The desktop app's data dir under a home, on this OS.
fn data_dir_rel() -> PathBuf {
    if cfg!(target_os = "macos") {
        Path::new("Library/Application Support").join(hive_core::compat::DESKTOP_APP_ID)
    } else {
        Path::new(".local/share").join(hive_core::compat::DESKTOP_APP_ID)
    }
}

/// (path relative to the home, content) of every decoy secret.
fn decoys() -> Vec<(PathBuf, &'static str)> {
    vec![
        (".ssh/id_ed25519".into(), "DECOY-SSH-KEY"),
        ("Documents/taxes.txt".into(), "DECOY-DOCUMENT"),
        (data_dir_rel().join("settings.json"), "DECOY-SETTINGS"),
        (".ligus/keys/device.seed".into(), "DECOY-SEED"),
        ("projects/other/.ligus/worktrees/chat-b/notes.txt".into(), "DECOY-OTHER-WORKTREE"),
        ("projects/other/src/lib.rs".into(), "DECOY-OTHER-REPO"),
        (".cargo/credentials.toml".into(), "DECOY-CARGO-TOKEN"),
        (".npmrc".into(), "DECOY-NPM-TOKEN"),
        (".gitconfig".into(), "[url \"https://DECOY-GIT-TOKEN@example.com/\"]\n\tinsteadOf = https://example.com/\n[user]\n\tname = Decoy Person\n\temail = decoy@example.com\n"),
        (".zshrc".into(), "DECOY-ZSHRC"),
    ]
}

impl DecoyHome {
    pub fn new() -> Self {
        let root = canonical(&tempfile::tempdir().unwrap().keep());
        let home = root.join("home");
        for (rel, body) in decoys() {
            let p = home.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
        }
        // A decoy toolchain (allowed) and grantable dirs.
        let tool = home.join(".cargo/bin/decoy-tool");
        std::fs::create_dir_all(tool.parent().unwrap()).unwrap();
        std::fs::write(&tool, "#!/bin/sh\necho TOOLCHAIN-OK\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::create_dir_all(home.join("granted/ro")).unwrap();
        std::fs::write(home.join("granted/ro/data.txt"), "GRANTED-RO").unwrap();
        std::fs::create_dir_all(home.join("granted/rw")).unwrap();
        let worktree = home.join("projects/ws/.ligus/worktrees/chat-a");
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        std::fs::write(worktree.join("README.md"), "hello").unwrap();
        DecoyHome { home, worktree }
    }

    /// This host's paths for the decoy home: its data dirs denied, its decoy
    /// toolchain allowed.
    pub fn host(&self) -> HostPaths {
        let t = toolchain_allow(&self.home, None, &|_| None, &|p| p.exists());
        HostPaths {
            data_dirs: vec![self.home.join(".ligus"), self.home.join(data_dir_rel())],
            toolchain_read: t.read.into_iter().map(|(p, _)| p).collect(),
            toolchain_env: t.env,
            ..Default::default()
        }
    }
}

/// Run `script` (`sh -c`) as `purpose` in the decoy's worktree with `grants`.
pub(crate) fn run_in(e: &dyn TurnExecutor, d: &DecoyHome, purpose: Purpose, script: &str, grants: PathGrants) -> (bool, String) {
    let policy = TurnPolicy { worktree_is_isolated: true, grants, ..TurnPolicy::baseline(HostKind::Desktop) };
    with_turn_sync(policy, || {
        // The tier flags ride along as positional args so a claude turn is
        // write-capable.
        let spec = SpawnSpec::new(purpose, "/bin/sh", ["-c", script, "sh", "--permission-mode", "acceptEdits"]).cwd(Some(&d.worktree));
        let mut p = prepare_in(e, spec, &d.home, d.host()).expect("prepare");
        let out = p.command.output().expect("spawn");
        drop(p.guard);
        (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    })
}

const PURPOSES: &[Purpose] = &[Purpose::Agent(ModelProviderKind::ClaudeCode), Purpose::Agent(ModelProviderKind::Codex), Purpose::Tool];

#[test]
fn sandbox_home_is_default_deny() {
    let Some(e) = super::tests::real_executor() else { return };
    let d = DecoyHome::new();
    let h = d.home.display().to_string();
    for purpose in PURPOSES {
        for (rel, body) in decoys() {
            let target = d.home.join(&rel);
            let (ok, out) = run_in(&*e, &d, *purpose, &format!("cat '{}'", target.display()), PathGrants::default());
            let marker = body.lines().next().unwrap_or(body);
            assert!(!ok && !out.contains("DECOY"), "{purpose:?} read {}: {out}", rel.display());
            let _ = marker;
        }
        // The home can't be listed, nor any dir in it.
        for dir in [h.clone(), format!("{h}/Documents"), format!("{h}/projects"), format!("{h}/projects/other")] {
            let (ok, out) = run_in(&*e, &d, *purpose, &format!("ls '{dir}'"), PathGrants::default());
            assert!(!ok || !(out.contains("Documents") || out.contains("taxes") || out.contains("src")), "{purpose:?} listed {dir}: {out}");
        }
        // Nothing in it can be written: a shell rc, a new file.
        let (ok, out) = run_in(&*e, &d, *purpose, &format!("echo pwned >> '{h}/.zshrc'; echo x > '{h}/new-file'; test -e '{h}/new-file' && echo MADE"), PathGrants::default());
        assert!(!out.contains("MADE"), "{purpose:?}: {ok} {out}");
        assert_eq!(std::fs::read_to_string(d.home.join(".zshrc")).unwrap(), "DECOY-ZSHRC", "{purpose:?}");
        assert!(!d.home.join("new-file").exists());
        // The worktree inside the home works: read, write, resolve its path.
        let (ok, out) = run_in(&*e, &d, *purpose, "cat README.md && echo w > src/new.txt && cat src/new.txt && pwd -P", PathGrants::default());
        assert!(ok && out.contains("hello") && out.contains(&d.worktree.display().to_string()), "{purpose:?}: {out}");
        // The decoy toolchain runs (the allow-list), its credentials don't read.
        let (ok, out) = run_in(&*e, &d, *purpose, &format!("'{h}/.cargo/bin/decoy-tool'"), PathGrants::default());
        assert!(ok && out.contains("TOOLCHAIN-OK"), "{purpose:?}: {out}");
        // HOME is the turn's own, never the real one.
        let (ok, out) = run_in(&*e, &d, *purpose, &format!("test \"$HOME\" != '{h}' && echo OTHER-HOME"), PathGrants::default());
        assert!(ok && out.contains("OTHER-HOME"), "{purpose:?}: {out}");
    }
}

#[test]
fn sandbox_home_grants_open_only_what_they_name() {
    let Some(e) = super::tests::real_executor() else { return };
    let d = DecoyHome::new();
    let h = d.home.display().to_string();
    let grants = PathGrants {
        // A grant of a credential dir or the data dir opens nothing.
        read_only: vec!["~/granted/ro".into(), "~/.ssh".into(), format!("~/{}", data_dir_rel().display())],
        read_write: vec!["~/granted/rw".into(), "~/.ligus".into()],
    };
    for purpose in PURPOSES {
        let (ok, out) = run_in(&*e, &d, *purpose, &format!("cat '{h}/granted/ro/data.txt'"), grants.clone());
        assert!(ok && out.contains("GRANTED-RO"), "{purpose:?}: {out}");
        let (_, out) = run_in(&*e, &d, *purpose, &format!("echo x > '{h}/granted/ro/new' && echo WROTE"), grants.clone());
        assert!(!out.contains("WROTE") && !d.home.join("granted/ro/new").exists(), "{purpose:?}: read-only grant written: {out}");
        let (ok, out) = run_in(&*e, &d, *purpose, &format!("echo x > '{h}/granted/rw/new' && cat '{h}/granted/rw/new'"), grants.clone());
        assert!(ok && out.contains('x'), "{purpose:?}: {out}");
        let _ = std::fs::remove_file(d.home.join("granted/rw/new"));
        for rel in [".ssh/id_ed25519".into(), data_dir_rel().join("settings.json"), PathBuf::from(".ligus/keys/device.seed")] {
            let (ok, out) = run_in(&*e, &d, *purpose, &format!("cat '{}'", d.home.join(&rel).display()), grants.clone());
            assert!(!ok && !out.contains("DECOY"), "{purpose:?}: a grant opened {}: {out}", rel.display());
        }
        let (_, out) = run_in(&*e, &d, *purpose, &format!("echo x > '{h}/.ligus/planted' && echo WROTE"), grants.clone());
        assert!(!out.contains("WROTE") && !d.home.join(".ligus/planted").exists(), "{purpose:?}: {out}");
    }
}

/// The turn home carries the user's git identity and nothing else from
/// their git config (no URL rewrites with tokens, no credential helpers).
#[test]
fn sandbox_turn_home_has_only_the_git_identity() {
    let Some(e) = super::tests::real_executor() else { return };
    if std::process::Command::new("git").arg("--version").output().map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("SKIPPED sandbox_turn_home_has_only_the_git_identity: no git");
        return;
    }
    let d = DecoyHome::new();
    for purpose in PURPOSES {
        let (ok, out) = run_in(&*e, &d, *purpose, "git config --global user.name; cat \"$HOME/.gitconfig\"", PathGrants::default());
        assert!(ok && out.contains("Decoy Person"), "{purpose:?}: {out}");
        assert!(!out.contains("DECOY-GIT-TOKEN"), "{purpose:?}: {out}");
        // A commit in a scratch repo works with that identity (in the
        // worktree `.git` stays read-only: Ligus captures the diff).
        let (ok, out) = run_in(
            &*e,
            &d,
            *purpose,
            "cd \"$TMPDIR\" && git init -q scratch && cd scratch && echo a > a && git add a && git commit -qm first && git log --format=%an",
            PathGrants::default(),
        );
        assert!(ok && out.contains("Decoy Person"), "{purpose:?}: {out}");
    }
}

// ---------------------------------------------------------------------------
// The plan (pure)
// ---------------------------------------------------------------------------

fn plan_for(home: &Path, host: &HostPaths, purpose: Purpose, grants: &PathGrants) -> EnforcementPlan {
    EnvelopeEnforcer::plan(&PlanInputs {
        host: HostKind::Desktop,
        envelope: &SandboxEnvelope::workspace_default(),
        purpose,
        gate: &GatePlan::Full,
        worktree: &home.join("projects/ws"),
        worktree_is_isolated: true,
        write_capable: true,
        tmp: Path::new("/tmp/ligus-turn-x"),
        home,
        host_paths: host,
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
fn plan_denies_the_home_and_lists_what_it_lets_back() {
    let home = Path::new("/home/u");
    let host = HostPaths {
        data_dirs: vec![home.join(".ligus")],
        toolchain_read: vec![home.join(".cargo/bin"), home.join(".rustup")],
        toolchain_env: vec![("RUSTUP_HOME".into(), home.join(".rustup"))],
        home_read_allow: vec![home.join(".aws")],
        ..Default::default()
    };
    let grants = PathGrants {
        read_only: vec!["~/data".into(), "~/.ssh".into(), "~/.ligus/keys".into(), "/opt/models".into()],
        read_write: vec!["~/scratch".into(), "~/.ssh/x".into(), "~/.ligus".into()],
    };
    for purpose in [Purpose::Tool, Purpose::Preflight, Purpose::McpServer, Purpose::Agent(ModelProviderKind::Aider)] {
        let p = plan_for(home, &host, purpose, &grants);
        assert_eq!(p.home.as_deref(), Some(home), "{purpose:?}");
        for r in [home.join(".cargo/bin"), home.join(".rustup"), home.join("data"), PathBuf::from("/opt/models"), home.join(".aws")] {
            assert!(p.home_read.contains(&r), "{purpose:?} {}", r.display());
        }
        // Signed grants never open a credential dir or the data dir; the
        // device-local list may open a credential dir (.aws above).
        for r in [home.join(".ssh"), home.join(".ligus/keys"), home.join(".ligus")] {
            assert!(!p.home_read.contains(&r) && !p.write_extra.contains(&r), "{purpose:?} {}", r.display());
        }
        assert!(p.write_extra.contains(&home.join("scratch")) && !p.write_extra.contains(&home.join(".ssh/x")), "{purpose:?}");
        assert!(p.deny_read.contains(&home.join(".ssh")) && p.deny_read.contains(&home.join(".ligus")), "{purpose:?}");
        // The turn home and the toolchain roots.
        let env = |k: &str| p.env.iter().find(|(x, _)| x == k).map(|(_, v)| PathBuf::from(v));
        assert_eq!(env("RUSTUP_HOME"), Some(home.join(".rustup")), "{purpose:?}");
        if !matches!(purpose, Purpose::Agent(_)) {
            assert_eq!(env("HOME"), Some(PathBuf::from("/tmp/ligus-turn-x/home")), "{purpose:?}");
        }
    }
}

#[test]
fn toolchain_allow_lists_install_dirs_only() {
    let home = Path::new("/home/u");
    let exists = |p: &Path| {
        [".cargo/bin", ".rustup", ".nvm/versions", ".local/bin", ".pyenv", "tools/bin", ".ssh/bin", ".config/gh"].iter().any(|r| p == home.join(r))
    };
    let path = std::env::join_paths([home.join("tools/bin"), home.join(".ssh/bin"), PathBuf::from("/usr/bin"), home.to_path_buf()]).unwrap();
    let env = |k: &str| (k == "PYENV_ROOT").then(|| OsString::from("/opt/pyenv"));
    let t = toolchain_allow(home, Some(&path), &env, &|p| exists(p) || p == Path::new("/opt/pyenv"));
    let read: Vec<PathBuf> = t.read.iter().map(|(p, _)| p.clone()).collect();
    for r in [".cargo/bin", ".rustup", ".nvm/versions", ".local/bin", "tools/bin"] {
        assert!(read.contains(&home.join(r)), "{r}: {read:?}");
    }
    // Never the home, a credential dir, a config dir, or a path outside it.
    for r in [home.to_path_buf(), home.join(".ssh/bin"), home.join(".config/gh"), PathBuf::from("/usr/bin"), home.join(".cargo")] {
        assert!(!read.contains(&r), "{}: {read:?}", r.display());
    }
    // Root variables: the default dir when it exists, the user's own value
    // when set.
    assert!(t.env.contains(&("RUSTUP_HOME".into(), home.join(".rustup"))));
    assert!(t.env.contains(&("PYENV_ROOT".into(), PathBuf::from("/opt/pyenv"))));
    // Every fixed entry says why.
    assert!(TOOLCHAIN_DIRS.iter().all(|(rel, why)| !rel.is_empty() && !why.is_empty() && !rel.starts_with(".ssh")));
}

#[test]
fn program_install_dir_is_the_prefix_under_the_home() {
    let root = canonical(&tempfile::tempdir().unwrap().keep());
    let home = root.join("home");
    let pkg = home.join(".codex/packages/standalone/1.0/bin");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(pkg.join("codex"), "").unwrap();
    std::fs::create_dir_all(home.join(".local/bin")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(pkg.join("codex"), home.join(".local/bin/codex")).unwrap();
    #[cfg(unix)]
    assert_eq!(program_install_dir(home.join(".local/bin/codex").as_os_str(), &home), Some(home.join(".codex/packages/standalone/1.0")));
    // Outside the home, or the home itself: nothing.
    assert_eq!(program_install_dir(OsStr::new("/bin/sh"), &home), None);
    std::fs::write(home.join("tool"), "").unwrap();
    assert_eq!(program_install_dir(home.join("tool").as_os_str(), &home), None);
}

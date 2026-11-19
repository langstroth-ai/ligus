//! Phase C: the home dir is default-deny for every agent turn. The probes
//! run against a decoy home in a temp dir (decoy keys, documents, a decoy
//! desktop data dir, another workspace's worktree), never the user's own
//! files, and write only inside temp dirs.

use super::*;
use ligus_core::sandbox::PathGrants;

/// A decoy home: secrets and documents an agent must never read, a decoy
/// toolchain it may run, and a turn worktree inside it.
pub(crate) struct DecoyHome {
    pub home: PathBuf,
    pub worktree: PathBuf,
}

/// The desktop app's data dir under a home, on this OS.
pub(crate) fn data_dir_rel() -> PathBuf {
    if cfg!(target_os = "macos") {
        Path::new("Library/Application Support").join(ligus_core::compat::DESKTOP_APP_ID)
    } else {
        Path::new(".local/share").join(ligus_core::compat::DESKTOP_APP_ID)
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

#[cfg(unix)]
pub(crate) fn plan_for(home: &Path, host: &HostPaths, purpose: Purpose, grants: &PathGrants) -> EnforcementPlan {
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

// Unix paths (`/home/u`): Windows has no agent sandbox to plan for.
#[cfg(unix)]
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

// Unix paths (`/home/u`): Windows has no agent sandbox to plan for.
#[cfg(unix)]
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

#[test]
fn effective_policy_names_every_path_with_a_reason() {
    let d = DecoyHome::new();
    let host = d.host();
    let grants = PathGrants { read_only: vec!["~/granted/ro".into(), "~/.ssh".into()], read_write: vec!["~/granted/rw".into()] };
    let p = effective_policy_in(Some(ModelProviderKind::ClaudeCode), &grants, false, &d.home, &host, None, &|_| None);
    let find = |path: &Path, access: &str| p.entries.iter().find(|e| e.path == path.display().to_string() && e.access == access).cloned();
    assert!(find(&d.home, "denied").is_some(), "{p:?}");
    assert!(find(&d.home.join(".cargo/bin"), "read").is_some_and(|e| e.why.starts_with("toolchain")), "{p:?}");
    assert!(find(&d.home.join("granted/ro"), "read").is_some_and(|e| e.why.contains("signed")), "{p:?}");
    assert!(find(&d.home.join("granted/rw"), "read-write").is_some_and(|e| e.why.contains("signed")), "{p:?}");
    assert!(find(&d.home.join(".ssh"), "denied").is_some() && find(&d.home.join(".ssh"), "read").is_none(), "{p:?}");
    assert!(find(&d.home.join(".ligus"), "denied").is_some_and(|e| e.why.contains("Ligus's data")), "{p:?}");
    assert!(p.entries.iter().all(|e| !e.why.is_empty()));
    assert!(p.shell.contains("Owner"));
    let codex = effective_policy_in(Some(ModelProviderKind::Codex), &PathGrants::default(), false, &d.home, &host, None, &|_| None);
    assert!(codex.shell.contains("refused"));
    let granted = effective_policy_in(Some(ModelProviderKind::Codex), &PathGrants::default(), true, &d.home, &host, None, &|_| None);
    assert!(granted.shell.starts_with("on"));
}

// ---------------------------------------------------------------------------
// Adversarial review: escapes from the allow-list and the grants
// ---------------------------------------------------------------------------

/// Run `program args` as `purpose` in the decoy's worktree, with `host`
/// paths and `grants`.
#[cfg(unix)]
pub(crate) fn run_prog(e: &dyn TurnExecutor, d: &DecoyHome, host: HostPaths, purpose: Purpose, program: &Path, args: &[&str], grants: PathGrants) -> (bool, String) {
    let policy = TurnPolicy { worktree_is_isolated: true, grants, ..TurnPolicy::baseline(HostKind::Desktop) };
    with_turn_sync(policy, || {
        let spec = SpawnSpec::new(purpose, program.as_os_str(), args.iter().copied()).cwd(Some(&d.worktree));
        let mut p = prepare_in(e, spec, &d.home, host).expect("prepare");
        let out = p.command.output().expect("spawn");
        drop(p.guard);
        (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    })
}

#[cfg(unix)]
pub(crate) fn exe(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The program's own prefix is never a tool's home: `cargo` lives in
/// `~/.cargo/bin`, and `~/.cargo` holds `credentials.toml` (the crates.io
/// token); a real launcher in `~/.local/bin` (uv's installer, pip --user)
/// would open all of `~/.local`.
#[cfg(unix)]
#[test]
fn review_program_prefix_never_opens_a_tool_home() {
    let root = canonical(&tempfile::tempdir().unwrap().keep());
    let home = root.join("home");
    exe(&home.join(".cargo/bin/cargo"), "#!/bin/sh\n");
    std::fs::write(home.join(".cargo/credentials.toml"), "DECOY").unwrap();
    exe(&home.join(".local/bin/uv"), "#!/bin/sh\n");
    exe(&home.join("go/bin/gopls"), "#!/bin/sh\n");
    for (prog, never) in [(".cargo/bin/cargo", ".cargo"), (".local/bin/uv", ".local"), ("go/bin/gopls", "go")] {
        let got = program_install_dir(home.join(prog).as_os_str(), &home);
        assert_ne!(got, Some(home.join(never)), "{prog} opened ~/{never}");
    }
}

#[cfg(unix)]
#[test]
fn review_program_prefix_never_opens_a_tool_home_live() {
    let Some(e) = super::tests::real_executor() else { return };
    let d = DecoyHome::new();
    let cat = d.home.join(".cargo/bin/decoy-cat");
    exe(&cat, "#!/bin/sh\ncat \"$1\"\n");
    let creds = d.home.join(".cargo/credentials.toml");
    for purpose in [Purpose::Tool, Purpose::McpServer, Purpose::Agent(ModelProviderKind::Aider)] {
        let (_, out) = run_prog(&*e, &d, d.host(), purpose, &cat, &[creds.to_str().unwrap()], PathGrants::default());
        assert!(!out.contains("DECOY-CARGO-TOKEN"), "{purpose:?} read ~/.cargo/credentials.toml through the program's prefix: {out}");
    }
}

/// An allow-listed dir that is a symlink is judged by where it resolves:
/// `~/.local/bin` pointing at the home (or a grant pointing into `~/.ssh`)
/// opens nothing.
#[cfg(unix)]
fn symlinked_decoy() -> (DecoyHome, HostPaths, PathGrants) {
    let d = DecoyHome::new();
    std::fs::create_dir_all(d.home.join(".local")).unwrap();
    std::os::unix::fs::symlink(&d.home, d.home.join(".local/bin")).unwrap();
    std::os::unix::fs::symlink(d.home.join(".ssh"), d.home.join("granted/ssh-link")).unwrap();
    let path = std::env::join_paths([d.home.join(".local/bin")]).unwrap();
    let t = toolchain_allow(&d.home, Some(&path), &|_| None, &|p| p.exists());
    let host = HostPaths { toolchain_read: t.read.into_iter().map(|(p, _)| p).collect(), ..d.host() };
    let grants = PathGrants { read_only: vec!["~/granted/ssh-link".into()], read_write: vec![] };
    (d, host, grants)
}

#[cfg(unix)]
#[test]
fn review_symlinked_allow_entries_resolve_before_they_open() {
    let (d, host, grants) = symlinked_decoy();
    // The plan: nothing it opens resolves to the home, or into a denied path.
    let p = plan_for(&d.home, &host, Purpose::Tool, &grants);
    let real_home = canonical(&d.home);
    for r in p.home_read.iter().chain(p.write_extra.iter()) {
        let c = canonical(r);
        assert!(!real_home.starts_with(&c), "{} resolves to the home or above it", r.display());
        assert!(!p.deny_read.iter().any(|x| c.starts_with(canonical(x))), "{} resolves into a denied path", r.display());
    }
}

#[cfg(unix)]
#[test]
fn review_symlinked_allow_entries_resolve_before_they_open_live() {
    let Some(e) = super::tests::real_executor() else { return };
    let (d, host, grants) = symlinked_decoy();
    for purpose in PURPOSES {
        for target in [d.home.join("Documents/taxes.txt"), d.home.join(".ssh/id_ed25519"), d.home.join("granted/ssh-link/id_ed25519")] {
            let (_, out) = run_prog(&*e, &d, host.clone(), *purpose, Path::new("/bin/cat"), &[target.to_str().unwrap()], grants.clone());
            assert!(!out.contains("DECOY"), "{purpose:?} read {} through a symlinked allow entry: {out}", target.display());
        }
    }
}

/// A grant of the home's parent (`/home`, `/Users`) never brings the home
/// back, read-only or read-write.
#[cfg(unix)]
#[test]
fn review_a_grant_above_the_home_opens_nothing_in_it() {
    let d = DecoyHome::new();
    let parent = d.home.parent().unwrap().display().to_string();
    let grants = PathGrants { read_only: vec![parent.clone()], read_write: vec![parent.clone()] };
    let p = plan_for(&d.home, &d.host(), Purpose::Tool, &grants);
    let real_home = canonical(&d.home);
    for r in p.home_read.iter().chain(p.write_extra.iter()) {
        assert!(!real_home.starts_with(canonical(r)), "{} is the home or above it", r.display());
    }
}

#[cfg(unix)]
#[test]
fn review_a_grant_above_the_home_opens_nothing_in_it_live() {
    let Some(e) = super::tests::real_executor() else { return };
    let d = DecoyHome::new();
    let parent = d.home.parent().unwrap().display().to_string();
    let grants = PathGrants { read_only: vec![parent.clone()], read_write: vec![parent.clone()] };
    let h = d.home.display().to_string();
    for purpose in PURPOSES {
        let (_, out) = run_in(&*e, &d, *purpose, &format!("cat '{h}/Documents/taxes.txt'; echo x > '{h}/new-file'; test -e '{h}/new-file' && echo MADE"), grants.clone());
        assert!(!out.contains("DECOY") && !out.contains("MADE"), "{purpose:?}: a grant above the home opened it: {out}");
        assert!(!d.home.join("new-file").exists());
    }
}

/// A signed read-write grant never makes a path that runs code outside the
/// sandbox writable: a dir on `PATH` or in the toolchain allow-list (a planted
/// `git` runs as the user), a shell rc file, a login item. Those would hand
/// an Admin (and every member whose turns that agent answers) the shell the
/// gate keeps for the Owner, unconfined.
#[cfg(unix)]
fn code_path_decoy() -> (DecoyHome, HostPaths, Vec<&'static str>, PathGrants) {
    let d = DecoyHome::new();
    for dir in ["Library/LaunchAgents", ".config/autostart", ".config/systemd/user", ".local/bin"] {
        std::fs::create_dir_all(d.home.join(dir)).unwrap();
    }
    let path = std::env::join_paths([d.home.join(".local/bin")]).unwrap();
    let t = toolchain_allow(&d.home, Some(&path), &|_| None, &|p| p.exists());
    let host = HostPaths { toolchain_read: t.read.into_iter().map(|(p, _)| p).collect(), ..d.host() };
    let rw = [".cargo/bin", ".local/bin", ".zshrc", "Library/LaunchAgents", ".config/autostart", ".config/systemd/user", ".config", ".cargo"];
    let grants = PathGrants { read_only: vec![], read_write: rw.iter().map(|r| format!("~/{r}")).collect() };
    (d, host, rw.to_vec(), grants)
}

#[cfg(unix)]
#[test]
fn review_rw_grants_never_open_code_that_runs_outside_the_sandbox() {
    let (d, host, rw, grants) = code_path_decoy();
    let p = plan_for(&d.home, &host, Purpose::Tool, &grants);
    for r in rw {
        assert!(!p.write_extra.contains(&d.home.join(r)), "~/{r} granted read-write: {:?}", p.write_extra);
    }
    // A grant beside them still works. (`~/.cargo/registry` is a code path
    // too since the second review: a build compiles its sources.)
    let ok = PathGrants { read_only: vec![], read_write: vec!["~/granted/rw".into(), "~/datasets".into()] };
    let p = plan_for(&d.home, &host, Purpose::Tool, &ok);
    assert!(p.write_extra.contains(&d.home.join("granted/rw")) && p.write_extra.contains(&d.home.join("datasets")), "{:?}", p.write_extra);
}

#[cfg(unix)]
#[test]
fn review_rw_grants_never_open_code_that_runs_outside_the_sandbox_live() {
    let Some(e) = super::tests::real_executor() else { return };
    let (d, host, _, grants) = code_path_decoy();
    let h = d.home.display().to_string();
    let script = format!(
        "echo pwned >> '{h}/.zshrc'; printf '#!/bin/sh\\n' > '{h}/.cargo/bin/git' && echo PLANTED-CARGO; printf x > '{h}/.local/bin/git' && echo PLANTED-LOCAL; printf x > '{h}/Library/LaunchAgents/evil.plist' && echo PLANTED-AGENT; printf x > '{h}/.config/autostart/evil.desktop' && echo PLANTED-AUTOSTART"
    );
    let (_, out) = run_prog(&*e, &d, host.clone(), Purpose::Tool, Path::new("/bin/sh"), &["-c", &script], grants.clone());
    assert!(!out.contains("PLANTED"), "{out}");
    assert_eq!(std::fs::read_to_string(d.home.join(".zshrc")).unwrap(), "DECOY-ZSHRC");
}

/// Case and spelling never get a grant past the hard denies (APFS is case
/// insensitive): `~/.SSH`, `~/.LIGUS`.
#[cfg(target_os = "macos")]
#[test]
fn review_grant_case_variants_open_no_denied_path() {
    let Some(e) = super::tests::real_executor() else { return };
    let d = DecoyHome::new();
    let grants = PathGrants { read_only: vec!["~/.SSH".into(), "~/.LIGUS/keys".into()], read_write: vec!["~/.Ssh".into()] };
    for target in [".ssh/id_ed25519", ".SSH/id_ed25519", ".LIGUS/keys/device.seed", ".ligus/keys/device.seed"] {
        let (_, out) = run_in(&*e, &d, Purpose::Tool, &format!("cat '{}'", d.home.join(target).display()), grants.clone());
        assert!(!out.contains("DECOY"), "{target}: {out}");
    }
}

/// Risk 9: `ligus doctor --sandbox` and Settings, Sandbox show the login
/// keychain as readable for a CLI signed in by file, whose turns lose it.
#[cfg(target_os = "macos")]
#[test]
fn review_effective_policy_shows_the_keychain_denied_for_file_sign_in() {
    let d = DecoyHome::new();
    let host = d.host();
    let kc = d.home.join("Library/Keychains").display().to_string();
    let reads_kc = |p: &EffectivePolicy| p.entries.iter().any(|e| e.path == kc && e.access == "read");
    // Signed in by keychain: shown readable.
    let copilot = effective_policy_in(Some(ModelProviderKind::Copilot), &PathGrants::default(), false, &d.home, &host, None, &|_| None);
    assert!(reads_kc(&copilot), "{copilot:?}");
    // Signed in by file: shown denied, as the turn runs.
    for (kind, file) in [(ModelProviderKind::ClaudeCode, ".claude/.credentials.json"), (ModelProviderKind::Codex, ".codex/auth.json")] {
        let f = d.home.join(file);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, "{}").unwrap();
        let p = effective_policy_in(Some(kind), &PathGrants::default(), false, &d.home, &host, None, &|_| None);
        assert!(!reads_kc(&p), "{kind:?} signed in by file shows the keychain readable: {p:?}");
        assert!(p.entries.iter().any(|e| e.path == kc && e.access == "denied"), "{kind:?}: {p:?}");
    }
}

/// An agent CLI Ligus starts outside any turn (the chat summarizer and
/// compaction, a chat title, a runtime's Test) reads a prompt other members
/// wrote, in the workspace checkout itself: it writes nothing there and
/// reaches nothing but its model, whatever tier its runtime args name.
#[cfg(unix)]
#[test]
fn review_agent_outside_a_turn_is_read_only_live() {
    let Some(e) = super::tests::real_executor() else { return };
    assert!(current_turn().is_none());
    let d = DecoyHome::new();
    for kind in [ModelProviderKind::ClaudeCode, ModelProviderKind::Codex, ModelProviderKind::Copilot] {
        let spec = SpawnSpec::new(Purpose::Agent(kind), "/bin/sh", ["-c", "echo x > planted.txt && echo WROTE", "sh", "--permission-mode", "acceptEdits", "--mode", "auto"])
            .cwd(Some(&d.worktree));
        let mut p = prepare_in(&*e, spec, &d.home, d.host()).expect("prepare");
        assert!(!matches!(p.guard.plan().egress, EgressPlan::Open), "{kind:?}: open egress outside a turn");
        let out = p.command.output().expect("spawn");
        drop(p.guard);
        let out = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(!out.contains("WROTE") && !d.worktree.join("planted.txt").exists(), "{kind:?} wrote the checkout outside a turn: {out}");
    }
}

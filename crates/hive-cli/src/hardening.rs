//! Process hardening for long-running hosts (`ligus serve`, `ligus worker`),
//! mostly for container deployments (deploy/helm/ligus):
//!
//! - **Secrets as files.** `LIGUS_WORKSPACE_KEY_FILE`,
//!   `LIGUS_RELAY_ACCESS_TOKEN_FILE`, `LIGUS_REMOTE_REG_TOKEN_FILE` and
//!   `LIGUS_NTFY_TOKEN_FILE` name files holding the secret their
//!   non-`_FILE` variable would hold, and every file in `LIGUS_WS_SECRET_DIR`
//!   becomes `LIGUS_WS_SECRET_<file name>`. A Kubernetes Secret mounted as a
//!   volume then never appears in the pod spec's environment, in
//!   `/proc/<pid>/environ` or in `kubectl describe`. The values are loaded into
//!   this process only (agent processes never inherit Ligus's secrets), and the
//!   files are added to the paths agent processes can't read. The k8s
//!   secret-store backend's mount (`LIGUS_SECRETS_DIR`, where the Helm chart
//!   puts the workspace key as `cli.key`) is denied to agents the same way.
//! - **Not dumpable.** `PR_SET_DUMPABLE=0` on Linux, so no other process of
//!   the same user (an agent turn that escaped its sandbox, a debugging shell)
//!   can read this process's memory or `/proc/<pid>/environ`.
//! - **Fail closed without a sandbox.** With `LIGUS_REQUIRE_SANDBOX=1` a host
//!   refuses to start when this machine has no agent sandbox, instead of
//!   running and refusing every turn (`ligus doctor --require-confined` is the
//!   same check as a preflight).

use std::path::{Path, PathBuf};

/// The secrets a `<NAME>_FILE` variable may supply.
pub const FILE_SECRETS: &[&str] = &[
    "LIGUS_WORKSPACE_KEY",
    "LIGUS_RELAY_ACCESS_TOKEN",
    "LIGUS_REMOTE_REG_TOKEN",
    "LIGUS_NTFY_TOKEN",
];

/// The k8s secret-store backend's mount (see `hive_runtime::secret_store`).
pub const SECRETS_DIR: &str = "LIGUS_SECRETS_DIR";

/// Directory of workspace runtime secrets, one file per `secret_ref`.
pub const WS_SECRET_DIR: &str = "LIGUS_WS_SECRET_DIR";

/// Exit code of a host (or `ligus doctor --require-confined`) that refuses
/// to run because this machine has no agent sandbox.
pub const EXIT_UNCONFINED: i32 = 3;

/// A file secret's value: the file's contents without the trailing line end
/// (`kubectl create secret --from-file` keeps whatever the file had).
fn read_secret(path: &Path) -> std::io::Result<String> {
    let text = std::fs::read_to_string(path)?;
    Ok(text.trim_end_matches(['\n', '\r']).to_string())
}

/// What [`load_secret_files`] found: (variable, value) pairs to set and the
/// paths agent processes must not read.
#[derive(Debug, Default, PartialEq)]
pub struct SecretFiles {
    pub vars: Vec<(String, String)>,
    pub deny: Vec<PathBuf>,
    pub errors: Vec<String>,
}

/// A mounted Kubernetes Secret keeps its files behind `..data` symlinks that
/// change on every update; deny the mount directory, not just today's target.
fn deny_paths(path: &Path) -> Vec<PathBuf> {
    let mut v = vec![path.to_path_buf()];
    if let Ok(c) = std::fs::canonicalize(path) {
        if c != path {
            v.push(c);
        }
    }
    if let Some(parent) = path.parent().filter(|p| p.join("..data").exists()) {
        v.push(parent.to_path_buf());
    }
    v
}

/// Whether `name` may become part of an environment variable name.
fn valid_ref(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Resolve the `_FILE` variables and `LIGUS_WS_SECRET_DIR` with `get` (an
/// environment lookup). A variable already set directly wins over its file.
pub fn resolve_secret_files(get: impl Fn(&str) -> Option<String>) -> SecretFiles {
    let mut out = SecretFiles::default();
    for name in FILE_SECRETS {
        let Some(file) = get(&format!("{name}_FILE")) else { continue };
        if get(name).is_some() {
            out.errors.push(format!("both {name} and {name}_FILE are set; using {name}"));
            continue;
        }
        let path = PathBuf::from(file.trim());
        match read_secret(&path) {
            Ok(v) if !v.is_empty() => {
                out.vars.push((name.to_string(), v));
                out.deny.extend(deny_paths(&path));
            }
            Ok(_) => out.errors.push(format!("{name}_FILE {} is empty", path.display())),
            Err(e) => out.errors.push(format!("{name}_FILE {}: {e}", path.display())),
        }
    }
    // The k8s secret-store backend's mount (`LIGUS_SECRETS_DIR`, read by
    // `secret_store::K8sSecretFile` as `<dir>/<scope>.<name>`): Ligus's own
    // items, so agents never read it.
    // With the k8s backend and no LIGUS_SECRETS_DIR the store reads its
    // default mount; that one is Ligus's too.
    let k8s_backend = get("LIGUS_SECRET_BACKEND").is_some_and(|b| b.trim().eq_ignore_ascii_case("k8s"));
    let store_dir = get(SECRETS_DIR)
        .or_else(|| k8s_backend.then(|| hive_runtime::secret_store::K8S_SECRETS_DIR_DEFAULT.to_string()));
    if let Some(dir) = store_dir {
        let dir = PathBuf::from(dir.trim());
        if let Ok(c) = std::fs::canonicalize(&dir) {
            if c != dir {
                out.deny.push(c);
            }
        }
        out.deny.push(dir);
    }
    if let Some(dir) = get(WS_SECRET_DIR) {
        let dir = PathBuf::from(dir.trim());
        match std::fs::read_dir(&dir) {
            Ok(entries) => {
                let mut names: Vec<String> = entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    // Kubernetes' own `..data` / `..2026_…` entries.
                    .filter(|n| !n.starts_with('.'))
                    .collect();
                names.sort();
                for n in names {
                    if !valid_ref(&n) {
                        out.errors.push(format!("{WS_SECRET_DIR}: skipping {n:?} (letters, digits, _ and - only)"));
                        continue;
                    }
                    let var = hive_core::WorkspaceCredential::env_var(&n);
                    if get(&var).is_some() {
                        continue;
                    }
                    match read_secret(&dir.join(&n)) {
                        Ok(v) if !v.is_empty() => out.vars.push((var, v)),
                        Ok(_) => {}
                        Err(e) => out.errors.push(format!("{WS_SECRET_DIR}/{n}: {e}")),
                    }
                }
                out.deny.push(dir.clone());
                if let Ok(c) = std::fs::canonicalize(&dir) {
                    if c != dir {
                        out.deny.push(c);
                    }
                }
            }
            Err(e) => out.errors.push(format!("{WS_SECRET_DIR} {}: {e}", dir.display())),
        }
    }
    out
}

/// Load secret files into this process's environment. Call before any
/// thread starts (setting the environment is not thread-safe). Returns the
/// paths agent processes must not read.
pub fn load_secret_files() -> Vec<PathBuf> {
    let found = resolve_secret_files(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()));
    for e in &found.errors {
        eprintln!("secrets: {e}");
    }
    for (k, v) in &found.vars {
        std::env::set_var(k, v);
    }
    found.deny
}

/// Mark this process not dumpable (Linux): its memory and
/// `/proc/<pid>/environ` are then readable only with `CAP_SYS_PTRACE`, not by
/// other processes of the same user.
pub fn set_not_dumpable() {
    #[cfg(target_os = "linux")]
    // SAFETY: prctl(PR_SET_DUMPABLE) takes integer arguments only.
    unsafe {
        if libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0 {
            eprintln!("warning: prctl(PR_SET_DUMPABLE, 0) failed: {}", std::io::Error::last_os_error());
        }
    }
}

/// Whether this process is dumpable (Linux; always `None` elsewhere).
pub fn dumpable() -> Option<bool> {
    #[cfg(target_os = "linux")]
    // SAFETY: prctl(PR_GET_DUMPABLE) takes no pointers.
    unsafe {
        return Some(libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) != 0);
    }
    #[allow(unreachable_code)]
    None
}

/// `LIGUS_REQUIRE_SANDBOX=1`: refuse to run a host without an agent sandbox.
pub fn sandbox_required() -> bool {
    require_sandbox_value(hive_core::compat::env_opt("LIGUS_REQUIRE_SANDBOX").as_deref())
}

/// Fail closed: any value but empty or an explicit off (`0`, `false`, `no`,
/// `off`, any case) requires the sandbox, so `TRUE` or a typo never quietly
/// turns the guard off.
fn require_sandbox_value(v: Option<&str>) -> bool {
    let Some(v) = v.map(|v| v.trim().to_ascii_lowercase()).filter(|v| !v.is_empty()) else {
        return false;
    };
    !matches!(v.as_str(), "0" | "false" | "no" | "off")
}

/// With [`sandbox_required`], exit with [`EXIT_UNCONFINED`] when this host
/// has no agent sandbox.
pub fn require_sandbox_or_exit(what: &str) {
    if !sandbox_required() {
        return;
    }
    let s = hive_runtime::executor::sandbox_status();
    if s.kind == hive_runtime::executor::ExecutorKind::Unconfined {
        eprintln!("error: LIGUS_REQUIRE_SANDBOX=1 and this host has no agent sandbox, so {what} would refuse every agent turn:");
        eprintln!("  {}", s.summary);
        for n in &s.notes {
            eprintln!("  - {n}");
        }
        eprintln!("  `ligus doctor` explains; on Kubernetes see the supported-node matrix in the deploy docs");
        std::process::exit(EXIT_UNCONFINED);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(m: &HashMap<String, String>) -> impl Fn(&str) -> Option<String> + '_ {
        move |k| m.get(k).cloned()
    }

    #[test]
    fn file_secrets_load_and_are_denied_to_agents() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("key");
        std::fs::write(&key, "s3cret-key\n").unwrap();
        let tok = dir.path().join("tok");
        std::fs::write(&tok, "hrt1.a.b").unwrap();
        let env: HashMap<String, String> = [
            ("LIGUS_WORKSPACE_KEY_FILE".into(), key.display().to_string()),
            ("LIGUS_RELAY_ACCESS_TOKEN_FILE".into(), tok.display().to_string()),
        ]
        .into();
        let got = resolve_secret_files(lookup(&env));
        assert!(got.errors.is_empty(), "{:?}", got.errors);
        assert!(got.vars.contains(&("LIGUS_WORKSPACE_KEY".into(), "s3cret-key".into())));
        assert!(got.vars.contains(&("LIGUS_RELAY_ACCESS_TOKEN".into(), "hrt1.a.b".into())));
        assert!(got.deny.contains(&key) && got.deny.contains(&tok));
    }

    #[test]
    fn a_direct_variable_wins_and_missing_files_are_reported() {
        let env: HashMap<String, String> = [
            ("LIGUS_WORKSPACE_KEY".into(), "direct".into()),
            ("LIGUS_WORKSPACE_KEY_FILE".into(), "/nonexistent/key".into()),
            ("LIGUS_NTFY_TOKEN_FILE".into(), "/nonexistent/ntfy".into()),
        ]
        .into();
        let got = resolve_secret_files(lookup(&env));
        assert!(got.vars.is_empty());
        assert_eq!(got.errors.len(), 2, "{:?}", got.errors);
    }

    #[test]
    fn ws_secret_dir_maps_files_to_secret_refs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("anthropic"), "sk-ant\n").unwrap();
        std::fs::write(dir.path().join("bad name"), "x").unwrap();
        std::fs::create_dir(dir.path().join("..data")).unwrap();
        let env: HashMap<String, String> = [
            (WS_SECRET_DIR.into(), dir.path().display().to_string()),
        ]
        .into();
        let got = resolve_secret_files(lookup(&env));
        assert_eq!(got.vars, vec![("LIGUS_WS_SECRET_anthropic".to_string(), "sk-ant".to_string())]);
        assert_eq!(got.errors.len(), 1, "{:?}", got.errors);
        assert!(got.deny.contains(&dir.path().to_path_buf()));
    }

    #[test]
    fn the_secret_store_mount_is_denied_to_agents() {
        let dir = tempfile::tempdir().unwrap();
        let env: HashMap<String, String> = [(SECRETS_DIR.into(), dir.path().display().to_string())].into();
        let got = resolve_secret_files(lookup(&env));
        assert!(got.vars.is_empty());
        assert!(got.deny.contains(&dir.path().to_path_buf()));
    }

    #[test]
    fn a_kubernetes_secret_mount_denies_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("..data")).unwrap();
        let f = dir.path().join("key");
        std::fs::write(&f, "k").unwrap();
        assert!(deny_paths(&f).contains(&dir.path().to_path_buf()));
    }

    #[test]
    fn require_sandbox_fails_closed_on_any_truthy_spelling() {
        for v in ["1", "true", "TRUE", "True", "yes", "Yes", "on", "ON", "2", "enabled", " 1 "] {
            assert!(require_sandbox_value(Some(v)), "{v:?} must require the sandbox");
        }
        for v in ["", " ", "0", "false", "FALSE", "no", "off", "Off"] {
            assert!(!require_sandbox_value(Some(v)), "{v:?} must not require the sandbox");
        }
        assert!(!require_sandbox_value(None));
    }

    #[test]
    fn the_default_k8s_secret_mount_is_denied_when_the_backend_is_k8s() {
        let env: HashMap<String, String> = [("LIGUS_SECRET_BACKEND".into(), " K8s ".into())].into();
        let got = resolve_secret_files(lookup(&env));
        assert!(
            got.deny.contains(&PathBuf::from(hive_runtime::secret_store::K8S_SECRETS_DIR_DEFAULT)),
            "{:?}",
            got.deny
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn not_dumpable_after_hardening() {
        // Runs in its own process under nextest; under `cargo test` it only
        // flips this test binary's flag, which nothing else depends on.
        set_not_dumpable();
        assert_eq!(dumpable(), Some(false));
    }
}

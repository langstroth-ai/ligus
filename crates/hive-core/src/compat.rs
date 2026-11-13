//! Names after the product rename (Hive -> Ligus), and the one-time
//! migrations that carry existing data over.
//!
//! Ligus 2.0 reads only the new names: `LIGUS_*` environment variables,
//! `.ligus/` workspace state, `ligus.config.toml` and `~/.ligus`. Data written
//! under the old names is moved over once, the first time it is touched:
//!
//! * a workspace's `.hive/` dir is renamed to `.ligus/` (per-chat git
//!   worktrees inside it are re-registered with `git worktree repair`), see
//!   [`migrate_workspace_dir`];
//! * a `hive.config.toml` is renamed to `ligus.config.toml`, see
//!   [`find_config_file`];
//! * the CLI's `~/.hive` data dir is renamed to `~/.ligus`, see
//!   [`home_data_dir`];
//! * the desktop app copies its `com.hive.desktop` app-data dir (in the app
//!   crate, `data_migration.rs`).
//!
//! Environment variables are not migrated (they live in the user's shell,
//! unit files and CI). A `HIVE_X` that is set while `LIGUS_X` is not is
//! ignored, with a one-time warning naming the new spelling.
//!
//! Not renamed, ever: wire and protocol identifiers and crypto
//! domain-separation strings (pinned by tests in `crypto.rs`/`e2ee.rs`), the
//! `hive://` deep-link scheme on input, and git branch names such as
//! `hive/chat/<id>`.

use std::collections::HashSet;
use std::env::VarError;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Environment-variable prefix.
pub const ENV_PREFIX: &str = "LIGUS_";
/// The former prefix. Not read; only used to warn about leftovers.
pub const LEGACY_ENV_PREFIX: &str = "HIVE_";

/// The result of reading one variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The value of the `LIGUS_` (or unprefixed) name, if set.
    pub value: Option<OsString>,
    /// The name that was read.
    pub name: String,
    /// The former `HIVE_` name is set but `LIGUS_` is not: it was ignored.
    pub ignored_legacy: Option<String>,
}

/// `(name, former name)` for `name`, or `None` when `name` carries neither
/// product prefix. Callers may pass either spelling; only the `LIGUS_` one is
/// read.
pub fn env_names(name: &str) -> Option<(String, String)> {
    let suffix = name.strip_prefix(ENV_PREFIX).or_else(|| name.strip_prefix(LEGACY_ENV_PREFIX))?;
    Some((format!("{ENV_PREFIX}{suffix}"), format!("{LEGACY_ENV_PREFIX}{suffix}")))
}

/// Read `name` with an injected lookup (pure; used by tests and [`var_os`]).
pub fn resolve_with(name: &str, lookup: impl Fn(&str) -> Option<OsString>) -> Resolved {
    let Some((canonical, legacy)) = env_names(name) else {
        return Resolved { value: lookup(name), name: name.to_string(), ignored_legacy: None };
    };
    let value = lookup(&canonical);
    let unset = value.as_ref().is_none_or(|v| v.is_empty());
    let legacy_set = lookup(&legacy).is_some_and(|v| !v.is_empty());
    Resolved { value, name: canonical, ignored_legacy: (unset && legacy_set).then_some(legacy) }
}

static WARNED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// True the first time `key` is seen (process-wide).
fn first_time(key: &str) -> bool {
    let mut guard = WARNED.lock().unwrap_or_else(|e| e.into_inner());
    guard.get_or_insert_with(HashSet::new).insert(key.to_string())
}

/// Print `line` on stderr once per process.
fn note_once(line: &str) {
    if first_time(line) {
        eprintln!("{line}");
    }
}

/// The warning to print for `r`, if any (pure; tested).
pub fn warning_for(r: &Resolved) -> Option<String> {
    let legacy = r.ignored_legacy.as_deref()?;
    Some(format!("warning: {legacy} is set but no longer read; rename it to {}", r.name))
}

/// Like `std::env::var_os` for `LIGUS_X` (either spelling may be passed).
pub fn var_os(name: &str) -> Option<OsString> {
    let r = resolve_with(name, |n| std::env::var_os(n));
    if let Some(w) = warning_for(&r) {
        note_once(&w);
    }
    r.value
}

/// Like `std::env::var` for `LIGUS_X` (either spelling may be passed).
pub fn var(name: &str) -> Result<String, VarError> {
    match var_os(name) {
        Some(v) => v.into_string().map_err(VarError::NotUnicode),
        None => Err(VarError::NotPresent),
    }
}

/// The variable's value when set and not blank.
pub fn env_opt(name: &str) -> Option<String> {
    var(name).ok().filter(|s| !s.trim().is_empty())
}

/// Whether the variable is set (any value).
pub fn is_set(name: &str) -> bool {
    var_os(name).is_some()
}

// ---------------------------------------------------------------------------
// Desktop bundle identifiers
// ---------------------------------------------------------------------------

/// The desktop app's bundle identifier. It names the OS app-data dir
/// (`~/Library/Application Support/<id>`, `%APPDATA%\<id>`,
/// `~/.local/share/<id>`).
pub const DESKTOP_APP_ID: &str = "ai.ligus.desktop";
/// The former identifier, whose app-data dir the desktop copies on first
/// launch.
pub const LEGACY_DESKTOP_APP_ID: &str = "com.hive.desktop";

// ---------------------------------------------------------------------------
// Deep links
// ---------------------------------------------------------------------------

/// URL schemes accepted for phone deep links (`<scheme>://pair?…`,
/// `<scheme>://chat/<id>`). `hive` stays accepted for good: printed pairing
/// QR codes and older phone builds use it.
pub const DEEP_LINK_SCHEMES: [&str; 2] = ["ligus", "hive"];
/// The scheme links are emitted with. Stays `hive` until every paired phone
/// runs a build that registers `ligus://`.
pub const DEEP_LINK_SCHEME: &str = "hive";

/// A deep link `<DEEP_LINK_SCHEME>://<rest>`.
pub fn deep_link(rest: &str) -> String {
    format!("{DEEP_LINK_SCHEME}://{rest}")
}

/// The part after `ligus://` or `hive://` (scheme matched case-insensitively),
/// or `None` for any other link.
pub fn strip_deep_link_scheme(link: &str) -> Option<&str> {
    let (scheme, rest) = link.split_once("://")?;
    DEEP_LINK_SCHEMES.iter().any(|s| scheme.eq_ignore_ascii_case(s)).then_some(rest)
}

// ---------------------------------------------------------------------------
// Workspace state dir: `.ligus/` (migrated from `.hive/`)
// ---------------------------------------------------------------------------

/// Per-workspace state directory.
pub const WORKSPACE_DIR: &str = ".ligus";
/// The former name, migrated to [`WORKSPACE_DIR`] on first use.
pub const LEGACY_WORKSPACE_DIR: &str = ".hive";

/// What [`migrate_workspace_dir`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceMigration {
    /// Entries moved from `.hive/` into `.ligus/` (top-level names).
    pub moved: Vec<String>,
    /// Linked git worktrees found in the moved state and re-registered.
    pub worktrees: Vec<PathBuf>,
    /// Worktrees `git worktree repair` could not re-register. When non-empty,
    /// `.hive` is left as a symlink to `.ligus` so git's recorded paths still
    /// resolve.
    pub unrepaired: Vec<PathBuf>,
    /// Entries left in `.hive/` because `.ligus/` already had the same name.
    pub kept_in_legacy: Vec<String>,
}

/// Move `<root>/.hive/` to `<root>/.ligus/`, once. Returns `Ok(None)` when
/// there is nothing to do.
///
/// * No `.ligus/` yet: `.hive/` is renamed (atomic, same filesystem).
/// * Both exist: each entry of `.hive/` that `.ligus/` lacks is moved
///   (`worktrees/` is merged per worktree); clashing entries stay in `.hive/`.
///   An emptied `.hive/` is removed.
///
/// Per-chat git worktrees (`worktrees/<id>` holding a `.git` file) are then
/// re-registered with `git worktree repair` run in `root`, and verified. If
/// any can't be repaired, `.hive` becomes a symlink to `.ligus` so the paths
/// git recorded keep resolving (Unix; on Windows a warning is printed).
/// A `.hive` that is already a symlink is left alone, and so is `$HOME/.hive`
/// (that is the CLI's former data dir, see [`home_data_dir`], not workspace
/// state).
pub fn migrate_workspace_dir(root: &Path) -> io::Result<Option<WorkspaceMigration>> {
    if is_home_dir(root) {
        return Ok(None);
    }
    let old = root.join(LEGACY_WORKSPACE_DIR);
    let new = root.join(WORKSPACE_DIR);
    let old_meta = match std::fs::symlink_metadata(&old) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !old_meta.is_dir() {
        return Ok(None);
    }
    let mut m = WorkspaceMigration::default();
    if !new.exists() {
        std::fs::rename(&old, &new)?;
        for e in std::fs::read_dir(&new)? {
            m.moved.push(e?.file_name().to_string_lossy().into_owned());
        }
        m.moved.sort();
        m.worktrees = linked_worktrees(&new.join("worktrees"));
    } else {
        std::fs::create_dir_all(&new)?;
        for e in std::fs::read_dir(&old)? {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            let target = new.join(&name);
            if name == "worktrees" && e.file_type()?.is_dir() {
                std::fs::create_dir_all(&target)?;
                let mut left = false;
                for w in std::fs::read_dir(e.path())? {
                    let w = w?;
                    let to = target.join(w.file_name());
                    if to.exists() {
                        left = true;
                        m.kept_in_legacy.push(format!("worktrees/{}", w.file_name().to_string_lossy()));
                    } else {
                        std::fs::rename(w.path(), &to)?;
                        if to.join(".git").is_file() {
                            m.worktrees.push(to);
                        }
                    }
                }
                if !left {
                    let _ = std::fs::remove_dir(e.path());
                }
                m.moved.push(name);
            } else if target.exists() {
                m.kept_in_legacy.push(name);
            } else {
                std::fs::rename(e.path(), &target)?;
                m.moved.push(name);
            }
        }
        m.moved.sort();
        let _ = std::fs::remove_dir(&old); // only succeeds when emptied
    }
    if !m.worktrees.is_empty() {
        repair_worktrees(root, &m.worktrees);
        m.unrepaired = m.worktrees.iter().filter(|w| !worktree_link_ok(w)).cloned().collect();
        if !m.unrepaired.is_empty() && !old.exists() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(WORKSPACE_DIR, &old)?;
            #[cfg(not(unix))]
            eprintln!(
                "warning: {} worktree(s) under {} could not be re-registered; run `git worktree repair` there",
                m.unrepaired.len(),
                new.display()
            );
        }
    }
    Ok(Some(m))
}

/// Whether `dir` is the user's home directory.
fn is_home_dir(dir: &Path) -> bool {
    let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from) else { return false };
    dir == home || matches!((dir.canonicalize(), home.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

/// Linked worktrees directly under `dir` (each a dir holding a `.git` file).
fn linked_worktrees(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.join(".git").is_file()).collect();
    out.sort();
    out
}

/// `git worktree repair <paths…>` in `root` (best effort; verified after).
fn repair_worktrees(root: &Path, worktrees: &[PathBuf]) {
    let _ = crate::process_util::command("git").arg("-C").arg(root).args(["worktree", "repair"]).args(worktrees).output();
}

/// Whether git's two-way link for the linked worktree at `wt` is intact: its
/// `.git` file names an admin dir whose `gitdir` points back at `wt/.git`.
pub fn worktree_link_ok(wt: &Path) -> bool {
    let Ok(dotgit) = std::fs::read_to_string(wt.join(".git")) else { return false };
    let Some(admin) = dotgit.trim().strip_prefix("gitdir:").map(|s| PathBuf::from(s.trim())) else { return false };
    let admin = if admin.is_absolute() { admin } else { wt.join(admin) };
    let Ok(back) = std::fs::read_to_string(admin.join("gitdir")) else { return false };
    let back = PathBuf::from(back.trim());
    let back = if back.is_absolute() { back } else { admin.join(back) };
    match (back.canonicalize(), wt.join(".git").canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

static MIGRATED_ROOTS: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

/// Run [`migrate_workspace_dir`] for `root` once per process, printing a
/// one-line note when something moved or a warning when it failed.
pub fn ensure_workspace_migrated(root: &Path) {
    let mut guard = MIGRATED_ROOTS.lock().unwrap_or_else(|e| e.into_inner());
    if !guard.get_or_insert_with(HashSet::new).insert(root.to_path_buf()) {
        return;
    }
    match migrate_workspace_dir(root) {
        Ok(Some(m)) => {
            let mut line = format!("note: moved {} to {}", root.join(LEGACY_WORKSPACE_DIR).display(), root.join(WORKSPACE_DIR).display());
            if !m.worktrees.is_empty() {
                line.push_str(&format!(" ({} git worktree(s) re-registered", m.worktrees.len() - m.unrepaired.len()));
                if !m.unrepaired.is_empty() {
                    line.push_str(&format!(", {} kept reachable through a .hive symlink", m.unrepaired.len()));
                }
                line.push(')');
            }
            if !m.kept_in_legacy.is_empty() {
                line.push_str(&format!("; left in .hive because .ligus already has them: {}", m.kept_in_legacy.join(", ")));
            }
            eprintln!("{line}");
        }
        Ok(None) => {}
        Err(e) => eprintln!("warning: could not move {} to {}: {e}", root.join(LEGACY_WORKSPACE_DIR).display(), WORKSPACE_DIR),
    }
}

/// The path of workspace state `rel` (e.g. `mcp-servers.json` or
/// `worktrees/chat-<id>`) under `<root>/.ligus/`, after migrating a legacy
/// `.hive/` once.
pub fn workspace_state_path(root: &Path, rel: impl AsRef<Path>) -> PathBuf {
    ensure_workspace_migrated(root);
    root.join(WORKSPACE_DIR).join(rel)
}

/// Whether `dir` holds a workspace state directory. A legacy `.hive/` counts:
/// it marks a workspace that is migrated when first opened.
pub fn has_workspace_dir(dir: &Path) -> bool {
    dir.join(WORKSPACE_DIR).exists() || dir.join(LEGACY_WORKSPACE_DIR).is_dir()
}

// ---------------------------------------------------------------------------
// Config file: `ligus.config.toml` (migrated from `hive.config.toml`)
// ---------------------------------------------------------------------------

/// Workspace / data-dir config file name.
pub const CONFIG_FILE: &str = "ligus.config.toml";
/// The former name, renamed to [`CONFIG_FILE`] on first read.
pub const LEGACY_CONFIG_FILE: &str = "hive.config.toml";

/// Rename `<dir>/hive.config.toml` to `ligus.config.toml` when only the old
/// one exists. When both exist the old one is left alone and ignored, with a
/// one-time warning.
pub fn migrate_config_file(dir: &Path) {
    let old = dir.join(LEGACY_CONFIG_FILE);
    if !old.is_file() {
        return;
    }
    let new = dir.join(CONFIG_FILE);
    if new.exists() {
        note_once(&format!("warning: {} is ignored; {} is used", old.display(), new.display()));
        return;
    }
    match std::fs::rename(&old, &new) {
        Ok(()) => eprintln!("note: renamed {} to {}", old.display(), new.display()),
        Err(e) => note_once(&format!("warning: could not rename {} to {CONFIG_FILE}: {e}", old.display())),
    }
}

/// The config file to read in `dir`, if it exists (after [`migrate_config_file`]).
pub fn find_config_file(dir: &Path) -> Option<PathBuf> {
    migrate_config_file(dir);
    Some(dir.join(CONFIG_FILE)).filter(|p| p.is_file())
}

/// The config file to write in `dir`.
pub fn config_file_for_write(dir: &Path) -> PathBuf {
    migrate_config_file(dir);
    dir.join(CONFIG_FILE)
}

/// Candidate config files in read order across `dirs` (e.g. workspace root,
/// then data dir), each migrated first.
pub fn config_candidates(dirs: &[&Path]) -> Vec<PathBuf> {
    dirs.iter()
        .map(|d| {
            migrate_config_file(d);
            d.join(CONFIG_FILE)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// CLI data dir: `~/.ligus` (migrated from `~/.hive`)
// ---------------------------------------------------------------------------

/// The CLI data directory under `$HOME`.
pub const HOME_DATA_DIR: &str = ".ligus";
/// The former name, renamed to [`HOME_DATA_DIR`] on first use.
pub const LEGACY_HOME_DATA_DIR: &str = ".hive";

/// Files that mark a dir as holding CLI data (a database or an identity).
const CLI_DATA_MARKERS: [&str; 2] = ["hive.db", "identity.json"];

/// The default CLI data dir under `home` when no data-dir env var is set:
/// `~/.ligus`. If `~/.ligus` doesn't exist and `~/.hive` holds CLI data
/// (`hive.db` or `identity.json`), `~/.hive` is renamed to `~/.ligus` first
/// (same filesystem, atomic) and a note is printed; a `~/.hive` holding
/// anything else is left alone. A failed rename is an error rather than a
/// silent fresh start, which would mint a new device identity.
pub fn home_data_dir(home: &Path) -> io::Result<PathBuf> {
    let new = home.join(HOME_DATA_DIR);
    let old = home.join(LEGACY_HOME_DATA_DIR);
    let old_has_data = std::fs::symlink_metadata(&old).is_ok_and(|m| m.is_dir()) && CLI_DATA_MARKERS.iter().any(|m| old.join(m).exists());
    if !new.exists() && old_has_data {
        std::fs::rename(&old, &new).map_err(|e| {
            io::Error::new(e.kind(), format!("could not move {} to {}: {e} (move it by hand or set LIGUS_DATA_DIR)", old.display(), new.display()))
        })?;
        eprintln!("note: moved {} to {}", old.display(), new.display());
    }
    Ok(new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let m: HashMap<String, OsString> = pairs.iter().map(|(k, v)| (k.to_string(), OsString::from(v))).collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn names_accept_either_spelling() {
        let want = Some(("LIGUS_DATA_DIR".to_string(), "HIVE_DATA_DIR".to_string()));
        assert_eq!(env_names("HIVE_DATA_DIR"), want);
        assert_eq!(env_names("LIGUS_DATA_DIR"), want);
        assert_eq!(env_names("HOME"), None);
    }

    #[test]
    fn only_ligus_names_are_read() {
        let r = resolve_with("LIGUS_X", env(&[("LIGUS_X", "new"), ("HIVE_X", "old")]));
        assert_eq!(r.value.as_deref().and_then(|v| v.to_str()), Some("new"));
        assert_eq!(warning_for(&r), None, "both set: LIGUS wins quietly");

        let r = resolve_with("LIGUS_X", env(&[("HIVE_X", "old")]));
        assert_eq!(r.value, None, "HIVE_X is not a fallback");
        let w = warning_for(&r).unwrap();
        assert!(w.contains("HIVE_X is set but no longer read; rename it to LIGUS_X"), "{w}");

        // Passing the old spelling still reads the new name.
        let r = resolve_with("HIVE_X", env(&[("LIGUS_X", "new")]));
        assert_eq!(r.value.as_deref().and_then(|v| v.to_str()), Some("new"));

        let r = resolve_with("LIGUS_X", env(&[("LIGUS_X", ""), ("HIVE_X", "old")]));
        assert_eq!(r.value.as_deref().and_then(|v| v.to_str()), Some(""));
        assert!(warning_for(&r).is_some(), "blank LIGUS_X with a HIVE_X set is worth a warning");

        assert_eq!(resolve_with("LIGUS_X", env(&[])), Resolved { value: None, name: "LIGUS_X".into(), ignored_legacy: None });
    }

    #[test]
    fn unprefixed_names_read_as_is() {
        let r = resolve_with("HOME", env(&[("HOME", "/h")]));
        assert_eq!(r.value.as_deref().and_then(|v| v.to_str()), Some("/h"));
        assert_eq!(warning_for(&r), None);
    }

    #[test]
    fn warnings_print_once_per_line() {
        assert!(first_time("compat-test-unique-line"));
        assert!(!first_time("compat-test-unique-line"));
    }

    #[test]
    fn process_env_round_trip() {
        // Unique names so parallel tests can't collide.
        std::env::set_var("HIVE_COMPAT_RT_A", "old");
        assert_eq!(var("LIGUS_COMPAT_RT_A"), Err(VarError::NotPresent));
        std::env::set_var("LIGUS_COMPAT_RT_A", "new");
        assert_eq!(var("LIGUS_COMPAT_RT_A").as_deref(), Ok("new"));
        assert_eq!(var("HIVE_COMPAT_RT_A").as_deref(), Ok("new"));
        std::env::remove_var("LIGUS_COMPAT_RT_A");
        std::env::remove_var("HIVE_COMPAT_RT_A");
        assert_eq!(env_opt("LIGUS_COMPAT_RT_A"), None);
        assert!(!is_set("LIGUS_COMPAT_RT_A"));
    }

    /// A fresh, unique temp dir (hive-core has no tempfile dev-dependency).
    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!("compat-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A repo with one commit and a linked worktree at `.hive/worktrees/chat-a`
    /// on branch `hive/chat/a`, the layout Hive 1.x created.
    fn legacy_repo(tag: &str) -> Tmp {
        let tmp = Tmp::new(tag);
        let root = tmp.path();
        git(root, &["init", "-q", "-b", "main"]);
        git(root, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "init"]);
        std::fs::create_dir_all(root.join(".hive/worktrees")).unwrap();
        git(root, &["worktree", "add", "-q", "-b", "hive/chat/a", ".hive/worktrees/chat-a"]);
        std::fs::write(root.join(".hive/mcp-servers.json"), "[]").unwrap();
        std::fs::create_dir_all(root.join(".hive/stopped-turns")).unwrap();
        std::fs::write(root.join(".hive/stopped-turns/t.patch"), "x").unwrap();
        tmp
    }

    #[test]
    fn workspace_dir_is_renamed_and_worktrees_repaired() {
        let tmp = legacy_repo("wsmig");
        let root = tmp.path();
        assert!(has_workspace_dir(root), "a legacy .hive marks a workspace");
        let m = migrate_workspace_dir(root).unwrap().unwrap();
        assert_eq!(m.moved, ["mcp-servers.json", "stopped-turns", "worktrees"]);
        assert_eq!(m.worktrees, [root.join(".ligus/worktrees/chat-a")]);
        assert!(m.unrepaired.is_empty(), "{m:?}");
        assert!(!root.join(".hive").exists(), "no leftover .hive");
        assert!(root.join(".ligus/stopped-turns/t.patch").is_file());
        let wt = root.join(".ligus/worktrees/chat-a");
        assert!(worktree_link_ok(&wt));
        // git agrees: the worktree is listed at its new path and works.
        let list = git(root, &["worktree", "list", "--porcelain"]);
        let canon = wt.canonicalize().unwrap();
        assert!(list.lines().any(|l| l.strip_prefix("worktree ").is_some_and(|p| Path::new(p).canonicalize().ok() == Some(canon.clone()))), "{list}");
        assert!(!list.contains("prunable"), "{list}");
        assert_eq!(git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(), "hive/chat/a");
        // Idempotent.
        assert_eq!(migrate_workspace_dir(root).unwrap(), None);
        assert_eq!(workspace_state_path(root, "mcp-servers.json"), root.join(".ligus/mcp-servers.json"));
    }

    #[test]
    fn workspace_dirs_merge_when_both_exist() {
        let tmp = legacy_repo("wsmerge");
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".ligus/worktrees/chat-b")).unwrap();
        std::fs::write(root.join(".ligus/mcp-servers.json"), "[1]").unwrap();
        let m = migrate_workspace_dir(root).unwrap().unwrap();
        assert_eq!(m.kept_in_legacy, ["mcp-servers.json"]);
        assert_eq!(std::fs::read_to_string(root.join(".ligus/mcp-servers.json")).unwrap(), "[1]", "the new file wins");
        assert!(root.join(".hive/mcp-servers.json").is_file(), "the clashing old file is kept, not deleted");
        assert!(root.join(".ligus/worktrees/chat-a/.git").is_file());
        assert!(root.join(".ligus/worktrees/chat-b").is_dir());
        assert!(root.join(".ligus/stopped-turns/t.patch").is_file());
        assert!(m.unrepaired.is_empty(), "{m:?}");
        assert!(worktree_link_ok(&root.join(".ligus/worktrees/chat-a")));
    }

    #[test]
    fn home_dir_hive_is_not_workspace_state() {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
        // Never touches the real dir: it returns before looking at it.
        assert_eq!(migrate_workspace_dir(&home).unwrap(), None);
    }

    #[test]
    fn workspace_dir_without_legacy_is_untouched() {
        let tmp = Tmp::new("wsnone");
        assert_eq!(migrate_workspace_dir(tmp.path()).unwrap(), None);
        assert!(!has_workspace_dir(tmp.path()));
        assert_eq!(workspace_state_path(tmp.path(), "x.json"), tmp.path().join(".ligus/x.json"));
    }

    #[cfg(unix)]
    #[test]
    fn unrepairable_worktree_keeps_a_hive_symlink() {
        let tmp = Tmp::new("wsbroken");
        let root = tmp.path();
        // A worktree whose admin dir doesn't exist: repair can't fix it.
        std::fs::create_dir_all(root.join(".hive/worktrees/chat-z")).unwrap();
        std::fs::write(root.join(".hive/worktrees/chat-z/.git"), "gitdir: /nonexistent/admin\n").unwrap();
        let m = migrate_workspace_dir(root).unwrap().unwrap();
        assert_eq!(m.unrepaired, [root.join(".ligus/worktrees/chat-z")]);
        let link = std::fs::read_link(root.join(".hive")).unwrap();
        assert_eq!(link, Path::new(".ligus"));
        // A symlinked .hive is not migrated again.
        assert_eq!(migrate_workspace_dir(root).unwrap(), None);
    }

    #[test]
    fn config_file_is_renamed_once() {
        let tmp = Tmp::new("cfg");
        let dir = tmp.path();
        assert_eq!(find_config_file(dir), None);
        assert_eq!(config_file_for_write(dir), dir.join(CONFIG_FILE));
        std::fs::write(dir.join("hive.config.toml"), "a = 1").unwrap();
        assert_eq!(find_config_file(dir), Some(dir.join("ligus.config.toml")));
        assert!(!dir.join("hive.config.toml").exists());
        assert_eq!(std::fs::read_to_string(dir.join("ligus.config.toml")).unwrap(), "a = 1");
        // Both present: the old one is ignored and left alone.
        std::fs::write(dir.join("hive.config.toml"), "b = 2").unwrap();
        assert_eq!(find_config_file(dir), Some(dir.join("ligus.config.toml")));
        assert_eq!(std::fs::read_to_string(dir.join("hive.config.toml")).unwrap(), "b = 2");
        let c = config_candidates(&[Path::new("/nonexistent-w"), Path::new("/nonexistent-d")]);
        let names: Vec<_> = c.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(names, ["/nonexistent-w/ligus.config.toml", "/nonexistent-d/ligus.config.toml"]);
    }

    #[test]
    fn home_data_dir_moves_legacy_once() {
        let tmp = Tmp::new("home");
        let home = tmp.path();
        assert_eq!(home_data_dir(home).unwrap(), home.join(".ligus"));
        assert!(!home.join(".ligus").exists(), "nothing created eagerly");
        // A ~/.hive without CLI data (some other tool's file) is left alone.
        std::fs::create_dir(home.join(".hive")).unwrap();
        std::fs::write(home.join(".hive/issuer_ed25519.key"), "k").unwrap();
        assert_eq!(home_data_dir(home).unwrap(), home.join(".ligus"));
        assert!(home.join(".hive/issuer_ed25519.key").is_file());
        assert!(!home.join(".ligus").exists());
        // With CLI data it moves.
        std::fs::write(home.join(".hive/hive.db"), "db").unwrap();
        assert_eq!(home_data_dir(home).unwrap(), home.join(".ligus"));
        assert!(!home.join(".hive").exists());
        assert!(home.join(".ligus/issuer_ed25519.key").is_file(), "the whole dir moves");
        assert_eq!(std::fs::read_to_string(home.join(".ligus/hive.db")).unwrap(), "db");
        // Both present: the new one is used, the old one untouched.
        std::fs::create_dir(home.join(".hive")).unwrap();
        assert_eq!(home_data_dir(home).unwrap(), home.join(".ligus"));
        assert!(home.join(".hive").is_dir());
    }

    #[test]
    fn deep_links_parse_both_schemes_and_emit_hive() {
        assert_eq!(deep_link("pair?x=1"), "hive://pair?x=1");
        assert_eq!(strip_deep_link_scheme("hive://pair?x=1"), Some("pair?x=1"));
        assert_eq!(strip_deep_link_scheme("ligus://chat/abc"), Some("chat/abc"));
        assert_eq!(strip_deep_link_scheme("LIGUS://chat/abc"), Some("chat/abc"));
        assert_eq!(strip_deep_link_scheme("https://pair?x=1"), None);
        assert_eq!(strip_deep_link_scheme("hivex://pair"), None);
        assert_eq!(strip_deep_link_scheme("pair?x=1"), None);
    }
}

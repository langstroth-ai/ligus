//! Name-compatibility shims for the product rename (Hive -> Ligus).
//!
//! Environment variables have a canonical `LIGUS_` name and a legacy `HIVE_`
//! name. Every runtime read goes through [`var`] / [`env_opt`]: the `LIGUS_`
//! name wins, the `HIVE_` name is the fallback. Callers may pass either
//! spelling (`"HIVE_DATA_DIR"` or `"LIGUS_DATA_DIR"`); names with neither
//! prefix (`HOME`, `PATH`) are read as-is, so the helpers are drop-in
//! replacements for `std::env::var`.
//!
//! When both names are set to different values a one-time warning goes to
//! stderr (the `LIGUS_` value is used). Falling back to a `HIVE_` name warns
//! once per variable when [`WARN_ON_LEGACY_ENV`] is on.

use std::collections::HashSet;
use std::env::VarError;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Canonical environment-variable prefix.
pub const ENV_PREFIX: &str = "LIGUS_";
/// Legacy environment-variable prefix, still honoured as a fallback.
pub const LEGACY_ENV_PREFIX: &str = "HIVE_";

/// Whether reading a value from a legacy `HIVE_` name prints a deprecation
/// note (once per variable, on stderr).
pub const WARN_ON_LEGACY_ENV: bool = true;

/// Where a resolved value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvSource {
    /// The canonical `LIGUS_` name.
    Canonical,
    /// The legacy `HIVE_` name.
    Legacy,
    /// The name has no product prefix and was read as-is.
    Plain,
}

/// The result of resolving one variable against both spellings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub value: OsString,
    pub source: EnvSource,
    /// The canonical name (or the plain name when unprefixed).
    pub canonical: String,
    /// The legacy name, when the variable has one.
    pub legacy: Option<String>,
    /// Both spellings were set, non-empty and different (the canonical won).
    pub conflict: bool,
}

/// `(canonical, legacy)` names for `name`, or `None` when `name` carries
/// neither product prefix.
pub fn env_names(name: &str) -> Option<(String, String)> {
    let suffix = name.strip_prefix(ENV_PREFIX).or_else(|| name.strip_prefix(LEGACY_ENV_PREFIX))?;
    Some((format!("{ENV_PREFIX}{suffix}"), format!("{LEGACY_ENV_PREFIX}{suffix}")))
}

/// Resolve `name` with an injected lookup (pure; used by tests and by
/// [`var_os`]). A canonical value that is set but empty yields to a non-empty
/// legacy value, so an exported-but-blank `LIGUS_X=` doesn't mask `HIVE_X`.
pub fn resolve_with(name: &str, lookup: impl Fn(&str) -> Option<OsString>) -> Option<Resolved> {
    let Some((canonical, legacy)) = env_names(name) else {
        return lookup(name).map(|value| Resolved {
            value,
            source: EnvSource::Plain,
            canonical: name.to_string(),
            legacy: None,
            conflict: false,
        });
    };
    let new = lookup(&canonical);
    let old = lookup(&legacy);
    let filled = |v: &Option<OsString>| v.as_ref().is_some_and(|s| !s.is_empty());
    let (value, source, conflict) = if filled(&new) {
        let conflict = filled(&old) && old != new;
        (new?, EnvSource::Canonical, conflict)
    } else if filled(&old) {
        (old?, EnvSource::Legacy, false)
    } else if let Some(n) = new {
        (n, EnvSource::Canonical, false)
    } else {
        (old?, EnvSource::Legacy, false)
    };
    Some(Resolved { value, source, canonical, legacy: Some(legacy), conflict })
}

static WARNED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// True the first time `key` is seen (process-wide).
fn first_time(key: &str) -> bool {
    let mut guard = WARNED.lock().unwrap_or_else(|e| e.into_inner());
    guard.get_or_insert_with(HashSet::new).insert(key.to_string())
}

/// The one-line notes to print for `r`, if any (pure; tested).
pub fn warnings_for(r: &Resolved, warn_legacy: bool) -> Vec<String> {
    let mut out = Vec::new();
    let Some(legacy) = r.legacy.as_deref() else { return out };
    if r.conflict {
        out.push(format!("warning: {} and {legacy} are both set and differ; using {}", r.canonical, r.canonical));
    }
    if warn_legacy && r.source == EnvSource::Legacy {
        out.push(format!(
            "note: {legacy} is a legacy name; rename it to {} ({LEGACY_ENV_PREFIX}* names stop working in a future release)",
            r.canonical
        ));
    }
    out
}

fn report(r: &Resolved) {
    for line in warnings_for(r, WARN_ON_LEGACY_ENV) {
        if first_time(&line) {
            eprintln!("{line}");
        }
    }
}

/// Like `std::env::var_os`, resolving `LIGUS_X` then `HIVE_X`.
pub fn var_os(name: &str) -> Option<OsString> {
    let r = resolve_with(name, |n| std::env::var_os(n))?;
    report(&r);
    Some(r.value)
}

/// Like `std::env::var`, resolving `LIGUS_X` then `HIVE_X`.
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

/// Whether either spelling of the variable is set (any value).
pub fn is_set(name: &str) -> bool {
    var_os(name).is_some()
}

// ---------------------------------------------------------------------------
// Desktop bundle identifiers
// ---------------------------------------------------------------------------

/// The desktop app's bundle identifier after the rename. It names the OS
/// app-data dir (`~/Library/Application Support/<id>`, `%APPDATA%\<id>`,
/// `~/.local/share/<id>`).
pub const DESKTOP_APP_ID: &str = "ai.ligus.desktop";
/// The desktop app's original bundle identifier.
pub const LEGACY_DESKTOP_APP_ID: &str = "com.hive.desktop";
/// Desktop app-data dir names to probe, newest first.
pub const DESKTOP_APP_IDS: [&str; 2] = [DESKTOP_APP_ID, LEGACY_DESKTOP_APP_ID];

// ---------------------------------------------------------------------------
// Deep links
// ---------------------------------------------------------------------------

/// URL schemes accepted for phone deep links (`<scheme>://pair?…`,
/// `<scheme>://chat/<id>`), canonical first. Both are accepted forever.
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
// File and directory names
// ---------------------------------------------------------------------------

/// Per-workspace state directory names, in read order (canonical first).
/// Both are write-protected from agents and both mark a workspace root.
pub const WORKSPACE_DIRS: [&str; 2] = [".ligus", ".hive"];
/// Where new workspace state is created when no existing file is found.
pub const WORKSPACE_DIR_FOR_WRITES: &str = ".ligus";
/// Workspace / data-dir config file names, in read order (canonical first).
pub const CONFIG_FILES: [&str; 2] = ["ligus.config.toml", "hive.config.toml"];
/// Config file name used when neither exists yet.
pub const CONFIG_FILE_FOR_WRITES: &str = "ligus.config.toml";
/// CLI data directory names under `$HOME`, in read order.
pub const HOME_DATA_DIRS: [&str; 2] = [".ligus", ".hive"];
/// CLI data directory created under `$HOME` when neither exists yet.
pub const HOME_DATA_DIR_FOR_WRITES: &str = ".ligus";

/// The path of workspace state `rel` (e.g. `mcp-servers.json` or
/// `worktrees/chat-<id>`) under `root`: the first that already exists among
/// [`WORKSPACE_DIRS`], else the path under [`WORKSPACE_DIR_FOR_WRITES`]. An
/// existing file or git worktree keeps its location (git records worktree
/// paths, so they must never move).
pub fn workspace_state_path(root: &Path, rel: impl AsRef<Path>) -> PathBuf {
    let rel = rel.as_ref();
    WORKSPACE_DIRS
        .iter()
        .map(|d| root.join(d).join(rel))
        .find(|p| p.exists())
        .unwrap_or_else(|| root.join(WORKSPACE_DIR_FOR_WRITES).join(rel))
}

/// Whether `dir` holds a workspace state directory (either name).
pub fn has_workspace_dir(dir: &Path) -> bool {
    WORKSPACE_DIRS.iter().any(|d| dir.join(d).exists())
}

/// The config file to read in `dir`: the first that exists among
/// [`CONFIG_FILES`]. When both exist the canonical one wins and a one-time
/// note says the other is ignored.
pub fn find_config_file(dir: &Path) -> Option<PathBuf> {
    let found: Vec<PathBuf> = CONFIG_FILES.iter().map(|n| dir.join(n)).filter(|p| p.is_file()).collect();
    if found.len() > 1 {
        let line = format!("warning: both {} and {} exist; using {}", found[0].display(), found[1].display(), found[0].display());
        if first_time(&line) {
            eprintln!("{line}");
        }
    }
    found.into_iter().next()
}

/// The config file to write in `dir`: the one being read, else a new
/// [`CONFIG_FILE_FOR_WRITES`]. Writing back to the file that was read keeps a
/// single source of truth instead of leaving a stale twin behind.
pub fn config_file_for_write(dir: &Path) -> PathBuf {
    find_config_file(dir).unwrap_or_else(|| dir.join(CONFIG_FILE_FOR_WRITES))
}

/// Candidate config files in read order across `dirs` (e.g. workspace root,
/// then data dir), canonical name first within each directory.
pub fn config_candidates(dirs: &[&Path]) -> Vec<PathBuf> {
    dirs.iter().flat_map(|d| CONFIG_FILES.iter().map(move |n| d.join(n))).collect()
}

/// The default CLI data dir under `home` when no data-dir env var is set: the
/// first existing of [`HOME_DATA_DIRS`], else [`HOME_DATA_DIR_FOR_WRITES`].
/// Returns whether the legacy directory was chosen. Never moves anything (a
/// running `serve` or worker may hold the database open).
pub fn home_data_dir(home: &Path) -> (PathBuf, bool) {
    let legacy = HOME_DATA_DIRS[HOME_DATA_DIRS.len() - 1];
    match HOME_DATA_DIRS.iter().find(|d| home.join(d).is_dir()) {
        Some(d) => (home.join(d), *d == legacy && legacy != HOME_DATA_DIR_FOR_WRITES),
        None => (home.join(HOME_DATA_DIR_FOR_WRITES), false),
    }
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
    fn names_accept_either_prefix() {
        let want = Some(("LIGUS_DATA_DIR".to_string(), "HIVE_DATA_DIR".to_string()));
        assert_eq!(env_names("HIVE_DATA_DIR"), want);
        assert_eq!(env_names("LIGUS_DATA_DIR"), want);
        assert_eq!(env_names("HOME"), None);
    }

    #[test]
    fn canonical_wins_then_legacy_falls_back() {
        let r = resolve_with("HIVE_X", env(&[("LIGUS_X", "new"), ("HIVE_X", "new")])).unwrap();
        assert_eq!((r.value.to_str().unwrap(), r.source, r.conflict), ("new", EnvSource::Canonical, false));

        let r = resolve_with("LIGUS_X", env(&[("HIVE_X", "old")])).unwrap();
        assert_eq!((r.value.to_str().unwrap(), r.source), ("old", EnvSource::Legacy));

        let r = resolve_with("HIVE_X", env(&[("LIGUS_X", "new")])).unwrap();
        assert_eq!((r.value.to_str().unwrap(), r.source), ("new", EnvSource::Canonical));

        assert!(resolve_with("HIVE_X", env(&[])).is_none());
    }

    #[test]
    fn conflicting_values_use_canonical_and_flag_it() {
        let r = resolve_with("HIVE_X", env(&[("LIGUS_X", "new"), ("HIVE_X", "old")])).unwrap();
        assert_eq!(r.value.to_str().unwrap(), "new");
        assert!(r.conflict);
        let w = warnings_for(&r, false);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("LIGUS_X and HIVE_X are both set and differ; using LIGUS_X"), "{w:?}");
    }

    #[test]
    fn blank_canonical_does_not_mask_legacy() {
        let r = resolve_with("HIVE_X", env(&[("LIGUS_X", ""), ("HIVE_X", "old")])).unwrap();
        assert_eq!((r.value.to_str().unwrap(), r.source), ("old", EnvSource::Legacy));
        // Set-but-empty with nothing to fall back to stays set (empty).
        let r = resolve_with("HIVE_X", env(&[("LIGUS_X", "")])).unwrap();
        assert_eq!((r.value.to_str().unwrap(), r.source), ("", EnvSource::Canonical));
    }

    #[test]
    fn unprefixed_names_read_as_is() {
        let r = resolve_with("HOME", env(&[("HOME", "/h")])).unwrap();
        assert_eq!((r.value.to_str().unwrap(), r.source, r.legacy.clone()), ("/h", EnvSource::Plain, None));
        assert!(warnings_for(&r, true).is_empty());
    }

    #[test]
    fn legacy_note_only_when_enabled() {
        let r = resolve_with("HIVE_X", env(&[("HIVE_X", "old")])).unwrap();
        assert!(warnings_for(&r, false).is_empty());
        let w = warnings_for(&r, true);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("HIVE_X is a legacy name; rename it to LIGUS_X"), "{w:?}");
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
        assert_eq!(var("HIVE_COMPAT_RT_A").as_deref(), Ok("old"));
        assert_eq!(var("LIGUS_COMPAT_RT_A").as_deref(), Ok("old"));
        std::env::set_var("LIGUS_COMPAT_RT_A", "new");
        assert_eq!(var("HIVE_COMPAT_RT_A").as_deref(), Ok("new"));
        std::env::remove_var("LIGUS_COMPAT_RT_A");
        std::env::remove_var("HIVE_COMPAT_RT_A");
        assert_eq!(var("HIVE_COMPAT_RT_A"), Err(VarError::NotPresent));
        assert_eq!(env_opt("HIVE_COMPAT_RT_A"), None);
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

    #[test]
    fn workspace_state_reads_new_then_old_and_keeps_existing_paths() {
        let tmp = Tmp::new("ws");
        let root = tmp.path();
        // Nothing yet: the write dir.
        assert_eq!(workspace_state_path(root, "mcp-servers.json"), root.join(WORKSPACE_DIR_FOR_WRITES).join("mcp-servers.json"));
        assert!(!has_workspace_dir(root));
        // Legacy file only: found under .hive.
        std::fs::create_dir_all(root.join(".hive/worktrees/chat-a")).unwrap();
        std::fs::write(root.join(".hive/mcp-servers.json"), "[]").unwrap();
        assert!(has_workspace_dir(root));
        assert_eq!(workspace_state_path(root, "mcp-servers.json"), root.join(".hive/mcp-servers.json"));
        // Canonical file present: it wins.
        std::fs::create_dir_all(root.join(".ligus")).unwrap();
        std::fs::write(root.join(".ligus/mcp-servers.json"), "[]").unwrap();
        assert_eq!(workspace_state_path(root, "mcp-servers.json"), root.join(".ligus/mcp-servers.json"));
        // An existing legacy worktree stays where git recorded it.
        assert_eq!(workspace_state_path(root, "worktrees/chat-a"), root.join(".hive/worktrees/chat-a"));
        // Per file: a file only in .hive is still found with .ligus present.
        std::fs::write(root.join(".hive/runtimes.json"), "[]").unwrap();
        assert_eq!(workspace_state_path(root, "runtimes.json"), root.join(".hive/runtimes.json"));
    }

    #[test]
    fn config_file_new_name_first_and_writes_back_to_the_read_file() {
        let tmp = Tmp::new("cfg");
        let dir = tmp.path();
        assert_eq!(find_config_file(dir), None);
        assert_eq!(config_file_for_write(dir), dir.join(CONFIG_FILE_FOR_WRITES));
        std::fs::write(dir.join("hive.config.toml"), "").unwrap();
        assert_eq!(find_config_file(dir), Some(dir.join("hive.config.toml")));
        assert_eq!(config_file_for_write(dir), dir.join("hive.config.toml"));
        std::fs::write(dir.join("ligus.config.toml"), "").unwrap();
        assert_eq!(find_config_file(dir), Some(dir.join("ligus.config.toml")));
        assert_eq!(config_file_for_write(dir), dir.join("ligus.config.toml"));
        let c = config_candidates(&[Path::new("/w"), Path::new("/d")]);
        let names: Vec<_> = c.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(names, ["/w/ligus.config.toml", "/w/hive.config.toml", "/d/ligus.config.toml", "/d/hive.config.toml"]);
    }

    #[test]
    fn home_data_dir_prefers_existing_new_then_old() {
        let tmp = Tmp::new("home");
        let home = tmp.path();
        assert_eq!(home_data_dir(home).0, home.join(HOME_DATA_DIR_FOR_WRITES));
        std::fs::create_dir(home.join(".hive")).unwrap();
        assert_eq!(home_data_dir(home), (home.join(".hive"), true), "legacy dir reused, flagged");
        std::fs::create_dir(home.join(".ligus")).unwrap();
        assert_eq!(home_data_dir(home), (home.join(".ligus"), false));
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

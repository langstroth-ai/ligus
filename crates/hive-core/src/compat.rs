//! Names after the product rename (Hive -> Ligus, short for ligustica), and
//! the rule that Ligus 2.0 starts fresh.
//!
//! Ligus 2.0 reads and writes only the new names: `LIGUS_*` environment
//! variables, `.ligus/` workspace state, `ligus.config.toml`, `~/.ligus` and
//! the `ai.ligus.desktop` app-data dir. Nothing from Hive 1.x is migrated:
//! a 1.x `~/.hive`, `com.hive.desktop` / `ai.hive.desktop` app-data dir,
//! repo `.hive/` or `hive.config.toml` is never read, modified, moved,
//! renamed, copied or deleted, so a 1.x install keeps running side by side.
//! [`check_data_dir`] refuses a 2.0 data dir that is (or links to, or
//! contains) 1.x data, and [`workspace_state_dir`] refuses a `.ligus` that
//! links into 1.x state. The 1.x names survive only in [`hive1_locations`]
//! and the `HIVE1_*` constants, which exist to keep agents out of them
//! (sandbox deny-lists) and to refuse them.
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
// Hive 1.x locations: never read, written, moved, renamed or deleted
// ---------------------------------------------------------------------------

/// Hive 1.x desktop bundle identifiers (their OS app-data dirs).
pub const HIVE1_DESKTOP_APP_IDS: [&str; 2] = ["com.hive.desktop", "ai.hive.desktop"];
/// Hive 1.x CLI data dir under `$HOME`.
pub const HIVE1_HOME_DATA_DIR: &str = ".hive";
/// Hive 1.x per-workspace state dir.
pub const HIVE1_WORKSPACE_DIR: &str = ".hive";
/// Hive 1.x config file name.
pub const HIVE1_CONFIG_FILE: &str = "hive.config.toml";

/// The OS app-data bases the desktop's dir may sit under, on any platform
/// (listing extra ones only makes the 1.x check stricter).
fn app_data_bases(home: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(h) = home {
        out.push(h.join("Library/Application Support"));
        out.push(h.join(".local/share"));
        out.push(h.join(".config"));
    }
    for var in ["XDG_DATA_HOME", "XDG_CONFIG_HOME", "APPDATA", "LOCALAPPDATA"] {
        if let Some(v) = std::env::var_os(var).filter(|v| !v.is_empty()) {
            out.push(PathBuf::from(v));
        }
    }
    out
}

/// Every known Hive 1.x data location for `home`: `~/.hive` and the 1.x
/// desktop app-data dirs.
pub fn hive1_locations(home: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(h) = home {
        out.push(h.join(HIVE1_HOME_DATA_DIR));
    }
    for base in app_data_bases(home) {
        for id in HIVE1_DESKTOP_APP_IDS {
            out.push(base.join(id));
        }
    }
    out
}

/// `$HOME` (or `%USERPROFILE%`).
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// `p` with symlinks resolved, also when `p` (or a tail of it) doesn't exist
/// yet: the nearest existing ancestor is canonicalized and the rest appended.
pub fn resolve_path(p: &Path) -> PathBuf {
    let mut tail = Vec::new();
    let mut cur = p.to_path_buf();
    loop {
        if let Ok(c) = cur.canonicalize() {
            let mut out = c;
            for t in tail.iter().rev() {
                out.push(t);
            }
            return out;
        }
        match (cur.file_name().map(|n| n.to_os_string()), cur.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                cur = parent.to_path_buf();
            }
            _ => return p.to_path_buf(),
        }
    }
}

/// Whether `a` and `b` overlap after resolving symlinks (one is, or is
/// inside, the other).
fn overlaps(a: &Path, b: &Path) -> bool {
    let (a, b) = (resolve_path(a), resolve_path(b));
    a.starts_with(&b) || b.starts_with(&a)
}

/// A marker 2.0 writes into each data dir it owns (see [`claim_data_dir`]).
pub const DATA_DIR_MARKER: &str = ".ligus-data-dir";

/// Files that mean a dir already holds event/identity data.
const DATA_FILES: [&str; 2] = ["hive.db", "identity.json"];
/// Entries only Ligus 2.0 creates in a data dir (a dir from a 2.0 release
/// candidate predates [`DATA_DIR_MARKER`]): the macOS keychain dir and the
/// systemd-creds dir of the OS secret store.
const V2_ONLY_ENTRIES: [&str; 2] = ["secrets", "credentials"];

/// Refuse `dir` as a Ligus 2.0 data dir when it would touch Hive 1.x data:
///
/// * it is, is inside, or contains a known 1.x location (`~/.hive`, the 1.x
///   desktop app-data dirs, or a 1.x app id next to `dir`), symlinks
///   resolved, so a 2.0 dir that is a symlink to a 1.x dir is refused too;
/// * it already holds `hive.db` or `identity.json` but was never claimed by
///   2.0 (no marker, not a 2.0 default location, no 2.0-only entries): that
///   is a 1.x data dir under some other name (e.g. a serve box's
///   `~/.hive-serve`).
///
/// Read-only: it only stats and canonicalizes paths.
pub fn check_data_dir(dir: &Path, home: Option<&Path>) -> Result<(), String> {
    let mut known = hive1_locations(home);
    if let Some(parent) = dir.parent() {
        known.extend(HIVE1_DESKTOP_APP_IDS.iter().map(|id| parent.join(id)));
    }
    for loc in &known {
        if std::fs::symlink_metadata(loc).is_ok() && overlaps(dir, loc) {
            return Err(format!(
                "{} overlaps the Hive 1.x data at {}. Ligus 2.0 starts fresh and never uses or changes Hive 1.x data; choose another data dir (LIGUS_DATA_DIR).",
                dir.display(),
                loc.display()
            ));
        }
    }
    let real = resolve_path(dir);
    let has_data = DATA_FILES.iter().any(|f| real.join(f).exists());
    // 2.0's own default locations (resolved, so a link named like one doesn't
    // count) were never written by 1.x: a dir there from a 2.0 release
    // candidate is 2.0's even without the marker.
    let v2_location = real.file_name().is_some_and(|n| n == DESKTOP_APP_ID) || home.is_some_and(|h| real == resolve_path(&h.join(HOME_DATA_DIR)));
    let claimed = v2_location || real.join(DATA_DIR_MARKER).is_file() || V2_ONLY_ENTRIES.iter().any(|e| real.join(e).is_dir());
    if has_data && !claimed {
        return Err(format!(
            "{} holds data Ligus 2.0 did not create (it looks like a Hive 1.x data dir). Ligus 2.0 starts fresh and leaves Hive 1.x data as it is; point LIGUS_DATA_DIR at a new, empty dir.",
            dir.display()
        ));
    }
    Ok(())
}

/// [`check_data_dir`], then create `dir` and mark it as a 2.0 data dir.
pub fn claim_data_dir(dir: &Path, home: Option<&Path>) -> Result<(), String> {
    check_data_dir(dir, home)?;
    mark_data_dir(dir).map_err(|e| format!("could not set up {}: {e}", dir.display()))
}

/// Create `dir` and write [`DATA_DIR_MARKER`] into it (idempotent).
pub fn mark_data_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let marker = dir.join(DATA_DIR_MARKER);
    if !marker.is_file() {
        std::fs::write(marker, b"Ligus 2 data directory\n")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Workspace state dir: `.ligus/` (a 1.x `.hive/` is ignored)
// ---------------------------------------------------------------------------

/// Per-workspace state directory.
pub const WORKSPACE_DIR: &str = ".ligus";

/// Err when `path` resolves (through symlinks) into Hive 1.x state: the repo's
/// `.hive/` or any known 1.x location.
fn refuse_hive1_state(root: &Path, path: &Path) -> io::Result<()> {
    let mut legacy = hive1_locations(home_dir().as_deref());
    legacy.push(root.join(HIVE1_WORKSPACE_DIR));
    for loc in &legacy {
        if std::fs::symlink_metadata(loc).is_ok() && overlaps(path, loc) {
            return Err(io::Error::other(format!(
                "{} resolves into Hive 1.x state ({}); Ligus 2.0 never writes there. Remove the link and try again.",
                path.display(),
                loc.display()
            )));
        }
    }
    Ok(())
}

/// `<root>/.ligus`, unless that would land in Hive 1.x state: when `.ligus`
/// resolves (through a symlink) into the repo's `.hive/` or any known 1.x
/// location, an error, and nothing is created.
pub fn workspace_state_dir(root: &Path) -> io::Result<PathBuf> {
    let dir = root.join(WORKSPACE_DIR);
    refuse_hive1_state(root, &dir)?;
    Ok(dir)
}

/// The path of workspace state `rel` (e.g. `mcp-servers.json` or
/// `worktrees/chat-<id>`) under `<root>/.ligus/`. A 1.x `.hive/` next to it is
/// never read or changed; a path that resolves into it through any symlink
/// (`.ligus` itself or one inside it) is an error.
pub fn workspace_state_path(root: &Path, rel: impl AsRef<Path>) -> io::Result<PathBuf> {
    let path = workspace_state_dir(root)?.join(rel);
    refuse_hive1_state(root, &path)?;
    Ok(path)
}

/// Whether `dir` holds a Ligus workspace state directory (`.ligus/`; a 1.x
/// `.hive/` does not count).
pub fn has_workspace_dir(dir: &Path) -> bool {
    dir.join(WORKSPACE_DIR).exists()
}

// ---------------------------------------------------------------------------
// Config file: `ligus.config.toml` (a 1.x `hive.config.toml` is not read)
// ---------------------------------------------------------------------------

/// Workspace / data-dir config file name.
pub const CONFIG_FILE: &str = "ligus.config.toml";

/// When `dir` has a 1.x `hive.config.toml` but no `ligus.config.toml`, say once
/// that it is not read. Nothing is renamed or copied: 2.0 starts fresh, and the
/// file may be committed and still used by teammates on 1.x.
fn note_hive1_config(dir: &Path) {
    let old = dir.join(HIVE1_CONFIG_FILE);
    if old.is_file() && !dir.join(CONFIG_FILE).exists() {
        note_once(&format!(
            "note: {} is a Hive 1.x config and is not read; Ligus 2.0 reads {CONFIG_FILE} (copy the settings you want into it)",
            old.display()
        ));
    }
}

/// The config file to read in `dir`, if it exists.
pub fn find_config_file(dir: &Path) -> Option<PathBuf> {
    note_hive1_config(dir);
    Some(dir.join(CONFIG_FILE)).filter(|p| p.is_file())
}

/// The config file to write in `dir`.
pub fn config_file_for_write(dir: &Path) -> PathBuf {
    dir.join(CONFIG_FILE)
}

/// Candidate config files in read order across `dirs` (e.g. workspace root,
/// then data dir).
pub fn config_candidates(dirs: &[&Path]) -> Vec<PathBuf> {
    dirs.iter()
        .map(|d| {
            note_hive1_config(d);
            d.join(CONFIG_FILE)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// CLI data dir: `~/.ligus` (a 1.x `~/.hive` is left as it is)
// ---------------------------------------------------------------------------

/// The CLI data directory under `$HOME`.
pub const HOME_DATA_DIR: &str = ".ligus";

/// The default CLI data dir under `home` when no data-dir env var is set:
/// `~/.ligus`. Pure path logic: a 1.x `~/.hive` is never read, moved or
/// changed. On a first run (no `~/.ligus` yet) with a `~/.hive` present, a
/// one-line note says so.
pub fn home_data_dir(home: &Path) -> PathBuf {
    let new = home.join(HOME_DATA_DIR);
    let old = home.join(HIVE1_HOME_DATA_DIR);
    if std::fs::symlink_metadata(&new).is_err() && std::fs::symlink_metadata(&old).is_ok() {
        note_once(&format!(
            "note: Ligus 2.0 starts fresh in {}; the Hive 1.x data in {} is left as it is",
            new.display(),
            old.display()
        ));
    }
    new
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

    /// Every entry under `dir` (files with a content hash, dirs, symlinks with
    /// their target), for byte-identical before/after checks.
    pub(crate) fn fingerprint(dir: &Path) -> Vec<(PathBuf, String)> {
        use std::hash::{Hash, Hasher};
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                let rel = p.strip_prefix(dir).unwrap().to_path_buf();
                let meta = std::fs::symlink_metadata(&p).unwrap();
                if meta.file_type().is_symlink() {
                    out.push((rel, format!("link:{}", std::fs::read_link(&p).unwrap().display())));
                } else if meta.is_dir() {
                    out.push((rel, "dir".into()));
                    stack.push(p);
                } else {
                    let bytes = std::fs::read(&p).unwrap();
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    bytes.hash(&mut h);
                    out.push((rel, format!("{}:{:x}", bytes.len(), h.finish())));
                }
            }
        }
        out.sort();
        out
    }

    /// A temp HOME with a 1.x `~/.hive` (CLI data), a 1.x desktop app-data dir
    /// and a repo with a 1.x `.hive/` (incl. a chat worktree dir) and
    /// `hive.config.toml`.
    fn hive1_fixture(tag: &str) -> (Tmp, PathBuf, PathBuf) {
        let tmp = Tmp::new(tag);
        let home = tmp.path().join("home");
        let w = |p: PathBuf, body: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        w(home.join(".hive/hive.db"), "1.x cli db");
        w(home.join(".hive/identity.json"), "{\"deviceId\":\"old\"}");
        w(home.join(".hive/keys/device.key"), "secret");
        let desk = home.join("Library/Application Support/com.hive.desktop");
        w(desk.join("hive.db"), "1.x desktop db");
        w(desk.join("identity.json"), "{}");
        w(desk.join("keys/account.key"), "k");
        let repo = tmp.path().join("repo");
        w(repo.join(".hive/mcp-servers.json"), "[]");
        w(repo.join(".hive/worktrees/chat-a/.git"), "gitdir: /x\n");
        w(repo.join("hive.config.toml"), "[app]\ndefault_runtime = \"x\"\n");
        (tmp, home, repo)
    }

    #[test]
    fn hive1_data_is_never_touched_and_2_0_starts_empty() {
        let (tmp, home, repo) = hive1_fixture("fresh");
        let before = fingerprint(tmp.path());

        // CLI data-dir resolution.
        let data = home_data_dir(&home);
        assert_eq!(data, home.join(".ligus"));
        assert_eq!(check_data_dir(&data, Some(&home)), Ok(()));
        // Workspace open: state paths, the workspace marker and config lookup.
        assert_eq!(workspace_state_path(&repo, "mcp-servers.json").unwrap(), repo.join(".ligus/mcp-servers.json"));
        assert!(!has_workspace_dir(&repo), "a 1.x .hive/ is not a 2.0 workspace");
        assert_eq!(find_config_file(&repo), None, "hive.config.toml is not read");
        assert_eq!(config_file_for_write(&repo), repo.join("ligus.config.toml"));
        assert_eq!(config_candidates(&[&repo, &data]), [repo.join("ligus.config.toml"), data.join("ligus.config.toml")]);

        assert_eq!(fingerprint(tmp.path()), before, "every 1.x file byte-identical, nothing created");
        assert!(!home.join(".ligus").exists() && !repo.join(".ligus").exists(), "2.0 starts empty");

        // Claiming the fresh dir creates only 2.0's own dir.
        claim_data_dir(&data, Some(&home)).unwrap();
        assert_eq!(std::fs::read_dir(&data).unwrap().count(), 1, "only the marker");
        let after: Vec<_> = fingerprint(tmp.path()).into_iter().filter(|(p, _)| !p.starts_with("home/.ligus")).collect();
        assert_eq!(after, before);
    }

    #[test]
    fn data_dirs_overlapping_hive1_are_refused() {
        let (tmp, home, _repo) = hive1_fixture("refuse");
        let before = fingerprint(tmp.path());
        let desk = home.join("Library/Application Support/com.hive.desktop");
        for bad in [home.join(".hive"), home.join(".hive/sub"), desk.clone(), home.clone()] {
            let e = check_data_dir(&bad, Some(&home)).unwrap_err();
            assert!(e.contains("Hive 1.x"), "{e}");
            assert!(claim_data_dir(&bad, Some(&home)).is_err());
        }
        // A 1.x app id next to the 2.0 dir counts even under a base we don't list.
        let odd = tmp.path().join("base");
        std::fs::create_dir_all(odd.join("com.hive.desktop")).unwrap();
        assert!(check_data_dir(&odd.join("ai.ligus.desktop"), Some(&home)).is_ok(), "siblings are fine");
        assert!(check_data_dir(&odd.join("com.hive.desktop"), Some(&home)).is_err());
        std::fs::remove_dir_all(odd).unwrap();
        // A 1.x data dir under another name (a serve box's ~/.hive-serve).
        let serve = home.join(".hive-serve");
        std::fs::create_dir_all(&serve).unwrap();
        std::fs::write(serve.join("hive.db"), "x").unwrap();
        assert!(check_data_dir(&serve, Some(&home)).unwrap_err().contains("did not create"));
        // ...but a 2.0-claimed dir with data, or a 2.0 RC dir, is fine.
        let ok = home.join("claimed");
        mark_data_dir(&ok).unwrap();
        std::fs::write(ok.join("hive.db"), "2.0").unwrap();
        assert_eq!(check_data_dir(&ok, Some(&home)), Ok(()));
        // A 2.0 RC dir at a 2.0 default location, without the marker.
        let rc_default = tmp.path().join("base").join(DESKTOP_APP_ID);
        std::fs::create_dir_all(&rc_default).unwrap();
        std::fs::write(rc_default.join("hive.db"), "2.0 rc").unwrap();
        assert_eq!(check_data_dir(&rc_default, Some(&home)), Ok(()));
        #[cfg(unix)]
        {
            // ...but a link named like one, pointing at 1.x-looking data, is not.
            let fake = tmp.path().join("base2");
            std::fs::create_dir_all(&fake).unwrap();
            std::os::unix::fs::symlink(&serve, fake.join(DESKTOP_APP_ID)).unwrap();
            assert!(check_data_dir(&fake.join(DESKTOP_APP_ID), Some(&home)).is_err());
            std::fs::remove_dir_all(&fake).unwrap();
        }
        std::fs::remove_dir_all(tmp.path().join("base")).unwrap();
        let rc = home.join("rc");
        std::fs::create_dir_all(rc.join("secrets")).unwrap();
        std::fs::write(rc.join("identity.json"), "{}").unwrap();
        assert_eq!(check_data_dir(&rc, Some(&home)), Ok(()));
        let after: Vec<_> = fingerprint(tmp.path())
            .into_iter()
            .filter(|(p, _)| !(p.starts_with("home/claimed") || p.starts_with("home/rc") || p.starts_with("home/.hive-serve")))
            .collect();
        assert_eq!(after, before, "refusing writes nothing");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_into_hive1_are_refused() {
        let (tmp, home, repo) = hive1_fixture("links");
        // ~/.ligus -> ~/.hive, and a data dir linking to the 1.x desktop dir.
        std::os::unix::fs::symlink(home.join(".hive"), home.join(".ligus")).unwrap();
        assert!(check_data_dir(&home_data_dir(&home), Some(&home)).is_err());
        let link = tmp.path().join("desk-link");
        std::os::unix::fs::symlink(home.join("Library/Application Support/com.hive.desktop"), &link).unwrap();
        assert!(check_data_dir(&link, Some(&home)).is_err());
        assert!(check_data_dir(&link.join("nested"), Some(&home)).is_err());
        // A repo's .ligus linking to its .hive.
        std::os::unix::fs::symlink(".hive", repo.join(".ligus")).unwrap();
        assert!(workspace_state_dir(&repo).is_err());
        assert!(workspace_state_path(&repo, "worktrees/chat-b").is_err());
        // A plain .ligus dir is fine.
        std::fs::remove_file(repo.join(".ligus")).unwrap();
        std::fs::create_dir(repo.join(".ligus")).unwrap();
        assert_eq!(workspace_state_dir(&repo).unwrap(), repo.join(".ligus"));
        assert!(has_workspace_dir(&repo));
        // ...but a link inside it into .hive is refused too.
        std::os::unix::fs::symlink("../.hive/worktrees", repo.join(".ligus/worktrees")).unwrap();
        assert!(workspace_state_path(&repo, "worktrees/chat-b").is_err());
        assert!(workspace_state_path(&repo, "worktrees").is_err());
        assert_eq!(workspace_state_path(&repo, "mcp-servers.json").unwrap(), repo.join(".ligus/mcp-servers.json"));
    }

    #[test]
    fn resolve_path_handles_missing_tails() {
        let tmp = Tmp::new("resolve");
        let real = tmp.path().canonicalize().unwrap();
        assert_eq!(resolve_path(&tmp.path().join("a/b")), real.join("a/b"));
        assert_eq!(resolve_path(tmp.path()), real);
    }

    #[test]
    fn config_reads_only_ligus_config() {
        let tmp = Tmp::new("cfg");
        let dir = tmp.path();
        assert_eq!(find_config_file(dir), None);
        std::fs::write(dir.join(CONFIG_FILE), "a = 1").unwrap();
        std::fs::write(dir.join(HIVE1_CONFIG_FILE), "b = 2").unwrap();
        assert_eq!(find_config_file(dir), Some(dir.join(CONFIG_FILE)));
        assert_eq!(std::fs::read_to_string(dir.join(HIVE1_CONFIG_FILE)).unwrap(), "b = 2");
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

//! Product names on disk, in the environment and in links.
//!
//! Ligus (short for ligustica) reads and writes `LIGUS_*` environment
//! variables, `.ligus/` workspace state, `ligus.config.toml`, `~/.ligus` and
//! the `ai.ligus.desktop` app-data dir, and emits `ligus://` deep links.
//!
//! Wire and protocol identifiers and crypto domain-separation strings are
//! not product names and never change (they are pinned by the golden vectors
//! and by tests in `crypto.rs`/`e2ee.rs`).

use std::env::VarError;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// Environment-variable prefix.
pub const ENV_PREFIX: &str = "LIGUS_";

/// Like `std::env::var_os`. Every product variable is read through this
/// module (and the `fold_ignores_env` test lists the calls).
pub fn var_os(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

/// Like `std::env::var`.
pub fn var(name: &str) -> Result<String, VarError> {
    std::env::var(name)
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
// Desktop bundle identifier
// ---------------------------------------------------------------------------

/// The desktop app's bundle identifier. It names the OS app-data dir
/// (`~/Library/Application Support/<id>`, `%APPDATA%\<id>`,
/// `~/.local/share/<id>`).
pub const DESKTOP_APP_ID: &str = "ai.ligus.desktop";

// ---------------------------------------------------------------------------
// Deep links
// ---------------------------------------------------------------------------

/// The URL scheme of phone deep links (`ligus://pair?…`, `ligus://chat/<id>`).
pub const DEEP_LINK_SCHEME: &str = "ligus";

/// A deep link `ligus://<rest>`.
pub fn deep_link(rest: &str) -> String {
    format!("{DEEP_LINK_SCHEME}://{rest}")
}

/// The part after `ligus://` (scheme matched case-insensitively), or `None`
/// for any other link.
pub fn strip_deep_link_scheme(link: &str) -> Option<&str> {
    let (scheme, rest) = link.split_once("://")?;
    scheme
        .eq_ignore_ascii_case(DEEP_LINK_SCHEME)
        .then_some(rest)
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// `$HOME` (or `%USERPROFILE%`).
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
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

/// Per-workspace state directory.
pub const WORKSPACE_DIR: &str = ".ligus";

/// `<root>/.ligus`.
pub fn workspace_state_dir(root: &Path) -> io::Result<PathBuf> {
    Ok(root.join(WORKSPACE_DIR))
}

/// The path of workspace state `rel` (e.g. `mcp-servers.json` or
/// `worktrees/chat-<id>`) under `<root>/.ligus/`.
pub fn workspace_state_path(root: &Path, rel: impl AsRef<Path>) -> io::Result<PathBuf> {
    Ok(workspace_state_dir(root)?.join(rel))
}

/// Whether `dir` holds a Ligus workspace state directory (`.ligus/`).
pub fn has_workspace_dir(dir: &Path) -> bool {
    dir.join(WORKSPACE_DIR).exists()
}

/// Workspace / data-dir config file name.
pub const CONFIG_FILE: &str = "ligus.config.toml";

/// The config file to read in `dir`, if it exists.
pub fn find_config_file(dir: &Path) -> Option<PathBuf> {
    Some(dir.join(CONFIG_FILE)).filter(|p| p.is_file())
}

/// The config file to write in `dir`.
pub fn config_file_for_write(dir: &Path) -> PathBuf {
    dir.join(CONFIG_FILE)
}

/// Candidate config files in read order across `dirs` (e.g. workspace root,
/// then data dir).
pub fn config_candidates(dirs: &[&Path]) -> Vec<PathBuf> {
    dirs.iter().map(|d| d.join(CONFIG_FILE)).collect()
}

/// The CLI data directory under `$HOME`.
pub const HOME_DATA_DIR: &str = ".ligus";

/// The default CLI data dir under `home` when no data-dir env var is set:
/// `~/.ligus`.
pub fn home_data_dir(home: &Path) -> PathBuf {
    home.join(HOME_DATA_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_env_round_trip() {
        // A unique name so parallel tests can't collide.
        std::env::remove_var("LIGUS_NAMES_RT_A");
        assert_eq!(var("LIGUS_NAMES_RT_A"), Err(VarError::NotPresent));
        assert!(!is_set("LIGUS_NAMES_RT_A"));
        std::env::set_var("LIGUS_NAMES_RT_A", " ");
        assert!(is_set("LIGUS_NAMES_RT_A"));
        assert_eq!(env_opt("LIGUS_NAMES_RT_A"), None, "blank is unset");
        std::env::set_var("LIGUS_NAMES_RT_A", "v");
        assert_eq!(var("LIGUS_NAMES_RT_A").as_deref(), Ok("v"));
        assert_eq!(env_opt("LIGUS_NAMES_RT_A").as_deref(), Some("v"));
        std::env::remove_var("LIGUS_NAMES_RT_A");
    }

    #[test]
    fn paths_use_ligus_names() {
        let root = Path::new("/r");
        assert_eq!(
            workspace_state_path(root, "mcp-servers.json").unwrap(),
            root.join(".ligus/mcp-servers.json")
        );
        assert_eq!(config_file_for_write(root), root.join("ligus.config.toml"));
        assert_eq!(
            config_candidates(&[root, Path::new("/d")]),
            [root.join(CONFIG_FILE), Path::new("/d").join(CONFIG_FILE)]
        );
        assert_eq!(home_data_dir(Path::new("/h")), Path::new("/h/.ligus"));
    }

    #[test]
    fn resolve_path_handles_missing_tails() {
        let base = std::env::temp_dir().join(format!("names-resolve-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let real = base.canonicalize().unwrap();
        assert_eq!(resolve_path(&base.join("a/b")), real.join("a/b"));
        assert_eq!(resolve_path(&base), real);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn deep_links_use_the_ligus_scheme() {
        assert_eq!(deep_link("pair?x=1"), "ligus://pair?x=1");
        assert_eq!(strip_deep_link_scheme("ligus://chat/abc"), Some("chat/abc"));
        assert_eq!(strip_deep_link_scheme("LIGUS://chat/abc"), Some("chat/abc"));
        assert_eq!(strip_deep_link_scheme("https://pair?x=1"), None);
        assert_eq!(strip_deep_link_scheme("ligusx://pair"), None);
        assert_eq!(strip_deep_link_scheme("pair?x=1"), None);
    }
}

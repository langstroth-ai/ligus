//! Finding checkouts under this device's scan roots (repos-and-checkouts
//! 4.2). Bounded: depth 3, 2,000 directories per root, no symlinks, never a
//! shallow or system directory, never a linked worktree or Ligus's own
//! worktrees. Matches are recorded unconfirmed: a person says "use it"
//! before a turn ever runs there.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hive_core::workspace_repo::WorkspaceRepo;

/// How deep below a root a checkout may sit.
pub const MAX_DEPTH: usize = 3;
/// At most this many directories visited per root.
pub const MAX_DIRS: usize = 2000;

const SKIP: &[&str] = &["node_modules", "target", ".ligus", ".hive", ".git", "vendor", "Library", ".cache", ".Trash"];

/// Whether `p` is too broad or too sensitive to scan or adopt: the file
/// system root, a top-level or system directory, or a home directory
/// itself (the v1.10.2 launch-hang guard, shared).
pub fn is_shallow_or_system_dir(p: &Path) -> bool {
    let p = super::git::canonical(p);
    let comps = p.components().filter(|c| matches!(c, std::path::Component::Normal(_))).count();
    if comps <= 1 {
        return true;
    }
    for var in ["HOME", "USERPROFILE"] {
        if let Some(home) = std::env::var_os(var).map(PathBuf::from) {
            if super::git::canonical(&home) == p {
                return true;
            }
        }
    }
    let s = p.to_string_lossy().to_ascii_lowercase();
    const SYSTEM: &[&str] = &[
        "/system", "/usr", "/bin", "/sbin", "/etc", "/private/etc", "/var/db", "/private/var/db", "/var/log", "/library", "/applications",
        "/opt/homebrew", "/proc", "/sys", "/dev", "/boot", "/snap", "c:\\windows", "c:\\program files",
    ];
    SYSTEM.iter().any(|d| s == *d || s.starts_with(&format!("{d}/")) || s.starts_with(&format!("{d}\\")))
}

/// A checkout a scan found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub repo_id: uuid::Uuid,
    pub via_fork: bool,
}

/// Every main checkout (a `.git` directory, not a linked worktree's `.git`
/// file) under `root`, within the bounds.
pub fn checkouts_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if is_shallow_or_system_dir(root) {
        return out;
    }
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut visited = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        visited += 1;
        if visited > MAX_DIRS {
            break;
        }
        let dotgit = dir.join(".git");
        match std::fs::symlink_metadata(&dotgit) {
            Ok(m) if m.is_dir() => {
                out.push(dir.clone());
                continue;
            }
            // A linked worktree (its `.git` is a file): never a checkout of its own.
            Ok(_) => continue,
            Err(_) => {}
        }
        if depth >= MAX_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if !ft.is_dir() || ft.is_symlink() {
                continue;
            }
            let name = e.file_name();
            let name = name.to_string_lossy();
            if SKIP.contains(&name.as_ref()) {
                continue;
            }
            stack.push((e.path(), depth + 1));
        }
    }
    out.sort();
    out
}

/// The checkouts under `roots` that cover one of `repos` (by `origin`, or
/// `upstream` for a fork).
pub fn scan(roots: &[PathBuf], repos: &[WorkspaceRepo], aliases: &BTreeMap<String, String>) -> Vec<Found> {
    let mut out = Vec::new();
    for root in roots {
        for dir in checkouts_under(root) {
            let remotes = super::git::remotes(&dir);
            if remotes.is_empty() {
                continue;
            }
            for r in repos {
                if let Some(via) = super::git::match_repo(&remotes, r, aliases) {
                    out.push(Found { path: dir.clone(), repo_id: r.id, via_fork: via == super::git::MatchVia::Upstream });
                    break;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::git::testutil::*;
    use super::*;
    use hive_core::workspace_repo::repo_id;

    #[test]
    fn scan_is_bounded_and_skips_worktrees_and_symlinks() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("apps");
        repo_with_origin(&root.join("WeFit"), Some("git@github.com:zazulam/wefit.git"));
        repo_with_origin(&root.join("deep/a/b/c/Too"), Some("git@github.com:zazulam/wefit.git"));
        repo_with_origin(&root.join("node_modules/x"), Some("git@github.com:zazulam/wefit.git"));
        sh(&root.join("WeFit"), &["worktree", "add", "-q", &root.join("linked").to_string_lossy()]);
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("WeFit"), root.join("alias")).unwrap();
        let c = "github.com/zazulam/wefit";
        let repo = WorkspaceRepo { id: repo_id(c), canonical: c.into(), display_name: "WeFit".into(), forge_kind: String::new(), default_branch: String::new(), root_commit: None };
        let found = scan(std::slice::from_ref(&root), &[repo], &BTreeMap::new());
        assert_eq!(found.iter().map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned()).collect::<Vec<_>>(), vec!["WeFit"]);
    }

    #[test]
    fn system_and_home_dirs_are_never_scanned() {
        assert!(is_shallow_or_system_dir(Path::new("/")));
        assert!(is_shallow_or_system_dir(Path::new("/usr/local")));
        if let Some(h) = std::env::var_os("HOME") {
            assert!(is_shallow_or_system_dir(Path::new(&h)));
        }
        // Windows: canonical paths are verbatim (`\\?\C:\…`), and the home
        // directory is USERPROFILE.
        #[cfg(windows)]
        {
            assert!(is_shallow_or_system_dir(Path::new(r"C:\Windows\System32")));
            if let Some(h) = std::env::var_os("USERPROFILE") {
                assert!(is_shallow_or_system_dir(Path::new(&h)));
            }
        }
        let t = tempfile::tempdir().unwrap();
        let deep = t.path().join("a/b");
        std::fs::create_dir_all(&deep).unwrap();
        assert!(!is_shallow_or_system_dir(&deep));
    }
}

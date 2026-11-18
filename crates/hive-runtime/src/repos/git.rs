//! Reading a checkout's identity: its remotes (one reader, review F5e), its
//! git common dir, its root commit (one function, review F10).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hive_core::workspace_repo::{canonical_repo, is_commit_id, WorkspaceRepo};

/// Run hardened git in `dir`; stdout trimmed on success.
pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = hive_core::process_util::git()
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.first().copied().unwrap_or("")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!("git {}: {}", args.first().copied().unwrap_or(""), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// The git dir shared by all worktrees of the checkout at `dir` (where the
/// config and the managing-host marker live).
pub fn git_common_dir(dir: &Path) -> Option<PathBuf> {
    let raw = git(dir, &["rev-parse", "--git-common-dir"]).ok()?;
    let p = PathBuf::from(raw);
    let p = if p.is_absolute() { p } else { dir.join(p) };
    std::fs::canonicalize(&p).ok().or(Some(p))
}

/// The main checkout's top level for `dir` (itself, or for a linked
/// worktree such as a chat's, the checkout it belongs to).
pub fn main_checkout(dir: &Path) -> Option<PathBuf> {
    let common = git_common_dir(dir)?;
    if common.file_name().is_some_and(|n| n == ".git") {
        common.parent().map(Path::to_path_buf)
    } else {
        // A bare repository: the common dir is the repository.
        Some(common)
    }
}

/// A checkout's remotes, as its own config file says, without includes and
/// without `insteadOf` (rewrites are transport, never identity): remote
/// name → every `url` value it has.
pub fn remotes(dir: &Path) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let Some(common) = git_common_dir(dir) else { return out };
    let config = common.join("config");
    let Ok(text) = git(dir, &["config", "--file", &config.to_string_lossy(), "--no-includes", "--get-regexp", r"^remote\..*\.url$"]) else {
        return out;
    };
    for line in text.lines() {
        let Some((key, url)) = line.split_once(' ') else { continue };
        let Some(name) = key.strip_prefix("remote.").and_then(|k| k.strip_suffix(".url")) else { continue };
        out.entry(name.to_string()).or_default().push(url.trim().to_string());
    }
    out
}

/// How a checkout covers a workspace repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchVia {
    /// Its `origin` is the repo.
    Origin,
    /// Its `upstream` is the repo (a fork checkout).
    Upstream,
}

/// Whether the checkout with these `remotes` covers `repo`: `origin` (or,
/// for a fork, `upstream`) canonicalizes to the repo's canonical. A remote
/// with more than one `url` is ambiguous and never matches.
pub fn match_repo(remotes: &BTreeMap<String, Vec<String>>, repo: &WorkspaceRepo, aliases: &BTreeMap<String, String>) -> Option<MatchVia> {
    let is = |name: &str| {
        remotes
            .get(name)
            .filter(|urls| urls.len() == 1)
            .and_then(|urls| canonical_repo(&urls[0], &repo.forge_kind, aliases).ok())
            .is_some_and(|c| c == repo.canonical)
    };
    if repo.canonical.starts_with("local:") {
        return None;
    }
    if is("origin") {
        Some(MatchVia::Origin)
    } else if is("upstream") {
        Some(MatchVia::Upstream)
    } else {
        None
    }
}

/// The canonical of a checkout's `origin` (or, for a fork, `upstream`)
/// with no case hint: what to suggest when proposing a new repo.
pub fn suggested_canonical(dir: &Path, aliases: &BTreeMap<String, String>) -> Option<(String, bool)> {
    let r = remotes(dir);
    let one = |name: &str| r.get(name).filter(|u| u.len() == 1).and_then(|u| canonical_repo(&u[0], "", aliases).ok());
    match (one("upstream"), one("origin")) {
        (Some(up), Some(_)) => Some((up, true)),
        (_, Some(o)) => Some((o, false)),
        _ => None,
    }
}

/// The checkout's root commit: the last parentless commit reachable from
/// `HEAD` (the one function both repo fingerprints and workspace trust use).
/// `None` for an empty or shallow repository (a shallow clone's graft point
/// isn't a root).
pub fn root_commit(dir: &Path) -> Option<String> {
    if git(dir, &["rev-parse", "--is-shallow-repository"]).is_ok_and(|s| s == "true") {
        return None;
    }
    let out = git(dir, &["rev-list", "--max-parents=0", "HEAD"]).ok()?;
    out.lines().last().map(str::to_string).filter(|c| is_commit_id(c))
}

/// Fingerprint membership (review F10): `root` is a parentless commit in
/// this checkout. Never a recomputation, so merged-in histories, branches
/// and dates don't matter.
pub fn has_root(dir: &Path, root: &str) -> bool {
    if !is_commit_id(root) {
        return false;
    }
    git(dir, &["cat-file", "-e", &format!("{root}^{{commit}}")]).is_ok() && git(dir, &["rev-parse", "--verify", "--quiet", &format!("{root}^")]).is_err()
}

/// Whether `commit` is a commit object in the checkout.
pub fn has_commit(dir: &Path, commit: &str) -> bool {
    is_commit_id(commit) && git(dir, &["cat-file", "-e", &format!("{commit}^{{commit}}")]).is_ok()
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::Path;

    pub fn sh(dir: &Path, args: &[&str]) -> String {
        super::git(dir, args).unwrap_or_else(|e| panic!("{args:?}: {e}"))
    }

    /// A repo with one commit and an `origin` remote URL (never fetched).
    pub fn repo_with_origin(dir: &Path, origin: Option<&str>) {
        std::fs::create_dir_all(dir).unwrap();
        sh(dir, &["init", "-q", "-b", "main"]);
        sh(dir, &["config", "user.email", "t@example.com"]);
        sh(dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("README.md"), "x\n").unwrap();
        sh(dir, &["add", "-A"]);
        sh(dir, &["commit", "-q", "-m", "init"]);
        if let Some(o) = origin {
            sh(dir, &["remote", "add", "origin", o]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;
    use hive_core::workspace_repo::repo_id;

    fn wefit() -> WorkspaceRepo {
        let c = "github.com/zazulam/wefit";
        WorkspaceRepo { id: repo_id(c), canonical: c.into(), display_name: "WeFit".into(), forge_kind: "github".into(), default_branch: "main".into(), root_commit: None }
    }

    #[test]
    fn origin_or_upstream_match_and_multi_url_is_ambiguous() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("a");
        repo_with_origin(&a, Some("git@github.com:ZaZulam/WeFit.git"));
        let none = BTreeMap::new();
        assert_eq!(match_repo(&remotes(&a), &wefit(), &none), Some(MatchVia::Origin));
        // A fork: origin is the person's own, upstream the workspace repo.
        let b = t.path().join("b");
        repo_with_origin(&b, Some("https://github.com/alice/wefit"));
        sh(&b, &["remote", "add", "upstream", "https://github.com/zazulam/wefit.git"]);
        assert_eq!(match_repo(&remotes(&b), &wefit(), &none), Some(MatchVia::Upstream));
        assert_eq!(suggested_canonical(&b, &none), Some(("github.com/zazulam/wefit".into(), true)));
        // Two urls on origin: ambiguous.
        sh(&a, &["config", "--add", "remote.origin.url", "https://github.com/evil/x"]);
        assert_eq!(match_repo(&remotes(&a), &wefit(), &none), None, "multi_url_remote_is_ambiguous");
    }

    #[test]
    fn detection_ignores_insteadof_and_includes() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("a");
        repo_with_origin(&a, Some("https://example.com/other/thing"));
        // A rewrite that would make it "look like" WeFit to git's transport.
        sh(&a, &["config", "url.https://github.com/zazulam/wefit.insteadOf", "https://example.com/other/thing"]);
        // And an include that sets origin to WeFit.
        let inc = t.path().join("inc.cfg");
        std::fs::write(&inc, "[remote \"origin\"]\n\turl = https://github.com/zazulam/wefit\n").unwrap();
        sh(&a, &["config", "include.path", &inc.to_string_lossy()]);
        assert_eq!(match_repo(&remotes(&a), &wefit(), &BTreeMap::new()), None, "include_path_does_not_change_identity");
    }

    #[test]
    fn root_commit_and_membership() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("a");
        repo_with_origin(&a, None);
        let root = root_commit(&a).unwrap();
        assert!(has_root(&a, &root));
        std::fs::write(a.join("f"), "1").unwrap();
        sh(&a, &["add", "-A"]);
        sh(&a, &["commit", "-qm", "two"]);
        let head = sh(&a, &["rev-parse", "HEAD"]);
        assert!(!has_root(&a, &head), "a commit with a parent isn't a root");
        assert!(has_commit(&a, &head) && !has_commit(&a, &"0".repeat(40)));
        // A chat worktree reports the main checkout.
        let wt = t.path().join("wt");
        sh(&a, &["worktree", "add", "-q", &wt.to_string_lossy()]);
        assert_eq!(main_checkout(&wt).map(|p| std::fs::canonicalize(p).unwrap()), Some(std::fs::canonicalize(&a).unwrap()));
    }
}

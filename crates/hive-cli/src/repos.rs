//! Repo bindings for a headless host — the CLI's equivalent of the desktop's
//! device-local workspace code-dir bindings (`workspace_dirs` in the app's
//! settings).
//!
//! An agent turn on this box that may edit files needs a git checkout to cut
//! its per-turn worktree from. Checkouts live under one directory
//! (`HIVE_REPOS_DIR`, default `<HIVE_DATA_DIR>/repos`), and a small TOML file
//! (`<HIVE_DATA_DIR>/repo-bindings.toml`) maps the workspace — and optionally
//! single chats — to one of them:
//!
//! ```toml
//! workspace = "hive"              # a name under HIVE_REPOS_DIR, or an absolute path
//! [chats]
//! "6f1c…-chat-uuid" = "rdna-gateway"
//! ```
//!
//! Resolution for a turn: the chat's binding, else the workspace binding,
//! else none (the turn gets no file tools; MCP tools still apply). Written by
//! `hive bind-repo`; the binding is device-local and never synced, exactly
//! like the desktop's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The binding file's name inside the data dir.
pub const BINDINGS_FILE: &str = "repo-bindings.toml";

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepoBindings {
    /// The workspace's checkout (name under the repos dir, or absolute path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Per-chat overrides, keyed by chat id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub chats: BTreeMap<String, String>,
}

/// Where checkouts live: `HIVE_REPOS_DIR`, else `<data_dir>/repos`.
pub fn repos_dir(data_dir: &Path) -> PathBuf {
    crate::env_opt("HIVE_REPOS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join("repos"))
}

impl RepoBindings {
    pub fn load(data_dir: &Path) -> Self {
        std::fs::read_to_string(data_dir.join(BINDINGS_FILE))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Write the file atomically (a temp file in the same dir, then a rename),
    /// so a turn reading it mid-write never sees half a file.
    pub fn save(&self, data_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let tmp = data_dir.join(format!(".{BINDINGS_FILE}.{}.tmp", Uuid::new_v4().simple()));
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, data_dir.join(BINDINGS_FILE)).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })?;
        Ok(())
    }

    /// The raw binding a turn in `chat` uses, and whether it's the chat's own
    /// (`true`) or the workspace default (`false`).
    pub fn raw_for(&self, chat: Uuid) -> Option<(&str, bool)> {
        match self.chats.get(&chat.to_string()) {
            Some(r) => Some((r.as_str(), true)),
            None => self.workspace.as_deref().map(|w| (w, false)),
        }
    }

    /// The checkout a turn in `chat` uses: the chat's binding, else the
    /// workspace's. Relative names resolve under `repos_dir`.
    pub fn resolve(&self, repos_dir: &Path, chat: Uuid) -> Option<PathBuf> {
        let raw = self.chats.get(&chat.to_string()).or(self.workspace.as_ref())?;
        Some(resolve_path(repos_dir, raw))
    }
}

pub fn resolve_path(repos_dir: &Path, raw: &str) -> PathBuf {
    let p = Path::new(raw.trim());
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        repos_dir.join(p)
    }
}

// ── Names, specs and remotes (the phone API's input validation) ───────────

/// A GitHub account/org name: 1–39 ASCII alphanumerics or single hyphens, not
/// starting or ending with one.
pub fn valid_owner(s: &str) -> bool {
    (1..=39).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
        && !s.contains("--")
}

/// A GitHub repository name: 1–100 of `[A-Za-z0-9._-]`, never `.` or `..`.
pub fn valid_repo_name(s: &str) -> bool {
    (1..=100).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && s != "."
        && s != ".."
}

/// `owner/name` or `https://github.com/owner/name[.git][/]` → (owner, name).
/// Nothing else: no other hosts, schemes, credentials, paths, queries or
/// fragments.
pub fn parse_repo_spec(input: &str) -> Result<(String, String)> {
    let raw = input.trim();
    if raw.is_empty() || raw.len() > 300 || raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("give a GitHub repository as owner/name or https://github.com/owner/name");
    }
    let path = if let Some(rest) = raw.strip_prefix("https://") {
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        if !host.eq_ignore_ascii_case("github.com") && !host.eq_ignore_ascii_case("www.github.com") {
            bail!("only github.com repositories can be cloned here");
        }
        if path.contains(['?', '#']) {
            bail!("a repository URL can't carry a query or fragment");
        }
        path.trim_end_matches('/')
    } else if raw.contains("://") || raw.contains('@') || raw.contains(':') {
        bail!("only https://github.com/owner/name URLs (or owner/name) are accepted");
    } else {
        raw
    };
    let mut parts = path.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        bail!("give a GitHub repository as owner/name");
    };
    let name = name.strip_suffix(".git").unwrap_or(name);
    if !valid_owner(owner) || !valid_repo_name(name) {
        bail!("'{owner}/{name}' isn't a valid GitHub repository name");
    }
    Ok((owner.to_string(), name.to_string()))
}

/// The checkout directory name a GitHub repo clones into under the repos dir:
/// `<owner>__<name>`. Unambiguous, since an owner never contains `_`.
pub fn checkout_name(owner: &str, name: &str) -> String {
    format!("{owner}__{name}")
}

/// A binding the phone may set: a plain directory name directly under the
/// repos dir (no separators, no `..`, not hidden, not absolute).
pub fn valid_checkout_dir_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 160
        && !s.starts_with('.')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Where a checkout's `origin` points, with any credentials stripped: host
/// (lowercased, empty for a local path), owner and repo name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRef {
    pub host: String,
    pub owner: String,
    pub name: String,
}

impl RemoteRef {
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// Parse a git remote URL (`https://[user@]host/o/n(.git)`, `ssh://git@host/o/n`,
/// `git@host:o/n.git`, `file:///…/o/n.git` or a bare path) into a [`RemoteRef`].
pub fn parse_remote(url: &str) -> Option<RemoteRef> {
    let url = url.trim();
    let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
        if scheme.eq_ignore_ascii_case("file") {
            (String::new(), rest.to_string())
        } else {
            let (auth_host, path) = rest.split_once('/')?;
            let host = auth_host.rsplit('@').next()?.split(':').next()?.to_ascii_lowercase();
            (host, path.to_string())
        }
    } else if let Some((left, path)) = url.split_once(':').filter(|(l, _)| !l.contains('/') && l.len() > 1) {
        // scp-like `git@github.com:owner/name.git`
        (left.rsplit('@').next()?.to_ascii_lowercase(), path.to_string())
    } else {
        (String::new(), url.to_string())
    };
    let path = path.trim_end_matches('/');
    let mut segs = path.rsplit('/');
    let name = segs.next()?;
    let owner = segs.next()?;
    let name = name.strip_suffix(".git").unwrap_or(name);
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some(RemoteRef { host, owner: owner.to_string(), name: name.to_string() })
}

// ── Checkout inspection ───────────────────────────────────────────────────

/// `git -C dir <args>` → trimmed stdout, `None` on failure.
pub fn git_out(dir: &Path, args: &[&str]) -> Option<String> {
    let out = hive_core::process_util::command("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// What the phone shows for one checkout. Never a filesystem path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckoutInfo {
    /// The checked-out branch (`None` when detached or unborn).
    pub branch: Option<String>,
    /// Short commit id of `HEAD` (`None` for an empty repo).
    pub sha: Option<String>,
    /// The base a new chat branch is cut from: `origin`'s default branch,
    /// else `main` / `master`, else the current branch.
    pub default_branch: Option<String>,
    pub remote: Option<RemoteRef>,
    /// Uncommitted changes to tracked or untracked files (Hive's own
    /// `.hive/` worktrees excluded).
    pub dirty: bool,
}

pub fn inspect(dir: &Path) -> CheckoutInfo {
    let branch = git_out(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]).filter(|b| !b.is_empty());
    let sha = git_out(dir, &["rev-parse", "--short", "HEAD"]).filter(|s| !s.is_empty());
    let remote = git_out(dir, &["remote", "get-url", "origin"]).and_then(|u| parse_remote(&u));
    let default_branch = git_out(dir, &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"])
        .and_then(|r| r.strip_prefix("origin/").map(str::to_string))
        .or_else(|| {
            ["main", "master"]
                .into_iter()
                .find(|b| git_out(dir, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{b}")]).is_some())
                .map(str::to_string)
        })
        .or_else(|| branch.clone());
    let dirty = git_out(dir, &["status", "--porcelain", "--", ".", ":(exclude).hive"]).is_some_and(|s| !s.is_empty());
    CheckoutInfo { branch, sha, default_branch, remote, dirty }
}

/// The chat's persistent branch in every repo (`hive/chat/<id>`, see
/// `hive_runtime::git_worktree::chat_worktree`).
pub fn chat_branch(chat: Uuid) -> String {
    format!("hive/chat/{chat}")
}

/// What a turn's system prompt says about the repository this chat is bound
/// to on this host, so a chat that moves between repos stays coherent: the
/// agent always knows which checkout (and branch) its files come from now.
pub fn turn_context(data_dir: &Path, chat: Uuid) -> String {
    let b = RepoBindings::load(data_dir);
    let Some((raw, _)) = b.raw_for(chat) else {
        return "Repository: none is bound to this chat on this host, so there are no repository files to read or \
                edit in this turn. The owner can pick one from the phone (the repo chip) or with `hive bind-repo`."
            .into();
    };
    let dir = resolve_path(&repos_dir(data_dir), raw);
    let label = Path::new(raw.trim()).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| raw.into());
    if !hive_runtime::git_worktree::is_git_repo(&dir) {
        return format!("Repository: this chat is bound to `{label}`, but that checkout is missing on this host.");
    }
    let info = inspect(&dir);
    let what = match &info.remote {
        Some(r) if !r.host.is_empty() => format!("`{}` ({} checkout `{label}`)", r.full_name(), r.host),
        _ => format!("the local checkout `{label}`"),
    };
    let base = info.branch.or(info.default_branch).unwrap_or_else(|| "HEAD".into());
    format!(
        "Repository: this chat currently works in {what} on this host. Base branch: `{base}`; the chat's own branch is \
         `{}`, and file changes you make come back as a reviewable proposal applied there. The owner can switch this \
         chat to another repository between turns, so earlier messages may refer to a different repository; the \
         files you can see now are this one's.",
        chat_branch(chat)
    )
}

/// `hive bind-repo <name|path> [--chat <id>]` / `hive bind-repo --clear [--chat <id>]`.
pub fn cmd_bind_repo(data_dir: &Path, target: Option<&str>, chat: Option<&str>, clear: bool) -> Result<()> {
    let mut b = RepoBindings::load(data_dir);
    let chat = chat.map(|c| Uuid::parse_str(c.trim()).context("--chat must be a chat id")).transpose()?;
    if clear {
        match chat {
            Some(c) => {
                b.chats.remove(&c.to_string());
            }
            None => b.workspace = None,
        }
        b.save(data_dir)?;
        println!("binding cleared.");
        return Ok(());
    }
    let Some(target) = target.map(str::trim).filter(|t| !t.is_empty()) else {
        bail!("usage: hive bind-repo <name-under-repos-dir | /absolute/path> [--chat <chat-id>]  (or --clear)");
    };
    let dir = resolve_path(&repos_dir(data_dir), target);
    if !hive_runtime::git_worktree::is_git_repo(&dir) {
        bail!("{} is not a git checkout (clone it there first)", dir.display());
    }
    match chat {
        Some(c) => {
            b.chats.insert(c.to_string(), target.to_string());
        }
        None => b.workspace = Some(target.to_string()),
    }
    b.save(data_dir)?;
    println!("bound {} to {}.", chat.map(|c| format!("chat {c}")).unwrap_or_else(|| "the workspace".into()), dir.display());
    Ok(())
}

/// `hive repos` — show the repos dir and the bindings.
pub fn cmd_repos(data_dir: &Path) -> Result<()> {
    let dir = repos_dir(data_dir);
    let b = RepoBindings::load(data_dir);
    println!("repos dir : {}", dir.display());
    match &b.workspace {
        Some(w) => println!("workspace : {w} -> {}", resolve_path(&dir, w).display()),
        None => println!("workspace : (unbound: agent turns get no file tools)"),
    }
    for (chat, target) in &b.chats {
        println!("chat {chat} : {target} -> {}", resolve_path(&dir, target).display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_binding_wins_over_workspace_and_names_resolve_under_repos_dir() {
        let chat = Uuid::new_v4();
        let other = Uuid::new_v4();
        let mut b = RepoBindings { workspace: Some("main".into()), ..Default::default() };
        b.chats.insert(chat.to_string(), "/abs/elsewhere".into());
        let repos = Path::new("/data/repos");
        assert_eq!(b.resolve(repos, chat), Some(PathBuf::from("/abs/elsewhere")));
        assert_eq!(b.resolve(repos, other), Some(PathBuf::from("/data/repos/main")));
        assert_eq!(RepoBindings::default().resolve(repos, other), None);
    }

    #[test]
    fn repo_specs_accept_only_github_owner_name() {
        let ok = |s: &str| parse_repo_spec(s).unwrap();
        assert_eq!(ok("honeyhive-ai/hive"), ("honeyhive-ai".into(), "hive".into()));
        assert_eq!(ok(" https://github.com/zazulam/rdna-gateway.git "), ("zazulam".into(), "rdna-gateway".into()));
        assert_eq!(ok("https://github.com/a/b.c_d-e/"), ("a".into(), "b.c_d-e".into()));
        assert_eq!(ok("a/.github"), ("a".into(), ".github".into()));
        for bad in [
            "",
            "hive",
            "a/b/c",
            "../etc",
            "a/..",
            "a/.",
            "-a/b",
            "a--b/c",
            "a/b c",
            "a/b;rm",
            "http://github.com/a/b",
            "https://gitlab.com/a/b",
            "https://evil.com/github.com/a/b",
            "https://user:tok@github.com/a/b",
            "https://github.com/a/b?x=1",
            "git@github.com:a/b.git",
            "file:///etc/passwd",
            "/abs/path",
            "ext::sh -c id",
            "a/\nb",
        ] {
            assert!(parse_repo_spec(bad).is_err(), "{bad:?} should be refused");
        }
        assert_eq!(checkout_name("o", "n"), "o__n");
    }

    #[test]
    fn checkout_dir_names_are_single_safe_segments() {
        for ok in ["sandbox", "o__n", "rdna-gateway", "a.b"] {
            assert!(valid_checkout_dir_name(ok), "{ok}");
        }
        for bad in ["", "..", ".hidden", "a/b", "/abs", "a\\b", "a b", "~"] {
            assert!(!valid_checkout_dir_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn remotes_parse_and_drop_credentials() {
        let r = parse_remote("https://x-access-token:SECRET@github.com/Owner/Repo.git").unwrap();
        assert_eq!(r, RemoteRef { host: "github.com".into(), owner: "Owner".into(), name: "Repo".into() });
        assert!(!format!("{r:?}").contains("SECRET"));
        assert_eq!(parse_remote("git@github.com:o/n.git").unwrap().host, "github.com");
        assert_eq!(parse_remote("ssh://git@github.com:22/o/n").unwrap().full_name(), "o/n");
        let f = parse_remote("file:///tmp/remotes/o/n.git").unwrap();
        assert_eq!((f.host.as_str(), f.full_name().as_str()), ("", "o/n"));
        assert_eq!(parse_remote("/srv/git/o/n").unwrap().full_name(), "o/n");
        assert!(parse_remote("nothing").is_none());
    }

    #[test]
    fn save_is_atomic_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = RepoBindings::default();
        for i in 0..5 {
            b.chats.insert(Uuid::new_v4().to_string(), format!("r{i}"));
            b.save(dir.path()).unwrap();
        }
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from(BINDINGS_FILE)]);
        assert_eq!(RepoBindings::load(dir.path()), b);
        let chat = Uuid::new_v4();
        assert_eq!(b.raw_for(chat), None);
        b.workspace = Some("w".into());
        assert_eq!(b.raw_for(chat), Some(("w", false)));
        b.chats.insert(chat.to_string(), "c".into());
        assert_eq!(b.raw_for(chat), Some(("c", true)));
    }

    #[test]
    fn round_trips_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = RepoBindings { workspace: Some("hive".into()), ..Default::default() };
        b.chats.insert(Uuid::nil().to_string(), "x".into());
        b.save(dir.path()).unwrap();
        assert_eq!(RepoBindings::load(dir.path()), b);
    }
}

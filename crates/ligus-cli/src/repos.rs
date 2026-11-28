//! Repo bindings for a headless host — the CLI's equivalent of the desktop's
//! device-local workspace code-dir bindings (`workspace_dirs` in the app's
//! settings).
//!
//! An agent turn on this box that may edit files needs a git checkout to cut
//! its per-turn worktree from. Checkouts live under one directory
//! (`LIGUS_REPOS_DIR`, default `<LIGUS_DATA_DIR>/repos`), and a small TOML file
//! (`<LIGUS_DATA_DIR>/repo-bindings.toml`) maps the workspace — and optionally
//! single chats — to one of them:
//!
//! ```toml
//! workspace = "ligus"              # a name under LIGUS_REPOS_DIR, or an absolute path
//! [chats]
//! "6f1c…-chat-uuid" = "rdna-gateway"
//! ```
//!
//! Resolution for a turn: the chat's binding, else the workspace binding,
//! else none (the turn gets no file tools; MCP tools still apply). Written by
//! `ligus bind-repo`; the binding is device-local and never synced, exactly
//! like the desktop's.
//!
//! Since 0.5 the workspace binding lives in the `workspace_folders` table of
//! `ligus.db` (one table for every app's workspace folder). The file's
//! `workspace` line is imported once and then kept as it was, for a
//! downgrade; the `[chats]` overrides stay in the file.

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

/// Where checkouts live: `LIGUS_REPOS_DIR`, else `<data_dir>/repos`.
pub fn repos_dir(data_dir: &Path) -> PathBuf {
    crate::env_opt("LIGUS_REPOS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join("repos"))
}

/// The name this CLI's workspace-folder rows carry (`workspace_folders`).
pub const FOLDER_SOURCE: &str = "cli";

/// This data dir's workspace, as the CLI's config names it:
/// `LIGUS_WORKSPACE`, else `config.toml`'s room, else `default`. A cheap
/// read (no secret store), for the binding lookup.
pub fn data_dir_workspace(data_dir: &Path) -> Uuid {
    #[derive(Deserialize)]
    struct Room {
        room: Option<String>,
    }
    let room = crate::env_opt("LIGUS_WORKSPACE")
        .or_else(|| {
            std::fs::read_to_string(data_dir.join("config.toml")).ok().and_then(|t| toml::from_str::<Room>(&t).ok()).and_then(|r| r.room)
        })
        .unwrap_or_else(|| "default".into());
    ligus_core::room_workspace_id(&room)
}

impl RepoBindings {
    /// The bindings: the chats' from the file, the workspace's from the
    /// `workspace_folders` table once this data dir's import ran (0.5), else
    /// from the file as before.
    pub fn load(data_dir: &Path) -> Self {
        let mut b = Self::load_file(data_dir);
        let db = data_dir.join("ligus.db");
        if let Some(row) = ligus_runtime::event_store::peek_workspace_folder(&db, data_dir_workspace(data_dir), FOLDER_SOURCE) {
            b.workspace = row.map(|p| p.to_string_lossy().into_owned());
        }
        b
    }

    /// The file alone (what 0.4 reads, and the import's source).
    pub fn load_file(data_dir: &Path) -> Self {
        std::fs::read_to_string(data_dir.join(BINDINGS_FILE))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Save: the workspace binding to the `workspace_folders` table once the
    /// import ran (the file then keeps the workspace binding it had at the
    /// import, for a downgrade), the chats to the file. Before the import,
    /// all of it to the file, as before.
    pub fn save(&self, data_dir: &Path) -> Result<()> {
        let db = data_dir.join("ligus.db");
        let mut file = self.clone();
        if ligus_runtime::event_store::write_workspace_folder(&db, data_dir_workspace(data_dir), FOLDER_SOURCE, self.workspace.as_deref())
            .context("couldn't record the workspace's folder")?
        {
            file.workspace = Self::load_file(data_dir).workspace;
        }
        file.save_file(data_dir)
    }

    /// Write the file atomically (a temp file in the same dir, then a rename),
    /// so a turn reading it mid-write never sees half a file.
    fn save_file(&self, data_dir: &Path) -> Result<()> {
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
            // "" = this chat has no repository here, explicitly (its scratch
            // repository was removed): never the workspace default.
            Some(r) if r.trim().is_empty() => None,
            Some(r) => Some((r.as_str(), true)),
            None => self.workspace.as_deref().map(|w| (w, false)),
        }
    }

    /// The checkout a turn in `chat` uses: the chat's binding, else the
    /// workspace's. Relative names resolve under `repos_dir`.
    pub fn resolve(&self, repos_dir: &Path, chat: Uuid) -> Option<PathBuf> {
        let (raw, _) = self.raw_for(chat)?;
        Some(resolve_path(repos_dir, raw))
    }
}

pub fn resolve_path(repos_dir: &Path, raw: &str) -> PathBuf {
    let p = Path::new(raw.trim());
    if p.is_absolute() || p.has_root() {
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
    // A Windows local path (`C:\remotes\owner\name.git`) separates with `\`.
    let path = path.replace('\\', "/");
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
    let out = ligus_core::process_util::git()
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
    /// The base a new chat branch is cut from: upstream's default branch in a
    /// fork, else `origin`'s default branch, else `main` / `master`, else the
    /// current branch.
    pub default_branch: Option<String>,
    pub remote: Option<RemoteRef>,
    /// Uncommitted changes to tracked or untracked files (the app's own
    /// `.ligus/` state dirs excluded).
    pub dirty: bool,
    /// Set when this checkout is a fork with an `upstream` remote: chat
    /// branches start from upstream's default branch and PRs open upstream.
    pub fork: Option<ForkLayout>,
}

/// The name of the remote a fork checkout fetches the parent project from.
pub const UPSTREAM_REMOTE: &str = "upstream";

/// A fork checkout: `origin` is the owner's fork (pushes go there), and
/// `upstream` is the parent project (fetch-only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkLayout {
    /// The parent project `upstream` fetches from.
    pub upstream: RemoteRef,
    /// Upstream's default branch: the base for chat branches and PRs.
    pub base_branch: String,
    /// `upstream`'s push URL doesn't point at the parent project (a dummy
    /// such as `DISABLED`), so a stray `git push upstream` can't land.
    pub push_disabled: bool,
}

impl ForkLayout {
    /// The remote-tracking ref of the base: `refs/remotes/upstream/<base>`.
    pub fn base_ref(&self) -> String {
        format!("refs/remotes/{UPSTREAM_REMOTE}/{}", self.base_branch)
    }
}

/// The fork layout of a checkout, if it has an `upstream` remote that points
/// at a different repository than `origin`. The base branch is
/// `upstream/HEAD`'s target, else whichever of `main` / `master` upstream has,
/// else the local branch tracking upstream.
pub fn fork_layout(dir: &Path) -> Option<ForkLayout> {
    let upstream = parse_remote(&git_out(dir, &["remote", "get-url", UPSTREAM_REMOTE])?)?;
    let origin = git_out(dir, &["remote", "get-url", "origin"]).and_then(|u| parse_remote(&u));
    if origin.as_ref().is_some_and(|o| same_repo(o, &upstream)) {
        return None;
    }
    let prefix = format!("{UPSTREAM_REMOTE}/");
    let base_branch = git_out(dir, &["symbolic-ref", "--quiet", "--short", &format!("refs/remotes/{UPSTREAM_REMOTE}/HEAD")])
        .and_then(|r| r.strip_prefix(&prefix).map(str::to_string))
        .or_else(|| {
            ["main", "master"]
                .into_iter()
                .find(|b| ref_exists(dir, &format!("refs/remotes/{UPSTREAM_REMOTE}/{b}")))
                .map(str::to_string)
        })
        .or_else(|| {
            // A local branch configured to track upstream (e.g. master → upstream/master).
            git_out(dir, &["for-each-ref", "--format=%(refname:short) %(upstream:short)", "refs/heads"])?
                .lines()
                .find_map(|l| l.split_once(' ').and_then(|(_, up)| up.strip_prefix(&prefix)).map(str::to_string))
        })?;
    let push_disabled = !upstream_push_enabled(dir);
    Some(ForkLayout { upstream, base_branch, push_disabled })
}

/// `upstream`'s push URL points at the upstream repository itself (pushes
/// would land there).
pub fn upstream_push_enabled(dir: &Path) -> bool {
    let fetch = git_out(dir, &["remote", "get-url", UPSTREAM_REMOTE]).and_then(|u| parse_remote(&u));
    let push = git_out(dir, &["remote", "get-url", "--push", UPSTREAM_REMOTE]).and_then(|u| parse_remote(&u));
    matches!((fetch, push), (Some(f), Some(p)) if same_repo(&f, &p))
}

/// Same repository (case-insensitive owner/name; hosts compared when both
/// are known).
pub fn same_repo(a: &RemoteRef, b: &RemoteRef) -> bool {
    a.owner.eq_ignore_ascii_case(&b.owner)
        && a.name.eq_ignore_ascii_case(&b.name)
        && (a.host.is_empty() || b.host.is_empty() || a.host == b.host)
}

pub fn ref_exists(dir: &Path, r: &str) -> bool {
    git_out(dir, &["rev-parse", "--verify", "--quiet", r]).is_some()
}

/// Refuse a push whose destination is the parent project of a fork: the
/// remote's push URL must not resolve to `upstream`'s repository (or to
/// `forbidden`, e.g. the parent GitHub reports). `Ok` names the repo pushed to.
pub fn check_push_target(dir: &Path, remote: &str, forbidden: Option<&RemoteRef>) -> std::result::Result<RemoteRef, String> {
    // `git push <remote>` pushes to every push URL, not just the first.
    let urls = git_out(dir, &["remote", "get-url", "--push", "--all", remote]).ok_or_else(|| format!("no remote named {remote}"))?;
    let upstream = git_out(dir, &["remote", "get-url", UPSTREAM_REMOTE]).and_then(|u| parse_remote(&u));
    let mut first: Option<RemoteRef> = None;
    for url in urls.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let target = parse_remote(url).ok_or_else(|| format!("can't tell where {remote} pushes to"))?;
        for no in upstream.iter().chain(forbidden) {
            // Hosts are ignored here: the same repository is reachable as
            // ssh.github.com, www.github.com or an ssh config alias.
            if target.owner.eq_ignore_ascii_case(&no.owner) && target.name.eq_ignore_ascii_case(&no.name) {
                return Err(format!(
                    "refusing to push: {remote} points at {}, the upstream project; this host pushes only to your fork",
                    no.full_name()
                ));
            }
        }
        match &first {
            None => first = Some(target),
            Some(f) if same_repo(f, &target) => {}
            Some(_) => return Err(format!("refusing to push: {remote} has push URLs for different repositories")),
        }
    }
    first.ok_or_else(|| format!("can't tell where {remote} pushes to"))
}

/// Keep `ligus.chatBase` (where fresh chat branches start, see
/// `ligus_runtime::git_worktree::chat_base`) at upstream's default branch in a
/// fork checkout, and drop a stale upstream base when the layout is gone.
pub fn ensure_chat_base(dir: &Path) {
    use ligus_runtime::git_worktree::CHAT_BASE_CONFIG;
    let current = git_out(dir, &["config", "--get", CHAT_BASE_CONFIG]).filter(|v| !v.is_empty());
    match fork_layout(dir) {
        Some(f) => {
            let want = f.base_ref();
            if current.as_deref() != Some(want.as_str()) && ref_exists(dir, &want) {
                let _ = git_out(dir, &["config", CHAT_BASE_CONFIG, &want]);
            }
        }
        None => {
            if current.is_some_and(|c| c.starts_with(&format!("refs/remotes/{UPSTREAM_REMOTE}/"))) {
                let _ = git_out(dir, &["config", "--unset", CHAT_BASE_CONFIG]);
            }
        }
    }
}

/// (behind, ahead) of `a` relative to `b` (`git rev-list --left-right --count b...a`).
pub fn behind_ahead(dir: &Path, b: &str, a: &str) -> Option<(u64, u64)> {
    let out = git_out(dir, &["rev-list", "--left-right", "--count", &format!("{b}...{a}")])?;
    let mut it = out.split_whitespace().map(|n| n.parse::<u64>().ok());
    Some((it.next()??, it.next()??))
}

// ── Readable PR branch names on a fork ─────────────────────────────────────

/// The file mapping (checkout, chat) → the branch its PRs were pushed as.
pub const PR_BRANCHES_FILE: &str = "pr-branches.toml";

/// Branch names a chat's PR was pushed under on a fork, so a later push
/// updates the same branch (and the same PR) even if the chat is renamed.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct PrBranches {
    /// checkout name → chat id → remote branch.
    #[serde(default)]
    pub checkouts: BTreeMap<String, BTreeMap<String, String>>,
}

impl PrBranches {
    pub fn load(data_dir: &Path) -> Self {
        std::fs::read_to_string(data_dir.join(PR_BRANCHES_FILE))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let tmp = data_dir.join(format!(".{PR_BRANCHES_FILE}.{}.tmp", Uuid::new_v4().simple()));
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, data_dir.join(PR_BRANCHES_FILE)).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })?;
        Ok(())
    }

    pub fn get(&self, checkout: &str, chat: Uuid) -> Option<&str> {
        self.checkouts.get(checkout)?.get(&chat.to_string()).map(String::as_str)
    }

    /// The branch for (checkout, chat): the recorded one, else a new readable
    /// name (recorded; the caller saves).
    pub fn get_or_assign(&mut self, checkout: &str, chat: Uuid, title: &str) -> String {
        if let Some(b) = self.get(checkout, chat) {
            return b.to_string();
        }
        let b = pr_branch_name(title, chat);
        self.checkouts.entry(checkout.to_string()).or_default().insert(chat.to_string(), b.clone());
        b
    }
}

/// `ligus/<short-title-slug>-<first 8 of the chat id>`: lowercase ASCII
/// letters, digits and single hyphens, the slug at most 40 characters.
pub fn pr_branch_name(title: &str, chat: Uuid) -> String {
    let mut slug = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-');
    let slug = if slug.is_empty() { "chat" } else { slug };
    let id = chat.simple().to_string();
    format!("ligus/{slug}-{}", &id[..8])
}

pub fn inspect(dir: &Path) -> CheckoutInfo {
    let branch = git_out(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]).filter(|b| !b.is_empty());
    let sha = git_out(dir, &["rev-parse", "--short", "HEAD"]).filter(|s| !s.is_empty());
    let remote = git_out(dir, &["remote", "get-url", "origin"]).and_then(|u| parse_remote(&u));
    // origin's HEAD, else main or master, else the current branch (one
    // detector for the desktop, `ligus repos` and `ligus setup folder`).
    let default_branch = ligus_runtime::setup::validate::default_branch(dir).or_else(|| branch.clone());
    let dirty = git_out(dir, &["status", "--porcelain", "--", ".", ":(exclude).ligus"]).is_some_and(|s| !s.is_empty());
    let fork = fork_layout(dir);
    // A fork's base is upstream's default branch, not origin's.
    let default_branch = fork.as_ref().map(|f| f.base_branch.clone()).or(default_branch);
    CheckoutInfo { branch, sha, default_branch, remote, dirty, fork }
}

/// The chat's persistent branch in every repo (`ligus/chat/<id>`, see
/// `ligus_runtime::git_worktree::chat_worktree`).
pub fn chat_branch(chat: Uuid) -> String {
    format!("ligus/chat/{chat}")
}

/// What a turn's system prompt says about the repository this chat is bound
/// to on this host, so a chat that moves between repos stays coherent: the
/// agent always knows which checkout (and branch) its files come from now.
pub fn turn_context(data_dir: &Path, chat: Uuid) -> String {
    let b = RepoBindings::load(data_dir);
    let Some((raw, _)) = b.raw_for(chat) else {
        return "Repository: none is bound to this chat on this host, so there are no repository files to read or \
                edit in this turn. The owner can pick one from the phone (the repo chip) or with `ligus bind-repo`."
            .into();
    };
    let dir = resolve_path(&repos_dir(data_dir), raw);
    let label = Path::new(raw.trim()).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| raw.into());
    if !ligus_runtime::git_worktree::is_git_repo(&dir) {
        return format!("Repository: this chat is bound to `{label}`, but that checkout is missing on this host.");
    }
    let info = inspect(&dir);
    let what = match &info.remote {
        Some(r) if !r.host.is_empty() => format!("`{}` ({} checkout `{label}`)", r.full_name(), r.host),
        _ => format!("the local checkout `{label}`"),
    };
    if let Some(f) = &info.fork {
        return format!(
            "Repository: this chat currently works in {what}, a fork of `{up}`, on this host. Base branch: \
             `{up_remote}/{base}` (the upstream project's `{base}`); the chat's own branch is `{}`, and file changes \
             you make come back as a reviewable proposal applied there. Pull requests from this chat are pushed to \
             the fork and opened against `{up}` as drafts by the owner, never by you. The owner can switch this chat \
             to another repository between turns, so earlier messages may refer to a different repository; the files \
             you can see now are this one's.",
            chat_branch(chat),
            up = f.upstream.full_name(),
            up_remote = UPSTREAM_REMOTE,
            base = f.base_branch,
        );
    }
    let base = info.branch.or(info.default_branch).unwrap_or_else(|| "HEAD".into());
    format!(
        "Repository: this chat currently works in {what} on this host. Base branch: `{base}`; the chat's own branch is \
         `{}`, and file changes you make come back as a reviewable proposal applied there. The owner can switch this \
         chat to another repository between turns, so earlier messages may refer to a different repository; the \
         files you can see now are this one's.",
        chat_branch(chat)
    )
}

/// `ligus bind-repo <name|path> [--chat <id>]` / `ligus bind-repo --clear [--chat <id>]`.
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
        bail!("usage: ligus bind-repo <name-under-repos-dir | /absolute/path> [--chat <chat-id>]  (or --clear)");
    };
    let dir = resolve_path(&repos_dir(data_dir), target);
    if !ligus_runtime::git_worktree::is_git_repo(&dir) {
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

/// The repos dir and the bindings, for `ligus repos list --json`.
pub fn repos_json(data_dir: &Path) -> serde_json::Value {
    let dir = repos_dir(data_dir);
    let b = RepoBindings::load(data_dir);
    serde_json::json!({
        "reposDir": dir,
        "bindings": {
            "workspace": b.workspace.as_ref().map(|w| resolve_path(&dir, w)),
            // "" = explicitly no repository (a removed scratch repository's chats).
            "chats": b.chats.iter().map(|(c, t)| (c.clone(), if t.trim().is_empty() { serde_json::Value::Null } else { serde_json::json!(resolve_path(&dir, t)) })).collect::<serde_json::Map<_, _>>(),
        },
    })
}

/// `ligus repos` — show the repos dir and the bindings.
pub fn cmd_repos(data_dir: &Path) -> Result<()> {
    let dir = repos_dir(data_dir);
    let b = RepoBindings::load(data_dir);
    println!("repos dir : {}", dir.display());
    match &b.workspace {
        Some(w) => println!("workspace : {w} -> {}", resolve_path(&dir, w).display()),
        None => println!("workspace : (unbound: agent turns get no file tools)"),
    }
    for (chat, target) in &b.chats {
        if target.trim().is_empty() {
            println!("chat {chat} : (no repository)");
        } else {
            println!("chat {chat} : {target} -> {}", resolve_path(&dir, target).display());
        }
    }
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        if let Some(f) = fork_layout(&e.path()) {
            println!(
                "fork      : {} -> upstream {} (base {}; upstream push {})",
                e.file_name().to_string_lossy(),
                f.upstream.full_name(),
                f.base_branch,
                if f.push_disabled { "disabled" } else { "ENABLED: set a dummy push URL" }
            );
        }
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
        assert_eq!(ok("langstroth-ai/ligus"), ("langstroth-ai".into(), "ligus".into()));
        assert_eq!(ok(" https://github.com/acme/web-app.git "), ("acme".into(), "web-app".into()));
        assert_eq!(ok("https://github.com/a/b.c_d-e/"), ("a".into(), "b.c_d-e".into()));
        assert_eq!(ok("a/.github"), ("a".into(), ".github".into()));
        for bad in [
            "",
            "ligus",
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

    /// The workspace binding moves to the `workspace_folders` table once:
    /// the import takes the file's line, later binds and unbinds go to the
    /// table, the file keeps the line it had (a downgrade still reads it),
    /// and the chats stay in the file.
    #[test]
    fn the_workspace_binding_moves_to_the_table_once() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();
        std::fs::write(data.join("config.toml"), "room = \"team\"\n").unwrap();
        let ws = ligus_core::room_workspace_id("team");
        let chat = Uuid::new_v4();
        let mut b = RepoBindings { workspace: Some("app".into()), ..Default::default() };
        b.chats.insert(chat.to_string(), "other".into());
        b.save(data).unwrap();
        let store = ligus_runtime::EventStore::open(data.join("ligus.db")).unwrap();
        assert_eq!(RepoBindings::load(data).workspace.as_deref(), Some("app"), "before the import: the file");
        let legacy = ligus_runtime::event_store::LegacyLocal {
            source: FOLDER_SOURCE,
            folders: vec![ligus_runtime::event_store::LegacyFolder { workspace_id: data_dir_workspace(data), path: "app".into() }],
        };
        store.migrate_local_tables(&legacy).unwrap();
        assert_eq!(store.workspace_folder(ws).unwrap(), Some(PathBuf::from("app")));
        let mut b = RepoBindings::load(data);
        assert_eq!(b.workspace.as_deref(), Some("app"));
        b.workspace = Some("/abs/elsewhere".into());
        b.save(data).unwrap();
        assert_eq!(RepoBindings::load(data).workspace.as_deref(), Some("/abs/elsewhere"));
        assert_eq!(RepoBindings::load_file(data).workspace.as_deref(), Some("app"), "the file keeps its line");
        assert_eq!(RepoBindings::load(data).chats.get(&chat.to_string()).map(String::as_str), Some("other"));
        let mut b = RepoBindings::load(data);
        b.workspace = None;
        b.save(data).unwrap();
        assert_eq!(RepoBindings::load(data).workspace, None, "unbound stays unbound");
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
    fn pr_branch_names_are_readable_and_stable() {
        let chat = Uuid::parse_str("6f1c2d3e-0000-4000-8000-000000000000").unwrap();
        assert_eq!(pr_branch_name("Fix: the v2 compiler (again)!", chat), "ligus/fix-the-v2-compiler-again-6f1c2d3e");
        assert_eq!(pr_branch_name("  ", chat), "ligus/chat-6f1c2d3e");
        assert_eq!(pr_branch_name("Ünïcode ✨ only", chat), "ligus/n-code-only-6f1c2d3e");
        let long = pr_branch_name(&"word ".repeat(40), chat);
        assert!(long.len() <= "ligus/".len() + 40 + 9 && !long.contains("--"), "{long}");
        let mut m = PrBranches::default();
        let first = m.get_or_assign("o__n", chat, "First title");
        assert_eq!(m.get_or_assign("o__n", chat, "Renamed later"), first);
        assert_ne!(m.get_or_assign("other", chat, "Renamed later"), first);
    }

    #[test]
    fn fork_layout_and_push_guard() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let run = |args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success());
        run(&["init", "-q"]);
        run(&["remote", "add", "origin", "https://github.com/me/pipelines.git"]);
        assert_eq!(fork_layout(dir), None, "no upstream remote");
        run(&["remote", "add", "upstream", "https://github.com/kubeflow/pipelines.git"]);
        assert_eq!(fork_layout(dir), None, "upstream has no branches fetched yet");
        run(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x"]);
        run(&["update-ref", "refs/remotes/upstream/master", "HEAD"]);
        let f = fork_layout(dir).unwrap();
        assert_eq!((f.upstream.full_name().as_str(), f.base_branch.as_str(), f.push_disabled), ("kubeflow/pipelines", "master", false));
        assert_eq!(f.base_ref(), "refs/remotes/upstream/master");
        assert_eq!(inspect(dir).default_branch.as_deref(), Some("master"));
        // Pushing to origin is fine; to upstream (or anything resolving to it) is not.
        assert_eq!(check_push_target(dir, "origin", None).unwrap().full_name(), "me/pipelines");
        assert!(check_push_target(dir, "upstream", None).unwrap_err().contains("refusing"));
        run(&["remote", "set-url", "--push", "origin", "git@github.com:KubeFlow/Pipelines.git"]);
        assert!(check_push_target(dir, "origin", None).is_err());
        run(&["remote", "set-url", "--push", "origin", "https://github.com/me/pipelines.git"]);
        let parent = RemoteRef { host: "github.com".into(), owner: "me".into(), name: "pipelines".into() };
        assert!(check_push_target(dir, "origin", Some(&parent)).is_err(), "a forbidden parent from GitHub");
        run(&["remote", "set-url", "--push", "upstream", "DISABLED"]);
        assert!(fork_layout(dir).unwrap().push_disabled);
        // ligus.chatBase follows the layout.
        ensure_chat_base(dir);
        assert_eq!(git_out(dir, &["config", "ligus.chatBase"]).as_deref(), Some("refs/remotes/upstream/master"));
        run(&["remote", "remove", "upstream"]);
        ensure_chat_base(dir);
        assert_eq!(git_out(dir, &["config", "ligus.chatBase"]), None);
    }

    /// `git push origin` pushes to every `pushurl`, and the same GitHub repo
    /// is reachable under other host names. Neither may slip a push to the
    /// upstream project past the guard.
    #[test]
    fn push_guard_checks_every_push_url_and_host_aliases() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let run = |args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success());
        run(&["init", "-q"]);
        run(&["remote", "add", "origin", "https://github.com/me/pipelines.git"]);
        run(&["remote", "add", "upstream", "https://github.com/kubeflow/pipelines.git"]);
        run(&["config", "--add", "remote.origin.pushurl", "https://github.com/me/pipelines.git"]);
        run(&["config", "--add", "remote.origin.pushurl", "https://github.com/kubeflow/pipelines.git"]);
        assert!(check_push_target(dir, "origin", None).is_err(), "second push URL is upstream");
        run(&["config", "--unset-all", "remote.origin.pushurl"]);
        run(&["config", "--add", "remote.origin.pushurl", "ssh://git@ssh.github.com:443/kubeflow/pipelines.git"]);
        assert!(check_push_target(dir, "origin", None).is_err(), "ssh.github.com is github.com");
        run(&["config", "--unset-all", "remote.origin.pushurl"]);
        assert_eq!(check_push_target(dir, "origin", None).unwrap().full_name(), "me/pipelines");
    }

    #[test]
    fn round_trips_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = RepoBindings { workspace: Some("ligus".into()), ..Default::default() };
        b.chats.insert(Uuid::nil().to_string(), "x".into());
        b.save(dir.path()).unwrap();
        assert_eq!(RepoBindings::load(dir.path()), b);
    }
}

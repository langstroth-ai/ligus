//! Scratch repositories: a throwaway git repository a chat can work in.
//!
//! The host that runs the chat's agents (the box for a phone chat, the
//! desktop or `lt` for their own chats) creates an **empty** repository
//! (one empty commit on `main`) under its scratch root, by default
//! `<data dir>/scratch/<slug>-<short id>`, and records it here, in this
//! device's store (`repo_settings`, never synced). Agents then work in it
//! exactly as in any other checkout: the chat's own branch and worktree,
//! proposals, approvals and the sandbox. Nothing is relaxed.
//!
//! * **Registry** ([`list`], [`by_path`]): what this host created. Only an
//!   entry here ever lets this host delete a folder; the synced
//!   `WorkspaceRepo::scratch` hint never does.
//! * **Keep** ([`keep_dir`]): move it out of the scratch root to a chosen
//!   folder (or the host's repos dir) and drop the scratch flag.
//! * **Cleanup** ([`due`], [`remove_dir_checked`]): a scratch repository
//!   nobody kept is removed after [`DEFAULT_CLEANUP_DAYS`] days without
//!   activity (a host setting; off allowed), with a notice in its chats
//!   [`NOTICE_DAYS`] days before. Removal deletes only the folder this host
//!   created, after strict path checks: canonical, a direct child of the
//!   canonical scratch root, not a symlink, and carrying this entry's own
//!   marker in its git dir.
//! * **Limits** ([`reserve`]): at most [`DEFAULT_MAX_PER_HOST`] live
//!   scratch repositories per host and [`DEFAULT_MAX_PER_MEMBER`] per
//!   member (by account id), so a phone can't fill the disk.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::event_store::EventStore;

/// Days without activity before an unkept scratch repository is removed.
pub const DEFAULT_CLEANUP_DAYS: u32 = 14;
/// The most a host may set.
pub const MAX_CLEANUP_DAYS: u32 = 365;
/// Days of notice in the chat before removal.
pub const NOTICE_DAYS: u32 = 2;
/// Live scratch repositories per host (default).
pub const DEFAULT_MAX_PER_HOST: usize = 16;
/// Live scratch repositories per member on one host (default).
pub const DEFAULT_MAX_PER_MEMBER: usize = 8;
/// The most a host may allow, whatever its settings say.
pub const HARD_MAX_PER_HOST: usize = 64;
/// Registry entries kept at most (removed and kept ones age out first).
const MAX_ENTRIES: usize = 256;
/// The marker this host writes in a scratch repository's git dir.
pub const MARKER: &str = "ligus-scratch";

const DAY: i64 = 86_400;

const KEY_REGISTRY: &str = "scratch:registry";
const KEY_ROOT: &str = "scratch:root";
const KEY_DAYS: &str = "scratch:cleanupDays";
const KEY_MAX_HOST: &str = "scratch:maxPerHost";
const KEY_MAX_MEMBER: &str = "scratch:maxPerMember";

/// Where a scratch repository is in its life.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScratchState {
    #[default]
    Live,
    /// Kept: moved out of the scratch root; an ordinary repository now.
    Kept,
    /// Removed by the cleanup (or a person); the folder is gone.
    Removed,
}

/// One scratch repository this host created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScratchRepo {
    pub id: Uuid,
    pub workspace_id: Uuid,
    /// The display name.
    pub name: String,
    /// `<slug>-<first 8 hex of id>`: the folder's name under the root.
    pub dir_name: String,
    /// The canonical folder (under the root while live; the kept folder
    /// after a keep).
    pub path: PathBuf,
    /// The canonical scratch root it was created under.
    pub root: PathBuf,
    /// The empty first commit (the repository's `local:` identity).
    pub root_commit: String,
    pub created_at: i64,
    pub last_activity: i64,
    /// Who asked for it: their account id (`""` when unknown).
    #[serde(default)]
    pub created_by: String,
    /// The workspace repository it is registered as, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_id: Option<Uuid>,
    /// The chats that work in it on this host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chats: Vec<Uuid>,
    #[serde(default)]
    pub state: ScratchState,
    /// When the notice went to its chats (once per quiet spell).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub noticed_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_at: Option<i64>,
    /// The web URL it was published to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
}

impl ScratchRepo {
    /// The first 8 hex of its id.
    pub fn short_id(&self) -> String {
        self.id.simple().to_string()[..8].to_string()
    }
    pub fn is_live(&self) -> bool {
        self.state == ScratchState::Live
    }
}

/// Why a scratch operation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScratchError {
    /// Too many live scratch repositories (on this host, or for this member).
    Limit(String),
    /// A path check failed (outside the root, a symlink, not ours, …).
    Path(String),
    /// Not a known scratch repository (or not live).
    NotFound,
    Git(String),
    Io(String),
    Store(String),
    Invalid(String),
}

impl std::fmt::Display for ScratchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScratchError::Limit(m) | ScratchError::Path(m) | ScratchError::Invalid(m) => f.write_str(m),
            ScratchError::NotFound => f.write_str("no such scratch repository on this host"),
            ScratchError::Git(m) => write!(f, "git: {m}"),
            ScratchError::Io(m) => write!(f, "couldn't change the folder: {m}"),
            ScratchError::Store(m) => write!(f, "couldn't record it: {m}"),
        }
    }
}

impl std::error::Error for ScratchError {}

impl ScratchError {
    /// A stable code for APIs.
    pub fn code(&self) -> &'static str {
        match self {
            ScratchError::Limit(_) => "scratch_limit",
            ScratchError::Path(_) => "scratch_path",
            ScratchError::NotFound => "not_found",
            ScratchError::Git(_) => "git",
            ScratchError::Io(_) => "io",
            ScratchError::Store(_) => "store",
            ScratchError::Invalid(_) => "invalid",
        }
    }
}

type Result<T> = std::result::Result<T, ScratchError>;

fn store_err(e: impl std::fmt::Display) -> ScratchError {
    ScratchError::Store(e.to_string())
}

fn io_err(e: impl std::fmt::Display) -> ScratchError {
    ScratchError::Io(e.to_string())
}

// ── Settings ─────────────────────────────────────────────────────────────

/// This host's scratch settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// Where new scratch repositories go (not yet canonical: it may not exist).
    pub root: PathBuf,
    /// Whether `root` is this host's default.
    pub root_is_default: bool,
    /// Days without activity before removal; `None` = cleanup off.
    pub cleanup_days: Option<u32>,
    pub max_per_host: usize,
    pub max_per_member: usize,
}

/// This host's settings, with `default_root` (`<data dir>/scratch`) when
/// none is set.
pub fn settings(store: &EventStore, default_root: &Path) -> Settings {
    let get = |k: &str| store.repo_setting(k).ok().flatten();
    let root = get(KEY_ROOT).filter(|r| !r.trim().is_empty()).map(PathBuf::from);
    let cleanup_days = match get(KEY_DAYS).and_then(|d| d.trim().parse::<u32>().ok()) {
        Some(0) => None,
        Some(d) => Some(d.min(MAX_CLEANUP_DAYS)),
        None => Some(DEFAULT_CLEANUP_DAYS),
    };
    let num = |k: &str, d: usize| get(k).and_then(|v| v.trim().parse::<usize>().ok()).map(|v| v.clamp(1, HARD_MAX_PER_HOST)).unwrap_or(d);
    Settings {
        root_is_default: root.is_none(),
        root: root.unwrap_or_else(|| default_root.to_path_buf()),
        cleanup_days,
        max_per_host: num(KEY_MAX_HOST, DEFAULT_MAX_PER_HOST),
        max_per_member: num(KEY_MAX_MEMBER, DEFAULT_MAX_PER_MEMBER),
    }
}

/// Set (or with `None` reset) this host's scratch root. The root is checked
/// ([`check_root`]) before it is stored.
pub fn set_root(store: &EventStore, root: Option<&Path>, forbidden: &[PathBuf]) -> Result<Option<PathBuf>> {
    match root {
        None => {
            store.set_repo_setting(KEY_ROOT, None).map_err(store_err)?;
            Ok(None)
        }
        Some(r) => {
            let c = check_root(r, forbidden)?;
            store.set_repo_setting(KEY_ROOT, Some(&c.to_string_lossy())).map_err(store_err)?;
            Ok(Some(c))
        }
    }
}

/// Set the cleanup: `Some(days)` (1 to 365), `Some(0)` off, `None` back to
/// the default.
pub fn set_cleanup_days(store: &EventStore, days: Option<u32>) -> Result<()> {
    if days.is_some_and(|d| d > MAX_CLEANUP_DAYS) {
        return Err(ScratchError::Invalid(format!("at most {MAX_CLEANUP_DAYS} days (0 turns the cleanup off)")));
    }
    let v = days.map(|d| d.to_string());
    store.set_repo_setting(KEY_DAYS, v.as_deref()).map_err(store_err)
}

/// Set the per-host and per-member limits (1 to [`HARD_MAX_PER_HOST`];
/// `None` keeps the default).
pub fn set_limits(store: &EventStore, per_host: Option<usize>, per_member: Option<usize>) -> Result<()> {
    for (k, v) in [(KEY_MAX_HOST, per_host), (KEY_MAX_MEMBER, per_member)] {
        if v.is_some_and(|n| n == 0 || n > HARD_MAX_PER_HOST) {
            return Err(ScratchError::Invalid(format!("a limit is 1 to {HARD_MAX_PER_HOST}")));
        }
        let s = v.map(|n| n.to_string());
        store.set_repo_setting(k, s.as_deref()).map_err(store_err)?;
    }
    Ok(())
}

fn inside(path: &Path, dir: &Path) -> bool {
    path == dir || path.starts_with(dir)
}

/// The first ancestor of `p` (itself included) that is a git checkout's
/// work tree or git dir.
fn enclosing_repo(p: &Path) -> Option<PathBuf> {
    p.ancestors().find(|a| a.join(".git").exists() || (a.join("HEAD").is_file() && a.join("objects").is_dir() && a.join("refs").is_dir())).map(Path::to_path_buf)
}

/// Create (if needed) and check a scratch root: canonical, a directory and
/// not a symlink, not a filesystem root, never inside a git checkout, and
/// never inside one of `forbidden` (for a box: the desktop app's data dir).
pub fn check_root(root: &Path, forbidden: &[PathBuf]) -> Result<PathBuf> {
    if root.as_os_str().is_empty() || !root.is_absolute() {
        return Err(ScratchError::Path("the scratch root must be an absolute path".into()));
    }
    if std::fs::symlink_metadata(root).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(ScratchError::Path("the scratch root can't be a symlink".into()));
    }
    std::fs::create_dir_all(root).map_err(io_err)?;
    let c = super::git::canonical(root);
    if !c.is_dir() {
        return Err(ScratchError::Path("the scratch root isn't a folder".into()));
    }
    if c.parent().is_none() {
        return Err(ScratchError::Path("the scratch root can't be a filesystem root".into()));
    }
    if let Some(r) = enclosing_repo(&c) {
        return Err(ScratchError::Path(format!("the scratch root can't be inside another repository ({})", r.display())));
    }
    for f in forbidden {
        let fc = super::git::canonical(f);
        if inside(&c, &fc) || inside(&fc, &c) {
            return Err(ScratchError::Path("the scratch root can't be inside (or contain) the desktop app's data folder".into()));
        }
    }
    Ok(c)
}

// ── The registry ─────────────────────────────────────────────────────────

fn load_all(store: &EventStore) -> Vec<ScratchRepo> {
    store.repo_setting(KEY_REGISTRY).ok().flatten().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_all(store: &EventStore, mut all: Vec<ScratchRepo>) -> Result<()> {
    if all.len() > MAX_ENTRIES {
        // Forget the oldest finished ones first; live ones always stay.
        all.sort_by_key(|e| (e.is_live(), e.removed_at.unwrap_or(e.last_activity)));
        let drop = all.len() - MAX_ENTRIES;
        let mut n = 0;
        all.retain(|e| {
            if n < drop && !e.is_live() {
                n += 1;
                false
            } else {
                true
            }
        });
        all.sort_by_key(|e| e.created_at);
    }
    let text = serde_json::to_string(&all).map_err(store_err)?;
    store.set_repo_setting(KEY_REGISTRY, Some(&text)).map_err(store_err)
}

fn update(store: &EventStore, id: Uuid, f: impl FnOnce(&mut ScratchRepo)) -> Result<ScratchRepo> {
    let mut all = load_all(store);
    let e = all.iter_mut().find(|e| e.id == id).ok_or(ScratchError::NotFound)?;
    f(e);
    let out = e.clone();
    save_all(store, all)?;
    Ok(out)
}

/// Every scratch repository this host created in `workspace_id`, oldest
/// first (kept and removed ones too).
pub fn list(store: &EventStore, workspace_id: Uuid) -> Vec<ScratchRepo> {
    load_all(store).into_iter().filter(|e| e.workspace_id == workspace_id).collect()
}

/// The live ones.
pub fn live(store: &EventStore, workspace_id: Uuid) -> Vec<ScratchRepo> {
    list(store, workspace_id).into_iter().filter(ScratchRepo::is_live).collect()
}

/// One by id, id prefix (at least 4 hex), folder name or display name.
pub fn find(store: &EventStore, workspace_id: Uuid, sel: &str) -> Option<ScratchRepo> {
    let sel = sel.trim();
    let lower = sel.to_ascii_lowercase();
    let all = list(store, workspace_id);
    let hits: Vec<&ScratchRepo> = all
        .iter()
        .filter(|e| {
            (lower.len() >= 4 && e.id.simple().to_string().starts_with(&lower.replace('-', "")))
                || e.id.to_string() == lower
                || e.dir_name == sel
                || e.name.eq_ignore_ascii_case(sel)
        })
        .collect();
    // Prefer a live one when names repeat.
    hits.iter().find(|e| e.is_live()).or(hits.first()).map(|e| (*e).clone())
}

/// The live scratch repository at `path` (compared canonically).
pub fn by_path(store: &EventStore, path: &Path) -> Option<ScratchRepo> {
    let c = super::git::canonical(path);
    load_all(store).into_iter().find(|e| e.is_live() && e.path == c)
}

/// The live scratch repository registered as workspace repo `repo_id`.
pub fn by_repo(store: &EventStore, repo_id: Uuid) -> Option<ScratchRepo> {
    load_all(store).into_iter().find(|e| e.is_live() && e.repo_id == Some(repo_id))
}

/// Record activity in the scratch repository at `path` (a turn ran in it).
/// `false` when `path` isn't one.
pub fn touch(store: &EventStore, path: &Path, now: i64) -> bool {
    let Some(e) = by_path(store, path) else { return false };
    update(store, e.id, |e| {
        e.last_activity = e.last_activity.max(now);
        e.noticed_at = None;
    })
    .is_ok()
}

/// Note that `chat` works in it (or, with `on` false, no longer does).
pub fn set_chat(store: &EventStore, id: Uuid, chat: Uuid, on: bool, now: i64) -> Result<ScratchRepo> {
    update(store, id, |e| {
        e.chats.retain(|c| *c != chat);
        if on {
            e.chats.push(chat);
            e.last_activity = e.last_activity.max(now);
            e.noticed_at = None;
        }
    })
}

/// Drop `chat` from every scratch repository's chats here.
pub fn forget_chat(store: &EventStore, chat: Uuid) -> Result<()> {
    let mut all = load_all(store);
    let mut changed = false;
    for e in &mut all {
        let n = e.chats.len();
        e.chats.retain(|c| *c != chat);
        changed |= n != e.chats.len();
    }
    if changed {
        save_all(store, all)?;
    }
    Ok(())
}

/// Note the workspace repository it was registered as.
pub fn set_repo_id(store: &EventStore, id: Uuid, repo_id: Option<Uuid>) -> Result<ScratchRepo> {
    update(store, id, |e| e.repo_id = repo_id)
}

/// Note where it was published.
pub fn set_published(store: &EventStore, id: Uuid, url: &str) -> Result<ScratchRepo> {
    update(store, id, |e| e.published = Some(url.to_string()))
}

// ── Creating ─────────────────────────────────────────────────────────────

/// The folder-name slug of a display name (`[a-z0-9-]`, at most 40).
pub fn slug(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let s = s.split('-').filter(|x| !x.is_empty()).collect::<Vec<_>>().join("-");
    let s: String = s.chars().take(40).collect();
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() {
        "scratch".into()
    } else {
        s
    }
}

/// Whether one more live scratch repository may be created here for
/// `created_by` (an account id; `""` counts only against the host).
pub fn reserve(store: &EventStore, settings: &Settings, created_by: &str) -> Result<()> {
    let all: Vec<ScratchRepo> = load_all(store).into_iter().filter(ScratchRepo::is_live).collect();
    let host_max = settings.max_per_host.min(HARD_MAX_PER_HOST);
    if all.len() >= host_max {
        return Err(ScratchError::Limit(format!(
            "this host already has {host_max} scratch repositories; keep or remove one first (ligus repo scratch --list)"
        )));
    }
    if !created_by.is_empty() && all.iter().filter(|e| e.created_by == created_by).count() >= settings.max_per_member {
        return Err(ScratchError::Limit(format!("you already have {} scratch repositories on this host; keep or remove one first", settings.max_per_member)));
    }
    Ok(())
}

fn git_in(dir: &Path, args: &[&str]) -> Result<String> {
    super::git::git(dir, args).map_err(ScratchError::Git)
}

fn write_marker(dir: &Path, id: Uuid) -> Result<()> {
    let common = super::git::git_common_dir(dir).ok_or_else(|| ScratchError::Git("no git dir".into()))?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    use std::io::Write;
    let mut f = opts.open(common.join(MARKER)).map_err(io_err)?;
    f.write_all(format!("{id}\n").as_bytes()).map_err(io_err)?;
    f.sync_all().map_err(io_err)
}

/// Whether the repository at `dir` carries the marker of scratch repo `id`
/// (in its git common dir: a commit can't supply it).
pub fn has_marker(dir: &Path, id: Uuid) -> bool {
    let Some(common) = super::git::git_common_dir(dir) else { return false };
    // The git dir must be this folder's own `.git`, not one it points at.
    if common != super::git::canonical(&dir.join(".git")) {
        return false;
    }
    std::fs::read_to_string(common.join(MARKER)).ok().and_then(|t| t.lines().next().map(|l| l.trim().to_string())).is_some_and(|l| l == id.to_string())
}

/// Create an empty repository for a new scratch entry under `root` (already
/// checked with [`check_root`]): `git init -b main`, one empty commit, the
/// marker. Filesystem only (nothing recorded): call [`record`] next, or
/// [`discard_dir`] if that fails.
pub fn init_dir(root: &Path, workspace_id: Uuid, name: &str, created_by: &str, now: i64) -> Result<ScratchRepo> {
    let name = name.trim();
    if name.chars().count() > ligus_core::workspace_repo::MAX_DISPLAY_NAME || (!name.is_empty() && !ligus_core::workspace_repo::display_name_ok(name)) {
        return Err(ScratchError::Invalid("bad scratch repository name".into()));
    }
    let root = super::git::canonical(root);
    let id = Uuid::new_v4();
    let short = id.simple().to_string()[..8].to_string();
    let dir_name = format!("{}-{short}", slug(name));
    let display = if name.is_empty() { dir_name.clone() } else { name.to_string() };
    let path = root.join(&dir_name);
    // `create_dir`, never `_all`: a folder that exists (or a symlink planted
    // there) is refused, never reused.
    std::fs::create_dir(&path).map_err(io_err)?;
    let made = (|| -> Result<String> {
        git_in(&path, &["init", "--quiet", "--initial-branch=main"])?;
        // The scratch id in the message keeps two scratch repos made in the
        // same second from sharing a root commit (their `local:` identity).
        let msg = format!("Start scratch repository {display} ({id})");
        git_in(&path, &["-c", "user.name=Ligus", "-c", "user.email=ligus@localhost", "-c", "commit.gpgsign=false", "commit", "--quiet", "--allow-empty", "-m", &msg])?;
        write_marker(&path, id)?;
        super::git::root_commit(&path).ok_or_else(|| ScratchError::Git("no root commit".into()))
    })();
    let root_commit = match made {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&path);
            return Err(e);
        }
    };
    Ok(ScratchRepo {
        id,
        workspace_id,
        name: display,
        dir_name,
        path: super::git::canonical(&path),
        root,
        root_commit,
        created_at: now,
        last_activity: now,
        created_by: created_by.to_string(),
        repo_id: None,
        chats: Vec::new(),
        state: ScratchState::Live,
        noticed_at: None,
        removed_at: None,
        published: None,
    })
}

/// Record a scratch entry made by [`init_dir`], re-checking the limits
/// (another request may have raced this one).
pub fn record(store: &EventStore, settings: &Settings, entry: &ScratchRepo) -> Result<()> {
    reserve(store, settings, &entry.created_by)?;
    let mut all = load_all(store);
    all.push(entry.clone());
    save_all(store, all)
}

/// Remove the folder of a scratch entry that was never recorded.
pub fn discard_dir(entry: &ScratchRepo) {
    if entry.path.parent() == Some(entry.root.as_path()) && has_marker(&entry.path, entry.id) {
        let _ = std::fs::remove_dir_all(&entry.path);
    }
}

/// [`reserve`], [`init_dir`] and [`record`] in one (for callers that hold
/// the store anyway; git runs inside).
pub fn create(store: &EventStore, settings: &Settings, forbidden: &[PathBuf], workspace_id: Uuid, name: &str, created_by: &str, now: i64) -> Result<ScratchRepo> {
    reserve(store, settings, created_by)?;
    let root = check_root(&settings.root, forbidden)?;
    let e = init_dir(&root, workspace_id, name, created_by, now)?;
    if let Err(err) = record(store, settings, &e) {
        discard_dir(&e);
        return Err(err);
    }
    Ok(e)
}

// ── Path checks (removal and keep) ───────────────────────────────────────

/// Every check before this host touches a live scratch entry's folder:
/// the folder is not a symlink, is a direct child of the canonical scratch
/// root it was created under, canonicalizes to the recorded path, has the
/// recorded name, and carries this entry's marker in its own git dir.
pub fn verify_dir(entry: &ScratchRepo) -> Result<PathBuf> {
    let bad = |m: &str| Err(ScratchError::Path(format!("refused to touch {}: {m}", entry.dir_name)));
    if !entry.is_live() {
        return Err(ScratchError::NotFound);
    }
    let valid_name = !entry.dir_name.is_empty()
        && entry.dir_name != "."
        && entry.dir_name != ".."
        && entry.dir_name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !valid_name {
        return bad("not a scratch folder name");
    }
    let root = super::git::canonical(&entry.root);
    if root != entry.root {
        return bad("the scratch root moved");
    }
    let expected = root.join(&entry.dir_name);
    match std::fs::symlink_metadata(&expected) {
        Ok(m) if m.file_type().is_symlink() => return bad("it is a symlink"),
        Ok(m) if !m.is_dir() => return bad("it isn't a folder"),
        Ok(_) => {}
        Err(_) => return bad("it is gone"),
    }
    let c = super::git::canonical(&expected);
    if c != expected || c != entry.path || c.parent() != Some(root.as_path()) {
        return bad("it isn't directly under the scratch root");
    }
    if std::fs::symlink_metadata(c.join(".git")).map(|m| !m.is_dir()).unwrap_or(true) {
        return bad("its .git isn't a folder of its own");
    }
    if !has_marker(&c, entry.id) {
        return bad("it doesn't carry this host's scratch marker");
    }
    Ok(c)
}

/// Delete a live scratch entry's folder after [`verify_dir`]. It is first
/// renamed to a hidden name in the same root (so a half-finished removal
/// never leaves a usable checkout), then removed without following any
/// symlink inside. Filesystem only: [`mark_removed`] records it.
pub fn remove_dir_checked(entry: &ScratchRepo) -> Result<()> {
    let dir = verify_dir(entry)?;
    let trash = entry.root.join(format!(".removing-{}", entry.id.simple()));
    std::fs::rename(&dir, &trash).map_err(io_err)?;
    std::fs::remove_dir_all(&trash).map_err(io_err)
}

/// Record that a scratch entry's folder is gone; its chats are returned
/// (they have no repository on this host now).
pub fn mark_removed(store: &EventStore, id: Uuid, now: i64) -> Result<ScratchRepo> {
    update(store, id, |e| {
        e.state = ScratchState::Removed;
        e.removed_at = Some(now);
    })
}

/// Check a destination for [`keep_dir`]: absolute, not there yet, its
/// parent an existing folder, outside the scratch root, not inside another
/// repository, not inside one of `forbidden`.
pub fn check_keep_dest(entry: &ScratchRepo, dest: &Path, forbidden: &[PathBuf]) -> Result<PathBuf> {
    if !dest.is_absolute() {
        return Err(ScratchError::Path("keep it in an absolute path".into()));
    }
    if std::fs::symlink_metadata(dest).is_ok() {
        return Err(ScratchError::Path(format!("{} already exists", dest.display())));
    }
    let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if name.is_empty() || name == "." || name == ".." || name.starts_with('.') {
        return Err(ScratchError::Path("pick a folder name".into()));
    }
    let parent = dest.parent().ok_or_else(|| ScratchError::Path("pick a folder".into()))?;
    if !parent.is_dir() {
        return Err(ScratchError::Path(format!("{} isn't a folder", parent.display())));
    }
    let parent = super::git::canonical(parent);
    let target = parent.join(&name);
    if inside(&target, &super::git::canonical(&entry.root)) {
        return Err(ScratchError::Path("keep it outside the scratch folder".into()));
    }
    if let Some(r) = enclosing_repo(&parent) {
        return Err(ScratchError::Path(format!("keep it outside other repositories ({})", r.display())));
    }
    for f in forbidden {
        if inside(&target, &super::git::canonical(f)) {
            return Err(ScratchError::Path("keep it outside the desktop app's data folder".into()));
        }
    }
    Ok(target)
}

/// Move a live scratch repository out of the scratch root to `dest`
/// (checked with [`check_keep_dest`]) and repair its worktrees' links.
/// The same disk only (a rename). Filesystem only: [`mark_kept`] records it.
pub fn keep_dir(entry: &ScratchRepo, dest: &Path, forbidden: &[PathBuf]) -> Result<PathBuf> {
    let src = verify_dir(entry)?;
    let dest = check_keep_dest(entry, dest, forbidden)?;
    // The worktrees under the checkout (the chats' and turns') move with it.
    let worktrees: Vec<PathBuf> = git_in(&src, &["worktree", "list", "--porcelain"])
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(PathBuf::from)
        .filter(|p| p != &src && p.starts_with(&src))
        .collect();
    let _lock = super::marker::lock(&src, std::time::Duration::from_secs(30)).map_err(|e| ScratchError::Io(e.to_string()))?;
    std::fs::rename(&src, &dest).map_err(|e| {
        if e.raw_os_error() == Some(18) {
            ScratchError::Io("that folder is on another disk; pick one on the same disk as the scratch folder".into())
        } else {
            io_err(e)
        }
    })?;
    let moved: Vec<String> = worktrees.iter().filter_map(|w| w.strip_prefix(&src).ok()).map(|rel| dest.join(rel).to_string_lossy().into_owned()).collect();
    if !moved.is_empty() {
        let mut args = vec!["worktree", "repair"];
        args.extend(moved.iter().map(String::as_str));
        let _ = git_in(&dest, &args);
    }
    let marker = super::git::git_common_dir(&dest).map(|c| c.join(MARKER));
    if let Some(m) = marker {
        let _ = std::fs::remove_file(m);
    }
    Ok(super::git::canonical(&dest))
}

/// Record a keep: an ordinary repository at `path` now.
pub fn mark_kept(store: &EventStore, id: Uuid, path: &Path) -> Result<ScratchRepo> {
    update(store, id, |e| {
        e.state = ScratchState::Kept;
        e.path = path.to_path_buf();
    })
}

// ── Cleanup ──────────────────────────────────────────────────────────────

fn mtime(p: &Path) -> Option<i64> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    m.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)
}

/// The last activity of a live scratch repository: the latest of what this
/// host recorded (turns, binding) and its git dir's own traces (a commit,
/// a branch moved, a worktree added), never later than `now`.
pub fn activity(entry: &ScratchRepo, now: i64) -> i64 {
    let git = entry.path.join(".git");
    let traces = [git.join("HEAD"), git.join("index"), git.join("logs").join("HEAD"), git.join("refs").join("heads"), git.join("worktrees")];
    traces.iter().filter_map(|p| mtime(p)).fold(entry.last_activity, i64::max).min(now)
}

/// When a live scratch repository will be removed, and when its notice
/// starts (`None` when the cleanup is off).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Expiry {
    pub remove_at: i64,
    pub notice_from: i64,
    /// `now` is past `notice_from`: show "This scratch repo will be removed on …".
    pub notice: bool,
}

/// [`Expiry`] of a live entry under `settings` at `now`.
pub fn expiry(entry: &ScratchRepo, settings: &Settings, now: i64) -> Option<Expiry> {
    let days = settings.cleanup_days?;
    if !entry.is_live() {
        return None;
    }
    let remove_at = activity(entry, now) + i64::from(days) * DAY;
    let notice_from = remove_at - i64::from(NOTICE_DAYS.min(days)) * DAY;
    Some(Expiry { remove_at, notice_from, notice: now >= notice_from })
}

/// What the cleanup should do now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Due {
    /// Entered the notice window and not noticed yet: post the notice in
    /// each chat, then [`mark_noticed`].
    pub notice: Vec<(ScratchRepo, Expiry)>,
    /// Past `remove_at`: [`remove_dir_checked`], [`mark_removed`], then
    /// leave each chat with no repository.
    pub remove: Vec<ScratchRepo>,
}

/// The live entries in `workspace_id` that need a notice or a removal.
pub fn due(store: &EventStore, workspace_id: Uuid, settings: &Settings, now: i64) -> Due {
    let mut out = Due::default();
    for e in live(store, workspace_id) {
        let Some(x) = expiry(&e, settings, now) else { continue };
        if now >= x.remove_at {
            out.remove.push(e);
        } else if x.notice && e.noticed_at.is_none() {
            out.notice.push((e, x));
        }
    }
    out
}

pub fn mark_noticed(store: &EventStore, id: Uuid, now: i64) -> Result<ScratchRepo> {
    update(store, id, |e| e.noticed_at = Some(now))
}

/// The notice its chats get [`NOTICE_DAYS`] days before removal ("This
/// scratch repo will be removed on <date> · Keep it").
pub fn notice_text(entry: &ScratchRepo, x: &Expiry, host_label: &str) -> String {
    format!(
        "This scratch repo ({}) on {host_label} will be removed on {} after a quiet spell. Keep it as a repo to hold on to it.",
        entry.name,
        date(x.remove_at),
    )
}

/// "The scratch repository … was removed": what its chats are told.
pub fn removed_text(entry: &ScratchRepo, host_label: &str) -> String {
    format!(
        "The scratch repository {} was removed from {host_label} after a quiet spell. This chat has no repository now; its proposals and diffs stay in the chat.",
        entry.name
    )
}

/// `2026-12-09` for a unix time.
pub fn date(unix: i64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(unix).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!("{:04}-{:02}-{:02}", t.year(), u8::from(t.month()), t.day())
}

/// Seconds since the epoch now.
pub fn now() -> i64 {
    ligus_core::Timestamp::now().inner().unix_timestamp()
}

#[cfg(test)]
#[path = "scratch_tests.rs"]
mod tests;

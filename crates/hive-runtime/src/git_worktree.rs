//! Isolated git worktrees for concurrent agent turns.
//!
//! A file-mutating subprocess agent (claude-code / aider / pi) normally runs in
//! the shared workspace root, so two turns editing files at once clobber each
//! other's uncommitted work. This module gives each such turn its own worktree
//! on a throwaway task branch cut from the current `HEAD`: the agent edits in
//! isolation, and the resulting diff surfaces for review and merges back on
//! approval (the review-gated model). Turns that don't run in a git repo — or
//! don't mutate files — skip this entirely and run in place.
//!
//! Worktrees live under `<repo>/.hive/worktrees/<turn-id>` and are registered in
//! `.git/worktrees`, so git excludes them from the main tree's status. They are
//! removed when the turn's changes are merged or discarded.

use std::io;
use std::path::{Path, PathBuf};
// Spawns go through `process_util::command` so the ~dozen `git` invocations an
// isolated turn makes don't each flash a console window on Windows.
#[cfg(test)]
use hive_core::process_util::command;

// ---------------------------------------------------------------------------
// In-use turn worktrees: from the moment a turn's worktree is cut (or a parked
// one is claimed for reuse) until it is parked again or removed. The hygiene
// sweep never touches one of these, so a turn that just stopped streaming but
// hasn't parked its tree yet isn't mistaken for a crash leftover. Process-wide;
// the sweep runs in the same process as the turns it protects.
// ---------------------------------------------------------------------------

fn in_use_set() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
    static IN_USE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> = std::sync::OnceLock::new();
    IN_USE.get_or_init(Default::default)
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn mark_in_use(path: &Path) {
    in_use_set().lock().unwrap_or_else(|e| e.into_inner()).insert(canonical(path));
}

/// Release an in-use mark. `key` is from [`in_use_key`], taken while the
/// worktree was still at its path (a moved or removed tree can't be
/// canonicalized any more).
fn unmark_in_use(key: &Path) {
    in_use_set().lock().unwrap_or_else(|e| e.into_inner()).remove(key);
}

fn in_use_key(path: &Path) -> PathBuf {
    canonical(path)
}

/// Whether `path` is a turn worktree a turn in this process is using right
/// now (see the hygiene sweep).
pub fn is_in_use(path: &Path) -> bool {
    let set = in_use_set().lock().unwrap_or_else(|e| e.into_inner());
    set.contains(&canonical(path)) || set.contains(path)
}

/// A live isolated worktree for one turn. Drop does NOT auto-remove — call
/// [`Worktree::remove`] once the turn's diff has been captured (merged or
/// discarded), so a captured-but-unmerged diff isn't lost by an early drop.
#[derive(Debug, Clone)]
pub struct Worktree {
    /// Absolute path to the checked-out worktree (the agent's working dir).
    pub path: PathBuf,
    /// The task branch, e.g. `hive/turn-<id>`.
    pub branch: String,
    /// The repository the worktree belongs to (the main workspace root).
    repo_root: PathBuf,
}

/// Whether `root` is inside a git working tree. Isolation only applies to repos.
pub fn is_git_repo(root: &Path) -> bool {
    hive_core::process_util::git()
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success() && o.stdout.starts_with(b"true"))
        .unwrap_or(false)
}

fn git(dir: &Path, args: &[&str]) -> io::Result<String> {
    let out = hive_core::process_util::git().arg("-C").arg(dir).args(args).output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A filesystem-safe branch/worktree slug for a turn id.
fn slug(turn_id: &str) -> String {
    turn_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect()
}

/// Whether a local branch of exactly this name exists in `repo_root`. Used to
/// make [`chat_worktree`] idempotent (reuse an existing chat branch) without
/// treating a plain `git` failure as "exists".
fn branch_exists(repo_root: &Path, branch: &str) -> bool {
    hive_core::process_util::git()
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The branch name backing a chat's persistent worktree.
fn chat_branch(chat_id: &str) -> String {
    format!("hive/chat/{}", slug(chat_id))
}

/// The checkout path of a chat's persistent worktree.
fn chat_worktree_path(repo_root: &Path, chat_id: &str) -> PathBuf {
    // An existing checkout keeps the path git recorded (`.hive/worktrees/…`);
    // new ones go under the current workspace state dir.
    hive_core::compat::workspace_state_path(repo_root, format!("worktrees/chat-{}", slug(chat_id)))
}

/// Ensure a chat's **persistent** base worktree exists and return its path.
///
/// Each chat gets a base branch `hive/chat/<chatid>` and a long-lived checkout
/// at `<repo>/.hive/worktrees/chat-<chatid>` that is the chat's effective code
/// dir. Idempotent: if the branch already exists it is reused (never removed
/// between turns); a missing checkout for an existing branch is re-added. A
/// fresh chat cuts the branch off `HEAD`.
///
/// Assumes `repo_root` is a git repo with a commit (guard with [`is_git_repo`]
/// first). A repo with no `HEAD` has nothing to branch from and returns `Err` —
/// callers treat that as "run in the workspace root", exactly as for [`create`].
pub fn chat_worktree(repo_root: &Path, chat_id: &str) -> io::Result<PathBuf> {
    let branch = chat_branch(chat_id);
    let path = chat_worktree_path(repo_root, chat_id);
    let path_str = path.to_string_lossy().into_owned();
    let existed = branch_exists(repo_root, &branch);

    // A registered checkout that's already present for this branch is reused
    // as-is — the whole point of a persistent worktree is to survive turns.
    if existed && path.exists() {
        return Ok(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Clear any stale worktree registration at this path (e.g. the checkout dir
    // was deleted out from under git) so `worktree add` doesn't refuse.
    let _ = hive_core::process_util::git()
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "remove", "--force"])
        .arg(&path)
        .output();
    if existed {
        // Branch survives; re-attach a checkout for it without moving its tip.
        git(repo_root, &["worktree", "add", &path_str, &branch])?;
    } else {
        // Fresh chat: cut the base branch off the configured base, else HEAD.
        let base = chat_base(repo_root);
        git(repo_root, &["worktree", "add", "-b", &branch, &path_str, &base])?;
    }
    Ok(path)
}

/// The git config key naming the ref a fresh chat branch is cut from (e.g.
/// `refs/remotes/upstream/master` in a fork checkout, so chats start from the
/// upstream project rather than the fork's possibly stale copy).
pub const CHAT_BASE_CONFIG: &str = "hive.chatBase";

/// Where a fresh chat branch starts: `hive.chatBase` when it's set and names a
/// commit, else `HEAD`.
pub fn chat_base(repo_root: &Path) -> String {
    let configured = hive_core::process_util::git()
        .arg("-C")
        .arg(repo_root)
        .args(["config", "--get", CHAT_BASE_CONFIG])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|b| !b.is_empty() && !b.starts_with('-'));
    match configured {
        Some(b) if git(repo_root, &["rev-parse", "--verify", "--quiet", &format!("{b}^{{commit}}")]).is_ok() => b,
        _ => "HEAD".to_string(),
    }
}

/// Best-effort teardown of a chat's persistent worktree and its base branch.
/// Called when a chat is deleted. A failure just leaves a stale entry that a
/// later same-id [`chat_worktree`] clears anyway.
pub fn remove_chat_worktree(repo_root: &Path, chat_id: &str) -> io::Result<()> {
    let branch = chat_branch(chat_id);
    let path = chat_worktree_path(repo_root, chat_id);
    let path_str = path.to_string_lossy().into_owned();
    let _ = git(repo_root, &["worktree", "remove", "--force", &path_str]);
    let _ = git(repo_root, &["branch", "-D", &branch]);
    // And any turn worktrees parked for reuse.
    for parked in pooled(repo_root, chat_id) {
        let _ = git(repo_root, &["worktree", "remove", "--force", &parked.to_string_lossy()]);
    }
    Ok(())
}

impl Worktree {
    /// Create an isolated worktree for `turn_id`: a fresh branch `hive/turn-<id>`
    /// off the current `HEAD`, checked out at `<repo>/.hive/worktrees/<id>`.
    ///
    /// Assumes `repo_root` is a git repo (guard with [`is_git_repo`] first). A
    /// repo with no commits yet has no `HEAD` to branch from — callers should
    /// treat that as "run in place" rather than isolate.
    pub fn create(repo_root: &Path, turn_id: &str) -> io::Result<Worktree> {
        Self::create_from(repo_root, turn_id, "HEAD")
    }

    /// Like [`create`], but cuts the ephemeral turn branch from an arbitrary
    /// `base_ref` (a commit, branch, or `HEAD`) instead of always `HEAD`. This
    /// is how a turn isolates from its chat's base branch (`hive/chat/<id>`)
    /// rather than the global HEAD, so the captured diff is vs the chat branch.
    /// If `base_ref` doesn't resolve, `worktree add` fails and the caller falls
    /// back (to HEAD, then in-place).
    pub fn create_from(repo_root: &Path, turn_id: &str, base_ref: &str) -> io::Result<Worktree> {
        let slug = slug(turn_id);
        let branch = format!("hive/turn-{slug}");
        let path = hive_core::compat::workspace_state_path(repo_root, Path::new("worktrees").join(&slug));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // A stale worktree/branch from a crashed prior turn with the same id
        // would block creation — best-effort clear it first. Turn ids are
        // unique per turn, so this only ever meets a leftover, never a live
        // turn's tree; say so when it throws edits away.
        if path.exists() {
            let dirty = git(&path, &["status", "--porcelain"]).map(|s| !s.trim().is_empty()).unwrap_or(false);
            tracing::warn!(target: "worktree", path = %path.display(), dirty, "clearing a stale turn worktree");
        }
        let _ = hive_core::process_util::git()
            .arg("-C")
            .arg(repo_root)
            .args(["worktree", "remove", "--force"])
            .arg(&path)
            .output();
        let _ = git(repo_root, &["branch", "-D", &branch]);

        let path_str = path.to_string_lossy().into_owned();
        git(
            repo_root,
            &["worktree", "add", "-b", &branch, &path_str, base_ref],
        )?;
        mark_in_use(&path);
        Ok(Worktree { path, branch, repo_root: repo_root.to_path_buf() })
    }

    /// The unified diff of everything the turn changed vs the branch point,
    /// including new (untracked) files. Empty string means the turn touched no
    /// files. Staging (`add -A`) is what lets untracked files appear in the diff;
    /// it's local to this worktree's index and never touches the main tree.
    pub fn diff(&self) -> io::Result<String> {
        git(&self.path, &["add", "-A"])?;
        git(&self.path, &["diff", "--cached", "--no-ext-diff", "--no-textconv", "HEAD"])
    }

    /// Paths the turn changed (name-only), for a compact review summary.
    pub fn changed_files(&self) -> io::Result<Vec<String>> {
        git(&self.path, &["add", "-A"])?;
        let out = git(&self.path, &["diff", "--cached", "--name-only", "HEAD"])?;
        Ok(out.lines().filter(|l| !l.trim().is_empty()).map(str::to_owned).collect())
    }

    /// Remove the worktree checkout and delete its task branch. Call after the
    /// diff is captured (merged or discarded). Best-effort: a failure to prune
    /// leaves a stale entry that the next same-id `create` clears anyway.
    pub fn remove(self) -> io::Result<()> {
        // In use until it's gone; either way not after this (a tree that
        // fails to go is left for the hygiene sweep).
        let key = in_use_key(&self.path);
        let path_str = self.path.to_string_lossy().into_owned();
        let removed = git(&self.repo_root, &["worktree", "remove", "--force", &path_str]);
        unmark_in_use(&key);
        removed?;
        let _ = git(&self.repo_root, &["branch", "-D", &self.branch]);
        Ok(())
    }
}

/// Cut a turn's ephemeral worktree from the chat's persistent base branch
/// `hive/chat/<chat_id>` (Phase 2a: the captured diff is vs the chat branch,
/// and applying advances only that chat), ensuring the chat's base worktree
/// first. If that fails (e.g. a repo with no `HEAD`) the turn is cut from
/// `HEAD`; an `Err` means isolation is unavailable and the caller runs in place.
///
/// The worktree's path and branch are keyed by `turn_id` (unique per turn,
/// e.g. the reply's message id), not by the chat: two turns running in one
/// chat at once each get their own tree instead of the second force-removing
/// the first's.
pub fn cut_turn_worktree(repo_root: &Path, chat_id: &str, turn_id: &str) -> io::Result<Worktree> {
    match chat_worktree(repo_root, chat_id) {
        Ok(_) => Worktree::create_from(repo_root, turn_id, &format!("hive/chat/{chat_id}")),
        Err(e) => {
            tracing::warn!(target: "worktree", "chat base worktree unavailable, cutting turn from HEAD: {e}");
            Worktree::create(repo_root, turn_id)
        }
    }
}

/// A turn's worktree, cut only when the turn first writes. An HTTP model
/// turn with built-in file tools reads the chat's own dir; its first
/// `write_file` / `edit_file` / `run_command` calls [`cut_turn_worktree`]
/// (through [`crate::builtin_tools::WriteRoot`]) and the rest of the turn
/// runs there. Plain Q&A never creates (or removes) a worktree.
///
/// After the turn, [`LazyTurnWorktree::finish`] hands back the worktree, if
/// one was cut, for diff capture and removal — and closes this so a write
/// still racing the end of the turn can't cut a new one that nobody removes.
#[derive(Debug)]
pub struct LazyTurnWorktree {
    repo_root: PathBuf,
    chat_id: String,
    turn_id: String,
    state: std::sync::Mutex<LazyTurnState>,
    /// Take a parked worktree of the chat when cutting (see [`Worktree::park`]).
    reuse: bool,
}

#[derive(Debug)]
struct LazyTurnState {
    worktree: Option<Worktree>,
    closed: bool,
}

impl LazyTurnWorktree {
    /// `turn_id` names the worktree and its branch; it must be unique per
    /// turn (see [`cut_turn_worktree`]).
    pub fn new(repo_root: impl Into<PathBuf>, chat_id: impl Into<String>, turn_id: impl Into<String>) -> Self {
        Self {
            repo_root: repo_root.into(),
            chat_id: chat_id.into(),
            turn_id: turn_id.into(),
            state: std::sync::Mutex::new(LazyTurnState { worktree: None, closed: false }),
            reuse: false,
        }
    }

    /// Reuse a parked worktree of the chat when the turn first writes.
    pub fn with_reuse(mut self, reuse: bool) -> Self {
        self.reuse = reuse;
        self
    }

    /// Where a stopped turn's unfinished edits are saved (see `Drop`).
    pub fn stopped_patch_path(repo_root: &Path, turn_id: &str) -> PathBuf {
        hive_core::compat::workspace_state_path(repo_root, Path::new("stopped-turns").join(format!("{}.patch", slug(turn_id))))
    }

    /// The worktree cut so far, if any (without taking it).
    pub fn current(&self) -> Option<Worktree> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).worktree.clone()
    }

    /// Close and take the worktree this turn cut, if it wrote at all. Waits
    /// for a cut in progress to finish, so its worktree is never orphaned.
    pub fn finish(&self) -> Option<Worktree> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.closed = true;
        st.worktree.take()
    }
}

impl crate::builtin_tools::WriteRoot for LazyTurnWorktree {
    fn write_root(&self) -> Result<PathBuf, String> {
        // Held across the cut: `finish` waits for it rather than missing it.
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(wt) = &st.worktree {
            return Ok(wt.path.clone());
        }
        if st.closed {
            return Err("the turn has ended".into());
        }
        let cut = if self.reuse { cut_turn_worktree_reusing } else { cut_turn_worktree };
        let wt = cut(&self.repo_root, &self.chat_id, &self.turn_id).map_err(|e| format!("isolation unavailable: {e}"))?;
        tracing::info!(target: "worktree", path = %wt.path.display(), "turn wrote: cut its worktree");
        let path = wt.path.clone();
        st.worktree = Some(wt);
        Ok(path)
    }
}

/// A turn dropped before [`LazyTurnWorktree::finish`] — the user pressed
/// Stop, which drops the turn's future — would otherwise leave its worktree
/// and branch behind with whatever the model had edited. Save those edits as
/// a patch under `.hive/stopped-turns/` (so nothing is lost silently) and
/// remove the worktree.
impl Drop for LazyTurnWorktree {
    fn drop(&mut self) {
        let st = self.state.get_mut().unwrap_or_else(|e| e.into_inner());
        st.closed = true;
        let Some(wt) = st.worktree.take() else { return };
        let diff = wt.diff().unwrap_or_default();
        if !diff.trim().is_empty() {
            let patch = Self::stopped_patch_path(&self.repo_root, &self.turn_id);
            let saved = patch.parent().map(std::fs::create_dir_all).transpose().and_then(|_| std::fs::write(&patch, &diff));
            match saved {
                Ok(()) => tracing::warn!(target: "worktree", patch = %patch.display(), "turn stopped mid-edit: saved its changes as a patch"),
                Err(e) => {
                    tracing::warn!(target: "worktree", path = %wt.path.display(), "turn stopped mid-edit; could not save its patch ({e}), keeping the worktree");
                    return;
                }
            }
        }
        let path = wt.path.clone();
        if let Err(e) = wt.remove() {
            tracing::warn!(target: "worktree", path = %path.display(), "could not remove a stopped turn's worktree: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Worktree reuse: a finished turn's worktree is parked (reset to clean, with
// its ignored build artefacts and dependency caches kept) and the chat's next
// turn checks out its new branch there instead of building a fresh tree.
// ---------------------------------------------------------------------------

/// Parked worktrees kept per chat.
pub const POOL_PER_CHAT: usize = 2;

fn pool_dir(repo_root: &Path) -> PathBuf {
    hive_core::compat::workspace_state_path(repo_root, Path::new("worktrees"))
}

fn pool_prefix(chat_id: &str) -> String {
    format!("pool-{}-", slug(chat_id))
}

/// Parked worktrees for a chat, oldest name first.
fn pooled(repo_root: &Path, chat_id: &str) -> Vec<PathBuf> {
    let prefix = pool_prefix(chat_id);
    let mut v: Vec<PathBuf> = std::fs::read_dir(pool_dir(repo_root))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

impl Worktree {
    /// Park this worktree for the chat's next turn instead of removing it:
    /// detach, drop the turn branch, discard tracked changes and untracked
    /// files (ignored files — build output, dependency caches — stay), and
    /// move it into the chat's pool. Over [`POOL_PER_CHAT`] parked trees, or
    /// on any failure, it is removed as before.
    pub fn park(self, chat_id: &str) -> io::Result<()> {
        if pooled(&self.repo_root, chat_id).len() >= POOL_PER_CHAT {
            return self.remove();
        }
        let key = in_use_key(&self.path);
        let parked = (|| -> io::Result<()> {
            git(&self.path, &["checkout", "-q", "--detach"])?;
            git(&self.path, &["reset", "-q", "--hard"])?;
            git(&self.path, &["clean", "-q", "-fd"])?;
            let _ = git(&self.repo_root, &["branch", "-D", &self.branch]);
            let dest = pool_dir(&self.repo_root).join(format!("{}{}", pool_prefix(chat_id), slug(&uuid_like_suffix(&self.path))));
            git(&self.repo_root, &["worktree", "move", &self.path.to_string_lossy(), &dest.to_string_lossy()])?;
            Ok(())
        })();
        if parked.is_err() {
            // `remove` releases the mark (it can still canonicalize the path
            // when the move is what failed); release this one too in case
            // the tree moved and a later step failed.
            let r = self.remove();
            unmark_in_use(&key);
            return r;
        }
        unmark_in_use(&key);
        Ok(())
    }
}

/// The chat a parked (pooled) worktree belongs to, by the slug of its id,
/// from the worktree's directory name. `None` for anything that isn't a
/// parked tree. Parked trees are detached (no branch); the hygiene sweep uses
/// this to keep a live chat's pool and clear a gone or archived chat's.
pub fn parked_chat_slug(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    let rest = name.strip_prefix("pool-")?;
    // Chat ids are UUIDs: 36 characters, then `-` and the parked turn's slug.
    let (chat, tail) = (rest.get(..36)?, rest.get(36..)?);
    (tail.starts_with('-') && uuid::Uuid::parse_str(chat).is_ok()).then(|| chat.to_string())
}

fn uuid_like_suffix(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Like [`cut_turn_worktree`], but reuse a parked worktree of the same chat
/// when there is one (warm caches, built artefacts): it is moved to the
/// turn's path and its new turn branch is checked out at the chat branch tip.
/// Falls back to a fresh worktree on any failure.
pub fn cut_turn_worktree_reusing(repo_root: &Path, chat_id: &str, turn_id: &str) -> io::Result<Worktree> {
    for parked in pooled(repo_root, chat_id) {
        let slug_id = slug(turn_id);
        let branch = format!("hive/turn-{slug_id}");
        let path = hive_core::compat::workspace_state_path(repo_root, Path::new("worktrees").join(&slug_id));
        // Moving it out of the pool claims it; a concurrent turn that loses
        // the race gets an error and tries the next one.
        if git(repo_root, &["worktree", "move", &parked.to_string_lossy(), &path.to_string_lossy()]).is_err() {
            continue;
        }
        mark_in_use(&path);
        let base = match chat_worktree(repo_root, chat_id) {
            Ok(_) => format!("hive/chat/{chat_id}"),
            Err(_) => "HEAD".to_string(),
        };
        let ready = git(&path, &["checkout", "-q", "-B", &branch, &base])
            .and_then(|_| git(&path, &["reset", "-q", "--hard"]))
            .and_then(|_| git(&path, &["clean", "-q", "-fd"]));
        let wt = Worktree { path, branch, repo_root: repo_root.to_path_buf() };
        match ready {
            Ok(_) => {
                tracing::info!(target: "worktree", path = %wt.path.display(), "reused a parked turn worktree");
                return Ok(wt);
            }
            Err(e) => {
                tracing::warn!(target: "worktree", "couldn't reuse a parked worktree, cutting fresh: {e}");
                let _ = wt.remove();
            }
        }
    }
    cut_turn_worktree(repo_root, chat_id, turn_id)
}

#[cfg(test)]
mod reuse_tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let ok = command("git").arg("-C").arg(dir).args(args).output().unwrap().status.success();
        assert!(ok, "git {args:?} failed");
    }

    fn repo() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!("ligus-pool-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run(&dir, &["init", "-q"]);
        run(&dir, &["config", "user.email", "t@t"]);
        run(&dir, &["config", "user.name", "t"]);
        run(&dir, &["config", "core.autocrlf", "false"]);
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n.ligus/\n.hive/\n").unwrap();
        run(&dir, &["add", "-A"]);
        run(&dir, &["commit", "-qm", "init"]);
        dir
    }

    #[test]
    fn parked_worktree_is_reused_with_ignored_files_and_a_clean_tree() {
        let root = repo();
        let wt = cut_turn_worktree_reusing(&root, "chat1", "turn-1").unwrap();
        std::fs::create_dir_all(wt.path.join("target")).unwrap();
        std::fs::write(wt.path.join("target/cache.bin"), "warm").unwrap();
        std::fs::write(wt.path.join("a.txt"), "edited\n").unwrap();
        std::fs::write(wt.path.join("scratch.txt"), "junk").unwrap();
        let first = wt.path.clone();
        wt.park("chat1").unwrap();
        assert!(!first.exists());
        assert_eq!(pooled(&root, "chat1").len(), 1);

        let wt2 = cut_turn_worktree_reusing(&root, "chat1", "turn-2").unwrap();
        assert_eq!(wt2.branch, "hive/turn-turn-2");
        assert!(pooled(&root, "chat1").is_empty(), "claimed out of the pool");
        // The ignored cache survived; tracked edits and untracked junk did not.
        assert_eq!(std::fs::read_to_string(wt2.path.join("target/cache.bin")).unwrap(), "warm");
        assert_eq!(std::fs::read_to_string(wt2.path.join("a.txt")).unwrap(), "hello\n");
        assert!(!wt2.path.join("scratch.txt").exists());
        assert!(wt2.diff().unwrap().trim().is_empty());
        // Another chat never takes this chat's parked trees.
        wt2.park("chat1").unwrap();
        let other = cut_turn_worktree_reusing(&root, "chat2", "turn-3").unwrap();
        assert!(!other.path.join("target/cache.bin").exists());
        other.remove().unwrap();
        // Deleting the chat drops its parked trees.
        remove_chat_worktree(&root, "chat1").unwrap();
        assert!(pooled(&root, "chat1").is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn turn_trees_are_in_use_until_parked_or_removed() {
        let root = repo();
        let chat = uuid::Uuid::new_v4().to_string();
        let wt = cut_turn_worktree_reusing(&root, &chat, "t-a").unwrap();
        let first = wt.path.clone();
        assert!(is_in_use(&first), "a fresh turn tree is in use");
        wt.park(&chat).unwrap();
        assert!(!is_in_use(&first));
        let parked = pooled(&root, &chat);
        assert_eq!(parked.len(), 1);
        assert!(!is_in_use(&parked[0]), "a parked tree is idle, not in use");
        assert_eq!(parked_chat_slug(&parked[0]).as_deref(), Some(chat.as_str()));
        let wt2 = cut_turn_worktree_reusing(&root, &chat, "t-b").unwrap();
        assert!(is_in_use(&wt2.path), "a reused tree is in use again");
        let p2 = wt2.path.clone();
        wt2.remove().unwrap();
        assert!(!is_in_use(&p2));
        assert_eq!(parked_chat_slug(Path::new("/r/.ligus/worktrees/t-a")), None);
        assert_eq!(parked_chat_slug(Path::new("/r/pool-not-a-uuid-x")), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pool_is_capped() {
        let root = repo();
        let trees: Vec<Worktree> = (0..POOL_PER_CHAT + 1).map(|i| cut_turn_worktree(&root, "c", &format!("t{i}")).unwrap()).collect();
        for t in trees {
            t.park("c").unwrap();
        }
        assert_eq!(pooled(&root, "c").len(), POOL_PER_CHAT);
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, prog: &str, args: &[&str]) {
        let ok = command(prog)
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "{prog} {args:?} failed");
    }

    fn temp_repo() -> PathBuf {
        // A unique dir without Date/rand: use the process id + a static counter.
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("hive-wt-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run(&dir, "git", &["init", "-q"]);
        run(&dir, "git", &["config", "user.email", "t@t"]);
        run(&dir, "git", &["config", "user.name", "t"]);
        // Deterministic line endings across platforms — without this, git on
        // Windows (autocrlf=true by default) rewrites LF↔CRLF on checkout/apply,
        // so a round-tripped file wouldn't byte-match the LF we wrote.
        run(&dir, "git", &["config", "core.autocrlf", "false"]);
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        run(&dir, "git", &["add", "-A"]);
        run(&dir, "git", &["commit", "-qm", "init"]);
        dir
    }

    /// A file-write-only agent plants a hook in its turn tree (husky-style
    /// `core.hooksPath` points into the work tree). Hive's own git calls on
    /// that tree (park's checkout, diff, add) must never run it: they run
    /// outside any agent sandbox, with Hive's environment and network.
    #[cfg(unix)]
    #[test]
    fn hives_git_calls_never_run_repo_hooks() {
        use std::os::unix::fs::PermissionsExt;
        let repo = temp_repo();
        run(&repo, "git", &["config", "core.hooksPath", ".hooks"]);
        let marker = repo.with_extension("hook-ran");
        let _ = std::fs::remove_file(&marker);
        let wt = Worktree::create(&repo, "00000000-hook").unwrap();
        std::fs::create_dir_all(wt.path.join(".hooks")).unwrap();
        for hook in ["post-checkout", "pre-commit", "post-merge", "reference-transaction"] {
            let h = wt.path.join(".hooks").join(hook);
            std::fs::write(&h, format!("#!/bin/sh\necho {hook} >> '{}'\n", marker.display())).unwrap();
            std::fs::set_permissions(&h, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let _ = wt.diff();
        let _ = wt.changed_files();
        wt.park("11111111-2222-3333-4444-555555555555").unwrap();
        let reused = cut_turn_worktree_reusing(&repo, "11111111-2222-3333-4444-555555555555", "00000000-hook2").unwrap();
        let _ = reused.remove();
        let ran = std::fs::read_to_string(&marker).unwrap_or_default();
        let _ = std::fs::remove_file(&marker);
        let _ = std::fs::remove_dir_all(&repo);
        assert!(ran.is_empty(), "repo hooks ran during Hive's git calls: {ran}");
    }

    #[test]
    fn detects_repo_vs_nonrepo() {
        let repo = temp_repo();
        assert!(is_git_repo(&repo));
        let plain = std::env::temp_dir().join(format!("hive-notrepo-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&plain);
        assert!(!is_git_repo(&plain));
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&plain);
    }

    #[test]
    fn isolates_edits_and_captures_diff() {
        let repo = temp_repo();
        let wt = Worktree::create(&repo, "00000000-1111").unwrap();
        assert!(wt.path.exists());
        assert_eq!(wt.branch, "hive/turn-00000000-1111");

        // Edit an existing file AND add a new one inside the worktree.
        std::fs::write(wt.path.join("a.txt"), "hello\nworld\n").unwrap();
        std::fs::write(wt.path.join("b.txt"), "new file\n").unwrap();

        let diff = wt.diff().unwrap();
        assert!(diff.contains("a.txt") && diff.contains("+world"), "tracked edit in diff");
        assert!(diff.contains("b.txt") && diff.contains("+new file"), "untracked file in diff");

        let mut files = wt.changed_files().unwrap();
        files.sort();
        assert_eq!(files, vec!["a.txt".to_string(), "b.txt".to_string()]);

        // The MAIN tree is untouched by the worktree's edits.
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "hello\n");
        assert!(!repo.join("b.txt").exists());

        wt.remove().unwrap();
        // Branch is gone after removal.
        let branches = git(&repo, &["branch", "--list", "hive/turn-00000000-1111"]).unwrap();
        assert!(branches.trim().is_empty(), "task branch pruned");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn captured_diff_applies_to_the_base_tree() {
        // The whole review-gated flow: an isolated worktree's diff must apply
        // cleanly back to the base with `git apply` (what Implement does).
        use std::io::Write;
        use std::process::Stdio;
        let repo = temp_repo();
        let wt = Worktree::create(&repo, "apply-test").unwrap();
        std::fs::write(wt.path.join("a.txt"), "hello\nworld\n").unwrap();
        std::fs::write(wt.path.join("b.txt"), "new\n").unwrap();
        let diff = wt.diff().unwrap();
        wt.remove().unwrap();

        let mut child = command("git")
            .arg("-C")
            .arg(&repo)
            .args(["apply", "--whitespace=nowarn"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(diff.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success(), "captured diff applies to base tree");
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "hello\nworld\n");
        assert_eq!(std::fs::read_to_string(repo.join("b.txt")).unwrap(), "new\n");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn create_is_idempotent_after_a_stale_worktree() {
        let repo = temp_repo();
        let _first = Worktree::create(&repo, "dupe-id").unwrap();
        // A second create with the same id (as if the prior turn crashed without
        // cleanup) must succeed by clearing the stale worktree + branch.
        let second = Worktree::create(&repo, "dupe-id").unwrap();
        assert!(second.path.exists());
        second.remove().unwrap();
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn create_from_cuts_from_the_given_base() {
        // A turn cut from a chat branch must start from that branch's tip, not
        // HEAD: content committed on the base ref appears in the turn worktree.
        let repo = temp_repo();
        let chat = chat_worktree(&repo, "chatA").unwrap();
        // Advance the chat branch with a commit the main HEAD doesn't have.
        std::fs::write(chat.join("a.txt"), "hello\nfrom-chat\n").unwrap();
        run(&chat, "git", &["add", "-A"]);
        run(&chat, "git", &["commit", "-qm", "chat commit"]);

        let wt = Worktree::create_from(&repo, "turn-x", "hive/chat/chatA").unwrap();
        // The turn worktree sees the chat branch's content, not the base commit's.
        assert_eq!(
            std::fs::read_to_string(wt.path.join("a.txt")).unwrap(),
            "hello\nfrom-chat\n"
        );
        // And its diff is empty until it edits (i.e. base is the chat tip).
        assert!(wt.diff().unwrap().trim().is_empty(), "no diff vs chat tip yet");
        wt.remove().unwrap();
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn chat_branch_starts_from_the_configured_base() {
        let repo = temp_repo();
        let head = git(&repo, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        // A second line of history, as an upstream remote-tracking ref.
        git(&repo, &["checkout", "-q", "-b", "side"]).unwrap();
        std::fs::write(repo.join("side.txt"), "s\n").unwrap();
        git(&repo, &["add", "-A"]).unwrap();
        git(&repo, &["commit", "-qm", "side"]).unwrap();
        let side = git(&repo, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        git(&repo, &["update-ref", "refs/remotes/upstream/main", &side]).unwrap();
        git(&repo, &["checkout", "-q", "-"]).unwrap();
        git(&repo, &["branch", "-D", "side"]).unwrap();
        // Unset or unresolvable: HEAD.
        assert_eq!(chat_base(&repo), "HEAD");
        git(&repo, &["config", CHAT_BASE_CONFIG, "refs/remotes/upstream/nope"]).unwrap();
        assert_eq!(chat_base(&repo), "HEAD");
        let a = chat_worktree(&repo, "chatBaseA").unwrap();
        assert_eq!(git(&a, &["rev-parse", "HEAD"]).unwrap().trim(), head);
        // Set: fresh chats start there; existing chat branches don't move.
        git(&repo, &["config", CHAT_BASE_CONFIG, "refs/remotes/upstream/main"]).unwrap();
        let b = chat_worktree(&repo, "chatBaseB").unwrap();
        assert_eq!(git(&b, &["rev-parse", "HEAD"]).unwrap().trim(), side);
        assert_eq!(git(&chat_worktree(&repo, "chatBaseA").unwrap(), &["rev-parse", "HEAD"]).unwrap().trim(), head);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn chat_worktree_is_idempotent_and_creates_the_branch() {
        let repo = temp_repo();
        let p1 = chat_worktree(&repo, "chatB").unwrap();
        assert!(p1.exists());
        // The base branch now exists.
        let branches = git(&repo, &["branch", "--list", "hive/chat/chatB"]).unwrap();
        assert!(!branches.trim().is_empty(), "chat base branch created");

        // A second call returns the SAME path and does NOT wipe committed work.
        std::fs::write(p1.join("persist.txt"), "keep me\n").unwrap();
        run(&p1, "git", &["add", "-A"]);
        run(&p1, "git", &["commit", "-qm", "persisted"]);
        let p2 = chat_worktree(&repo, "chatB").unwrap();
        assert_eq!(p1, p2, "same persistent path");
        assert_eq!(
            std::fs::read_to_string(p2.join("persist.txt")).unwrap(),
            "keep me\n",
            "persistent worktree survives a re-ensure"
        );

        remove_chat_worktree(&repo, "chatB").unwrap();
        let gone = git(&repo, &["branch", "--list", "hive/chat/chatB"]).unwrap();
        assert!(gone.trim().is_empty(), "chat branch pruned on removal");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn apply_and_commit_advances_the_chat_branch() {
        // Mirrors the implement-proposal path: apply a captured diff INTO the
        // chat worktree and commit, so the chat branch advances and its checkout
        // reflects it — while the main tree stays untouched.
        use std::io::Write;
        use std::process::Stdio;
        let repo = temp_repo();
        let chat = chat_worktree(&repo, "chatC").unwrap();

        // Produce a diff from an ephemeral turn cut off the chat branch.
        let wt = Worktree::create_from(&repo, "turn-c", "hive/chat/chatC").unwrap();
        std::fs::write(wt.path.join("a.txt"), "hello\nworld\n").unwrap();
        let diff = wt.diff().unwrap();
        wt.remove().unwrap();

        // Apply into the chat worktree.
        let mut child = command("git")
            .arg("-C")
            .arg(&chat)
            .args(["apply", "--whitespace=nowarn"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(diff.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success(), "diff applies into chat worktree");
        run(&chat, "git", &["add", "-A"]);
        run(&chat, "git", &["commit", "-qm", "apply proposal"]);

        // The chat branch advanced and its checkout reflects the change.
        assert_eq!(
            std::fs::read_to_string(chat.join("a.txt")).unwrap(),
            "hello\nworld\n"
        );
        // The main tree is untouched.
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "hello\n");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn chat_helpers_err_or_noop_on_a_non_git_dir() {
        let plain = std::env::temp_dir().join(format!("hive-nogit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&plain);
        std::fs::create_dir_all(&plain).unwrap();
        assert!(!is_git_repo(&plain));
        // chat_worktree must fail cleanly (no panic) on a non-repo — the caller
        // guards with is_git_repo and falls back to the workspace root.
        assert!(chat_worktree(&plain, "x").is_err());
        // remove is best-effort and never errors.
        assert!(remove_chat_worktree(&plain, "x").is_ok());
        // create_from likewise fails cleanly rather than panicking.
        assert!(Worktree::create_from(&plain, "t", "HEAD").is_err());
        let _ = std::fs::remove_dir_all(&plain);
    }
}

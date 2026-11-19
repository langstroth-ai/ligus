//! Applying a patch to a chat's branch without ever touching a shared tree
//! (review F4), and replaying a chat's ledger (review F3).
//!
//! The exact patch is applied in a scratch **index** at the branch tip
//! (`GIT_INDEX_FILE`), never a working tree. Only when that fails, a
//! three-way apply runs in a scratch **worktree** cut detached at the tip;
//! a conflict there is thrown away with the scratch tree. The new commit
//! then advances the chat branch by compare-and-swap: a fast-forward in the
//! chat's worktree (which refuses if the branch moved or local changes are
//! in the way), or `update-ref <new> <old>` when no worktree is checked out.
//! Either the branch moves to exactly one new commit, or nothing changes.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use ligus_core::workspace_repo::{LedgerEntry, PROPOSAL_TRAILER, REVERT_TRAILER};
use uuid::Uuid;

use super::git::git;

/// Why a patch didn't land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyError {
    /// The patch doesn't apply on the branch, even three-way.
    Conflict(String),
    /// The branch moved since the caller looked (or under the apply).
    Moved,
    /// The chat's worktree has local changes the update would overwrite.
    Dirty(String),
    Other(String),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::Conflict(e) => write!(f, "the change doesn't apply on this chat's branch ({e})"),
            ApplyError::Moved => f.write_str("the chat's branch moved while applying"),
            ApplyError::Dirty(e) => write!(f, "the chat's checkout has local changes in the way ({e})"),
            ApplyError::Other(e) => f.write_str(e),
        }
    }
}

/// A patch that landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// The tip it applied onto.
    pub base: String,
    pub commit: String,
    pub tree: String,
    /// It needed a three-way merge (a different base than it was made on).
    pub three_way: bool,
}

fn branch_ref(chat_id: &str) -> String {
    format!("refs/heads/{}", crate::git_worktree::chat_branch_name(chat_id))
}

fn run_with_index(dir: &Path, index: &Path, args: &[&str], input: Option<&str>) -> Result<String, String> {
    let mut cmd = ligus_core::process_util::git();
    cmd.arg("-C").arg(dir).args(args).env("GIT_INDEX_FILE", index);
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    if let Some(text) = input {
        child.stdin.take().ok_or("no stdin")?.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn with_input(dir: &Path, args: &[&str], input: &str) -> Result<String, String> {
    let mut cmd = ligus_core::process_util::git();
    cmd.arg("-C").arg(dir).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    child.stdin.take().ok_or("no stdin")?.write_all(input.as_bytes()).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// The tree `diff` (reversed when `reverse`) makes from `base`, built in a
/// scratch index: no working tree is touched.
pub fn tree_for(checkout: &Path, base: &str, diff: &str, reverse: bool) -> Result<String, String> {
    let index = std::env::temp_dir().join(format!("ligus-apply-{}.index", Uuid::new_v4().simple()));
    let mut apply = vec!["apply", "--cached", "--whitespace=nowarn"];
    if reverse {
        apply.push("-R");
    }
    let r = run_with_index(checkout, &index, &["read-tree", base], None)
        .and_then(|_| run_with_index(checkout, &index, &apply, Some(diff)))
        .and_then(|_| run_with_index(checkout, &index, &["write-tree"], None));
    let _ = std::fs::remove_file(&index);
    r
}

fn three_way_tree(checkout: &Path, base: &str, diff: &str) -> Result<String, ApplyError> {
    let scratch: PathBuf = std::env::temp_dir().join(format!("ligus-3way-{}", Uuid::new_v4().simple()));
    let s = scratch.to_string_lossy().into_owned();
    git(checkout, &["worktree", "add", "-q", "--detach", &s, base]).map_err(ApplyError::Other)?;
    let result = (|| {
        if let Err(e) = with_input(&scratch, &["apply", "--3way", "--whitespace=nowarn"], diff) {
            return Err(ApplyError::Conflict(e.lines().last().unwrap_or("conflict").to_string()));
        }
        let unmerged = git(&scratch, &["diff", "--name-only", "--diff-filter=U"]).map_err(ApplyError::Other)?;
        if !unmerged.is_empty() {
            return Err(ApplyError::Conflict(format!("conflicts in {}", unmerged.lines().collect::<Vec<_>>().join(", "))));
        }
        git(&scratch, &["add", "-A"]).map_err(ApplyError::Other)?;
        git(&scratch, &["write-tree"]).map_err(ApplyError::Other)
    })();
    let _ = git(checkout, &["worktree", "remove", "--force", &s]);
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

fn commit_tree(checkout: &Path, tree: &str, parent: &str, message: &str) -> Result<String, String> {
    let named = git(checkout, &["config", "user.email"]).is_ok_and(|e| !e.is_empty());
    let mut args: Vec<&str> = if named { vec![] } else { vec!["-c", "user.name=Ligus", "-c", "user.email=ligus@localhost"] };
    args.extend(["commit-tree", tree, "-p", parent, "-F", "-"]);
    with_input(checkout, &args, message)
}

/// Advance the chat branch from `old` to `new` (a child of `old`), or
/// nothing: a fast-forward in the chat's worktree when one is checked out,
/// else a compare-and-swap `update-ref`.
fn advance(checkout: &Path, chat_id: &str, old: &str, new: &str) -> Result<(), ApplyError> {
    let wt = crate::git_worktree::chat_worktree_path_of(checkout, chat_id);
    if let Some(wt) = wt.filter(|w| w.join(".git").exists()) {
        let head = git(&wt, &["rev-parse", "HEAD"]).map_err(ApplyError::Other)?;
        if head != old {
            return Err(ApplyError::Moved);
        }
        return git(&wt, &["merge", "--ff-only", "-q", new]).map(|_| ()).map_err(|e| ApplyError::Dirty(e.lines().next().unwrap_or("").to_string()));
    }
    git(checkout, &["update-ref", &branch_ref(chat_id), new, old]).map(|_| ()).map_err(|_| ApplyError::Moved)
}

/// Apply `diff` (or take it back out, `reverse`) as one commit on the chat's
/// branch in `checkout`, with `message`. `expected_tip`: refuse when the
/// branch isn't there (the base a person saw).
pub fn apply_to_chat_branch(
    checkout: &Path,
    chat_id: &str,
    diff: &str,
    message: &str,
    reverse: bool,
    expected_tip: Option<&str>,
) -> Result<Applied, ApplyError> {
    let tip = git(checkout, &["rev-parse", "--verify", &branch_ref(chat_id)]).map_err(|_| ApplyError::Other("this chat has no branch here yet".into()))?;
    if expected_tip.is_some_and(|e| e != tip) {
        return Err(ApplyError::Moved);
    }
    let (tree, three_way) = match tree_for(checkout, &tip, diff, reverse) {
        Ok(t) => (t, false),
        Err(_) if !reverse => (three_way_tree(checkout, &tip, diff)?, true),
        Err(e) => return Err(ApplyError::Conflict(e)),
    };
    let commit = commit_tree(checkout, &tree, &tip, message).map_err(ApplyError::Other)?;
    advance(checkout, chat_id, &tip, &commit)?;
    Ok(Applied { base: tip, commit, tree, three_way })
}

/// Whether `diff` applies on the chat's branch in `checkout` now, without
/// changing anything: `Ok(None)` when it does (exactly, or three-way),
/// `Ok(Some(why))` with the conflict when it doesn't, for "Ask the agent to
/// rebase". The three-way try runs in a scratch tree that is thrown away.
pub fn conflict_probe(checkout: &Path, chat_id: &str, diff: &str) -> Result<Option<String>, String> {
    let tip = git(checkout, &["rev-parse", "--verify", &branch_ref(chat_id)]).map_err(|_| "this chat has no branch here yet".to_string())?;
    if tree_for(checkout, &tip, diff, false).is_ok() {
        return Ok(None);
    }
    match three_way_tree(checkout, &tip, diff) {
        Ok(_) => Ok(None),
        Err(ApplyError::Conflict(why)) => Ok(Some(why)),
        Err(e) => Err(e.to_string()),
    }
}

/// The values of `trailer` in the **trailer blocks** of the chat branch's
/// commits (each message's last paragraph, never its subject or body),
/// only on commits since `base` when one is given (the chat's pinned base:
/// nothing before it is a Ligus apply for this chat). An apply commit's
/// subject carries the proposal's title, which its producing agent wrote,
/// so a plain message grep would let a title such as "Ligus-Revert: <id>"
/// make a replay skip another proposal or its revert.
pub fn branch_trailer_values(checkout: &Path, chat_id: &str, trailer: &str, base: Option<&str>) -> HashSet<String> {
    let branch = branch_ref(chat_id);
    let range = match base.filter(|b| ligus_core::workspace_repo::is_commit_id(b)) {
        Some(b) => format!("{b}..{branch}"),
        None => branch,
    };
    let format = format!("--format=%(trailers:key={trailer},valueonly,separator=%x0a)");
    git(checkout, &["log", &format, &range, "--"])
        .map(|out| out.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
        .unwrap_or_default()
}

/// Whether the chat branch carries a commit whose trailer block has
/// `trailer: id` ([`branch_trailer_values`]).
pub fn branch_has_trailer(checkout: &Path, chat_id: &str, trailer: &str, id: Uuid) -> bool {
    branch_trailer_values(checkout, chat_id, trailer, None).contains(&id.to_string())
}

/// The message of an apply commit for a proposal without a repo (the
/// desktop's legacy path into the workspace folder's chat branch): its
/// title's first line only. A title is its producing agent's text, and a
/// multi-line one could otherwise end in a trailer block that a later
/// replay of that branch (once the chat gets a repo) reads as
/// `Ligus-Proposal: <another proposal>`.
pub fn legacy_apply_message(title: &str) -> String {
    format!("Apply proposal: {}", title.lines().next().unwrap_or(""))
}

/// The message of an apply commit for proposal `id` (with extra trailers).
pub fn apply_message(title: &str, id: Uuid, extra: &[(&str, String)]) -> String {
    let mut m = format!("Apply proposal: {}\n\n{PROPOSAL_TRAILER}: {id}\n", title.lines().next().unwrap_or(""));
    for (k, v) in extra {
        m.push_str(&format!("{k}: {v}\n"));
    }
    m
}

/// The commit the chat's branch was cut from: the first commit on its
/// first-parent line, from the tip down, that carries no Ligus trailer
/// (every apply, replay and revert commit carries one). `None` without a
/// branch, or past a bound of commits.
pub fn branch_cut_point(checkout: &Path, chat_id: &str) -> Option<String> {
    let format = format!("--format=%H%x00%(trailers:key={PROPOSAL_TRAILER},valueonly,separator=%x2c)%x00%(trailers:key={REVERT_TRAILER},valueonly,separator=%x2c)%x1e");
    let log = git(checkout, &["log", "--first-parent", "--max-count=10000", &format, &branch_ref(chat_id), "--"]).ok()?;
    log.split('\x1e').map(str::trim).filter(|r| !r.is_empty()).find_map(|rec| {
        let parts: Vec<&str> = rec.split('\0').collect();
        (parts.len() < 3 || (parts[1].trim().is_empty() && parts[2].trim().is_empty())).then(|| parts[0].to_string())
    })
}

/// Follow a re-pin of the chat's base (`ChatBaseRepinned`, review residual
/// 3): rebuild the chat branch on `new_base` by replaying the chat's ledger
/// there, then move the branch by compare-and-swap, or change nothing.
///
/// The branch must hold only ledger commits since `old_base` (the pin it
/// was cut from): a commit without a Ligus trailer is someone's work this
/// host would drop, so it refuses instead. Each entry is applied in a
/// scratch index (three-way in a scratch tree when the exact patch doesn't
/// fit), reverted entries are left out, and one whose change the new base
/// already has (merged upstream) is skipped. The chat's worktree, when one
/// is checked out, must be clean and still at the tip it had; the move is
/// `reset --keep` there, else `update-ref <new> <old>`. Returns how many
/// entries it replayed.
pub fn rebase_onto_repin(checkout: &Path, chat_id: &str, old_base: &str, new_base: &str, entries: &[LedgerEntry]) -> Result<usize, String> {
    let id_ok = |c: &str| ligus_core::workspace_repo::is_commit_id(c);
    if !id_ok(old_base) || !id_ok(new_base) {
        return Err("bad base".into());
    }
    let branch = branch_ref(chat_id);
    let tip = git(checkout, &["rev-parse", "--verify", &branch]).map_err(|_| "this chat has no branch here".to_string())?;
    let format = format!("--format=%H%x00%(trailers:key={PROPOSAL_TRAILER},valueonly,separator=%x2c)%x00%(trailers:key={REVERT_TRAILER},valueonly,separator=%x2c)%x1e");
    let log = git(checkout, &["log", &format, &format!("{old_base}..{tip}"), "--"])?;
    for rec in log.split('\x1e').map(str::trim).filter(|r| !r.is_empty()) {
        let parts: Vec<&str> = rec.split('\0').collect();
        if parts.len() < 3 || (parts[1].trim().is_empty() && parts[2].trim().is_empty()) {
            let short: String = parts[0].chars().take(12).collect();
            return Err(format!("commit {short} on this chat's branch isn't from the chat's ledger, and moving the branch would drop it"));
        }
    }
    let mut head = new_base.to_string();
    let mut made = 0;
    for e in entries.iter().filter(|e| !e.reverted) {
        let short: String = e.proposal_id.to_string().chars().take(8).collect();
        if tree_for(checkout, &head, &e.diff, true).is_ok() && tree_for(checkout, &head, &e.diff, false).is_err() {
            continue; // the new base already has it
        }
        let tree = match tree_for(checkout, &head, &e.diff, false) {
            Ok(t) => t,
            Err(_) => three_way_tree(checkout, &head, &e.diff).map_err(|err| format!("proposal {short} doesn't apply on the new base ({err})"))?,
        };
        head = commit_tree(checkout, &tree, &head, &apply_message("replayed onto the chat's new base", e.proposal_id, &[]))?;
        made += 1;
    }
    let wt = crate::git_worktree::chat_worktree_path_of(checkout, chat_id).filter(|w| w.join(".git").exists());
    match wt {
        Some(wt) => {
            let at = git(&wt, &["rev-parse", "HEAD"])?;
            if at != tip {
                return Err(ApplyError::Moved.to_string());
            }
            let dirty = git(&wt, &["status", "--porcelain", "--untracked-files=no"])?;
            if !dirty.is_empty() {
                return Err(ApplyError::Dirty(dirty.lines().next().unwrap_or("").to_string()).to_string());
            }
            git(&wt, &["reset", "-q", "--keep", &head]).map_err(|e| ApplyError::Dirty(e.lines().next().unwrap_or("").to_string()).to_string())?;
        }
        None => {
            git(checkout, &["update-ref", &branch, &head, &tip]).map_err(|_| ApplyError::Moved.to_string())?;
        }
    }
    Ok(made)
}

/// Bring the chat branch up to date with the chat's ledger: every applied
/// proposal the branch lacks (by its `Ligus-Proposal` trailer) is applied
/// in order; every reverted one it carries is taken back out once (a
/// `Ligus-Revert` trailer). Resumable: each entry is its own commit, so a
/// crash mid-way picks up at the next one. A patch whose content is already
/// on the branch (applied before trailers existed) counts as present.
/// Returns how many commits it made; an entry that conflicts stops it.
pub fn replay_ledger(checkout: &Path, chat_id: &str, entries: &[LedgerEntry], base: Option<&str>) -> Result<usize, String> {
    let mut applied_ids = branch_trailer_values(checkout, chat_id, PROPOSAL_TRAILER, base);
    let mut reverted_ids = branch_trailer_values(checkout, chat_id, REVERT_TRAILER, base);
    let mut made = 0;
    for e in entries {
        let id = e.proposal_id.to_string();
        let applied = applied_ids.contains(&id);
        if !e.reverted && !applied {
            let tip = git(checkout, &["rev-parse", &branch_ref(chat_id)])?;
            if tree_for(checkout, &tip, &e.diff, true).is_ok() && tree_for(checkout, &tip, &e.diff, false).is_err() {
                continue; // already there, without a trailer
            }
            apply_to_chat_branch(checkout, chat_id, &e.diff, &apply_message("replayed from the chat's ledger", e.proposal_id, &[]), false, None)
                .map_err(|err| format!("proposal {} ({err})", &id[..8]))?;
            applied_ids.insert(id);
            made += 1;
        } else if e.reverted && applied && !reverted_ids.contains(&id) {
            let msg = format!("Revert proposal\n\n{REVERT_TRAILER}: {}\n", e.proposal_id);
            apply_to_chat_branch(checkout, chat_id, &e.diff, &msg, true, None)
                .map_err(|err| format!("reverting proposal {} ({err})", &id[..8]))?;
            reverted_ids.insert(id);
            made += 1;
        }
    }
    Ok(made)
}

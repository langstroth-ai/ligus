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

use hive_core::workspace_repo::{LedgerEntry, PROPOSAL_TRAILER, REVERT_TRAILER};
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
    let mut cmd = hive_core::process_util::git();
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
    let mut cmd = hive_core::process_util::git();
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

/// The values of `trailer` in the **trailer blocks** of the chat branch's
/// commits (each message's last paragraph, never its subject or body),
/// only on commits since `base` when one is given (the chat's pinned base:
/// nothing before it is a Ligus apply for this chat). An apply commit's
/// subject carries the proposal's title, which its producing agent wrote,
/// so a plain message grep would let a title such as "Ligus-Revert: <id>"
/// make a replay skip another proposal or its revert.
pub fn branch_trailer_values(checkout: &Path, chat_id: &str, trailer: &str, base: Option<&str>) -> HashSet<String> {
    let branch = branch_ref(chat_id);
    let range = match base.filter(|b| hive_core::workspace_repo::is_commit_id(b)) {
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

/// The message of an apply commit for proposal `id` (with extra trailers).
pub fn apply_message(title: &str, id: Uuid, extra: &[(&str, String)]) -> String {
    let mut m = format!("Apply proposal: {}\n\n{PROPOSAL_TRAILER}: {id}\n", title.lines().next().unwrap_or(""));
    for (k, v) in extra {
        m.push_str(&format!("{k}: {v}\n"));
    }
    m
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

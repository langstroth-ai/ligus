//! Workspace repositories, chat repos and the explicit apply request (Phase
//! A, item 6: wire frozen in 2.0; UI later). Every case is signed by real
//! devices in a rules-v2 workspace.

use hive_core::authz::votes::diff_digest;
use hive_core::crypto::sign_envelope;
use hive_core::testkit::{Proof, SignedWorld};
use hive_core::workspace_repo::{repo_id, ApplyRequest, WorkspaceRepo};
use hive_core::{ActionProposal, ProposalApproval, ProposalKind, ProposalStatus, SessionEvent, Timestamp, Verdict, WorkspaceRole};
use uuid::Uuid;

fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["owner", "ada", "mara", "bea"], None);
    w.admit("owner", "ada", WorkspaceRole::Admin);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    w
}

fn repo(canonical: &str) -> WorkspaceRepo {
    WorkspaceRepo {
        id: repo_id(canonical),
        canonical: canonical.into(),
        display_name: "WeFit".into(),
        forge_kind: "github".into(),
        default_branch: "main".into(),
        root_commit: None,
    }
}

const WEFIT: &str = "github.com/zazulam/wefit";
const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const SHA2: &str = "89abcdef0123456789abcdef0123456789abcdef";

#[test]
fn repos_are_admin_presence_and_normal_form() {
    let mut w = world();
    let add = SessionEvent::WorkspaceRepoAdded { repo: repo(WEFIT) };
    let by_contributor = w.push("mara", add.clone());
    let bare = w.push_bare("ada", add.clone());
    let ok = w.push("ada", add.clone());
    let again = w.push("ada", add);
    assert_eq!(
        [w.verdict(by_contributor), w.verdict(bare), w.verdict(ok), w.verdict(again)],
        [Verdict::Denied, Verdict::Denied, Verdict::Allowed, Verdict::Denied]
    );
    assert!(w.roster().repo_live(repo_id(WEFIT)));
    // Not in normal form, or an id that doesn't match: refused.
    let shouty = w.push("ada", SessionEvent::WorkspaceRepoAdded { repo: repo("github.com/ZaZulam/Infra") });
    let mut wrong_id = repo("github.com/zazulam/infra");
    wrong_id.id = Uuid::new_v4();
    let forged = w.push("ada", SessionEvent::WorkspaceRepoAdded { repo: wrong_id });
    assert_eq!((w.verdict(shouty), w.verdict(forged)), (Verdict::Denied, Verdict::Denied));
    // A rename is cosmetic (no presence) but still an Admin's.
    let rename = SessionEvent::WorkspaceRepoUpdated { repo_id: repo_id(WEFIT), display_name: "WeFit app".into(), default_branch: "main".into() };
    let r_ok = w.push_bare("ada", rename.clone());
    let r_no = w.push("mara", rename);
    assert_eq!((w.verdict(r_ok), w.verdict(r_no)), (Verdict::Allowed, Verdict::Denied));
    // Removal needs presence; a removed repo comes back with a later add.
    let rm_bare = w.push_bare("ada", SessionEvent::WorkspaceRepoRemoved { repo_id: repo_id(WEFIT) });
    let rm = w.push("ada", SessionEvent::WorkspaceRepoRemoved { repo_id: repo_id(WEFIT) });
    assert_eq!((w.verdict(rm_bare), w.verdict(rm)), (Verdict::Denied, Verdict::Allowed));
    assert!(!w.roster().repo_live(repo_id(WEFIT)));
    let back = w.push("ada", SessionEvent::WorkspaceRepoAdded { repo: repo(WEFIT) });
    assert_eq!(w.verdict(back), Verdict::Allowed);
}

#[test]
fn a_chat_repo_must_be_live_and_is_never_an_agents_choice() {
    let mut w = world();
    w.push("ada", SessionEvent::WorkspaceRepoAdded { repo: repo(WEFIT) });
    let chat = w.chat("owner", "c");
    let unknown = w.say(chat, "mara", SessionEvent::ChatRepoSet { repo_id: Some(repo_id("github.com/zazulam/other")), base_branch: String::new() });
    let ok = w.say(chat, "mara", SessionEvent::ChatRepoSet { repo_id: Some(repo_id(WEFIT)), base_branch: "main".into() });
    // A runtime effect (the chat's primary runtime) can't redirect the chat.
    let mut env = w.draft("mara", Some(chat), SessionEvent::ChatRepoSet { repo_id: None, base_branch: String::new() }, None);
    env.actor_stamp.as_mut().unwrap().runtime_effect = true;
    env.clear_raw();
    let p = &w.people["mara"];
    sign_envelope(&mut env, p.device_id, &p.device);
    let by_runtime = w.push_env(Some(chat), env);
    let pin = w.say(chat, "mara", SessionEvent::ChatBasePinned { repo_id: repo_id(WEFIT), commit: SHA.into() });
    let bad_pin = w.say(chat, "mara", SessionEvent::ChatBasePinned { repo_id: repo_id(WEFIT), commit: "HEAD".into() });
    let v = w.verdicts();
    assert_eq!(
        [v[&unknown], v[&ok], v[&by_runtime], v[&pin], v[&bad_pin]],
        [Verdict::Denied, Verdict::Allowed, Verdict::Denied, Verdict::Allowed, Verdict::Denied]
    );
    let s = w.state(chat);
    let r = s.repo.unwrap();
    assert_eq!((r.repo_id, r.base_commit.as_deref()), (Some(repo_id(WEFIT)), Some(SHA)));
    // After removal, the chat can't be pointed at it.
    w.push("ada", SessionEvent::WorkspaceRepoRemoved { repo_id: repo_id(WEFIT) });
    let stale = w.say(chat, "mara", SessionEvent::ChatRepoSet { repo_id: Some(repo_id(WEFIT)), base_branch: String::new() });
    assert_eq!(w.verdict(stale), Verdict::Denied);
}

/// An approved proposal in a fresh chat: filed by mara, approved by ada.
fn approved(w: &mut SignedWorld) -> (Uuid, Uuid, String) {
    let chat = w.chat("owner", "c");
    let mut p = ActionProposal::file_diff("p", "mara", "diff --git a/f b/f\n+1\n".into(), vec!["f".into()]);
    p.created_at = Timestamp::epoch();
    let pid = p.id;
    w.say(chat, "mara", SessionEvent::ProposalUpserted { proposal: p });
    let d = diff_digest(&w.state(chat).proposals[0]);
    let vote = ProposalApproval { actor_id: "ada".into(), role: WorkspaceRole::Admin, approved: true, created_at: Timestamp::epoch(), diff_digest: Some(d.clone()) };
    w.say(chat, "ada", SessionEvent::ProposalVoteCast { proposal_id: pid, approval: vote });
    assert_eq!(w.state(chat).proposals[0].status, ProposalStatus::Approved);
    (chat, pid, d)
}

fn applied(pid: Uuid, d: &str, on: Option<Uuid>, base: Option<&str>, request: Option<Uuid>) -> SessionEvent {
    SessionEvent::ProposalApplied {
        proposal_id: pid,
        diff_digest: d.into(),
        commit: Some(SHA2.into()),
        at: Timestamp::epoch(),
        repo_id: None,
        applied_on: on,
        base_commit: base.map(str::to_owned),
        applied_tree_hash: Some(SHA2.into()),
        request_id: request,
    }
}

#[test]
fn a_host_applies_only_what_a_person_asked_it_to() {
    let mut w = world();
    let (chat, pid, d) = approved(&mut w);
    let host = w.people["owner"].device_id;
    let other = w.people["ada"].device_id;
    let req = |id: u128, digest: &str| ApplyRequest {
        id: Uuid::from_u128(id),
        proposal_id: pid,
        diff_digest: digest.into(),
        host_device_id: host,
        expected_base_commit: Some(SHA.into()),
        repo_id: None,
    };
    // A request naming other content is refused; mara's request stands.
    let wrong = w.say(chat, "mara", SessionEvent::ProposalApplyRequested { request: req(1, "00") });
    let asked = w.say(chat, "mara", SessionEvent::ProposalApplyRequested { request: req(2, &d) });
    assert_eq!((w.verdict(wrong), w.verdict(asked)), (Verdict::Denied, Verdict::Allowed));
    // Another device can't fulfil it; nor can the host on another base.
    let by_other = w.say(chat, "ada", applied(pid, &d, Some(other), Some(SHA), Some(Uuid::from_u128(2))));
    let other_base = w.say(chat, "owner", applied(pid, &d, Some(host), Some(SHA2), Some(Uuid::from_u128(2))));
    assert_eq!((w.verdict(by_other), w.verdict(other_base)), (Verdict::Denied, Verdict::Denied));
    assert_eq!(w.state(chat).proposals[0].status, ProposalStatus::Approved);
    // The named host, on the base the person saw: applied.
    let done = w.say(chat, "owner", applied(pid, &d, Some(host), Some(SHA), Some(Uuid::from_u128(2))));
    assert_eq!(w.verdict(done), Verdict::Allowed);
    assert_eq!(w.state(chat).proposals[0].status, ProposalStatus::Applied);
}

#[test]
fn a_person_applies_through_their_own_device_and_names_it() {
    let mut w = world();
    let (chat, pid, d) = approved(&mut w);
    let mara_dev = w.people["mara"].device_id;
    // Signed by someone else's device but stamped as mara: refused.
    let mut env = w.draft("mara", Some(chat), applied(pid, &d, None, None, None), None);
    let o = &w.people["owner"];
    sign_envelope(&mut env, o.device_id, &o.device);
    let foreign = w.push_env(Some(chat), env);
    // Naming another device as the one that applied: refused.
    let misnamed = w.say(chat, "mara", applied(pid, &d, Some(w.people["owner"].device_id), None, None));
    // A request id that doesn't exist: refused.
    let ghost = w.say(chat, "mara", applied(pid, &d, Some(mara_dev), None, Some(Uuid::from_u128(9))));
    let ok = w.say(chat, "mara", applied(pid, &d, Some(mara_dev), None, None));
    let v = w.verdicts();
    assert_eq!([v[&foreign], v[&misnamed], v[&ghost], v[&ok]], [Verdict::Denied, Verdict::Denied, Verdict::Denied, Verdict::Allowed]);
}

#[test]
fn nobody_asks_to_apply_an_unapproved_proposal() {
    let mut w = world();
    let chat = w.chat("owner", "c");
    let p = ActionProposal::new("q", ProposalKind::Decision, "mara");
    let pid = p.id;
    w.say(chat, "mara", SessionEvent::ProposalUpserted { proposal: p });
    let d = diff_digest(&w.state(chat).proposals[0]);
    let request = ApplyRequest { id: Uuid::new_v4(), proposal_id: pid, diff_digest: d, host_device_id: w.people["owner"].device_id, expected_base_commit: None, repo_id: None };
    let id = w.write("ada", Some(chat), SessionEvent::ProposalApplyRequested { request }, None, Proof::None);
    assert_eq!(w.verdict(id), Verdict::Denied);
}

// ---- the vote digest binds proposal, repo and base; the repo is sticky ----

use proptest::prelude::*;

/// A chat on WeFit, with a proposal filed there.
fn chat_on_wefit(w: &mut SignedWorld) -> (Uuid, Uuid) {
    w.push("ada", SessionEvent::WorkspaceRepoAdded { repo: repo(WEFIT) });
    w.push("ada", SessionEvent::WorkspaceRepoAdded { repo: repo("github.com/zazulam/infra") });
    let chat = w.chat("owner", "c");
    w.say(chat, "mara", SessionEvent::ChatRepoSet { repo_id: Some(repo_id(WEFIT)), base_branch: String::new() });
    let mut p = ActionProposal::file_diff("p", "mara", "diff --git a/f b/f\n+1\n".into(), vec!["f".into()]);
    p.created_at = Timestamp::epoch();
    // Whatever the author claims, the repo comes from the chat.
    p.repo_id = Some(repo_id("github.com/zazulam/infra"));
    p.base_commit = Some(SHA.into());
    let pid = p.id;
    w.say(chat, "mara", SessionEvent::ProposalUpserted { proposal: p });
    (chat, pid)
}

/// Repos review F1: an approved proposal can't be re-pointed at another
/// repo by a re-upsert (an agent's own, or anyone's): the repo is the
/// chat's at creation and stays, and the votes stay with it.
#[test]
fn agent_reupsert_changing_repo_id_keeps_original_repo() {
    let mut w = world();
    let (chat, pid) = chat_on_wefit(&mut w);
    let p = w.state(chat).proposals[0].clone();
    assert_eq!(p.repo_id, Some(repo_id(WEFIT)), "derived from the chat, not the payload");
    let d = diff_digest(&p);
    let vote = ProposalApproval { actor_id: "ada".into(), role: WorkspaceRole::Admin, approved: true, created_at: Timestamp::epoch(), diff_digest: Some(d) };
    w.say(chat, "ada", SessionEvent::ProposalVoteCast { proposal_id: pid, approval: vote });
    assert_eq!(w.state(chat).proposals[0].status, ProposalStatus::Approved);
    let mut repoint = w.state(chat).proposals[0].clone();
    repoint.repo_id = Some(repo_id("github.com/zazulam/infra"));
    repoint.status = ProposalStatus::Open;
    repoint.approvals.clear();
    repoint.title = "same diff, new title".into();
    w.say(chat, "mara", SessionEvent::ProposalUpserted { proposal: repoint });
    let p = w.state(chat).proposals[0].clone();
    assert_eq!(p.repo_id, Some(repo_id(WEFIT)));
    assert_eq!(p.status, ProposalStatus::Approved, "a title edit keeps the votes");
}

#[test]
fn base_commit_change_clears_votes() {
    let mut w = world();
    let (chat, pid) = chat_on_wefit(&mut w);
    let d = diff_digest(&w.state(chat).proposals[0]);
    let vote = ProposalApproval { actor_id: "ada".into(), role: WorkspaceRole::Admin, approved: true, created_at: Timestamp::epoch(), diff_digest: Some(d.clone()) };
    w.say(chat, "ada", SessionEvent::ProposalVoteCast { proposal_id: pid, approval: vote.clone() });
    let mut rebased = w.state(chat).proposals[0].clone();
    rebased.base_commit = Some(SHA2.into());
    rebased.status = ProposalStatus::Open;
    rebased.approvals.clear();
    w.say(chat, "mara", SessionEvent::ProposalUpserted { proposal: rebased });
    let p = w.state(chat).proposals[0].clone();
    assert!(p.approvals.is_empty() && p.status == ProposalStatus::Open, "a re-based proposal needs new votes");
    // The old vote, replayed, names the old digest and doesn't count.
    w.say(chat, "ada", SessionEvent::ProposalVoteCast { proposal_id: pid, approval: vote });
    assert_eq!(w.state(chat).proposals[0].status, ProposalStatus::Open);
}

#[test]
fn a_vote_for_one_proposal_never_counts_for_a_twin() {
    let mut w = world();
    let chat = w.chat("owner", "c");
    let a = ActionProposal::file_diff("a", "mara", "diff --git a/f b/f\n+1\n".into(), vec!["f".into()]);
    let mut b = a.clone();
    b.id = Uuid::new_v4();
    w.say(chat, "mara", SessionEvent::ProposalUpserted { proposal: a.clone() });
    w.say(chat, "mara", SessionEvent::ProposalUpserted { proposal: b.clone() });
    let s = w.state(chat);
    let da = diff_digest(s.proposals.iter().find(|p| p.id == a.id).unwrap());
    let vote = ProposalApproval { actor_id: "ada".into(), role: WorkspaceRole::Admin, approved: true, created_at: Timestamp::epoch(), diff_digest: Some(da) };
    let wrong = w.say(chat, "ada", SessionEvent::ProposalVoteCast { proposal_id: b.id, approval: vote });
    assert_eq!(w.verdict(wrong), Verdict::Denied);
}

proptest! {
    /// Any sequence of upserts (by the author, re-pointing, re-basing,
    /// retitling) never changes a proposal's repo after creation.
    #[test]
    fn prop_repo_is_sticky(steps in prop::collection::vec((0u8..3, any::<bool>(), any::<bool>()), 1..8)) {
        let mut w = world();
        let (chat, _pid) = chat_on_wefit(&mut w);
        for (repo_pick, rebase, retitle) in steps {
            let mut p = w.state(chat).proposals[0].clone();
            p.repo_id = [None, Some(repo_id(WEFIT)), Some(repo_id("github.com/zazulam/infra"))][repo_pick as usize];
            if rebase { p.base_commit = Some(SHA2.into()); }
            if retitle { p.title.push('!'); }
            p.status = ProposalStatus::Open;
            p.approvals.clear();
            w.say(chat, "mara", SessionEvent::ProposalUpserted { proposal: p });
            prop_assert_eq!(w.state(chat).proposals[0].repo_id, Some(repo_id(WEFIT)));
        }
    }
}

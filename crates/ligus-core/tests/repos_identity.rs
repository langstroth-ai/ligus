//! Repos and checkouts: canonical identity (review F5), repo fold order
//! (F17), and the chat ledger (F3). Pure functions and signed folds only.

use std::collections::BTreeMap;

use ligus_core::authz::votes::diff_digest;
use ligus_core::testkit::SignedWorld;
use ligus_core::workspace_repo::{canonical_is_normal, canonical_repo, ledger, repo_id, CanonError, WorkspaceRepo};
use ligus_core::{ActionProposal, ProposalApproval, ProposalStatus, SessionEvent, Timestamp, Verdict, WorkspaceRole};
use proptest::prelude::*;
use uuid::Uuid;

fn none() -> BTreeMap<String, String> {
    BTreeMap::new()
}

fn canon(remote: &str) -> Result<String, CanonError> {
    canonical_repo(remote, "", &none())
}

#[test]
fn canonical_ssh_https_scp_equal() {
    for r in [
        "git@github.com:ZaZulam/WeFit.git",
        "ssh://git@github.com:22/zazulam/wefit",
        "https://www.github.com/zazulam/wefit/",
        "https://x-access-token:SECRET@github.com/zazulam/wefit.git",
        "ssh://git@ssh.github.com:443/zazulam/wefit.git",
        "git://github.com/zazulam/wefit",
        "http://GitHub.com./zazulam/wefit",
    ] {
        assert_eq!(canon(r).as_deref(), Ok("github.com/zazulam/wefit"), "{r}");
    }
}

#[test]
fn canonical_self_hosted_with_port_and_alias() {
    let mut aliases = none();
    aliases.insert("ssh.git.example.com".into(), "git.example.com".into());
    let c = canonical_repo("ssh://git@ssh.git.example.com:2222/grp/sub/repo.git", "", &aliases).unwrap();
    assert_eq!(c, "git.example.com/grp/sub/repo");
    // Without this device's alias, the SSH host is its own identity.
    assert_eq!(canon("ssh://git@ssh.git.example.com:2222/grp/sub/repo.git").unwrap(), "ssh.git.example.com/grp/sub/repo");
    // A per-account SSH alias (`Host github-work` in ~/.ssh/config).
    let mut work = none();
    work.insert("github-work".into(), "github.com".into());
    assert_eq!(canonical_repo("github-work:Acme/App.git", "", &work).unwrap(), "github.com/acme/app");
}

#[test]
fn canonical_unknown_forge_keeps_case() {
    assert_eq!(canon("https://git.example.com/Org/Repo.git").unwrap(), "git.example.com/Org/Repo");
    // The repo entry's own forge kind folds case; a device's settings never do.
    assert_eq!(canonical_repo("https://git.example.com/Org/Repo.git", "gitea", &none()).unwrap(), "git.example.com/org/repo");
}

#[test]
fn canonical_rejects_non_ascii_backslash_space_percent() {
    for bad in [
        "https://bücher.example/o/r",
        "https://github.com/a\\b/c",
        "https://github.com/a b/c",
        "https://github.com/a/we%2Ffit",
        "https://github.com/a/b?x=1",
        "https://github.com/a/b#frag",
        "https://github.com/a/../b",
        "https://github.com/a/./b",
        "https://github.com/a",
        "ext::sh -c id",
        "fd::17",
        "gopher://github.com/a/b",
        "https://[::1]/a/b",
        "https://-bad.com/a/b",
        "https://github.com/a//b",
    ] {
        assert!(matches!(canon(bad), Err(CanonError::Refused(_))), "{bad:?} gave {:?}", canon(bad));
    }
    for local in ["", "/srv/git/a/b", "file:///tmp/a/b.git", "../sibling", "C:\\code\\wefit", "C:/code/wefit"] {
        assert_eq!(canon(local), Err(CanonError::NoRemote), "{local:?}");
    }
    let deep = format!("https://h.example/{}", vec!["s"; 21].join("/"));
    assert!(canon(&deep).is_err(), "more than 20 segments");
}

#[test]
fn ssh_alias_map_is_device_local_and_only_changes_the_host() {
    // The same remote on two devices with different alias maps: identity
    // differs only through the host, never the path.
    let mut a = none();
    a.insert("work".into(), "gitlab.com".into());
    assert_eq!(canonical_repo("work:Grp/P.git", "", &a).unwrap(), "gitlab.com/grp/p");
    assert_eq!(canon("work:Grp/P.git").unwrap(), "work/Grp/P");
}

fn host() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("github.com".to_string()),
        Just("gitlab.com".to_string()),
        Just("codeberg.org".to_string()),
        "[a-z][a-z0-9]{0,6}(\\.[a-z][a-z0-9]{0,6}){1,2}",
    ]
}

fn segment() -> impl Strategy<Value = String> {
    "[A-Za-z0-9][A-Za-z0-9_-]{0,10}"
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(256)))]

    /// Every spelling of one remote gives one canonical, in normal form, and
    /// the canonical reads back as itself (no I/O, no device state).
    #[test]
    fn prop_canonical_pure_and_idempotent(
        h in host(),
        segs in prop::collection::vec(segment(), 2..5),
        kind in prop_oneof![Just(""), Just("github"), Just("gitea")],
        port in prop::option::of(1u16..65535),
        www in any::<bool>(),
        dotgit in any::<bool>(),
        slash in any::<bool>(),
        user in any::<bool>(),
    ) {
        let path = segs.join("/");
        let suffix = format!("{}{}", if dotgit { ".git" } else { "" }, if slash && !dotgit { "/" } else { "" });
        let hh = if www { format!("www.{h}") } else { h.clone() };
        let auth = if user { "git@" } else { "" };
        let p = port.map(|p| format!(":{p}")).unwrap_or_default();
        let forms = [
            format!("https://{auth}{hh}{p}/{path}{suffix}"),
            format!("ssh://{auth}{hh}{p}/{path}{suffix}"),
            format!("{auth}{hh}:{path}{suffix}"),
        ];
        let first = canonical_repo(&forms[0], kind, &none()).unwrap();
        prop_assert!(canonical_is_normal(&first, kind), "{first}");
        for f in &forms[1..] {
            prop_assert_eq!(&canonical_repo(f, kind, &none()).unwrap(), &first);
        }
        prop_assert_eq!(canonical_repo(&format!("https://{first}"), kind, &none()).unwrap(), first.clone());
        prop_assert_eq!(repo_id(&first), repo_id(&canonical_repo(&forms[2], kind, &none()).unwrap()));
    }

    /// Whatever is fed in, an accepted canonical is in normal form (so the
    /// fold accepts an add of it).
    #[test]
    fn prop_any_accepted_remote_is_normal(s in "\\PC{0,60}", kind in prop_oneof![Just(""), Just("gitlab")]) {
        if let Ok(c) = canonical_repo(&s, kind, &none()) {
            prop_assert!(canonical_is_normal(&c, kind), "{s:?} -> {c:?}");
        }
    }
}

// ---- the repo fold: order rules (review F17) ----

fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["owner", "ada", "bo", "mara"], None);
    w.admit("owner", "ada", WorkspaceRole::Admin);
    w.admit("owner", "bo", WorkspaceRole::Admin);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    w
}

fn repo(canonical: &str, name: &str) -> WorkspaceRepo {
    WorkspaceRepo {
        id: repo_id(canonical),
        canonical: canonical.into(),
        display_name: name.into(),
        forge_kind: String::new(),
        default_branch: "main".into(),
        root_commit: None,
        scratch: false,
    }
}

const WEFIT: &str = "github.com/zazulam/wefit";

/// Two Admins add the same repo at once from two devices: one repo after
/// the fold, whichever arrives first (the id is derived from the canonical).
#[test]
fn repo_id_deterministic_concurrent_adds_merge() {
    let mut w = world();
    let a = w.draft("ada", None, SessionEvent::WorkspaceRepoAdded { repo: repo(WEFIT, "WeFit") }, None);
    let b = w.draft("bo", None, SessionEvent::WorkspaceRepoAdded { repo: repo(WEFIT, "WeFit (Bo)") }, None);
    let mut a = a;
    let mut b = b;
    let pa = w.recovery_presence("ada", &a);
    w.finish(&mut a, "ada", Some(pa));
    let pb = w.recovery_presence("bo", &b);
    w.finish(&mut b, "bo", Some(pb));
    let (ia, ib) = (a.event_id, b.event_id);
    w.push_env(None, a);
    w.push_env(None, b);
    let v = w.verdicts();
    assert_eq!([v[&ia], v[&ib]].iter().filter(|x| **x == Verdict::Allowed).count(), 1, "first in canonical order wins");
    let roster = w.roster();
    assert_eq!(roster.repos.len(), 1);
    // Reversed delivery folds the same.
    let mut rev = w.cfg.clone();
    rev.reverse();
    let (_, t) = ligus_core::events::project_config(&rev, None, None);
    assert_eq!(t.current().repos, roster.repos);
}

#[test]
fn chat_repo_set_to_removed_repo_denied_and_readd_resurrects() {
    let mut w = world();
    w.push("ada", SessionEvent::WorkspaceRepoAdded { repo: repo(WEFIT, "WeFit") });
    let chat = w.chat("owner", "c");
    let set = w.say(chat, "mara", SessionEvent::ChatRepoSet { repo_id: Some(repo_id(WEFIT)), base_branch: String::new() });
    w.push("ada", SessionEvent::WorkspaceRepoRemoved { repo_id: repo_id(WEFIT) });
    let denied = w.say(chat, "mara", SessionEvent::ChatRepoSet { repo_id: Some(repo_id(WEFIT)), base_branch: "dev".into() });
    assert_eq!((w.verdict(set), w.verdict(denied)), (Verdict::Allowed, Verdict::Denied));
    // The chat keeps its (now removed) repo; a later add brings it back.
    assert_eq!(w.state(chat).repo.unwrap().repo_id, Some(repo_id(WEFIT)));
    assert!(w.roster().repos[&repo_id(WEFIT)].removed);
    w.push("bo", SessionEvent::WorkspaceRepoAdded { repo: repo(WEFIT, "WeFit") });
    assert!(w.roster().repo_live(repo_id(WEFIT)));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(256)))]

    /// Any interleaving of adds, renames and removals by two Admins folds to
    /// one roster, whatever order the relay delivers the log in (the
    /// canonical order decides, not arrival).
    #[test]
    fn prop_repo_add_remove_update_order_independent_given_canonical_order(
        ops in prop::collection::vec((0u8..3, 0u8..2, any::<bool>(), any::<u64>()), 1..10),
    ) {
        let mut w = world();
        let names = [WEFIT, "github.com/zazulam/infra"];
        for (op, which, by_bo, _) in &ops {
            let by = if *by_bo { "bo" } else { "ada" };
            let c = names[*which as usize];
            let e = match op {
                0 => SessionEvent::WorkspaceRepoAdded { repo: repo(c, "R") },
                1 => SessionEvent::WorkspaceRepoUpdated { repo_id: repo_id(c), display_name: format!("R{by}"), default_branch: "main".into(), scratch: None },
                _ => SessionEvent::WorkspaceRepoRemoved { repo_id: repo_id(c) },
            };
            w.push(by, e);
        }
        let want = w.roster().repos;
        let mut shuffled = w.cfg.clone();
        // A deterministic shuffle from the generated seeds.
        let mut keys: Vec<(u64, usize)> = ops.iter().map(|o| o.3).chain(std::iter::repeat(7)).zip(0..shuffled.len()).collect();
        keys.sort();
        let order: Vec<usize> = keys.into_iter().map(|(_, i)| i).collect();
        shuffled = order.into_iter().map(|i| shuffled[i].clone()).collect();
        let (_, t) = ligus_core::events::project_config(&shuffled, None, None);
        prop_assert_eq!(t.current().repos.clone(), want);
    }
}

// ---- the ledger (review F3) ----

#[test]
fn concurrent_apply_dedupes_in_ledger_and_revert_marks_it() {
    let mut w = world();
    w.push("ada", SessionEvent::WorkspaceRepoAdded { repo: repo(WEFIT, "WeFit") });
    let chat = w.chat("owner", "c");
    w.say(chat, "mara", SessionEvent::ChatRepoSet { repo_id: Some(repo_id(WEFIT)), base_branch: String::new() });
    let mut p = ActionProposal::file_diff("p", "mara", "diff --git a/f b/f\n+1\n".into(), vec!["f".into()]);
    p.created_at = Timestamp::epoch();
    let pid = p.id;
    w.say(chat, "mara", SessionEvent::ProposalUpserted { proposal: p });
    let d = diff_digest(&w.state(chat).proposals[0]);
    let vote = ProposalApproval { actor_id: "ada".into(), role: WorkspaceRole::Admin, approved: true, created_at: Timestamp::epoch(), diff_digest: Some(d.clone()) };
    w.say(chat, "ada", SessionEvent::ProposalVoteCast { proposal_id: pid, approval: vote });
    assert_eq!(w.state(chat).proposals[0].status, ProposalStatus::Approved);
    let applied = |dev: Uuid| SessionEvent::ProposalApplied {
        proposal_id: pid,
        diff_digest: d.clone(),
        commit: None,
        at: Timestamp::epoch(),
        repo_id: Some(repo_id(WEFIT)),
        applied_on: Some(dev),
        base_commit: None,
        applied_tree_hash: None,
        request_id: None,
    };
    // Two people apply it at once from their own devices.
    let ea = w.draft("ada", Some(chat), applied(w.people["ada"].device_id), None);
    let eb = w.draft("mara", Some(chat), applied(w.people["mara"].device_id), None);
    w.push_env(Some(chat), ea);
    w.push_env(Some(chat), eb);
    let s = w.state(chat);
    assert_eq!(s.applied_order, vec![pid]);
    let l = ledger(&s, repo_id(WEFIT));
    assert_eq!(l.len(), 1);
    assert!(!l[0].reverted);
    assert!(ledger(&s, repo_id("github.com/zazulam/infra")).is_empty(), "another repo's ledger is empty");
    w.say(chat, "ada", SessionEvent::ProposalReverted { proposal_id: pid, at: Timestamp::epoch() });
    let s = w.state(chat);
    assert!(ledger(&s, repo_id(WEFIT))[0].reverted, "a revert takes it back out on replay");
}

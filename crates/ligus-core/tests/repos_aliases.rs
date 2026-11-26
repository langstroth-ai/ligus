//! Workspace-synced repo and host aliases (design repos-and-checkouts 12;
//! review F6, R2): Admin plus presence, never an agent or a phone, names
//! only, disjoint from every live name and from look-alikes, first in
//! canonical order wins. Every case is signed by real devices in a rules-v2
//! workspace.

use ligus_core::testkit::SignedWorld;
use ligus_core::workspace_repo::{repo_aliases_ok, repo_id, skeleton, WorkspaceRepo};
use ligus_core::{ChatSession, SessionEvent, Verdict, WorkspaceRole};
use proptest::prelude::*;
use uuid::Uuid;

const WEFIT: &str = "github.com/zazulam/wefit";
const INFRA: &str = "git.example.com/Ops/Infra";

fn repo(canonical: &str, kind: &str) -> WorkspaceRepo {
    WorkspaceRepo {
        id: repo_id(canonical),
        canonical: canonical.into(),
        display_name: "R".into(),
        forge_kind: kind.into(),
        default_branch: String::new(),
        root_commit: None,
        scratch: false,
    }
}

fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["owner", "ada", "bo", "mara"], None);
    w.admit("owner", "ada", WorkspaceRole::Admin);
    w.admit("owner", "bo", WorkspaceRole::Admin);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    w.push(
        "ada",
        SessionEvent::WorkspaceRepoAdded {
            repo: repo(WEFIT, "github"),
        },
    );
    w.push(
        "ada",
        SessionEvent::WorkspaceRepoAdded {
            repo: repo(INFRA, ""),
        },
    );
    w
}

fn aliases(id: &str, list: &[&str]) -> SessionEvent {
    SessionEvent::WorkspaceRepoAliasesSet {
        repo_id: repo_id(id),
        aliases: list.iter().map(|s| s.to_string()).collect(),
    }
}

fn host(alias: &str, host: &str) -> SessionEvent {
    SessionEvent::WorkspaceHostAliasSet {
        alias: alias.into(),
        host: host.into(),
    }
}

#[test]
fn alias_change_requires_presence_v2_and_an_admin() {
    let mut w = world();
    let e = aliases(WEFIT, &["github.com/old-org/wefit"]);
    let bare = w.push_bare("ada", e.clone());
    let by_contributor = w.push("mara", e.clone());
    let ok = w.push("ada", e);
    assert_eq!(
        [w.verdict(bare), w.verdict(by_contributor), w.verdict(ok)],
        [Verdict::Denied, Verdict::Denied, Verdict::Allowed]
    );
    assert_eq!(
        w.roster().repo_aliases[&repo_id(WEFIT)],
        vec!["github.com/old-org/wefit".to_string()]
    );
    let h = host("ssh.git.example.com", "git.example.com");
    let (hb, hc, hok) = (
        w.push_bare("ada", h.clone()),
        w.push("mara", h.clone()),
        w.push("ada", h),
    );
    assert_eq!(
        [w.verdict(hb), w.verdict(hc), w.verdict(hok)],
        [Verdict::Denied, Verdict::Denied, Verdict::Allowed]
    );
    assert_eq!(
        w.roster().host_aliases["ssh.git.example.com"],
        "git.example.com"
    );
    // Clearing needs the same.
    let clear = host("ssh.git.example.com", "");
    let (cb, cok) = (w.push_bare("ada", clear.clone()), w.push("ada", clear));
    assert_eq!(
        [w.verdict(cb), w.verdict(cok)],
        [Verdict::Denied, Verdict::Allowed]
    );
    assert!(w.roster().host_aliases.is_empty());
}

#[test]
fn agents_and_phones_cannot_set_aliases() {
    let fold = ChatSession::new("", Uuid::nil(), "");
    for e in [
        aliases(WEFIT, &["github.com/old/wefit"]),
        host("a.example.com", "github.com"),
    ] {
        assert!(
            ligus_core::authz::agent::tier_allows(&e, Uuid::new_v4(), Uuid::new_v4(), &fold)
                .is_err()
        );
        assert!(ligus_core::authz::phone::phone_tier_allows(&e, Uuid::new_v4()).is_err());
        assert!(ligus_core::authz::rules::requires_presence(
            &e,
            "ada",
            &Default::default(),
            &fold
        ));
    }
}

#[test]
fn aliases_are_disjoint_from_live_names() {
    let mut w = world();
    // Another repo's canonical, the repo's own canonical, a local root.
    for bad in [
        &[INFRA][..],
        &[WEFIT],
        &["local:0123456789abcdef0123456789abcdef01234567"],
        &["github.com/Old/WeFit"],
        &["not a name"],
    ] {
        let id = w.push("ada", aliases(WEFIT, bad));
        assert_eq!(w.verdict(id), Verdict::Denied, "{bad:?}");
    }
    let ok = w.push("ada", aliases(WEFIT, &["github.com/old-org/wefit"]));
    assert_eq!(w.verdict(ok), Verdict::Allowed);
    // Infra can't take WeFit's alias, nor a look-alike of it.
    let taken = w.push("ada", aliases(INFRA, &["github.com/old-org/wefit"]));
    let alike = w.push("ada", aliases(INFRA, &["github.com/old_org/wefit"]));
    let dup = w.push(
        "ada",
        aliases(INFRA, &["git.example.com/a/b", "git.example.com/a/b"]),
    );
    assert_eq!(
        [w.verdict(taken), w.verdict(alike), w.verdict(dup)],
        [Verdict::Denied; 3]
    );
    // Too many.
    let many: Vec<String> = (0..9).map(|i| format!("git.example.com/x/r{i}")).collect();
    let many = w.push(
        "ada",
        SessionEvent::WorkspaceRepoAliasesSet {
            repo_id: repo_id(INFRA),
            aliases: many,
        },
    );
    assert_eq!(w.verdict(many), Verdict::Denied);
}

#[test]
fn alias_lookalike_refused_and_case_split_allowed() {
    let mut w = world();
    // `zazu1am` reads as `zazulam`: a look-alike of WeFit's own canonical.
    let alike = w.push("ada", aliases(WEFIT, &["github.com/zazu1am/wefit"]));
    let corn = w.push("ada", aliases(INFRA, &["git.example.corn/Ops/Infra"]));
    assert_eq!([w.verdict(alike), w.verdict(corn)], [Verdict::Denied; 2]);
    // On a case-sensitive forge a case split is fixed with an alias.
    let split = w.push("ada", aliases(INFRA, &["git.example.com/ops/infra"]));
    assert_eq!(w.verdict(split), Verdict::Allowed);
    assert_eq!(skeleton("github.com/zazu1am/wefit"), skeleton(WEFIT));
}

/// Review: the alias skeleton is the repos' look-alike fold, so an alias
/// can't pass with a confusable the repo list would flag (`5`/`s`, `8`/`b`,
/// `|`/`l`, or `rn` split by a dropped separator).
#[test]
fn alias_skeleton_folds_every_confusable_the_repo_flags_fold() {
    for (a, b) in [
        ("github.com/zazulam/wefit5", "github.com/zazulam/wefits"),
        ("github.com/8ob/x", "github.com/bob/x"),
        ("github.com/zazu|am/wefit", WEFIT),
        ("github.com/r-n/x", "github.com/m/x"),
        ("github.com/r.n/x", "github.com/m/x"),
    ] {
        assert_eq!(skeleton(a), skeleton(b), "{a} vs {b}");
    }
    let mut w = world();
    let split_rn = w.push("ada", aliases(INFRA, &["github.com/zazu-lam/wef-it"]));
    assert_eq!(
        w.verdict(split_rn),
        Verdict::Denied,
        "a separator doesn't hide a look-alike"
    );
    // A `local:` name compares exactly.
    assert_ne!(skeleton("local:0a"), skeleton("local:oa"));
}

#[test]
fn host_alias_must_target_a_live_repo_host_and_never_shadow_one() {
    let mut w = world();
    let unknown = w.push("ada", host("ssh.example.org", "example.org"));
    let shadow = w.push("ada", host("github.com", "git.example.com"));
    let unrelated = w.push("ada", host("gh-ssh.example.net", "github.com"));
    let alike2 = w.push("ada", host("g1thub.com", "git.example.com"));
    let ported = w.push("ada", host("ssh.git.example.com:2222", "git.example.com"));
    let selfie = w.push("ada", host("git.example.com", "git.example.com"));
    assert_eq!(
        [
            w.verdict(unknown),
            w.verdict(shadow),
            w.verdict(alike2),
            w.verdict(ported),
            w.verdict(selfie)
        ],
        [Verdict::Denied; 5]
    );
    // An unrelated SSH host for github.com: allowed.
    assert_eq!(w.verdict(unrelated), Verdict::Allowed);
    let ok = w.push("ada", host("ssh.git.example.com", "git.example.com"));
    assert_eq!(w.verdict(ok), Verdict::Allowed);
    // An alias's host can't then be a repo's, and a repo alias can't sit on it.
    let on_aliased = w.push(
        "ada",
        aliases(INFRA, &["ssh.git.example.com/ops/infra-old"]),
    );
    let add = w.push(
        "ada",
        SessionEvent::WorkspaceRepoAdded {
            repo: repo("ssh.git.example.com/a/b", ""),
        },
    );
    assert_eq!(
        [w.verdict(on_aliased), w.verdict(add)],
        [Verdict::Denied; 2]
    );
}

#[test]
fn repo_add_shadowed_by_alias_refused_and_removal_drops_aliases() {
    let mut w = world();
    w.push("ada", aliases(WEFIT, &["github.com/old-org/wefit"]));
    let same = w.push(
        "ada",
        SessionEvent::WorkspaceRepoAdded {
            repo: repo("github.com/old-org/wefit", "github"),
        },
    );
    let alike = w.push(
        "ada",
        SessionEvent::WorkspaceRepoAdded {
            repo: repo("github.com/o1d-org/wefit", "github"),
        },
    );
    assert_eq!([w.verdict(same), w.verdict(alike)], [Verdict::Denied; 2]);
    w.push(
        "ada",
        SessionEvent::WorkspaceRepoRemoved {
            repo_id: repo_id(WEFIT),
        },
    );
    assert!(!w.roster().repo_aliases.contains_key(&repo_id(WEFIT)));
    let now_free = w.push(
        "ada",
        SessionEvent::WorkspaceRepoAdded {
            repo: repo("github.com/old-org/wefit", "github"),
        },
    );
    assert_eq!(w.verdict(now_free), Verdict::Allowed);
}

/// Two Admins at once give two repos the same alias: exactly one gets it,
/// whatever order the relay delivers (the canonical order decides).
#[test]
fn alias_collision_noop_order_independent() {
    let mut w = world();
    let mut a = w.draft(
        "ada",
        None,
        aliases(WEFIT, &["github.com/old-org/thing"]),
        None,
    );
    let mut b = w.draft(
        "bo",
        None,
        aliases(INFRA, &["github.com/old-org/thing"]),
        None,
    );
    let pa = w.recovery_presence("ada", &a);
    w.finish(&mut a, "ada", Some(pa));
    let pb = w.recovery_presence("bo", &b);
    w.finish(&mut b, "bo", Some(pb));
    let (ia, ib) = (a.event_id, b.event_id);
    w.push_env(None, a);
    w.push_env(None, b);
    let v = w.verdicts();
    assert_eq!(
        [v[&ia], v[&ib]]
            .iter()
            .filter(|x| **x == Verdict::Allowed)
            .count(),
        1
    );
    let want = w.roster();
    let mut rev = w.cfg.clone();
    rev.reverse();
    let (_, t) = ligus_core::events::project_config(&rev, None, None);
    assert_eq!(
        (
            t.current().repo_aliases.clone(),
            t.current().host_aliases.clone()
        ),
        (want.repo_aliases, want.host_aliases)
    );
}

const POOL: [&str; 6] = [
    "github.com/old-org/wefit",
    "github.com/o1d-org/wefit",
    "git.example.com/ops/infra",
    "git.example.com/Ops/Infra2",
    "github.com/zazulam/wefit",
    "github.com/zazulam/other",
];
const HOSTS: [&str; 4] = [
    "ssh.git.example.com",
    "github.com",
    "g1thub.com",
    "ssh.github.com",
];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(256)))]

    /// Any interleaving of alias, host-alias, add and remove events by two
    /// Admins, some drafted concurrently: the fold is the same whatever the
    /// delivery order, and every alias has its own skeleton among the live
    /// names, except a repo's own case-split alias.
    #[test]
    fn prop_aliases_disjoint_and_order_independent(
        ops in prop::collection::vec((0u8..4, 0usize..6, 0usize..6, 0usize..4, any::<bool>(), any::<bool>()), 1..12),
        seed in any::<u64>(),
    ) {
        let mut w = world();
        let repos = [WEFIT, INFRA];
        let mut pending = Vec::new();
        for (op, a, b, h, by_bo, concurrent) in &ops {
            let by = if *by_bo { "bo" } else { "ada" };
            let target = repos[a % 2];
            let e = match op {
                0 => SessionEvent::WorkspaceRepoAliasesSet { repo_id: repo_id(target), aliases: vec![POOL[*a].into(), POOL[*b].into()].into_iter().take(1 + (b % 2)).collect() },
                1 => host(HOSTS[*h], ["git.example.com", "github.com"][b % 2]),
                2 => SessionEvent::WorkspaceRepoAdded { repo: repo(POOL[*b], if POOL[*b].starts_with("github") { "github" } else { "" }) },
                _ => SessionEvent::WorkspaceRepoRemoved { repo_id: repo_id(POOL[*b]) },
            };
            let mut env = w.draft(by, None, e, None);
            let p = w.recovery_presence(by, &env);
            w.finish(&mut env, by, Some(p));
            if *concurrent {
                pending.push(env);
            } else {
                for e in pending.drain(..) {
                    w.push_env(None, e);
                }
                w.push_env(None, env);
            }
        }
        for e in pending.drain(..) {
            w.push_env(None, e);
        }
        let want = w.roster();
        // Disjoint: an alias has its own skeleton among every live name
        // (canonicals and aliases), but a repo's own case split. (Two
        // canonicals are the add rule's business, unchanged here.)
        let mut names: Vec<(String, Uuid, bool)> = Vec::new();
        for (id, e) in want.repos.iter().filter(|(_, e)| !e.removed) {
            names.push((e.repo.canonical.clone(), *id, false));
            for a in want.repo_aliases.get(id).into_iter().flatten() {
                names.push((a.clone(), *id, true));
            }
        }
        for (i, (x, xi, xa)) in names.iter().enumerate() {
            for (y, yi, ya) in &names[i + 1..] {
                if !xa && !ya {
                    continue;
                }
                let case_split = xi == yi && x.eq_ignore_ascii_case(y);
                prop_assert!(case_split || skeleton(x) != skeleton(y), "{x} and {y} collide");
            }
        }
        for (alias, target) in &want.host_aliases {
            prop_assert!(!want.repos.values().any(|e| !e.removed && e.repo.canonical.starts_with(&format!("{alias}/"))), "{alias} shadows a repo host");
            prop_assert_ne!(alias, target);
        }
        // Order-independent.
        let mut shuffled = w.cfg.clone();
        let mut s = seed | 1;
        for i in (1..shuffled.len()).rev() {
            s ^= s << 13; s ^= s >> 7; s ^= s << 17;
            shuffled.swap(i, (s % (i as u64 + 1)) as usize);
        }
        let (_, t) = ligus_core::events::project_config(&shuffled, None, None);
        prop_assert_eq!(&t.current().repo_aliases, &want.repo_aliases);
        prop_assert_eq!(&t.current().host_aliases, &want.host_aliases);
        // The check the fold ran agrees with the final state.
        for (id, list) in &want.repo_aliases {
            let mut without = want.clone();
            without.repo_aliases.remove(id);
            prop_assert!(repo_aliases_ok(&without, *id, list).is_ok() || !want.repo_live(*id));
        }
    }
}

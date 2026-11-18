//! The desktop hides or disables a membership action the fold will refuse,
//! and says why (Phase B, item 3). It asks `authz::member_action_refusal`,
//! which runs the fold's own rules; this checks the two never disagree: for
//! random rosters (several Owners, Admins, Contributors, Viewers and service
//! members, a founder), every removal and role change a person tries is
//! offered exactly when the fold, judging the real signed event with the
//! person's passkey presence, allows it.

use hive_core::authz::{member_action_refusal, MemberAction};
use hive_core::testkit::SignedWorld;
use hive_core::{ActorKind, MemberRoleChange, SessionEvent, Verdict, WorkspaceRole};
use proptest::prelude::*;

const NAMES: [&str; 6] = ["f", "p1", "p2", "p3", "p4", "p5"];
const ROLES: [WorkspaceRole; 4] = [WorkspaceRole::Owner, WorkspaceRole::Admin, WorkspaceRole::Contributor, WorkspaceRole::Viewer];

fn world(roles: &[(usize, bool)]) -> SignedWorld {
    let mut w = SignedWorld::genesis(&NAMES, None);
    for (i, (r, service)) in roles.iter().enumerate() {
        let name = NAMES[i + 1];
        let role = if *service {
            w.kinds.insert(name.into(), ActorKind::Service);
            [WorkspaceRole::Contributor, WorkspaceRole::Viewer][r % 2]
        } else {
            ROLES[*r % 4]
        };
        w.admit("f", name, role);
    }
    w
}

fn payload(w: &SignedWorld, action: &MemberAction) -> SessionEvent {
    match action {
        MemberAction::Remove { member_id } => SessionEvent::MemberRemoved { member_id: member_id.clone(), watermark: vec![] },
        MemberAction::SetRole { member_id, role } => {
            let old = w.config().role_of(member_id);
            SessionEvent::MemberRoleChanged {
                change: MemberRoleChange { member_id: member_id.clone(), old_role: old, new_role: *role },
                watermark: vec![],
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(64), ..ProptestConfig::default() })]

    #[test]
    fn the_ui_offers_exactly_what_the_fold_allows(
        roles in prop::collection::vec((0usize..4, prop::bool::weighted(0.25)), 5),
        author in 0usize..6,
        target in 0usize..6,
        set_role in prop::option::of(0usize..4),
    ) {
        let mut w = world(&roles);
        let author = NAMES[author];
        // The desktop's person: a human.
        prop_assume!(w.kinds.get(author).is_none());
        let action = match set_role {
            None => MemberAction::Remove { member_id: NAMES[target].into() },
            Some(r) => MemberAction::SetRole { member_id: NAMES[target].into(), role: ROLES[r] },
        };
        let config = w.config();
        let ui = member_action_refusal(&w.roster(), &config, author, &action);
        // "Already that role" is the UI's own no-op; the fold has nothing to judge.
        if ui.as_deref() == Some("already that role") {
            return Ok(());
        }
        let ev = payload(&w, &action);
        let id = w.push(author, ev);
        let allowed = w.verdict(id) == Verdict::Allowed;
        prop_assert_eq!(ui.is_none(), allowed, "ui said {:?}, the fold said {}", ui, allowed);
    }
}

#[test]
fn the_named_cases_read_as_the_rank_rules() {
    // f (founder, Owner), p1 Owner, p2 Admin, p3 Admin, p4 Contributor, p5 a service.
    let w = world(&[(0, false), (1, false), (1, false), (2, false), (0, true)]);
    let (r, c) = (w.roster(), w.config());
    let rm = |m: &str| MemberAction::Remove { member_id: m.into() };
    let set = |m: &str, role| MemberAction::SetRole { member_id: m.into(), role };
    // An Admin removing or demoting an Admin: refused.
    assert!(member_action_refusal(&r, &c, "p2", &rm("p3")).is_some());
    assert!(member_action_refusal(&r, &c, "p2", &set("p3", WorkspaceRole::Contributor)).is_some());
    // A non-founder Owner cutting another Owner: refused; the founder may.
    assert!(member_action_refusal(&r, &c, "p1", &set("f", WorkspaceRole::Admin)).is_some());
    assert!(member_action_refusal(&r, &c, "f", &rm("p1")).is_none());
    // Anyone cutting the founder: refused.
    assert!(member_action_refusal(&r, &c, "p1", &rm("f")).is_some());
    // A service member above Contributor: refused; removing it is an Admin's.
    assert!(member_action_refusal(&r, &c, "f", &set("p5", WorkspaceRole::Admin)).is_some());
    assert!(member_action_refusal(&r, &c, "p2", &rm("p5")).is_none());
    // A Contributor removes no one.
    assert!(member_action_refusal(&r, &c, "p4", &rm("p5")).is_some());
    // Leaving yourself stays open.
    assert!(member_action_refusal(&r, &c, "p2", &rm("p2")).is_none());
}

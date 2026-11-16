//! The workspace's passkey relying party (Phase A, item 5): carried by the
//! signed genesis `WorkspaceRulesSet`, folded into the roster, and the only
//! RP presence verifies under. Nothing device-local decides it.

use hive_core::presence::soft::SoftAuthenticator;
use hive_core::presence::PresencePolicy;
use hive_core::testkit::SignedWorld;
use hive_core::{MemberRoleChange, SessionEvent, Verdict, WorkspaceRole};

/// Register `auth` for `who` (proved by their recovery key).
fn register(w: &mut SignedWorld, who: &str, auth: &SoftAuthenticator) -> uuid::Uuid {
    let cred = auth.credential(w.people[who].account_id, "key");
    w.push(who, SessionEvent::PasskeyRegistered { credential: cred })
}

/// A presence-gated event by the Owner, confirmed with `auth`.
fn confirm_with(w: &mut SignedWorld, auth: &SoftAuthenticator) -> uuid::Uuid {
    let change = MemberRoleChange { member_id: "owner".into(), old_role: WorkspaceRole::Owner, new_role: WorkspaceRole::Owner };
    let mut env = w.draft("owner", None, SessionEvent::MemberRoleChanged { change }, None);
    let a = auth.get(&hive_core::presence::challenge(&env));
    w.finish(&mut env, "owner", Some(a));
    w.push_env(None, env)
}

fn soft_for(rp: &str) -> SoftAuthenticator {
    let mut a = SoftAuthenticator::es256();
    a.rp_id = rp.into();
    a.origin = format!("https://{rp}");
    a
}

#[test]
fn presence_verifies_under_the_workspaces_rp_and_not_another() {
    let mut w = SignedWorld::genesis(&["owner"], Some(PresencePolicy::for_rp("example.org")));
    assert_eq!(w.roster().presence_policy.rp_id, "example.org");
    let ours = soft_for("example.org");
    let theirs = soft_for("ligus.ai");
    register(&mut w, "owner", &ours);
    register(&mut w, "owner", &theirs);
    let ok = confirm_with(&mut w, &ours);
    let wrong = confirm_with(&mut w, &theirs);
    assert_eq!(w.verdict(ok), Verdict::Allowed);
    assert_eq!(w.verdict(wrong), Verdict::Denied, "a ligus.ai assertion doesn't verify in an example.org workspace");
}

#[test]
fn a_workspace_without_a_policy_uses_ligus_ai() {
    let mut w = SignedWorld::genesis(&["owner"], None);
    assert_eq!(w.roster().presence_policy, PresencePolicy::ligus());
    let ligus = soft_for("ligus.ai");
    let other = soft_for("example.org");
    register(&mut w, "owner", &ligus);
    register(&mut w, "owner", &other);
    let ok = confirm_with(&mut w, &ligus);
    let wrong = confirm_with(&mut w, &other);
    assert_eq!(w.verdict(ok), Verdict::Allowed);
    assert_eq!(w.verdict(wrong), Verdict::Denied);
}

#[test]
fn the_rp_is_fixed_once_a_passkey_is_registered() {
    let mut w = SignedWorld::genesis(&["owner"], Some(PresencePolicy::for_rp("example.org")));
    // Only the recovery key so far: the RP may still move.
    let early = w.push("owner", SessionEvent::WorkspaceRulesSet { version: 2, presence: Some(PresencePolicy::for_rp("corp.example")) });
    assert_eq!(w.verdict(early), Verdict::Allowed);
    let key = soft_for("corp.example");
    register(&mut w, "owner", &key);
    let moved = w.push("owner", SessionEvent::WorkspaceRulesSet { version: 2, presence: Some(PresencePolicy::for_rp("example.org")) });
    assert_eq!(w.verdict(moved), Verdict::Denied);
    // More origins on the same RP are fine.
    let mut more = PresencePolicy::for_rp("corp.example");
    more.origins.push("https://id.corp.example".into());
    let added = w.push("owner", SessionEvent::WorkspaceRulesSet { version: 2, presence: Some(more) });
    assert_eq!(w.verdict(added), Verdict::Allowed);
    assert_eq!(w.roster().presence_policy.origins.len(), 2);
}

#[test]
fn a_malformed_policy_is_refused() {
    let mut w = SignedWorld::genesis(&["owner"], None);
    for bad in [
        PresencePolicy { rp_id: "Ligus.AI".into(), origins: vec!["https://ligus.ai".into()] },
        PresencePolicy { rp_id: "ligus.ai".into(), origins: vec![] },
        PresencePolicy { rp_id: "ligus.ai".into(), origins: vec!["http://ligus.ai".into()] },
        PresencePolicy { rp_id: "ligus.ai".into(), origins: vec!["https://evil.example".into()] },
    ] {
        let id = w.push("owner", SessionEvent::WorkspaceRulesSet { version: 2, presence: Some(bad.clone()) });
        assert_eq!(w.verdict(id), Verdict::Denied, "{bad:?}");
    }
    assert_eq!(w.roster().presence_policy, PresencePolicy::ligus());
}

#[test]
fn only_the_owner_sets_the_policy() {
    let mut w = SignedWorld::genesis(&["owner", "ada"], None);
    w.admit("owner", "ada", WorkspaceRole::Admin);
    let id = w.push("ada", SessionEvent::WorkspaceRulesSet { version: 2, presence: Some(PresencePolicy::for_rp("example.org")) });
    assert_eq!(w.verdict(id), Verdict::Denied);
}

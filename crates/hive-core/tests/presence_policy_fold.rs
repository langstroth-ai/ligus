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
    let mut env = w.draft("owner", None, SessionEvent::MemberRoleChanged { change, watermark: Vec::new() }, None);
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

/// Changing the RP needs presence verified under the OLD relying party:
/// not none, and not an assertion made under the new one.
#[test]
fn the_rp_changes_only_with_presence_under_the_old_rp() {
    let mut w = SignedWorld::genesis(&["owner"], Some(PresencePolicy::for_rp("corp.example")));
    let old = soft_for("corp.example");
    let new = soft_for("example.org");
    register(&mut w, "owner", &old);
    register(&mut w, "owner", &new);
    let to_new = SessionEvent::WorkspaceRulesSet { version: 2, presence: Some(PresencePolicy::for_rp("example.org")) };
    let bare = w.push_bare("owner", to_new.clone());
    assert_eq!(w.verdict(bare), Verdict::Denied, "no presence");
    let confirm = |w: &mut SignedWorld, auth: &SoftAuthenticator| {
        let mut env = w.draft("owner", None, to_new.clone(), None);
        let a = auth.get(&hive_core::presence::challenge(&env));
        w.finish(&mut env, "owner", Some(a));
        w.push_env(None, env)
    };
    let under_new = confirm(&mut w, &new);
    assert_eq!(w.verdict(under_new), Verdict::Denied, "presence under the new RP doesn't authorize the switch");
    assert_eq!(w.roster().presence_policy.rp_id, "corp.example");
    let under_old = confirm(&mut w, &old);
    assert_eq!(w.verdict(under_old), Verdict::Allowed);
    assert_eq!(w.roster().presence_policy.rp_id, "example.org");
    // From here the new RP's passkey verifies and the old one doesn't.
    let (a, b) = (confirm_with(&mut w, &new), confirm_with(&mut w, &old));
    assert_eq!((w.verdict(a), w.verdict(b)), (Verdict::Allowed, Verdict::Denied));
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

/// Only the Owner changes the relying party (it decides which page every
/// member's desktop opens for a passkey ceremony) or the rules version; an
/// Admin or a Contributor does neither.
#[test]
fn only_the_owner_sets_the_policy_or_the_version() {
    let mut w = SignedWorld::genesis(&["owner", "ada", "mara"], None);
    w.admit("owner", "ada", WorkspaceRole::Admin);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    let policy = w.push("ada", SessionEvent::WorkspaceRulesSet { version: 2, presence: Some(PresencePolicy::for_rp("example.org")) });
    assert_eq!(w.verdict(policy), Verdict::Denied);
    let same = w.push("ada", SessionEvent::WorkspaceRulesSet { version: 2, presence: Some(PresencePolicy::ligus()) });
    assert_eq!(w.verdict(same), Verdict::Allowed, "restating the policy in force changes nothing");
    let by_owner_rp = w.push("owner", SessionEvent::WorkspaceRulesSet { version: 2, presence: Some(PresencePolicy::for_rp("example.org")) });
    assert_eq!(w.verdict(by_owner_rp), Verdict::Allowed);
    let version = w.push("ada", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    assert_eq!(w.verdict(version), Verdict::Denied);
    let by_contributor = w.push("mara", SessionEvent::WorkspaceRulesSet { version: 2, presence: Some(PresencePolicy::for_rp("example.org")) });
    assert_eq!(w.verdict(by_contributor), Verdict::Denied);
    let by_owner = w.push("owner", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    assert_eq!(w.verdict(by_owner), Verdict::Allowed);
}

//! Phase A, item 1: every workspace is on rules v2 from its seed. There is no
//! rules v1 and no trust-on-first-use passkey: the founder's first credential
//! registers with the seed's one grant, everyone else's with an Admin's
//! presence-confirmed grant.

use ligus_core::presence::soft::SoftAuthenticator;
use ligus_core::testkit::{Proof, SignedWorld};
use ligus_core::{project_config, SessionEvent, Verdict, WorkspaceRole};

#[test]
fn a_seed_alone_is_rules_v2() {
    let w = SignedWorld::genesis(&["owner"], None);
    // Even a log whose genesis rules set never arrived folds as v2.
    let only_seed: Vec<_> = w.cfg.iter().take(1).cloned().collect();
    let (_, t) = project_config(&only_seed, None, None);
    assert_eq!(t.current().rules, 2);
    assert_eq!(w.roster().rules, 2);
}

#[test]
fn governance_needs_presence_from_the_first_event() {
    let mut w = SignedWorld::genesis(&["owner", "bea"], None);
    let bea = w.people["bea"].member(WorkspaceRole::Contributor);
    let bare = w.push_bare("owner", SessionEvent::MemberAdded { member: bea.clone(), invite: None, proof: None });
    assert_eq!(w.verdict(bare), Verdict::Denied);
    let confirmed = w.push("owner", SessionEvent::MemberAdded { member: bea, invite: None, proof: None });
    assert_eq!(w.verdict(confirmed), Verdict::Allowed);
}

#[test]
fn the_founders_grant_is_one_use_and_nobody_elses() {
    let mut w = SignedWorld::genesis(&["owner", "bea"], None);
    // The founder used their grant on the recovery key at genesis: a
    // passkey now needs that key's presence, not the device key alone.
    let key = SoftAuthenticator::es256();
    let cred = key.credential(w.people["owner"].account_id, "k");
    let mut env = w.draft("owner", None, SessionEvent::PasskeyRegistered { credential: cred.clone() }, None);
    let a = key.get(&ligus_core::presence::challenge(&env));
    w.finish(&mut env, "owner", Some(a));
    let self_proved = w.push_env(None, env);
    assert_eq!(w.verdict(self_proved), Verdict::Denied, "a second credential proved only by itself");
    // A stranger who published an identity has no grant at all.
    w.publish_identity("bea");
    let bea = w.people["bea"].account_id;
    let mut env = w.draft("bea", None, ligus_core::testkit::recovery_registration(bea), None);
    let p = &w.people["bea"];
    ligus_core::testkit::prove_recovery(&mut env, bea, p.device_id, &p.device);
    let stranger = w.push_env(None, env);
    assert_eq!(w.verdict(stranger), Verdict::Denied, "no trust on first use for an outsider");
}

#[test]
fn the_genesis_rules_set_needs_no_presence_only_before_any_credential() {
    let w = SignedWorld::genesis(&["owner"], None);
    // Before any credential: a bare genesis rules set folds.
    let mut early = SignedWorld { cfg: w.cfg[..3].to_vec(), ..w.clone() };
    let id = early.write("owner", None, SessionEvent::WorkspaceRulesSet { version: 2, presence: None }, None, Proof::None);
    assert_eq!(early.verdict(id), Verdict::Allowed);
    // But not a different version, even then.
    let raise = early.write("owner", None, SessionEvent::WorkspaceRulesSet { version: 3, presence: None }, None, Proof::None);
    assert_eq!(early.verdict(raise), Verdict::Denied);
    // After the founder's recovery key: presence is needed.
    let mut later = w.clone();
    let bare = later.push_bare("owner", SessionEvent::WorkspaceRulesSet { version: 2, presence: None });
    assert_eq!(later.verdict(bare), Verdict::Denied);
}

#[test]
fn rules_v1_never_folds() {
    let mut w = SignedWorld::genesis(&["owner"], None);
    let id = w.push("owner", SessionEvent::WorkspaceRulesSet { version: 1, presence: None });
    assert_eq!(w.verdict(id), Verdict::Denied);
    assert_eq!(w.roster().rules, 2);
}

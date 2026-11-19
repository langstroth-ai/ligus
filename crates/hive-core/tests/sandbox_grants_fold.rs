//! Phase C: the sandbox grants (`WorkspaceSandboxSet`) on the config log.
//! Path grants are an Admin's with presence; an agent's shell grant is the
//! Owner's with presence; agents, phones, Contributors and chat logs never set
//! them. The property test runs 256 cases by default (`PROPTEST_CASES=4096`
//! in release for the gate).

use hive_core::sandbox::{AgentGrant, SandboxGrants};
use hive_core::testkit::{Proof, SignedWorld};
use hive_core::{ActorKind, SessionEvent, Verdict, WorkspaceRole};
use proptest::prelude::*;
use uuid::Uuid;

fn agent(i: u128) -> Uuid {
    Uuid::from_u128(0xa000 + i)
}

fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["owner", "adm", "mara", "bot"], None);
    w.admit("owner", "adm", WorkspaceRole::Admin);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    w
}

fn set(g: &SandboxGrants) -> SessionEvent {
    SessionEvent::WorkspaceSandboxSet { grants: g.clone() }
}

fn paths(ro: &[&str], rw: &[&str]) -> SandboxGrants {
    let mut g = SandboxGrants::default();
    g.workspace.read_only = ro.iter().map(|s| s.to_string()).collect();
    g.workspace.read_write = rw.iter().map(|s| s.to_string()).collect();
    g
}

fn shell_for(id: Uuid) -> SandboxGrants {
    let mut g = SandboxGrants::default();
    g.agents.insert(id, AgentGrant { shell: true, ..Default::default() });
    g
}

#[test]
fn an_admin_with_presence_sets_path_grants() {
    let mut w = world();
    let g = paths(&["~/.m2/repository"], &["~/scratch"]);
    let id = w.push("adm", set(&g));
    assert_eq!(w.verdict(id), Verdict::Allowed);
    assert_eq!(w.config().guards.sandbox_grants, g);
}

#[test]
fn path_grants_need_presence_and_an_admin() {
    let mut w = world();
    let g = paths(&["~/data"], &[]);
    let bare = w.push_bare("adm", set(&g));
    assert_eq!(w.verdict(bare), Verdict::Denied, "no presence");
    let by_contributor = w.push("mara", set(&g));
    assert_eq!(w.verdict(by_contributor), Verdict::Denied, "a Contributor");
    assert!(w.config().guards.sandbox_grants.is_empty());
}

#[test]
fn only_the_owner_grants_or_withdraws_a_shell() {
    let mut w = world();
    let a = agent(1);
    let by_admin = w.push("adm", set(&shell_for(a)));
    assert_eq!(w.verdict(by_admin), Verdict::Denied, "an Admin can't grant a shell");
    let by_owner_bare = w.push_bare("owner", set(&shell_for(a)));
    assert_eq!(w.verdict(by_owner_bare), Verdict::Denied, "the Owner needs presence too");
    let by_owner = w.push("owner", set(&shell_for(a)));
    assert_eq!(w.verdict(by_owner), Verdict::Allowed);
    assert!(w.config().guards.sandbox_grants.shell_granted(a));
    // An Admin may change paths while keeping the shell grant as it is ...
    let mut keep = shell_for(a);
    keep.agents.get_mut(&a).unwrap().read_only.push("/opt/data".into());
    let keeps = w.push("adm", set(&keep));
    assert_eq!(w.verdict(keeps), Verdict::Allowed);
    // ... but can't withdraw it either (dropping the agent's entry is a
    // shell change).
    let drops = w.push("adm", set(&SandboxGrants::default()));
    assert_eq!(w.verdict(drops), Verdict::Denied);
    assert!(w.config().guards.sandbox_grants.shell_granted(a));
    let owner_drops = w.push("owner", set(&SandboxGrants::default()));
    assert_eq!(w.verdict(owner_drops), Verdict::Allowed);
    assert!(w.config().guards.sandbox_grants.is_empty());
}

#[test]
fn agents_chats_and_malformed_sets_are_refused() {
    let mut w = world();
    // An agent-kind author (no agent key, no presence of its own).
    w.kinds.insert("bot".into(), ActorKind::Agent);
    let by_agent = w.push("bot", set(&paths(&["~/x"], &[])));
    assert_eq!(w.verdict(by_agent), Verdict::Denied);
    // On a chat log.
    let chat = w.chat("owner", "c");
    let on_chat = w.write("owner", Some(chat), set(&paths(&["~/x"], &[])), None, Proof::Recovery);
    assert_ne!(w.verdicts().get(&on_chat), Some(&Verdict::Allowed));
    // Malformed paths.
    for bad in ["~", "/", "rel", "~/../etc", "/a//b"] {
        let id = w.push("owner", set(&paths(&[bad], &[])));
        assert_eq!(w.verdict(id), Verdict::Denied, "{bad}");
    }
    assert!(w.config().guards.sandbox_grants.is_empty());
}

// ---------------------------------------------------------------------------
// Property: the folded grants are exactly what a reference model of the
// rules gives, and a shell grant only ever changes by the Owner's hand.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Step {
    by: usize,
    presence: bool,
    grants: SandboxGrants,
}

fn arb_path() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("~/.m2/repository".to_string()),
        Just("~/scratch".to_string()),
        Just("/opt/data".to_string()),
        // Malformed ones the fold refuses.
        Just("~".to_string()),
        Just("rel/x".to_string()),
        Just("~/../etc".to_string()),
    ]
}

fn arb_grants() -> impl Strategy<Value = SandboxGrants> {
    (
        proptest::collection::vec(arb_path(), 0..3),
        proptest::collection::vec(arb_path(), 0..2),
        proptest::collection::vec((0u128..3, any::<bool>(), proptest::collection::vec(arb_path(), 0..2)), 0..3),
    )
        .prop_map(|(ro, rw, agents)| {
            let mut g = SandboxGrants::default();
            g.workspace.read_only = ro;
            g.workspace.read_write = rw;
            for (i, shell, ro) in agents {
                g.agents.insert(agent(i), AgentGrant { read_only: ro, read_write: Vec::new(), shell });
            }
            g
        })
}

fn arb_step() -> impl Strategy<Value = Step> {
    (0usize..4, any::<bool>(), arb_grants()).prop_map(|(by, presence, grants)| Step { by, presence, grants })
}

const WHO: [&str; 4] = ["owner", "adm", "mara", "bot"];

proptest! {
    #[test]
    fn prop_sandbox_grants_follow_the_rules(steps in proptest::collection::vec(arb_step(), 1..8)) {
        let mut w = world();
        w.kinds.insert("bot".into(), ActorKind::Agent);
        let mut model = SandboxGrants::default();
        let mut ids = Vec::new();
        for s in &steps {
            let by = WHO[s.by];
            let proof = if s.presence { Proof::Recovery } else { Proof::None };
            let id = w.write(by, None, set(&s.grants), None, proof);
            let role = match by { "owner" => Some(WorkspaceRole::Owner), "adm" => Some(WorkspaceRole::Admin), "mara" => Some(WorkspaceRole::Contributor), _ => None };
            let allowed = s.presence
                && role.is_some_and(|r| r.rank() >= WorkspaceRole::Admin.rank())
                && s.grants.well_formed().is_ok()
                && (s.grants.shell_changes(&model).is_empty() || role == Some(WorkspaceRole::Owner));
            if allowed {
                model = s.grants.clone();
            }
            ids.push((id, allowed));
        }
        for (id, allowed) in &ids {
            prop_assert_eq!(w.verdict(*id) == Verdict::Allowed, *allowed);
        }
        prop_assert_eq!(&w.config().guards.sandbox_grants, &model);
        // Arrival order doesn't matter: the log folded from its events in
        // reverse gives the same grants.
        let mut rev = w.cfg.clone();
        rev.reverse();
        let folded = hive_core::events::project_config(&rev, None, None).0.unwrap();
        prop_assert_eq!(&folded.guards.sandbox_grants, &model);
    }
}

//! Agent access controls: who may use an agent (`access`) and its daily
//! budgets (`limits`, `notifyOnUse`) are the agent's owner's to change, and
//! only the owner's, whatever another member's role; on an unowned (shared)
//! agent they are an Admin's. The property test runs 256 cases by default
//! (`PROPTEST_CASES=4096` in release for the gate).

use ligus_core::agent::{AgentLimits, ACCESS_EVERYONE, ACCESS_OWNER};
use ligus_core::testkit::{Proof, SignedWorld};
use ligus_core::{SessionEvent, Verdict, WorkspaceAgent, WorkspaceRole};
use proptest::prelude::*;
use uuid::Uuid;

fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["owner", "adm", "mara", "carl"], None);
    w.admit("owner", "adm", WorkspaceRole::Admin);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    w.admit("owner", "carl", WorkspaceRole::Contributor);
    w
}

fn agent(id: u128, name: &str, owner: &str) -> WorkspaceAgent {
    let mut a = WorkspaceAgent::new(name, "rt");
    a.id = Uuid::from_u128(id);
    a.owner_actor_id = owner.into();
    a.created_at = ligus_core::Timestamp::from_unix_seconds(0);
    a
}

fn up(a: &WorkspaceAgent) -> SessionEvent {
    SessionEvent::AgentUpserted { agent: a.clone() }
}

/// A config event by `by`, with presence (adding an agent with a runtime
/// needs it under rules v2; changing who may use one doesn't).
fn bare(w: &mut SignedWorld, by: &str, e: SessionEvent) -> Uuid {
    w.write(by, None, e, None, Proof::Recovery)
}

/// Push a config event and return its verdict.
fn judge(w: &mut SignedWorld, by: &str, e: SessionEvent) -> Verdict {
    let id = bare(w, by, e);
    w.verdict(id)
}

fn folded(w: &SignedWorld, id: Uuid) -> WorkspaceAgent {
    w.config()
        .workspace_agents
        .into_iter()
        .find(|a| a.id == id)
        .expect("agent folded")
}

#[test]
fn the_owner_opens_their_agent_and_no_one_else_can() {
    let mut w = world();
    let mut a = agent(1, "scout", "mara");
    assert_eq!(judge(&mut w, "mara", up(&a)), Verdict::Allowed);
    assert_eq!(
        folded(&w, a.id).effective_access(),
        ligus_core::agent::Access::Owner,
        "personal: only me"
    );

    // An Admin, and even the workspace Owner, can't open it up.
    a.access = ACCESS_EVERYONE.into();
    for by in ["adm", "owner", "carl"] {
        let id = bare(&mut w, by, up(&a));
        assert_eq!(w.verdict(id), Verdict::Denied, "{by}");
    }
    assert!(folded(&w, a.id).access.is_empty());

    // The owner can.
    assert_eq!(judge(&mut w, "mara", up(&a)), Verdict::Allowed);
    assert_eq!(folded(&w, a.id).access, ACCESS_EVERYONE);

    // An Admin's rename that keeps the settings is still an Admin's edit.
    let mut renamed = a.clone();
    renamed.name = "scout2".into();
    assert_eq!(judge(&mut w, "adm", up(&renamed)), Verdict::Allowed);
    // ... but one that drops them (or sets budgets) is refused.
    let mut dropped = renamed.clone();
    dropped.access.clear();
    assert_eq!(judge(&mut w, "adm", up(&dropped)), Verdict::Denied);
    let mut budget = renamed.clone();
    budget.limits = AgentLimits {
        member_turns_per_day: Some(0),
        usd_cents_per_day: None,
    };
    assert_eq!(judge(&mut w, "adm", up(&budget)), Verdict::Denied);
    let mut notify = renamed.clone();
    notify.notify_on_use = true;
    assert_eq!(judge(&mut w, "adm", up(&notify)), Verdict::Denied);
    assert_eq!(folded(&w, a.id).access, ACCESS_EVERYONE);
    assert!(folded(&w, a.id).limits.is_default());
}

#[test]
fn a_shared_agent_is_an_admins_to_restrict() {
    let mut w = world();
    let mut b = agent(2, "helper", "");
    assert_eq!(
        judge(&mut w, "carl", up(&b)),
        Verdict::Allowed,
        "a Contributor adds a shared agent"
    );
    b.access = ACCESS_OWNER.into();
    assert_eq!(
        judge(&mut w, "carl", up(&b)),
        Verdict::Denied,
        "but can't restrict it"
    );
    assert_eq!(
        judge(&mut w, "adm", up(&b)),
        Verdict::Allowed,
        "an Admin can"
    );
    assert_eq!(folded(&w, b.id).access, ACCESS_OWNER);
    // A new shared agent with settings: an Admin's too.
    let mut c = agent(3, "capped", "");
    c.limits.usd_cents_per_day = Some(500);
    assert_eq!(judge(&mut w, "mara", up(&c)), Verdict::Denied);
    assert_eq!(judge(&mut w, "adm", up(&c)), Verdict::Allowed);
}

#[test]
fn a_new_agent_carries_settings_only_from_its_owner() {
    let mut w = world();
    // An Admin creating an agent owned by someone else (an Admin act) can't
    // open it up on their behalf.
    let mut a = agent(4, "theirs", "mara");
    a.access = ACCESS_EVERYONE.into();
    assert_eq!(judge(&mut w, "adm", up(&a)), Verdict::Denied);
    a.access.clear();
    assert_eq!(judge(&mut w, "adm", up(&a)), Verdict::Allowed);
    // Its owner creating it open: fine.
    let mut own = agent(5, "mine", "carl");
    own.access = ACCESS_EVERYONE.into();
    own.limits.member_turns_per_day = Some(10);
    assert_eq!(judge(&mut w, "carl", up(&own)), Verdict::Allowed);
}

#[test]
fn a_chat_copy_cant_open_up_the_workspace_agent() {
    let mut w = world();
    let a = agent(6, "scout", "mara");
    bare(&mut w, "mara", up(&a));
    let chat = w.chat("adm", "c");
    let mut open = a.clone();
    open.access = ACCESS_EVERYONE.into();
    let id = w.write("adm", Some(chat), up(&open), None, Proof::Recovery);
    assert_ne!(
        w.verdicts().get(&id),
        Some(&Verdict::Allowed),
        "an Admin's chat-level copy that opens it up"
    );
    assert!(w
        .state(chat)
        .workspace_agents
        .iter()
        .all(|x| x.access.is_empty()));
}

/// Opening an agent to everyone spends its owner's credentials on other
/// people's requests: under rules v2 it takes the owner's presence, like
/// rebinding its host or runtime (a stolen device key alone can't open it).
/// Narrowing it and changing its budgets or notice need none.
#[test]
fn opening_an_agent_needs_presence_narrowing_doesnt() {
    let mut w = world();
    let mut a = agent(8, "scout", "mara");
    bare(&mut w, "mara", up(&a));
    a.limits.member_turns_per_day = Some(3);
    a.notify_on_use = true;
    let id = w.write("mara", None, up(&a), None, Proof::None);
    assert_eq!(
        w.verdict(id),
        Verdict::Allowed,
        "budgets and the notice: no presence"
    );
    a.access = ACCESS_EVERYONE.into();
    let id = w.write("mara", None, up(&a), None, Proof::None);
    assert_eq!(w.verdict(id), Verdict::Denied, "opening without presence");
    let id = w.write("mara", None, up(&a), None, Proof::Recovery);
    assert_eq!(w.verdict(id), Verdict::Allowed, "opening with presence");
    a.access = ACCESS_OWNER.into();
    let id = w.write("mara", None, up(&a), None, Proof::None);
    assert_eq!(w.verdict(id), Verdict::Allowed, "narrowing: no presence");
    // A new personal agent created open needs it too; a roster replace that
    // opens one does.
    let mut b = agent(9, "open", "mara");
    b.runtime_id.clear();
    b.access = ACCESS_EVERYONE.into();
    let id = w.write("mara", None, up(&b), None, Proof::None);
    assert_eq!(w.verdict(id), Verdict::Denied);
    let mut opened = a.clone();
    opened.access = ACCESS_EVERYONE.into();
    let id = w.write(
        "mara",
        None,
        SessionEvent::AgentRosterUpdated {
            agents: vec![opened],
        },
        None,
        Proof::None,
    );
    assert_eq!(w.verdict(id), Verdict::Denied);
}

#[test]
fn a_whole_roster_replace_is_judged_per_agent() {
    let mut w = world();
    let a = agent(7, "scout", "mara");
    bare(&mut w, "mara", up(&a));
    let mut open = a.clone();
    open.access = ACCESS_EVERYONE.into();
    let id = bare(
        &mut w,
        "adm",
        SessionEvent::AgentRosterUpdated {
            agents: vec![open.clone()],
        },
    );
    assert_eq!(w.verdict(id), Verdict::Denied);
    let id = bare(
        &mut w,
        "mara",
        SessionEvent::AgentRosterUpdated { agents: vec![open] },
    );
    assert_eq!(w.verdict(id), Verdict::Allowed);
}

// ---------------------------------------------------------------------------
// Property: the folded settings are exactly what a reference model of the
// rule gives (only the owner changes a personal agent's, only an Admin a
// shared one's), and arrival order doesn't change the fold.
// ---------------------------------------------------------------------------

const WHO: [&str; 4] = ["owner", "adm", "mara", "carl"];

fn rank(by: &str) -> WorkspaceRole {
    match by {
        "owner" => WorkspaceRole::Owner,
        "adm" => WorkspaceRole::Admin,
        _ => WorkspaceRole::Contributor,
    }
}

#[derive(Debug, Clone)]
struct Step {
    by: usize,
    which: usize,
    access: Option<&'static str>,
    turns: Option<u32>,
    cents: Option<u64>,
    notify: bool,
}

fn arb_step() -> impl Strategy<Value = Step> {
    (
        0usize..4,
        0usize..2,
        prop_oneof![
            Just(None),
            Just(Some(ACCESS_OWNER)),
            Just(Some(ACCESS_EVERYONE)),
            Just(Some("members"))
        ],
        prop_oneof![Just(None), Just(Some(0u32)), Just(Some(5u32))],
        prop_oneof![Just(None), Just(Some(250u64))],
        any::<bool>(),
    )
        .prop_map(|(by, which, access, turns, cents, notify)| Step {
            by,
            which,
            access,
            turns,
            cents,
            notify,
        })
}

proptest! {
    #[test]
    fn prop_only_the_owner_or_an_admin_for_shared_changes_access(steps in proptest::collection::vec(arb_step(), 1..10)) {
        let mut w = world();
        // A personal agent of mara's and a shared one.
        let mut model = [agent(10, "mine", "mara"), agent(11, "shared", "")];
        bare(&mut w, "mara", up(&model[0]));
        bare(&mut w, "owner", up(&model[1]));
        let mut ids = Vec::new();
        for s in &steps {
            let by = WHO[s.by];
            let mut next = model[s.which].clone();
            next.access = s.access.unwrap_or("").to_string();
            next.limits = AgentLimits { member_turns_per_day: s.turns, usd_cents_per_day: s.cents };
            next.notify_on_use = s.notify;
            let id = bare(&mut w, by, up(&next));
            let owner = &model[s.which].owner_actor_id;
            let changed = next.policy() != model[s.which].policy();
            let allowed = !changed || if owner.is_empty() { rank(by).rank() >= WorkspaceRole::Admin.rank() } else { by == owner };
            if allowed {
                model[s.which] = next;
            }
            ids.push((id, allowed));
        }
        for (id, allowed) in &ids {
            prop_assert_eq!(w.verdict(*id) == Verdict::Allowed, *allowed);
        }
        let check = |agents: &[WorkspaceAgent]| -> Result<(), TestCaseError> {
            for m in &model {
                let got = agents.iter().find(|a| a.id == m.id).expect("agent");
                prop_assert_eq!(got.policy(), m.policy());
            }
            Ok(())
        };
        check(&w.config().workspace_agents)?;
        // Arrival order doesn't matter: the reversed log folds the same.
        let mut rev = w.cfg.clone();
        rev.reverse();
        let folded = ligus_core::events::project_config(&rev, None, None).0.unwrap();
        check(&folded.workspace_agents)?;
        // And the roster authorization reads carries the same settings.
        let roster = w.roster();
        for m in &model {
            prop_assert_eq!(&roster.agents[&m.id].policy, &m.policy());
        }
    }
}

/// The golden `agentUpserted/access` line decodes to the settings it pins,
/// and still verifies byte for byte (`raw_bytes.rs` checks every line).
#[test]
fn the_access_golden_decodes() {
    let line = include_str!("golden/envelopes_v3.jsonl")
        .lines()
        .find(|l| l.contains("\"label\":\"agentUpserted/access\""))
        .expect("golden line");
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    let env: ligus_core::SessionEventEnvelope =
        serde_json::from_value(v["envelope"].clone()).unwrap();
    let SessionEvent::AgentUpserted { agent } = env.payload else {
        panic!("kind")
    };
    assert_eq!(agent.access, ACCESS_EVERYONE);
    assert_eq!(
        agent.limits,
        AgentLimits {
            member_turns_per_day: Some(20),
            usd_cents_per_day: Some(500)
        }
    );
    assert!(agent.notify_on_use);
}

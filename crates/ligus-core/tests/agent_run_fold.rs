//! An agent's run and placement (workspace model v3, 5.2, 5.3 and 5.6) in
//! the signed fold, on rules 3: sticky against older writers, legacy wins,
//! authz judged on the merged record, `host_gone` for a former member's
//! host, and the record a 0.4 build reads (its `WorkspaceAgent`, pinned
//! below). The property tests run 64 cases (each one folds a whole signed
//! workspace).

use ligus_core::agent_run::{legacy_id, AgentRun, Placement, ResolvedRun};
use ligus_core::events::{project_config, Verdict};
use ligus_core::testkit::{Proof, SignedWorld};
use ligus_core::{HostKind, SessionEvent, Timestamp, WorkspaceAgent, WorkspaceHost, WorkspaceRole};
use proptest::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `WorkspaceAgent` as 0.4.1 serializes and reads it (no `run`, no
/// `placement`): the pinned older writer and reader.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentV04 {
    id: Uuid,
    name: String,
    runtime_id: String,
    #[serde(default)]
    owner_actor_id: String,
    #[serde(default)]
    role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    avatar_color_hex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    avatar_url: Option<String>,
    #[serde(default)]
    host_id: String,
    #[serde(default)]
    created_at: Timestamp,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    access: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limits: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    notify_on_use: bool,
}

/// What a 0.4 build reads from a 0.5 record.
fn as_read_by_v04(a: &WorkspaceAgent) -> AgentV04 {
    serde_json::from_value(serde_json::to_value(a).unwrap()).unwrap()
}

/// The upsert a 0.4 build writes after editing `a` as it read it.
fn v04_upsert(a: &AgentV04) -> SessionEvent {
    let agent: WorkspaceAgent = serde_json::from_value(serde_json::to_value(a).unwrap()).unwrap();
    assert!(agent.run.is_none() && agent.placement.is_none(), "a 0.4 record has no sticky fields");
    SessionEvent::AgentUpserted { agent }
}

/// Ana (Owner), Ben (Admin), Cy and Dee (Contributors), on rules 3. Cy and
/// Dee each register their own computer as a host.
fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["ana", "ben", "cy", "dee"], None);
    w.admit("ana", "ben", WorkspaceRole::Admin);
    w.admit("ana", "cy", WorkspaceRole::Contributor);
    w.admit("ana", "dee", WorkspaceRole::Contributor);
    w.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    for who in ["cy", "dee"] {
        let mut host = WorkspaceHost::new(host_of(&w, who), HostKind::Device, format!("{who}-mbp"));
        host.owner_actor_id = who.into();
        w.push_bare(who, SessionEvent::WorkspaceHostUpserted { host });
    }
    w
}

fn host_of(w: &SignedWorld, who: &str) -> String {
    w.people[who].device_id.to_string()
}

const AGENT: u128 = 0xa9e;

/// Cy's agent, written by this build (run and placement mirrored), on Cy's
/// own computer.
fn cy_agent(w: &SignedWorld) -> WorkspaceAgent {
    let mut a = WorkspaceAgent::new("scout", "ws-claude");
    a.id = Uuid::from_u128(AGENT);
    a.owner_actor_id = "cy".into();
    a.created_at = Timestamp::from_unix_seconds(0);
    a.host_id = host_of(w, "cy");
    a.for_write(Some(ResolvedRun { harness: "claudeCode".into(), model: "opus".into() }))
}

fn up(a: &WorkspaceAgent) -> SessionEvent {
    SessionEvent::AgentUpserted { agent: a.clone() }
}

fn folded(w: &SignedWorld) -> WorkspaceAgent {
    w.config().workspace_agents.into_iter().find(|a| a.id == Uuid::from_u128(AGENT)).expect("agent folded")
}

/// A world with Cy's agent in it (an Admin creates it: a new agent with a
/// host is an Admin action under rules 3).
fn with_agent() -> (SignedWorld, WorkspaceAgent) {
    let mut w = world();
    let a = cy_agent(&w);
    let id = w.push("ben", up(&a));
    assert_eq!(w.verdict(id), Verdict::Allowed);
    let a = folded(&w);
    assert_eq!(a.placement, Some(Placement::Host { host_id: host_of(&w, "cy") }));
    (w, a)
}

/// H8: a 0.4 rename keeps run and placement; a 0.4 move (an Admin's) moves
/// the agent on every build, and the placement follows.
#[test]
fn an_older_rename_keeps_the_sticky_fields_and_an_older_move_wins() {
    let (mut w, a) = with_agent();
    let mut old = as_read_by_v04(&a);
    assert_eq!((old.runtime_id.as_str(), old.host_id.as_str()), ("ws-claude", host_of(&w, "cy").as_str()), "0.4 reads the same routing");
    old.name = "scout-2".into();
    let id = w.push_bare("cy", v04_upsert(&old));
    assert_eq!(w.verdict(id), Verdict::Allowed, "the owner renames without presence");
    let after = folded(&w);
    assert_eq!(after.name, "scout-2");
    assert_eq!((after.run.clone(), after.placement.clone()), (a.run.clone(), a.placement.clone()), "kept");

    old.host_id = String::new();
    let id = w.push("ben", v04_upsert(&old));
    assert_eq!(w.verdict(id), Verdict::Allowed, "an Admin moves it with presence");
    let moved = folded(&w);
    assert_eq!(moved.placement, Some(Placement::OwnerDevices), "legacy wins");
    assert_eq!(moved.host_id, "");
}

/// H10 on rules 3: a Contributor's 0.4-shaped upsert that moves their agent
/// to someone else's host is denied (with or without presence); a 0.5-shaped
/// one whose placement names that host but whose `host_id` doesn't move
/// nothing.
#[test]
fn an_old_shaped_upsert_cant_escalate() {
    let (mut w, a) = with_agent();
    let mut old = as_read_by_v04(&a);
    old.host_id = host_of(&w, "dee");
    for proof in [Proof::None, Proof::Recovery] {
        let id = w.write("cy", None, v04_upsert(&old), None, proof);
        assert_eq!(w.verdict(id), Verdict::Denied, "a Contributor moves an agent onto Dee's computer");
    }
    assert_eq!(folded(&w).host_id, host_of(&w, "cy"));

    let mut sneaky = a.clone();
    sneaky.placement = Some(Placement::Host { host_id: host_of(&w, "dee") });
    sneaky.run = Some(AgentRun { harness: "codex".into(), source: "codex".into(), model: "gpt-5.3-codex".into(), resolved: None });
    let id = w.push_bare("cy", up(&sneaky));
    let after = folded(&w);
    assert_eq!(after.effective_placement(), Placement::Host { host_id: host_of(&w, "cy") }, "{:?}", w.verdict(id));
    assert_eq!(after.effective_run().runtime_ref(), Some("ws-claude"), "rules 3: a run spec is inert");
}

/// H15: on rules 3 the M13 write (run and placement mirrored, `runtime_id`
/// untouched) is the owner's own, without an Admin or presence, and the
/// fold keeps it.
#[test]
fn the_m13_write_needs_no_admin_or_presence() {
    let mut w = world();
    let mut a = WorkspaceAgent::new("mine", "ws-claude");
    a.id = Uuid::from_u128(AGENT);
    a.owner_actor_id = "cy".into();
    let old = as_read_by_v04(&a);
    let id = w.push("cy", v04_upsert(&old));
    assert_eq!(w.verdict(id), Verdict::Allowed);
    assert!(folded(&w).run.is_none());
    let m13 = folded(&w).for_write(Some(ResolvedRun { harness: "claudeCode".into(), model: "opus".into() }));
    let id = w.push_bare("cy", up(&m13));
    assert_eq!(w.verdict(id), Verdict::Allowed);
    let f = folded(&w);
    assert_eq!(f.runtime_id, "ws-claude", "never rewritten");
    assert_eq!(f.run.as_ref().and_then(|r| r.runtime_ref()), Some("ws-claude"));
    assert_eq!(f.run.as_ref().and_then(|r| r.resolved.clone()).map(|r| r.model), Some("opus".into()));
    assert_eq!(f.placement, Some(Placement::OwnerDevices));
}

/// 5.3: when the host's owner leaves, the agent is `host_gone`: it keeps
/// naming the host, older builds read `host_id = ""` as today, and a 0.5
/// rename by its owner keeps it gone. Placing it again clears it.
#[test]
fn a_former_members_host_leaves_its_agents_host_gone() {
    let mut w = world();
    let mut a = WorkspaceAgent::new("boxed", "ws-claude");
    a.id = Uuid::from_u128(AGENT);
    a.owner_actor_id = "cy".into();
    a.host_id = host_of(&w, "dee");
    let id = w.push("ben", up(&a.for_write(None)));
    assert_eq!(w.verdict(id), Verdict::Allowed);
    // An honest removal: Dee's chain so far stands (her host record too).
    let dee = std::collections::BTreeSet::from([w.people["dee"].device_id]);
    let watermark = ligus_core::watermark::heads_of(&w.cfg, ligus_core::workspace_config_session_id(w.wid), &dee);
    w.push("ana", SessionEvent::MemberRemoved { member_id: "dee".into(), watermark });
    let gone_fold = w.config();
    let gone = folded(&w);
    assert!(gone.host_gone && gone.is_host_gone());
    assert_eq!(gone.host_id, "", "older readers: unbound, as today");
    assert_eq!(gone.effective_placement(), Placement::Host { host_id: host_of(&w, "dee") });
    assert_eq!(as_read_by_v04(&gone).host_id, "");
    assert!(serde_json::to_value(&gone).unwrap().get("hostGone").is_none(), "fold-only, never written");

    let mut renamed = gone.clone();
    renamed.name = "boxed-2".into();
    // The author's own device judges its write against the finished fold
    // (as `ChatService` does before signing) and agrees with the fold: the
    // rename is fine, writing `host_id = ""` is a move.
    let roster = w.roster();
    let judge_here = |w: &mut SignedWorld, a: &WorkspaceAgent| {
        let env = w.draft("cy", None, up(a), None);
        let ctx = ligus_core::authz::Ctx { fold: &gone_fold, running: &roster, workspace: None, trusted: false, checked: false };
        ligus_core::authz::authorize(&env, &ctx) == ligus_core::authz::Decision::Allow
    };
    let mut as_04 = renamed.clone();
    as_04.run = None;
    as_04.placement = None;
    assert!(judge_here(&mut w, &renamed.for_write(None)));
    assert!(!judge_here(&mut w, &as_04), "host_id \"\" from the finished fold is a move");
    let id = w.push_bare("cy", up(&renamed.for_write(None)));
    assert_eq!(w.verdict(id), Verdict::Allowed, "a rename isn't a move");
    let f = folded(&w);
    assert!(f.is_host_gone() && f.name == "boxed-2");

    let mut placed = f.clone();
    placed.place(&host_of(&w, "cy"));
    let id = w.push("ben", up(&placed.for_write(None)));
    assert_eq!(w.verdict(id), Verdict::Allowed);
    let f = folded(&w);
    assert!(!f.is_host_gone() && f.host_id == host_of(&w, "cy"));
}

/// 5.6 rule 4: a removal starts the record over.
#[test]
fn a_removal_drops_the_sticky_fields() {
    let (mut w, a) = with_agent();
    w.push("cy", SessionEvent::AgentRemoved { agent_id: a.id });
    let mut again = as_read_by_v04(&a);
    again.host_id = String::new();
    w.push("cy", v04_upsert(&again));
    let f = folded(&w);
    assert!(f.run.is_none() && f.placement.is_none(), "{f:?}");
}

fn arb_run() -> impl Strategy<Value = Option<AgentRun>> {
    prop_oneof![
        Just(None),
        Just(Some(AgentRun::mirror("ws-claude", None))),
        Just(Some(AgentRun::mirror("rt-2", None))),
        Just(Some(AgentRun { harness: "codex".into(), source: "codex".into(), model: "gpt-5.3-codex".into(), resolved: None })),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// H10 (rules 3): whatever shape a Contributor's upsert of their own
    /// agent has (sticky fields absent, agreeing or disagreeing; legacy
    /// fields kept or changed), without an Admin the effective host,
    /// placement, runtime and run source never change.
    #[test]
    fn a_contributor_never_moves_their_agent(
        host_pick in 0usize..3,
        runtime in proptest::sample::select(vec!["ws-claude".to_string(), "rt-2".to_string(), legacy_id(&AgentRun { source: "codex".into(), model: "x".into(), ..Default::default() })]),
        pl_seed in any::<u8>(),
        run in arb_run(),
        proof in any::<bool>(),
    ) {
        let (mut w, before) = with_agent();
        let hosts = vec![String::new(), host_of(&w, "cy"), host_of(&w, "dee")];
        let mut p = before.clone();
        p.host_id = hosts[host_pick].clone();
        p.runtime_id = runtime;
        p.run = run;
        let pls = [None, Some(Placement::OwnerDevices), Some(Placement::Host { host_id: host_of(&w, "dee") }), Some(Placement::Pool), Some(Placement::Unknown), Some(Placement::Host { host_id: host_of(&w, "cy") })];
        p.placement = pls[pl_seed as usize % pls.len()].clone();
        p.name = "renamed".into();
        let id = w.write("cy", None, up(&p), None, if proof { Proof::Recovery } else { Proof::None });
        let after = folded(&w);
        if w.verdict(id) == Verdict::Allowed {
            prop_assert!(!after.moves_from(&before), "moved: {before:?} -> {after:?}");
        } else {
            prop_assert_eq!(after, before);
        }
    }

    /// L1: the fold is a function of the event set: agent upserts from old
    /// (no sticky fields) and new writers, in any arrival order, give the
    /// same agents, sticky fields and legacy-wins included.
    #[test]
    fn any_arrival_order_gives_the_same_agent(
        steps in proptest::collection::vec((any::<bool>(), 0usize..3, any::<bool>(), any::<u8>()), 1..6),
        seed in any::<u64>(),
    ) {
        let (mut w, base) = with_agent();
        let hosts = vec![String::new(), host_of(&w, "cy"), host_of(&w, "dee")];
        for (i, (old_writer, h, new_rt, pl)) in steps.iter().enumerate() {
            let mut a = base.clone();
            a.name = format!("n{i}");
            a.host_id = hosts[*h].clone();
            if *new_rt { a.runtime_id = "rt-2".into(); }
            let ev = if *old_writer {
                v04_upsert(&as_read_by_v04(&a))
            } else {
                let mut a = a.for_write(None);
                if pl % 3 == 0 { a.placement = Some(Placement::Host { host_id: hosts[(*pl as usize) % 3].clone() }); }
                up(&a)
            };
            w.push("ben", ev);
        }
        let reference = project_config(&w.cfg, None, None).0.unwrap().workspace_agents;
        // A deterministic shuffle of everything after the seed.
        let mut shuffled = w.cfg.clone();
        let mut s = seed | 1;
        for i in (2..shuffled.len()).rev() {
            s ^= s << 13; s ^= s >> 7; s ^= s << 17;
            let j = 1 + (s as usize) % i;
            shuffled.swap(i, j);
        }
        let again = project_config(&shuffled, None, None).0.unwrap().workspace_agents;
        prop_assert_eq!(again, reference);
    }
}

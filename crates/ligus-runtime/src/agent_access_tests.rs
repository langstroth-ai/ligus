//! Agent access controls through the chat service: the decision a device
//! that runs an agent makes for a real trigger chain (a mention, a cascade,
//! a hand-off, a workflow stage), the refusal note, the daily budgets and
//! the usage view.

use std::path::Path;

use ligus_core::agent::{AgentLimits, ACCESS_EVERYONE};
use ligus_core::crypto::SigningKeypair;
use ligus_core::{ActorIdentity, ActorKind, MessageRole, Timestamp, WorkspaceAgent, WorkspaceMember, WorkspaceRole};
use uuid::Uuid;

use crate::agent_access::{AccessDecision, AgentUse};
use crate::{ChatService, EventStore};

fn svc_on(path: &Path, id: &str, name: &str) -> ChatService {
    let store = EventStore::open(path).unwrap();
    ChatService::new(store, SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), ActorIdentity::new(id, name, ActorKind::Human))
}

fn member(id: &str, name: &str, role: WorkspaceRole) -> WorkspaceMember {
    WorkspaceMember { id: id.into(), actor: ActorIdentity::new(id, name, ActorKind::Human), role, title: String::new(), index: 0, joined_at: Timestamp::now() }
}

struct Fx {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    wid: Uuid,
    chat: Uuid,
    /// The owner's device (Ana, `u1`), which runs `scout`.
    host: ChatService,
    scout: WorkspaceAgent,
}

/// Ana (`u1`, the workspace Owner) owns `scout`, a personal agent, and runs
/// it; an Admin (`adm`) and a Contributor (`con`) share the chat.
fn fixture() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ligus.db");
    let wid = Uuid::new_v4();
    let mut host = svc_on(&path, "u1", "Ana");
    let chat = host.create_chat("A", wid, "rt").unwrap().id;
    host.add_member(chat, wid, member("adm", "Adm", WorkspaceRole::Admin)).unwrap();
    host.add_member(chat, wid, member("con", "Con", WorkspaceRole::Contributor)).unwrap();
    let mut scout = WorkspaceAgent::new("scout", "rt");
    scout.owner_actor_id = "u1".into();
    host.add_agent(chat, wid, scout.clone()).unwrap();
    Fx { _dir: dir, path, wid, chat, host, scout }
}

fn post(fx: &Fx, who: &str, body: &str) -> Uuid {
    svc_on(&fx.path, who, who).post_user_message(fx.chat, fx.wid, body).unwrap().id
}

fn agent_reply(svc: &mut ChatService, (chat, wid): (Uuid, Uuid), agent: &WorkspaceAgent, to: Uuid, body: &str) -> Uuid {
    let id = svc.begin_assistant_message(chat, wid, agent.name.clone(), "rt", Some(agent.id), Some(to)).unwrap();
    svc.complete_assistant_message(chat, wid, id, body).unwrap();
    id
}

fn open(fx: &mut Fx) {
    fx.scout.access = ACCESS_EVERYONE.into();
    fx.host.add_agent(fx.chat, fx.wid, fx.scout.clone()).unwrap();
}

const ONLY_OWNER: &str = "scout is only available to its owner.";

#[test]
fn a_teammate_is_refused_the_owner_runs_and_opening_it_lets_them_in() {
    let mut fx = fixture();
    let con = post(&fx, "con", "@scout fix the build");
    let adm = post(&fx, "adm", "@scout fix the build");
    let own = fx.host.post_user_message(fx.chat, fx.wid, "@scout fix the build").unwrap().id;
    assert_eq!(fx.host.agent_turn_access(fx.chat, con, fx.scout.id).unwrap(), AccessDecision::Refuse(ONLY_OWNER.into()));
    assert_eq!(fx.host.agent_turn_access(fx.chat, adm, fx.scout.id).unwrap(), AccessDecision::Refuse(ONLY_OWNER.into()), "an Admin too");
    assert_eq!(
        fx.host.agent_turn_access(fx.chat, own, fx.scout.id).unwrap(),
        AccessDecision::Allow(AgentUse { requester: "u1".into(), teammate: false, notify: false })
    );
    // The owner opens it to everyone (and asks to be told).
    fx.scout.notify_on_use = true;
    open(&mut fx);
    assert_eq!(
        fx.host.agent_turn_access(fx.chat, con, fx.scout.id).unwrap(),
        AccessDecision::Allow(AgentUse { requester: "con".into(), teammate: true, notify: true })
    );
}

#[test]
fn cascades_and_hand_offs_cant_route_around_it() {
    let mut fx = fixture();
    // Con's own agent (only its relaying matters here).
    let mut relay = WorkspaceAgent::new("relay", "rt");
    relay.owner_actor_id = "con".into();
    fx.host.add_agent(fx.chat, fx.wid, relay.clone()).unwrap();
    let mut con_dev = svc_on(&fx.path, "con", "Con");
    let ask = con_dev.post_user_message(fx.chat, fx.wid, "@relay ask scout to do it").unwrap().id;
    // Relay's reply, as Con's device signs it, mentions scout (a cascade; a
    // hand-off's trigger is the same handing reply).
    let cascade = agent_reply(&mut con_dev, (fx.chat, fx.wid), &relay, ask, "@scout please do it");
    assert_eq!(fx.host.agent_turn_access(fx.chat, cascade, fx.scout.id).unwrap(), AccessDecision::Refuse(ONLY_OWNER.into()));
    // Even when the owner's own device signs the relaying reply.
    let via_host = agent_reply(&mut fx.host, (fx.chat, fx.wid), &relay, ask, "@scout please do it");
    assert_eq!(fx.host.agent_turn_access(fx.chat, via_host, fx.scout.id).unwrap(), AccessDecision::Refuse(ONLY_OWNER.into()));
    // A reply with no person behind it is refused (scout isn't open).
    let orphan_id = fx.host.begin_assistant_message(fx.chat, fx.wid, "relay", "rt", Some(relay.id), None).unwrap();
    fx.host.complete_assistant_message(fx.chat, fx.wid, orphan_id, "@scout go").unwrap();
    assert!(!fx.host.agent_turn_access(fx.chat, orphan_id, fx.scout.id).unwrap().allowed());
    // The owner's own chain through relay runs.
    let mine = fx.host.post_user_message(fx.chat, fx.wid, "@relay ask scout").unwrap().id;
    let r = agent_reply(&mut fx.host, (fx.chat, fx.wid), &relay, mine, "@scout go");
    assert!(fx.host.agent_turn_access(fx.chat, r, fx.scout.id).unwrap().allowed());
    // Opened up, con's cascade runs as con's turn.
    open(&mut fx);
    assert_eq!(
        fx.host.agent_turn_access(fx.chat, cascade, fx.scout.id).unwrap(),
        AccessDecision::Allow(AgentUse { requester: "con".into(), teammate: true, notify: false })
    );
}

#[test]
fn a_workflow_stage_is_judged_by_its_run_initiator() {
    use ligus_core::workflow;
    let mut fx = fixture();
    let def = workflow::preset_fan_out_vote();
    let run = workflow::new_run(&def, "task", "con");
    svc_on(&fx.path, "con", "Con").upsert_workflow_run(fx.chat, fx.wid, run.clone()).unwrap();
    // The owner's device drives the run and posts the stage prompt.
    let stage = fx.host.post_workflow_stage_prompt(fx.chat, fx.wid, "stage", run.id).unwrap().id;
    assert_eq!(fx.host.agent_turn_access(fx.chat, stage, fx.scout.id).unwrap(), AccessDecision::Refuse(ONLY_OWNER.into()));
    let own = workflow::new_run(&def, "task", "u1");
    fx.host.upsert_workflow_run(fx.chat, fx.wid, own.clone()).unwrap();
    let stage = fx.host.post_workflow_stage_prompt(fx.chat, fx.wid, "stage", own.id).unwrap().id;
    assert!(fx.host.agent_turn_access(fx.chat, stage, fx.scout.id).unwrap().allowed());
}

/// The refusal answers the trigger once, in the agent's name, so neither a
/// host's queue nor the sender's deferred fallback picks it up again.
#[test]
fn a_refusal_answers_the_trigger_once() {
    let mut fx = fixture();
    let con = post(&fx, "con", "@scout fix the build");
    let s = fx.host.load(fx.chat).unwrap().unwrap();
    assert!(crate::pending_turns(&s).iter().any(|p| p.message_id == con), "precondition: queued");
    assert_eq!(fx.host.gate_agent_turn(fx.chat, fx.wid, con, fx.scout.id).unwrap(), Err(ONLY_OWNER.to_string()));
    // Another device (the sender's fallback) refuses again: no second note.
    let mut con_dev = svc_on(&fx.path, "con", "Con");
    assert_eq!(con_dev.gate_agent_turn(fx.chat, fx.wid, con, fx.scout.id).unwrap(), Err(ONLY_OWNER.to_string()));
    let s = fx.host.load(fx.chat).unwrap().unwrap();
    let notes: Vec<_> = s.messages.iter().filter(|m| m.reply_to_message_id == Some(con)).collect();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].body, ONLY_OWNER);
    assert_eq!(notes[0].role, MessageRole::Assistant);
    assert_eq!(notes[0].responder_id, Some(fx.scout.id));
    assert!(!notes[0].is_streaming);
    assert!(!crate::pending_turns(&s).iter().any(|p| p.message_id == con), "answered: not queued");
    assert!(crate::turn_dispatch::user_turn_answered(&s, con));
    assert!(!crate::turn_dispatch::turn_still_open(&s, con), "the deferred fallback stands down");
    assert!(crate::cascade_target(&s, notes[0]).is_none(), "the note hands nothing on");
    // Nothing counted.
    assert!(fx.host.agent_usage_view(fx.scout.id).unwrap().today.is_empty());
}

#[test]
fn budgets_count_per_member_per_day_and_reset() {
    let mut fx = fixture();
    fx.scout.limits = AgentLimits { member_turns_per_day: Some(2), usd_cents_per_day: Some(100) };
    open(&mut fx);
    let con = post(&fx, "con", "@scout one");
    let adm = post(&fx, "adm", "@scout one");
    let own = fx.host.post_user_message(fx.chat, fx.wid, "@scout one").unwrap().id;
    for _ in 0..2 {
        fx.host.record_agent_turn(fx.wid, fx.scout.id, "con").unwrap();
    }
    let limit = "scout reached today's limit of 2 turns per member (resets at 00:00 UTC).";
    assert_eq!(fx.host.agent_turn_access(fx.chat, con, fx.scout.id).unwrap(), AccessDecision::Refuse(limit.into()));
    assert!(fx.host.agent_turn_access(fx.chat, adm, fx.scout.id).unwrap().allowed(), "per member");
    // Yesterday's turns don't count today.
    let yesterday = crate::event_store::utc_day(time::OffsetDateTime::now_utc() - time::Duration::days(1));
    for _ in 0..5 {
        fx.host.store().record_agent_turn(fx.wid, fx.scout.id, "adm", &yesterday).unwrap();
    }
    assert!(fx.host.agent_turn_access(fx.chat, adm, fx.scout.id).unwrap().allowed(), "a new day starts over");
    // The day's spend reaches the $1.00 cap: others are refused, the owner runs.
    let day = fx.host.record_agent_turn(fx.wid, fx.scout.id, "u1").unwrap();
    fx.host.record_agent_cost(fx.wid, fx.scout.id, "u1", &day, Some(1.0)).unwrap();
    assert!(matches!(fx.host.agent_turn_access(fx.chat, adm, fx.scout.id).unwrap(), AccessDecision::Refuse(n) if n.contains("spending cap of $1.00")));
    assert!(fx.host.agent_turn_access(fx.chat, own, fx.scout.id).unwrap().allowed());
}

#[test]
fn the_usage_view_counts_turns_and_costs() {
    let fx = fixture();
    let day = fx.host.record_agent_turn(fx.wid, fx.scout.id, "con").unwrap();
    fx.host.record_agent_cost(fx.wid, fx.scout.id, "con", &day, Some(0.12)).unwrap();
    fx.host.record_agent_turn(fx.wid, fx.scout.id, "con").unwrap();
    fx.host.record_agent_cost(fx.wid, fx.scout.id, "con", &day, None).unwrap();
    fx.host.record_agent_turn(fx.wid, fx.scout.id, "u1").unwrap();
    let month_day = format!("{}-01", &day[..7]);
    if month_day != day {
        fx.host.store().record_agent_turn(fx.wid, fx.scout.id, "con", &month_day).unwrap();
    }
    let v = fx.host.agent_usage_view(fx.scout.id).unwrap();
    assert_eq!(v.day, day);
    let con = v.today.iter().find(|r| r.member == "con").unwrap();
    assert_eq!((con.turns, con.uncosted), (2, 1));
    assert!((con.cost_usd - 0.12).abs() < 1e-9);
    assert_eq!(v.today.iter().find(|r| r.member == "u1").unwrap().turns, 1);
    let month = v.month.iter().find(|r| r.member == "con").unwrap();
    assert_eq!(month.turns, if month_day != day { 3 } else { 2 });
}

/// The device-local workspace (one person, never synced) never refuses.
#[test]
fn the_local_workspace_is_always_the_owners() {
    let dir = tempfile::tempdir().unwrap();
    let mut solo = svc_on(&dir.path().join("l.db"), "me", "Me");
    let local = Uuid::new_v4();
    solo.set_local_workspace(local);
    let c = solo.create_chat("solo", local, "rt").unwrap().id;
    let mut a = WorkspaceAgent::new("scout", "rt");
    a.owner_actor_id = "stale-self".into();
    solo.add_agent(c, local, a.clone()).unwrap();
    let m = solo.post_user_message(c, local, "@scout go").unwrap().id;
    assert!(solo.agent_turn_access(c, m, a.id).unwrap().allowed());
}

#[test]
fn only_the_owner_sets_access_through_the_service() {
    let mut fx = fixture();
    let policy = ligus_core::AgentPolicy { access: ACCESS_EVERYONE.into(), limits: AgentLimits { member_turns_per_day: Some(5), usd_cents_per_day: None }, notify_on_use: true };
    let mut adm = svc_on(&fx.path, "adm", "Adm");
    assert!(!adm.may_manage_agent(fx.wid, &fx.scout));
    assert!(adm.set_agent_policy(fx.wid, fx.scout.id, policy.clone()).is_err(), "an Admin can't open someone's agent");
    assert!(fx.host.may_manage_agent(fx.wid, &fx.scout));
    let bad = ligus_core::AgentPolicy { access: "members".into(), ..policy.clone() };
    assert!(fx.host.set_agent_policy(fx.wid, fx.scout.id, bad).is_err(), "only owner or everyone");
    fx.host.set_agent_policy(fx.wid, fx.scout.id, policy.clone()).unwrap();
    let s = fx.host.load(fx.chat).unwrap().unwrap();
    assert_eq!(s.workspace_agents.iter().find(|a| a.id == fx.scout.id).unwrap().policy(), policy);
    // A shared agent: an Admin manages it, a Contributor doesn't.
    let shared = WorkspaceAgent::new("helper", "rt");
    fx.host.add_agent(fx.chat, fx.wid, shared.clone()).unwrap();
    assert!(adm.may_manage_agent(fx.wid, &shared));
    assert!(!svc_on(&fx.path, "con", "Con").may_manage_agent(fx.wid, &shared));
}

/// A turn stamped as answering a System note that trails the message (the
/// "last message" rule) is judged by the message before the note.
#[test]
fn a_trailing_system_note_doesnt_hide_or_lend_the_asker() {
    let mut fx = fixture();
    let own = fx.host.post_user_message(fx.chat, fx.wid, "@scout go").unwrap().id;
    let _ = own;
    fx.host.post_system_note(fx.chat, fx.wid, "routed").unwrap();
    let note = fx.host.load(fx.chat).unwrap().unwrap().messages.last().unwrap().id;
    assert!(fx.host.agent_turn_access(fx.chat, note, fx.scout.id).unwrap().allowed());
    post(&fx, "con", "@scout go");
    fx.host.post_system_note(fx.chat, fx.wid, "routed").unwrap();
    let note = fx.host.load(fx.chat).unwrap().unwrap().messages.last().unwrap().id;
    assert_eq!(fx.host.agent_turn_access(fx.chat, note, fx.scout.id).unwrap(), AccessDecision::Refuse(ONLY_OWNER.into()));
}

/// The deferred (offline) fallback: the owner's device stays silent, so the
/// sender's device would answer the turn itself. For someone else's
/// personal agent it refuses instead: the note, no run, nothing counted on
/// the sender's device, and the turn is closed for every device.
#[test]
fn the_senders_deferred_fallback_refuses_someone_elses_agent() {
    let fx = fixture();
    let mut sender = svc_on(&fx.path, "con", "Con");
    let ask = sender.post_user_message(fx.chat, fx.wid, "@scout run the tests").unwrap().id;
    let s = sender.load(fx.chat).unwrap().unwrap();
    assert!(crate::turn_dispatch::deferred_turn_unanswered(&s, ask), "precondition: the fallback would take it");
    assert_eq!(sender.gate_agent_turn(fx.chat, fx.wid, ask, fx.scout.id).unwrap(), Err(ONLY_OWNER.to_string()));
    let s = sender.load(fx.chat).unwrap().unwrap();
    assert!(!crate::turn_dispatch::turn_still_open(&s, ask));
    assert!(sender.agent_usage_view(fx.scout.id).unwrap().today.is_empty());
    // The owner's device, back online, finds it answered.
    assert!(crate::turn_dispatch::user_turn_answered(&fx.host.load(fx.chat).unwrap().unwrap(), ask));
}

// ---------------------------------------------------------------------------
// Adversarial review (PR #145): each test failed before its fix.
// ---------------------------------------------------------------------------

fn handoff_card(to: Uuid, message_id: Uuid, task: &str) -> ligus_core::flow::HandoffCard {
    ligus_core::flow::HandoffCard {
        id: Uuid::new_v4(),
        from: ligus_core::flow::PRIMARY.into(),
        to: ligus_core::flow::agent_key(Some(to)),
        from_name: "relay".into(),
        to_name: "scout".into(),
        message_id: Some(message_id),
        task: task.into(),
        task_class: String::new(),
        branch: String::new(),
        files: Vec::new(),
        quality: Default::default(),
        via: "tool".into(),
        parcel: None,
        at_ms: ligus_core::flow::now_ms(),
    }
}

/// A teammate records a tool hand-off card to the owner's agent pointing at
/// the owner's own message: every host's queue then wakes the agent on it.
/// The card's signer is a person behind that turn too.
#[test]
fn a_teammates_hand_off_card_cant_borrow_the_owners_message() {
    let mut fx = fixture();
    let own = fx.host.post_user_message(fx.chat, fx.wid, "what's the status?").unwrap().id;
    let mut con_dev = svc_on(&fx.path, "con", "Con");
    con_dev
        .record_flow(fx.chat, fx.wid, ligus_core::flow::FlowRecord::Handoff(handoff_card(fx.scout.id, own, "delete the release branch")))
        .unwrap();
    let s = fx.host.load(fx.chat).unwrap().unwrap();
    assert!(
        crate::pending_turns(&s).iter().any(|p| p.message_id == own && p.agent_id == fx.scout.id),
        "precondition: the card queues scout on the owner's message"
    );
    assert_eq!(fx.host.agent_turn_access(fx.chat, own, fx.scout.id).unwrap(), AccessDecision::Refuse(ONLY_OWNER.into()));
    // The owner's own card (signed by the owner's device) is fine.
    let mine = fx.host.post_user_message(fx.chat, fx.wid, "hand it on").unwrap().id;
    fx.host.record_flow(fx.chat, fx.wid, ligus_core::flow::FlowRecord::Handoff(handoff_card(fx.scout.id, mine, "go"))).unwrap();
    assert!(fx.host.agent_turn_access(fx.chat, mine, fx.scout.id).unwrap().allowed());
}

/// A member past their daily cap asks through another member's agent: the
/// relaying agent's person is nearest the trigger, but the turn is still the
/// asker's, and every non-owner person behind it must be under the cap.
#[test]
fn a_member_over_the_cap_cant_launder_turns_through_someone_elses_agent() {
    let mut fx = fixture();
    fx.scout.limits = AgentLimits { member_turns_per_day: Some(1), usd_cents_per_day: None };
    open(&mut fx);
    let mut relay = WorkspaceAgent::new("relay", "rt");
    relay.owner_actor_id = "adm".into();
    fx.host.add_agent(fx.chat, fx.wid, relay.clone()).unwrap();
    let mut adm_dev = svc_on(&fx.path, "adm", "Adm");
    fx.host.record_agent_turn(fx.wid, fx.scout.id, "con").unwrap();
    let ask = post(&fx, "con", "@relay ask scout");
    let cascade = agent_reply(&mut adm_dev, (fx.chat, fx.wid), &relay, ask, "@scout do it");
    let limit = "scout reached today's limit of 1 turn per member (resets at 00:00 UTC).";
    assert_eq!(fx.host.agent_turn_access(fx.chat, cascade, fx.scout.id).unwrap(), AccessDecision::Refuse(limit.into()));
    // Under the cap, the turn is counted against the person who asked.
    fx.scout.limits.member_turns_per_day = Some(5);
    fx.host.add_agent(fx.chat, fx.wid, fx.scout.clone()).unwrap();
    match fx.host.agent_turn_access(fx.chat, cascade, fx.scout.id).unwrap() {
        AccessDecision::Allow(u) => assert_eq!(u.requester, "con"),
        r => panic!("{r:?}"),
    }
}

/// A member spamming mentions of an agent they can't use gets one note per
/// agent per few minutes; later refusals are silent (and still refused).
#[test]
fn refusal_notes_are_rate_limited_per_member_per_agent() {
    let mut fx = fixture();
    let asks: Vec<Uuid> = (0..5).map(|i| post(&fx, "con", &format!("@scout {i}"))).collect();
    for a in &asks {
        assert_eq!(fx.host.gate_agent_turn(fx.chat, fx.wid, *a, fx.scout.id).unwrap(), Err(ONLY_OWNER.to_string()));
    }
    let s = fx.host.load(fx.chat).unwrap().unwrap();
    let notes = s.messages.iter().filter(|m| m.responder_id == Some(fx.scout.id)).count();
    assert_eq!(notes, 1, "one note, the rest silent");
    // Another member gets their own note.
    let adm = post(&fx, "adm", "@scout hi");
    assert!(fx.host.gate_agent_turn(fx.chat, fx.wid, adm, fx.scout.id).unwrap().is_err());
    let s = fx.host.load(fx.chat).unwrap().unwrap();
    assert_eq!(s.messages.iter().filter(|m| m.responder_id == Some(fx.scout.id)).count(), 2);
}

/// Gating and counting are one step: two turns admitted back to back can't
/// both pass a cap of one.
#[test]
fn admitting_a_turn_counts_it_in_the_same_step() {
    let mut fx = fixture();
    fx.scout.limits = AgentLimits { member_turns_per_day: Some(1), usd_cents_per_day: None };
    open(&mut fx);
    let a = post(&fx, "con", "@scout one");
    let b = post(&fx, "con", "@scout two");
    assert!(fx.host.admit_agent_turn(fx.chat, fx.wid, a, fx.scout.id).unwrap().is_ok());
    assert!(fx.host.admit_agent_turn(fx.chat, fx.wid, b, fx.scout.id).unwrap().is_err());
}

/// An agent whose owner id is a stale identity of this device's own person
/// (a reset, an import) in a workspace where they are the only person: the
/// device answers it, like dispatch's solo rule. A real other person isn't.
#[test]
fn a_stale_owner_id_doesnt_lock_out_the_only_person() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("l.db");
    let mut me = svc_on(&path, "me", "Me");
    let wid = Uuid::new_v4();
    let c = me.create_chat("solo", wid, "rt").unwrap().id;
    let mut a = WorkspaceAgent::new("scout", "rt");
    a.owner_actor_id = "stale-self".into();
    me.add_agent(c, wid, a.clone()).unwrap();
    let m = me.post_user_message(c, wid, "@scout go").unwrap().id;
    assert!(me.agent_turn_access(c, m, a.id).unwrap().allowed(), "the only person runs it");
    // With a second person in the room the owner id means someone else.
    me.add_member(c, wid, member("con", "Con", WorkspaceRole::Contributor)).unwrap();
    let theirs = svc_on(&path, "con", "Con").post_user_message(c, wid, "@scout go").unwrap().id;
    assert!(!me.agent_turn_access(c, theirs, a.id).unwrap().allowed());
}

/// `@ligus` (the chat's primary runtime) runs on its device's credentials:
/// in a shared room only that device's person may use it, unless they open
/// it; alone, or in the device-local workspace, it always answers.
#[test]
fn the_primary_is_its_devices_person_unless_opened() {
    let mut fx = fixture();
    let con = post(&fx, "con", "@ligus summarize");
    let own = fx.host.post_user_message(fx.chat, fx.wid, "@ligus summarize").unwrap().id;
    assert!(!fx.host.primary_turn_access(fx.chat, con, false).unwrap().allowed());
    assert!(fx.host.primary_turn_access(fx.chat, own, false).unwrap().allowed());
    assert!(fx.host.primary_turn_access(fx.chat, con, true).unwrap().allowed(), "opened");
    // Refused with a note in Ligus's name, once.
    assert!(fx.host.gate_primary_turn(fx.chat, fx.wid, con, false).unwrap().is_err());
    let s = fx.host.load(fx.chat).unwrap().unwrap();
    let notes: Vec<_> = s.messages.iter().filter(|m| m.reply_to_message_id == Some(con)).collect();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].responder_id.is_none() && notes[0].role == MessageRole::Assistant);
    assert!(crate::turn_dispatch::user_turn_answered(&s, con));
}

/// A silently refused turn stays refused on this device without being
/// judged again (a host's queue asks every tick).
#[test]
fn a_silent_refusal_is_remembered() {
    let mut fx = fixture();
    let first = post(&fx, "con", "@scout one");
    let second = post(&fx, "con", "@scout two");
    assert!(fx.host.gate_agent_turn(fx.chat, fx.wid, first, fx.scout.id).unwrap().is_err());
    assert_eq!(fx.host.gate_agent_turn(fx.chat, fx.wid, second, fx.scout.id).unwrap(), Err(ONLY_OWNER.to_string()));
    // Opened meanwhile: the remembered refusal still stands for that turn
    // (ask again), while a new one runs.
    open(&mut fx);
    assert!(fx.host.gate_agent_turn(fx.chat, fx.wid, second, fx.scout.id).unwrap().is_err());
    let third = post(&fx, "con", "@scout three");
    assert!(fx.host.gate_agent_turn(fx.chat, fx.wid, third, fx.scout.id).unwrap().is_ok());
}

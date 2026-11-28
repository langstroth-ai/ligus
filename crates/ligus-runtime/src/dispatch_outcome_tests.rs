//! The dispatch outcome end to end through the chat service (workspace
//! model v3, 5.4 and 8.1): an agent on an offline host is queued, then reads
//! as offline past the window; "Run here this time" on its owner's device
//! runs it exactly once (the host that comes back stands down); nobody else
//! answers it silently; the reply reports what ran it.

use std::path::Path;

use ligus_core::crypto::SigningKeypair;
use ligus_core::outcome::{GoneReason, ReplyStatus, TurnRoute, Why};
use ligus_core::{ActorIdentity, ActorKind, HostKind, MessageRole, Timestamp, WorkspaceAgent, WorkspaceHost, WorkspaceMember, WorkspaceRole};
use uuid::Uuid;

use crate::turn_dispatch::{host_binding, runs_here_linked};
use crate::turn_route::{self as tr, Here, HostFacts, QUEUE_WINDOW_SECS};
use crate::{ChatService, DispatchCtx, EventStore};

fn svc_on(path: &Path, id: &str, name: &str) -> ChatService {
    let store = EventStore::open(path).unwrap();
    let mut svc = ChatService::new(store, SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), ActorIdentity::new(id, name, ActorKind::Human));
    // A fixture of accountless, named members (the rules-3 shape, which
    // rules 4 refuses: D12): found its workspaces on rules 3. The rules-4
    // paths (delegated turns, full run specs) are in `chat_service::agent_run`.
    svc.set_founding_rules(ligus_core::authz::RULES_V3);
    svc
}

fn member(id: &str, name: &str) -> WorkspaceMember {
    WorkspaceMember {
        id: id.into(),
        actor: ActorIdentity::new(id, name, ActorKind::Human),
        role: WorkspaceRole::Contributor,
        title: String::new(),
        index: 0,
        joined_at: Timestamp::now(),
    }
}

struct Fx {
    _dir: tempfile::TempDir,
    wid: Uuid,
    chat: Uuid,
    /// Ana's desktop (`u1`).
    desk: ChatService,
    /// Ana's box (`u1` too, another device): the host `coder` is placed on.
    boxed: ChatService,
    /// Ben, another person in the chat.
    ben: ChatService,
    coder: WorkspaceAgent,
}

fn fixture() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ligus.db");
    let wid = Uuid::new_v4();
    let mut desk = svc_on(&path, "u1", "Ana");
    let chat = desk.create_chat("A", wid, "rt").unwrap().id;
    desk.add_member(chat, wid, member("ben", "Ben")).unwrap();
    let mut boxed = svc_on(&path, "u1", "Ana");
    boxed.ensure_host(wid, WorkspaceHost::new(boxed.device_id().to_string(), HostKind::Worker, "box-1".to_string())).unwrap();
    let mut coder = WorkspaceAgent::new("coder", "rt");
    coder.owner_actor_id = "u1".into();
    coder.access = ligus_core::agent::ACCESS_EVERYONE.into();
    coder.place(&boxed.device_id().to_string());
    desk.add_agent(chat, wid, coder.clone()).unwrap();
    let ben = svc_on(&path, "ben", "Ben");
    Fx { _dir: dir, wid, chat, desk, boxed, ben, coder }
}

/// The route `svc` computes for `trigger` at `now`, with every host offline
/// and this device resolving the agent's runtime.
fn route_seen(svc: &ChatService, fx: &Fx, trigger: Uuid, now: Timestamp) -> Option<TurnRoute> {
    let s = svc.load_in(fx.wid, fx.chat).unwrap().unwrap();
    let m = s.messages.iter().find(|m| m.id == trigger).unwrap().clone();
    let facts = HostFacts::load(svc, fx.wid);
    let offline = |_: &WorkspaceHost| false;
    let status = |_: &str| None;
    let view = facts.view(&offline, &status);
    let links = svc.device_links(fx.wid).unwrap_or_default();
    let me = svc.author().id.clone();
    let here_dev = svc.device_id().to_string();
    let here = |a: Option<&WorkspaceAgent>| {
        let (owner, host) = a.map(|a| (a.owner_actor_id.clone(), a.as_judged().host_id.clone())).unwrap_or_default();
        Here { eligible: runs_here_linked(false, &links, &s, &me, &owner, host_binding(&host, &here_dev)), resolves: Ok(()) }
    };
    tr::pending_route(&s, &m, &links, false, &here, &view, now).map(|p| p.route)
}

fn after(trigger_at: Timestamp, secs: i64) -> Timestamp {
    Timestamp::from_unix_seconds(trigger_at.inner().unix_timestamp() + secs)
}

/// H3 + D3: queued, then the host reads offline past the window; neither
/// Ana's desktop nor Ben's device answers it on its own; "Run here this
/// time" on Ana's desktop runs it once, and the box that comes back stands
/// down.
#[test]
fn an_offline_hosts_turn_waits_then_runs_here_once_on_request() {
    let mut fx = fixture();
    let box_id = fx.boxed.device_id().to_string();
    let trigger = fx.desk.post_user_message(fx.chat, fx.wid, "@coder fix the build").unwrap();
    let at = trigger.created_at;

    // Queued, then "box-1 has been offline for a while".
    assert_eq!(route_seen(&fx.desk, &fx, trigger.id, after(at, 5)), Some(TurnRoute::Queued { on: vec![box_id.clone()], since: route_since(&fx, trigger.id) }));
    let late = route_seen(&fx.desk, &fx, trigger.id, after(at, QUEUE_WINDOW_SECS + 1)).unwrap();
    assert_eq!(late, TurnRoute::Unrunnable(Why::HostGone { host: box_id.clone(), reason: GoneReason::Offline }));
    let facts = HostFacts::load(&fx.desk, fx.wid);
    let label = |h: &str| facts.label(h);
    let line = tr::line(&late, &tr::Names { agent: "coder", owner: "Ana", host: &label });
    assert_eq!(line, "box-1 has been offline for a while. @coder will answer when it's back.");

    // No path answers it silently elsewhere: the agent is placed on the
    // box, so neither gate puts it on Ana's desktop or Ben's device.
    let s = fx.desk.load_in(fx.wid, fx.chat).unwrap().unwrap();
    let links = fx.desk.device_links(fx.wid).unwrap_or_default();
    let desk_dev = fx.desk.device_id().to_string();
    let ben_dev = fx.ben.device_id().to_string();
    assert!(!runs_here_linked(false, &links, &s, "u1", "u1", host_binding(&box_id, &desk_dev)));
    assert!(!runs_here_linked(false, &links, &s, "ben", "u1", host_binding(&box_id, &ben_dev)));
    assert!(tr::run_here_allowed(Some(&fx.coder), &late, "ben", &links, &Ok(())).is_err(), "only the owner's own devices");

    // "Run here this time" on Ana's desktop: allowed, claimed, answered once.
    tr::run_here_allowed(Some(&fx.coder), &late, links.person("u1"), &links, &Ok(())).expect("Ana may run it here");
    let live = |_: Uuid| true;
    let step = fx.desk.dispatch_turn(fx.chat, fx.wid, trigger.id, None, DispatchCtx::plain(&live)).unwrap();
    assert_eq!(step, ligus_core::dispatch::Step::Run);
    let reply = fx.desk.begin_assistant_message_with_model(fx.chat, fx.wid, "coder", "rt", Some("opus".into()), Some(fx.coder.id), Some(trigger.id)).unwrap();
    fx.desk.note_reply_run(reply, ligus_core::outcome::ReplyOutcome { harness: "claudeCode".into(), source: "rt".into(), model: "opus".into(), host: desk_dev.clone(), ..Default::default() });
    fx.desk.complete_assistant_message(fx.chat, fx.wid, reply, "fixed").unwrap();

    // Answered: no row any more, on any device.
    assert_eq!(route_seen(&fx.desk, &fx, trigger.id, after(at, QUEUE_WINDOW_SECS + 1)), None);
    // The box comes back: it doesn't run the turn a second time.
    let again = fx.boxed.dispatch_turn(fx.chat, fx.wid, trigger.id, None, DispatchCtx::plain(&live)).unwrap();
    assert_ne!(again, ligus_core::dispatch::Step::Run, "the box stands down");
    assert!(crate::pending_turns(&fx.boxed.load_in(fx.wid, fx.chat).unwrap().unwrap()).iter().all(|p| p.message_id != trigger.id));
    let s = fx.desk.load_in(fx.wid, fx.chat).unwrap().unwrap();
    let answers: Vec<_> = s.messages.iter().filter(|m| m.role == MessageRole::Assistant && m.reply_to_message_id == Some(trigger.id) && !m.superseded).collect();
    assert_eq!(answers.len(), 1, "answered exactly once");
    // The completion says how it ended and what answered (H1, E2).
    let o = answers[0].outcome.as_ref().expect("an outcome");
    assert_eq!((o.status, o.model.as_str(), o.host.as_str()), (ReplyStatus::Ok, "opus", desk_dev.as_str()));
    let t = s.turn(trigger.id, None).unwrap();
    assert_eq!(t.state, ligus_core::turn::TurnState::Posted);
}

fn route_since(fx: &Fx, trigger: Uuid) -> Timestamp {
    fx.desk.load_in(fx.wid, fx.chat).unwrap().unwrap().messages.iter().find(|m| m.id == trigger).unwrap().appended_at()
}

/// H13: once the box's record is gone, its agent is "gone" at once and still
/// never runs on its owner's desktop by itself.
#[test]
fn a_removed_hosts_agent_is_gone_not_moved_to_its_owners_devices() {
    let mut fx = fixture();
    let box_id = fx.boxed.device_id().to_string();
    fx.boxed.remove_host(fx.wid, &box_id).unwrap();
    let trigger = fx.desk.post_user_message(fx.chat, fx.wid, "@coder fix the build").unwrap();
    let route = route_seen(&fx.desk, &fx, trigger.id, after(trigger.created_at, 1)).unwrap();
    assert!(matches!(route, TurnRoute::Unrunnable(Why::HostGone { reason: GoneReason::Removed, .. })), "{route:?}");
    let s = fx.desk.load_in(fx.wid, fx.chat).unwrap().unwrap();
    let links = fx.desk.device_links(fx.wid).unwrap_or_default();
    let agent = s.workspace_agents.iter().find(|a| a.id == fx.coder.id).unwrap();
    assert!(!runs_here_linked(false, &links, &s, "u1", "u1", host_binding(&agent.as_judged().host_id, &fx.desk.device_id().to_string())));
}

/// E2: the stale-stream sweep finishes a reply cut off by a crash as
/// canceled, and the turn reads Canceled.
#[test]
fn the_stale_sweep_writes_canceled() {
    let mut fx = fixture();
    let trigger = fx.desk.post_user_message(fx.chat, fx.wid, "@coder go").unwrap();
    let reply = fx.desk.begin_assistant_message(fx.chat, fx.wid, "coder", "rt", Some(fx.coder.id), Some(trigger.id)).unwrap();
    fx.desk.append_chunk(fx.chat, fx.wid, reply, "half").unwrap();
    assert_eq!(fx.desk.clear_stale_streaming().unwrap(), 1);
    let s = fx.desk.load_in(fx.wid, fx.chat).unwrap().unwrap();
    let m = s.messages.iter().find(|m| m.id == reply).unwrap();
    assert_eq!(m.outcome.as_ref().map(|o| o.status), Some(ReplyStatus::Canceled));
    assert!(!m.is_streaming);
}

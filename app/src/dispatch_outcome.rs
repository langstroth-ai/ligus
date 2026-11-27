//! The dispatch outcome on the desktop (workspace model v3, 5.4 and 8.1).
//!
//! - **No silent substitution.** An agent whose runtime this device doesn't
//!   have, or has without its key, doesn't run here on Claude Code instead:
//!   its turn is "can't run here" with the reason, and the person may pick
//!   "Use Claude Code instead" for that one turn ([`AppState::resolve_runtime_strict`]).
//! - **No answer-locally fallback.** A turn this device isn't eligible for
//!   (another person's agent, an agent on another host) is never answered
//!   here after a wait. The sender sees where it waits instead
//!   ([`add_routes`]), and the agent's owner may pick "Run here this time"
//!   ([`run_turn_here`]): under rules 3 only the owner's own devices.
//! - **Host policy (D16).** This computer runs only its own person's agents
//!   placed on it unless Settings turns on "Run other people's agents on this
//!   computer" ([`binding_for`]); its beat says which it refuses
//!   ([`host_refusals`]).

use ligus_core::outcome::{AgentRefusal, RefusalKind, TurnRoute, Why};
use ligus_core::{ChatSession, DeviceLinks, MessageRole, WorkspaceAgent};
use ligus_proto::ChatSessionDto;
use ligus_runtime::turn_dispatch::{host_binding, runs_here_linked, HostBinding};
use ligus_runtime::turn_route::{self as tr, Here, HostFacts};
use ligus_runtime::{ChatService, ResolvedRuntime};
use tauri::{AppHandle, State};
use uuid::Uuid;

use crate::{map_err, AppState};

/// The runtime id of the Claude Code CLI this device always has (the
/// synthesized entry, not a config one).
pub(crate) const CLAUDE_CODE_RUNTIME: &str = "claude-code";

impl AppState {
    /// Resolve `runtime_id` exactly, or say why it can't run here: a
    /// runtime this device doesn't have is [`Why::SourceNotOffered`] on
    /// `this_host`, one without its key [`Why::NoKeyHere`]. Never the
    /// Claude Code fallback [`AppState::resolve_runtime`] takes (that is the
    /// explicit "Use Claude Code instead"). The Claude Code CLI itself, and
    /// the chat default with nothing configured, resolve as they are.
    pub(crate) fn resolve_runtime_strict(&self, runtime_id: &str, this_host: &str) -> Result<ResolvedRuntime, Why> {
        let runtimes = self.combined_runtimes();
        let resolved = self.resolve_runtime_in(&runtimes, runtime_id);
        let found = runtimes.iter().find(|r| r.id == runtime_id).map(|r| r.provider_kind);
        strict_verdict(found, resolved.provider, runtime_id, this_host).map(|()| resolved)
    }

    /// The Claude Code CLI as this device runs it ("Use Claude Code
    /// instead").
    pub(crate) fn claude_code_runtime(&self) -> ResolvedRuntime {
        self.resolve_runtime(CLAUDE_CODE_RUNTIME)
    }
}

/// [`AppState::resolve_runtime_strict`]'s verdict: `found` is the provider
/// of the catalog entry with that id (if any), `resolved` what resolving it
/// gave. A found runtime that resolved to another provider fell back (a
/// hosted API with no key here); an id with no entry is a runtime this
/// device doesn't have, except Claude Code itself and the chat default.
pub(crate) fn strict_verdict(found: Option<ligus_core::ModelProviderKind>, resolved: ligus_core::ModelProviderKind, runtime_id: &str, this_host: &str) -> Result<(), Why> {
    match found {
        Some(p) if p != resolved => Err(Why::NoKeyHere { source: runtime_id.to_string() }),
        Some(_) => Ok(()),
        None if runtime_id.is_empty() || runtime_id == CLAUDE_CODE_RUNTIME => Ok(()),
        None => Err(Why::SourceNotOffered {
            host: this_host.to_string(),
            source: runtime_id.to_string(),
            detail: format!("this device doesn't have runtime '{}'", ligus_proto::names::clean_display_name(runtime_id)),
        }),
    }
}

/// Whether this computer runs a turn of an agent owned by `owner` placed on
/// it: its own person's always; anyone else's only with "Run other people's
/// agents on this computer" (D16, host-local).
pub(crate) fn runs_owner_here(local_actor: &str, links: &DeviceLinks, owner: &str) -> bool {
    owner.is_empty() || links.person(owner) == links.person(local_actor) || ligus_runtime::host_config::host().run_others_agents
}

/// [`host_binding`] with this computer's host policy applied: an agent of
/// another person placed on this computer is someone else's work unless the
/// person opted in (it never runs here, and the beat says so). Both dispatch
/// gates and the workflow stage gate take their binding here.
pub(crate) fn binding_for(local_host: &str, local_actor: &str, links: &DeviceLinks, owner: &str, host_id: &str) -> HostBinding {
    let b = host_binding(host_id, local_host);
    if b == HostBinding::ThisDevice && !runs_owner_here(local_actor, links, owner) {
        return HostBinding::Elsewhere;
    }
    b
}

/// The agents placed on this computer that it won't run in `workspace`,
/// with why, for its beat: another person's (unless opted in), or one whose
/// runtime this device doesn't have or has without its key.
pub(crate) fn host_refusals(state: &AppState, workspace: Uuid) -> Vec<AgentRefusal> {
    let (agents, me, my_name, here, links) = {
        let svc = state.service.lock().unwrap();
        let agents = svc
            .load_in(workspace, ligus_core::workspace_config_session_id(workspace))
            .ok()
            .flatten()
            .map(|s| s.workspace_agents)
            .unwrap_or_default();
        (agents, svc.author().id.clone(), svc.author().display_name.clone(), svc.device_id().to_string(), svc.device_links(workspace).unwrap_or_default())
    };
    let mine = ligus_proto::names::clean_display_name(&my_name);
    agents
        .iter()
        .filter(|a| a.as_judged().host_id == here)
        .filter_map(|a| {
            if !runs_owner_here(&me, &links, &a.owner_actor_id) {
                let why = if mine.trim().is_empty() { "it runs only its own person's agents".to_string() } else { format!("it runs only {mine}'s agents") };
                return Some(AgentRefusal { agent_id: a.id, kind: RefusalKind::NotAllowed, why });
            }
            match state.resolve_runtime_strict(&a.runtime_id, &here) {
                Ok(_) => None,
                Err(Why::NoKeyHere { source }) => Some(AgentRefusal {
                    agent_id: a.id,
                    kind: RefusalKind::NoKey,
                    why: format!("it has no key for '{}'", ligus_proto::names::clean_display_name(&source)),
                }),
                Err(_) => Some(AgentRefusal {
                    agent_id: a.id,
                    kind: RefusalKind::SourceNotOffered,
                    why: format!("it doesn't have '{}'", ligus_proto::names::clean_display_name(&a.runtime_id)),
                }),
            }
        })
        .collect()
}

/// The chat line for a turn of `agent_name` this device can't run, and why.
pub(crate) fn cant_run_line(agent_name: &str, why: &Why) -> String {
    let host = |h: &str| if h.is_empty() { "this device".to_string() } else { format!("host {}", h.get(..8).unwrap_or(h)) };
    tr::line(&TurnRoute::Unrunnable(why.clone()), &tr::Names { agent: agent_name, owner: "", host: &host })
}

/// Whether `m` was written by this device's person (on any of their devices).
fn written_by_me(svc: &ChatService, links: &DeviceLinks, m: &ligus_core::ChatMessage) -> bool {
    if m.role != MessageRole::User {
        return false;
    }
    let me = svc.author();
    let Some(a) = m.actor_identity.as_ref() else { return false };
    a.id == me.id || (me.account_id.is_some() && a.account_id == me.account_id) || links.person(&a.id) == links.person(&me.id)
}

/// What this device knows about running the turn of `agent` (`None`: the
/// chat's primary) in `s`: whether both gates put it here, and whether it
/// could run the agent's own runtime.
fn here_for(state: &AppState, svc: &ChatService, s: &ChatSession, links: &DeviceLinks, agent: Option<&WorkspaceAgent>) -> Here {
    let me = svc.author().id.clone();
    let local_host = svc.device_id().to_string();
    let local_only = state.is_local_only_ws(s.workspace_id);
    let (owner, host) = match agent {
        Some(a) => (a.owner_actor_id.clone(), a.as_judged().host_id.clone()),
        None => (s.creator_actor_id.clone(), String::new()),
    };
    let binding = binding_for(&local_host, &me, links, &owner, &host);
    let eligible = runs_here_linked(local_only, links, s, &me, &owner, binding);
    let resolves = match agent {
        Some(a) => state.resolve_runtime_strict(&a.runtime_id, &local_host).map(|_| ()),
        None => Ok(()),
    };
    Here { eligible, resolves }
}

/// Whether "Use Claude Code instead" fits `route`: the turn can't run on
/// this device because of its runtime, and it is the agent owner's.
fn claude_code_fits(route: &TurnRoute, agent: Option<&WorkspaceAgent>, links: &DeviceLinks, me: &str, local_host: &str) -> bool {
    let mine = agent.is_some_and(|a| !a.owner_actor_id.is_empty() && links.person(&a.owner_actor_id) == links.person(me));
    mine && match route {
        TurnRoute::Unrunnable(Why::NoKeyHere { .. }) => true,
        TurnRoute::Unrunnable(Why::SourceNotOffered { host, .. }) => host.is_empty() || host == local_host,
        _ => false,
    }
}

/// The quiet rows under this person's own messages whose turn doesn't run
/// (yet): queued, host offline or gone, or can't run, with "Run here this
/// time" when it fits (5.4). Only the sender's devices show them; nothing
/// is written.
pub(crate) fn add_routes(state: &AppState, svc: &ChatService, s: &ChatSession, dto: &mut ChatSessionDto) {
    if state.is_local_only_ws(s.workspace_id) {
        return;
    }
    let links = svc.device_links(s.workspace_id).unwrap_or_default();
    let me = svc.author().id.clone();
    let my_person = links.person(&me).to_string();
    let local_host = svc.device_id().to_string();
    let facts = HostFacts::load(svc, s.workspace_id);
    let now = ligus_core::Timestamp::now();
    let online = |h: &ligus_core::WorkspaceHost| h.id == local_host || state.host_presence.is_online(h, &now);
    let status = |h: &str| {
        if h == local_host {
            ligus_runtime::host_offers::beat_status(s.workspace_id)
        } else {
            state.host_presence.status_of(h)
        }
    };
    let view = facts.view(&online, &status);
    let here = |a: Option<&WorkspaceAgent>| here_for(state, svc, s, &links, a);
    let label = |h: &str| if h == local_host { "this device".to_string() } else { facts.label(h) };
    for m in s.messages.iter().filter(|m| written_by_me(svc, &links, m)) {
        let Some(p) = tr::pending_route(s, m, &links, false, &here, &view, now) else { continue };
        let agent = p.agent.as_ref();
        let name = agent.map(|a| a.name.clone()).unwrap_or_else(|| "Ligus".to_string());
        let owner_id = agent.map(|a| a.owner_actor_id.clone()).unwrap_or_else(|| s.creator_actor_id.clone());
        let owner = s.members.iter().find(|x| x.id == owner_id || x.actor.id == owner_id).map(|x| x.actor.display_name.clone()).unwrap_or_default();
        let resolves = here(agent).resolves;
        let run_here = tr::run_here_allowed(agent, &p.route, &my_person, &links, &resolves).is_ok();
        let use_cc = claude_code_fits(&p.route, agent, &links, &me, &local_host);
        let names = tr::Names { agent: &name, owner: &owner, host: &label };
        if let Some(row) = tr::route_dto(&p.route, &names, run_here, use_cc) {
            if let Some(d) = dto.messages.iter_mut().find(|d| d.id == m.id.to_string()) {
                d.route = Some(row);
            }
        }
    }
}

/// "Run here this time" (D3) and "Use Claude Code instead" for the turn
/// `trigger_id` started: runs it once on this device, after the same checks
/// the row's offer made, through the dispatcher's claim (so a host that comes
/// back sees the turn taken and backs off). Under rules 3 only the agent's
/// owner's own devices may.
#[tauri::command]
pub(crate) async fn run_turn_here(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    trigger_id: String,
    use_claude_code: Option<bool>,
) -> Result<(), String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let trigger = Uuid::parse_str(&trigger_id).map_err(map_err)?;
    let use_cc = use_claude_code == Some(true);
    let (workspace_id, responder) = {
        let svc = state.service.lock().unwrap();
        let s = svc.load_resolving(sid).map_err(map_err)?.ok_or("unknown chat")?;
        let links = svc.device_links(s.workspace_id).unwrap_or_default();
        let m = s.messages.iter().find(|m| m.id == trigger).ok_or("that message is gone")?;
        let local_host = svc.device_id().to_string();
        let facts = HostFacts::load(&svc, s.workspace_id);
        let now = ligus_core::Timestamp::now();
        let online = |h: &ligus_core::WorkspaceHost| h.id == local_host || state.host_presence.is_online(h, &now);
        let status = |h: &str| state.host_presence.status_of(h);
        let view = facts.view(&online, &status);
        let here = |a: Option<&WorkspaceAgent>| here_for(&state, &svc, &s, &links, a);
        // Any age: the person asked now (the row's own timing doesn't matter).
        let later = ligus_core::Timestamp::from_unix_seconds(now.inner().unix_timestamp() + 3600);
        let Some(p) = tr::pending_route(&s, m, &links, false, &here, &view, later) else {
            return Err("that turn is already answered or running".into());
        };
        let me = svc.author().id.clone();
        let my_person = links.person(&me).to_string();
        let agent = p.agent.clone();
        if use_cc {
            if !claude_code_fits(&p.route, agent.as_ref(), &links, &me, &local_host) {
                return Err("Claude Code stands in only for your own agent's turn that can't run here".into());
            }
        } else {
            tr::run_here_allowed(agent.as_ref(), &p.route, &my_person, &links, &here(agent.as_ref()).resolves).map_err(|why| format!("Couldn't run it here: {why}."))?;
        }
        drop(svc);
        let mut responder = crate::responder_for(&state, &s, agent.as_ref());
        responder.unrunnable = None;
        if use_cc {
            responder.runtime = state.claude_code_runtime();
        }
        (s.workspace_id, responder)
    };
    if !crate::agent_turn_allowed(&state, sid, workspace_id, trigger, &responder, true) {
        return Err("this agent's access rules don't let it answer that here".into());
    }
    match crate::dispatch_here(&state, sid, workspace_id, trigger, None, true, std::time::Duration::ZERO)? {
        ligus_core::dispatch::Step::Run => {}
        _ => return Err("another device is answering it".into()),
    }
    tracing::info!(target: "dispatch", responder = %responder.author, claude_code = use_cc, "running a turn here this time");
    crate::run_turn_answering(&app, &state, sid, workspace_id, &responder, Some(trigger)).await.map(|_| ())
}

/// Whether this computer runs other people's agents placed on it (Settings:
/// "Run other people's agents on this computer"; `run_others_agents` in its
/// host.toml). Off by default.
#[tauri::command]
pub(crate) fn get_run_others_agents() -> bool {
    ligus_runtime::host_config::host().run_others_agents
}

/// Turn running other people's agents on this computer on or off. The next
/// beat carries the new refusals.
#[tauri::command]
pub(crate) fn set_run_others_agents(state: State<AppState>, run: bool) -> Result<(), String> {
    let cfg = ligus_runtime::host_config::set_run_others_agents(&state.data_dir, run).map_err(map_err)?;
    ligus_runtime::host_config::install(cfg);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// H2 on the desktop: no key, or no such runtime here, is a reason,
    /// never the Claude Code stand-in; Claude Code itself resolves.
    #[test]
    fn a_runtime_this_device_cant_run_as_asked_is_a_reason() {
        use ligus_core::ModelProviderKind as P;
        assert_eq!(strict_verdict(Some(P::Anthropic), P::Anthropic, "anthropic", ""), Ok(()));
        assert_eq!(strict_verdict(Some(P::Anthropic), P::ClaudeCode, "anthropic", ""), Err(Why::NoKeyHere { source: "anthropic".into() }));
        assert!(matches!(strict_verdict(None, P::ClaudeCode, "team-gw", "desk"), Err(Why::SourceNotOffered { host, .. }) if host == "desk"));
        assert_eq!(strict_verdict(None, P::ClaudeCode, CLAUDE_CODE_RUNTIME, ""), Ok(()));
        assert_eq!(strict_verdict(Some(P::ClaudeCode), P::ClaudeCode, "cc", ""), Ok(()));
    }

    /// H9: a desktop runs only its own person's agents placed on it; another
    /// person's runs elsewhere (never here) unless the person opts in.
    #[test]
    fn a_desktop_runs_only_its_own_persons_agents() {
        let links = DeviceLinks::default();
        ligus_runtime::host_config::install(ligus_runtime::host_config::HostConfig::default());
        assert_eq!(binding_for("desk", "ana", &links, "ana", "desk"), HostBinding::ThisDevice);
        assert_eq!(binding_for("desk", "ana", &links, "ben", "desk"), HostBinding::Elsewhere, "Ben's agent on Ana's desktop");
        assert_eq!(binding_for("desk", "ana", &links, "ben", ""), HostBinding::Unbound);
        assert_eq!(binding_for("desk", "ana", &links, "ben", "box"), HostBinding::Elsewhere);
        let session = {
            let mut s = ChatSession::new("t", Uuid::new_v4(), "");
            for id in ["ana", "ben"] {
                s.members.push(ligus_core::WorkspaceMember {
                    id: id.into(),
                    actor: ligus_core::ActorIdentity::new(id.to_string(), id.to_string(), ligus_core::ActorKind::Human),
                    role: ligus_core::WorkspaceRole::Contributor,
                    title: String::new(),
                    index: 1,
                    joined_at: ligus_core::Timestamp::now(),
                });
            }
            s
        };
        let b = binding_for("desk", "ana", &links, "ben", "desk");
        assert!(!runs_here_linked(false, &links, &session, "ana", "ben", b), "never on Ana's desktop");
        let mut opted = ligus_runtime::host_config::HostConfig::default();
        opted.run_others_agents = true;
        ligus_runtime::host_config::install(opted);
        assert_eq!(binding_for("desk", "ana", &links, "ben", "desk"), HostBinding::ThisDevice, "opted in");
        ligus_runtime::host_config::install(ligus_runtime::host_config::HostConfig::default());
    }
}

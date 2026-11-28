//! Rules version 4 (workspace model v3, 15): what the fold allows once a
//! workspace sets it. Events before the rules-4 position are judged as
//! before and never re-judged; a build that doesn't know version 4 makes the
//! workspace read-only on its device.
//!
//! | Gate | Rule | Here |
//! |---|---|---|
//! | G1 | who may create and move agents (5.7) | [`agent_floor`], presence in `authz::rules` |
//! | G2 | a placement's host resolves at the event's position | [`placement_refusal`] |
//! | G3 | hosts of revoked devices and departed members are inert | [`host_inert`], the fold's post-pass, [`super::RosterState::agent_bound_to`] |
//! | G4 | `MemberLeft` is a removal (like a self-removal, never a cut) | the fold; [`leave_watermark_refusal`] |
//! | G5 | no agent records on chat logs | [`refusal`] |
//! | G6 | the default agent's reserved id | [`default_agent_refusal`], [`admitted`] |
//! | G7 | a Human or Service member names its account | [`refusal`] |
//! | G8 | a payload's "who" agrees with the envelope's stamp | [`who_refusal`] |
//! | G9 | no new or changed runtime endpoint on the log | [`refusal`] |
//! | G10 | question eviction by canonical position | `flow::asks` |
//! | G11 | no whole-list agent, runtime or host events on the config log | [`refusal`] |
//! | G12 | filing a chat into a channel is judged by the workspace role | `authz::authorize` |
//! | G13 | a host record is its own trusted device's | [`refusal`] |
//! | G14 | agent output from every host the placement admits, and from a `TurnDelegated` device for that one turn | [`admitted`], `authz::authorize_agent` |
//!
//! Every check on an agent record judges the merged record
//! ([`crate::agent_run::merge_upsert`] with [`RunRules::Full`]), never the
//! payload alone (5.6 rule 5).

use std::collections::{BTreeMap, BTreeSet};

use uuid::Uuid;

use crate::agent::WorkspaceAgent;
use crate::agent_run::{merge_upsert, Placement, RunRules, RUNTIME_SOURCE_PREFIX};
use crate::events::{SessionEvent, SessionEventEnvelope};
use crate::identity::{ActorKind, WorkspaceRole};
use crate::session::ChatSession;
use crate::workspace_host::WorkspaceHost;

use super::RosterState;

/// The default agent's name, locked on its record (5.8).
pub const DEFAULT_AGENT_NAME: &str = "Ligus";

/// The default agent's well-known id in a workspace (5.8, D15):
/// `UUIDv5(workspace_id, "ligus-default-agent")`. Reserved under rules 4:
/// known without a record, and its record (when written) is locked to
/// [`DEFAULT_AGENT_NAME`] and the `chatCreator` placement.
pub fn default_agent_id(workspace_id: Uuid) -> Uuid {
    Uuid::new_v5(&workspace_id, b"ligus-default-agent")
}

/// At most this many entries in a leave's watermark (it names only the
/// leaver's own signers, so it stays far below a removal's bound).
pub const MAX_LEAVE_WATERMARK: usize = 1024;

/// Whether host `h` is inert under rules 4 (G3, G13): its device is
/// revoked, or its latest record wasn't signed by the device it names.
/// (A host whose owner isn't a member is inert under rules 3 already.)
pub fn host_inert(h: &WorkspaceHost, revoked: &BTreeSet<Uuid>, signers: &BTreeMap<String, Option<Uuid>>) -> bool {
    Uuid::parse_str(&h.id).is_ok_and(|d| revoked.contains(&d))
        || !signers.get(&h.id).is_some_and(|s| crate::workspace_host::signed_by_own_device(&h.id, *s))
}

/// What rules 4 refuses outright, before the role and presence checks
/// (G5, G7, G8, G9, G11, G13, and the shape of `TurnDelegated`).
/// `roster`: the roster at the event's position (the config log's own, or
/// the workspace's at a chat event's frontier).
pub fn refusal(env: &SessionEventEnvelope, on_config: bool, roster: &RosterState, fold: &ChatSession, author: &str) -> Option<String> {
    match &env.payload {
        // G5: an agent is the workspace's; a chat never redefines one.
        SessionEvent::AgentUpserted { .. } | SessionEvent::AgentRosterUpdated { .. } | SessionEvent::AgentRemoved { .. } if !on_config => {
            return Some("agents are kept on the workspace config log".into());
        }
        // G11: one record per event, so concurrent writers never erase
        // each other.
        SessionEvent::AgentRosterUpdated { .. } | SessionEvent::WorkspaceRuntimesUpdated { .. } | SessionEvent::WorkspaceHostsUpdated { .. } => {
            return Some("whole-list agent, runtime and host events are refused: write one record".into());
        }
        // G7 (D12): no new accountless members.
        SessionEvent::MemberAdded { member, .. }
            if matches!(member.actor.kind, ActorKind::Human | ActorKind::Service) && member.actor.account_id.is_none() =>
        {
            return Some("a person or a box joins with its account".into());
        }
        // G9: model server addresses stay on the host that runs them.
        SessionEvent::WorkspaceRuntimeUpserted { runtime } if !runtime.endpoint.is_empty() => {
            let kept = fold.workspace_runtimes.iter().any(|r| r.id == runtime.id && r.endpoint == runtime.endpoint);
            if !kept {
                return Some("a model server's address stays on its host: list it in that host's host.toml".into());
            }
        }
        // G13: a host record is written by the device it names, trusted
        // here, for an owner whose account holds that device.
        SessionEvent::WorkspaceHostUpserted { host } => {
            if !crate::workspace_host::signed_by_own_device(&host.id, env.signer_device_id) {
                return Some("a host record is written by its own device".into());
            }
            if let Err(why) = super::certified_signer(env, roster) {
                return Some(format!("a host record is signed by its own trusted device: {why}"));
            }
            let device = env.signer_device_id.unwrap_or_default();
            let owner_account = roster.members.get(&host.owner_actor_id).and_then(|m| m.account_id);
            if !owner_account.is_some_and(|a| roster.account_holds_device(a, device)) {
                return Some("a host record names an owner who holds that device".into());
            }
        }
        SessionEvent::TurnDelegated { .. } if on_config => {
            return Some("a turn is delegated on its chat's log".into());
        }
        _ => {}
    }
    who_refusal(env, fold, author, roster)
}

/// G8: a payload naming a member as who did something names its signed
/// author. Only names of current members count (a display label such as
/// the primary runtime's, or an agent's name, impersonates no one); an
/// agent's reply may name its agent or the agent's owner, as the Agent tier
/// allows for an agent's own key.
pub fn who_refusal(env: &SessionEventEnvelope, fold: &ChatSession, author: &str, roster: &RosterState) -> Option<String> {
    let stamp = env.actor_stamp.as_ref()?;
    // The Agent tier holds agent-signed payloads to the same (and more).
    if stamp.actor.kind == ActorKind::Agent {
        return None;
    }
    let someone_else = |id: &str| id != author && roster.members.contains_key(id);
    let bad = match &env.payload {
        SessionEvent::MessageAppended { message } => message.actor_identity.as_ref().is_some_and(|id| {
            let owner = message.responder_id.and_then(|r| roster.agents.get(&r)).map(|a| a.owner.as_str());
            let agent_reply = message.role != crate::chat::MessageRole::User && owner.is_some_and(|o| o == id.id);
            someone_else(&id.id) && !agent_reply
        }),
        SessionEvent::MessageReactionAdded { reaction, .. } => someone_else(&reaction.actor_id),
        SessionEvent::MessageReactionRemoved { actor_id, .. } => someone_else(actor_id),
        SessionEvent::ChatLockdownRaised { lockdown, .. } => someone_else(&lockdown.raised_by),
        SessionEvent::WorkflowRunUpserted { run } => fold.workflow_runs.iter().all(|r| r.id != run.id) && someone_else(&run.initiator_actor_id),
        SessionEvent::ProposalUpserted { proposal } => fold.proposals.iter().all(|p| p.id != proposal.id) && someone_else(&proposal.author_actor_id),
        _ => false,
    };
    bad.then(|| format!("{} names another member as its author", env.payload.kind_str()))
}

/// Whether `a` moves the agent compared with `old` under rules 4: its host,
/// placement, run source or harness changes. A model change within one
/// source is not a move (5.7).
pub fn moves(a: &WorkspaceAgent, old: &WorkspaceAgent) -> bool {
    let (r, o) = (a.effective_run(), old.effective_run());
    a.host_id != old.host_id || a.effective_placement() != old.effective_placement() || r.source != o.source || r.harness != o.harness
}

/// The record an `AgentUpserted` of `agent` leaves under rules 4, and the
/// one it replaces (as authz sees it at the event's position).
pub fn merged(agent: &WorkspaceAgent, fold: &ChatSession) -> (Option<WorkspaceAgent>, WorkspaceAgent) {
    let old = fold.workspace_agents.iter().find(|a| a.id == agent.id).map(|a| a.as_judged().into_owned());
    let m = merge_upsert(old.as_ref(), agent, RunRules::Full);
    (old, m)
}

/// Whether `author` owns host `host_id` here: its record names them as the
/// owner and their account holds the device.
fn owns_host(author: &str, host_id: &str, fold: &ChatSession, roster: &RosterState) -> bool {
    let account = roster.members.get(author).and_then(|m| m.account_id);
    fold.workspace_hosts.iter().any(|h| h.id == host_id && h.owner_actor_id == author)
        && Uuid::parse_str(host_id).is_ok_and(|d| account.is_some_and(|a| roster.account_holds_device(a, d)))
}

/// G1 (table 5.7): the role an agent upsert needs under rules 4, judged on
/// the merged record. Presence is the presence table's
/// ([`needs_presence`]).
pub fn agent_floor(agent: &WorkspaceAgent, author: &str, fold: &ChatSession, roster: &RosterState) -> WorkspaceRole {
    let (old, m) = merged(agent, fold);
    let own = !m.owner_actor_id.is_empty() && m.owner_actor_id == author;
    let place_floor = |m: &WorkspaceAgent| -> WorkspaceRole {
        match m.effective_placement() {
            Placement::OwnerDevices if own || m.owner_actor_id.is_empty() => WorkspaceRole::Contributor,
            Placement::Host { host_id } if own && owns_host(author, &host_id, fold, roster) => WorkspaceRole::Contributor,
            _ => WorkspaceRole::Admin,
        }
    };
    if m.id == default_agent_id(fold.workspace_id) {
        return WorkspaceRole::Admin;
    }
    match old {
        None if !m.owner_actor_id.is_empty() && !own => WorkspaceRole::Admin,
        None => place_floor(&m),
        Some(old) if old.owner_actor_id != m.owner_actor_id => WorkspaceRole::Admin,
        Some(old) if moves(&m, &old) => place_floor(&m),
        Some(old) if old.owner_actor_id.is_empty() || own => WorkspaceRole::Contributor,
        Some(_) => WorkspaceRole::Admin,
    }
}

/// Whether an agent upsert needs its author's presence under rules 4 (5.7):
/// creating or moving an agent anywhere but its owner's devices, any change
/// of its source or harness, and any run on the default agent. A rename, an
/// avatar and a model change within one source need none.
pub fn needs_presence(agent: &WorkspaceAgent, fold: &ChatSession) -> bool {
    let (old, m) = merged(agent, fold);
    if m.id == default_agent_id(fold.workspace_id) {
        return match &old {
            None => m.run.as_ref().is_some_and(|r| !r.source.is_empty() || !r.harness.is_empty()),
            Some(old) => moves(&m, old),
        };
    }
    match &old {
        None => m.effective_placement() != Placement::OwnerDevices,
        Some(old) => moves(&m, old),
    }
}

/// G6: the default agent's record is locked to its name, no owner and the
/// `chatCreator` placement; no other agent takes that placement.
pub fn default_agent_refusal(agent: &WorkspaceAgent, fold: &ChatSession) -> Option<&'static str> {
    let (_, m) = merged(agent, fold);
    let default = m.id == default_agent_id(fold.workspace_id);
    let chat_creator = m.placement == Some(Placement::ChatCreator);
    if default {
        if m.name != DEFAULT_AGENT_NAME || !m.owner_actor_id.is_empty() || !m.host_id.is_empty() || !chat_creator {
            return Some("the default agent is \"Ligus\", unowned, and answers on the chat creator's devices");
        }
    } else if chat_creator {
        return Some("only the default agent answers on the chat creator's devices");
    }
    None
}

/// G2: a placement that names a host (a `host` placement, or a `pool`'s
/// first host) resolves at the event's position: the host's record exists
/// and its own device signed it, the device is trusted, the owner is a
/// member, and the source is among the offers when the host published
/// some. A `pool` needs a source by name (a worker can't tell anyone which
/// workspace runtimes it has). Judged on a new agent and on a move only, so
/// renaming an agent whose host is gone isn't refused.
pub fn placement_refusal(agent: &WorkspaceAgent, fold: &ChatSession, roster: &RosterState) -> Option<String> {
    let (old, m) = merged(agent, fold);
    if old.as_ref().is_some_and(|o| !moves(&m, o)) {
        return None;
    }
    let run = m.effective_run();
    let host = match m.effective_placement() {
        Placement::Host { host_id } => Some(host_id),
        Placement::Pool => {
            if run.source.is_empty() || run.source.starts_with(RUNTIME_SOURCE_PREFIX) {
                return Some("a pool runs a source workers offer by name, not a workspace runtime".into());
            }
            (!m.host_id.is_empty()).then(|| m.host_id.clone())
        }
        _ => None,
    }?;
    let Some(h) = fold.workspace_hosts.iter().find(|h| h.id == host) else {
        return Some("that host isn't in the workspace".into());
    };
    if host_inert(h, &fold.guards.revoked_devices, &fold.authz.host_signers) {
        return Some("that host's record isn't its own device's, or its device was revoked".into());
    }
    if !Uuid::parse_str(&host).is_ok_and(|d| roster.device_trusted(d)) {
        return Some("that host's device isn't trusted here".into());
    }
    if !fold.members.iter().any(|x| x.id == h.owner_actor_id) {
        return Some("that host's owner isn't a member".into());
    }
    if h.offers.is_some() && !run.source.starts_with(RUNTIME_SOURCE_PREFIX) && !h.offers_source(&run.source) {
        let label = if h.label.trim().is_empty() { "that host" } else { h.label.as_str() };
        return Some(format!("{label} doesn't offer {}", run.source));
    }
    None
}

/// G4: a leave's watermark is the leaver's own word, so the fold never
/// denies anything by it; it must still be well formed and bounded: at
/// most [`MAX_LEAVE_WATERMARK`] entries, one per (signer, log), each with
/// one to [`crate::watermark::MAX_HEADS`] heads and at most as many gaps,
/// naming only the leaver's own devices and its agents' keys.
pub fn leave_watermark_refusal(watermark: &[crate::watermark::Watermark], account: Uuid, member_id: &str, roster: &RosterState) -> Option<&'static str> {
    if watermark.len() > MAX_LEAVE_WATERMARK {
        return Some("a leave's watermark is too large");
    }
    let mut seen = BTreeSet::new();
    for w in watermark {
        if w.heads.is_empty() || w.heads.len() > crate::watermark::MAX_HEADS || w.gaps.len() > crate::watermark::MAX_HEADS {
            return Some("a leave's watermark entry is malformed");
        }
        if !seen.insert((w.signer, w.log)) {
            return Some("a leave's watermark names one signer and log twice");
        }
        let own_device = roster.devices.get(&w.signer).is_some_and(|d| d.accounts.contains(&account));
        let own_agent = roster.agent_keys.get(&w.signer).is_some_and(|k| roster.agents.get(&k.agent_id).is_some_and(|a| a.owner == member_id));
        if !own_device && !own_agent {
            return Some("a leave's watermark names only the leaver's own signers");
        }
    }
    None
}

/// G6 and G14: whether `device` may write `agent_id`'s output in this chat
/// (host-signed, or with a key `device` certified): the default agent on the
/// chat creator's devices; a shared agent with no owner on any member's
/// trusted device; any other agent where its placement admits
/// `device` at the event's position, or for the one turn a `TurnDelegated`
/// gave `device` (`trigger`: the turn the output answers, when it names
/// one; `None` accepts any turn delegated to `device` for this agent in
/// this chat).
pub fn admitted(agent_id: Uuid, device: Uuid, trigger: Option<Uuid>, fold: &ChatSession, at: &RosterState) -> bool {
    if agent_id == default_agent_id(fold.workspace_id) {
        let creator = at.members.get(&fold.creator_actor_id).and_then(|m| m.account_id);
        return creator.is_some_and(|a| at.account_holds_device(a, device)) && at.device_trusted(device);
    }
    // A shared agent with no owner, on "the owner's devices", runs wherever
    // a member asks it (as below rules 4): any trusted device of a member.
    let shared = at.agents.get(&agent_id).is_some_and(|a| a.owner.is_empty() && a.placement == Placement::OwnerDevices);
    let members_device = || {
        at.device_trusted(device)
            && at.members.values().filter_map(|m| m.account_id).any(|a| at.account_holds_device(a, device))
    };
    at.agent_bound_to(agent_id, device) || (shared && members_device()) || delegated(agent_id, device, trigger, fold)
}

/// Whether a `TurnDelegated` in this chat lets `device` answer for
/// `agent_id` (the turn `trigger`, or any when `None`).
pub fn delegated(agent_id: Uuid, device: Uuid, trigger: Option<Uuid>, fold: &ChatSession) -> bool {
    let ok = |d: &super::Delegation| d.agent_id == agent_id && d.device_id == device;
    match trigger {
        Some(t) => fold.authz.delegations.get(&t).is_some_and(ok),
        None => fold.authz.delegations.values().any(ok),
    }
}

/// The turn an agent's output answers, when it names one.
pub fn trigger_of(event: &SessionEvent, fold: &ChatSession) -> Option<Uuid> {
    match event {
        SessionEvent::MessageAppended { message } => message.reply_to_message_id.map(|r| crate::dispatch::trigger_of(fold, r)),
        _ => None,
    }
}

/// `TurnDelegated` (rules 4, chat log): signed by the device it names, a
/// person's own (not a phone, not a delegated box), for an agent of the
/// workspace that is the author's (or shared, by an Admin), for a message
/// of this chat, once per turn.
pub fn delegation_refusal(env: &SessionEventEnvelope, fold: &ChatSession, at: &RosterState, author: &str) -> Option<&'static str> {
    let SessionEvent::TurnDelegated { trigger_id, agent_id, device_id } = &env.payload else { return None };
    let stamp = env.actor_stamp.as_ref()?;
    if stamp.actor.kind != ActorKind::Human || stamp.runtime_effect {
        return Some("a person runs a turn on their own device");
    }
    if env.signer_device_id != Some(*device_id) {
        return Some("a turn is delegated by the device that runs it");
    }
    let account = stamp.actor.account_id;
    let own = at.devices.get(device_id).is_some_and(|d| {
        !d.revoked && !d.phone && account.is_some_and(|a| d.accounts.contains(&a) && !d.delegated_for(a))
    });
    if !own || at.members.get(author).and_then(|m| m.account_id) != account {
        return Some("a turn runs on the person's own computer");
    }
    if *agent_id == default_agent_id(fold.workspace_id) {
        return Some("the default agent answers on the chat creator's devices");
    }
    let Some(agent) = at.agents.get(agent_id) else { return Some("unknown agent") };
    let admin = at.role_of(author).is_some_and(|r| r.rank() >= WorkspaceRole::Admin.rank());
    if !(agent.owner == author || (agent.owner.is_empty() && admin)) {
        return Some("only an agent's owner (an Admin, for a shared agent) runs its turn elsewhere");
    }
    if !fold.messages.iter().any(|m| m.id == *trigger_id) {
        return Some("no such message in this chat");
    }
    if fold.authz.delegations.contains_key(trigger_id) {
        return Some("that turn is already delegated");
    }
    None
}

/// What an Owner sees before switching an existing workspace to rules 4
/// (15.2). It informs; the switch goes ahead anyway. Builds below 0.5 come
/// from host beats, which the fold doesn't hold.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Readiness {
    /// Members with no account (they keep working; no new ones join).
    pub accountless_members: Vec<String>,
    /// Agents whose placement wouldn't resolve under rules 4 (by id).
    pub unresolved_agents: Vec<Uuid>,
    /// Host records not signed by the device they name (by host id).
    pub unsigned_hosts: Vec<String>,
}

impl Readiness {
    pub fn is_clear(&self) -> bool {
        self.accountless_members.is_empty() && self.unresolved_agents.is_empty() && self.unsigned_hosts.is_empty()
    }
}

/// The readiness list of a workspace's config fold at its head.
pub fn readiness(config: &ChatSession, roster: &RosterState) -> Readiness {
    let accountless_members = config
        .members
        .iter()
        .filter(|m| matches!(m.actor.kind, ActorKind::Human | ActorKind::Service) && m.actor.account_id.is_none())
        .map(|m| m.id.clone())
        .collect();
    let unsigned_hosts = config
        .workspace_hosts
        .iter()
        .filter(|h| host_inert(h, &BTreeSet::new(), &config.authz.host_signers))
        .map(|h| h.id.clone())
        .collect();
    let unresolved_agents = config
        .workspace_agents
        .iter()
        .filter(|a| a.is_host_gone() || placement_refusal(&WorkspaceAgent { host_gone: false, ..(*a.as_judged()).clone() }, &without(config, a.id), roster).is_some())
        .map(|a| a.id)
        .collect();
    Readiness { accountless_members, unresolved_agents, unsigned_hosts }
}

/// `config` without agent `id`: judging its record as a new one.
fn without(config: &ChatSession, id: Uuid) -> ChatSession {
    let mut c = config.clone();
    c.workspace_agents.retain(|a| a.id != id);
    c
}

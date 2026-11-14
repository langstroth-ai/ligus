//! The authorization engine (Phase 0, A2): one pure answer to "was this event
//! allowed when it happened".
//!
//! Every event a fold applies (except the base seed) goes through
//! [`authorize`], and so does every event this device authors
//! (`ChatService::append_signed`), so authoring and folding cannot disagree.
//!
//! * **Its own log.** Governance and config events
//!   ([`crate::authorization::requires_projection_authz`]) are judged against
//!   the roster folded so far on their own log, in canonical order.
//! * **Workspace position.** Chat events that need the workspace roster
//!   ([`needs_workspace_authz`]: proposal-set events, votes, `ProposalApplied`)
//!   are judged against the config-log roster at the frontier the author had
//!   folded (`ActorStamp::cfg_frontier`), never against today's roster: a later
//!   promotion doesn't validate an old refusal, and a later demotion doesn't
//!   undo an old decision. A frontier this device doesn't hold yet, or a
//!   chat whose workspace config log hasn't synced, **holds** the event
//!   ([`HoldReason`]): it stays in the store, is left out of the projection,
//!   blocks side effects on what it touches, and is re-decided on the next fold.
//!   Only the config roster counts; a chat's own seed roster never grants a
//!   workspace role.
//! * **Votes** count only when the voter they name is their signed author, at
//!   that author's role at the vote's position (the role in the payload is
//!   ignored), and only for the content digest they name. Proposal status is
//!   derived from counted votes; `Applied` comes only from `ProposalApplied`.
//! * **Rules** are versioned on the config log (`WorkspaceRulesSet`); a version
//!   this build doesn't know makes the workspace read-only on this device.
//! * **Bounds** (S5 lockdowns, S6 flow state) and the run-write rule (S4) are
//!   fold rules too.
//!
//! The device-local workspace (never synced, single human) is exempt from
//! role checks, as before.

pub mod agent;
pub mod bounds;
pub mod timeline;
pub mod votes;

use std::collections::BTreeMap;

use uuid::Uuid;

pub use timeline::{AgentKeyState, AgentState, ConfigTimeline, DeviceState, MemberState, RosterState, RosterTracker};

use crate::agent::WorkspaceAgent;
use crate::authorization::{evaluate, requires_projection_authz};
use crate::events::{SessionEvent, SessionEventEnvelope};
use crate::identity::{ActorKind, CfgFrontier, WorkspaceRole};
use crate::session::ChatSession;

/// The workspace rules version: 1 = the 2.0 base rules. Track B's human
/// presence is version 2; this build does not enforce it yet, so it treats 2
/// as unknown (read-only) rather than claim rules it would not apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RulesVersion(pub u32);

/// The highest rules version this build enforces.
pub const RULES_SUPPORTED: u32 = 1;

/// Why an event was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenyReason(pub String);

/// Why an event can't be decided yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldReason {
    /// Its frontier names a config event this device doesn't hold yet.
    ConfigFrontierAhead,
    /// It needs the workspace roster and the config log hasn't synced.
    NoConfigLog,
    /// Its signer isn't known yet.
    SignerUnknownYet,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(DenyReason),
    Hold(HoldReason),
}

impl Decision {
    fn deny(why: impl Into<String>) -> Self {
        Decision::Deny(DenyReason(why.into()))
    }
    pub fn is_allow(&self) -> bool {
        matches!(self, Decision::Allow)
    }
}

/// Who signed an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signer {
    HumanDevice { device: Uuid, account: Uuid },
    Agent { agent_id: Uuid, key_id: Uuid },
    Unsigned,
}

impl Signer {
    pub fn of(env: &SessionEventEnvelope) -> Self {
        match env.signer_device_id {
            None => Signer::Unsigned,
            Some(device) => Signer::HumanDevice {
                device,
                account: env.actor_stamp.as_ref().and_then(|s| s.actor.account_id).unwrap_or_else(Uuid::nil),
            },
        }
    }
}

/// An event this device holds but the fold left out until it can be decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldEvent {
    pub event_id: Uuid,
    pub reason: HoldReason,
    /// The proposal it concerns, if any.
    pub proposal_id: Option<Uuid>,
    /// The proposal set it concerns, if any.
    pub set_id: Option<Uuid>,
}

/// Device-local authorization facts about a projection. Never serialized.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthzState {
    /// Events left out of this projection until they can be decided.
    pub held: Vec<HeldEvent>,
    /// The workspace rules version in force at the head (0 = not known here).
    pub rules: u32,
    /// Signed raiser of each active lockdown (S5 accounting).
    pub lockdown_raisers: BTreeMap<Uuid, String>,
    /// The config frontier each proposal's current content was filed at. A
    /// vote or apply naming an older frontier is refused, so a stale role
    /// (a demoted Admin who keeps naming a pre-demotion frontier) never
    /// weighs on content filed after the demotion reached its author.
    pub content_frontier: BTreeMap<Uuid, CfgFrontier>,
    /// The config frontier each proposal set was opened at (same rule).
    pub set_frontier: BTreeMap<Uuid, CfgFrontier>,
    /// The signed author who first appended each message: only they continue
    /// or rewrite it (and only they or an Admin remove it).
    pub message_authors: BTreeMap<Uuid, String>,
}

impl AuthzState {
    /// Whether this device may write to the workspace: its rules are ones this
    /// build enforces.
    pub fn read_only(&self) -> bool {
        self.rules > RULES_SUPPORTED
    }

    /// Whether a held event touches `proposal` (or its set): no side effect
    /// (Implement, pre-flight, apply-on-approve, a workflow stage) may run on
    /// it until the hold resolves.
    pub fn blocks(&self, proposal_id: Uuid, set_id: Option<Uuid>) -> bool {
        self.held
            .iter()
            .any(|h| h.proposal_id == Some(proposal_id) || (set_id.is_some() && h.set_id == set_id))
    }

    /// Whether a held event touches the proposal set `set_id` or any of its
    /// alternatives.
    pub fn blocks_set(&self, set_id: Uuid, alternatives: &[Uuid]) -> bool {
        self.held
            .iter()
            .any(|h| h.set_id == Some(set_id) || h.proposal_id.is_some_and(|p| alternatives.contains(&p)))
    }
}

/// Chat events judged against the workspace roster at their position.
pub fn needs_workspace_authz(event: &SessionEvent) -> bool {
    crate::authorization::requires_hoisted_authz(event)
        || matches!(event, SessionEvent::ProposalVoteCast { .. } | SessionEvent::ProposalApplied { .. })
}

/// The proposal and set an event concerns (for holds and side-effect blocks).
pub fn subject_of(event: &SessionEvent) -> (Option<Uuid>, Option<Uuid>) {
    match event {
        SessionEvent::ProposalVoteCast { proposal_id, .. }
        | SessionEvent::ProposalApplied { proposal_id, .. }
        | SessionEvent::ProposalReverted { proposal_id, .. } => (Some(*proposal_id), None),
        SessionEvent::ProposalSetOpened { set } => (None, Some(set.id)),
        SessionEvent::ProposalSetAlternativeAdded { set_id, proposal_id, .. }
        | SessionEvent::ProposalSetQuorumReached { set_id, proposal_id, .. }
        | SessionEvent::ProposalSetAlternativeStopped { set_id, proposal_id, .. }
        | SessionEvent::ProposalSetPreflightRecorded { set_id, proposal_id, .. } => (Some(*proposal_id), Some(*set_id)),
        SessionEvent::ScoutAssessmentPosted { set_id, assessment } => (Some(assessment.proposal_id), Some(*set_id)),
        SessionEvent::ProposalSetRoundAdvanced { set_id, .. }
        | SessionEvent::ProposalSetTuned { set_id, .. } => (None, Some(*set_id)),
        SessionEvent::ProposalSetDecided { set_id, winner, .. } => (*winner, Some(*set_id)),
        _ => (None, None),
    }
}

/// What [`authorize`] judges an event against.
pub struct Ctx<'a> {
    /// The log's fold so far (canonical order, before this event).
    pub fold: &'a ChatSession,
    /// The roster this log's own events built so far (devices, rules).
    pub running: &'a RosterState,
    /// For a chat event that needs the workspace roster: the config roster
    /// at its position (resolved by the fold; holds never reach here).
    pub workspace: Option<&'a RosterState>,
    /// The device-local workspace: role checks are skipped.
    pub trusted: bool,
    /// Workspace authorization is on (a chat log folded against its config
    /// timeline). Off for the config log itself and for unchecked folds.
    pub checked: bool,
}

fn author_of(env: &SessionEventEnvelope) -> &str {
    env.actor_stamp.as_ref().map(|s| s.actor.id.as_str()).unwrap_or("")
}

/// The single authorization entry point for folds and authoring.
pub fn authorize(env: &SessionEventEnvelope, ctx: &Ctx<'_>) -> Decision {
    let author = author_of(env);
    let fold = ctx.fold;

    if !lease_signer_ok(env, fold) {
        return Decision::deny("coordinator write not signed by the lease holder or the run's creator");
    }
    // Bounds hold everywhere, the device-local workspace included.
    match &env.payload {
        SessionEvent::ChatLockdownRaised { chat_id, lockdown } => {
            if !bounds::lockdown_within_bounds(fold, *chat_id, lockdown.id, author) {
                return Decision::deny("too many active lockdowns");
            }
        }
        SessionEvent::FlowRecorded { record } => {
            if !bounds::flow_within_bounds(fold, record) {
                return Decision::deny("flow state bound reached");
            }
        }
        _ => {}
    }
    if ctx.trusted {
        return Decision::Allow;
    }

    // Every author must be a workspace member at the event's position: on the
    // config log, the roster folded so far; on a chat (checked), the config
    // roster at its frontier. Self-validating trust publications are exempt,
    // and a self-enrollment is judged by its invite below.
    let on_config = env.session_id == crate::channel::workspace_config_session_id(env.workspace_id);
    // Agent-signed events (A3): judged by the Agent tier, capped by the
    // agent's owner's role, with the key valid at this position.
    // A message is its author's (checked chat folds).
    if ctx.checked && !on_config {
        let target = match &env.payload {
            SessionEvent::MessageAppended { message } => Some((message.id, false)),
            SessionEvent::MessageChunkReceived { message_id, .. } | SessionEvent::MessageCompleted { message_id, .. } => {
                Some((*message_id, false))
            }
            SessionEvent::MessageRemoved { message_id } => Some((*message_id, true)),
            _ => None,
        };
        if let Some((id, removal)) = target {
            if fold.authz.message_authors.get(&id).is_some_and(|orig| orig != author) {
                let moderator = removal
                    && ctx.workspace.and_then(|w| w.role_of(author)).is_some_and(|r| r.rank() >= WorkspaceRole::Admin.rank());
                if !moderator {
                    return Decision::deny("only its author continues, rewrites or removes a message");
                }
            }
        }
    }
    let roster_here = if ctx.checked { ctx.workspace } else if on_config { Some(ctx.running) } else { None };
    if let Some(roster) = roster_here {
        if env.actor_stamp.as_ref().is_some_and(|s| s.actor.kind == ActorKind::Agent) {
            return authorize_agent(env, ctx, roster);
        }
    }
    let exempt = is_trust_publication(&env.payload) || matches!(env.payload, SessionEvent::MemberAdded { .. });
    if !exempt {
        let member = if ctx.checked {
            ctx.workspace.is_some_and(|at| !author.is_empty() && at.members.contains_key(author))
        } else if on_config {
            !author.is_empty() && fold.members.iter().any(|m| m.id == author)
        } else {
            true
        };
        if !member {
            return Decision::deny("not a workspace member at this position");
        }
    }

    if requires_projection_authz(&env.payload) {
        let mut role = fold.role_of(author);
        // A chat's own roster (its creator is its Owner) never grants a
        // workspace role: on a chat log, governance and workspace-config kinds
        // are capped by the author's workspace role at this position.
        if ctx.checked && !on_config && caps_by_workspace_role(&env.payload) {
            let ws_role = ctx.workspace.and_then(|at| at.role_of(author)).unwrap_or(WorkspaceRole::Viewer);
            if ws_role.rank() < role.rank() {
                role = ws_role;
            }
        }
        // R5: revoking a device your own account certified is self-service.
        if let SessionEvent::DeviceRevoked { device_id } = &env.payload {
            let account = env.actor_stamp.as_ref().and_then(|s| s.actor.account_id);
            if account.is_some_and(|a| ctx.running.account_certified(a, *device_id)) {
                return Decision::Allow;
            }
        }
        // A3: a host certifies a key for an agent bound to it (or, unbound,
        // owned by an account that certified the host), signing with its own
        // device key, which must be the envelope's signer.
        if let SessionEvent::AgentKeyCertified { cert } = &env.payload {
            if let Err(why) = agent_cert_ok(env, cert, ctx.running) {
                return Decision::deny(why);
            }
        }
        // S2: binding an agent to a host or runtime is an Admin action.
        let ws_agents = if ctx.checked && !on_config { ctx.workspace } else { None };
        if let Some(floor) = agent_change_floor(&env.payload, author, fold, ws_agents) {
            if role.rank() < floor.rank() {
                return Decision::deny(format!("role {role:?} cannot change this agent's host, runtime or owner"));
            }
        }
        let d = evaluate(&env.payload, author, role, fold);
        if !d.allowed {
            return Decision::deny(d.summary);
        }
        return Decision::Allow;
    }

    if ctx.checked && needs_workspace_authz(&env.payload) {
        let Some(at) = ctx.workspace else {
            return Decision::deny("no workspace position");
        };
        let Some(role) = at.role_of(author).filter(|_| !author.is_empty()) else {
            return Decision::deny("not a workspace member at this position");
        };
        // No judging content from a position older than the one it was filed
        // at (contains the backdated-frontier residual).
        if let Some(f) = env.actor_stamp.as_ref().and_then(|s| s.cfg_frontier) {
            let (pid, sid) = subject_of(&env.payload);
            let filed_at = pid
                .and_then(|p| fold.authz.content_frontier.get(&p))
                .max(sid.and_then(|s| fold.authz.set_frontier.get(&s)));
            if filed_at.is_some_and(|min| f < *min) {
                return Decision::deny("names a config position older than the content it judges");
            }
        }
        // Rules v2 (A3 transition): a host-signed event that speaks for an
        // agent is judged with the Agent tier. Folds use the highest rules
        // this build knows.
        if at.rules.min(RULES_SUPPORTED) >= 2 {
            if let Some(agent_id) = agent::agent_authored(&env.payload) {
                if let Err(why) = agent::tier_allows(&env.payload, agent_id, env.signer_device_id.unwrap_or_default(), fold) {
                    return Decision::deny(why);
                }
            }
        }
        match &env.payload {
            SessionEvent::ProposalVoteCast { proposal_id, approval } => {
                if approval.actor_id != author {
                    return Decision::deny("a vote must be cast by the voter it names");
                }
                // P0.6: a delegated host (a serve box enrolled as its owner's
                // device) signs as the owner without the owner being there.
                // Until Track B's presence proofs, its votes don't count
                // toward a human gate; `ligus serve` refuses them up front
                // (403 presence_required).
                let account = env.actor_stamp.as_ref().and_then(|s| s.actor.account_id);
                let signer = env.signer_device_id.and_then(|d| at.devices.get(&d));
                if signer.zip(account).is_some_and(|(d, a)| d.delegated_for(a)) {
                    return Decision::deny("a vote signed by a delegated host doesn't count toward the human gate");
                }
                if let (Some(d), Some(p)) =
                    (&approval.diff_digest, fold.proposals.iter().find(|p| p.id == *proposal_id))
                {
                    if *d != votes::diff_digest(p) {
                        return Decision::deny("the vote names different content");
                    }
                }
            }
            SessionEvent::ProposalApplied { proposal_id, diff_digest, .. } => {
                let human = env.actor_stamp.as_ref().is_some_and(|s| s.actor.kind == ActorKind::Human)
                    && at.members.get(author).is_some_and(|m| m.kind == ActorKind::Human);
                if !human {
                    return Decision::deny("only a human member applies a proposal");
                }
                let Some(p) = fold.proposals.iter().find(|p| p.id == *proposal_id) else {
                    return Decision::deny("unknown proposal");
                };
                if votes::diff_digest(p) != *diff_digest {
                    return Decision::deny("applied content differs from the approved content");
                }
                if !p.ready_to_apply() {
                    return Decision::deny("the proposal is not approved");
                }
                if let Some(why) = crate::guard::human_gate_reason(fold, p) {
                    return Decision::deny(why);
                }
            }
            _ => {}
        }
        let d = evaluate(&env.payload, author, role, fold);
        if !d.allowed {
            return Decision::deny(d.summary);
        }
    }
    Decision::Allow
}

/// An agent-signed event (A3): a key valid at this position whose agent the
/// stamp names, an owner who is a member here, the Agent tier, and the
/// owner's role as the cap.
fn authorize_agent(env: &SessionEventEnvelope, ctx: &Ctx<'_>, roster: &RosterState) -> Decision {
    let author = author_of(env);
    let Some(key) = env.signer_device_id.and_then(|k| roster.valid_agent_key(k)) else {
        return Decision::deny("not signed by an agent key valid here");
    };
    if key.agent_id.to_string() != author {
        return Decision::deny("an agent key signs only for its own agent");
    }
    let Some(owner) = roster.agents.get(&key.agent_id).map(|a| a.owner.clone()) else {
        return Decision::deny("unknown agent");
    };
    let Some(cap) = roster.role_of(&owner) else {
        return Decision::deny("the agent's owner is not a workspace member here");
    };
    if let Err(why) = agent::tier_allows(&env.payload, key.agent_id, key.host_device_id, ctx.fold) {
        return Decision::deny(why);
    }
    let d = evaluate(&env.payload, author, cap, ctx.fold);
    if !d.allowed {
        return Decision::deny(d.summary);
    }
    Decision::Allow
}

/// The certificate checks of an `AgentKeyCertified`.
fn agent_cert_ok(
    env: &SessionEventEnvelope,
    cert: &crate::crypto::AgentKeyCertificate,
    roster: &RosterState,
) -> Result<(), String> {
    if env.signer_device_id != Some(cert.host_device_id) {
        return Err("an agent key is certified by the host device that holds it".into());
    }
    let host = roster.devices.get(&cert.host_device_id).filter(|d| !d.revoked && !d.accounts.is_empty());
    let Some(host) = host else {
        return Err("the host device isn't certified here".into());
    };
    cert.verify(&host.public_key).map_err(|e| format!("agent certificate: {e}"))?;
    if !roster.agent_bound_to(cert.agent_id, cert.host_device_id) {
        return Err("the agent isn't bound to this host".into());
    }
    Ok(())
}

/// Kinds whose chat-log role is capped by the workspace role: everything that
/// needs projection authz except archiving a chat (a chat-level act its
/// creator keeps).
fn caps_by_workspace_role(e: &SessionEvent) -> bool {
    !matches!(e, SessionEvent::SessionArchivedChanged { .. })
}

/// Account keys and device certificates: self-validating (the roster checks
/// the chain), published by members and joiners alike.
fn is_trust_publication(e: &SessionEvent) -> bool {
    matches!(
        e,
        SessionEvent::AccountKeyRegistered { .. }
            | SessionEvent::AccountKeyRotated { .. }
            | SessionEvent::DeviceCertificateAdded { .. }
    )
}

/// The role a counted vote carries: the signer's workspace role at its
/// position (the payload's own `role` is ignored).
pub fn counted_role(at: &RosterState, author: &str) -> Option<WorkspaceRole> {
    at.role_of(author)
}

/// S2: the role an agent change needs, when it touches agents. A new agent
/// bound to a host or owned by someone else, or any change of an agent's
/// host, runtime or owner, is
/// Admin; editing an agent you don't own (name, role text, avatar) is Admin
/// unless it is unowned; everything else is the usual Contributor floor.
///
/// On a chat log (`workspace` = the config roster at the event's position), an
/// agent the chat doesn't define yet is judged against the workspace's agent
/// of that id: a chat-level copy shadows it in that chat.
fn agent_change_floor(
    event: &SessionEvent,
    author: &str,
    fold: &ChatSession,
    workspace: Option<&RosterState>,
) -> Option<WorkspaceRole> {
    let one = |new: &WorkspaceAgent| -> WorkspaceRole {
        let in_chat = fold.workspace_agents.iter().find(|a| a.id == new.id);
        if let (None, Some(ws)) = (in_chat, workspace.and_then(|w| w.agents.get(&new.id))) {
            return if ws.host_id != new.host_id || ws.runtime_id != new.runtime_id || ws.owner != new.owner_actor_id {
                WorkspaceRole::Admin
            } else if ws.owner.is_empty() || ws.owner == author {
                WorkspaceRole::Contributor
            } else {
                WorkspaceRole::Admin
            };
        }
        match in_chat {
            None if !new.host_id.is_empty() => WorkspaceRole::Admin,
            // An unbound agent runs on its owner's device: naming someone else
            // as the owner binds it to their device (and runtimes, and keys).
            None if !new.owner_actor_id.is_empty() && new.owner_actor_id != author => WorkspaceRole::Admin,
            None => WorkspaceRole::Contributor,
            Some(old) if old == new => WorkspaceRole::Contributor,
            Some(old)
                if old.host_id != new.host_id
                    || old.runtime_id != new.runtime_id
                    || old.owner_actor_id != new.owner_actor_id =>
            {
                WorkspaceRole::Admin
            }
            Some(old) if old.owner_actor_id.is_empty() || old.owner_actor_id == author => WorkspaceRole::Contributor,
            Some(_) => WorkspaceRole::Admin,
        }
    };
    let max = |a: WorkspaceRole, b: WorkspaceRole| if b.rank() > a.rank() { b } else { a };
    match event {
        SessionEvent::AgentUpserted { agent } => Some(one(agent)),
        SessionEvent::AgentRosterUpdated { agents } => {
            let mut floor = WorkspaceRole::Contributor;
            for a in agents {
                floor = max(floor, one(a));
            }
            Some(floor)
        }
        _ => None,
    }
}

/// Coordinator leases and run writes are bound to devices:
///   * a `CoordinatorLeaseClaimed` must be signed by the device it names;
///   * a `CoordinatorLeaseReleased` by the holder it names;
///   * a `WorkflowRunUpserted` under a lease epoch must match the run's
///     current epoch and be signed by its holder; one **without** an epoch
///     (S4) is accepted only when signed by the device that created the run
///     (`creator_device_id`), or, for a run without one, when its stamped
///     author is the run's initiator.
pub(crate) fn lease_signer_ok(env: &SessionEventEnvelope, fold: &ChatSession) -> bool {
    let signer = env.signer_device_id;
    match &env.payload {
        SessionEvent::CoordinatorLeaseClaimed { lease } => signer == Some(lease.holder),
        SessionEvent::CoordinatorLeaseReleased { release } => signer == Some(release.holder),
        SessionEvent::WorkflowRunUpserted { run } => {
            let existing = fold.workflow_runs.iter().find(|r| r.id == run.id);
            let Some(epoch) = run.lease_epoch else {
                let (creator, initiator) = match existing {
                    Some(r) => (r.creator_device_id, r.initiator_actor_id.as_str()),
                    None => (run.creator_device_id, run.initiator_actor_id.as_str()),
                };
                return match creator {
                    Some(c) => signer == Some(c),
                    None => !initiator.is_empty() && author_of(env) == initiator,
                };
            };
            let key = crate::lease::lease_key(crate::lease::LeaseScope::WorkflowRun, &run.id.to_string());
            match fold.guards.leases.get(&key) {
                None => false,
                Some(state) if epoch > state.current.epoch => false,
                Some(state) if epoch == state.current.epoch => signer == Some(state.current.holder),
                Some(_) => true,
            }
        }
        _ => true,
    }
}

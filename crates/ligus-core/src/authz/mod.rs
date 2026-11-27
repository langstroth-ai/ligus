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
//!   Every workspace starts at version 2; a `WorkspaceRulesSet` needs its
//!   author's presence once the workspace holds any credential (the genesis
//!   one, written before the founder's first passkey, can't).
//! * **Bounds** (S5 lockdowns, S6 flow state) and the run-write rule (S4) are
//!   fold rules too.
//!
//! The device-local workspace (never synced, single human) is exempt from
//! role checks, as before.

pub mod agent;
pub mod attachments;
pub mod bounds;
pub mod timeline;
pub mod votes;
pub mod passkeys;
pub mod links;
pub mod profiles;
pub mod retire;
pub mod leave;
pub mod phone;
pub mod rules;

use std::collections::BTreeMap;

use uuid::Uuid;

pub use timeline::{AgentKeyState, AgentState, ConfigTimeline, Cut, CutKind, DeviceState, MemberState, RosterState, RosterTracker};

use crate::agent::WorkspaceAgent;
use crate::authorization::{evaluate, requires_projection_authz};
use crate::events::{SessionEvent, SessionEventEnvelope};
use crate::identity::{ActorKind, CfgFrontier, WorkspaceRole};
use crate::session::ChatSession;

/// The workspace rules version. 0.1 starts every workspace at version 2
/// (human presence for governance, the Agent tier for host-signed agent
/// output); there is no version 1 (Phase A: 0.1 starts fresh, so the rules
/// without presence were deleted rather than kept behind a switch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RulesVersion(pub u32);

/// The highest rules version this build enforces.
pub const RULES_SUPPORTED: u32 = 3;
/// Rules version 3 (identity model v2, "Rules 3 in 0.4"): a certificate
/// counts only with its device's own acceptance, a retired member is
/// removed, inline credentials and member records whose ids disagree are
/// denied, and hosts of former members are inert.
pub const RULES_V3: u32 = 3;
/// The rules a workspace founded by this build starts at.
pub const RULES_NEW_WORKSPACE: u32 = 3;
/// The rules every workspace starts at (the seed's roster), and the lowest
/// version a `WorkspaceRulesSet` may name.
pub const RULES_GENESIS: u32 = 2;

/// The refusal of an event that needs its author's passkey presence (B3).
/// Authoring paths match on it to ask for a passkey confirmation.
pub const PRESENCE_REQUIRED: &str = "this needs a passkey confirmation from its author";

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
    /// Its `log_head` names an event of its own log this device doesn't
    /// hold yet ([`crate::lamport`]).
    LogHeadAhead,
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
    /// Signed raiser of each lockdown, by the event that raised it (S5
    /// accounting; the first copy's, when signers' copies share the id).
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
    /// The signed author of each apply request: they (or an Admin) withdraw it.
    pub apply_requesters: BTreeMap<Uuid, String>,
    /// The turns a person wrote themselves, by their author: a User message
    /// signed for a human and not a runtime effect, never since rewritten or
    /// continued by anything else. Only these name local files
    /// (`[Attached: <path>]`, issue #118): an agent's reply or a runtime
    /// effect the host signs under its person's identity (an automatic
    /// pickup posts whatever title someone else filed) is not the person.
    /// Kept in the device-local workspace too.
    pub person_messages: BTreeMap<Uuid, String>,
    /// The signed author of each proposal comment folded from its own
    /// `ProposalRefined`, by comment id (the first copy's). A comment counts
    /// as its author's only when this names them: what is forwarded to an
    /// agent ("Send to agent to revise") is only these. A comment that came
    /// some other way (a compaction seed) is shown but never forwarded. Kept
    /// in the device-local workspace too.
    pub refinement_authors: BTreeMap<Uuid, String>,
    /// The signing device of each host's latest allowed record, by host id
    /// (workspace model v3, 5.1). Readers and pickers use only a host whose
    /// latest record its own device signed ([`crate::ChatSession::bound_hosts`]),
    /// never one someone else wrote under its id (H11); the fold's
    /// `workspace_hosts` keeps every allowed record for authorization.
    pub host_signers: BTreeMap<String, Option<Uuid>>,
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
        || matches!(
            event,
            SessionEvent::ProposalVoteCast { .. }
                | SessionEvent::ProposalApplied { .. }
                | SessionEvent::ProposalApplyRequested { .. }
                | SessionEvent::ProposalApplyFailed { .. }
                | SessionEvent::ProposalApplyWithdrawn { .. }
                | SessionEvent::ChatRepoSet { .. }
                | SessionEvent::ChatBasePinned { .. }
                | SessionEvent::ChatBaseRepinned { .. }
                | SessionEvent::AttachmentShared { .. }
                | SessionEvent::AttachmentDeleted { .. }
        )
}

/// The proposal and set an event concerns (for holds and side-effect blocks).
pub fn subject_of(event: &SessionEvent) -> (Option<Uuid>, Option<Uuid>) {
    match event {
        SessionEvent::ProposalVoteCast { proposal_id, .. }
        | SessionEvent::ProposalApplied { proposal_id, .. }
        | SessionEvent::ProposalReverted { proposal_id, .. } => (Some(*proposal_id), None),
        SessionEvent::ProposalApplyRequested { request } => (Some(request.proposal_id), None),
        SessionEvent::ProposalApplyFailed { proposal_id, .. } | SessionEvent::ProposalApplyWithdrawn { proposal_id, .. } => {
            (Some(*proposal_id), None)
        }
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
            if lockdown.raise_event_id.is_some() {
                return Decision::deny("a lockdown is known by the event that raised it, never by its own claim");
            }
            if !bounds::lockdown_within_bounds(fold, *chat_id, author) {
                return Decision::deny("too many active lockdowns");
            }
        }
        SessionEvent::ChatLockdownCleared { raises, .. } => {
            if let Err(why) = bounds::clear_names_raises(raises) {
                return Decision::deny(why);
            }
        }
        SessionEvent::ProposalArchivePolicySet { policy } => {
            if policy.after_days > crate::proposal_archive::MAX_AFTER_DAYS {
                return Decision::deny("auto-archive is at most a year");
            }
            if env.session_id != crate::channel::workspace_config_session_id(env.workspace_id) {
                return Decision::deny("the auto-archive setting is kept on the workspace config log");
            }
        }
        SessionEvent::FlowRecorded { record } => {
            if !bounds::flow_within_bounds(fold, record) {
                return Decision::deny("flow state bound reached");
            }
            // Track D (D.4.2): only a person answers an agent's question.
            if matches!(record, crate::flow::FlowRecord::HumanAnswered(_))
                && !env.actor_stamp.as_ref().is_some_and(|s| s.actor.kind == ActorKind::Human)
            {
                return Decision::deny("only a human answers an agent's question");
            }
            // Memory and task state (`crate::memory`): their log, the ids
            // bound to their events, then the fold's own rules (bounds, a
            // known base, one writer per slot).
            let on_config_log = env.session_id == crate::channel::workspace_config_session_id(env.workspace_id);
            if crate::memory::is_memory(record) && !on_config_log {
                return Decision::deny("workspace memory is kept on the workspace config log");
            }
            if matches!(record, crate::flow::FlowRecord::TaskState(_)) && on_config_log {
                return Decision::deny("task state is kept on its chat's log");
            }
            if let Err(why) = crate::memory::id_binding_ok(record, env.event_id) {
                return Decision::deny(why);
            }
            if let Some(Err(why)) = crate::memory::validate(&fold.flow, record, env.signer_device_id, author) {
                return Decision::deny(why);
            }
            // A daily outcome summary speaks for the device that signed it:
            // the fold keys summaries by (agent, host, day), so another
            // member's device must not replace or invent a host's numbers.
            if let crate::flow::FlowRecord::AgentOutcomeSummary(sum) = record {
                if env.signer_device_id.map(|d| d.to_string()).as_deref() != Some(sum.host_id.as_str()) {
                    return Decision::deny("an outcome summary is signed by its own host");
                }
            }
        }
        _ => {}
    }
    if ctx.trusted {
        return Decision::Allow;
    }
    // A proposal's comment thread is append-only and every comment is its
    // signer's own.
    if let Some(why) = thread_refusal(env, author, fold) {
        return Decision::deny(why);
    }

    // Every author must be a workspace member at the event's position: on the
    // config log, the roster folded so far; on a chat (checked), the config
    // roster at its frontier. Self-validating trust publications are exempt,
    // and a self-enrollment is judged by its invite below.
    let on_config = env.session_id == crate::channel::workspace_config_session_id(env.workspace_id);
    // B1 old-epoch policy: an event that arrived sealed under epoch `e` is
    // accepted only if `e` is at least the authorized epoch at its author's
    // frontier (an author who had seen a rotation must have used it).
    if let Some(why) = stale_epoch(env, ctx, on_config) {
        return Decision::deny(why);
    }
    // File attachments (issue #118): what holds on every log and tier.
    if let Some(why) = attachments::payload_refusal(env, on_config, ctx.checked && !on_config, if ctx.checked { ctx.workspace } else { None }, ctx.running) {
        return Decision::deny(why);
    }
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
        // An applied proposal's content is what the chat's ledger replays on
        // every device that hasn't applied it yet
        // (`workspace_repo::ledger`): it never changes after the apply, or a
        // re-upsert (its producing agent's own, or anyone's) would have
        // other devices replay bytes nobody approved. A title edit is fine.
        if let SessionEvent::ProposalUpserted { proposal } = &env.payload {
            if let Some(old) = fold.proposals.iter().find(|p| p.id == proposal.id && p.status == crate::proposals::ProposalStatus::Applied) {
                let mut incoming = proposal.clone();
                incoming.repo_id = old.repo_id;
                if votes::diff_digest(old) != votes::diff_digest(&incoming) {
                    return Decision::deny("an applied proposal's content can't change");
                }
            }
        }
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
    // Authorship (defense in depth over ingest, which quarantines both): a
    // device certified here signs only for an account it is certified for,
    // and a stamp names its author's own member account. Otherwise one
    // member's device could sign a message, a clear or a memory write
    // stamped as another member, and any store that took it in would fold
    // it as theirs. Identity publications and self-enrollments are judged by
    // their own rules.
    if let (Some(roster), Some(stamp)) = (roster_here, env.actor_stamp.as_ref()) {
        let own_rules = is_trust_publication(&env.payload)
            || matches!(env.payload, SessionEvent::MemberAdded { .. } | SessionEvent::PasskeyRegistered { .. });
        if stamp.actor.kind != ActorKind::Agent && !own_rules {
            if let Some(account) = stamp.actor.account_id {
                if roster.members.get(author).and_then(|m| m.account_id).is_some_and(|a| a != account) {
                    return Decision::deny("stamped with an account that isn't its author's");
                }
                let device = env.signer_device_id.and_then(|d| roster.devices.get(&d));
                if device.is_some_and(|d| !d.accounts.is_empty() && !d.accounts.contains(&account)) {
                    return Decision::deny("signed by a device that doesn't act for its author");
                }
            }
        }
    }
    if let Some(roster) = roster_here {
        if env.actor_stamp.as_ref().is_some_and(|s| s.actor.kind == ActorKind::Agent) {
            return authorize_agent(env, ctx, roster);
        }
        // A phone's own key: the phone tier (Phase A, item 6).
        if let Some(device) = env.signer_device_id.filter(|d| roster.devices.get(d).is_some_and(|x| x.phone)) {
            if let Err(why) = phone::phone_tier_allows(&env.payload, device) {
                return Decision::deny(why);
            }
            // A phone posts new messages only: its box picks the message id,
            // and the phone can't tell a fresh id from one of its person's
            // earlier messages, which the post would otherwise rewrite.
            if let SessionEvent::MessageAppended { message } = &env.payload {
                if fold.authz.message_authors.contains_key(&message.id) || fold.messages.iter().any(|m| m.id == message.id) {
                    return Decision::deny("a phone posts new messages only");
                }
            }
        }
    }
    // A service member (Phase A, item 6) is capped at Contributor, never
    // decides a proposal set, and never has presence.
    if let Some(why) = service_member_refusal(env, author, roster_here, on_config, fold) {
        return Decision::deny(why);
    }
    // Box admissions (Phase B): what an admission may say when it is issued,
    // and who may redeem it.
    if let Some(why) = crate::admission::issuance_refusal(env, fold, on_config) {
        return Decision::deny(why);
    }
    if let (true, SessionEvent::MemberAdded { member, invite, proof }) = (on_config, &env.payload) {
        if let Some(why) = crate::admission::redemption_refusal(env, member, *invite, proof.as_deref(), fold) {
            return Decision::deny(why);
        }
        // The proof names the signing device, so that device must really
        // have signed. Ingest verifies every envelope against its device's
        // certificate wherever that sorts; here, when the device is already
        // certified at this position (a box publishes its identity after it
        // joins, so usually it isn't), the fold checks it too: certified for
        // the stamp's account, not revoked, and the signature verifies.
        if member.id == author
            && invite.is_some_and(|id| crate::admission::admission_of(fold, id).is_some() || fold.guards.invite_keys.contains_key(&id))
        {
            let known = env.signer_device_id.is_some_and(|d| ctx.running.devices.contains_key(&d));
            if known {
                if let Err(why) = certified_signer(env, ctx.running) {
                    return Decision::deny(format!("an admission is redeemed by its own certified device: {why}"));
                }
            }
        }
    }
    if let Some(why) = crate::admission::closure_refusal(env, fold, on_config) {
        return Decision::deny(why);
    }
    // A phone is enrolled on the config log by a trusted device of its own
    // account (certified, not revoked, not delegated, not a phone), with that
    // account's passkey presence (the presence table).
    if let SessionEvent::DeviceCertificateAdded { certificate } = &env.payload {
        // A device id is the one its key derives (`crypto::device_id_for`):
        // a certificate of any other id is someone else's device id under a
        // key of their own (squatting), refused wherever it sorts.
        if !certificate.id_matches_key() {
            return Decision::deny("a device certificate names the device id its key derives");
        }
        // A phone certificate never takes over a device id this roster
        // already holds as a desktop or box, or as a phone with another key:
        // it would replace that device's key (and make it a phone) without
        // the enrollment below, since "already certified" reads as a
        // re-publish that needs no presence.
        if certificate.alg == crate::crypto::SigAlg::P256 {
            if let Some(d) = ctx.running.devices.get(&certificate.device_id) {
                if !d.phone || d.public_key != certificate.device_public_key {
                    return Decision::deny("a phone certificate can't take over a device already certified here with another key, or as a desktop or box");
                }
            }
        }
        // A copy carrying the phone's own statement of a certificate already
        // allowed here is the phone accepting it (rules 3): any member's
        // device may relay it (its box).
        let consent_copy = ctx.running.unaccepted.contains(&(certificate.device_id, certificate.account_id))
            && certificate.phone_consent_ok(env.workspace_id);
        if certificate.alg == crate::crypto::SigAlg::P256 && !consent_copy && !ctx.running.account_certified(certificate.account_id, certificate.device_id) {
            if !on_config {
                return Decision::deny("a phone is enrolled on the workspace config log");
            }
            let enroller_ok = certified_signer(env, ctx.running).is_ok_and(|d| {
                d.accounts.contains(&certificate.account_id) && !d.delegated_for(certificate.account_id) && !d.phone
            });
            if !enroller_ok || env.signer_device_id == Some(certificate.device_id) {
                return Decision::deny("a phone is enrolled by a trusted device of its own account");
            }
        }
    }
    // A host-signed chat event that speaks for an agent (its reply, its
    // streaming, a proposal it produced) is judged with the Agent tier,
    // whether or not the host could certify a key for that agent.
    if ctx.checked && !on_config && ctx.workspace.is_some() {
        let speaks_for = agent::agent_authored(&env.payload).or_else(|| match &env.payload {
            SessionEvent::MessageChunkReceived { message_id, .. } | SessionEvent::MessageCompleted { message_id, .. } => fold
                .messages
                .iter()
                .find(|m| m.id == *message_id && m.role != crate::chat::MessageRole::User)
                .and_then(|m| m.responder_id),
            _ => None,
        });
        if let Some(agent_id) = speaks_for {
            if let Err(why) = agent::tier_allows(&env.payload, agent_id, env.signer_device_id.unwrap_or_default(), fold) {
                return Decision::deny(why);
            }
        }
    }
    // An effect of the chat's primary runtime (host-signed, no agent
    // identity) is judged at the Agent tier too.
    if env.actor_stamp.as_ref().is_some_and(|s| s.runtime_effect) && roster_here.is_some() {
        if let Err(why) = agent::runtime_tier_allows(&env.payload) {
            return Decision::deny(why);
        }
    }
    if let SessionEvent::FlowRecorded { record } = &env.payload {
        if let Some(why) = memory_refusal(env, record, ctx, author) {
            return Decision::deny(why);
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

    // Rules 3 (identity model v2): records whose ids disagree, and model
    // keys stored in the workspace, are refused.
    let rules_here = if on_config { ctx.running.rules } else { ctx.workspace.map(|w| w.rules).unwrap_or(ctx.running.rules) };
    if rules_here >= RULES_V3 {
        if let Some(why) = rules3_refusal(&env.payload) {
            return Decision::deny(why);
        }
    }

    // The workspace rules: the relying party is well formed and on the
    // config log; only the Owner changes it (with presence verified under
    // the policy in force before the change) or the version.
    if let SessionEvent::WorkspaceRulesSet { version, presence } = &env.payload {
        if !on_config {
            return Decision::deny("the workspace rules are set on the workspace config log");
        }
        if let Some(p) = presence {
            if let Err(e) = p.well_formed() {
                return Decision::deny(format!("presence policy: {}", e.0));
            }
        }
        if *version != ctx.running.rules && fold.role_of(author) != WorkspaceRole::Owner {
            return Decision::deny("only the Owner changes the rules version");
        }
        // Rules only tighten: a workspace on rules 3 never goes back (its
        // older devices are read-only there already).
        if *version >= RULES_GENESIS && *version < ctx.running.rules && ctx.running.rules_written {
            return Decision::deny("the rules version never goes back");
        }
        // The relying party decides which page every member's desktop opens
        // for each passkey ceremony and what a re-registration under it binds:
        // the Owner's call, like the version.
        if presence.as_ref().is_some_and(|p| *p != ctx.running.presence_policy) && fold.role_of(author) != WorkspaceRole::Owner {
            return Decision::deny("only the Owner changes the relying party");
        }
    }

    // Phase C: the sandbox grants live on the config log, are well formed,
    // and only the Owner turns an agent's shell or local-servers grant on or
    // off (those are the Owner's; path grants are an Admin's). Presence
    // below.
    if let SessionEvent::WorkspaceSandboxSet { grants } = &env.payload {
        if !on_config {
            return Decision::deny("the sandbox grants are set on the workspace config log");
        }
        if let Err(e) = grants.well_formed() {
            return Decision::deny(format!("sandbox grants: {}", e.0));
        }
        if !grants.shell_changes(&fold.guards.sandbox_grants).is_empty() && fold.role_of(author) != WorkspaceRole::Owner {
            return Decision::deny("only the Owner grants or withdraws an agent's shell");
        }
        // Local servers open the machine's own addresses to the agent (on
        // macOS, listening on any of them): the Owner's, like the shell.
        if !grants.local_servers_changes(&fold.guards.sandbox_grants).is_empty() && fold.role_of(author) != WorkspaceRole::Owner {
            return Decision::deny("only the Owner lets an agent run local servers");
        }
    }

    // The config log's title is the workspace's name: the phone shows it when
    // it asks to be added to someone's devices "in <workspace>", and boxes put
    // it in their invites. An Admin's or Owner's, not content: a Contributor
    // or Viewer could otherwise rename the workspace (or name it first) to
    // pass one workspace off as another. A chat's title stays content.
    if on_config && matches!(env.payload, SessionEvent::SessionTitleChanged { .. }) && fold.role_of(author).rank() < WorkspaceRole::Admin.rank() {
        return Decision::deny("only an Owner or Admin names the workspace");
    }

    // B3: human presence. A stamped assertion must verify, and the events of
    // the presence table need one. Passkey registrations prove themselves
    // below. Rules: every workspace is on version 2 from its seed; a version
    // below that doesn't exist.
    let mut present: Option<Uuid> = None;
    if let SessionEvent::WorkspaceRulesSet { version, .. } = &env.payload {
        if *version < RULES_GENESIS {
            return Decision::deny("rules version 1 doesn't exist: every workspace is on version 2 or later");
        }
    }
    if let Some(roster) = roster_here {
        if !matches!(env.payload, SessionEvent::PasskeyRegistered { .. }) {
            match passkeys::verify_stamped(env, roster) {
                Ok(p) => present = p,
                Err(why) => return Decision::deny(why),
            }
        }
        // The genesis rules set (version 2 and the relying party, before
        // anyone holds a credential) can't carry presence; any other one
        // needs it.
        // Once: the first rules set on the log, never a second one written
        // while the workspace still holds no credential.
        let genesis_rules = matches!(env.payload, SessionEvent::WorkspaceRulesSet { version, .. } if version == RULES_GENESIS)
            && on_config
            && !roster.rules_written
            && roster.passkeys.values().all(|m| m.is_empty());
        // Rules 3: a device accepting a certificate someone else published
        // for it (with presence) publishes its own copy, which needs none.
        // So does a copy of a phone's certificate carrying the phone's own
        // statement (its acceptance), whoever relays it (its box).
        let accepting = matches!(&env.payload, SessionEvent::DeviceCertificateAdded { certificate: c }
            if roster.unaccepted.contains(&(c.device_id, c.account_id))
                && (env.signer_device_id == Some(c.device_id) || c.phone_consent_ok(env.workspace_id)));
        if !genesis_rules && !accepting && rules::requires_presence(&env.payload, author, roster, fold) {
            // A device is added with its own account's presence, whoever
            // publishes the certificate.
            let whose = match &env.payload {
                SessionEvent::DeviceCertificateAdded { certificate } => Some(certificate.account_id),
                _ => None,
            };
            if present.is_none() || whose.is_some_and(|a| present != Some(a)) {
                return Decision::deny(PRESENCE_REQUIRED);
            }
        }
    }
    match &env.payload {
        SessionEvent::PasskeyRegistered { credential } => {
            return match passkeys::registration_ok(env, credential, on_config, ctx.running) {
                Ok(()) => Decision::Allow,
                Err(why) => Decision::deny(why),
            };
        }
        SessionEvent::PasskeyRemoved { account_id, credential_id } => {
            return match passkeys::removal_ok(env, *account_id, credential_id, on_config, ctx.running, fold, author) {
                Ok(()) => Decision::Allow,
                Err(why) => Decision::deny(why),
            };
        }
        SessionEvent::IdentityLinkPublished { link } => {
            return match links::publish_ok(env, link, on_config, ctx.running) {
                Ok(()) => Decision::Allow,
                Err(why) => Decision::deny(why),
            };
        }
        SessionEvent::IdentityLinkRemoved { account_id, link_id } => {
            return match links::removal_ok(env, *account_id, *link_id, on_config, ctx.running, fold, author) {
                Ok(()) => Decision::Allow,
                Err(why) => Decision::deny(why),
            };
        }
        SessionEvent::ProfileUpdated { profile } => {
            return match profiles::profile_ok(env, profile, on_config, ctx.running) {
                Ok(()) => Decision::Allow,
                Err(why) => Decision::deny(why),
            };
        }
        SessionEvent::DeviceLabelSet { label } => {
            return match profiles::label_ok(env, label, on_config, ctx.running) {
                Ok(()) => Decision::Allow,
                Err(why) => Decision::deny(why),
            };
        }
        SessionEvent::MemberRetired { retirement } => {
            return match retire::retirement_ok(env, retirement, on_config, ctx.running, fold) {
                Ok(()) => Decision::Allow,
                Err(why) => Decision::deny(why),
            };
        }
        SessionEvent::MemberLeft { member_id, account_id, .. } => {
            return match leave::leave_ok(env, member_id, *account_id, on_config, ctx.running, fold) {
                Ok(()) => Decision::Allow,
                Err(why) => Decision::deny(why),
            };
        }
        _ => {}
    }

    if let SessionEvent::DeviceKeyAgreementPublished { device_id, ka_public } = &env.payload {
        return match ka_publication_ok(env, *device_id, ka_public, on_config, ctx.running) {
            Ok(()) => Decision::Allow,
            Err(why) => Decision::deny(why),
        };
    }

    if requires_projection_authz(&env.payload) {
        let mut role = fold.role_of(author);
        // B1: a key rotation follows the authorized head by exactly one and
        // is signed by a certified device of its author.
        if let SessionEvent::WorkspaceKeyRotated { rotation } = &env.payload {
            if let Err(why) = rotation_ok(env, rotation, on_config, ctx.running) {
                return Decision::deny(why);
            }
        }
        // A chat's own roster (its creator is its Owner) never grants a
        // workspace role: on a chat log, governance and workspace-config kinds
        // are capped by the author's workspace role at this position.
        if ctx.checked && !on_config && caps_by_workspace_role(&env.payload) {
            let ws_role = ctx.workspace.and_then(|at| at.role_of(author)).unwrap_or(WorkspaceRole::Viewer);
            if ws_role.rank() < role.rank() {
                role = ws_role;
            }
        }
        // R5: revoking a device your own account holds is self-service (not
        // someone else's device whose key you certified for yourself, nor one
        // that can't tell you from such an adopter).
        if let SessionEvent::DeviceRevoked { device_id, .. } = &env.payload {
            let account = env.actor_stamp.as_ref().and_then(|s| s.actor.account_id);
            if account.is_some_and(|a| ctx.running.account_owns_device(a, *device_id)) {
                return Decision::Allow;
            }
            // Rules 3: withdrawing a certificate of yours its device never
            // accepted (approved, never finished: identity model v2, 8.2).
            // Only when every certificate of that device is the author's own:
            // a revocation revokes the whole device id, so a member who
            // certified someone else's unaccepted device can't burn it.
            if account.is_some_and(|a| {
                ctx.running.unaccepted.contains(&(*device_id, a))
                    && ctx.running.unaccepted.iter().filter(|(d, _)| d == device_id).all(|(_, x)| *x == a)
                    && !ctx.running.devices.contains_key(device_id)
            }) {
                return Decision::Allow;
            }
        }
        // Never above you: no one grants a role above their own, or removes,
        // demotes or revokes a device of a member ranked above them. A cut
        // (Phase A, item 2) also judges its subject's history, so an Admin
        // revoking the founder's device with an empty watermark would
        // otherwise erase the workspace's genesis.
        let ranks = if on_config { ctx.running } else { ctx.workspace.unwrap_or(ctx.running) };
        if let Some(why) = rank_rules(&env.payload, author, role, ranks) {
            return Decision::deny(why);
        }
        // A3: a host certifies a key for an agent bound to it (or, unbound,
        // owned by an account that certified the host), signing with its own
        // device key, which must be the envelope's signer.
        if let SessionEvent::AgentKeyCertified { cert } = &env.payload {
            if let Err(why) = agent_cert_ok(env, cert, ctx.running) {
                return Decision::deny(why);
            }
        }
        // Workspace repos (repos-and-checkouts 3.1, review F5/F17).
        if let Err(why) = repo_event_ok(&env.payload, on_config, ctx.running) {
            return Decision::deny(why);
        }
        // S2: binding an agent to a host or runtime is an Admin action.
        let ws_agents = if ctx.checked && !on_config { ctx.workspace } else { None };
        if let Some(floor) = agent_change_floor(&env.payload, author, fold, ws_agents) {
            if role.rank() < floor.rank() {
                return Decision::deny(format!("role {role:?} cannot change this agent's host, runtime or owner"));
            }
        }
        // Who may use an agent, and its budgets, are its owner's (an Admin's
        // on an unowned agent): never another member's, whatever their role.
        if let Some(why) = agent_policy_refusal(&env.payload, author, role, fold, ws_agents) {
            return Decision::deny(why);
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
        match &env.payload {
            SessionEvent::ProposalVoteCast { proposal_id, approval } => {
                if approval.actor_id != author {
                    return Decision::deny("a vote must be cast by the voter it names");
                }
                // Phase B: a vote counts as its voter's only when a device
                // certified for the voter's account at this position (not
                // revoked) signed it: a phone votes for its own person and
                // nobody else, and an uncertified key votes for no one.
                // (Ingest already quarantines both; the fold doesn't rely on it.)
                if !voter_signed(env, at) {
                    return Decision::deny("a vote is signed by a certified device of its voter");
                }
                // P0.6 / B3: a delegated host (a serve box enrolled as its
                // owner's device) signs as the owner without the owner being
                // there. Its vote counts only with the owner's passkey
                // presence over it (the phone's `presence/prepare` and
                // `presence/commit`): the counted human is the credential's
                // account, which must be the voter's.
                let account = env.actor_stamp.as_ref().and_then(|s| s.actor.account_id);
                let signer = env.signer_device_id.and_then(|d| at.devices.get(&d));
                if signer.zip(account).is_some_and(|(d, a)| d.delegated_for(a)) && (present.is_none() || present != account) {
                    // A delegated host's vote needs the owner's presence.
                    return Decision::deny(PRESENCE_REQUIRED);
                }
                if let (Some(d), Some(p)) =
                    (&approval.diff_digest, fold.proposals.iter().find(|p| p.id == *proposal_id))
                {
                    if *d != votes::diff_digest(p) {
                        return Decision::deny("the vote names different content");
                    }
                }
            }
            SessionEvent::ProposalApplied {
                proposal_id, diff_digest, repo_id, applied_on, base_commit, applied_tree_hash, request_id, ..
            } => {
                if env.actor_stamp.as_ref().is_some_and(|s| s.runtime_effect) {
                    return Decision::deny("a runtime never applies a proposal");
                }
                let signer = env.signer_device_id;
                if applied_on.is_some() && *applied_on != signer {
                    return Decision::deny("an apply names the device that applied it, which signs it");
                }
                if [base_commit, applied_tree_hash].into_iter().flatten().any(|c| !crate::workspace_repo::is_commit_id(c)) {
                    return Decision::deny("malformed commit or tree id");
                }
                match request_id {
                    // Fulfilling a person's request: only the host they named,
                    // for the digest and base they saw.
                    Some(rid) => {
                        let Some(req) = fold.apply_requests.get(rid) else {
                            return Decision::deny("no such apply request");
                        };
                        if req.proposal_id != *proposal_id || req.diff_digest != *diff_digest {
                            return Decision::deny("the apply doesn't match its request");
                        }
                        if signer != Some(req.host_device_id) || applied_on.is_none() {
                            return Decision::deny("only the host a request names fulfils it");
                        }
                        if req.expected_base_commit.is_some() && *base_commit != req.expected_base_commit {
                            return Decision::deny("applied onto another base than the person saw");
                        }
                        // Once: a retry, or the same host applying again,
                        // never fulfils a closed request.
                        if fold.apply_outcomes.contains_key(rid) {
                            return Decision::deny("that apply request is already closed");
                        }
                    }
                    // Applying yourself: a human member, through their own
                    // device (not a host acting for them).
                    None => {
                        let human = env.actor_stamp.as_ref().is_some_and(|s| s.actor.kind == ActorKind::Human)
                            && at.members.get(author).is_some_and(|m| m.kind == ActorKind::Human);
                        if !human {
                            return Decision::deny("only a human member applies a proposal");
                        }
                        if !own_device(env, at) {
                            return Decision::deny("a person applies through their own device");
                        }
                        // One applier per proposal: while a request names a
                        // host, only that host applies it.
                        if fold.open_apply_request(*proposal_id).is_some() {
                            return Decision::deny("an apply of this proposal is already requested on a host");
                        }
                    }
                }
                let Some(p) = fold.proposals.iter().find(|p| p.id == *proposal_id) else {
                    return Decision::deny("unknown proposal");
                };
                if votes::diff_digest(p) != *diff_digest {
                    return Decision::deny("applied content differs from the approved content");
                }
                if p.repo_id.is_some() && *repo_id != p.repo_id {
                    return Decision::deny("applied to another repository than the proposal's");
                }
                if !p.ready_to_apply() {
                    return Decision::deny("the proposal is not approved");
                }
                if let Some(why) = crate::guard::human_gate_reason(fold, p) {
                    return Decision::deny(why);
                }
            }
            SessionEvent::ProposalApplyRequested { request } => {
                if env.actor_stamp.as_ref().is_some_and(|s| s.runtime_effect) {
                    return Decision::deny("a runtime never asks for an apply");
                }
                let human = env.actor_stamp.as_ref().is_some_and(|s| s.actor.kind == ActorKind::Human)
                    && at.members.get(author).is_some_and(|m| m.kind == ActorKind::Human);
                if !human || !own_device(env, at) {
                    return Decision::deny("a person asks for an apply through their own device");
                }
                if fold.apply_requests.contains_key(&request.id) {
                    return Decision::deny("that apply request already exists");
                }
                // One applier per proposal: a second request waits until the
                // open one is applied, fails or is withdrawn.
                if fold.open_apply_request(request.proposal_id).is_some() {
                    return Decision::deny("an apply of this proposal is already requested on a host");
                }
                let Some(p) = fold.proposals.iter().find(|p| p.id == request.proposal_id) else {
                    return Decision::deny("unknown proposal");
                };
                if votes::diff_digest(p) != request.diff_digest {
                    return Decision::deny("the request names other content than the approved content");
                }
                if !p.ready_to_apply() {
                    return Decision::deny("the proposal is not approved");
                }
                if request.repo_id != p.repo_id {
                    return Decision::deny("the request names another repository than the proposal's");
                }
                if request.expected_base_commit.as_deref().is_some_and(|c| !crate::workspace_repo::is_commit_id(c)) {
                    return Decision::deny("malformed base commit");
                }
                if !at.device_trusted(request.host_device_id) {
                    return Decision::deny("the host isn't a trusted device here");
                }
                // A phone applies nothing (its tier): a request naming one
                // would only hold the proposal's single applier slot.
                if at.devices.get(&request.host_device_id).is_some_and(|d| d.phone) {
                    return Decision::deny("a phone isn't a host");
                }
                if let Some(why) = crate::guard::human_gate_reason(fold, p) {
                    return Decision::deny(why);
                }
            }
            SessionEvent::ProposalApplyFailed { request_id, proposal_id, reason } => {
                if env.actor_stamp.as_ref().is_some_and(|s| s.runtime_effect) {
                    return Decision::deny("a runtime never reports an apply");
                }
                let Some(req) = fold.apply_requests.get(request_id).filter(|r| r.proposal_id == *proposal_id) else {
                    return Decision::deny("no such apply request");
                };
                if env.signer_device_id != Some(req.host_device_id) {
                    return Decision::deny("only the host a request names reports on it");
                }
                if fold.apply_outcomes.contains_key(request_id) {
                    return Decision::deny("that apply request is already closed");
                }
                if reason.chars().count() > crate::workspace_repo::MAX_APPLY_FAILURE_CHARS {
                    return Decision::deny("the failure reason is too long");
                }
            }
            SessionEvent::ProposalApplyWithdrawn { request_id, proposal_id } => {
                if env.actor_stamp.as_ref().is_some_and(|s| s.runtime_effect) {
                    return Decision::deny("a runtime never withdraws an apply");
                }
                let human = env.actor_stamp.as_ref().is_some_and(|s| s.actor.kind == ActorKind::Human)
                    && at.members.get(author).is_some_and(|m| m.kind == ActorKind::Human);
                if !human || !own_device(env, at) {
                    return Decision::deny("a person withdraws an apply through their own device");
                }
                if fold.apply_requests.get(request_id).is_none_or(|r| r.proposal_id != *proposal_id) {
                    return Decision::deny("no such apply request");
                }
                if fold.apply_outcomes.contains_key(request_id) {
                    return Decision::deny("that apply request is already closed");
                }
                let requester = fold.authz.apply_requesters.get(request_id).map(String::as_str);
                if requester != Some(author) && role.rank() < WorkspaceRole::Admin.rank() {
                    return Decision::deny("only its requester or an Admin withdraws an apply request");
                }
            }
            SessionEvent::ChatRepoSet { repo_id, base_branch } => {
                if repo_id.is_some_and(|r| !at.repo_live(r)) {
                    return Decision::deny("not a live workspace repository at this position");
                }
                if base_branch.len() > 255 || base_branch.chars().any(char::is_control) {
                    return Decision::deny("bad base branch");
                }
            }
            SessionEvent::AttachmentShared { attachment } => {
                if let Err(why) = attachments::share_ok(env, attachment, fold, at, author, role) {
                    return Decision::deny(why);
                }
            }
            SessionEvent::AttachmentDeleted { attachment_id } => {
                if let Err(why) = attachments::delete_ok(env, *attachment_id, fold, at, author, role) {
                    return Decision::deny(why);
                }
            }
            SessionEvent::ChatBasePinned { repo_id, commit } => {
                if !crate::workspace_repo::is_commit_id(commit) {
                    return Decision::deny("malformed commit id");
                }
                if fold.repo.as_ref().and_then(|r| r.repo_id) != Some(*repo_id) || !at.repo_live(*repo_id) {
                    return Decision::deny("pins a base for a repository this chat doesn't use");
                }
            }
            // Review residual 3: re-pinning is explicit, a person's (never an
            // agent or a runtime effect, through their own device), an
            // Admin's or the chat's creator's, from the pin in force.
            SessionEvent::ChatBaseRepinned { repo_id, commit, previous } => {
                if env.actor_stamp.as_ref().is_some_and(|s| s.runtime_effect) {
                    return Decision::deny("a runtime never re-pins a chat's base");
                }
                let human = env.actor_stamp.as_ref().is_some_and(|s| s.actor.kind == ActorKind::Human)
                    && at.members.get(author).is_some_and(|m| m.kind == ActorKind::Human);
                if !human || !own_device(env, at) {
                    return Decision::deny("a person re-pins a chat's base through their own device");
                }
                let creator = !fold.creator_actor_id.is_empty() && fold.creator_actor_id == author;
                if !creator && role.rank() < WorkspaceRole::Admin.rank() {
                    return Decision::deny("only an Admin or the chat's creator re-pins its base");
                }
                if !crate::workspace_repo::is_commit_id(commit) || !crate::workspace_repo::is_commit_id(previous) || commit == previous {
                    return Decision::deny("malformed re-pin");
                }
                if fold.repo.as_ref().and_then(|r| r.repo_id) != Some(*repo_id) || !at.repo_live(*repo_id) {
                    return Decision::deny("re-pins a base for a repository this chat doesn't use");
                }
                let history = fold.repo_bases.get(repo_id);
                if history.and_then(|h| h.last()) != Some(previous) {
                    return Decision::deny("re-pins from a base that isn't the one in force");
                }
                if history.is_some_and(|h| h.len() >= crate::workspace_repo::MAX_BASE_PINS) {
                    return Decision::deny("this chat has re-pinned its base too many times");
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
/// Who may write memory and task state (`crate::memory`; design sections 4
/// to 6), beyond the tiers: a direct memory write or a decision is a
/// person's (a human stamp, never a runtime effect), and changing an entry
/// is its creator's or an Admin's; a host-signed task-state slot is an agent
/// bound to the signing device (the A3 transition), and only the runtime
/// writes `primary`.
fn memory_refusal(env: &SessionEventEnvelope, record: &crate::flow::FlowRecord, ctx: &Ctx<'_>, author: &str) -> Option<&'static str> {
    use crate::flow::FlowRecord;
    let stamp = env.actor_stamp.as_ref();
    let runtime = stamp.is_some_and(|s| s.runtime_effect);
    let kind = stamp.map(|s| s.actor.kind);
    if crate::memory::is_person_only(record) {
        if runtime || kind != Some(ActorKind::Human) {
            return Some("only a person writes workspace memory or decides a memory proposal; agents propose");
        }
        let m = &ctx.fold.flow.memory;
        let role = ctx.fold.role_of(author);
        let op = match record {
            FlowRecord::MemoryWritten(w) => Some(crate::memory::write_op(w)),
            FlowRecord::MemoryRemoved(r) => Some(crate::memory::MemoryOp::Remove { entry_id: r.entry_id, base: r.base }),
            FlowRecord::MemoryProposalDecided(d) if d.accepted => m.proposals.get(&d.proposal_id).map(|p| p.proposal.op.clone()),
            _ => None,
        };
        if op.is_some_and(|op| !crate::memory::may_change(m, &op, author, role)) {
            return Some("only its creator or an Admin changes a memory entry");
        }
    }
    if let FlowRecord::MemoryProposed(p) = record {
        // A person or a service member proposes as itself; an agent's
        // proposal comes from its key (the Agent tier) or a runtime effect.
        if !runtime && kind != Some(ActorKind::Agent) && !p.proposer.is_empty() {
            return Some("a memory proposal names an agent only when that agent files it");
        }
        // A runtime effect speaks for the chat's runtime or for an agent
        // bound to the signing device (the A3 fallback), never for anyone
        // else and never as the host person (an empty proposer would read
        // as the person's own proposal).
        if runtime && p.proposer != crate::flow::PRIMARY {
            let roster = if env.session_id == crate::channel::workspace_config_session_id(env.workspace_id) {
                Some(ctx.running)
            } else {
                ctx.workspace
            };
            let bound = crate::flow::agent_from_key(&p.proposer)
                .zip(env.signer_device_id)
                .is_some_and(|(a, d)| roster.is_some_and(|r| r.agent_bound_to(a, d)));
            if !bound {
                return Some("a runtime's memory proposal names the chat's runtime or an agent on the signing device");
            }
        }
    }
    if let FlowRecord::TaskState(t) = record {
        if kind == Some(ActorKind::Agent) {
            return None;
        }
        if runtime {
            return (t.agent_key != crate::flow::PRIMARY).then_some("the chat's runtime writes only its own task state");
        }
        if let Some(roster) = ctx.workspace {
            let bound = crate::flow::agent_from_key(&t.agent_key)
                .zip(env.signer_device_id)
                .is_some_and(|(a, d)| roster.agent_bound_to(a, d));
            if !bound {
                return Some("task state is written by its agent, or the host that agent runs on");
            }
        }
    }
    None
}

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
    // Agent output only: never a user turn (a prompt other agents answer),
    // and never under a human's identity other than its own host's user
    // (whose host generated it) or its owner.
    if let SessionEvent::MessageAppended { message } = &env.payload {
        if message.role != crate::chat::MessageRole::Assistant {
            return Decision::deny("an agent appends only its own replies");
        }
        if let Some(id) = &message.actor_identity {
            let ok = match id.kind {
                ActorKind::Agent => id.id == author,
                _ => {
                    id.id == owner
                        || id.account_id.is_some_and(|acct| {
                            roster.devices.get(&key.host_device_id).is_some_and(|d| d.accounts.contains(&acct))
                        })
                }
            };
            if !ok {
                return Decision::deny("an agent reply names someone else");
            }
        }
    }
    let d = evaluate(&env.payload, author, cap, ctx.fold);
    if !d.allowed {
        return Decision::deny(d.summary);
    }
    Decision::Allow
}

/// The B1 write rule (see [`authorize`]): `Some(reason)` when the envelope
/// arrived sealed under an epoch older than the one authorized at its
/// author's frontier.
fn stale_epoch(env: &SessionEventEnvelope, ctx: &Ctx<'_>, on_config: bool) -> Option<String> {
    let sealed = env.sealed_epoch()?;
    let frontier = env.actor_stamp.as_ref()?.cfg_frontier?;
    let roster = if on_config {
        ctx.running
    } else if ctx.checked {
        ctx.workspace?
    } else {
        return None;
    };
    let required = roster.key_epochs.version_at(frontier.key());
    (sealed < required).then(|| format!("sealed under key epoch {sealed}, but its author had seen epoch {required}"))
}

/// The signing device of a key event: certified here (not revoked), certified
/// for the stamped account when there is one, and its signature verifies
/// against the certificate's key (defense in depth over ingest).
pub(crate) fn certified_signer<'r>(env: &SessionEventEnvelope, roster: &'r RosterState) -> Result<&'r timeline::DeviceState, String> {
    let device = env.signer_device_id.ok_or("unsigned")?;
    let d = roster
        .devices
        .get(&device)
        .filter(|d| !d.revoked && !d.accounts.is_empty())
        .ok_or("signed by a device that isn't certified here")?;
    if let Some(account) = env.actor_stamp.as_ref().and_then(|s| s.actor.account_id) {
        if !d.accounts.contains(&account) {
            return Err("signed by a device that doesn't act for its author".into());
        }
    }
    d.verifying_key(env).ok_or("bad signature")?;
    Ok(d)
}

/// What rules 3 refuses outright (identity model v2, "Rules 3 in 0.4"): a
/// member record whose member id isn't its actor id, or whose actor id isn't
/// its account id (when it names one); and a workspace runtime carrying a
/// model key on the log (keys are never stored in the workspace, O11).
fn rules3_refusal(e: &SessionEvent) -> Option<&'static str> {
    let inline = |r: &crate::workspace_runtime::WorkspaceRuntime| matches!(r.credential, crate::workspace_runtime::WorkspaceCredential::Inline { .. });
    match e {
        SessionEvent::MemberAdded { member, .. } => {
            if member.id != member.actor.id {
                return Some("a member's id is its actor's id");
            }
            if member.actor.account_id.is_some_and(|a| a.to_string() != member.actor.id) {
                return Some("a member's actor id is its account id");
            }
            None
        }
        SessionEvent::WorkspaceRuntimeUpserted { runtime } if inline(runtime) => Some("model keys are never stored in the workspace"),
        SessionEvent::WorkspaceRuntimesUpdated { runtimes } if runtimes.iter().any(inline) => {
            Some("model keys are never stored in the workspace")
        }
        _ => None,
    }
}

/// `DeviceKeyAgreementPublished`: on the config log, signed by the very device
/// it names (certified here), with a 32-byte X25519 key.
fn ka_publication_ok(
    env: &SessionEventEnvelope,
    device_id: Uuid,
    ka_public: &[u8],
    on_config: bool,
    roster: &RosterState,
) -> Result<(), String> {
    if !on_config {
        return Err("a key-agreement key is published on the workspace config log".into());
    }
    if env.signer_device_id != Some(device_id) {
        return Err("a device publishes only its own key-agreement key".into());
    }
    if ka_public.len() != 32 {
        return Err("malformed key-agreement key".into());
    }
    certified_signer(env, roster).map(|_| ())
}

/// `WorkspaceKeyRotated`: on the config log, from a certified device, and the
/// next version after the authorized head.
fn rotation_ok(
    env: &SessionEventEnvelope,
    rotation: &crate::keys::SignedRotation,
    on_config: bool,
    roster: &RosterState,
) -> Result<(), String> {
    if !on_config {
        return Err("a key rotation is published on the workspace config log".into());
    }
    certified_signer(env, roster)?;
    roster.key_epochs.check_next(rotation)
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

/// The service-member rules (Phase A, item 6): a `Service` member holds at
/// most Contributor, can't decide a proposal set (a person's call), never
/// registers a passkey and never carries presence. Its votes count by its
/// role but never as a human's (`guard::has_human_approval` counts humans
/// only), and it applies a proposal only to fulfil a person's request.
fn service_member_refusal(
    env: &SessionEventEnvelope,
    author: &str,
    roster: Option<&RosterState>,
    on_config: bool,
    fold: &ChatSession,
) -> Option<String> {
    let cap = WorkspaceRole::Contributor;
    let service_kind = |id: &str| -> bool {
        roster.and_then(|r| r.members.get(id)).map(|m| m.kind == ActorKind::Service).unwrap_or(false)
            || (on_config && fold.members.iter().any(|m| m.id == id && m.actor.kind == ActorKind::Service))
    };
    match &env.payload {
        SessionEvent::MemberAdded { member, .. } if member.actor.kind == ActorKind::Service && member.role.rank() > cap.rank() => {
            return Some("a service member holds at most Contributor".into());
        }
        SessionEvent::MemberRoleChanged { change, .. } if service_kind(&change.member_id) && change.new_role.rank() > cap.rank() => {
            return Some("a service member holds at most Contributor".into());
        }
        _ => {}
    }
    if !service_kind(author) {
        return None;
    }
    // Every human check that reads the stamp's kind (answering an agent's
    // question, identity links, applying) would otherwise take the service's
    // own word for it: a service member's stamp says Service.
    if env.actor_stamp.as_ref().is_some_and(|s| s.actor.kind != ActorKind::Service) {
        return Some("a service member signs as a service".into());
    }
    if env.actor_stamp.as_ref().is_some_and(|s| s.presence.is_some()) {
        return Some("a service member carries no presence".into());
    }
    match &env.payload {
        SessionEvent::FlowRecorded { record } if crate::memory::is_person_only(record) => {
            Some("a person writes workspace memory, not a service".into())
        }
        SessionEvent::PasskeyRegistered { .. } => Some("a service member holds no passkey".into()),
        SessionEvent::ProposalSetDecided { .. } => Some("a person decides a proposal set, not a service".into()),
        // Phase B: a service member cuts no one and admits no one. Leaving,
        // demoting itself and revoking its own device stay open (none is a
        // cut), though the last two need presence it never has.
        SessionEvent::MemberRemoved { member_id, .. } if member_id != author => Some("a service member cuts no one".into()),
        SessionEvent::MemberRoleChanged { change, .. } if change.member_id != author => {
            Some("a service member changes no one's role".into())
        }
        SessionEvent::DeviceRevoked { device_id, .. } => {
            let own = env.actor_stamp.as_ref().and_then(|s| s.actor.account_id);
            let mine = roster.is_some_and(|r| own.is_some_and(|a| r.account_owns_device(a, *device_id)));
            (!mine).then(|| "a service member revokes only its own device".into())
        }
        SessionEvent::MemberAdded { member, .. } if member.id != author => Some("a service member admits no one".into()),
        SessionEvent::JoinApproved { .. } => Some("a service member admits no one".into()),
        _ => None,
    }
}

/// A membership change a person may try from a desktop: what the UI asks
/// [`member_action_refusal`] about before offering it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberAction {
    /// Remove `member_id` (a cut, unless it is yourself).
    Remove { member_id: String },
    /// Give `member_id` the role `role` (a cut when it is lower).
    SetRole { member_id: String, role: WorkspaceRole },
}

/// Why the fold would refuse `author` doing `action` against the workspace
/// roster `roster` and its config fold `config` (both at the head), or
/// `None` when it would allow it (with the author's passkey presence, which
/// the desktop asks for). The UI hides or disables what this refuses and
/// shows the reason; it runs the very rules [`authorize`] runs for these
/// kinds (the role floor, the rank rules, the service-member rules), so the
/// two never disagree (`tests/member_action_parity.rs`).
pub fn member_action_refusal(roster: &RosterState, config: &ChatSession, author: &str, action: &MemberAction) -> Option<String> {
    let Some(me) = config.members.iter().find(|m| m.id == author) else {
        return Some("you aren't a member of this workspace".into());
    };
    let payload = match action {
        MemberAction::Remove { member_id } => SessionEvent::MemberRemoved { member_id: member_id.clone(), watermark: vec![] },
        MemberAction::SetRole { member_id, role } => {
            let Some(target) = config.members.iter().find(|m| m.id == *member_id) else {
                return Some("not a member".into());
            };
            if target.role == *role {
                return Some("already that role".into());
            }
            SessionEvent::MemberRoleChanged {
                change: crate::events::MemberRoleChange { member_id: member_id.clone(), old_role: target.role, new_role: *role },
                watermark: vec![],
            }
        }
    };
    let mut env = SessionEventEnvelope::new(crate::channel::workspace_config_session_id(config.workspace_id), config.workspace_id, 0, payload);
    env.actor_stamp = Some(crate::identity::ActorStamp {
        actor: me.actor.clone(),
        recorded_at: crate::Timestamp::epoch(),
        cfg_frontier: None,
        presence: None,
        runtime_effect: false,
        prev: None,
        sig_alg: Default::default(), log_head: None,
    });
    let role = config.role_of(author);
    if let Some(why) = service_member_refusal(&env, author, Some(roster), true, config) {
        return Some(why);
    }
    if let Some(why) = rank_rules(&env.payload, author, role, roster) {
        return Some(why);
    }
    let d = evaluate(&env.payload, author, role, config);
    (!d.allowed).then_some(d.summary)
}

/// The rank rules on membership and device governance (review P-A2b, P-A2d):
///
/// * no one grants a role above their own, or changes the role of a member
///   above them;
/// * a cut (a removal, a demotion, a revocation of another member's device)
///   goes strictly **down** the ranks: an Admin cuts only roles below Admin,
///   an Owner cuts an Admin, and with several Owners only the founder (the
///   config seed's creator) cuts an Owner; nobody cuts the founder. Leaving
///   yourself, demoting yourself and revoking your own device stay open.
///
/// Ranks are judged at each cut's own position, so the subject of a cut
/// never cuts its author on the roster the cut was judged on: a removed
/// member can't undo their removal with a backdated counter-cut. A subject
/// whose rank an allowed role change later raises (the founder promoting a
/// demoted member to Owner) may cut the earlier author after it; that is a
/// sequence, not a mutual cut.
fn rank_rules(e: &SessionEvent, author: &str, role: WorkspaceRole, roster: &RosterState) -> Option<String> {
    let role_of = |id: &str| roster.members.get(id).map(|m| m.role);
    let targets: Vec<String> = match e {
        // (Self-enrollment is judged by its invite: `self_enroll_allowance`.)
        SessionEvent::MemberAdded { member, .. } if member.id != author && member.role.rank() > role.rank() => {
            return Some("no one grants a role above their own".into());
        }
        SessionEvent::MemberRoleChanged { change, .. } => {
            if change.new_role.rank() > role.rank() {
                return Some("no one grants a role above their own".into());
            }
            let current = role_of(&change.member_id);
            if change.member_id != author && current.is_some_and(|r| r.rank() > role.rank()) {
                return Some("no one changes the role of a member above them".into());
            }
            if change.member_id != author && current.is_some_and(|r| change.new_role.rank() < r.rank()) {
                vec![change.member_id.clone()]
            } else {
                Vec::new()
            }
        }
        SessionEvent::MemberRemoved { member_id, .. } if member_id != author => vec![member_id.clone()],
        SessionEvent::DeviceRevoked { device_id, .. } => {
            // Whose device it is: the members who hold it. A device someone
            // certified here must be held by someone other than the author
            // (the author's own is R5's, above): a device id is derived from
            // its key, so anyone who knows a key can certify it for
            // themselves, which makes it no more theirs to revoke. A device
            // nobody certified here (a phone known only by its pairing) is
            // an Admin's to revoke, as before.
            let own = roster.members.get(author).and_then(|m| m.account_id);
            let empty = std::collections::BTreeSet::new();
            let accounts = match roster.devices.get(device_id).filter(|d| !d.accounts.is_empty()) {
                None => &empty,
                Some(d) if d.holders.iter().any(|a| Some(*a) != own) => &d.holders,
                Some(_) => return Some("a revocation names a device someone else holds here".into()),
            };
            roster
                .members
                .iter()
                .filter(|(id, m)| id.as_str() != author && m.account_id.is_some_and(|a| accounts.contains(&a)))
                .map(|(id, _)| id.clone())
                .collect()
        }
        _ => Vec::new(),
    };
    let founder = roster.founder.as_str();
    for t in targets {
        if !founder.is_empty() && t == founder {
            return Some("nobody cuts the workspace's founder".into());
        }
        let Some(r) = role_of(&t) else { continue };
        if r == WorkspaceRole::Owner {
            if author != founder || role != WorkspaceRole::Owner {
                return Some("only the founder cuts an Owner".into());
            }
        } else if r.rank() >= role.rank() {
            return Some("a cut goes strictly down the ranks".into());
        }
    }
    None
}

/// Whether `env` is signed by a device certified here (not revoked) for its
/// stamped account, which is its author's member account. Unlike
/// [`own_device`], a host acting for the account (a delegated device) counts;
/// its votes need the owner's presence on top.
fn voter_signed(env: &SessionEventEnvelope, at: &RosterState) -> bool {
    let Some(account) = env.actor_stamp.as_ref().and_then(|s| s.actor.account_id) else { return false };
    if at.members.get(author_of(env)).and_then(|m| m.account_id).is_some_and(|a| a != account) {
        return false;
    }
    env.signer_device_id
        .and_then(|d| at.devices.get(&d))
        .is_some_and(|d| !d.revoked && d.accounts.contains(&account))
}

/// Whether `env` is signed by a device of its stamped author's own account:
/// certified for that account here, not revoked, and not a host acting for
/// it (a delegated device).
fn own_device(env: &SessionEventEnvelope, at: &RosterState) -> bool {
    let Some(account) = env.actor_stamp.as_ref().and_then(|s| s.actor.account_id) else { return false };
    let author = author_of(env);
    if at.members.get(author).and_then(|m| m.account_id).is_some_and(|a| a != account) {
        return false;
    }
    env.signer_device_id
        .and_then(|d| at.devices.get(&d))
        .is_some_and(|d| !d.revoked && d.accounts.contains(&account) && !d.delegated_for(account))
}

/// The config-log checks of the workspace repo events (repos-and-checkouts
/// 3.1): on the config log; an add is well formed (normal-form canonical,
/// derived id), not already live, within the bound; an update or a removal
/// names a live repo.
fn repo_event_ok(e: &SessionEvent, on_config: bool, roster: &RosterState) -> Result<(), String> {
    use crate::workspace_repo as wr;
    let live = |id: &Uuid| roster.repo_live(*id);
    match e {
        SessionEvent::WorkspaceRepoAdded { .. }
        | SessionEvent::WorkspaceRepoUpdated { .. }
        | SessionEvent::WorkspaceRepoRemoved { .. }
        | SessionEvent::WorkspaceRepoAliasesSet { .. }
        | SessionEvent::WorkspaceHostAliasSet { .. }
            if !on_config =>
        {
            Err("workspace repositories live on the workspace config log".into())
        }
        SessionEvent::WorkspaceRepoAdded { repo } => {
            wr::repo_well_formed(repo)?;
            if live(&repo.id) {
                return Err("that repository is already in the workspace".into());
            }
            if roster.repos.values().filter(|r| !r.removed).count() >= wr::MAX_LIVE_REPOS {
                return Err("too many repositories".into());
            }
            // Design 12: an alias already owns this name.
            if let Some(why) = wr::shadowed_by_alias(roster, &repo.canonical) {
                return Err(why);
            }
            Ok(())
        }
        SessionEvent::WorkspaceRepoAliasesSet { repo_id, aliases } => wr::repo_aliases_ok(roster, *repo_id, aliases),
        SessionEvent::WorkspaceHostAliasSet { alias, host } => wr::host_alias_ok(roster, alias, host),
        SessionEvent::WorkspaceRepoUpdated { repo_id, display_name, default_branch, .. } => {
            if !live(repo_id) {
                return Err("no such repository".into());
            }
            if !wr::display_name_ok(display_name) || default_branch.len() > 255 || default_branch.chars().any(char::is_control) {
                return Err("bad repository name or branch".into());
            }
            Ok(())
        }
        SessionEvent::WorkspaceRepoRemoved { repo_id } => {
            if live(repo_id) {
                Ok(())
            } else {
                Err("no such repository".into())
            }
        }
        _ => Ok(()),
    }
}

/// Account keys and device certificates: self-validating (the roster checks
/// the chain), published by members and joiners alike.
pub(crate) fn is_trust_publication(e: &SessionEvent) -> bool {
    matches!(
        e,
        SessionEvent::AccountKeyRegistered { .. }
            | SessionEvent::AccountKeyRotated { .. }
            | SessionEvent::DeviceCertificateAdded { .. }
    )
}

/// Why a comment or a proposal edit would misstate the comment thread, if it
/// does. A person's comment (`ProposalRefined`) names its signer as its
/// author, as a vote names its voter (an agent's is held to the same by the
/// Agent tier). An edit (`ProposalUpserted`) never brings comments under
/// another name onto a new proposal, and never changes a comment the thread
/// already holds. The fold keeps the thread it has whatever an edit carries
/// (`ChatSession::apply`), so the comments an edit leaves out (an older
/// client's stale copy) stay, and what else it carries is ignored: only a
/// forgery is refused.
fn thread_refusal(env: &SessionEventEnvelope, author: &str, fold: &ChatSession) -> Option<&'static str> {
    let agent = env.actor_stamp.as_ref().is_some_and(|s| s.actor.kind == ActorKind::Agent);
    match &env.payload {
        SessionEvent::ProposalRefined { refinement, .. } if !agent && refinement.actor_id != author => {
            Some("a comment is by the member who signs it")
        }
        SessionEvent::ProposalUpserted { proposal } => match fold.proposals.iter().find(|p| p.id == proposal.id) {
            None => proposal
                .refinements
                .iter()
                .any(|r| r.actor_id != author)
                .then_some("a new proposal carries no one else's comments"),
            Some(old) => proposal
                .refinements
                .iter()
                .any(|r| {
                    old.refinements.iter().any(|o| {
                        o.id == r.id && (o.actor_id != r.actor_id || o.text != r.text || o.request_changes != r.request_changes)
                    })
                })
                .then_some("an edit never changes a comment on the proposal"),
        },
        _ => None,
    }
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
        let in_chat = fold.workspace_agents.iter().find(|a| a.id == new.id).map(|a| a.as_judged());
        let in_chat = in_chat.as_deref();
        // Judged on the record the upsert leaves, never the payload alone
        // (workspace model v3, 5.6 rule 5): an older writer's upsert that
        // drops the sticky fields, or a newer one whose sticky fields
        // disagree with its legacy ones, is judged by where the agent ends
        // up.
        let merged = crate::agent_run::merge_upsert(in_chat, new, crate::agent_run::RunRules::Mirror);
        let new = &merged;
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
            Some(old) if new.moves_from(old) || old.owner_actor_id != new.owner_actor_id => {
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

/// Who may change an agent's owner-only settings (`access`, `limits`,
/// `notify_on_use`: [`crate::agent::AgentPolicy`]): its owner, and only its
/// owner, whatever anyone else's role; on an unowned agent, an Admin. A new
/// agent may carry settings only when its author is its owner (or, unowned,
/// an Admin). An edit that leaves them as they were (a rename, an avatar, an
/// Admin's re-host) is not judged here. On a chat log, an agent the chat
/// doesn't define yet is judged against the workspace's agent of that id, as
/// in [`agent_change_floor`]: a chat-level copy can't open up the
/// workspace's agent.
pub fn agent_policy_refusal(
    event: &SessionEvent,
    author: &str,
    role: WorkspaceRole,
    fold: &ChatSession,
    workspace: Option<&RosterState>,
) -> Option<String> {
    let admin = role.rank() >= WorkspaceRole::Admin.rank();
    let one = |new: &WorkspaceAgent| -> Option<String> {
        let policy = new.policy();
        let before = match fold.workspace_agents.iter().find(|a| a.id == new.id) {
            Some(old) => Some((old.owner_actor_id.clone(), old.policy())),
            None => workspace.and_then(|w| w.agents.get(&new.id)).map(|ws| (ws.owner.clone(), ws.policy.clone())),
        };
        let (owner, changed) = match before {
            Some((owner, old)) => (owner, old != policy),
            None => (new.owner_actor_id.clone(), !policy.is_default()),
        };
        if !changed {
            return None;
        }
        let ok = if owner.is_empty() { admin } else { !author.is_empty() && author == owner };
        (!ok).then(|| {
            if owner.is_empty() {
                format!("only an Admin changes who may use the shared agent {}", new.name)
            } else {
                format!("only {}'s owner changes who may use it and its budgets", new.name)
            }
        })
    };
    match event {
        SessionEvent::AgentUpserted { agent } => one(agent),
        SessionEvent::AgentRosterUpdated { agents } => agents.iter().find_map(one),
        _ => None,
    }
}

/// Turn claims, coordinator leases and run writes are bound to devices:
///   * a `TurnClaimed` must be signed by the device it names;
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
        // A turn claim speaks for the device it names: another device can't
        // claim a turn for it (or claim one to keep it from answering).
        // (An agent-signed claim names its key's host device; the Agent tier
        // checks that binding.)
        SessionEvent::TurnClaimed { device_id, .. } => {
            signer == Some(*device_id) || env.actor_stamp.as_ref().is_some_and(|s| s.actor.kind == ActorKind::Agent)
        }
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

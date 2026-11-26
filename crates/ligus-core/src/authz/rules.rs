//! Which events need a human's passkey presence under workspace rules v2
//! (Phase 0, B3, table B.4.4). One exhaustive `match`: a new `SessionEvent`
//! kind doesn't compile until it is placed here.

use crate::events::SessionEvent;
use crate::identity::ActorKind;
use crate::session::ChatSession;

use super::RosterState;

/// Whether `event`, authored by `author`, needs presence under rules v2,
/// judged against the roster at its position and its log's fold so far.
pub fn requires_presence(event: &SessionEvent, author: &str, roster: &RosterState, fold: &ChatSession) -> bool {
    match event {
        // Membership and roles. Adding yourself (the founder's own seed
        // member, or redeeming an Admin's invite) is the bootstrap exception.
        SessionEvent::MemberAdded { member, .. } => member.id != author,
        SessionEvent::MemberRemoved { .. }
        | SessionEvent::MemberRoleChanged { .. }
        | SessionEvent::JoinApproved { .. } => true,
        // Closing a box admission only narrows who may join, and an Admin's
        // device does it unattended when it sees the admission redeemed or
        // lapse (Phase B).
        SessionEvent::AdmissionClosed { .. } => false,
        // Devices: self-revoke included.
        SessionEvent::DeviceRevoked { .. } => true,
        // A new device for an account that holds a passkey or a recovery key
        // (a device-key thief would otherwise add devices of their own); the
        // presence must be that account's (`authz::authorize`). Re-publishing a
        // certificate already in force here needs nothing.
        // A phone (P-256 key) is enrolled only with its person's passkey,
        // always.
        SessionEvent::DeviceCertificateAdded { certificate: c } => {
            (c.alg == crate::crypto::SigAlg::P256 && !roster.account_certified(c.account_id, c.device_id))
                || (roster.passkeys.get(&c.account_id).is_some_and(|m| !m.is_empty())
                    && !roster.account_certified(c.account_id, c.device_id))
        }
        // Keys. A passkey registration has its own rule (`authz::passkeys`):
        // the account's first proves itself, every later one needs presence
        // from an existing credential, under every rules version.
        SessionEvent::WorkspaceKeyRotated { .. }
        | SessionEvent::PasskeyRemoved { .. }
        | SessionEvent::AccountKeyRotated { .. } => true,
        SessionEvent::PasskeyRegistered { .. } => false,
        // A link grants nothing, so it needs no presence (§2.2).
        SessionEvent::IdentityLinkPublished { .. } | SessionEvent::IdentityLinkRemoved { .. } => false,
        // Names and pictures are render-only (identity model v2, 5.2, 6.2).
        SessionEvent::ProfileUpdated { .. } | SessionEvent::DeviceLabelSet { .. } => false,
        // The old account's own key signs it; it holds no passkey to give.
        SessionEvent::MemberRetired { .. } => false,
        // Guards.
        SessionEvent::DearthModeChanged { .. } | SessionEvent::ChatLockdownCleared { .. } => true,
        // Zones and the beacon (the governance flow records).
        SessionEvent::FlowRecorded { record } => record.is_governance(),
        // Workflows.
        SessionEvent::WorkflowDefinitionsUpdated { .. } => true,
        // Agents and runtimes: rebinding an agent's host or runtime,
        // removing one, every workspace runtime change, and touching a host
        // someone else owns.
        // Opening an agent to everyone (its owner's credentials, spent on
        // other people's requests) too.
        SessionEvent::AgentUpserted { agent } => agent_rebinds(agent, fold) || agent_opens(agent, fold),
        SessionEvent::AgentRosterUpdated { agents } => {
            agents.iter().any(|a| agent_rebinds(a, fold) || agent_opens(a, fold))
                || fold.workspace_agents.iter().any(|old| !agents.iter().any(|a| a.id == old.id))
        }
        SessionEvent::AgentRemoved { .. } => true,
        SessionEvent::WorkspaceRuntimesUpdated { .. }
        | SessionEvent::WorkspaceRuntimeUpserted { .. }
        | SessionEvent::WorkspaceRuntimeRemoved { .. } => true,
        SessionEvent::WorkspaceHostUpserted { host } => host.owner_actor_id != author || foreign_host(fold, &host.id, author),
        SessionEvent::WorkspaceHostRemoved { host_id } => {
            fold.workspace_hosts.iter().any(|h| &h.id == host_id && h.owner_actor_id != author)
        }
        SessionEvent::WorkspaceHostsUpdated { hosts } => {
            hosts.iter().any(|h| h.owner_actor_id != author || foreign_host(fold, &h.id, author))
                || fold.workspace_hosts.iter().any(|h| h.owner_actor_id != author && !hosts.iter().any(|n| n.id == h.id))
        }
        // Gates and quorum.
        SessionEvent::ProposalSetTuned { .. } | SessionEvent::WorkspaceRulesSet { .. } => true,
        // Sandbox grants widen what agents reach on every host that runs them.
        SessionEvent::WorkspaceSandboxSet { .. } => true,
        SessionEvent::ProposalUpserted { proposal } => fold.proposals.iter().find(|p| p.id == proposal.id).is_some_and(|old| {
            proposal.required_approvals < old.required_approvals
                || proposal.approval_role_floor.rank() < old.approval_role_floor.rank()
        }),
        // Human-gated decisions.
        SessionEvent::ProposalVoteCast { proposal_id, .. } => fold
            .proposals
            .iter()
            .find(|p| p.id == *proposal_id)
            .is_some_and(|p| p.workflow.is_some() || human_gated(fold, p)),
        SessionEvent::ProposalSetDecided { .. } => {
            roster.members.get(author).is_some_and(|m| m.kind == ActorKind::Human)
        }
        // A host fulfilling a person's apply request carries no presence of
        // its own: the request carried the person's.
        SessionEvent::ProposalApplied { request_id: Some(_), .. } => false,
        SessionEvent::ProposalApplied { proposal_id, .. } => {
            fold.proposals.iter().find(|p| p.id == *proposal_id).is_some_and(|p| human_gated(fold, p))
        }
        SessionEvent::ProposalApplyRequested { request } => {
            fold.proposals.iter().find(|p| p.id == request.proposal_id).is_some_and(|p| human_gated(fold, p))
        }
        // A host's failure report and a withdrawal only close a request.
        SessionEvent::ProposalApplyFailed { .. } | SessionEvent::ProposalApplyWithdrawn { .. } => false,
        // Repos: adding or removing one steers clones and chats (repos
        // review F7); renaming is cosmetic; using one in a chat is content.
        SessionEvent::WorkspaceRepoAdded { .. } | SessionEvent::WorkspaceRepoRemoved { .. } => true,
        // Aliases change identity matching (repos review F6, R2): as add.
        SessionEvent::WorkspaceRepoAliasesSet { .. } | SessionEvent::WorkspaceHostAliasSet { .. } => true,
        SessionEvent::WorkspaceRepoUpdated { .. } | SessionEvent::ChatRepoSet { .. } | SessionEvent::ChatBasePinned { .. } => false,
        // Moving a chat's pinned base rewrites every host's chat branch.
        SessionEvent::ChatBaseRepinned { .. } => true,
        // Attachments (issue #118): the policy decides who shares what, so
        // changing it is a person's, with presence; lifting a block widens
        // it too. Blocking a hash only narrows; sharing and deleting are
        // content, judged by the policy.
        SessionEvent::AttachmentPolicySet { .. } | SessionEvent::AttachmentHashUnblocked { .. } => true,
        SessionEvent::AttachmentHashBlocked { .. } | SessionEvent::AttachmentShared { .. } | SessionEvent::AttachmentDeleted { .. } => false,
        // Everything else is content, coordination or a self-validating
        // publication.
        SessionEvent::SessionSnapshot { .. }
        | SessionEvent::MessageAppended { .. }
        | SessionEvent::MessageChunkReceived { .. }
        | SessionEvent::MessageCompleted { .. }
        | SessionEvent::MessageRemoved { .. }
        | SessionEvent::TurnClaimed { .. }
        | SessionEvent::SessionRuntimeChanged { .. }
        | SessionEvent::SessionTitleChanged { .. }
        | SessionEvent::SessionArchivedChanged { .. }
        | SessionEvent::SkillsUpdated { .. }
        | SessionEvent::ProposalDismissed { .. }
        | SessionEvent::ProposalRefined { .. }
        | SessionEvent::ProposalSetOpened { .. }
        | SessionEvent::ProposalSetAlternativeAdded { .. }
        | SessionEvent::ScoutAssessmentPosted { .. }
        | SessionEvent::ProposalSetRoundAdvanced { .. }
        | SessionEvent::ProposalSetQuorumReached { .. }
        | SessionEvent::ProposalSetAlternativeStopped { .. }
        | SessionEvent::ProposalSetPreflightRecorded { .. }
        | SessionEvent::ProposalReverted { .. }
        | SessionEvent::VaultSourcesUpdated { .. }
        | SessionEvent::WorkflowRunUpserted { .. }
        | SessionEvent::MessageReactionAdded { .. }
        | SessionEvent::MessageReactionRemoved { .. }
        | SessionEvent::AccountKeyRegistered { .. }
        | SessionEvent::ChannelCreated { .. }
        | SessionEvent::ChannelRenamed { .. }
        | SessionEvent::ChannelReordered { .. }
        | SessionEvent::ChannelArchived { .. }
        | SessionEvent::ChatChannelChanged { .. }
        | SessionEvent::CoordinatorLeaseClaimed { .. }
        | SessionEvent::CoordinatorLeaseReleased { .. }
        | SessionEvent::ChatLockdownRaised { .. }
        | SessionEvent::ProposalMarkedStale { .. }
        | SessionEvent::AgentKeyCertified { .. }
        | SessionEvent::DeviceKeyAgreementPublished { .. }
        | SessionEvent::Unknown => false,
    }
}

/// Whether host `id` exists in the fold and someone other than `author` owns
/// it (re-owning it under the same id touches it).
fn foreign_host(fold: &ChatSession, id: &str, author: &str) -> bool {
    fold.workspace_hosts.iter().any(|h| h.id == id && h.owner_actor_id != author)
}

fn agent_rebinds(agent: &crate::agent::WorkspaceAgent, fold: &ChatSession) -> bool {
    match fold.workspace_agents.iter().find(|a| a.id == agent.id) {
        None => !agent.host_id.is_empty() || !agent.runtime_id.is_empty(),
        Some(old) => old.host_id != agent.host_id || old.runtime_id != agent.runtime_id,
    }
}

/// Whether `agent` is open to everyone where it wasn't (a new agent: where
/// its kind's default isn't). Narrowing it, and its budgets, need nothing.
fn agent_opens(agent: &crate::agent::WorkspaceAgent, fold: &ChatSession) -> bool {
    use crate::agent::Access;
    if agent.effective_access() != Access::Everyone {
        return false;
    }
    match fold.workspace_agents.iter().find(|a| a.id == agent.id) {
        Some(old) => old.effective_access() != Access::Everyone,
        None => !agent.owner_actor_id.is_empty(),
    }
}

/// Whether a proposal sits behind a human gate (a lockdown, or dearth mode
/// over brood paths), whether or not a human already approved it.
pub fn human_gated(fold: &ChatSession, p: &crate::ActionProposal) -> bool {
    if fold.guards.lockdown_active() {
        return true;
    }
    fold.guards.dearth_on().is_some_and(|d| {
        crate::guard::touched_paths(p).iter().any(|t| crate::guard::touches_any(&d.brood_paths, std::slice::from_ref(t)))
    })
}

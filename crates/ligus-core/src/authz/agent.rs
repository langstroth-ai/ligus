//! The Agent tier (A3): what an agent may sign with its own key. A
//! capability allow-list, not a `WorkspaceRole` (so the role enum and its
//! Swift mirror stay as they are), and capped by the agent's owner's role at
//! the event's position: an agent of a Viewer can do only what a Viewer can.

use uuid::Uuid;

use crate::events::SessionEvent;
use crate::flow::FlowRecord;
use crate::proposals::ProposalStatus;
use crate::session::ChatSession;

/// Whether the Agent tier allows `agent_id` (holding a key certified by
/// `host_device`) to sign `event` on a log whose fold so far is `fold`.
/// Everything not listed is denied: every admin or governance kind, votes,
/// `ProposalApplied`, set decisions, tuning, quorum and pre-flight, clearing a
/// lockdown, workflow definitions.
pub fn tier_allows(
    event: &SessionEvent,
    agent_id: Uuid,
    host_device: Uuid,
    fold: &ChatSession,
) -> Result<(), String> {
    let me = agent_id.to_string();
    let own_message = |id: Uuid| {
        fold.messages
            .iter()
            .find(|m| m.id == id)
            .is_some_and(|m| m.responder_id == Some(agent_id))
    };
    let ok = match event {
        SessionEvent::MessageAppended { message } => message.responder_id == Some(agent_id),
        SessionEvent::MessageChunkReceived { message_id, .. }
        | SessionEvent::MessageCompleted { message_id, .. } => own_message(*message_id),
        SessionEvent::MessageReactionAdded { reaction, .. } => reaction.actor_id == me,
        SessionEvent::MessageReactionRemoved { actor_id, .. } => *actor_id == me,
        SessionEvent::ProposalUpserted { proposal } => {
            // As itself, or authorless naming itself as producer; never
            // under someone else's name or for another agent.
            (proposal.author_actor_id == me
                || (proposal.author_actor_id.is_empty()
                    && proposal.producing_agent_id == Some(agent_id)))
                && proposal.producing_agent_id.is_none_or(|p| p == agent_id)
                && proposal.status == ProposalStatus::Open
                && proposal.approvals.is_empty()
                && fold
                    .proposals
                    .iter()
                    .find(|p| p.id == proposal.id)
                    .is_none_or(|p| {
                        p.author_actor_id == me || p.producing_agent_id == Some(agent_id)
                    })
        }
        SessionEvent::ProposalRefined { refinement, .. } => refinement.actor_id == me,
        SessionEvent::ProposalSetOpened { .. }
        | SessionEvent::ProposalSetAlternativeAdded { .. }
        | SessionEvent::ScoutAssessmentPosted { .. } => true,
        SessionEvent::FlowRecorded { record } => match record {
            FlowRecord::Mark(_)
            | FlowRecord::MarkCleared { .. }
            | FlowRecord::Handoff(_)
            | FlowRecord::Recruit(_)
            | FlowRecord::RouteDecision(_) => true,
            FlowRecord::AgentProfile(p) => p.agent_id == agent_id,
            // Track D: an agent's own ask and its own turn's tool summary.
            FlowRecord::HumanAsk(a) => a.agent_key == me,
            FlowRecord::TurnToolSummary(t) => t.agent_key == me,
            // Memory: proposals only, as itself; its own task-state slot.
            FlowRecord::MemoryProposed(p) => p.proposer == me,
            FlowRecord::TaskState(t) => t.agent_key == me,
            _ => false,
        },
        SessionEvent::ChatLockdownRaised { .. } => true,
        SessionEvent::TurnClaimed { device_id, .. } => *device_id == host_device,
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(format!("an agent can't sign {}", event.kind_str()))
    }
}

/// The agent a host-signed event speaks for, when its payload is
/// agent-authored: a message an agent's turn produced, or a proposal with a
/// producing agent. Such events are judged with the Agent tier even before
/// the host certified a key for that agent (A3 transition).
pub fn agent_authored(event: &SessionEvent) -> Option<Uuid> {
    match event {
        SessionEvent::MessageAppended { message }
            if message.role != crate::chat::MessageRole::User =>
        {
            message.responder_id
        }
        SessionEvent::ProposalUpserted { proposal } => proposal.producing_agent_id,
        _ => None,
    }
}

/// The Agent tier for the chat's primary runtime (no agent identity of its
/// own): a host-signed effect its tool loop wrote (`ActorStamp::runtime_effect`).
/// Same capability list as an agent's, with the ownership checks on what it
/// files: open, unapproved proposals, its messages and reactions, marks and
/// hand-offs; never a vote, a governance or admin event, an apply.
pub fn runtime_tier_allows(event: &SessionEvent) -> Result<(), String> {
    let ok = match event {
        SessionEvent::MessageAppended { .. }
        | SessionEvent::MessageChunkReceived { .. }
        | SessionEvent::MessageCompleted { .. }
        | SessionEvent::MessageReactionAdded { .. }
        | SessionEvent::MessageReactionRemoved { .. }
        | SessionEvent::ProposalRefined { .. }
        | SessionEvent::ProposalSetOpened { .. }
        | SessionEvent::ProposalSetAlternativeAdded { .. }
        | SessionEvent::ScoutAssessmentPosted { .. }
        | SessionEvent::ChatLockdownRaised { .. }
        | SessionEvent::TurnClaimed { .. } => true,
        SessionEvent::ProposalUpserted { proposal } => {
            proposal.status == ProposalStatus::Open && proposal.approvals.is_empty()
        }
        SessionEvent::FlowRecorded { record } => match record {
            FlowRecord::Mark(_)
            | FlowRecord::MarkCleared { .. }
            | FlowRecord::Handoff(_)
            | FlowRecord::Recruit(_)
            | FlowRecord::RouteDecision(_)
            | FlowRecord::HumanAsk(_)
            | FlowRecord::TurnToolSummary(_)
            // A memory proposal (the host's fallback for an agent without
            // its own key names that agent), never a write.
            | FlowRecord::MemoryProposed(_) => true,
            FlowRecord::TaskState(t) => t.agent_key == crate::flow::PRIMARY,
            _ => false,
        },
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "the chat's runtime can't sign {}",
            event.kind_str()
        ))
    }
}

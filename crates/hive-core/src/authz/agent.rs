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
pub fn tier_allows(event: &SessionEvent, agent_id: Uuid, host_device: Uuid, fold: &ChatSession) -> Result<(), String> {
    let me = agent_id.to_string();
    let own_message = |id: Uuid| {
        fold.messages.iter().find(|m| m.id == id).is_some_and(|m| m.responder_id == Some(agent_id))
    };
    let ok = match event {
        SessionEvent::MessageAppended { message } => message.responder_id == Some(agent_id),
        SessionEvent::MessageChunkReceived { message_id, .. } | SessionEvent::MessageCompleted { message_id, .. } => {
            own_message(*message_id)
        }
        SessionEvent::MessageReactionAdded { reaction, .. } => reaction.actor_id == me,
        SessionEvent::MessageReactionRemoved { actor_id, .. } => *actor_id == me,
        SessionEvent::ProposalUpserted { proposal } => {
            (proposal.author_actor_id == me || proposal.producing_agent_id == Some(agent_id))
                && proposal.status == ProposalStatus::Open
                && proposal.approvals.is_empty()
                && fold.proposals.iter().find(|p| p.id == proposal.id).is_none_or(|p| {
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
/// producing agent. Under rules v2 such events are judged with the Agent tier
/// even before the host certified a key for that agent (A3 transition).
pub fn agent_authored(event: &SessionEvent) -> Option<Uuid> {
    match event {
        SessionEvent::MessageAppended { message } if message.role != crate::chat::MessageRole::User => message.responder_id,
        SessionEvent::ProposalUpserted { proposal } => proposal.producing_agent_id,
        _ => None,
    }
}

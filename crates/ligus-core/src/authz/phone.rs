//! The phone tier (Phase A, item 6; Phase B): what a phone's own P-256
//! (Secure Enclave) key may sign. A phone posts, reacts, votes and approves,
//! answers an agent's question, asks a host to apply (and withdraws that
//! request), and raises or clears a chat lockdown; it never changes
//! membership, roles, devices or keys (its own revocation, its own name from
//! rules 4, and admitting a box as a service member excepted), never rebinds an agent, never issues a
//! person's invite, and never touches workspace configuration. Capped by its
//! person's role and judged by the presence table like every event.

use uuid::Uuid;

use crate::events::SessionEvent;
use crate::flow::FlowRecord;

/// Whether a phone (`device`) may sign `event`.
pub fn phone_tier_allows(event: &SessionEvent, device: Uuid) -> Result<(), String> {
    let ok = match event {
        SessionEvent::MessageAppended { message } => message.role == crate::chat::MessageRole::User,
        SessionEvent::MessageRemoved { .. }
        | SessionEvent::MessageReactionAdded { .. }
        | SessionEvent::MessageReactionRemoved { .. }
        | SessionEvent::ProposalVoteCast { .. }
        | SessionEvent::ProposalRefined { .. }
        | SessionEvent::ProposalDismissed { .. }
        | SessionEvent::ProposalApplyRequested { .. }
        | SessionEvent::ChatLockdownRaised { .. } => true,
        // Phase B: withdrawing its person's own apply request (it narrows),
        // and clearing a lockdown (a person's call, with their passkey
        // presence like on any device).
        SessionEvent::ProposalApplyWithdrawn { .. } | SessionEvent::ChatLockdownCleared { .. } => true,
        // Answering an agent, and proposing a memory change as its person
        // (a proposal grants nothing; a person decides it on a desktop).
        SessionEvent::FlowRecorded { record } => match record {
            FlowRecord::HumanAnswered(_) => true,
            FlowRecord::MemoryProposed(p) => p.proposer.is_empty(),
            _ => false,
        },
        // Its own revocation (a lost phone revoking itself is harmless).
        SessionEvent::DeviceRevoked { device_id, .. } => *device_id == device,
        // Its own name (workspace model v3, 17.6): only from rules 4, which
        // `authz::profiles::label_ok` enforces; render-only either way.
        SessionEvent::DeviceLabelSet { label } => label.device_id == device,
        // Phase B: a box admission (a service member, at most Contributor),
        // with its person's passkey presence and Admin role like every
        // invite. No other membership change.
        SessionEvent::JoinApproved { admission: Some(_), .. } => true,
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(format!("a phone key can't sign {}", event.kind_str()))
    }
}

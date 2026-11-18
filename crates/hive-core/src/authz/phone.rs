//! The phone tier (Phase A, item 6): what a phone's own P-256 (Secure
//! Enclave) key may sign. A phone posts, reacts, votes and approves, and
//! asks a host to apply; it never changes membership, roles, devices or
//! keys (its own revocation and admitting a box as a service member
//! excepted), and never touches workspace configuration. Capped by its person's role like every event.

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
        SessionEvent::FlowRecorded { record } => matches!(record, FlowRecord::HumanAnswered(_)),
        // Its own revocation (a lost phone revoking itself is harmless).
        SessionEvent::DeviceRevoked { device_id, .. } => *device_id == device,
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

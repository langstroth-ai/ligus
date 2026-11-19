//! The fold rules of file attachments (issue #118).
//!
//! * `AttachmentPolicySet`, `AttachmentHashBlocked` and
//!   `AttachmentHashUnblocked` live on the config log, are an Admin's
//!   (the role floor), and the policy change and the unblock need presence
//!   (`rules::requires_presence`).
//! * `AttachmentShared` lives on a chat log and is judged against the
//!   workspace roster at its author's position: the policy in force there
//!   (on, the author's kind and role, size, types), the blocklist there, a
//!   fresh attachment id bound to the signing device
//!   (`attachment::share_id`, or `attachment::offer_id` for an agent's
//!   offer), the author's own certified device. A share that
//!   carries out an agent's proposal must match the approved proposal's
//!   file exactly, be signed by the host that produced it, and the policy
//!   must let agents offer files.
//! * `AttachmentDeleted` is the sharer's or an Admin's.
//! * A proposal that carries a file (an agent's offer) needs the policy to
//!   let agents offer files, and the file to pass the policy.
//!
//! Removed members, demoted members and revoked devices are handled by the
//! cut rules like every chat event: past its watermark a removed member's
//! share is judged as of the removal (not a member), a revoked device's
//! never folds.

use uuid::Uuid;

use super::{own_device, voter_signed, RosterState};
use crate::attachment::{self, AttachmentRef};
use crate::events::{SessionEvent, SessionEventEnvelope};
use crate::identity::{ActorKind, WorkspaceRole};
use crate::session::ChatSession;

/// Payload-only refusals that hold on every log and tier (checked before
/// the agent and phone tiers return): config kinds stay on the config log,
/// chat kinds on chat logs, a policy is well formed, a hash is a hash, the
/// blocklist is bounded, and a proposal that carries a file is allowed by the
/// workspace's policy at its position.
pub fn payload_refusal(env: &SessionEventEnvelope, on_config: bool, checked: bool, at: Option<&RosterState>, running: &RosterState) -> Option<String> {
    match &env.payload {
        SessionEvent::AttachmentPolicySet { policy } => {
            if !on_config {
                return Some("the attachment policy is set on the workspace config log".into());
            }
            policy.well_formed().err().map(|e| format!("attachment policy: {e}"))
        }
        SessionEvent::AttachmentHashBlocked { sha256 } | SessionEvent::AttachmentHashUnblocked { sha256 } => {
            if !on_config {
                return Some("the attachment blocklist lives on the workspace config log".into());
            }
            if !attachment::is_sha256_hex(sha256) {
                return Some("a blocked file is named by the lowercase hex SHA-256 of its content".into());
            }
            let adding = matches!(env.payload, SessionEvent::AttachmentHashBlocked { .. });
            if adding && !running.attachments.blocked.contains(sha256) && running.attachments.blocked.len() >= attachment::MAX_BLOCKLIST {
                return Some("the attachment blocklist is full".into());
            }
            None
        }
        SessionEvent::AttachmentShared { .. } | SessionEvent::AttachmentDeleted { .. } if on_config => {
            Some("attachments are shared in a chat, not on the workspace config log".into())
        }
        SessionEvent::ProposalUpserted { proposal } => {
            let a = proposal.attachment.as_deref()?;
            if on_config {
                return Some("a proposal is filed in a chat".into());
            }
            if let Err(e) = attachment::check_ref(a, false) {
                return Some(format!("proposed attachment: {e}"));
            }
            if proposal.kind != crate::proposals::ProposalKind::Decision {
                return Some("a proposed attachment rides a decision proposal".into());
            }
            if proposal.producing_agent_id.is_none() {
                return Some("only an agent's file is offered as a proposal".into());
            }
            if !checked {
                return None;
            }
            let Some(at) = at else { return Some("no workspace position".into()) };
            // The offer names the host that holds the file, and that host
            // signs it (itself, or through an agent key it certified): nobody
            // can point an offer at another member's device.
            let signer = env.signer_device_id;
            let host = signer.and_then(|k| at.agent_keys.get(&k)).map(|k| k.host_device_id).or(signer);
            if proposal.produced_on.is_none() || proposal.produced_on != host {
                return Some("an offered file names the host that produced it, which signs the offer".into());
            }
            if host.is_none_or(|h| a.id != attachment::offer_id(h, proposal.id)) {
                return Some("an offered file's id is bound to its host and proposal".into());
            }
            let rules = &at.attachments;
            if !rules.policy.enabled || !rules.policy.agents {
                return Some("agents can't offer files in this workspace".into());
            }
            if rules.blocked.contains(&a.sha256) {
                return Some("this file is blocked in this workspace".into());
            }
            rules.policy.file_refusal(&attachment::type_tokens(&a.name, None), a.size)
        }
        _ => None,
    }
}

/// `AttachmentShared` against the workspace roster `at` at its position.
pub fn share_ok(env: &SessionEventEnvelope, a: &AttachmentRef, fold: &ChatSession, at: &RosterState, author: &str, role: WorkspaceRole) -> Result<(), String> {
    attachment::check_ref(a, true)?;
    let rules = &at.attachments;
    let kind = at.members.get(author).map(|m| m.kind).unwrap_or(ActorKind::Human);
    if let Some(why) = rules.policy.sharer_refusal(kind, role) {
        return Err(why);
    }
    if rules.blocked.contains(&a.sha256) {
        return Err("this file is blocked in this workspace".into());
    }
    if let Some(why) = rules.policy.file_refusal(&attachment::type_tokens(&a.name, None), a.size) {
        return Err(why);
    }
    if fold.attachments.contains_key(&a.id) {
        return Err("that attachment was already shared".into());
    }
    if let Some(m) = a.message_id {
        if fold.authz.message_authors.get(&m).map(String::as_str) != Some(author) {
            return Err("an attachment names only its sharer's own message".into());
        }
    }
    match a.proposal_id {
        // A person (or an allowed service) shares from their own device,
        // under the id bound to that device and the blob.
        None => {
            if !own_device(env, at) {
                return Err("a file is shared from its sharer's own certified device".into());
            }
            if env.signer_device_id.is_none_or(|d| a.id != attachment::share_id(d, &a.blob_id)) {
                return Err("an attachment id is bound to the device that shares it".into());
            }
        }
        // Carrying out an agent's approved offer.
        Some(pid) => {
            if !rules.policy.agents {
                return Err("agents can't offer files in this workspace".into());
            }
            let Some(p) = fold.proposals.iter().find(|p| p.id == pid) else {
                return Err("no such proposal".into());
            };
            if p.producing_agent_id.is_none() {
                return Err("only an agent's offer is shared this way".into());
            }
            if !p.attachment.as_deref().is_some_and(|pa| pa.same_file(a)) {
                return Err("the shared file isn't the one the proposal offered".into());
            }
            // Approved, with a signed approval by a person who is a human
            // member (Contributor or above) of the workspace at this position.
            let by_person = fold.guards.verified_approvals.get(&pid).is_some_and(|voters| {
                voters.iter().any(|v| {
                    at.members.get(v).is_some_and(|m| m.kind == ActorKind::Human && m.role.rank() >= WorkspaceRole::Contributor.rank())
                        && p.producing_agent_id.is_none_or(|a| a.to_string() != *v)
                })
            });
            if !p.ready_to_apply() || !by_person {
                return Err("a file an agent offered is shared only after a person approves it".into());
            }
            if p.produced_on.is_none() || env.signer_device_id != p.produced_on {
                return Err("an agent's file is shared by the host that produced it".into());
            }
            if !voter_signed(env, at) {
                return Err("signed by a device that doesn't act for its author".into());
            }
            if fold.attachments.values().any(|s| s.attachment.proposal_id == Some(pid)) {
                return Err("that proposal's file was already shared".into());
            }
        }
    }
    Ok(())
}

/// `AttachmentDeleted`: the sharer's, or an Admin's, through a certified
/// device of theirs.
pub fn delete_ok(env: &SessionEventEnvelope, attachment_id: Uuid, fold: &ChatSession, at: &RosterState, author: &str, role: WorkspaceRole) -> Result<(), String> {
    let Some(s) = fold.attachments.get(&attachment_id) else {
        return Err("no such attachment".into());
    };
    if s.deleted {
        return Err("that attachment is already deleted".into());
    }
    if s.author != author && role.rank() < WorkspaceRole::Admin.rank() {
        return Err("only its sharer or an Admin deletes an attachment".into());
    }
    if !voter_signed(env, at) {
        return Err("signed by a device that doesn't act for its author".into());
    }
    Ok(())
}

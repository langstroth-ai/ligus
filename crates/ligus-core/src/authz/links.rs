//! Linked identities inside the fold (IDENTITY-AND-FORGES §2.2): who may
//! publish and remove one, and the offline checks a link must pass.
//!
//! Only the account itself publishes its links, from a certified device that
//! holds the account (not a delegated host, never an agent), on the workspace
//! config log, at its own role (any member). Every check here is offline:
//! the SSHSIG and the forge are checked device-locally
//! (`ligus_runtime::identity_links`), never in the fold, and a link changes
//! nothing about membership, roles or sealing.

use uuid::Uuid;

use crate::events::SessionEventEnvelope;
use crate::identity::{ActorKind, WorkspaceRole};
use crate::identity_link::{self, IdentityLink, MAX_LIVE_LINKS};
use crate::session::ChatSession;

use super::RosterState;

/// `IdentityLinkPublished`.
pub fn publish_ok(
    env: &SessionEventEnvelope,
    link: &IdentityLink,
    on_config: bool,
    roster: &RosterState,
) -> Result<(), String> {
    if !on_config {
        return Err("an identity link is published on the workspace config log".into());
    }
    let stamp = env
        .actor_stamp
        .as_ref()
        .ok_or("an identity link names its author")?;
    if stamp.actor.kind != ActorKind::Human || stamp.runtime_effect {
        return Err("only a person publishes an identity link".into());
    }
    if stamp.actor.account_id != Some(link.account_id) {
        return Err("an identity link is published by its own account".into());
    }
    let device = super::certified_signer(env, roster)?;
    if device.delegated_for(link.account_id) {
        return Err("a delegated host can't publish its owner's identity links".into());
    }
    identity_link::check_shape(link)?;
    identity_link::check_statement(
        link,
        env.workspace_id,
        roster
            .account_keys
            .get(&link.account_id)
            .map(|k| k.as_slice()),
    )?;
    let live = roster.links.get(&link.account_id);
    if live.is_some_and(|m| {
        m.values()
            .any(|l| l.link_id == link.link_id && l.subject_key() != link.subject_key())
    }) {
        return Err("that link id names another identity".into());
    }
    if roster
        .links
        .iter()
        .any(|(a, m)| *a != link.account_id && m.contains_key(&link.link_id))
    {
        return Err("that link id belongs to another account".into());
    }
    let others = live
        .map(|m| {
            m.values()
                .filter(|l| l.subject_key() != link.subject_key())
                .count()
        })
        .unwrap_or(0);
    if others >= MAX_LIVE_LINKS {
        return Err(format!(
            "at most {MAX_LIVE_LINKS} identity links per account"
        ));
    }
    Ok(())
}

/// `IdentityLinkRemoved`: the account itself, or an Admin+ who isn't below
/// the account's member (moderation), of a link that exists.
pub fn removal_ok(
    env: &SessionEventEnvelope,
    account_id: Uuid,
    link_id: Uuid,
    on_config: bool,
    roster: &RosterState,
    fold: &ChatSession,
    author: &str,
) -> Result<(), String> {
    if !on_config {
        return Err("an identity link is removed on the workspace config log".into());
    }
    if !roster
        .links
        .get(&account_id)
        .is_some_and(|m| m.contains_key(&link_id))
    {
        return Err("no such identity link".into());
    }
    let stamp = env.actor_stamp.as_ref().ok_or("unsigned")?;
    if stamp.actor.kind != ActorKind::Human || stamp.runtime_effect {
        return Err("only a person removes an identity link".into());
    }
    if stamp.actor.account_id == Some(account_id) {
        let device = super::certified_signer(env, roster)?;
        if device.delegated_for(account_id) {
            return Err("a delegated host can't remove its owner's identity links".into());
        }
        return Ok(());
    }
    let rank = fold.role_of(author).rank();
    if rank < WorkspaceRole::Admin.rank() {
        return Err("only the account itself or an Admin removes an identity link".into());
    }
    let target = fold
        .members
        .iter()
        .filter(|m| m.actor.account_id == Some(account_id))
        .map(|m| m.role.rank())
        .max()
        .unwrap_or(0);
    if target > rank {
        return Err("an Admin can't remove the links of a member above them".into());
    }
    Ok(())
}

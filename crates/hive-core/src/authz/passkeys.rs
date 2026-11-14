//! Passkey checks inside the fold (Phase 0, B3): the stamped presence, and
//! the passkey registration and removal events.

use uuid::Uuid;

use crate::events::SessionEventEnvelope;
use crate::identity::WorkspaceRole;
use crate::presence::{self, PasskeyCredential, PasskeyKey, Policy};
use crate::session::ChatSession;

use super::RosterState;

/// The account whose passkey the stamped presence proves, if the stamp
/// carries one. A presence that doesn't verify is an error, never ignored:
/// the credential must be registered to the stamped account at this
/// position, and the assertion must sign this event's digest.
pub fn verify_stamped(env: &SessionEventEnvelope, roster: &RosterState) -> Result<Option<Uuid>, String> {
    let Some(a) = presence::presence_of(env).map_err(|e| e.0)? else { return Ok(None) };
    let account = env
        .actor_stamp
        .as_ref()
        .and_then(|s| s.actor.account_id)
        .ok_or("presence on an event without an account")?;
    let key = roster
        .passkeys
        .get(&account)
        .and_then(|m| m.get(&a.credential_id))
        .ok_or("the presence credential isn't registered to the stamped account here")?;
    presence::verify_get(a, key, &presence::challenge(env), &Policy::ligus()).map_err(|e| format!("presence: {}", e.0))?;
    Ok(Some(account))
}

/// `PasskeyRegistered`: on the config log, for the signer's own account,
/// from a certified device that holds the account key (not a delegated
/// host), well formed, new; the account's first credential proves itself (an
/// assertion by the new passkey, or the recovery key's own signature, over
/// this event's digest), every later one needs presence from a credential the
/// account already has.
pub fn registration_ok(
    env: &SessionEventEnvelope,
    c: &PasskeyCredential,
    on_config: bool,
    roster: &RosterState,
) -> Result<(), String> {
    if !on_config {
        return Err("a passkey is registered on the workspace config log".into());
    }
    let account = env.actor_stamp.as_ref().and_then(|s| s.actor.account_id);
    if account != Some(c.account_id) {
        return Err("a passkey is registered by its own account".into());
    }
    let device = super::certified_signer(env, roster)?;
    if device.delegated_for(c.account_id) {
        return Err("a delegated host can't register its owner's passkeys".into());
    }
    presence::credential_well_formed(c).map_err(|e| e.0)?;
    let existing = roster.passkeys.get(&c.account_id).filter(|m| !m.is_empty());
    if existing.is_some_and(|m| m.contains_key(&c.credential_id)) {
        return Err("that credential is already registered".into());
    }
    let a = presence::presence_of(env).map_err(|e| e.0)?.ok_or("a passkey registration carries its proof")?;
    let challenge = presence::challenge(env);
    match existing {
        Some(m) => {
            let key = m.get(&a.credential_id).ok_or("another passkey needs presence from one the account already has")?;
            presence::verify_get(a, key, &challenge, &Policy::ligus()).map_err(|e| format!("presence: {}", e.0))
        }
        None if c.recovery => {
            if a.credential_id != c.credential_id {
                return Err("a recovery key proves itself".into());
            }
            presence::verify_recovery(a, &PasskeyKey::from(c), &challenge).map_err(|e| format!("recovery proof: {}", e.0))
        }
        // The account's first passkey proves possession: an assertion by the
        // new credential itself over this registration's digest (a
        // `webauthn.create` can't sign it: its key is part of the digest).
        None => {
            if a.credential_id != c.credential_id {
                return Err("the first passkey proves itself".into());
            }
            presence::verify_get(a, &PasskeyKey::from(c), &challenge, &Policy::ligus()).map_err(|e| format!("registration proof: {}", e.0))
        }
    }
}

/// `PasskeyRemoved`: on the config log, by the account itself or an Admin+,
/// of a credential that exists.
pub fn removal_ok(
    env: &SessionEventEnvelope,
    account_id: Uuid,
    credential_id: &str,
    on_config: bool,
    roster: &RosterState,
    fold: &ChatSession,
    author: &str,
) -> Result<(), String> {
    if !on_config {
        return Err("a passkey is removed on the workspace config log".into());
    }
    if !roster.passkeys.get(&account_id).is_some_and(|m| m.contains_key(credential_id)) {
        return Err("no such passkey".into());
    }
    let own = env.actor_stamp.as_ref().and_then(|s| s.actor.account_id) == Some(account_id);
    if own {
        return Ok(());
    }
    let rank = fold.role_of(author).rank();
    if rank < WorkspaceRole::Admin.rank() {
        return Err("only the account itself or an Admin removes a passkey".into());
    }
    // Never someone above you: stripping an Owner's credentials would let
    // that account's next passkey register on a device key alone (TOFU).
    let target = fold
        .members
        .iter()
        .filter(|m| m.actor.account_id == Some(account_id))
        .map(|m| m.role.rank())
        .max()
        .unwrap_or(0);
    if target > rank {
        return Err("an Admin can't remove the passkeys of a member above them".into());
    }
    Ok(())
}

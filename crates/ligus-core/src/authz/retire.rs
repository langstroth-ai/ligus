//! Who may retire a duplicate member (identity model v2, 8.3; decision O5).

use crate::events::SessionEventEnvelope;
use crate::identity::{ActorKind, WorkspaceRole};
use crate::retirement::MemberRetirement;
use crate::session::ChatSession;

use super::{RosterState, RULES_V3};

/// `MemberRetired`, under rules 3 only: on the config log, by a person (the
/// old member or their person), carrying the old account's own signature;
/// the successor is a current human member whose certificate of the linked
/// computer that computer accepted; the old member ranks no higher than the
/// successor, isn't the founder or the last Owner, and holds no passkey
/// (its presence can't be given from the computer; an Admin removes it
/// instead).
pub fn retirement_ok(env: &SessionEventEnvelope, r: &MemberRetirement, on_config: bool, roster: &RosterState, fold: &ChatSession) -> Result<(), String> {
    if !on_config {
        return Err("a member is retired on the workspace config log".into());
    }
    if roster.rules < RULES_V3 {
        return Err("a member is retired into their person only under workspace rules 3".into());
    }
    let stamp = env.actor_stamp.as_ref().ok_or("a retirement names its author")?;
    if stamp.actor.kind != ActorKind::Human || stamp.runtime_effect {
        return Err("only a person retires a member".into());
    }
    if !stamp.actor.account_id.is_some_and(|a| a == r.account_id || a == r.successor_account_id) {
        return Err("a member is retired by itself or by its person".into());
    }
    super::certified_signer(env, roster)?;
    // Once: a retirement signature can't be replayed after an Admin
    // deliberately added the member back.
    if fold.retired.contains_key(&r.member_id) {
        return Err("that member was retired before: an Admin removes it now".into());
    }
    if r.account_id == r.successor_account_id {
        return Err("a member retires into someone else".into());
    }
    let old = fold
        .members
        .iter()
        .find(|m| m.id == r.member_id && m.actor.kind == ActorKind::Human && m.actor.account_id == Some(r.account_id))
        .ok_or("no such member")?;
    let new = fold
        .members
        .iter()
        .find(|m| m.actor.kind == ActorKind::Human && m.actor.account_id == Some(r.successor_account_id))
        .ok_or("the person isn't a member here")?;
    let key = roster.account_keys.get(&r.account_id).ok_or("the retired account has no key here")?;
    if !r.verifies(env.workspace_id, key) {
        return Err("a retirement carries the retired account's own signature".into());
    }
    let device = roster
        .devices
        .get(&r.device_id)
        .filter(|d| !d.revoked && !d.phone && d.accounts.contains(&r.successor_account_id))
        .ok_or("the linked computer isn't the person's device here")?;
    let _ = device;
    if old.role.rank() > new.role.rank() {
        return Err("a member retires only into a person ranked at least as high".into());
    }
    if !roster.founder.is_empty() && roster.founder == old.id {
        return Err("the founder isn't retired".into());
    }
    if old.role == WorkspaceRole::Owner && fold.members.iter().filter(|m| m.role == WorkspaceRole::Owner).count() < 2 {
        return Err("the last Owner isn't retired".into());
    }
    if roster.passkeys.get(&r.account_id).is_some_and(|m| !m.is_empty()) {
        return Err("a member with a passkey is removed by an Admin, not retired".into());
    }
    Ok(())
}

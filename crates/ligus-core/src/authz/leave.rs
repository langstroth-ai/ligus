//! Who may leave (`MemberLeft`, workspace model v3, 10.4). Rules 3: display
//! only, so nothing here changes what anyone else may do.

use crate::events::SessionEventEnvelope;
use crate::identity::{ActorKind, WorkspaceRole};
use crate::session::ChatSession;

use super::{RosterState, RULES_V3};

/// `MemberLeft`, under rules 3 and later: on the config log; by the member
/// itself (a person or a box, never an agent or a runtime effect); for its
/// own account; signed by a device certified for that account here that is
/// neither a phone nor delegated (a linked computer, a box acting for the
/// person), so a stolen linked computer or phone can't make its person
/// leave; a current member that hasn't asked already and wasn't retired;
/// never the founder, and never the last Owner counting the Owners who
/// already asked to leave. The passkey, when the account holds one, is the
/// presence table's (`authz::rules`).
pub fn leave_ok(env: &SessionEventEnvelope, member_id: &str, account_id: Option<uuid::Uuid>, on_config: bool, roster: &RosterState, fold: &ChatSession) -> Result<(), String> {
    if !on_config {
        return Err("a member leaves on the workspace config log".into());
    }
    if roster.rules < RULES_V3 {
        return Err("this workspace is on older rules: ask an Admin to remove you".into());
    }
    let stamp = env.actor_stamp.as_ref().ok_or("a leave names its author")?;
    if !matches!(stamp.actor.kind, ActorKind::Human | ActorKind::Service) || stamp.runtime_effect {
        return Err("only a person or a box leaves".into());
    }
    if stamp.actor.id != member_id {
        return Err("a member leaves only by itself: an Admin removes someone else".into());
    }
    let member = fold.members.iter().find(|m| m.id == member_id).ok_or("not a member here")?;
    if member.actor.account_id != account_id || stamp.actor.account_id != account_id {
        return Err("a leave names the leaver's own account".into());
    }
    let device = super::certified_signer(env, roster)?;
    if device.phone {
        return Err("a phone can't make its person leave: leave from your desktop".into());
    }
    if let Some(a) = account_id {
        if device.delegated_for(a) {
            return Err("a linked device can't make its person leave: leave from your desktop".into());
        }
    }
    if fold.left.contains_key(member_id) {
        return Err("already asked to leave".into());
    }
    if fold.retired.contains_key(member_id) {
        return Err("that member was retired".into());
    }
    if !roster.founder.is_empty() && roster.founder == member_id {
        return Err(crate::leave::FOUNDER.into());
    }
    if member.role == WorkspaceRole::Owner {
        let staying = fold.members.iter().filter(|m| m.role == WorkspaceRole::Owner && m.id != member_id && !fold.left.contains_key(&m.id)).count();
        if staying == 0 {
            return Err(crate::leave::LAST_OWNER.into());
        }
    }
    Ok(())
}

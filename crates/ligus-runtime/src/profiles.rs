//! Names and pictures resolved at render time (identity model v2, 4.3 and
//! 5): the newest `ProfileUpdated` for an account on the workspace's config
//! log wins over every frozen copy (the roster's `MemberAdded`, a stamp, a
//! message's author). Agents keep their own names.

use std::collections::BTreeMap;

use ligus_core::profile::Profile;
use ligus_core::{ActorIdentity, ActorKind, ChatSession};
use uuid::Uuid;

/// Apply `profiles` to an actor (not an agent).
pub fn resolve_actor(actor: &mut ActorIdentity, profiles: &BTreeMap<Uuid, Profile>) {
    if actor.kind == ActorKind::Agent {
        return;
    }
    let Some(p) = actor.account_id.and_then(|a| profiles.get(&a)) else { return };
    if let Some(n) = &p.display_name {
        actor.display_name = n.clone();
    }
    if let Some(a) = &p.avatar {
        actor.avatar_url = Some(a.data_url());
    } else if p.avatar_cleared {
        actor.avatar_url = None;
    }
}

/// Resolve every name and picture in `session` from `profiles`: members,
/// message authors, attachment sharers.
pub fn resolve(session: &mut ChatSession, profiles: &BTreeMap<Uuid, Profile>) {
    if profiles.is_empty() {
        return;
    }
    for m in &mut session.members {
        resolve_actor(&mut m.actor, profiles);
    }
    for msg in &mut session.messages {
        if let Some(actor) = msg.actor_identity.as_mut() {
            if actor.kind != ActorKind::Agent && msg.role == ligus_core::MessageRole::User {
                let before = actor.display_name.clone();
                resolve_actor(actor, profiles);
                if msg.author == before {
                    msg.author = actor.display_name.clone();
                }
            }
        }
    }
    for a in session.attachments.values_mut() {
        if let Some(p) = a.author_account.and_then(|acct| profiles.get(&acct)).and_then(|p| p.display_name.clone()) {
            a.author_name = p;
        }
    }
}

/// The name `account` goes by in a workspace: its profile, else its roster
/// record, else `fallback`.
pub fn name_of(config: &ChatSession, account: Uuid, fallback: &str) -> String {
    config
        .profiles
        .get(&account)
        .and_then(|p| p.display_name.clone())
        .or_else(|| config.members.iter().find(|m| m.actor.account_id == Some(account)).map(|m| m.actor.display_name.clone()))
        .unwrap_or_else(|| fallback.to_string())
}

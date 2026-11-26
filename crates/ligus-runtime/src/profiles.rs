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
    let Some(p) = actor.account_id.and_then(|a| profiles.get(&a)) else {
        return;
    };
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
    resolve_names(session, profiles);
    disambiguate(session);
}

/// Names are never identities (5.6): a person whose name looks like
/// another account's, an agent's, or a reserved name ("Ligus", "Hive") is
/// shown as `Name#abcd` (the first four hex digits of their account id),
/// everywhere this session renders, prompts included.
fn disambiguate(session: &mut ChatSession) {
    use ligus_proto::names::name_skeleton;
    let mut by_skeleton: BTreeMap<String, std::collections::BTreeSet<Uuid>> = BTreeMap::new();
    let mut note = |name: &str, account: Uuid| {
        by_skeleton
            .entry(name_skeleton(name))
            .or_default()
            .insert(account);
    };
    for m in &session.members {
        if let (ActorKind::Human | ActorKind::Service, Some(a)) = (m.actor.kind, m.actor.account_id)
        {
            note(&m.actor.display_name, a);
        }
    }
    for msg in &session.messages {
        if let Some(a) = msg
            .actor_identity
            .as_ref()
            .filter(|a| a.kind != ActorKind::Agent)
        {
            if let Some(acct) = a.account_id {
                note(&a.display_name, acct);
            }
        }
    }
    let mut reserved: std::collections::BTreeSet<String> =
        ["ligus", "hive"].iter().map(|n| name_skeleton(n)).collect();
    reserved.extend(
        session
            .workspace_agents
            .iter()
            .map(|a| name_skeleton(&a.name)),
    );
    let collides = |name: &str, account: Uuid| {
        let k = name_skeleton(name);
        reserved.contains(&k)
            || by_skeleton
                .get(&k)
                .is_some_and(|s| s.iter().any(|x| *x != account))
    };
    let tag = |name: &str, account: Uuid| format!("{name}#{}", &account.simple().to_string()[..4]);
    let mut renamed: BTreeMap<Uuid, String> = BTreeMap::new();
    for m in &mut session.members {
        if let (ActorKind::Human | ActorKind::Service, Some(a)) = (m.actor.kind, m.actor.account_id)
        {
            if collides(&m.actor.display_name, a) {
                let n = tag(&m.actor.display_name, a);
                renamed.insert(a, n.clone());
                m.actor.display_name = n;
            }
        }
    }
    for msg in &mut session.messages {
        if msg.role != ligus_core::MessageRole::User {
            continue;
        }
        let Some(actor) = msg
            .actor_identity
            .as_mut()
            .filter(|a| a.kind != ActorKind::Agent)
        else {
            continue;
        };
        let Some(acct) = actor.account_id else {
            continue;
        };
        let name = renamed.get(&acct).cloned().or_else(|| {
            collides(&actor.display_name, acct).then(|| tag(&actor.display_name, acct))
        });
        if let Some(n) = name {
            if msg.author == actor.display_name {
                msg.author = n.clone();
            }
            actor.display_name = n;
        }
    }
}

fn resolve_names(session: &mut ChatSession, profiles: &BTreeMap<Uuid, Profile>) {
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
        if let Some(p) = a
            .author_account
            .and_then(|acct| profiles.get(&acct))
            .and_then(|p| p.display_name.clone())
        {
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
        .or_else(|| {
            config
                .members
                .iter()
                .find(|m| m.actor.account_id == Some(account))
                .map(|m| m.actor.display_name.clone())
        })
        .unwrap_or_else(|| fallback.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ligus_core::ChatMessage;

    /// Review R6: a person renamed "Ligus", or to another member's name,
    /// never reads as the default agent or as that member.
    #[test]
    fn a_colliding_name_is_told_apart() {
        let (ana, mallory) = (Uuid::new_v4(), Uuid::new_v4());
        let mut s = ChatSession::new("plans", Uuid::new_v4(), "rt");
        let mut by = |acct: Uuid, name: &str| {
            let mut a = ActorIdentity::new(acct.to_string(), name, ActorKind::Human);
            a.account_id = Some(acct);
            let mut m = ChatMessage::new(ligus_core::MessageRole::User, name, "hi");
            m.actor_identity = Some(a);
            s.messages.push(m);
        };
        by(ana, "Ana Ruiz");
        by(mallory, "Mallory");
        let mut profiles = BTreeMap::new();
        profiles.insert(
            ana,
            Profile {
                display_name: Some("Ana Ruiz".into()),
                ..Default::default()
            },
        );
        profiles.insert(
            mallory,
            Profile {
                display_name: Some("Ligus".into()),
                ..Default::default()
            },
        );
        let mut s2 = s.clone();
        resolve(&mut s, &profiles);
        assert_ne!(
            s.messages[1].author, "Ligus",
            "a person renamed 'Ligus' reads as the default agent"
        );
        assert!(s.messages[1].author.starts_with("Ligus#"));
        assert_eq!(s.messages[0].author, "Ana Ruiz", "no collision, no tag");
        profiles.insert(
            mallory,
            Profile {
                display_name: Some("Ana Ruiz".into()),
                ..Default::default()
            },
        );
        resolve(&mut s2, &profiles);
        assert_ne!(s2.messages[0].author, s2.messages[1].author);
    }
}

//! Which device answers a turn: the dispatch gates every client shares.
//!
//! A message is answered by at most one runtime, on one device. Two gates
//! decide whether that device is this one: the **local-send** gate (a person
//! posts here and this device may answer at once) and the **relay-sync** gate
//! (a message arrives through sync and this device may pick it up). Both
//! must call [`turn_runs_here`] (through [`runs_here_linked`]) with the same
//! inputs, or a turn is answered twice or never. A workspace with no relay,
//! or with at most one person in it, always answers here: there is no other
//! device to defer to.
//!
//! Also here: what a message triggers ([`dispatch_target`]), whether a
//! synced user turn is still unanswered ([`user_turn_answered`],
//! [`synced_user_triggers`]), and when a deferred turn falls back to this
//! device ([`deferred_turn_unanswered`], [`DEFERRED_CLAIM_TIMEOUT`]).

use hive_core::{ChatSession, DeviceLinks, MessageRole};
use uuid::Uuid;

use crate::mentions::parse_mentions;

/// Whether *this* device runs a stage or turn owned by `owner_actor_id`: an
/// unowned responder (legacy / local-only) or one owned by us.
pub fn owner_runs_here(local_actor_id: &str, owner_actor_id: &str) -> bool {
    owner_actor_id.is_empty() || owner_actor_id == local_actor_id
}

/// Where a responder's host binding puts its turns, seen from this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostBinding {
    /// No host: the owner's device answers.
    Unbound,
    /// Sent to this device.
    ThisDevice,
    /// Sent to another host (a worker, the home host).
    Elsewhere,
}

/// The binding of a responder whose host is `host_id` (empty: none), seen
/// from the host `this_device`.
pub fn host_binding(host_id: &str, this_device: &str) -> HostBinding {
    if host_id.is_empty() {
        HostBinding::Unbound
    } else if host_id == this_device {
        HostBinding::ThisDevice
    } else {
        HostBinding::Elsewhere
    }
}

/// Whether *this* device runs a turn for a responder owned by `owner`.
///
/// The cross-device ownership split only means anything when the workspace
/// actually syncs to other devices, i.e. a relay is configured. Two cases make
/// this device the sole possible responder, so it must always answer:
///   - `local_only`: no relay configured. Nothing ever syncs off this device, so
///     deferring to an "owner" would be permanent silence. This is also the only
///     robust guard against identity churn: a churned self is hoisted into the
///     workspace roster as a *second* human member under a new account id, so
///     `solo` (≤1 human) no longer holds and the primary would defer to a stale
///     self-identity that no live device owns.
///   - `solo`: a relay *is* configured but there's ≤1 human: a room you made
///     that no one else has joined. Still no other device to defer to.
///
/// Only a real, synced, multi-human room falls back to the ownership check.
///
/// An agent sent to a host (`binding`) runs on that host only: the worker
/// there answers every mention for it (`ligus worker` / `ligus serve`).
pub fn turn_runs_here(local_only: bool, solo: bool, local: &str, owner: &str, binding: HostBinding) -> bool {
    match binding {
        _ if local_only => true,
        HostBinding::ThisDevice => true,
        HostBinding::Elsewhere => false,
        HostBinding::Unbound => solo || owner_runs_here(local, owner),
    }
}

/// Distinct *people* among the chat's human members. A member's own devices
/// (a `ligus serve` box linked to its owner, see [`DeviceLinks`]) count once
/// with them, so you plus your phone's home box is still a solo room.
pub fn human_member_count(session: &ChatSession, links: &DeviceLinks) -> usize {
    links.human_count(session)
}

/// [`turn_runs_here`] with device links applied: solo counts people, not
/// member records, and a responder owned by a linked device (the chat a phone
/// created through its owner's `ligus serve` box) is owned by that device's
/// account, so the owner's desktop answers it. The single-answer claim still
/// arbitrates between the owner's devices. This is the one gate both the
/// local-send and the relay-sync paths call.
pub fn runs_here_linked(
    local_only: bool,
    links: &DeviceLinks,
    session: &ChatSession,
    local: &str,
    owner: &str,
    binding: HostBinding,
) -> bool {
    let solo = human_member_count(session, links) <= 1;
    turn_runs_here(local_only, solo, local, links.person(owner), binding)
}

/// Who, if anyone, a message should be answered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchTarget {
    /// The chat's primary runtime.
    Primary,
    /// A workspace agent.
    Agent(Uuid),
}

/// Decide whether a message triggers a runtime, and which one. An `@agent` or
/// `@primary` mention is always answered. An *un-addressed* message defaults
/// to the primary **only in an effectively-solo workspace**: multi-human rooms
/// stay human-to-human until someone explicitly addresses an assistant.
///
/// `local_only` (no relay) means nothing ever syncs in from another device, so
/// every roster member was necessarily created here and *is this user*: the
/// workspace is solo by definition however many members a churned roster
/// shows. A relay-backed room uses the real human count.
pub fn dispatch_target(body: &str, session: &ChatSession, local_only: bool, links: &DeviceLinks) -> Option<DispatchTarget> {
    let mentions = parse_mentions(body, session);
    if let Some(id) = mentions.agents.first().copied() {
        return Some(DispatchTarget::Agent(id));
    }
    if mentions.primary {
        return Some(DispatchTarget::Primary);
    }
    (local_only || human_member_count(session, links) <= 1).then_some(DispatchTarget::Primary)
}

/// How long a deferred turn waits for its owner's device to pick it up before
/// this device answers it anyway. Long enough that a healthy peer's
/// `TurnClaimed` syncs first and this never fires; short enough that the user
/// isn't left staring at a dead chat.
pub const DEFERRED_CLAIM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

/// Whether a turn we deferred is *still* unanswered once
/// [`DEFERRED_CLAIM_TIMEOUT`] has elapsed, and this device should take it over.
///
/// Three independent guards, each conservative in the same direction: a false
/// "already handled" costs one unanswered turn, a false "unanswered" costs a
/// duplicate answer in a live room.
pub fn deferred_turn_unanswered(session: &ChatSession, trigger_id: Uuid) -> bool {
    // 1. Someone claimed it, including us on an earlier pass.
    if session.turn_claims.contains_key(&trigger_id) {
        return false;
    }
    let Some(at) = session.messages.iter().position(|m| m.id == trigger_id) else {
        // Trigger vanished (message deleted mid-wait): nothing to answer.
        return false;
    };
    // 2. Someone is answering (or has answered) it, claim synced or not: an
    //    assistant row after the trigger that replies to anything at or after
    //    the trigger's own position. Positional, because a System row trailing
    //    the user turn can make the two ends of the handshake stamp different
    //    ids.
    let answered = session.messages[at + 1..].iter().any(|m| {
        m.role == MessageRole::Assistant
            && m.reply_to_message_id.is_some_and(|id| session.messages[at..].iter().any(|target| target.id == id))
    });
    if answered {
        return false;
    }
    // 3. The user moved on: a later user turn supersedes this one, and its own
    //    send arms a fresh fallback against the fuller history.
    !session.messages[at + 1..].iter().any(|m| m.role == MessageRole::User)
}

/// Whether `trigger` (a user message) already has an answer: a later reply
/// stamped with its id. A peer that predates exact trigger stamping stamped
/// the last non-streaming row, which is a System row when one trails the
/// user turn, so a reply to a System row directly after `trigger` counts too.
/// Deliberately exact otherwise: a reply to a *later* user message does not
/// answer this one.
pub fn user_turn_answered(session: &ChatSession, trigger: Uuid) -> bool {
    let Some(at) = session.messages.iter().position(|m| m.id == trigger) else {
        return true; // Gone (deleted): nothing to answer.
    };
    let keys: Vec<Uuid> = std::iter::once(trigger)
        .chain(session.messages[at + 1..].iter().take_while(|m| m.role == MessageRole::System).map(|m| m.id))
        .collect();
    session.messages[at + 1..].iter().any(|m| {
        matches!(m.role, MessageRole::Assistant | MessageRole::Agent) && m.reply_to_message_id.is_some_and(|id| keys.contains(&id))
    })
}

/// The user messages among `arrived` (what one sync pull brought into this
/// chat) that may still need an answer, in transcript order: unanswered, and
/// not claimed by another device. Ownership, addressing and the claim are
/// checked per trigger when it runs.
pub fn synced_user_triggers(session: &ChatSession, arrived: &[Uuid], my_device: Uuid) -> Vec<Uuid> {
    session
        .messages
        .iter()
        .filter(|m| m.role == MessageRole::User && arrived.contains(&m.id))
        .filter(|m| session.turn_claims.get(&m.id).is_none_or(|d| *d == my_device))
        .filter(|m| !user_turn_answered(session, m.id))
        .map(|m| m.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::{ActorIdentity, ActorKind, ChatMessage, Timestamp, WorkspaceAgent, WorkspaceMember, WorkspaceRole};

    fn member(id: &str) -> WorkspaceMember {
        WorkspaceMember {
            id: id.into(),
            actor: ActorIdentity::new(id.to_string(), id.to_string(), ActorKind::Human),
            role: WorkspaceRole::Contributor,
            title: String::new(),
            index: 1,
            joined_at: Timestamp::now(),
        }
    }

    fn session(humans: &[&str]) -> ChatSession {
        let mut s = ChatSession::new("t", Uuid::new_v4(), "");
        s.members = humans.iter().map(|h| member(h)).collect();
        s
    }

    fn msg(role: MessageRole, reply_to: Option<Uuid>) -> ChatMessage {
        let mut m = ChatMessage::new(role, "x", "author");
        m.reply_to_message_id = reply_to;
        m
    }

    #[test]
    fn a_host_binding_wins_over_ownership_unless_nothing_syncs() {
        let elsewhere = host_binding("sff", "desk");
        assert_eq!(elsewhere, HostBinding::Elsewhere);
        assert!(!turn_runs_here(false, true, "me", "me", elsewhere));
        assert!(turn_runs_here(true, false, "me", "me", elsewhere));
        assert!(turn_runs_here(false, false, "me", "you", host_binding("desk", "desk")));
        assert_eq!(host_binding("", "desk"), HostBinding::Unbound);
    }

    #[test]
    fn solo_or_local_always_answers_and_a_room_defers_to_the_owner() {
        let u = HostBinding::Unbound;
        assert!(turn_runs_here(false, true, "me", "stale-self", u));
        assert!(turn_runs_here(true, false, "me", "stale-self", u));
        assert!(!turn_runs_here(false, false, "me", "teammate", u));
        assert!(turn_runs_here(false, false, "me", "me", u));
        assert!(turn_runs_here(false, false, "me", "", u), "unowned runs anywhere");
    }

    #[test]
    fn the_linked_gate_counts_people_not_member_records() {
        let links = DeviceLinks::default();
        assert!(runs_here_linked(false, &links, &session(&["me"]), "me", "other", HostBinding::Unbound));
        assert!(!runs_here_linked(false, &links, &session(&["me", "you"]), "me", "you", HostBinding::Unbound));
    }

    #[test]
    fn unaddressed_messages_go_to_the_primary_only_when_solo() {
        let links = DeviceLinks::default();
        let mut room = session(&["me", "you"]);
        assert_eq!(dispatch_target("hi", &room, false, &links), None);
        assert_eq!(dispatch_target("hi", &room, true, &links), Some(DispatchTarget::Primary));
        assert_eq!(dispatch_target("hi", &session(&["me"]), false, &links), Some(DispatchTarget::Primary));
        let agent = WorkspaceAgent::new("coder", "rt");
        room.workspace_agents.push(agent.clone());
        assert_eq!(dispatch_target("@coder fix it", &room, false, &links), Some(DispatchTarget::Agent(agent.id)));
    }

    #[test]
    fn answered_and_deferred_turns() {
        let mut s = session(&["me"]);
        let user = msg(MessageRole::User, None);
        let trigger = user.id;
        s.messages.push(user);
        assert!(!user_turn_answered(&s, trigger));
        assert!(deferred_turn_unanswered(&s, trigger));
        let me = Uuid::new_v4();
        assert_eq!(synced_user_triggers(&s, &[trigger], me), vec![trigger]);
        // Claimed by another device: not ours, and not a fallback.
        s.turn_claims.insert(trigger, Uuid::new_v4());
        assert!(synced_user_triggers(&s, &[trigger], me).is_empty());
        assert!(!deferred_turn_unanswered(&s, trigger));
        s.turn_claims.clear();
        // A reply to a System row right after the trigger answers it.
        let sys = msg(MessageRole::System, None);
        let sys_id = sys.id;
        s.messages.push(sys);
        s.messages.push(msg(MessageRole::Assistant, Some(sys_id)));
        assert!(user_turn_answered(&s, trigger));
        assert!(!deferred_turn_unanswered(&s, trigger));
        // A deleted trigger counts as answered.
        assert!(user_turn_answered(&s, Uuid::new_v4()));
    }

    #[test]
    fn a_later_user_turn_supersedes_a_deferred_one() {
        let mut s = session(&["me"]);
        let user = msg(MessageRole::User, None);
        let trigger = user.id;
        s.messages.push(user);
        s.messages.push(msg(MessageRole::User, None));
        assert!(!deferred_turn_unanswered(&s, trigger));
    }
}

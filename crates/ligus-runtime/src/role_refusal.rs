//! What a refusal of the role gate means, in words a person can act on.
//!
//! The authorization fold says `role Viewer cannot perform messageAppended`
//! (`ligus_core::authorization::evaluate`). That reads like a bug report. A
//! client shows instead what was refused, why, and the fix: a member whose
//! role is too low asks an Admin for another role; a device that signs as
//! someone who isn't a member here (removed, or a computer still signing as
//! its old member after it was linked to the person's devices) is told so,
//! with how to link it again. Only the text changes: the decision is the
//! fold's, unchanged.

use ligus_core::{SessionEvent, WorkspaceMember, WorkspaceRole};

/// The fold's rank-gate refusal text, `role X cannot perform Y`.
pub fn is_role_refusal(summary: &str) -> bool {
    summary.starts_with("role ") && summary.contains(" cannot perform ")
}

/// What `kind` (an event's `kind_str`) does, for "X needs Contributor".
pub fn action_phrase(kind: &str) -> String {
    match kind {
        "messageAppended" => "posting a message".into(),
        "messageEdited" | "messageRedacted" => "changing a message".into(),
        "sessionCreated" | "chatCreated" => "starting a chat".into(),
        "proposalVoteCast" => "voting".into(),
        "memberAdded" => "adding a member".into(),
        "memberRemoved" => "removing a member".into(),
        "memberRoleChanged" => "changing a role".into(),
        other => format!("this change ({other})"),
    }
}

fn role_name(r: WorkspaceRole) -> &'static str {
    match r {
        WorkspaceRole::Owner => "Owner",
        WorkspaceRole::Admin => "Admin",
        WorkspaceRole::Contributor => "Contributor",
        WorkspaceRole::Viewer => "Viewer",
    }
}

/// The refusal of `payload` for the actor `author_id` (shown as `name`)
/// against `members`, explained. `summary` is returned unchanged when it
/// isn't the role gate's.
pub fn explain(summary: String, payload: &SessionEvent, author_id: &str, name: &str, members: &[WorkspaceMember]) -> String {
    if !is_role_refusal(&summary) {
        return summary;
    }
    let action = action_phrase(payload.kind_str());
    let shown = crate::display::shown(name);
    match members.iter().find(|m| m.id == author_id || m.actor.id == author_id) {
        None => format!(
            "Refused: this device signs as {shown}, who isn't a member of this workspace (any more), so {action} isn't allowed. \
             Why: an Admin removed that member, or this computer was linked to your devices and is still using its old member. \
             Fix: if this computer is one of your devices, link it again (on it: Link this computer to my devices, \
             or ligus devices request; then on your desktop: Settings > Profile > My devices > Add a computer); \
             otherwise ask an Admin for a new invite."
        ),
        Some(m) => {
            let needed = ligus_core::authorization::min_role_for(payload);
            format!(
                "Refused: your role here is {}, and {action} needs {} or above. Fix: ask an Owner or Admin to change your role (People).",
                role_name(m.role),
                role_name(needed)
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ligus_core::{ActorIdentity, ActorKind};

    fn member(id: &str, role: WorkspaceRole) -> WorkspaceMember {
        let actor = ActorIdentity::new(id, id, ActorKind::Human);
        WorkspaceMember { id: id.into(), actor, role, title: String::new(), index: 1, joined_at: ligus_core::Timestamp::now() }
    }

    fn post() -> SessionEvent {
        SessionEvent::MessageAppended { message: ligus_core::chat::ChatMessage::new(ligus_core::chat::MessageRole::User, "x", "hi") }
    }

    #[test]
    fn a_non_member_is_told_to_link_again() {
        let s = explain("role Viewer cannot perform messageAppended".into(), &post(), "zaz", "zaz", &[member("ana", WorkspaceRole::Owner)]);
        assert!(s.contains("zaz, who isn't a member"), "{s}");
        assert!(s.contains("Link this computer to my devices"), "{s}");
        assert!(!s.contains("cannot perform"), "{s}");
    }

    #[test]
    fn a_low_role_is_told_which_role_it_needs() {
        let s = explain("role Viewer cannot perform messageAppended".into(), &post(), "v", "v", &[member("v", WorkspaceRole::Viewer)]);
        assert!(s.contains("your role here is Viewer, and posting a message needs Contributor"), "{s}");
    }

    #[test]
    fn other_refusals_pass_through() {
        let s = explain("cannot remove the last owner".into(), &post(), "v", "v", &[]);
        assert_eq!(s, "cannot remove the last owner");
    }
}

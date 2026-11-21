//! Removing a relay roster entry from the workspace as well (the People
//! pane's server-side membership list).
//!
//! The relay knows a member only by its token's subject. Before the
//! workspace key is rotated, the matching **workspace member** must be gone
//! from the config log, or the rotation (sealed to every current member's
//! devices, `RosterState::rotation_recipients`) hands it the new key:
//!
//! * `ligus:<account id>` of an account on the config roster is a person:
//!   every member of that account is removed.
//! * A box's subject (`ligus-box:<random id>`, minted by an Admin's desktop
//!   under People > Remote agents) names no account. That desktop records,
//!   device-local, the **box admission** it issued with the subject
//!   (`EventStore::box_relay_admission`). The admission's redemption on the
//!   config log names the box's service member, so the box is removed by
//!   member id, never matched by name. An admission never redeemed is
//!   closed instead, so the box can't join later through it.
//! * Anything else (a box minted before the link was kept, an older
//!   `ligus:<random id>` box subject, a langstroth-auth `<tenant>:<user>`
//!   subject, a person whose membership hasn't synced here) maps to no
//!   member. The caller says so: such a box keeps getting new keys until
//!   its member is removed under Members.

use ligus_core::{workspace_config_session_id, ActorKind, SessionEvent};
use uuid::Uuid;

use super::{ChatError, ChatService, Result, RotationIssued};

/// The prefix of the relay subject an Admin's desktop mints for a box.
/// Deliberately not `ligus:`, which names a Ligus account.
pub const BOX_SUBJECT_PREFIX: &str = "ligus-box:";

/// What a relay roster entry names in this workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelaySubject {
    /// A person's account on the config roster: its member ids.
    Person { members: Vec<String> },
    /// A box this device minted the subject for, with the admission it
    /// issued alongside: the box's service member when the admission was
    /// redeemed and the box is still on the roster, and the redemption's
    /// event id when there is one.
    Box { invite: Uuid, member: Option<String>, redemption: Option<Uuid> },
    /// No workspace member this device can match. `box_like` when the
    /// subject has a box's shape (`ligus-box:`, or `ligus:` with an id that
    /// is no account here).
    Unmapped { box_like: bool },
}

/// What [`ChatService::remove_for_relay_subject`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayRemoval {
    pub target: RelaySubject,
    /// The members removed (`MemberRemoved` on the config log).
    pub removed: Vec<String>,
    /// The box admission closed (`AdmissionClosed`), if one was.
    pub closed_admission: Option<Uuid>,
}

impl RelayRemoval {
    /// Whether the entry matched no workspace member this device knows.
    pub fn unmapped(&self) -> bool {
        matches!(self.target, RelaySubject::Unmapped { .. })
    }
}

/// Why removing a relay subject from the workspace and rotating stopped.
#[derive(Debug, thiserror::Error)]
pub enum RelayRemovalError {
    /// Writing the removal failed: nothing was rotated.
    #[error("removing them from the workspace failed, so the key was not rotated: {0}")]
    Removal(ChatError),
    /// The removal was written but the rotation failed.
    #[error("{0}")]
    Rotation(ChatError),
}

impl ChatService {
    /// What relay subject `subject` names in `workspace_id` (reads only).
    pub fn relay_subject_target(&self, workspace_id: Uuid, subject: &str) -> Result<RelaySubject> {
        let config_id = workspace_config_session_id(workspace_id);
        let config = self.load(config_id)?;
        let members = config.as_ref().map(|c| c.members.as_slice()).unwrap_or_default();
        let subject = subject.trim();

        if let Some(invite) = self.store.box_relay_admission(workspace_id, subject)? {
            let redemption = self.admission_redemption(workspace_id, invite)?;
            let member = redemption.as_ref().and_then(|(_, id)| {
                members.iter().find(|m| m.id == *id && m.actor.kind == ActorKind::Service).map(|m| m.id.clone())
            });
            return Ok(RelaySubject::Box { invite, member, redemption: redemption.map(|(e, _)| e) });
        }

        if subject.starts_with(BOX_SUBJECT_PREFIX) {
            return Ok(RelaySubject::Unmapped { box_like: true });
        }
        let Some(account) = subject.strip_prefix("ligus:").and_then(|s| Uuid::parse_str(s.trim()).ok()) else {
            return Ok(RelaySubject::Unmapped { box_like: false });
        };
        let id = account.to_string();
        let ids: Vec<String> = members
            .iter()
            .filter(|m| m.actor.account_id == Some(account) || m.actor.id == id)
            .map(|m| m.id.clone())
            .collect();
        if ids.is_empty() {
            // `ligus:<random id>` was a box's subject before `ligus-box:`.
            return Ok(RelaySubject::Unmapped { box_like: true });
        }
        Ok(RelaySubject::Person { members: ids })
    }

    /// Remove what `target` names from `workspace_id`'s config log: the
    /// person's or the box's member (`MemberRemoved`, Admin+ with presence),
    /// then, for a box, close its admission (`AdmissionClosed`, naming the
    /// redemption when there is one, so it is never spent again). Writes
    /// nothing for an unmapped subject. A failed write stops here.
    pub fn remove_for_relay_subject(&mut self, workspace_id: Uuid, target: &RelaySubject) -> Result<RelayRemoval> {
        let mut out = RelayRemoval { target: target.clone(), removed: Vec::new(), closed_admission: None };
        match target {
            RelaySubject::Person { members } => {
                for id in members {
                    self.remove_member(workspace_config_session_id(workspace_id), workspace_id, id.clone())?;
                    out.removed.push(id.clone());
                }
            }
            RelaySubject::Box { invite, member, redemption } => {
                if let Some(id) = member {
                    self.remove_member(workspace_config_session_id(workspace_id), workspace_id, id.clone())?;
                    out.removed.push(id.clone());
                }
                let closed = self
                    .load(workspace_config_session_id(workspace_id))?
                    .is_some_and(|c| c.guards.closed_admissions.contains_key(invite));
                if !closed {
                    self.append_signed(
                        workspace_config_session_id(workspace_id),
                        workspace_id,
                        SessionEvent::AdmissionClosed { invite: *invite, redeemed: *redemption },
                    )?;
                    out.closed_admission = Some(*invite);
                }
            }
            RelaySubject::Unmapped { .. } => {}
        }
        Ok(out)
    }

    /// [`Self::remove_for_relay_subject`], then rotate the workspace key to
    /// whoever remains, the recipients read from the config log *after* the
    /// removal. A failed removal never rotates.
    pub fn remove_for_relay_subject_and_rotate(
        &mut self,
        workspace_id: Uuid,
        target: &RelaySubject,
    ) -> std::result::Result<(RelayRemoval, RotationIssued), RelayRemovalError> {
        let removal = self.remove_for_relay_subject(workspace_id, target).map_err(RelayRemovalError::Removal)?;
        match self.rotate_workspace_key(workspace_id) {
            Ok(issued) => Ok((removal, issued)),
            Err(e) => Err(RelayRemovalError::Rotation(e)),
        }
    }

    /// The allowed redemption of box admission `invite` on `workspace_id`'s
    /// config log, if any: its event id and the member it added (signed by
    /// that member).
    pub(crate) fn admission_redemption(&self, workspace_id: Uuid, invite: Uuid) -> Result<Option<(Uuid, String)>> {
        let envelopes = self.store.config_envelopes(workspace_id)?;
        let verdicts = ligus_core::fold_copy_decisions(
            &envelopes,
            self.trusted_founder.as_deref(),
            self.local_workspace_id,
            ligus_core::WorkspaceAuthz::Unchecked,
        );
        Ok(envelopes
            .iter()
            .find_map(|e| match &e.payload {
                SessionEvent::MemberAdded { member, invite: Some(i), .. }
                    if *i == invite
                        && verdicts.get(e) == Some(ligus_core::Verdict::Allowed)
                        && e.actor_stamp.as_ref().is_some_and(|s| s.actor.id == member.id) =>
                {
                    Some((e.event_id, member.id.clone()))
                }
                _ => None,
            }))
    }
}

//! Ids that mean one thing each.
//!
//! A relay **room** is a string, the relay's address for one workspace's
//! logs; a **workspace id** is a UUID, derived from the room
//! ([`crate::room_workspace_id`]) and stamped on every event. Both used to
//! travel as plain strings and UUIDs, and a workspace id passed where a room
//! belongs syncs to a room nobody else reads. A **turn id** is a turn's
//! placeholder reply message id: not the message that triggered the turn, and
//! not the claim key ([`crate::turn_claim_key`]) its claims are kept under.
//!
//! These newtypes are code-only: each serializes exactly as the value it
//! wraps, so no wire shape changes.
//!
//! A room is never a workspace id, and the other way round:
//!
//! ```compile_fail
//! use ligus_core::ids::{RoomName, WorkspaceId};
//! fn sync(_room: &RoomName) {}
//! let ws = WorkspaceId::from(uuid::Uuid::nil());
//! sync(&ws);
//! ```
//!
//! ```compile_fail
//! use ligus_core::ids::{RoomName, WorkspaceId};
//! fn scope(_ws: WorkspaceId) {}
//! scope(RoomName::new("room"));
//! ```
//!
//! ```compile_fail
//! use ligus_core::ids::RoomName;
//! // A UUID doesn't become a room by accident either.
//! let _room: RoomName = uuid::Uuid::nil().into();
//! ```
//!
//! The ways that do compile:
//!
//! ```
//! use ligus_core::ids::{RoomName, WorkspaceId};
//! fn sync(_room: &RoomName) {}
//! fn scope(_ws: WorkspaceId) {}
//! let room = RoomName::new("room");
//! sync(&room);
//! scope(room.workspace_id());
//! scope(WorkspaceId::from(uuid::Uuid::nil()));
//! ```

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A workspace's id: what every event of the workspace is stamped with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceId(pub Uuid);

impl WorkspaceId {
    pub fn uuid(self) -> Uuid {
        self.0
    }
}

impl From<Uuid> for WorkspaceId {
    fn from(id: Uuid) -> Self {
        Self(id)
    }
}

impl From<WorkspaceId> for Uuid {
    fn from(id: WorkspaceId) -> Self {
        id.0
    }
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A relay room: the relay's address for one workspace's logs. Only builds
/// relay URLs and names the room in messages; durable state is kept by
/// workspace id, never by this string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoomName(String);

impl RoomName {
    pub fn new(room: impl Into<String>) -> Self {
        Self(room.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The workspace this room carries ([`crate::room_workspace_id`]).
    pub fn workspace_id(&self) -> WorkspaceId {
        WorkspaceId(crate::room_workspace_id(&self.0))
    }
}

impl From<String> for RoomName {
    fn from(room: String) -> Self {
        Self(room)
    }
}

impl From<&str> for RoomName {
    fn from(room: &str) -> Self {
        Self(room.to_owned())
    }
}

impl From<&String> for RoomName {
    fn from(room: &String) -> Self {
        Self(room.clone())
    }
}

impl AsRef<str> for RoomName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for RoomName {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RoomName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A turn: its placeholder reply message id (the dispatch commit, the
/// worktree's name and the id its questions name). Not the triggering
/// message, and not the claim key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TurnId(pub Uuid);

impl TurnId {
    pub fn uuid(self) -> Uuid {
        self.0
    }
}

impl fmt::Display for TurnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_serializes_as_the_value_it_wraps() {
        let id = Uuid::from_u128(7);
        assert_eq!(serde_json::to_value(WorkspaceId(id)).unwrap(), serde_json::to_value(id).unwrap());
        assert_eq!(serde_json::to_value(TurnId(id)).unwrap(), serde_json::to_value(id).unwrap());
        assert_eq!(serde_json::to_value(RoomName::new("r1")).unwrap(), serde_json::json!("r1"));
        assert_eq!(serde_json::from_value::<RoomName>(serde_json::json!("r1")).unwrap(), RoomName::new("r1"));
    }

    #[test]
    fn a_rooms_workspace_is_the_derived_id() {
        assert_eq!(RoomName::new(" r1 ").workspace_id().uuid(), crate::room_workspace_id("r1"));
    }
}

//! Workspace guard state folded onto a session: coordinator leases (and, on
//! the workspace-config log, the workspace's guard settings). Kept in one
//! field on [`crate::ChatSession`] so the projection grows by a single,
//! defaulted member and older stored sessions deserialize unchanged.

use serde::{Deserialize, Serialize};

use crate::lease::LeaseTable;

/// Guard state carried by a projected session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardState {
    /// Coordinator leases folded on this log, keyed by `lease::lease_key`.
    #[serde(default, skip_serializing_if = "LeaseTable::is_empty")]
    pub leases: LeaseTable,
}

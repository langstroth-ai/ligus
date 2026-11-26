//! Retiring a duplicate member (identity model v2, 8.3; decision O5).
//!
//! When a computer that was already a member (its own account, `A_old`)
//! links as its person (`A_new`), it retires `A_old` itself at redeem: a
//! `MemberRetired` on the config log carrying `A_old`'s **account-key**
//! signature over the workspace, both accounts and the linked device. Under
//! rules 3 it removes `A_old` like a self-leave (no cut: its history stays
//! its own). Under rules 2 it is refused (0.3 peers fold it as `Unknown`; no
//! state changes on either build).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::crypto::{verify, SigningKeypair};

/// FROZEN signature context.
pub const RETIRE_TAG: &[u8] = b"ligus-member-retire-v1\0";

/// What `MemberRetired` carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberRetirement {
    /// `A_old`'s member (and actor) id.
    pub member_id: String,
    /// `A_old`.
    pub account_id: Uuid,
    /// `A_new`: the person the computer now acts for.
    pub successor_account_id: Uuid,
    /// The linked computer: certified for `A_new`, and its device accepted it.
    pub device_id: Uuid,
    /// `A_old`'s account key over [`retirement_preimage`].
    pub signature: Vec<u8>,
}

/// The bytes `A_old`'s account key signs.
pub fn retirement_preimage(workspace_id: Uuid, old: Uuid, new: Uuid, device: Uuid) -> Vec<u8> {
    let mut out = Vec::with_capacity(RETIRE_TAG.len() + 64);
    out.extend_from_slice(RETIRE_TAG);
    out.extend_from_slice(workspace_id.as_bytes());
    out.extend_from_slice(old.as_bytes());
    out.extend_from_slice(new.as_bytes());
    out.extend_from_slice(device.as_bytes());
    out
}

impl MemberRetirement {
    /// `A_old` (holding `old_account`) retires `member_id` into `new`, for
    /// the linked `device` in `workspace_id`.
    pub fn sign(old_account: &SigningKeypair, workspace_id: Uuid, member_id: &str, old: Uuid, new: Uuid, device: Uuid) -> Self {
        let signature = old_account.sign(&retirement_preimage(workspace_id, old, new, device)).to_vec();
        Self { member_id: member_id.to_string(), account_id: old, successor_account_id: new, device_id: device, signature }
    }

    /// Whether the account signature verifies under `old_key` for `workspace_id`.
    pub fn verifies(&self, workspace_id: Uuid, old_key: &[u8]) -> bool {
        verify(old_key, &retirement_preimage(workspace_id, self.account_id, self.successor_account_id, self.device_id), &self.signature).is_ok()
    }
}

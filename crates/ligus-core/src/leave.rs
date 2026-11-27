//! A member leaving on its own: `MemberLeft` (workspace model v3, 10.4;
//! decision D10).
//!
//! A person leaves from a non-delegated, non-phone device of their account,
//! with a passkey when the account holds one; a box leaves with `ligus serve
//! leave`, signed by its own device key. Never the founder, and never the
//! last Owner: two Owners leaving at once resolve in canonical order and the
//! second is refused ("Couldn't leave: you're the last Owner").
//!
//! Under rules 3 a leave is **display only**: the member is shown as "asked
//! to leave" (with Remove for Admins), keeps writing and keeps receiving key
//! rotations, so the roster, every other verdict and the rotation set are the
//! same with and without it. Under rules 4 it is a removal (G4), judged like
//! a self-removal; that rule lands with rules 4. Builds before 0.5 read the
//! kind as `Unknown` and fold it as a no-op.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::time_util::Timestamp;

/// A folded leave: who asked, when (the envelope's time), from which device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Left {
    pub account_id: Option<Uuid>,
    pub at: Timestamp,
    pub event_id: Uuid,
    pub device_id: Option<Uuid>,
}

/// The refusal when the leaver is the last Owner (or would be, after an
/// earlier leave).
pub const LAST_OWNER: &str = "Couldn't leave: you're the last Owner. Make someone else an Owner first";

/// The refusal for the founder.
pub const FOUNDER: &str = "Couldn't leave: the founder stays in the workspace";

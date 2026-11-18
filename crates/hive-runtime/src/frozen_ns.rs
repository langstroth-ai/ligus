//! Frozen UUIDv5 namespaces from 1.x that 0.1 no longer uses.
//!
//! 1.x derived two ids from public identifiers: the account id from a GitHub
//! numeric user id, and a DM room from the two participants' GitHub ids. 0.1
//! derives neither: an account id is the random UUIDv4 the identity store
//! makes on first launch, and a DM room and its invite id are random (see
//! [`crate::dm`]). The namespaces stay here, unused, only so nobody mistakes
//! them for free values and reuses them with another meaning. Never derive
//! anything from them in new code; `tests/no_public_id_derivation.rs`
//! enforces that no `new_v5(` call site uses a public identifier.

use uuid::Uuid;

/// 1.x: `account_id = UUIDv5(ACCOUNT_NS, "github:<numeric id>")`. ASCII
/// "hivegithaccount\x01". Frozen; unused.
pub const ACCOUNT_NS: Uuid = Uuid::from_u128(0x6869_7665_6769_7468_6163_636f_756e_7401);

/// 1.x: `room = "dm-" + UUIDv5(DM_NS, "<lo>\0<hi>")` over two GitHub ids.
/// ASCII "hivedmroompair01". Frozen; unused.
pub const DM_NS: Uuid = Uuid::from_u128(0x6869_7665_646d_726f_6f6d_7061_6972_3031);

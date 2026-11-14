//! Relay-directory trust for invites by GitHub handle and DM keys (Track B
//! residual 7).
//!
//! The relay directory lists each GitHub account's devices with their X25519
//! key-agreement keys, unsigned: a hostile relay can add a device of its own
//! and read whatever is sealed to the list (an invite code carries every
//! authorized epoch key). Before sealing, the desktop checks each listed key
//! against what the invitee's account already published, signed, on a
//! workspace config log this device holds: a `DeviceKeyAgreementPublished`
//! signed by the device it names, certified by the account key and not
//! revoked (the same form rotations seal to, B1). Verified keys are the only
//! recipients when there are any; with none, the inviter must confirm an
//! "unverified recipient" warning first. Signed directory records are
//! deferred to a relay change (see the threat model).

use std::collections::BTreeSet;

use uuid::Uuid;

use crate::relay_client::DirectoryDevice;
use crate::EventStore;

/// The start of the error a sealing command returns when the recipient's
/// keys are vouched for by the relay alone and the inviter hasn't confirmed.
pub const UNVERIFIED_RECIPIENT: &str = "unverified recipient";

/// Key-agreement keys `account` published on the config logs this device
/// holds with a founder pin, each by a non-revoked device certified for that
/// account.
pub fn signed_ka_keys(store: &EventStore, account: Uuid) -> BTreeSet<[u8; 32]> {
    let mut out = BTreeSet::new();
    for ws in store.workspaces_with_ka_publications().unwrap_or_default() {
        let Ok(Some(pin)) = store.workspace_pin(ws) else { continue };
        let Ok((Some(_), timeline)) = store.load_config(ws, Some(&pin.founder_account.to_string()), None) else { continue };
        let head = timeline.current();
        for (device, key) in &head.ka_keys {
            let vouched = head.devices.get(device).is_some_and(|d| !d.revoked && d.accounts.contains(&account));
            if let (true, Ok(k)) = (vouched, <[u8; 32]>::try_from(key.as_slice())) {
                out.insert(k);
            }
        }
    }
    out
}

/// One directory device to seal to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    pub device_id: String,
    pub ka_public: [u8; 32],
}

/// Who an invite or DM code is sealed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealPlan {
    pub recipients: Vec<Recipient>,
    /// The recipients are keys the invitee's account signed (else the relay
    /// alone vouches for them, and the inviter confirmed).
    pub verified: bool,
}

/// The devices to seal to: only the verified ones when any listed key is
/// verified; otherwise every listed device, but only when the inviter
/// `confirmed` the warning (else `Err` starting with
/// [`UNVERIFIED_RECIPIENT`]). Malformed keys are skipped.
pub fn plan(devices: &[DirectoryDevice], signed: &BTreeSet<[u8; 32]>, confirmed: bool) -> Result<SealPlan, String> {
    let listed: Vec<Recipient> = devices
        .iter()
        .filter_map(|d| {
            Some(Recipient { device_id: d.device_id.clone(), ka_public: hex32(d.ka_public.trim())? })
        })
        .collect();
    let verified: Vec<Recipient> = listed.iter().filter(|r| signed.contains(&r.ka_public)).cloned().collect();
    if !verified.is_empty() {
        return Ok(SealPlan { recipients: verified, verified: true });
    }
    if listed.is_empty() || confirmed {
        return Ok(SealPlan { recipients: listed, verified: false });
    }
    Err(format!(
        "{UNVERIFIED_RECIPIENT}: only the relay vouches for {} device key{}; nothing this device holds confirms they are theirs",
        listed.len(),
        if listed.len() == 1 { "" } else { "s" }
    ))
}

/// 64 hex characters as 32 bytes.
fn hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

//! Co-member trust: whose keys this device can seal to without asking a
//! server.
//!
//! Anyone who shares a workspace with you is already discoverable, with
//! verified keys: each of their devices published its X25519 key-agreement
//! key in a `DeviceKeyAgreementPublished` signed by that device, whose
//! certificate chains to their account key, on a config log this device
//! holds with a founder pin (the same form rotations seal to, B1). Inviting a
//! co-member to another workspace, or opening a DM with one, needs no
//! directory: the invite is sealed to those keys. There is no relay
//! directory in 2.0 and no lookup by handle; a display name is never an
//! identifier (two co-members may share one), so everything here is keyed by
//! the random account id and shown with the account's safety code.
//!
//! A key from anywhere else (a contact record matched without an
//! out-of-band safety code, for example) is unverified: [`plan`] seals to it
//! only after the inviter confirms.

use std::collections::{BTreeMap, BTreeSet};

use hive_core::identity::ActorKind;
use uuid::Uuid;

use crate::envelope_verifier::{build_roster_pinned, FounderPin};
use crate::EventStore;

/// The start of the error a sealing command returns when the recipient's
/// keys are vouched for by nothing this device holds and the inviter hasn't
/// confirmed.
pub const UNVERIFIED_RECIPIENT: &str = "unverified recipient";

/// Key-agreement keys `account` published on the config logs this device
/// holds with a founder pin, each by a non-revoked device certified for that
/// account.
pub fn signed_ka_keys(store: &EventStore, account: Uuid) -> BTreeSet<[u8; 32]> {
    verified_recipients(store, account).into_iter().map(|r| r.ka_public).collect()
}

/// [`signed_ka_keys`] with each key's device id.
pub fn verified_recipients(store: &EventStore, account: Uuid) -> Vec<Recipient> {
    let mut out: BTreeMap<[u8; 32], String> = BTreeMap::new();
    for ws in store.workspaces_with_ka_publications().unwrap_or_default() {
        let Ok(Some(pin)) = store.workspace_pin(ws) else { continue };
        let Ok((Some(_), timeline)) = store.load_config(ws, Some(&pin.founder_account.to_string()), None) else { continue };
        let head = timeline.current();
        for (device, key) in &head.ka_keys {
            let vouched = head.devices.get(device).is_some_and(|d| !d.revoked && d.accounts.contains(&account));
            if let (true, Ok(k)) = (vouched, <[u8; 32]>::try_from(key.as_slice())) {
                out.entry(k).or_insert_with(|| device.to_string());
            }
        }
    }
    out.into_iter().map(|(ka_public, device_id)| Recipient { device_id, ka_public }).collect()
}

/// One device to seal to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    pub device_id: String,
    pub ka_public: [u8; 32],
}

/// Who an invite or DM code is sealed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealPlan {
    pub recipients: Vec<Recipient>,
    /// The recipients are keys the invitee's account signed (else nothing
    /// this device holds vouches for them, and the inviter confirmed).
    pub verified: bool,
}

/// The devices to seal to, from `candidates` (keys from a source this device
/// can't check by itself): only the verified ones when any candidate is in
/// `signed`; otherwise every candidate, but only when the inviter
/// `confirmed` (else `Err` starting with [`UNVERIFIED_RECIPIENT`]).
pub fn plan(candidates: &[Recipient], signed: &BTreeSet<[u8; 32]>, confirmed: bool) -> Result<SealPlan, String> {
    let verified: Vec<Recipient> = candidates.iter().filter(|r| signed.contains(&r.ka_public)).cloned().collect();
    if !verified.is_empty() {
        return Ok(SealPlan { recipients: verified, verified: true });
    }
    if candidates.is_empty() || confirmed {
        return Ok(SealPlan { recipients: candidates.to_vec(), verified: false });
    }
    Err(format!(
        "{UNVERIFIED_RECIPIENT}: nothing this device holds confirms {} device key{} belong{} to them",
        candidates.len(),
        if candidates.len() == 1 { "" } else { "s" },
        if candidates.len() == 1 { "s" } else { "" },
    ))
}

/// Someone this device shares at least one pinned workspace with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoMember {
    /// The random account id (never derived from anything public).
    pub account_id: Uuid,
    /// The name they chose. Not unique and never an identifier.
    pub display_name: String,
    pub avatar_url: Option<String>,
    /// Hex of the first 16 bytes of SHA-256 of their account signing key, as
    /// the config logs register it (the `fk` an invite carries). `None` when
    /// no log registers it undisputed, or two logs disagree.
    pub fingerprint: Option<String>,
    /// Two of the logs this device holds register different keys for this
    /// account, or one disputes it: sealing to it is refused.
    pub key_conflict: bool,
    /// Their devices' key-agreement keys, verified on the config logs.
    pub devices: Vec<Recipient>,
    /// Devices certified for them that a shared log revoked (an invite
    /// signed by one of these is not from them).
    pub revoked_devices: Vec<Uuid>,
    /// The shared workspaces.
    pub workspaces: Vec<Uuid>,
    /// Their contact id (relay v0.3 drop box) as their roster entry or latest
    /// event stamp carries it, and the workspace it was seen in (deliver
    /// through that workspace's relay).
    pub contact: Option<(String, Uuid)>,
}

impl CoMember {
    /// The account's safety code ([`safety_code`] of [`Self::fingerprint`]).
    pub fn safety_code(&self) -> Option<String> {
        self.fingerprint.as_deref().and_then(safety_code)
    }

    /// The devices an invite to this co-member is sealed to: their verified
    /// keys, or an error when there are none or their account key is in
    /// conflict.
    pub fn seal_plan(&self) -> Result<SealPlan, String> {
        if self.key_conflict || self.fingerprint.is_none() {
            return Err(format!(
                "{UNVERIFIED_RECIPIENT}: the workspaces you share don't agree on {}'s account key",
                self.display_name
            ));
        }
        if self.devices.is_empty() {
            return Err(format!(
                "{UNVERIFIED_RECIPIENT}: none of {}'s devices has published a key yet; send the link instead",
                self.display_name
            ));
        }
        Ok(SealPlan { recipients: self.devices.clone(), verified: true })
    }
}

/// Every human co-member of the pinned workspaces this device holds, except
/// `me`, by account id.
pub fn co_members(store: &EventStore, me: Uuid) -> Vec<CoMember> {
    let mut by_account: BTreeMap<Uuid, CoMember> = BTreeMap::new();
    let mut fks: BTreeMap<Uuid, BTreeSet<String>> = BTreeMap::new();
    for ws in store.pinned_workspaces().unwrap_or_default() {
        let Ok(Some(pin)) = store.workspace_pin(ws) else { continue };
        let Ok((Some(config), timeline)) = store.load_config(ws, Some(&pin.founder_account.to_string()), None) else {
            continue;
        };
        let head = timeline.current();
        let roster = build_roster_pinned(&store.trust_envelopes(ws).unwrap_or_default(), Some(&pin));
        for m in head.members.values() {
            let (Some(account), ActorKind::Human) = (m.account_id, m.kind) else { continue };
            if account == me {
                continue;
            }
            let entry = by_account.entry(account).or_insert_with(|| CoMember {
                account_id: account,
                display_name: String::new(),
                avatar_url: None,
                fingerprint: None,
                key_conflict: false,
                devices: Vec::new(),
                revoked_devices: Vec::new(),
                workspaces: Vec::new(),
                contact: None,
            });
            if !entry.workspaces.contains(&ws) {
                entry.workspaces.push(ws);
            }
            if let Some(member) = config.members.iter().find(|x| x.actor.account_id == Some(account)) {
                if entry.display_name.trim().is_empty() {
                    entry.display_name = member.actor.display_name.clone();
                }
                if entry.avatar_url.is_none() {
                    entry.avatar_url = member.actor.avatar_url.clone();
                }
            }
            if entry.contact.is_none() {
                let cid = config
                    .members
                    .iter()
                    .find(|x| x.actor.account_id == Some(account))
                    .and_then(|x| x.actor.contact.clone())
                    .or_else(|| store.latest_contact_of(ws, account).ok().flatten())
                    .filter(|c| crate::contacts::valid_cid(c));
                entry.contact = cid.map(|c| (c, ws));
            }
            if roster.is_disputed(account) {
                entry.key_conflict = true;
            } else if let Some(key) = roster.account_signing_key(account) {
                fks.entry(account).or_default().insert(FounderPin::fingerprint_hex(key));
            }
            for (device, d) in &head.devices {
                if d.revoked && !entry.revoked_devices.contains(device) {
                    entry.revoked_devices.push(*device);
                }
            }
            for (device, key) in &head.ka_keys {
                let vouched = head.devices.get(device).is_some_and(|d| !d.revoked && d.accounts.contains(&account));
                let Ok(k) = <[u8; 32]>::try_from(key.as_slice()) else { continue };
                if vouched && !entry.devices.iter().any(|r| r.ka_public == k) {
                    entry.devices.push(Recipient { device_id: device.to_string(), ka_public: k });
                }
            }
        }
    }
    for (account, set) in fks {
        if let Some(c) = by_account.get_mut(&account) {
            if set.len() == 1 && !c.key_conflict {
                c.fingerprint = set.into_iter().next();
            } else {
                c.key_conflict = true;
            }
        }
    }
    by_account.into_values().collect()
}

/// The safety code people compare out of band: the account fingerprint (32
/// hex characters, the existing `fk`) as eight groups of four. `None` for
/// anything that isn't 32 hex characters.
pub fn safety_code(fingerprint_hex: &str) -> Option<String> {
    let f = fingerprint_hex.trim().to_ascii_lowercase();
    if f.len() != 32 || !f.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(f.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap_or_default()).collect::<Vec<_>>().join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safety_code_groups_the_fingerprint() {
        assert_eq!(
            safety_code("00112233445566778899AABBCCDDEEFF").as_deref(),
            Some("0011 2233 4455 6677 8899 aabb ccdd eeff")
        );
        assert_eq!(safety_code("0011"), None);
        assert_eq!(safety_code("zz112233445566778899aabbccddeeff"), None);
    }

    #[test]
    fn plan_without_verified_keys_needs_confirmation() {
        let r = Recipient { device_id: "d".into(), ka_public: [7; 32] };
        let err = plan(std::slice::from_ref(&r), &BTreeSet::new(), false).unwrap_err();
        assert!(err.starts_with(UNVERIFIED_RECIPIENT), "{err}");
        assert!(!plan(&[r], &BTreeSet::new(), true).unwrap().verified);
    }
}

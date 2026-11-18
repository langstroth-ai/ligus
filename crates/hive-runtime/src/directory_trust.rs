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
//! directory in 0.1 and no lookup by handle; a display name is never an
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
/// account, on a log whose key for the account is the one this device
/// trusts for it ([`account_keys`]).
pub fn signed_ka_keys(store: &EventStore, account: Uuid) -> BTreeSet<[u8; 32]> {
    verified_recipients(store, account).into_iter().map(|r| r.ka_public).collect()
}

/// [`signed_ka_keys`] with each key's device id.
pub fn verified_recipients(store: &EventStore, account: Uuid) -> Vec<Recipient> {
    let logs = pinned_logs(store);
    let keys = account_keys(&logs);
    let mut out: BTreeMap<[u8; 32], String> = BTreeMap::new();
    for log in &logs {
        if !keys.get(&account).is_some_and(|k| k.agrees(log, account)) {
            continue;
        }
        let head = log.timeline.current();
        for (device, key) in &head.ka_keys {
            let vouched = head.devices.get(device).is_some_and(|d| !d.revoked && d.accounts.contains(&account));
            if let (true, Ok(k)) = (vouched, <[u8; 32]>::try_from(key.as_slice())) {
                out.entry(k).or_insert_with(|| device.to_string());
            }
        }
    }
    out.into_iter().map(|(ka_public, device_id)| Recipient { device_id, ka_public }).collect()
}

/// One pinned config log this device holds, folded with its pin.
pub struct PinnedLog {
    pub ws: Uuid,
    pub pin: FounderPin,
    pub config: hive_core::ChatSession,
    pub timeline: std::sync::Arc<hive_core::authz::ConfigTimeline>,
    pub roster: crate::envelope_verifier::WorkspaceRoster,
}

impl PinnedLog {
    /// The fingerprint this log registers for `account`; `Err` when the log
    /// itself disputes it (two keys registered there).
    fn fingerprint_of(&self, account: Uuid) -> Result<Option<String>, ()> {
        if self.roster.is_disputed(account) {
            return Err(());
        }
        Ok(self.roster.account_signing_key(account).map(FounderPin::fingerprint_hex))
    }
}

/// Every pinned log, in the order this device pinned them (first joined
/// first), each with a verified founding.
pub fn pinned_logs(store: &EventStore) -> Vec<PinnedLog> {
    let mut out = Vec::new();
    for ws in store.pinned_workspaces().unwrap_or_default() {
        let Ok(Some(pin)) = store.workspace_pin(ws) else { continue };
        let Ok((Some(config), timeline)) = store.load_config(ws, Some(&pin.founder_account.to_string()), None) else {
            continue;
        };
        let roster = build_roster_pinned(&store.trust_envelopes(ws).unwrap_or_default(), Some(&pin));
        out.push(PinnedLog { ws, pin, config, timeline, roster });
    }
    out
}

/// The account key this device trusts for one co-member (S1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountKey {
    /// This fingerprint. From the founder pin of a workspace the account
    /// founded (out of band, from its signed invite) when there is one;
    /// otherwise from the earliest-pinned log that registers a key for it
    /// undisputed.
    Trusted(String),
    /// Every log that registers a key for it disputes it.
    Conflict,
}

impl AccountKey {
    /// Whether `log` registers this key for `account` (a log registering
    /// another, or disputing it, contributes nothing about the account).
    pub fn agrees(&self, log: &PinnedLog, account: Uuid) -> bool {
        match self {
            AccountKey::Trusted(fk) => log.fingerprint_of(account) == Ok(Some(fk.clone())),
            AccountKey::Conflict => false,
        }
    }
}

/// Which key this device trusts for each account the pinned logs register.
///
/// A single log can register any key under any account id (whoever founds a
/// workspace can add an account and a key and device for it), so no one log
/// may override what the others say, nor mark the account conflicted by
/// itself. The trusted key is, in order: the fingerprint a founder pin names
/// for its founder (the earliest such pin), else the key of the
/// earliest-pinned log that registers one undisputed. A later log that
/// registers another key is ignored for that account. Only when every log
/// that registers a key disputes it is the account in conflict.
pub fn account_keys(logs: &[PinnedLog]) -> BTreeMap<Uuid, AccountKey> {
    let mut out: BTreeMap<Uuid, AccountKey> = BTreeMap::new();
    for log in logs {
        let fk: String = log.pin.fk.iter().map(|b| format!("{b:02x}")).collect();
        out.entry(log.pin.founder_account).or_insert(AccountKey::Trusted(fk));
    }
    for log in logs {
        for m in log.timeline.current().members.values() {
            let Some(account) = m.account_id else { continue };
            match (out.get(&account), log.fingerprint_of(account)) {
                (Some(AccountKey::Trusted(_)), _) | (_, Ok(None)) => {}
                (_, Ok(Some(fk))) => {
                    out.insert(account, AccountKey::Trusted(fk));
                }
                (None, Err(())) => {
                    out.insert(account, AccountKey::Conflict);
                }
                (Some(AccountKey::Conflict), Err(())) => {}
            }
        }
    }
    out
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
    /// The name they chose, cleaned for display
    /// ([`hive_proto::names::clean_display_name`]). Not unique and never an
    /// identifier.
    pub display_name: String,
    /// Another co-member's name looks like this one (S3): show their safety
    /// codes side by side.
    pub look_alike: bool,
    pub avatar_url: Option<String>,
    /// Hex of the first 16 bytes of SHA-256 of their account signing key, as
    /// the config logs register it (the `fk` an invite carries). `None` when
    /// no log registers it undisputed, or two logs disagree.
    pub fingerprint: Option<String>,
    /// Every log this device holds that registers a key for this account
    /// disputes it: sealing to it is refused. (One log disagreeing with the
    /// trusted key is [`Self::key_disputed_in`], not a conflict.)
    pub key_conflict: bool,
    /// Their devices' key-agreement keys, verified on the config logs.
    pub devices: Vec<Recipient>,
    /// Devices certified for them that a shared log revoked (an invite
    /// signed by one of these is not from them).
    pub revoked_devices: Vec<Uuid>,
    /// The shared workspaces (those whose log registers the key this device
    /// trusts for them).
    pub workspaces: Vec<Uuid>,
    /// Workspaces whose log registers a different key under their account id
    /// (S1): someone there claims to be them. Nothing from those logs is
    /// used for them.
    pub key_disputed_in: Vec<Uuid>,
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
/// `me`, by account id. What a log says about an account (name, devices,
/// contact) counts only when that log registers the key this device trusts
/// for the account ([`account_keys`]); the others are listed in
/// [`CoMember::key_disputed_in`].
pub fn co_members(store: &EventStore, me: Uuid) -> Vec<CoMember> {
    let logs = pinned_logs(store);
    let keys = account_keys(&logs);
    let mut by_account: BTreeMap<Uuid, CoMember> = BTreeMap::new();
    for log in &logs {
        let (ws, config, head) = (log.ws, &log.config, log.timeline.current());
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
                key_disputed_in: Vec::new(),
                look_alike: false,
                contact: None,
            });
            match keys.get(&account) {
                Some(AccountKey::Conflict) => entry.key_conflict = true,
                Some(k @ AccountKey::Trusted(fk)) => {
                    if !k.agrees(log, account) {
                        // This log registers another key under their id (or
                        // none, or disputes it): nothing from it is theirs.
                        if log.fingerprint_of(account) != Ok(None) && !entry.key_disputed_in.contains(&ws) {
                            entry.key_disputed_in.push(ws);
                        }
                        continue;
                    }
                    entry.fingerprint = Some(fk.clone());
                }
                None => {}
            }
            if !entry.workspaces.contains(&ws) {
                entry.workspaces.push(ws);
            }
            if let Some(member) = config.members.iter().find(|x| x.actor.account_id == Some(account)) {
                if entry.display_name.is_empty() {
                    // Shown as cleaned: no bidi overrides, invisible or
                    // control characters (S3).
                    entry.display_name = hive_proto::names::clean_display_name(&member.actor.display_name);
                }
                if entry.avatar_url.is_none() {
                    entry.avatar_url = member.actor.avatar_url.clone();
                }
            }
            if entry.contact.is_none() {
                // What their own certified, non-revoked devices stamped first
                // (anyone can stamp any account on what they sign; an Admin
                // writes the roster entry), then the roster entry.
                let signer_key = |device: Uuid| {
                    head.devices
                        .get(&device)
                        .filter(|d| !d.revoked && d.accounts.contains(&account))
                        .and_then(|d| d.keys_for(Some(account)).first().map(|k| k.to_vec()))
                };
                let cid = store
                    .latest_contact_of(ws, account, &signer_key)
                    .ok()
                    .flatten()
                    .or_else(|| {
                        config.members.iter().find(|x| x.actor.account_id == Some(account)).and_then(|x| x.actor.contact.clone())
                    })
                    .filter(|c| crate::contacts::valid_cid(c));
                entry.contact = cid.map(|c| (c, ws));
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
    let mut out: Vec<CoMember> = by_account.into_values().filter(|c| !c.workspaces.is_empty() || c.key_conflict).collect();
    // Two different accounts whose names look alike (same skeleton: case,
    // look-alike letters from other scripts, invisible characters) are both
    // flagged: their safety codes tell them apart.
    let mut by_skeleton: BTreeMap<String, usize> = BTreeMap::new();
    for c in &out {
        *by_skeleton.entry(hive_proto::names::name_skeleton(&c.display_name)).or_default() += 1;
    }
    for c in &mut out {
        let sk = hive_proto::names::name_skeleton(&c.display_name);
        c.look_alike = !sk.is_empty() && by_skeleton.get(&sk).is_some_and(|n| *n > 1);
    }
    out
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

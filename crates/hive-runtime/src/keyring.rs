//! The one workspace keyring (Phase 0, B1): which keys a device reads relay
//! bodies with and which single key it seals new ones under. The desktop,
//! `ligus serve`, `ligus worker` and the CLI all build it the same way, from
//! the folded config log ([`ConfigTimeline`]) plus what the device holds.
//!
//! * **Seal** under the newest *authorized* rotation (a signed
//!   `WorkspaceKeyRotated` an Admin issued at the head of the config log),
//!   and only with a key whose commitment matches it. With no authorized
//!   rotation the passphrase key (epoch 0) seals. A device that isn't a
//!   recipient of the authorized head doesn't seal at all: an older key
//!   would be refused by every peer (the old-epoch write rule).
//! * **Read** with every key this device can open: authorized rotations,
//!   rotations that lost a tie, rotation bodies from the relay keyring route,
//!   keys an invite delivered, and the passphrase key. Reading with a key that
//!   was never authorized is harmless: every event it opens is still signature-
//!   and authorization-checked by the fold.
//!
//! Unsigned (pre-2.0) `WorkspaceKeyRotation` rows from the relay keyring are
//! ignored entirely: 2.0 starts from scratch, so there is nothing legitimate
//! to read with them, and nothing is ever sealed under them.

use std::collections::BTreeMap;

use hive_core::authz::ConfigTimeline;
use hive_core::e2ee::{open as open_blob, KeyAgreementKeypair};
use hive_core::keys::{key_commitment, kid_of, SignedRotation};
use serde_json::Value;
use uuid::Uuid;

/// What a device holds besides the config log.
#[derive(Default)]
pub struct KeySource {
    /// This device's id (the recipient key of a rotation's `sealed` map).
    pub device_id: Option<Uuid>,
    /// This device's X25519 key-agreement keypair.
    pub ka: Option<KeyAgreementKeypair>,
    /// The passphrase-derived epoch-0 key.
    pub passphrase: Option<[u8; 32]>,
    /// Epoch keys delivered out of band (an invite or pairing payload):
    /// version → key. Read-only until a matching authorized commitment exists.
    pub delivered: Vec<(u32, [u8; 32])>,
}

impl KeySource {
    pub fn passphrase_only(key: [u8; 32]) -> Self {
        Self { passphrase: Some(key), ..Default::default() }
    }

    /// What every host (desktop, serve, worker, CLI) holds, built one way.
    pub fn for_device(
        device_id: Uuid,
        ka: KeyAgreementKeypair,
        passphrase: Option<&str>,
        delivered: &[DeliveredKey],
    ) -> Self {
        Self {
            device_id: Some(device_id),
            ka: Some(ka),
            passphrase: passphrase.filter(|p| !p.is_empty()).map(hive_core::derive_workspace_key),
            delivered: delivered.iter().filter_map(|d| d.key().map(|k| (d.v, k))).collect(),
        }
    }
}

/// An epoch key delivered out of band (an invite or pairing payload carries
/// every authorized epoch key the inviter holds, so a newcomer can read the
/// history and seal at the head before any rotation names its device).
/// Serialized as `{"v":3,"k":"<64 hex>"}`. A bearer secret, like the
/// passphrase it travels with.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeliveredKey {
    pub v: u32,
    pub k: String,
}

impl DeliveredKey {
    pub fn new(v: u32, key: &[u8; 32]) -> Self {
        let k = hive_core::keys::hex(key);
        crate::secret_redact::register(&k);
        Self { v, k }
    }

    pub fn key(&self) -> Option<[u8; 32]> {
        let s = self.k.trim();
        if s.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
        }
        crate::secret_redact::register(s);
        Some(out)
    }
}

/// The key a device seals new relay bodies under.
#[derive(Clone, PartialEq, Eq)]
pub struct SealKey {
    pub version: u32,
    pub kid: String,
    pub key: [u8; 32],
}

impl std::fmt::Debug for SealKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealKey").field("version", &self.version).field("kid", &self.kid).finish()
    }
}

/// Why a device can't seal, when it can't.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealStatus {
    /// Sealing under an authorized rotation (or the passphrase epoch 0 when
    /// none exists yet).
    Ready,
    /// The workspace has an authorized rotation this device can't open (it
    /// isn't a recipient yet, or was removed). It reads what it can and
    /// writes nothing until an Admin rotates to it.
    NotARecipient { version: u32 },
    /// No key at all.
    NoKey,
}

/// The keyring (see the module docs).
#[derive(Clone, Default)]
pub struct KeyringEngine {
    workspace_id: Uuid,
    /// (version, kid) → key: everything this device can open.
    read: BTreeMap<(u32, String), [u8; 32]>,
    seal: Option<SealKey>,
    /// The authorized head version (0 when there is no rotation yet).
    head: u32,
    /// Rotations this device can open that are not authorized and are at or
    /// below the head: a tie lost, or a stale issuer. (version, kid).
    lost: Vec<(u32, String)>,
    /// The authorized key id of each version (0 = the passphrase key).
    authorized: BTreeMap<u32, String>,
}

impl std::fmt::Debug for KeyringEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyringEngine")
            .field("workspace_id", &self.workspace_id)
            .field("read", &self.read.keys().collect::<Vec<_>>())
            .field("seal", &self.seal)
            .field("head", &self.head)
            .finish()
    }
}

impl KeyringEngine {
    /// Build the ring for `workspace_id` from its folded config log (`None`
    /// while it hasn't synced), what this device holds, and the raw rows of
    /// the relay keyring route (rotation bodies; anything else is ignored).
    pub fn build(workspace_id: Uuid, cfg: Option<&ConfigTimeline>, src: &KeySource, relay_rows: &[Value]) -> Self {
        let mut read: BTreeMap<(u32, String), [u8; 32]> = BTreeMap::new();
        let mut add = |version: u32, key: [u8; 32]| {
            read.insert((version, kid_of(&key_commitment(workspace_id, version, &key))), key);
        };
        if let Some(k) = src.passphrase {
            add(0, k);
        }
        for (v, k) in &src.delivered {
            add(*v, *k);
        }
        let mut bodies: Vec<SignedRotation> = cfg.map(|c| c.rotation_candidates().to_vec()).unwrap_or_default();
        bodies.extend(relay_rows.iter().filter_map(rotation_of_row));
        for r in &bodies {
            if let Some(k) = open_rotation(workspace_id, r, src) {
                add(r.version, k);
            }
        }

        let epochs = cfg.map(|c| c.current().key_epochs.clone()).unwrap_or_default();
        let head = epochs.head_version();
        let seal = match epochs.head() {
            Some(e) => read.get(&(e.version, e.kid())).map(|k| SealKey { version: e.version, kid: e.kid(), key: *k }),
            None => src.passphrase.map(|k| SealKey {
                version: 0,
                kid: kid_of(&key_commitment(workspace_id, 0, &k)),
                key: k,
            }),
        };
        let lost = read
            .keys()
            .filter(|(v, kid)| *v > 0 && *v <= head && epochs.get(*v).is_none_or(|e| e.kid() != *kid))
            .cloned()
            .collect();
        let mut authorized: BTreeMap<u32, String> = epochs.epochs.iter().map(|e| (e.version, e.kid())).collect();
        if let Some(k) = src.passphrase {
            authorized.insert(0, kid_of(&key_commitment(workspace_id, 0, &k)));
        }
        Self { workspace_id, read, seal, head, lost, authorized }
    }

    /// The key a `WorkspaceKeyRotated` body is sealed under: the epoch it
    /// follows (`prev_version`), so every recipient of that epoch can read the
    /// rotation and learn the new key. Falls back to the seal key.
    pub fn seal_key_for_rotation(&self, rotation: &SignedRotation) -> Option<SealKey> {
        let prev = rotation.prev_version;
        self.authorized
            .get(&prev)
            .and_then(|kid| self.read.get(&(prev, kid.clone())).map(|k| SealKey { version: prev, kid: kid.clone(), key: *k }))
            .or_else(|| self.seal.clone())
    }

    /// A ring with only the passphrase key (the solo, local-only case).
    pub fn passphrase(workspace_id: Uuid, key: [u8; 32]) -> Self {
        Self::build(workspace_id, None, &KeySource::passphrase_only(key), &[])
    }

    pub fn workspace_id(&self) -> Uuid {
        self.workspace_id
    }

    /// The key new bodies are sealed under, if this device may seal.
    pub fn seal_key(&self) -> Option<&SealKey> {
        self.seal.as_ref()
    }

    pub fn seal_status(&self) -> SealStatus {
        match (&self.seal, self.head) {
            (Some(_), _) => SealStatus::Ready,
            (None, 0) => SealStatus::NoKey,
            (None, v) => SealStatus::NotARecipient { version: v },
        }
    }

    /// The authorized head version (0 = none).
    pub fn authorized_head(&self) -> u32 {
        self.head
    }

    /// Whether any key is held (a keyless ring never pushes plaintext).
    pub fn is_empty(&self) -> bool {
        self.read.is_empty()
    }

    /// Keys to try for a body sealed at `version` (by `kid` when it names one).
    pub fn keys_for(&self, version: u32, kid: Option<&str>) -> Vec<&[u8; 32]> {
        match kid {
            Some(kid) => self.read.get(&(version, kid.to_string())).into_iter().collect(),
            None => self.read.range((version, String::new())..).take_while(|((v, _), _)| *v == version).map(|(_, k)| k).collect(),
        }
    }

    /// Every (version, kid) this device can read.
    pub fn readable(&self) -> Vec<(u32, String)> {
        self.read.keys().cloned().collect()
    }

    /// The keys of every *authorized* epoch this device holds, ascending
    /// (what an invite carries so a newcomer can read the history).
    pub fn authorized_keys(&self, cfg: &ConfigTimeline) -> Vec<(u32, [u8; 32])> {
        cfg.current()
            .key_epochs
            .epochs
            .iter()
            .filter_map(|e| self.read.get(&(e.version, e.kid())).map(|k| (e.version, *k)))
            .collect()
    }

    /// Rotations this device can open that lost to an authorized one (or
    /// were issued against a stale head): `(version, kid)`. Their issuer
    /// should rotate again; their events stay readable.
    pub fn lost_rotations(&self) -> &[(u32, String)] {
        &self.lost
    }

    /// Test seam for transport tests that predate signed rotations: hold
    /// `key` at `version` and seal under it when it is the highest held.
    #[cfg(test)]
    pub(crate) fn force_key(&mut self, version: u32, key: [u8; 32]) {
        let kid = kid_of(&key_commitment(self.workspace_id, version, &key));
        self.read.insert((version, kid.clone()), key);
        if self.seal.as_ref().is_none_or(|s| version >= s.version) {
            self.seal = Some(SealKey { version, kid, key });
        }
    }

    /// Byte-for-byte comparable form of the ring (host parity tests).
    pub fn fingerprint(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for ((v, kid), k) in &self.read {
            out.extend_from_slice(&v.to_le_bytes());
            out.extend_from_slice(kid.as_bytes());
            out.extend_from_slice(k);
        }
        out.push(0xff);
        if let Some(s) = &self.seal {
            out.extend_from_slice(&s.version.to_le_bytes());
            out.extend_from_slice(s.kid.as_bytes());
        }
        out
    }
}

/// A rotation body from a relay keyring row (a bare signed rotation; legacy
/// unsigned rows don't carry a commitment and are ignored).
fn rotation_of_row(row: &Value) -> Option<SignedRotation> {
    let r: SignedRotation = serde_json::from_value(row.clone()).ok()?;
    hive_core::keys::commitment_well_formed(&r.key_commitment).then_some(r)
}

/// The key `r` seals to this device, if it does and the key matches the
/// rotation's own commitment (a blob that doesn't is junk).
fn open_rotation(workspace_id: Uuid, r: &SignedRotation, src: &KeySource) -> Option<[u8; 32]> {
    let ka = src.ka.as_ref()?;
    let blobs: Vec<_> = match src.device_id.and_then(|d| r.sealed.get(&d.to_string())) {
        Some(b) => vec![b],
        None => r.sealed.values().collect(),
    };
    for blob in blobs {
        let Ok(bytes) = open_blob(ka, blob) else { continue };
        let Ok(key) = <[u8; 32]>::try_from(bytes.as_slice()) else { continue };
        if key_commitment(workspace_id, r.version, &key) == r.key_commitment {
            return Some(key);
        }
    }
    None
}

/// A fresh rotation following `cfg`'s authorized head, sealed to every
/// recipient at the head (see `RosterState::rotation_recipients`). Returns the
/// body and the new key.
pub fn new_rotation(workspace_id: Uuid, cfg: &ConfigTimeline) -> Result<(SignedRotation, [u8; 32]), RotateError> {
    let roster = cfg.current();
    let recipients = roster.rotation_recipients();
    if recipients.is_empty() {
        return Err(RotateError::NoRecipients);
    }
    let key = hive_core::e2ee::generate_workspace_key().map_err(|e| RotateError::Crypto(e.to_string()))?;
    let prev = roster.key_epochs.head_version();
    let version = prev.checked_add(1).ok_or(RotateError::Exhausted)?;
    let mut sealed = BTreeMap::new();
    for (device, ka) in &recipients {
        let blob = hive_core::e2ee::seal(ka, &key).map_err(|e| RotateError::Crypto(e.to_string()))?;
        sealed.insert(device.to_string(), blob);
    }
    let rotation =
        SignedRotation { version, prev_version: prev, sealed, key_commitment: key_commitment(workspace_id, version, &key) };
    Ok((rotation, key))
}

#[derive(Debug, thiserror::Error)]
pub enum RotateError {
    #[error("no device has published a key-agreement key to seal the new key to")]
    NoRecipients,
    #[error("key versions exhausted")]
    Exhausted,
    #[error("crypto: {0}")]
    Crypto(String),
}

/// Publish a rotation body to the relay keyring route (best effort: the
/// config log is authoritative; a full keyring only slows a lagging device).
pub async fn publish_rotation(
    relay: &crate::relay_client::RelayClient,
    room: &str,
    rotation: &SignedRotation,
) -> Result<(), crate::relay_client::RelayError> {
    relay.publish_keyring_row(room, &serde_json::to_value(rotation)?).await
}

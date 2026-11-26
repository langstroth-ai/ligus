//! Signed workspace-key rotations (Phase 0, B1).
//!
//! A rotation is a config-log event ([`crate::events::SessionEvent::WorkspaceKeyRotated`])
//! signed by an Admin's device, so every device folds the same key history
//! instead of trusting whatever the relay keyring returns. Each rotation names
//! the version it follows (`prev_version`) and carries a **key commitment**:
//! `SHA-256("ligus-workspace-key-commit-v1\0" || workspace_id || u32le(version) || key)`.
//! A key is used for sealing only when it matches the commitment of the
//! newest authorized rotation; any key a device can merely open (a junk relay
//! row, a rotation that lost a tie) is only ever used to read.
//!
//! The first 8 bytes of the commitment (16 hex) are the key id (`kid`) stamped
//! on every sealed relay body ([`crate::e2ee::SealedEnvelope::kid`]), so a
//! reader picks the right key even when two keys share a version.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::e2ee::SealedBlob;

/// Domain string of the workspace-key commitment (added by Phase 0; never
/// change it).
pub const KEY_COMMIT_CONTEXT: &[u8] = b"ligus-workspace-key-commit-v1\0";

/// The body of a signed `WorkspaceKeyRotated` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedRotation {
    /// Must equal `prev_version + 1`.
    pub version: u32,
    /// The authorized head the issuer had folded.
    pub prev_version: u32,
    /// Device id → the new key sealed to that device's published X25519 key.
    pub sealed: BTreeMap<String, SealedBlob>,
    /// Hex [`key_commitment`] of the new key.
    pub key_commitment: String,
}

/// Lower-case hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The commitment to `key` as workspace `workspace_id`'s epoch `version`.
pub fn key_commitment(workspace_id: Uuid, version: u32, key: &[u8; 32]) -> String {
    let mut h = Sha256::new();
    h.update(KEY_COMMIT_CONTEXT);
    h.update(workspace_id.as_bytes());
    h.update(version.to_le_bytes());
    h.update(key);
    hex(&h.finalize())
}

/// The key id of a commitment: its first 8 bytes as 16 hex characters.
pub fn kid_of(commitment: &str) -> String {
    commitment.chars().take(16).collect()
}

/// The key id of `key` at `version` (see [`kid_of`]).
pub fn key_id(workspace_id: Uuid, version: u32, key: &[u8; 32]) -> String {
    kid_of(&key_commitment(workspace_id, version, key))
}

/// Whether `commitment` is well formed (64 lower-case hex characters).
pub fn commitment_well_formed(commitment: &str) -> bool {
    commitment.len() == 64
        && commitment
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// One authorized rotation, as the roster remembers it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyEpoch {
    pub version: u32,
    pub key_commitment: String,
    /// The rotation's canonical position on the config log.
    pub at: (u64, Uuid),
}

impl KeyEpoch {
    pub fn kid(&self) -> String {
        kid_of(&self.key_commitment)
    }
}

/// The authorized key history of a workspace, ascending by version (and by
/// position: each one follows its predecessor).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct KeyEpochs {
    pub epochs: Vec<KeyEpoch>,
}

impl KeyEpochs {
    /// The newest authorized version (0 = none: the passphrase epoch).
    pub fn head_version(&self) -> u32 {
        self.epochs.last().map(|e| e.version).unwrap_or(0)
    }

    pub fn head(&self) -> Option<&KeyEpoch> {
        self.epochs.last()
    }

    /// The seal epoch a device that had folded the config log up to `key`
    /// must have used: the newest authorized version at or before it.
    pub fn version_at(&self, key: (u64, Uuid)) -> u32 {
        self.epochs
            .iter()
            .rev()
            .find(|e| e.at <= key)
            .map(|e| e.version)
            .unwrap_or(0)
    }

    /// The authorized epoch `version`, if any.
    pub fn get(&self, version: u32) -> Option<&KeyEpoch> {
        self.epochs.iter().find(|e| e.version == version)
    }

    /// Why `rotation` can't follow this history, if it can't.
    pub fn check_next(&self, rotation: &SignedRotation) -> Result<(), String> {
        let head = self.head_version();
        if rotation.prev_version != head {
            return Err(format!(
                "rotation follows version {}, but the head is {head}",
                rotation.prev_version
            ));
        }
        if rotation.version != head.saturating_add(1) || rotation.version == 0 {
            return Err(format!(
                "rotation version {} must be {}",
                rotation.version,
                head.saturating_add(1)
            ));
        }
        if !commitment_well_formed(&rotation.key_commitment) {
            return Err("malformed key commitment".into());
        }
        if rotation.sealed.is_empty() {
            return Err("a rotation seals the key to at least one device".into());
        }
        Ok(())
    }

    pub fn push(&mut self, rotation: &SignedRotation, at: (u64, Uuid)) {
        self.epochs.push(KeyEpoch {
            version: rotation.version,
            key_commitment: rotation.key_commitment.clone(),
            at,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commitment_context_is_pinned() {
        // b"ligus-workspace-key-commit-v1\0", as hex so a rename can't "update" it.
        assert_eq!(
            hex(KEY_COMMIT_CONTEXT),
            "6c696775732d776f726b73706163652d6b65792d636f6d6d69742d763100"
        );
        let ws = Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
        let c = key_commitment(ws, 3, &[7u8; 32]);
        assert!(commitment_well_formed(&c));
        assert_eq!(kid_of(&c).len(), 16);
        assert_ne!(c, key_commitment(ws, 4, &[7u8; 32]), "the version is bound");
        assert_ne!(
            c,
            key_commitment(Uuid::nil(), 3, &[7u8; 32]),
            "the workspace is bound"
        );
    }

    #[test]
    fn version_at_follows_positions() {
        let mut k = KeyEpochs::default();
        let a = Uuid::from_u128(1);
        let rot = |v: u32| SignedRotation {
            version: v,
            prev_version: v - 1,
            sealed: BTreeMap::from([(
                "d".into(),
                SealedBlob {
                    ephemeral_public: vec![],
                    ciphertext: vec![],
                },
            )]),
            key_commitment: "a".repeat(64),
        };
        assert!(k.check_next(&rot(1)).is_ok());
        k.push(&rot(1), (10, a));
        assert!(k.check_next(&rot(1)).is_err(), "same version twice");
        assert!(k.check_next(&rot(3)).is_err(), "a gap");
        k.push(&rot(2), (20, a));
        assert_eq!(k.version_at((5, a)), 0);
        assert_eq!(k.version_at((10, a)), 1);
        assert_eq!(k.version_at((19, a)), 1);
        assert_eq!(k.version_at((99, a)), 2);
    }
}

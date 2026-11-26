//! Identity bootstrap + private-key storage — ported from `Identity.swift`,
//! `IdentityStore.swift`, and `KeychainIdentityStore`/`FileIdentityStore`.
//!
//! On first run we generate an Ed25519 **account** keypair and a per-device
//! keypair, issue a [`DeviceCertificate`] chaining the device key to the
//! account key, and persist the **public** records as JSON. The **private**
//! seeds go to a [`KeyVault`].
//!
//! Phase 2 ships a [`FileKeyVault`] (raw seed bytes on disk) — cross-platform
//! and CI-portable. An OS-keystore-backed vault (the `keyring` crate, with its
//! per-OS backend features) drops in behind the same trait at packaging time;
//! it needs platform-specific backends that don't exist in headless CI, so it
//! is intentionally not the default here.

use std::path::{Path, PathBuf};

use ligus_core::crypto::{
    device_id_for, DeviceCertificate, DeviceIdentity, HumanAccount, Platform, SigAlg,
    SigningKeypair,
};
use ligus_core::Timestamp;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("crypto error: {0}")]
    Crypto(#[from] ligus_core::CryptoError),
    #[error("key vault: {0}")]
    Vault(String),
    #[error("stored seed for {0} is missing or malformed")]
    MissingSeed(String),
}

/// Stores/loads 32-byte private signing seeds, keyed by a stable id.
pub trait KeyVault {
    fn store_seed(&self, key_id: &str, seed: &[u8; 32]) -> Result<(), IdentityError>;
    fn load_seed(&self, key_id: &str) -> Result<Option<[u8; 32]>, IdentityError>;
}

/// File-backed key vault: one `<key_id>.seed` file of raw bytes per key.
pub struct FileKeyVault {
    dir: PathBuf,
}

impl FileKeyVault {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self {
            dir: dir.as_ref().join("keys"),
        }
    }

    fn path_for(&self, key_id: &str) -> PathBuf {
        // key ids look like "account:<uuid>" / "device:<uuid>"; keep them
        // filesystem-safe.
        self.dir.join(format!("{}.seed", key_id.replace(':', "_")))
    }
}

impl KeyVault for FileKeyVault {
    fn store_seed(&self, key_id: &str, seed: &[u8; 32]) -> Result<(), IdentityError> {
        crate::secret_redact::register_bytes(seed);
        crate::private_file::ensure_private_dir(&self.dir)?;
        let path = self.path_for(key_id);
        #[cfg(unix)]
        {
            // Owner-only, whatever the umask: these are private signing keys.
            use std::io::Write;
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)?;
            // `mode` applies only on create: tighten a file left by an older build.
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            f.write_all(seed)?;
            f.sync_all()?;
        }
        #[cfg(not(unix))]
        std::fs::write(&path, seed)?;
        Ok(())
    }

    fn load_seed(&self, key_id: &str) -> Result<Option<[u8; 32]>, IdentityError> {
        match std::fs::read(self.path_for(key_id)) {
            Ok(bytes) => {
                // Best effort: tighten a seed an older build left world-readable.
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(
                        self.path_for(key_id),
                        std::fs::Permissions::from_mode(0o600),
                    );
                }
                let seed: [u8; 32] = bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| IdentityError::MissingSeed(key_id.to_string()))?;
                crate::secret_redact::register_bytes(&seed);
                Ok(Some(seed))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

/// The persisted public identity for this install: one account + this device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredIdentity {
    pub account: HumanAccount,
    pub device: DeviceIdentity,
    /// This install is a box (`ligus serve`, `ligus worker`): it acts as its
    /// own **service** member (Phase B), never as a person. Device-local;
    /// left out when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub service: bool,
}

impl StoredIdentity {
    /// The actor this install writes as: its account, as a service when
    /// this install is a box.
    pub fn actor(&self) -> ligus_core::ActorIdentity {
        let mut a = self.account.actor();
        if self.service {
            a.kind = ligus_core::ActorKind::Service;
        }
        a
    }
}

fn account_key_id(account_id: Uuid) -> String {
    format!("account:{account_id}")
}

fn device_key_id(device_id: Uuid) -> String {
    format!("device:{device_id}")
}

/// Loads or bootstraps the local identity, backed by a [`KeyVault`] for private
/// seeds and a JSON file for the public records.
pub struct IdentityStore<V: KeyVault> {
    records_path: PathBuf,
    vault: V,
}

impl<V: KeyVault> IdentityStore<V> {
    pub fn new(app_data_dir: impl AsRef<Path>, vault: V) -> Self {
        Self {
            records_path: app_data_dir.as_ref().join("identity.json"),
            vault,
        }
    }

    pub fn load(&self) -> Result<Option<StoredIdentity>, IdentityError> {
        match std::fs::read_to_string(&self.records_path) {
            Ok(s) => Ok(Some(serde_json::from_str(&s)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Load the existing identity or generate one (account keypair + device
    /// keypair + device certificate). Idempotent across launches.
    pub fn bootstrap(
        &self,
        display_name: &str,
        handle: &str,
        device_name: &str,
    ) -> Result<StoredIdentity, IdentityError> {
        if let Some(existing) = self.load()? {
            // A device whose id isn't the one its key derives (made before
            // ids were derived) is never certified anywhere: replace it.
            if existing.device.id
                != device_id_for(SigAlg::Ed25519, &existing.device.signing_public_key)
            {
                return self.rotate_device();
            }
            return Ok(existing);
        }

        let now = Timestamp::now();

        // Account root of trust.
        let account_kp = SigningKeypair::generate()?;
        let account_id = Uuid::new_v4();
        self.vault
            .store_seed(&account_key_id(account_id), &account_kp.seed_bytes())?;
        let account = HumanAccount {
            id: account_id,
            display_name: display_name.to_string(),
            handle: handle.to_string(),
            signing_public_key: account_kp.public_key_bytes().to_vec(),
            avatar_url: None,
            created_at: now,
        };

        // This device, certified by the account key.
        // Its id is the one its key derives (`ligus_core::crypto::device_id_for`).
        let device_kp = SigningKeypair::generate()?;
        let device_id = device_id_for(SigAlg::Ed25519, &device_kp.public_key_bytes());
        self.vault
            .store_seed(&device_key_id(device_id), &device_kp.seed_bytes())?;
        let certificate =
            DeviceCertificate::issue(&account_kp, account_id, &device_kp.public_key_bytes(), now);
        let device = DeviceIdentity {
            id: device_id,
            account_id,
            platform: Platform::current(),
            device_name: device_name.to_string(),
            signing_public_key: device_kp.public_key_bytes().to_vec(),
            certificate,
            created_at: now,
            revoked_at: None,
        };

        let stored = StoredIdentity {
            account,
            device,
            service: false,
        };
        self.save(&stored)?;
        Ok(stored)
    }

    /// Mark this install as a box (a service member, Phase B) and persist.
    /// One way: a box never becomes a person.
    pub fn mark_service(&self) -> Result<StoredIdentity, IdentityError> {
        let mut stored = self
            .load()?
            .ok_or_else(|| IdentityError::Vault("no identity to mark".into()))?;
        if !stored.service {
            stored.service = true;
            self.save(&stored)?;
        }
        Ok(stored)
    }

    /// Update the account display name (keys unchanged) and persist.
    pub fn update_display_name(&self, display_name: &str) -> Result<StoredIdentity, IdentityError> {
        let mut stored = self
            .load()?
            .ok_or_else(|| IdentityError::Vault("no identity to update".into()))?;
        stored.account.display_name = display_name.to_string();
        self.save(&stored)?;
        Ok(stored)
    }

    /// Set (or clear, with `None`) the account avatar and persist. Keys
    /// unchanged. The avatar rides the actor identity into the synced roster.
    pub fn update_avatar(
        &self,
        avatar_url: Option<String>,
    ) -> Result<StoredIdentity, IdentityError> {
        let mut stored = self
            .load()?
            .ok_or_else(|| IdentityError::Vault("no identity to update".into()))?;
        stored.account.avatar_url = avatar_url;
        self.save(&stored)?;
        Ok(stored)
    }

    /// Replace this install's **device** with a fresh one (new id + signing key,
    /// certified by the same account key) and persist. For a device that was
    /// revoked: a revoked device id stays revoked, so re-enrolling needs a new
    /// one. The old device's seed is left in the vault (never used again).
    pub fn rotate_device(&self) -> Result<StoredIdentity, IdentityError> {
        let mut stored = self
            .load()?
            .ok_or_else(|| IdentityError::Vault("no identity to rotate".into()))?;
        let account_kp = self.account_keypair(stored.account.id)?;
        let now = Timestamp::now();
        // A new key is a new device: its id is the one the new key derives.
        let device_kp = SigningKeypair::generate()?;
        let device_id = device_id_for(SigAlg::Ed25519, &device_kp.public_key_bytes());
        self.vault
            .store_seed(&device_key_id(device_id), &device_kp.seed_bytes())?;
        let certificate = DeviceCertificate::issue(
            &account_kp,
            stored.account.id,
            &device_kp.public_key_bytes(),
            now,
        );
        stored.device = DeviceIdentity {
            id: device_id,
            account_id: stored.account.id,
            platform: stored.device.platform,
            device_name: stored.device.device_name.clone(),
            signing_public_key: device_kp.public_key_bytes().to_vec(),
            certificate,
            created_at: now,
            revoked_at: None,
        };
        self.save(&stored)?;
        Ok(stored)
    }

    /// A new device key for this identity, kept in the vault next to the
    /// current one but **not** switched to (a link request names it; the
    /// install switches only when the request is answered, with
    /// [`IdentityStore::adopt_device`]). Returns its id and keypair.
    pub fn prepare_device(&self) -> Result<(Uuid, SigningKeypair), IdentityError> {
        let device_kp = SigningKeypair::generate()?;
        let device_id = device_id_for(SigAlg::Ed25519, &device_kp.public_key_bytes());
        self.vault
            .store_seed(&device_key_id(device_id), &device_kp.seed_bytes())?;
        Ok((device_id, device_kp))
    }

    /// Switch this identity to the device `device_id` a
    /// [`IdentityStore::prepare_device`] made (its seed is in the vault),
    /// certified under this identity's own account like any rotation. A
    /// no-op when it is already the current device.
    pub fn adopt_device(&self, device_id: Uuid) -> Result<StoredIdentity, IdentityError> {
        let mut stored = self
            .load()?
            .ok_or_else(|| IdentityError::Vault("no identity to switch".into()))?;
        if stored.device.id == device_id {
            return Ok(stored);
        }
        let device_kp = self.device_keypair(device_id)?;
        if device_id_for(SigAlg::Ed25519, &device_kp.public_key_bytes()) != device_id {
            return Err(IdentityError::Vault(
                "that device key doesn't derive its id".into(),
            ));
        }
        let account_kp = self.account_keypair(stored.account.id)?;
        let now = Timestamp::now();
        let certificate = DeviceCertificate::issue(
            &account_kp,
            stored.account.id,
            &device_kp.public_key_bytes(),
            now,
        );
        stored.device = DeviceIdentity {
            id: device_id,
            account_id: stored.account.id,
            platform: stored.device.platform,
            device_name: stored.device.device_name.clone(),
            signing_public_key: device_kp.public_key_bytes().to_vec(),
            certificate,
            created_at: now,
            revoked_at: None,
        };
        self.save(&stored)?;
        Ok(stored)
    }

    fn save(&self, stored: &StoredIdentity) -> Result<(), IdentityError> {
        if let Some(parent) = self.records_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.records_path, serde_json::to_vec_pretty(stored)?)?;
        Ok(())
    }

    /// Reconstruct the account signing keypair from the vault.
    pub fn account_keypair(&self, account_id: Uuid) -> Result<SigningKeypair, IdentityError> {
        let seed = self
            .vault
            .load_seed(&account_key_id(account_id))?
            .ok_or_else(|| IdentityError::MissingSeed(account_key_id(account_id)))?;
        Ok(SigningKeypair::from_seed(&seed)?)
    }

    /// Reconstruct this device's signing keypair from the vault — used to sign
    /// event envelopes.
    pub fn device_keypair(&self, device_id: Uuid) -> Result<SigningKeypair, IdentityError> {
        let seed = self
            .vault
            .load_seed(&device_key_id(device_id))?
            .ok_or_else(|| IdentityError::MissingSeed(device_key_id(device_id)))?;
        Ok(SigningKeypair::from_seed(&seed)?)
    }

    /// This device's X25519 key-agreement keypair (B1), generated and kept in
    /// the vault on first use. Rotations seal the workspace key to its public
    /// half once the device publishes it on the config log.
    pub fn key_agreement(
        &self,
        device_id: Uuid,
    ) -> Result<ligus_core::KeyAgreementKeypair, IdentityError> {
        let id = format!("ka:{device_id}");
        if let Some(seed) = self.vault.load_seed(&id)? {
            return Ok(ligus_core::KeyAgreementKeypair::from_seed(&seed)?);
        }
        let kp = ligus_core::KeyAgreementKeypair::generate()?;
        self.vault.store_seed(&id, &kp.seed_bytes())?;
        Ok(kp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ligus_core::crypto::verify_envelope;
    use ligus_core::{ChatMessage, MessageRole, SessionEvent, SessionEventEnvelope};

    fn store(dir: &Path) -> IdentityStore<FileKeyVault> {
        IdentityStore::new(dir, FileKeyVault::new(dir))
    }

    /// Private key seeds must be readable by the owner only, whatever the
    /// process umask (often 022, which made them 0644), and the key dir too.
    #[cfg(unix)]
    #[test]
    fn key_seeds_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let vault = FileKeyVault::new(dir.path());
        vault.store_seed("device:x", &[1u8; 32]).unwrap();
        let mode = std::fs::metadata(vault.path_for("device:x"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "seed file mode {mode:o}");
        let dmode = std::fs::metadata(&vault.dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dmode & 0o077, 0, "key dir mode {dmode:o}");
        // Re-storing over an existing, too-open file tightens it.
        std::fs::set_permissions(
            vault.path_for("device:x"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        vault.store_seed("device:x", &[2u8; 32]).unwrap();
        let mode = std::fs::metadata(vault.path_for("device:x"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(vault.load_seed("device:x").unwrap(), Some([2u8; 32]));
        // Loading a seed an older build left at 0644 tightens it too.
        std::fs::set_permissions(
            vault.path_for("device:x"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        vault.load_seed("device:x").unwrap();
        assert_eq!(
            std::fs::metadata(vault.path_for("device:x"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn bootstrap_is_idempotent_and_certificate_chains() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let first = s.bootstrap("Mara", "mara", "Mara's Mac").unwrap();
        let again = s.bootstrap("ignored", "ignored", "ignored").unwrap();
        assert_eq!(first, again, "identity must be stable across launches");

        // device certificate chains to the account key
        assert!(first
            .device
            .certificate
            .verify(&first.account.signing_public_key)
            .is_ok());
    }

    #[test]
    fn rotate_device_keeps_the_account_and_issues_a_fresh_certified_device() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let first = s.bootstrap("Alex", "alex", "box").unwrap();
        let rotated = s.rotate_device().unwrap();
        assert_eq!(rotated.account, first.account);
        assert_ne!(rotated.device.id, first.device.id);
        assert_ne!(
            rotated.device.signing_public_key,
            first.device.signing_public_key
        );
        assert!(rotated
            .device
            .certificate
            .verify(&first.account.signing_public_key)
            .is_ok());
        assert_eq!(s.load().unwrap().unwrap(), rotated);
        // The new device's key is in the vault.
        let kp = s.device_keypair(rotated.device.id).unwrap();
        assert_eq!(
            kp.public_key_bytes().to_vec(),
            rotated.device.signing_public_key
        );
    }

    #[test]
    fn device_keypair_signs_envelopes_verifiable_by_public_record() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let id = s.bootstrap("Mara", "mara", "Mara's Mac").unwrap();

        let kp = s.device_keypair(id.device.id).unwrap();
        let mut env = SessionEventEnvelope::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            1,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "Mara", "signed by my device"),
            },
        );
        ligus_core::sign_envelope(&mut env, id.device.id, &kp);

        // the public device record verifies what the private seed signed
        assert!(verify_envelope(&env, &id.device.signing_public_key).is_ok());
    }
}

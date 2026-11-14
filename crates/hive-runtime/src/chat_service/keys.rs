//! Workspace keys on the config log (Phase 0, B1): this device's own
//! key-agreement key, and signed key rotations.

use hive_core::keys::SignedRotation;
use hive_core::{workspace_config_session_id, SessionEvent, SessionEventEnvelope};
use uuid::Uuid;

use super::{ChatError, ChatService, Result};

/// A rotation this device just issued.
pub struct RotationIssued {
    pub rotation: SignedRotation,
    /// The new workspace key (this device is always one of its recipients).
    pub key: [u8; 32],
    pub envelope: SessionEventEnvelope,
}

impl ChatService {
    /// Publish this device's own X25519 key-agreement key on `workspace_id`'s
    /// config log, unless the log already carries exactly that key for this
    /// device. Rotations seal only to keys published this way. Returns whether
    /// it published.
    pub fn publish_key_agreement(&mut self, workspace_id: Uuid, ka_public: [u8; 32]) -> Result<bool> {
        if self.local_workspace_id == Some(workspace_id) {
            return Ok(false);
        }
        let (config, timeline) =
            self.store.load_config(workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        if config.is_none() {
            return Ok(false);
        }
        if timeline.current().ka_keys.get(&self.device_id).is_some_and(|k| k.as_slice() == ka_public) {
            return Ok(false);
        }
        // The config log's roster must certify this device (a joiner may have
        // published its identity on a chat log only).
        if !timeline.current().device_trusted(self.device_id) && !self.delegated {
            if let Some(account_id) = self.author.account_id {
                let config_id = workspace_config_session_id(workspace_id);
                self.append_signed(
                    config_id,
                    workspace_id,
                    SessionEvent::AccountKeyRegistered {
                        account_id,
                        signing_public_key: self.account_keypair.public_key_bytes().to_vec(),
                    },
                )?;
                let certificate = hive_core::crypto::DeviceCertificate::issue(
                    &self.account_keypair,
                    account_id,
                    self.device_id,
                    &self.keypair.public_key_bytes(),
                    hive_core::Timestamp::now(),
                );
                self.append_signed(config_id, workspace_id, SessionEvent::DeviceCertificateAdded { certificate })?;
            }
        }
        self.append_signed(
            workspace_config_session_id(workspace_id),
            workspace_id,
            SessionEvent::DeviceKeyAgreementPublished { device_id: self.device_id, ka_public: ka_public.to_vec() },
        )?;
        Ok(true)
    }

    /// Rotate `workspace_id`'s key: a fresh key sealed to every recipient at
    /// the head of the config log (each current member's devices that
    /// published a key-agreement key, and bound agent hosts), signed as a
    /// `WorkspaceKeyRotated` at version head + 1. Admin+ (and presence under
    /// rules v2). The caller publishes the body to the relay keyring route
    /// ([`crate::keyring::publish_rotation`]) for devices whose config log is
    /// behind.
    pub fn rotate_workspace_key(&mut self, workspace_id: Uuid) -> Result<RotationIssued> {
        if self.local_workspace_id == Some(workspace_id) {
            return Err(ChatError::Unauthorized("the local workspace has no shared key".into()));
        }
        let (config, timeline) =
            self.store.load_config(workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        if config.is_none() {
            return Err(ChatError::Unauthorized("the workspace configuration hasn't synced to this device yet".into()));
        }
        if !timeline.current().ka_keys.contains_key(&self.device_id) {
            return Err(ChatError::Unauthorized(
                "this device hasn't published its key-agreement key, so it couldn't read the new key".into(),
            ));
        }
        let (rotation, key) = crate::keyring::new_rotation(workspace_id, &timeline)
            .map_err(|e| ChatError::Unauthorized(e.to_string()))?;
        let envelope = self.append_signed(
            workspace_config_session_id(workspace_id),
            workspace_id,
            SessionEvent::WorkspaceKeyRotated { rotation: rotation.clone() },
        )?;
        Ok(RotationIssued { rotation, key, envelope })
    }
}

impl ChatService {
    /// Every authorized epoch key this device holds for `workspace_id`, for an
    /// invite or pairing payload (B1 newcomer bootstrap).
    pub fn delivered_epoch_keys(
        &self,
        workspace_id: Uuid,
        src: &crate::keyring::KeySource,
    ) -> Result<Vec<crate::keyring::DeliveredKey>> {
        let (config, timeline) =
            self.store.load_config(workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        if config.is_none() {
            return Ok(Vec::new());
        }
        let ring = crate::keyring::KeyringEngine::build(workspace_id, Some(&timeline), src, &[]);
        Ok(ring
            .authorized_keys(&timeline)
            .into_iter()
            .map(|(v, k)| crate::keyring::DeliveredKey::new(v, &k))
            .collect())
    }
}

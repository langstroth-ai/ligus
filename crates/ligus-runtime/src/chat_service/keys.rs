//! Workspace keys on the config log (Phase 0, B1): this device's own
//! key-agreement key, and signed key rotations.

use ligus_core::keys::SignedRotation;
use ligus_core::{workspace_config_session_id, SessionEvent, SessionEventEnvelope};
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
    /// The config log folded with this device's founder pin for it. Keys are
    /// only ever decided on a pinned fold (B2 residual): without a pin a
    /// forged lower-lamport seed could choose the roster rotations seal to.
    pub(crate) fn pinned_config(
        &self,
        workspace_id: Uuid,
    ) -> Result<(Option<ligus_core::ChatSession>, std::sync::Arc<ligus_core::authz::ConfigTimeline>)> {
        let Some(pin) = self.store.workspace_pin(workspace_id)? else {
            return Err(ChatError::Unauthorized(
                "this device has no founder pin for the workspace (join it from an invite)".into(),
            ));
        };
        Ok(self.store.load_config(workspace_id, Some(&pin.founder_account.to_string()), self.local_workspace_id)?)
    }

    /// Publish this device's own X25519 key-agreement key on `workspace_id`'s
    /// config log, unless the log already carries exactly that key for this
    /// device. Rotations seal only to keys published this way. Returns whether
    /// it published.
    pub fn publish_key_agreement(&mut self, workspace_id: Uuid, ka_public: [u8; 32]) -> Result<bool> {
        if self.local_workspace_id == Some(workspace_id) {
            return Ok(false);
        }
        let (config, timeline) = self.pinned_config(workspace_id)?;
        if config.is_none() {
            return Ok(false);
        }
        if timeline.current().ka_keys.get(&self.device_id).is_some_and(|k| k.as_slice() == ka_public) {
            return Ok(false);
        }
        // The config log's roster must certify this device (a joiner may have
        // published its identity on a chat log only). This never asks for a
        // passkey: under rules v2 a new device of an account with one is
        // confirmed explicitly (`confirm_device`), and until then it publishes
        // no key.
        if !timeline.current().device_trusted(self.device_id) && !self.delegated && self.author.account_id.is_some() {
            let config_id = workspace_config_session_id(workspace_id);
            let certified = self.store.has_device_certificate_in(config_id, self.device_id, self.author.account_id.unwrap_or_default())?;
            if !certified {
                self.publish_identity(config_id, workspace_id)?;
            }
            if !self.device_confirmed(workspace_id)? {
                return Err(ChatError::Unauthorized(
                    "this device isn't confirmed for the workspace yet (Settings → Passkeys & keys → Confirm this device)".into(),
                ));
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
        let (config, timeline) = self.pinned_config(workspace_id)?;
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
        let Ok((config, timeline)) = self.pinned_config(workspace_id) else { return Ok(Vec::new()) };
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

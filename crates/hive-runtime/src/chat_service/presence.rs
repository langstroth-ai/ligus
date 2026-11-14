//! Authoring events that need passkey presence (Phase 0, B3).
//!
//! When the fold would refuse a drafted event for lack of presence
//! ([`hive_core::authz::PRESENCE_REQUIRED`]), `append_signed` asks this
//! device's [`PresenceProvider`] (the desktop's browser hand-off) for an
//! assertion over the event digest, attaches it inside the stamp, re-signs
//! and appends. A host without a provider (`ligus serve`) captures the draft
//! instead ([`ChatService::capture_presence`]): the phone signs its digest
//! with a passkey and [`ChatService::commit_presence`] appends it with the
//! digest fields frozen at prepare time.

use std::sync::Arc;

use hive_core::authz::PRESENCE_REQUIRED;
use hive_core::crypto::sign_envelope;
use hive_core::presence::{self, PasskeyCredential, PresenceAssertion};
use hive_core::{workspace_config_session_id, SessionEvent, SessionEventEnvelope};
use uuid::Uuid;

use super::{ChatError, ChatService, Result};

/// What a person confirms: the event and the digest their passkey signs.
#[derive(Debug, Clone)]
pub struct PresenceRequest {
    /// The WebAuthn challenge (the event digest).
    pub challenge: [u8; 32],
    /// The event kind (`memberRemoved`, `proposalVoteCast`, …).
    pub kind: String,
    /// The account whose passkey must sign.
    pub account_id: Uuid,
    /// That account's credentials here (WebAuthn `allowCredentials`).
    pub credential_ids: Vec<String>,
    /// First 8 hex characters of the digest, shown to the person.
    pub digest_short: String,
    /// What is being authorized, in words.
    pub summary: String,
}

/// Obtains a passkey assertion for a request (the desktop's browser
/// hand-off). Blocking; returns an error when the person cancels or the
/// gesture times out.
pub trait PresenceProvider: Send + Sync {
    fn assert(&self, req: &PresenceRequest) -> std::result::Result<PresenceAssertion, String>;
}

/// An event waiting for its author's passkey (serve's prepare/commit).
#[derive(Debug, Clone)]
pub struct PresenceDraft {
    pub env: SessionEventEnvelope,
    pub request: PresenceRequest,
}

/// A short description of what an event authorizes (the confirmation sheet).
pub fn summarize(payload: &SessionEvent) -> String {
    match payload {
        SessionEvent::MemberAdded { member, .. } => format!("Add {} as {:?}", member.actor.display_name, member.role),
        SessionEvent::MemberRemoved { member_id } => format!("Remove member {member_id}"),
        SessionEvent::MemberRoleChanged { change } => format!("Change {}'s role to {:?}", change.member_id, change.new_role),
        SessionEvent::DeviceRevoked { device_id } => format!("Revoke device {device_id}"),
        SessionEvent::WorkspaceKeyRotated { rotation } => {
            format!("Rotate the workspace key to version {} ({} devices)", rotation.version, rotation.sealed.len())
        }
        SessionEvent::PasskeyRegistered { credential } => format!("Register passkey \"{}\"", credential.label),
        SessionEvent::PasskeyRemoved { .. } => "Remove a passkey".into(),
        SessionEvent::WorkspaceRulesSet { version } => format!("Switch the workspace to rules version {version}"),
        SessionEvent::ProposalVoteCast { approval, .. } => {
            if approval.approved { "Approve a proposal".into() } else { "Reject a proposal".into() }
        }
        SessionEvent::ChatLockdownCleared { .. } => "Clear a chat's lockdown".into(),
        SessionEvent::DearthModeChanged { .. } => "Change dearth mode".into(),
        other => format!("Confirm {}", other.kind_str()),
    }
}

impl ChatService {
    /// Use `provider` for events that need presence (the desktop).
    pub fn set_presence_provider(&mut self, provider: Option<Arc<dyn PresenceProvider>>) {
        self.presence_provider = provider;
    }

    /// Capture the next write instead of appending it (serve): it returns
    /// `Err(Unauthorized(PRESENCE_REQUIRED))` and leaves its draft in
    /// [`ChatService::take_presence_draft`], whether or not the rules need
    /// presence for it (a delegated host's action always carries its
    /// owner's passkey).
    pub fn capture_presence(&mut self, on: bool) {
        self.capture_presence = on;
        if !on {
            self.presence_draft = None;
        }
    }

    /// The draft the last refused append captured.
    pub fn take_presence_draft(&mut self) -> Option<PresenceDraft> {
        self.presence_draft.take()
    }

    /// The request for a drafted envelope.
    pub(super) fn presence_request(&self, env: &SessionEventEnvelope) -> Result<PresenceRequest> {
        let account_id = env
            .actor_stamp
            .as_ref()
            .and_then(|s| s.actor.account_id)
            .ok_or_else(|| ChatError::Unauthorized("a passkey confirmation needs an account".into()))?;
        let (_, timeline) = self.store.load_config(env.workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        let credential_ids = timeline
            .current()
            .passkeys
            .get(&account_id)
            .map(|m| m.iter().filter(|(_, k)| !k.recovery).map(|(id, _)| id.clone()).collect())
            .unwrap_or_default();
        let challenge = presence::challenge(env);
        Ok(PresenceRequest {
            challenge,
            kind: env.payload.kind_str().to_string(),
            account_id,
            credential_ids,
            digest_short: hive_core::keys::hex(&challenge[..4]),
            summary: summarize(&env.payload),
        })
    }

    /// `append_signed`'s answer to a presence refusal: ask the provider, or
    /// capture the draft, or refuse.
    pub(super) fn with_presence(&mut self, env: &mut SessionEventEnvelope) -> Result<()> {
        let request = self.presence_request(env)?;
        if let Some(provider) = self.presence_provider.clone() {
            let assertion = provider.assert(&request).map_err(|e| ChatError::Unauthorized(format!("passkey confirmation: {e}")))?;
            if let Some(stamp) = env.actor_stamp.as_mut() {
                stamp.presence = Some(assertion);
            }
            env.clear_raw();
            sign_envelope(env, self.device_id, &self.keypair);
            return Ok(());
        }
        let _ = request;
        Err(ChatError::Unauthorized(PRESENCE_REQUIRED.into()))
    }

    /// Keep `env` as the draft a passkey will confirm (capture mode), and
    /// leave capture mode: one write per confirmation.
    pub(super) fn capture_draft(&mut self, env: &SessionEventEnvelope) -> Result<()> {
        let request = self.presence_request(env)?;
        self.presence_draft = Some(PresenceDraft { env: env.clone(), request });
        self.capture_presence = false;
        Ok(())
    }

    /// Append a captured draft with the author's assertion attached inside
    /// its stamp (the digest fields are the ones frozen at prepare time).
    pub fn commit_presence(&mut self, draft: PresenceDraft, assertion: PresenceAssertion) -> Result<SessionEventEnvelope> {
        let mut env = draft.env;
        if self.store.has_event(env.event_id)? {
            return Err(ChatError::Unauthorized("already committed".into()));
        }
        if let Some(stamp) = env.actor_stamp.as_mut() {
            stamp.presence = Some(assertion);
        }
        env.clear_raw();
        env.sequence = self.store.max_sequence(env.session_id)?.unwrap_or(0) + 1;
        sign_envelope(&mut env, self.device_id, &self.keypair);
        let (config, timeline) = self.store.load_config(env.workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        self.authorize_as_fold(&env, config.as_ref(), &timeline)?;
        self.store.append_envelope(&env)?;
        Ok(env)
    }

    /// Register a passkey for this device's account. `prove` receives the
    /// registration event's digest and returns a `get` assertion over it: by
    /// the new credential itself for the account's first (or the recovery
    /// key's own signature), by an existing credential for every later one.
    pub fn register_passkey(
        &mut self,
        workspace_id: Uuid,
        credential: PasskeyCredential,
        prove: &dyn Fn(&PresenceRequest) -> std::result::Result<PresenceAssertion, String>,
    ) -> Result<SessionEventEnvelope> {
        if self.delegated {
            return Err(ChatError::Unauthorized("a delegated host can't register its owner's passkeys".into()));
        }
        let config_id = workspace_config_session_id(workspace_id);
        let mut env = self.draft_signed(config_id, workspace_id, SessionEvent::PasskeyRegistered { credential })?;
        let request = self.presence_request(&env)?;
        let proof = prove(&request).map_err(|e| ChatError::Unauthorized(format!("passkey: {e}")))?;
        if let Some(stamp) = env.actor_stamp.as_mut() {
            stamp.presence = Some(proof);
        }
        env.clear_raw();
        sign_envelope(&mut env, self.device_id, &self.keypair);
        let (config, timeline) = self.store.load_config(workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        self.authorize_as_fold(&env, config.as_ref(), &timeline)?;
        self.store.append_envelope(&env)?;
        Ok(env)
    }

    /// Remove one of `account_id`'s passkeys on `workspace_id` (the account
    /// itself, or an Admin; a passkey confirmation under rules v2).
    pub fn remove_passkey(&mut self, workspace_id: Uuid, account_id: Uuid, credential_id: &str) -> Result<SessionEventEnvelope> {
        self.append_signed(
            workspace_config_session_id(workspace_id),
            workspace_id,
            SessionEvent::PasskeyRemoved { account_id, credential_id: credential_id.to_string() },
        )
    }

    /// This device's account's credentials on `workspace_id`.
    pub fn passkeys_of(&self, workspace_id: Uuid, account_id: Uuid) -> Result<Vec<(String, bool)>> {
        let (_, timeline) = self.store.load_config(workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        Ok(timeline
            .current()
            .passkeys
            .get(&account_id)
            .map(|m| m.iter().map(|(id, k)| (id.clone(), k.recovery)).collect())
            .unwrap_or_default())
    }

    /// Set the workspace rules version (Owner). Turning on rules v2 needs
    /// this device's account to hold a recovery key and a passkey first, so a
    /// sole Owner can't lock themself out (the fold itself only needs the
    /// switch to carry presence).
    pub fn set_workspace_rules(&mut self, workspace_id: Uuid, version: u32) -> Result<SessionEventEnvelope> {
        if version >= 2 {
            let account = self.author.account_id.ok_or_else(|| ChatError::Unauthorized("rules v2 needs an account".into()))?;
            let creds = self.passkeys_of(workspace_id, account)?;
            if !creds.iter().any(|(_, recovery)| *recovery) || !creds.iter().any(|(_, recovery)| !*recovery) {
                return Err(ChatError::Unauthorized(
                    "register a passkey and write down a recovery key before turning on rules v2".into(),
                ));
            }
        }
        self.append_signed(workspace_config_session_id(workspace_id), workspace_id, SessionEvent::WorkspaceRulesSet { version })
    }

    /// Re-sign a test envelope with this device's key.
    #[cfg(test)]
    pub(crate) fn sign_for_test(&self, env: &mut SessionEventEnvelope) {
        sign_envelope(env, self.device_id, &self.keypair);
    }
}

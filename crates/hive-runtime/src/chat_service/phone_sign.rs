//! Events a phone signs with its own key, relayed by a box (Phase B).
//!
//! A box is its own service member: it never signs as the phone's person.
//! For what only a person may do, the box drafts the event **as the phone's
//! person, to be signed by the phone's own P-256 device key**: it fixes the
//! digest fields, the phone confirms with the person's passkey over the
//! presence challenge, then signs the preimage; the box checks the signature
//! against the phone's certificate, judges the event exactly as every fold
//! will, and appends it. The box can withhold such an event, never forge or
//! alter one. Today this carries box admissions (the phone tier allows
//! nothing else about membership).

use hive_core::crypto::{DeviceCertificate, SigAlg};
use hive_core::presence::PresenceAssertion;
use hive_core::{workspace_config_session_id, ActorStamp, SessionEvent, SessionEventEnvelope, Timestamp};
use uuid::Uuid;

use super::presence::PresenceRequest;
use super::{ChatError, ChatService, Result};

/// A drafted phone-signed event and its presence request.
#[derive(Debug, Clone)]
pub struct PhoneDraft {
    pub env: SessionEventEnvelope,
    pub request: PresenceRequest,
}

impl ChatService {
    /// Certify `device_id`'s P-256 key as a phone of this device's account in
    /// `workspace_id` (the person's presence confirms it). Never from a box.
    pub fn certify_phone(&mut self, workspace_id: Uuid, device_id: Uuid, public_key: &[u8]) -> Result<DeviceCertificate> {
        if self.delegated || self.author.kind == hive_core::ActorKind::Service {
            return Err(ChatError::Unauthorized("only a person's own device enrolls their phone".into()));
        }
        let account = self.author.account_id.ok_or_else(|| ChatError::Unauthorized("this device has no account".into()))?;
        let certificate = DeviceCertificate::issue_p256(&self.account_keypair, account, device_id, public_key, Timestamp::now());
        let config_id = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(config_id, workspace_id, SessionEvent::DeviceCertificateAdded { certificate: certificate.clone() })?;
        Ok(certificate)
    }

    /// Draft a box admission for the phone `phone` to sign itself: as the
    /// person the phone is certified for, on the config log, at this box's
    /// head. Nothing is written.
    pub fn draft_phone_admission(&mut self, workspace_id: Uuid, phone: Uuid, ttl_ms: i64, account: Option<Uuid>) -> Result<(PhoneDraft, Uuid)> {
        let ttl = ttl_ms.clamp(60_000, hive_core::admission::MAX_ADMISSION_TTL_MS - 60_000);
        let now = (Timestamp::now().inner().unix_timestamp_nanos() / 1_000_000) as i64;
        let invite = Uuid::new_v4();
        let payload = SessionEvent::JoinApproved {
            subject: hive_core::authorization::invite_subject(invite),
            admission: Some(hive_core::admission::Admission::service(now + ttl, account)),
        };
        Ok((self.draft_for_phone(workspace_id, phone, payload)?, invite))
    }

    /// Draft `payload` on `workspace_id`'s config log as the person phone
    /// `phone` is certified for, to be signed by that phone.
    pub fn draft_for_phone(&mut self, workspace_id: Uuid, phone: Uuid, payload: SessionEvent) -> Result<PhoneDraft> {
        let config_id = workspace_config_session_id(workspace_id);
        let (config, timeline) = self.store.load_config(workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        let config = config.ok_or_else(|| ChatError::Unauthorized("the workspace configuration hasn't synced to this box yet".into()))?;
        let roster = timeline.current();
        let device = roster
            .devices
            .get(&phone)
            .filter(|d| d.phone && !d.revoked)
            .ok_or_else(|| ChatError::Unauthorized("that isn't an enrolled phone of this workspace".into()))?;
        let member = config
            .members
            .iter()
            .find(|m| m.actor.account_id.is_some_and(|a| device.accounts.contains(&a)))
            .ok_or_else(|| ChatError::Unauthorized("that phone's person isn't a member here".into()))?;
        let mut actor = member.actor.clone();
        actor.device_id = Some(phone);
        let mut env = SessionEventEnvelope::new(config_id, workspace_id, self.store.max_sequence(config_id)?.unwrap_or(0) + 1, payload);
        env.lamport = self.store.max_lamport(config_id)?.saturating_add(1);
        env.actor_stamp = Some(ActorStamp {
            actor,
            recorded_at: Timestamp::now(),
            cfg_frontier: timeline.head(),
            presence: None,
            runtime_effect: false,
            prev: self.store.last_signed_by(config_id, phone)?,
            sig_alg: SigAlg::P256,
        });
        let request = self.presence_request(&env)?;
        Ok(PhoneDraft { env, request })
    }

    /// Stamp the person's passkey assertion into `draft` and return the
    /// preimage the phone signs.
    pub fn phone_preimage(draft: &mut PhoneDraft, assertion: PresenceAssertion) -> Vec<u8> {
        if let Some(s) = draft.env.actor_stamp.as_mut() {
            s.presence = Some(assertion);
        }
        draft.env.clear_raw();
        hive_core::crypto::envelope_preimage(&draft.env)
    }

    /// Append `draft` with the phone's signature (64 bytes, P-256), after
    /// checking it against the phone's certificate here and judging it as
    /// every fold will. The box's own key never signs it.
    pub fn append_phone_signed(&mut self, draft: PhoneDraft, signature: Vec<u8>) -> Result<SessionEventEnvelope> {
        let mut env = draft.env;
        let phone = env.actor_stamp.as_ref().and_then(|s| s.actor.device_id).ok_or_else(|| ChatError::Unauthorized("no phone".into()))?;
        if self.store.has_event(env.event_id)? {
            return Err(ChatError::Unauthorized("already committed".into()));
        }
        env.signer_device_id = Some(phone);
        env.signature = Some(signature);
        let (config, timeline) = self.store.load_config(env.workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        let key = timeline
            .current()
            .devices
            .get(&phone)
            .filter(|d| d.phone && !d.revoked)
            .map(|d| d.public_key.clone())
            .ok_or_else(|| ChatError::Unauthorized("that isn't an enrolled phone of this workspace".into()))?;
        hive_core::crypto::verify_envelope(&env, &key).map_err(|_| ChatError::Unauthorized("the phone's signature doesn't verify".into()))?;
        self.authorize_as_fold(&env, config.as_ref(), &timeline)?;
        self.store.append_envelope(&env)?;
        Ok(env)
    }
}

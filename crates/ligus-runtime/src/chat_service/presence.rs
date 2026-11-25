//! Authoring events that need passkey presence (Phase 0, B3).
//!
//! When the fold would refuse a drafted event for lack of presence
//! ([`ligus_core::authz::PRESENCE_REQUIRED`]), `append_signed` asks this
//! device's [`PresenceProvider`] (the desktop's browser hand-off) for an
//! assertion over the event digest, attaches it inside the stamp, re-signs
//! and appends. A host without a provider (`ligus serve`) captures the draft
//! instead ([`ChatService::capture_presence`]): the phone signs its digest
//! with a passkey and [`ChatService::commit_presence`] appends it with the
//! digest fields frozen at prepare time.

use std::sync::Arc;

use ligus_core::authz::PRESENCE_REQUIRED;
use ligus_core::crypto::sign_envelope;
use ligus_core::presence::{self, PasskeyCredential, PresenceAssertion};
use ligus_core::{workspace_config_session_id, SessionEvent, SessionEventEnvelope};
use uuid::Uuid;

use super::{ChatError, ChatService, Result};

#[cfg(any(test, feature = "test-presence"))]
pub use ligus_core::testkit::test_recovery_key;

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
    /// First 16 hex characters (64 bits) of the digest, shown to the person.
    pub digest_short: String,
    /// What is being authorized, in words.
    pub summary: String,
    /// The workspace's relying party (its `WorkspaceRulesSet.presence`), which
    /// the ceremony must use and the fold verifies against.
    pub policy: presence::PresencePolicy,
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

/// A name or label someone else chose, as the confirmation sheet shows it:
/// bidi and invisible characters removed (so it can't reorder the sentence
/// around it), quoted (so `Eve as Viewer;` can't pass for the role), at most
/// 64 characters.
fn shown(raw: &str) -> String {
    let clean = ligus_proto::names::clean_display_name(raw);
    let mut s: String = clean.chars().take(64).collect();
    if s.len() < clean.len() {
        s.push('…');
    }
    format!("\u{201C}{s}\u{201D}")
}

/// A short description of what an event authorizes (the confirmation sheet).
pub fn summarize(payload: &SessionEvent) -> String {
    match payload {
        SessionEvent::MemberAdded { member, .. } => format!("Add {} as {:?}", shown(&member.actor.display_name), member.role),
        SessionEvent::MemberRemoved { member_id, .. } => format!("Remove member {member_id}"),
        SessionEvent::MemberRoleChanged { change, .. } => format!("Change {}'s role to {:?}", change.member_id, change.new_role),
        SessionEvent::DeviceRevoked { device_id, .. } => format!("Revoke device {device_id}"),
        SessionEvent::WorkspaceKeyRotated { rotation } => {
            format!("Rotate the workspace key to version {} ({} devices)", rotation.version, rotation.sealed.len())
        }
        SessionEvent::PasskeyRegistered { credential } => format!("Register passkey {}", shown(&credential.label)),
        SessionEvent::PasskeyRemoved { .. } => "Remove a passkey".into(),
        // The relying party decides where every later passkey ceremony
        // happens: the sheet names it, never just the version.
        SessionEvent::WorkspaceRulesSet { version, presence: Some(p) } => {
            format!("Set rules version {version} with passkeys on {} ({})", p.rp_id, p.origins.join(", "))
        }
        SessionEvent::WorkspaceRulesSet { version, .. } => format!("Switch the workspace to rules version {version}"),
        SessionEvent::DeviceCertificateAdded { certificate } if certificate.alg == ligus_core::crypto::SigAlg::P256 => {
            // The fingerprint the person compared with the phone's screen.
            format!(
                "Enroll phone {} (key {}) as one of your devices",
                certificate.device_id,
                crate::device_enroll::fingerprint(&certificate.device_public_key)
            )
        }
        SessionEvent::DeviceCertificateAdded { certificate } => format!(
            "Add device {} (key {}) as one of your devices",
            certificate.device_id,
            crate::device_enroll::fingerprint(&certificate.device_public_key)
        ),
        SessionEvent::ProposalApplyRequested { request } => {
            format!("Ask host {} to apply a proposal", request.host_device_id)
        }
        SessionEvent::ProposalVoteCast { approval, .. } => {
            if approval.approved { "Approve a proposal".into() } else { "Reject a proposal".into() }
        }
        SessionEvent::ChatLockdownCleared { .. } => "Clear a chat's lockdown".into(),
        SessionEvent::DearthModeChanged { .. } => "Change dearth mode".into(),
        // The sheet says what agents will reach on every host that runs them.
        SessionEvent::WorkspaceSandboxSet { grants } => {
            let paths = |g: &ligus_core::sandbox::PathGrants| g.read_only.len() + g.read_write.len();
            let agent_paths: usize = grants.agents.values().map(|a| a.read_only.len() + a.read_write.len()).sum();
            let shells: Vec<String> = grants.agents.iter().filter(|(_, a)| a.shell).map(|(id, _)| id.to_string()).collect();
            let mut s = format!(
                "Set the agent sandbox grants: {} path(s) for every agent ({} read-write), {} for single agents",
                paths(&grants.workspace),
                grants.workspace.read_write.len(),
                agent_paths
            );
            if shells.is_empty() {
                s.push_str("; no agent keeps a shell for other members");
            } else {
                s.push_str(&format!("; the shell for other members' turns granted to agent(s) {}", shells.join(", ")));
            }
            s
        }
        SessionEvent::JoinApproved { subject, .. } if subject.starts_with("invite:") => {
            "Let someone join with a single-use invite".into()
        }
        SessionEvent::JoinApproved { subject, .. } => format!("Let {} join", shown(subject)),
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
            policy: timeline.current().presence_policy.clone(),
            challenge,
            kind: env.payload.kind_str().to_string(),
            account_id,
            credential_ids,
            digest_short: ligus_core::keys::hex(&challenge[..8]),
            summary: summarize(&env.payload),
        })
    }

    /// `append_signed`'s answer to a presence refusal: ask the provider, or
    /// capture the draft, or refuse.
    pub(super) fn with_presence(&mut self, env: &mut SessionEventEnvelope) -> Result<()> {
        // Compiled only into tests (this crate's, or a dev-dependency's
        // `test-presence`); a normal build has no path to it.
        #[cfg(any(test, feature = "test-presence"))]
        if self.presence_provider.is_none() && self.test_presence {
            return self.test_presence(env);
        }
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

    /// Tests only: confirm `env` with this account's test recovery key
    /// ([`test_recovery_key`]), registering it first when the workspace
    /// doesn't hold it (with the founder's or an Admin's grant, like a person
    /// would), then re-stamping `env` after that registration.
    #[cfg(any(test, feature = "test-presence"))]
    fn test_presence(&mut self, env: &mut SessionEventEnvelope) -> Result<()> {
        let wid = env.workspace_id;
        if self.delegated {
            return Err(ChatError::Unauthorized(PRESENCE_REQUIRED.into()));
        }
        // Presence is an account's: an account-less test author gets one.
        if self.author.account_id.is_none() {
            self.author.account_id = Some(Uuid::new_v4());
            if let Some(s) = env.actor_stamp.as_mut() {
                s.actor.account_id = self.author.account_id;
            }
        }
        let account = self.author.account_id.unwrap_or_default();
        let key = test_recovery_key(account);
        let config_id = workspace_config_session_id(wid);
        let cred_id = key.credential(account, "").credential_id;
        let held = self.passkeys_of(wid, account)?.iter().any(|(id, _)| *id == cred_id);
        if !held {
            self.test_presence = false;
            let r = self
                .publish_identity(config_id, wid)
                .and_then(|_| self.register_passkey(wid, key.credential(account, "Test recovery key"), &|req| Ok(key.assert(&req.challenge))));
            self.test_presence = true;
            r?;
            // Sort after the registration, at the new head.
            env.sequence = self.store.max_sequence(env.workspace_id, env.session_id)?.unwrap_or(0) + 1;
            let log_head = self.store.lamport_head(env.workspace_id, env.session_id)?;
            ligus_core::lamport::stamp_next(env, log_head);
            let (_, timeline) = self.store.load_config(wid, self.trusted_founder.as_deref(), self.local_workspace_id)?;
            // The registration is this device's newest event on the log: the
            // chain link moves past it too (else both name the same link).
            let prev = self.store.last_signed_by(env.workspace_id, env.session_id, self.device_id, env.lamport)?;
            if let Some(s) = env.actor_stamp.as_mut() {
                s.cfg_frontier = timeline.head();
                s.prev = prev;
            }
        }
        let a = key.assert(&presence::challenge(env));
        if let Some(stamp) = env.actor_stamp.as_mut() {
            stamp.presence = Some(a);
        }
        env.clear_raw();
        sign_envelope(env, self.device_id, &self.keypair);
        Ok(())
    }

    /// Tests only: answer presence prompts with this account's test recovery
    /// key (registered on first use). Also gives an account-less test author
    /// an account, since presence is an account's.
    #[cfg(any(test, feature = "test-presence"))]
    pub fn enable_test_presence(&mut self) {
        if self.author.account_id.is_none() {
            self.author.account_id = Some(Uuid::new_v4());
        }
        self.test_presence = true;
    }

    /// Tests only: no automatic presence (the refusal paths).
    #[cfg(any(test, feature = "test-presence"))]
    pub fn disable_test_presence(&mut self) {
        self.test_presence = false;
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
        if self.store.has_event_in(env.workspace_id, env.event_id)? {
            return Err(ChatError::Unauthorized("already committed".into()));
        }
        if let Some(stamp) = env.actor_stamp.as_mut() {
            stamp.presence = Some(assertion);
        }
        env.clear_raw();
        env.sequence = self.store.max_sequence(env.workspace_id, env.session_id)?.unwrap_or(0) + 1;
        // This device may have written on the log since the draft (the
        // confirmation took a while): link past that, as it would have. The
        // confirmation covers the lamport, not the link. A write of its own
        // at the draft's lamport or later can't be linked past without a new
        // confirmation: the store refuses that one (`StaleDraft`) rather than
        // fork this device's chain, and the caller prepares it again at the
        // head and asks the person again (serve answers 409).
        let prev = self.store.last_signed_by(env.workspace_id, env.session_id, self.device_id, env.lamport)?;
        if let Some(stamp) = env.actor_stamp.as_mut() {
            stamp.prev = prev;
        }
        sign_envelope(&mut env, self.device_id, &self.keypair);
        let (config, timeline) = self.store.load_config(env.workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        self.authorize_as_fold(&env, config.as_ref(), &timeline)?;
        self.store_authored(&env)?;
        self.changes.local(&env);
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
        // The founder's first passkey can be the workspace's first act (the
        // desktop's create flow): found the config log first.
        let config_id = self.ensure_workspace_config(workspace_id)?;
        // Asked once more, at the new head, if a write of this device landed
        // on the config log while the person confirmed (storing it would
        // fork this device's chain: `StaleDraft`); a second race goes back to
        // the caller.
        let mut asked = 0;
        loop {
            asked += 1;
            let mut env = self.draft_signed(config_id, workspace_id, SessionEvent::PasskeyRegistered { credential: credential.clone() })?;
            let request = self.presence_request(&env)?;
            let proof = prove(&request).map_err(|e| ChatError::Unauthorized(format!("passkey: {e}")))?;
            if let Some(stamp) = env.actor_stamp.as_mut() {
                stamp.presence = Some(proof);
            }
            env.clear_raw();
            sign_envelope(&mut env, self.device_id, &self.keypair);
            let (config, timeline) = self.store.load_config(workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
            self.authorize_as_fold(&env, config.as_ref(), &timeline)?;
            match self.store_authored(&env) {
                Err(ChatError::Store(crate::event_store::EventStoreError::StaleDraft)) if asked < 2 => continue,
                r => r?,
            }
            self.changes.local(&env);
            // A workspace this build founded moves to its founding rules (3)
            // with the founder's first credential: its genesis is rules 2,
            // presence-free, which a 0.3 build accepts, so the switch is an
            // ordinary Owner act with presence that makes 0.3 builds
            // read-only there (identity model v2, "Rules 3 in 0.4").
            if let Err(e) = self.adopt_founding_rules(workspace_id, prove) {
                tracing::warn!(target: "identity", error = %e, "moving the new workspace to its founding rules failed; Settings offers it");
            }
            return Ok(env);
        }
    }

    /// Switch a workspace this device founded, still its founder's alone, to
    /// this build's founding rules, confirmed by `prove`. A no-op elsewhere.
    fn adopt_founding_rules(
        &mut self,
        workspace_id: Uuid,
        prove: &dyn Fn(&PresenceRequest) -> std::result::Result<PresenceAssertion, String>,
    ) -> Result<()> {
        let config_id = ligus_core::workspace_config_session_id(workspace_id);
        let Some(config) = self.load_in(workspace_id, config_id)? else { return Ok(()) };
        if config.authz.rules >= self.founding_rules || config.creator_actor_id != self.author.id || config.members.len() != 1 {
            return Ok(());
        }
        let mut env = self.draft_signed(config_id, workspace_id, SessionEvent::WorkspaceRulesSet { version: self.founding_rules, presence: None })?;
        let request = self.presence_request(&env)?;
        let proof = prove(&request).map_err(|e| ChatError::Unauthorized(format!("passkey: {e}")))?;
        if let Some(stamp) = env.actor_stamp.as_mut() {
            stamp.presence = Some(proof);
        }
        env.clear_raw();
        sign_envelope(&mut env, self.device_id, &self.keypair);
        let (config, timeline) = self.store.load_config(workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        self.authorize_as_fold(&env, config.as_ref(), &timeline)?;
        self.store_authored(&env)?;
        self.changes.local(&env);
        Ok(())
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

    /// Let member `member_id` register a first passkey again (B.4.6): an
    /// Admin's presence-confirmed re-grant of their role, for an account whose
    /// credentials were all removed (or that never had one under rules v2).
    pub fn regrant_passkey(&mut self, workspace_id: Uuid, member_id: &str) -> Result<SessionEventEnvelope> {
        let config_id = workspace_config_session_id(workspace_id);
        let role = self
            .load_in(workspace_id, config_id)?
            .and_then(|c| c.members.iter().find(|m| m.id == member_id).map(|m| m.role))
            .ok_or_else(|| ChatError::Unauthorized("no such member".into()))?;
        self.append_signed(
            config_id,
            workspace_id,
            SessionEvent::MemberRoleChanged {
                change: ligus_core::events::MemberRoleChange { member_id: member_id.to_string(), old_role: role, new_role: role }, watermark: Vec::new(),
            },
        )
    }

    /// Whether this device's certificate is in force on `workspace_id`'s
    /// config log (under rules v2 a new device of an account with a passkey
    /// is only once [`ChatService::confirm_device`] ran).
    pub fn device_confirmed(&self, workspace_id: Uuid) -> Result<bool> {
        let Some(account) = self.author.account_id else { return Ok(false) };
        let (_, timeline) = self.store.load_config(workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        let head = timeline.current();
        Ok(head.device_trusted(self.device_id) && head.account_certified(account, self.device_id))
    }

    /// Confirm this device on `workspace_id`: publish its certificate on the
    /// config log, asking for a passkey when the rules need one (rules v2, an
    /// account with a passkey). The identity publications Ligus makes on its
    /// own never ask ([`ChatService::publish_identity`]). Returns whether it
    /// published.
    pub fn confirm_device(&mut self, workspace_id: Uuid) -> Result<bool> {
        if self.delegated || self.device_confirmed(workspace_id)? {
            return Ok(false);
        }
        let Some(account_id) = self.author.account_id else { return Ok(false) };
        let config_id = workspace_config_session_id(workspace_id);
        self.publish_identity(config_id, workspace_id)?;
        if self.device_confirmed(workspace_id)? {
            return Ok(true);
        }
        let certificate = ligus_core::crypto::DeviceCertificate::issue(
            &self.account_keypair,
            account_id,
            &self.keypair.public_key_bytes(),
            ligus_core::Timestamp::now(),
        );
        self.append_signed(config_id, workspace_id, SessionEvent::DeviceCertificateAdded { certificate })?;
        Ok(true)
    }

    /// The relying party `workspace_id`'s passkeys are under (its signed
    /// `WorkspaceRulesSet.presence`, `ligus.ai` until one is set).
    pub fn workspace_presence_policy(&self, workspace_id: Uuid) -> Result<presence::PresencePolicy> {
        let (_, timeline) = self.store.load_config(workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        Ok(timeline.current().presence_policy.clone())
    }

    /// The relying party this device writes into a workspace it founds (the
    /// deployment's setting; `ligus.ai` by default). Authoring only: the fold
    /// reads the policy from the log, never from here.
    pub fn set_deployment_presence_policy(&mut self, policy: presence::PresencePolicy) {
        self.deployment_presence = policy;
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

    /// Set the workspace rules version (Owner, with presence). Every
    /// workspace starts on version 2 (genesis); there is no version 1. The
    /// account must hold a passkey and a recovery key first, so a sole Owner
    /// can't lock themself out.
    pub fn set_workspace_rules(&mut self, workspace_id: Uuid, version: u32) -> Result<SessionEventEnvelope> {
        if version < ligus_core::authz::RULES_GENESIS {
            return Err(ChatError::Unauthorized("rules version 1 doesn't exist in Ligus".into()));
        }
        let account = self.author.account_id.ok_or_else(|| ChatError::Unauthorized("changing the rules needs an account".into()))?;
        let creds = self.passkeys_of(workspace_id, account)?;
        if !creds.iter().any(|(_, recovery)| *recovery) || !creds.iter().any(|(_, recovery)| !*recovery) {
            return Err(ChatError::Unauthorized("register a passkey and write down a recovery key first".into()));
        }
        self.append_signed(workspace_config_session_id(workspace_id), workspace_id, SessionEvent::WorkspaceRulesSet { version, presence: None })
    }

    /// Whether `workspace_id` is secured for its founder: this account holds
    /// a passkey and a recovery key there (the desktop's create flow asks for
    /// both; a workspace founded elsewhere shows "finish securing").
    pub fn workspace_secured(&self, workspace_id: Uuid) -> Result<bool> {
        let Some(account) = self.author.account_id else { return Ok(false) };
        let creds = self.passkeys_of(workspace_id, account)?;
        Ok(creds.iter().any(|(_, r)| *r) && creds.iter().any(|(_, r)| !*r))
    }

    /// Re-sign a test envelope with this device's key.
    #[cfg(test)]
    pub(crate) fn sign_for_test(&self, env: &mut SessionEventEnvelope) {
        sign_envelope(env, self.device_id, &self.keypair);
    }
}

#[cfg(test)]
mod summary_tests {
    use super::summarize;
    use ligus_core::identity::{ActorIdentity, ActorKind, WorkspaceMember, WorkspaceRole};
    use ligus_core::SessionEvent;

    /// Review P-A5: confirming a relying-party change shows the new RP,
    /// not only the version (the sheet is what the person consents to).
    #[test]
    fn a_relying_party_change_names_the_relying_party() {
        let e = SessionEvent::WorkspaceRulesSet { version: 2, presence: Some(ligus_core::presence::PresencePolicy::for_rp("evil.example")) };
        let s = summarize(&e);
        assert!(s.contains("evil.example"), "{s}");
    }

    /// The joiner chooses their own name, and the approver reads it on the
    /// passkey sheet: it must not reorder the sentence or fake the role.
    #[test]
    fn a_joiners_name_cannot_rewrite_the_sheet() {
        let ev = |name: &str| SessionEvent::MemberAdded {
            invite: None,
            proof: None,
            member: WorkspaceMember {
                id: "m".into(),
                actor: ActorIdentity::new("a", name, ActorKind::Human),
                role: WorkspaceRole::Admin,
                title: String::new(),
                index: 0,
                joined_at: Default::default(),
            },
        };
        let s = summarize(&ev("Eve as Viewer;\u{202E} \u{200B}x"));
        assert!(!s.chars().any(ligus_proto::names::is_hidden_name_char), "{s:?}");
        assert_eq!(s, "Add \u{201C}Eve as Viewer; x\u{201D} as Admin");
        let long = summarize(&ev(&"n".repeat(500)));
        assert!(long.chars().count() < 100, "{long}");
    }
}

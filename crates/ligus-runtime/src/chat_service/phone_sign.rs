//! Events a phone signs with its own key, relayed by a box (Phase B).
//!
//! A box is its own service member: it never signs as the phone's person.
//! For what only a person may do, the box drafts the event **as the phone's
//! person, to be signed by the phone's own P-256 device key**: it fixes the
//! digest fields, the phone confirms with the person's passkey over the
//! presence challenge, then signs the preimage; the box checks the signature
//! against the phone's certificate, judges the event exactly as every fold
//! will, and appends it. The box can withhold such an event; it can't forge
//! or alter one as long as the phone reads the preimage before signing it
//! (`ligus_core::admission::check_phone_admission_preimage`,
//! `ligus_core::phone_action::check_phone_action_preimage`): the bytes come
//! from the box. It carries box admissions (the phone tier allows nothing
//! else about membership) and the typed phone actions
//! ([`ligus_core::phone_action::PhoneAction`]: post, vote, answer, clear a
//! lockdown, ask a host to apply and withdraw that request).

use ligus_core::crypto::{DeviceCertificate, SigAlg};
use ligus_core::presence::PresenceAssertion;
use ligus_core::phone_action::PhoneAction;
use ligus_core::{workspace_config_session_id, ActorStamp, SessionEvent, SessionEventEnvelope, Timestamp};
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
        if self.delegated || self.author.kind == ligus_core::ActorKind::Service {
            return Err(ChatError::Unauthorized("only a person's own device enrolls their phone".into()));
        }
        let account = self.author.account_id.ok_or_else(|| ChatError::Unauthorized("this device has no account".into()))?;
        // A phone's device id is the one its Secure Enclave key derives.
        if device_id != ligus_core::crypto::device_id_for(SigAlg::P256, public_key) {
            return Err(ChatError::Unauthorized("the phone's device id isn't the one its key derives".into()));
        }
        let certificate = DeviceCertificate::issue_p256(&self.account_keypair, account, public_key, Timestamp::now());
        let config_id = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(config_id, workspace_id, SessionEvent::DeviceCertificateAdded { certificate: certificate.clone() })?;
        Ok(certificate)
    }

    /// Draft a box admission for the phone `phone` to sign itself: as the
    /// person the phone is certified for, on the config log, at this box's
    /// head. Nothing is written.
    /// Returns the draft, its invite and the redeem key's seed (for the
    /// admission code only).
    pub fn draft_phone_admission(
        &mut self,
        workspace_id: Uuid,
        phone: Uuid,
        ttl_ms: i64,
        request: Option<&crate::box_admission::JoinRequest>,
    ) -> Result<(PhoneDraft, Uuid, [u8; 32])> {
        let (payload, invite, seed) = super::box_admission_payload(ttl_ms, request)?;
        Ok((self.draft_for_phone(workspace_id, phone, payload)?, invite, seed))
    }

    /// Draft `payload` on `workspace_id`'s config log as the person phone
    /// `phone` is certified for, to be signed by that phone.
    pub fn draft_for_phone(&mut self, workspace_id: Uuid, phone: Uuid, payload: SessionEvent) -> Result<PhoneDraft> {
        self.draft_for_phone_on(workspace_config_session_id(workspace_id), workspace_id, phone, payload)
    }

    /// The member phone `phone` is certified for here (its actor, stamped
    /// with the phone as its device). Refused for a revoked or unknown
    /// phone, or one whose person isn't a member.
    pub fn phone_person(&self, workspace_id: Uuid, phone: Uuid) -> Result<ligus_core::ActorIdentity> {
        let (config, timeline) = self.store.load_config(workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        let config = config.ok_or_else(|| ChatError::Unauthorized("the workspace configuration hasn't synced to this box yet".into()))?;
        let roster = timeline.current();
        let device = roster
            .devices
            .get(&phone)
            .filter(|d| d.phone && !d.revoked)
            .ok_or_else(|| ChatError::Unauthorized("that isn't an enrolled phone of this workspace".into()))?;
        // The one member who holds this phone (someone else who certified
        // its key for themselves leaves it naming no one: fail closed).
        let mut people = config
            .members
            .iter()
            .filter(|m| m.actor.kind == ligus_core::ActorKind::Human && m.actor.account_id.is_some_and(|a| device.holders.contains(&a)));
        let member = match (people.next(), people.next()) {
            (Some(m), None) => m,
            _ => return Err(ChatError::Unauthorized("that phone's person isn't a member here".into())),
        };
        let mut actor = member.actor.clone();
        actor.device_id = Some(phone);
        Ok(actor)
    }

    /// The accounts of the person who admitted this box (repos review F12,
    /// the box owner): the Admins whose `JoinApproved` issued the admission
    /// this box's own `MemberAdded` redeemed, while they are still members
    /// at Admin or above. Empty when this device wasn't admitted that way.
    pub fn box_owner_accounts(&self, workspace_id: Uuid) -> Result<std::collections::BTreeSet<Uuid>> {
        let mut out = std::collections::BTreeSet::new();
        let (config, timeline) = self.store.load_config(workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        let Some(config) = config else { return Ok(out) };
        let me = self.author.id.as_str();
        let invite = self.store.config_envelopes(workspace_id)?.into_iter().find_map(|e| match &e.payload {
            SessionEvent::MemberAdded { member, invite: Some(i), .. }
                if member.id == me
                    && e.signer_device_id == Some(self.device_id)
                    && e.actor_stamp.as_ref().is_some_and(|st| st.actor.id == me) =>
            {
                Some(*i)
            }
            _ => None,
        });
        let Some(invite) = invite else { return Ok(out) };
        let roster = timeline.current();
        for approver in config.guards.join_approvals.get(&ligus_core::authorization::invite_subject(invite)).into_iter().flatten() {
            if let Some(m) = roster.members.get(approver).filter(|m| m.role.rank() >= ligus_core::WorkspaceRole::Admin.rank()) {
                out.extend(m.account_id);
            }
        }
        Ok(out)
    }

    /// Whether phone `phone` is certified here (enrolled, not revoked, its
    /// person a member) for one of `accounts`.
    pub fn phone_of_accounts(&self, workspace_id: Uuid, phone: Uuid, accounts: &std::collections::BTreeSet<Uuid>) -> bool {
        let Ok(person) = self.phone_person(workspace_id, phone) else { return false };
        person.account_id.is_some_and(|a| accounts.contains(&a))
    }

    /// Draft `payload` on `session_id` (a chat of `workspace_id`, or its
    /// config log) as the person phone `phone` is certified for, to be signed
    /// by that phone, at this box's head. Nothing is written.
    pub fn draft_for_phone_on(&mut self, session_id: Uuid, workspace_id: Uuid, phone: Uuid, payload: SessionEvent) -> Result<PhoneDraft> {
        let actor = self.phone_person(workspace_id, phone)?;
        let (_, timeline) = self.store.load_config(workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        let mut env = SessionEventEnvelope::new(session_id, workspace_id, self.store.max_sequence(session_id)?.unwrap_or(0) + 1, payload);
        // One past the newest event of the log this box holds, named as the
        // stamp's `log_head` (`ligus_core::lamport`): the phone signs a lamport
        // every fold can account for.
        let log_head = self.store.lamport_head(session_id)?;
        env.lamport = ligus_core::lamport::next(log_head);
        env.actor_stamp = Some(ActorStamp {
            actor,
            recorded_at: Timestamp::now(),
            cfg_frontier: timeline.head(),
            presence: None,
            runtime_effect: false,
            prev: self.store.last_signed_by(session_id, phone, env.lamport)?,
            sig_alg: SigAlg::P256,
            log_head,
        });
        let request = self.presence_request(&env)?;
        Ok(PhoneDraft { env, request })
    }

    /// Draft the event a phone asked for (`action`), as its person, and say
    /// whether the rules need the person's passkey presence for it: the box
    /// judges the unsigned draft as every fold will, so anything the fold
    /// would refuse for another reason is refused here, before the phone
    /// signs anything. Nothing is written.
    pub fn draft_phone_action(&mut self, workspace_id: Uuid, phone: Uuid, action: &PhoneAction) -> Result<(PhoneDraft, bool)> {
        if let Some(why) = action.malformed() {
            return Err(ChatError::Unauthorized(why.into()));
        }
        let actor = self.phone_person(workspace_id, phone)?;
        let chat = self
            .load(action.chat_id())?
            .filter(|c| c.workspace_id == workspace_id)
            .ok_or_else(|| ChatError::Unauthorized("no such chat in this workspace".into()))?;
        let payload = match action {
            PhoneAction::Post { text, .. } => {
                let mut m = ligus_core::ChatMessage::new(ligus_core::MessageRole::User, actor.display_name.clone(), text.clone());
                m.actor_identity = Some(actor.clone());
                SessionEvent::MessageAppended { message: m }
            }
            PhoneAction::Vote { proposal_id, diff_digest, approved, .. } => {
                if !chat.proposals.iter().any(|p| p.id == *proposal_id) {
                    return Err(ChatError::Unauthorized("no such proposal in this chat".into()));
                }
                SessionEvent::ProposalVoteCast {
                    proposal_id: *proposal_id,
                    approval: ligus_core::ProposalApproval {
                        actor_id: actor.id.clone(),
                        role: chat.role_of(&actor.id),
                        approved: *approved,
                        created_at: Timestamp::now(),
                        diff_digest: Some(diff_digest.clone()),
                    },
                }
            }
            PhoneAction::Answer { ask_id, answer, option_index, .. } => {
                if !chat.flow.asks.get(ask_id).is_some_and(|a| a.is_open()) {
                    return Err(ChatError::Unauthorized("that question is already answered or closed".into()));
                }
                SessionEvent::FlowRecorded {
                    record: ligus_core::flow::FlowRecord::HumanAnswered(ligus_core::flow::HumanAnswered {
                        ask_id: *ask_id,
                        answer: answer.clone(),
                        option_index: *option_index,
                        by: actor.display_name.clone(),
                        at_ms: ligus_core::flow::now_ms(),
                    }),
                }
            }
            PhoneAction::ClearLockdown { chat_id, raises } => {
                // Only lockdowns in force here, each named by its raise: a
                // phone showing an older view prepares again.
                let active: Vec<Uuid> =
                    self.workspace_guards(workspace_id)?.lockdowns_of(*chat_id).iter().filter_map(|l| l.raise_event_id).collect();
                if raises.iter().any(|r| !active.contains(r)) {
                    return Err(ChatError::Unauthorized("a lockdown it names isn't in force here: refresh and prepare again".into()));
                }
                SessionEvent::ChatLockdownCleared { chat_id: *chat_id, raises: raises.clone() }
            }
            PhoneAction::RequestApply { proposal_id, diff_digest, host_device_id, expected_base_commit, repo_id, .. } => {
                SessionEvent::ProposalApplyRequested {
                    request: ligus_core::workspace_repo::ApplyRequest {
                        id: Uuid::new_v4(),
                        proposal_id: *proposal_id,
                        diff_digest: diff_digest.clone(),
                        host_device_id: *host_device_id,
                        expected_base_commit: expected_base_commit.clone(),
                        repo_id: *repo_id,
                    },
                }
            }
            PhoneAction::WithdrawApply { proposal_id, request_id, .. } => {
                SessionEvent::ProposalApplyWithdrawn { request_id: *request_id, proposal_id: *proposal_id }
            }
            PhoneAction::ProposeMemory { chat_id, op, rationale } => {
                // A new entry is named by its proposal; the source chat's
                // egress is recorded (it can only narrow who reads the note).
                let proposal_id = Uuid::new_v4();
                let op = match op {
                    ligus_core::memory::MemoryOp::Create { title, body, tags, .. } => {
                        ligus_core::memory::MemoryOp::Create { entry_id: proposal_id, title: title.clone(), body: body.clone(), tags: tags.clone() }
                    }
                    other => other.clone(),
                };
                let mut source = ligus_core::memory::MemorySource { chat_id: Some(*chat_id), ..Default::default() };
                self.stamp_memory_egress(workspace_id, &mut source);
                SessionEvent::FlowRecorded {
                    record: ligus_core::flow::FlowRecord::MemoryProposed(ligus_core::memory::MemoryProposal {
                        proposal_id,
                        op,
                        proposer: String::new(),
                        source,
                        rationale: rationale.clone(),
                    }),
                }
            }
        };
        let session_id = action.session_id(workspace_id);
        let draft = self.draft_for_phone_on(session_id, workspace_id, phone, payload)?;
        // Judge the unsigned draft as the phone's (the phone tier, the
        // presence table, the action's own rules).
        let mut probe = draft.env.clone();
        probe.signer_device_id = Some(phone);
        let (config, timeline) = self.store.load_config(workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        let presence = match self.authorize_as_fold(&probe, config.as_ref(), &timeline) {
            Ok(()) => false,
            Err(ChatError::Unauthorized(why)) if why == ligus_core::authz::PRESENCE_REQUIRED => true,
            Err(e) => return Err(e),
        };
        Ok((draft, presence))
    }

    /// The preimage of `draft` without presence (an action the rules don't
    /// need a passkey for), for the phone to check and sign.
    pub fn phone_unconfirmed_preimage(draft: &mut PhoneDraft) -> Vec<u8> {
        if let Some(s) = draft.env.actor_stamp.as_mut() {
            s.presence = None;
        }
        draft.env.clear_raw();
        ligus_core::crypto::envelope_preimage(&draft.env)
    }

    /// Stamp the person's passkey assertion into `draft` and return the
    /// preimage the phone signs.
    pub fn phone_preimage(draft: &mut PhoneDraft, assertion: PresenceAssertion) -> Vec<u8> {
        if let Some(s) = draft.env.actor_stamp.as_mut() {
            s.presence = Some(assertion);
        }
        draft.env.clear_raw();
        ligus_core::crypto::envelope_preimage(&draft.env)
    }

    /// Append `draft` with the phone's signature (64 bytes, P-256), after
    /// checking it against the phone's certificate here and judging it as
    /// every fold will. The box's own key never signs it. Stored under a
    /// fresh local sequence (the phone signed the draft's own), and refused
    /// when this phone signed something else on that log since the draft
    /// (its chain link would fork): prepare again.
    pub fn append_phone_signed(&mut self, draft: PhoneDraft, signature: Vec<u8>) -> Result<SessionEventEnvelope> {
        let mut env = draft.env;
        let phone = env.actor_stamp.as_ref().and_then(|s| s.actor.device_id).ok_or_else(|| ChatError::Unauthorized("no phone".into()))?;
        if self.store.has_event(env.event_id)? {
            return Err(ChatError::Unauthorized("already committed".into()));
        }
        if env.actor_stamp.as_ref().and_then(|s| s.prev) != self.store.last_signed_by(env.session_id, phone, u64::MAX)? {
            return Err(ChatError::Unauthorized(STALE_DRAFT.into()));
        }
        env.signer_device_id = Some(phone);
        env.signature = Some(signature);
        let (config, timeline) = self.store.load_config(env.workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        let device = timeline
            .current()
            .devices
            .get(&phone)
            .filter(|d| d.phone && !d.revoked)
            .ok_or_else(|| ChatError::Unauthorized("that isn't an enrolled phone of this workspace".into()))?;
        device.verifying_key(&env).ok_or_else(|| ChatError::Unauthorized("the phone's signature doesn't verify".into()))?;
        self.authorize_as_fold(&env, config.as_ref(), &timeline)?;
        // Under the store's write lock: refused if this phone's chain moved
        // past the draft's link since the check above (another connection).
        match self.store.ingest_authored(&env) {
            Err(crate::event_store::EventStoreError::StaleDraft) => return Err(ChatError::Unauthorized(STALE_DRAFT.into())),
            r => r?,
        };
        self.changes.local(&env);
        Ok(env)
    }
}

// "Add this phone" (`crate::phone_enroll`).
impl ChatService {
    /// Where `code`'s device stands in `workspace_id`.
    pub fn phone_standing(&self, workspace_id: Uuid, code: &crate::phone_enroll::PhoneCode) -> Result<crate::phone_enroll::PhoneStanding> {
        let (_, timeline) = self.store.load_config(workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        let account = self.author.account_id;
        Ok(match timeline.current().devices.get(&code.device_id) {
            None => crate::phone_enroll::PhoneStanding::New,
            Some(d) if d.revoked => crate::phone_enroll::PhoneStanding::Revoked,
            Some(d) if !d.phone => crate::phone_enroll::PhoneStanding::Conflict("that device id is a desktop or box of this workspace, not a phone".into()),
            Some(d) if d.public_key != code.public_key => {
                crate::phone_enroll::PhoneStanding::Conflict("that phone is certified here with a different key".into())
            }
            Some(d) if account.is_some_and(|a| d.accounts.contains(&a)) => crate::phone_enroll::PhoneStanding::AlreadyMine,
            Some(_) => crate::phone_enroll::PhoneStanding::Conflict("that phone is someone else's".into()),
        })
    }

    /// "Add this phone": certify the phone `code` names as this person's
    /// phone in `workspace_id` (a P-256 certificate, the phone tier), after
    /// the checks [`Self::phone_standing`] makes. The person's passkey
    /// presence is asked for by the presence provider (the desktop's sheet,
    /// a CLI's recovery key). Returns the certificate's device id.
    pub fn certify_phone_code(&mut self, workspace_id: Uuid, code: &crate::phone_enroll::PhoneCode) -> Result<Uuid> {
        code.verify().map_err(|e| ChatError::Unauthorized(e.to_string()))?;
        crate::phone_enroll::check_device_id(code.device_id, &code.public_key).map_err(ChatError::Unauthorized)?;
        match self.phone_standing(workspace_id, code)? {
            crate::phone_enroll::PhoneStanding::New => {}
            crate::phone_enroll::PhoneStanding::AlreadyMine => return Ok(code.device_id),
            crate::phone_enroll::PhoneStanding::Revoked => {
                return Err(ChatError::Unauthorized(
                    "this phone was revoked here: in the app, choose Reset this phone's key (on the screen that says it was revoked, or Settings > This phone), then add it again".into(),
                ))
            }
            crate::phone_enroll::PhoneStanding::Conflict(why) => return Err(ChatError::Unauthorized(why)),
        }
        let cert = self.certify_phone(workspace_id, code.device_id, &code.public_key)?;
        Ok(cert.device_id)
    }
}

/// The refusal of a phone draft whose chain link went stale.
pub const STALE_DRAFT: &str = "this phone signed something else here since the draft; prepare it again";

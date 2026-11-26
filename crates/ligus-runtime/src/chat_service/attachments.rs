//! Authoring file-attachment events (issue #118). The fold judges every one
//! (`ligus_core::authz::attachments`); these only build the payloads.

use ligus_core::attachment::{AttachmentPolicy, AttachmentRef, AttachmentRules, SharedAttachment};
use ligus_core::{workspace_config_session_id, ActionProposal, ActorKind, ProposalKind, SessionEvent, WorkspaceRole};
use uuid::Uuid;

use super::{ChatError, ChatService, Result};

impl ChatService {
    /// The attachment policy and blocklist at the workspace config head.
    pub fn attachment_rules(&self, workspace_id: Uuid) -> Result<AttachmentRules> {
        let (_, timeline) = self.store.load_config(workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        Ok(timeline.current().attachments.clone())
    }

    /// This device's author's kind and role on the workspace roster at the
    /// head (a non-member: Viewer).
    pub fn attachment_standing(&self, workspace_id: Uuid) -> Result<(ActorKind, WorkspaceRole)> {
        let (_, timeline) = self.store.load_config(workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        let m = timeline.current().members.get(&self.author.id).cloned();
        Ok(m.map(|m| (m.kind, m.role)).unwrap_or((self.author.kind, WorkspaceRole::Viewer)))
    }

    /// The files shared in a chat (tombstones included).
    pub fn chat_attachments(&self, workspace_id: Uuid, session_id: Uuid) -> Result<std::collections::BTreeMap<Uuid, SharedAttachment>> {
        Ok(self.load_in(workspace_id, session_id)?.map(|s| s.attachments).unwrap_or_default())
    }

    /// Every attachment shared in the workspace's chats (tombstones
    /// included).
    fn workspace_attachments(&self, workspace_id: Uuid) -> Result<Vec<SharedAttachment>> {
        let mut out = Vec::new();
        for session_id in self.store.sessions_of_workspace(workspace_id)? {
            if session_id == workspace_config_session_id(workspace_id) {
                continue;
            }
            if let Some(s) = self.load_in(workspace_id, session_id)? {
                out.extend(s.attachments.into_values());
            }
        }
        Ok(out)
    }

    /// The relay blobs this device should renew now (identity model v2,
    /// 10.5): live attachments of the workspace it shared, or every live one
    /// when `desktop` and this person is an Admin, past a third of their
    /// life ([`crate::attachment_blob::plan_renewals`]). Nothing for the
    /// device-local workspace.
    pub fn blob_renewals_due(&self, workspace_id: Uuid, desktop: bool, now: i64) -> Result<Vec<String>> {
        if self.local_workspace_id == Some(workspace_id) {
            return Ok(Vec::new());
        }
        let (_, role) = self.attachment_standing(workspace_id)?;
        let state = self.store.blob_renewals(workspace_id)?;
        let live = self.workspace_attachments(workspace_id)?;
        Ok(crate::attachment_blob::plan_renewals(&live, &self.author.id, role, desktop, &state, self.device_id, now))
    }

    /// Keep what a renewal pass found ([`crate::attachment_blob::renew_blobs`]).
    pub fn record_blob_renewals(&self, workspace_id: Uuid, outcomes: &[(String, crate::attachment_blob::RenewalOutcome)], now: i64) -> Result<()> {
        use crate::attachment_blob::RenewalOutcome;
        for (blob, r) in outcomes {
            match r {
                RenewalOutcome::Answered => self.store.record_blob_renewed(workspace_id, blob, now)?,
                RenewalOutcome::Gone => self.store.mark_blob_gone(workspace_id, blob, now)?,
                RenewalOutcome::Failed(_) => {}
            }
        }
        Ok(())
    }

    /// The relay no longer has `blob_id` (a download found it gone).
    pub fn mark_blob_gone(&self, workspace_id: Uuid, blob_id: &str, now: i64) -> Result<()> {
        Ok(self.store.mark_blob_gone(workspace_id, blob_id, now)?)
    }

    /// The blobs of the workspace this device saw gone from the relay.
    pub fn gone_blobs(&self, workspace_id: Uuid) -> Result<std::collections::HashSet<String>> {
        Ok(self.store.blob_renewals(workspace_id)?.into_iter().filter(|(_, r)| r.gone_at.is_some()).map(|(b, _)| b).collect())
    }

    /// Sign a share of an uploaded file in a chat.
    pub fn share_attachment(&mut self, session_id: Uuid, workspace_id: Uuid, attachment: AttachmentRef) -> Result<()> {
        if self.local_workspace_id == Some(workspace_id) {
            return Err(ChatError::Unauthorized("files in your own workspace stay on this device".into()));
        }
        self.append_signed(session_id, workspace_id, SessionEvent::AttachmentShared { attachment })?;
        Ok(())
    }

    /// Delete a shared file (its sharer or an Admin). The caller then deletes
    /// the blob from the relay.
    pub fn delete_attachment(&mut self, session_id: Uuid, workspace_id: Uuid, attachment_id: Uuid) -> Result<()> {
        self.append_signed(session_id, workspace_id, SessionEvent::AttachmentDeleted { attachment_id })?;
        Ok(())
    }

    /// Delete every live file `member_id` shared in the workspace's chats
    /// (an Admin removing a member, or a member cleaning up their own).
    /// Returns the deleted references, for the relay deletes.
    pub fn delete_member_attachments(&mut self, workspace_id: Uuid, member_id: &str) -> Result<Vec<AttachmentRef>> {
        let mut out = Vec::new();
        for session_id in self.store.sessions_of_workspace(workspace_id)? {
            if session_id == workspace_config_session_id(workspace_id) {
                continue;
            }
            let Some(session) = self.load_in(workspace_id, session_id)? else { continue };
            for s in session.attachments.values().filter(|s| s.author == member_id && !s.deleted) {
                if self.delete_attachment(session_id, workspace_id, s.attachment.id).is_ok() {
                    out.push(s.attachment.clone());
                }
            }
        }
        Ok(out)
    }

    /// Set the workspace attachment policy (Admin; passkey presence).
    pub fn set_attachment_policy(&mut self, workspace_id: Uuid, policy: AttachmentPolicy) -> Result<()> {
        policy.well_formed().map_err(ChatError::Unauthorized)?;
        self.append_signed(workspace_config_session_id(workspace_id), workspace_id, SessionEvent::AttachmentPolicySet { policy })?;
        Ok(())
    }

    /// Block a file by its content hash (Admin).
    pub fn block_attachment_hash(&mut self, workspace_id: Uuid, sha256: &str) -> Result<()> {
        let sha256 = sha256.trim().to_ascii_lowercase();
        self.append_signed(workspace_config_session_id(workspace_id), workspace_id, SessionEvent::AttachmentHashBlocked { sha256 })?;
        Ok(())
    }

    /// Lift a block (Admin; passkey presence).
    pub fn unblock_attachment_hash(&mut self, workspace_id: Uuid, sha256: &str) -> Result<()> {
        let sha256 = sha256.trim().to_ascii_lowercase();
        self.append_signed(workspace_config_session_id(workspace_id), workspace_id, SessionEvent::AttachmentHashUnblocked { sha256 })?;
        Ok(())
    }

    /// File an agent's offer to share a file as a proposal (a `Decision` that
    /// carries the file's reference, no blob yet). Produced on this device:
    /// once a person approves it, this device uploads the staged file and
    /// signs the share ([`ChatService::approved_offers`]).
    #[allow(clippy::too_many_arguments)]
    pub fn file_attachment_offer(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        title: &str,
        body: &str,
        author_id: &str,
        agent_id: Option<Uuid>,
        offer: AttachmentRef,
        proposal_id: Uuid,
    ) -> Result<Uuid> {
        let mut p = ActionProposal::new(title, ProposalKind::Decision, author_id);
        p.id = proposal_id;
        p.body = body.to_string();
        p.producing_agent_id = agent_id;
        p.produced_on = Some(self.device_id);
        // The file's id is bound to this host and the proposal: only this
        // host can share it, and no one can claim it first.
        let mut offer = offer;
        offer.id = ligus_core::attachment::offer_id(self.device_id, proposal_id);
        p.attachment = Some(Box::new(offer));
        let id = p.id;
        self.upsert_proposal(session_id, workspace_id, p)?;
        Ok(id)
    }

    /// Offers this device produced that will never be shared: rejected,
    /// dismissed, superseded or already shared. Their staged files can go.
    pub fn settled_offers(&self, workspace_id: Uuid) -> Result<Vec<Uuid>> {
        let mut out = Vec::new();
        for session_id in self.store.sessions_of_workspace(workspace_id)? {
            if session_id == workspace_config_session_id(workspace_id) {
                continue;
            }
            let Some(s) = self.load_in(workspace_id, session_id)? else { continue };
            for p in s.proposals.iter().filter(|p| p.attachment.is_some() && p.produced_on == Some(self.device_id)) {
                let shared = s.attachments.values().any(|x| x.attachment.proposal_id == Some(p.id));
                if shared || p.dismissed || p.superseded_by.is_some() || p.status == ligus_core::ProposalStatus::Rejected {
                    out.push(p.id);
                }
            }
        }
        Ok(out)
    }

    /// Approved agent offers this device produced that aren't shared yet:
    /// (chat, proposal id, the offered reference). The host uploads each and
    /// signs its share.
    pub fn approved_offers(&self, workspace_id: Uuid) -> Result<Vec<(Uuid, Uuid, AttachmentRef)>> {
        let mut out = Vec::new();
        // Nothing is owed while the head policy would refuse the share (it
        // would be uploaded, refused and deleted again on every sync).
        let rules = self.attachment_rules(workspace_id)?;
        let (kind, role) = self.attachment_standing(workspace_id)?;
        if !rules.policy.agents || rules.policy.sharer_refusal(kind, role).is_some() {
            return Ok(out);
        }
        for session_id in self.store.sessions_of_workspace(workspace_id)? {
            if session_id == workspace_config_session_id(workspace_id) {
                continue;
            }
            let Some(s) = self.load_in(workspace_id, session_id)? else { continue };
            for p in &s.proposals {
                let Some(a) = p.attachment.as_deref() else { continue };
                if p.produced_on != Some(self.device_id) || !p.ready_to_apply() {
                    continue;
                }
                if s.attachments.values().any(|x| x.attachment.proposal_id == Some(p.id)) {
                    continue;
                }
                out.push((session_id, p.id, a.clone()));
            }
        }
        Ok(out)
    }
}

//! `ChatSession` — the projected workspace conversation state.
//!
//! A lean spine over the workspace subsystems (runtime catalog, leases, key
//! rotation, review queue, artifacts, vault exports, …), grown as features need
//! them. The event log + projector are the source of truth.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

use crate::agent::WorkspaceAgent;
use crate::channel::Channel;
use crate::workspace_runtime::WorkspaceRuntime;
use crate::chat::{ChatMessage, MessageRole};
use crate::identity::{WorkspaceMember, WorkspaceRole};
use crate::proposal_sets::ProposalSet;
use crate::proposals::ActionProposal;
use crate::skills::SkillProfile;
use crate::time_util::Timestamp;
use crate::vault::MountedVault;
use crate::workflow::{WorkflowDefinition, WorkflowRun};
use crate::workspace_host::WorkspaceHost;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSession {
    pub id: Uuid,
    pub title: String,
    pub workspace_id: Uuid,
    /// The runtime id that drives the primary (non-agent) turns. Resolved
    /// against the configured runtime catalog.
    pub runtime_id: String,
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub members: Vec<WorkspaceMember>,
    /// Workspace-canonical agent roster; each chat picks participants from it.
    #[serde(default)]
    pub workspace_agents: Vec<WorkspaceAgent>,
    /// Subset of `workspace_agents.id` active in this chat.
    #[serde(default)]
    pub participant_agent_ids: Vec<Uuid>,
    /// Skills loaded into this session; their instructions are injected into
    /// participants' system prompts.
    #[serde(default)]
    pub loaded_skills: Vec<SkillProfile>,
    /// Action proposals awaiting review / quorum.
    #[serde(default)]
    pub proposals: Vec<ActionProposal>,
    /// Proposal sets: competing alternatives scored by scouts (see
    /// [`crate::proposal_sets`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proposal_sets: Vec<ProposalSet>,
    /// Reference-material vault sources mounted into the workspace.
    #[serde(default)]
    pub vault_sources: Vec<MountedVault>,
    /// Agentic workflow definitions available in this chat.
    #[serde(default)]
    pub workflow_definitions: Vec<WorkflowDefinition>,
    /// Workflow runs (live + finished) executed in this chat.
    #[serde(default)]
    pub workflow_runs: Vec<WorkflowRun>,
    /// Soft-delete flag — archived chats are hidden from the sidebar by
    /// default but their events remain (hard delete removes the events).
    #[serde(default)]
    pub archived: bool,
    /// Account id of the device/user that created this chat. Owns the primary
    /// (non-agent) runtime for cross-device dispatch: only the creator's device
    /// answers un-`@mentioned` messages. Empty on legacy sessions (→ local).
    #[serde(default)]
    pub creator_actor_id: String,
    /// Turn-answer claims: `claim key → winning device id`. The key is
    /// [`turn_claim_key`]: the trigger message itself for a turn with one
    /// responder, a per-receiver key for one receiver of a hand-off. The
    /// first `TurnClaimed` in canonical fold order wins, so every device agrees
    /// which one answers a given turn — preventing account-scoped double-answers.
    /// Ephemeral coordination state; empty on legacy sessions.
    #[serde(default)]
    pub turn_claims: BTreeMap<Uuid, Uuid>,
    /// The dispatcher's per-turn state (`crate::dispatch`): epoch, signed
    /// coverage and the committed answer, by claim key. Fold state; left out
    /// when empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub turn_dispatch: BTreeMap<Uuid, crate::dispatch::TurnSlot>,
    /// Channels defined on the workspace. Only populated when this session IS
    /// the workspace-config log (§11); empty on ordinary chats.
    #[serde(default)]
    pub channels: Vec<Channel>,
    /// This chat's channel assignment (channel id). Empty = unfiled (pre-channel
    /// or the config log itself).
    #[serde(default)]
    pub channel_id: String,
    /// Workspace-owned runtimes (spec §12.5) — detached/headless agents run on
    /// these. Populated on the config log; overlaid onto chats like other config.
    #[serde(default)]
    pub workspace_runtimes: Vec<WorkspaceRuntime>,
    /// Workspace hosts (spec §12.4) — devices + workers agents run on.
    #[serde(default)]
    pub workspace_hosts: Vec<WorkspaceHost>,
    /// Guard state: coordinator leases on this log, plus the workspace's guard
    /// settings when this is (or is overlaid from) the config log.
    /// Skipped when empty so a session without guard state serializes exactly
    /// as before (snapshots are signed over that form).
    #[serde(default, skip_serializing_if = "crate::guard::GuardState::is_empty")]
    pub guards: crate::guard::GuardState,
    /// Flow records (routing, hand-offs, marks, …) folded from this log; see
    /// [`crate::flow`]. Skipped when empty so a session that never used flow
    /// serializes exactly as before (snapshots are signed over that form).
    #[serde(default, skip_serializing_if = "crate::flow::FlowState::is_empty")]
    pub flow: crate::flow::FlowState,
    /// This chat's repository and pinned base (`ChatRepoSet`,
    /// `ChatBasePinned`). Skipped when unset (snapshots are signed over that
    /// form).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<crate::workspace_repo::ChatRepo>,
    /// Every base this chat has pinned for each repo, oldest first; the last
    /// is the one in force (`ChatBasePinned` sets the first, first pin
    /// wins; `ChatBaseRepinned` appends). Kept across repo switches, and so
    /// hosts with a branch on an earlier pin can follow a re-pin. Skipped
    /// when empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub repo_bases: BTreeMap<Uuid, Vec<String>>,
    /// Apply requests (`ProposalApplyRequested`) by id: a human asked a host
    /// to apply an approved proposal. Skipped when empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub apply_requests: BTreeMap<Uuid, crate::workspace_repo::ApplyRequest>,
    /// How each closed apply request ended (applied, failed, withdrawn); a
    /// request without one is open. Skipped when empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub apply_outcomes: BTreeMap<Uuid, crate::workspace_repo::ApplyOutcome>,
    /// Proposals in the order the fold first saw each applied: the chat's
    /// ledger (`workspace_repo::ledger`). A second `ProposalApplied` for the
    /// same proposal (two devices at once) never adds it again. Skipped when
    /// empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_order: Vec<Uuid>,
    #[serde(default)]
    pub created_at: Timestamp,
    #[serde(default)]
    pub updated_at: Timestamp,
    /// Device-local authorization facts of this projection (held events, the
    /// rules version). Never serialized: snapshots don't carry it.
    #[serde(skip)]
    pub authz: crate::authz::AuthzState,
    /// The files shared in this chat (`AttachmentShared`) by attachment id,
    /// with their signed author; a deleted one stays as a tombstone. Folded
    /// only from signed events and never serialized, so a snapshot can't
    /// plant one.
    #[serde(skip)]
    pub attachments: BTreeMap<Uuid, crate::attachment::SharedAttachment>,
    /// Each account's profile (`ProfileUpdated`, identity model v2, 5), on
    /// the config log. Render-only, folded only from signed events and never
    /// serialized, so a snapshot can't plant one. A removed member's stays,
    /// for rendering history.
    #[serde(skip)]
    pub profiles: BTreeMap<Uuid, crate::profile::Profile>,
    /// Device names (`DeviceLabelSet`, identity model v2, 6): device →
    /// account → label. Render-only and never serialized, like `profiles`.
    #[serde(skip)]
    pub device_labels: BTreeMap<Uuid, BTreeMap<Uuid, String>>,
    /// Members retired into their person (`MemberRetired`, identity model
    /// v2, 8.3): member id → the successor account. Folded only from signed
    /// events, never serialized.
    #[serde(skip)]
    pub retired: BTreeMap<String, Uuid>,
    /// Fold state, never serialized: a counter the fold stamps on each
    /// message as it is posted (appended finished, or completed), so the
    /// transcript draws a reply where it finished
    /// ([`ChatSession::transcript`], workspace model v3, 8.1).
    #[serde(skip)]
    pub transcript_seq: u64,
}

/// Safety ceiling on agent replies since the last human turn — a runaway/loop
/// backstop, NOT a conversation limiter. A healthy cascade ends on its own when
/// an agent stops `@mention`-ing another; this only bites when agents keep
/// addressing each other, so it's set generously enough for 2+ agents to hold a
/// real multi-turn conversation without a human ping, while still bounding an
/// infinite A↔B loop and its token cost. Bounds the chain inline on one device
/// *and* across devices (both read the synced transcript via
/// [`ChatSession::cascade_depth`]). Could later be a per-workspace setting.
pub const MAX_CASCADE_DEPTH: usize = 24;

impl ChatSession {
    /// How many consecutive agent/assistant replies trail the transcript back to
    /// the last human turn — the cascade depth. Used to bound agent→agent chains
    /// (the desktop turn loop, cross-device pickup, and the worker queue all read
    /// the same synced value, so the bound holds across devices).
    pub fn cascade_depth(&self) -> usize {
        let mut n = 0;
        for m in self.messages.iter().rev() {
            match m.role {
                MessageRole::System => continue,
                // A fenced duplicate answer isn't a reply in the chain.
                MessageRole::Assistant | MessageRole::Agent if m.superseded => continue,
                MessageRole::Assistant | MessageRole::Agent => n += 1,
                MessageRole::User => break,
            }
        }
        n
    }

    /// The comments on `proposal` whose author is verified: folded from their
    /// own signed `ProposalRefined`, signed by the member they name
    /// (`AuthzState::refinement_authors`). Only these may be forwarded to an
    /// agent as review feedback.
    pub fn verified_refinements<'a>(
        &'a self,
        proposal: &'a ActionProposal,
    ) -> impl Iterator<Item = &'a crate::proposals::ProposalRefinement> + 'a {
        proposal
            .refinements
            .iter()
            .filter(move |r| self.authz.refinement_authors.get(&r.id).is_some_and(|a| *a == r.actor_id))
    }

    /// The workspace role `actor_id` holds in this session's projected roster,
    /// or the safe floor (`Viewer`) if they're not a member. Mirrors the
    /// authoring-side floor (`ChatService::actor_role`) so ingest/projection
    /// authz and author-time authz agree on a non-member's privileges.
    pub fn role_of(&self, actor_id: &str) -> WorkspaceRole {
        self.members
            .iter()
            .find(|m| m.id == actor_id)
            .map(|m| m.role)
            .unwrap_or(WorkspaceRole::Viewer)
    }

    /// The open apply request of `proposal_id`, if any: one a person filed
    /// that no apply, failure or withdrawal has closed. A proposal has at
    /// most one (one applier per proposal; the fold refuses a second).
    pub fn open_apply_request(&self, proposal_id: Uuid) -> Option<&crate::workspace_repo::ApplyRequest> {
        self.apply_requests
            .values()
            .find(|r| r.proposal_id == proposal_id && !self.apply_outcomes.contains_key(&r.id))
    }

    pub fn new(title: impl Into<String>, workspace_id: Uuid, runtime_id: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: Uuid::new_v4(),
            title: title.into(),
            workspace_id,
            runtime_id: runtime_id.into(),
            messages: Vec::new(),
            members: Vec::new(),
            workspace_agents: Vec::new(),
            participant_agent_ids: Vec::new(),
            loaded_skills: Vec::new(),
            proposals: Vec::new(),
            proposal_sets: Vec::new(),
            vault_sources: Vec::new(),
            workflow_definitions: Vec::new(),
            workflow_runs: Vec::new(),
            archived: false,
            creator_actor_id: String::new(),
            turn_claims: BTreeMap::new(),
            turn_dispatch: BTreeMap::new(),
            channels: Vec::new(),
            channel_id: String::new(),
            workspace_runtimes: Vec::new(),
            workspace_hosts: Vec::new(),
            guards: Default::default(),
            flow: crate::flow::FlowState::default(),
            repo: None,
            repo_bases: BTreeMap::new(),
            apply_requests: BTreeMap::new(),
            apply_outcomes: BTreeMap::new(),
            applied_order: Vec::new(),
            created_at: now,
            updated_at: now,
            authz: Default::default(),
            attachments: BTreeMap::new(),
            profiles: BTreeMap::new(),
            device_labels: BTreeMap::new(),
            retired: BTreeMap::new(),
            transcript_seq: 0,
        }
    }

    /// Most-recent message timestamp, or the session's creation time when
    /// empty. Mirrors Swift's `lastActivityAt`.
    pub fn last_activity_at(&self) -> Timestamp {
        self.messages
            .iter()
            .rev()
            .find(|m| !m.superseded)
            .map(|m| m.created_at)
            .unwrap_or(self.created_at)
    }

    pub fn message_index(&self, id: Uuid) -> Option<usize> {
        self.messages.iter().position(|m| m.id == id)
    }
}

/// The key a turn claim is recorded under (`TurnClaimed.trigger_id`): the
/// trigger itself for a turn with one responder (a user message), and a
/// UUIDv5 of (trigger, receiving agent) for one receiver of a hand-off,
/// which can start several agents on several devices from one reply. The
/// plain form is what every device has always written, so older devices
/// still coordinate on user-message turns.
pub fn turn_claim_key(trigger_id: Uuid, agent_id: Option<Uuid>) -> Uuid {
    /// Namespace for per-receiver claim keys (fixed; never change it).
    const NS: Uuid = Uuid::from_u128(0x6c69_6775_732d_7475_726e_2d63_6c61_696d);
    match agent_id {
        None => trigger_id,
        Some(a) => {
            let mut name = [0u8; 32];
            name[..16].copy_from_slice(trigger_id.as_bytes());
            name[16..].copy_from_slice(a.as_bytes());
            Uuid::new_v5(&NS, &name)
        }
    }
}

#[cfg(test)]
mod serialized_form_tests {
    use super::ChatSession;
    use uuid::Uuid;

    /// A session that never used guard or flow state must serialize with
    /// the same keys as 0.1, because snapshots are signed over that form.
    #[test]
    fn empty_guard_and_flow_state_are_left_out() {
        let session = ChatSession::new("t", Uuid::nil(), "claude-code");
        let json = serde_json::to_value(&session).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("guards"));
        assert!(!obj.contains_key("flow"));
    }
}

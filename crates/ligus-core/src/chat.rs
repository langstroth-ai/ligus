//! Transcript models — ported from `LigusModels.swift` (`ChatMessage` and
//! friends). Optional/defaulted fields use `#[serde(default)]` so older stored
//! JSON decodes unchanged, mirroring the Swift `decodeIfPresent` decoders.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::identity::{ActorIdentity, ActorKind};
use crate::time_util::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TranscriptEventKind {
    Message,
    Proposal,
    ExecutionResult,
    HandoffSummary,
}

impl Default for TranscriptEventKind {
    fn default() -> Self {
        Self::Message
    }
}

/// First-class tool-call block emitted by an assistant message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatToolCall {
    pub id: String,
    pub name: String,
    pub input_json: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
}

/// First-class tool-result block returned by a user-role message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatToolResult {
    pub call_id: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

/// A single emoji reaction on a message by one actor (human or agent).
/// Identity is `(actor_id, emoji)` so toggling the same emoji is idempotent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageReaction {
    pub emoji: String,
    pub actor_id: String,
    pub actor_display_name: String,
    #[serde(default = "ActorKind::human")]
    pub actor_kind: ActorKind,
    #[serde(default)]
    pub created_at: Timestamp,
}

impl MessageReaction {
    /// Two reactions are "the same vote" when the same actor used the same
    /// emoji — used for toggling and dedupe (timestamp/name ignored).
    pub fn is_same_vote(&self, other: &MessageReaction) -> bool {
        self.actor_id == other.actor_id && self.emoji == other.emoji
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: Uuid,
    pub role: MessageRole,
    pub author: String,
    pub body: String,
    #[serde(default)]
    pub created_at: Timestamp,
    #[serde(default)]
    pub is_streaming: bool,
    #[serde(default)]
    pub kind: TranscriptEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_proposal_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_identity: Option<ActorIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_persona_id: Option<String>,
    /// The responding agent's stable id (None = the primary runtime or a human).
    /// "Own vs other" turn attribution keys on this, not the display name, so a
    /// rename or a duplicate name can't mis-label a turn as the responder's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responder_id: Option<Uuid>,
    /// The message this turn is answering — the id of the user/agent message that
    /// triggered it. This is the per-request correlation key: without it, replies
    /// are matched to requests by author name, so two concurrent tasks to the
    /// SAME agent collapse onto one reply. None on human messages and on legacy
    /// turns from before this field (correlation falls back to author name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to_message_id: Option<Uuid>,
    /// The device that authored a streaming assistant message. The startup
    /// stale-stream sweep only completes messages from THIS device, so it can't
    /// truncate a peer's in-flight stream. None on human/legacy messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_device_id: Option<Uuid>,
    /// The workflow run this message is a stage prompt of. The author gate
    /// judges such a prompt at the lower of its poster's role and the run's
    /// initiator's (`ChatService::trigger_author_role`), so a device that
    /// drives or recovers someone else's run can't lend the stages its own
    /// user's tier. Self-asserted by the poster, so it can only lower a role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_run_id: Option<Uuid>,
    /// Fold state, never an author's word: a second answer to a turn the
    /// dispatcher already committed (or one from a device that didn't hold
    /// the turn), hidden from the transcript, the answered checks, cascades
    /// and the next turn's context (`crate::dispatch`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub superseded: bool,
    /// Fold state, never an author's word: when a streamed reply was
    /// finished, from the signed time of its `MessageCompleted`. A turn's
    /// reply is appended as a placeholder when the turn starts, so
    /// `created_at` is the turn's start; a reply that waited on a person
    /// (`ligus_ask_person`) is posted well after that. Read it through
    /// [`ChatMessage::posted_at`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<Timestamp>,
    /// Fold state, never an author's word and never serialized: the signed
    /// time of the envelope that first appended this message (the first in
    /// fold order). `created_at` is a payload field its author chooses; this
    /// is the clock ages are read from (a turn's takeover age). `None` on a
    /// message seeded from a snapshot. Read it through
    /// [`ChatMessage::appended_at`].
    #[serde(skip)]
    pub appended_at: Option<Timestamp>,
    /// Fold state, never an author's word: how a streamed reply ended, from
    /// its `MessageCompleted` (`ok` for a completion without an outcome, an
    /// older writer's). `None` while it streams, and on a message that never
    /// streamed. Display only (workspace model v3, 8.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<crate::outcome::ReplyOutcome>,
    /// Fold state, never serialized: the device (or agent key) that signed
    /// this message's first append. A `canceled` completion is overridden
    /// only by a later completion from this signer (E7).
    #[serde(skip)]
    pub appended_by: Option<Uuid>,
    /// Fold state, never serialized: where the message was posted in fold
    /// order ([`crate::ChatSession::transcript_seq`]); `None` while it
    /// streams, and on a message seeded from a snapshot.
    #[serde(skip)]
    pub posted_seq: Option<u64>,
    #[serde(default)]
    pub tool_calls: Vec<ChatToolCall>,
    #[serde(default)]
    pub tool_results: Vec<ChatToolResult>,
    #[serde(default)]
    pub reactions: Vec<MessageReaction>,
    #[serde(default)]
    pub reaction_options: Vec<String>,
}

impl ChatMessage {
    /// Convenience constructor for the common case (role + author + body).
    pub fn new(role: MessageRole, author: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            role,
            author: author.into(),
            body: body.into(),
            created_at: Timestamp::now(),
            is_streaming: false,
            kind: TranscriptEventKind::Message,
            related_proposal_id: None,
            related_agent_id: None,
            actor_identity: None,
            runtime_id: None,
            runtime_label: None,
            agent_persona_id: None,
            responder_id: None,
            reply_to_message_id: None,
            origin_device_id: None,
            workflow_run_id: None,
            superseded: false,
            completed_at: None,
            appended_at: None,
            outcome: None,
            appended_by: None,
            posted_seq: None,
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            reactions: Vec::new(),
            reaction_options: Vec::new(),
        }
    }

    /// When the message was posted, for showing and ordering it: a streamed
    /// reply's completion (its final text), else when it was appended. Never
    /// before `created_at`.
    pub fn posted_at(&self) -> Timestamp {
        self.completed_at.filter(|c| *c > self.created_at).unwrap_or(self.created_at)
    }

    /// When this message was appended: its envelope's signed time, or the
    /// payload's `created_at` when the fold didn't see the envelope (a
    /// snapshot seed).
    pub fn appended_at(&self) -> Timestamp {
        self.appended_at.unwrap_or(self.created_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_minimal_legacy_message_with_defaults() {
        // A message written before tool/reaction fields existed must still
        // decode, with the new collections defaulting to empty.
        let json = r#"{
            "id": "00000000-0000-0000-0000-000000000001",
            "role": "user",
            "author": "Mara",
            "body": "hello",
            "createdAt": "2026-01-01T00:00:00Z"
        }"#;
        let msg: ChatMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.role, MessageRole::User);
        assert_eq!(msg.body, "hello");
        assert!(!msg.is_streaming);
        assert_eq!(msg.kind, TranscriptEventKind::Message);
        assert!(msg.tool_calls.is_empty());
        assert!(msg.reactions.is_empty());
    }

    #[test]
    fn reaction_same_vote_ignores_timestamp_and_name() {
        let a = MessageReaction {
            emoji: "👍".into(),
            actor_id: "u1".into(),
            actor_display_name: "A".into(),
            actor_kind: ActorKind::Human,
            created_at: Timestamp::epoch(),
        };
        let b = MessageReaction {
            emoji: "👍".into(),
            actor_id: "u1".into(),
            actor_display_name: "different".into(),
            actor_kind: ActorKind::Human,
            created_at: Timestamp::now(),
        };
        assert!(a.is_same_vote(&b));
    }
}

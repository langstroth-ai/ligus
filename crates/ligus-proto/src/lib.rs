//! `ligus-proto` — the IPC contract shared between the Rust backend
//! (`ligus-runtime` / `app`) and the TypeScript frontend (`web/`).
//!
//! Every type derives `serde` (for Tauri command (de)serialization) and
//! `ts-rs::TS`. The `export_bindings` test writes the matching TypeScript into
//! `web/src/bindings/`; CI fails if those files drift from these definitions,
//! so the frontend can never get out of sync with the backend contract.
//!
//! These are presentation DTOs — flat, string-keyed, camelCase — deliberately
//! decoupled from the richer `ligus-core` domain types. The backend converts
//! `ligus-core` values into these at the IPC boundary.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub mod guards;
pub use guards::*;
pub mod ask_text;
pub mod names;
mod proposal_sets;
pub use proposal_sets::{
    PreflightCheckDto, PreflightDto, ProposalSetDto, ScoutAssessmentDto, SetAlternativeDto,
};

/// Backend build/version info — the Phase 0 smoke-test round-trip.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    pub core_version: String,
    pub build_profile: String,
    /// This host's agent sandbox tier: "seatbelt" | "bwrap" | "landlock" |
    /// "unconfined" (write-capable agent turns refused).
    #[serde(default)]
    pub sandbox_tier: String,
    /// One line: the tier and why it was chosen.
    #[serde(default)]
    pub sandbox_summary: String,
    /// What the tier leaves open here, and how to get a stronger one.
    #[serde(default)]
    pub sandbox_notes: Vec<String>,
}

/// One emoji reaction on a message.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ReactionDto {
    pub emoji: String,
    pub actor_id: String,
    #[serde(serialize_with = "crate::names::ser_clean")]
    pub actor_display_name: String,
}

/// One transcript message, flattened for rendering.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ChatMessageDto {
    pub id: String,
    /// "system" | "user" | "assistant" | "agent"
    pub role: String,
    #[serde(serialize_with = "crate::names::ser_clean")]
    pub author: String,
    pub body: String,
    pub is_streaming: bool,
    /// RFC 3339 timestamp.
    pub created_at: String,
    #[serde(default)]
    pub reactions: Vec<ReactionDto>,
    /// Tool calls this assistant turn made (for inline tool-call cards).
    #[serde(default)]
    pub tool_calls: Vec<ToolCallDto>,
    /// Tool results carried by this (user-role) turn, keyed back to a call id.
    #[serde(default)]
    pub tool_results: Vec<ToolResultDto>,
    /// The author's avatar image (`data:` URL) for this turn — from the stamped
    /// actor identity (humans) or the agent roster (agents). None = initials.
    #[serde(default)]
    pub author_avatar_url: Option<String>,
    /// The author's avatar accent color (`#rrggbb`), when set. None = derived.
    #[serde(default)]
    pub author_color_hex: Option<String>,
    /// For a primary-`@ligus` turn: the display name of the user whose device
    /// answered (every user's default agent is named "ligus", so this
    /// disambiguates "Alice's @ligus" from "Bob's @ligus"). None for human turns and
    /// named agents (whose own name is the label).
    #[serde(default)]
    pub responder_name: Option<String>,
    /// Where a primary-`@ligus` turn ran, relative to the viewer: "this" (this
    /// device), "mine" (another device of the viewer's own account), or None
    /// (someone else's device, or not a primary turn). The UI shows "This
    /// device" / "Your other device" instead of the viewer's own name, so a
    /// default name like "You" never reads as the agent's.
    #[serde(default)]
    pub responder_device: Option<String>,
    /// The runtime that produced THIS turn (stamped at generation), so the UI
    /// attributes each turn to the model that actually ran it — not the chat's
    /// currently-selected runtime, which retroactively relabels history. None on
    /// human/system turns and pre-existing messages.
    #[serde(default)]
    pub runtime_id: Option<String>,
    /// A human label for that runtime (e.g. "claude-code / opus"), when stamped.
    #[serde(default)]
    pub runtime_label: Option<String>,
    /// For an agent's reply that writes `@you`: the display name of the person
    /// it addresses (whose message started the turn), shown as `@<name>` in
    /// place of `@you`. Rendering only: the signed body keeps `@you`. None for
    /// other messages, or when no person started the turn.
    #[serde(default)]
    pub requester_name: Option<String>,
    /// The viewer is that person: `@you` is highlighted as mentioning them.
    #[serde(default)]
    pub requester_is_viewer: bool,
    /// How a finished reply ended and what answered (its completion's
    /// outcome). None while it streams, and on other messages.
    #[serde(default)]
    pub outcome: Option<ReplyOutcomeDto>,
    /// On a person's message whose turn doesn't run (yet): where it waits or
    /// why it can't run, for one quiet row under the message. Only on the
    /// sender's own devices. None when the turn runs or was answered.
    #[serde(default)]
    pub route: Option<TurnRouteDto>,
}

/// How a reply ended (workspace model v3, 8.1), as its author reported it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ReplyOutcomeDto {
    /// "ok" | "failed" | "canceled"
    pub status: String,
    /// The harness that ran it ("claudeCode", "codex", "openai", ...).
    #[serde(default)]
    pub harness: Option<String>,
    /// The model that answered.
    #[serde(default)]
    pub model: Option<String>,
    /// The host that ran it, by its label ("this device" for this one).
    #[serde(default, serialize_with = "crate::names::ser_clean_opt")]
    pub host: Option<String>,
}

/// Where a turn that doesn't run is (workspace model v3, 5.4).
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct TurnRouteDto {
    /// "queued" | "hostGone" | "unrunnable"
    pub state: String,
    /// Why, as a code: "offline", "removed", "revoked", "left",
    /// "sourceNotOffered", "modelNotOffered", "notAllowedOnHost", "ownerGone",
    /// "noKeyHere", "waiting".
    pub reason: String,
    /// The one line the chat shows.
    pub text: String,
    /// The host it names, by its label.
    #[serde(default, serialize_with = "crate::names::ser_clean_opt")]
    pub host: Option<String>,
    /// RFC 3339: since when it waits.
    #[serde(default)]
    pub since: Option<String>,
    /// "Run here this time" is offered (this device is one of the agent's
    /// owner's and can run it).
    #[serde(default)]
    pub run_here: bool,
    /// "Use Claude Code instead" for this one turn is offered.
    #[serde(default)]
    pub use_claude_code: bool,
}

/// A tool invocation an assistant turn made (MCP or built-in).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ToolCallDto {
    pub id: String,
    pub name: String,
    /// JSON-encoded arguments.
    pub input_json: String,
    #[serde(default)]
    pub server_id: Option<String>,
}

/// The result of a tool call, keyed to its `call_id`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ToolResultDto {
    pub call_id: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

/// One vote on a proposal.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ApprovalDto {
    pub actor_id: String,
    /// Human-readable name of the voter, resolved from the workspace roster at
    /// projection time so the Review pane can tag who approved/rejected without a
    /// separate roster lookup. Falls back to a short actor id when unknown.
    #[serde(default, serialize_with = "crate::names::ser_clean")]
    pub display_name: String,
    pub role: String,
    pub approved: bool,
}

/// One note on a proposal's refinement thread.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct RefinementDto {
    pub id: String,
    pub actor_id: String,
    /// Voter/commenter name resolved from the roster at projection time.
    #[serde(default, serialize_with = "crate::names::ser_clean")]
    pub display_name: String,
    pub role: String,
    pub text: String,
    #[serde(default)]
    pub request_changes: bool,
    pub created_at: String,
}

/// A proposal in the review queue, with computed quorum state.
/// The file an agent's proposal offers (issue #118).
#[derive(Debug, Clone, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct OfferedFileDto {
    pub name: String,
    pub size: u64,
    /// The type detected from the content when the agent offered it.
    pub content_type: String,
    pub sha256: String,
    /// Its name says it can run code, or hides a second extension.
    pub risky: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ProposalDto {
    pub id: String,
    pub title: String,
    pub body: String,
    /// Actor who created the proposal; their own approval doesn't count toward
    /// quorum. Empty for authorless proposals (e.g. workflow gates).
    #[serde(default)]
    pub author_actor_id: String,
    /// "fileDiff" | "command" | "decision"
    pub kind: String,
    /// "open" | "approved" | "rejected" | "applied"
    pub status: String,
    pub required_approvals: u32,
    pub qualifying_approvals: u32,
    pub quorum_met: bool,
    pub approvals: Vec<ApprovalDto>,
    /// For a fileDiff proposal: the unified diff the agent produced in its
    /// isolated worktree (rendered in the Diff canvas; applied on Implement).
    /// None for command/decision proposals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// Paths the diff touches, for a compact summary in the Review pane.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    /// RFC 3339. The Review pane orders newest-first; without this the UI only
    /// has fold order (oldest first, settled interleaved with open).
    pub created_at: String,
    /// Hidden from the Review inbox (the record survives; see `ActionProposal`).
    pub dismissed: bool,
    /// The refinement discussion thread (comments + change requests), oldest first.
    #[serde(default)]
    pub refinements: Vec<RefinementDto>,
    /// The proposal this one revises (a version chain), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// The newer version that superseded this one, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// An agent-authored workflow up for review: approving the proposal adds
    /// it (or, when its id matches an existing workflow, replaces that one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<WorkflowDefinitionDto>,
    /// The proposal set this is an alternative in; the Review pane shows it
    /// inside the set instead of on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_class: Option<String>,
    /// An applied change that was later reverted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub reverted: bool,
    /// Review badges (Track D4): "human" | "agent".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    #[ts(as = "Option<String>", optional)]
    pub author_kind: String,
    /// The author's display name (`@coder`, a person's name).
    #[serde(default, skip_serializing_if = "String::is_empty", serialize_with = "crate::names::ser_clean")]
    #[ts(as = "Option<String>", optional)]
    pub author_name: String,
    /// Who signed it: "agent" (the agent's own key), "host" (a host device
    /// speaking for an agent: legacy), "human".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    #[ts(as = "Option<String>", optional)]
    pub signed_by: String,
    /// It touches configuration, CI, hooks, protected dirs or MCP config.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub config_impact: bool,
    /// A human gate applies (dearth mode, a lockdown, a path zone).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub human_gate: bool,
    /// Applying needs a passkey presence proof (rules v2).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub presence_required: bool,
    /// A command proposal without a structured argv (Implement falls back
    /// to an instruction).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub legacy_command: bool,
    /// A structured command's exact argv.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub argv: Option<Vec<String>>,
    /// The agent whose turn produced it (Implement's default target).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub producing_agent_id: Option<String>,
    /// A file an agent offers to share (issue #118): what the reviewer is
    /// approving. Nothing is shared until a person approves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub offered_file: Option<OfferedFileDto>,
    /// Apply on a host (Phase B): every request a person filed for it, and
    /// how each ended. At most one is `open` at a time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<ApplyRequestDto>>", optional)]
    pub apply_requests: Vec<ApplyRequestDto>,
    /// A person may ask a host to apply it now: approved, and no request
    /// open (one applier per proposal).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub can_request_apply: bool,
    /// Archived, and how: "person" (someone archived it, for everyone) or
    /// "auto" (settled for longer than the workspace's auto-archive days).
    /// Absent: in the inbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub archived: Option<String>,
    /// When it was archived by a person (RFC 3339; that event's own time).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub archived_at: Option<String>,
    /// Who archived it (the name of the account on the signed event).
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "crate::names::ser_clean_opt")]
    #[ts(optional)]
    pub archived_by: Option<String>,
    /// When it settled (RFC 3339; the settling event's own time).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub settled_at: Option<String>,
    /// Hidden on this device by this person ("Hide for me"; never synced).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub hidden: bool,
    /// It may be archived: settled (applied, rejected, superseded) or stale,
    /// and not archived yet. A proposal that still waits on someone can't be.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub can_archive: bool,
}

/// One archived (or hidden-for-me) proposal in the workspace-wide Archived
/// view, with the chat it belongs to.
#[derive(Debug, Clone, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ArchivedProposalDto {
    pub chat_id: String,
    #[serde(serialize_with = "crate::names::ser_clean")]
    pub chat_title: String,
    pub proposal: ProposalDto,
}

/// A person's request that one host apply a proposal (`ProposalApplyRequested`),
/// with its outcome.
#[derive(Debug, Clone, Serialize, Deserialize, TS, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ApplyRequestDto {
    pub id: String,
    /// The host's workspace device id.
    pub host_device_id: String,
    /// The host's label, when it is a registered host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub host_label: Option<String>,
    /// "open" | "applied" | "failed" | "withdrawn"
    pub state: String,
    /// Why it failed (`failed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
    /// The host's commit (`applied`, when it made one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub commit: Option<String>,
    /// This device filed it (it may withdraw it).
    pub mine: bool,
}

/// What Implement will do for a proposal, exactly (the preview sheet). The
/// text and argv here come from the same function the runner uses.
#[derive(Debug, Clone, Serialize, Deserialize, TS, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ImplementPreviewDto {
    /// "argv" | "instruction" | "applyDiff" | "addWorkflow"
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub argv: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cwd: Option<String>,
    /// The exact instruction text the agent receives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub instruction: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub target_agent_id: Option<String>,
    pub target_name: String,
    /// Agents the picker offers: `[id, name]`.
    pub targets: Vec<(String, String)>,
    pub runtime: String,
    /// "acp" | "legacy" | "" (not a CLI runtime)
    pub driver: String,
    pub permission_tier: String,
    /// "Seatbelt" | "Bwrap" | "Landlock" | "Unconfined" | …
    pub executor: String,
    /// Endpoints the run may reach (empty = no network).
    pub egress: Vec<String>,
    pub presence_required: bool,
    pub human_gate: bool,
    pub legacy_command: bool,
}

/// A chat as listed in the sidebar.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ChatSummaryDto {
    pub id: String,
    pub title: String,
    pub last_activity_at: String,
    pub message_count: u32,
    pub archived: bool,
    /// The channel this chat is filed under (empty = unfiled). Spec §11.
    #[serde(default)]
    pub channel_id: String,
    /// True when this chat is its channel's drop-in default — not movable,
    /// renamable, or deletable on its own.
    #[serde(default)]
    pub is_channel_default: bool,
    /// The workspace this chat belongs to. A chat is (workspace, chat):
    /// another workspace can hold a chat under the same id, so the desktop
    /// opens a chat in this workspace.
    #[serde(default)]
    pub workspace_id: String,
}

/// A workspace channel — organization only, no config (spec §11).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ChannelDto {
    pub id: String,
    pub name: String,
    pub purpose: Option<String>,
    pub position: i32,
    pub archived: bool,
    /// The channel's drop-in default chat (created with it).
    pub default_chat_id: String,
}

/// One file's uncommitted change, for the Diff canvas.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct GitFileDiffDto {
    pub path: String,
    /// "modified" | "added" | "deleted" | "renamed" | "untracked" | "conflicted"
    pub kind: String,
    /// Unified-diff text (synthesized for untracked files).
    pub patch: String,
    pub added_lines: u32,
    pub removed_lines: u32,
}

/// One entry in a workspace directory listing (the lazy file tree). `path` is
/// repo-relative (POSIX `/` separators, so the key is stable across platforms).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FsEntryDto {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
}

/// A workspace agent in the roster (a named participant on a runtime).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WorkspaceAgentDto {
    pub id: String,
    pub name: String,
    pub runtime_id: String,
    pub role: String,
    /// Actor id of the member who owns this agent (empty if unowned). Only the
    /// owner may edit the agent's avatar.
    pub owner_actor_id: String,
    /// Id of the host this agent runs on. Empty means the owner's own device
    /// (a local persona); a Worker id means it is headless (worker-hosted).
    pub host_id: String,
    /// Optional avatar image (`data:` URL); falls back to the color, then to
    /// initials-on-color when absent.
    pub avatar_url: Option<String>,
    /// Optional avatar accent color (`#rrggbb`).
    pub avatar_color_hex: Option<String>,
    /// Who may use the agent, as enforced: `"owner"` (a personal agent's
    /// default, "Only me"; on a shared agent, workspace Admins) or
    /// `"everyone"`.
    pub access: String,
    /// Turns each member other than the owner may start per day as set
    /// (`None`: the default; `0`: no limit).
    pub member_turns_per_day: Option<u32>,
    /// The per-member daily cap in force (`None`: no limit).
    pub effective_member_turns_per_day: Option<u32>,
    /// The agent's estimated daily spending cap in US cents (`None`: none).
    pub usd_cents_per_day: Option<u32>,
    /// Its owner is told when someone else uses it.
    pub notify_on_use: bool,
    /// Whether this device's person may use the agent (the @mention
    /// autocomplete greys it out otherwise).
    pub available: bool,
    /// Why not, when `available` is false.
    pub unavailable_reason: Option<String>,
    /// Whether this device's person may change who may use it, its budgets,
    /// and see its usage (its owner; an Admin on a shared agent).
    pub can_manage_access: bool,
}

/// One member's use of an agent, as the device that runs it counted it.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct AgentUsageRowDto {
    /// The member's id (empty: a turn no person could be traced to).
    pub member: String,
    /// Their display name, when known.
    pub member_name: String,
    pub turns: u32,
    /// Turns whose runtime reported no token usage (not in the cost).
    pub uncosted_turns: u32,
    /// Estimated USD, when any of the turns reported usage.
    pub estimated_cost_usd: Option<f64>,
}

/// An agent's usage on this device: today and this month (UTC days).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct AgentUsageDto {
    pub agent_id: String,
    /// Today, `YYYY-MM-DD` (UTC).
    pub day: String,
    pub today: Vec<AgentUsageRowDto>,
    pub month: Vec<AgentUsageRowDto>,
}

/// A configured runtime surfaced to the desktop shell.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct RuntimeSummaryDto {
    pub id: String,
    pub name: String,
    pub label: String,
    pub provider: String,
    pub location: String,
    pub model: String,
    pub endpoint: String,
    pub supports_tools: bool,
    pub supports_embeddings: bool,
    pub is_default: bool,
    pub is_managed: bool,
    /// OpenAI-compatible base URL (pi → local backends). None for providers that
    /// don't use one. Carried so an edit form can round-trip it.
    pub model_base_url: Option<String>,
    /// Sub-provider id for pi-style bridges (e.g. "ollama"). None otherwise.
    pub model_provider_id: Option<String>,
    /// Explicit context-window override (Ollama/custom endpoints). None = inferred.
    pub context_window: Option<u32>,
    /// Ollama `keep_alive` ("5m", "-1" = forever). None = server default.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub keep_alive: Option<String>,
    /// Ollama `think`: let a reasoning model think. None = Ligus default (off).
    #[serde(default)]
    #[ts(optional = nullable)]
    pub think: Option<bool>,
    /// Model rounds per tool-using turn (HTTP runtimes). None = Ligus default (24).
    #[serde(default)]
    #[ts(optional = nullable)]
    pub max_tool_rounds: Option<u32>,
    /// Offer the built-in workspace file tools even though the endpoint isn't
    /// local or private. None = off.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub allow_remote_file_tools: Option<bool>,
    /// The operator-listed model gateway this runtime goes through (by
    /// name). None = the runtime's own endpoint.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub gateway: Option<String>,
}

/// Actual context-budget telemetry computed by the backend planner.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ContextTelemetryDto {
    pub session_id: String,
    pub runtime_id: String,
    pub model: String,
    pub context_window_tokens: u32,
    pub reserved_output_tokens: u32,
    pub summary_reserve_tokens: u32,
    pub system_prompt_tokens: u32,
    pub history_budget_tokens: u32,
    pub history_tokens: u32,
    pub message_count: u32,
    pub kept_message_count: u32,
    pub kept_tokens: u32,
    pub overflow_message_count: u32,
    pub overflow_tokens: u32,
    pub skill_count: u32,
    pub skill_tokens: u32,
    pub vault_count: u32,
    /// "none" | "cached" | "incremental" | "fresh"
    pub summary_strategy: String,
}

/// A workspace member with a governance role.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WorkspaceMemberDto {
    pub id: String,
    /// The member's actor id — used to match against the presence feed (online).
    pub actor_id: String,
    #[serde(serialize_with = "crate::names::ser_clean")]
    pub display_name: String,
    /// "owner" | "admin" | "contributor" | "viewer"
    pub role: String,
    pub title: String,
    /// Per-workspace join index (1-based). Disambiguates duplicate display
    /// names — surfaced as `Name #N`, matchable as `@Name#N`.
    pub index: u32,
    /// True when this member is the local user. The People list hides "you"
    /// (you're shown in the identity card), so it shows only collaborators.
    pub is_self: bool,
    /// Optional avatar image (`data:` URL) the member chose; rides the synced
    /// roster. Absent = initials-on-color.
    pub avatar_url: Option<String>,
    /// "human" | "agent" | "service" (a `ligus serve` or worker box).
    #[serde(default)]
    pub kind: String,
    /// The workspace's founder (nobody cuts the founder).
    #[serde(default)]
    pub is_founder: bool,
    /// Why you can't remove this member (the fold's own rules), or none
    /// when you can.
    #[serde(default)]
    pub remove_refusal: Option<String>,
    /// For each role, why you can't give it to this member (none: you can).
    #[serde(default)]
    pub role_options: Vec<MemberRoleOptionDto>,
    /// Why "Allow new passkey" doesn't apply (a service member holds no
    /// passkey), or none.
    #[serde(default)]
    pub regrant_refusal: Option<String>,
    /// When this member asked to leave (RFC 3339), if they did (workspace
    /// model v3, 10.4). Under rules 3 they are still a member: People shows
    /// them as "asked to leave", with Remove for Admins.
    #[serde(default)]
    pub left_at: Option<String>,
}

/// One role in a member's role picker and why it is off-limits, if it is.
#[derive(Debug, Clone, Serialize, Deserialize, TS, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct MemberRoleOptionDto {
    /// "owner" | "admin" | "contributor" | "viewer"
    pub role: String,
    pub refusal: Option<String>,
}

/// One selectable workspace in the switcher: the local "My workspace", or a
/// joined relay room. The chat list is scoped to whichever is `active`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WorkspaceInfoDto {
    pub id: String,
    pub name: String,
    /// "local" | "room"
    pub kind: String,
    pub active: bool,
    /// Optional workspace icon as a `data:` URL (set by an owner/admin); when
    /// present the rail renders it in place of the initials/brand mark.
    pub icon_url: Option<String>,
}

/// A loaded skill (instruction bundle injected into prompts).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct SkillDto {
    pub id: String,
    pub name: String,
    pub instructions: String,
    pub source_url: Option<String>,
    /// Agent ids this skill targets. Empty ⇒ global (primary + all agents).
    pub agent_ids: Vec<String>,
}

/// A configured MCP server and whether it's enabled (the inert-until-enabled
/// gate: a disabled server is never launched/connected).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct McpServerDto {
    pub id: String,
    /// "stdio" | "http"
    pub transport: String,
    pub detail: String,
    pub enabled: bool,
    pub is_managed: bool,
    /// A static bearer token is saved for this server on this device. The
    /// token itself never crosses to the UI — only this flag.
    #[serde(default)]
    pub has_bearer_token: bool,
    /// An OAuth token (Connect flow) is stored for this server on this device.
    #[serde(default)]
    pub oauth_connected: bool,
    /// A token or OAuth connection is saved under this server's id, but for
    /// a different URL origin, so it is withheld from this server. Set the
    /// token again or reconnect to use it here.
    #[serde(default)]
    pub secret_for_other_url: bool,
    /// The tools this server listed that run shell commands (`bash`, `exec`,
    /// `run_command`, ...), from the last time this device listed its tools;
    /// `None` until it has. Such a server is offered only to turns that have
    /// the shell (the workspace Owner's, or an agent the Owner granted it).
    #[serde(default)]
    pub shell_tools: Option<Vec<String>>,
}

/// A reference-material vault source mounted in the workspace.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct VaultSourceDto {
    /// "github" | "gitlab" | "https"
    pub kind: String,
    pub label: String,
    pub url: String,
    /// Agent ids this vault targets. Empty ⇒ global (primary + all agents).
    pub agent_ids: Vec<String>,
}

/// App + workspace settings surfaced to the Settings pane.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct AppSettingsDto {
    pub display_name: String,
    /// Git email used to attribute commits agents make on this user's behalf.
    pub git_email: String,
    pub device_name: String,
    /// The effective code dir — the active chat's isolated worktree when one is
    /// active, else the workspace folder. Drives the Code view / terminal / diff.
    pub workspace_root: String,
    /// The active workspace's linked folder on this device (the git repo root),
    /// independent of any chat worktree. Use this for workspace display (the
    /// crumb, Folder & Git) so a chat's worktree path never shows as "the folder".
    pub workspace_folder: String,
    pub known_workspaces: Vec<String>,
    pub model: String,
    pub git_branch: Option<String>,
    pub git_dirty_count: u32,
    /// The local user's chosen avatar (`data:` URL), or None for initials.
    pub avatar_url: Option<String>,
    /// First-run onboarding finished on this device's data dir. False on a
    /// fresh or factory-reset data dir, whatever the webview remembers.
    #[serde(default)]
    pub onboarded: bool,
    /// The feature tour already ran once on this data dir.
    #[serde(default)]
    pub tour_seen: bool,
}

/// A chat opened in the transcript pane.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ChatSessionDto {
    pub id: String,
    pub title: String,
    pub runtime_id: String,
    pub messages: Vec<ChatMessageDto>,
}

/// A streaming update pushed as a Tauri event while an assistant reply is
/// generated. `phase` is "delta" (append `text`), "completed" (final body in
/// `text`), or "error" (`text` is the message).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ChatStreamEvent {
    pub session_id: String,
    pub message_id: String,
    pub phase: String,
    pub text: String,
}

impl ChatStreamEvent {
    /// The Tauri event name the frontend listens on.
    pub const EVENT: &'static str = "chat://stream";
}

/// Ephemeral background-activity for a running subprocess-agent turn (Claude
/// Code today): the tool calls it makes, their results, and a thinking marker.
/// Pushed live and NOT persisted — the UI shows it under the generating bubble
/// while the turn runs, then it's dropped when the turn completes. `kind` is
/// "tool" (a tool call — `id`/`name`/`inputJson`), "result" (its output — `id`
/// is the matching call id, `content`/`isError`), or "thinking".
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ChatActivityEvent {
    pub session_id: String,
    pub message_id: String,
    pub kind: String,
    pub id: String,
    pub name: String,
    pub input_json: String,
    pub content: String,
    pub is_error: bool,
}

impl ChatActivityEvent {
    /// The Tauri event name the frontend listens on.
    pub const EVENT: &'static str = "chat://activity";
}

/// One stage of a workflow definition, flattened for the wire: `kind` is
/// "agent" | "gate" and only that kind's optionals are populated.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WorkflowNodeDto {
    pub id: String,
    pub name: String,
    pub depends_on: Vec<String>,
    /// "agent" | "gate"
    pub kind: String,
    /// Agent stages: workspace agent id, empty ⇒ session primary runtime.
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub prompt_template: Option<String>,
    /// Gate stages.
    #[serde(default)]
    pub gate_title: Option<String>,
    #[serde(default)]
    pub gate_body: Option<String>,
    #[serde(default)]
    pub required_approvals: Option<u32>,
    /// "halt" | "routeTo"
    #[serde(default)]
    pub on_reject: Option<String>,
    #[serde(default)]
    pub reject_target: Option<String>,
}

/// A workflow definition (DAG of stages) available in a chat.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WorkflowDefinitionDto {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub input_label: Option<String>,
    pub nodes: Vec<WorkflowNodeDto>,
    /// Paused: listed, but no new run can start.
    #[serde(default)]
    pub disabled: bool,
}

/// Live state of one stage within a run.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WorkflowNodeRunDto {
    pub node_id: String,
    pub name: String,
    /// "agent" | "gate"
    pub kind: String,
    /// "pending" | "running" | "awaitingApproval" | "succeeded" | "failed" |
    /// "rejected" | "skipped"
    pub status: String,
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub proposal_id: Option<String>,
    pub output_excerpt: String,
    pub attempts: u32,
    pub error: String,
}

/// A workflow run card: frozen definition name + per-stage state.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WorkflowRunDto {
    pub id: String,
    pub definition_id: String,
    pub definition_name: String,
    pub input: String,
    /// "running" | "awaitingGate" | "completed" | "failed" | "halted" | "canceled"
    pub status: String,
    pub nodes: Vec<WorkflowNodeRunDto>,
    pub started_at: String,
    /// Whether an in-process driver is live for this run right now. False after
    /// an app restart until crash-recovery re-spawns the driver; gates the
    /// frontend's Resume affordance.
    pub driver_alive: bool,
    /// The run's coordinator lease (which device drives it, and whether that
    /// device is still beating). Absent for runs started before leases.
    #[serde(default)]
    #[ts(optional)]
    pub lease: Option<RunLeaseDto>,
}

/// Pushed as a Tauri event whenever a run's persisted state changes.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WorkflowRunEvent {
    pub session_id: String,
    pub run_id: String,
    pub status: String,
}

impl WorkflowRunEvent {
    /// The Tauri event name the frontend listens on.
    pub const EVENT: &'static str = "workflow://run";
}

/// One unit of queued work: an unanswered agent mention, tagged with the host
/// its agent is bound to and that host's live status. Surfaces the CLI's `ligus
/// queue` in the desktop Review pane so offline/detached work is visible and
/// reassignable (spec §12.5).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct QueuedWorkDto {
    pub session_id: String,
    pub chat_title: String,
    pub agent_id: String,
    pub agent_name: String,
    /// The host the agent is bound to (empty = the owner's device).
    pub host_id: String,
    pub host_label: String,
    /// "online" (host heartbeating) | "offline" (bound to a stale host) |
    /// "device" (owner-device, no worker host) | "unknown" (host not found).
    pub host_status: String,
}

/// A registered workspace host, for the "run on worker" reassignment menu.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WorkspaceHostDto {
    pub id: String,
    /// "device" | "worker"
    pub kind: String,
    pub label: String,
    pub online: bool,
    /// Last heartbeat as an RFC 3339 string, so the presence UI can render a
    /// relative "last seen 3m ago".
    pub last_seen: String,
    /// Model gateways this host's operator lists (names and wire only; the
    /// URL and credentials stay on the host).
    #[serde(default)]
    pub model_gateways: Vec<HostModelGatewayDto>,
    /// Display name of the member who registered the host (empty when
    /// unknown). Labels are chosen by their owners and can repeat, so a
    /// picker names the owner too.
    #[serde(default)]
    pub owner_name: String,
    /// The host belongs to the local account.
    #[serde(default)]
    pub mine: bool,
    /// What the host offers (names and kinds from its own configuration),
    /// with readiness and models when its signed beat reports them.
    #[serde(default)]
    pub offers: Vec<HostOfferDto>,
    /// One line for a Hosts row: "claude-code ready · codex not signed in"
    /// (empty: nothing to say).
    #[serde(default)]
    pub readiness: String,
    /// The host's Ligus build from its beat (empty: not reported).
    #[serde(default)]
    pub build: String,
    /// A newer release this box or worker could upgrade to: the latest one
    /// the desktop knows from the docs site's version manifest, when it is
    /// newer than [`Self::build`] (empty: up to date, or either unknown).
    /// Display only; nothing upgrades on its own.
    #[serde(default)]
    pub upgrade_to: String,
}

/// One source a host offers. Never a URL or a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct HostOfferDto {
    pub name: String,
    /// "cli" | "local" | "gateway" | "provider"
    pub kind: String,
    /// The harness that runs it (`claudeCode`, `ollama`); empty for a gateway.
    #[serde(default)]
    pub harness: String,
    /// A gateway's wire (`openai` | `anthropic`).
    #[serde(default)]
    pub wire: String,
    /// Installed and signed in (or the server answered); `None`: not reported.
    #[serde(default)]
    pub ready: Option<bool>,
    /// Why it isn't ready, in a few words.
    #[serde(default)]
    pub why: String,
    /// Models the host reported (at most 64).
    #[serde(default)]
    pub models: Vec<String>,
    /// The list is everything it runs.
    #[serde(default)]
    pub complete: bool,
}

/// A model gateway a host offers: its operator-given name and wire
/// (`openai` | `anthropic`). Never a URL or a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct HostModelGatewayDto {
    pub name: String,
    pub api: String,
}

/// Where the local user was last `@`-mentioned in a chat, so the sidebar can
/// highlight the channel (and chat) until it's read. Read state is per-device
/// (localStorage), so this DTO carries only the raw positions: a mention at
/// `last_mention_ordinal` is *unread* when it exceeds the read cursor the client
/// stored when it last opened the chat (≈ `message_count` at that time).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct MentionStateDto {
    pub session_id: String,
    /// The workspace this chat belongs to (local workspace id, or a relay-room
    /// workspace id). Lets the client group cross-workspace mention state so a
    /// background workspace's switcher entry can flag an unread self-mention.
    pub workspace_id: String,
    /// The channel the chat is filed under (empty = unfiled).
    pub channel_id: String,
    /// 1-based position (in canonical message order) of the last message that
    /// mentions the local user. Always ≥ 1 (chats with none are omitted).
    pub last_mention_ordinal: u32,
    /// Total messages in the chat — the value the client stores as its read
    /// cursor when it opens the chat.
    pub message_count: u32,
}

/// One entry in the LSP language-server registry, surfaced to the editor so it
/// can attach language intelligence only for servers the user actually has.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct LspServerDto {
    /// Stable registry id (e.g. "typescript", "rust-analyzer", "pyright", "gopls").
    pub id: String,
    /// Human-readable language(s) the server serves (e.g. "typescript/javascript").
    pub language: String,
    /// The command line the backend would spawn (binary + args), for display.
    pub command: String,
    /// Whether the server's binary is resolvable on the current PATH. When false
    /// the editor attaches nothing for this language and degrades silently.
    pub available: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regenerate all TypeScript bindings into `web/src/bindings/`. CI runs this
    /// (filter: `export_bindings`) and fails if the committed files differ.
    #[test]
    fn export_bindings() {
        const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/src/bindings");
        // ts-rs 12: export methods take a Config carrying the output dir (was
        // `export_all_to(DIR)` in ts-rs 10).
        let cfg = ts_rs::Config::new().with_out_dir(DIR);
        AppInfo::export_all(&cfg).unwrap();
        ChatMessageDto::export_all(&cfg).unwrap();
        ToolCallDto::export_all(&cfg).unwrap();
        ToolResultDto::export_all(&cfg).unwrap();
        ChatSummaryDto::export_all(&cfg).unwrap();
        ChannelDto::export_all(&cfg).unwrap();
        ChatSessionDto::export_all(&cfg).unwrap();
        ChatStreamEvent::export_all(&cfg).unwrap();
        ChatActivityEvent::export_all(&cfg).unwrap();
        GitFileDiffDto::export_all(&cfg).unwrap();
        FsEntryDto::export_all(&cfg).unwrap();
        AppSettingsDto::export_all(&cfg).unwrap();
        RuntimeSummaryDto::export_all(&cfg).unwrap();
        ContextTelemetryDto::export_all(&cfg).unwrap();
        WorkspaceAgentDto::export_all(&cfg).unwrap();
        AgentUsageDto::export_all(&cfg).unwrap();
        SkillDto::export_all(&cfg).unwrap();
        McpServerDto::export_all(&cfg).unwrap();
        ReactionDto::export_all(&cfg).unwrap();
        ProposalDto::export_all(&cfg).unwrap();
        ArchivedProposalDto::export_all(&cfg).unwrap();
        ImplementPreviewDto::export_all(&cfg).unwrap();
        RefinementDto::export_all(&cfg).unwrap();
        ApprovalDto::export_all(&cfg).unwrap();
        WorkspaceMemberDto::export_all(&cfg).unwrap();
        MemberRoleOptionDto::export_all(&cfg).unwrap();
        ApplyRequestDto::export_all(&cfg).unwrap();
        WorkspaceInfoDto::export_all(&cfg).unwrap();
        VaultSourceDto::export_all(&cfg).unwrap();
        WorkflowNodeDto::export_all(&cfg).unwrap();
        WorkflowDefinitionDto::export_all(&cfg).unwrap();
        WorkflowNodeRunDto::export_all(&cfg).unwrap();
        WorkflowRunDto::export_all(&cfg).unwrap();
        WorkflowRunEvent::export_all(&cfg).unwrap();
        QueuedWorkDto::export_all(&cfg).unwrap();
        WorkspaceHostDto::export_all(&cfg).unwrap();
        HostModelGatewayDto::export_all(&cfg).unwrap();
        HostOfferDto::export_all(&cfg).unwrap();
        MentionStateDto::export_all(&cfg).unwrap();
        LspServerDto::export_all(&cfg).unwrap();
        ProposalSetDto::export_all(&cfg).unwrap();
        OfferedFileDto::export_all(&cfg).unwrap();
    }
}

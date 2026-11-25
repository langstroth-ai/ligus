//! Flow: how work moves through a workspace — who gets a task, how it is
//! handed on, where it queues, and what agents leave in the workspace for each
//! other. The ideas come from how a honeybee colony allocates labour (Ligus is
//! short for *ligustica*, the Italian honeybee), but every mechanism here is
//! plain engineering:
//!
//! - **Hand-off cards and context parcels** ([`handoff`]): what an agent hands
//!   on, where it lives and how good it was, plus a compact summary so the
//!   receiver starts primed.
//! - **Outcome-weighted routing** ([`score`], [`route`]): a decayed score per
//!   agent and task class computed only from measured outcomes (approvals,
//!   revise loops, test runs, turns per success), sampled in proportion to the
//!   score with an adaptive exploration share. Ships in shadow mode.
//! - **Route cool-down** ([`route::RouteStop`]): a severe failure pulls a route
//!   out of rotation at once instead of waiting for its average to drift.
//! - **Backpressure** ([`tremble`]): queue length and waiting time at the human
//!   gate, a note telling upstream agents to stop opening new work, and opt-in
//!   recruiting of more reviewers or an idle host.
//! - **Threshold task board** ([`board`]) and **careers** ([`career`]): open
//!   work is picked up by the agent whose threshold it crosses first, with
//!   per-agent threshold diversity and a reversible default career.
//! - **Workspace marks and path zones** ([`marks`], [`zones`]): claims,
//!   under-construction locks and needs-review markers that agents read on
//!   their own schedule; "core" and "low-bar" path zones that set the review
//!   bar from where a change lands.
//!
//! Everything is recorded as additive [`FlowRecord`]s on the existing event
//! log (`SessionEvent::FlowRecorded`): an older build decodes the event as
//! `Unknown` and ignores it. Nothing here performs IO.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod asks;
pub mod board;
pub mod career;
pub mod directives;
pub mod handoff;
pub mod keys;
pub mod marks;
pub mod route;
pub mod score;
pub mod task_class;
pub mod tremble;
pub mod zones;

pub use board::{BoardItem, BoardItemKind};
pub use career::{AgentFlowProfile, Career};
pub use handoff::{ContextParcel, HandoffCard, HandoffQuality};
pub use marks::{MarkKind, WorkspaceMark};
pub use route::{RouteDecision, RouteMode, RouteStop};
pub use zones::{PathZone, ZoneKind};

/// The key routing and scoring use for "who": an agent id, or [`PRIMARY`] for
/// the chat's own runtime (the `@ligus` primary, which has no agent id).
pub const PRIMARY: &str = "primary";

/// [`PRIMARY`] for `None`, else the agent id as a string.
pub fn agent_key(agent: Option<Uuid>) -> String {
    agent.map(|a| a.to_string()).unwrap_or_else(|| PRIMARY.to_string())
}

/// The inverse of [`agent_key`]: `None` for the primary (or anything that
/// isn't an id).
pub fn agent_from_key(key: &str) -> Option<Uuid> {
    Uuid::parse_str(key).ok()
}

/// Milliseconds since the Unix epoch for a [`crate::Timestamp`].
pub fn ms(ts: &crate::Timestamp) -> i64 {
    (ts.inner().unix_timestamp_nanos() / 1_000_000) as i64
}

/// Milliseconds since the Unix epoch, now.
pub fn now_ms() -> i64 {
    ms(&crate::Timestamp::now())
}

/// What role a turn played, for the role mix: proposing work, reviewing it,
/// running tools, or a workflow stage. `conversation` is the default.
pub mod roles {
    pub const PROPOSER: &str = "proposer";
    pub const REVIEWER: &str = "reviewer";
    pub const TOOL_RUNNER: &str = "tool_runner";
    pub const STAGE: &str = "stage";
    pub const CONVERSATION: &str = "conversation";
}

/// Experiment arms a turn can be tagged with (shadow vs routed for routing;
/// note vs control for the stop-upstream note).
pub mod arms {
    /// Routing is in shadow mode: the router only logged its pick.
    pub const SHADOW: &str = "shadow";
    /// The router picked who answered.
    pub const ROUTED: &str = "routed";
    /// Someone addressed the agent by name (or routing is off).
    pub const DIRECT: &str = "direct";
}

/// One completed turn, tagged after it finishes: the measured facts routing
/// and the reports read. Nothing here is the agent's own claim; `tests_passed`
/// comes from the exit status of a test command the harness ran.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnTag {
    /// The reply message the turn produced.
    pub message_id: Uuid,
    /// The message it answered, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_id: Option<Uuid>,
    /// [`agent_key`] of the responder.
    pub agent: String,
    /// Runtime the turn ran on.
    #[serde(default)]
    pub runtime_id: String,
    /// Host (device or worker id) the turn ran on.
    #[serde(default)]
    pub host_id: String,
    pub task_class: String,
    /// See [`roles`].
    pub role: String,
    /// See [`arms`]; empty when no experiment applies.
    #[serde(default)]
    pub arm: String,
    /// Experiment arm of the stop-upstream note (`note` / `control`), if the
    /// A/B split was on.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub upstream_arm: String,
    /// The turn ended in an error (provider failure, timeout, refusal).
    #[serde(default)]
    pub error: bool,
    /// Short error class when `error` (see [`route::FailureKind`]).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error_kind: String,
    /// Result of the last test command the turn ran, if it ran one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_passed: Option<bool>,
    /// Tool calls the turn made.
    #[serde(default)]
    pub tool_calls: u32,
    /// Wall-clock duration.
    #[serde(default)]
    pub duration_ms: i64,
    /// Estimated cost in USD when the runtime has a configured price, else 0.
    #[serde(default)]
    pub cost_usd: f64,
    /// Proposals the turn filed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proposal_ids: Vec<Uuid>,
    /// The turn received a context parcel with its hand-off.
    #[serde(default)]
    pub handoff_parcel: bool,
    pub at_ms: i64,
}

/// A unit of measured progress on a chat (e.g. a test run newly passing).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressMark {
    /// The turn that made the progress.
    pub message_id: Uuid,
    pub agent: String,
    pub units: f64,
    /// `tests_passing` or `diff_converged`.
    pub kind: String,
    pub at_ms: i64,
}

/// A file a turn changed, by opaque key only (see [`keys`]): recorded once per
/// changed file when a proposal is applied, attributed to the producing turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceTouch {
    /// The reply message of the producing turn, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<Uuid>,
    pub proposal_id: Uuid,
    pub agent: String,
    /// Keyed hash of the workspace-relative path. Never the path.
    pub file_key: String,
    /// Keyed hash of the repository.
    pub repo_key: String,
    /// `edit`, `create` or `delete`.
    pub kind: String,
    pub at_ms: i64,
}

/// Measured waiting time at a hand-off point: a human gate (proposal created
/// → decided), a workflow stage (prompt posted → picked up by its host) or an
/// agent hand-off (card → the receiver's turn).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeWait {
    /// `gate`, `stage` or `handoff`.
    pub edge: String,
    /// Proposal id, `run:node`, or hand-off card id.
    pub reference: String,
    pub wait_ms: i64,
    /// Host the work waited on (stages).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host_id: String,
    pub at_ms: i64,
}

/// A backpressure response that fired: more reviewers were asked, a stage was
/// offered to an idle host, or upstream agents were told to stop opening work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecruitNotice {
    pub id: Uuid,
    /// `approvers`, `idle_host` or `stop_upstream`.
    pub action: String,
    /// Proposals waiting at the gate when it fired.
    #[serde(default)]
    pub queue_len: u32,
    /// Oldest wait at the gate.
    #[serde(default)]
    pub oldest_wait_ms: i64,
    /// Members asked to review (`approvers`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub member_ids: Vec<String>,
    /// Host the work was offered to, and the one it waited on (`idle_host`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub to_host: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from_host: String,
    /// `run:node` for a stage offer.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reference: String,
    pub at_ms: i64,
}

/// The canonical "home" for a workspace — where a joining or reconnecting
/// device should point. No secrets: never a token or passphrase.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HomeBeacon {
    /// Relay base URL the workspace syncs through (empty = no relay).
    #[serde(default)]
    pub relay_url: String,
    /// The branch chats are cut from (e.g. `main`).
    #[serde(default)]
    pub base_branch: String,
    /// Host that coordinates the workspace (runs its workflows, serves the
    /// phone), if any.
    #[serde(default)]
    pub coordinator_host_id: String,
    /// Free-form note ("ask Ana for an invite").
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub updated_by: String,
    #[serde(default)]
    pub at_ms: i64,
}

/// One flow record. Carried by `SessionEvent::FlowRecorded`. Records marked
/// "config" ride the workspace-config log (workspace-scoped and authorized on
/// ingest); the rest ride the chat they happened in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FlowRecord {
    /// A routing decision for one un-addressed message (chat log).
    RouteDecision(RouteDecision),
    /// A finished turn's measured facts (chat log).
    TurnTagged(TurnTag),
    /// A hand-off card, with an optional context parcel (chat log).
    Handoff(HandoffCard),
    /// A file changed by an applied proposal, by opaque key (chat log).
    Touch(WorkspaceTouch),
    /// Measured progress (chat log).
    Progress(ProgressMark),
    /// Waiting time at a gate, stage or hand-off (chat log).
    EdgeWait(EdgeWait),
    /// A backpressure response fired (chat log).
    Recruit(RecruitNotice),
    /// A route pulled out of rotation until a time (config).
    RouteStop(RouteStop),
    /// A mark left in the workspace (config).
    Mark(WorkspaceMark),
    /// A mark removed (config).
    MarkCleared { mark_id: Uuid },
    /// A path zone set or replaced (config, admin).
    Zone(PathZone),
    /// A path zone removed (config, admin).
    ZoneRemoved { zone_id: Uuid },
    /// An agent's flow profile (career override, threshold bias) (config).
    AgentProfile(AgentFlowProfile),
    /// The workspace's home beacon (config, admin).
    Beacon(HomeBeacon),
    /// One host's bounded daily outcome summary for one agent (config): the
    /// synced input to cross-device scoring, now that per-turn measurements
    /// are device-local facts (P0.5).
    AgentOutcomeSummary(AgentOutcomeSummary),
    /// An agent asked the chat's humans a question and paused its turn
    /// (chat log, Agent tier; D.4.2).
    HumanAsk(HumanAsk),
    /// A human answered an ask (chat log; humans only).
    HumanAnswered(HumanAnswered),
    /// One turn's Ligus tool use, content-free (chat log, Agent tier; D.3.5).
    TurnToolSummary(TurnToolSummary),
    /// Create or edit a workspace memory entry (config; a person,
    /// Contributor+; see [`crate::memory`]).
    MemoryWritten(crate::memory::MemoryWrite),
    /// Remove a memory entry, leaving a tombstone (config; a person).
    MemoryRemoved(crate::memory::MemoryRemoval),
    /// A proposed memory change (config; agents, the runtime, phones and
    /// members).
    MemoryProposed(crate::memory::MemoryProposal),
    /// A person accepts or rejects a memory proposal (config).
    MemoryProposalDecided(crate::memory::MemoryDecision),
    /// One writer's live task state on this chat (chat log).
    TaskState(crate::memory::TaskStateUpdate),
    /// A record type from a newer build — kept in the log, inert here.
    #[serde(other)]
    Unknown,
}

impl FlowRecord {
    /// Stable short name, for logs and the exporter.
    pub fn type_str(&self) -> &'static str {
        match self {
            FlowRecord::RouteDecision(_) => "routeDecision",
            FlowRecord::TurnTagged(_) => "turnTagged",
            FlowRecord::Handoff(_) => "handoff",
            FlowRecord::Touch(_) => "touch",
            FlowRecord::Progress(_) => "progress",
            FlowRecord::EdgeWait(_) => "edgeWait",
            FlowRecord::Recruit(_) => "recruit",
            FlowRecord::RouteStop(_) => "routeStop",
            FlowRecord::Mark(_) => "mark",
            FlowRecord::MarkCleared { .. } => "markCleared",
            FlowRecord::Zone(_) => "zone",
            FlowRecord::ZoneRemoved { .. } => "zoneRemoved",
            FlowRecord::AgentProfile(_) => "agentProfile",
            FlowRecord::Beacon(_) => "beacon",
            FlowRecord::AgentOutcomeSummary(_) => "agentOutcomeSummary",
            FlowRecord::HumanAsk(_) => "humanAsk",
            FlowRecord::HumanAnswered(_) => "humanAnswered",
            FlowRecord::TurnToolSummary(_) => "turnToolSummary",
            FlowRecord::MemoryWritten(_) => "memoryWritten",
            FlowRecord::MemoryRemoved(_) => "memoryRemoved",
            FlowRecord::MemoryProposed(_) => "memoryProposed",
            FlowRecord::MemoryProposalDecided(_) => "memoryProposalDecided",
            FlowRecord::TaskState(_) => "taskState",
            FlowRecord::Unknown => "unknown",
        }
    }

    /// Whether the record is workspace configuration: it rides the
    /// workspace-config log, is workspace-scoped, and is re-authorized when a
    /// peer's copy is folded.
    pub fn is_config(&self) -> bool {
        matches!(
            self,
            FlowRecord::RouteStop(_)
                | FlowRecord::Mark(_)
                | FlowRecord::MarkCleared { .. }
                | FlowRecord::Zone(_)
                | FlowRecord::ZoneRemoved { .. }
                | FlowRecord::AgentProfile(_)
                | FlowRecord::Beacon(_)
                | FlowRecord::AgentOutcomeSummary(_)
                | FlowRecord::MemoryWritten(_)
                | FlowRecord::MemoryRemoved(_)
                | FlowRecord::MemoryProposed(_)
                | FlowRecord::MemoryProposalDecided(_)
        )
    }

    /// Per-turn measurement records (P0.5): no longer written to the signed
    /// log. They are device-local facts (`ligus.facts.v1`), and a bounded
    /// daily [`AgentOutcomeSummary`] is what syncs. Ones already in a log
    /// still fold.
    pub fn is_measurement(&self) -> bool {
        matches!(
            self,
            FlowRecord::TurnTagged(_) | FlowRecord::Touch(_) | FlowRecord::Progress(_) | FlowRecord::EdgeWait(_) | FlowRecord::RouteStop(_)
        )
    }

    /// Whether only an admin may write it: path zones set the review bar and
    /// the beacon tells devices where home is.
    pub fn is_governance(&self) -> bool {
        matches!(
            self,
            FlowRecord::Zone(_) | FlowRecord::ZoneRemoved { .. } | FlowRecord::Beacon(_) | FlowRecord::Unknown
        )
    }
}

/// How many of each high-volume record a session keeps in its projection.
/// Older entries are dropped from the folded state (they stay in the log).
pub const MAX_KEPT: usize = 2000;

/// Flow state folded from a session's [`FlowRecord`]s.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowState {
    /// Routing decisions by trigger message id. First decision per trigger in
    /// canonical order wins, so every device honours the same one.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub route_decisions: BTreeMap<Uuid, RouteDecision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub turns: Vec<TurnTag>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handoffs: Vec<HandoffCard>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touches: Vec<WorkspaceTouch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub progress: Vec<ProgressMark>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waits: Vec<EdgeWait>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recruits: Vec<RecruitNotice>,
    /// Latest stop per route key (agent key or `runtime:<id>`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub route_stops: BTreeMap<String, RouteStop>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub marks: BTreeMap<Uuid, WorkspaceMark>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zones: Vec<PathZone>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<Uuid, AgentFlowProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beacon: Option<HomeBeacon>,
    /// Daily outcome summaries per agent key, newest last, at most
    /// [`MAX_SUMMARIES_PER_AGENT`] each; one per (host, day).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outcome_summaries: BTreeMap<String, Vec<AgentOutcomeSummary>>,
    /// Questions agents asked the chat's humans, by ask id (D.4.2).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub asks: BTreeMap<Uuid, AskState>,
    /// Per-turn Ligus tool summaries, newest last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_summaries: Vec<TurnToolSummary>,
    /// Workspace memory (config log; [`crate::memory`]).
    #[serde(default, skip_serializing_if = "crate::memory::MemoryState::is_empty")]
    pub memory: crate::memory::MemoryState,
    /// Live task-state slots (chat log).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<crate::memory::TaskSlot>,
}

/// Bounds on asks and tool summaries (fold rules, `authz::bounds`).
pub const MAX_ASK_QUESTION_CHARS: usize = 2000;
pub const MAX_ASK_OPTIONS: usize = 6;
pub const MAX_ASK_OPTION_CHARS: usize = 200;
pub const MAX_ANSWER_CHARS: usize = 2000;
pub const MAX_SUMMARY_TOOLS: usize = 16;
/// Asks kept per chat (open or settled).
pub const MAX_ASKS_PER_CHAT: usize = 256;

/// An agent's question to the chat's humans. The asking turn is paused until
/// the first answer (canonical order) or its timeout.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanAsk {
    pub ask_id: Uuid,
    /// The asking turn (its reply message id).
    pub turn_id: Uuid,
    /// [`agent_key`] of the asking agent.
    pub agent_key: String,
    pub question: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// When the asking turn stops waiting (ms since the epoch).
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub expires_at_ms: i64,
    /// The host closed the ask without an answer (timeout, cancel, restart).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub closed: bool,
    pub at_ms: i64,
    /// A permission escalation (an ACP agent asking to run something above
    /// its tier), posted by the host, rather than an agent's question. Only
    /// the host's person or the agent's owner can grant one. Older asks
    /// lack the field and are questions.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub permission: bool,
    /// [`ASK_V2`] for an ask with the 0.4 rules (who may answer, quorum,
    /// effect; `docs/design/agent-questions.md` section 5). Absent before.
    /// Every field from here on decodes leniently: a value this build can't
    /// read folds as absent, never as an event it can't decode (which older
    /// builds would accept), so every version accepts the same events.
    #[serde(default, skip_serializing_if = "is_zero_u8", deserialize_with = "lenient")]
    pub v: u8,
    /// Who may answer. Absent: any Contributor or above (the older rule).
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub audience: Option<AskAudience>,
    /// Distinct eligible people who must pick the same option (1 when
    /// absent).
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub quorum: Option<u8>,
    /// A free-text answer is allowed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not", deserialize_with = "lenient")]
    pub allow_text: bool,
    /// What an answer authorises, written only by a host (absent: a
    /// question). It counts only when [`asks`] says so.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub effect: Option<AskEffect>,
    /// Where the ask came from (a sub-agent, a child chat, outside Ligus).
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub origin: Option<AskOrigin>,
    /// On a closing record by the asker: the answer event it acted on.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub settled_by: Option<Uuid>,
}

/// The ask rules version [`HumanAsk::v`] names.
pub const ASK_V2: u8 = 2;
/// [`AskAudience::kind`]: one or more named members.
pub const AUDIENCE_PEOPLE: &str = "people";
/// [`AskAudience::kind`]: any member at or above a role.
pub const AUDIENCE_ROOM: &str = "room";
/// [`AskEffect::kind`]: allow one tool call once.
pub const EFFECT_PERMISSION: &str = "permission";
/// [`AskEffect::kind`]: reveal a secret (defined, posted by no host yet).
pub const EFFECT_SECRET: &str = "secret";
/// [`AskEffect::presence`]: an answer counts only with the person's passkey.
pub const PRESENCE_REQUIRED: &str = "required";
/// [`AskOrigin::via`] values.
pub const ORIGIN_SUBAGENT: &str = "subagent";
pub const ORIGIN_CHAT: &str = "chat";
pub const ORIGIN_OUTSIDE: &str = "outside";
/// The most members an audience names, and the most voters a quorum needs.
pub const MAX_AUDIENCE_PEOPLE: usize = 8;
pub const MAX_QUORUM: u8 = 5;
/// The longest effect detail a reader shows (larger: the effect is inert).
pub const MAX_EFFECT_DETAIL_BYTES: usize = 16 * 1024;

/// Who may answer a [`HumanAsk`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskAudience {
    /// [`AUDIENCE_PEOPLE`] or [`AUDIENCE_ROOM`]; anything else: nobody.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub member_ids: Vec<String>,
    /// For a room: `contributor`, `admin` or `owner`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_role: Option<String>,
}

impl AskAudience {
    pub fn people(ids: Vec<String>) -> Self {
        Self { kind: AUDIENCE_PEOPLE.into(), member_ids: ids, min_role: None }
    }
    pub fn room(min_role: crate::WorkspaceRole) -> Self {
        let r = match min_role {
            crate::WorkspaceRole::Owner => "owner",
            crate::WorkspaceRole::Admin => "admin",
            _ => "contributor",
        };
        Self { kind: AUDIENCE_ROOM.into(), member_ids: Vec::new(), min_role: Some(r.into()) }
    }
    /// The room's floor, never below Contributor; `None` for anything this
    /// build can't read.
    pub fn floor(&self) -> Option<crate::WorkspaceRole> {
        match self.min_role.as_deref().unwrap_or("contributor") {
            "contributor" => Some(crate::WorkspaceRole::Contributor),
            "admin" => Some(crate::WorkspaceRole::Admin),
            "owner" => Some(crate::WorkspaceRole::Owner),
            _ => None,
        }
    }
    /// A single named person.
    pub fn single(&self) -> Option<&str> {
        (self.kind == AUDIENCE_PEOPLE && self.member_ids.len() == 1).then(|| self.member_ids[0].as_str())
    }
}

/// What answering a [`HumanAsk`] authorises (section 6 of the note).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskEffect {
    /// [`EFFECT_PERMISSION`] or [`EFFECT_SECRET`].
    pub kind: String,
    /// The tool the call is for (`Bash`, an ACP tool kind).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool: String,
    /// The agent's own description of the call (unverified; shown as such).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// The call's own content: the command, the path and content or diff.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    /// A secret's name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The device that posted it and would act on the answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_device_id: Option<Uuid>,
    /// [`PRESENCE_REQUIRED`], or `none`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub presence: String,
    /// The agent offered only a permission for the rest of its session
    /// (the 0.3.1 rule: a person may pick it knowingly).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub standing: bool,
    /// The call is too large to review on a card: no Allow is offered.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub too_large: bool,
}

impl AskEffect {
    /// An answer counts only with its author's passkey presence.
    pub fn needs_presence(&self) -> bool {
        self.presence == PRESENCE_REQUIRED || self.kind == EFFECT_SECRET
    }
}

/// Where a [`HumanAsk`] came from.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskOrigin {
    /// [`ORIGIN_SUBAGENT`], [`ORIGIN_CHAT`] or [`ORIGIN_OUTSIDE`].
    pub via: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<Uuid>,
    /// For an outside prompt: the runtime (`claude-code`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub runtime: String,
}

fn is_zero_u8(x: &u8) -> bool {
    *x == 0
}

/// Decode a field, or take its default when the value isn't one this build
/// reads (the record still decodes).
fn lenient<'de, D: serde::Deserializer<'de>, T: serde::de::DeserializeOwned + Default>(d: D) -> Result<T, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}

/// A human's answer to a [`HumanAsk`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanAnswered {
    pub ask_id: Uuid,
    pub answer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_index: Option<u32>,
    /// Display name of who answered.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
    pub at_ms: i64,
    /// Free text, apart from the chosen option (whose text stays in
    /// `answer` for older readers).
    #[serde(default, skip_serializing_if = "String::is_empty", deserialize_with = "lenient")]
    pub text: String,
    /// [`asks::ask_digest`] of the ask this answers, computed by the
    /// answering device from the signed record it received (hex).
    #[serde(default, skip_serializing_if = "String::is_empty", deserialize_with = "lenient")]
    pub ask_digest: String,
    /// "Not now": closes an ask to one named person; in a room, that
    /// person's abstention.
    #[serde(default, skip_serializing_if = "std::ops::Not::not", deserialize_with = "lenient")]
    pub decline: bool,
}

/// An ask and its settlement, as folded.
///
/// Folded with its envelope's provenance ([`asks`]): `ask.permission` is
/// set only when the asking turn's own host posted it, `ask.closed` only by
/// a close from the ask's own signer (or its host), and `answer` is the
/// first answer in canonical order from someone who may give it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskState {
    pub ask: HumanAsk,
    /// The first answer in canonical order that counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<HumanAnswered>,
    /// Who posted the ask, when it wasn't the asking agent or its turn's
    /// host (a person, or another device, signed it under the agent's
    /// name). Shown as theirs; it is never a permission prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posted_by: Option<AskPoster>,
    /// The key that signed the ask (a device, or an agent key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer: Option<Uuid>,
    /// The device that key resolves to (itself, or the host that certified
    /// the agent key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<Uuid>,
    /// Settled as builds before these rules fold it (any close naming the
    /// asking turn, any answer). Only eviction reads it, so every version
    /// keeps the same asks and judges an answer's `optionIndex` alike.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub legacy_settled: bool,
    /// [`asks::ask_digest`] of the record that posted the ask (hex; empty
    /// when it was folded without its envelope).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub digest: String,
    /// The config frontier the ask was stamped with: an answer stamped
    /// with an older one doesn't count (v2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontier: Option<crate::identity::CfgFrontier>,
    /// Answers that count, in canonical order (at most
    /// [`MAX_COUNTED_ANSWERS`]): each eligible person's latest is their vote.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub counted: Vec<AskVote>,
    /// Who settled it: the voters for the winning option (one for a
    /// first-answer ask), in canonical order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answered_by: Vec<AskVote>,
    /// The one person it was put to said "Not now".
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub declined: bool,
    /// Answers that were kept but don't count (not eligible, no or another
    /// digest, no presence where it is needed), at most
    /// [`MAX_COUNTED_ANSWERS`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncounted: Vec<AskVote>,
}

/// Answers an ask keeps in [`AskState::counted`] and [`AskState::uncounted`].
pub const MAX_COUNTED_ANSWERS: usize = 32;

/// One answer as an ask's fold keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskVote {
    /// The answer's signed author (a member id).
    pub member_id: String,
    /// The name its stamp gave (readers show the roster's).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The option's text as answered.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub answer: String,
    /// The event that carried it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<Uuid>,
    /// The device that signed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_index: Option<u32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub decline: bool,
    pub at_ms: i64,
    /// Why it doesn't count (an [`AskState::uncounted`] entry).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
}

/// Who signed an ask that isn't the asking agent's or its host's.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskPoster {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
}

impl AskState {
    /// Not settled, closed or declined (expiry is a view: see
    /// [`AskState::is_live`]).
    pub fn is_open(&self) -> bool {
        self.answer.is_none() && !self.ask.closed && !self.declined
    }

    /// Open and not past its expiry at `now_ms` (readers and attention
    /// counts; never the fold).
    pub fn is_live(&self, now_ms: i64) -> bool {
        self.is_open() && (self.ask.expires_at_ms == 0 || now_ms < self.ask.expires_at_ms)
    }

    /// An ask with the 0.4 rules.
    pub fn is_v2(&self) -> bool {
        self.ask.v >= ASK_V2
    }

    /// The effect that counts (a grant card), if any: a v2 effect that
    /// passed the fold, or an older permission prompt from the turn's host.
    pub fn grant(&self) -> Option<AskEffect> {
        if let Some(e) = &self.ask.effect {
            return Some(e.clone());
        }
        self.ask.permission.then(|| AskEffect { kind: EFFECT_PERMISSION.into(), ..Default::default() })
    }

    /// The quorum, between 1 and [`MAX_QUORUM`].
    pub fn quorum(&self) -> u8 {
        self.ask.quorum.unwrap_or(1).clamp(1, MAX_QUORUM)
    }

    /// Each eligible person's current vote (their latest counted answer;
    /// a "Not now" in a room is an abstention and no vote).
    pub fn tally(&self) -> Vec<u32> {
        let mut latest: BTreeMap<&str, &AskVote> = BTreeMap::new();
        for v in &self.counted {
            latest.insert(v.member_id.as_str(), v);
        }
        let mut t = vec![0u32; self.ask.options.len()];
        for v in latest.values() {
            if let Some(i) = v.option_index.filter(|_| !v.decline) {
                if let Some(slot) = t.get_mut(i as usize) {
                    *slot += 1;
                }
            }
        }
        t
    }

    /// The name to show for [`AskState::posted_by`]: the roster's name for
    /// that member (`members`, the chat's roster), never the stamp's own
    /// claim, which its signer chose; the member id when unknown.
    pub fn poster_name(&self, members: &[crate::identity::WorkspaceMember]) -> Option<String> {
        let p = self.posted_by.as_ref()?;
        Some(members.iter().find(|m| m.id == p.id).map(|m| m.actor.display_name.clone()).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| p.id.clone()))
    }

    /// Settled for eviction: what every version agrees on (the v1 shadow:
    /// any close naming the asking turn, any answer). Every way an ask
    /// stops being open sets it too, so this is what 0.3 reads.
    fn evictable(&self) -> bool {
        self.legacy_settled
    }
}

/// What one turn did with the Ligus tools, without content: call counts per
/// tool, denials, and whether a budget ran out.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnToolSummary {
    pub turn_id: Uuid,
    pub agent_key: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub calls: BTreeMap<String, u32>,
    #[serde(default)]
    pub denied: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub budget_exhausted: bool,
    pub at_ms: i64,
}

fn is_zero_i64(x: &i64) -> bool {
    *x == 0
}

/// Fold cap on summaries kept per agent.
pub const MAX_SUMMARIES_PER_AGENT: usize = 400;
/// Cap on any one count in a summary (turns, per-class counts) and on its
/// per-summary samples: a host doesn't run more turns of one agent in a day.
pub const MAX_SUMMARY_COUNT: u32 = 2_000;
/// Cap on the distinct agent keys that carry summaries.
pub const MAX_SUMMARY_AGENTS: usize = 256;
/// Cap on a summary's estimated cost, in USD.
pub const MAX_SUMMARY_COST_USD: f64 = 100_000.0;
/// Cap on the task classes one summary carries.
pub const MAX_SUMMARY_CLASSES: usize = 16;

/// One host's outcome counts for one agent over one UTC day. Ids, enums and
/// numbers only.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentOutcomeSummary {
    /// [`agent_key`] of the agent.
    pub agent_key: String,
    /// The host (device or worker id) whose turns these are.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host_id: String,
    /// Start of the UTC day, in ms since the epoch.
    pub window_start: i64,
    pub turns: u32,
    /// Proposals filed by these turns, by their status when summarized.
    #[serde(default)]
    pub accepted: u32,
    #[serde(default)]
    pub reverted: u32,
    #[serde(default)]
    pub rejected: u32,
    /// Turns per task class (at most [`MAX_SUMMARY_CLASSES`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub task_class_counts: BTreeMap<String, u32>,
    /// Turns that ended in an error, per task class.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub errors_by_class: BTreeMap<String, u32>,
    /// Test runs that passed / failed, per task class.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tests_passed_by_class: BTreeMap<String, u32>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tests_failed_by_class: BTreeMap<String, u32>,
    /// Estimated cost of these turns, in USD.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cost_usd: f64,
}

fn is_zero(x: &f64) -> bool {
    *x == 0.0
}

impl AgentOutcomeSummary {
    /// Keep the class maps within [`MAX_SUMMARY_CLASSES`] (the busiest classes).
    pub fn bounded(mut self) -> Self {
        // Counts and cost come off the wire: clamp them so one summary can't
        // inflate scores or make a scorer expand billions of samples.
        self.turns = self.turns.min(MAX_SUMMARY_COUNT);
        self.accepted = self.accepted.min(MAX_SUMMARY_COUNT);
        self.reverted = self.reverted.min(MAX_SUMMARY_COUNT);
        self.rejected = self.rejected.min(MAX_SUMMARY_COUNT);
        for m in [&mut self.task_class_counts, &mut self.errors_by_class, &mut self.tests_passed_by_class, &mut self.tests_failed_by_class] {
            for v in m.values_mut() {
                *v = (*v).min(MAX_SUMMARY_COUNT);
            }
        }
        self.cost_usd = if self.cost_usd.is_finite() { self.cost_usd.clamp(0.0, MAX_SUMMARY_COST_USD) } else { 0.0 };
        if self.task_class_counts.len() > MAX_SUMMARY_CLASSES {
            let mut by: Vec<(String, u32)> = self.task_class_counts.clone().into_iter().collect();
            by.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let keep: std::collections::BTreeSet<String> = by.into_iter().take(MAX_SUMMARY_CLASSES).map(|(k, _)| k).collect();
            for m in [&mut self.task_class_counts, &mut self.errors_by_class, &mut self.tests_passed_by_class, &mut self.tests_failed_by_class] {
                m.retain(|k, _| keep.contains(k));
            }
        }
        for m in [&mut self.errors_by_class, &mut self.tests_passed_by_class, &mut self.tests_failed_by_class] {
            while m.len() > MAX_SUMMARY_CLASSES {
                let k = m.keys().next_back().cloned().unwrap_or_default();
                m.remove(&k);
            }
        }
        self
    }
}

/// Start of the UTC day containing `at_ms`.
pub fn utc_day(at_ms: i64) -> i64 {
    at_ms.div_euclid(86_400_000) * 86_400_000
}

fn push_capped<T>(v: &mut Vec<T>, item: T) {
    v.push(item);
    if v.len() > MAX_KEPT {
        let drop = v.len() - MAX_KEPT;
        v.drain(..drop);
    }
}

impl FlowState {
    /// Nothing recorded — keeps a session that never used flow byte-identical
    /// when serialized (snapshots are signed over their serialized form).
    pub fn is_empty(&self) -> bool {
        *self == FlowState::default()
    }

    /// Fold one record. Deterministic in canonical order.
    pub fn apply(&mut self, record: &FlowRecord) {
        match record {
            FlowRecord::RouteDecision(d) => {
                self.route_decisions.entry(d.trigger_id).or_insert_with(|| d.clone());
                if self.route_decisions.len() > MAX_KEPT {
                    if let Some(oldest) = self
                        .route_decisions
                        .iter()
                        .min_by_key(|(_, d)| d.at_ms)
                        .map(|(k, _)| *k)
                    {
                        self.route_decisions.remove(&oldest);
                    }
                }
            }
            FlowRecord::TurnTagged(t) => {
                if !self.turns.iter().any(|x| x.message_id == t.message_id) {
                    push_capped(&mut self.turns, t.clone());
                }
            }
            FlowRecord::Handoff(h) => {
                if let Some(slot) = self.handoffs.iter_mut().find(|x| x.id == h.id) {
                    *slot = h.clone();
                } else {
                    push_capped(&mut self.handoffs, h.clone());
                }
            }
            FlowRecord::Touch(t) => push_capped(&mut self.touches, t.clone()),
            FlowRecord::Progress(p) => push_capped(&mut self.progress, p.clone()),
            FlowRecord::EdgeWait(w) => push_capped(&mut self.waits, w.clone()),
            FlowRecord::Recruit(r) => {
                if !self.recruits.iter().any(|x| x.id == r.id) {
                    push_capped(&mut self.recruits, r.clone());
                }
            }
            FlowRecord::RouteStop(s) => {
                self.route_stops.insert(s.route.clone(), s.clone());
            }
            FlowRecord::Mark(m) => {
                self.marks.insert(m.id, m.clone());
            }
            FlowRecord::MarkCleared { mark_id } => {
                self.marks.remove(mark_id);
            }
            FlowRecord::Zone(z) => {
                if let Some(slot) = self.zones.iter_mut().find(|x| x.id == z.id) {
                    *slot = z.clone();
                } else {
                    self.zones.push(z.clone());
                }
            }
            FlowRecord::ZoneRemoved { zone_id } => self.zones.retain(|z| &z.id != zone_id),
            FlowRecord::AgentProfile(p) => {
                self.profiles.insert(p.agent_id, p.clone());
            }
            FlowRecord::Beacon(b) => self.beacon = Some(b.clone()),
            FlowRecord::AgentOutcomeSummary(sum) => {
                if !self.outcome_summaries.contains_key(&sum.agent_key) && self.outcome_summaries.len() >= MAX_SUMMARY_AGENTS {
                    return;
                }
                let list = self.outcome_summaries.entry(sum.agent_key.clone()).or_default();
                // One per (host, day): a re-emission replaces.
                if let Some(slot) = list.iter_mut().find(|x| x.host_id == sum.host_id && x.window_start == sum.window_start) {
                    *slot = sum.clone().bounded();
                } else {
                    list.push(sum.clone().bounded());
                    list.sort_by_key(|x| (x.window_start, x.host_id.clone()));
                    if list.len() > MAX_SUMMARIES_PER_AGENT {
                        let drop = list.len() - MAX_SUMMARIES_PER_AGENT;
                        list.drain(..drop);
                    }
                }
            }
            FlowRecord::HumanAsk(_) | FlowRecord::HumanAnswered(_) => {
                self.apply_ask(record, &asks::AskProvenance::legacy());
            }
            FlowRecord::TurnToolSummary(t) => {
                if !self.tool_summaries.iter().any(|x| x.turn_id == t.turn_id) {
                    push_capped(&mut self.tool_summaries, t.clone());
                }
            }
            // Folded with their envelope's provenance
            // (`memory::apply_enveloped`, from the event fold), never here.
            FlowRecord::MemoryWritten(_)
            | FlowRecord::MemoryRemoved(_)
            | FlowRecord::MemoryProposed(_)
            | FlowRecord::MemoryProposalDecided(_)
            | FlowRecord::TaskState(_) => {}
            FlowRecord::Unknown => {}
        }
    }

    /// The hand-off card that led to `responder`'s next turn in this chat:
    /// the latest card addressed to it that no later turn of its own has
    /// consumed yet.
    pub fn pending_handoff_for(&self, agent: &str) -> Option<&HandoffCard> {
        let card = self.handoffs.iter().rev().find(|h| h.to == agent)?;
        let consumed = self.turns.iter().any(|t| t.agent == agent && t.at_ms > card.at_ms);
        (!consumed).then_some(card)
    }

    /// The reply that answered `card`: the receiver's first turn in this chat
    /// after the card was recorded (the same turn [`Self::pending_handoff_for`]
    /// counts as consuming it). `None` until the receiver has replied.
    pub fn handoff_reply(&self, card: &HandoffCard) -> Option<Uuid> {
        self.turns
            .iter()
            .filter(|t| t.agent == card.to && t.at_ms > card.at_ms)
            .min_by_key(|t| t.at_ms)
            .map(|t| t.message_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_answer_wins_and_closed_asks_take_none() {
        let ask = HumanAsk { ask_id: Uuid::from_u128(1), turn_id: Uuid::from_u128(2), agent_key: "a".into(), question: "q?".into(), at_ms: 1, ..Default::default() };
        let mut s = FlowState::default();
        s.apply(&FlowRecord::HumanAsk(ask.clone()));
        assert!(s.asks[&ask.ask_id].is_open());
        let ans = |t: &str| FlowRecord::HumanAnswered(HumanAnswered { ask_id: ask.ask_id, answer: t.into(), at_ms: 2, ..Default::default() });
        s.apply(&ans("first"));
        s.apply(&ans("second"));
        assert_eq!(s.asks[&ask.ask_id].answer.as_ref().unwrap().answer, "first");
        // Another turn can't rewrite or close it.
        let mut forged = ask.clone();
        forged.turn_id = Uuid::from_u128(9);
        forged.question = "other".into();
        s.apply(&FlowRecord::HumanAsk(forged));
        assert_eq!(s.asks[&ask.ask_id].ask.question, "q?");
        // A closed ask takes no answer.
        let mut t = FlowState::default();
        t.apply(&FlowRecord::HumanAsk(ask.clone()));
        t.apply(&FlowRecord::HumanAsk(HumanAsk { closed: true, ..ask.clone() }));
        t.apply(&ans("late"));
        assert!(t.asks[&ask.ask_id].answer.is_none() && !t.asks[&ask.ask_id].is_open());
        // Wire tags.
        assert!(serde_json::to_string(&FlowRecord::HumanAsk(ask)).unwrap().contains("\"type\":\"humanAsk\""));
    }

    #[test]
    fn empty_state_serializes_to_an_empty_object() {
        assert_eq!(serde_json::to_string(&FlowState::default()).unwrap(), "{}");
        assert!(FlowState::default().is_empty());
    }

    #[test]
    fn unknown_record_type_decodes_as_unknown() {
        let r: FlowRecord = serde_json::from_str(r#"{"type":"somethingNew","x":1}"#).unwrap();
        assert_eq!(r, FlowRecord::Unknown);
        let mut s = FlowState::default();
        s.apply(&r);
        assert!(s.is_empty());
    }

    #[test]
    fn first_route_decision_per_trigger_wins() {
        let trigger = Uuid::new_v4();
        let mut a = route::RouteDecision::shadow_example(trigger);
        a.preferred = "a".into();
        let mut b = a.clone();
        b.preferred = "b".into();
        let mut s = FlowState::default();
        s.apply(&FlowRecord::RouteDecision(a));
        s.apply(&FlowRecord::RouteDecision(b));
        assert_eq!(s.route_decisions[&trigger].preferred, "a");
    }

    #[test]
    fn config_and_governance_classes() {
        assert!(FlowRecord::MarkCleared { mark_id: Uuid::nil() }.is_config());
        assert!(!FlowRecord::MarkCleared { mark_id: Uuid::nil() }.is_governance());
        assert!(FlowRecord::ZoneRemoved { zone_id: Uuid::nil() }.is_governance());
        assert!(!FlowRecord::Progress(ProgressMark {
            message_id: Uuid::nil(),
            agent: PRIMARY.into(),
            units: 1.0,
            kind: "tests_passing".into(),
            at_ms: 0
        })
        .is_config());
    }

    #[test]
    fn agent_keys_round_trip() {
        let id = Uuid::new_v4();
        assert_eq!(agent_from_key(&agent_key(Some(id))), Some(id));
        assert_eq!(agent_key(None), PRIMARY);
        assert_eq!(agent_from_key(PRIMARY), None);
    }

    #[test]
    fn pending_handoff_is_consumed_by_the_receivers_next_turn() {
        let mut s = FlowState::default();
        let card = handoff::HandoffCard::example("a", "b", 10);
        s.apply(&FlowRecord::Handoff(card));
        assert!(s.pending_handoff_for("b").is_some());
        assert!(s.pending_handoff_for("a").is_none());
        s.apply(&FlowRecord::TurnTagged(TurnTag {
            message_id: Uuid::new_v4(),
            trigger_id: None,
            agent: "b".into(),
            runtime_id: String::new(),
            host_id: String::new(),
            task_class: "general".into(),
            role: roles::CONVERSATION.into(),
            arm: String::new(),
            upstream_arm: String::new(),
            error: false,
            error_kind: String::new(),
            tests_passed: None,
            tool_calls: 0,
            duration_ms: 0,
            cost_usd: 0.0,
            proposal_ids: vec![],
            handoff_parcel: true,
            at_ms: 20,
        }));
        assert!(s.pending_handoff_for("b").is_none());
    }

    fn turn(agent: &str, at_ms: i64) -> TurnTag {
        TurnTag {
            message_id: Uuid::new_v4(),
            trigger_id: None,
            agent: agent.into(),
            runtime_id: String::new(),
            host_id: String::new(),
            task_class: "general".into(),
            role: roles::CONVERSATION.into(),
            arm: String::new(),
            upstream_arm: String::new(),
            error: false,
            error_kind: String::new(),
            tests_passed: None,
            tool_calls: 0,
            duration_ms: 0,
            cost_usd: 0.0,
            proposal_ids: vec![],
            handoff_parcel: false,
            at_ms,
        }
    }

    #[test]
    fn handoff_reply_is_the_receivers_first_turn_after_the_card() {
        let mut s = FlowState::default();
        let card = handoff::HandoffCard::example("a", "b", 10);
        s.apply(&FlowRecord::TurnTagged(turn("b", 5)));
        s.apply(&FlowRecord::Handoff(card.clone()));
        assert_eq!(s.handoff_reply(&card), None, "an earlier turn of the receiver isn't the reply");
        s.apply(&FlowRecord::TurnTagged(turn("a", 15)));
        assert_eq!(s.handoff_reply(&card), None, "the sender's turn isn't the reply");
        let later = turn("b", 30);
        let first = turn("b", 20);
        s.apply(&FlowRecord::TurnTagged(later.clone()));
        s.apply(&FlowRecord::TurnTagged(first.clone()));
        assert_eq!(s.handoff_reply(&card), Some(first.message_id), "the earliest turn after the card, in any fold order");
    }
}

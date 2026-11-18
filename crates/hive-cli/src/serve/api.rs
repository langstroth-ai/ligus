//! The phone-facing HTTP API + SSE stream (contract: `docs/ios-app.md`).
//!
//! JSON, camelCase, ISO-8601 UTC timestamps. Every route but `GET /v1/health`
//! needs `Authorization: Bearer <token>`; health answers `{ok, version}` to
//! anyone and adds `workspace` / `deviceLabel` only when authorized.
//!
//! Handlers lock the shared chat service only inside synchronous helpers, so a
//! guard never lives across an `.await`.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Extension, Json, Router};
use futures_util::stream::{self, Stream, StreamExt};
use hive_core::proposals::ProposalRefinement;
use hive_core::{
    ActionProposal, ChatMessage, ChatSession, MessageRole, ProposalKind, ProposalStatus, Timestamp, WorkspaceRole,
};
use serde::Deserialize;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use super::tunnel::TunnelPeer;
use super::{dashboard, lab, pair, Shared};

type S = State<Arc<Shared>>;

// ── Errors ─────────────────────────────────────────────────────────────────

pub struct ApiError(StatusCode, String);

impl ApiError {
    pub(super) fn not_found(what: &str) -> Self {
        Self(StatusCode::NOT_FOUND, format!("{what} not found"))
    }
    pub(super) fn new(code: StatusCode, msg: impl Into<String>) -> Self {
        Self(code, msg.into())
    }
    pub(super) fn bad_request(msg: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, msg.into())
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}

impl From<hive_runtime::chat_service::ChatError> for ApiError {
    fn from(e: hive_runtime::chat_service::ChatError) -> Self {
        use hive_runtime::chat_service::ChatError;
        match e {
            ChatError::Unauthorized(m) if m == hive_core::authz::PRESENCE_REQUIRED => {
                Self(StatusCode::FORBIDDEN, PRESENCE_REQUIRED.into())
            }
            ChatError::Unauthorized(m) => Self(StatusCode::FORBIDDEN, format!("unauthorized: {m}")),
            ChatError::NotFound(m) => Self(StatusCode::NOT_FOUND, m),
            other => Self(StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
        }
    }
}

impl ApiError {
    /// 409 `waiting_for_founder`: writes wait for the founder's signed
    /// founding (a state the phone shows, not a failure).
    pub(crate) fn waiting_for_founder(founder_name: Option<String>) -> Self {
        Self(StatusCode::CONFLICT, format!("{WAITING_FOR_FOUNDER}\u{0}{}", founder_name.unwrap_or_default()))
    }
}

/// The error code of [`ApiError::waiting_for_founder`].
pub(super) const WAITING_FOR_FOUNDER: &str = "waiting_for_founder";

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if self.0 == StatusCode::FORBIDDEN && self.1 == PRESENCE_REQUIRED {
            return (self.0, Json(json!({ "error": PRESENCE_REQUIRED, "hint": PRESENCE_HINT }))).into_response();
        }
        if let Some(name) = self.1.strip_prefix(WAITING_FOR_FOUNDER).and_then(|r| r.strip_prefix('\u{0}')) {
            let mut v = json!({ "error": WAITING_FOR_FOUNDER, "message": hive_runtime::chat_service::WAITING_FOR_FOUNDER });
            if !name.is_empty() {
                v["founderName"] = json!(name);
            }
            return (self.0, Json(v)).into_response();
        }
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

/// P0.6: the 403 a human-only action gets from a host that can't tell a
/// human approved it.
pub(super) const PRESENCE_REQUIRED: &str = "presence_required";
const PRESENCE_HINT: &str = "confirm with your passkey: POST /v1/presence/prepare, then /v1/presence/commit";

/// P0.6 interim: while this box acts for the owner (`act_as_owner_device`),
/// anything it signs is signed as the owner, so a phone's or a bearer
/// caller's vote would pass for the owner's own. Until passkey presence
/// lands, such a host refuses votes (approve, reject), lockdown clears,
/// agent host rebinds and applies from the API, except from a phone the owner marked as theirs
/// (`owner_phone`), whose action is recorded as delegated. Every decision is
/// one content-free fact naming the phone (`pid`) or `bearer`.
pub(super) fn presence_gate(s: &Shared, peer: Option<&TunnelPeer>, action: &str) -> ApiResult<()> {
    if !s.lock().is_delegated() {
        return Ok(());
    }
    presence_gate_always(s, peer, action)
}

/// The decision of [`presence_gate`] for a host where presence applies:
/// accept a request from a phone the owner marked as theirs (recorded as
/// delegated), refuse anything else with `presence_required`.
pub(super) fn presence_gate_always(s: &Shared, peer: Option<&TunnelPeer>, action: &str) -> ApiResult<()> {
    let owner_phone = peer
        .and_then(|p| s.tunnel.as_ref().and_then(|t| t.registry.get(&p.pid)))
        .is_some_and(|r| r.is_active() && r.owner_phone);
    #[cfg(test)]
    let owner_phone = owner_phone || s.test_presence.load(std::sync::atomic::Ordering::SeqCst);
    let source = peer.map(|p| p.pid.clone()).unwrap_or_else(|| "bearer".into());
    let (verdict, reason) = if owner_phone {
        (hive_core::signal::GuardVerdict::Accept, format!("delegated_{action}"))
    } else {
        (hive_core::signal::GuardVerdict::Reject, format!("presence_required_{action}"))
    };
    let _ = s.lock().record_signal(
        s.ws,
        None,
        &hive_core::signal::GuardSignal::GuardDecision {
            gate: hive_core::signal::GuardGate::Presence,
            verdict,
            subject: hive_core::signal::GuardSubject::Nestmate,
            source,
            reason,
        },
    );
    if owner_phone {
        Ok(())
    } else {
        Err(ApiError(StatusCode::FORBIDDEN, PRESENCE_REQUIRED.into()))
    }
}

pub(super) type ApiResult<T> = Result<T, ApiError>;

// ── DTOs ───────────────────────────────────────────────────────────────────

/// RFC 3339 / ISO-8601 in UTC (`…Z`).
pub fn iso(ts: Timestamp) -> String {
    ts.inner().to_offset(time::UtcOffset::UTC).format(&Rfc3339).unwrap_or_default()
}

fn role_str(m: &ChatMessage) -> &'static str {
    match m.role {
        MessageRole::User => "user",
        MessageRole::System => "system",
        MessageRole::Agent => "agent",
        // A named agent's reply carries its stable id; the chat's primary
        // runtime ("hive") has none.
        MessageRole::Assistant if m.responder_id.is_some() => "agent",
        MessageRole::Assistant => "assistant",
    }
}

/// Who is looking: this box's device and the account it writes as (its
/// owner's once enrolled as the owner's device). Places a primary @hive turn
/// the way the desktop's `Viewer::place_of` does.
#[derive(Clone, Copy, Default, Debug)]
pub struct Viewer {
    pub device: Option<Uuid>,
    pub account: Option<Uuid>,
}

impl Viewer {
    pub fn of(svc: &hive_runtime::ChatService) -> Self {
        Self { device: Some(svc.device_id()), account: svc.author().account_id }
    }

    /// "this" (the box ran it), "mine" (another device of the same account)
    /// or none, for turn `m` answered by `actor`. The device that ran the turn
    /// is the message's `origin_device_id`; it only counts when the account
    /// matches too (the device id is the writer's claim).
    pub fn place_of(&self, m: &ChatMessage, actor: &hive_core::ActorIdentity) -> Option<&'static str> {
        let mine = actor.account_id.is_some() && actor.account_id == self.account;
        let ran_on = m.origin_device_id.or(actor.device_id);
        if mine && ran_on.is_some() && ran_on == self.device {
            return Some("this");
        }
        mine.then_some("mine")
    }
}

pub fn message_dto(chat: Uuid, m: &ChatMessage, viewer: Viewer) -> Value {
    let mut v = json!({
        "id": m.id,
        "chatId": chat,
        "author": hive_proto::names::clean_display_name(&m.author),
        "role": role_str(m),
        "body": m.body,
        "createdAt": iso(m.created_at),
        "streaming": m.is_streaming,
    });
    if let Some(a) = m.responder_id {
        v["agentId"] = json!(a);
    }
    // Who wrote a person's message (matches `GET /v1/people` `actorId`), so the
    // phone can draw their avatar.
    if let Some(actor) = m.actor_identity.as_ref().filter(|_| m.role == MessageRole::User) {
        v["actorId"] = json!(actor.id);
    }
    // A primary @hive turn (an assistant reply by no roster agent): whose hive
    // answered (`responderName`) and, when it was this box or another of the
    // same person's devices, `responderDevice` "this" / "mine" (absent
    // otherwise: the phone says "<name>'s device").
    if m.role == MessageRole::Assistant && m.responder_id.is_none() {
        if let Some(actor) = m.actor_identity.as_ref() {
            let name = hive_proto::names::clean_display_name(&actor.display_name);
            if !name.trim().is_empty() {
                v["responderName"] = json!(name);
                if let Some(place) = viewer.place_of(m, actor) {
                    v["responderDevice"] = json!(place);
                }
            }
        }
    }
    v
}

/// An agent reply that writes `@you`: who it addresses — `requester`
/// `{ name, actorId, isYou }`, the person whose message started the turn —
/// so the phone shows `@you` as `@<name>` and highlights it only for them.
/// Rendering only: `body` keeps `@you`. Absent otherwise.
pub fn add_requester(v: &mut Value, chat: &ChatSession, m: &ChatMessage, me: &super::notify::Me) {
    if !hive_runtime::parse_mentions(&m.body, chat).you {
        return;
    }
    let Some(r) = hive_runtime::requester_of(chat, m) else { return };
    let name = hive_proto::names::clean_display_name(&r.display_name);
    if name.is_empty() {
        return;
    }
    let is_you = me.is(&r.actor_id) || r.member_id.as_deref().is_some_and(|id| me.is(id));
    v["requester"] = json!({ "name": name, "actorId": r.actor_id, "isYou": is_you });
}

/// [`message_dto`], with a message by a person's other identity — the separate
/// member this box used before it was enrolled as its owner's device — shown as
/// theirs: `author` "Alex via Phone", `actorId` the owner's (for the avatar),
/// `via` the old name.
pub fn linked_message_dto(chat: &ChatSession, m: &ChatMessage, links: &hive_core::DeviceLinks, viewer: Viewer) -> Value {
    let mut v = message_dto(chat.id, m, viewer);
    if m.role != MessageRole::User {
        return v;
    }
    let Some(owner) = m.actor_identity.as_ref().and_then(|a| links.owner_of(&a.id)) else { return v };
    let Some(name) = chat.members.iter().find(|x| x.actor.id == owner).map(|x| x.actor.display_name.clone()) else {
        return v;
    };
    v["author"] = json!(format!("{} via {}", hive_proto::names::clean_display_name(&name), hive_proto::names::clean_display_name(&m.author)));
    v["via"] = json!(hive_proto::names::clean_display_name(&m.author));
    v["actorId"] = json!(owner);
    v
}

fn truncate(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>())
    }
}

/// When a chat last changed: its own stamp or its newest message.
fn updated_at(s: &ChatSession) -> Timestamp {
    s.messages.iter().map(|m| m.created_at).chain([s.updated_at, s.created_at]).max().unwrap_or(s.updated_at)
}

pub fn chat_dto(s: &ChatSession, unread: usize) -> Value {
    let preview = s
        .messages
        .iter()
        .rev()
        .find(|m| m.role != MessageRole::System && !m.body.trim().is_empty())
        .map(|m| truncate(&m.body.replace('\n', " "), 140))
        .unwrap_or_default();
    json!({
        "id": s.id,
        "title": if s.title.trim().is_empty() { "Untitled" } else { s.title.as_str() },
        "updatedAt": iso(updated_at(s)),
        "preview": preview,
        "unread": unread,
        // Channels (`GET /v1/channels`): "" = unfiled.
        "channelId": s.channel_id,
        "participants": super::people::participants(s, 5),
    })
}

pub(super) fn kind_str(k: ProposalKind) -> &'static str {
    match k {
        ProposalKind::FileDiff => "fileDiff",
        ProposalKind::Command => "command",
        ProposalKind::Decision => "decision",
    }
}

pub(super) fn status_str(s: ProposalStatus) -> &'static str {
    match s {
        ProposalStatus::Open => "open",
        ProposalStatus::Approved => "approved",
        ProposalStatus::Rejected => "rejected",
        ProposalStatus::Applied => "applied",
    }
}

/// Who a proposal is from: its author's roster name, else the agent whose
/// turn produced it, else empty.
pub(super) fn proposal_author(s: &ChatSession, p: &ActionProposal) -> String {
    if !p.author_actor_id.is_empty() {
        if let Some(m) = s.members.iter().find(|m| m.id == p.author_actor_id) {
            return hive_proto::names::clean_display_name(&m.actor.display_name);
        }
        return p.author_actor_id.clone();
    }
    p.producing_agent_id
        .and_then(|id| s.workspace_agents.iter().find(|a| a.id == id))
        .map(|a| format!("@{}", a.name))
        .unwrap_or_default()
}

pub fn proposal_dto(s: &ChatSession, p: &ActionProposal) -> Value {
    let mut v = json!({
        "id": p.id,
        "chatId": s.id,
        "title": p.title,
        "body": p.body,
        "kind": kind_str(p.kind),
        "status": status_str(p.status),
        "createdAt": iso(p.created_at),
        "author": proposal_author(s, p),
    });
    if let Some(d) = &p.diff {
        v["diff"] = json!(d);
    }
    // An alternative in a proposal set (the desktop decides those through the
    // set's pre-flight; the phone shows them grouped).
    if let Some(set_id) = p.set_id {
        v["setId"] = json!(set_id);
    }
    if let Some(class) = &p.task_class {
        v["taskClass"] = json!(class);
    }
    collab_fields(&mut v, s, p);
    review_fields(&mut v, s, p);
    v
}

/// The review badges (Track D4), as on the desktop: author kind, config
/// impact, the human gate and presence, a structured command's argv, and the
/// exact Implement plan (the same function the runner uses).
fn review_fields(v: &mut Value, s: &ChatSession, p: &ActionProposal) {
    let (kind, name) = hive_runtime::review::author_of(s, p);
    v["authorKind"] = json!(kind);
    v["authorName"] = json!(hive_proto::names::clean_display_name(&name));
    let protected = hive_core::sandbox::envelope_for_session(s).protected_dirs;
    v["configImpact"] = json!(hive_runtime::review::config_impact(p, &protected));
    v["humanGate"] = json!(hive_core::guard::human_gate_reason(s, p).is_some());
    v["presenceRequired"] = json!(s.authz.rules >= 2);
    if let Some(c) = &p.command {
        v["argv"] = json!(c.argv);
    }
    v["implementPlan"] = json!(hive_runtime::review::implement_plan(s, p, None));
}

/// An agent's open question, for the phone.
pub fn ask_dto(chat: Uuid, a: &hive_core::flow::HumanAsk) -> Value {
    json!({
        "id": a.ask_id,
        "chatId": chat,
        "turnId": a.turn_id,
        "agentKey": a.agent_key,
        "question": a.question,
        "options": a.options,
        "expiresAtMs": a.expires_at_ms,
        "closed": a.closed,
        // `permission`: an ACP permission escalation (only the host's person
        // or the agent's owner can grant it); `question`: an agent's question.
        "kind": if a.permission { "permission" } else { "question" },
    })
}

fn ask_state_dto(s: &ChatSession, st: &hive_core::flow::AskState) -> Value {
    let mut v = ask_dto(s.id, &st.ask);
    v["agentName"] = json!(hive_core::flow::agent_from_key(&st.ask.agent_key)
        .and_then(|id| s.workspace_agents.iter().find(|a| a.id == id))
        .map(|a| format!("@{}", a.name))
        .unwrap_or_else(|| "@hive".into()));
    v["open"] = json!(st.is_open());
    if let Some(a) = &st.answer {
        v["answer"] = json!(a.answer);
        v["answeredBy"] = json!(a.by);
    }
    v
}

pub(super) fn role_name(r: WorkspaceRole) -> &'static str {
    match r {
        WorkspaceRole::Owner => "owner",
        WorkspaceRole::Admin => "admin",
        WorkspaceRole::Contributor => "contributor",
        WorkspaceRole::Viewer => "viewer",
    }
}

/// A voter's or commenter's name, as the desktop's Review pane tags them: the
/// roster name, else a workspace agent's `@name`, else a short id.
pub(super) fn actor_name(s: &ChatSession, actor_id: &str) -> String {
    if let Some(m) = s.members.iter().find(|m| m.id == actor_id) {
        if !hive_proto::names::clean_display_name(&m.actor.display_name).is_empty() {
            return hive_proto::names::clean_display_name(&m.actor.display_name);
        }
    }
    if let Some(a) = s.workspace_agents.iter().find(|a| a.id.to_string() == actor_id) {
        return format!("@{}", a.name);
    }
    let short = actor_id.get(..8).unwrap_or(actor_id);
    if short.is_empty() { "someone".into() } else { short.into() }
}

pub fn comment_dto(s: &ChatSession, r: &ProposalRefinement) -> Value {
    json!({
        "id": r.id,
        "actorId": r.actor_id,
        "author": actor_name(s, &r.actor_id),
        "role": role_name(r.role),
        "text": r.text,
        "requestChanges": r.request_changes,
        "createdAt": iso(r.created_at),
    })
}

/// The collaboration state the desktop's Review card shows: quorum, who voted
/// (name + role at the time of the vote), the refinement thread, and the
/// revision chain. Additive to the base proposal JSON.
fn collab_fields(v: &mut Value, s: &ChatSession, p: &ActionProposal) {
    v["requiredApprovals"] = json!(p.required_approvals);
    v["qualifyingApprovals"] = json!(p.qualifying_approvals());
    v["quorumMet"] = json!(p.is_quorum_met());
    v["approvalRoleFloor"] = json!(role_name(p.approval_role_floor));
    v["votes"] = json!(p
        .approvals
        .iter()
        .map(|a| json!({
            "actorId": a.actor_id,
            "voter": actor_name(s, &a.actor_id),
            "role": role_name(a.role),
            "approved": a.approved,
            "createdAt": iso(a.created_at),
        }))
        .collect::<Vec<_>>());
    v["comments"] = json!(p.refinements.iter().map(|r| comment_dto(s, r)).collect::<Vec<_>>());
    v["commentCount"] = json!(p.refinements.len());
    v["changeRequestCount"] = json!(p.refinements.iter().filter(|r| r.request_changes).count());
    v["changedFiles"] = json!(p.changed_files);
    v["dismissed"] = json!(p.dismissed);
    if let Some(id) = p.parent_id {
        v["parentId"] = json!(id);
    }
    if let Some(id) = p.superseded_by {
        v["supersededBy"] = json!(id);
    }
}

/// [`proposal_dto`] plus what this box's identity — the one the phone acts as
/// — can do: its current vote and whether its role may vote or comment at all
/// (the same content floor `authorize` applies to `ProposalVoteCast` /
/// `ProposalRefined`, so a viewer sees disabled controls instead of a 403).
pub fn proposal_dto_as(s: &ChatSession, p: &ActionProposal, me: &str) -> Value {
    let mut v = proposal_dto(s, p);
    let role = s.role_of(me);
    v["myRole"] = json!(role_name(role));
    v["myVote"] = match p.approvals.iter().find(|a| a.actor_id == me) {
        Some(a) if a.approved => json!("approve"),
        Some(_) => json!("reject"),
        None => Value::Null,
    };
    let floor = hive_core::authorization::min_role_for(&hive_core::SessionEvent::ProposalVoteCast {
        proposal_id: p.id,
        approval: hive_core::proposals::ProposalApproval {
            actor_id: me.to_string(),
            role,
            approved: true,
            created_at: Timestamp::now(), diff_digest: None },
    });
    v["canVote"] = json!(role.rank() >= floor.rank());
    v["isMine"] = json!(!p.author_actor_id.is_empty() && p.author_actor_id == me);
    v
}

// ── Router + auth ──────────────────────────────────────────────────────────

/// Whether the request carried the right bearer token (set by [`auth`]).
#[derive(Clone, Copy)]
struct Authorized(bool);

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let v = headers.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

async fn auth(State(s): S, mut req: Request, next: Next) -> Response {
    // Through the relay tunnel (§9): the phone proved its Noise static; re-check
    // on every request that it is still paired, so a revocation bites at once.
    if let Some(peer) = req.extensions().get::<TunnelPeer>() {
        if !s.tunnel.as_ref().is_some_and(|t| t.registry.is_active(&peer.pid)) {
            return ApiError(StatusCode::UNAUTHORIZED, "this phone's pairing was revoked".into()).into_response();
        }
        req.extensions_mut().insert(Authorized(true));
        return next.run(req).await;
    }
    let ok = bearer(req.headers()).is_some_and(|t| pair::token_matches(&s.token(), t));
    if !ok && req.uri().path() != "/v1/health" {
        return ApiError(StatusCode::UNAUTHORIZED, "missing or invalid bearer token".into()).into_response();
    }
    req.extensions_mut().insert(Authorized(ok));
    next.run(req).await
}

pub fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/presence/prepare", post(super::presence_api::prepare))
        .route("/v1/presence/commit", post(super::presence_api::commit))
        .route("/v1/admissions/prepare", post(super::admissions_api::prepare))
        .route("/v1/admissions/presence", post(super::admissions_api::presence))
        .route("/v1/admissions/commit", post(super::admissions_api::commit))
        .route("/v1/chats", get(list_chats).post(create_chat))
        .route("/v1/chats/{id}/messages", get(list_messages).post(post_message))
        .route("/v1/chats/{id}/typing", get(super::typing_api::get_typing).post(super::typing_api::post_typing))
        .route("/v1/agents", get(list_agents))
        .route("/v1/agents/{id}/host", put(set_agent_host))
        .route("/v1/hosts", get(list_hosts))
        .route("/v1/guards", get(workspace_guards))
        .route("/v1/chats/{id}/lockdown/clear", post(clear_lockdown))
        .route("/v1/proposal-sets", get(super::sets::list_sets))
        .route("/v1/proposal-sets/{id}", get(super::sets::get_set))
        .route("/v1/chats/{id}/proposal-sets", get(super::sets::chat_sets))
        .route("/v1/flow", get(super::flow_api::flow_view))
        .route("/v1/chats/{id}/handoffs", get(super::flow_api::chat_handoffs))
        .route("/v1/people", get(super::people::list_people))
        .route("/v1/people/{account}/links", get(super::identity_api::person_links))
        .route("/v1/profile", get(super::identity_api::profile))
        .route("/v1/invites", post(super::identity_api::create_invite))
        .route("/v1/invites/inspect", post(super::identity_api::inspect))
        .route("/v1/invites/join", post(super::identity_api::join))
        .route("/v1/invites/pending", get(super::identity_api::pending))
        .route("/v1/invites/pending/accept", post(super::identity_api::accept_pending))
        .route("/v1/invites/pending/dismiss", post(super::identity_api::dismiss_pending))
        .route("/v1/dms", get(super::identity_api::list_dms).post(super::identity_api::open_dm))
        .route("/v1/dms/{id}/invite", get(super::identity_api::held_invite).delete(super::identity_api::dismiss_held_invite))
        .route("/v1/dms/{id}/invite/confirm", post(super::identity_api::confirm_held_invite))
        .route("/v1/chats/{id}/founder", get(super::identity_api::chat_founder))
        .route("/v1/channels", get(super::people::list_channels))
        .route("/v1/chats/{id}/details", get(super::people::chat_details))
        .route("/v1/repos", get(super::repos_api::list_repos))
        .route("/v1/repos/fork", get(super::repos_api::fork_of))
        .route("/v1/repos/clone", post(super::repos_api::clone_repo))
        .route("/v1/repos/clone/{job}", get(super::repos_api::clone_job))
        .route("/v1/chats/{id}/repo", get(super::repos_api::get_chat_repo).put(super::repos_api::put_chat_repo))
        .route("/v1/chats/{id}/repo/pull", post(super::repos_api::pull_chat_repo))
        .route("/v1/chats/{id}/pr", post(super::repos_api::open_pr))
        .route("/v1/forges", get(super::forges_api::list).post(super::forges_api::add))
        .route("/v1/forges/{id}", delete(super::forges_api::remove))
        .route("/v1/forges/{id}/test", post(super::forges_api::test))
        .route("/v1/forges/{id}/token", put(super::forges_api::put_token).delete(super::forges_api::delete_token))
        .route("/v1/forges/{id}/auth/device", post(super::forges_api::device_start).get(super::forges_api::device_status))
        .route("/v1/proposals", get(list_proposals))
        .route("/v1/proposals/{id}/approve", post(approve_proposal))
        .route("/v1/proposals/{id}/reject", post(reject_proposal))
        .route("/v1/proposals/{id}", get(get_proposal))
        .route("/v1/proposals/{id}/file", get(super::proposal_file::proposal_file))
        .route("/v1/proposals/{id}/comments", get(list_comments).post(post_comment))
        .route("/v1/proposals/{id}/request-changes", post(request_changes))
        .route("/v1/proposals/{id}/implement-plan", get(implement_plan))
        .route("/v1/chats/{id}/asks", get(list_asks))
        .route("/v1/asks/{id}/answer", post(answer_ask))
        .route("/v1/dashboard", get(dashboard_view))
        .route("/v1/lab/status", get(lab_status))
        .route("/v1/appearance", get(appearance))
        .route("/v1/events", get(events))
        .route("/v1/tunnel/credential", post(tunnel_credential))
        .route("/v1/tunnel/push", put(tunnel_push))
        .layer(middleware::from_fn_with_state(shared.clone(), auth))
        .with_state(shared)
}

/// Parse an optional JSON body (empty = `{}`).
fn optional_json<T: for<'de> Deserialize<'de> + Default>(body: &Bytes) -> ApiResult<T> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))
}

fn parse_id(raw: &str, what: &str) -> ApiResult<Uuid> {
    Uuid::parse_str(raw.trim()).map_err(|_| ApiError::not_found(what))
}

// ── Handlers ───────────────────────────────────────────────────────────────

async fn health(State(s): S, Extension(Authorized(ok)): Extension<Authorized>) -> Json<Value> {
    let mut v = json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") });
    if ok {
        v["workspace"] = json!(s.cfg.room);
        v["deviceLabel"] = json!(s.opts.label);
        v["identity"] = s.identity_json();
    }
    Json(v)
}

async fn list_chats(State(s): S) -> ApiResult<Json<Value>> {
    let me = s.my_actor_id();
    let mut chats = s.phone_chats()?;
    chats.sort_by_key(|c| std::cmp::Reverse(updated_at(c)));
    let ctx = PhoneChatCtx::new(&s);
    let list: Vec<Value> = chats.iter().map(|c| ctx.dto(&s, c, s.unread(c, &me))).collect();
    Ok(Json(json!({ "chats": list })))
}

/// What the phone's chat rows add to [`chat_dto`]: for a DM's chat, who it
/// is with (`dm`), and for any chat whether sending waits for the
/// workspace's founder (`waitingForFounder`, `founderName`).
pub struct PhoneChatCtx {
    dms: Vec<Value>,
    people: Vec<hive_runtime::directory_trust::CoMember>,
}

impl PhoneChatCtx {
    pub fn new(s: &Shared) -> Self {
        let people = s.co_members();
        let dms = if s.dms.list().is_empty() { Vec::new() } else { s.dm_rows(&people) };
        Self { dms, people }
    }

    pub fn dto(&self, s: &Shared, c: &ChatSession, unread: usize) -> Value {
        let mut v = chat_dto(c, unread);
        let waiting = s.lock().waiting_for_founder(c.workspace_id).ok().flatten();
        if let Some(f) = waiting {
            v["waitingForFounder"] = json!(true);
            if let Some(p) = self.people.iter().find(|p| p.account_id == f) {
                v["founderName"] = json!(p.display_name);
            }
        }
        if c.workspace_id != s.ws {
            if let Some(dm) = self.dms.iter().find(|d| d["workspaceId"] == json!(c.workspace_id)) {
                let mut d = json!({
                    "workspaceId": dm["workspaceId"],
                    "account": dm["account"],
                    "name": dm["name"],
                    "archived": dm["archived"],
                });
                if let Some(code) = dm.get("safetyCode") {
                    d["safetyCode"] = code.clone();
                }
                v["dm"] = d;
                // A DM's chat is named for the other person.
                v["title"] = dm["name"].clone();
                v["channelId"] = json!("");
            }
        }
        v
    }
}

#[derive(Deserialize, Default)]
struct NewChat {
    #[serde(default)]
    title: Option<String>,
}

async fn create_chat(State(s): S, body: Bytes) -> ApiResult<Json<Value>> {
    let req: NewChat = optional_json(&body)?;
    let title = req.title.map(|t| t.trim().to_string()).unwrap_or_default();
    let chat = {
        let mut svc = s.lock();
        let session = svc.create_chat(title, s.ws, "")?;
        // File it under the workspace's first channel (#general), as the
        // desktop files a new chat; a channel-less workspace leaves it unfiled.
        if let Some(ch) = svc.list_channels(s.ws)?.into_iter().find(|c| !c.archived) {
            svc.set_chat_channel(session.id, s.ws, ch.id)?;
        }
        svc.load(session.id)?.unwrap_or(session)
    };
    s.after_write();
    Ok(Json(json!({ "chat": chat_dto(&chat, 0) })))
}

pub(super) fn load_chat(s: &Shared, raw: &str) -> ApiResult<ChatSession> {
    let id = parse_id(raw, "chat")?;
    s.workspace_chats()?.into_iter().find(|c| c.id == id).ok_or_else(|| ApiError::not_found("chat"))
}

/// A chat the phone may read and post in: the workspace's, or a DM's. Only
/// the message routes (and the founder status) take a DM's chat; everything
/// else (agents, proposals, repos, guards) is the workspace's alone.
pub(super) fn load_phone_chat(s: &Shared, raw: &str) -> ApiResult<ChatSession> {
    let id = parse_id(raw, "chat")?;
    if let Some(c) = s.workspace_chats()?.into_iter().find(|c| c.id == id) {
        return Ok(c);
    }
    s.dm_chats().into_iter().find(|c| c.id == id).ok_or_else(|| ApiError::not_found("chat"))
}

#[derive(Deserialize)]
struct Page {
    after: Option<String>,
    limit: Option<usize>,
}

async fn list_messages(State(s): S, Path(id): Path<String>, Query(page): Query<Page>) -> ApiResult<Json<Value>> {
    let chat = load_phone_chat(&s, &id)?;
    let limit = page.limit.unwrap_or(100).clamp(1, 1000);
    let msgs = &chat.messages;
    let slice = match page.after.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
        Some(after) => {
            let after = parse_id(after, "message")?;
            let i = msgs.iter().position(|m| m.id == after).ok_or_else(|| ApiError::not_found("message"))?;
            &msgs[i + 1..(i + 1 + limit).min(msgs.len())]
        }
        None => &msgs[msgs.len().saturating_sub(limit)..],
    };
    if let Some(last) = slice.last() {
        s.mark_read(&chat, last.id);
    }
    let (links, viewer, author) = {
        let svc = s.lock();
        (svc.device_links(s.ws).unwrap_or_default(), Viewer::of(&svc), svc.author().clone())
    };
    let me = super::notify::Me::in_chat(&author, &chat, &links);
    let list: Vec<Value> = slice
        .iter()
        .map(|m| {
            let mut v = linked_message_dto(&chat, m, &links, viewer);
            add_requester(&mut v, &chat, m, &me);
            v
        })
        .collect();
    Ok(Json(json!({ "messages": list })))
}

#[derive(Deserialize)]
struct NewMessage {
    body: String,
}

/// Whether `body` addresses anyone with an `@mention` (an agent of the chat,
/// or `@hive` / a member — anything the desktop would route).
fn has_mention(body: &str, chat: &ChatSession) -> bool {
    let t = hive_runtime::parse_mentions(body, chat);
    !t.is_empty()
}

async fn post_message(State(s): S, Path(id): Path<String>, Json(req): Json<NewMessage>) -> ApiResult<Json<Value>> {
    let chat = load_phone_chat(&s, &id)?;
    let mut body = req.body.trim().to_string();
    if body.is_empty() {
        return Err(ApiError::bad_request("message body is empty"));
    }
    let in_dm = chat.workspace_id != s.ws;
    if in_dm && s.dms.is_archived(chat.workspace_id) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "You both opened a conversation at the same time; this one was archived. Write in the one that was kept.",
        ));
    }
    // Opt-in convenience: route an un-addressed phone message to a default
    // agent (never in a DM: agents don't run there).
    if let Some(agent) = s.opts.default_agent.as_ref().filter(|_| !in_dm) {
        if !has_mention(&body, &chat) && chat.workspace_agents.iter().any(|a| a.name.eq_ignore_ascii_case(agent)) {
            body = format!("@{agent} {body}");
        }
    }
    let posted = {
        let mut svc = s.lock();
        svc.post_user_message(chat.id, chat.workspace_id, body)
    };
    let message = match posted {
        Ok(m) => m,
        // Not a failure: the founder's signed founding hasn't synced yet.
        // The phone keeps the draft and shows the waiting state.
        Err(hive_runtime::chat_service::ChatError::Unauthorized(m)) if m.starts_with("waiting for the workspace's founder") => {
            let (_, name) = super::identity_api::founder_wait(&s, chat.workspace_id);
            return Err(ApiError::waiting_for_founder(name));
        }
        Err(e) => return Err(e.into()),
    };
    s.mark_read_after_post(chat.id, message.id);
    s.after_write();
    Ok(Json(json!({ "message": message_dto(chat.id, &message, Viewer::of(&s.lock())) })))
}

impl Shared {
    /// The phone has seen its own message.
    fn mark_read_after_post(&self, chat: Uuid, message: Uuid) {
        let loaded = self.lock().load(chat);
        if let Ok(Some(s)) = loaded {
            self.mark_read(&s, message);
        }
    }
}

/// The registered hosts and the workspace-config log. A workspace whose config
/// log hasn't synced in yet has no agents: an empty log stands in, so
/// `GET /v1/agents` answers `[]` rather than 404 (the phone's pairing check
/// calls it).
pub(super) fn hosts_and_agents(s: &Shared) -> ApiResult<(Vec<hive_core::WorkspaceHost>, ChatSession)> {
    let svc = s.lock();
    let hosts = svc.list_hosts(s.ws)?;
    let config_id = hive_core::workspace_config_session_id(s.ws);
    let config = svc.load(config_id)?.unwrap_or_else(|| {
        let mut empty = ChatSession::new("", s.ws, "");
        empty.id = config_id;
        empty
    });
    Ok((hosts, config))
}

pub(super) fn agent_dto(s: &Shared, a: &hive_core::WorkspaceAgent, config: &ChatSession, hosts: &[hive_core::WorkspaceHost]) -> Value {
    // What the agent runs on: a workspace runtime, or — when hosted here —
    // this box's own runtime for it (see `worker::local_runtimes`).
    let mut known = config.workspace_runtimes.clone();
    if a.host_id == s.host_id {
        known.extend(crate::worker::local_runtimes(&s.cfg.data_dir));
    }
    let rt = if a.host_id == s.host_id {
        crate::worker::runtime_for(&known, a)
    } else {
        known.iter().find(|r| r.id == a.runtime_id)
    };
    let mut v = json!({
        "id": a.id,
        "name": a.name,
        "runtimeLabel": rt.map(|r| r.name.clone()).unwrap_or_else(|| a.runtime_id.clone()),
    });
    // Who may grant the agent's permission requests (with this host's
    // person); the phone compares it with its own actor.
    if !a.owner_actor_id.trim().is_empty() {
        v["ownerActorId"] = json!(a.owner_actor_id);
    }
    if let Some(r) = rt.filter(|r| !r.model.trim().is_empty()) {
        v["model"] = json!(r.model);
    }
    if !a.host_id.is_empty() {
        v["hostId"] = json!(a.host_id);
        let label = if a.host_id == s.host_id {
            Some(format!("{} (this host)", s.opts.label))
        } else {
            hosts.iter().find(|h| h.id == a.host_id).map(|h| h.label.clone())
        };
        if let Some(l) = label {
            v["hostLabel"] = json!(l);
        }
    } else {
        // Empty host = the agent owner's own device (the desktop's default).
        v["hostLabel"] = json!("Owner's device");
    }
    super::people::decorate_agent(&mut v, s, a, hosts);
    v
}

async fn list_agents(State(s): S) -> ApiResult<Json<Value>> {
    let (hosts, config) = hosts_and_agents(&s)?;
    let list: Vec<Value> = config.workspace_agents.iter().map(|a| agent_dto(&s, a, &config, &hosts)).collect();
    Ok(Json(json!({ "agents": list })))
}

/// The workspace's guard state for a client to show: whether dearth mode is
/// on, why, and what it restricts; the locked-down chats and by what kind of
/// signal; the workspace coordinator lease; and whether this box's identity
/// may clear a lockdown (`POST /v1/chats/:id/lockdown/clear`). Content-free
/// beyond the reason an Admin typed and the chat titles.
async fn workspace_guards(State(s): S) -> ApiResult<Json<Value>> {
    let g = s.lock().workspace_guards(s.ws)?;
    let chats = s.workspace_chats()?;
    let me = s.my_actor_id();
    let dearth = g.dearth_on().map(|d| {
        json!({
            "reason": d.reason,
            "broodPaths": d.brood_paths,
            "allowedMcpServers": d.allowed_mcp_servers,
            "setBy": d.set_by,
            "setAt": d.set_at,
        })
    });
    let locked: Vec<Value> = g
        .lockdowns
        .iter()
        .filter(|(_, l)| !l.is_empty())
        .map(|(chat, l)| {
            let title = chats.iter().find(|c| c.id == *chat).map(|c| c.title.clone()).unwrap_or_default();
            json!({
                "chatId": chat,
                "chatTitle": title,
                "signals": l.iter().map(|x| {
                    let mut v = json!({
                        "signal": x.signal,
                        "detail": x.detail,
                        "raisedAt": iso(x.raised_at),
                    });
                    if let Some(m) = x.message_id {
                        v["messageId"] = json!(m);
                    }
                    v
                }).collect::<Vec<_>>(),
            })
        })
        .collect();
    // The workspace coordinator lease (held by a serve box), if any.
    let coordinator = g
        .leases
        .get(&hive_core::lease::lease_key(hive_core::lease::LeaseScope::Workspace, &s.ws.to_string()))
        .map(|l| json!({ "holder": l.current.holder, "epoch": l.current.epoch, "leaseMs": l.current.lease_ms, "released": l.released, "isThis": l.current.holder.to_string() == s.host_id }));
    let (role, human) = my_standing(&s, &me)?;
    Ok(Json(json!({
        "dearth": dearth,
        "pendingMembers": g.pending_members.len(),
        "lockedChats": locked,
        "coordinator": coordinator,
        "myRole": role_name(role),
        "canClearLockdown": human,
    })))
}

/// This box's identity's role on the workspace roster (the config log, which
/// every chat's roster is hoisted from) and whether it may clear a lockdown:
/// a human member at Contributor or above — the same rule the core's
/// authorization applies to `ChatLockdownCleared`.
fn my_standing(s: &Shared, me: &str) -> ApiResult<(WorkspaceRole, bool)> {
    let (_, config) = hosts_and_agents(s)?;
    let role = config.role_of(me);
    Ok((role, can_clear_lockdown(&config.members, me, role)))
}

/// The lockdown-clear rule: a human member at Contributor or above. A
/// Viewer, an agent or an unknown identity never may.
pub(super) fn can_clear_lockdown(members: &[hive_core::WorkspaceMember], me: &str, role: WorkspaceRole) -> bool {
    hive_core::guard::is_human_member(members, me) && role.rank() >= WorkspaceRole::Contributor.rank()
}

/// `POST /v1/chats/:id/lockdown/clear`: clear a chat's lockdown as this box's
/// identity, through the same signed `ChatLockdownCleared` event (on the
/// workspace-config log) the desktop's *Clear lockdown* writes. Only a human
/// member at Contributor or above may: anyone else gets 403 and nothing is
/// written (the core re-checks when the event is authored and on every peer
/// that folds it). A chat that isn't locked answers 409.
async fn clear_lockdown(State(s): S, peer: Option<axum::Extension<TunnelPeer>>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let chat = load_chat(&s, &id)?;
    presence_gate(&s, peer.as_deref(), "lockdown_clear")?;
    let me = s.my_actor_id();
    let (_, allowed) = my_standing(&s, &me)?;
    if !allowed {
        return Err(ApiError(StatusCode::FORBIDDEN, "only a human member (Contributor or above) can clear a lockdown".into()));
    }
    let locked = !s.lock().workspace_guards(s.ws)?.lockdowns_of(chat.id).is_empty();
    if !locked {
        return Err(ApiError(StatusCode::CONFLICT, "this chat isn't locked down".into()));
    }
    s.lock().clear_lockdown(s.ws, chat.id)?;
    s.after_write();
    Ok(Json(json!({ "chatId": chat.id, "cleared": true })))
}

async fn list_hosts(State(s): S) -> ApiResult<Json<Value>> {
    let (hosts, _) = hosts_and_agents(&s)?;
    let now = Timestamp::now();
    let list: Vec<Value> = hosts
        .iter()
        .map(|h| {
            let is_this = h.id == s.host_id;
            let mut v = json!({
                "id": h.id,
                "label": h.label,
                "isThis": is_this,
                "online": is_this || s.presence.is_online(h, &now),
            });
            // The model gateways the host offers (names and wire only),
            // left out when none.
            let gws: Vec<Value> = h.offered_gateways().map(|g| json!({ "name": g.name, "api": g.api })).collect();
            if !gws.is_empty() {
                v["modelGateways"] = Value::Array(gws);
            }
            v
        })
        .collect();
    Ok(Json(json!({ "hosts": list })))
}

#[derive(Deserialize)]
struct HostReq {
    #[serde(rename = "hostId")]
    host_id: String,
}

async fn set_agent_host(
    State(s): S,
    peer: Option<axum::Extension<TunnelPeer>>,
    Path(id): Path<String>,
    Json(req): Json<HostReq>,
) -> ApiResult<Json<Value>> {
    let agent_id = parse_id(&id, "agent")?;
    // S2: a host rebind is an Admin action; signed as the owner from here,
    // so it needs the same presence as a vote.
    presence_gate(&s, peer.as_deref(), "agent_host")?;
    let host_id = req.host_id.trim().to_string();
    let v = {
        let (hosts, config) = hosts_and_agents(&s)?;
        if !config.workspace_agents.iter().any(|a| a.id == agent_id) {
            return Err(ApiError::not_found("agent"));
        }
        // An empty id hands the agent back to its owner's device; anything
        // else must be a registered host.
        if !host_id.is_empty() && !hosts.iter().any(|h| h.id == host_id) {
            return Err(ApiError::bad_request("unknown host"));
        }
        let mut svc = s.lock();
        svc.set_agent_host(s.ws, agent_id, &host_id)?;
        let config = svc
            .load(hive_core::workspace_config_session_id(s.ws))?
            .ok_or_else(|| ApiError::not_found("workspace config"))?;
        drop(svc);
        let agent = config.workspace_agents.iter().find(|a| a.id == agent_id).ok_or_else(|| ApiError::not_found("agent"))?;
        agent_dto(&s, agent, &config, &hosts)
    };
    s.after_write();
    Ok(Json(json!({ "agent": v })))
}

#[derive(Deserialize)]
struct ProposalFilter {
    status: Option<String>,
}

async fn list_proposals(State(s): S, Query(f): Query<ProposalFilter>) -> ApiResult<Json<Value>> {
    let all = match f.status.as_deref().map(str::trim) {
        None | Some("") | Some("open") => false,
        Some("all") => true,
        Some(other) => return Err(ApiError::bad_request(format!("status must be open or all (got '{other}')"))),
    };
    let me = s.my_actor_id();
    let mut out: Vec<(Timestamp, Value)> = Vec::new();
    for chat in s.workspace_chats()? {
        for p in &chat.proposals {
            let open = p.status == ProposalStatus::Open && !p.dismissed && p.superseded_by.is_none();
            if all || open {
                out.push((p.created_at, proposal_dto_as(&chat, p, &me)));
            }
        }
    }
    out.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    Ok(Json(json!({ "proposals": out.into_iter().map(|(_, v)| v).collect::<Vec<_>>() })))
}

/// Find a proposal's chat.
pub(super) fn find_proposal(s: &Shared, raw: &str) -> ApiResult<(ChatSession, ActionProposal)> {
    let pid = parse_id(raw, "proposal")?;
    for chat in s.workspace_chats()? {
        if let Some(p) = chat.proposals.iter().find(|p| p.id == pid).cloned() {
            return Ok((chat, p));
        }
    }
    Err(ApiError::not_found("proposal"))
}

/// Cast this box's vote — the same `ProposalVoteCast` delta the desktop's
/// Review pane emits, at this identity's real roster role, so the proposal's
/// quorum and role floor apply unchanged.
fn vote(s: &Shared, chat: &ChatSession, pid: Uuid, approved: bool) -> ApiResult<ActionProposal> {
    let mut svc = s.lock();
    svc.vote_on_proposal(chat.id, chat.workspace_id, pid, approved)?
        .ok_or_else(|| ApiError::not_found("proposal"))
}

async fn approve_proposal(State(s): S, peer: Option<axum::Extension<TunnelPeer>>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let (chat, p) = find_proposal(&s, &id)?;
    presence_gate(&s, peer.as_deref(), "approve")?;
    let mut updated = vote(&s, &chat, p.id, true)?;
    // Only a proposal whose folded status is Approved: quorum alone stays met
    // after a qualifying reject, and Applied is terminal on every peer, so
    // applying on quorum would override a rejection for good. Dismissed and
    // superseded versions aren't open work either.
    // An alternative in an open proposal set is applied only through the
    // set's two-phase commit (pre-flight first), never on approval here.
    let in_open_set = chat.proposal_sets.iter().any(|set| set.is_open() && set.contains(updated.id));
    // A service member (every real box, Phase B) applies only to fulfil a
    // person's request, never on its own approval.
    let service = s.lock().author().kind == hive_core::ActorKind::Service;
    if s.opts.apply_on_approve && !service && updated.ready_to_apply() && !updated.dismissed && !in_open_set {
        if let Some(applied) = apply_on_this_host(&s, &chat, &updated)? {
            updated = applied;
        }
    }
    s.after_write();
    let chat = load_chat(&s, &chat.id.to_string()).unwrap_or(chat);
    Ok(Json(json!({ "proposal": proposal_dto_as(&chat, &updated, &s.my_actor_id()) })))
}

#[derive(Deserialize, Default)]
struct RejectReq {
    #[serde(default)]
    reason: Option<String>,
}

async fn reject_proposal(State(s): S, peer: Option<axum::Extension<TunnelPeer>>, Path(id): Path<String>, body: Bytes) -> ApiResult<Json<Value>> {
    let req: RejectReq = optional_json(&body)?;
    let (chat, p) = find_proposal(&s, &id)?;
    presence_gate(&s, peer.as_deref(), "reject")?;
    // A reason rides the proposal's refinement thread as a comment, the way a
    // reviewer's note does on the desktop.
    if let Some(reason) = req.reason.map(|r| r.trim().to_string()).filter(|r| !r.is_empty()) {
        let mut svc = s.lock();
        let actor = svc.author().id.clone();
        let role = chat.role_of(&actor);
        svc.refine_proposal(chat.id, chat.workspace_id, p.id, actor, role, reason, false)?;
    }
    let updated = vote(&s, &chat, p.id, false)?;
    s.after_write();
    let chat = load_chat(&s, &chat.id.to_string()).unwrap_or(chat);
    let updated = chat.proposals.iter().find(|q| q.id == updated.id).cloned().unwrap_or(updated);
    Ok(Json(json!({ "proposal": proposal_dto_as(&chat, &updated, &s.my_actor_id()) })))
}

// ── Proposal collaboration: thread + request changes (desktop parity) ─────

async fn get_proposal(State(s): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let (chat, p) = find_proposal(&s, &id)?;
    Ok(Json(json!({ "proposal": proposal_dto_as(&chat, &p, &s.my_actor_id()) })))
}

async fn list_comments(State(s): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let (chat, p) = find_proposal(&s, &id)?;
    Ok(Json(json!({ "comments": p.refinements.iter().map(|r| comment_dto(&chat, r)).collect::<Vec<_>>() })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewComment {
    text: String,
    #[serde(default)]
    request_changes: bool,
}

/// Append a note to the proposal's refinement thread as this box's identity at
/// its real roster role — the desktop's `refine_proposal`. The core's authz
/// gate (`ProposalRefined` needs a content-tier role) answers a viewer 403.
fn add_note(s: &Shared, chat: &ChatSession, pid: Uuid, text: &str, request_changes: bool) -> ApiResult<ActionProposal> {
    let mut svc = s.lock();
    let actor = svc.author().id.clone();
    let role = chat.role_of(&actor);
    svc.refine_proposal(chat.id, chat.workspace_id, pid, actor, role, text, request_changes)?
        .ok_or_else(|| ApiError::not_found("proposal"))
}

async fn post_comment(State(s): S, Path(id): Path<String>, Json(req): Json<NewComment>) -> ApiResult<Json<Value>> {
    let text = req.text.trim();
    if text.is_empty() {
        return Err(ApiError::bad_request("a refinement note can't be empty"));
    }
    let (chat, p) = find_proposal(&s, &id)?;
    let updated = add_note(&s, &chat, p.id, text, req.request_changes)?;
    s.after_write();
    let chat = load_chat(&s, &chat.id.to_string()).unwrap_or(chat);
    let updated = chat.proposals.iter().find(|q| q.id == updated.id).cloned().unwrap_or(updated);
    let comment = updated.refinements.last().map(|r| comment_dto(&chat, r)).unwrap_or(Value::Null);
    Ok(Json(json!({ "comment": comment, "proposal": proposal_dto_as(&chat, &updated, &s.my_actor_id()) })))
}

async fn implement_plan(State(s): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let (chat, p) = find_proposal(&s, &id)?;
    Ok(Json(json!({ "plan": hive_runtime::review::implement_plan(&chat, &p, None) })))
}

async fn list_asks(State(s): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let chat = load_chat(&s, &id)?;
    let mut asks: Vec<&hive_core::flow::AskState> = chat.flow.asks.values().collect();
    asks.sort_by_key(|a| std::cmp::Reverse(a.ask.at_ms));
    Ok(Json(json!({ "asks": asks.into_iter().take(50).map(|a| ask_state_dto(&chat, a)).collect::<Vec<_>>() })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AnswerReq {
    #[serde(default)]
    answer: String,
    #[serde(default)]
    option_index: Option<u32>,
}

/// `POST v1/asks/{id}/answer` (D.4.2): a person answers an agent's question
/// from the phone. Answering authorizes nothing by itself, so it needs no
/// presence; the first answer in canonical order wins.
async fn answer_ask(State(s): S, Path(id): Path<String>, Json(req): Json<AnswerReq>) -> ApiResult<Json<Value>> {
    let ask_id = parse_id(&id, "question")?;
    let chat = s
        .workspace_chats()?
        .into_iter()
        .find(|c| c.flow.asks.contains_key(&ask_id))
        .ok_or_else(|| ApiError::not_found("question"))?;
    let st = chat.flow.asks.get(&ask_id).cloned().ok_or_else(|| ApiError::not_found("question"))?;
    if !st.is_open() {
        return Err(ApiError::bad_request("that question is already answered or closed"));
    }
    let text = match req.option_index.and_then(|i| st.ask.options.get(i as usize)) {
        Some(o) if req.answer.trim().is_empty() => o.clone(),
        _ => req.answer.trim().to_string(),
    };
    if text.is_empty() {
        return Err(ApiError::bad_request("an answer is required"));
    }
    {
        let mut svc = s.lock();
        let by = svc.author().display_name.clone();
        let rec = hive_core::flow::FlowRecord::HumanAnswered(hive_core::flow::HumanAnswered {
            ask_id,
            answer: text.chars().take(hive_core::flow::MAX_ANSWER_CHARS).collect(),
            option_index: req.option_index,
            by,
            at_ms: hive_core::flow::now_ms(),
        });
        svc.record_flow(chat.id, chat.workspace_id, rec)?;
    }
    s.after_write();
    let chat = load_chat(&s, &chat.id.to_string())?;
    let st = chat.flow.asks.get(&ask_id).ok_or_else(|| ApiError::not_found("question"))?;
    Ok(Json(json!({ "ask": ask_state_dto(&chat, st) })))
}

/// A "Send to agent to revise" waiting for the agent's new version.
#[derive(Debug, Clone)]
pub struct PendingRevision {
    pub chat: Uuid,
    pub agent: Uuid,
    pub requested_at: Timestamp,
}

#[derive(Deserialize, Default)]
struct ChangesReq {
    #[serde(default)]
    note: Option<String>,
}

/// The desktop's `request_proposal_changes`: bundle the thread's change
/// requests (else all notes) into an instruction, record "Sent to the agent to
/// revise." on the thread, and post the instruction as this box's user message
/// addressed to the agent that produced the proposal — so whichever device
/// hosts that agent answers it, like any mention. The revision the turn files
/// is linked back (`parentId` / `supersededBy`) when it arrives
/// ([`Shared::link_revisions`]). An optional `note` is added first as a change
/// request, so the phone can do both in one tap.
async fn request_changes(State(s): S, Path(id): Path<String>, body: Bytes) -> ApiResult<Json<Value>> {
    let req: ChangesReq = optional_json(&body)?;
    let (chat, p) = find_proposal(&s, &id)?;
    if p.superseded_by.is_some() {
        return Err(ApiError(StatusCode::CONFLICT, "this proposal was already revised".into()));
    }
    if p.status == ProposalStatus::Applied {
        return Err(ApiError(StatusCode::CONFLICT, "this proposal was already implemented".into()));
    }
    // Who revises it: the agent whose turn produced it. The chat's primary
    // runtime (@hive) only runs on a desktop, so fall back to the configured
    // default agent, else refuse rather than post a message nobody answers.
    let agent = p
        .producing_agent_id
        .and_then(|aid| chat.workspace_agents.iter().find(|a| a.id == aid))
        .or_else(|| {
            let name = s.opts.default_agent.as_deref()?;
            chat.workspace_agents.iter().find(|a| a.name.eq_ignore_ascii_case(name))
        })
        .cloned()
        .ok_or_else(|| {
            ApiError(
                StatusCode::CONFLICT,
                "this proposal came from the chat's primary runtime — send it back from the desktop".into(),
            )
        })?;
    let mut current = p.clone();
    if let Some(note) = req.note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()) {
        current = add_note(&s, &chat, p.id, &note, true)?;
    }
    // Prefer explicit change requests; fall back to all notes so a plain
    // comment thread can still drive a revision (as the desktop does).
    let mut notes: Vec<&ProposalRefinement> = current.refinements.iter().filter(|r| r.request_changes).collect();
    if notes.is_empty() {
        notes = current.refinements.iter().collect();
    }
    if notes.is_empty() {
        return Err(ApiError::bad_request("add a refinement note before sending it back to the agent"));
    }
    let feedback = notes.iter().map(|r| format!("- {}", r.text.trim())).collect::<Vec<_>>().join("\n");
    let instruction = format!(
        "@{} Please revise your proposal \"{}\" based on this review feedback, then produce an updated proposal:\n\n{}",
        agent.name, p.title, feedback
    );
    let requested_at = Timestamp::now();
    add_note(&s, &chat, p.id, "Sent to the agent to revise.", true)?;
    let message = {
        let mut svc = s.lock();
        svc.post_user_message(chat.id, chat.workspace_id, instruction)?
    };
    s.revisions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(p.id, PendingRevision { chat: chat.id, agent: agent.id, requested_at });
    s.mark_read_after_post(chat.id, message.id);
    s.after_write();
    let chat = load_chat(&s, &chat.id.to_string()).unwrap_or(chat);
    let updated = chat.proposals.iter().find(|q| q.id == p.id).cloned().unwrap_or(current);
    Ok(Json(json!({
        "proposal": proposal_dto_as(&chat, &updated, &s.my_actor_id()),
        "message": message_dto(chat.id, &message, Viewer::of(&s.lock())),
    })))
}

impl Shared {
    /// Link each newly-seen proposal that answers a pending "Send to agent to
    /// revise" back to the version it revises, as the desktop does after its
    /// revision turn: the new one gets `parent_id`, the original
    /// `superseded_by`. Matches on the producing agent and the request time,
    /// so it holds whichever device ran the turn. Returns whether it wrote.
    pub(crate) fn link_revisions(&self, created: &[(Uuid, Uuid)]) -> bool {
        let mut pending = self.revisions.lock().unwrap_or_else(|e| e.into_inner());
        if pending.is_empty() || created.is_empty() {
            return false;
        }
        let mut wrote = false;
        let mut svc = self.lock();
        for (chat_id, new_id) in created {
            let Ok(Some(chat)) = svc.load(*chat_id) else { continue };
            let Some(child) = chat.proposals.iter().find(|p| p.id == *new_id) else { continue };
            let parent_id = pending
                .iter()
                .filter(|(pid, r)| {
                    r.chat == *chat_id
                        && **pid != child.id
                        && child.producing_agent_id == Some(r.agent)
                        && child.created_at.inner() >= r.requested_at.inner()
                })
                .max_by_key(|(_, r)| r.requested_at)
                .map(|(pid, _)| *pid);
            let Some(parent_id) = parent_id else { continue };
            pending.remove(&parent_id);
            let mut child = child.clone();
            child.parent_id = Some(parent_id);
            if let Err(e) = svc.upsert_proposal(chat.id, chat.workspace_id, child) {
                eprintln!("serve: linking revision failed: {e}");
                continue;
            }
            if let Some(mut parent) = chat.proposals.iter().find(|p| p.id == parent_id).cloned() {
                parent.superseded_by = Some(*new_id);
                if let Err(e) = svc.upsert_proposal(chat.id, chat.workspace_id, parent) {
                    eprintln!("serve: marking superseded failed: {e}");
                }
            }
            wrote = true;
        }
        wrote
    }
}

/// Opt-in (`LIGUS_SERVE_APPLY_ON_APPROVE=1`): carry out an approved file-diff
/// proposal on this box, as the desktop's Implement does — apply it into the
/// chat's base worktree of the bound checkout, commit (advancing
/// `hive/chat/<id>`), and mark it Applied. `Ok(None)` when there is nothing to
/// apply here (not a diff, or no checkout bound).
fn apply_on_this_host(s: &Shared, chat: &ChatSession, p: &ActionProposal) -> ApiResult<Option<ActionProposal>> {
    let (ProposalKind::FileDiff, Some(diff)) = (p.kind, p.diff.as_deref()) else {
        return Ok(None);
    };
    // Workspace guards (dearth-mode brood paths, a chat lockdown): never apply
    // automatically without a human member's own signed approval. Re-read the
    // chat so the vote just cast is folded in.
    if let Some(fresh) = s.lock().load(chat.id)? {
        if let Some(reason) = hive_core::guard::human_gate_reason(&fresh, p) {
            eprintln!("serve: not applying proposal {}: {reason}", p.id);
            return Ok(None);
        }
        // A held vote or decision (its config frontier hasn't synced): wait.
        if fresh.authz.blocks(p.id, p.set_id) {
            eprintln!("serve: not applying proposal {}: a vote on it is still waiting to sync", p.id);
            return Ok(None);
        }
    }
    let repos = crate::repos::repos_dir(&s.cfg.data_dir);
    let Some(repo) = crate::repos::RepoBindings::load(&s.cfg.data_dir).resolve(&repos, chat.id) else {
        return Ok(None);
    };
    if !hive_runtime::git_worktree::is_git_repo(&repo) {
        return Ok(None);
    }
    crate::repos::ensure_chat_base(&repo);
    let wt = hive_runtime::git_worktree::chat_worktree(&repo, &chat.id.to_string())
        .map_err(|e| ApiError(StatusCode::CONFLICT, format!("chat worktree unavailable: {e}")))?;
    git_apply(&wt, diff).map_err(|e| ApiError(StatusCode::CONFLICT, e))?;
    git_commit(&wt, &format!("Apply proposal: {}", p.title)).map_err(|e| ApiError(StatusCode::CONFLICT, e))?;
    Ok(s.lock().mark_proposal_applied(chat.id, chat.workspace_id, p.id, None)?)
}

fn git_apply(dir: &std::path::Path, diff: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = hive_core::process_util::git()
        .arg("-C")
        .arg(dir)
        .args(["apply", "--whitespace=nowarn"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git apply: {e}"))?;
    child
        .stdin
        .take()
        .ok_or("git apply: no stdin")?
        .write_all(diff.as_bytes())
        .map_err(|e| format!("write patch: {e}"))?;
    let out = child.wait_with_output().map_err(|e| format!("git apply: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "couldn't apply the change — the checkout may have moved since it was proposed ({})",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

fn git_commit(dir: &std::path::Path, message: &str) -> Result<(), String> {
    let run = |args: &[&str]| {
        hive_core::process_util::git().arg("-C").arg(dir).args(args).output().map_err(|e| e.to_string())
    };
    let add = run(&["add", "-A"])?;
    if !add.status.success() {
        return Err(format!("git add: {}", String::from_utf8_lossy(&add.stderr).trim()));
    }
    // A headless box may have no git identity configured; give the commit one.
    let out = run(&["-c", "user.name=Ligus", "-c", "user.email=ligus@localhost", "commit", "-m", message])?;
    if !out.status.success() {
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        if text.contains("nothing to commit") {
            return Ok(());
        }
        return Err(format!("git commit: {}", text.trim()));
    }
    Ok(())
}

/// The host's theme for a phone set to "Match host" (`appearance.rs`).
async fn appearance(State(s): S) -> Json<Value> {
    Json(super::appearance::resolve(&s.opts.appearance))
}

// ── Tunnel-only routes (§9): 404 unless the request came through the tunnel ──

fn tunnel_phone(s: &Shared, peer: Option<Extension<TunnelPeer>>) -> ApiResult<(Arc<super::tunnel::TunnelHost>, super::tunnel::PhoneRecord)> {
    let (Some(Extension(peer)), Some(t)) = (peer, s.tunnel.clone()) else { return Err(ApiError::not_found("route")) };
    let rec = t.registry.get(&peer.pid).filter(|r| r.is_active()).ok_or_else(|| ApiError::not_found("phone"))?;
    Ok((t, rec))
}

/// `POST /v1/tunnel/credential`: a fresh `hpc1` for the same pid and key (§13.1).
async fn tunnel_credential(State(s): S, peer: Option<Extension<TunnelPeer>>) -> ApiResult<Json<Value>> {
    let (t, rec) = tunnel_phone(&s, peer)?;
    let pk: [u8; 32] = super::tunnel::b64u_fixed(&rec.auth_pub)?;
    let (credential, exp) = super::tunnel::cred::issue(&t.keys, &rec.pid, &pk, super::tunnel::now());
    t.registry.set_cred_exp(&rec.pid, exp)?;
    Ok(Json(json!({ "credential": credential, "exp": exp })))
}

#[derive(Deserialize)]
struct PushPref {
    enabled: bool,
    /// Message previews in pushes (§14). Absent = keep the stored value
    /// (on by default).
    #[serde(default)]
    previews: Option<bool>,
}

/// `PUT /v1/tunnel/push {"enabled": bool, "previews"?: bool}` →
/// `{"enabled": bool, "previews": bool}`.
async fn tunnel_push(State(s): S, peer: Option<Extension<TunnelPeer>>, body: Bytes) -> ApiResult<Json<Value>> {
    let (t, rec) = tunnel_phone(&s, peer)?;
    let pref: PushPref = serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))?;
    let (enabled, previews) = t.registry.set_push(&rec.pid, pref.enabled, pref.previews)?.ok_or_else(|| ApiError::not_found("phone"))?;
    Ok(Json(json!({ "enabled": enabled, "previews": previews })))
}

/// Deprecated: kept for app builds older than `/v1/dashboard`.
async fn lab_status(State(s): S) -> Response {
    let body = Json(lab::status(&s.http, &s.opts.prometheus_url).await);
    ([("deprecation", "true"), ("link", "</v1/dashboard>; rel=\"successor-version\"")], body).into_response()
}

async fn dashboard_view(State(s): S) -> Json<Value> {
    let loaded = dashboard::load(&s.opts.dashboard_file, s.opts.prometheus_configured);
    Json(dashboard::render(&loaded, &s.dashboard_sources(), Some(&s.dashboard_cache)).await)
}

fn to_event(ev: super::hub::SseEvent) -> Event {
    Event::default().id(ev.id.to_string()).event(ev.name).data(ev.data.to_string())
}

async fn events(State(s): S, peer: Option<Extension<TunnelPeer>>, headers: HeaderMap) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let last = headers.get("last-event-id").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
    let (backlog, rx) = s.hub.subscribe(last);
    // Through the tunnel, an open events stream means the app is in the
    // foreground on that phone (it cancels the stream when backgrounded): no
    // pushes to it while the stream lives. The guard rides in the stream
    // state, so it drops with the response body however the stream ends.
    let foreground = peer.and_then(|Extension(p)| s.tunnel.as_ref().map(|t| t.foreground(&p.pid)));
    // Open with a comment so the client sees body bytes at once: URLSession
    // (the phone) doesn't hand back the response until the first body byte, so
    // without this a quiet stream looked "connecting" until the 15 s heartbeat.
    let hello = stream::once(async { Ok(Event::default().comment("connected")) });
    let backlog = hello.chain(stream::iter(backlog.into_iter().map(|e| Ok(to_event(e)))));
    let live = stream::unfold((rx, foreground), |(mut rx, foreground)| async move {
        loop {
            match rx.recv().await {
                Ok(ev) => return Some((Ok(to_event(ev)), (rx, foreground))),
                // A slow client skipped some events; it keeps the stream and
                // can refetch (best effort, like Last-Event-ID).
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(backlog.chain(live)).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("heartbeat"))
}

#[cfg(test)]
mod ask_kind_tests {
    /// A permission escalation is marked as one; a question (and an ask
    /// from before the field existed) is a question.
    #[test]
    fn ask_dto_says_its_kind() {
        let chat = uuid::Uuid::new_v4();
        let q = hive_core::flow::HumanAsk { question: "Which database?".into(), ..Default::default() };
        assert_eq!(super::ask_dto(chat, &q)["kind"], "question");
        let p = hive_core::flow::HumanAsk { question: "@coder asks permission to run rm (execute). Allow it once?".into(), permission: true, ..Default::default() };
        assert_eq!(super::ask_dto(chat, &p)["kind"], "permission");
        let old: hive_core::flow::HumanAsk = serde_json::from_value(serde_json::json!({"askId": chat, "turnId": chat, "agentKey": "a", "question": "q", "atMs": 1})).unwrap();
        assert_eq!(super::ask_dto(chat, &old)["kind"], "question");
    }
}

#[cfg(test)]
mod responder_device_tests {
    use super::{message_dto, Viewer};
    use hive_core::{ChatMessage, MessageRole};
    use uuid::Uuid;

    /// A primary @hive turn reads "this" when this box ran it for its own
    /// account, "mine" when another device of that account did, and carries
    /// no place for someone else's (even one naming this box's device). A
    /// named agent's reply and a person's message carry neither field.
    #[test]
    fn primary_turns_are_placed_from_the_boxs_point_of_view() {
        let (box_dev, other_dev, acct) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let viewer = Viewer { device: Some(box_dev), account: Some(acct) };
        let turn = |account: Option<Uuid>, ran_on: Option<Uuid>| {
            let mut a = hive_core::ActorIdentity::new("actor", "Olive\u{202E}", hive_core::ActorKind::Human);
            a.account_id = account;
            let mut m = ChatMessage::new(MessageRole::Assistant, "hive", "ok");
            m.actor_identity = Some(a);
            m.origin_device_id = ran_on;
            m
        };
        let chat = Uuid::new_v4();
        let v = message_dto(chat, &turn(Some(acct), Some(box_dev)), viewer);
        assert_eq!(v["responderDevice"], "this");
        assert_eq!(v["responderName"], "Olive");
        assert_eq!(message_dto(chat, &turn(Some(acct), Some(other_dev)), viewer)["responderDevice"], "mine");
        let theirs = message_dto(chat, &turn(Some(Uuid::new_v4()), Some(box_dev)), viewer);
        assert!(theirs.get("responderDevice").is_none(), "{theirs}");
        assert_eq!(theirs["responderName"], "Olive");

        let mut agent = turn(Some(acct), Some(box_dev));
        agent.responder_id = Some(Uuid::new_v4());
        let v = message_dto(chat, &agent, viewer);
        assert!(v.get("responderDevice").is_none() && v.get("responderName").is_none());
        let mut person = turn(Some(acct), Some(box_dev));
        person.role = MessageRole::User;
        assert!(message_dto(chat, &person, viewer).get("responderDevice").is_none());
    }
}

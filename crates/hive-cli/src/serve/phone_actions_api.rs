//! What a person does from the phone (Phase B): `POST v1/phone-actions/prepare`,
//! `POST v1/phone-actions/presence` and `POST v1/phone-actions/commit`.
//!
//! This box is its own service member: it never posts, votes, answers,
//! clears a lockdown or asks for an apply as the person. The phone does, with
//! its own P-256 key, through the typed and allow-listed
//! [`PhoneAction`]s:
//!
//! 1. `prepare {deviceId, action}`: the box drafts the exact event as the
//!    phone's person, stamped for that phone (nothing is written), judges the
//!    unsigned draft as every fold will (a refusal is a 403 here, before the
//!    phone signs anything), and answers `{prepareId, kind, eventKind,
//!    presenceRequired, preview}` plus either `challenge` (presence needed:
//!    the person's passkey signs it) or `preimage` (no presence).
//! 2. `presence {prepareId, assertion}` (only when `presenceRequired`): the
//!    assertion goes into the stamp; the answer is the `preimage`.
//! 3. The phone checks the preimage
//!    (`hive_core::phone_action::check_phone_action_preimage`: exactly this
//!    action, workspace, log, person and phone, the presence it produced,
//!    nothing else) and signs it (ECDSA P-256 / SHA-256, 64-byte low-S).
//! 4. `commit {prepareId, signature}`: the box checks the signature against
//!    the phone's certificate, judges the event as every fold will, stores it
//!    and syncs it, and answers `{committed, eventId, kind}` (plus `messageId`
//!    for a post, `requestId` for an apply request).
//!
//! Drafts are single use, in memory, for five minutes. Through the tunnel,
//! `deviceId` must be the device the pairing proved it holds.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::{Extension, Json};
use base64::Engine;
use hive_core::phone_action::PhoneAction;
use hive_core::presence::{b64url, PresenceAssertion};
use hive_runtime::chat_service::{ChatError, PhoneDraft};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::api::{ApiError, ApiResult};
use super::tunnel::TunnelPeer;
use super::Shared;

const TTL: Duration = Duration::from_secs(300);
const MAX_PENDING: usize = 32;

struct Pending {
    draft: PhoneDraft,
    action: PhoneAction,
    presence_required: bool,
    presence: bool,
    at: Instant,
}

/// Drafts waiting for the phone (single use, in memory).
#[derive(Default)]
pub struct Drafts(Mutex<HashMap<String, Pending>>);

impl Drafts {
    fn put(&self, p: Pending) -> ApiResult<String> {
        let mut raw = [0u8; 16];
        getrandom::fill(&mut raw).map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("no OS randomness: {e}")))?;
        let id = b64url(&raw);
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, p| p.at.elapsed() < TTL);
        if m.len() >= MAX_PENDING {
            if let Some(oldest) = m.iter().min_by_key(|(_, p)| p.at).map(|(k, _)| k.clone()) {
                m.remove(&oldest);
            }
        }
        m.insert(id.clone(), p);
        Ok(id)
    }
    fn take(&self, id: &str) -> Option<Pending> {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.remove(id).filter(|p| p.at.elapsed() < TTL)
    }
    fn put_back(&self, id: String, p: Pending) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).insert(id, p);
    }
}

fn unb64(s: &str, what: &str) -> ApiResult<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim().trim_end_matches('=').as_bytes())
        .map_err(|_| ApiError::bad_request(format!("{what} isn't base64url")))
}

/// Through the tunnel, the device the pairing proved it holds must be the
/// one the request names (a pairing never drafts for another phone).
fn bound_device(s: &Shared, peer: Option<&TunnelPeer>, device: Uuid) -> ApiResult<()> {
    let Some(p) = peer else { return Ok(()) };
    let bound = s.tunnel.as_ref().and_then(|t| t.registry.get(&p.pid)).and_then(|r| r.device_id);
    if bound != Some(device) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "this pairing isn't that phone's"));
    }
    Ok(())
}

fn refused(e: ChatError) -> ApiError {
    match e {
        ChatError::Unauthorized(m) if m == hive_runtime::chat_service::STALE_DRAFT => ApiError::new(StatusCode::CONFLICT, m),
        ChatError::Unauthorized(m) => ApiError::new(StatusCode::FORBIDDEN, m),
        other => other.into(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrepareReq {
    /// The phone's workspace device id (its P-256 certificate's).
    device_id: Uuid,
    action: PhoneAction,
}

/// What the confirmation sheet shows (the box's description; the phone shows
/// what it parsed from the preimage, not this).
fn preview(s: &Shared, action: &PhoneAction) -> Value {
    let chat = s.lock().load(action.chat_id()).ok().flatten();
    let title = chat.as_ref().map(|c| c.title.clone()).unwrap_or_default();
    let mut v = json!({ "chatTitle": title });
    match action {
        PhoneAction::Post { .. } => v["summary"] = json!("Post a message"),
        PhoneAction::Vote { proposal_id, approved, .. } => {
            let p = chat.as_ref().and_then(|c| c.proposals.iter().find(|p| p.id == *proposal_id));
            v["summary"] = json!(if *approved { "Approve a proposal" } else { "Reject a proposal" });
            v["proposalTitle"] = json!(p.map(|p| p.title.clone()).unwrap_or_default());
        }
        PhoneAction::Answer { ask_id, .. } => {
            v["summary"] = json!("Answer an agent's question");
            v["question"] = json!(chat.as_ref().and_then(|c| c.flow.asks.get(ask_id)).map(|a| a.ask.question.clone()).unwrap_or_default());
        }
        PhoneAction::ClearLockdown { chat_id, raises } => {
            v["summary"] = json!("Clear this chat's lockdowns");
            // The lockdowns it names, as `GET /v1/guards` lists them.
            let guards = s.lock().workspace_guards(s.ws).ok();
            let named: Vec<Value> = guards
                .iter()
                .flat_map(|g| g.lockdowns_of(*chat_id).to_vec())
                .filter(|l| l.raise_event_id.is_some_and(|r| raises.contains(&r)))
                .map(|l| json!({ "raiseEventId": l.raise_event_id, "signal": l.signal, "detail": l.detail, "raisedAt": super::api::iso(l.raised_at) }))
                .collect();
            v["lockdowns"] = json!(named);
        }
        PhoneAction::RequestApply { proposal_id, host_device_id, .. } => {
            let p = chat.as_ref().and_then(|c| c.proposals.iter().find(|p| p.id == *proposal_id));
            v["summary"] = json!("Apply an approved proposal on a host");
            v["proposalTitle"] = json!(p.map(|p| p.title.clone()).unwrap_or_default());
            let hosts = s.lock().list_hosts(s.ws).unwrap_or_default();
            let label = hosts.iter().find(|h| h.id == host_device_id.to_string()).map(|h| h.label.clone());
            v["hostLabel"] = json!(label.unwrap_or_else(|| host_device_id.to_string()));
        }
        PhoneAction::WithdrawApply { .. } => v["summary"] = json!("Withdraw an apply request"),
    }
    v
}

/// `POST /v1/phone-actions/prepare` → `{prepareId, kind, eventKind,
/// presenceRequired, challenge | preimage, preview}`.
pub async fn prepare(State(s): State<std::sync::Arc<Shared>>, peer: Option<Extension<TunnelPeer>>, Json(req): Json<PrepareReq>) -> ApiResult<Json<Value>> {
    bound_device(&s, peer.as_deref(), req.device_id)?;
    if let Some(reason) = s.lock().write_block() {
        return Err(ApiError::new(StatusCode::FORBIDDEN, reason.to_string()));
    }
    if let PhoneAction::ClearLockdown { chat_id, .. } = &req.action {
        let locked = !s.lock().workspace_guards(s.ws)?.lockdowns_of(*chat_id).is_empty();
        if !locked {
            return Err(ApiError::new(StatusCode::CONFLICT, "this chat isn't locked down"));
        }
    }
    let (draft, presence_required) = s.lock().draft_phone_action(s.ws, req.device_id, &req.action).map_err(refused)?;
    let mut preview = preview(&s, &req.action);
    let mut out = json!({
        "kind": req.action.kind(),
        "eventKind": req.action.event_kind(),
        "presenceRequired": presence_required,
    });
    let mut draft = draft;
    if presence_required {
        let r = &draft.request;
        out["challenge"] = json!(b64url(&r.challenge));
        preview["digestShort"] = json!(r.digest_short);
        preview["rpId"] = json!(r.policy.rp_id);
        preview["origins"] = json!(r.policy.origins);
        preview["allowCredentials"] = json!(r.credential_ids);
    } else {
        out["preimage"] = json!(b64url(&hive_runtime::ChatService::phone_unconfirmed_preimage(&mut draft)));
    }
    out["preview"] = preview;
    let id = s.phone_action_drafts.put(Pending { draft, action: req.action, presence_required, presence: false, at: Instant::now() })?;
    out["prepareId"] = json!(id);
    Ok(Json(out))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Assertion {
    credential_id: String,
    authenticator_data: String,
    #[serde(alias = "clientDataJSON")]
    client_data_json: String,
    signature: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceReq {
    prepare_id: String,
    assertion: Assertion,
}

/// `POST /v1/phone-actions/presence` → `{preimage}` (base64url).
pub async fn presence(State(s): State<std::sync::Arc<Shared>>, Json(req): Json<PresenceReq>) -> ApiResult<Json<Value>> {
    let mut p = s.phone_action_drafts.take(&req.prepare_id).ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown or expired prepareId"))?;
    if !p.presence_required {
        s.phone_action_drafts.put_back(req.prepare_id, p);
        return Err(ApiError::new(StatusCode::CONFLICT, "this action needs no passkey: sign the preimage from prepare"));
    }
    if p.presence {
        s.phone_action_drafts.put_back(req.prepare_id, p);
        return Err(ApiError::new(StatusCode::CONFLICT, "already confirmed"));
    }
    let a = PresenceAssertion {
        credential_id: req.assertion.credential_id,
        authenticator_data: req.assertion.authenticator_data,
        client_data_json: req.assertion.client_data_json,
        signature: req.assertion.signature,
    };
    let preimage = hive_runtime::ChatService::phone_preimage(&mut p.draft, a);
    p.presence = true;
    s.phone_action_drafts.put_back(req.prepare_id, p);
    Ok(Json(json!({ "preimage": b64url(&preimage) })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitReq {
    prepare_id: String,
    /// The phone's P-256 signature over the preimage (64 bytes, base64url).
    signature: String,
}

/// `POST /v1/phone-actions/commit` → `{committed, eventId, kind, messageId?,
/// requestId?}`.
pub async fn commit(State(s): State<std::sync::Arc<Shared>>, Json(req): Json<CommitReq>) -> ApiResult<Json<Value>> {
    let p = s.phone_action_drafts.take(&req.prepare_id).ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown or expired prepareId"))?;
    if p.presence_required && !p.presence {
        return Err(ApiError::new(StatusCode::CONFLICT, "confirm with a passkey first (phone-actions/presence)"));
    }
    let signature = unb64(&req.signature, "signature")?;
    let action = p.action.clone();
    let env = s.lock().append_phone_signed(p.draft, signature).map_err(refused)?;
    let mut out = json!({ "committed": true, "eventId": env.event_id, "kind": action.kind() });
    match (&action, &env.payload) {
        (PhoneAction::Post { chat_id, .. }, hive_core::SessionEvent::MessageAppended { message }) => {
            out["messageId"] = json!(message.id);
            let chat = s.lock().load(*chat_id).ok().flatten();
            if let Some(c) = chat {
                s.mark_read(&c, message.id);
            }
        }
        (PhoneAction::RequestApply { host_device_id, .. }, hive_core::SessionEvent::ProposalApplyRequested { request }) => {
            out["requestId"] = json!(request.id);
            if host_device_id.to_string() == s.host_id {
                // This box applies it on its next worker tick.
                s.worker_kick.notify_one();
            }
        }
        (PhoneAction::ClearLockdown { chat_id, .. }, _) => {
            let who = env.actor_stamp.as_ref().map(|st| st.actor.display_name.clone()).unwrap_or_default();
            let mut svc = s.lock();
            if svc.load(*chat_id).ok().flatten().is_some() {
                let _ = svc.post_system_note(*chat_id, s.ws, format!("{who} cleared this chat's lockdown from their phone."));
            }
        }
        _ => {}
    }
    s.after_write();
    Ok(Json(out))
}

//! A phone renames itself (workspace model v3, 17.6): `POST
//! v1/phone-label/prepare` and `POST v1/phone-label/commit`.
//!
//! This box is its own service member: it never names a person's device. The
//! phone does, with its own P-256 key, as its person (the principal of the
//! pairing, never the box):
//!
//! 1. `prepare {deviceId, label}`: the box drafts `deviceLabelSet` for that
//!    phone under its person, on the workspace config log, stamped for the
//!    phone (nothing is written), judges the unsigned draft as every fold
//!    will, and answers `{prepareId, preimage}`. `label` must already be a
//!    clean name (at most 48 characters, no hidden characters or extra
//!    spaces): the box drafts exactly it or refuses. Before rules 4 a phone
//!    can't name itself: `409 {"error":"rename_on_desktop"}`.
//! 2. The phone checks the preimage
//!    (`ligus_core::phone_action::check_phone_label_preimage`: exactly this
//!    name, for itself, under its person, on the config log, no presence)
//!    and signs it (ECDSA P-256 / SHA-256, 64-byte low-S).
//! 3. `commit {prepareId, signature}`: the box checks the signature against
//!    the phone's certificate, judges the event as every fold will, stores
//!    and syncs it, and answers `{committed, eventId, label}`.
//!
//! A name needs no passkey (it is render-only). Drafts are single use, in
//! memory, for five minutes. Through the tunnel, `deviceId` must be the
//! device the pairing proved it holds. `GET /v1/me` says whether this works
//! here: `capabilities.phoneLabel` is 1 only when the workspace's rules let a
//! phone name itself.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use base64::Engine;
use ligus_core::presence::b64url;
use ligus_runtime::chat_service::{ChatError, PhoneDraft, RENAME_ON_DESKTOP};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::api::{ApiError, ApiResult};
use super::tunnel::TunnelPeer;
use super::Shared;

const TTL: Duration = Duration::from_secs(300);
/// Drafts one credential keeps (its oldest goes past this).
const PER_CALLER: usize = 2;
const MAX_PENDING: usize = 16;

/// The stable code of a rename this workspace's rules don't allow yet.
pub const RENAME_ON_DESKTOP_CODE: &str = "rename_on_desktop";

struct Pending {
    draft: PhoneDraft,
    label: String,
}

/// Drafts waiting for the phone (single use, in memory), per credential.
pub struct Drafts(super::drafts::OwnedDrafts<Pending>);

impl Default for Drafts {
    fn default() -> Self {
        Self(super::drafts::OwnedDrafts::new(TTL, PER_CALLER, MAX_PENDING))
    }
}

fn caller() -> String {
    super::api::caller().key()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrepareReq {
    /// The phone's workspace device id (its P-256 certificate's).
    device_id: Uuid,
    /// The new name, already clean.
    label: String,
}

fn rename_on_desktop() -> Response {
    (StatusCode::CONFLICT, Json(json!({ "error": RENAME_ON_DESKTOP_CODE, "message": RENAME_ON_DESKTOP }))).into_response()
}

fn refused(e: ChatError) -> ApiError {
    match e {
        ChatError::Unauthorized(m) if m == ligus_runtime::chat_service::STALE_DRAFT => ApiError::new(StatusCode::CONFLICT, m),
        ChatError::Unauthorized(m) => ApiError::new(StatusCode::FORBIDDEN, m),
        other => other.into(),
    }
}

/// `POST /v1/phone-label/prepare` → `{prepareId, preimage}`.
pub async fn prepare(State(s): State<std::sync::Arc<Shared>>, peer: Option<Extension<TunnelPeer>>, Json(req): Json<PrepareReq>) -> Response {
    match prepare_inner(&s, peer.as_deref(), req) {
        Ok(v) => v,
        Err(e) => e.into_response(),
    }
}

fn prepare_inner(s: &Shared, peer: Option<&TunnelPeer>, req: PrepareReq) -> ApiResult<Response> {
    super::phone_actions_api::bound_device(s, peer, req.device_id)?;
    if let Some(reason) = s.lock().write_block() {
        return Err(ApiError::new(StatusCode::FORBIDDEN, reason.to_string()));
    }
    let drafted = s.lock().draft_phone_label(s.ws, req.device_id, &req.label);
    let mut draft = match drafted {
        Ok(d) => d,
        Err(ChatError::Unauthorized(m)) if m == RENAME_ON_DESKTOP => return Ok(rename_on_desktop()),
        Err(e) => return Err(refused(e)),
    };
    let preimage = ligus_runtime::ChatService::phone_unconfirmed_preimage(&mut draft);
    let id = s
        .phone_label_drafts
        .0
        .put(&caller(), Pending { draft, label: req.label })
        .map_err(super::api::draft_refused)?;
    Ok(Json(json!({ "prepareId": id, "preimage": b64url(&preimage) })).into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommitReq {
    prepare_id: String,
    /// The phone's P-256 signature over the preimage (64 bytes, base64url).
    signature: String,
}

/// `POST /v1/phone-label/commit` → `{committed, eventId, label}`.
pub async fn commit(State(s): State<std::sync::Arc<Shared>>, Json(req): Json<CommitReq>) -> ApiResult<Json<serde_json::Value>> {
    let p = s
        .phone_label_drafts
        .0
        .take(&caller(), &req.prepare_id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown or expired prepareId"))?;
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(req.signature.trim().trim_end_matches('=').as_bytes())
        .map_err(|_| ApiError::bad_request("signature isn't base64url"))?;
    let env = s.lock().append_phone_signed(p.draft, signature).map_err(refused)?;
    s.after_write();
    Ok(Json(json!({ "committed": true, "eventId": env.event_id, "label": p.label })))
}

//! Admitting a box from the phone (Phase B): `POST v1/admissions/prepare`,
//! `POST v1/admissions/presence` and `POST v1/admissions/commit`.
//!
//! This box is its own service member, so it never admits anyone itself.
//! The phone does, with its own P-256 key and its person's passkey:
//!
//! 1. `prepare {deviceId, request?, ttlMinutes?}`: the box drafts the box
//!    admission as the phone's person, signed by that phone (nothing is
//!    written) and answers `{prepareId, challenge, preview}`.
//! 2. `presence {prepareId, assertion}`: the passkey assertion over the
//!    challenge goes into the stamp; the answer is the `preimage` the phone
//!    signs (ECDSA P-256 / SHA-256, 64-byte low-S), after checking it is the
//!    admission its passkey confirmed and nothing else
//!    (`ligus_core::admission::check_phone_admission_preimage`).
//! 3. `commit {prepareId, signature}`: the box checks the signature against
//!    the phone's certificate, judges the event as every fold will, appends
//!    and syncs it, and answers the admission `code` (sealed to the box that
//!    sent `request`, else a bearer code). The code carries the workspace
//!    key: tunnel or loopback only.
//!
//! The admitting person must be an Admin; the phone tier allows a box
//! admission and nothing else about membership.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::{Extension, Json};
use base64::Engine;
use ligus_core::presence::{b64url, PresenceAssertion};
use ligus_runtime::box_admission::JoinRequest;
use ligus_runtime::chat_service::PhoneDraft;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::api::{ApiError, ApiResult};

/// The calling credential's key ([`super::api::Caller::key`]).
fn caller() -> String {
    super::api::caller().key()
}
use super::tunnel::TunnelPeer;
use super::Shared;

const TTL: Duration = Duration::from_secs(300);
/// Drafts one credential keeps (its oldest goes past this).
const PER_CALLER: usize = 4;
const MAX_PENDING: usize = 16;

struct Pending {
    draft: PhoneDraft,
    invite: Uuid,
    /// The admission's redeem key seed: into the code at commit, never kept.
    seed: [u8; 32],
    request: Option<JoinRequest>,
    presence: bool,
}

/// Drafts waiting for the phone (single use, in memory), per credential.
pub struct Drafts(super::drafts::OwnedDrafts<Pending>);

impl Default for Drafts {
    fn default() -> Self {
        Self(super::drafts::OwnedDrafts::new(TTL, PER_CALLER, MAX_PENDING))
    }
}

fn unb64(s: &str, what: &str) -> ApiResult<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim().trim_end_matches('=').as_bytes())
        .map_err(|_| ApiError::bad_request(format!("{what} isn't base64url")))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareReq {
    /// The phone's workspace device id (its P-256 certificate's).
    device_id: Uuid,
    /// The box's join request (`ligusjoin1:…`); absent: a bearer code.
    #[serde(default)]
    request: Option<String>,
    #[serde(default)]
    ttl_minutes: Option<i64>,
}

/// `POST /v1/admissions/prepare` → `{prepareId, challenge, preview}`.
pub async fn prepare(State(s): State<std::sync::Arc<Shared>>, peer: Option<Extension<TunnelPeer>>, Json(req): Json<PrepareReq>) -> ApiResult<Json<Value>> {
    super::identity_api::require_secret_channel(&s, peer.as_deref())?;
    // Through the tunnel, only for the phone this pairing proved it holds.
    super::phone_actions_api::bound_device(&s, peer.as_deref(), req.device_id)?;
    let request = req.request.filter(|r| !r.trim().is_empty()).map(|r| JoinRequest::decode(&r)).transpose().map_err(ApiError::bad_request)?;
    let default = if request.is_some() {
        ligus_runtime::box_admission::DEFAULT_TTL_MS
    } else {
        ligus_runtime::box_admission::DEFAULT_BEARER_TTL_MS
    };
    let ttl = req.ttl_minutes.map(|m| m.saturating_mul(60_000)).unwrap_or(default);
    let (draft, invite, seed) = s
        .lock()
        .draft_phone_admission(s.ws, req.device_id, ttl, request.as_ref())
        .map_err(|e| ApiError::new(StatusCode::FORBIDDEN, e.to_string()))?;
    let r = &draft.request;
    let preview = json!({
        "kind": r.kind,
        "summary": "Admit a box as a service member",
        "digestShort": r.digest_short,
        "rpId": r.policy.rp_id,
        "origins": r.policy.origins,
        "allowCredentials": r.credential_ids,
        "boxFingerprint": request.as_ref().map(|q| q.fingerprint()),
        "boxLabel": request.as_ref().map(|q| ligus_runtime::display::shown(&q.label)),
        "sealed": request.is_some(),
    });
    let challenge = b64url(&r.challenge);
    let id = s.admission_drafts.0.put(&caller(), Pending { draft, invite, seed, request, presence: false }).map_err(super::api::draft_refused)?;
    Ok(Json(json!({ "prepareId": id, "challenge": challenge, "preview": preview })))
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

/// `POST /v1/admissions/presence` → `{preimage}` (base64url) for the phone
/// to sign.
pub async fn presence(State(s): State<std::sync::Arc<Shared>>, peer: Option<Extension<TunnelPeer>>, Json(req): Json<PresenceReq>) -> ApiResult<Json<Value>> {
    super::identity_api::require_secret_channel(&s, peer.as_deref())?;
    let (mut p, at) = s.admission_drafts.0.take_aged(&caller(), &req.prepare_id).ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown or expired prepareId"))?;
    if p.presence {
        return Err(ApiError::new(StatusCode::CONFLICT, "already confirmed"));
    }
    let a = PresenceAssertion {
        credential_id: req.assertion.credential_id,
        authenticator_data: req.assertion.authenticator_data,
        client_data_json: req.assertion.client_data_json,
        signature: req.assertion.signature,
    };
    let preimage = ligus_runtime::ChatService::phone_preimage(&mut p.draft, a);
    p.presence = true;
    s.admission_drafts.0.put_back(&caller(), req.prepare_id, p, at);
    Ok(Json(json!({ "preimage": b64url(&preimage) })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitReq {
    prepare_id: String,
    /// The phone's P-256 signature over the preimage (64 bytes, base64url).
    signature: String,
}

/// `POST /v1/admissions/commit` → `{committed, eventId, code, sealed,
/// expiresInMinutes}`.
pub async fn commit(State(s): State<std::sync::Arc<Shared>>, peer: Option<Extension<TunnelPeer>>, Json(req): Json<CommitReq>) -> ApiResult<Json<Value>> {
    super::identity_api::require_secret_channel(&s, peer.as_deref())?;
    let p = s.admission_drafts.0.take(&caller(), &req.prepare_id).ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown or expired prepareId"))?;
    if !p.presence {
        return Err(ApiError::new(StatusCode::CONFLICT, "confirm with a passkey first (admissions/presence)"));
    }
    let signature = unb64(&req.signature, "signature")?;
    let expires_ms = match &p.draft.env.payload {
        ligus_core::SessionEvent::JoinApproved { admission: Some(a), .. } => a.expires_ms,
        _ => 0,
    };
    let issued_ms = ligus_core::admission::envelope_ms(&p.draft.env);
    let env = s.lock().append_phone_signed(p.draft, signature).map_err(|e| ApiError::new(StatusCode::FORBIDDEN, e.to_string()))?;
    s.after_write();
    let code = {
        let svc = s.lock();
        crate::serve::admission_code(&s.cfg, &svc, p.invite, &p.seed).map_err(|e| ApiError::new(StatusCode::CONFLICT, e.to_string()))?
    };
    let (code, sealed) = match &p.request {
        Some(r) => (r.seal(&code).map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?, true),
        None => (code, false),
    };
    Ok(Json(json!({
        "committed": true,
        "eventId": env.event_id,
        "code": code,
        "sealed": sealed,
        "expiresInMinutes": (expires_ms - issued_ms).max(0) / 60_000,
    })))
}

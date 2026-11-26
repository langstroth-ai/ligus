//! `GET /v1/me`: who the requesting phone is to this box, so the app needs no
//! manual setup after "Add this phone".
//!
//! Only over the tunnel (end-to-end encrypted to this box) or a loopback
//! listener. Through the tunnel the phone is the workspace device its
//! pairing proved (`tunnel::device_bind`), never one the request names; on
//! loopback the caller names it with `?deviceId=`.
//!
//! Answer: this workspace's id and room, its config log id, the phone
//! device (id, key fingerprint, tier), the account it is certified for with
//! that person's member id, name and role, and the passkey relying party
//! (`rpId`, `origins`) every presence ceremony must use. A phone this box
//! doesn't know as a certified phone of a member gets `403
//! {"error":"device_not_certified"}`.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::tunnel::TunnelPeer;
use super::Shared;

/// The stable code of a phone that isn't (yet) a certified phone here.
pub const DEVICE_NOT_CERTIFIED: &str = "device_not_certified";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeQuery {
    /// Loopback only: the phone device to describe. Ignored through the
    /// tunnel (the pairing's device is used).
    #[serde(default)]
    device_id: Option<Uuid>,
}

fn refuse(status: StatusCode, error: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({ "error": error, "message": message.into() })),
    )
        .into_response()
}

/// `GET /v1/me`.
pub async fn me(
    State(s): State<std::sync::Arc<Shared>>,
    peer: Option<Extension<TunnelPeer>>,
    Query(q): Query<MeQuery>,
) -> Response {
    if !super::forges_api::secure_channel(&s, peer.as_deref()) {
        return refuse(
            StatusCode::FORBIDDEN,
            "secure_channel_required",
            "GET /v1/me answers only through the end-to-end encrypted tunnel or on the box itself (loopback)",
        );
    }
    let device = match peer.as_deref() {
        Some(p) => match s
            .tunnel
            .as_ref()
            .and_then(|t| t.registry.get(&p.pid))
            .and_then(|r| r.device_id)
        {
            Some(d) => d,
            None => {
                return refuse(
                    StatusCode::FORBIDDEN,
                    DEVICE_NOT_CERTIFIED,
                    "this pairing isn't bound to a workspace device",
                )
            }
        },
        None => match q.device_id {
            Some(d) => d,
            None => {
                return refuse(
                    StatusCode::BAD_REQUEST,
                    "device_id_required",
                    "on loopback, name the phone with ?deviceId=",
                )
            }
        },
    };
    let svc = s.lock();
    let person = match svc.phone_person(s.ws, device) {
        Ok(p) => p,
        Err(e) => {
            return refuse(
                StatusCode::FORBIDDEN,
                DEVICE_NOT_CERTIFIED,
                format!("{e}. Certify the phone on your desktop (My devices > Add this phone), let this box sync, then try again"),
            )
        }
    };
    let config_id = ligus_core::workspace_config_session_id(s.ws);
    let config = svc.load_resolving(config_id).ok().flatten();
    let role = config.as_ref().map(|c| c.role_of(&person.id));
    let (_, timeline) = match svc.store().load_config(s.ws, None, None) {
        Ok(v) => v,
        Err(e) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()),
    };
    let fingerprint = timeline
        .current()
        .devices
        .get(&device)
        .map(|d| ligus_runtime::device_enroll::fingerprint(&d.public_key))
        .unwrap_or_default();
    let account = person.account_id.unwrap_or_default();
    let consented = svc.phone_consented(s.ws, device, account).unwrap_or(false);
    let label = svc
        .device_label_of(s.ws, device, account)
        .unwrap_or_default();
    let policy = match svc.workspace_presence_policy(s.ws) {
        Ok(p) => p,
        Err(e) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()),
    };
    Json(json!({
        "workspaceId": s.ws,
        "room": s.cfg.room,
        "workspaceName": config.as_ref().map(|c| ligus_runtime::display::workspace_name(&c.title)).unwrap_or_default(),
        "configLogId": config_id,
        "device": { "id": device, "fingerprint": fingerprint, "tier": "phone", "label": label },
        "account": {
            "id": person.account_id,
            "memberId": person.id,
            "displayName": person.display_name,
            "role": role,
            "consented": consented,
        },
        // Identity model v2: this box takes the phone's enrollment
        // statement (`POST /v1/phone-consent`).
        "capabilities": { "phoneConsent": 1 },
        "rpId": policy.rp_id,
        "origins": policy.origins,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct ConsentBody {
    /// base64url (no padding) of the phone's 64-byte P-256 signature over
    /// `ligus_core::crypto::phone_consent_preimage(workspace, account, phone)`.
    signature: String,
}

/// `POST /v1/phone-consent`: the phone's signed enrollment statement for the
/// person this box knows it as (identity model v2, 7.3a; decision O12).
/// Through the tunnel only: the phone is the pairing's device. The box
/// checks it against the phone's certified key and publishes it with the
/// phone's certificate.
pub async fn phone_consent(
    State(s): State<std::sync::Arc<Shared>>,
    peer: Option<Extension<TunnelPeer>>,
    Json(body): Json<ConsentBody>,
) -> Response {
    use base64::Engine as _;
    let Some(p) = peer.as_deref() else {
        return refuse(
            StatusCode::FORBIDDEN,
            "secure_channel_required",
            "a phone's statement comes through the end-to-end encrypted tunnel only",
        );
    };
    let Some(device) = s
        .tunnel
        .as_ref()
        .and_then(|t| t.registry.get(&p.pid))
        .and_then(|r| r.device_id)
    else {
        return refuse(
            StatusCode::FORBIDDEN,
            DEVICE_NOT_CERTIFIED,
            "this pairing isn't bound to a workspace device",
        );
    };
    let Ok(sig) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(body.signature.trim())
    else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "bad_signature",
            "the signature isn't base64url",
        );
    };
    let mut svc = s.lock();
    let person = match svc.phone_person(s.ws, device) {
        Ok(p) => p,
        Err(e) => return refuse(StatusCode::FORBIDDEN, DEVICE_NOT_CERTIFIED, e.to_string()),
    };
    let Some(account) = person.account_id else {
        return refuse(
            StatusCode::CONFLICT,
            "not_this_account",
            "this phone's person has no account here",
        );
    };
    match svc.publish_phone_consent(s.ws, device, account, &sig) {
        Ok(true) => {
            drop(svc);
            s.after_write();
            Json(json!({ "published": true })).into_response()
        }
        Ok(false) => {
            Json(json!({ "published": false, "reason": "already published" })).into_response()
        }
        Err(e)
            if e.to_string()
                .contains(ligus_runtime::chat_service::BAD_CONSENT) =>
        {
            refuse(StatusCode::BAD_REQUEST, "bad_signature", e.to_string())
        }
        Err(e) => refuse(StatusCode::CONFLICT, "not_this_account", e.to_string()),
    }
}

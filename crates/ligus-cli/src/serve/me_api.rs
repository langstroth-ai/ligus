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
    (status, Json(json!({ "error": error, "message": message.into() }))).into_response()
}

/// `GET /v1/me`.
pub async fn me(State(s): State<std::sync::Arc<Shared>>, peer: Option<Extension<TunnelPeer>>, Query(q): Query<MeQuery>) -> Response {
    if !super::forges_api::secure_channel(&s, peer.as_deref()) {
        return refuse(
            StatusCode::FORBIDDEN,
            "secure_channel_required",
            "GET /v1/me answers only through the end-to-end encrypted tunnel or on the box itself (loopback)",
        );
    }
    let device = match peer.as_deref() {
        Some(p) => match s.tunnel.as_ref().and_then(|t| t.registry.get(&p.pid)).and_then(|r| r.device_id) {
            Some(d) => d,
            None => return refuse(StatusCode::FORBIDDEN, DEVICE_NOT_CERTIFIED, "this pairing isn't bound to a workspace device"),
        },
        None => match q.device_id {
            Some(d) => d,
            None => return refuse(StatusCode::BAD_REQUEST, "device_id_required", "on loopback, name the phone with ?deviceId="),
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
    let config = svc.load(config_id).ok().flatten();
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
    let policy = match svc.workspace_presence_policy(s.ws) {
        Ok(p) => p,
        Err(e) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()),
    };
    Json(json!({
        "workspaceId": s.ws,
        "room": s.cfg.room,
        "workspaceName": config.as_ref().map(|c| c.title.clone()).unwrap_or_default(),
        "configLogId": config_id,
        "device": { "id": device, "fingerprint": fingerprint, "tier": "phone" },
        "account": {
            "id": person.account_id,
            "memberId": person.id,
            "displayName": person.display_name,
            "role": role,
        },
        "rpId": policy.rp_id,
        "origins": policy.origins,
    }))
    .into_response()
}

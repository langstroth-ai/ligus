//! `GET /v1/setup`: the "Set up Ligus" checklist for the person behind the
//! calling phone (the phone's Health shows it), the same rows the desktop's
//! Settings > Set up, `ligus setup` and lt show
//! (`ligus_runtime::setup::checklist`, `ligus.setup.v1`).
//!
//! The rows are the person's, never this box's: their profile, passkeys and
//! devices, with "This device" the calling phone and push from this box's own
//! pairing records. A phone this box can't place gets `403
//! {"error":"unresolved"}` and no rows (it would only be the box's). Clean up
//! is listed for the person to see; every removal stays a passkey ceremony
//! on their desktop, so the phone offers no action for it.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use uuid::Uuid;

use ligus_runtime::setup::checklist::{self, Live, Viewer};

use super::Shared;

/// `GET /v1/setup`.
pub async fn checklist(State(s): State<std::sync::Arc<Shared>>) -> Response {
    let who = super::api::principal();
    let (Some(person), Some(device)) = (who.person.clone(), who.device) else {
        return (StatusCode::FORBIDDEN, Json(json!({ "error": "unresolved", "message": super::principal::UNRESOLVED_HINT }))).into_response();
    };
    if who.is_box() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "not_a_phone", "message": "the box's own checklist is `ligus setup`" }))).into_response();
    }
    // Push per phone device, from this box's pairings (live ones only).
    let pairings: Vec<(Uuid, bool)> = s
        .tunnel
        .as_ref()
        .map(|t| t.registry.list().into_iter().filter(|r| r.revoked_at.is_none()).filter_map(|r| Some((r.device_id?, r.push_enabled))).collect())
        .unwrap_or_default();
    let push = move |d: Uuid| pairings.iter().find(|(id, _)| *id == d).map(|(_, on)| *on);
    let now = ligus_core::Timestamp::now();
    let here = s.host_id.clone();
    let online = |h: &ligus_core::WorkspaceHost| h.id == here || s.presence.is_online(h, &now);
    let status = |id: &str| s.presence.status_of(id);
    let live = Live {
        online: &online,
        status: &status,
        latest_release: None,
        this_device_word: "this phone".into(),
        workspace_name: String::new(),
        push: &push,
        hidden: Vec::new(),
        now,
    };
    let viewer = Viewer { actor_id: person.id.clone(), account: person.account_id, device };
    let svc = s.lock();
    let mut facts = checklist::gather(&svc, s.ws, &viewer, &live);
    drop(svc);
    // This box reports its own build: the phone compares it with the latest
    // release itself (its Health already does).
    if let Some(b) = facts.boxes.iter_mut().find(|b| b.id == s.host_id) {
        b.build = env!("CARGO_PKG_VERSION").to_string();
    }
    Json(checklist::checklist(&facts)).into_response()
}

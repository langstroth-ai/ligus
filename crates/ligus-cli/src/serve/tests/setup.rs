//! `GET /v1/setup`: the "Set up Ligus" rows for the person behind a phone
//! (the phone's Health), never the box's own, and nothing for a phone the
//! box can't place.

use axum::http::Request;
use tower::ServiceExt as _;

use super::approvals::world;
use super::*;
use crate::serve::principal::{Principal, Standing};

async fn get_setup(w: &super::approvals::World, who: &Principal) -> (u16, Value) {
    let app = axum::Router::new().route("/v1/setup", axum::routing::get(crate::serve::setup_api::checklist)).with_state(w.h.handle.shared.clone());
    let req = Request::builder().uri("/v1/setup").body(Body::empty()).unwrap();
    let resp = crate::serve::api::acting_as(who.clone(), app.oneshot(req)).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_phone_gets_its_persons_rows_and_an_unplaced_phone_none() {
    let w = world().await;
    let person = w.h.handle.shared.lock().phone_person(w.ws, w.phone.device_id).unwrap();
    let ana = Principal { person: Some(person.clone()), device: Some(w.phone.device_id), via_host: None, standing: Standing::Person };

    let (status, v) = get_setup(&w, &ana).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["schema"], "ligus.setup.v1");
    let ids: Vec<&str> = v["rows"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["you", "passkey", "device", "devices", "phone", "box", "agents", "workspace", "cleanup"]);
    let row = |id: &str| v["rows"].as_array().unwrap().iter().find(|r| r["id"] == id).unwrap().clone();
    // The person's own role, not the box's (a Contributor service member).
    let role = w.h.handle.shared.lock().load_in(w.ws, ligus_core::workspace_config_session_id(w.ws)).unwrap().unwrap().role_of(&person.id);
    assert_eq!(role, ligus_core::WorkspaceRole::Owner, "the desktop's person founded it");
    // This box is the always-on box, with its own build.
    assert!(row("box")["detail"].as_str().unwrap().contains(env!("CARGO_PKG_VERSION")), "{}", row("box"));
    for r in v["rows"].as_array().unwrap() {
        assert!(["done", "notSetUp", "attention"].contains(&r["state"].as_str().unwrap()), "{r}");
    }

    // A phone this box can't place, and the operator: no rows.
    let (status, v) = get_setup(&w, &Principal::unresolved()).await;
    assert_eq!((status, v["error"].clone()), (403, json!("unresolved")), "{v}");
    let me = w.h.handle.shared.lock().author().clone();
    let operator = Principal { person: Some(me), device: None, via_host: None, standing: Standing::Box };
    let (status, _) = get_setup(&w, &operator).await;
    assert_ne!(status, 200);
    w.h.handle.shutdown();
}

//! B1 (identity model v2, 7.2): a certificate another member puts on a
//! running box's device key never blocks, redirects or adopts the box.
//! Device keys are public (every certificate on the log carries one), so
//! before 0.3.x any member could pause a box's writes by certifying its key
//! for their own account. Only a revocation stops it.

use super::*;

/// Wait (bounded) until `f` holds on the box's health.
async fn health_until(h: &Harness, what: &str, f: impl Fn(&Value) -> bool) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let v = h.get("/v1/health").await;
        if f(&v) {
            return v;
        }
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}: {v}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_foreign_certificate_on_a_running_box_blocks_nothing_and_revocation_still_does() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    let ws = h.handle.shared.ws;
    let (box_device, box_key, box_actor) = {
        let svc = h.handle.shared.lock();
        (svc.device_id(), svc.device_public_key(), svc.author().clone())
    };
    let chat = h.send(reqwest::Method::POST, "/v1/chats", Some(json!({ "title": "t" }))).await;
    let chat_id = Uuid::parse_str(chat["chat"]["id"].as_str().unwrap()).unwrap();

    // The desktop's person certifies the box's public device key for their
    // own account (a modified client can; the rules accept it), after
    // seeing the box's admission (a certificate drafted before it, sorting
    // before the box's own redemption, is a fold matter: rules 3).
    sync_once(&h.desktop).await.unwrap();
    let founder = {
        let mut svc = open_service(&h.desktop).unwrap();
        svc.publish_foreign_certificate(ws, &box_key).unwrap();
        svc.author().clone()
    };
    sync_once(&h.desktop).await.unwrap();
    let health = health_until(&h, "the foreign certificate to reach the box", |v| {
        v["identity"]["alsoCertifiedBy"].as_array().is_some_and(|a| !a.is_empty())
    })
    .await;
    assert_eq!(health["identity"]["mode"], "member", "{health}");
    assert_eq!(health["identity"]["actorId"], json!(box_actor.id), "the box still acts as itself: {health}");
    assert_eq!(h.handle.shared.standing(), Standing::Own);

    // Writes still work, as the box itself.
    let posted = h.send(reqwest::Method::POST, &format!("/v1/chats/{chat_id}/messages"), Some(json!({ "body": "still here" }))).await;
    let mid = Uuid::parse_str(posted["message"]["id"].as_str().unwrap()).unwrap();
    let s = h.handle.shared.lock().load_resolving(chat_id).unwrap().unwrap();
    let m = s.messages.iter().find(|m| m.id == mid).unwrap();
    assert_eq!(m.actor_identity.as_ref().map(|a| a.id.clone()), Some(box_actor.id.clone()), "never redirected to {}", founder.id);

    // A revocation still stops it.
    {
        let mut svc = open_service(&h.desktop).unwrap();
        sync_once(&h.desktop).await.unwrap();
        svc.revoke_device(workspace_config_session_id(ws), ws, box_device).unwrap();
    }
    sync_once(&h.desktop).await.unwrap();
    health_until(&h, "the revocation to reach the box", |v| v["identity"]["mode"] == "revoked").await;
    let (code, v) = status_of(&h, reqwest::Method::POST, &format!("/v1/chats/{chat_id}/messages"), Some(json!({ "body": "after" }))).await;
    assert!(!(200..300).contains(&code), "a revoked box still wrote: {code} {v}");
    h.handle.shutdown();
}

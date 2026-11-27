//! Who a request acts for (`serve::principal`), on the direct path: the
//! operator is the box itself; the shared direct-path token (every phone
//! paired before per-phone credentials) acts for nobody, so a "you" field
//! claims nothing for it and it is told why. Two people's phones through the
//! tunnel and on their own direct-path credentials are
//! `tunnel_e2e::two_peoples_phones_each_see_their_own`.

use super::*;

async fn raw(h: &Harness, token: &str, path: &str) -> reqwest::Response {
    h.client.get(format!("{}{path}", h.base)).bearer_auth(token).timeout(REQ).send().await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_shared_token_acts_for_nobody_and_the_operator_for_the_box() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    // A person's box, so the box itself has a row in People.
    let h = harness_with(&endpoint, "m", &[("README.md", "x\n")], |o| o.test_person = true).await;
    let (shared, operator) = (h.handle.shared.token(), h.handle.shared.operator_token());
    let box_id = h.handle.shared.my_actor_id();

    // Health says who the request acts for; the shared token's answers are
    // marked so a newer app swaps it for its own.
    let r = raw(&h, &shared, "/v1/health").await;
    assert_eq!(r.headers().get(super::principal::DIRECT_CREDENTIAL_HEADER).unwrap(), "shared");
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["principal"], "unresolved");
    assert_eq!(v["hint"], super::principal::UNRESOLVED_HINT);
    let r = raw(&h, &operator, "/v1/health").await;
    assert!(r.headers().get(super::principal::DIRECT_CREDENTIAL_HEADER).is_none());
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["principal"], "box");
    assert!(v.get("hint").is_none());
    // Without a credential: nothing about anyone.
    let v: Value = h.client.get(format!("{}/v1/health", h.base)).send().await.unwrap().json().await.unwrap();
    assert!(v.get("principal").is_none(), "{v}");

    // A proposal: the box's standing for the operator, none for nobody.
    let (_, pid) = filed_proposal(&h, Some(h.agent_id)).await;
    let p: Value = raw(&h, &shared, &format!("/v1/proposals/{pid}")).await.json().await.unwrap();
    assert_eq!((p["proposal"]["myRole"].clone(), p["proposal"]["canVote"].clone()), (Value::Null, json!(false)), "{p}");
    let p: Value = raw(&h, &operator, &format!("/v1/proposals/{pid}")).await.json().await.unwrap();
    assert!(p["proposal"]["myRole"].is_string(), "{p}");

    // Guards: nobody has a role, or may clear a lockdown.
    let g: Value = raw(&h, &shared, "/v1/guards").await.json().await.unwrap();
    assert_eq!((g["myRole"].clone(), g["canClearLockdown"].clone()), (Value::Null, json!(false)), "{g}");

    // People: "you" is nobody's row for the shared token, the box's for the
    // operator.
    let selves = |v: &Value| -> Vec<String> {
        v["people"].as_array().unwrap().iter().filter(|p| p["isSelf"] == json!(true)).map(|p| p["actorId"].as_str().unwrap().to_string()).collect()
    };
    assert!(selves(&raw(&h, &shared, "/v1/people").await.json().await.unwrap()).is_empty());
    assert_eq!(selves(&raw(&h, &operator, "/v1/people").await.json().await.unwrap()), vec![box_id]);

    // A shared-token request still works: it reads and posts in the
    // workspace's chats (as the box, as before), and its badges clear.
    let chat = h.send(reqwest::Method::POST, "/v1/chats", Some(json!({ "title": "Old phone" }))).await;
    let chat_id = chat["chat"]["id"].as_str().unwrap().to_string();
    h.send(reqwest::Method::POST, &format!("/v1/chats/{chat_id}/messages"), Some(json!({ "body": "still works" }))).await;
    let row = |v: Value| v["chats"].as_array().unwrap().iter().find(|c| c["id"] == json!(chat_id)).unwrap().clone();
    assert_eq!(row(h.get("/v1/chats").await)["unread"], json!(0), "its own post is read");
    {
        let ws = h.handle.shared.ws;
        let c = Uuid::parse_str(&chat_id).unwrap();
        let mid = h.handle.shared.lock().begin_assistant_message(c, ws, "coder".to_string(), "gw".to_string(), Some(h.agent_id), None).unwrap();
        h.handle.shared.lock().complete_assistant_message(c, ws, mid, "a reply").unwrap();
        h.handle.shared.refresh();
    }
    assert_eq!(row(h.get("/v1/chats").await)["unread"], json!(1));
    h.get(&format!("/v1/chats/{chat_id}/messages")).await;
    assert_eq!(row(h.get("/v1/chats").await)["unread"], json!(0), "reading clears it");
    // The operator's marks are the box's own, apart from those.
    assert_eq!(row(raw(&h, &operator, "/v1/chats").await.json().await.unwrap())["unread"], json!(1));
    h.handle.shutdown();
}

/// The read marks an older box kept (one map for every phone) are the
/// baseline: a person who never opened a chat here counts its unread from
/// there, never from the start of its history; their own marks win.
#[test]
fn an_older_boxs_read_marks_are_everyones_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), "http://127.0.0.1:9", "room");
    let (chat, seen, later) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    std::fs::write(dir.path().join("serve-state.json"), json!({ "readMarks": { chat.to_string(): seen.to_string() } }).to_string()).unwrap();
    let mut marks = load_read_marks(&cfg);
    assert_eq!(marks.mark(Some("ana"), chat), Some(seen));
    assert_eq!(marks.mark(Some(super::principal::UNRESOLVED_READS), chat), Some(seen));
    marks.by_person.entry("ana".into()).or_default().insert(chat, later);
    assert_eq!(marks.mark(Some("ana"), chat), Some(later));
    assert_eq!(marks.mark(Some("ben"), chat), Some(seen));
    // Saved, an older box still finds its own map.
    std::fs::write(dir.path().join("serve-state.json"), marks.to_json().to_string()).unwrap();
    let back = load_read_marks(&cfg);
    assert_eq!(back.baseline.get(&chat), Some(&seen));
    assert_eq!(back.mark(Some("ana"), chat), Some(later));
}

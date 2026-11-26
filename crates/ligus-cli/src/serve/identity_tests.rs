//! Identity on the box (IDENTITY-AND-FORGES §3-§4, review R1/S1-S4): two
//! `ligus serve` boxes on one relay with contact routes. Box A founds the
//! workspace and invites box B with a signed link; A opens a DM with B, the
//! invite reaches B's drop box sealed to B's verified device, B accepts it
//! and they talk. Forged, unsigned and misaddressed invites are refused.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, put};
use axum::{Json, Router};
use serde_json::{json, Value};
use uuid::Uuid;

use super::invite_code::InviteConn;
use super::*;

#[path = "../../../ligus-runtime/src/test_relay.rs"]
#[allow(dead_code)]
mod test_relay;

// ── A relay with contact routes (relay v0.3's shape, in memory) ────────────

#[derive(Default)]
struct Contacts {
    /// cid → (write key, record).
    records: HashMap<String, (String, Value)>,
    /// cid → [(seq, blob)].
    drops: HashMap<String, Vec<(u64, Value)>>,
    seq: u64,
    /// Every header a contact route saw (to check what the box sends).
    auth_seen: Vec<String>,
    /// While set, record writes fail (503): no box gets a contact id.
    hold_records: bool,
    /// cid → every drop ever posted (acks don't lower it).
    posted: HashMap<String, usize>,
}

type C = Arc<Mutex<Contacts>>;

fn key_of(h: &HeaderMap) -> String {
    h.get(ligus_runtime::contacts::CONTACT_KEY_HEADER).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string()
}

async fn put_rec(State(c): State<C>, Path(cid): Path<String>, h: HeaderMap, Json(b): Json<Value>) -> (StatusCode, Json<Value>) {
    let mut c = c.lock().unwrap();
    if let Some(a) = h.get("authorization").and_then(|v| v.to_str().ok()) {
        c.auth_seen.push(a.to_string());
    }
    if c.hold_records {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({})));
    }
    let key = key_of(&h);
    let created = match c.records.get(&cid) {
        Some((k, _)) if *k != key => return (StatusCode::FORBIDDEN, Json(json!({}))),
        Some(_) => false,
        None => {
            if b.get("writeKey").and_then(Value::as_str) != Some(key.as_str()) {
                return (StatusCode::BAD_REQUEST, Json(json!({})));
            }
            true
        }
    };
    c.records.insert(cid, (key, b["record"].clone()));
    (StatusCode::OK, Json(json!({ "created": created, "updatedAt": 1 })))
}

async fn get_rec(State(c): State<C>, Path(cid): Path<String>) -> (StatusCode, Json<Value>) {
    match c.lock().unwrap().records.get(&cid) {
        Some((_, r)) => (StatusCode::OK, Json(json!({ "record": r, "updatedAt": 1 }))),
        None => (StatusCode::NOT_FOUND, Json(json!({}))),
    }
}

async fn post_drop(State(c): State<C>, Path(cid): Path<String>, Json(b): Json<Value>) -> StatusCode {
    let mut c = c.lock().unwrap();
    if !c.records.contains_key(&cid) {
        return StatusCode::NOT_FOUND;
    }
    c.seq += 1;
    let seq = c.seq;
    *c.posted.entry(cid.clone()).or_default() += 1;
    c.drops.entry(cid).or_default().push((seq, b["blob"].clone()));
    StatusCode::OK
}

async fn list_drops(State(c): State<C>, Path(cid): Path<String>, h: HeaderMap, Query(q): Query<HashMap<String, String>>) -> (StatusCode, Json<Value>) {
    let c = c.lock().unwrap();
    if c.records.get(&cid).map(|(k, _)| k.as_str()) != Some(key_of(&h).as_str()) {
        return (StatusCode::FORBIDDEN, Json(json!({})));
    }
    let after: u64 = q.get("after").and_then(|a| a.parse().ok()).unwrap_or(0);
    let items: Vec<Value> = c
        .drops
        .get(&cid)
        .into_iter()
        .flatten()
        .filter(|(s, _)| *s > after)
        .map(|(s, b)| json!({ "seq": s, "blob": b, "createdAt": 1 }))
        .collect();
    (StatusCode::OK, Json(json!({ "items": items, "more": false })))
}

async fn ack_drops(State(c): State<C>, Path(cid): Path<String>, h: HeaderMap, Query(q): Query<HashMap<String, String>>) -> StatusCode {
    let mut c = c.lock().unwrap();
    if c.records.get(&cid).map(|(k, _)| k.as_str()) != Some(key_of(&h).as_str()) {
        return StatusCode::FORBIDDEN;
    }
    let through: u64 = q.get("through").and_then(|a| a.parse().ok()).unwrap_or(0);
    if let Some(d) = c.drops.get_mut(&cid) {
        d.retain(|(s, _)| *s > through);
    }
    StatusCode::OK
}

async fn spawn_relay() -> (String, C) {
    let c: C = Arc::default();
    let contacts = Router::new()
        .route("/v1/contacts/{cid}", put(put_rec).get(get_rec))
        .route("/v1/contacts/{cid}/drop", get(list_drops).post(post_drop).delete(ack_drops))
        .with_state(c.clone());
    let router = test_relay::router().merge(contacts);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{addr}"), c)
}

// ── Boxes ──────────────────────────────────────────────────────────────────

struct Box_ {
    handle: ServeHandle,
    base: String,
    token: String,
    client: reqwest::Client,
}

fn opts(dir: &std::path::Path, name: &str) -> ServeOptions {
    ServeOptions {
        listen: "127.0.0.1:0".parse().unwrap(),
        token_file: dir.join("serve.token"),
        header_read_timeout: super::HEADER_READ_TIMEOUT,
        idle_timeout: super::IDLE_TIMEOUT,
        ntfy: None,
        public_url: None,
        label: format!("{name}-box"),
        name: Some(name.into()),
        pair: false,
        prometheus_url: "http://127.0.0.1:9".into(),
        prometheus_configured: false,
        dashboard_file: dir.join("dashboard.toml"),
        default_agent: None,
        sync_every: Duration::from_millis(200),
        remote: None,
        appearance: Default::default(),
        // DMs and invites are a person's: these boxes run as people.
        test_person: true,
    }
}

impl Box_ {
    async fn start(cfg: Config, name: &str) -> Self {
        let handle = start(cfg.clone(), opts(&cfg.data_dir, name)).await.unwrap();
        let base = format!("http://{}", handle.addr);
        // The person at the box: the operator's token (invites and admission
        // codes travel only to the operator on loopback, or the tunnel).
        let token = handle.shared.operator_token();
        Self { handle, base, token, client: ligus_forge::new_http_client() }
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = self.client.request(method, format!("{}{path}", self.base)).bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await.unwrap();
        let status = r.status().as_u16();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> Value {
        let (code, v) = self.call(reqwest::Method::GET, path, None).await;
        assert_eq!(code, 200, "GET {path}: {v}");
        v
    }

    async fn post(&self, path: &str, body: Value) -> Value {
        let (code, v) = self.call(reqwest::Method::POST, path, Some(body)).await;
        assert!((200..300).contains(&code), "POST {path}: {code} {v}");
        v
    }

    fn account(&self) -> Uuid {
        self.handle.shared.my_account().unwrap()
    }

    fn kick(&self) {
        self.handle.shared.sync_kick.notify_one();
        self.handle.shared.dm_kick.notify_one();
    }
}

fn cfg_for(dir: &std::path::Path, relay: &str, room: &str, key: Option<String>) -> Config {
    Config { data_dir: dir.to_path_buf(), relay_url: Some(relay.into()), token: None, room: room.into(), key, epoch_keys: Vec::new() }
}

/// Poll `f` until it returns `Some` (kicking both boxes' sync) or time runs out.
async fn until<T>(boxes: &[&Box_], secs: u64, mut f: impl AsyncFnMut() -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out");
        for b in boxes {
            b.kick();
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// Box A founds the workspace; B joins from A's signed invite link, as
/// `ligus enroll` would (pin, join invite, the key from the code).
async fn two_boxes() -> (Box_, Box_, C, tempfile::TempDir) {
    two_boxes_with(None).await
}

async fn two_boxes_with(b_ntfy: Option<ntfy::Ntfy>) -> (Box_, Box_, C, tempfile::TempDir) {
    two_boxes_opts(b_ntfy, |_| {}).await
}

async fn two_boxes_opts(b_ntfy: Option<ntfy::Ntfy>, b_opts: impl FnOnce(&mut ServeOptions)) -> (Box_, Box_, C, tempfile::TempDir) {
    two_boxes_on(spawn_relay().await, b_ntfy, b_opts).await
}

/// Wait until the relay holds A's invite `inv`, read through a scratch data
/// dir with B's keys. B's first sync must find it: A pushes on its own sync
/// loop, and a round that fails (a busy database) retries seconds later.
async fn relay_holds_invite(a: &Box_, b_cfg: &Config, probe: &std::path::Path, inv: &str) {
    let cfg = Config { data_dir: probe.to_path_buf(), ..b_cfg.clone() };
    std::fs::create_dir_all(probe).unwrap();
    let config_id = ligus_core::workspace_config_session_id(uuid_of_room(&cfg.room));
    let subject = format!("invite:{inv}");
    until(&[a], 60, async || {
        sync_once(&cfg).await.ok()?;
        let store = ligus_runtime::EventStore::open(probe.join("ligus.db")).ok()?;
        let events = store.list(config_id).ok()?;
        events
            .iter()
            .any(|e| matches!(&e.payload, ligus_core::SessionEvent::JoinApproved { subject: s, .. } if *s == subject))
            .then_some(())
    })
    .await;
}

async fn two_boxes_on(
    (relay, contacts): (String, C),
    b_ntfy: Option<ntfy::Ntfy>,
    b_opts: impl FnOnce(&mut ServeOptions),
) -> (Box_, Box_, C, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let room = format!("id-test-{}", Uuid::new_v4());
    let a = Box_::start(cfg_for(&tmp.path().join("a"), &relay, &room, Some("ws-passphrase".into())), "Ada").await;
    let inv = a.post("/v1/invites", json!({})).await;
    assert_eq!(inv["signed"], json!(true), "the founder's own box signs: {inv}");
    let code = inv["code"].as_str().unwrap().to_string();
    let conn = InviteConn::decode(&code).unwrap();
    assert_eq!(conn.founder_actor_id.as_deref(), Some(a.account().to_string().as_str()));
    let b_dir = tmp.path().join("b");
    std::fs::create_dir_all(&b_dir).unwrap();
    {
        let store = ligus_runtime::EventStore::open(b_dir.join("ligus.db")).unwrap();
        let ws = conn.workspace_id();
        store.set_join_invite(ws, Uuid::parse_str(conn.inv.as_deref().unwrap()).unwrap()).unwrap();
        store.set_join_redeem(ws, ligus_runtime::box_admission::redeem_key_of(&code).as_ref()).unwrap();
        store.set_workspace_pin(ws, conn.founder_actor_id.as_deref().unwrap(), conn.fk.as_deref().unwrap()).unwrap();
    }
    let mut b_cfg = cfg_for(&b_dir, &conn.relay_url, &conn.room, conn.key.clone());
    b_cfg.epoch_keys = conn.ek.clone();
    relay_holds_invite(&a, &b_cfg, &tmp.path().join("probe"), conn.inv.as_deref().unwrap()).await;
    let mut o = opts(&b_cfg.data_dir, "Bea");
    o.ntfy = b_ntfy;
    b_opts(&mut o);
    let handle = start(b_cfg, o).await.unwrap();
    let base = format!("http://127.0.0.1:{}", handle.addr.port());
    let token = handle.shared.operator_token();
    let b = Box_ { handle, base, token, client: ligus_forge::new_http_client() };
    (a, b, contacts, tmp)
}

/// Wait until each box knows the other as a co-member with a verified
/// device.
async fn know_each_other(a: &Box_, b: &Box_) {
    let (a_acc, b_acc) = (a.account(), b.account());
    until(&[a, b], 30, async || {
        let ab = a.handle.shared.co_members().iter().any(|c| c.account_id == b_acc && !c.devices.is_empty());
        let ba = b.handle.shared.co_members().iter().any(|c| c.account_id == a_acc && !c.devices.is_empty());
        (ab && ba).then_some(())
    })
    .await;
}

/// Wait until `a` knows `b`'s drop box (the contact id `b` announces once a
/// relay holds its record). Opening a DM delivers its invite there only when
/// it is known; before that the opener gets the link to share instead.
async fn knows_contact(a: &Box_, b: &Box_) {
    let b_acc = b.account();
    until(&[a, b], 30, async || {
        a.handle.shared.co_members().iter().any(|c| c.account_id == b_acc && !c.devices.is_empty() && c.contact.is_some()).then_some(())
    })
    .await;
}

/// The whole flow: safety codes on People, a DM through the drop box, a
/// message each way.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dm_through_the_drop_box_end_to_end() {
    let (a, b, contacts, _tmp) = two_boxes().await;
    let (a_acc, b_acc) = (a.account(), b.account());

    // A's profile: its own safety code (it holds its key), contacts on.
    let pa = a.get("/v1/profile").await;
    assert_eq!(pa["accountId"], json!(a_acc.to_string()));
    let a_code = pa["safetyCode"].as_str().unwrap().to_string();
    assert_eq!(a_code.split(' ').count(), 8);
    assert_eq!(pa["canFoundDm"], json!(true));

    // A sees B with B's safety code, a verified device and B's drop box.
    let b_code = b.get("/v1/profile").await["safetyCode"].as_str().unwrap().to_string();
    let a2 = &a;
    let person = until(&[&a, &b], 30, async || {
        let people = a2.get("/v1/people").await;
        let p = people["people"].as_array()?.iter().find(|p| p["accountId"] == json!(b_acc.to_string()))?.clone();
        let contact_known = a2.handle.shared.co_members().iter().any(|c| c.account_id == b_acc && c.contact.is_some());
        (p["devices"].as_u64() >= Some(1) && contact_known).then_some(p)
    })
    .await;
    assert_eq!(person["safetyCode"], json!(b_code));
    assert_eq!(person["keyConflict"], json!(false));
    assert_eq!(person["lookAlike"], json!(false));
    // A's own entry carries A's code; B sees A with the same code.
    let people = a.get("/v1/people").await;
    let me = people["people"].as_array().unwrap().iter().find(|p| p["isSelf"] == json!(true)).unwrap();
    assert_eq!(me["safetyCode"], json!(a_code));
    let b2 = &b;
    let seen_by_b = until(&[&a, &b], 20, async || {
        let people = b2.get("/v1/people").await;
        people["people"].as_array()?.iter().find(|p| p["accountId"] == json!(a_acc.to_string()))?["safetyCode"].as_str().map(str::to_string)
    })
    .await;
    assert_eq!(seen_by_b, a_code, "both sides compute the same code for A");

    // A opens a DM with B: a random room, a signed invite, delivered.
    let opened = a.post("/v1/dms", json!({ "accountId": b_acc.to_string() })).await;
    assert_eq!(opened["delivered"], json!(true), "{opened}");
    let invite = InviteConn::decode(opened["invite"].as_str().unwrap()).unwrap();
    assert!(invite.room.starts_with("dm-") && invite.room.len() == 35, "random room: {}", invite.room);
    assert_eq!(invite.dm_account.as_deref(), Some(b_acc.to_string().as_str()));
    assert!(invite.sig.is_some());
    // The workspace token never went to the relay with the contact calls
    // (none is configured here); nothing else did either.
    assert!(contacts.lock().unwrap().auth_seen.iter().all(|a| !a.to_ascii_lowercase().contains("github")));
    // Opening again lands in the same DM.
    let again = a.post("/v1/dms", json!({ "accountId": b_acc.to_string() })).await;
    assert_eq!(again["workspaceId"], opened["workspaceId"]);
    assert!(again.get("invite").is_none_or(Value::is_null));

    // B finds it in its drop box: from A, signed, acceptable; the code
    // itself stays on the box.
    let pending = until(&[&a, &b], 20, async || {
        let p = b2.get("/v1/invites/pending").await;
        (p["invites"].as_array().map(Vec::len) == Some(1)).then_some(p)
    })
    .await;
    assert_eq!(pending["contacts"], json!("on"));
    let inv = &pending["invites"][0];
    assert_eq!(inv["isDm"], json!(true));
    assert_eq!(inv["trust"], json!("known"), "{inv}");
    assert_eq!(inv["founderName"], json!("Ada"));
    assert_eq!(inv["safetyCode"], json!(a_code));
    assert_eq!(inv["acceptable"], json!(true));
    assert!(inv.get("code").is_none(), "the code stays on the box");
    let accepted = b.post("/v1/invites/pending/accept", json!({ "relay": inv["relay"], "seq": inv["seq"] })).await;
    assert_eq!(accepted["workspaceId"], opened["workspaceId"]);
    // The drop was handled: it is gone at the next read.
    let p2 = b.get("/v1/invites/pending").await;
    assert_eq!(p2["invites"].as_array().unwrap().len(), 0);

    // Both list the DM; B's chat appears once A's founding synced.
    let chat_b = until(&[&a, &b], 30, async || {
        let d = b2.get("/v1/dms").await;
        let row = d["dms"].as_array()?.first()?.clone();
        (row["waitingForFounder"] == json!(false) && row["chatId"].is_string()).then_some(row)
    })
    .await;
    assert_eq!(chat_b["account"], json!(a_acc.to_string()));
    assert_eq!(chat_b["name"], json!("Ada"));
    assert_eq!(chat_b["safetyCode"], json!(a_code));
    let chat_id = chat_b["chatId"].as_str().unwrap().to_string();
    assert_eq!(opened["chatId"].as_str(), Some(chat_id.as_str()));

    // The DM's chat is in the phone's chat list, marked as a DM and named
    // for the other person, and nowhere the workspace's chats are.
    let chats = b.get("/v1/chats").await;
    let row = chats["chats"].as_array().unwrap().iter().find(|c| c["id"] == json!(chat_id)).expect("DM chat listed").clone();
    assert_eq!(row["dm"]["account"], json!(a_acc.to_string()));
    assert_eq!(row["title"], json!("Ada"));
    let proposals = b.get("/v1/proposals").await;
    assert!(!proposals.to_string().contains(&chat_id));

    // B writes once it's a member; A reads it.
    let b_sent = until(&[&a, &b], 30, async || {
        let (code, v) = b2.call(reqwest::Method::POST, &format!("/v1/chats/{chat_id}/messages"), Some(json!({ "body": "hi Ada" }))).await;
        (code == 200).then_some(v)
    })
    .await;
    assert_eq!(b_sent["message"]["body"], json!("hi Ada"));
    let got = until(&[&a, &b], 30, async || {
        let m = a2.get(&format!("/v1/chats/{chat_id}/messages")).await;
        m["messages"].as_array()?.iter().find(|x| x["body"] == json!("hi Ada")).map(|x| x["author"].clone())
    })
    .await;
    assert_eq!(got, json!("Bea"));
    a.post(&format!("/v1/chats/{chat_id}/messages"), json!({ "body": "hello Bea" })).await;
    until(&[&a, &b], 30, async || {
        let m = b2.get(&format!("/v1/chats/{chat_id}/messages")).await;
        m["messages"].as_array()?.iter().any(|x| x["body"] == json!("hello Bea")).then_some(())
    })
    .await;
    // The person row links the DM.
    let people = b.get("/v1/people").await;
    let ada = people["people"].as_array().unwrap().iter().find(|p| p["accountId"] == json!(a_acc.to_string())).unwrap();
    assert_eq!(ada["dmChatId"], json!(chat_id));
}

/// R1 on the box: an invite that names a co-member and their safety code
/// without their device's signature only says it's from them, and a DM
/// invite like that is refused; so is one addressed to someone else, one
/// for a room the box holds, and a workspace invite (a box serves one).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forged_and_unsigned_invites_are_refused() {
    let (a, b, _contacts, _tmp) = two_boxes().await;
    let (a_acc, b_acc) = (a.account(), b.account());
    know_each_other(&a, &b).await;
    let a_fk = a.handle.shared.my_fingerprint().unwrap();
    let base = InviteConn {
        name: "Ada".into(),
        relay_url: a.handle.shared.cfg.relay_url.clone().unwrap(),
        room: ligus_runtime::dm::new_room(),
        key: Some("k".into()),
        icon: None,
        dm_account: Some(b_acc.to_string()),
        founder_actor_id: Some(a_acc.to_string()),
        fk: Some(a_fk.clone()),
        inv: Some(Uuid::new_v4().to_string()),
        ek: Vec::new(),
        sig: None,
    };
    // Unsigned: "claimed", refused.
    let info = b.post("/v1/invites/inspect", json!({ "code": base.encode() })).await;
    assert_eq!(info["trust"], json!("claimed"));
    assert_eq!(info["founderName"], json!("Ada"));
    assert_eq!(info["acceptable"], json!(false));
    let (code, v) = b.call(reqwest::Method::POST, "/v1/invites/join", Some(json!({ "code": base.encode(), "confirmSafetyCode": true }))).await;
    assert_eq!(code, 403, "{v}");
    assert!(b.handle.shared.dms.list().is_empty());

    // Signed by B's own device but naming A: not A's signature.
    let mut forged = base.clone();
    let identity = ligus_runtime::IdentityStore::new(&b.handle.shared.cfg.data_dir, ligus_runtime::secret_store::StoreKeyVault::for_data_dir(&b.handle.shared.cfg.data_dir));
    let cert = identity.load().unwrap().unwrap().device.certificate;
    let sig = {
        let svc = b.handle.shared.lock();
        // B's service refuses to sign for someone else...
        assert!(forged.with_claims(|c| svc.sign_invite(c, &cert)).is_none());
        // ...so forge one with B's device key directly.
        let device = identity.device_keypair(svc.device_id()).unwrap();
        let account = identity.account_keypair(b_acc).unwrap();
        forged.with_claims(|c| ligus_runtime::invite_sig::sign(c, 1, &device, &cert, &account.public_key_bytes()))
    };
    forged.sig = Some(sig);
    let info = b.post("/v1/invites/inspect", json!({ "code": forged.encode() })).await;
    assert_eq!(info["trust"], json!("claimed"), "another account's signature isn't A's: {info}");
    let (code, _) = b.call(reqwest::Method::POST, "/v1/invites/join", Some(json!({ "code": forged.encode() }))).await;
    assert_eq!(code, 403);

    // A co-member's id with another key: mismatch.
    let mismatch = InviteConn { fk: Some("ff".repeat(16)), ..base.clone() };
    let info = b.post("/v1/invites/inspect", json!({ "code": mismatch.encode() })).await;
    assert_eq!(info["trust"], json!("mismatch"));

    // Addressed to someone else (A's real, signed DM invite to B, read by A).
    let opened = a.post("/v1/dms", json!({ "accountId": b_acc.to_string() })).await;
    let real = opened["invite"].as_str().unwrap();
    let info = a.post("/v1/invites/inspect", json!({ "code": real })).await;
    assert_eq!(info["addressedToMe"], json!(false));
    assert_eq!(info["acceptable"], json!(false));
    // B can take the real one (pasted); a second time it's a room B holds.
    let info = b.post("/v1/invites/inspect", json!({ "code": real })).await;
    assert_eq!(info["trust"], json!("known"));
    b.post("/v1/invites/join", json!({ "code": real })).await;
    let (code, _) = b.call(reqwest::Method::POST, "/v1/invites/join", Some(json!({ "code": real }))).await;
    assert_eq!(code, 403, "a DM invite for a room already held is refused");

    // A workspace invite: inspected, never joined on a box.
    let ws_inv = a.post("/v1/invites", json!({})).await;
    let (code, v) = b.call(reqwest::Method::POST, "/v1/invites/join", Some(json!({ "code": ws_inv["code"] }))).await;
    assert_eq!(code, 409, "{v}");
    let info = b.post("/v1/invites/inspect", json!({ "code": ws_inv["code"] })).await;
    assert_eq!(info["alreadyJoined"], json!(true));
    // Junk.
    let (code, _) = b.call(reqwest::Method::POST, "/v1/invites/inspect", Some(json!({ "code": "hivews1:%%%" }))).await;
    assert_eq!(code, 400);
}

/// Waiting for the founder: a DM pinned to a founder whose signed founding
/// isn't here holds writes (409 `waiting_for_founder`, the draft is the
/// phone's to keep) and says so on the chat row and `…/founder`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_wait_for_the_founder() {
    let (a, b, _contacts, _tmp) = two_boxes().await;
    let b_acc = b.account();
    let a_acc = a.account();
    let b2 = &b;
    know_each_other(&a, &b).await;
    // A opens a DM.
    let opened = a.post("/v1/dms", json!({ "accountId": b_acc.to_string() })).await;
    let code = opened["invite"].as_str().unwrap().to_string();
    let ws: Uuid = serde_json::from_value(opened["workspaceId"].clone()).unwrap();
    // B joins at once: until A's signed founding reaches B, B's writes
    // there wait (and the row says so); then it clears on its own. B's DM
    // sync is held while we look, or the founding can land between the
    // reads.
    b.handle.shared.dm_sync_held.store(true, std::sync::atomic::Ordering::SeqCst);
    b.post("/v1/invites/join", json!({ "code": code })).await;
    let waiting = b.handle.shared.lock().waiting_for_founder(ws).unwrap();
    assert_eq!(waiting, Some(a_acc), "nothing of the DM has synced to B yet");
    let rows = b.get("/v1/dms").await;
    assert_eq!(rows["dms"][0]["waitingForFounder"], json!(true));
    // Once synced it clears on its own.
    b.handle.shared.dm_sync_held.store(false, std::sync::atomic::Ordering::SeqCst);
    until(&[&a, &b], 30, async || {
        let d = b2.get("/v1/dms").await;
        (d["dms"][0]["waitingForFounder"] == json!(false) && d["dms"][0]["chatId"].is_string()).then_some(())
    })
    .await;
    let chat_id = b.get("/v1/dms").await["dms"][0]["chatId"].as_str().unwrap().to_string();
    let f = b.get(&format!("/v1/chats/{chat_id}/founder")).await;
    assert_eq!(f["waiting"], json!(false));

}

/// The waiting state's wire form: 409 `waiting_for_founder` with the
/// founder's name, so the phone keeps the draft and says who it waits for.
#[tokio::test]
async fn waiting_for_founder_answer_shape() {
    use axum::response::IntoResponse;
    let r = super::api::ApiError::waiting_for_founder(Some("Ada".into())).into_response();
    assert_eq!(r.status().as_u16(), 409);
    let body = axum::body::to_bytes(r.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"], json!("waiting_for_founder"));
    assert_eq!(v["founderName"], json!("Ada"));
    assert!(v["message"].as_str().unwrap().contains("founder"));
    let r = super::api::ApiError::waiting_for_founder(None).into_response();
    let body = axum::body::to_bytes(r.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert!(v.get("founderName").is_none());
}

/// S3: names are cleaned everywhere the box shows them, and two people
/// whose names look alike are flagged (also someone whose name looks like
/// the box's own).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn names_are_cleaned_and_look_alikes_flagged() {
    let tmp = tempfile::tempdir().unwrap();
    let (relay, _c) = spawn_relay().await;
    let room = format!("names-{}", Uuid::new_v4());
    let a = Box_::start(cfg_for(&tmp.path().join("a"), &relay, &room, Some("p".into())), "Ada").await;
    let code = a.post("/v1/invites", json!({})).await["code"].as_str().unwrap().to_string();
    let inv = InviteConn::decode(&code).unwrap();
    let b_dir = tmp.path().join("b");
    std::fs::create_dir_all(&b_dir).unwrap();
    {
        let store = ligus_runtime::EventStore::open(b_dir.join("ligus.db")).unwrap();
        store.set_join_invite(inv.workspace_id(), Uuid::parse_str(inv.inv.as_deref().unwrap()).unwrap()).unwrap();
        store.set_join_redeem(inv.workspace_id(), ligus_runtime::box_admission::redeem_key_of(&code).as_ref()).unwrap();
        store.set_workspace_pin(inv.workspace_id(), inv.founder_actor_id.as_deref().unwrap(), inv.fk.as_deref().unwrap()).unwrap();
    }
    // B's first sync must find A's invite, or B starts as no member.
    let b_cfg = cfg_for(&b_dir, &relay, &room, Some("p".into()));
    relay_holds_invite(&a, &b_cfg, &tmp.path().join("probe"), inv.inv.as_deref().unwrap()).await;
    // B calls itself "Ada" with a Cyrillic a and a right-to-left override.
    let b = Box_::start(b_cfg, "\u{202E}\u{0430}da\u{200B}").await;
    let b_acc = b.account();
    let a2 = &a;
    let row = until(&[&a, &b], 30, async || {
        let people = a2.get("/v1/people").await;
        people["people"].as_array()?.iter().find(|p| p["accountId"] == json!(b_acc.to_string())).cloned()
    })
    .await;
    assert_eq!(row["displayName"], json!("\u{0430}da"), "bidi and zero-width removed");
    assert_eq!(row["lookAlike"], json!(true), "looks like the box's own name");
    let health = b.get("/v1/health").await;
    assert_eq!(health["identity"]["displayName"], json!("\u{0430}da"));
    let profile = b.get("/v1/profile").await;
    assert_eq!(profile["displayName"], json!("\u{0430}da"));
}

/// Linked identities: read-only, cached statuses only (this route never
/// asks a forge), and only for the box's own account and co-members.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn link_chips_are_read_only_and_scoped() {
    let (a, b, _contacts, _tmp) = two_boxes().await;
    let (a_acc, b_acc) = (a.account(), b.account());
    let a2 = &a;
    until(&[&a, &b], 30, async || a2.handle.shared.co_members().iter().any(|c| c.account_id == b_acc).then_some(())).await;
    assert_eq!(a.get(&format!("/v1/people/{b_acc}/links")).await, json!({ "links": [] }));
    assert_eq!(a.get(&format!("/v1/people/{a_acc}/links")).await, json!({ "links": [] }));
    let (code, _) = a.call(reqwest::Method::GET, &format!("/v1/people/{}/links", Uuid::new_v4()), None).await;
    assert_eq!(code, 404, "a stranger's links aren't served");
    let (code, _) = a.call(reqwest::Method::GET, "/v1/people/not-a-uuid/links", None).await;
    assert_eq!(code, 404);
    // Without the bearer token: nothing.
    let r = a.client.get(format!("{}/v1/people/{b_acc}/links", a.base)).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 401);
    for path in ["/v1/profile", "/v1/dms", "/v1/invites/pending"] {
        let r = a.client.get(format!("{}{path}", a.base)).send().await.unwrap();
        assert_eq!(r.status().as_u16(), 401, "{path}");
    }
    let r = a.client.post(format!("{}/v1/invites", a.base)).json(&json!({})).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

/// Presence: on a box acting for its owner, a bearer's invite would pass
/// for the owner's own, so it needs presence; the passkey path prepares
/// exactly the `JoinApproved` the plain route would write.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invites_need_presence_on_a_delegated_box() {
    let (a, _b, _contacts, _tmp) = two_boxes().await;
    let me = a.handle.shared.lock().author().clone();
    a.handle.shared.lock().act_as_owner_device(me);
    let (code, v) = a.call(reqwest::Method::POST, "/v1/invites", Some(json!({}))).await;
    assert_eq!((code, v["error"].clone()), (403, json!("presence_required")), "{v}");
    let prepared = a.post("/v1/presence/prepare", json!({ "action": "invite", "target": "workspace" })).await;
    assert_eq!(prepared["preview"]["kind"], json!("joinApproved"), "{prepared}");
    assert_eq!(prepared["preview"]["summary"], json!("Let someone join with a single-use invite"));
    assert!(prepared["challenge"].as_str().is_some_and(|c| c.len() == 43));
    // A made-up assertion is refused: a stamped assertion must verify, so a
    // bearer can't commit the invite without the owner's passkey.
    let before = a.handle.shared.lock().store().list(ligus_core::workspace_config_session_id(a.handle.shared.ws)).unwrap().len();
    let (code, v) = a
        .call(
            reqwest::Method::POST,
            "/v1/presence/commit",
            Some(json!({ "prepareId": prepared["prepareId"], "assertion": {
                "credentialId": "AAAA", "authenticatorData": "AAAA", "clientDataJSON": "AAAA", "signature": "AAAA" } })),
        )
        .await;
    assert!(code == 403 || code == 400 || code == 500, "{code} {v}");
    assert!(v.get("invite").is_none());
    let after = a.handle.shared.lock().store().list(ligus_core::workspace_config_session_id(a.handle.shared.ws)).unwrap().len();
    assert_eq!(before, after, "nothing was appended");
    // A delegated box can't found a DM.
    let (code, v) = a.call(reqwest::Method::POST, "/v1/dms", Some(json!({ "accountId": Uuid::new_v4().to_string() }))).await;
    assert!(code == 404 || code == 409, "{code} {v}");
    let p = a.get("/v1/profile").await;
    assert_eq!(p["canFoundDm"], json!(false));
}

/// The box's DMs are among the workspaces the foreign-workspace cleanup
/// keeps (a joined DM this box hasn't written in yet would otherwise go).
#[test]
fn dms_are_kept_by_the_cleanup() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = cfg_for(tmp.path(), "http://127.0.0.1:9", "main-room", None);
    let dms = dms::DmStore::load(tmp.path());
    let room = ligus_runtime::dm::new_room();
    dms.add(
        dms::DmConn {
            name: "x".into(),
            relay_url: "http://127.0.0.1:9".into(),
            room: room.clone(),
            peer: Uuid::new_v4().to_string(),
            founder_actor_id: Uuid::new_v4().to_string(),
            fk: "00".repeat(16),
        },
        "k",
        &[],
    )
    .unwrap();
    let keep = crate::kept_workspaces(&cfg).unwrap();
    assert!(keep.contains(&ligus_core::room_workspace_id(&room)));
    assert!(keep.contains(&ligus_core::room_workspace_id("main-room")));
    assert_eq!(keep.len(), 2);
    // An unreadable list is never taken as "no DMs": the cleanup would
    // otherwise delete every DM's history.
    std::fs::write(tmp.path().join("serve-dms.json"), "{ not json").unwrap();
    assert!(crate::kept_workspaces(&cfg).is_err());
    // No file at all: just the room.
    let empty = tempfile::tempdir().unwrap();
    let cfg = cfg_for(empty.path(), "http://127.0.0.1:9", "main-room", None);
    assert_eq!(crate::kept_workspaces(&cfg).unwrap().len(), 1);
}

/// DMs never push: a mention of Bea in a DM stays off her ntfy topic (its
/// text is between the two of them and her box), while the same mention in
/// a workspace chat is pushed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dm_messages_never_push() {
    let pushes: Arc<Mutex<Vec<String>>> = Arc::default();
    let p = pushes.clone();
    let router = Router::new().route(
        "/{topic}",
        axum::routing::post(move |body: String| {
            let p = p.clone();
            async move {
                p.lock().unwrap().push(body);
                "{}"
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let ntfy = ntfy::Ntfy { url: format!("http://{addr}"), topic: "bea".into(), token: None, titles: true };
    let (a, b, _contacts, _tmp) = two_boxes_with(Some(ntfy)).await;
    know_each_other(&a, &b).await;
    // The invite goes to B's drop box, so A must have seen B's contact id
    // first (B announces it after its first sync, which can trail the device
    // keys on a loaded machine).
    knows_contact(&a, &b).await;
    let b_acc = b.account();
    let opened = a.post("/v1/dms", json!({ "accountId": b_acc.to_string() })).await;
    assert_eq!(opened["delivered"], json!(true), "{opened}");
    let b2 = &b;
    let pending = until(&[&a, &b], 30, async || {
        let p = b2.get("/v1/invites/pending").await;
        p["invites"].as_array().and_then(|l| l.first().cloned())
    })
    .await;
    b.post("/v1/invites/pending/accept", json!({ "relay": pending["relay"], "seq": pending["seq"] })).await;
    let chat = opened["chatId"].as_str().unwrap().to_string();
    a.post(&format!("/v1/chats/{chat}/messages"), json!({ "body": "@Bea secret DM text" })).await;
    until(&[&a, &b], 30, async || {
        let (code, m) = b2.call(reqwest::Method::GET, &format!("/v1/chats/{chat}/messages"), None).await;
        (code == 200 && m.to_string().contains("secret DM text")).then_some(())
    })
    .await;
    // The same mention in a workspace chat.
    let ws_chat = a.post("/v1/chats", json!({ "title": "plans" })).await["chat"]["id"].as_str().unwrap().to_string();
    a.post(&format!("/v1/chats/{ws_chat}/messages"), json!({ "body": "@Bea workspace ping" })).await;
    let pushed = until(&[&a, &b], 30, async || {
        let all = pushes.lock().unwrap().join("\n");
        all.contains("plans").then_some(all)
    })
    .await;
    assert!(!pushed.contains("secret DM text"), "{pushed}");
    // The DM's chat is its #general: nothing was pushed for it.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let all = pushes.lock().unwrap().clone();
    assert!(all.iter().all(|x| x.contains("plans")), "only the workspace chat pushed: {all:?}");
}

/// Invite codes carry the workspace passphrase and epoch keys (a DM's too):
/// over plain HTTP on a network bind the box hands none out and takes none
/// in. The tunnel and a loopback bind are the secure channels (A, on
/// loopback, still works).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invite_secrets_never_over_plain_http() {
    let (a, b, _contacts, _tmp) = two_boxes_opts(None, |o| o.listen = "0.0.0.0:0".parse().unwrap()).await;
    know_each_other(&a, &b).await;
    let refused = |what: &str, code: u16, v: &Value| {
        assert_eq!(code, 403, "{what}: {v}");
        let msg = v["error"].as_str().unwrap_or_default();
        assert!(msg.contains("secure connection"), "{what}: the phone can show why: {v}");
    };
    // A fresh workspace invite: refused before one is issued.
    let (code, v) = b.call(reqwest::Method::POST, "/v1/invites", Some(json!({}))).await;
    refused("POST /v1/invites", code, &v);
    let (code, v) = b.call(reqwest::Method::POST, "/v1/presence/prepare", Some(json!({ "action": "invite", "target": "workspace" }))).await;
    refused("presence prepare invite", code, &v);
    // A code sent in (inspect, join) is refused too: it carries the key.
    let inv = a.post("/v1/invites", json!({})).await;
    let code_str = inv["code"].as_str().unwrap();
    let (code, v) = b.call(reqwest::Method::POST, "/v1/invites/inspect", Some(json!({ "code": code_str }))).await;
    refused("inspect", code, &v);
    let (code, v) = b.call(reqwest::Method::POST, "/v1/invites/join", Some(json!({ "code": code_str }))).await;
    refused("join", code, &v);
    // Founding a DM returns its invite: refused, and nothing was founded.
    let (code, v) = b.call(reqwest::Method::POST, "/v1/dms", Some(json!({ "accountId": a.account().to_string() }))).await;
    refused("POST /v1/dms (founding)", code, &v);
    assert!(b.handle.shared.dms.list().is_empty(), "no DM founded on a refused request");
    // A (loopback) founds it; B's view of the kept DM carries no secret and
    // still opens over plain HTTP.
    knows_contact(&a, &b).await;
    let opened = a.post("/v1/dms", json!({ "accountId": b.account().to_string() })).await;
    assert!(opened["invite"].is_string());
    assert_eq!(opened["delivered"], json!(true), "{opened}");
    let b2 = &b;
    let pending = until(&[&a, &b], 20, async || {
        let p = b2.get("/v1/invites/pending").await;
        (p["invites"].as_array().map(Vec::len) == Some(1)).then_some(p)
    })
    .await;
    let inv = &pending["invites"][0];
    assert!(inv.get("code").is_none());
    // Accepting by reference moves no code over the wire.
    b.post("/v1/invites/pending/accept", json!({ "relay": inv["relay"], "seq": inv["seq"] })).await;
    let a_acc = a.account();
    let again = until(&[&a, &b], 30, async || {
        let (code, v) = b2.call(reqwest::Method::POST, "/v1/dms", Some(json!({ "accountId": a_acc.to_string() }))).await;
        (code == 200).then_some(v)
    })
    .await;
    assert!(again.get("invite").is_none_or(Value::is_null), "{again}");
}

/// A DM opened before this box knows the peer's contact id: the invite is
/// held (sealed, on the box), reported as pending, and delivered once, only
/// once, when the id arrives.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dm_invite_held_until_the_contact_id_arrives_and_delivered_once() {
    let (relay, contacts) = spawn_relay().await;
    contacts.lock().unwrap().hold_records = true;
    let (a, b, contacts, _tmp) = two_boxes_on((relay, contacts), None, |_| {}).await;
    let b_acc = b.account();
    know_each_other(&a, &b).await;
    assert!(
        a.handle.shared.co_members().iter().any(|c| c.account_id == b_acc && c.contact.is_none()),
        "B has no contact id yet"
    );

    // Opened now: not delivered, but held.
    let opened = a.post("/v1/dms", json!({ "accountId": b_acc.to_string() })).await;
    assert_eq!(opened["delivered"], json!(false), "{opened}");
    assert_eq!(opened["pending"], json!(true), "{opened}");
    assert_eq!(opened["inviteDelivery"]["state"], json!("waiting"), "{opened}");
    let invite = opened["invite"].as_str().unwrap().to_string();
    let ws = opened["workspaceId"].as_str().unwrap().to_string();
    let row = a.get("/v1/dms").await["dms"].as_array().unwrap().iter().find(|d| d["workspaceId"] == json!(ws)).unwrap().clone();
    assert_eq!(row["inviteDelivery"]["pending"], json!(true), "{row}");

    // At rest: the held list and its secret hold no part of the invite.
    let a_dir = a.handle.shared.cfg.data_dir.clone();
    let mut held_files = vec![a_dir.join("held-dm-invites.json")];
    held_files.extend(std::fs::read_dir(a_dir.join("secrets").join("dm-invite")).unwrap().flatten().map(|e| e.path()));
    assert_eq!(held_files.len(), 2, "{held_files:?}");
    let conn = InviteConn::decode(&invite).unwrap();
    for f in &held_files {
        let text = String::from_utf8_lossy(&std::fs::read(f).unwrap()).to_string();
        for needle in [invite.as_str(), "hivews1:", conn.key.as_deref().unwrap(), conn.room.as_str()] {
            assert!(!text.contains(needle), "{} holds part of the invite", f.display());
        }
    }

    // B's relay takes records again: B publishes, announces, A learns the
    // id and delivers. B finds it in its drop box.
    contacts.lock().unwrap().hold_records = false;
    let b2 = &b;
    let pending = until(&[&a, &b], 60, async || {
        let p = b2.get("/v1/invites/pending").await;
        (p["invites"].as_array().map(Vec::len) == Some(1)).then_some(p)
    })
    .await;
    assert_eq!(pending["invites"][0]["isDm"], json!(true));
    assert_eq!(pending["invites"][0]["acceptable"], json!(true), "{pending}");
    let a2 = &a;
    let delivered = until(&[&a, &b], 30, async || {
        let d = a2.get("/v1/dms").await;
        let row = d["dms"].as_array()?.iter().find(|r| r["workspaceId"] == json!(ws))?.clone();
        (row["inviteDelivery"]["state"] == json!("delivered")).then_some(row)
    })
    .await;
    assert_eq!(delivered["inviteDelivery"]["delivered"], json!(true));
    assert_eq!(delivered["inviteDelivery"]["pending"], json!(false));

    // Never twice: more passes, B accepts, more passes.
    for _ in 0..5 {
        a.handle.shared.held_kick.notify_one();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let inv = &pending["invites"][0];
    let accepted = b.post("/v1/invites/pending/accept", json!({ "relay": inv["relay"], "seq": inv["seq"] })).await;
    assert_eq!(accepted["workspaceId"], json!(ws));
    for _ in 0..5 {
        a.kick();
        a.handle.shared.held_kick.notify_one();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let c = contacts.lock().unwrap();
    assert_eq!(c.posted.values().sum::<usize>(), 1, "one drop in all: {:?}", c.posted);
    drop(c);
    assert!(!a_dir.join("secrets").join("dm-invite").read_dir().unwrap().any(|_| true), "the sealed invite is forgotten once delivered");
}

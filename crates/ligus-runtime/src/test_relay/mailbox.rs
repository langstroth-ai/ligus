//! The approval mailbox routes of the mock relay
//! (`docs/design/code-match-wire.md` section 4), with every limit.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{request::Parts, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ligus_core::match_code::{self as mc, hex};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::TestClock;

/// Live requests per mailbox.
pub const MAX_LIVE: usize = 64;
/// Messages per request.
pub const MAX_MSGS: usize = 10;
/// Puts per client address per minute.
pub const PUTS_PER_MIN: usize = 10;
/// The member-auth window.
pub const AUTH_WINDOW_MS: i64 = 300_000;
const MAX_REQUEST_BLOB: usize = 8 * 1024;
const MAX_MSG_BLOB: usize = 32 * 1024;
const MAX_WAIT_SECS: u64 = 25;

struct Msg {
    seq: u64,
    blob: String,
    to: String,
    from: &'static str,
    at: i64,
}

struct Req {
    blob: String,
    read_key_hash: [u8; 32],
    put_at: i64,
    expires_at: i64,
    msgs: Vec<Msg>,
}

struct Mailbox {
    auth_pub: [u8; 32],
    cap_hash: [u8; 32],
    counter: u32,
    requests: BTreeMap<String, Req>,
    /// rid → until when it can't be put again (deleted or expired).
    spent: HashMap<String, i64>,
}

impl Mailbox {
    fn sweep(&mut self, now: i64) {
        let expired: Vec<String> = self.requests.iter().filter(|(_, r)| r.expires_at <= now).map(|(k, _)| k.clone()).collect();
        for rid in expired {
            if let Some(r) = self.requests.remove(&rid) {
                self.spent.insert(rid, r.expires_at + mc::REQUEST_TTL_MS);
            }
        }
        self.spent.retain(|_, until| *until > now);
    }
}

#[derive(Default)]
struct Inner {
    boxes: HashMap<String, Mailbox>,
    puts: HashMap<String, Vec<i64>>,
}

#[derive(Clone)]
struct St {
    inner: Arc<Mutex<Inner>>,
    notify: Arc<tokio::sync::Notify>,
    clock: TestClock,
}

pub(super) fn router(clock: TestClock) -> Router {
    let st = St { inner: Arc::default(), notify: Arc::new(tokio::sync::Notify::new()), clock };
    Router::new()
        .route("/v1/mailboxes/{mb}", get(get_mailbox).put(register))
        .route("/v1/mailboxes/{mb}/requests", get(list))
        .route("/v1/mailboxes/{mb}/requests/{rid}", put(put_request).delete(delete_request))
        .route("/v1/mailboxes/{mb}/requests/{rid}/msgs", get(get_msgs).post(post_msg))
        .with_state(st)
}

fn problem(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "error": code, "code": code, "message": code }))).into_response()
}

fn b64(s: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(s.as_bytes()).ok()
}

/// A blob is non-empty base64url.
fn blob_ok(blob: &str) -> bool {
    !blob.is_empty() && blob.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

fn fixed<const N: usize>(v: Option<&Value>) -> Option<[u8; N]> {
    b64(v?.as_str()?)?.try_into().ok()
}

async fn get_mailbox(State(st): State<St>, Path(mb): Path<String>) -> Response {
    let inner = st.inner.lock().unwrap();
    match inner.boxes.get(&mb) {
        Some(m) => Json(json!({ "mailbox": mb, "counter": m.counter })).into_response(),
        None => problem(StatusCode::NOT_FOUND, "mailbox_unknown"),
    }
}

async fn register(State(st): State<St>, Path(mb): Path<String>, Json(body): Json<Value>) -> Response {
    let (Some(auth_pub), Some(cap_hash), Some(counter), Some(sig)) = (
        fixed::<32>(body.get("authPub")),
        fixed::<32>(body.get("putCapHash")),
        body.get("counter").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()),
        body.get("sig").and_then(Value::as_str).and_then(b64),
    ) else {
        return problem(StatusCode::BAD_REQUEST, "invalid_request");
    };
    let mailbox = mc::mailbox_id(&auth_pub);
    if hex(&mailbox) != mb {
        return problem(StatusCode::BAD_REQUEST, "mailbox_mismatch");
    }
    if ligus_core::crypto::verify(&auth_pub, &mc::register_preimage(&mailbox, &cap_hash, counter), &sig).is_err() {
        return problem(StatusCode::FORBIDDEN, "bad_signature");
    }
    let mut inner = st.inner.lock().unwrap();
    let created = match inner.boxes.get_mut(&mb) {
        None => {
            inner.boxes.insert(mb.clone(), Mailbox { auth_pub, cap_hash, counter, requests: BTreeMap::new(), spent: HashMap::new() });
            true
        }
        Some(m) if counter == m.counter && cap_hash == m.cap_hash => false,
        Some(m) if counter > m.counter => {
            m.counter = counter;
            m.cap_hash = cap_hash;
            false
        }
        Some(_) => return problem(StatusCode::CONFLICT, "stale_counter"),
    };
    let status = if created { StatusCode::CREATED } else { StatusCode::OK };
    (status, Json(json!({ "mailbox": mb, "counter": counter }))).into_response()
}

/// Who a call is: a member (signed by the mailbox auth key, within the
/// window) or the requester of `rid` (its read key).
enum Caller {
    Member,
    Requester,
}

fn member_ok(m: &Mailbox, headers: &HeaderMap, method: &str, path: &str, body: &[u8], now: i64) -> Option<bool> {
    let h = headers.get("x-ligus-mailbox-auth")?.to_str().ok()?;
    let (ts, sig) = h.split_once('.')?;
    let ts: i64 = ts.parse().ok()?;
    let sig = b64(sig)?;
    if (now - ts).abs() > AUTH_WINDOW_MS {
        return Some(false);
    }
    Some(ligus_core::crypto::verify(&m.auth_pub, &mc::member_preimage(method, path, ts, body), &sig).is_ok())
}

fn read_key_ok(r: &Req, headers: &HeaderMap) -> Option<bool> {
    let k = b64(headers.get("x-ligus-read-key")?.to_str().ok()?)?;
    Some(<[u8; 32]>::from(Sha256::digest(&k)) == r.read_key_hash)
}

/// The caller of a call on `rid`, or the refusal.
fn caller(m: &Mailbox, rid: &str, parts: &Parts, body: &[u8], now: i64) -> Result<Caller, (StatusCode, &'static str)> {
    let method = parts.method.as_str();
    let path = parts.uri.path();
    if let Some(ok) = member_ok(m, &parts.headers, method, path, body, now) {
        return if ok { Ok(Caller::Member) } else { Err((StatusCode::UNAUTHORIZED, "bad_member_auth")) };
    }
    let Some(r) = m.requests.get(rid) else { return Err((StatusCode::NOT_FOUND, "request_unknown")) };
    match read_key_ok(r, &parts.headers) {
        Some(true) => Ok(Caller::Requester),
        _ => Err((StatusCode::UNAUTHORIZED, "bad_read_key")),
    }
}

fn client_of(parts: &Parts) -> String {
    parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip().to_string()).unwrap_or_else(|| "local".into())
}

async fn put_request(State(st): State<St>, Path((mb, rid)): Path<(String, String)>, parts: Parts, body: Bytes) -> Response {
    let now = st.clock.now();
    let mut inner = st.inner.lock().unwrap();
    let client = client_of(&parts);
    let log = inner.puts.entry(client).or_default();
    log.retain(|t| now - *t < 60_000);
    if log.len() >= PUTS_PER_MIN {
        let wait = (60_000 - (now - log[0])) / 1000 + 1;
        let mut r = problem(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
        r.headers_mut().insert("retry-after", wait.to_string().parse().unwrap());
        return r;
    }
    log.push(now);
    let Some(m) = inner.boxes.get_mut(&mb) else { return problem(StatusCode::NOT_FOUND, "mailbox_unknown") };
    m.sweep(now);
    let cap_ok = parts
        .headers
        .get("x-ligus-put-cap")
        .and_then(|h| h.to_str().ok())
        .and_then(b64)
        .is_some_and(|c| <[u8; 32]>::from(Sha256::digest(&c)) == m.cap_hash);
    if !cap_ok {
        return problem(StatusCode::FORBIDDEN, "bad_put_cap");
    }
    let Ok(v) = serde_json::from_slice::<Value>(&body) else { return problem(StatusCode::BAD_REQUEST, "invalid_request") };
    let (Some(blob), Some(read_key_hash)) = (v.get("blob").and_then(Value::as_str), fixed::<32>(v.get("readKeyHash"))) else {
        return problem(StatusCode::BAD_REQUEST, "invalid_request");
    };
    if !blob_ok(blob) {
        return problem(StatusCode::BAD_REQUEST, "invalid_request");
    }
    if blob.len() > MAX_REQUEST_BLOB {
        return problem(StatusCode::PAYLOAD_TOO_LARGE, "too_large");
    }
    if rid.len() != 32 || mc::unhex(&rid).is_none() {
        return problem(StatusCode::BAD_REQUEST, "invalid_request");
    }
    if m.requests.contains_key(&rid) || m.spent.contains_key(&rid) {
        return problem(StatusCode::CONFLICT, "rid_exists");
    }
    if m.requests.len() >= MAX_LIVE {
        return problem(StatusCode::TOO_MANY_REQUESTS, "mailbox_full");
    }
    let expires_at = now + mc::REQUEST_TTL_MS;
    m.requests.insert(rid.clone(), Req { blob: blob.to_string(), read_key_hash, put_at: now, expires_at, msgs: Vec::new() });
    (StatusCode::CREATED, Json(json!({ "rid": rid, "putAt": now, "expiresAt": expires_at }))).into_response()
}

async fn list(State(st): State<St>, Path(mb): Path<String>, parts: Parts) -> Response {
    let now = st.clock.now();
    let mut inner = st.inner.lock().unwrap();
    let Some(m) = inner.boxes.get_mut(&mb) else { return problem(StatusCode::NOT_FOUND, "mailbox_unknown") };
    m.sweep(now);
    if member_ok(m, &parts.headers, "GET", parts.uri.path(), b"", now) != Some(true) {
        return problem(StatusCode::UNAUTHORIZED, "bad_member_auth");
    }
    let mut rows: Vec<(&String, &Req)> = m.requests.iter().collect();
    rows.sort_by_key(|(rid, r)| (r.put_at, (*rid).clone()));
    let requests: Vec<Value> = rows
        .into_iter()
        .map(|(rid, r)| json!({ "rid": rid, "blob": r.blob, "putAt": r.put_at, "expiresAt": r.expires_at, "msgCount": r.msgs.len() }))
        .collect();
    Json(json!({ "requests": requests })).into_response()
}

async fn post_msg(State(st): State<St>, Path((mb, rid)): Path<(String, String)>, parts: Parts, body: Bytes) -> Response {
    let now = st.clock.now();
    let mut inner = st.inner.lock().unwrap();
    let Some(m) = inner.boxes.get_mut(&mb) else { return problem(StatusCode::NOT_FOUND, "mailbox_unknown") };
    m.sweep(now);
    let who = match caller(m, &rid, &parts, &body, now) {
        Ok(c) => c,
        Err((status, code)) => return problem(status, code),
    };
    let Some(r) = m.requests.get_mut(&rid) else { return problem(StatusCode::NOT_FOUND, "request_unknown") };
    let Ok(v) = serde_json::from_slice::<Value>(&body) else { return problem(StatusCode::BAD_REQUEST, "invalid_request") };
    let (Some(blob), Some(to)) = (v.get("blob").and_then(Value::as_str), v.get("to").and_then(Value::as_str)) else {
        return problem(StatusCode::BAD_REQUEST, "invalid_request");
    };
    if !matches!(to, "members" | "requester" | "approver") {
        return problem(StatusCode::BAD_REQUEST, "invalid_request");
    }
    if !blob_ok(blob) {
        return problem(StatusCode::BAD_REQUEST, "invalid_request");
    }
    if blob.len() > MAX_MSG_BLOB {
        return problem(StatusCode::PAYLOAD_TOO_LARGE, "too_large");
    }
    if r.msgs.len() >= MAX_MSGS {
        return problem(StatusCode::TOO_MANY_REQUESTS, "too_many_messages");
    }
    let seq = r.msgs.len() as u64 + 1;
    let from = match who {
        Caller::Member => "member",
        Caller::Requester => "requester",
    };
    r.msgs.push(Msg { seq, blob: blob.to_string(), to: to.to_string(), from, at: now });
    drop(inner);
    st.notify.notify_waiters();
    (StatusCode::CREATED, Json(json!({ "seq": seq }))).into_response()
}

#[derive(serde::Deserialize)]
struct MsgQuery {
    #[serde(default)]
    after: u64,
    #[serde(default)]
    wait: u64,
}

async fn get_msgs(State(st): State<St>, Path((mb, rid)): Path<(String, String)>, Query(q): Query<MsgQuery>, parts: Parts) -> Response {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(q.wait.min(MAX_WAIT_SECS));
    loop {
        let notified = st.notify.notified();
        {
            let now = st.clock.now();
            let mut inner = st.inner.lock().unwrap();
            let Some(m) = inner.boxes.get_mut(&mb) else { return problem(StatusCode::NOT_FOUND, "mailbox_unknown") };
            m.sweep(now);
            if let Err((status, code)) = caller(m, &rid, &parts, b"", now) {
                return problem(status, code);
            }
            let Some(r) = m.requests.get(&rid) else { return problem(StatusCode::NOT_FOUND, "request_unknown") };
            let msgs: Vec<Value> = r
                .msgs
                .iter()
                .filter(|m| m.seq > q.after)
                .map(|m| json!({ "seq": m.seq, "blob": m.blob, "to": m.to, "from": m.from, "at": m.at }))
                .collect();
            if !msgs.is_empty() || tokio::time::Instant::now() >= deadline {
                return Json(json!({ "msgs": msgs })).into_response();
            }
        }
        let _ = tokio::time::timeout_at(deadline, notified).await;
    }
}

async fn delete_request(State(st): State<St>, Path((mb, rid)): Path<(String, String)>, parts: Parts) -> Response {
    let now = st.clock.now();
    let mut inner = st.inner.lock().unwrap();
    let Some(m) = inner.boxes.get_mut(&mb) else { return problem(StatusCode::NOT_FOUND, "mailbox_unknown") };
    m.sweep(now);
    let Some(r) = m.requests.get(&rid) else { return problem(StatusCode::NOT_FOUND, "request_unknown") };
    if read_key_ok(r, &parts.headers) != Some(true) {
        return problem(StatusCode::UNAUTHORIZED, "bad_read_key");
    }
    let r = m.requests.remove(&rid).expect("present");
    m.spent.insert(rid, r.expires_at + mc::REQUEST_TTL_MS);
    drop(inner);
    st.notify.notify_waiters();
    StatusCode::NO_CONTENT.into_response()
}

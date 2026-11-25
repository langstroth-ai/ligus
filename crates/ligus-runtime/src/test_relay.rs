//! In-memory mock relay for ligus-runtime tests.
//!
//! Speaks the subset of the hrt1 `/v1` surface the sync-engine and relay-client
//! tests exercise — envelope push/fetch, pairing codes, and keyring rotations —
//! so those tests keep validating the client and the E2EE guarantees without
//! depending on a separate relay crate/binary. It's **content-blind**: it stores
//! whatever opaque JSON body it's handed, verbatim, exactly as a real relay does.
//!
//! `GET /envelopes` pages like the real relay: at most `limit` rows (clamped to
//! the router's page cap, which is also the default), with `X-Hive-Next-After`
//! and, when rows remain, `X-Hive-More: true`. [`router_with_page_cap`] sets a
//! tiny cap so tests cross page boundaries with a handful of envelopes.
//!
//! The blob channel (upload, download, delete and the 0.4 retain route) is
//! here too, with the relay's expiry rule: a blob expires after the
//! retention, a renewal moves that to now plus the retention but never past
//! its upload plus the maximum age. [`router_with_blobs`] hands back a
//! [`MockBlobs`] to look at and age the stored blobs; `retain: false` is a
//! relay older than the retain route.
//!
//! Deliberately omitted: the SSE `/events` stream (the client degrades to polling
//! on 404, which the tests rely on) and everything social/directory/admin, which
//! no test here touches.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

#[derive(Default)]
struct MockState {
    /// workspace → pushed bodies; the server sequence is the 1-based index.
    envelopes: HashMap<String, Vec<Value>>,
    /// workspace → published key rotations (opaque).
    keyrings: HashMap<String, Vec<Value>>,
    /// workspace → device slot → presence blob (overwritten in place).
    presence: HashMap<String, HashMap<String, Value>>,
    /// pairing code → opaque payload.
    pairings: HashMap<String, String>,
    pair_seq: u64,
    /// Most envelopes one `GET /envelopes` returns (and the default limit).
    page_cap: usize,
    /// How many `GET /envelopes` requests were served (paging assertions).
    envelope_gets: usize,
    /// (workspace, blob id) → stored blob.
    blobs: HashMap<(String, String), MockBlob>,
    blob_seq: u64,
    /// Retain requests served, by blob id (any answer).
    retain_calls: Vec<String>,
}

/// One stored blob (unix seconds; `expires_at` 0 = never).
#[derive(Clone, Debug)]
pub struct MockBlob {
    pub data: Vec<u8>,
    pub created_at: i64,
    pub expires_at: i64,
}

/// The real relay's blob defaults.
pub const BLOB_RETENTION_SECS: i64 = 30 * 24 * 60 * 60;
pub const BLOB_MAX_AGE_SECS: i64 = 365 * 24 * 60 * 60;

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// The real relay's default and maximum page size.
pub const DEFAULT_PAGE_CAP: usize = 1000;

type Shared = Arc<Mutex<MockState>>;

/// A fresh mock relay router with its own in-memory state.
pub fn router() -> Router {
    router_with_page_cap(DEFAULT_PAGE_CAP)
}

/// A mock relay whose envelope pages hold at most `page_cap` rows.
pub fn router_with_page_cap(page_cap: usize) -> Router {
    router_with_stats(page_cap).0
}

/// Like [`router_with_page_cap`], plus a counter of `GET /envelopes` requests.
pub fn router_with_stats(page_cap: usize) -> (Router, EnvelopeGets) {
    let state: Shared = Arc::new(Mutex::new(MockState { page_cap: page_cap.max(1), ..Default::default() }));
    let gets = EnvelopeGets(state.clone());
    (routes(state, true), gets)
}

/// A mock relay with a handle on its blobs; `retain: false` leaves out the
/// retain route (a relay from before Ligus 0.4: plain `404`).
pub fn router_with_blobs(retain: bool) -> (Router, MockBlobs) {
    let state: Shared = Arc::new(Mutex::new(MockState { page_cap: DEFAULT_PAGE_CAP, ..Default::default() }));
    (routes(state.clone(), retain), MockBlobs(state))
}

fn routes(state: Shared, retain: bool) -> Router {
    let mut router = Router::new()
        .route("/v1/workspaces/{ws}/blobs", post(post_blob))
        .route("/v1/workspaces/{ws}/blobs/{id}", get(get_blob).delete(delete_blob));
    if retain {
        router = router.route("/v1/workspaces/{ws}/blobs/{id}/retain", post(retain_blob));
    }
    router
        .route("/v1/health", get(|| async { "ok" }))
        .route(
            "/v1/workspaces/{ws}/envelopes",
            post(push_envelope).get(list_envelopes),
        )
        .route(
            "/v1/workspaces/{ws}/keyring",
            post(push_keyring).get(list_keyring),
        )
        .route("/v1/workspaces/{ws}/presence", post(put_presence).get(list_presence))
        .route("/v1/pair", post(create_pair))
        .route("/v1/pair/{code}", get(resolve_pair))
        .with_state(state)
}

/// Looks at and ages a mock relay's blobs.
#[derive(Clone)]
pub struct MockBlobs(Shared);

impl MockBlobs {
    pub fn get(&self, ws: &str, id: &str) -> Option<MockBlob> {
        self.0.lock().unwrap().blobs.get(&(ws.to_string(), id.to_string())).cloned()
    }
    /// Rewrite a blob's upload and expiry times.
    pub fn set_times(&self, ws: &str, id: &str, created_at: i64, expires_at: i64) {
        if let Some(b) = self.0.lock().unwrap().blobs.get_mut(&(ws.to_string(), id.to_string())) {
            b.created_at = created_at;
            b.expires_at = expires_at;
        }
    }
    /// The blob ids retain was asked about, in order.
    pub fn retain_calls(&self) -> Vec<String> {
        self.0.lock().unwrap().retain_calls.clone()
    }
    /// How many blobs the relay holds (expired ones included until asked).
    pub fn count(&self) -> usize {
        self.0.lock().unwrap().blobs.len()
    }
}

fn blob_not_found() -> axum::response::Response {
    use axum::response::IntoResponse;
    (StatusCode::NOT_FOUND, Json(json!({"error": "blob_not_found", "message": "no such blob"}))).into_response()
}

/// A live blob of `ws` (expired ones answer as absent).
fn live_blob<'a>(st: &'a mut MockState, ws: &str, id: &str, now: i64) -> Option<&'a mut MockBlob> {
    st.blobs.get_mut(&(ws.to_string(), id.to_string())).filter(|b| b.expires_at == 0 || b.expires_at > now)
}

async fn post_blob(State(st): State<Shared>, Path(ws): Path<String>, body: axum::body::Bytes) -> axum::response::Response {
    use axum::response::IntoResponse;
    let mut st = st.lock().unwrap();
    st.blob_seq += 1;
    // 43 characters of base64url, like the relay's ids.
    let id = format!("{:0>43}", format!("mockblob{}", st.blob_seq));
    let now = unix_now();
    let b = MockBlob { data: body.to_vec(), created_at: now, expires_at: now + BLOB_RETENTION_SECS };
    let size = b.data.len();
    let expires_at = b.expires_at;
    st.blobs.insert((ws, id.clone()), b);
    (StatusCode::CREATED, Json(json!({"id": id, "size": size, "sha256": "", "expiresAt": expires_at}))).into_response()
}

async fn get_blob(State(st): State<Shared>, Path((ws, id)): Path<(String, String)>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let mut st = st.lock().unwrap();
    match live_blob(&mut st, &ws, &id, unix_now()) {
        Some(b) => (StatusCode::OK, b.data.clone()).into_response(),
        None => blob_not_found(),
    }
}

async fn delete_blob(State(st): State<Shared>, Path((ws, id)): Path<(String, String)>) -> StatusCode {
    st.lock().unwrap().blobs.remove(&(ws, id));
    StatusCode::NO_CONTENT
}

/// The relay's rule: now plus the retention, never past the upload plus the
/// maximum age, never earlier than it was; a never-expiring blob stays so.
async fn retain_blob(State(st): State<Shared>, Path((ws, id)): Path<(String, String)>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let mut st = st.lock().unwrap();
    st.retain_calls.push(id.clone());
    let now = unix_now();
    let Some(b) = live_blob(&mut st, &ws, &id, now) else {
        return blob_not_found();
    };
    if b.expires_at != 0 {
        let next = (now + BLOB_RETENTION_SECS).min(b.created_at + BLOB_MAX_AGE_SECS);
        b.expires_at = b.expires_at.max(next);
    }
    (StatusCode::OK, Json(json!({"id": id, "expiresAt": b.expires_at}))).into_response()
}

/// Reads how many `GET /envelopes` requests a mock relay has served.
#[derive(Clone)]
pub struct EnvelopeGets(Shared);

impl EnvelopeGets {
    pub fn count(&self) -> usize {
        self.0.lock().unwrap().envelope_gets
    }
}

async fn push_envelope(
    State(st): State<Shared>,
    Path(ws): Path<String>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let mut st = st.lock().unwrap();
    let list = st.envelopes.entry(ws).or_default();
    list.push(body);
    Json(json!({ "seq": list.len() as u64 }))
}

#[derive(serde::Deserialize)]
struct AfterQuery {
    #[serde(default)]
    after: u64,
    #[serde(default)]
    limit: Option<usize>,
}

async fn list_envelopes(
    State(st): State<Shared>,
    Path(ws): Path<String>,
    Query(q): Query<AfterQuery>,
) -> impl axum::response::IntoResponse {
    let mut st = st.lock().unwrap();
    st.envelope_gets += 1;
    let cap = st.page_cap;
    let limit = q.limit.filter(|n| *n > 0).map_or(cap, |n| n.min(cap));
    let tail: Vec<(u64, &Value)> = st
        .envelopes
        .get(&ws)
        .map(|list| {
            list.iter()
                .enumerate()
                .map(|(i, body)| (i as u64 + 1, body))
                .filter(|(seq, _)| *seq > q.after)
                .collect()
        })
        .unwrap_or_default();
    let more = tail.len() > limit;
    let page: Vec<Value> = tail
        .iter()
        .take(limit)
        .map(|(seq, body)| json!({ "seq": seq, "body": body }))
        .collect();
    let next = tail.get(limit.min(tail.len()).wrapping_sub(1)).map_or(q.after, |(seq, _)| *seq);
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-hive-next-after", next.to_string().parse().unwrap());
    if more {
        headers.insert("x-hive-more", "true".parse().unwrap());
    }
    (headers, Json(Value::Array(page)))
}

#[derive(serde::Deserialize)]
struct PresenceReq {
    device_id: String,
    data: Value,
}

async fn put_presence(State(st): State<Shared>, Path(ws): Path<String>, Json(req): Json<PresenceReq>) -> Json<Value> {
    st.lock().unwrap().presence.entry(ws).or_default().insert(req.device_id, req.data);
    Json(json!({ "ok": true }))
}

async fn list_presence(State(st): State<Shared>, Path(ws): Path<String>) -> Json<Value> {
    let map = st.lock().unwrap().presence.get(&ws).cloned().unwrap_or_default();
    Json(json!(map))
}

async fn push_keyring(
    State(st): State<Shared>,
    Path(ws): Path<String>,
    Json(body): Json<Value>,
) -> Json<Value> {
    st.lock().unwrap().keyrings.entry(ws).or_default().push(body);
    Json(json!({ "ok": true }))
}

async fn list_keyring(State(st): State<Shared>, Path(ws): Path<String>) -> Json<Value> {
    let rows = st.lock().unwrap().keyrings.get(&ws).cloned().unwrap_or_default();
    Json(Value::Array(rows))
}

#[derive(serde::Deserialize)]
struct PairReq {
    payload: String,
    #[serde(default)]
    ttl_secs: Option<u64>,
}

async fn create_pair(State(st): State<Shared>, Json(req): Json<PairReq>) -> Json<Value> {
    let mut st = st.lock().unwrap();
    st.pair_seq += 1;
    // Codes are stored uppercase; resolve normalizes too (the real relay is
    // case/separator tolerant). Echo the requested TTL back like the relay does.
    let code = format!("PAIR{:04}", st.pair_seq);
    let expires_in = req.ttl_secs.unwrap_or(900);
    st.pairings.insert(code.clone(), req.payload);
    Json(json!({ "code": code, "expires_in": expires_in }))
}

async fn resolve_pair(
    State(st): State<Shared>,
    Path(code): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    match st.lock().unwrap().pairings.get(&code.to_uppercase()) {
        Some(payload) => Ok(Json(json!({ "payload": payload }))),
        None => Err(StatusCode::NOT_FOUND),
    }
}

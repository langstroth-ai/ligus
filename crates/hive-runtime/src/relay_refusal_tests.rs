//! The client against a fake relay that refuses (`429`/`507` with
//! `{error, message, retryAfter}` and `Retry-After`): what each refusal makes
//! the relay client and the sync engine do.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::IntoResponse;
use hive_core::{ChatMessage, ChatSession, MessageRole, SessionEvent, SessionEventEnvelope};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::event_store::EventStore;
use crate::relay_client::{RelayClient, RelayError};
use crate::relay_refusal::RefusalClass;
use crate::sync_engine::{SyncAttention, SyncEngine};
use crate::SseConnectError;

/// One recorded request.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    query: String,
    idem: Option<String>,
    body: Vec<u8>,
}

/// A scripted answer: status, `Retry-After`, body.
type Answer = (u16, Option<&'static str>, Value);

#[derive(Default)]
struct Script {
    /// Answers for `POST …/envelopes`, used in order; when empty, a 200.
    push: VecDeque<Answer>,
    /// Same for `GET …/envelopes`; when empty, `rows`.
    fetch: VecDeque<Answer>,
    /// Same for every other route; when empty, `200 []`.
    other: VecDeque<Answer>,
    /// What an unscripted `GET …/envelopes?after=0` returns.
    rows: Vec<Value>,
    seen: Vec<Seen>,
    next_seq: u64,
}

#[derive(Clone)]
struct Fake(Arc<Mutex<Script>>);

impl Fake {
    fn seen(&self, method: &str, path_part: &str) -> Vec<Seen> {
        self.0.lock().unwrap().seen.iter().filter(|s| s.method == method && s.path.contains(path_part)).cloned().collect()
    }
}

fn refusal(status: u16, code: &str, retry: &'static str) -> Answer {
    (status, Some(retry), json!({ "error": code, "message": format!("{code} refused"), "retryAfter": retry.parse::<u64>().unwrap() }))
}

async fn spawn(script: Script) -> (String, Fake) {
    let fake = Fake(Arc::new(Mutex::new(script)));
    let f = fake.clone();
    let router = axum::Router::new().fallback(move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
        let f = f.clone();
        async move {
            let mut s = f.0.lock().unwrap();
            let path = uri.path().to_string();
            s.seen.push(Seen {
                method: method.to_string(),
                path: path.clone(),
                query: uri.query().unwrap_or_default().to_string(),
                idem: headers.get("idempotency-key").and_then(|v| v.to_str().ok()).map(str::to_owned),
                body: body.to_vec(),
            });
            let envelopes = path.ends_with("/envelopes");
            let scripted = if envelopes && method == Method::POST {
                s.push.pop_front()
            } else if envelopes && method == Method::GET {
                s.fetch.pop_front()
            } else {
                s.other.pop_front()
            };
            let (status, retry, body) = match scripted {
                Some(a) => a,
                None if envelopes && method == Method::POST => {
                    s.next_seq += 1;
                    (200, None, json!({ "seq": s.next_seq }))
                }
                None if envelopes && uri.query().unwrap_or_default().contains("after=0") => (200, None, Value::Array(s.rows.clone())),
                None => (200, None, json!([])),
            };
            let mut resp = (StatusCode::from_u16(status).unwrap(), axum::Json(body)).into_response();
            if let Some(r) = retry {
                resp.headers_mut().insert("retry-after", r.parse().unwrap());
            }
            resp
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{addr}"), fake)
}

/// A store holding one chat (its snapshot and a message): two envelopes.
fn store_with_chat() -> (EventStore, Uuid, Uuid) {
    let mut store = EventStore::open_in_memory().unwrap();
    let session = ChatSession::new("Shared", Uuid::new_v4(), "anthropic");
    let (sid, wid) = (session.id, session.workspace_id);
    store.ingest_next(&SessionEventEnvelope::new(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(session) })).unwrap();
    store
        .ingest_next(&SessionEventEnvelope::new(
            sid,
            wid,
            2,
            SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "Mara", "hi") },
        ))
        .unwrap();
    (store, sid, wid)
}

fn engine(base: &str) -> SyncEngine {
    SyncEngine::unscoped(RelayClient::new(base), Uuid::new_v4().to_string()).with_key(hive_core::derive_workspace_key("refusals"))
}

async fn round(eng: &mut SyncEngine, store: &mut EventStore) -> Result<usize, crate::SyncError> {
    let batch = eng.take_unpushed(store)?;
    eng.push_envelopes(&batch).await?;
    eng.pull_round(store).await
}

/// A rate limit waits `Retry-After` and sends the same request again: the
/// same sealed bytes and the same idempotency key, so the relay can't store
/// it twice; the outbox empties and nothing is left paused.
#[tokio::test]
async fn a_rate_limited_push_is_retried_in_place_with_the_same_body_and_key() {
    let (base, fake) = spawn(Script { push: [refusal(429, "write_rate", "1")].into(), ..Default::default() }).await;
    let (mut store, _, _) = store_with_chat();
    let mut eng = engine(&base);
    let started = std::time::Instant::now();
    round(&mut eng, &mut store).await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(900), "waited Retry-After");
    let posts = fake.seen("POST", "/envelopes");
    assert_eq!(posts.len(), 3, "the refused push again, then the second envelope");
    assert_eq!(posts[0].body, posts[1].body, "the retry is the same request");
    assert!(posts[0].idem.is_some());
    assert_eq!(posts[0].idem, posts[1].idem);
    assert_ne!(posts[1].idem, posts[2].idem, "one key per event");
    assert!(eng.attention().is_none());
    assert!(eng.take_unpushed(&store).unwrap().is_empty(), "the outbox drained");
}

/// The key is derived from the event id (stable across re-seals and
/// restarts) and doesn't reveal it.
#[test]
fn the_idempotency_key_is_stable_and_reveals_no_event_id() {
    let id = Uuid::new_v4();
    let k = crate::relay_client::idempotency_key(id);
    assert_eq!(k, crate::relay_client::idempotency_key(id));
    assert_ne!(k, crate::relay_client::idempotency_key(Uuid::new_v4()));
    assert_eq!(k.len(), 32);
    assert!(!k.contains(&id.simple().to_string()) && !k.contains(&id.to_string()));
}

/// Unknown codes go by status: an unknown `429` is retried like a rate limit.
#[tokio::test]
async fn an_unknown_429_is_a_rate_limit() {
    let (base, fake) = spawn(Script { push: [refusal(429, "some_future_limit", "1")].into(), ..Default::default() }).await;
    let (mut store, _, _) = store_with_chat();
    let mut eng = engine(&base);
    round(&mut eng, &mut store).await.unwrap();
    assert_eq!(fake.seen("POST", "/envelopes").len(), 3);
    assert!(eng.attention().is_none());
}

/// A rate limit longer than a request may sleep ends the round's writes with
/// a short pause (the outbox kept), not an error and not a hot loop.
#[tokio::test]
async fn a_long_rate_limit_pauses_writes_until_retry_after() {
    let (base, fake) = spawn(Script { push: [refusal(429, "room_writer_rate", "120")].into(), ..Default::default() }).await;
    let (mut store, _, _) = store_with_chat();
    let mut eng = engine(&base);
    round(&mut eng, &mut store).await.unwrap();
    assert_eq!(fake.seen("POST", "/envelopes").len(), 1, "not slept in place");
    match eng.attention() {
        Some(SyncAttention::WritesPaused { refusal, until: Some(_) }) => assert_eq!(refusal.class(), RefusalClass::RateLimited),
        other => panic!("{other:?}"),
    }
    round(&mut eng, &mut store).await.unwrap();
    assert_eq!(fake.seen("POST", "/envelopes").len(), 1, "no push while paused");
}

/// A full room (`room_bytes`, `room_envelopes`, `keyring`) pauses writes to
/// that room only: reads go on, nothing loops, the person is told
/// "workspace full", and the events wait in the outbox for the next try.
#[tokio::test]
async fn a_full_room_pauses_its_writes_and_keeps_reading() {
    for code in ["room_bytes", "room_envelopes", "keyring"] {
        let (base, fake) = spawn(Script { push: [refusal(507, code, "3600")].into(), ..Default::default() }).await;
        let (mut store, _, _) = store_with_chat();
        let mut eng = engine(&base);
        round(&mut eng, &mut store).await.expect("a full room isn't a failed round");
        let att = eng.attention().expect("attention");
        assert!(att.is_write_pause());
        assert!(att.message().starts_with("workspace full"), "{}", att.message());
        assert!(att.message().contains(code));
        for _ in 0..5 {
            round(&mut eng, &mut store).await.unwrap();
        }
        assert_eq!(fake.seen("POST", "/envelopes").len(), 1, "{code}: no loop against a full room");
        assert_eq!(fake.seen("GET", "/envelopes").len(), 6, "{code}: reads go on");
        // Space freed (or the hour passed): the same events go out.
        eng.resume_writes();
        round(&mut eng, &mut store).await.unwrap();
        assert_eq!(fake.seen("POST", "/envelopes").len(), 3, "{code}: both envelopes pushed after the pause");
        assert!(eng.attention().is_none());
        assert!(eng.take_unpushed(&store).unwrap().is_empty());
    }
}

/// Another room on the same relay keeps writing while one is full.
#[tokio::test]
async fn a_full_room_pauses_only_that_room() {
    let (base, fake) = spawn(Script { push: [refusal(507, "room_bytes", "3600")].into(), ..Default::default() }).await;
    let (mut full_store, _, _) = store_with_chat();
    let (mut other_store, _, _) = store_with_chat();
    let mut full = engine(&base);
    let mut other = engine(&base);
    round(&mut full, &mut full_store).await.unwrap();
    round(&mut other, &mut other_store).await.unwrap();
    assert!(full.attention().is_some() && other.attention().is_none());
    assert_eq!(fake.seen("POST", "/envelopes").len(), 3);
}

/// A tenant's quota reads "organisation quota reached".
#[tokio::test]
async fn an_organisation_quota_pauses_writes_with_its_own_words() {
    for code in ["tenant_bytes", "tenant_rooms"] {
        let (base, _) = spawn(Script { push: [refusal(507, code, "3600")].into(), ..Default::default() }).await;
        let (mut store, _, _) = store_with_chat();
        let mut eng = engine(&base);
        round(&mut eng, &mut store).await.unwrap();
        let m = eng.attention().unwrap().message();
        assert!(m.starts_with("organisation quota reached"), "{m}");
    }
}

/// A full relay (and an unknown `507`) backs off until `Retry-After`.
#[tokio::test]
async fn a_full_relay_backs_off() {
    for code in ["store_bytes", "total_bytes", "total_rooms", "contacts_full", "a_new_507"] {
        let (base, fake) = spawn(Script { push: [refusal(507, code, "600")].into(), ..Default::default() }).await;
        let (mut store, _, _) = store_with_chat();
        let mut eng = engine(&base);
        round(&mut eng, &mut store).await.unwrap();
        match eng.attention() {
            Some(SyncAttention::WritesPaused { refusal, until: Some(u) }) => {
                assert_eq!(refusal.class(), RefusalClass::RelayFull, "{code}");
                let left = u.saturating_duration_since(std::time::Instant::now());
                assert!(left > Duration::from_secs(590) && left <= Duration::from_secs(600), "{code}: {left:?}");
            }
            other => panic!("{code}: {other:?}"),
        }
        round(&mut eng, &mut store).await.unwrap();
        assert_eq!(fake.seen("POST", "/envelopes").len(), 1, "{code}");
    }
}

/// `client_rooms` and `too_many_pending` are never retried by themselves:
/// one request, a pause with no end, and a message that says so.
#[tokio::test]
async fn a_client_quota_is_surfaced_and_never_retried() {
    for code in ["client_rooms", "too_many_pending"] {
        let (base, fake) = spawn(Script { push: [refusal(429, code, "1")].into(), ..Default::default() }).await;
        let (mut store, _, _) = store_with_chat();
        let mut eng = engine(&base);
        round(&mut eng, &mut store).await.unwrap();
        match eng.attention() {
            Some(SyncAttention::WritesPaused { refusal, until: None }) => assert_eq!(refusal.class(), RefusalClass::ClientQuota),
            other => panic!("{code}: {other:?}"),
        }
        assert!(eng.attention().unwrap().message().contains("Not retried automatically"));
        tokio::time::sleep(Duration::from_millis(1100)).await;
        round(&mut eng, &mut store).await.unwrap();
        assert_eq!(fake.seen("POST", "/envelopes").len(), 1, "{code}: not retried");
    }
}

/// A read refused past what a request may wait fails the round with the
/// relay's `Retry-After`, so the loop waits that long.
#[tokio::test]
async fn a_long_read_rate_limit_fails_the_round_with_retry_after() {
    let (base, fake) = spawn(Script { fetch: [refusal(429, "read_rate", "45")].into(), ..Default::default() }).await;
    let mut store = EventStore::open_in_memory().unwrap();
    let mut eng = engine(&base);
    let err = eng.pull_round(&mut store).await.unwrap_err();
    assert_eq!(err.retry_after(), Some(Duration::from_secs(45)));
    assert!(matches!(err, crate::SyncError::Relay(RelayError::Refused(_))), "{err}");
    assert!(err.to_string().contains("read_rate"), "{err}");
    assert_eq!(fake.seen("GET", "/envelopes").len(), 1);
    let left = eng.hold_left().expect("the next round waits");
    assert!(left > Duration::from_secs(40) && left <= Duration::from_secs(45), "{left:?}");
}

/// A fresh fold whose first envelope isn't seq 1 reports "room expired or
/// truncated" instead of quietly folding a headless log.
#[tokio::test]
async fn a_fresh_fold_past_seq_one_reports_a_truncated_room() {
    let rows = vec![json!({ "seq": 41, "body": { "x": 1 } }), json!({ "seq": 42, "body": { "x": 2 } })];
    let (base, _) = spawn(Script { rows, ..Default::default() }).await;
    let mut store = EventStore::open_in_memory().unwrap();
    let mut eng = engine(&base);
    eng.pull_round(&mut store).await.unwrap();
    match eng.attention() {
        Some(SyncAttention::RoomTruncated { first_seq: 41 }) => {}
        other => panic!("{other:?}"),
    }
    assert!(eng.attention().unwrap().message().starts_with("room expired or truncated"));
    // A room that starts at 1 is whole.
    let (base, _) = spawn(Script { rows: vec![json!({ "seq": 1, "body": { "x": 1 } })], ..Default::default() }).await;
    let mut eng = engine(&base);
    eng.pull_round(&mut EventStore::open_in_memory().unwrap()).await.unwrap();
    assert!(eng.attention().is_none());
}

/// `sse_streams`: the stream open fails with the refusal and its wait.
#[tokio::test]
async fn too_many_streams_backs_the_stream_off() {
    let (base, _) = spawn(Script { other: [refusal(429, "sse_streams", "30")].into(), ..Default::default() }).await;
    let err = RelayClient::new(&base).open_event_stream("room").await.err().expect("refused");
    assert_eq!(err.retry_after(), Some(Duration::from_secs(30)));
    match &err {
        SseConnectError::Refused(r) => assert_eq!(r.class(), RefusalClass::StreamLimit),
        other => panic!("{other:?}"),
    }
}

/// `pair_lockout`: refused, and no further lookup is sent to that relay
/// until it ends.
#[tokio::test]
async fn a_pairing_lockout_stops_lookups_until_it_ends() {
    let (base, fake) = spawn(Script { other: [refusal(429, "pair_lockout", "300")].into(), ..Default::default() }).await;
    let c = RelayClient::new(&base);
    let e = c.resolve_pairing("ABCDEF").await.unwrap_err();
    assert_eq!(e.refusal().map(|r| r.class()), Some(RefusalClass::PairLockout));
    let e = RelayClient::new(&base).resolve_pairing("GHIJKL").await.unwrap_err();
    assert!(e.refusal().is_some_and(|r| r.retry_after > Duration::from_secs(290)), "{e}");
    assert_eq!(fake.seen("GET", "/v1/pair/").len(), 1, "nothing sent during the lockout");
}

/// Contact routes get the same treatment: a contact rate limit is retried in
/// place, `contacts_full` comes back typed.
#[tokio::test]
async fn contact_routes_retry_rate_limits_and_type_the_rest() {
    let cid = crate::contacts::new_cid();
    let (base, fake) = spawn(Script {
        other: [refusal(429, "contact_drop_rate", "1"), (200, None, json!({})), refusal(507, "contacts_full", "600")].into(),
        ..Default::default()
    })
    .await;
    let c = RelayClient::new(&base);
    c.contact_drop(&cid, &json!({})).await.unwrap();
    assert_eq!(fake.seen("POST", "/drop").len(), 2);
    match c.contact_drop(&cid, &json!({})).await {
        Err(crate::contacts::ContactsError::Refused(r)) => assert_eq!(r.class(), RefusalClass::RelayFull),
        other => panic!("{other:?}"),
    }
}

/// The idempotency key rides every envelope push, the query is untouched.
#[tokio::test]
async fn every_envelope_push_carries_its_key() {
    let (base, fake) = spawn(Script::default()).await;
    let (mut store, _, _) = store_with_chat();
    let mut eng = engine(&base);
    round(&mut eng, &mut store).await.unwrap();
    let posts = fake.seen("POST", "/envelopes");
    assert_eq!(posts.len(), 2);
    assert!(posts.iter().all(|p| p.idem.as_deref().is_some_and(|k| k.len() == 32) && p.query.is_empty()));
}

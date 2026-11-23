//! Several connections to one `ligus.db`, as a running device has them: the
//! chat service authoring events, the sync loop ingesting the relay's, a DM
//! sync, a one-shot `ligus sync`. Each test races them on one file for many
//! rounds and checks that nothing reports "database is locked" or a
//! sequence collision, and that no event is lost or stored twice.
//!
//! `LIGUS_STRESS_ROUNDS` raises the rounds (default 200 per writer).

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;

use ligus_core::{ChatMessage, MessageRole, SessionEvent, SessionEventEnvelope};
use ligus_runtime::event_store::{EventStore, HoldItem};
use uuid::Uuid;

fn rounds() -> usize {
    std::env::var("LIGUS_STRESS_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(200)
}

fn message(body: String) -> SessionEvent {
    SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "u", body) }
}

/// A fresh database with one event in `sid`, so every writer opens an
/// existing WAL file.
fn seeded(path: &Path, sid: Uuid, ws: Uuid) {
    let store = EventStore::open(path).unwrap();
    store.append_envelope(&SessionEventEnvelope::new(sid, ws, 1, message("seed".into()))).unwrap();
}

/// Run `writers` on their own threads, all released at once; every error
/// any of them saw.
fn race(writers: Vec<Box<dyn FnOnce() -> Vec<String> + Send>>) -> Vec<String> {
    let gate = Arc::new(Barrier::new(writers.len()));
    let handles: Vec<_> = writers
        .into_iter()
        .map(|w| {
            let gate = gate.clone();
            thread::spawn(move || {
                gate.wait();
                w()
            })
        })
        .collect();
    handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
}

/// Author the next event of `sid` the way `ChatService::draft_signed` and
/// `store_authored` do: the sequence is read first, the envelope stored
/// after (a phone-signed one can wait seconds in between).
fn author(store: &EventStore, sid: Uuid, ws: Uuid, body: String) -> Result<SessionEventEnvelope, String> {
    let seq = store.max_sequence(sid).map_err(|e| e.to_string())?.unwrap_or(0) + 1;
    let env = SessionEventEnvelope::new(sid, ws, seq, message(body));
    store.append_envelope(&env).map_err(|e| e.to_string())?;
    Ok(env)
}

/// The failure seen between two devices through a relay: this device
/// authors in a chat while its sync loop ingests the other device's events
/// of the same chat, on another connection. Every event lands exactly once,
/// at its own position, and an authored envelope is stored as it was signed.
#[test]
fn authoring_and_relay_ingest_of_one_chat_never_collide() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ligus.db");
    let (sid, ws) = (Uuid::new_v4(), Uuid::new_v4());
    seeded(&path, sid, ws);
    let n = rounds();

    let mut writers: Vec<Box<dyn FnOnce() -> Vec<String> + Send>> = Vec::new();
    let authored = Arc::new(std::sync::Mutex::new(Vec::new()));
    for w in 0..2 {
        let (path, authored) = (path.clone(), authored.clone());
        writers.push(Box::new(move || {
            let store = EventStore::open(&path).unwrap();
            let mut errors = Vec::new();
            for i in 0..n {
                match author(&store, sid, ws, format!("local {w} {i}")) {
                    Ok(env) => authored.lock().unwrap().push(env),
                    Err(e) => errors.push(format!("author: {e}")),
                }
            }
            errors
        }));
    }
    let ingested: Vec<SessionEventEnvelope> =
        (0..n).map(|i| SessionEventEnvelope::new(sid, ws, (i + 1) as i64, message(format!("peer {i}")))).collect();
    {
        let (path, ingested) = (path.clone(), ingested.clone());
        writers.push(Box::new(move || {
            let mut store = EventStore::open(&path).unwrap();
            let mut errors = Vec::new();
            for env in &ingested {
                match store.ingest(env) {
                    Ok(true) => {}
                    Ok(false) => errors.push(format!("ingest: {} reported as already stored", env.event_id)),
                    Err(e) => errors.push(format!("ingest: {e}")),
                }
            }
            errors
        }));
    }
    let errors = race(writers);
    assert!(errors.is_empty(), "{} of {} writes failed, first: {:?}", errors.len(), 3 * n, &errors[..errors.len().min(5)]);

    let store = EventStore::open(&path).unwrap();
    let stored = store.list(sid).unwrap();
    assert_eq!(stored.len(), 1 + 3 * n, "every event stored exactly once");
    let ids: HashSet<Uuid> = stored.iter().map(|e| e.event_id).collect();
    assert_eq!(ids.len(), stored.len(), "no event stored twice");
    for env in authored.lock().unwrap().iter().chain(&ingested) {
        assert!(ids.contains(&env.event_id), "{} lost", env.event_id);
    }
    // An authored envelope is kept exactly as written (its sequence is
    // signed); only its place in this device's log may move.
    let by_id: std::collections::HashMap<Uuid, &SessionEventEnvelope> = stored.iter().map(|e| (e.event_id, e)).collect();
    for env in authored.lock().unwrap().iter() {
        assert_eq!(by_id[&env.event_id].sequence, env.sequence, "an authored envelope's own sequence never changes");
    }
    assert_eq!(store.max_sequence(sid).unwrap(), Some((1 + 3 * n) as i64), "positions 1..=n, no gaps");
}

/// Two sync engines (the serve loop and a one-shot sync, or the DM sync)
/// pull the same relay page at once: each event is stored once, exactly
/// one of them reports it new, and neither fails.
#[test]
fn concurrent_ingest_of_the_same_events_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ligus.db");
    let (sid, ws) = (Uuid::new_v4(), Uuid::new_v4());
    seeded(&path, sid, ws);
    let n = rounds();
    let page: Arc<Vec<SessionEventEnvelope>> =
        Arc::new((0..n).map(|i| SessionEventEnvelope::new(sid, ws, (i + 1) as i64, message(format!("peer {i}")))).collect());
    let fresh = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let writers: Vec<Box<dyn FnOnce() -> Vec<String> + Send>> = (0..3)
        .map(|_| {
            let (path, page, fresh) = (path.clone(), page.clone(), fresh.clone());
            Box::new(move || {
                let mut store = EventStore::open(&path).unwrap();
                let mut errors = Vec::new();
                for env in page.iter() {
                    match store.ingest(env) {
                        Ok(true) => {
                            fresh.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                        Ok(false) => {}
                        Err(e) => errors.push(format!("ingest: {e}")),
                    }
                }
                errors
            }) as Box<dyn FnOnce() -> Vec<String> + Send>
        })
        .collect();
    let errors = race(writers);
    assert!(errors.is_empty(), "{} of {} ingests failed, first: {:?}", errors.len(), 3 * n, &errors[..errors.len().min(5)]);
    assert_eq!(fresh.load(std::sync::atomic::Ordering::SeqCst), n, "each event reported new exactly once");
    assert_eq!(EventStore::open(&path).unwrap().list(sid).unwrap().len(), 1 + n);
}

/// Bookkeeping that reads before it writes in one transaction (holding
/// events it can't verify yet, expiring old ones) beside another
/// connection's appends: it waits for the write lock instead of failing.
#[test]
fn held_event_bookkeeping_beside_appends_never_reports_busy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ligus.db");
    let (sid, ws) = (Uuid::new_v4(), Uuid::new_v4());
    seeded(&path, sid, ws);
    let n = rounds();
    let mut writers: Vec<Box<dyn FnOnce() -> Vec<String> + Send>> = Vec::new();
    {
        let path = path.clone();
        writers.push(Box::new(move || {
            let store = EventStore::open(&path).unwrap();
            let mut errors = Vec::new();
            let other = Uuid::new_v4();
            for i in 0..n {
                let env = SessionEventEnvelope::new(other, ws, 1, message(format!("held {i}")));
                if let Err(e) = store.hold_events(ws, &[HoldItem { env: &env, reason: "unknown signer", seq: Some(i as u64) }], i as i64) {
                    errors.push(format!("hold: {e}"));
                }
                if let Err(e) = store.expire_held(i as i64) {
                    errors.push(format!("expire: {e}"));
                }
            }
            errors
        }));
    }
    {
        let path = path.clone();
        writers.push(Box::new(move || {
            let store = EventStore::open(&path).unwrap();
            (0..n).filter_map(|i| author(&store, sid, ws, format!("local {i}")).err().map(|e| format!("author: {e}"))).collect()
        }));
    }
    let errors = race(writers);
    assert!(errors.is_empty(), "{} of {} writes failed, first: {:?}", errors.len(), 3 * n, &errors[..errors.len().min(5)]);
}

/// Several connections opening a brand-new database at once (a first
/// start: the service, the sync loop, a dashboard read): each waits for the
/// others' locks instead of failing, and the schema is created once.
#[test]
fn opening_a_new_store_from_several_connections_at_once_never_reports_busy() {
    let dir = tempfile::tempdir().unwrap();
    let (sid, ws) = (Uuid::new_v4(), Uuid::new_v4());
    let n = rounds();
    let mut failures = Vec::new();
    for round in 0..(n / 10).max(5) {
        let path = dir.path().join(format!("fresh-{round}.db"));
        let writers: Vec<Box<dyn FnOnce() -> Vec<String> + Send>> = (0..4)
            .map(|w| {
                let path = path.clone();
                Box::new(move || match EventStore::open(&path) {
                    Ok(store) => author(&store, sid, ws, format!("w{w}")).err().map(|e| format!("author: {e}")).into_iter().collect(),
                    Err(e) => vec![format!("open: {e}")],
                }) as Box<dyn FnOnce() -> Vec<String> + Send>
            })
            .collect();
        let failed = race(writers);
        let stored = EventStore::open(&path).unwrap().list(sid).unwrap();
        assert_eq!(stored.len(), 4 - failed.len(), "round {round}: every connection that opened stored its event");
        failures.extend(failed);
    }
    assert!(failures.is_empty(), "{} failed, first: {:?}", failures.len(), &failures[..failures.len().min(5)]);
}

/// A connection opened (by a sync round, a one-shot command, a dashboard
/// read) while another writes: opening waits for the lock instead of
/// failing.
#[test]
fn opening_the_store_while_another_connection_writes_never_reports_busy() {
    let dir = tempfile::tempdir().unwrap();
    let (sid, ws) = (Uuid::new_v4(), Uuid::new_v4());
    let n = rounds();
    let path = dir.path().join("ligus.db");
    seeded(&path, sid, ws);
    let mut writers: Vec<Box<dyn FnOnce() -> Vec<String> + Send>> = Vec::new();
    {
        let path = path.clone();
        writers.push(Box::new(move || {
            let store = EventStore::open(&path).unwrap();
            (0..n).filter_map(|i| author(&store, sid, ws, format!("local {i}")).err().map(|e| format!("author: {e}"))).collect()
        }));
    }
    for _ in 0..2 {
        let path = path.clone();
        writers.push(Box::new(move || {
            (0..n / 4)
                .filter_map(|_| EventStore::open(&path).and_then(|s| s.max_sequence(sid)).err().map(|e| format!("open: {e}")))
                .collect()
        }));
    }
    let failures = race(writers);
    assert!(failures.is_empty(), "{} failed, first: {:?}", failures.len(), &failures[..failures.len().min(5)]);
}

//! Log hygiene, `user_version` 2 (workspace model v3, 7.2 to 7.4, 13, 16.2
//! M8 and M10; invariants L3, L5 and V1).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ligus_core::{crypto::SigningKeypair, ActorIdentity, ActorKind, ChatMessage, MessageRole, SessionEvent, SessionEventEnvelope};
use uuid::Uuid;

use super::*;
use crate::chat_service::{ChatService, CHUNK_BYTES, CHUNK_EVERY};

const HOUR_MS: i64 = 60 * 60 * 1000;
const DAY_MS: i64 = 24 * HOUR_MS;

fn now() -> i64 {
    storage_v2::now_ms()
}

fn rows(store: &EventStore, sql: &str) -> i64 {
    store.conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn chunks_in(store: &EventStore, wid: Uuid, chat: Uuid) -> Vec<SessionEventEnvelope> {
    store.list_in(wid, chat).unwrap().into_iter().filter(|e| matches!(e.payload, SessionEvent::MessageChunkReceived { .. })).collect()
}

/// One device's keys and data dir; [`Device::service`] opens another
/// connection to it.
struct Device {
    _dir: tempfile::TempDir,
    path: PathBuf,
    seed: [u8; 32],
    account: [u8; 32],
}

impl Device {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ligus.db");
        Device { _dir: dir, path, seed: SigningKeypair::generate().unwrap().seed_bytes(), account: SigningKeypair::generate().unwrap().seed_bytes() }
    }

    /// Another computer of the same person.
    fn sibling(&self) -> Self {
        Device { account: self.account, ..Device::new() }
    }

    fn service(&self) -> ChatService {
        ChatService::new(
            EventStore::open(&self.path).unwrap(),
            SigningKeypair::from_seed(&self.seed).unwrap(),
            SigningKeypair::from_seed(&self.account).unwrap(),
            ActorIdentity::new("u", "U", ActorKind::Human),
        )
    }
}

/// Everything `from` holds, stored by `to` (a pull from the start: a sync
/// cursor reset). How many rows it added.
fn pull_all(from: &ChatService, to: &mut ChatService) -> usize {
    let (_, envs) = from.store().envelopes_after_row(0).unwrap();
    envs.iter().filter(|e| to.store_mut().ingest(e).unwrap()).count()
}

/// A reply streamed in `parts`, each written to the log (spaced past the
/// coalescing interval), then completed.
fn streamed_reply(svc: &mut ChatService, wid: Uuid, chat: Uuid, to: Uuid, parts: &[&str]) -> Uuid {
    let reply = svc.begin_assistant_message(chat, wid, "ligus", "anthropic", None, Some(to)).unwrap();
    let t0 = Instant::now();
    for (i, part) in parts.iter().enumerate() {
        svc.append_chunk_at(chat, wid, reply, *part, t0 + CHUNK_EVERY * (i as u32 + 1)).unwrap();
    }
    svc.complete_assistant_message(chat, wid, reply, parts.concat()).unwrap();
    reply
}

// ---------------------------------------------------------------------------
// Tombstones (L3)
// ---------------------------------------------------------------------------

/// L3: a pruned chunk fetched again is a witness only, never a row again.
#[test]
fn a_pruned_chunk_fetched_again_is_never_stored_again() {
    let dev = Device::new();
    let mut svc = dev.service();
    let wid = Uuid::new_v4();
    let chat = svc.create_chat("c", wid, "anthropic").unwrap().id;
    let q = svc.post_user_message(chat, wid, "hello").unwrap();
    streamed_reply(&mut svc, wid, chat, q.id, &["one ", "two ", "three "]);
    let chunks = chunks_in(svc.store(), wid, chat);
    assert_eq!(chunks.len(), 3);
    assert_eq!(svc.prune_superseded_chunks().unwrap(), 3);
    assert_eq!(svc.store().tombstone_count().unwrap(), 3);
    let before = rows(svc.store(), "SELECT COUNT(*) FROM events");
    let witnesses = rows(svc.store(), "SELECT COUNT(*) FROM lamport_witnesses");
    for c in &chunks {
        assert!(!svc.store_mut().ingest(c).unwrap(), "a pruned chunk was stored again");
    }
    // Fetched again re-encoded (as a relay hands it back): the same copy.
    for c in &chunks {
        let again: SessionEventEnvelope = serde_json::from_str(&serde_json::to_string(c).unwrap()).unwrap();
        assert!(!svc.store_mut().ingest(&again).unwrap());
    }
    assert_eq!(rows(svc.store(), "SELECT COUNT(*) FROM events"), before);
    assert_eq!(rows(svc.store(), "SELECT COUNT(*) FROM equivocations"), 0);
    assert_eq!(rows(svc.store(), "SELECT COUNT(*) FROM lamport_witnesses"), witnesses, "one witness per pruned chunk");
    let body = svc.load_in(wid, chat).unwrap().unwrap().messages.last().unwrap().body.clone();
    assert_eq!(body, "one two three ");
}

/// A different copy under a pruned id isn't hidden by the tombstone: it is
/// kept beside it (the fold decides), never stored in the log's own place,
/// and the next prune takes it too.
#[test]
fn a_different_copy_under_a_pruned_id_is_kept_beside_the_tombstone() {
    let dev = Device::new();
    let mut svc = dev.service();
    let wid = Uuid::new_v4();
    let chat = svc.create_chat("c", wid, "anthropic").unwrap().id;
    let q = svc.post_user_message(chat, wid, "hello").unwrap();
    let reply = streamed_reply(&mut svc, wid, chat, q.id, &["genuine "]);
    let chunk = chunks_in(svc.store(), wid, chat).remove(0);
    assert_eq!(svc.prune_superseded_chunks().unwrap(), 1);
    let mut other = chunk.clone();
    other.payload = SessionEvent::MessageChunkReceived { message_id: reply, chunk: "forged ".into() };
    other.clear_raw();
    ligus_core::sign_envelope(&mut other, Uuid::new_v4(), &SigningKeypair::generate().unwrap());
    let events = rows(svc.store(), "SELECT COUNT(*) FROM events");
    assert!(!svc.store_mut().ingest(&other).unwrap(), "stored in the log's own place");
    assert_eq!(rows(svc.store(), "SELECT COUNT(*) FROM events"), events);
    assert!(svc.store().list_copies_in(wid, chat).unwrap().iter().any(|e| e.event_id == chunk.event_id), "hidden by the tombstone");
    // The reply is complete either way.
    assert_eq!(svc.load_in(wid, chat).unwrap().unwrap().messages.last().unwrap().body, "genuine ");
    assert_eq!(svc.prune_superseded_chunks().unwrap(), 0, "no log row");
    assert!(svc.store().list_copies_in(wid, chat).unwrap().iter().all(|e| e.event_id != chunk.event_id), "the copy outlived the prune");
    assert_eq!(svc.store().tombstone_count().unwrap(), 2);
    assert!(!svc.store_mut().ingest(&other).unwrap());
    assert!(svc.store().list_copies_in(wid, chat).unwrap().iter().all(|e| e.event_id != chunk.event_id), "the pruned copy came back");
}

// ---------------------------------------------------------------------------
// Dispatch novelty (L5)
// ---------------------------------------------------------------------------

/// L5: a message written offline below the chat's head is answered once; a
/// re-pull after prune stores nothing and dispatch never takes anything
/// again; a message older than the age bound by its envelope time is never
/// taken.
#[test]
fn dispatch_takes_each_message_once_whatever_its_place_and_never_after_a_re_pull() {
    let desk_dev = Device::new();
    let laptop_dev = desk_dev.sibling();
    let (mut desk, mut laptop) = (desk_dev.service(), laptop_dev.service());
    let wid = Uuid::new_v4();
    let chat = desk.create_chat("c", wid, "anthropic").unwrap().id;
    desk.post_user_message(chat, wid, "first").unwrap();
    pull_all(&desk, &mut laptop);
    // The laptop goes offline and posts at the head it has; the chat moves
    // on thirty events on the desktop meanwhile.
    let offline = laptop.post_user_message(chat, wid, "written offline").unwrap();
    for i in 0..30 {
        desk.post_user_message(chat, wid, &format!("desk {i}")).unwrap();
    }
    let head = desk.store().lamport_head(wid, chat).unwrap().unwrap().lamport;
    let before = desk.store().max_row_id().unwrap();
    assert!(desk.store().take_dispatch_candidates(before).unwrap().is_empty(), "the desktop's own posts");
    assert!(pull_all(&laptop, &mut desk) > 0);
    let low = desk.store().list_in(wid, chat).unwrap().into_iter().find(|e| matches!(&e.payload, SessionEvent::MessageAppended { message } if message.id == offline.id)).unwrap();
    assert!(low.lamport < head, "written below the chat's head");
    let taken = desk.store().take_dispatch_candidates(before).unwrap();
    assert_eq!(taken.iter().filter(|c| c.2 == offline.id).count(), 1, "answered once: {taken:?}");
    assert!(desk.store().take_dispatch_candidates(0).unwrap().is_empty(), "taken again");
    // The desktop answers it; its chunks are pruned; a cursor reset pulls
    // the whole log back: nothing is stored and nothing is taken.
    streamed_reply(&mut desk, wid, chat, offline.id, &["sure ", "thing "]);
    let chunks = chunks_in(desk.store(), wid, chat);
    assert_eq!(chunks.len(), 2);
    pull_all(&desk, &mut laptop);
    assert_eq!(desk.prune_superseded_chunks().unwrap(), 2);
    let events = rows(desk.store(), "SELECT COUNT(*) FROM events");
    let before = desk.store().max_row_id().unwrap();
    assert_eq!(pull_all(&laptop, &mut desk), 0, "a re-pull after prune stored something");
    assert_eq!(rows(desk.store(), "SELECT COUNT(*) FROM events"), events);
    assert!(desk.store().take_dispatch_candidates(before).unwrap().is_empty(), "dispatch ran again");
    // A message from the laptop that the desktop first sees three days after
    // it was signed (by its envelope time, whatever its payload says).
    let late = laptop.post_user_message(chat, wid, "three days late").unwrap();
    let before = desk.store().max_row_id().unwrap();
    assert!(pull_all(&laptop, &mut desk) > 0);
    assert!(desk.store().take_dispatch_candidates_at(before, now() + 3 * DAY_MS).unwrap().is_empty(), "an old message was taken over");
    let _ = late;
}

/// The age bound reads the signed envelope time, never the payload's
/// `created_at` (a clock its author fills in).
#[test]
fn the_age_bound_is_the_envelope_time() {
    let mut store = EventStore::open_in_memory().unwrap();
    let (wid, sid) = (Uuid::new_v4(), Uuid::new_v4());
    let mut fresh_claim = ChatMessage::new(MessageRole::User, "x", "claims to be new");
    fresh_claim.created_at = ligus_core::Timestamp::now();
    let mut env = SessionEventEnvelope::new(sid, wid, 1, SessionEvent::MessageAppended { message: fresh_claim });
    env.timestamp = ligus_core::Timestamp(time::OffsetDateTime::now_utc() - time::Duration::days(3));
    let env = fixture_next(&env, None);
    assert!(store.ingest(&env).unwrap());
    assert!(store.take_dispatch_candidates(0).unwrap().is_empty(), "taken by its payload's word");
    let mut old_claim = ChatMessage::new(MessageRole::User, "x", "claims to be old");
    old_claim.created_at = ligus_core::Timestamp(time::OffsetDateTime::now_utc() - time::Duration::days(3));
    let head = store.lamport_head(wid, sid).unwrap();
    let env = fixture_next(&SessionEventEnvelope::new(sid, wid, 2, SessionEvent::MessageAppended { message: old_claim.clone() }), head);
    assert!(store.ingest(&env).unwrap());
    let taken = store.take_dispatch_candidates(0).unwrap();
    assert_eq!(taken.iter().map(|c| c.2).collect::<Vec<_>>(), vec![old_claim.id]);
}

/// Processed marks are trimmed once their event is past the bound, but a
/// message dated in the future keeps its mark (so it is never taken twice).
#[test]
fn processed_marks_are_trimmed_past_the_bound_only() {
    let mut store = EventStore::open_in_memory().unwrap();
    let (wid, sid) = (Uuid::new_v4(), Uuid::new_v4());
    let mut env = SessionEventEnvelope::new(sid, wid, 1, SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "x", "from the future") });
    env.timestamp = ligus_core::Timestamp(time::OffsetDateTime::now_utc() + time::Duration::days(30));
    let future = fixture_next(&env, None);
    assert!(store.ingest(&future).unwrap());
    let head = store.lamport_head(wid, sid).unwrap();
    let now_msg = fixture_next(&SessionEventEnvelope::new(sid, wid, 2, SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "x", "now") }), head);
    assert!(store.ingest(&now_msg).unwrap());
    assert_eq!(store.take_dispatch_candidates(0).unwrap().len(), 2);
    assert_eq!(store.trim_dispatch_processed(now() + 2 * DAY_MS).unwrap(), 1, "the present one is past the bound by then");
    assert!(store.dispatch_processed(wid, future.event_id).unwrap());
    assert!(store.take_dispatch_candidates_at(0, now() + 2 * DAY_MS).unwrap().is_empty(), "the future one was taken twice");
}

// ---------------------------------------------------------------------------
// Prune cadence
// ---------------------------------------------------------------------------

/// Launching twice in a day prunes once; the day after, it prunes again.
#[test]
fn launching_twice_in_a_day_prunes_once() {
    let dev = Device::new();
    let wid = Uuid::new_v4();
    let t = now();
    let chat = {
        let mut svc = dev.service();
        assert_eq!(svc.store().schema_version().unwrap(), SCHEMA_VERSION);
        let chat = svc.create_chat("c", wid, "anthropic").unwrap().id;
        let q = svc.post_user_message(chat, wid, "a").unwrap();
        streamed_reply(&mut svc, wid, chat, q.id, &["x ", "y "]);
        assert_eq!(svc.store_mut().prune_if_due_at(t).unwrap(), Some(2), "the first launch prunes");
        let q = svc.post_user_message(chat, wid, "b").unwrap();
        streamed_reply(&mut svc, wid, chat, q.id, &["z "]);
        chat
    };
    // The second launch that day.
    let mut svc = dev.service();
    assert_eq!(svc.store_mut().prune_if_due_at(t + HOUR_MS).unwrap(), None, "pruned twice in a day");
    assert_eq!(chunks_in(svc.store(), wid, chat).len(), 1);
    assert_eq!(svc.store_mut().prune_if_due_at(t + DAY_MS - 1).unwrap(), None);
    assert_eq!(svc.store_mut().prune_if_due_at(t + DAY_MS).unwrap(), Some(1), "the next day");
    assert_eq!(svc.store_mut().prune_if_due_at(t + DAY_MS + HOUR_MS).unwrap(), None);
    // A clock set back by days doesn't stop it for good.
    assert_eq!(svc.store_mut().prune_if_due_at(t - 3 * DAY_MS).unwrap(), Some(0));
}

/// Before version 2 the prune waits: an older build on the same data dir
/// would store a pruned chunk again.
#[test]
fn the_prune_waits_for_version_2() {
    let dev = Device::new();
    let mut svc = dev.service();
    let wid = Uuid::new_v4();
    let chat = svc.create_chat("c", wid, "anthropic").unwrap().id;
    let q = svc.post_user_message(chat, wid, "a").unwrap();
    streamed_reply(&mut svc, wid, chat, q.id, &["x "]);
    svc.store().conn.pragma_update(None, "user_version", SYNC_SCHEMA_VERSION).unwrap();
    assert_eq!(svc.prune_if_due().unwrap(), None);
    assert_eq!(chunks_in(svc.store(), wid, chat).len(), 1);
}

// ---------------------------------------------------------------------------
// Chunk coalescing (8.1)
// ---------------------------------------------------------------------------

/// A reply streams in many small deltas: the log gets at most one chunk per
/// CHUNK_EVERY plus one per CHUNK_BYTES (and the first at once), and the
/// folded text is the whole text at every step that wrote.
#[test]
fn streaming_chunks_are_coalesced_by_time_and_size() {
    let dev = Device::new();
    let mut svc = dev.service();
    let wid = Uuid::new_v4();
    let chat = svc.create_chat("c", wid, "anthropic").unwrap().id;
    let q = svc.post_user_message(chat, wid, "go").unwrap();
    let reply = svc.begin_assistant_message(chat, wid, "ligus", "anthropic", None, Some(q.id)).unwrap();
    // 400 deltas of 10 bytes, 50 ms apart: 20 s, 4000 bytes.
    let t0 = Instant::now();
    let mut text = String::new();
    for i in 0..400u32 {
        let delta = format!("w{i:03} more ");
        let delta = &delta[..10.min(delta.len())];
        text.push_str(delta);
        svc.append_chunk_at(chat, wid, reply, delta, t0 + Duration::from_millis(50) * i).unwrap();
    }
    let n = chunks_in(svc.store(), wid, chat).len();
    let span = Duration::from_millis(50) * 399;
    let bound = 1 + (span.as_millis() / CHUNK_EVERY.as_millis()) as usize + text.len() / CHUNK_BYTES;
    assert!(n <= bound, "{n} chunks for {span:?} and {} bytes (bound {bound})", text.len());
    assert!(n >= 5, "still streams: {n}");
    let streamed = svc.load_in(wid, chat).unwrap().unwrap().messages.iter().find(|m| m.id == reply).unwrap().body.clone();
    assert!(text.starts_with(&streamed) && text.len() - streamed.len() < CHUNK_BYTES, "the log lags the text by at most what is held");
    // A burst past the byte budget writes at once, inside the interval.
    let t1 = t0 + span + CHUNK_EVERY;
    svc.append_chunk_at(chat, wid, reply, " ", t1).unwrap();
    let before = chunks_in(svc.store(), wid, chat).len();
    svc.append_chunk_at(chat, wid, reply, "a".repeat(3000) + " ", t1 + Duration::from_millis(10)).unwrap();
    assert_eq!(chunks_in(svc.store(), wid, chat).len(), before, "3 KiB waits for the interval");
    svc.append_chunk_at(chat, wid, reply, "b".repeat(3000) + " ", t1 + Duration::from_millis(20)).unwrap();
    assert_eq!(chunks_in(svc.store(), wid, chat).len(), before + 1, "6 KiB is written at once");
    svc.complete_assistant_message(chat, wid, reply, "done").unwrap();
    let m = svc.load_in(wid, chat).unwrap().unwrap().messages.into_iter().find(|m| m.id == reply).unwrap();
    assert_eq!(m.body, "done");
    assert!(!m.is_streaming);
}

/// The secret hold-back still applies to a coalesced chunk: a credential
/// split across deltas is never written in part.
#[test]
fn a_coalesced_chunk_still_holds_back_a_split_secret() {
    let dev = Device::new();
    let mut svc = dev.service();
    let wid = Uuid::new_v4();
    let chat = svc.create_chat("c", wid, "anthropic").unwrap().id;
    let q = svc.post_user_message(chat, wid, "go").unwrap();
    let reply = svc.begin_assistant_message(chat, wid, "ligus", "anthropic", None, Some(q.id)).unwrap();
    let t0 = Instant::now();
    svc.append_chunk_at(chat, wid, reply, "the key is AKIAIOSF", t0 + CHUNK_EVERY).unwrap();
    svc.append_chunk_at(chat, wid, reply, "ODNN7EXAMPLE and more ", t0 + CHUNK_EVERY * 3).unwrap();
    for c in chunks_in(svc.store(), wid, chat) {
        let SessionEvent::MessageChunkReceived { chunk, .. } = &c.payload else { unreachable!() };
        assert!(!chunk.contains("AKIA"), "{chunk}");
    }
}

// ---------------------------------------------------------------------------
// Snapshots (7.4)
// ---------------------------------------------------------------------------

/// Only the seed snapshot is written: a long chat writes no periodic one.
#[test]
fn a_long_chat_writes_no_periodic_snapshot() {
    let dev = Device::new();
    let mut svc = dev.service();
    let wid = Uuid::new_v4();
    let chat = svc.create_chat("c", wid, "anthropic").unwrap().id;
    let seeds = svc.store().list_in(wid, chat).unwrap().iter().filter(|e| matches!(e.payload, SessionEvent::SessionSnapshot { .. })).count();
    assert_eq!(seeds, 1, "the seed");
    for i in 0..120 {
        let q = svc.post_user_message(chat, wid, &format!("q{i}")).unwrap();
        let r = svc.begin_assistant_message(chat, wid, "ligus", "anthropic", None, Some(q.id)).unwrap();
        svc.complete_assistant_message(chat, wid, r, "ok").unwrap();
    }
    let snaps = svc.store().list_in(wid, chat).unwrap().iter().filter(|e| matches!(e.payload, SessionEvent::SessionSnapshot { .. })).count();
    assert_eq!(snaps, 1, "a periodic snapshot was written");
    assert!(svc.load_in(wid, chat).unwrap().unwrap().messages.len() >= 240);
}

// ---------------------------------------------------------------------------
// Contacts across logs
// ---------------------------------------------------------------------------

/// The newest contact stamp is read by the signed envelope time across a
/// workspace's logs: a busy chat's high lamport doesn't make an old stamp
/// newer than a quiet chat's recent one.
#[test]
fn the_latest_contact_is_the_newest_by_envelope_time_across_logs() {
    let mut store = EventStore::open_in_memory().unwrap();
    let kp = SigningKeypair::from_seed(&[0x51; 32]).unwrap();
    let dev = ligus_core::crypto::device_id_for(ligus_core::crypto::SigAlg::Ed25519, &kp.public_key_bytes());
    let account = Uuid::new_v4();
    let wid = Uuid::new_v4();
    let stamp = |sid: Uuid, lamport: u64, age: time::Duration, cid: &str| {
        let mut env = SessionEventEnvelope::new(sid, wid, 1, SessionEvent::SessionTitleChanged { title: "t".into() });
        let mut actor = ActorIdentity::new("ada", "Ada", ActorKind::Human);
        actor.account_id = Some(account);
        actor.contact = Some(cid.to_string());
        env.actor_stamp = Some(ligus_core::ActorStamp {
            actor,
            recorded_at: ligus_core::Timestamp::now(),
            cfg_frontier: None,
            presence: None,
            runtime_effect: false,
            prev: None,
            sig_alg: Default::default(),
            log_head: None,
        });
        env.lamport = lamport;
        env.timestamp = ligus_core::Timestamp(time::OffsetDateTime::now_utc() - age);
        ligus_core::sign_envelope(&mut env, dev, &kp);
        env
    };
    let (busy, quiet) = (Uuid::new_v4(), Uuid::new_v4());
    store.ingest(&stamp(busy, 500, time::Duration::hours(5), "c_old")).unwrap();
    store.ingest(&stamp(quiet, 3, time::Duration::minutes(1), "c_new")).unwrap();
    let key = kp.public_key_bytes().to_vec();
    let signer_key = |d: Uuid| (d == dev).then(|| key.clone());
    assert_eq!(store.latest_contact_of(wid, account, &signer_key).unwrap().as_deref(), Some("c_new"));
}

// ---------------------------------------------------------------------------
// Migration (M8, M10; V1) from a 0.4.1 database
// ---------------------------------------------------------------------------

/// The fixture written by the 0.4.1 release (`tests/fixtures/db-v041`): one
/// chat with two answered turns (the first reply's chunks pruned by 0.4.1,
/// so only witnesses; the second's still stored) and one unanswered turn.
fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/db-v041")
}

struct Fixture {
    _dir: tempfile::TempDir,
    db: PathBuf,
    ids: serde_json::Value,
    pruned: Vec<SessionEventEnvelope>,
}

fn v041_copy() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ligus.db");
    std::fs::copy(fixture_dir().join("ligus.db"), &db).unwrap();
    let ids = serde_json::from_str(&std::fs::read_to_string(fixture_dir().join("ids.json")).unwrap()).unwrap();
    let pruned = serde_json::from_str(&std::fs::read_to_string(fixture_dir().join("pruned_chunks.json")).unwrap()).unwrap();
    Fixture { _dir: dir, db, ids, pruned }
}

fn id(f: &Fixture, k: &str) -> Uuid {
    Uuid::parse_str(f.ids[k].as_str().unwrap()).unwrap()
}

/// When the fixture's last message was signed (ms): the tests read the
/// fixture "an hour later", whenever they run.
fn fixture_time(store: &EventStore, f: &Fixture) -> i64 {
    let env = store
        .list_in(id(f, "workspace"), id(f, "chat"))
        .unwrap()
        .into_iter()
        .find(|e| matches!(&e.payload, SessionEvent::MessageAppended { message } if message.id == id(f, "q3")))
        .unwrap();
    (env.timestamp.0.unix_timestamp_nanos() / 1_000_000) as i64
}

fn backups(db: &Path) -> Vec<PathBuf> {
    seed_backups(db).into_iter().filter(|p| p.to_string_lossy().contains(log_hygiene::HYGIENE_BACKUP_MARK)).collect()
}

/// V1: a 0.4.1 database is migrated up once, backed up first: the pruned
/// chunks' witnesses become tombstones, the answered turns are marked as
/// handled and the unanswered one stays eligible; the chunks 0.4.1 pruned
/// never come back; the next prune takes the rest. Idempotent.
#[test]
fn a_0_4_1_database_migrates_up_once_with_a_backup() {
    let f = v041_copy();
    let (wid, chat) = (id(&f, "workspace"), id(&f, "chat"));
    let mut store = EventStore::open(&f.db).unwrap();
    assert_eq!(store.schema_version().unwrap(), SYNC_SCHEMA_VERSION, "a 0.4.1 database");
    let at = fixture_time(&store, &f) + HOUR_MS;
    let events = rows(&store, "SELECT COUNT(*) FROM events");
    assert_eq!(store.migrate_sync_tables(&[]).unwrap(), SyncMigration::AlreadyDone);
    assert_eq!(store.prune_if_due_at(at).unwrap(), None, "pruned before the tombstones");
    let HygieneMigration::Migrated { tombstones, processed, backup } = store.migrate_log_hygiene_at(at).unwrap() else { panic!("not migrated") };
    assert_eq!(tombstones, f.pruned.len());
    assert_eq!(processed, 4, "q1, r1, q2 and r2");
    let backup = backup.expect("a backup");
    // Compare real paths: the store names its file by the real path, the
    // listing by the temp dir's (on macOS /var is a link to /private/var).
    let real = |p: &Path| std::fs::canonicalize(p).unwrap();
    assert_eq!(backups(&f.db).iter().map(|p| real(p.as_path())).collect::<Vec<_>>(), vec![real(&backup)]);
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    // The backup is the 0.4.1 database as it was.
    let old = rusqlite::Connection::open(&backup).unwrap();
    assert_eq!(old.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap(), SYNC_SCHEMA_VERSION);
    assert_eq!(old.query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0)).unwrap(), events);
    // Nothing in the log changed, and the sync state is kept.
    assert_eq!(rows(&store, "SELECT COUNT(*) FROM events"), events);
    assert_eq!(store.sync_cursor(&SyncKey::new("https://relay.example", wid)).unwrap(), 7);
    // The chunks 0.4.1 pruned, fetched again: never stored.
    for c in &f.pruned {
        assert!(!store.ingest(c).unwrap(), "a chunk 0.4.1 pruned came back");
    }
    assert_eq!(rows(&store, "SELECT COUNT(*) FROM events"), events);
    // Dispatch takes the unanswered turn only, once.
    let taken = store.take_dispatch_candidates_at(i64::MAX, at).unwrap();
    assert_eq!(taken.iter().map(|c| c.2).collect::<Vec<_>>(), vec![id(&f, "q3")]);
    assert!(store.take_dispatch_candidates_at(i64::MAX, at).unwrap().is_empty());
    // The prune runs now, behind tombstones.
    assert_eq!(store.prune_if_due_at(at).unwrap(), Some(2), "the second reply's chunks");
    let s = store.load_session_in(wid, chat).unwrap().unwrap();
    let bodies: Vec<&str> = s.messages.iter().map(|m| m.body.as_str()).collect();
    assert_eq!(bodies, vec!["first question", "one two three", "second question", "four five", "still waiting"]);
    drop(store);
    // Again: nothing to do, and no second backup.
    let store = EventStore::open(&f.db).unwrap();
    assert_eq!(store.migrate_log_hygiene_at(at).unwrap(), HygieneMigration::AlreadyDone);
    assert_eq!(backups(&f.db).len(), 1);
    // The migrated database still takes new writes from its device.
    let mut svc = ChatService::new(
        store,
        SigningKeypair::from_seed(&[0x41; 32]).unwrap(),
        SigningKeypair::from_seed(&[0x42; 32]).unwrap(),
        ActorIdentity::new("alice", "Alice", ActorKind::Human),
    );
    svc.set_author_account(Uuid::from_u128(0xa11ce).to_string(), "Alice", None);
    svc.post_user_message(chat, wid, "after the upgrade").unwrap();
    assert_eq!(svc.load_in(wid, chat).unwrap().unwrap().messages.last().unwrap().body, "after the upgrade");
}

/// V1: a failed migration leaves the 0.4.1 database as it was and no
/// backup; the next start runs it.
#[test]
fn a_failed_migration_changes_nothing_and_runs_again() {
    let f = v041_copy();
    let store = EventStore::open(&f.db).unwrap();
    let at = fixture_time(&store, &f) + HOUR_MS;
    test_hooks::FAIL_STEP.with(|s| s.set(Some("m10")));
    let failed = store.migrate_log_hygiene_at(at);
    test_hooks::FAIL_STEP.with(|s| s.set(None));
    assert!(failed.is_err());
    assert_eq!(store.schema_version().unwrap(), SYNC_SCHEMA_VERSION);
    assert_eq!(store.tombstone_count().unwrap(), 0);
    assert_eq!(rows(&store, "SELECT COUNT(*) FROM dispatch_processed"), 0);
    assert!(backups(&f.db).is_empty(), "a failed migration left its backup");
    test_hooks::FAIL_STEP.with(|s| s.set(Some("m8-backup")));
    assert!(store.migrate_log_hygiene_at(at).is_err());
    test_hooks::FAIL_STEP.with(|s| s.set(None));
    assert!(backups(&f.db).is_empty());
    assert!(matches!(store.migrate_log_hygiene_at(at).unwrap(), HygieneMigration::Migrated { .. }));
}

/// M8 waits while another connection holds the store (as M1 does): the
/// tables are there, the version stays, and dispatch reads only what each
/// pull stored.
#[test]
fn the_migration_waits_for_the_store_alone_and_dispatch_keeps_to_the_pull() {
    let f = v041_copy();
    let store = EventStore::open(&f.db).unwrap();
    let other = EventStore::open(&f.db).unwrap();
    let at = fixture_time(&store, &f) + HOUR_MS;
    assert_eq!(store.migrate_log_hygiene_at(at).unwrap(), HygieneMigration::Busy);
    assert_eq!(store.schema_version().unwrap(), SYNC_SCHEMA_VERSION);
    let after = store.max_row_id().unwrap();
    assert!(store.take_dispatch_candidates_at(after, at).unwrap().is_empty(), "the stored messages were taken before M10");
    drop(other);
    assert!(matches!(store.migrate_log_hygiene_at(at).unwrap(), HygieneMigration::Migrated { .. }));
}

/// V1: a build that knows only version 1 (0.4.x) opens a version-2
/// database read-only, with the newer-data message, and changes nothing.
/// (`deploy/tests/store-compat-test.sh` runs the released 0.4.1 binary on
/// a migrated data dir.)
#[test]
fn a_version_2_database_is_newer_data_to_a_version_1_build() {
    let f = v041_copy();
    let store = EventStore::open(&f.db).unwrap();
    let at = fixture_time(&store, &f) + HOUR_MS;
    assert!(matches!(store.migrate_log_hygiene_at(at).unwrap(), HygieneMigration::Migrated { .. }));
    drop(store);
    // What 0.4.1's `EventStore::open` reads first.
    let c = rusqlite::Connection::open(&f.db).unwrap();
    let v: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert!(v > SYNC_SCHEMA_VERSION, "0.4.1 opens it read-write");
}

/// A new database starts at version 2 with nothing to migrate.
#[test]
fn a_new_database_starts_at_version_2() {
    let dev = Device::new();
    let svc = dev.service();
    assert_eq!(svc.store().schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(svc.store().migrate_log_hygiene().unwrap(), HygieneMigration::AlreadyDone);
    assert!(backups(&dev.path).is_empty());
}

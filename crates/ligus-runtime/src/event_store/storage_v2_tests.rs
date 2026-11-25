//! Identity model v2 storage: tests M1t, M1c, M2t and M4t (design note,
//! section 16). M1d, which needs two relays, is in `sync_engine`'s tests.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ligus_core::{crypto::SigningKeypair, ActorIdentity, ActorKind};
use rusqlite::Connection;
use uuid::Uuid;
use zeroize::Zeroizing;

use super::*;
use crate::chat_service::ChatService;
use crate::secret_store::{Backend, File0600, SecretError, SecretId, SecretStore};

/// A data dir whose secrets are owner-only files (never a keychain), with
/// that store registered as the host would.
fn data_dir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    crate::secret_store::set_for_data_dir(dir.path(), Arc::new(File0600::new(dir.path())));
    let db = dir.path().join("ligus.db");
    (dir, db)
}

fn files(dir: &Path) -> Arc<dyn SecretStore> {
    Arc::new(File0600::new(dir))
}

/// What an older build (0.3) does to the file: plain SQL, no lock file, no
/// secret store.
fn old_build(db: &Path) -> Connection {
    let c = Connection::open(db).unwrap();
    c.busy_timeout(std::time::Duration::from_secs(10)).unwrap();
    c
}

fn hex(seed: &[u8; 32]) -> String {
    seed.iter().map(|b| format!("{b:02x}")).collect()
}

fn contains(path: &Path, needle: &[u8]) -> bool {
    std::fs::read(path).map(|b| b.windows(needle.len()).any(|w| w == needle)).unwrap_or(false)
}

fn wal(db: &Path) -> PathBuf {
    let mut p = db.as_os_str().to_os_string();
    p.push("-wal");
    PathBuf::from(p)
}

fn count(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn svc_on(path: &Path, id: &str, name: &str) -> ChatService {
    let store = EventStore::open(path).unwrap();
    let author = ActorIdentity::new(id, name, ActorKind::Human);
    let mut svc = ChatService::new(store, SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), author);
    svc.set_author_account(Uuid::new_v4().to_string(), name, None);
    svc
}

/// A 0.3 database: sync state for `team` in the room-keyed tables, and the
/// version 0.3 leaves (none).
fn legacy_sync_state(db: &Path) -> (Uuid, Uuid) {
    drop(EventStore::open(db).unwrap());
    let (e1, e2) = (Uuid::new_v4(), Uuid::new_v4());
    let c = old_build(db);
    c.execute_batch(&format!(
        "INSERT INTO sync_cursor (workspace, last_fetched_seq) VALUES ('team', 42);
         INSERT INTO pushed (workspace, event_id) VALUES ('team', '{e1}'), ('team', '{e2}');
         INSERT INTO parked_bodies (workspace, seq, body, priority) VALUES ('team', 5, '{{\"version\":9}}', 1);
         PRAGMA user_version = 0;"
    ))
    .unwrap();
    (e1, e2)
}

fn team_key(relay: &str) -> SyncKey {
    SyncKey::new(relay, ligus_core::room_workspace_id("team"))
}

#[test]
fn relay_origins_have_one_form() {
    assert_eq!(relay_origin("https://Relay.Example/"), "https://relay.example");
    assert_eq!(relay_origin("https://relay.example:443"), "https://relay.example");
    assert_eq!(relay_origin("http://127.0.0.1:8443/"), "http://127.0.0.1:8443");
    assert_eq!(relay_origin("https://user:pw@relay.example/r/?q=1#f"), "https://relay.example/r");
    assert_ne!(relay_origin("https://host.example/a"), relay_origin("https://host.example/b"), "two relays under one host");
}

/// A database made by this build has nothing for M1 to move: it is at the
/// current version from the start, and M1 is a no-op.
#[test]
fn a_new_database_starts_at_the_current_version() {
    let (_dir, db) = data_dir();
    let store = EventStore::open(&db).unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(store.migrate_sync_tables(&[]).unwrap(), SyncMigration::AlreadyDone);
    assert!(seed_backups(&db).is_empty());
}

/// M1t: the backup exists and opens, the state of a room exactly one
/// connection names is copied, the old tables are left as they were, the
/// version is set, and a second run is a no-op.
#[test]
fn m1_copies_a_single_relay_room_after_a_backup_and_runs_once() {
    let (dir, db) = data_dir();
    let (e1, e2) = legacy_sync_state(&db);
    let store = EventStore::open(&db).unwrap();
    assert_eq!(store.schema_version().unwrap(), 0, "a database with old sync state waits for M1");
    let relay = "https://relay.example/";
    let conns = vec![(relay.to_string(), "team".to_string()), (String::new(), "local-only".to_string())];
    let SyncMigration::Migrated { copied, reset, backup } = store.migrate_sync_tables(&conns).unwrap() else { panic!("not migrated") };
    assert_eq!((copied, reset), (vec!["team".to_string()], vec![]));
    let backup = backup.expect("a backup");
    assert!(backup.to_string_lossy().contains(SYNC_BACKUP_MARK));
    // The backup opens and holds the old state as it was.
    let copy = Connection::open_with_flags(&backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(count(&copy, "SELECT last_fetched_seq FROM sync_cursor WHERE workspace = 'team'"), 42);
    assert_eq!(count(&copy, "SELECT COUNT(*) FROM sync_cursor_v2"), 0);
    drop(copy);
    let listed: Vec<_> = seed_backups(&db).iter().map(|p| p.file_name().unwrap().to_owned()).collect();
    assert_eq!(listed, vec![backup.file_name().unwrap().to_owned()], "doctor lists it");

    let key = team_key("https://RELAY.example");
    assert_eq!(store.sync_cursor(&key).unwrap(), 42);
    let mut pushed = store.pushed_ids(&key).unwrap();
    pushed.sort();
    let mut want = vec![e1, e2];
    want.sort();
    assert_eq!(pushed, want);
    assert_eq!(store.parked_bodies(&key).unwrap().len(), 1);
    assert!(store.legacy_parked_bodies("team").unwrap().is_empty(), "a copied room's bodies are drained from the copy");
    // Another relay's state for the same workspace starts at 0.
    assert_eq!(store.sync_cursor(&team_key("https://other.example")).unwrap(), 0);
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);

    // The old tables are unchanged, for a 0.3 process or a downgrade.
    let old = old_build(&db);
    assert_eq!(count(&old, "SELECT last_fetched_seq FROM sync_cursor WHERE workspace = 'team'"), 42);
    assert_eq!(count(&old, "SELECT COUNT(*) FROM pushed WHERE workspace = 'team'"), 2);
    assert_eq!(count(&old, "SELECT COUNT(*) FROM parked_bodies WHERE workspace = 'team'"), 1);

    // A second run (this start or the next) does nothing and takes no backup.
    assert_eq!(store.migrate_sync_tables(&conns).unwrap(), SyncMigration::AlreadyDone);
    drop(store);
    let again = EventStore::open(&db).unwrap();
    assert_eq!(again.migrate_sync_tables(&conns).unwrap(), SyncMigration::AlreadyDone);
    assert_eq!(seed_backups(&db).len(), 1);
    // Nothing removes the backup by itself.
    assert!(backup.exists());
    drop(dir);
}

/// M1t: a failure part way (after a room's cursor and pushed set were
/// copied, before its parked bodies) leaves the database as it was and
/// removes the backup; a backup that fails leaves nothing behind. The next
/// start migrates.
#[test]
fn m1_failing_part_way_leaves_the_database_as_it_was() {
    let (_dir, db) = data_dir();
    legacy_sync_state(&db);
    let store = EventStore::open(&db).unwrap();
    let conns = vec![("https://relay.example".to_string(), "team".to_string())];
    for step in ["m1", "m1-backup"] {
        super::super::test_hooks::FAIL_STEP.with(|f| f.set(Some(step)));
        assert!(store.migrate_sync_tables(&conns).is_err(), "{step}");
        super::super::test_hooks::FAIL_STEP.with(|f| f.set(None));
        assert_eq!(store.schema_version().unwrap(), 0, "{step}: the version is unchanged");
        assert_eq!(store.sync_cursor(&team_key("https://relay.example")).unwrap(), 0, "{step}: nothing copied");
        let c = old_build(&db);
        assert_eq!(count(&c, "SELECT COUNT(*) FROM pushed_v2") + count(&c, "SELECT COUNT(*) FROM sync_v2_rooms"), 0, "{step}");
        assert_eq!(count(&c, "SELECT last_fetched_seq FROM sync_cursor WHERE workspace = 'team'"), 42, "{step}");
        assert!(seed_backups(&db).is_empty(), "{step}: the partial backup is removed");
        assert!(store.legacy_parked_bodies("team").unwrap().len() == 1, "{step}: the old path still drains it");
    }
    assert!(matches!(store.migrate_sync_tables(&conns).unwrap(), SyncMigration::Migrated { .. }));
    assert_eq!(store.sync_cursor(&team_key("https://relay.example")).unwrap(), 42);
}

/// M1d, store level: two connections naming one room on two relays copy no
/// cursor or pushed set; both relays sync again from 0, and the room's old
/// parked bodies stay in the old table for the old path.
#[test]
fn m1_copies_nothing_for_a_room_on_two_relays() {
    let (_dir, db) = data_dir();
    legacy_sync_state(&db);
    let store = EventStore::open(&db).unwrap();
    let conns = vec![("https://a.example".to_string(), "team".to_string()), ("https://b.example".to_string(), "team".to_string())];
    let SyncMigration::Migrated { copied, reset, .. } = store.migrate_sync_tables(&conns).unwrap() else { panic!("not migrated") };
    assert_eq!((copied, reset), (vec![], vec!["team".to_string()]));
    for relay in ["https://a.example", "https://b.example"] {
        assert_eq!(store.sync_cursor(&team_key(relay)).unwrap(), 0, "{relay} starts from 0");
        assert!(store.pushed_ids(&team_key(relay)).unwrap().is_empty());
        assert!(store.parked_bodies(&team_key(relay)).unwrap().is_empty());
    }
    assert_eq!(store.legacy_parked_bodies("team").unwrap().len(), 1, "drained by the old path");
    // A room no connection names now isn't copied either.
    let (_dir2, db2) = data_dir();
    legacy_sync_state(&db2);
    let lone = EventStore::open(&db2).unwrap();
    let SyncMigration::Migrated { reset, .. } = lone.migrate_sync_tables(&[]).unwrap() else { panic!() };
    assert_eq!(reset, vec!["team".to_string()]);
}

/// The lock file: another 0.4 connection to the store (another process, or
/// another connection in this one) makes M1 wait for a later start.
#[test]
fn m1_waits_while_another_connection_holds_the_store() {
    let (_dir, db) = data_dir();
    legacy_sync_state(&db);
    let store = EventStore::open(&db).unwrap();
    let other = EventStore::open(&db).unwrap();
    let conns = vec![("https://relay.example".to_string(), "team".to_string())];
    assert_eq!(store.migrate_sync_tables(&conns).unwrap(), SyncMigration::Busy);
    assert_eq!(store.schema_version().unwrap(), 0);
    assert!(seed_backups(&db).is_empty(), "no backup while it waits");
    // Sync works meanwhile: from 0, in the new tables.
    store.set_sync_cursor(&team_key("https://relay.example"), 3).unwrap();
    drop(other);
    assert!(matches!(store.migrate_sync_tables(&conns).unwrap(), SyncMigration::Migrated { .. }));
    // A cursor kept since stays; the old one isn't copied over it.
    assert_eq!(store.sync_cursor(&team_key("https://relay.example")).unwrap(), 3);
    // Still open shared afterwards: a new connection opens straight away.
    drop(EventStore::open(&db).unwrap());
}

/// M1c (store level): a 0.3 process writes the old tables (and a seed of a
/// live invite) while a 0.4 process starts, moves seeds and runs M1. Every
/// old-table write lands and stays usable, and no live seed is deleted.
#[test]
fn m1c_an_older_process_writing_beside_the_upgrade() {
    let (dir, db) = data_dir();
    legacy_sync_state(&db);
    // A person invite, open on the log.
    let mut owner = svc_on(&db, "o", "Ola");
    let ws = Uuid::new_v4();
    owner.ensure_workspace_config(ws).unwrap();
    let invite = owner.issue_invite(ws).unwrap();
    let seed = owner.store().issued_invite_key(ws, invite).unwrap().unwrap();
    drop(owner);

    let writer_db = db.clone();
    let writer = std::thread::spawn(move || {
        let c = old_build(&writer_db);
        // A box's redeem key the older process writes for an admission it
        // is about to redeem (nothing on this device's log closes it yet).
        c.execute("INSERT INTO join_redeem_keys (workspace_id, seed) VALUES (?1, ?2)", [Uuid::new_v4().to_string(), hex(&[6u8; 32])]).unwrap();
        for i in 0..200 {
            c.execute("INSERT INTO sync_cursor (workspace, last_fetched_seq) VALUES ('team', ?1) ON CONFLICT(workspace) DO UPDATE SET last_fetched_seq = excluded.last_fetched_seq", [100 + i]).unwrap();
            c.execute("INSERT OR IGNORE INTO pushed (workspace, event_id) VALUES ('team', ?1)", [Uuid::new_v4().to_string()]).unwrap();
            c.execute("INSERT OR REPLACE INTO parked_bodies (workspace, seq, body, priority) VALUES ('later', ?1, '{}', 0)", [i]).unwrap();
        }
    });
    for _ in 0..20 {
        let store = EventStore::open(&db).unwrap();
        store.move_seeds().unwrap();
        let _ = store.migrate_sync_tables(&[("https://relay.example".to_string(), "team".to_string())]).unwrap();
    }
    writer.join().unwrap();
    let store = EventStore::open(&db).unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    let old = old_build(&db);
    assert_eq!(count(&old, "SELECT last_fetched_seq FROM sync_cursor WHERE workspace = 'team'"), 299, "the older process's last write");
    assert_eq!(count(&old, "SELECT COUNT(*) FROM pushed WHERE workspace = 'team'"), 202);
    assert_eq!(count(&old, "SELECT COUNT(*) FROM parked_bodies WHERE workspace = 'later'"), 200);
    // The live seeds are kept: the older process's in its table (and the
    // store), this build's in the store only.
    assert_eq!(count(&old, "SELECT COUNT(*) FROM issued_invite_keys"), 0);
    assert_eq!(count(&old, "SELECT COUNT(*) FROM join_redeem_keys"), 1);
    assert_eq!(files(dir.path()).get(&invite_secret(ws, invite)).unwrap().map(|v| v.to_vec()), Some(seed.to_vec()));
    // A room only the older process synced, after M1: drained from the old
    // table, never mixed into the new one.
    assert_eq!(store.legacy_parked_bodies("later").unwrap().len(), 200);
}

/// M2t: a seed is copied to the secret store and read back, whoever wrote
/// it (this build writes the store only; a 0.3 row is copied at the next
/// start); reads prefer the store.
#[test]
fn m2_seeds_are_copied_and_read_back() {
    let (dir, db) = data_dir();
    let store = EventStore::open(&db).unwrap();
    let (ws, inv, seed) = (Uuid::new_v4(), Uuid::new_v4(), [7u8; 32]);
    store.set_issued_invite_key(ws, inv, &seed).unwrap();
    let secrets = files(dir.path());
    assert_eq!(secrets.get(&invite_secret(ws, inv)).unwrap().map(|v| v.to_vec()), Some(seed.to_vec()));
    let old = old_build(&db);
    assert_eq!(count(&old, "SELECT COUNT(*) FROM issued_invite_keys"), 0, "this build's seed never reaches the table");

    // A 0.3 process writes a box's redeem key: copied on the next start.
    let (ws2, seed2) = (Uuid::new_v4(), [9u8; 32]);
    old.execute("INSERT INTO join_redeem_keys (workspace_id, seed) VALUES (?1, ?2)", [ws2.to_string(), hex(&seed2)]).unwrap();
    assert!(secrets.get(&redeem_secret(ws2)).unwrap().is_none());
    let (copied, deleted) = store.move_seeds().unwrap();
    assert_eq!((copied, deleted), (1, 0));
    assert_eq!(secrets.get(&redeem_secret(ws2)).unwrap().map(|v| v.to_vec()), Some(seed2.to_vec()));
    assert_eq!(store.join_redeem_key(ws2).unwrap(), Some(seed2));
    // The store's copy answers once the row is gone (a row an older build
    // removed); the table answers without a store.
    old.execute("DELETE FROM join_redeem_keys", []).unwrap();
    assert_eq!(store.join_redeem_key(ws2).unwrap(), Some(seed2));
    let mut bare = EventStore::open(&db).unwrap();
    bare.set_secret_store(None);
    assert_eq!(bare.join_redeem_key(ws2).unwrap(), None);
    // A seed this build issued lives in the store only: without one (or on
    // 0.3) it isn't found, and that invite's code can't be built (design 12).
    assert_eq!(bare.issued_invite_key(ws, inv).unwrap(), None);
    // Taking a seed forgets it in both places.
    assert_eq!(store.take_issued_invite_key(ws, inv).unwrap(), Some(seed));
    assert!(secrets.get(&invite_secret(ws, inv)).unwrap().is_none());
    assert_eq!(count(&old, "SELECT COUNT(*) FROM issued_invite_keys"), 0);
    // Clearing one does too.
    store.set_join_redeem_key(ws2, &seed2).unwrap();
    store.clear_join_redeem_key(ws2).unwrap();
    assert!(secrets.get(&redeem_secret(ws2)).unwrap().is_none());
    assert_eq!(store.join_redeem_key(ws2).unwrap(), None);
}

/// M2t: a person invite's seed stays while the invite is open on the log and
/// goes once its redemption closes it: from the table with secure delete,
/// and from the store. Afterwards no byte of it is in the database file or
/// its write-ahead log.
#[test]
fn m2_a_seed_stays_while_its_invite_is_open_and_goes_once_it_closes() {
    let (dir, db) = data_dir();
    let ws = Uuid::new_v4();
    let cfg = ligus_core::workspace_config_session_id(ws);
    let mut owner = svc_on(&db, "o", "Ola");
    owner.create_chat("general", ws, "rt").unwrap();
    owner.ensure_workspace_config(ws).unwrap();
    let invite = owner.issue_invite(ws).unwrap();
    let seed = owner.store().issued_invite_key(ws, invite).unwrap().unwrap();
    let secrets = files(dir.path());

    // Open: kept, however often M2 runs. This build wrote it to the secret
    // store only: no row to copy, nothing in the database.
    for _ in 0..2 {
        assert_eq!(owner.store().move_seeds().unwrap(), (0, 0));
    }
    assert_eq!(owner.store().issued_invite_key(ws, invite).unwrap(), Some(seed));
    assert!(secrets.get(&invite_secret(ws, invite)).unwrap().is_some());

    // The invitee redeems it (the code carried the seed).
    let mut joiner = svc_on(&db, "j", "Jo");
    joiner.store().set_join_invite(ws, invite).unwrap();
    joiner.store().set_join_redeem(ws, Some(&seed)).unwrap();
    assert!(joiner.ensure_self_member(cfg, ws).unwrap(), "redeemed");
    drop(joiner);
    drop(owner);

    let store = EventStore::open(&db).unwrap();
    let (_, deleted) = store.move_seeds().unwrap();
    assert_eq!(deleted, 1, "the closed invite's seed goes");
    assert_eq!(store.issued_invite_key(ws, invite).unwrap(), None);
    assert!(secrets.get(&invite_secret(ws, invite)).unwrap().is_none());
    assert!(secrets.get(&redeem_secret(ws)).unwrap().is_none(), "the invitee's copy went at redemption");
    drop(store);
    for file in [db.clone(), wal(&db)] {
        assert!(!contains(&file, hex(&seed).as_bytes()), "{} holds the seed", file.display());
        assert!(!contains(&file, &seed), "{} holds the seed's bytes", file.display());
    }
}

/// M2t: a box's admission seed goes once the admission expires.
#[test]
fn m2_a_box_admission_seed_goes_when_it_expires() {
    let (dir, db) = data_dir();
    let ws = Uuid::new_v4();
    let mut owner = svc_on(&db, "o", "Ola");
    owner.enable_test_presence();
    owner.create_chat("general", ws, "rt").unwrap();
    owner.ensure_workspace_config(ws).unwrap();
    let ttl = 60 * 60 * 1000;
    let issued = now_ms();
    let (invite, seed) = owner.issue_box_admission(ws, ttl, None).unwrap();
    // The box's side, in its store.
    owner.store().set_join_invite(ws, invite).unwrap();
    owner.store().set_join_redeem_key(ws, &seed).unwrap();
    assert_eq!(owner.store().move_seeds_at(issued + ttl / 2).unwrap(), (0, 0), "kept while it can be redeemed");
    assert_eq!(owner.store().move_seeds_at(issued + ttl + 60_000).unwrap(), (0, 1), "gone once expired");
    assert!(files(dir.path()).get(&redeem_secret(ws)).unwrap().is_none());
    assert_eq!(owner.store().join_redeem_key(ws).unwrap(), None);
}

/// M2t: a person invite nothing closes (never redeemed) loses its seed 7
/// days after it was first copied, not before.
#[test]
fn m2_a_person_invite_never_redeemed_goes_after_seven_days() {
    let (_dir, db) = data_dir();
    let ws = Uuid::new_v4();
    let mut owner = svc_on(&db, "o", "Ola");
    owner.ensure_workspace_config(ws).unwrap();
    let copied = now_ms();
    let invite = owner.issue_invite(ws).unwrap();
    let store = owner.store();
    assert_eq!(store.move_seeds_at(copied + PERSON_SEED_KEEP_MS - 60_000).unwrap(), (0, 0));
    assert!(store.issued_invite_key(ws, invite).unwrap().is_some());
    assert_eq!(store.move_seeds_at(copied + PERSON_SEED_KEEP_MS + 60_000).unwrap(), (0, 1));
    assert!(store.issued_invite_key(ws, invite).unwrap().is_none());
    // A seed this device never saw on any log ages out the same way.
    let lone = Uuid::new_v4();
    store.set_join_redeem_key(lone, &[3u8; 32]).unwrap();
    assert_eq!(store.move_seeds_at(now_ms() + PERSON_SEED_KEEP_MS + 60_000).unwrap().1, 1);
}

struct Broken;

impl SecretStore for Broken {
    fn backend(&self) -> Backend {
        Backend::Unavailable
    }
    fn get(&self, _: &SecretId) -> std::result::Result<Option<Zeroizing<Vec<u8>>>, SecretError> {
        Err(SecretError::ReadOnly("test"))
    }
    fn put(&self, _: &SecretId, _: &[u8]) -> std::result::Result<(), SecretError> {
        Err(SecretError::ReadOnly("test"))
    }
    fn delete(&self, _: &SecretId) -> std::result::Result<(), SecretError> {
        Err(SecretError::ReadOnly("test"))
    }
}

/// M2t: a secret store that fails leaves the row where it is (even for an
/// invite long closed) and the seed is still read from it.
#[test]
fn m2_a_failing_secret_store_leaves_the_row() {
    let dir = tempfile::tempdir().unwrap();
    crate::secret_store::set_for_data_dir(dir.path(), Arc::new(Broken));
    let db = dir.path().join("ligus.db");
    let store = EventStore::open(&db).unwrap();
    let (ws, inv, seed) = (Uuid::new_v4(), Uuid::new_v4(), [5u8; 32]);
    store.set_issued_invite_key(ws, inv, &seed).unwrap();
    store.set_join_redeem_key(ws, &seed).unwrap();
    assert_eq!(store.move_seeds_at(now_ms() + 30 * PERSON_SEED_KEEP_MS).unwrap(), (0, 0));
    let old = old_build(&db);
    assert_eq!(count(&old, "SELECT COUNT(*) FROM issued_invite_keys") + count(&old, "SELECT COUNT(*) FROM join_redeem_keys"), 2);
    assert_eq!(store.issued_invite_key(ws, inv).unwrap(), Some(seed));
    assert_eq!(store.join_redeem_key(ws).unwrap(), Some(seed));
}

/// M2t: a row a 0.3 process rewrites (a new seed for the same workspace)
/// overwrites the store's copy: on the next read, and on the next start.
#[test]
fn m2_a_row_an_older_process_rewrites_overwrites_the_store_copy() {
    let (dir, db) = data_dir();
    let store = EventStore::open(&db).unwrap();
    let secrets = files(dir.path());
    let ws = Uuid::new_v4();
    store.set_join_redeem_key(ws, &[1u8; 32]).unwrap();
    let old = old_build(&db);
    // A 0.3 process on this data dir writes its own seed to the table.
    old.execute("INSERT OR REPLACE INTO join_redeem_keys (workspace_id, seed) VALUES (?1, ?2)", [ws.to_string(), hex(&[2u8; 32])]).unwrap();
    assert_eq!(store.move_seeds().unwrap(), (1, 0));
    assert_eq!(secrets.get(&redeem_secret(ws)).unwrap().map(|v| v.to_vec()), Some(vec![2u8; 32]), "the start copied the newer row");
    old.execute("UPDATE join_redeem_keys SET seed = ?2 WHERE workspace_id = ?1", [ws.to_string(), hex(&[4u8; 32])]).unwrap();
    assert_eq!(store.join_redeem_key(ws).unwrap(), Some([4u8; 32]), "a read prefers the newer row");
    assert_eq!(secrets.get(&redeem_secret(ws)).unwrap().map(|v| v.to_vec()), Some(vec![4u8; 32]), "and overwrites the copy");
}

/// M2: the first open in a process moves the seeds by itself (the host
/// registered its secret store first).
#[test]
fn m2_runs_on_the_first_open() {
    let (dir, db) = data_dir();
    drop(EventStore::open(&db).unwrap());
    let ws = Uuid::new_v4();
    old_build(&db).execute("INSERT INTO join_redeem_keys (workspace_id, seed) VALUES (?1, ?2)", [ws.to_string(), hex(&[8u8; 32])]).unwrap();
    // This process already opened it: a fresh path is a fresh start.
    let fresh = dir.path().join("second.db");
    std::fs::copy(&db, &fresh).unwrap();
    let _ = std::fs::copy(wal(&db), wal(&fresh));
    drop(EventStore::open(&fresh).unwrap());
    assert_eq!(files(dir.path()).get(&redeem_secret(ws)).unwrap().map(|v| v.to_vec()), Some(vec![8u8; 32]));
}

/// The backups next to the database are listed (they hold seeds), with
/// their side files left out; nothing deletes them.
#[test]
fn seed_backups_lists_full_copies_only() {
    let (dir, db) = data_dir();
    drop(EventStore::open(&db).unwrap());
    for name in ["ligus.db.bak-ws-event-ids-1-2-3", "ligus.db.bak-ws-event-ids-1-2-3-wal", "ligus.db.bak-foreign-purge-4", "other.db.bak-x"] {
        std::fs::write(dir.path().join(name), b"x").unwrap();
    }
    let names: Vec<String> = seed_backups(&db).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(names, vec!["ligus.db.bak-foreign-purge-4", "ligus.db.bak-ws-event-ids-1-2-3"]);
}

/// M4t: a database with a version above this build's opens read-only and
/// says so; nothing is changed, M1 doesn't run, and reads still work.
#[test]
fn m4_a_newer_database_opens_read_only() {
    let (_dir, db) = data_dir();
    let key = team_key("https://relay.example");
    {
        let store = EventStore::open(&db).unwrap();
        store.set_sync_cursor(&key, 7).unwrap();
    }
    old_build(&db).execute_batch(&format!("PRAGMA user_version = {}", SCHEMA_VERSION + 1)).unwrap();
    let store = EventStore::open(&db).unwrap();
    assert_eq!(store.newer_schema(), Some(SCHEMA_VERSION + 1));
    assert_eq!(store.sync_cursor(&key).unwrap(), 7, "reads work");
    assert!(store.set_sync_cursor(&key, 8).is_err(), "writes are refused");
    assert_eq!(store.migrate_sync_tables(&[]).unwrap(), SyncMigration::ReadOnly);
    assert_eq!(store.move_seeds().unwrap(), (0, 0));
    drop(store);
    let c = old_build(&db);
    assert_eq!(count(&c, "PRAGMA user_version"), SCHEMA_VERSION + 1);
    assert_eq!(count(&c, "SELECT last_fetched_seq FROM sync_cursor_v2"), 7);
    assert!(NEWER_DATA_MESSAGE.contains("newer Ligus"));
}

/// Design 10.3: a seed this build writes, with a secret store, never
/// reaches the database file or its log.
#[test]
fn a_new_seed_never_reaches_sqlite() {
    let (dir, db) = data_dir();
    let mut store = EventStore::open(&db).unwrap();
    store.set_secret_store(Some(files(dir.path())));
    let (ws, invite) = (Uuid::new_v4(), Uuid::new_v4());
    let seed = [0x3cu8; 32];
    store.set_issued_invite_key(ws, invite, &seed).unwrap();
    store.set_join_redeem_key(ws, &seed).unwrap();
    assert_eq!(store.issued_invite_key(ws, invite).unwrap(), Some(seed));
    assert_eq!(store.join_redeem_key(ws).unwrap(), Some(seed));
    drop(store);
    let mut wal = db.as_os_str().to_os_string();
    wal.push("-wal");
    assert!(!contains(&db, hex(&seed).as_bytes()), "a new seed is in the database file");
    assert!(!contains(Path::new(&wal), hex(&seed).as_bytes()), "a new seed is in the log");
}

/// Review S1: a seed deleted while another connection reads stays only
/// until the next start, which truncates the log again.
#[test]
fn a_seed_deleted_beside_a_reader_is_gone_after_the_next_start() {
    let (dir, db) = data_dir();
    let mut store = EventStore::open(&db).unwrap();
    store.set_secret_store(None);
    let (ws, invite) = (Uuid::new_v4(), Uuid::new_v4());
    let seed = [0x5au8; 32];
    store.set_issued_invite_key(ws, invite, &seed).unwrap(); // a 0.3-style row
    store.set_secret_store(Some(files(dir.path())));
    store.move_seeds_at(now_ms()).unwrap(); // copied now
    {
        let c = Connection::open(&db).unwrap();
        let _ = c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    }
    let reader = Connection::open(&db).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let _: i64 = reader.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0)).unwrap();
    let later = now_ms() + 8 * 86_400_000;
    let (_, deleted) = store.move_seeds_at(later).unwrap();
    assert_eq!(deleted, 1);
    reader.execute_batch("COMMIT").unwrap();
    drop(reader);
    drop(store);
    let mut again = EventStore::open(&db).unwrap();
    again.set_secret_store(Some(files(dir.path())));
    again.move_seeds_at(later).unwrap();
    drop(again);
    let mut wal = db.as_os_str().to_os_string();
    wal.push("-wal");
    assert!(!contains(&db, hex(&seed).as_bytes()), "the database file holds the deleted seed");
    assert!(!contains(Path::new(&wal), hex(&seed).as_bytes()), "the log holds the deleted seed");
}

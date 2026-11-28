//! The local tables of `user_version` 2 (workspace model v3, 9, 13, 16.2
//! M9; invariants E5, V1 and V3): the scratch registry and the workspace
//! folders move from a blob and files into tables, once, with a backup.

use std::path::{Path, PathBuf};

use uuid::Uuid;

use super::local_tables::{LOCAL_BACKUP_MARK, SCRATCH_BLOB_KEY, SCRATCH_IMPORT};
use super::*;
use crate::repos::scratch;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/db-v041")
}

struct Db {
    _dir: tempfile::TempDir,
    db: PathBuf,
}

/// A copy of the 0.4.1 release's database (version 1).
fn v041() -> Db {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ligus.db");
    std::fs::copy(fixture_dir().join("ligus.db"), &db).unwrap();
    Db { _dir: dir, db }
}

/// A database as a 0.5 pre-release left it: version 2 (M8 ran) but none of
/// the local tables yet.
fn v05_pre() -> Db {
    let d = v041();
    let store = EventStore::open(&d.db).unwrap();
    assert!(matches!(store.migrate_log_hygiene().unwrap(), HygieneMigration::Migrated { .. }));
    drop(store);
    let c = rusqlite::Connection::open(&d.db).unwrap();
    c.execute_batch("DROP TABLE scratch_repos; DROP TABLE scratch_repo_chats; DROP TABLE workspace_folders; DROP TABLE local_imports;").unwrap();
    assert_eq!(c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap(), SCHEMA_VERSION);
    drop(c);
    d
}

const WS: &str = "6f0c8a52-6d1e-4c41-9a4c-1f6a3e7d2b10";
const LIVE: &str = "0b4e1f0e-2a37-4d7e-b0a5-7a1c9d4e8f21";
const GONE: &str = "a3c6e9d2-5b18-47f4-8e2b-c1d0f9a8b7e6";
const CHAT: &str = "d9e8f7a6-b5c4-4d3e-8f2a-1b0c9d8e7f6a";

/// The registry as 0.4.1 wrote it (`serde_json` of its `ScratchRepo`
/// list): one live entry with a chat, one removed.
fn blob_041() -> String {
    format!(
        r#"[{{"id":"{LIVE}","workspaceId":"{WS}","name":"Try a parser","dirName":"try-a-parser-0b4e1f0e","path":"/data/scratch/try-a-parser-0b4e1f0e","root":"/data/scratch","rootCommit":"4b825dc642cb6eb9a060e54bf8d69288fbee4904","createdAt":1000,"lastActivity":2000,"createdBy":"acct-1","chats":["{CHAT}"],"state":"live"}},{{"id":"{GONE}","workspaceId":"{WS}","name":"Old","dirName":"old-a3c6e9d2","path":"/data/scratch/old-a3c6e9d2","root":"/data/scratch","rootCommit":"4b825dc642cb6eb9a060e54bf8d69288fbee4904","createdAt":500,"lastActivity":600,"createdBy":"","repoId":"{WS}","state":"removed","removedAt":700,"retirePending":true}}]"#
    )
}

fn put_blob(db: &Path, text: &str) {
    let c = rusqlite::Connection::open(db).unwrap();
    c.execute(
        "INSERT INTO repo_settings (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [SCRATCH_BLOB_KEY, text],
    )
    .unwrap();
}

fn blob(store: &EventStore) -> Option<String> {
    store.repo_setting(SCRATCH_BLOB_KEY).unwrap()
}

fn local_backups(db: &Path) -> Vec<PathBuf> {
    seed_backups(db).into_iter().filter(|p| p.to_string_lossy().contains(LOCAL_BACKUP_MARK)).collect()
}

fn ws() -> Uuid {
    Uuid::parse_str(WS).unwrap()
}

fn desktop(folders: &[(Uuid, &str)]) -> LegacyLocal {
    LegacyLocal { source: "desktop", folders: folders.iter().map(|(w, p)| LegacyFolder { workspace_id: *w, path: p.to_string() }).collect() }
}

/// V1: a 0.4.1 database with a scratch registry is migrated once, after
/// M8, with a backup; the rows match the blob, the blob stays as it was
/// (a downgrade still reads it), and a second start does nothing.
#[test]
fn a_0_4_1_database_imports_the_scratch_registry_once() {
    let d = v041();
    put_blob(&d.db, &blob_041());
    let store = EventStore::open(&d.db).unwrap();
    assert_eq!(store.schema_version().unwrap(), SYNC_SCHEMA_VERSION);
    assert_eq!(store.migrate_local_tables(&desktop(&[])).unwrap(), LocalTablesMigration::NotYet, "after M8");
    assert!(matches!(store.migrate_log_hygiene().unwrap(), HygieneMigration::Migrated { .. }));
    let folder = Uuid::new_v4();
    let LocalTablesMigration::Migrated { scratch, folders, backup, scratch_blocked } =
        store.migrate_local_tables(&desktop(&[(folder, "/Users/ana/src/app")])).unwrap()
    else {
        panic!("not migrated")
    };
    assert_eq!((scratch, folders, scratch_blocked), (2, 1, false));
    let backup = backup.expect("a backup");
    let old = rusqlite::Connection::open(&backup).unwrap();
    assert!(old.prepare("SELECT 1 FROM scratch_repos").unwrap().query([]).unwrap().next().unwrap().is_none(), "the backup is from before");
    drop(old);
    let all = scratch::list(&store, ws());
    assert_eq!(all.iter().map(|e| e.id.to_string()).collect::<Vec<_>>(), vec![LIVE.to_string(), GONE.to_string()], "in the list's order");
    let live = &all[0];
    assert_eq!(live.chats, vec![Uuid::parse_str(CHAT).unwrap()]);
    assert_eq!((live.created_by.as_str(), live.last_activity, live.is_live()), ("acct-1", 2000, true));
    let gone = &all[1];
    assert_eq!((gone.state, gone.removed_at, gone.retire_pending, gone.repo_id), (scratch::ScratchState::Removed, Some(700), true, Some(ws())));
    assert_eq!(blob(&store).as_deref(), Some(blob_041().as_str()), "the blob is kept as it was");
    assert_eq!(store.workspace_folder(folder).unwrap(), Some(PathBuf::from("/Users/ana/src/app")));
    assert!(store.workspace_folders_imported("desktop"));
    drop(store);
    let store = EventStore::open(&d.db).unwrap();
    assert_eq!(store.migrate_local_tables(&desktop(&[(folder, "/elsewhere")])).unwrap(), LocalTablesMigration::AlreadyDone);
    assert_eq!(local_backups(&d.db).len(), 1, "no second backup");
    assert_eq!(store.workspace_folder(folder).unwrap(), Some(PathBuf::from("/Users/ana/src/app")), "imported once");
    assert_eq!(scratch::list(&store, ws()).len(), 2);
}

/// V1: a 0.5 pre-release database (version 2, no local tables) gets them on
/// open and imports as a 0.4.1 one does.
#[test]
fn a_0_5_pre_database_gets_the_tables_and_imports() {
    let d = v05_pre();
    put_blob(&d.db, &blob_041());
    let store = EventStore::open(&d.db).unwrap();
    let LocalTablesMigration::Migrated { scratch, .. } = store.migrate_local_tables(&LegacyLocal::default()).unwrap() else { panic!() };
    assert_eq!(scratch, 2);
    assert_eq!(scratch::live(&store, ws()).len(), 1);
    assert_eq!(store.migrate_local_tables(&LegacyLocal::default()).unwrap(), LocalTablesMigration::AlreadyDone);
}

/// E5: a registry blob that doesn't parse stops its import: rows already
/// in the table are kept, the blob is never changed, every start tries
/// again, and `ligus doctor` warns. The registry never shrinks because of a
/// parse error.
#[test]
fn an_unreadable_registry_blob_stops_the_import_and_keeps_every_row() {
    let d = v05_pre();
    let store = EventStore::open(&d.db).unwrap();
    // A row written before the import (another build on this data dir).
    let t = tempfile::tempdir().unwrap();
    let settings = scratch::settings(&store, &t.path().join("scratch"));
    let made = scratch::create(&store, &settings, &[], ws(), "Kept", "acct-2", 10).unwrap();
    store.conn.execute("DELETE FROM local_imports", []).unwrap();
    let garbled = r#"[{"id": "not a uuid", "#;
    put_blob(&d.db, garbled);
    assert!(store.scratch_import_problem().is_some_and(|w| w.contains("couldn't be read")), "doctor warns");
    let LocalTablesMigration::Migrated { scratch, scratch_blocked, backup, .. } = store.migrate_local_tables(&LegacyLocal::default()).unwrap() else {
        panic!()
    };
    assert_eq!((scratch, scratch_blocked, backup), (0, true, None));
    assert_eq!(scratch::list(&store, ws()).iter().map(|e| e.id).collect::<Vec<_>>(), vec![made.id], "the row is kept");
    assert_eq!(blob(&store).as_deref(), Some(garbled), "never overwritten");
    // A write keeps the rows and the blob, and doesn't note an import.
    let second = scratch::create(&store, &settings, &[], ws(), "Second", "", 20).unwrap();
    assert_eq!(scratch::list(&store, ws()).len(), 2);
    assert_eq!(blob(&store).as_deref(), Some(garbled));
    assert!(!store.local_import_done(SCRATCH_IMPORT).unwrap());
    assert!(store.scratch_import_problem().is_some(), "still warns");
    // Every start tries again; still nothing lost.
    assert!(matches!(store.migrate_local_tables(&LegacyLocal::default()).unwrap(), LocalTablesMigration::Migrated { scratch_blocked: true, .. }));
    assert_eq!(scratch::list(&store, ws()).iter().map(|e| e.id).collect::<Vec<_>>(), vec![made.id, second.id]);
}

/// The copies an older build set aside are named by doctor too.
#[test]
fn doctor_names_copies_an_older_build_set_aside() {
    let store = EventStore::open_in_memory().unwrap();
    assert_eq!(store.scratch_import_problem(), None);
    store.set_repo_setting(&format!("{SCRATCH_BLOB_KEY}.corrupt-123"), Some("[")).unwrap();
    assert!(store.scratch_import_problem().is_some_and(|w| w.contains("1 unreadable")));
}

/// V1: a failed import changes nothing and leaves no backup; the next start
/// runs it.
#[test]
fn a_failed_import_changes_nothing_and_runs_again() {
    let d = v05_pre();
    put_blob(&d.db, &blob_041());
    let store = EventStore::open(&d.db).unwrap();
    let f = Uuid::new_v4();
    for hook in ["m9", "m9-backup"] {
        test_hooks::FAIL_STEP.with(|s| s.set(Some(hook)));
        assert!(store.migrate_local_tables(&desktop(&[(f, "/a")])).is_err());
        test_hooks::FAIL_STEP.with(|s| s.set(None));
        assert!(local_backups(&d.db).is_empty(), "{hook} left its backup");
        assert!(!store.local_import_done(SCRATCH_IMPORT).unwrap());
        assert_eq!(store.workspace_folder(f).unwrap(), None);
        let n: i64 = store.conn.query_row("SELECT COUNT(*) FROM scratch_repos", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }
    assert!(matches!(store.migrate_local_tables(&desktop(&[(f, "/a")])).unwrap(), LocalTablesMigration::Migrated { scratch: 2, folders: 1, .. }));
}

/// The import waits while another connection holds the store; meanwhile
/// readers see the blob's entries beside the table's, and the first write
/// merges them in (the blob itself stays as it was).
#[test]
fn before_the_import_readers_see_the_blob_and_a_write_merges_it() {
    let d = v05_pre();
    put_blob(&d.db, &blob_041());
    let store = EventStore::open(&d.db).unwrap();
    let other = EventStore::open(&d.db).unwrap();
    assert_eq!(store.migrate_local_tables(&LegacyLocal::default()).unwrap(), LocalTablesMigration::Busy);
    assert_eq!(scratch::list(&store, ws()).len(), 2, "read from the blob");
    let n: i64 = store.conn.query_row("SELECT COUNT(*) FROM scratch_repos", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
    scratch::mark_noticed(&store, Uuid::parse_str(LIVE).unwrap(), 3000).unwrap();
    let n: i64 = store.conn.query_row("SELECT COUNT(*) FROM scratch_repos", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 2, "merged on the write");
    assert!(store.local_import_done(SCRATCH_IMPORT).unwrap());
    assert_eq!(blob(&store).as_deref(), Some(blob_041().as_str()));
    assert_eq!(other.scratch_rows().unwrap().iter().find(|e| e.id.to_string() == LIVE).unwrap().noticed_at, Some(3000));
    drop(other);
    // The import has no scratch work left; the row it wrote is not undone.
    assert!(matches!(store.migrate_local_tables(&LegacyLocal::default()).unwrap(), LocalTablesMigration::AlreadyDone));
    assert_eq!(scratch::list(&store, ws()).iter().find(|e| e.id.to_string() == LIVE).unwrap().noticed_at, Some(3000));
}

/// Workspace folders: one row per workspace; a folder bound on this
/// device wins over an import; unbinding deletes the row; each surface's
/// import runs once.
#[test]
fn workspace_folders_bind_unbind_and_import_per_surface() {
    let store = EventStore::open_in_memory().unwrap();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    store.set_workspace_folder(a, Some("/picked")).unwrap();
    assert!(!store.workspace_folders_imported("cli"));
    let LocalTablesMigration::Migrated { folders, backup, .. } = store
        .migrate_local_tables(&LegacyLocal {
            source: "cli",
            folders: vec![LegacyFolder { workspace_id: a, path: "/old".into() }, LegacyFolder { workspace_id: b, path: " ".into() }],
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!((folders, backup), (0, None), "the bound folder wins; a blank one is skipped; no file, no backup");
    assert_eq!(store.workspace_folder(a).unwrap(), Some(PathBuf::from("/picked")));
    assert_eq!(store.workspace_folder(b).unwrap(), None);
    assert!(store.workspace_folders_imported("cli"));
    assert!(!store.workspace_folders_imported("lt"), "per surface");
    store.set_workspace_folder(b, Some("/b")).unwrap();
    assert_eq!(store.workspace_folders().unwrap().len(), 2);
    store.set_workspace_folder(a, None).unwrap();
    assert_eq!(store.workspace_folder(a).unwrap(), None);
    store.set_workspace_folder(b, Some("  ")).unwrap();
    assert_eq!(store.workspace_folder(b).unwrap(), None, "blank unbinds");
}

/// V3: the new tables hold ids, names, folders and times only. A decoy
/// secret written beside them (a setting, the connection secrets) never
/// lands in them.
#[test]
fn no_secret_reaches_the_local_tables() {
    let d = v05_pre();
    put_blob(&d.db, &blob_041());
    let store = EventStore::open(&d.db).unwrap();
    store.migrate_local_tables(&desktop(&[(ws(), "/Users/ana/src/app")])).unwrap();
    let decoy = "hrt1-decoy-secret-0123456789";
    store.set_repo_setting("unrelated", Some(decoy)).unwrap();
    for table in ["scratch_repos", "scratch_repo_chats", "workspace_folders", "local_imports"] {
        let mut st = store.conn.prepare(&format!("SELECT * FROM {table}")).unwrap();
        let cols = st.column_count();
        let mut rows = st.query([]).unwrap();
        while let Some(r) = rows.next().unwrap() {
            for i in 0..cols {
                let v: rusqlite::types::Value = r.get(i).unwrap();
                assert!(!format!("{v:?}").contains(decoy), "{table} holds the decoy");
            }
        }
    }
}

/// The light read and write a reader with no store open uses: the older
/// source until that surface's import ran, then the table.
#[test]
fn the_light_read_and_write_follow_the_import() {
    let d = v05_pre();
    let w = Uuid::new_v4();
    assert_eq!(peek_workspace_folder(&d.db.with_file_name("none.db"), w, "cli"), None, "no database");
    let store = EventStore::open(&d.db).unwrap();
    assert_eq!(peek_workspace_folder(&d.db, w, "cli"), None, "not imported yet");
    assert!(!write_workspace_folder(&d.db, w, "cli", Some("/x")).unwrap());
    store.migrate_local_tables(&LegacyLocal { source: "cli", folders: vec![LegacyFolder { workspace_id: w, path: "/repos/app".into() }] }).unwrap();
    assert_eq!(peek_workspace_folder(&d.db, w, "cli"), Some(Some(PathBuf::from("/repos/app"))));
    assert!(write_workspace_folder(&d.db, w, "cli", Some("/repos/other")).unwrap());
    assert_eq!(store.workspace_folder(w).unwrap(), Some(PathBuf::from("/repos/other")));
    assert!(write_workspace_folder(&d.db, w, "cli", None).unwrap());
    assert_eq!(peek_workspace_folder(&d.db, w, "cli"), Some(None), "unbound, not the older source");
}

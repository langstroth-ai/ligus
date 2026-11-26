//! M4t (identity model v2, 10.4): a data dir whose database a newer Ligus
//! wrote stops every command with exit 3 and the same words, and `ligus
//! doctor` reports it. Against the real binary in a throwaway data dir (file
//! secret backend, a fake HOME), so nothing touches the person's own data or
//! keychain.

use std::process::{Command, Output};

/// A clean environment that still runs on every OS (as `setup_cli.rs`).
fn clean_env(c: &mut Command) -> &mut Command {
    c.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default());
    for k in [
        "SystemRoot",
        "SYSTEMROOT",
        "windir",
        "SystemDrive",
        "TEMP",
        "TMP",
        "ComSpec",
        "PATHEXT",
    ] {
        if let Ok(v) = std::env::var(k) {
            c.env(k, v);
        }
    }
    c
}

fn ligus(home: &std::path::Path, data: &std::path::Path, args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ligus"));
    clean_env(&mut c)
        .args(args)
        .env("HOME", home)
        .env("LIGUS_DATA_DIR", data)
        .env("LIGUS_SECRET_BACKEND", "file")
        .env("LIGUS_NO_UPDATE_CHECK", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

/// Set the database's `user_version` (bytes 60..64 of its header, big
/// endian) as a newer build would, with no connection open.
fn set_user_version(db: &std::path::Path, v: u32) {
    let mut bytes = std::fs::read(db).unwrap();
    bytes[60..64].copy_from_slice(&v.to_be_bytes());
    std::fs::write(db, bytes).unwrap();
}

#[test]
fn data_from_a_newer_ligus_stops_every_command_with_exit_3() {
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let db = data.join("ligus.db");
    {
        let store = ligus_runtime::EventStore::open(&db).unwrap();
        assert_eq!(
            store.schema_version().unwrap(),
            ligus_runtime::event_store::SCHEMA_VERSION
        );
    }
    // At this build's version every command runs.
    let ok = ligus(home.path(), &data, &["status", "--json"]);
    assert_ne!(
        ok.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );

    set_user_version(&db, ligus_runtime::event_store::SCHEMA_VERSION as u32 + 1);
    let before = std::fs::read(&db).unwrap();
    let o = ligus(home.path(), &data, &["status", "--json"]);
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(3), "{stderr}");
    assert!(
        stderr.contains("this data was written by a newer Ligus; update or use another data dir"),
        "{stderr}"
    );
    let doc: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(doc["schema"], "ligus.error.v1");
    assert_eq!(doc["exitCode"], 3);
    assert_eq!(doc["error"]["code"], "newer_data");

    // Doctor runs, says so, and exits 3 too.
    let d = ligus(home.path(), &data, &["doctor", "--json"]);
    assert_eq!(
        d.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&d.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&d.stdout).unwrap();
    let check = doc["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "store.schema")
        .expect("a store.schema check");
    assert_eq!(check["status"], "fail");
    assert!(check["detail"].as_str().unwrap().contains("newer Ligus"));

    // Nothing was written to the database.
    assert_eq!(std::fs::read(&db).unwrap(), before);
}

/// `ligus doctor` lists the database backups (they hold invite and
/// admission seeds) and never deletes them.
#[test]
fn doctor_lists_the_database_backups() {
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    drop(ligus_runtime::EventStore::open(data.join("ligus.db")).unwrap());
    let backup = data.join("ligus.db.bak-sync-v2-1795600000-000000001-42");
    std::fs::write(&backup, b"a copy").unwrap();
    let d = ligus(home.path(), &data, &["doctor", "--json"]);
    let doc: serde_json::Value = serde_json::from_slice(&d.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&d.stderr)));
    let check = doc["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "store.backups")
        .expect("a store.backups check");
    assert_eq!(check["status"], "warn");
    assert!(
        check["detail"]
            .as_str()
            .unwrap()
            .contains("ligus.db.bak-sync-v2-1795600000-000000001-42"),
        "{check}"
    );
    assert_eq!(check["fix"], "delete once you no longer need the backup");
    assert!(backup.exists(), "nothing deletes a backup by itself");
}

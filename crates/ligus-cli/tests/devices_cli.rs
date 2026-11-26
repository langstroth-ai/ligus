//! `ligus devices request|redeem` and `ligus status` against the real binary
//! in a throwaway data dir (file secret backend, a fake HOME): the request
//! alone on stdout, the rest on stderr; redeem's refusals away from a
//! terminal; a half-done link in `ligus status`, and expired requests
//! cleaned up.

use std::process::{Command, Output};

use serde_json::Value;

struct Dir {
    _home: tempfile::TempDir,
    data: std::path::PathBuf,
    home: std::path::PathBuf,
}

fn fresh() -> Dir {
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    Dir { data, home: home.path().to_path_buf(), _home: home }
}

fn ligus(d: &Dir, args: &[&str], stdin: Option<&str>) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ligus"));
    c.env_clear().env("PATH", std::env::var("PATH").unwrap_or_default());
    for k in ["SystemRoot", "SYSTEMROOT", "windir", "SystemDrive", "TEMP", "TMP", "ComSpec", "PATHEXT"] {
        if let Ok(v) = std::env::var(k) {
            c.env(k, v);
        }
    }
    c.args(args)
        .env("HOME", &d.home)
        .env("LIGUS_DATA_DIR", &d.data)
        .env("LIGUS_SECRET_BACKEND", "file")
        .env("LIGUS_NO_UPDATE_CHECK", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = c.spawn().unwrap();
    match stdin {
        Some(s) => {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
        }
        None => drop(child.stdin.take()),
    }
    child.wait_with_output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn doc(o: &Output) -> Value {
    serde_json::from_str(out(o).trim()).unwrap_or_else(|e| panic!("not one JSON document ({e}): {}\nstderr: {}", out(o), err(o)))
}

#[test]
fn request_prints_only_the_code_on_stdout() {
    let d = fresh();
    let o = ligus(&d, &["devices", "request", "--label", "laptop"], None);
    assert!(o.status.success(), "{}", err(&o));
    let stdout = out(&o);
    assert_eq!(stdout.lines().count(), 1, "one line: {stdout}");
    assert!(stdout.starts_with("liguslink1:") && stdout.ends_with('\n'), "{stdout}");
    let e = err(&o);
    assert!(e.contains("Fingerprint: ") && e.contains("(label \"laptop\")"), "{e}");
    assert!(e.contains("On your desktop: Settings > Profile > My devices > Add a computer"), "{e}");
    assert!(e.contains("Then here: ligus devices redeem"), "{e}");
    assert!(!e.contains("liguslink1:"), "the code isn't on stderr too: {e}");

    let o = ligus(&d, &["devices", "request", "--json"], None);
    assert!(o.status.success(), "{}", err(&o));
    let v = doc(&o);
    let r = &v["result"];
    assert!(r["code"].as_str().unwrap().starts_with("liguslink1:"));
    assert_eq!(r["code"], r["request"]);
    assert_eq!(r["fingerprint"].as_str().unwrap().len(), 24, "{r}");
    assert!(r["expiresAt"].as_i64().unwrap() > 0 && r["newDevice"].is_boolean(), "{r}");

    // The link waits for the desktop: ligus status says so.
    let o = ligus(&d, &["status"], None);
    assert!(out(&o).contains("Linking waiting for your desktop's code (started "), "{}", out(&o));
    let o = ligus(&d, &["status", "--json"], None);
    let l = &doc(&o)["linking"];
    assert_eq!(l["pending"], 2, "{l}");
    assert_eq!(l["approveBy"].as_i64().unwrap() - l["startedAt"].as_i64().unwrap(), 15 * 60 * 1000);
    assert_eq!(l["expiresAt"], r["expiresAt"].as_i64().unwrap() + 30 * 60 * 1000);
}

#[test]
fn expired_requests_are_cleaned_up() {
    let d = fresh();
    assert!(ligus(&d, &["devices", "request"], None).status.success());
    let path = d.data.join("computer-link.json");
    let mut state: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    state["pending"][0]["createdAt"] = Value::from(1_000);
    std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    let o = ligus(&d, &["status", "--json"], None);
    assert!(doc(&o)["linking"].is_null(), "an expired request doesn't count: {}", out(&o));
    assert!(!out(&ligus(&d, &["status"], None)).contains("Linking"));
    // The next devices command cleans it up on disk.
    assert!(ligus(&d, &["devices", "request"], None).status.success());
    let state: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let pending = state["pending"].as_array().unwrap();
    assert_eq!(pending.len(), 1, "{state}");
    assert_ne!(pending[0]["createdAt"], 1_000);
}

#[test]
fn redeem_away_from_a_terminal_needs_the_code_and_never_echoes_input() {
    let d = fresh();
    assert!(ligus(&d, &["devices", "request"], None).status.success());
    let o = ligus(&d, &["devices", "redeem"], None);
    assert_eq!(o.status.code(), Some(4), "{}", err(&o));
    assert!(err(&o).contains("ligus devices redeem -"), "{}", err(&o));

    let o = ligus(&d, &["devices", "redeem", "-"], Some("not-a-code SECRETISH\n"));
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("doesn't hold a desktop code"), "{}", err(&o));
    assert!(!err(&o).contains("SECRETISH") && !out(&o).contains("SECRETISH"));

    let req = out(&ligus(&d, &["devices", "request"], None));
    let o = ligus(&d, &["devices", "redeem", "-"], Some(&req));
    assert!(err(&o).contains("this computer's request"), "{}", err(&o));

    // A grant from stdin that answers nothing here: refused, never shown.
    let o = ligus(&d, &["devices", "redeem", "-"], Some("pbpaste output: ligusgrant1:QUJDREVGR0hJSktMTU5PUA\n"));
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(!err(&o).contains("QUJDREVGR0hJSktMTU5PUA") && !out(&o).contains("QUJDREVGR0hJSktMTU5PUA"), "{}", err(&o));
}

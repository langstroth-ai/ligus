//! `ligus setup` end to end: the `--json` shape and the exit codes, against
//! the real binary in a throwaway data dir (file secret backend, a fake
//! HOME), so nothing touches the person's own data or keychain.

use std::process::{Command, Output};

use serde_json::Value;

struct Box_ {
    _home: tempfile::TempDir,
    data: std::path::PathBuf,
    home: std::path::PathBuf,
}

fn fresh() -> Box_ {
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    Box_ { data, home: home.path().to_path_buf(), _home: home }
}

/// A clean environment that still runs on every OS: Windows needs its
/// system variables (Winsock can't load its providers without `SystemRoot`,
/// so every socket fails), the rest only `PATH`.
fn clean_env(c: &mut Command) -> &mut Command {
    c.env_clear().env("PATH", std::env::var("PATH").unwrap_or_default());
    for k in ["SystemRoot", "SYSTEMROOT", "windir", "SystemDrive", "TEMP", "TMP", "ComSpec", "PATHEXT"] {
        if let Ok(v) = std::env::var(k) {
            c.env(k, v);
        }
    }
    c
}

fn ligus(b: &Box_, args: &[&str], stdin: Option<&str>) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ligus"));
    clean_env(&mut c)
        .args(args)
        .env("HOME", &b.home)
        .env("LIGUS_DATA_DIR", &b.data)
        .env("LIGUS_SECRET_BACKEND", "file")
        .env("LIGUS_NO_UPDATE_CHECK", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = c.spawn().unwrap();
    if let Some(s) = stdin {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
    } else {
        drop(child.stdin.take());
    }
    child.wait_with_output().unwrap()
}

fn doc(o: &Output) -> Value {
    let out = String::from_utf8_lossy(&o.stdout);
    serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("not one JSON document ({e}): {out}\nstderr: {}", String::from_utf8_lossy(&o.stderr)))
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn check<'a>(d: &'a Value, id: &str) -> Option<&'a Value> {
    d["checks"].as_array()?.iter().find(|c| c["id"] == id)
}

#[test]
fn the_list_is_one_checks_report_of_seven_flows() {
    let b = fresh();
    let o = ligus(&b, &["setup", "--json"], None);
    assert_eq!(code(&o), 0);
    let d = doc(&o);
    assert_eq!(d["schema"], "ligus.checks.v1");
    assert_eq!(d["command"], "setup");
    let ids: Vec<&str> = d["checks"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["agent.status", "tool.status", "folder.status", "box.status", "phone.status", "invite.status", "relay.status"]);
    let text = ligus(&b, &["setup"], None);
    assert_eq!(code(&text), 0);
    assert!(String::from_utf8_lossy(&text.stdout).contains("Add an agent"));
}

#[test]
fn usage_errors_exit_2_with_the_flag_named() {
    let b = fresh();
    let o = ligus(&b, &["setup", "nope"], None);
    assert_eq!(code(&o), 2);
    let o = ligus(&b, &["setup", "agent", "--colour", "blue", "--json"], None);
    assert_eq!(code(&o), 2);
    assert_eq!(doc(&o)["schema"], "ligus.error.v1");
    // A reserved name: the engine's check, in the flow's report.
    let o = ligus(&b, &["setup", "agent", "--harness", "local", "--model", "m", "--name", "ligus", "--endpoint", "http://localhost:9", "--json"], None);
    assert_eq!(code(&o), 2, "{}", String::from_utf8_lossy(&o.stdout));
    let d = doc(&o);
    assert_eq!(d["command"], "setup agent");
    assert_eq!(d["exitCode"], 2);
    assert_eq!(check(&d, "agent.input.name").unwrap()["status"], "fail");
    // Plain http to another host is refused.
    let o = ligus(&b, &["setup", "relay", "--kind", "own", "--url", "http://relay.example.com", "--json"], None);
    assert_eq!(code(&o), 2);
    assert!(check(&doc(&o), "relay.input.url").is_some());
}

#[test]
fn a_secret_on_the_command_line_is_refused_and_never_echoed() {
    let b = fresh();
    for args in [
        &["setup", "relay", "--token", "hrt1-canary-0123456789", "--json"][..],
        &["setup", "relay", "--token=hrt1-canary-0123456789", "--json"][..],
        &["setup", "box", "--code", "hrt1-canary-0123456789"][..],
    ] {
        let o = ligus(&b, args, None);
        assert_eq!(code(&o), 2, "{args:?}");
        let all = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        assert!(!all.contains("hrt1-canary-0123456789"), "{all}");
    }
}

#[test]
fn person_steps_exit_4_and_never_print_their_secret() {
    let b = fresh();
    // Inviting needs a workspace (none yet: fixable, exit 3).
    let o = ligus(&b, &["setup", "invite", "--json"], None);
    assert_eq!(code(&o), 3, "{}", String::from_utf8_lossy(&o.stdout));
    // A box: a join request (public) and a stop for the Admin, or no sandbox.
    let o = ligus(&b, &["setup", "box", "--label", "ci", "--json"], None);
    let d = doc(&o);
    match code(&o) {
        4 => {
            assert!(d["results"]["joinRequest"].as_str().unwrap().starts_with("ligusjoin1:"));
            assert!(d["next"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("--code -")));
        }
        3 => assert_eq!(check(&d, "box.sandbox").unwrap()["status"], "fail"),
        c => panic!("exit {c}: {d}"),
    }
}

#[test]
fn dry_run_writes_no_server() {
    let b = fresh();
    let o = ligus(&b, &["setup", "tool", "--id", "notes", "--command", "/bin/echo hi", "--dry-run", "--json"], None);
    let d = doc(&o);
    assert!(matches!(code(&o), 0 | 3), "{d}");
    assert_eq!(d["dryRun"], true);
    assert!(!b.data.join("mcp-servers.toml").exists());
}

/// The saved relay access token is never sent to a relay it wasn't saved
/// for (here: no relay saved at all, a token in the environment, and a
/// "try my relay" URL), dry run or not.
#[test]
fn the_access_token_never_goes_to_another_relay() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // A fake relay that answers one request, or gives up after 30 s (never
    // a blocking accept: a CLI that can't connect must fail this test, not
    // hang it).
    listener.set_nonblocking(true).unwrap();
    let seen = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            match listener.accept() {
                Ok((mut s, _)) => {
                    s.set_nonblocking(false).unwrap();
                    s.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                    let mut buf = [0u8; 8192];
                    let n = s.read(&mut buf).unwrap_or(0);
                    let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
                    return Some(String::from_utf8_lossy(&buf[..n]).into_owned());
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(_) => return None,
            }
        }
    });
    let b = fresh();
    let url = format!("http://127.0.0.1:{port}");
    let mut c = Command::new(env!("CARGO_BIN_EXE_ligus"));
    clean_env(&mut c)
        .args(["setup", "relay", "--kind", "own", "--url", &url, "--dry-run", "--json"])
        .env("HOME", &b.home)
        .env("LIGUS_DATA_DIR", &b.data)
        .env("LIGUS_SECRET_BACKEND", "file")
        .env("LIGUS_RELAY_ACCESS_TOKEN", "hrt1CANARYOLDRELAY")
        .stdin(std::process::Stdio::null());
    let o = c.output().unwrap();
    let Some(request) = seen.join().unwrap() else {
        panic!(
            "the CLI never reached the fake relay at {url} within 30 s (exit {:?})\nstdout: {}\nstderr: {}",
            o.status.code(),
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
    };
    assert!(request.starts_with("GET "), "the relay was probed: {request:?} / {}", String::from_utf8_lossy(&o.stdout));
    assert!(!request.contains("hrt1CANARYOLDRELAY"), "the token went to another relay: {request}");
}

#[test]
fn a_stray_argument_is_never_echoed() {
    let b = fresh();
    let o = ligus(&b, &["setup", "box", "ligusadmit1:CANARYSECRET456"], None);
    assert_eq!(code(&o), 2);
    let all = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(!all.contains("CANARYSECRET456"), "{all}");
}

/// `ligus agents set <agent> --model` and `ligus workspace rules` on a data
/// dir with no workspace: the engine's not-found, a readable status, and an
/// upgrade only the Owner may run.
#[test]
fn agents_set_and_workspace_rules_answer_without_a_workspace() {
    let b = fresh();
    let o = ligus(&b, &["agents", "set", "ghost", "--model", "opus", "--json"], None);
    assert_eq!(code(&o), 5, "{}", String::from_utf8_lossy(&o.stdout));
    let d = doc(&o);
    assert_eq!(check(&d, "agent.input.agent").map(|c| c["status"].clone()), Some(Value::from("fail")));
    let o = ligus(&b, &["agents", "set", "ghost", "--no-wait"], None);
    assert_eq!(code(&o), 2, "a flow flag alone is a usage error");
    let o = ligus(&b, &["workspace", "rules", "--json"], None);
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    let d = doc(&o);
    assert!(d["changes"].as_array().is_some_and(|c| !c.is_empty()));
    let o = ligus(&b, &["workspace", "rules", "upgrade"], None);
    assert_eq!(code(&o), 6, "only the Owner: {}", String::from_utf8_lossy(&o.stderr));
    let o = ligus(&b, &["workspace"], None);
    assert_eq!(code(&o), 2);
    let help = ligus(&b, &["workspace", "rules", "--help"], None);
    assert!(String::from_utf8_lossy(&help.stdout).contains("0.4.1"));
}

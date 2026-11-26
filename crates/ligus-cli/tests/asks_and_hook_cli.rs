//! Q-T9 and Q-T10 at the command line: `ligus hook permission` prints
//! nothing when no Ligus is running (and never creates a data dir), and
//! `ligus asks` exits with the documented codes and one JSON document.

use std::io::Write;
use std::process::{Command, Output, Stdio};

fn ligus(home: &std::path::Path, data: Option<&std::path::Path>, args: &[&str], stdin: &str) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ligus"));
    for (k, _) in std::env::vars() {
        if k.starts_with("LIGUS_") {
            c.env_remove(k);
        }
    }
    c.args(args).env("HOME", home).env("LIGUS_SECRET_BACKEND", "file").env("LIGUS_NO_UPDATE_CHECK", "1");
    if let Some(d) = data {
        c.env("LIGUS_DATA_DIR", d);
    }
    // No controlling terminal: a confirmation can't be given.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn the_hook_prints_nothing_without_a_running_ligus_and_opens_no_data_dir() {
    let home = tempfile::tempdir().unwrap();
    let input = r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"ls"},"cwd":"/tmp/shop","session_id":"s","permission_mode":"default"}"#;
    let out = ligus(home.path(), None, &["hook", "permission", "--workspace", "11111111-1111-4111-8111-111111111111", "--chat", "22222222-2222-4222-8222-222222222222"], input);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout.is_empty(), "no decision: Claude Code decides");
    assert!(String::from_utf8_lossy(&out.stderr).contains("no running Ligus"));
    assert!(std::fs::read_dir(home.path()).unwrap().next().is_none(), "the hook created nothing: no data dir");
    // Garbage on stdin: still exit 0, nothing printed.
    let out = ligus(home.path(), None, &["hook", "permission", "--workspace", "w", "--chat", "c"], "not json");
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
}

#[test]
fn hook_install_writes_and_then_changes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let settings = home.path().join("proj/.claude/settings.json");
    let s = settings.to_str().unwrap();
    let args = ["hook", "install", "--settings", s, "--workspace", "11111111-1111-4111-8111-111111111111", "--chat", "22222222-2222-4222-8222-222222222222"];
    let out = ligus(home.path(), None, &args, "");
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(v["hooks"]["PermissionRequest"][0]["hooks"][0]["command"], "ligus hook permission --workspace 11111111-1111-4111-8111-111111111111 --chat 22222222-2222-4222-8222-222222222222");
    let out = ligus(home.path(), None, &args, "");
    assert!(String::from_utf8_lossy(&out.stdout).contains("already installed"));
    // Your own user-level settings need a yes at the terminal.
    let user = home.path().join(".claude/settings.json");
    let mut uargs = args;
    let u = user.to_str().unwrap().to_string();
    uargs[3] = &u;
    let out = ligus(home.path(), None, &uargs, "");
    assert_eq!(out.status.code(), Some(4), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!user.exists());
}

#[test]
fn asks_exit_codes_and_json() {
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    let out = ligus(home.path(), Some(&data), &["asks", "list", "--json"], "");
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!((v["schema"].as_str(), v["exitCode"].as_i64()), (Some("ligus.agent-deploy.v1"), Some(0)));
    assert!(v["asks"].as_array().unwrap().is_empty());
    let out = ligus(home.path(), Some(&data), &["asks", "answer", "0b7e2f8a-3c41-4d2e-9a55-6f1d2c3b4a59", "--yes", "--json"], "");
    assert_eq!(out.status.code(), Some(5), "an unknown ask");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["exitCode"], 5);
    let out = ligus(home.path(), Some(&data), &["asks", "answer", "nope", "--json"], "");
    assert_eq!(out.status.code(), Some(2));
}

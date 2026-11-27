//! `ligus secrets set | list | rm` against the real binary in a throwaway
//! data dir (file secret backend, a fake HOME): the value comes from stdin
//! only, a value on the command line is refused before anything is read or
//! written, `host.toml` gets (and loses) the declaration, `--json` has its
//! shape, the exit codes are the documented ones, and no value is ever
//! printed.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const SECRET: &str = "sk-secrets-cli-test-0123456789abcdef";
const WS: &str = "6f1c0c4e-0000-4000-8000-000000000001";

fn clean_env(c: &mut Command) -> &mut Command {
    c.env_clear().env("PATH", std::env::var("PATH").unwrap_or_default());
    for k in ["SystemRoot", "SYSTEMROOT", "windir", "SystemDrive", "TEMP", "TMP", "ComSpec", "PATHEXT"] {
        if let Ok(v) = std::env::var(k) {
            c.env(k, v);
        }
    }
    c
}

fn ligus_with(home: &Path, data: &Path, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ligus"));
    clean_env(&mut c)
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("LIGUS_DATA_DIR", data)
        .env("LIGUS_SECRET_BACKEND", "file")
        .env("LIGUS_NO_UPDATE_CHECK", "1")
        .envs(env.iter().copied())
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    if let Some(text) = stdin {
        // A refused command may exit before reading: ignore a broken pipe.
        let _ = child.stdin.take().unwrap().write_all(text.as_bytes());
    }
    child.wait_with_output().unwrap()
}

fn ligus(home: &Path, data: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    ligus_with(home, data, args, stdin, &[])
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn json(o: &Output) -> serde_json::Value {
    let s = String::from_utf8_lossy(&o.stdout).into_owned();
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "one JSON line on stdout: {s}\nstderr: {}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_str(lines[0]).unwrap()
}

fn never_printed(o: &Output) {
    assert!(!text(o).contains(SECRET), "a value was printed:\n{}", text(o));
}

#[test]
fn set_list_rm_with_the_value_on_stdin_only() {
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    let host_toml = data.join("host.toml");

    // Set from stdin (one trailing newline dropped), declared for WS.
    let o = ligus(home.path(), &data, &["secrets", "set", "team-claude", "-", "--workspace", WS, "--json"], Some(&format!("{SECRET}\n")));
    assert!(o.status.success(), "{}", text(&o));
    never_printed(&o);
    let j = json(&o);
    assert_eq!(j["schema"], "ligus.secrets.v1");
    assert_eq!(j["name"], "team-claude");
    assert_eq!(j["workspace"], WS);
    assert_eq!(j["declared"], true);
    assert_eq!(j["replaced"], false);
    assert_eq!(j["backend"], "file");
    let toml = std::fs::read_to_string(&host_toml).unwrap();
    assert!(toml.contains(WS) && toml.contains("team-claude"), "{toml}");
    assert!(!toml.contains(SECRET), "host.toml never holds a value");
    let stored = std::fs::read_to_string(data.join("secrets/ws-secret/team-claude")).unwrap();
    assert_eq!(stored, SECRET, "the value without its line end, in the store");

    // Text form, and setting again replaces.
    let o = ligus(home.path(), &data, &["secrets", "set", "team-claude", "--workspace", WS], Some(SECRET));
    assert!(o.status.success(), "{}", text(&o));
    never_printed(&o);
    assert!(text(&o).contains("Replaced team-claude"), "{}", text(&o));

    // List: names, where declared, where the value is; never the value.
    let o = ligus(home.path(), &data, &["secrets", "list", "--workspace", WS, "--json"], None);
    assert!(o.status.success(), "{}", text(&o));
    never_printed(&o);
    let j = json(&o);
    let row = &j["secrets"][0];
    assert_eq!(row["name"], "team-claude");
    assert_eq!(row["declaredFor"], serde_json::json!([WS]));
    assert_eq!(row["declaredHere"], true);
    assert_eq!(row["source"], "store");
    // The environment wins, and shows.
    let o = ligus_with(home.path(), &data, &["secrets", "list", "--json"], None, &[("LIGUS_WS_SECRET_team-claude", SECRET), ("LIGUS_WS_SECRET_other", SECRET)]);
    never_printed(&o);
    let j = json(&o);
    let names: Vec<&str> = j["secrets"].as_array().unwrap().iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["other", "team-claude"]);
    assert_eq!(j["secrets"][1]["source"], "env");
    assert_eq!(j["secrets"][0]["declaredFor"], serde_json::json!([]));
    let o = ligus(home.path(), &data, &["secrets", "list"], None);
    assert!(text(&o).contains("team-claude") && text(&o).contains("held: secret store"), "{}", text(&o));
    never_printed(&o);

    // rm: undeclared and, as no workspace uses it now, deleted.
    let o = ligus(home.path(), &data, &["secrets", "rm", "team-claude", "--workspace", WS, "--json"], None);
    assert!(o.status.success(), "{}", text(&o));
    let j = json(&o);
    assert_eq!((j["undeclared"].as_bool(), j["deletedFromStore"].as_bool()), (Some(true), Some(true)));
    assert!(!data.join("secrets/ws-secret/team-claude").exists());
    assert!(!std::fs::read_to_string(&host_toml).unwrap().contains("team-claude"));
    // Again: not found (5).
    let o = ligus(home.path(), &data, &["secrets", "rm", "team-claude", "--workspace", WS], None);
    assert_eq!(o.status.code(), Some(5), "{}", text(&o));
}

#[test]
fn a_value_on_the_command_line_is_refused_and_never_echoed() {
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    for args in [
        vec!["secrets", "set", "team-claude", SECRET],
        vec!["secrets", "set", "team-claude", "-", SECRET],
        vec!["secrets", "set", "team-claude", &format!("--value={SECRET}")],
        vec!["secrets", "set", "team-claude", "--secret", SECRET, "--json"],
    ] {
        let o = ligus(home.path(), &data, &args, Some("from-stdin-should-not-be-used\n"));
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", text(&o));
        never_printed(&o);
        assert!(text(&o).contains("never takes the value on the command line"), "{}", text(&o));
        assert!(!data.join("host.toml").exists(), "{args:?}: nothing declared");
        assert!(!data.join("secrets/ws-secret").exists(), "{args:?}: nothing stored");
    }
}

#[test]
fn bad_input_exits_2() {
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    for (args, stdin) in [
        (vec!["secrets", "set", "team-claude", "-"], "\n"),
        (vec!["secrets", "set", "team-claude", "-"], ""),
        (vec!["secrets", "set", "bad name", "-"], "v"),
        (vec!["secrets", "set", "team-claude", "-", "--workspace", "not-a-uuid"], "v"),
        (vec!["secrets", "set"], "v"),
        (vec!["secrets", "bogus"], ""),
    ] {
        let o = ligus(home.path(), &data, &args, Some(stdin));
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", text(&o));
    }
    assert!(!data.join("host.toml").exists());
}

#[test]
fn help_documents_every_subcommand() {
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    for args in [&["secrets", "--help"][..], &["secrets", "set", "--help"], &["secrets", "list", "--help"], &["secrets", "rm", "--help"]] {
        let o = ligus(home.path(), &data, &args, None);
        assert!(o.status.success(), "{args:?}");
        let t = text(&o);
        for sec in ["USAGE", "EXIT CODES", "EXAMPLES"] {
            assert!(t.contains(sec), "{args:?} lacks {sec}");
        }
    }
}

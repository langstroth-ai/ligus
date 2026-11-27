//! `ligus gateway add | list | check | remove` against the real binary in a
//! throwaway data dir (file secret backend, a fake HOME): the file it writes
//! is the one serve reads, the refusals keep the host's rules, the commands
//! are idempotent, `--json` has its shape, and no secret is ever printed.

use std::path::Path;
use std::process::{Command, Output};

fn clean_env(c: &mut Command) -> &mut Command {
    c.env_clear().env("PATH", std::env::var("PATH").unwrap_or_default());
    for k in ["SystemRoot", "SYSTEMROOT", "windir", "SystemDrive", "TEMP", "TMP", "ComSpec", "PATHEXT"] {
        if let Ok(v) = std::env::var(k) {
            c.env(k, v);
        }
    }
    c
}

fn ligus_env(home: &Path, data: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ligus"));
    clean_env(&mut c)
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("LIGUS_DATA_DIR", data)
        .env("LIGUS_SECRET_BACKEND", "file")
        .env("LIGUS_NO_UPDATE_CHECK", "1")
        .envs(env.iter().copied())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn ligus(home: &Path, data: &Path, args: &[&str]) -> Output {
    ligus_env(home, data, args, &[])
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}
fn json(o: &Output) -> serde_json::Value {
    let s = out(o);
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "one JSON line on stdout: {s}\nstderr: {}", err(o));
    serde_json::from_str(lines[0]).unwrap()
}

const SECRET: &str = "sk-gateway-test-0123456789abcdef";

struct Box_ {
    _home: tempfile::TempDir,
    home: std::path::PathBuf,
    data: std::path::PathBuf,
    key: std::path::PathBuf,
}

fn setup() -> Box_ {
    let home = tempfile::tempdir().unwrap();
    let h = home.path().to_path_buf();
    let data = h.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let kd = h.join(".config/ligus-gateways");
    std::fs::create_dir_all(&kd).unwrap();
    let key = kd.join("box-gw.key");
    std::fs::write(&key, SECRET).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    Box_ { _home: home, home: h, data, key }
}

#[test]
fn add_list_remove_round_trip() {
    let b = setup();
    let key = b.key.to_str().unwrap();
    let file = b.data.join("model-gateways.json");

    // Nothing listed yet: list says how to add one, and writes nothing.
    let o = ligus(&b.home, &b.data, &["gateway", "list"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("ligus gateway add NAME"), "{}", out(&o));
    assert!(!file.exists());

    // Plain http is refused without the operator's flag (exit 2), nothing written.
    let add = ["gateway", "add", "box-gw", "--url", "http://100.64.0.2:8080/v1", "--api", "openai", "--auth", "bearer-file", key];
    let o = ligus(&b.home, &b.data, &add);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(err(&o).contains("--allow-plaintext"), "{}", err(&o));
    assert!(!file.exists());

    // With it: added, exclusive by default (said in one line), the warning.
    let mut add_pt = add.to_vec();
    add_pt.push("--allow-plaintext");
    let o = ligus(&b.home, &b.data, &add_pt);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let t = out(&o);
    assert!(t.contains("Added gateway box-gw"), "{t}");
    assert!(t.contains("Plain http is readable on the path. Prefer HTTPS (e.g. tailscale serve)."), "{t}");
    assert!(t.contains("only through its gateways; other agents here will be refused. Allow both: ligus gateway add box-gw"), "{t}");
    assert!(t.contains("--not-exclusive"), "{t}");
    assert!(t.contains("Restart it so the desktop lists the change"), "{t}");
    assert!(!t.contains(SECRET) && !err(&o).contains(SECRET), "a secret was printed");

    // The file: what the host reads, owner-only, the key by path only.
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.contains(SECRET), "the secret went into the file");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["exclusive"], true, "{text}");
    assert_eq!(v["gateways"][0]["allowPlaintext"], true, "{text}");
    let real_key = std::fs::canonicalize(&b.key).unwrap();
    assert_eq!(v["gateways"][0]["auth"]["keyFile"], real_key.display().to_string(), "{text}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o077, 0, "the file must be owner-only");
    }
    assert_eq!(ligus_runtime::model_gateway::ModelGateways::parse(&text).unwrap().len(), 1);

    // Idempotent: the same add changes nothing.
    let o = ligus(&b.home, &b.data, &[&add_pt[..], &["--json"]].concat());
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let d = json(&o);
    assert_eq!(d["schema"], "ligus.gateways.v1");
    assert_eq!(d["command"], "gateway add");
    assert_eq!(d["ok"], true);
    assert_eq!(d["exitCode"], 0);
    assert_eq!(d["result"]["action"], "unchanged");
    assert_eq!(d["result"]["changed"], false);
    assert_eq!(d["exclusive"], true);
    assert_eq!(d["exclusiveSource"], "file");
    assert_eq!(d["fileSource"], "data-dir");
    assert_eq!(d["file"], file.display().to_string());
    assert_eq!(d["gateways"][0]["name"], "box-gw");
    assert_eq!(d["gateways"][0]["auth"]["mode"], "apiKey");
    assert_eq!(d["restart"], serde_json::json!([]), "nothing changed: nothing to restart");
    assert!(!out(&o).contains(SECRET));

    // The explicit opt-out: only the mode changes, no restart needed.
    let o = ligus(&b.home, &b.data, &[&add_pt[..], &["--not-exclusive", "--json"]].concat());
    let d = json(&o);
    assert_eq!(d["result"]["action"], "unchanged", "the entry is the same");
    assert_eq!(d["result"]["changed"], true);
    assert_eq!(d["exclusive"], false);
    assert_eq!(d["restart"].as_array().unwrap().len(), 1, "a mode change applies without a restart: {d}");
    // A later add without a flag keeps the operator's choice.
    let d = json(&ligus(&b.home, &b.data, &[&add_pt[..], &["--json"]].concat()));
    assert_eq!(d["exclusive"], false);
    // LIGUS_MODEL_GATEWAYS_EXCLUSIVE in the environment wins, and list says so.
    let o = ligus_env(&b.home, &b.data, &["gateway", "list"], &[("LIGUS_MODEL_GATEWAYS_EXCLUSIVE", "1")]);
    assert!(out(&o).contains("gateways only (LIGUS_MODEL_GATEWAYS_EXCLUSIVE=1"), "{}", out(&o));

    // The same name with other settings: refused (exit 1) unless --replace.
    let other = ["gateway", "add", "box-gw", "--url", "https://gw.example:8443/v1", "--api", "openai"];
    let o = ligus(&b.home, &b.data, &other);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("--replace"), "{}", err(&o));
    let d = json(&ligus(&b.home, &b.data, &[&other[..], &["--replace", "--json"]].concat()));
    assert_eq!(d["result"]["action"], "replaced");
    assert_eq!(d["gateways"][0]["url"], "https://gw.example:8443/v1");

    // list --json
    let d = json(&ligus(&b.home, &b.data, &["gateway", "list", "--json"]));
    assert_eq!(d["command"], "gateway list");
    assert_eq!(d["exists"], true);
    assert_eq!(d["gateways"].as_array().unwrap().len(), 1);

    // remove, then remove again (idempotent).
    let d = json(&ligus(&b.home, &b.data, &["gateway", "remove", "box-gw", "--json"]));
    assert_eq!(d["result"]["action"], "removed");
    assert_eq!(d["gateways"], serde_json::json!([]));
    assert_eq!(d["exclusive"], false, "no gateway listed: never exclusive");
    let o = ligus(&b.home, &b.data, &["gateway", "remove", "box-gw"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("not listed; nothing changed"), "{}", out(&o));
}

#[test]
fn refusals() {
    let b = setup();
    let h = b.home.to_str().unwrap();
    // Names, schemes and credentials in the URL: the host's own rules (exit 2).
    for args in [
        vec!["gateway", "add", "Box_GW", "--url", "https://gw", "--api", "openai"],
        vec!["gateway", "add", "gw", "--url", "https://u:p@gw", "--api", "openai"],
        vec!["gateway", "add", "gw", "--url", "ftp://gw", "--api", "openai"],
        vec!["gateway", "add", "gw", "--url", "https://169.254.169.254", "--api", "openai"],
        vec!["gateway", "add", "gw", "--url", "https://gw"],
        vec!["gateway", "add", "gw", "--url", "https://gw", "--api", "gemini"],
        vec!["gateway", "add", "gw", "--url", "https://gw", "--api", "openai", "--exclusive", "--not-exclusive"],
        vec!["gateway", "add", "gw", "--url", "https://gw", "--api", "openai", "--auth", "password"],
        vec!["gateway", "frob"],
    ] {
        let o = ligus(&b.home, &b.data, &args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", err(&o));
    }
    // A missing credential file: exit 3, with how to make one.
    let o = ligus(&b.home, &b.data, &["gateway", "add", "gw", "--url", "https://gw", "--api", "openai", "--auth", "bearer-file", "/nonexistent/gw.key"]);
    assert_eq!(o.status.code(), Some(3), "{}", err(&o));
    assert!(err(&o).contains("umask 077"), "{}", err(&o));
    // A key directly in the home dir would hide all of it from agents: refused.
    let in_home = format!("{h}/gw.key");
    std::fs::write(&in_home, SECRET).unwrap();
    let o = ligus(&b.home, &b.data, &["gateway", "add", "gw", "--url", "https://gw", "--api", "openai", "--auth", "bearer-file", &in_home]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(err(&o).contains("directory of its own"), "{}", err(&o));
    assert!(!err(&o).contains(SECRET) && !out(&o).contains(SECRET));
    // check: nothing listed is a fixable precondition; an unknown name is not found.
    let o = ligus(&b.home, &b.data, &["gateway", "check"]);
    assert_eq!(o.status.code(), Some(3), "{}", err(&o));
    let ok = ligus(&b.home, &b.data, &["gateway", "add", "gw", "--url", "https://gw.invalid", "--api", "openai"]);
    assert_eq!(ok.status.code(), Some(0), "{}", err(&ok));
    let o = ligus(&b.home, &b.data, &["gateway", "check", "nope"]);
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    // Nothing was written by the refused adds.
    let text = std::fs::read_to_string(b.data.join("model-gateways.json")).unwrap();
    assert!(!text.contains("Box_GW") && !text.contains("169.254"), "{text}");
}

/// An invalid file stops other commands (as it stops serve) but not
/// `ligus gateway`, so it can be seen and fixed; a file named by
/// LIGUS_MODEL_GATEWAYS_FILE is the one written.
#[test]
fn invalid_file_and_env_override() {
    let b = setup();
    let file = b.data.join("model-gateways.json");
    std::fs::write(&file, r#"{"gateways":[{"name":"bad","url":"http://gw:4000","auth":{"mode":"none"}}]}"#).unwrap();
    let o = ligus(&b.home, &b.data, &["status", "--json"]);
    assert_eq!(o.status.code(), Some(2), "an invalid file stops the host: {}", err(&o));
    let o = ligus(&b.home, &b.data, &["gateway", "list"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("ligus gateway remove"), "{}", err(&o));
    let o = ligus(&b.home, &b.data, &["gateway", "remove", "bad"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = ligus(&b.home, &b.data, &["status", "--json"]);
    assert_ne!(o.status.code(), Some(2), "fixed: {}", err(&o));

    let named = b.home.join("ops/gateways.json");
    std::fs::create_dir_all(named.parent().unwrap()).unwrap();
    let env = [("LIGUS_MODEL_GATEWAYS_FILE", named.to_str().unwrap())];
    let d = json(&ligus_env(&b.home, &b.data, &["gateway", "add", "gw", "--url", "https://gw.example", "--api", "anthropic", "--json"], &env));
    assert_eq!(d["fileSource"], "env");
    assert_eq!(d["file"], named.display().to_string());
    assert!(named.exists());
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.contains("gw.example"), "the data dir's file is not the one in effect");
}

#[test]
fn help_is_complete() {
    let b = setup();
    for sub in ["add", "list", "check", "remove"] {
        let o = ligus(&b.home, &b.data, &["gateway", sub, "--help"]);
        assert_eq!(o.status.code(), Some(0));
        let t = out(&o);
        for sec in ["USAGE", "FLAGS", "EXAMPLES", "EXIT CODES"] {
            assert!(t.contains(sec), "gateway {sub} --help lacks {sec}");
        }
    }
}

/// The box's service reads another gateways file (LIGUS_MODEL_GATEWAYS_FILE
/// in its env file): editing the data dir's would change nothing it reads,
/// so add refuses (exit 3) and says how to run it instead.
#[cfg(unix)]
#[test]
fn refuses_when_the_service_reads_another_file() {
    let b = setup();
    let env = b.home.join(".config/ligus-serve/ligus-serve.env");
    std::fs::create_dir_all(env.parent().unwrap()).unwrap();
    std::fs::write(&env, format!("LIGUS_DATA_DIR='{}'\nLIGUS_MODEL_GATEWAYS_FILE=/etc/ligus/gw.json\n", b.data.display())).unwrap();
    let units = b.home.join(".config/systemd/user");
    std::fs::create_dir_all(&units).unwrap();
    std::fs::write(units.join("ligus-serve.service"), format!("[Service]\nEnvironmentFile={}\n", env.display())).unwrap();
    let o = ligus(&b.home, &b.data, &["gateway", "add", "gw", "--url", "https://gw.example", "--api", "openai"]);
    assert_eq!(o.status.code(), Some(3), "{}", err(&o));
    assert!(err(&o).contains("/etc/ligus/gw.json") && err(&o).contains("set -a"), "{}", err(&o));
    assert!(!b.data.join("model-gateways.json").exists());
    // With the env file's variable in this shell, the file it names is edited.
    let named = b.home.join("gw.json");
    std::fs::write(&env, format!("LIGUS_DATA_DIR='{}'\nLIGUS_MODEL_GATEWAYS_FILE={}\n", b.data.display(), named.display())).unwrap();
    let o = ligus_env(&b.home, &b.data, &["gateway", "add", "gw", "--url", "https://gw.example", "--api", "openai"], &[("LIGUS_MODEL_GATEWAYS_FILE", named.to_str().unwrap())]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("systemctl --user restart ligus-serve"), "{}", out(&o));
}

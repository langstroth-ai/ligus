//! `ligus secrets set | list | rm`: keep a workspace secret (a model API key
//! a workspace runtime or provider names with `--secret-ref`) on this device,
//! and declare it in `host.toml` for one workspace, without exporting
//! `LIGUS_WS_SECRET_<NAME>` or editing the file by hand.
//!
//! - The value goes in this device's secret store (`LIGUS_SECRET_BACKEND`,
//!   scope [`ligus_runtime::host_config::SECRET_SCOPE`]), one value per name.
//!   It is read from stdin only (hidden when stdin is a terminal): a value on
//!   the command line lands in shell history and `ps`, so it is refused.
//! - The declaration (`[workspaces."<id>"] secrets = [...]` in `host.toml`)
//!   says which workspace may use that name here; a host resolves only names
//!   it declared for the workspace a turn is in.
//! - `LIGUS_WS_SECRET_<NAME>` (or a `LIGUS_WS_SECRET_DIR` file), when set,
//!   still wins over the stored value.
//! - Nothing here prints, logs or returns a value, in text, JSON or errors.

use std::io::Read as _;
use std::path::Path;

use anyhow::Result;
use ligus_runtime::host_config::{self as hc, HostConfig};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::help::usage_error;
use crate::problem::{Problem, EXIT_ERROR, EXIT_FIXABLE, EXIT_NOT_FOUND};

/// The schema of every `ligus secrets` `--json` document.
pub const SCHEMA: &str = "ligus.secrets.v1";

pub const USAGE: &str = "usage: ligus secrets set NAME - [--workspace ID] | list [--workspace ID] | rm NAME [--workspace ID]";

/// The longest name accepted (a longer "name" is more likely a pasted key).
const MAX_NAME: usize = 64;

/// The largest value read from stdin.
const MAX_VALUE: u64 = crate::secret_arg::MAX_SECRET_BYTES;

/// Why a value on the command line is refused. Never includes the value.
pub const REFUSE_ARGV: &str = "ligus secrets set never takes the value on the command line: it would land in your shell \
history and in ps. Pipe it in instead, e.g. ligus secrets set NAME - < keyfile. If that was a real key, treat it as exposed \
and rotate it with the provider.";

#[derive(Debug, Default)]
struct Parsed {
    words: Vec<String>,
    workspace: Option<String>,
    json: bool,
}

impl Parsed {
    fn new(rest: &[String]) -> Result<Self> {
        let mut p = Parsed::default();
        let mut it = rest.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--json" => p.json = true,
                "--workspace" => {
                    let v = it.next().filter(|v| !v.starts_with("--")).ok_or_else(|| usage_error("--workspace needs a workspace id"))?;
                    p.workspace = Some(v.clone());
                }
                "--" => p.words.extend(it.by_ref().cloned()),
                "-" => p.words.push(a.clone()),
                f if f.starts_with('-') => {
                    // Only the flag's name: `--value=…` would otherwise be echoed.
                    let name = f.split('=').next().unwrap_or(f);
                    if matches!(name, "--value" | "--secret" | "--key") {
                        return Err(usage_error(REFUSE_ARGV));
                    }
                    return Err(usage_error(format!("unknown flag {name} for `ligus secrets`; {USAGE}")));
                }
                _ => p.words.push(a.clone()),
            }
        }
        Ok(p)
    }
}

fn problem(exit: i32, code: &'static str, message: impl Into<String>, fix: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Problem { exit, code, message: message.into(), fix: Some(fix.into()) })
}

fn host_err(e: hc::HostConfigError) -> anyhow::Error {
    problem(EXIT_ERROR, "host_toml", format!("{e}"), "fix host.toml by hand (ligus help hosts shows its format), then retry")
}

fn store_err(e: ligus_runtime::secret_store::SecretError) -> anyhow::Error {
    use ligus_runtime::secret_store::SecretError;
    match e {
        SecretError::ReadOnly(what) => problem(EXIT_FIXABLE, "store_read_only", format!("this device's secret store is {what}, which is read-only"), "add the secret to the mounted Secret instead (LIGUS_WS_SECRET_DIR)"),
        e => problem(
            EXIT_FIXABLE,
            "store_unavailable",
            format!("this device's secret store refused: {e}"),
            "ligus doctor shows the store; on a headless box set LIGUS_SECRET_BACKEND=file",
        ),
    }
}

/// The workspace `--workspace` names (an id), else this data dir's.
fn workspace_of(p: &Parsed, default: Uuid) -> Result<Uuid> {
    match &p.workspace {
        Some(w) => Uuid::parse_str(w.trim()).map_err(|_| usage_error("--workspace takes a workspace id (ligus status shows it)")),
        None => Ok(default),
    }
}

/// A secret name, checked. An invalid one isn't echoed (it may be a key).
fn name_of(p: &Parsed) -> Result<String> {
    let Some(n) = p.words.get(1) else { return Err(usage_error(format!("a secret NAME is needed; {USAGE}"))) };
    if !hc::valid_secret_name(n) || n.len() > MAX_NAME {
        return Err(usage_error(format!(
            "a secret name is 1-{MAX_NAME} letters, digits, _ and - (it becomes LIGUS_WS_SECRET_<NAME>); the one given isn't"
        )));
    }
    Ok(n.clone())
}

/// `ligus secrets …` (the words after `secrets`).
pub fn cmd(data_dir: &Path, workspace: Uuid, rest: &[String]) -> Result<()> {
    let p = Parsed::new(rest)?;
    match p.words.first().map(String::as_str) {
        Some("set") => set(data_dir, &p, workspace, &mut read_value),
        Some("list") | None => list(data_dir, &p, workspace),
        Some("rm") => rm(data_dir, &p, workspace),
        Some(_) => Err(usage_error(format!("unknown `ligus secrets` subcommand; {USAGE}"))),
    }
}

/// The value from stdin: hidden when it is a terminal, else all of it (at
/// most [`MAX_VALUE`] bytes). One trailing line end is dropped.
fn read_value(name: &str) -> Result<zeroize::Zeroizing<Vec<u8>>> {
    use std::io::IsTerminal;
    let mut buf = zeroize::Zeroizing::new(Vec::new());
    if std::io::stdin().is_terminal() {
        let typed = zeroize::Zeroizing::new(crate::link_io::read_hidden_max(&format!("Value for {name} (hidden), then Enter: "), MAX_VALUE as usize)?);
        buf.extend_from_slice(typed.as_bytes());
    } else {
        std::io::stdin().lock().take(MAX_VALUE + 1).read_to_end(&mut buf)?;
    }
    Ok(buf)
}

/// `value` with one trailing `\n` or `\r\n` dropped, checked: not empty, not
/// too big, UTF-8. Errors never include it.
fn clean_value(mut v: zeroize::Zeroizing<Vec<u8>>) -> Result<zeroize::Zeroizing<Vec<u8>>> {
    if v.len() as u64 > MAX_VALUE {
        return Err(usage_error(format!("the value on stdin is bigger than {} KiB", MAX_VALUE / 1024)));
    }
    if v.ends_with(b"\n") {
        v.pop();
        if v.ends_with(b"\r") {
            v.pop();
        }
    }
    if v.is_empty() {
        return Err(usage_error("the value on stdin is empty: pipe the secret in (ligus secrets set NAME - < keyfile)"));
    }
    if std::str::from_utf8(&v).is_err() {
        return Err(usage_error("the value on stdin isn't UTF-8 text"));
    }
    Ok(v)
}

const RESTART: &str = "A running ligus serve or worker on this data dir reads host.toml at start: restart it to use the change.";

fn set(data_dir: &Path, p: &Parsed, default_ws: Uuid, read: &mut dyn FnMut(&str) -> Result<zeroize::Zeroizing<Vec<u8>>>) -> Result<()> {
    // The value is never an argument: refused before anything is read.
    match p.words.len() {
        0..=1 => return Err(usage_error(format!("a secret NAME is needed; {USAGE}"))),
        2 => {}
        3 if p.words[2] == "-" => {}
        _ => return Err(usage_error(REFUSE_ARGV)),
    }
    let name = name_of(p)?;
    let ws = workspace_of(p, default_ws)?;
    let value = clean_value(read(&name)?)?;
    let store = ligus_runtime::secret_store::for_data_dir(data_dir);
    let existed = store.get(&hc::secret_id(&name)).map_err(store_err)?.is_some();
    store.put(&hc::secret_id(&name), &value).map_err(store_err)?;
    drop(value);
    let declared = hc::declare_secret(data_dir, ws, &name).map_err(host_err)?;
    let cfg = HostConfig::load(data_dir).map_err(host_err)?;
    let others: Vec<String> = cfg.workspaces_declaring(&name).into_iter().filter(|w| *w != ws.to_string()).collect();
    let env_wins = env_holds(&name);
    if p.json {
        println!(
            "{}",
            json!({
                "schema": SCHEMA,
                "action": "set",
                "name": name,
                "workspace": ws.to_string(),
                "backend": store.backend().as_str(),
                "replaced": existed,
                "declared": declared,
                "alsoDeclaredFor": others,
                "envOverrides": env_wins,
                "hostToml": hc::path_in(data_dir).display().to_string(),
            })
        );
        return Ok(());
    }
    println!(
        "{} {name} in this device's secret store ({}).",
        if existed { "Replaced" } else { "Saved" },
        store.backend().as_str()
    );
    if declared {
        println!("Declared {name} for workspace {ws} in {}.", hc::path_in(data_dir).display());
    } else {
        println!("{name} was already declared for workspace {ws}.");
    }
    if !others.is_empty() {
        println!("The same value is used in {} other workspace(s) that declare {name}: {}.", others.len(), others.join(", "));
    }
    if env_wins {
        println!("Note: LIGUS_WS_SECRET_{name} is set in this environment and wins over the stored value.");
    }
    println!("Use it with: ligus add-runtime <id> <name> <provider> <model> --secret-ref {name}");
    println!("{RESTART}");
    Ok(())
}

/// Whether `LIGUS_WS_SECRET_<name>` is set (or came from a file).
fn env_holds(name: &str) -> bool {
    hc::env_secret_names().iter().any(|n| n == name)
}

/// One row of `list`.
fn row(store: &dyn ligus_runtime::secret_store::SecretStore, cfg: &HostConfig, name: &str, ws: Uuid) -> Value {
    let env = env_holds(name);
    let stored = store.get(&hc::secret_id(name)).ok().flatten().is_some();
    let workspaces = cfg.workspaces_declaring(name);
    let source = if env {
        "env"
    } else if stored {
        "store"
    } else {
        "missing"
    };
    json!({
        "name": name,
        "declaredFor": workspaces,
        "declaredHere": cfg.declares_secret(ws, name),
        "source": source,
        "stored": stored,
        "env": env,
    })
}

fn list(data_dir: &Path, p: &Parsed, default_ws: Uuid) -> Result<()> {
    if p.words.len() > 1 {
        return Err(usage_error(format!("ligus secrets list takes no NAME; {USAGE}")));
    }
    let ws = workspace_of(p, default_ws)?;
    let cfg = HostConfig::load(data_dir).map_err(host_err)?;
    let store = ligus_runtime::secret_store::for_data_dir(data_dir);
    let mut names = cfg.declared_names();
    names.extend(hc::env_secret_names());
    names.sort();
    names.dedup();
    let rows: Vec<Value> = names.iter().map(|n| row(store.as_ref(), &cfg, n, ws)).collect();
    if p.json {
        println!(
            "{}",
            json!({
                "schema": SCHEMA,
                "action": "list",
                "workspace": ws.to_string(),
                "backend": store.backend().as_str(),
                "hostToml": hc::path_in(data_dir).display().to_string(),
                "secrets": rows,
            })
        );
        return Ok(());
    }
    if rows.is_empty() {
        println!("No workspace secrets on this device. Add one: ligus secrets set NAME - < keyfile");
        return Ok(());
    }
    println!("Workspace secrets on this device (values are never shown); this workspace: {ws}");
    for r in &rows {
        let name = r["name"].as_str().unwrap_or("");
        let wss: Vec<&str> = r["declaredFor"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        let held = match r["source"].as_str() {
            Some("env") => "held: environment",
            Some("store") => "held: secret store",
            _ => "no value here",
        };
        let decl = if wss.is_empty() {
            "not declared (unused)".to_string()
        } else {
            let here = if r["declaredHere"].as_bool() == Some(true) { " (this one)" } else { "" };
            format!("declared for {}{here}", wss.join(", "))
        };
        println!("  {name:<24} {held:<20} {decl}");
    }
    Ok(())
}

fn rm(data_dir: &Path, p: &Parsed, default_ws: Uuid) -> Result<()> {
    if p.words.len() > 2 {
        return Err(usage_error(format!("ligus secrets rm takes one NAME; {USAGE}")));
    }
    let name = name_of(p)?;
    let ws = workspace_of(p, default_ws)?;
    let store = ligus_runtime::secret_store::for_data_dir(data_dir);
    let undeclared = hc::undeclare_secret(data_dir, ws, &name).map_err(host_err)?;
    let cfg = HostConfig::load(data_dir).map_err(host_err)?;
    let kept_for = cfg.workspaces_declaring(&name);
    let stored = store.get(&hc::secret_id(&name)).map_err(store_err)?.is_some();
    if !undeclared && !(stored && kept_for.is_empty()) {
        return Err(problem(
            EXIT_NOT_FOUND,
            "not_found",
            format!("{name} isn't declared for workspace {ws} on this device"),
            "ligus secrets list shows the names and where they're declared",
        ));
    }
    // The value goes only when no other workspace here still uses it.
    let deleted = stored && kept_for.is_empty();
    if deleted {
        store.delete(&hc::secret_id(&name)).map_err(store_err)?;
    }
    let env = env_holds(&name);
    if p.json {
        println!(
            "{}",
            json!({
                "schema": SCHEMA,
                "action": "rm",
                "name": name,
                "workspace": ws.to_string(),
                "undeclared": undeclared,
                "deletedFromStore": deleted,
                "keptFor": kept_for,
                "envStillSet": env,
            })
        );
        return Ok(());
    }
    if undeclared {
        println!("{name} is no longer declared for workspace {ws}.");
    }
    if deleted {
        println!("Deleted {name}'s value from this device's secret store.");
    } else if stored {
        println!("Kept the stored value: still declared for {}.", kept_for.join(", "));
    }
    if env {
        println!("Note: LIGUS_WS_SECRET_{name} is still set in this environment; remove it there too.");
    }
    println!("{RESTART}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| s.to_string()).collect()
    }

    fn z(b: &[u8]) -> zeroize::Zeroizing<Vec<u8>> {
        zeroize::Zeroizing::new(b.to_vec())
    }

    #[test]
    fn values_on_argv_are_refused_unechoed() {
        for args in [&["set", "acme", "sk-live-123"][..], &["set", "acme", "-", "sk-live-123"], &["set", "acme", "--value=sk-live-123"]] {
            let dir = tempfile::tempdir().unwrap();
            let e = Parsed::new(&words(args)).and_then(|p| set(dir.path(), &p, Uuid::new_v4(), &mut |_| panic!("stdin read before refusing")));
            let msg = format!("{:#}", e.unwrap_err());
            assert!(msg.contains("never takes the value on the command line"), "{msg}");
            assert!(!msg.contains("sk-live-123"), "the value is never echoed: {msg}");
            assert!(!hc::path_in(dir.path()).exists());
        }
    }

    #[test]
    fn values_are_cleaned() {
        assert_eq!(clean_value(z(b"sk-1\n")).unwrap().as_slice(), b"sk-1");
        assert_eq!(clean_value(z(b"sk-1\r\n")).unwrap().as_slice(), b"sk-1");
        assert_eq!(clean_value(z(b"sk-1\n\n")).unwrap().as_slice(), b"sk-1\n", "only one line end goes");
        assert!(clean_value(z(b"\n")).is_err());
        assert!(clean_value(z(b"")).is_err());
        assert!(clean_value(z(&[0xff, 0xfe])).is_err());
        assert!(clean_value(z(&vec![b'a'; MAX_VALUE as usize + 1])).is_err());
    }

    #[test]
    fn bad_names_are_not_echoed() {
        let dir = tempfile::tempdir().unwrap();
        let long = "k".repeat(MAX_NAME + 1);
        for n in ["sk live", long.as_str()] {
            let p = Parsed::new(&words(&["set", n, "-"])).unwrap();
            let msg = set(dir.path(), &p, Uuid::new_v4(), &mut |_| Ok(z(b"v"))).unwrap_err().to_string();
            assert!(!msg.contains(n), "{msg}");
        }
    }

    #[test]
    fn set_list_rm_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let (ws, other) = (Uuid::new_v4(), Uuid::new_v4());
        let p = Parsed::new(&words(&["set", "team-claude-t1", "-"])).unwrap();
        set(dir.path(), &p, ws, &mut |_| Ok(z(b"sk-abc\n"))).unwrap();
        let p = Parsed::new(&words(&["set", "team-claude-t1", "--workspace", &other.to_string()])).unwrap();
        set(dir.path(), &p, ws, &mut |_| Ok(z(b"sk-abc"))).unwrap();
        let store = ligus_runtime::secret_store::for_data_dir(dir.path());
        assert_eq!(store.get(&hc::secret_id("team-claude-t1")).unwrap().unwrap().as_slice(), b"sk-abc");
        let cfg = HostConfig::load(dir.path()).unwrap();
        assert!(cfg.declares_secret(ws, "team-claude-t1") && cfg.declares_secret(other, "team-claude-t1"));
        let r = row(store.as_ref(), &cfg, "team-claude-t1", ws);
        assert_eq!(r["source"], "store");
        assert!(!r.to_string().contains("sk-abc"), "a row never holds the value");
        // Removing it from one workspace keeps the value for the other.
        rm(dir.path(), &Parsed::new(&words(&["rm", "team-claude-t1"])).unwrap(), ws).unwrap();
        assert!(store.get(&hc::secret_id("team-claude-t1")).unwrap().is_some());
        assert!(!HostConfig::load(dir.path()).unwrap().declares_secret(ws, "team-claude-t1"));
        let again = rm(dir.path(), &Parsed::new(&words(&["rm", "team-claude-t1"])).unwrap(), ws).unwrap_err();
        assert_eq!(again.downcast_ref::<Problem>().map(|p| p.exit), Some(EXIT_NOT_FOUND));
        rm(dir.path(), &Parsed::new(&words(&["rm", "team-claude-t1", "--workspace", &other.to_string()])).unwrap(), ws).unwrap();
        assert!(store.get(&hc::secret_id("team-claude-t1")).unwrap().is_none(), "the last workspace's rm deletes the value");
    }
}

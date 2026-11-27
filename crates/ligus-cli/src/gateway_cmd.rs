//! `ligus gateway add | list | check | remove`: list the model gateways this
//! host (a box running `ligus serve`, or a worker) may send agent model
//! traffic through, without writing the allow-list file by hand.
//!
//! - The file is the one the host reads ([`mg::gateways_file`]):
//!   `LIGUS_MODEL_GATEWAYS_FILE` when set, else `model-gateways.json` in the
//!   data dir. It is written whole and atomically, owner-only.
//! - `add` checks an entry with the rules the host loads it with (https
//!   unless `--allow-plaintext`, names `[a-z0-9-]`, no credentials in the URL)
//!   and stores a credential only as a file path: it never reads, stores or
//!   prints the secret.
//! - Exclusive mode (every model turn on this host goes through a listed
//!   gateway) stays the default once a gateway is listed; `add` writes the
//!   choice into the file and says it in one line, and `--not-exclusive`
//!   is the explicit opt-out.
//! - A running serve or worker reads the file again when it changes
//!   ([`mg::watch_host`]); the gateway names it advertises to the desktop
//!   change at its next start, so `add` and `remove` print the restart
//!   command for how this data dir's serve runs (systemd, launchd, brew).

use std::path::{Path, PathBuf};

use anyhow::Result;
use ligus_runtime::model_gateway::{self as mg, Api, AuthConfig, ExclusiveSource, FileSource, GatewayConfig, GatewaysFile};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::help::usage_error;
use crate::problem::{Problem, EXIT_ERROR, EXIT_FIXABLE, EXIT_NOT_FOUND};

/// The schema of `add`, `list` and `remove`'s `--json` document.
pub const SCHEMA: &str = "ligus.gateways.v1";

pub const USAGE: &str = "usage: ligus gateway add NAME --url URL --api openai|anthropic [--auth none | bearer-file PATH | api-key-file PATH | token-file PATH | mtls] \
     [--allow-plaintext] [--exclusive | --not-exclusive] [--replace] | list | check [NAME] [--model ID] | remove NAME";

/// Flags that take a value (`--auth` may take a second one: its file).
const VALUE: &[&str] = &["--url", "--api", "--auth", "--header", "--cert-file", "--client-key-file", "--ca-file", "--model"];
const BOOL: &[&str] = &["--allow-plaintext", "--attribution", "--exclusive", "--not-exclusive", "--replace", "--json"];

/// The words and flags after `ligus gateway`.
#[derive(Debug, Default)]
struct Parsed {
    words: Vec<String>,
    values: Vec<(String, String)>,
    bools: Vec<String>,
    /// `--auth bearer-file PATH` (and the other file modes): PATH.
    auth_file: Option<String>,
}

impl Parsed {
    fn new(rest: &[String]) -> Result<Self> {
        let mut p = Parsed::default();
        let mut it = rest.iter().peekable();
        while let Some(a) = it.next() {
            if a == "--" {
                p.words.extend(it.by_ref().cloned());
                break;
            }
            if !a.starts_with('-') || a == "-" {
                p.words.push(a.clone());
                continue;
            }
            if BOOL.contains(&a.as_str()) {
                p.bools.push(a.clone());
            } else if VALUE.contains(&a.as_str()) {
                let v = it.next().filter(|v| !v.starts_with("--")).ok_or_else(|| usage_error(format!("{a} needs a value")))?;
                if a == "--auth" && matches!(v.as_str(), "bearer-file" | "api-key-file" | "token-file") {
                    let f = it.next().filter(|v| !v.starts_with("--")).ok_or_else(|| usage_error(format!("--auth {v} needs the credential file's path")))?;
                    p.auth_file = Some(f.clone());
                }
                p.values.push((a.clone(), v.clone()));
            } else {
                return Err(usage_error(format!("unknown flag {a} for `ligus gateway`")));
            }
        }
        Ok(p)
    }
    fn has(&self, f: &str) -> bool {
        self.bools.iter().any(|b| b == f)
    }
    fn flag_val(&self, f: &str) -> Option<String> {
        self.values.iter().rev().find(|(k, _)| k == f).map(|(_, v)| v.clone())
    }
}

fn problem(exit: i32, code: &'static str, message: impl Into<String>, fix: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Problem { exit, code, message: message.into(), fix: Some(fix.into()) })
}

/// `ligus gateway …` (the words after `gateway`).
pub fn cmd(data_dir: &Path, workspace: Option<Uuid>, rest: &[String]) -> Result<()> {
    let p = Parsed::new(rest)?;
    match p.words.first().map(String::as_str) {
        Some("add") => add(data_dir, &p),
        Some("list") | None => list(data_dir, &p),
        Some("check") => check(workspace, &p),
        Some("remove") => remove(data_dir, &p),
        Some(other) => Err(usage_error(format!("unknown `ligus gateway {other}`; {USAGE}"))),
    }
}

// ------------------------------------------------------------------ the file

/// The file this host reads, and where that choice came from.
fn file_of(data_dir: &Path) -> (PathBuf, FileSource) {
    mg::gateways_file(Some(data_dir)).unwrap_or_else(|| (data_dir.join(mg::DEFAULT_GATEWAYS_FILE), FileSource::DataDir))
}

/// The file's contents; None when it doesn't exist yet. Invalid JSON is an
/// error: it is never overwritten (a person wrote something there).
fn read(path: &Path) -> Result<Option<GatewaysFile>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(problem(EXIT_ERROR, "unreadable", format!("can't read {}: {e}", path.display()), "check the file's owner and mode")),
    };
    GatewaysFile::from_json(&text).map(Some).map_err(|e| {
        problem(EXIT_ERROR, "invalid", format!("{} is not a valid gateways file: {e}", path.display()), format!("fix it by hand, or move it away and add the gateways again ({})", path.display()))
    })
}

fn write(path: &Path, f: &GatewaysFile) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    ligus_runtime::private_file::write_private_atomic(path, f.to_json().as_bytes())
        .map_err(|e| problem(EXIT_ERROR, "unwritable", format!("can't write {}: {e}", path.display()), "check the directory's owner and mode"))
}

fn exclusive_env() -> Option<String> {
    ligus_core::names::var(mg::EXCLUSIVE_ENV).ok().filter(|v| !v.trim().is_empty())
}

/// The names and wires a host advertises for `f` (what the desktop lists).
fn offers(f: &GatewaysFile) -> Vec<(String, &'static str)> {
    let mut v: Vec<(String, &'static str)> = f.gateways.iter().map(|g| (g.name.clone(), g.api.as_str())).collect();
    v.sort();
    v
}

/// One entry as JSON: its settings and credential paths (never a secret).
fn entry_json(g: &GatewayConfig) -> Value {
    serde_json::to_value(g).unwrap_or(Value::Null)
}

/// One entry in a line: name, wire, URL, how it authenticates.
fn entry_line(g: &GatewayConfig) -> String {
    let auth = match &g.auth {
        AuthConfig::None => "no credential".to_string(),
        AuthConfig::ApiKey { key_file, header } => match header.as_deref() {
            None | Some("authorization") => format!("bearer key file {}", key_file.display()),
            Some(h) => format!("{h} key file {}", key_file.display()),
        },
        AuthConfig::ServiceAccountToken { token_file } => format!("token file {}", token_file.display()),
        AuthConfig::Mtls { cert_file, client_key_file } => format!("mtls {} {}", cert_file.display(), client_key_file.display()),
    };
    let mut s = format!("{:<20} {:<9} {}  ({auth}", g.name, g.api.as_str(), g.url);
    if g.allow_plaintext && g.url.trim_start().starts_with("http://") {
        s.push_str(", plain http");
    }
    if g.attribution {
        s.push_str(", attribution");
    }
    s.push(')');
    s
}

/// The document `add`, `list` and `remove` print with `--json`.
fn doc(command: &str, path: &Path, source: FileSource, f: Option<&GatewaysFile>) -> Value {
    let f0 = GatewaysFile::default();
    let f = f.unwrap_or(&f0);
    let env = exclusive_env();
    let (exclusive, src) = mg::exclusive_of(f.gateways.len(), f.exclusive, env.as_deref());
    json!({
        "schema": SCHEMA,
        "tool": "ligus",
        "version": env!("CARGO_PKG_VERSION"),
        "command": command,
        "ok": true,
        "exitCode": 0,
        "file": path.display().to_string(),
        "fileSource": source.as_str(),
        "exclusive": exclusive,
        "exclusiveSource": src.as_str(),
        "gateways": f.gateways.iter().map(entry_json).collect::<Vec<_>>(),
    })
}

// ----------------------------------------------------------------------- add

/// A credential path as the file stores it: absolute (serve's working
/// directory isn't this shell's) and, when it exists, with every symbolic
/// link resolved, since the host hides the path and its directory from
/// agents as written.
fn real_path(p: impl AsRef<Path>) -> Result<PathBuf> {
    let p = p.as_ref();
    Ok(std::fs::canonicalize(p).or_else(|_| std::path::absolute(p))?)
}

/// The `--auth` choice as the file's auth entry, its paths [`real_path`]s.
fn auth_of(p: &Parsed) -> Result<AuthConfig> {
    let mode = p.flag_val("--auth").unwrap_or_else(|| "none".into());
    let file = || -> Result<PathBuf> {
        let f = p.auth_file.clone().ok_or_else(|| usage_error(format!("--auth {mode} needs the credential file's path")))?;
        real_path(&f)
    };
    let header = p.flag_val("--header");
    if header.is_some() && mode != "api-key-file" {
        return Err(usage_error("--header goes with --auth api-key-file"));
    }
    if (p.flag_val("--cert-file").is_some() || p.flag_val("--client-key-file").is_some()) && mode != "mtls" {
        return Err(usage_error("--cert-file and --client-key-file go with --auth mtls"));
    }
    Ok(match mode.as_str() {
        "none" => AuthConfig::None,
        "bearer-file" => AuthConfig::ApiKey { key_file: file()?, header: None },
        "api-key-file" => {
            let header = match header.as_deref().map(str::to_ascii_lowercase).as_deref() {
                None | Some("authorization") => None,
                Some(h @ ("x-api-key" | "api-key")) => Some(h.to_string()),
                Some(h) => return Err(usage_error(format!("--header {h}: use authorization (bearer), x-api-key or api-key"))),
            };
            AuthConfig::ApiKey { key_file: file()?, header }
        }
        "token-file" => AuthConfig::ServiceAccountToken { token_file: file()? },
        "mtls" => {
            let cert = p.flag_val("--cert-file").ok_or_else(|| usage_error("--auth mtls needs --cert-file PATH and --client-key-file PATH"))?;
            let key = p.flag_val("--client-key-file").ok_or_else(|| usage_error("--auth mtls needs --cert-file PATH and --client-key-file PATH"))?;
            AuthConfig::Mtls { cert_file: real_path(cert)?, client_key_file: real_path(key)? }
        }
        m => return Err(usage_error(format!("--auth {m}: use none, bearer-file PATH, api-key-file PATH, token-file PATH or mtls"))),
    })
}

/// Directories a credential must not sit directly in: the host hides a
/// credential's directory from every agent, so a key in the home directory
/// would hide all of it.
fn too_broad(dir: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let dir = canon(dir);
    if dir.parent().is_none() {
        return true;
    }
    if let Some(home) = ligus_runtime::executor::home_dir().map(|h| canon(&h)) {
        if home.starts_with(&dir) {
            return true;
        }
    }
    const SYSTEM: &[&str] = &["/tmp", "/private/tmp", "/var/tmp", "/private/var/tmp", "/etc", "/private/etc", "/usr", "/usr/local", "/opt", "/opt/homebrew", "/var", "/private/var"];
    SYSTEM.iter().any(|s| dir == Path::new(s))
}

/// Check a credential file without reading it. Returns warnings.
fn check_secret_file(name: &str, f: &Path) -> Result<Vec<String>> {
    let shown = f.display();
    let fix_new = format!(
        "create it first, owner-only, in a directory of its own: mkdir -p ~/.config/ligus-gateways && (umask 077; cat > ~/.config/ligus-gateways/{name}.key)"
    );
    let md = match std::fs::metadata(f) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(problem(EXIT_FIXABLE, "missing_file", format!("credential file {shown} doesn't exist"), fix_new));
        }
        Err(e) => return Err(problem(EXIT_FIXABLE, "unreadable", format!("credential file {shown}: {e}"), "check its owner and mode")),
    };
    if !md.is_file() {
        return Err(problem(EXIT_FIXABLE, "not_a_file", format!("credential path {shown} is not a file"), fix_new));
    }
    if md.len() == 0 {
        return Err(problem(EXIT_FIXABLE, "empty_file", format!("credential file {shown} is empty"), "write the key into it (it is read on every request, never copied)"));
    }
    if f.parent().is_none_or(too_broad) {
        return Err(usage_error(format!(
            "credential file {shown}: its directory would be hidden from every agent on this host (a credential's directory is); \
             keep it in a directory of its own, for example ~/.config/ligus-gateways/{name}.key"
        )));
    }
    let mut warn = Vec::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if md.permissions().mode() & 0o077 != 0 {
            warn.push(format!("{shown} can be read by other users: chmod 600 {}", shell_word(&shown.to_string())));
        }
    }
    Ok(warn)
}

fn add(data_dir: &Path, p: &Parsed) -> Result<()> {
    let name = p.words.get(1).cloned().ok_or_else(|| usage_error(format!("ligus gateway add needs a NAME; {USAGE}")))?;
    let url = p.flag_val("--url").or_else(|| p.words.get(2).cloned()).ok_or_else(|| usage_error("ligus gateway add needs --url URL (the gateway's base URL)"))?;
    if p.words.len() > 3 || (p.words.len() == 3 && p.flag_val("--url").is_some()) {
        return Err(usage_error(format!("unexpected argument {:?}; {USAGE}", p.words.last().cloned().unwrap_or_default())));
    }
    let api = match p.flag_val("--api").as_deref() {
        Some("openai") => Api::Openai,
        Some("anthropic") => Api::Anthropic,
        Some(a) => return Err(usage_error(format!("--api {a}: use openai (chat completions) or anthropic (Messages)"))),
        None => return Err(usage_error("ligus gateway add needs --api openai|anthropic (the wire the gateway speaks)")),
    };
    if p.has("--exclusive") && p.has("--not-exclusive") {
        return Err(usage_error("--exclusive and --not-exclusive can't both be given"));
    }
    let auth = auth_of(p)?;
    let plaintext = url.trim().to_ascii_lowercase().starts_with("http://");
    let cfg = GatewayConfig {
        name: name.clone(),
        url: url.trim().to_string(),
        api,
        allow_plaintext: plaintext && p.has("--allow-plaintext"),
        attribution: p.has("--attribution"),
        auth,
        ca_file: p.flag_val("--ca-file").map(real_path).transpose()?,
    };
    // The host's own rules.
    if let Err(e) = mg::Gateway::new(cfg.clone()) {
        let msg = e.to_string();
        if plaintext && !p.has("--allow-plaintext") {
            return Err(usage_error(format!(
                "{msg}. Plain http to a gateway is the box operator's choice: add --allow-plaintext (prefer https, for example tailscale serve)"
            )));
        }
        return Err(usage_error(msg));
    }
    let mut notices: Vec<String> = Vec::new();
    for f in mg::Gateway::new(cfg.clone()).map(|g| g.secret_files()).unwrap_or_default() {
        notices.extend(check_secret_file(&name, &f)?);
    }
    if let Some(ca) = &cfg.ca_file {
        if !ca.is_file() {
            return Err(problem(EXIT_FIXABLE, "missing_file", format!("CA file {} doesn't exist", ca.display()), "give the PEM file of the CA that signed the gateway's certificate"));
        }
    }

    let (path, source) = file_of(data_dir);
    let svc = service_for(data_dir);
    same_file_as_service(svc.as_ref(), &path)?;
    let before = read(&path)?;
    let existed = before.is_some();
    let mut f = before.clone().unwrap_or_default();
    let had_exclusive = mg::exclusive_of(f.gateways.len(), f.exclusive, None).0;
    let action = match f.gateways.iter().position(|g| g.name == name) {
        Some(i) if f.gateways[i] == cfg => "unchanged",
        Some(_) if !p.has("--replace") => {
            return Err(problem(
                EXIT_ERROR,
                "exists",
                format!("gateway '{name}' is already listed with other settings"),
                format!("add --replace to overwrite it, or ligus gateway remove {name} first (ligus gateway list shows it)"),
            ));
        }
        Some(i) => {
            f.gateways[i] = cfg.clone();
            "replaced"
        }
        None => {
            if f.gateways.len() >= ligus_core::workspace_host::MAX_HOST_GATEWAYS {
                return Err(problem(
                    EXIT_ERROR,
                    "too_many",
                    format!("this host already lists {} gateways, the most it may", f.gateways.len()),
                    "remove one first: ligus gateway remove NAME",
                ));
            }
            f.gateways.push(cfg.clone());
            "added"
        }
    };
    // Exclusive: what the flag says; else what the file already says; else on
    // (written down, so the file states the choice).
    f.exclusive = Some(if p.has("--not-exclusive") {
        false
    } else if p.has("--exclusive") {
        true
    } else {
        f.exclusive.unwrap_or(true)
    });
    f.validate().map_err(|e| {
        problem(EXIT_ERROR, "invalid", format!("{}: another entry is invalid: {e}", path.display()), format!("fix or remove it: ligus gateway remove NAME ({})", path.display()))
    })?;
    let changed = before.as_ref() != Some(&f);
    if changed {
        write(&path, &f)?;
    }

    let excl = f.exclusive.unwrap_or(true);
    if plaintext {
        notices.push("Plain http is readable on the path. Prefer HTTPS (e.g. tailscale serve).".into());
    }
    let again = |mode: &str| add_command(&cfg, mode);
    if excl && !had_exclusive {
        notices.push(format!("This box now sends model traffic only through its gateways; other agents here will be refused. Allow both: {}", again("--not-exclusive")));
    } else if excl {
        notices.push("This box sends model traffic only through its gateways (exclusive).".into());
    } else {
        notices.push(format!("Agents on this box that don't use a gateway still run directly (not exclusive). Gateways only: {}", again("--exclusive")));
    }
    notices.extend(env_override_notice(svc.as_ref(), excl));
    let restart = if changed { restart_lines(svc.as_ref(), offers(before.as_ref().unwrap_or(&GatewaysFile::default())) != offers(&f)) } else { Vec::new() };

    if p.has("--json") {
        let mut d = doc("gateway add", &path, source, Some(&f));
        d["result"] = json!({ "action": action, "changed": changed, "created": !existed, "gateway": entry_json(&cfg) });
        d["notices"] = json!(notices);
        d["restart"] = json!(restart);
        println!("{d}");
        return Ok(());
    }
    match action {
        "unchanged" if changed => println!("Gateway {name} is already listed with these settings; this box's mode changed."),
        "unchanged" => println!("Gateway {name} is already listed with these settings; nothing changed."),
        "replaced" => println!("Replaced gateway {name}: {}", cfg.url),
        _ => println!("Added gateway {name} ({}, {})", cfg.api.as_str(), cfg.url),
    }
    println!("  file: {}", path.display());
    for n in notices.iter().chain(restart.iter()) {
        println!("{n}");
    }
    Ok(())
}

/// The `ligus gateway add` line that writes exactly `g`, plus `mode`.
fn add_command(g: &GatewayConfig, mode: &str) -> String {
    let w = |p: &Path| shell_word(&p.display().to_string());
    let mut c = format!("ligus gateway add {} --url {} --api {}", g.name, shell_word(&g.url), g.api.as_str());
    match &g.auth {
        AuthConfig::None => {}
        AuthConfig::ApiKey { key_file, header: None } => c.push_str(&format!(" --auth bearer-file {}", w(key_file))),
        AuthConfig::ApiKey { key_file, header: Some(h) } => c.push_str(&format!(" --auth api-key-file {} --header {h}", w(key_file))),
        AuthConfig::ServiceAccountToken { token_file } => c.push_str(&format!(" --auth token-file {}", w(token_file))),
        AuthConfig::Mtls { cert_file, client_key_file } => {
            c.push_str(&format!(" --auth mtls --cert-file {} --client-key-file {}", w(cert_file), w(client_key_file)))
        }
    }
    if let Some(ca) = &g.ca_file {
        c.push_str(&format!(" --ca-file {}", w(ca)));
    }
    if g.allow_plaintext {
        c.push_str(" --allow-plaintext");
    }
    if g.attribution {
        c.push_str(" --attribution");
    }
    c.push(' ');
    c.push_str(mode);
    c
}

/// `s` as one shell word (single-quoted when it needs it).
fn shell_word(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:@%+=,".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

// ---------------------------------------------------------------------- list

fn list(data_dir: &Path, p: &Parsed) -> Result<()> {
    if p.words.len() > 1 {
        return Err(usage_error(format!("`ligus gateway list` takes no argument; {USAGE}")));
    }
    let (path, source) = file_of(data_dir);
    let f = read(&path)?;
    // Entries the host would refuse: listed as a failure, as serve would stop.
    let invalid = f.as_ref().and_then(|f| f.validate().err());
    if p.has("--json") {
        let mut d = doc("gateway list", &path, source, f.as_ref());
        d["exists"] = json!(f.is_some());
        if let Some(e) = &invalid {
            d["ok"] = json!(false);
            d["exitCode"] = json!(EXIT_ERROR);
            d["error"] = json!({ "code": "invalid", "message": e.to_string(), "fix": "fix or remove the entry: ligus gateway remove NAME" });
        }
        println!("{d}");
        if invalid.is_some() {
            std::process::exit(EXIT_ERROR);
        }
        return Ok(());
    }
    let from = match source {
        FileSource::Env => "from LIGUS_MODEL_GATEWAYS_FILE",
        FileSource::DataDir => "in the data dir",
    };
    let Some(f) = f.filter(|f| !f.gateways.is_empty()) else {
        println!("No model gateways listed ({} {from}).", path.display());
        println!("Add one: ligus gateway add NAME --url URL --api openai|anthropic");
        return Ok(());
    };
    println!("Gateways file: {} ({from})", path.display());
    let env = exclusive_env();
    let (excl, src) = mg::exclusive_of(f.gateways.len(), f.exclusive, env.as_deref());
    let why = match src {
        ExclusiveSource::Env => format!(" (LIGUS_MODEL_GATEWAYS_EXCLUSIVE={} in this environment)", env.as_deref().unwrap_or("").trim()),
        ExclusiveSource::File => String::new(),
        ExclusiveSource::Default => " (the default)".into(),
    };
    if excl {
        println!("Mode: gateways only{why}: model traffic goes only through these; other agent turns here are refused");
    } else {
        println!("Mode: not exclusive{why}: agents that don't use a gateway still run directly");
    }
    for g in &f.gateways {
        println!("  {}", entry_line(g));
    }
    if let Some(e) = invalid {
        return Err(problem(EXIT_ERROR, "invalid", format!("the host refuses this file: {e}"), "fix or remove the entry: ligus gateway remove NAME; serve and workers won't start (or refuse every model turn) until then"));
    }
    Ok(())
}

// --------------------------------------------------------------------- check

fn check(workspace: Option<Uuid>, p: &Parsed) -> Result<()> {
    if p.words.len() > 2 {
        return Err(usage_error(format!("`ligus gateway check` takes at most one NAME; {USAGE}")));
    }
    let name = p.words.get(1).cloned();
    let host = mg::host();
    if host.is_empty() {
        return Err(problem(EXIT_FIXABLE, "no_gateways", "this host lists no model gateways", "ligus gateway add NAME --url URL --api openai|anthropic"));
    }
    if let Some(n) = &name {
        if host.get(n).is_none() {
            return Err(anyhow::Error::new(Problem {
                exit: EXIT_NOT_FOUND,
                code: "not_found",
                message: format!("this host lists no gateway '{n}'"),
                fix: Some("ligus gateway list shows the names".into()),
            }));
        }
    }
    let model = p.flag_val("--model").unwrap_or_else(|| "ligus-gateway-check".into());
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    if p.has("--json") {
        let mut r = crate::checks::Report::new("gateway check");
        for c in rt.block_on(crate::gateway_check::checks(workspace, name, model, false)) {
            r.add(c);
        }
        let code = if r.ok() { 0 } else { EXIT_ERROR };
        println!("{}", r.to_json(code));
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }
    let code = rt.block_on(crate::gateway_check::run(workspace, name, model, false))?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

// -------------------------------------------------------------------- remove

fn remove(data_dir: &Path, p: &Parsed) -> Result<()> {
    let name = p.words.get(1).cloned().ok_or_else(|| usage_error(format!("ligus gateway remove needs a NAME; {USAGE}")))?;
    if p.words.len() > 2 {
        return Err(usage_error(format!("`ligus gateway remove` takes one NAME; {USAGE}")));
    }
    let (path, source) = file_of(data_dir);
    let svc = service_for(data_dir);
    same_file_as_service(svc.as_ref(), &path)?;
    let before = read(&path)?;
    let mut f = before.clone().unwrap_or_default();
    let n = f.gateways.len();
    f.gateways.retain(|g| g.name != name);
    let changed = f.gateways.len() != n;
    if changed {
        write(&path, &f)?;
    }
    let restart = if changed { restart_lines(svc.as_ref(), true) } else { Vec::new() };
    if p.has("--json") {
        let mut d = doc("gateway remove", &path, source, Some(&f));
        d["result"] = json!({ "action": if changed { "removed" } else { "unchanged" }, "changed": changed, "name": name });
        d["restart"] = json!(restart);
        println!("{d}");
        return Ok(());
    }
    if !changed {
        println!("Gateway {name} is not listed; nothing changed ({}).", path.display());
        return Ok(());
    }
    println!("Removed gateway {name} ({}).", path.display());
    if f.gateways.is_empty() {
        println!("No gateways are left, so this box is no longer exclusive: agents run directly again.");
    }
    for l in &restart {
        println!("{l}");
    }
    Ok(())
}

// ------------------------------------------------------------ the service

/// How this data dir's serve or worker runs, when a unit or agent for it
/// is found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    /// What to type to restart it.
    pub restart: String,
    /// Its env file, when it has one.
    pub env_file: Option<PathBuf>,
    /// The gateways file its env file names (LIGUS_MODEL_GATEWAYS_FILE).
    pub gateways_file: Option<PathBuf>,
}

/// Refuse to edit a file the service for this data dir doesn't read: its
/// env file names another one (LIGUS_MODEL_GATEWAYS_FILE), and this shell
/// doesn't have it.
fn same_file_as_service(svc: Option<&Service>, path: &Path) -> Result<()> {
    let (Some(s), Some(theirs)) = (svc, svc.and_then(|s| s.gateways_file.as_ref())) else { return Ok(()) };
    if same_dir(theirs, path) {
        return Ok(());
    }
    let env = s.env_file.as_ref().map(|e| e.display().to_string()).unwrap_or_default();
    Err(problem(
        EXIT_FIXABLE,
        "other_file",
        format!("the ligus serve or worker for this data dir reads {} (LIGUS_MODEL_GATEWAYS_FILE in {env}), not {}", theirs.display(), path.display()),
        format!("load its env file first (set -a; . {}; set +a) and run this again, or remove LIGUS_MODEL_GATEWAYS_FILE from it", shell_word(&env)),
    ))
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let c = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    c(a) == c(b)
}

/// `KEY=value` from an env file (`'…'` or `"…"` quotes removed; last wins).
pub fn env_file_value(text: &str, key: &str) -> Option<String> {
    text.lines()
        .rev()
        .find_map(|l| {
            let l = l.trim_start();
            let l = l.strip_prefix("export ").unwrap_or(l);
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then(|| {
                let v = v.trim();
                v.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')).or_else(|| v.strip_prefix('"').and_then(|v| v.strip_suffix('"'))).unwrap_or(v).to_string()
            })
        })
}

fn expand_home(p: &str, home: &Path) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(p.replace("%h", &home.display().to_string())),
    }
}

/// The service running `data_dir`, looked for under `home`: a systemd user
/// unit (ligus-local's `ligus-serve`/`ligus-worker`, or Homebrew's
/// `homebrew.ligus`), or a LaunchAgent (ligus-local's `ai.ligus.*`, or
/// Homebrew's `homebrew.mxcl.ligus`).
pub fn find_service(home: &Path, data_dir: &Path) -> Option<Service> {
    let config = ligus_core::names::env_opt("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
    find_service_in(home, &config, data_dir)
}

/// [`find_service`] with the config dir (`$XDG_CONFIG_HOME`) given.
pub fn find_service_in(home: &Path, config: &Path, data_dir: &Path) -> Option<Service> {
    let brew_env = config.join("ligus-serve/ligus-serve.env");
    let gw_of = |e: &Path| -> Option<PathBuf> {
        std::fs::read_to_string(e).ok().and_then(|t| env_file_value(&t, mg::GATEWAYS_FILE_ENV)).filter(|v| !v.trim().is_empty()).map(|v| expand_home(v.trim(), home))
    };
    let env_names = |p: &Path| -> Option<PathBuf> { std::fs::read_to_string(p).ok().and_then(|t| env_file_value(&t, "LIGUS_DATA_DIR")).map(|d| expand_home(&d, home)) };
    // systemd user units.
    let sd = config.join("systemd/user");
    for (unit, cmd) in [("ligus-serve", "systemctl --user restart ligus-serve"), ("ligus-worker", "systemctl --user restart ligus-worker"), ("homebrew.ligus", "brew services restart ligus")] {
        let Ok(text) = std::fs::read_to_string(sd.join(format!("{unit}.service"))) else { continue };
        let envs: Vec<PathBuf> = text
            .lines()
            .filter_map(|l| l.trim().strip_prefix("EnvironmentFile="))
            .map(|v| expand_home(v.trim_start_matches('-'), home))
            .chain((unit == "homebrew.ligus").then(|| brew_env.clone()))
            .collect();
        for e in envs {
            if env_names(&e).is_some_and(|d| same_dir(&d, data_dir)) {
                return Some(Service { restart: cmd.into(), gateways_file: gw_of(&e), env_file: Some(e) });
            }
        }
    }
    // LaunchAgents.
    let la = home.join("Library/LaunchAgents");
    let mut plists: Vec<PathBuf> = std::fs::read_dir(&la).into_iter().flatten().flatten().map(|e| e.path()).collect();
    plists.sort();
    for p in plists {
        let Some(label) = p.file_stem().and_then(|s| s.to_str()).map(str::to_string) else { continue };
        if !(label.starts_with("ai.ligus.") || label == "homebrew.mxcl.ligus") || p.extension().is_none_or(|x| x != "plist") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let restart = if label == "homebrew.mxcl.ligus" { "brew services restart ligus".to_string() } else { format!("launchctl kickstart -k gui/$(id -u)/{}", shell_word(&label)) };
        let dir = plist_value(&text, "LIGUS_DATA_DIR").map(|d| expand_home(&d, home));
        if dir.is_some_and(|d| same_dir(&d, data_dir)) {
            let gateways_file = plist_value(&text, mg::GATEWAYS_FILE_ENV).filter(|v| !v.trim().is_empty()).map(|v| expand_home(v.trim(), home));
            return Some(Service { restart, env_file: None, gateways_file });
        }
        if label == "homebrew.mxcl.ligus" && env_names(&brew_env).is_some_and(|d| same_dir(&d, data_dir)) {
            return Some(Service { restart, gateways_file: gw_of(&brew_env), env_file: Some(brew_env.clone()) });
        }
    }
    None
}

/// The `<string>` after `<key>KEY</key>` in a plist, XML-unescaped.
fn plist_value(text: &str, key: &str) -> Option<String> {
    let after = text.split(&format!("<key>{key}</key>")).nth(1)?;
    let v = after.trim_start().strip_prefix("<string>")?.split("</string>").next()?;
    Some(v.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&"))
}

fn service_for(data_dir: &Path) -> Option<Service> {
    find_service(&ligus_runtime::executor::home_dir()?, data_dir)
}

/// What to say after the file changed. `advertised`: the gateway names the
/// host advertises changed, which needs a restart.
fn restart_lines(svc: Option<&Service>, advertised: bool) -> Vec<String> {
    let mut v = vec![match svc {
        Some(_) => "A running ligus serve or worker on this data dir applies the change within a few seconds (turns already running finish on their old route).".to_string(),
        None => "No systemd unit, LaunchAgent or Homebrew service runs this data dir; a ligus serve or worker started on it by hand applies the change within a few seconds. Check this is the box's data dir (LIGUS_DATA_DIR in its env file).".to_string(),
    }];
    if advertised {
        v.push(match svc {
            Some(s) => format!("Restart it so the desktop lists the change: {}", s.restart),
            None => "Restart it so the desktop lists the change: systemctl --user restart ligus-serve (systemd), \
                     launchctl kickstart -k gui/$(id -u)/ai.ligus.serve (launchd), or brew services restart ligus (Homebrew)"
                .to_string(),
        });
    }
    v
}

/// A `LIGUS_MODEL_GATEWAYS_EXCLUSIVE` that wins over the file's choice: in
/// this shell, or in the service's env file.
fn env_override_notice(svc: Option<&Service>, file_exclusive: bool) -> Vec<String> {
    let mut v = Vec::new();
    let differs = |val: &str| mg::exclusive_of(1, Some(file_exclusive), Some(val)).0 != file_exclusive;
    if let Some(val) = exclusive_env().filter(|v| differs(v)) {
        v.push(format!("LIGUS_MODEL_GATEWAYS_EXCLUSIVE={} is set in this shell and wins over the file's choice.", val.trim()));
    }
    if let Some(e) = svc.and_then(|s| s.env_file.as_ref()) {
        if let Some(val) = std::fs::read_to_string(e).ok().and_then(|t| env_file_value(&t, mg::EXCLUSIVE_ENV)).filter(|v| !v.trim().is_empty() && differs(v)) {
            v.push(format!(
                "{} sets LIGUS_MODEL_GATEWAYS_EXCLUSIVE={}, which wins over this choice: remove that line and restart to let the file decide.",
                e.display(),
                val.trim()
            ));
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_auth_with_its_file() {
        let p = Parsed::new(&args(&["add", "gw", "--url", "https://gw", "--api", "openai", "--auth", "bearer-file", "/k/gw.key", "--json"])).unwrap();
        assert_eq!(p.words, args(&["add", "gw"]));
        assert_eq!(p.auth_file.as_deref(), Some("/k/gw.key"));
        assert!(p.has("--json"));
        assert!(Parsed::new(&args(&["add", "gw", "--auth", "bearer-file"])).is_err());
        assert!(Parsed::new(&args(&["add", "gw", "--url"])).is_err());
        assert!(Parsed::new(&args(&["add", "gw", "--frob"])).is_err());
        let p = Parsed::new(&args(&["add", "gw", "--auth", "mtls", "--cert-file", "/c", "--client-key-file", "/k"])).unwrap();
        assert!(matches!(auth_of(&p).unwrap(), AuthConfig::Mtls { .. }));
        let p = Parsed::new(&args(&["add", "gw", "--auth", "none", "--header", "x-api-key"])).unwrap();
        assert!(auth_of(&p).is_err(), "--header without api-key-file");
        let p = Parsed::new(&args(&["add", "gw", "--auth", "api-key-file", "/k", "--header", "X-Api-Key"])).unwrap();
        // The stored path is the real path: "/k" itself on Unix, made absolute (a drive prefix) on Windows.
        assert_eq!(auth_of(&p).unwrap(), AuthConfig::ApiKey { key_file: real_path("/k").unwrap(), header: Some("x-api-key".into()) });
    }

    #[test]
    fn broad_directories_are_refused() {
        assert!(too_broad(Path::new("/")));
        assert!(too_broad(Path::new("/tmp")));
        assert!(too_broad(Path::new("/etc")));
        if let Some(h) = ligus_runtime::executor::home_dir() {
            assert!(too_broad(&h));
            assert!(!too_broad(&h.join(".config/ligus-gateways")));
        }
    }

    #[test]
    fn env_file_values() {
        let t = "# c\nLIGUS_DATA_DIR='/srv/a b'\nexport LIGUS_MODEL_GATEWAYS_EXCLUSIVE=0\nLIGUS_DATA_DIR=\"/srv/c\"\n";
        assert_eq!(env_file_value(t, "LIGUS_DATA_DIR").as_deref(), Some("/srv/c"));
        assert_eq!(env_file_value(t, "LIGUS_MODEL_GATEWAYS_EXCLUSIVE").as_deref(), Some("0"));
        assert_eq!(env_file_value(t, "NOPE"), None);
    }

    /// The restart command follows how this data dir's serve runs.
    #[test]
    fn finds_the_service_for_a_data_dir() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let data = h.join(".ligus-serve");
        std::fs::create_dir_all(&data).unwrap();
        assert_eq!(find_service_in(h, &h.join(".config"), &data), None);
        // ligus-local's systemd unit, its env file naming this data dir.
        std::fs::create_dir_all(h.join(".config/systemd/user")).unwrap();
        std::fs::create_dir_all(h.join(".config/ligus-serve")).unwrap();
        let env = h.join(".config/ligus-serve/ligus-serve.env");
        std::fs::write(&env, format!("LIGUS_DATA_DIR='{}'\nLIGUS_MODEL_GATEWAYS_EXCLUSIVE=0\n", data.display())).unwrap();
        std::fs::write(h.join(".config/systemd/user/ligus-serve.service"), format!("[Service]\nEnvironmentFile={}\n", env.display())).unwrap();
        let s = find_service_in(h, &h.join(".config"), &data).unwrap();
        assert_eq!(s.restart, "systemctl --user restart ligus-serve");
        assert_eq!(s.env_file.as_deref(), Some(env.as_path()));
        // The env file's LIGUS_MODEL_GATEWAYS_EXCLUSIVE=0 wins over an exclusive file: said.
        assert!(env_override_notice(Some(&s), true).iter().any(|n| n.contains("wins over this choice")));
        assert!(env_override_notice(Some(&s), false).is_empty(), "no notice when they agree");
        // Another data dir: not this one.
        assert_eq!(find_service_in(h, &h.join(".config"), &h.join("other")), None);
        // A LaunchAgent ligus-local wrote.
        std::fs::remove_file(h.join(".config/systemd/user/ligus-serve.service")).unwrap();
        std::fs::create_dir_all(h.join("Library/LaunchAgents")).unwrap();
        std::fs::write(
            h.join("Library/LaunchAgents/ai.ligus.serve.plist"),
            format!("<dict>\n\t\t<key>LIGUS_DATA_DIR</key>\n\t\t<string>{}</string>\n</dict>", data.display()),
        )
        .unwrap();
        assert_eq!(find_service_in(h, &h.join(".config"), &data).unwrap().restart, "launchctl kickstart -k gui/$(id -u)/ai.ligus.serve");
    }

    /// A service whose env file names another gateways file: editing this
    /// one would change nothing it reads.
    #[test]
    fn refuses_a_file_the_service_does_not_read() {
        let s = |g: Option<&str>| Service { restart: String::new(), env_file: Some("/e/serve.env".into()), gateways_file: g.map(PathBuf::from) };
        assert!(same_file_as_service(None, Path::new("/d/model-gateways.json")).is_ok());
        assert!(same_file_as_service(Some(&s(None)), Path::new("/d/model-gateways.json")).is_ok());
        assert!(same_file_as_service(Some(&s(Some("/d/model-gateways.json"))), Path::new("/d/model-gateways.json")).is_ok());
        let e = same_file_as_service(Some(&s(Some("/etc/ligus/gw.json"))), Path::new("/d/model-gateways.json")).unwrap_err();
        assert!(e.to_string().contains("/etc/ligus/gw.json"), "{e}");
    }

    #[test]
    fn restart_lines_say_what_applies() {
        let l = restart_lines(None, false);
        assert_eq!(l.len(), 1, "a mode change applies without a restart");
        let l = restart_lines(None, true);
        assert!(l[1].contains("systemctl --user restart ligus-serve") && l[1].contains("launchctl kickstart") && l[1].contains("brew services restart ligus"));
        let s = Service { restart: "brew services restart ligus".into(), env_file: None, gateways_file: None };
        assert_eq!(restart_lines(Some(&s), true)[1], "Restart it so the desktop lists the change: brew services restart ligus");
    }

    /// The command add suggests writes the same entry (so it only flips the mode).
    #[test]
    fn suggested_command_round_trips() {
        let cfg = GatewayConfig {
            name: "gw".into(),
            url: "http://10.0.0.2:8080/v1".into(),
            api: Api::Anthropic,
            allow_plaintext: true,
            attribution: true,
            // A real (absolute) path, as `add` stores it, on every platform.
            auth: AuthConfig::ApiKey { key_file: real_path("/k/a b.key").unwrap(), header: Some("x-api-key".into()) },
            ca_file: Some("/k/ca.pem".into()),
        };
        let line = add_command(&cfg, "--not-exclusive");
        let key = real_path("/k/a b.key").unwrap();
        assert!(line.contains(&format!("'{}'", key.display())), "{line}");
        // Split it as a shell would (single quotes only, as shell_word writes).
        let mut words = Vec::new();
        let (mut cur, mut q, mut any) = (String::new(), false, false);
        for ch in line.chars() {
            match ch {
                '\'' => {
                    q = !q;
                    any = true;
                }
                ' ' if !q => {
                    if any || !cur.is_empty() {
                        words.push(std::mem::take(&mut cur));
                    }
                    any = false;
                }
                c => cur.push(c),
            }
        }
        words.push(cur);
        let p = Parsed::new(&words[2..]).unwrap();
        assert!(p.has("--not-exclusive"));
        let back = GatewayConfig {
            name: p.words[1].clone(),
            url: p.flag_val("--url").unwrap(),
            api: Api::Anthropic,
            allow_plaintext: p.has("--allow-plaintext"),
            attribution: p.has("--attribution"),
            auth: auth_of(&p).unwrap(),
            ca_file: p.flag_val("--ca-file").map(PathBuf::from),
        };
        assert_eq!(back, cfg);
    }

    #[test]
    fn shell_words() {
        assert_eq!(shell_word("http://10.0.0.2:8080/v1"), "http://10.0.0.2:8080/v1");
        assert_eq!(shell_word("a b"), "'a b'");
    }
}

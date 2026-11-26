//! `ligus hook …`: Claude Code's permission prompts, from sessions Ligus
//! didn't start, as grant cards on the person's own devices
//! (`ligus_runtime::hook`; design `docs/design/agent-questions.md` 8.2).
//!
//! Runs before the data dir, the config or the secret store are touched:
//! the hook never opens a data dir (AGENTS.md rule 4). It reads only the
//! hook token file of a running Ligus process and talks to its socket.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use ligus_runtime::hook;
use serde_json::{json, Value};

/// The data dirs a running Ligus process may hold, in order: the one named
/// (`--data-dir`, `LIGUS_DATA_DIR`), else the desktop app's, then the CLI's
/// default (`ligus serve`).
pub fn candidates(explicit: Option<&str>) -> Vec<PathBuf> {
    if let Some(d) = explicit.filter(|d| !d.trim().is_empty()) {
        return vec![PathBuf::from(d)];
    }
    if let Some(d) = std::env::var_os("LIGUS_DATA_DIR").filter(|d| !d.is_empty()) {
        return vec![PathBuf::from(d)];
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if cfg!(target_os = "macos") {
        out.push(home.join("Library/Application Support/ai.ligus.desktop"));
    } else {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        out.push(data.join("ai.ligus.desktop"));
    }
    out.push(ligus_core::names::home_data_dir(&home));
    out
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

/// `ligus hook <sub> …` (the words after `hook`).
pub fn cmd(args: &[String]) -> Result<()> {
    let rest = &args[args.len().min(1)..];
    match args.first().map(String::as_str) {
        Some("permission") => {
            // Never fails the hook: Claude Code reads a non-zero exit as an
            // error, and no output as no decision.
            if let Err(e) = permission(rest) {
                eprintln!("ligus hook: {e}");
            }
            Ok(())
        }
        Some("token") => token(rest),
        Some("install") => install(rest),
        None => Err(crate::help::usage_error(
            "usage: ligus hook permission | token | install (see `ligus hook --help`)",
        )),
        Some(other) => Err(crate::help::usage_error(format!(
            "unknown `ligus hook {other}`: see `ligus hook --help`"
        ))),
    }
}

/// Read Claude Code's input, ask a running Ligus, print its decision.
fn permission(args: &[String]) -> Result<()> {
    let workspace = flag(args, "--workspace").ok_or_else(|| {
        anyhow!("--workspace is required (a workspace of your own: see `ligus help hook`)")
    })?;
    let chat = flag(args, "--chat").ok_or_else(|| anyhow!("--chat is required"))?;
    let timeout = flag(args, "--timeout")
        .map(|t| t.parse::<u64>())
        .transpose()
        .map_err(|_| anyhow!("--timeout is seconds"))?
        .unwrap_or(hook::DEFAULT_TIMEOUT_SECS);
    let timeout = timeout.clamp(hook::MIN_TIMEOUT_SECS, hook::MAX_TIMEOUT_SECS);
    let mut raw = Vec::new();
    std::io::stdin()
        .take(hook::MAX_REQUEST_BYTES as u64)
        .read_to_end(&mut raw)?;
    let input: Value = serde_json::from_slice(&raw)
        .map_err(|_| anyhow!("stdin isn't Claude Code's hook input"))?;
    if input
        .get("hook_event_name")
        .and_then(Value::as_str)
        .is_some_and(|e| e != "PermissionRequest")
    {
        bail!("this hook answers PermissionRequest only");
    }
    let out = decide(
        &candidates(flag(args, "--data-dir").as_deref()),
        &workspace,
        &chat,
        timeout,
        input,
    );
    if let Some(o) = out {
        println!("{o}");
    }
    Ok(())
}

/// Ask the first running process among `dirs`; the stdout line to print
/// (`None`: print nothing).
pub fn decide(
    dirs: &[PathBuf],
    workspace: &str,
    chat: &str,
    timeout: u64,
    input: Value,
) -> Option<String> {
    for dir in dirs {
        let socket = hook::socket_path(dir);
        if !socket.exists() {
            continue;
        }
        let Some(token) = hook::read_token(&hook::token_path(dir)) else {
            continue;
        };
        let req = hook::HookRequest {
            token,
            workspace: workspace.into(),
            chat: chat.into(),
            timeout_secs: timeout,
            input: input.clone(),
        };
        let Some(reply) = hook::ask(&socket, &req, Duration::from_secs(timeout)) else {
            continue;
        };
        if let Some(w) = &reply.warning {
            eprintln!("ligus hook: {w}");
        }
        if let Some(r) = reply.reason.as_ref().filter(|_| reply.decision == "none") {
            eprintln!("ligus hook: no decision ({r}); Claude Code decides");
        }
        return hook::claude_output(Some(&reply));
    }
    eprintln!("ligus hook: no running Ligus (the desktop) to ask; Claude Code decides");
    None
}

/// `ligus hook token [--data-dir DIR] [--json]`: where each install's hook
/// token is, and whether its socket listens. Never its value, and never
/// creates anything: the running desktop (or box) makes its own token when
/// it starts listening, so this CLI writes nothing into a data dir it
/// doesn't hold.
fn token(args: &[String]) -> Result<()> {
    let dirs = candidates(flag(args, "--data-dir").as_deref());
    let mut rows = Vec::new();
    for d in dirs.iter().filter(|d| d.is_dir()) {
        let p = hook::token_path(d);
        rows.push(json!({"dataDir": d, "token": p, "present": hook::read_token(&p).is_some(), "listening": hook::socket_path(d).exists()}));
    }
    if rows.is_empty() {
        return Err(anyhow::Error::new(crate::problem::Problem {
            exit: crate::problem::EXIT_FIXABLE,
            code: "no_data_dir",
            message: "no Ligus data dir here".into(),
            fix: Some("start the desktop app once, or pass --data-dir".into()),
        }));
    }
    if has(args, "--json") {
        println!(
            "{}",
            json!({"schema": "ligus.hook-token.v1", "installs": rows})
        );
    } else {
        for r in rows {
            let state = match (r["present"] == true, r["listening"] == true) {
                (true, true) => "listening",
                (true, false) => "not listening",
                (false, _) => "not created yet: the desktop makes it when it starts",
            };
            println!("{}  ({state})", r["token"].as_str().unwrap_or(""));
        }
    }
    Ok(())
}

/// The command `install` writes for `--workspace W --chat C`.
pub fn hook_command(workspace: &str, chat: &str) -> String {
    format!("ligus hook permission --workspace {workspace} --chat {chat}")
}

/// `settings` with the Ligus permission hook in it (an existing one
/// replaced, everything else kept). `None` when it is already there.
pub fn merged(settings: &Value, workspace: &str, chat: &str) -> Result<Option<Value>> {
    let mut s = if settings.is_null() {
        json!({})
    } else {
        settings.clone()
    };
    let obj = s
        .as_object_mut()
        .ok_or_else(|| anyhow!("the settings file isn't a JSON object"))?;
    let entry = json!({"matcher": "*", "hooks": [{"type": "command", "command": hook_command(workspace, chat), "timeout": 120}]});
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| anyhow!("\"hooks\" isn't an object"))?;
    let list = hooks
        .entry("PermissionRequest")
        .or_insert_with(|| json!([]));
    let list = list
        .as_array_mut()
        .ok_or_else(|| anyhow!("\"hooks.PermissionRequest\" isn't a list"))?;
    let ours = |g: &Value| {
        g["hooks"].as_array().is_some_and(|h| {
            h.iter().any(|x| {
                x["command"]
                    .as_str()
                    .is_some_and(|c| c.starts_with("ligus hook permission"))
            })
        })
    };
    match list.iter().position(ours) {
        Some(i) if list[i] == entry => return Ok(None),
        Some(i) => list[i] = entry,
        None => list.push(entry),
    }
    Ok(Some(s))
}

fn line_diff(old: &str, new: &str) -> String {
    let (a, b): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    let mut out = String::new();
    for l in &a {
        if !b.contains(l) {
            out.push_str(&format!("-{l}\n"));
        }
    }
    for l in &b {
        if !a.contains(l) {
            out.push_str(&format!("+{l}\n"));
        }
    }
    out
}

/// A yes from the person at the terminal (`/dev/tty`); `false` without one.
pub fn confirm_tty(question: &str) -> bool {
    use std::io::{BufRead, Write};
    let Ok(mut tty) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
    else {
        return false;
    };
    if write!(tty, "{question} [y/N] ").is_err() || tty.flush().is_err() {
        return false;
    }
    let mut line = String::new();
    if std::io::BufReader::new(&tty).read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim(), "y" | "Y" | "yes" | "Yes")
}

fn is_user_settings(p: &Path) -> bool {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return false;
    };
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(p) == canon(&home.join(".claude/settings.json"))
}

/// `ligus hook install --settings PATH --workspace W --chat C [--dry-run]`.
fn install(args: &[String]) -> Result<()> {
    let path = PathBuf::from(flag(args, "--settings").ok_or_else(|| {
        crate::help::usage_error("usage: ligus hook install --settings PATH --workspace W --chat C")
    })?);
    let workspace = flag(args, "--workspace")
        .ok_or_else(|| crate::help::usage_error("--workspace is required"))?;
    let chat =
        flag(args, "--chat").ok_or_else(|| crate::help::usage_error("--chat is required"))?;
    for (n, v) in [("--workspace", &workspace), ("--chat", &chat)] {
        if uuid::Uuid::parse_str(v.trim()).is_err() {
            return Err(crate::help::usage_error(format!(
                "{n} is an id (see `ligus chats --json`)"
            )));
        }
    }
    let before = match std::fs::read_to_string(&path) {
        Ok(t) => serde_json::from_str::<Value>(&t)
            .map_err(|e| anyhow!("{} isn't JSON: {e}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Null,
        Err(e) => return Err(e.into()),
    };
    let Some(after) = merged(&before, &workspace, &chat)? else {
        println!("already installed in {}", path.display());
        return Ok(());
    };
    let old = if before.is_null() {
        String::new()
    } else {
        serde_json::to_string_pretty(&before)?
    };
    let new = serde_json::to_string_pretty(&after)?;
    print!("{}", line_diff(&old, &new));
    if has(args, "--dry-run") {
        println!("(dry run: {} not changed)", path.display());
        return Ok(());
    }
    // Your own settings file reaches every Claude Code session you run: a
    // person says yes at the terminal.
    if is_user_settings(&path)
        && !confirm_tty(&format!(
            "Send every Claude Code permission prompt to Ligus chat {chat}?"
        ))
    {
        return Err(anyhow::Error::new(crate::problem::Problem {
            exit: crate::problem::EXIT_HUMAN,
            code: "needs_person",
            message: "installing the hook in your own Claude Code settings needs your yes at the terminal".into(),
            fix: Some(format!("run `ligus hook install --settings {} …` yourself in a terminal", path.display())),
        }));
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, format!("{new}\n"))?;
    println!("installed in {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_merges_and_is_idempotent() {
        let w = "11111111-1111-4111-8111-111111111111";
        let c = "22222222-2222-4222-8222-222222222222";
        let existing = json!({"model": "opus", "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": []}], "PermissionRequest": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "./mine.sh"}]}]}});
        let m = merged(&existing, w, c).unwrap().unwrap();
        assert_eq!(m["model"], "opus");
        assert_eq!(m["hooks"]["PreToolUse"], existing["hooks"]["PreToolUse"]);
        let pr = m["hooks"]["PermissionRequest"].as_array().unwrap();
        assert_eq!(pr.len(), 2, "theirs kept, ours added");
        assert_eq!(
            pr[1],
            json!({"matcher": "*", "hooks": [{"type": "command", "command": format!("ligus hook permission --workspace {w} --chat {c}"), "timeout": 120}]})
        );
        assert!(merged(&m, w, c).unwrap().is_none(), "idempotent");
        let moved = merged(&m, w, "33333333-3333-4333-8333-333333333333")
            .unwrap()
            .unwrap();
        assert_eq!(
            moved["hooks"]["PermissionRequest"]
                .as_array()
                .unwrap()
                .len(),
            2,
            "replaced, not added twice"
        );
        assert!(merged(&json!([1]), w, c).is_err());
        assert_eq!(
            merged(&Value::Null, w, c).unwrap().unwrap()["hooks"]["PermissionRequest"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    /// No running process: nothing on stdout (Claude Code's own dialog, or
    /// a deny in `-p`).
    #[test]
    fn no_running_ligus_prints_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            decide(
                &[dir.path().to_path_buf()],
                "w",
                "c",
                10,
                json!({"tool_name": "Bash"})
            ),
            None
        );
    }

    #[test]
    fn an_explicit_data_dir_is_the_only_candidate() {
        assert_eq!(candidates(Some("/tmp/x")), vec![PathBuf::from("/tmp/x")]);
    }
}

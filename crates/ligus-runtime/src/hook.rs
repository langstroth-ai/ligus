//! `ligus hook permission` (`docs/design/agent-questions.md` 8.2): Claude
//! Code's permission prompts from sessions Ligus didn't start, put to the
//! person as a grant card on their own devices.
//!
//! A running Ligus process of this user (the desktop) listens on an
//! owner-only unix socket in its data dir, `hook/hook.sock`. The hook
//! connects, presents the per-install token (`hook/hook.token`, 0600), and
//! sends one request: the workspace, the chat and Claude Code's
//! `PermissionRequest` input. The process posts the card (signed by its
//! device as its person, put to that person alone, passkey always
//! required), waits, and answers `allow`, `deny` or `none`. The hook then
//! prints Claude Code's decision, or nothing (Claude Code's own dialog, or
//! a deny in `-p`).
//!
//! The hook never opens a data dir: it reads only the token file and talks
//! to the socket. It never auto-allows, and never prints `updatedInput` or
//! `updatedPermissions`: Ligus adds no standing rule.
//!
//! Anything running as this user can read the token, so it can raise
//! prompts to the person (risk A8). It can't grant them: the card needs the
//! person's passkey, and the CLI refuses to answer outside prompts.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::chat_service::OutsidePrompt;
use crate::ligus_mcp::LigusHost;

/// The hook's directory in a data dir.
pub const HOOK_DIR: &str = "hook";
pub const SOCKET_FILE: &str = "hook.sock";
pub const TOKEN_FILE: &str = "hook.token";
/// The most detail a card shows before it offers no Allow.
pub const MAX_DETAIL_BYTES: usize = 8 * 1024;
/// Hook asks per minute per install, and open at once.
pub const MAX_PER_MINUTE: usize = 10;
pub const MAX_OPEN: usize = 3;
/// The hook's own wait (default), and the most it waits: always 10 s under
/// Claude Code's default hook timeout (600 s).
pub const DEFAULT_TIMEOUT_SECS: u64 = 90;
pub const MAX_TIMEOUT_SECS: u64 = 590;
pub const MIN_TIMEOUT_SECS: u64 = 10;
/// The longest request line (the hook's whole input).
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;

pub fn hook_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(HOOK_DIR)
}

pub fn socket_path(data_dir: &Path) -> PathBuf {
    hook_dir(data_dir).join(SOCKET_FILE)
}

pub fn token_path(data_dir: &Path) -> PathBuf {
    hook_dir(data_dir).join(TOKEN_FILE)
}

fn private_dir(dir: &Path) -> std::io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        b.mode(0o700);
        b.create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    b.create(dir)?;
    Ok(())
}

/// The install's hook token: created (0600, 32 random bytes) when missing.
/// Returns its path; the value is never printed.
pub fn ensure_token(data_dir: &Path) -> std::io::Result<PathBuf> {
    let dir = hook_dir(data_dir);
    private_dir(&dir)?;
    let path = token_path(data_dir);
    if read_token(&path).is_none() {
        let mut b = [0u8; 32];
        getrandom::fill(&mut b).map_err(|e| std::io::Error::other(e.to_string()))?;
        let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
        crate::private_file::write_private_atomic(&path, hex.as_bytes())?;
    }
    Ok(path)
}

/// The token at `path`, when it is a well-formed one.
pub fn read_token(path: &Path) -> Option<String> {
    let t = std::fs::read_to_string(path).ok()?;
    let t = t.trim();
    (t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit())).then(|| t.to_string())
}

fn same_token(a: &str, b: &str) -> bool {
    // Constant time over the shorter's length; lengths are public.
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// What the hook sends.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HookRequest {
    pub token: String,
    pub workspace: String,
    pub chat: String,
    pub timeout_secs: u64,
    /// Claude Code's `PermissionRequest` input, as it arrived.
    pub input: Value,
}

/// What the process answers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct HookReply {
    /// `allow`, `deny` or `none` (no decision: Claude Code decides).
    pub decision: String,
    /// Who decided (a roster name), for a deny's message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// Why there is no decision (for the hook's stderr).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// A warning for the hook's stderr (others can read the chat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

impl HookReply {
    fn none(reason: impl Into<String>) -> Self {
        Self { decision: "none".into(), reason: Some(reason.into()), ..Default::default() }
    }
}

/// What the hook prints on stdout for `reply` (Claude Code hooks reference,
/// `PermissionRequest` decision control): `None` prints nothing.
pub fn claude_output(reply: Option<&HookReply>) -> Option<String> {
    let r = reply?;
    let decision = match r.decision.as_str() {
        "allow" => serde_json::json!({"behavior": "allow"}),
        "deny" => {
            let by = r.by.as_deref().map(ligus_proto::names::clean_display_name).filter(|n| !n.is_empty()).unwrap_or_else(|| "you".into());
            serde_json::json!({"behavior": "deny", "message": format!("Denied from Ligus by {by}"), "interrupt": false})
        }
        _ => return None,
    };
    Some(serde_json::json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": decision}}).to_string())
}

fn cut_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut i = max;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    format!("{}…", &s[..i])
}

/// The card's detail for Claude Code's `PermissionRequest` input: what the
/// allow covers, never cut silently. Bash: the exact command. Write, Edit,
/// MultiEdit, NotebookEdit: the path and the full new content (or the old
/// and new strings). Others: the input as pretty JSON. Over
/// [`MAX_DETAIL_BYTES`] the card offers no Allow. `permission_suggestions`
/// and `mcp_server` follow under "Details", shown and never applied.
pub fn outside_detail(input: &Value) -> OutsidePrompt {
    let tool = input.get("tool_name").and_then(Value::as_str).unwrap_or("a tool").to_string();
    let ti = input.get("tool_input").cloned().unwrap_or(Value::Null);
    let s = |k: &str| ti.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    // Every key the card doesn't already show, as JSON: Allow once covers the
    // whole call, so nothing in it goes unseen.
    let rest_of = |shown: &[&str]| -> String {
        let rest: serde_json::Map<String, Value> = ti.as_object().map(|o| o.iter().filter(|(k, _)| !shown.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
        if rest.is_empty() { String::new() } else { format!("\n\n{}", serde_json::to_string_pretty(&Value::Object(rest)).unwrap_or_default()) }
    };
    // Something with no content to show (an empty or non-string command, no
    // path) offers no Allow: nothing to show is nothing to allow.
    let mut empty = false;
    let main = match tool.as_str() {
        "Bash" => {
            let cmd = ti.get("command").and_then(Value::as_str).unwrap_or("");
            empty = cmd.trim().is_empty();
            let mut d = String::new();
            if ti.get("dangerouslyDisableSandbox").and_then(Value::as_bool) == Some(true) {
                d.push_str("Runs outside Claude Code's sandbox\n\n");
            }
            d.push_str(cmd);
            d.push_str(&rest_of(&["command"]));
            d
        }
        "Write" => {
            empty = s("file_path").is_empty();
            format!("{}\n\n{}{}", s("file_path"), s("content"), rest_of(&["file_path", "content"]))
        }
        "Edit" => {
            empty = s("file_path").is_empty();
            format!("{}\n--- old\n{}\n+++ new\n{}{}", s("file_path"), s("old_string"), s("new_string"), rest_of(&["file_path", "old_string", "new_string"]))
        }
        "MultiEdit" => {
            empty = s("file_path").is_empty();
            let mut d = s("file_path");
            for e in ti.get("edits").and_then(Value::as_array).into_iter().flatten() {
                let g = |k: &str| e.get(k).and_then(Value::as_str).unwrap_or("");
                d.push_str(&format!("\n--- old\n{}\n+++ new\n{}", g("old_string"), g("new_string")));
            }
            d.push_str(&rest_of(&["file_path", "edits"]));
            d
        }
        "NotebookEdit" => {
            empty = s("notebook_path").is_empty();
            format!("{}\n\n{}{}", s("notebook_path"), s("new_source"), rest_of(&["notebook_path", "new_source"]))
        }
        _ => {
            empty = ti.as_object().is_none_or(|o| o.is_empty()) && !ti.is_string();
            serde_json::to_string_pretty(&ti).unwrap_or_default()
        }
    };
    let too_large = main.len() > MAX_DETAIL_BYTES || main.trim().is_empty() || empty;
    let mut detail = cut_bytes(&main, MAX_DETAIL_BYTES);
    let mut extra = String::new();
    if let Some(sug) = input.get("permission_suggestions").filter(|v| !v.is_null()) {
        extra.push_str(&format!("\nSuggested by Claude Code (never applied by Ligus): {}", serde_json::to_string(sug).unwrap_or_default()));
    }
    if let Some(m) = input.get("mcp_server").filter(|v| !v.is_null()) {
        extra.push_str(&format!("\nMCP server: {}", serde_json::to_string(m).unwrap_or_default()));
    }
    if !extra.is_empty() {
        detail.push_str("\n\nDetails");
        detail.push_str(&cut_bytes(&extra, 2048));
    }
    let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or("").to_string();
    let label = Path::new(&cwd).file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| "a folder".into());
    // Claude Code names the sub-agent a prompt came from (`agent_type`,
    // with `agent_id`; checked against Claude Code 2.1.332).
    let subagent = input.get("agent_type").and_then(Value::as_str).unwrap_or("").to_string();
    OutsidePrompt { tool, detail, cwd, label, too_large, subagent }
}

/// The install's limits: asks per minute and open at once.
#[derive(Default)]
pub struct Limits {
    recent: Mutex<VecDeque<Instant>>,
    open: AtomicUsize,
}

impl Limits {
    /// Take a slot, or say why not.
    fn take(&self) -> Result<OpenSlot<'_>, &'static str> {
        let now = Instant::now();
        let mut r = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        while r.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60)) {
            r.pop_front();
        }
        if r.len() >= MAX_PER_MINUTE {
            return Err("more than 10 prompts this minute");
        }
        if self.open.load(Ordering::SeqCst) >= MAX_OPEN {
            return Err("3 prompts are already waiting");
        }
        r.push_back(now);
        self.open.fetch_add(1, Ordering::SeqCst);
        Ok(OpenSlot(self))
    }
}

struct OpenSlot<'a>(&'a Limits);

impl Drop for OpenSlot<'_> {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A listening hook socket.
pub struct HookServer {
    pub path: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for HookServer {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// How a process answers hook prompts.
#[derive(Clone)]
pub enum Answerer {
    /// Post cards through this host's service (a person's device).
    Host(Arc<dyn LigusHost>),
    /// Never post: answer `none` with this reason (a `ligus serve` box is
    /// its own member, so its card couldn't be a grant).
    Refuse(String),
}

/// Listen on `data_dir`'s hook socket (0600, in a 0700 dir), with the
/// install's token. Must run inside a tokio runtime.
#[cfg(unix)]
pub fn listen(data_dir: &Path, answerer: Answerer) -> std::io::Result<HookServer> {
    let token_file = ensure_token(data_dir)?;
    let token = read_token(&token_file).ok_or_else(|| std::io::Error::other("hook token unreadable"))?;
    let path = socket_path(data_dir);
    // A socket a crashed run left behind.
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(&path).is_ok() {
            return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, "another Ligus process holds this data dir's hook socket"));
        }
        let _ = std::fs::remove_file(&path);
    }
    let listener = tokio::net::UnixListener::bind(&path)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    let limits = Arc::new(Limits::default());
    let warned = Arc::new(Mutex::new(std::collections::HashSet::<Uuid>::new()));
    let me = unsafe { libc::geteuid() };
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { break };
            // Only this user: the socket's mode already says so; the peer's
            // uid says it again.
            if stream.peer_cred().map(|c| c.uid()).ok() != Some(me) {
                continue;
            }
            let (token, answerer, limits, warned) = (token.clone(), answerer.clone(), limits.clone(), warned.clone());
            tokio::spawn(async move {
                let _ = serve_conn(stream, &token, answerer, &limits, &warned).await;
            });
        }
    });
    Ok(HookServer { path, task })
}

#[cfg(not(unix))]
pub fn listen(_data_dir: &Path, _answerer: Answerer) -> std::io::Result<HookServer> {
    Err(std::io::Error::other("the hook socket needs unix sockets"))
}

#[cfg(unix)]
async fn serve_conn(stream: tokio::net::UnixStream, token: &str, answerer: Answerer, limits: &Limits, warned: &Mutex<std::collections::HashSet<Uuid>>) -> std::io::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r);
    let mut line = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(10), (&mut reader).take(MAX_REQUEST_BYTES as u64).read_until(b'\n', &mut line)).await;
    let reply = match read {
        Ok(Ok(n)) if n > 0 => match serde_json::from_slice::<HookRequest>(&line) {
            Ok(req) if same_token(&req.token, token) => {
                let mut closed = Box::pin(async move {
                    // The hook went away (killed, timed out): stop waiting.
                    let mut buf = [0u8; 64];
                    loop {
                        match reader.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(_) => {}
                        }
                    }
                });
                handle(req, &answerer, limits, warned, &mut closed).await
            }
            Ok(_) => HookReply::none("wrong hook token"),
            Err(_) => HookReply::none("malformed request"),
        },
        _ => HookReply::none("no request"),
    };
    let mut out = serde_json::to_vec(&reply).unwrap_or_default();
    out.push(b'\n');
    let _ = w.write_all(&out).await;
    let _ = w.shutdown().await;
    Ok(())
}

/// Post the card, wait for an answer that counts, close it.
async fn handle(
    req: HookRequest,
    answerer: &Answerer,
    limits: &Limits,
    warned: &Mutex<std::collections::HashSet<Uuid>>,
    closed: &mut (impl std::future::Future<Output = ()> + Unpin),
) -> HookReply {
    let host = match answerer {
        Answerer::Refuse(why) => return HookReply::none(why.clone()),
        Answerer::Host(h) => h.clone(),
    };
    let (Ok(ws), Ok(chat)) = (Uuid::parse_str(req.workspace.trim()), Uuid::parse_str(req.chat.trim())) else {
        return HookReply::none("--workspace and --chat are ids (see `ligus chats --json`)");
    };
    let _slot = match limits.take() {
        Ok(s) => s,
        Err(why) => return HookReply::none(why),
    };
    let timeout = Duration::from_secs(req.timeout_secs.clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS));
    let prompt = outside_detail(&req.input);
    let mut posted: Result<(ligus_core::flow::HumanAsk, String), String> = Err("no service".into());
    let mut others = 0usize;
    host.with_service(&mut |svc| {
        posted = svc.post_outside_ask(ws, chat, &prompt, timeout).map_err(|e| e.to_string());
        others = svc.others_in_chat(ws, chat).unwrap_or(0);
    });
    let (ask, digest) = match posted {
        Ok(p) => p,
        Err(e) => return HookReply::none(e),
    };
    host.ask_posted(chat, &ask);
    let warning = (others > 0 && warned.lock().unwrap_or_else(|e| e.into_inner()).insert(chat))
        .then(|| format!("{others} other member(s) can read this chat, and so this prompt. Hook prompts belong in a workspace of your own (Personal)."));
    let deadline = Instant::now() + timeout;
    let mut outcome: Option<(ligus_core::flow::AskState, bool)> = None;
    loop {
        let mut seen = None;
        host.with_service(&mut |svc| {
            let device = svc.device_id();
            let Some(st) = svc.load_in(ws, chat).ok().flatten().and_then(|s| s.flow.asks.get(&ask.ask_id).cloned()) else { return };
            let ours = st.host == Some(device) && st.digest == digest && st.ask.agent_key == ask.agent_key;
            seen = Some((st, ours));
        });
        if let Some((st, ours)) = &seen {
            if !ours {
                return HookReply { warning, ..HookReply::none("the prompt's id is held by another record") };
            }
            if st.answer.is_some() || st.declined || st.ask.closed {
                outcome = seen;
                break;
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
            _ = &mut *closed => break,
        }
    }
    let mut reply = HookReply::none("no answer");
    let mut settled_by = None;
    if let Some((st, _)) = outcome {
        if let (Some(a), Some(v)) = (st.answer.as_ref(), st.answered_by.last()) {
            // The fold counted it: the person, their device, their passkey,
            // this card's digest. Allow only option 0, never a card too
            // large to show.
            settled_by = v.event_id;
            let name = host_name(&*host, ws, chat, &v.member_id).unwrap_or_else(|| v.name.clone());
            reply = match a.option_index {
                Some(0) if !prompt.too_large && st.grant().is_some() => HookReply { decision: "allow".into(), by: Some(name), ..Default::default() },
                Some(_) => HookReply { decision: "deny".into(), by: Some(name), ..Default::default() },
                None => HookReply::none("no option chosen"),
            };
        } else if st.declined {
            reply = HookReply::none("not now: answer at the terminal");
        }
    }
    host.with_service(&mut |svc| {
        if let Err(e) = svc.close_outside_ask(ws, chat, &ask, settled_by) {
            tracing::warn!(target: "hook", "closing a hook prompt: {e}");
        }
    });
    HookReply { warning, ..reply }
}

fn host_name(host: &dyn LigusHost, ws: Uuid, chat: Uuid, member: &str) -> Option<String> {
    let mut out = None;
    host.with_service(&mut |svc| {
        out = svc.load_in(ws, chat).ok().flatten().and_then(|s| s.members.iter().find(|m| m.id == member).map(|m| m.actor.display_name.clone()));
    });
    out
}

/// The hook's side: send `req` to the socket at `socket` and wait up to
/// `timeout` (plus a little) for the reply. `None` when nothing answers.
#[cfg(unix)]
pub fn ask(socket: &Path, req: &HookRequest, timeout: Duration) -> Option<HookReply> {
    use std::io::{BufRead, Write};
    let mut s = std::os::unix::net::UnixStream::connect(socket).ok()?;
    s.set_read_timeout(Some(timeout + Duration::from_secs(5))).ok()?;
    let mut line = serde_json::to_vec(req).ok()?;
    line.push(b'\n');
    s.write_all(&line).ok()?;
    let mut r = std::io::BufReader::new(s);
    let mut out = String::new();
    r.read_line(&mut out).ok()?;
    serde_json::from_str(out.trim()).ok()
}

#[cfg(not(unix))]
pub fn ask(_socket: &Path, _req: &HookRequest, _timeout: Duration) -> Option<HookReply> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Q-T9: the documented output shapes, and never a standing rule.
    #[test]
    fn output_matches_claude_codes_decision_control() {
        let allow = claude_output(Some(&HookReply { decision: "allow".into(), by: Some("Ana".into()), ..Default::default() })).unwrap();
        assert_eq!(allow, r#"{"hookSpecificOutput":{"decision":{"behavior":"allow"},"hookEventName":"PermissionRequest"}}"#);
        let deny = claude_output(Some(&HookReply { decision: "deny".into(), by: Some("Ana\u{202E}".into()), ..Default::default() })).unwrap();
        let v: Value = serde_json::from_str(&deny).unwrap();
        assert_eq!(v, json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {"behavior": "deny", "message": "Denied from Ligus by Ana", "interrupt": false}}}));
        for out in [&allow, &deny] {
            assert!(!out.contains("updatedInput") && !out.contains("updatedPermissions"));
        }
        assert_eq!(claude_output(Some(&HookReply::none("timeout"))), None, "no decision prints nothing");
        assert_eq!(claude_output(None), None, "no running process prints nothing");
    }

    #[test]
    fn detail_is_the_calls_own_content() {
        let d = outside_detail(&json!({"tool_name": "Bash", "tool_input": {"command": "rm -rf node_modules"}, "cwd": "/home/ana/shop"}));
        assert_eq!((d.tool.as_str(), d.detail.as_str(), d.label.as_str(), d.too_large), ("Bash", "rm -rf node_modules", "shop", false));
        let d = outside_detail(&json!({"tool_name": "Edit", "tool_input": {"file_path": "a.rs", "old_string": "x", "new_string": "y"}}));
        assert_eq!(d.detail, "a.rs\n--- old\nx\n+++ new\ny");
        let d = outside_detail(&json!({"tool_name": "Edit", "tool_input": {"file_path": "a.rs", "old_string": "x", "new_string": "y", "replace_all": true}}));
        assert!(d.detail.contains("\"replace_all\": true"), "every key Allow covers is shown");
        let d = outside_detail(&json!({"tool_name": "Write", "tool_input": {"file_path": "big.txt", "content": "z".repeat(9000)}}));
        assert!(d.too_large, "a write over 8 KiB offers no Allow");
        assert!(d.detail.len() <= MAX_DETAIL_BYTES + 4);
        let d = outside_detail(&json!({"tool_name": "mcp__gh__issue", "tool_input": {"n": 1}, "mcp_server": {"name": "gh"}, "permission_suggestions": [{"type": "addRules"}]}));
        assert!(d.detail.contains("\"n\": 1") && d.detail.contains("never applied") && d.detail.contains("MCP server"));
        let d = outside_detail(&json!({"tool_name": "Bash", "tool_input": {"command": "touch sub1.txt"}, "agent_id": "aed5aacb28d201d4a", "agent_type": "general-purpose"}));
        assert_eq!(d.subagent, "general-purpose");
    }

    /// Review S4: Allow once covers the whole call, so the card shows all
    /// of it, says loudly when it leaves Claude Code's sandbox, and offers
    /// no Allow when there is nothing to show.
    #[test]
    fn the_card_shows_the_whole_call() {
        let d = outside_detail(&json!({"tool_name": "Bash", "tool_input": {"command": "make", "description": "build", "timeout": 600000, "run_in_background": true, "dangerouslyDisableSandbox": true}}));
        assert!(d.detail.starts_with("Runs outside Claude Code's sandbox\n\nmake"), "{}", d.detail);
        assert!(d.detail.contains("\"timeout\": 600000") && d.detail.contains("\"run_in_background\": true") && d.detail.contains("dangerouslyDisableSandbox"));
        assert!(d.detail.contains("\"description\": \"build\""), "every other key, the agent's description too");
        assert!(!d.too_large);
        for ti in [json!({"command": ""}), json!({"command": "   "}), json!({"command": ["rm", "-rf"]}), json!({})] {
            assert!(outside_detail(&json!({"tool_name": "Bash", "tool_input": ti})).too_large, "no command, no Allow: {ti}");
        }
        assert!(outside_detail(&json!({"tool_name": "Write", "tool_input": {"content": "x"}})).too_large, "a write with no path");
        assert!(outside_detail(&json!({"tool_name": "mcp__x__y", "tool_input": {}})).too_large, "an empty call");
        assert!(outside_detail(&json!({"tool_name": "mcp__x__y"})).too_large, "no input at all");
        let d = outside_detail(&json!({"tool_name": "WebFetch", "tool_input": {"url": "https://example.com", "prompt": "p"}}));
        assert!(d.detail.contains("example.com") && d.detail.contains("\"prompt\"") && !d.too_large);
    }

    #[test]
    fn limits_cap_the_rate_and_the_open_prompts() {
        let l = Limits::default();
        let held: Vec<_> = (0..3).map(|_| l.take().unwrap()).collect();
        assert_eq!(l.take().err(), Some("3 prompts are already waiting"));
        drop(held);
        let mut n = 3;
        while l.take().is_ok() {
            n += 1;
        }
        assert_eq!(n, MAX_PER_MINUTE, "10 a minute");
    }

    #[test]
    fn tokens_are_private_and_compared_whole() {
        let dir = tempfile::tempdir().unwrap();
        let p = ensure_token(dir.path()).unwrap();
        let t = read_token(&p).unwrap();
        assert_eq!(ensure_token(dir.path()).unwrap(), p);
        assert_eq!(read_token(&p).unwrap(), t, "kept, not rotated");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(hook_dir(dir.path())).unwrap().permissions().mode() & 0o777, 0o700);
        }
        assert!(same_token(&t, &t) && !same_token(&t, &t[1..]) && !same_token(&t, &"0".repeat(64)));
    }
}

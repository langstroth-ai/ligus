//! The per-turn unix socket the Ligus MCP server listens on (D.3.2), in a
//! fresh 0700 directory. Inside the sandbox the agent's MCP config runs
//! `ligus mcp-bridge`, which connects here; the socket is the one extra path
//! the executor's plan allows.
//!
//! Protocol: MCP JSON-RPC 2.0, one message per line. The first message on a
//! connection must be `initialize` carrying `params._meta.ligusTurnCred`
//! (the bridge adds it); anything else closes the connection. Every later
//! `tools/call` re-checks the credential, so a connection dies with its turn.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::server::LigusMcp;
use crate::agent_turn::McpAttach;

/// The MCP protocol versions the server answers with.
pub const MCP_PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// A listening socket for one turn. Dropping it stops the listener and
/// removes the directory.
pub struct SocketEndpoint {
    pub dir: PathBuf,
    pub path: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for SocketEndpoint {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The program that bridges stdio to the socket: an explicit one (tests,
/// `LIGUS_MCP_BRIDGE`), a `ligus-mcp-bridge` next to this executable, else
/// this executable with the `mcp-bridge` argument (the `ligus` CLI and the
/// desktop app both handle it).
pub fn bridge_command() -> (PathBuf, Vec<String>) {
    if let Some(p) = hive_core::compat::var("LIGUS_MCP_BRIDGE").ok().filter(|s| !s.trim().is_empty()) {
        return (PathBuf::from(p.trim()), Vec::new());
    }
    if let Ok(me) = std::env::current_exe() {
        for dir in [me.parent(), me.parent().and_then(Path::parent)].into_iter().flatten() {
            let sibling = dir.join("ligus-mcp-bridge");
            if sibling.is_file() {
                return (sibling, Vec::new());
            }
        }
        return (me, vec![super::bridge::BRIDGE_ARG.to_string()]);
    }
    (PathBuf::from("ligus"), vec![super::bridge::BRIDGE_ARG.to_string()])
}

impl SocketEndpoint {
    /// The agent-side config: the bridge, with the socket and the credential
    /// in its env (never on a command line).
    pub fn attach(&self, credential: &str) -> McpAttach {
        let (command, args) = bridge_command();
        McpAttach {
            name: crate::executor::LIGUS_MCP_ID.to_string(),
            command,
            args,
            env: vec![
                (super::bridge::SOCKET_ENV.to_string(), self.path.to_string_lossy().into_owned()),
                (super::bridge::CRED_ENV.to_string(), credential.to_string()),
            ],
            socket: self.path.clone(),
            config_dir: self.dir.clone(),
        }
    }
}

fn make_dir() -> std::io::Result<PathBuf> {
    let mut rnd = [0u8; 8];
    getrandom::getrandom(&mut rnd).map_err(|e| std::io::Error::other(e.to_string()))?;
    let id: String = rnd.iter().map(|b| format!("{b:02x}")).collect();
    let dir = crate::executor::canonical(&std::env::temp_dir()).join(format!("ligus-mcp-{id}"));
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(&dir)?;
    Ok(dir)
}

/// Start listening for `mcp`'s turns. Must run inside a tokio runtime.
#[cfg(unix)]
pub fn listen(mcp: Arc<LigusMcp>) -> std::io::Result<SocketEndpoint> {
    let dir = make_dir()?;
    let path = dir.join("s");
    let listener = tokio::net::UnixListener::bind(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { break };
            let mcp = mcp.clone();
            tokio::spawn(async move {
                let _ = serve_conn(mcp, stream).await;
            });
        }
    });
    Ok(SocketEndpoint { dir, path, task })
}

#[cfg(not(unix))]
pub fn listen(_mcp: Arc<LigusMcp>) -> std::io::Result<SocketEndpoint> {
    Err(std::io::Error::other("the Ligus MCP socket needs unix sockets"))
}

#[cfg(unix)]
async fn serve_conn(mcp: Arc<LigusMcp>, stream: tokio::net::UnixStream) -> std::io::Result<()> {
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    let mut cred: Option<String> = None;
    while let Some(line) = lines.next_line().await? {
        let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else { continue };
        let reply = handle(&mcp, &mut cred, &msg).await;
        let close = cred.is_none();
        if let Some(r) = reply {
            let mut b = serde_json::to_vec(&r)?;
            b.push(b'\n');
            w.write_all(&b).await?;
            w.flush().await?;
        }
        if close {
            // No valid credential on the first message: hang up.
            break;
        }
    }
    Ok(())
}

/// Handle one message. `cred` is set by a valid `initialize`.
pub async fn handle(mcp: &LigusMcp, cred: &mut Option<String>, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let err = |code: i64, message: String| id.clone().map(|id| json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}));
    let ok = |result: Value| id.clone().map(|id| json!({"jsonrpc": "2.0", "id": id, "result": result}));
    if cred.is_none() {
        if method != "initialize" {
            return err(-32001, "the first message must be initialize with a Ligus turn credential".into());
        }
        let presented = msg.pointer("/params/_meta/ligusTurnCred").and_then(Value::as_str).unwrap_or("");
        return match mcp.authorize(presented) {
            Ok(_) => {
                *cred = Some(presented.to_string());
                let asked = msg.pointer("/params/protocolVersion").and_then(Value::as_str).unwrap_or("");
                let version = MCP_PROTOCOL_VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(MCP_PROTOCOL_VERSIONS[0]);
                ok(json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "ligus", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": "Ligus workspace tools: propose (never apply), coordinate, read the workspace, ask the humans.",
                }))
            }
            Err(e) => err(-32001, e.to_string()),
        };
    }
    let c = cred.clone().unwrap_or_default();
    match method {
        "notifications/initialized" | "notifications/cancelled" => None,
        "ping" => ok(json!({})),
        "tools/list" => match mcp.authorize(&c) {
            Ok(_) => ok(json!({ "tools": super::tools::mcp_tools() })),
            Err(e) => err(-32001, e.to_string()),
        },
        "tools/call" => {
            let name = msg.pointer("/params/name").and_then(Value::as_str).unwrap_or("");
            let args = msg.pointer("/params/arguments").cloned().unwrap_or_else(|| json!({}));
            match mcp.call(&c, name, &args).await {
                Ok(r) => ok(json!({ "content": [{ "type": "text", "text": r.text }], "isError": r.is_error })),
                Err(e) => err(-32001, e.to_string()),
            }
        }
        _ if id.is_some() => err(-32601, format!("method not found: {method}")),
        _ => None,
    }
}

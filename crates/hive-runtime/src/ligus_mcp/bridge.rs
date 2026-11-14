//! `ligus mcp-bridge`: the stdio MCP server an agent's config runs inside
//! the sandbox. It connects to `$LIGUS_MCP_SOCKET`, adds
//! `params._meta.ligusTurnCred = $LIGUS_TURN_CRED` to the first message
//! (`initialize`), and pipes MCP JSON-RPC lines both ways until either side
//! closes. It holds no state and grants nothing by itself: the server checks
//! the credential.

use std::io::{BufRead, BufReader, Write};

use serde_json::Value;

/// The hidden argument that turns a Ligus binary into the bridge.
pub const BRIDGE_ARG: &str = "mcp-bridge";
pub const SOCKET_ENV: &str = "LIGUS_MCP_SOCKET";
pub const CRED_ENV: &str = "LIGUS_TURN_CRED";

/// Add the credential to an `initialize` message.
pub fn with_credential(line: &str, cred: &str) -> String {
    match serde_json::from_str::<Value>(line.trim()) {
        Ok(mut v) if v.get("method").and_then(Value::as_str) == Some("initialize") => {
            if !v.get("params").is_some_and(Value::is_object) {
                v["params"] = serde_json::json!({});
            }
            if !v["params"].get("_meta").is_some_and(Value::is_object) {
                v["params"]["_meta"] = serde_json::json!({});
            }
            v["params"]["_meta"]["ligusTurnCred"] = Value::String(cred.to_string());
            v.to_string()
        }
        _ => line.trim_end().to_string(),
    }
}

/// Run the bridge on this process's stdio. Returns the exit code.
#[cfg(unix)]
pub fn run_stdio() -> i32 {
    let (Ok(sock), Ok(cred)) = (std::env::var(SOCKET_ENV), std::env::var(CRED_ENV)) else {
        eprintln!("ligus mcp-bridge: {SOCKET_ENV} and {CRED_ENV} must be set (Ligus sets them for an agent turn)");
        return 2;
    };
    let stream = match std::os::unix::net::UnixStream::connect(&sock) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ligus mcp-bridge: can't reach the Ligus MCP server: {e}");
            return 1;
        }
    };
    let Ok(mut to_server) = stream.try_clone() else { return 1 };
    let from_server = stream;
    let out = std::thread::spawn(move || {
        let mut stdout = std::io::stdout();
        for line in BufReader::new(from_server).lines() {
            let Ok(line) = line else { break };
            if writeln!(stdout, "{line}").and_then(|_| stdout.flush()).is_err() {
                break;
            }
        }
        std::process::exit(0);
    });
    let stdin = std::io::stdin();
    let mut first = true;
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let msg = if first { with_credential(&line, &cred) } else { line };
        first = false;
        if writeln!(to_server, "{msg}").and_then(|_| to_server.flush()).is_err() {
            break;
        }
    }
    let _ = to_server.shutdown(std::net::Shutdown::Write);
    let _ = out.join();
    0
}

#[cfg(not(unix))]
pub fn run_stdio() -> i32 {
    eprintln!("ligus mcp-bridge needs unix sockets");
    2
}

/// When this process was started as the bridge (first argument
/// [`BRIDGE_ARG`]), run it and exit. Hosts call this first in `main`.
pub fn maybe_run_bridge() {
    if std::env::args().nth(1).as_deref() == Some(BRIDGE_ARG) {
        std::process::exit(run_stdio());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_initialize_gets_the_credential() {
        let l = with_credential(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#, "CRED");
        let v: Value = serde_json::from_str(&l).unwrap();
        assert_eq!(v["params"]["_meta"]["ligusTurnCred"], "CRED");
        assert_eq!(v["params"]["protocolVersion"], "2025-06-18");
        let l = with_credential(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#, "CRED");
        assert!(!l.contains("CRED"));
    }
}

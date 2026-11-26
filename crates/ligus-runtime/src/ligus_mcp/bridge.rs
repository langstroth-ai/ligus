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
/// Under the Linux sandbox, where seccomp refuses new unix sockets (C-10),
/// the bridge reaches the server through this loopback address instead: the
/// sandbox helper's forwarder inside the turn's own network namespace
/// (bubblewrap), or the endpoint's own listener (the Landlock tier).
pub const TCP_ENV: &str = "LIGUS_MCP_TCP";
/// The forwarder's port inside the sandbox's network namespace.
pub const SANDBOX_TCP_PORT: u16 = 29417;

/// The extra bridge env this host's sandbox needs: the forwarder's address
/// under bubblewrap, nothing elsewhere (see [`sandbox_tcp_env_for`] for the
/// Landlock tier, whose port is per endpoint).
pub fn sandbox_tcp_env() -> Option<(String, String)> {
    (crate::executor::default_executor().kind() == crate::executor::ExecutorKind::Bwrap)
        .then(|| (TCP_ENV.to_string(), format!("127.0.0.1:{SANDBOX_TCP_PORT}")))
}

/// The extra bridge env for the endpoint listening on `socket`: under
/// bubblewrap the helper's forwarder; under the Landlock tier (no network
/// namespace) the endpoint's own loopback port
/// ([`super::socket::tcp_port_for`]); nothing elsewhere.
pub fn sandbox_tcp_env_for(socket: &std::path::Path) -> Option<(String, String)> {
    match crate::executor::default_executor().kind() {
        crate::executor::ExecutorKind::Landlock => super::socket::tcp_port_for(socket).map(|p| (TCP_ENV.to_string(), format!("127.0.0.1:{p}"))),
        _ => sandbox_tcp_env(),
    }
}

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

/// Adds the credential to the first `initialize` the agent sends, wherever
/// it comes: MCP clients from the 2026-07 spec on open with a
/// `server/discover` probe first. Every other line passes through as is.
pub struct Crediting<'a> {
    cred: &'a str,
    sent: bool,
}

impl<'a> Crediting<'a> {
    pub fn new(cred: &'a str) -> Self {
        Self { cred, sent: false }
    }

    pub fn line(&mut self, line: &str) -> String {
        if self.sent {
            return line.trim_end().to_string();
        }
        let out = with_credential(line, self.cred);
        if out.contains("ligusTurnCred") {
            self.sent = true;
        }
        out
    }
}

/// Run the bridge on this process's stdio. Returns the exit code.
#[cfg(unix)]
pub fn run_stdio() -> i32 {
    let (Ok(sock), Ok(cred)) = (std::env::var(SOCKET_ENV), std::env::var(CRED_ENV)) else {
        eprintln!("ligus mcp-bridge: {SOCKET_ENV} and {CRED_ENV} must be set (Ligus sets them for an agent turn)");
        return 2;
    };
    match std::os::unix::net::UnixStream::connect(&sock) {
        Ok(s) => match s.try_clone() {
            Ok(w) => pipe(s, w, &cred, |w| {
                let _ = w.shutdown(std::net::Shutdown::Write);
            }),
            Err(_) => 1,
        },
        // Inside the Linux sandbox new unix sockets are refused (EPERM from
        // seccomp): use the helper's loopback forwarder. Only on that exact
        // refusal, so the credential never goes to a loopback port anywhere
        // else.
        Err(e) if e.raw_os_error() == Some(libc::EPERM) && std::env::var_os(TCP_ENV).is_some() => {
            let addr = std::env::var(TCP_ENV).unwrap_or_default();
            match std::net::TcpStream::connect(addr.trim()).and_then(|s| Ok((s.try_clone()?, s))) {
                Ok((w, r)) => pipe(r, w, &cred, |w| {
                    let _ = w.shutdown(std::net::Shutdown::Write);
                }),
                Err(e) => {
                    eprintln!("ligus mcp-bridge: can't reach the Ligus MCP server through the sandbox: {e}");
                    1
                }
            }
        }
        Err(e) => {
            eprintln!("ligus mcp-bridge: can't reach the Ligus MCP server: {e}");
            1
        }
    }
}

/// Pipe stdio to the server connection (`from_server` / `to_server` are the
/// two halves of one stream) until either side closes.
#[cfg(unix)]
fn pipe<R, W>(from_server: R, mut to_server: W, cred: &str, close: impl FnOnce(&mut W)) -> i32
where
    R: std::io::Read + Send + 'static,
    W: std::io::Write,
{
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
    let mut crediting = Crediting::new(cred);
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let msg = crediting.line(&line);
        if writeln!(to_server, "{msg}").and_then(|_| to_server.flush()).is_err() {
            break;
        }
    }
    close(&mut to_server);
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

//! A stub ACP agent for the `acp_stub` integration tests: it speaks the
//! Agent Client Protocol (v1) over stdio the way Copilot CLI (`copilot
//! --acp`), Antigravity (`agy acp`) and the Codex / Claude adapters do, so
//! the ACP driver is tested end to end without any real CLI or subscription.
//!
//! `--mode <flavor>`:
//! - `copilot`, `agy`, `claude`: initialize, session/new, then per prompt:
//!   a thought, a tool call, a permission request for an `edit`, a file
//!   write through `fs/write_text_file` when allowed, and a reply that says
//!   what happened (`permission=<optionId|cancelled>`, `mcp=<names>`).
//! - `codex`: as above, but `session/new` reports auth required until the
//!   client authenticates with the advertised API-key method.
//! - `broken`: exits without answering `initialize` (an old CLI without ACP).
//! - `slow`: never answers the prompt (idle timeout).
//! - `flood`: answers the prompt after writing one 100 MiB line.
//! - `spend`: reports a cost of $0.25, then $0.50, then answers.
//! - `eur`: reports a cost in EUR, then answers.
//! - `always`: as `copilot`, but its permission request offers only a
//!   standing `allow_always` option (no `allow_once`).
//!
//! Std-only; exits when stdin closes.

use std::io::{BufRead, Write};

use serde_json::{json, Value};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.iter().position(|a| a == "--mode").and_then(|i| args.get(i + 1)).cloned().unwrap_or_else(|| "copilot".into());
    if mode == "broken" {
        eprintln!("error: unknown option '--acp'");
        std::process::exit(2);
    }
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut send = |v: Value| {
        let _ = writeln!(out, "{v}");
        let _ = out.flush();
    };
    let mut authed = mode != "codex";
    let mut mcp_names: Vec<String> = Vec::new();
    let mut cwd = String::new();
    let mut next_id = 1000u64;
    while let Some(Ok(line)) = lines.next() {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("").to_string();
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match method.as_str() {
            "initialize" => {
                let methods = if mode == "codex" {
                    json!([{ "id": "chatgpt", "name": "Log in with ChatGPT" }, { "id": "openai-api-key", "name": "Use OPENAI_API_KEY" }])
                } else {
                    json!([])
                };
                send(json!({"jsonrpc":"2.0","id":id,"result":{
                    "protocolVersion": 1,
                    "agentCapabilities": {"loadSession": false, "mcpCapabilities": {"http": false, "sse": false}},
                    "agentInfo": {"name": format!("stub-{mode}"), "version": "0.0.0"},
                    "authMethods": methods
                }}));
            }
            "authenticate" => {
                if params.get("methodId").and_then(Value::as_str) == Some("openai-api-key") {
                    authed = true;
                    send(json!({"jsonrpc":"2.0","id":id,"result":{}}));
                } else {
                    send(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"interactive login not available"}}));
                }
            }
            "session/new" => {
                if !authed {
                    send(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":"Authentication required"}}));
                    continue;
                }
                cwd = params.get("cwd").and_then(Value::as_str).unwrap_or("").to_string();
                mcp_names = params
                    .get("mcpServers")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|s| s.get("name").and_then(Value::as_str).map(str::to_owned)).collect())
                    .unwrap_or_default();
                send(json!({"jsonrpc":"2.0","id":id,"result":{"sessionId":"sess-1"}}));
            }
            "session/set_model" => send(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"not supported"}})),
            "session/prompt" => {
                if mode == "slow" {
                    continue;
                }
                if mode == "flood" {
                    // Stdout's lock is reentrant, so this writes alongside `send`.
                    let mut raw = std::io::stdout();
                    let chunk = vec![b'x'; 1024 * 1024];
                    for _ in 0..100 {
                        if raw.write_all(&chunk).is_err() {
                            std::process::exit(0);
                        }
                    }
                    let _ = raw.write_all(b"\n");
                    let _ = raw.flush();
                }
                let sid = params.get("sessionId").cloned().unwrap_or(Value::Null);
                let update = |u: Value| json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":sid,"update":u}});
                if mode == "spend" || mode == "eur" {
                    let cur = if mode == "eur" { "EUR" } else { "USD" };
                    send(update(json!({"sessionUpdate":"usage_update","used":10,"size":100,"cost":{"amount":0.25,"currency":cur}})));
                    send(update(json!({"sessionUpdate":"usage_update","used":20,"size":100,"cost":{"amount":0.50,"currency":cur}})));
                    send(update(json!({"sessionUpdate":"agent_message_chunk","messageId":"m1","content":{"type":"text","text":"kept going"}})));
                    send(json!({"jsonrpc":"2.0","id":id,"result":{"stopReason":"end_turn"}}));
                    continue;
                }
                send(update(json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"thinking"}})));
                send(update(json!({"sessionUpdate":"tool_call","toolCallId":"t1","title":"Edit notes.txt","kind":"edit","status":"pending","rawInput":{"path":"notes.txt"}})));
                // Ask permission for the edit.
                next_id += 1;
                let perm_id = next_id;
                send(json!({"jsonrpc":"2.0","id":perm_id,"method":"session/request_permission","params":{
                    "sessionId": sid,
                    "toolCall": {"toolCallId":"t1","title":"Edit notes.txt","kind":"edit"},
                    "options": if mode == "always" {
                        json!([
                            {"optionId":"allow-always","name":"Always allow","kind":"allow_always"},
                            {"optionId":"reject-once","name":"Reject","kind":"reject_once"}
                        ])
                    } else {
                        json!([
                            {"optionId":"allow-once","name":"Allow","kind":"allow_once"},
                            {"optionId":"reject-once","name":"Reject","kind":"reject_once"}
                        ])
                    }
                }}));
                let mut outcome = String::from("none");
                let mut wrote = String::from("no");
                // Read until the permission answer arrives.
                while let Some(Ok(l)) = lines.next() {
                    let Ok(m) = serde_json::from_str::<Value>(&l) else { continue };
                    if m.get("id").and_then(Value::as_u64) == Some(perm_id) {
                        let o = m.pointer("/result/outcome").cloned().unwrap_or(Value::Null);
                        outcome = match o.get("outcome").and_then(Value::as_str) {
                            Some("selected") => o.get("optionId").and_then(Value::as_str).unwrap_or("").to_string(),
                            Some(other) => other.to_string(),
                            None => "invalid".into(),
                        };
                        break;
                    }
                }
                if outcome == "allow-once" || outcome == "allow-always" {
                    next_id += 1;
                    let wid = next_id;
                    send(json!({"jsonrpc":"2.0","id":wid,"method":"fs/write_text_file","params":{
                        "sessionId": sid, "path": format!("{cwd}/notes.txt"), "content": "written over ACP\n"
                    }}));
                    while let Some(Ok(l)) = lines.next() {
                        let Ok(m) = serde_json::from_str::<Value>(&l) else { continue };
                        if m.get("id").and_then(Value::as_u64) == Some(wid) {
                            wrote = if m.get("error").is_some() { "refused".into() } else { "yes".into() };
                            break;
                        }
                    }
                }
                send(update(json!({"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed","content":[{"type":"content","content":{"type":"text","text":"done"}}]})));
                send(update(json!({"sessionUpdate":"usage_update","used":10,"size":100,"cost":{"amount":0.25,"currency":"USD"}})));
                let text = format!("hello from {mode}; permission={outcome}; wrote={wrote}; mcp={}", mcp_names.join(","));
                send(update(json!({"sessionUpdate":"agent_message_chunk","messageId":"m1","content":{"type":"text","text":text}})));
                send(json!({"jsonrpc":"2.0","id":id,"result":{"stopReason":"end_turn"}}));
            }
            "session/cancel" => {}
            _ => {
                if !id.is_null() {
                    send(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"unknown method"}}));
                }
            }
        }
    }
}

//! The Ligus tool definitions. The JSON Schemas live in `schema/*.json` and
//! are frozen per release (`tool_schemas_golden`).

use serde_json::{json, Value};

/// The full tool set, offered over MCP to agents that take MCP servers.
pub const TOOLS: [(&str, &str); 16] = [
    ("ligus_propose_diff", include_str!("schema/ligus_propose_diff.json")),
    ("ligus_propose_decision", include_str!("schema/ligus_propose_decision.json")),
    ("ligus_propose_command", include_str!("schema/ligus_propose_command.json")),
    ("ligus_propose_workflow", include_str!("schema/ligus_propose_workflow.json")),
    ("ligus_open_alternatives", include_str!("schema/ligus_open_alternatives.json")),
    ("ligus_assess", include_str!("schema/ligus_assess.json")),
    ("ligus_handoff", include_str!("schema/ligus_handoff.json")),
    ("ligus_mark", include_str!("schema/ligus_mark.json")),
    ("ligus_unmark", include_str!("schema/ligus_unmark.json")),
    ("ligus_react", include_str!("schema/ligus_react.json")),
    ("ligus_list_chats", include_str!("schema/ligus_list_chats.json")),
    ("ligus_read_chat", include_str!("schema/ligus_read_chat.json")),
    ("ligus_read_proposals", include_str!("schema/ligus_read_proposals.json")),
    ("ligus_read_board", include_str!("schema/ligus_read_board.json")),
    ("ligus_ask_human", include_str!("schema/ligus_ask_human.json")),
    ("ligus_report", include_str!("schema/ligus_report.json")),
];

/// The same capabilities folded into four tools, for in-process tool loops
/// (API and local runtimes) whose offered-tool cap is 16 (D.3.3).
pub const FOLDED: [(&str, &str); 4] = [
    ("ligus_propose", include_str!("schema/ligus_propose.json")),
    ("ligus_flow", include_str!("schema/ligus_flow.json")),
    ("ligus_read", include_str!("schema/ligus_read.json")),
    ("ligus_ask", include_str!("schema/ligus_ask.json")),
];

fn parse(raw: &str) -> Value {
    serde_json::from_str(raw).expect("frozen tool schema is valid JSON")
}

/// `tools/list` entries (`{name, description, inputSchema}`).
pub fn mcp_tools() -> Vec<Value> {
    TOOLS.iter().map(|(_, raw)| parse(raw)).collect()
}

/// Tool-loop definitions (`{name, description, input_schema}`) for the four
/// folded tools.
pub fn tool_loop_defs() -> Vec<Value> {
    FOLDED
        .iter()
        .map(|(_, raw)| {
            let v = parse(raw);
            json!({ "name": v["name"], "description": v["description"], "input_schema": v["inputSchema"] })
        })
        .collect()
}

/// Whether `name` is one of the Ligus tools (full or folded).
pub fn is_ligus_tool(name: &str) -> bool {
    TOOLS.iter().chain(FOLDED.iter()).any(|(n, _)| *n == name)
}

/// A folded call as the full tool it stands for.
pub fn unfold(name: &str, input: &Value) -> Result<(String, Value), String> {
    let field = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let full = match name {
        "ligus_propose" => match field("kind").as_str() {
            "diff" => "ligus_propose_diff",
            "decision" => "ligus_propose_decision",
            "command" => "ligus_propose_command",
            "workflow" => "ligus_propose_workflow",
            "alternatives" => "ligus_open_alternatives",
            "assess" => "ligus_assess",
            other => return Err(format!("unknown proposal kind {other:?}")),
        },
        "ligus_flow" => match field("op").as_str() {
            "handoff" => "ligus_handoff",
            "mark" => "ligus_mark",
            "unmark" => "ligus_unmark",
            "react" => "ligus_react",
            "report" => "ligus_report",
            other => return Err(format!("unknown flow op {other:?}")),
        },
        "ligus_read" => match field("what").as_str() {
            "chats" => "ligus_list_chats",
            "chat" => "ligus_read_chat",
            "proposals" => "ligus_read_proposals",
            "board" => "ligus_read_board",
            other => return Err(format!("unknown read {other:?}")),
        },
        "ligus_ask" => "ligus_ask_human",
        other if TOOLS.iter().any(|(n, _)| *n == other) => other,
        other => return Err(format!("unknown Ligus tool {other:?}")),
    };
    let mut args = input.clone();
    if let Some(o) = args.as_object_mut() {
        for k in ["kind", "op", "what"] {
            // `kind` is also a mark's field; only strip the folding selector.
            if (k == "kind" && name == "ligus_propose") || (k == "op" && name == "ligus_flow") || (k == "what" && name == "ligus_read") {
                o.remove(k);
            }
        }
    }
    Ok((full.to_string(), args))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    /// `tool_schemas_golden`: the schemas are frozen per release. Changing a
    /// tool's schema changes this digest; update it only with a release note.
    #[test]
    fn tool_schemas_golden() {
        let mut h = Sha256::new();
        for (name, raw) in TOOLS.iter().chain(FOLDED.iter()) {
            let v = parse(raw);
            assert_eq!(v["name"], *name, "file name and tool name agree");
            assert_eq!(v["inputSchema"]["type"], "object");
            h.update(name.as_bytes());
            h.update(serde_json::to_vec(&v).unwrap());
        }
        let digest: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(digest, GOLDEN, "a frozen Ligus tool schema changed");
        assert_eq!(TOOLS.len(), 16);
        assert_eq!(FOLDED.len(), 4, "the folded set fits the reserved MCP slots");
        assert!(FOLDED.len() <= crate::agent_tools::MCP_RESERVED_SLOTS);
    }

    const GOLDEN: &str = "2f9f554d6897e0d75f82e371f42d9a7eee44b67c34391e11701faa69b17a57c0";

    #[test]
    fn folded_calls_unfold_to_the_full_tools() {
        let (n, a) = unfold("ligus_propose", &json!({"kind": "command", "title": "t", "argv": ["ls"]})).unwrap();
        assert_eq!(n, "ligus_propose_command");
        assert!(a.get("kind").is_none() && a["argv"][0] == "ls");
        let (n, a) = unfold("ligus_flow", &json!({"op": "mark", "kind": "lock", "path": "x"})).unwrap();
        assert_eq!(n, "ligus_mark");
        assert_eq!(a["kind"], "lock", "a mark keeps its own kind");
        assert_eq!(unfold("ligus_read", &json!({"what": "board"})).unwrap().0, "ligus_read_board");
        assert_eq!(unfold("ligus_ask", &json!({"question": "?"})).unwrap().0, "ligus_ask_human");
        assert!(unfold("ligus_propose", &json!({"kind": "vote"})).is_err());
        assert!(unfold("ligus_apply", &json!({})).is_err());
    }
}

//! The Ligus tool definitions. The JSON Schemas live in `schema/*.json` and
//! are frozen per release (`tool_schemas_golden`).

use serde_json::{json, Value};

/// The full tool set, offered over MCP to agents that take MCP servers.
/// Release note (0.1): six read-only `ligus_forge_*` tools joined the set,
/// and `ligus_read` gained the matching `forge.*` reads; the memory and
/// task-state tools joined it, with `ligus_propose` kind `memory`,
/// `ligus_flow` op `taskUpdate` and `ligus_read` `memory`, `memoryEntry`
/// and `taskState`; `ligus_offer_file` joined it with secure attachments
/// (issue #118). Release note (0.4): `ligus_ask_person` and `ligus_ask_room`
/// joined the set (decision cards: who may answer, quorum, yes/no, free
/// text); folded `ligus_ask` gained `to` (`person`, `room`); and
/// `ligus_ask_human`'s description points at them. Nothing was renamed.
pub const TOOLS: [(&str, &str); 30] = [
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
    // Decision cards (agent questions, 0.4).
    ("ligus_ask_person", include_str!("schema/ligus_ask_person.json")),
    ("ligus_ask_room", include_str!("schema/ligus_ask_room.json")),
    ("ligus_report", include_str!("schema/ligus_report.json")),
    // Workspace memory (search, read, propose) and live task state.
    ("ligus_memory_search", include_str!("schema/ligus_memory_search.json")),
    ("ligus_memory_read", include_str!("schema/ligus_memory_read.json")),
    ("ligus_memory_propose", include_str!("schema/ligus_memory_propose.json")),
    ("ligus_task_update", include_str!("schema/ligus_task_update.json")),
    ("ligus_task_state", include_str!("schema/ligus_task_state.json")),
    // Read-only code-host tools (IDENTITY-AND-FORGES 5.5): the token stays
    // in the host process; see `forge_tools`.
    ("ligus_forge_list_prs", include_str!("schema/ligus_forge_list_prs.json")),
    ("ligus_forge_read_pr", include_str!("schema/ligus_forge_read_pr.json")),
    ("ligus_forge_list_issues", include_str!("schema/ligus_forge_list_issues.json")),
    ("ligus_forge_read_issue", include_str!("schema/ligus_forge_read_issue.json")),
    ("ligus_forge_ci_status", include_str!("schema/ligus_forge_ci_status.json")),
    ("ligus_forge_read_file", include_str!("schema/ligus_forge_read_file.json")),
    // Issue #118: an agent's file offer, filed as a proposal a person
    // approves before anything is shared.
    ("ligus_offer_file", include_str!("schema/ligus_offer_file.json")),
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
            "memory" => "ligus_memory_propose",
            other => return Err(format!("unknown proposal kind {other:?}")),
        },
        "ligus_flow" => match field("op").as_str() {
            "handoff" => "ligus_handoff",
            "mark" => "ligus_mark",
            "unmark" => "ligus_unmark",
            "react" => "ligus_react",
            "report" => "ligus_report",
            "taskUpdate" => "ligus_task_update",
            other => return Err(format!("unknown flow op {other:?}")),
        },
        "ligus_read" => match field("what").as_str() {
            "chats" => "ligus_list_chats",
            "chat" => "ligus_read_chat",
            "proposals" => "ligus_read_proposals",
            "board" => "ligus_read_board",
            "memory" => "ligus_memory_search",
            "memoryEntry" => "ligus_memory_read",
            "taskState" => "ligus_task_state",
            "forge.prs" => "ligus_forge_list_prs",
            "forge.pr" => "ligus_forge_read_pr",
            "forge.issues" => "ligus_forge_list_issues",
            "forge.issue" => "ligus_forge_read_issue",
            "forge.ci" => "ligus_forge_ci_status",
            "forge.file" => "ligus_forge_read_file",
            other => return Err(format!("unknown read {other:?}")),
        },
        // Without `to`: the older tool, as before.
        "ligus_ask" => match field("to").as_str() {
            "person" => "ligus_ask_person",
            "room" => "ligus_ask_room",
            "" => "ligus_ask_human",
            other => return Err(format!("unknown ask audience {other:?}")),
        },
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
        if name == "ligus_ask" {
            o.remove("to");
            if let Some(m) = o.remove("member") {
                o.insert("to".into(), m);
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
        assert_eq!(TOOLS.len(), 30);
        assert_eq!(FOLDED.len(), 4, "the folded set fits the reserved MCP slots");
        assert!(FOLDED.len() <= crate::agent_tools::MCP_RESERVED_SLOTS);
    }

    const GOLDEN: &str = "50f833ff0fe2a77ab9611e441d93eb0b3fa9ee175962015a948237322cfa4cca";

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
        let (n, a) = unfold("ligus_ask", &json!({"to": "person", "member": "m1", "question": "?"})).unwrap();
        assert_eq!((n.as_str(), a["to"].as_str()), ("ligus_ask_person", Some("m1")));
        let (n, a) = unfold("ligus_ask", &json!({"to": "room", "quorum": 2})).unwrap();
        assert_eq!(n, "ligus_ask_room");
        assert!(a.get("to").is_none() && a["quorum"] == 2);
        assert!(unfold("ligus_ask", &json!({"to": "everyone"})).is_err());
        assert!(unfold("ligus_propose", &json!({"kind": "vote"})).is_err());
        assert!(unfold("ligus_apply", &json!({})).is_err());
        let (n, a) = unfold("ligus_read", &json!({"what": "forge.pr", "number": "7"})).unwrap();
        assert_eq!((n.as_str(), a["number"].as_str()), ("ligus_forge_read_pr", Some("7")));
        assert!(a.get("what").is_none());
        let (n, a) = unfold("ligus_propose", &json!({"kind": "memory", "op": "create", "title": "t"})).unwrap();
        assert_eq!((n.as_str(), a["op"].as_str()), ("ligus_memory_propose", Some("create")), "a memory proposal keeps its op");
        assert_eq!(unfold("ligus_flow", &json!({"op": "taskUpdate"})).unwrap().0, "ligus_task_update");
        assert_eq!(unfold("ligus_read", &json!({"what": "memory", "query": "x"})).unwrap().0, "ligus_memory_search");
        assert_eq!(unfold("ligus_read", &json!({"what": "memoryEntry"})).unwrap().0, "ligus_memory_read");
        assert_eq!(unfold("ligus_read", &json!({"what": "taskState"})).unwrap().0, "ligus_task_state");
    }
}

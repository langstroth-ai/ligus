//! End-to-end stdio MCP round trip against a real child process (the
//! `mcp_stdio_fixture` bin). The unit tests in `mcp.rs` cover the gate and
//! wire parsing with fakes; this is the only place the actual subprocess
//! spawn + line-framed JSON-RPC handshake is exercised.

use ligus_runtime::mcp::McpError;
use ligus_runtime::{McpRegistry, McpServerSpec, McpTransport};
use serde_json::json;

fn fixture_spec(enabled: bool) -> McpServerSpec {
    McpServerSpec {
        id: "fixture".into(),
        transport: McpTransport::Stdio {
            command: env!("CARGO_BIN_EXE_mcp_stdio_fixture").into(),
            args: Vec::new(),
        },
        enabled,
        auth: None,
        agents: Vec::new(),
    }
}

/// A stdio MCP server runs in the agent sandbox. A host without one
/// (Windows) refuses to start it, fail closed, and says why: `true` when
/// that is this host (after checking the refusal).
async fn refused_without_sandbox(registry: &McpRegistry) -> bool {
    if !ligus_runtime::executor::skip_without_sandbox("stdio MCP round trip") {
        return false;
    }
    let err = registry.list_tools("fixture").await.expect_err("no sandbox, no stdio MCP server");
    assert!(matches!(&err, McpError::Transport(m) if m.contains("no agent sandbox")), "{err:?}");
    true
}

#[tokio::test]
async fn stdio_list_tools_round_trip() {
    let registry = McpRegistry::new(vec![fixture_spec(true)]);
    if refused_without_sandbox(&registry).await {
        return;
    }
    let tools = registry.list_tools("fixture").await.expect("tools/list");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");
    assert_eq!(tools[0].description, "Echo a message back");
    assert_eq!(tools[0].input_schema["properties"]["msg"]["type"], "string");
}

#[tokio::test]
async fn stdio_call_tool_round_trip() {
    let registry = McpRegistry::new(vec![fixture_spec(true)]);
    if refused_without_sandbox(&registry).await {
        return;
    }
    let text = registry
        .call_tool("fixture", "echo", &json!({ "msg": "hi ligus" }))
        .await
        .expect("tools/call");
    assert_eq!(text, "echo: hi ligus");
}

#[tokio::test]
async fn stdio_disabled_server_stays_inert() {
    // The install-is-inert-until-enabled gate, at the integration level: the
    // fixture binary exists and works, but a disabled spec must never launch it.
    let registry = McpRegistry::new(vec![fixture_spec(false)]);
    assert!(matches!(
        registry.list_tools("fixture").await,
        Err(McpError::Disabled)
    ));
    assert!(matches!(
        registry.call_tool("fixture", "echo", &json!({ "msg": "x" })).await,
        Err(McpError::Disabled)
    ));
}

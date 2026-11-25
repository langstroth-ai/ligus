//! MCP tools for a turn, and the Anthropic tool-loop adapter. The desktop and
//! the headless worker each had a copy; this is the one every host runs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::mcp::{McpRegistry, McpServerSpec, McpTool};
use crate::provider::anthropic::{content_value, AnthropicClient, AnthropicResponse, ChatTurn, ProviderError};
use crate::tool_loop::{MessagesApi, ToolExecutor};

/// Prompt-injection markers seen in a turn's tool results. Cheap to clone
/// (shared), so the executor and the turn see the same list.
#[derive(Clone, Default)]
pub struct InjectionHits(Arc<Mutex<Vec<&'static str>>>);

impl InjectionHits {
    /// Scan an untrusted tool or MCP result.
    pub fn scan(&self, text: &str) {
        let found = crate::guard_scan::injection_markers(text);
        if !found.is_empty() {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).extend(found);
        }
    }

    pub fn take(&self) -> Vec<&'static str> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// Executes a tool call by routing the (sanitized) tool name back to its MCP
/// server. Honors the registry's enable gate.
pub struct McpToolExecutor {
    /// Where results are scanned for injection markers (`None`: not scanned).
    pub injection_hits: Option<InjectionHits>,
    registry: McpRegistry,
    /// sanitized tool name → (server id, real tool name)
    names: HashMap<String, (String, String)>,
}

impl ToolExecutor for McpToolExecutor {
    async fn call(&self, name: &str, input: &Value) -> (String, bool) {
        match self.names.get(name) {
            Some((server, tool)) => match self.registry.call_tool(server, tool, input).await {
                Ok(text) => {
                    if let Some(h) = &self.injection_hits {
                        h.scan(&text);
                    }
                    (text, false)
                }
                Err(e) => (format!("tool error: {e}"), true),
            },
            None => (format!("unknown tool {name}"), true),
        }
    }
}

/// Anthropic tool names must match `^[a-zA-Z0-9_-]{1,64}$`.
pub fn sanitize_tool_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    cleaned.chars().take(64).collect()
}

/// Provider turns as the Anthropic message array the tool loop seeds
/// (`[Attached: <image>]` markers become vision blocks).
pub fn turns_to_messages(turns: &[ChatTurn]) -> Vec<Value> {
    turns.iter().map(|t| json!({ "role": t.role, "content": content_value(&t.content) })).collect()
}

/// The servers' tools as neutral provider definitions plus the executor that
/// routes a call back to its server. `listed` sees every tool the servers
/// listed (the desktop records which ones are shells); without the shell
/// (`shell = false`) a tool that runs shell commands is never offered.
/// `None` when no server is enabled or none lists an offered tool.
pub async fn tool_set(
    servers: Vec<McpServerSpec>,
    agent: Option<uuid::Uuid>,
    shell: bool,
    injection_hits: Option<InjectionHits>,
    listed: impl FnOnce(&[(String, McpTool)]),
) -> Option<(Vec<Value>, McpToolExecutor)> {
    // Per-agent reach (device-local, `McpServerSpec::agents`): a server
    // limited to some agents is never offered to another agent's turn, nor
    // to a turn with no workspace agent.
    let servers: Vec<McpServerSpec> = servers.into_iter().filter(|s| s.reaches(agent)).collect();
    let registry = McpRegistry::new(servers);
    registry.enabled().next()?;
    let tagged = registry.list_all_tools().await;
    listed(&tagged);
    let tagged: Vec<_> = tagged.into_iter().filter(|(_, t)| shell || !crate::is_shell_tool(&t.name)).collect();
    if tagged.is_empty() {
        return None;
    }
    let mut defs = Vec::with_capacity(tagged.len());
    let mut names = HashMap::new();
    for (server, tool) in &tagged {
        let tname = sanitize_tool_name(&format!("{server}__{}", tool.name));
        defs.push(json!({ "name": tname, "description": tool.description, "input_schema": tool.input_schema }));
        names.insert(tname, (server.clone(), tool.name.clone()));
    }
    Some((defs, McpToolExecutor { injection_hits, registry, names }))
}

/// One non-streaming Anthropic turn for the tool loop.
pub struct AnthropicMessages {
    pub client: AnthropicClient,
    pub api_key: String,
    pub model: String,
    pub system: Option<String>,
    pub max_tokens: u32,
}

impl MessagesApi for AnthropicMessages {
    async fn run(&self, messages: &[Value], tools: &[Value]) -> Result<AnthropicResponse, ProviderError> {
        self.client.run_messages(&self.api_key, &self.model, self.system.as_deref(), messages, tools, self.max_tokens).await
    }
}

#[cfg(test)]
mod reach_tests {
    use super::*;

    /// `ligus reply` and the turn engine both go through `tool_set`: a
    /// server limited to other agents is dropped before anything starts.
    #[tokio::test]
    async fn tool_set_drops_servers_the_agent_may_not_reach() {
        let a = uuid::Uuid::new_v4();
        let mut s = McpServerSpec::new("limited", crate::mcp::McpTransport::Stdio { command: "/nonexistent/ligus-test-server".into(), args: vec![] });
        s.enabled = true;
        s.agents = vec![a];
        for agent in [None, Some(uuid::Uuid::new_v4())] {
            let mut listed = false;
            assert!(tool_set(vec![s.clone()], agent, false, None, |_| listed = true).await.is_none());
            assert!(!listed, "{agent:?}: never started");
        }
        let mut listed = false;
        let _ = tool_set(vec![s], Some(a), false, None, |_| listed = true).await;
        assert!(listed, "the named agent's turn lists it");
    }
}

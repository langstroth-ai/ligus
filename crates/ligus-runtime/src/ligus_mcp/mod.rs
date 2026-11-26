//! The Ligus MCP server (Track D2): typed workspace tools for agents in place
//! of the `[[…]]` reply directives.
//!
//! - [`server`]: the tools, per-turn credentials, budgets and audit;
//! - [`socket`]: the per-turn unix socket agents reach through
//!   `ligus mcp-bridge` ([`bridge`]) inside the sandbox;
//! - [`attach`]: how the legacy CLI adapters get the server;
//! - [`tools`]: the frozen schemas (22 tools, folded into 4 for tool loops);
//! - [`cred`]: the per-turn credential (`ligus-mcp-turn-cred-v1\0`);
//! - [`forge_tools`]: the read-only code-host tools (no writes, no token in the sandbox).

pub mod attach;
pub mod bridge;
pub mod cred;
pub mod forge_tools;
pub mod server;
pub mod socket;
pub mod tools;

#[cfg(test)]
mod tests;

pub use server::{Budgets, CascadeBudgets, HostAsker, InProcessTools, LigusHost, LigusMcp, Outcome, SelfReport, ToolResult, TurnHandle, TurnSpec, WithLigus};

impl TurnHandle {
    /// Serve this turn on its own unix socket (for CLI agents). Must run
    /// inside a tokio runtime.
    pub fn listen(&self) -> std::io::Result<std::path::PathBuf> {
        let ep = socket::listen(self.mcp.clone())?;
        let path = ep.path.clone();
        *self.socket.lock().unwrap_or_else(|e| e.into_inner()) = Some(ep);
        Ok(path)
    }
}

/// Whether a turn on `rt` can use the Ligus MCP server: the subscription
/// CLIs over ACP, Claude Code and Copilot on their legacy adapters, and the
/// in-process tool loops (folded tools). Everything else (aider, pi, hermes,
/// Codex and Antigravity on their legacy adapters, a runtime without tool
/// use) keeps the reply directives (D.4.4).
pub fn offered_for(rt: &crate::ResolvedRuntime, restricted: bool) -> bool {
    use ligus_core::ModelProviderKind as K;
    match rt.provider {
        K::ClaudeCode | K::Codex | K::Antigravity | K::Copilot => {
            crate::provider::acp::resolve(rt, restricted).is_ok() || attach::legacy_attaches(rt.provider)
        }
        K::Anthropic => true,
        _ => crate::provider::dispatch::openai_tools_allowed(rt) || (rt.provider == K::Ollama && rt.tools),
    }
}

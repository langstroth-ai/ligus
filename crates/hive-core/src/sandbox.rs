//! The sandbox envelope: one protective layer the **workspace** applies to every
//! agent turn, whatever runtime runs it (Claude Code, Codex, aider, pi, an API
//! model with MCP tools...). Individual runtimes don't each need their own
//! safety configuration; the workspace decides the envelope and each runtime
//! adapter (`hive_runtime::sandbox`) enforces it as far as that runtime allows,
//! refusing the turn when it can't.
//!
//! Four parts:
//!   * **isolated worktree**: a write-capable turn runs in its own git worktree
//!     and its changes come back as a review-gated proposal;
//!   * **filesystem allow-list**: the roots a turn may read and write (the
//!     turn's own directory, never `.git`, `.ligus` or config files);
//!   * **egress policy**: open, only allow-listed MCP servers, or closed;
//!   * **secret redaction**: credentials are masked in agent replies before they
//!     reach the log (and so other members and the relay).
//!
//! The envelope only ever tightens from the workspace default: dearth mode and
//! a chat lockdown each narrow it, and combining narrows further
//! ([`SandboxEnvelope::tighten`]). On Kubernetes the same envelope becomes the
//! turn executor's pod and NetworkPolicy (see docs/design/kubernetes.md §4, §7).

use serde::{Deserialize, Serialize};

/// Where a turn may reach over the network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum EgressPolicy {
    /// No restriction beyond the runtime's own (the default).
    Open,
    /// Only these MCP servers (by id); no web tools, no shell.
    AllowList { mcp_servers: Vec<String> },
    /// No MCP servers, no web tools, no shell.
    Closed,
}

impl EgressPolicy {
    pub fn is_open(&self) -> bool {
        matches!(self, EgressPolicy::Open)
    }

    /// Whether MCP server `id` may be offered to the turn.
    pub fn allows_mcp(&self, id: &str) -> bool {
        match self {
            EgressPolicy::Open => true,
            EgressPolicy::AllowList { mcp_servers } => mcp_servers.iter().any(|s| s == id),
            EgressPolicy::Closed => false,
        }
    }

    /// The stricter of two policies (an allow-list intersection when both are
    /// allow-lists).
    pub fn stricter(&self, other: &EgressPolicy) -> EgressPolicy {
        use EgressPolicy::*;
        match (self, other) {
            (Closed, _) | (_, Closed) => Closed,
            (Open, x) | (x, Open) => x.clone(),
            (AllowList { mcp_servers: a }, AllowList { mcp_servers: b }) => AllowList {
                mcp_servers: a.iter().filter(|s| b.contains(s)).cloned().collect(),
            },
        }
    }

    /// Short label for logs and the UI.
    pub fn label(&self) -> &'static str {
        match self {
            EgressPolicy::Open => "open",
            EgressPolicy::AllowList { .. } => "allow-listed MCP only",
            EgressPolicy::Closed => "closed",
        }
    }
}

/// Why the envelope is tighter than the workspace default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TightenedBy {
    /// The workspace is in dearth mode.
    Dearth,
    /// This chat is locked down after an infection signal.
    Lockdown,
}

/// The envelope one turn runs in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxEnvelope {
    /// Write-capable turns must run in an isolated worktree; if one can't be
    /// cut, the turn is refused rather than run in place.
    pub isolate_writes: bool,
    /// Directory names a turn may never write into, at any depth of its root.
    pub protected_dirs: Vec<String>,
    pub egress: EgressPolicy,
    /// Mask credentials in replies before they are stored or synced.
    pub redact_secrets: bool,
    /// Every proposal from this turn needs a human approval before it applies,
    /// whatever its own quorum says.
    pub human_gate: bool,
    #[serde(default)]
    pub tightened_by: Vec<TightenedBy>,
}

/// Directories no agent writes into (mirrors the built-in tools' list).
pub const PROTECTED_DIRS: &[&str] = &[".git", ".ligus", ".hive", ".claude", ".vscode"];

impl Default for SandboxEnvelope {
    fn default() -> Self {
        Self::workspace_default()
    }
}

impl SandboxEnvelope {
    /// The always-on envelope: isolation for writes, protected dirs, secret
    /// redaction, open egress (subject to each runtime's own permission tier).
    pub fn workspace_default() -> Self {
        Self {
            isolate_writes: false,
            protected_dirs: PROTECTED_DIRS.iter().map(|s| s.to_string()).collect(),
            egress: EgressPolicy::Open,
            redact_secrets: true,
            human_gate: false,
            tightened_by: Vec::new(),
        }
    }

    /// Narrow this envelope: stricter egress, isolation and the human gate are
    /// sticky; nothing a tightening does can loosen another.
    pub fn tighten(mut self, by: TightenedBy, egress: EgressPolicy, human_gate: bool) -> Self {
        self.egress = self.egress.stricter(&egress);
        self.human_gate |= human_gate;
        self.isolate_writes = true;
        self.redact_secrets = true;
        if !self.tightened_by.contains(&by) {
            self.tightened_by.push(by);
        }
        self
    }

    pub fn is_tightened(&self) -> bool {
        !self.tightened_by.is_empty()
    }
}

/// The envelope a turn in `session` runs in, from the guard state the session
/// carries (the workspace's settings are overlaid from the config log).
pub fn envelope_for_session(session: &crate::ChatSession) -> SandboxEnvelope {
    let mut env = SandboxEnvelope::workspace_default();
    if let Some(d) = session.guards.dearth_on() {
        // Dearth mode: agents keep only the allow-listed MCP servers.
        env = env.tighten(
            TightenedBy::Dearth,
            EgressPolicy::AllowList { mcp_servers: d.allowed_mcp_servers.clone() },
            false,
        );
    }
    if session.guards.lockdown_active() {
        // Self-medication: no egress at all, and a human gate on every proposal.
        env = env.tighten(TightenedBy::Lockdown, EgressPolicy::Closed, true);
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow(xs: &[&str]) -> EgressPolicy {
        EgressPolicy::AllowList { mcp_servers: xs.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn egress_only_ever_narrows() {
        assert_eq!(EgressPolicy::Open.stricter(&allow(&["a"])), allow(&["a"]));
        assert_eq!(allow(&["a", "b"]).stricter(&allow(&["b", "c"])), allow(&["b"]));
        assert_eq!(allow(&["a"]).stricter(&EgressPolicy::Closed), EgressPolicy::Closed);
        assert_eq!(EgressPolicy::Closed.stricter(&EgressPolicy::Open), EgressPolicy::Closed);
        assert!(allow(&["a"]).allows_mcp("a"));
        assert!(!allow(&["a"]).allows_mcp("b"));
        assert!(!EgressPolicy::Closed.allows_mcp("a"));
    }

    #[test]
    fn tightening_is_sticky_and_combines() {
        let e = SandboxEnvelope::workspace_default();
        assert!(!e.is_tightened() && e.egress.is_open() && e.redact_secrets);
        let e = e.tighten(TightenedBy::Dearth, allow(&["docs"]), false);
        assert!(e.isolate_writes && !e.human_gate);
        let e = e.tighten(TightenedBy::Lockdown, EgressPolicy::Open, true);
        // A later, looser egress can't reopen what dearth closed.
        assert_eq!(e.egress, allow(&["docs"]));
        assert!(e.human_gate);
        assert_eq!(e.tightened_by, vec![TightenedBy::Dearth, TightenedBy::Lockdown]);
        let v = serde_json::to_value(&e.egress).unwrap();
        assert_eq!(v["mode"], "allowList");
        assert_eq!(v["mcpServers"][0], "docs");
    }
}

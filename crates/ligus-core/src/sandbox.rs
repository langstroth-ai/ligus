//! The sandbox envelope: one protective layer the **workspace** applies to every
//! agent turn, whatever runtime runs it (Claude Code, Codex, aider, pi, an API
//! model with MCP tools...). Individual runtimes don't each need their own
//! safety configuration; the workspace decides the envelope and each runtime
//! adapter (`ligus_runtime::sandbox`) enforces it as far as that runtime allows,
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
#[serde(
    tag = "mode",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
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
pub const PROTECTED_DIRS: &[&str] = &[".git", ".ligus", ".claude", ".vscode"];

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
            EgressPolicy::AllowList {
                mcp_servers: d.allowed_mcp_servers.clone(),
            },
            false,
        );
    }
    if session.guards.lockdown_active() {
        // Self-medication: no egress at all, and a human gate on every proposal.
        env = env.tighten(TightenedBy::Lockdown, EgressPolicy::Closed, true);
    }
    env
}

// ---------------------------------------------------------------------------
// Sandbox grants (signed, config log)
// ---------------------------------------------------------------------------

/// Most paths one grant list holds.
pub const MAX_GRANT_PATHS: usize = 32;
/// Most agents with a grant of their own.
pub const MAX_GRANT_AGENTS: usize = 64;
/// Longest path in a grant (bytes).
pub const MAX_GRANT_PATH_BYTES: usize = 256;

/// Paths an agent turn may reach on the host that runs it, on top of its
/// worktree, its per-turn home and the runtime's own allow-list. Each path is
/// `~/...` (under the host's home) or absolute. They never open what the
/// sandbox always denies (Ligus's data dirs, `serve.token`, key files, the
/// credential dirs such as `~/.ssh`): the host keeps those out whatever a
/// grant says.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathGrants {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_only: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_write: Vec<String>,
}

impl PathGrants {
    pub fn is_empty(&self) -> bool {
        self.read_only.is_empty() && self.read_write.is_empty()
    }
}

/// One agent's grants: extra paths, and the shell for turns a non-Owner
/// starts (off unless an Owner turns it on).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentGrant {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_only: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_write: Vec<String>,
    /// The agent keeps its shell (`run_command`, a CLI's Bash or shell tool)
    /// in turns that a member other than the workspace Owner starts.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shell: bool,
}

/// The sandbox grants in force for a workspace (`WorkspaceSandboxSet`, config
/// log). Admin + presence to change; turning any agent's shell grant on or off
/// is the Owner's. Never an agent's: agents can't hold presence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxGrants {
    /// For every agent in the workspace.
    #[serde(default, skip_serializing_if = "PathGrants::is_empty")]
    pub workspace: PathGrants,
    /// Per agent, by agent id.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub agents: std::collections::BTreeMap<uuid::Uuid, AgentGrant>,
}

/// Why a grant set is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantError(pub String);

/// Whether `p` is a well-formed grant path: `~/rel` or `/abs`, normalized (no
/// `.`/`..`/empty components, no trailing slash), not the home or the root
/// itself, no control characters, at most [`MAX_GRANT_PATH_BYTES`].
pub fn grant_path_ok(p: &str) -> Result<(), GrantError> {
    let bad = |why: &str| Err(GrantError(format!("{p:?}: {why}")));
    if p.is_empty() || p.len() > MAX_GRANT_PATH_BYTES {
        return bad("empty or too long");
    }
    if p.chars().any(|c| c.is_control()) {
        return bad("control characters");
    }
    let rest = if let Some(r) = p.strip_prefix("~/") {
        r
    } else if let Some(r) = p.strip_prefix('/') {
        r
    } else {
        return bad("must start with ~/ or /");
    };
    if rest.is_empty() {
        return bad("the home dir or the root itself can't be granted");
    }
    if rest
        .split('/')
        .any(|c| c.is_empty() || c == "." || c == "..")
    {
        return bad("not normalized (empty, . or .. components, or a trailing slash)");
    }
    Ok(())
}

impl SandboxGrants {
    pub fn is_empty(&self) -> bool {
        self.workspace.is_empty() && self.agents.is_empty()
    }

    /// Bounds and path shapes (the fold refuses a set that fails).
    pub fn well_formed(&self) -> Result<(), GrantError> {
        if self.agents.len() > MAX_GRANT_AGENTS {
            return Err(GrantError(format!("more than {MAX_GRANT_AGENTS} agents")));
        }
        let lists = std::iter::once((&self.workspace.read_only, &self.workspace.read_write))
            .chain(self.agents.values().map(|a| (&a.read_only, &a.read_write)));
        for (ro, rw) in lists {
            for l in [ro, rw] {
                if l.len() > MAX_GRANT_PATHS {
                    return Err(GrantError(format!(
                        "more than {MAX_GRANT_PATHS} paths in one list"
                    )));
                }
                for p in l {
                    grant_path_ok(p)?;
                }
            }
        }
        Ok(())
    }

    /// The agents whose shell grant differs between `self` and `other`.
    pub fn shell_changes(&self, other: &SandboxGrants) -> Vec<uuid::Uuid> {
        let shell = |g: &SandboxGrants, id: &uuid::Uuid| g.agents.get(id).is_some_and(|a| a.shell);
        let mut ids: Vec<uuid::Uuid> = self
            .agents
            .keys()
            .chain(other.agents.keys())
            .copied()
            .collect();
        ids.sort();
        ids.dedup();
        ids.into_iter()
            .filter(|id| shell(self, id) != shell(other, id))
            .collect()
    }

    /// Whether agent `id` keeps its shell in a turn a non-Owner starts.
    pub fn shell_granted(&self, id: uuid::Uuid) -> bool {
        self.agents.get(&id).is_some_and(|a| a.shell)
    }

    /// The paths agent `id` gets: the workspace's, then its own (sorted,
    /// deduplicated).
    pub fn paths_for(&self, id: Option<uuid::Uuid>) -> PathGrants {
        let mut p = self.workspace.clone();
        if let Some(a) = id.and_then(|i| self.agents.get(&i)) {
            p.read_only.extend(a.read_only.iter().cloned());
            p.read_write.extend(a.read_write.iter().cloned());
        }
        p.read_only.sort();
        p.read_only.dedup();
        p.read_write.sort();
        p.read_write.dedup();
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_paths_are_normalized_home_or_absolute() {
        for ok in ["~/.cargo/registry", "/opt/tools", "~/work/data", "~/a b"] {
            assert!(grant_path_ok(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "~", "~/", "/", "rel/path", "~/../etc", "/a/./b", "/a//b", "/a/", "~user/x",
            "/a\nb", "~/..",
        ] {
            assert!(grant_path_ok(bad).is_err(), "{bad:?}");
        }
        assert!(grant_path_ok(&format!("/{}", "a".repeat(MAX_GRANT_PATH_BYTES))).is_err());
    }

    #[test]
    fn grant_sets_are_bounded_and_shell_changes_are_named() {
        let id = uuid::Uuid::from_u128(7);
        let mut g = SandboxGrants::default();
        assert!(g.well_formed().is_ok() && g.is_empty());
        g.agents.insert(
            id,
            AgentGrant {
                shell: true,
                ..Default::default()
            },
        );
        assert_eq!(g.shell_changes(&SandboxGrants::default()), vec![id]);
        assert!(g.shell_granted(id) && !g.shell_granted(uuid::Uuid::from_u128(8)));
        // A path-only change is not a shell change.
        let mut h = g.clone();
        h.agents.get_mut(&id).unwrap().read_only.push("~/x".into());
        assert!(h.shell_changes(&g).is_empty());
        assert_eq!(h.paths_for(Some(id)).read_only, vec!["~/x".to_string()]);
        assert!(h.paths_for(None).is_empty());
        h.workspace.read_write = (0..=MAX_GRANT_PATHS).map(|i| format!("~/d{i}")).collect();
        assert!(h.well_formed().is_err());
        let mut many = SandboxGrants::default();
        for i in 0..=MAX_GRANT_AGENTS as u128 {
            many.agents
                .insert(uuid::Uuid::from_u128(i), AgentGrant::default());
        }
        assert!(many.well_formed().is_err());
        // Empty lists and an off shell stay off the wire.
        assert_eq!(serde_json::to_string(&AgentGrant::default()).unwrap(), "{}");
        assert_eq!(
            serde_json::to_string(&SandboxGrants::default()).unwrap(),
            "{}"
        );
    }

    fn allow(xs: &[&str]) -> EgressPolicy {
        EgressPolicy::AllowList {
            mcp_servers: xs.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn egress_only_ever_narrows() {
        assert_eq!(EgressPolicy::Open.stricter(&allow(&["a"])), allow(&["a"]));
        assert_eq!(
            allow(&["a", "b"]).stricter(&allow(&["b", "c"])),
            allow(&["b"])
        );
        assert_eq!(
            allow(&["a"]).stricter(&EgressPolicy::Closed),
            EgressPolicy::Closed
        );
        assert_eq!(
            EgressPolicy::Closed.stricter(&EgressPolicy::Open),
            EgressPolicy::Closed
        );
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
        assert_eq!(
            e.tightened_by,
            vec![TightenedBy::Dearth, TightenedBy::Lockdown]
        );
        let v = serde_json::to_value(&e.egress).unwrap();
        assert_eq!(v["mode"], "allowList");
        assert_eq!(v["mcpServers"][0], "docs");
    }
}

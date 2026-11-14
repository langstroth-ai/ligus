//! Runtime adapters for the workspace sandbox envelope
//! ([`hive_core::sandbox::SandboxEnvelope`]).
//!
//! The envelope is decided once per turn by the workspace; this module turns it
//! into what each runtime understands, and refuses the turn when a runtime
//! can't be confined to it. That keeps the policy runtime-agnostic: a
//! Claude Code turn, a Codex turn, an API model calling MCP tools and an aider
//! turn all get the same egress rule, enforced or refused, never silently
//! skipped.
//!
//! | Runtime | How egress is closed |
//! |---|---|
//! | Claude Code | `--disallowedTools WebFetch WebSearch Bash`, user settings only with hooks off, `--strict-mcp-config` with only the allow-listed servers |
//! | Codex | its own `workspace-write` sandbox with `network_access=false`, no MCP servers |
//! | API models (Anthropic, OpenAI wire, Ollama) | Ligus runs the tools: MCP filtered to the allow-list, `run_command` off |
//! | aider, pi, Hermes, Antigravity, Copilot | can't be confined from outside: refused while egress is not open |

use hive_core::sandbox::{EgressPolicy, SandboxEnvelope};
use hive_core::ModelProviderKind;
use serde_json::{json, Value};

/// What a runtime needs to honor an envelope.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Enforcement {
    /// Extra CLI args for subprocess runtimes.
    pub extra_args: Vec<String>,
    /// Whether Ligus's own `run_command` tool may be offered.
    pub allow_run_command: bool,
    /// The turn can't be confined: refuse it with this reason.
    pub refuse: Option<String>,
}

/// MCP servers (id → spec JSON for Claude Code's `--mcp-config`) the envelope
/// allows. Only used for Claude Code under an allow-list.
pub type McpConfigEntries = Vec<(String, Value)>;

/// Map `envelope` onto `provider`. `allowed_mcp` is the Ligus MCP catalog
/// already filtered by the envelope's egress policy, as Claude Code
/// `mcpServers` entries.
pub fn enforce(provider: ModelProviderKind, envelope: &SandboxEnvelope, allowed_mcp: &McpConfigEntries) -> Enforcement {
    let open = envelope.egress.is_open();
    let mut out = Enforcement { allow_run_command: open, ..Default::default() };
    if open {
        return out;
    }
    match provider {
        ModelProviderKind::ClaudeCode => {
            out.extra_args.extend(["--disallowedTools".into(), "WebFetch".into(), "WebSearch".into(), "Bash".into()]);
            // Repo-shipped `.claude/settings*.json` hooks run shell commands
            // with full network; load only the user's own settings and run
            // no hooks at all while egress is restricted.
            out.extra_args.extend([
                "--setting-sources".into(),
                "user".into(),
                "--settings".into(),
                json!({ "disableAllHooks": true }).to_string(),
            ]);
            let servers: serde_json::Map<String, Value> = match &envelope.egress {
                EgressPolicy::AllowList { .. } => allowed_mcp.iter().cloned().collect(),
                _ => Default::default(),
            };
            out.extra_args.push("--strict-mcp-config".into());
            out.extra_args.push("--mcp-config".into());
            out.extra_args.push(json!({ "mcpServers": servers }).to_string());
        }
        ModelProviderKind::Codex => {
            out.extra_args.extend([
                "-c".into(),
                "sandbox_workspace_write.network_access=false".into(),
                "-c".into(),
                "mcp_servers={}".into(),
            ]);
        }
        // Ligus runs these runtimes' tools itself: MCP is filtered upstream
        // and `run_command` stays off (`allow_run_command` above).
        ModelProviderKind::Anthropic
        | ModelProviderKind::OpenAI
        | ModelProviderKind::OpenRouter
        | ModelProviderKind::Azure
        | ModelProviderKind::Custom
        | ModelProviderKind::Ollama
        | ModelProviderKind::HiveDaemon => {}
        other => {
            out.refuse = Some(format!(
                "This turn runs with network egress {} (dearth mode or a chat lockdown), and Ligus can't confine {:?} to that. \
                 Use Claude Code, Codex or an API model for this chat until the restriction is cleared.",
                envelope.egress.label(),
                other
            ));
        }
    }
    out
}

/// Whether `provider` can be confined to a non-open egress policy.
pub fn confinable(provider: ModelProviderKind) -> bool {
    let closed = SandboxEnvelope { egress: EgressPolicy::Closed, ..SandboxEnvelope::workspace_default() };
    enforce(provider, &closed, &Vec::new()).refuse.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::sandbox::TightenedBy;

    fn tight(egress: EgressPolicy) -> SandboxEnvelope {
        SandboxEnvelope::workspace_default().tighten(TightenedBy::Dearth, egress, false)
    }

    #[test]
    fn open_envelope_changes_nothing() {
        for p in [ModelProviderKind::ClaudeCode, ModelProviderKind::Aider, ModelProviderKind::Codex] {
            let e = enforce(p, &SandboxEnvelope::workspace_default(), &Vec::new());
            assert!(e.extra_args.is_empty() && e.refuse.is_none() && e.allow_run_command);
        }
    }

    #[test]
    fn claude_code_loses_web_shell_and_foreign_mcp() {
        let allowed = vec![("docs".to_string(), json!({"type": "http", "url": "https://docs.example"}))];
        let e = enforce(ModelProviderKind::ClaudeCode, &tight(EgressPolicy::AllowList { mcp_servers: vec!["docs".into()] }), &allowed);
        assert!(e.extra_args.windows(4).any(|w| w == ["--disallowedTools", "WebFetch", "WebSearch", "Bash"]));
        assert!(e.extra_args.contains(&"--strict-mcp-config".to_string()));
        let cfg = e.extra_args.last().unwrap();
        assert!(cfg.contains("docs.example"));
        assert!(!e.allow_run_command);
        // Closed: an empty server set even if the caller passed some.
        let e = enforce(ModelProviderKind::ClaudeCode, &tight(EgressPolicy::Closed), &allowed);
        assert_eq!(e.extra_args.last().unwrap(), r#"{"mcpServers":{}}"#);
    }

    /// A repo can ship `.claude/settings.json` (or an earlier turn can write
    /// the gitignored `settings.local.json`) with hooks, which Claude Code runs
    /// as shell commands with full network in `-p` mode. Under a restricted
    /// envelope those must not load, or the egress rule is moot.
    #[test]
    fn claude_code_ignores_repo_settings_and_hooks_under_restriction() {
        for egress in [EgressPolicy::Closed, EgressPolicy::AllowList { mcp_servers: vec![] }] {
            let e = enforce(ModelProviderKind::ClaudeCode, &tight(egress), &Vec::new());
            assert!(e.extra_args.windows(2).any(|w| w == ["--setting-sources", "user"]), "{:?}", e.extra_args);
            let i = e.extra_args.iter().position(|a| a == "--settings").expect("--settings");
            let v: Value = serde_json::from_str(&e.extra_args[i + 1]).unwrap();
            assert_eq!(v["disableAllHooks"], json!(true));
        }
    }

    #[test]
    fn codex_network_off_and_unconfinable_runtimes_refused() {
        let e = enforce(ModelProviderKind::Codex, &tight(EgressPolicy::Closed), &Vec::new());
        assert!(e.extra_args.contains(&"sandbox_workspace_write.network_access=false".to_string()));
        for p in [ModelProviderKind::Aider, ModelProviderKind::Pi, ModelProviderKind::Hermes, ModelProviderKind::Antigravity, ModelProviderKind::Copilot] {
            assert!(enforce(p, &tight(EgressPolicy::Closed), &Vec::new()).refuse.is_some(), "{p:?} must be refused");
            assert!(!confinable(p));
        }
        for p in [ModelProviderKind::Anthropic, ModelProviderKind::Ollama, ModelProviderKind::Custom] {
            let e = enforce(p, &tight(EgressPolicy::Closed), &Vec::new());
            assert!(e.refuse.is_none() && !e.allow_run_command);
        }
    }
}

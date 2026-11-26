//! Runtime adapters for the workspace sandbox envelope
//! ([`ligus_core::sandbox::SandboxEnvelope`]).
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
//! | Claude Code | `--disallowedTools WebFetch WebSearch Bash`, `--strict-mcp-config` with only the allow-listed servers (every turn, open too); no settings, hooks or plugins (`provider::claude_code::confine`) |
//! | Codex | its own `workspace-write` sandbox with `network_access=false`, no MCP servers |
//! | API models (Anthropic, OpenAI wire, Ollama) | Ligus runs the tools: MCP filtered to the allow-list, `run_command` off |
//! | aider, pi, Hermes, Antigravity, Copilot | can't be confined from outside: refused while egress is not open |

use ligus_core::sandbox::{EgressPolicy, SandboxEnvelope};
use ligus_core::ModelProviderKind;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Signed grants on this device
// ---------------------------------------------------------------------------

/// Whether this device applies the workspace's signed sandbox grants (its
/// paths and its agents' shell grants). A grant reaches the device of every
/// member whose host runs that agent, so each device decides: on by default
/// for the workspace Owner's own devices (and the device-local workspace),
/// off for every other member's, who turns it on in Settings, Sandbox (a
/// headless host: `LIGUS_SANDBOX_GRANTS=apply`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantsHere {
    pub apply: bool,
    /// What applies without a setting.
    pub default: bool,
    /// This device's own choice, when it made one.
    pub setting: Option<bool>,
    /// Why, in a sentence (doctor, Settings).
    pub why: String,
}

/// The decision for a device whose user holds `my_role` in the workspace
/// (`local`: the device-local workspace), with this device's `setting`.
pub fn grants_here(
    setting: Option<bool>,
    my_role: Option<ligus_core::WorkspaceRole>,
    local: bool,
) -> GrantsHere {
    let owner = local || my_role == Some(ligus_core::WorkspaceRole::Owner);
    let default = owner;
    let apply = setting.unwrap_or(default);
    let why = match (setting, owner) {
        (Some(true), _) => "applied: this device chose to apply the workspace's grants".to_string(),
        (Some(false), _) => "not applied: this device chose not to apply the workspace's grants".to_string(),
        (None, true) => "applied: this device is the workspace Owner's (the default)".to_string(),
        (None, false) => "not applied: this device isn't the workspace Owner's, so grants apply only once it opts in (the default)".to_string(),
    };
    GrantsHere {
        apply,
        default,
        setting,
        why,
    }
}

/// The env var a headless host (`ligus worker`, `serve`, `agent`) sets to
/// `apply` or `ignore` the workspace's signed grants; unset, the default.
pub const GRANTS_ENV: &str = "LIGUS_SANDBOX_GRANTS";

/// [`GRANTS_ENV`] as a setting.
pub fn grants_setting_from_env() -> Option<bool> {
    let v = ligus_core::names::var(GRANTS_ENV).ok()?;
    match v.trim().to_ascii_lowercase().as_str() {
        "apply" | "on" | "1" | "true" | "yes" => Some(true),
        "ignore" | "off" | "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

/// A short digest of a grant set (empty for none): what a device saw last,
/// so it can say when the grants changed.
pub fn grants_fingerprint(g: &ligus_core::sandbox::SandboxGrants) -> String {
    use sha2::{Digest, Sha256};
    if g.is_empty() {
        return String::new();
    }
    let bytes = serde_json::to_vec(g).unwrap_or_default();
    Sha256::digest(&bytes)
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect()
}

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
pub fn enforce(
    provider: ModelProviderKind,
    envelope: &SandboxEnvelope,
    allowed_mcp: &McpConfigEntries,
) -> Enforcement {
    let open = envelope.egress.is_open();
    let mut out = Enforcement {
        allow_run_command: open,
        ..Default::default()
    };
    if provider == ModelProviderKind::ClaudeCode {
        // Every turn, open or not: only the MCP servers this chat may use
        // (the Ligus server joins at dispatch). The rest of Claude Code's
        // confinement (settings sources, hooks, plugins, the tool
        // allow-list, out-of-turn tools) is applied at launch for every
        // caller: `provider::claude_code::confine`.
        if !open {
            out.extra_args.extend([
                "--disallowedTools".into(),
                "WebFetch".into(),
                "WebSearch".into(),
                "Bash".into(),
            ]);
        }
        let servers: serde_json::Map<String, Value> = match &envelope.egress {
            EgressPolicy::Closed => Default::default(),
            _ => allowed_mcp.iter().cloned().collect(),
        };
        out.extra_args.push("--strict-mcp-config".into());
        out.extra_args.push("--mcp-config".into());
        out.extra_args
            .push(json!({ "mcpServers": servers }).to_string());
        return out;
    }
    if open {
        return out;
    }
    match provider {
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
        | ModelProviderKind::LigusDaemon => {}
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
    let closed = SandboxEnvelope {
        egress: EgressPolicy::Closed,
        ..SandboxEnvelope::workspace_default()
    };
    enforce(provider, &closed, &Vec::new()).refuse.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ligus_core::sandbox::TightenedBy;

    fn tight(egress: EgressPolicy) -> SandboxEnvelope {
        SandboxEnvelope::workspace_default().tighten(TightenedBy::Dearth, egress, false)
    }

    #[test]
    fn open_envelope_changes_nothing() {
        for p in [ModelProviderKind::Aider, ModelProviderKind::Codex] {
            let e = enforce(p, &SandboxEnvelope::workspace_default(), &Vec::new());
            assert!(e.extra_args.is_empty() && e.refuse.is_none() && e.allow_run_command);
        }
    }

    /// Claude Code under an open envelope still gets only the chat's MCP
    /// servers, never the user's own (`~/.claude.json`, plugins, claude.ai
    /// connectors).
    #[test]
    fn claude_code_open_envelope_is_strict_mcp() {
        let allowed = vec![(
            "docs".to_string(),
            json!({"type": "http", "url": "https://docs.example"}),
        )];
        let e = enforce(
            ModelProviderKind::ClaudeCode,
            &SandboxEnvelope::workspace_default(),
            &allowed,
        );
        assert!(e.refuse.is_none() && e.allow_run_command);
        assert!(e.extra_args.contains(&"--strict-mcp-config".to_string()));
        assert!(e.extra_args.last().unwrap().contains("docs.example"));
        assert!(!e.extra_args.contains(&"--disallowedTools".to_string()));
    }

    #[test]
    fn claude_code_loses_web_shell_and_foreign_mcp() {
        let allowed = vec![(
            "docs".to_string(),
            json!({"type": "http", "url": "https://docs.example"}),
        )];
        let e = enforce(
            ModelProviderKind::ClaudeCode,
            &tight(EgressPolicy::AllowList {
                mcp_servers: vec!["docs".into()],
            }),
            &allowed,
        );
        assert!(e
            .extra_args
            .windows(4)
            .any(|w| w == ["--disallowedTools", "WebFetch", "WebSearch", "Bash"]));
        assert!(e.extra_args.contains(&"--strict-mcp-config".to_string()));
        let cfg = e.extra_args.last().unwrap();
        assert!(cfg.contains("docs.example"));
        assert!(!e.allow_run_command);
        // Closed: an empty server set even if the caller passed some.
        let e = enforce(
            ModelProviderKind::ClaudeCode,
            &tight(EgressPolicy::Closed),
            &allowed,
        );
        assert_eq!(e.extra_args.last().unwrap(), r#"{"mcpServers":{}}"#);
    }

    /// A repo can ship `.claude/settings.json` (or an earlier turn can write
    /// the gitignored `settings.local.json`) with hooks and permission rules.
    /// Under a restricted envelope those must not load even with the
    /// workspace's opt-in, or the egress rule is moot.
    #[test]
    fn claude_code_ignores_repo_settings_and_hooks_under_restriction() {
        use crate::provider::claude_code::{confine, Confine};
        for egress in [
            EgressPolicy::Closed,
            EgressPolicy::AllowList {
                mcp_servers: vec![],
            },
        ] {
            let e = enforce(
                ModelProviderKind::ClaudeCode,
                &tight(egress.clone()),
                &Vec::new(),
            );
            let (args, _) = confine(
                &e.extra_args,
                Confine {
                    egress_open: egress.is_open(),
                    user_config: true,
                    writes_allowed: true,
                    connectors: true,
                    shell: true,
                },
            );
            assert!(
                args.windows(2)
                    .any(|w| w[0] == "--setting-sources" && w[1].is_empty()),
                "{args:?}"
            );
            let i = args
                .iter()
                .position(|a| a == "--settings")
                .expect("--settings");
            let v: Value = serde_json::from_str(&args[i + 1]).unwrap();
            assert_eq!(v["disableAllHooks"], json!(true));
        }
    }

    #[test]
    fn codex_network_off_and_unconfinable_runtimes_refused() {
        let e = enforce(
            ModelProviderKind::Codex,
            &tight(EgressPolicy::Closed),
            &Vec::new(),
        );
        assert!(e
            .extra_args
            .contains(&"sandbox_workspace_write.network_access=false".to_string()));
        for p in [
            ModelProviderKind::Aider,
            ModelProviderKind::Pi,
            ModelProviderKind::Hermes,
            ModelProviderKind::Antigravity,
            ModelProviderKind::Copilot,
        ] {
            assert!(
                enforce(p, &tight(EgressPolicy::Closed), &Vec::new())
                    .refuse
                    .is_some(),
                "{p:?} must be refused"
            );
            assert!(!confinable(p));
        }
        for p in [
            ModelProviderKind::Anthropic,
            ModelProviderKind::Ollama,
            ModelProviderKind::Custom,
        ] {
            let e = enforce(p, &tight(EgressPolicy::Closed), &Vec::new());
            assert!(e.refuse.is_none() && !e.allow_run_command);
        }
    }
}

#[cfg(test)]
mod grants_here_tests {
    use super::*;
    use ligus_core::WorkspaceRole::*;

    /// Review: a grant applied on every member's host with no say. The
    /// Owner's devices (and the local workspace) apply them by default;
    /// every other member's device only once it opts in; a device's own
    /// choice wins either way.
    #[test]
    fn grants_apply_by_default_only_on_the_owners_devices() {
        assert!(grants_here(None, Some(Owner), false).apply);
        assert!(grants_here(None, None, true).apply);
        for r in [Admin, Contributor, Viewer] {
            assert!(!grants_here(None, Some(r), false).apply, "{r:?}");
            assert!(grants_here(Some(true), Some(r), false).apply, "{r:?}");
        }
        assert!(!grants_here(None, None, false).apply);
        assert!(!grants_here(Some(false), Some(Owner), false).apply);
        assert!(grants_here(None, Some(Admin), false)
            .why
            .contains("opts in"));
    }

    #[test]
    fn fingerprint_changes_with_the_grants() {
        let mut g = ligus_core::sandbox::SandboxGrants::default();
        assert_eq!(grants_fingerprint(&g), "");
        g.workspace.read_only.push("~/data".into());
        let a = grants_fingerprint(&g);
        g.workspace.read_write.push("~/scratch".into());
        assert!(!a.is_empty() && a != grants_fingerprint(&g));
    }
}

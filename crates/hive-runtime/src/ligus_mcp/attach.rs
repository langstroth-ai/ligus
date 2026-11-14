//! Attaching the Ligus MCP server to the legacy (non-ACP) CLI adapters.
//!
//! | CLI | How |
//! |---|---|
//! | Claude Code | `--mcp-config <0600 file>` (merged with an envelope's `--strict-mcp-config` list) |
//! | Copilot CLI | `--additional-mcp-config @<0600 file>` |
//! | Codex, Antigravity | not attached: their legacy adapters use the reply directives |
//!
//! The file lives in the turn's 0700 MCP directory, so the credential in its
//! env never appears on a command line.

use hive_core::ModelProviderKind;

use crate::agent_turn::McpAttach;

/// Whether a legacy adapter can take the Ligus MCP server.
pub fn legacy_attaches(kind: ModelProviderKind) -> bool {
    matches!(kind, ModelProviderKind::ClaudeCode | ModelProviderKind::Copilot)
}

/// Add the Ligus server to a legacy CLI's args (in place). A no-op for CLIs
/// that can't take it, or when the config file can't be written.
pub fn legacy_args(kind: ModelProviderKind, a: &McpAttach, args: &mut Vec<String>) {
    match kind {
        ModelProviderKind::ClaudeCode => {
            let Ok(path) = a.write_config_file("claude-mcp.json", &Default::default()) else { return };
            let path = path.to_string_lossy().into_owned();
            // `--mcp-config` takes several configs; add ours after an
            // existing list (an envelope's strict allow-list keeps it).
            if let Some(i) = args.iter().position(|x| x == "--mcp-config") {
                args.insert((i + 2).min(args.len()), path);
            } else {
                args.push("--mcp-config".into());
                args.push(path);
            }
        }
        ModelProviderKind::Copilot => {
            let Ok(path) = a.write_config_file("copilot-mcp.json", &Default::default()) else { return };
            args.push("--additional-mcp-config".into());
            args.push(format!("@{}", path.to_string_lossy()));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attach(dir: &std::path::Path) -> McpAttach {
        McpAttach {
            name: "ligus".into(),
            command: "/usr/local/bin/ligus".into(),
            args: vec!["mcp-bridge".into()],
            env: vec![("LIGUS_TURN_CRED".into(), "SECRET-CRED".into())],
            socket: dir.join("s"),
            config_dir: dir.to_path_buf(),
        }
    }

    #[test]
    fn credential_stays_off_the_command_line() {
        let d = tempfile::tempdir().unwrap();
        let a = attach(d.path());
        let mut args = vec!["--strict-mcp-config".to_string(), "--mcp-config".into(), "{\"mcpServers\":{}}".into(), "--model".into(), "x".into()];
        legacy_args(ModelProviderKind::ClaudeCode, &a, &mut args);
        assert!(args.iter().all(|x| !x.contains("SECRET-CRED")));
        assert_eq!(args[1], "--mcp-config");
        let file = &args[3];
        let body = std::fs::read_to_string(file).unwrap();
        assert!(body.contains("SECRET-CRED") && body.contains("mcp-bridge"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(file).unwrap().permissions().mode() & 0o077, 0, "0600");
        }
        let mut c = vec![];
        legacy_args(ModelProviderKind::Copilot, &a, &mut c);
        assert_eq!(c[0], "--additional-mcp-config");
        assert!(c[1].starts_with('@'));
        let mut g = vec![];
        legacy_args(ModelProviderKind::Antigravity, &a, &mut g);
        assert!(g.is_empty());
    }
}

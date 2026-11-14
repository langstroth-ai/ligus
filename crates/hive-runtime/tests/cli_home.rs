//! `agent_cannot_persist_cli_config` (hardening C-9): for every first-class
//! CLI, a child spawned for a turn that writes hooks or MCP servers into its
//! CLI's config (through `$HOME` or the real absolute path) leaves the user's
//! real config untouched; the turn sees a copy of what it needs to start.
//!
//! Own test binary: it points `HOME` at a temporary "real" home.

use std::process::Stdio;

use hive_core::sandbox::SandboxEnvelope;
use hive_core::ModelProviderKind;
use hive_runtime::author_gate::GatePlan;
use hive_runtime::executor::{self, HostKind, Purpose, SpawnSpec, TurnPolicy};

/// Per CLI: (config file it reads, a hook/MCP file an attacker would add).
fn targets(kind: ModelProviderKind) -> (&'static str, &'static str) {
    match kind {
        ModelProviderKind::ClaudeCode => (".claude/settings.json", ".claude.json"),
        ModelProviderKind::Codex => (".codex/config.toml", ".codex/hooks.toml"),
        ModelProviderKind::Copilot => (".copilot/config.json", ".copilot/mcp-config.json"),
        ModelProviderKind::Antigravity => (".gemini/settings.json", ".gemini/extensions.json"),
        _ => unreachable!(),
    }
}

#[test]
fn agent_cannot_persist_cli_config() {
    if executor::skip_without_sandbox("agent_cannot_persist_cli_config") {
        return;
    }
    let real = tempfile::tempdir().unwrap();
    let real = executor::canonical(real.path());
    std::env::set_var("HOME", &real);
    let work = tempfile::tempdir().unwrap();
    for kind in [ModelProviderKind::ClaudeCode, ModelProviderKind::Codex, ModelProviderKind::Copilot, ModelProviderKind::Antigravity] {
        let (cfg, extra) = targets(kind);
        let cfg_real = real.join(cfg);
        std::fs::create_dir_all(cfg_real.parent().unwrap()).unwrap();
        std::fs::write(&cfg_real, "ORIGINAL").unwrap();
        let extra_real = real.join(extra);
        std::fs::create_dir_all(extra_real.parent().unwrap()).unwrap();
        let script = format!(
            "cat \"$HOME/{cfg}\"; echo; \
             echo EVIL > \"$HOME/{cfg}\"; echo EVIL > \"$HOME/{extra}\"; \
             echo EVIL > '{}' 2>/dev/null; echo EVIL > '{}' 2>/dev/null; \
             mkdir -p \"$HOME/.claude/hooks\" '{}/.claude/hooks' 2>/dev/null; true",
            cfg_real.display(),
            extra_real.display(),
            real.display()
        );
        let mut policy = TurnPolicy::new(HostKind::Desktop, SandboxEnvelope::workspace_default(), GatePlan::Full);
        policy.worktree_is_isolated = true;
        let out = executor::with_turn_sync(policy, || {
            let mut spec = SpawnSpec::new(Purpose::Agent(kind), "/bin/sh", ["-c", script.as_str()]).cwd(Some(work.path()));
            spec.writes = Some(true);
            let executor::Prepared { mut command, guard } = executor::prepare(spec).unwrap();
            assert_ne!(guard.kind(), executor::ExecutorKind::Unconfined);
            let out = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).output().unwrap();
            drop(guard);
            out
        });
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("ORIGINAL"), "{kind:?}: the turn reads a copy of its config: {stdout}");
        assert_eq!(std::fs::read_to_string(&cfg_real).unwrap(), "ORIGINAL", "{kind:?}: real config untouched");
        assert!(!extra_real.exists(), "{kind:?}: no hook or MCP file in the real config");
        assert!(!real.join(".claude/hooks").exists(), "{kind:?}: no hooks dir");
    }
}

//! H1: the author-role gate, the same on every host (the desktop, `ligus
//! worker`, `ligus serve`, `ligus agent`) and for every runtime.
//!
//! A turn runs tools on the host's own keys and files on behalf of whoever
//! wrote the message that triggered it. A member below the host's floor must
//! not be able to drive those tools. Ligus can withhold its own tool loop
//! (MCP, built-in file tools) from an HTTP model, but a CLI agent's shell and
//! edit tools are part of the CLI, so for those the gate picks the CLI's
//! read-only tier, or refuses the turn when the CLI has none.
//!
//! The floor ([`min_role`]):
//! - **Admin** when the turn can write (a raised permission tier, or a CLI
//!   that always edits files: codex, aider, pi, hermes);
//! - **Contributor** for a read-only turn;
//! - `LIGUS_MCP_MIN_ROLE` overrides both; a value below the default logs a
//!   warning (see [`warn_if_lowered`]).
//!
//! The author's role is taken **as of the triggering message**: every host
//! resolves it with `ChatService::author_role_at`, the config-log roster at
//! the frontier the message stamped (Track A's `ConfigTimeline`). An author
//! that can't be resolved is below every floor (fail closed).

use hive_core::{ModelProviderKind, WorkspaceRole};

/// Env var that sets the minimum author role for tool use on this host.
pub const MIN_ROLE_ENV: &str = "LIGUS_MCP_MIN_ROLE";

/// What a turn may do, given its author's role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatePlan {
    /// The runtime's configured tier and Ligus's tools.
    Full,
    /// Read-only: a CLI at its read-only tier; an HTTP model with no tools.
    /// The OS sandbox also mounts the worktree read-only.
    ReadOnly,
    /// The runtime can't be held read-only: don't run it.
    Refuse(String),
}

impl GatePlan {
    pub fn allows_tools(&self) -> bool {
        matches!(self, GatePlan::Full)
    }
    pub fn allows_writes(&self) -> bool {
        matches!(self, GatePlan::Full)
    }
}

/// Parse a role name (viewer|contributor|admin|owner, any case).
pub fn parse_role(v: &str) -> Option<WorkspaceRole> {
    match v.trim().to_ascii_lowercase().as_str() {
        "viewer" => Some(WorkspaceRole::Viewer),
        "contributor" => Some(WorkspaceRole::Contributor),
        "admin" => Some(WorkspaceRole::Admin),
        "owner" => Some(WorkspaceRole::Owner),
        _ => None,
    }
}

/// The default floor: Admin for a write-capable turn, Contributor otherwise.
pub fn default_min_role(write_capable: bool) -> WorkspaceRole {
    if write_capable {
        WorkspaceRole::Admin
    } else {
        WorkspaceRole::Contributor
    }
}

/// The floor from an explicit setting (`LIGUS_MCP_MIN_ROLE`'s value), or the
/// default for the turn's tier when unset or unparseable.
pub fn min_role_from(explicit: Option<&str>, write_capable: bool) -> WorkspaceRole {
    explicit.and_then(parse_role).unwrap_or_else(|| default_min_role(write_capable))
}

/// This host's floor for a turn (reads `LIGUS_MCP_MIN_ROLE`).
pub fn min_role(write_capable: bool) -> WorkspaceRole {
    min_role_from(hive_core::compat::var(MIN_ROLE_ENV).ok().as_deref(), write_capable)
}

/// The startup warning for an explicit floor below the default, if any.
pub fn lowered_floor_warning(explicit: Option<&str>, write_capable: bool) -> Option<String> {
    let set = explicit.and_then(parse_role)?;
    let default = default_min_role(write_capable);
    (set.rank() < default.rank()).then(|| {
        format!(
            "{MIN_ROLE_ENV}={set:?} is below the default floor {default:?} for this host's {} tier: \
             members at {set:?} can drive this host's tools",
            if write_capable { "write-capable" } else { "read-only" }
        )
    })
}

/// Log [`lowered_floor_warning`] for this host's setting. Call once at
/// startup.
pub fn warn_if_lowered(write_capable: bool) {
    if let Some(w) = lowered_floor_warning(hive_core::compat::var(MIN_ROLE_ENV).ok().as_deref(), write_capable) {
        tracing::warn!(target: "gate", "{w}");
        eprintln!("warning: {w}");
    }
}

/// Whether a runtime always edits files (no read-only tier to fall back to).
pub fn always_writes(runtime: ModelProviderKind) -> bool {
    matches!(
        runtime,
        ModelProviderKind::Codex | ModelProviderKind::Aider | ModelProviderKind::Pi | ModelProviderKind::Hermes
    )
}

/// The gate for one turn: `author_role` is the triggering message's author's
/// role at that message's position (`None` = unresolved).
pub fn gate(runtime: ModelProviderKind, author_role: Option<WorkspaceRole>, min_role: WorkspaceRole) -> GatePlan {
    if author_role.is_some_and(|r| r.rank() >= min_role.rank()) {
        return GatePlan::Full;
    }
    if always_writes(runtime) {
        return GatePlan::Refuse(format!(
            "{runtime:?} always runs with its own file and shell tools, and the message's author is below this host's \
             tool floor ({min_role:?}; {MIN_ROLE_ENV})"
        ));
    }
    GatePlan::ReadOnly
}

/// A CLI runtime's args at its read-only tier: the permission-tier flags are
/// dropped (Claude Code's `--permission-mode`, `agy --mode`, `copilot
/// --allow-tool`/`--allow-all-tools`) and the rest (model, envelope flags)
/// kept.
pub fn read_only_args(runtime: ModelProviderKind, args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut skip_next = false;
    for a in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        let tier_flag_with_value = match runtime {
            ModelProviderKind::ClaudeCode => a == "--permission-mode" || a == "--allowedTools",
            ModelProviderKind::Antigravity => a == "--mode",
            ModelProviderKind::Copilot => a == "--allow-tool",
            _ => false,
        };
        if tier_flag_with_value {
            skip_next = true;
            continue;
        }
        let tier_flag = match runtime {
            ModelProviderKind::ClaudeCode => {
                a.starts_with("--permission-mode=") || a == "--dangerously-skip-permissions" || a.starts_with("--allowedTools=")
            }
            ModelProviderKind::Antigravity if a.starts_with("--mode=") => true,
            ModelProviderKind::Copilot => {
                a.starts_with("--allow-tool=") || a.starts_with("--deny-tool=") || a == "--allow-all-tools" || a == "--allow-all-paths"
            }
            ModelProviderKind::Antigravity => a == "--dangerously-skip-permissions",
            _ => false,
        };
        if !tier_flag {
            out.push(a.clone());
        }
    }
    // Copilot's read-only tier is explicit (deny rules beat allow rules).
    if runtime == ModelProviderKind::Copilot {
        out.splice(0..0, crate::provider::copilot::permission_args("default"));
    }
    out
}

/// Apply a gate plan to a runtime's args. `Err` carries the refusal reason.
pub fn apply(plan: &GatePlan, runtime: ModelProviderKind, args: &mut Vec<String>) -> Result<(), String> {
    match plan {
        GatePlan::Full => Ok(()),
        GatePlan::ReadOnly => {
            *args = read_only_args(runtime, args);
            Ok(())
        }
        GatePlan::Refuse(reason) => Err(reason.clone()),
    }
}

/// Whether a turn with these args can write: a CLI that always edits files,
/// or a tier flag in `args` that the read-only tier drops.
pub fn turn_writes(runtime: ModelProviderKind, args: &[String]) -> bool {
    always_writes(runtime) || read_only_args(runtime, args) != args
}

/// The gate as every host applies it (desktop, `ligus worker`/`serve`,
/// `ligus agent`): the floor follows the turn's tier ([`turn_writes`]),
/// `args` are narrowed to the read-only tier below it, and the plan is
/// returned (a `Refuse` leaves `args` untouched; the caller doesn't run).
pub fn gate_turn(runtime: ModelProviderKind, args: &mut Vec<String>, author_role: Option<WorkspaceRole>) -> GatePlan {
    let floor = min_role(turn_writes(runtime, args));
    let plan = gate(runtime, author_role, floor);
    let _ = apply(&plan, runtime, args);
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use WorkspaceRole::*;

    #[test]
    fn default_floor_follows_the_tier() {
        assert_eq!(default_min_role(true), Admin);
        assert_eq!(default_min_role(false), Contributor);
        assert_eq!(min_role_from(None, true), Admin);
        assert_eq!(min_role_from(Some("bogus"), true), Admin);
        assert_eq!(min_role_from(Some("viewer"), true), Viewer);
        assert!(lowered_floor_warning(Some("contributor"), true).is_some());
        assert!(lowered_floor_warning(Some("contributor"), false).is_none());
        assert!(lowered_floor_warning(Some("owner"), true).is_none());
        assert!(lowered_floor_warning(None, true).is_none());
    }

    #[test]
    fn below_floor_is_read_only_or_refused() {
        for p in [ModelProviderKind::ClaudeCode, ModelProviderKind::Antigravity, ModelProviderKind::Copilot, ModelProviderKind::Ollama, ModelProviderKind::Anthropic] {
            assert_eq!(gate(p, Some(Contributor), Admin), GatePlan::ReadOnly, "{p:?}");
            assert_eq!(gate(p, None, Viewer), GatePlan::ReadOnly, "unresolved author fails closed: {p:?}");
            assert_eq!(gate(p, Some(Admin), Admin), GatePlan::Full);
        }
        for p in [ModelProviderKind::Codex, ModelProviderKind::Aider, ModelProviderKind::Pi, ModelProviderKind::Hermes] {
            assert!(matches!(gate(p, Some(Viewer), Contributor), GatePlan::Refuse(_)), "{p:?}");
            assert_eq!(gate(p, Some(Owner), Admin), GatePlan::Full);
        }
    }

    #[test]
    fn read_only_args_drop_every_tier_flag() {
        let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            read_only_args(ModelProviderKind::ClaudeCode, &a(&["--permission-mode", "acceptEdits", "--allowedTools", "Bash", "--model", "opus", "--dangerously-skip-permissions"])),
            a(&["--model", "opus"])
        );
        assert_eq!(read_only_args(ModelProviderKind::Antigravity, &a(&["--mode", "auto", "--model", "x"])), a(&["--model", "x"]));
        assert_eq!(read_only_args(ModelProviderKind::Antigravity, &a(&["--dangerously-skip-permissions"])), a(&[]));
        assert_eq!(
            read_only_args(ModelProviderKind::Copilot, &a(&["--allow-tool=write", "--allow-tool", "shell", "--allow-all-tools", "--model", "m"])),
            a(&["--allow-tool=read", "--deny-tool=write", "--deny-tool=shell", "--model", "m"])
        );
    }

    /// H1 for CLI runtimes, as every host applies it ([`gate_turn`] is the
    /// one call the desktop, the worker/serve and `ligus agent` make): a
    /// Contributor's message can't drive a write-tier claude (it runs
    /// read-only) or codex (refused) on any host; an Admin's can.
    #[test]
    fn author_gate_applies_to_cli_runtimes_on_desktop_worker_agent() {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // desktop "runCommands" tier, worker LIGUS_WORKER_PERMISSION_MODE=bypassPermissions, agent default
        for host_args in [v(&["--permission-mode", "acceptEdits", "--allowedTools", "Bash"]), v(&["--permission-mode", "bypassPermissions"])] {
            let mut args = host_args.clone();
            assert_eq!(gate_turn(ModelProviderKind::ClaudeCode, &mut args, Some(Contributor)), GatePlan::ReadOnly);
            assert!(!turn_writes(ModelProviderKind::ClaudeCode, &args), "{args:?}");
            let mut args = host_args.clone();
            assert_eq!(gate_turn(ModelProviderKind::ClaudeCode, &mut args, Some(Admin)), GatePlan::Full);
            assert_eq!(args, host_args);
            let mut args = host_args.clone();
            assert_eq!(gate_turn(ModelProviderKind::ClaudeCode, &mut args, None), GatePlan::ReadOnly);
        }
        let mut args = Vec::new();
        assert!(matches!(gate_turn(ModelProviderKind::Codex, &mut args, Some(Contributor)), GatePlan::Refuse(_)));
        assert_eq!(gate_turn(ModelProviderKind::Codex, &mut args, Some(Admin)), GatePlan::Full);
        // A read-only claude turn: a Contributor still gets it.
        let mut args = v(&["--model", "opus"]);
        assert_eq!(gate_turn(ModelProviderKind::ClaudeCode, &mut args, Some(Contributor)), GatePlan::Full);
        assert_eq!(gate_turn(ModelProviderKind::ClaudeCode, &mut args, Some(Viewer)), GatePlan::ReadOnly);
    }
}

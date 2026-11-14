//! The TurnExecutor seam: every child Ligus starts for an agent turn (CLI
//! agents, stdio MCP servers, `run_command`, pre-flight tests) is spawned
//! through here, confined by the OS.
//!
//! 1. [`EnvelopeEnforcer::plan`] turns the turn's inputs (the workspace's
//!    [`SandboxEnvelope`], the runtime, the author gate's [`GatePlan`], the
//!    turn's directories and this host's paths) into an [`EnforcementPlan`]:
//!    what the child may write and read, where it may connect, and its exact
//!    environment. It is a pure function, so the desktop, `ligus worker`,
//!    `ligus serve` and `ligus agent` get the same plan for the same turn.
//! 2. A [`TurnExecutor`] confines a command to the plan: Seatbelt
//!    (`sandbox-exec`) on macOS; on Linux, bubblewrap + Landlock + seccomp,
//!    or (where bubblewrap can't create namespaces) Landlock + seccomp
//!    alone ([`ExecutorKind::Landlock`]); or [`Unconfined`] where none is
//!    available (then write-capable turns are refused; see
//!    [`refuses_writes`]). [`sandbox_status`] says which, and why.
//! 3. [`prepare`] is what spawn sites call: it resolves the plan from the
//!    turn scope ([`with_turn`]) or the safe default, starts the per-turn
//!    egress proxy ([`crate::egress`]), creates the turn's scratch dir, and
//!    returns the confined command plus a guard that tears it all down.
//!
//! The sandbox is one control among several (authorization, the author gate,
//! the MCP role gate, the human gate); none of them relies on it alone.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use hive_core::sandbox::{EgressPolicy, SandboxEnvelope};
use hive_core::ModelProviderKind;

use crate::author_gate::GatePlan;
use crate::egress::{EgressPreset, EgressProxy, HostPort};

pub mod cli_home;
#[cfg(unix)]
pub mod landlock_rules;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod seatbelt;

/// Which mechanism confines a turn. On Linux the tiers rank
/// `Bwrap` (namespaces + Landlock + seccomp) > `Landlock` (Landlock +
/// seccomp, no namespaces) > `Unconfined` (write-capable turns refused).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutorKind {
    Unconfined,
    Seatbelt,
    Bwrap,
    /// Linux without usable user namespaces: Landlock (filesystem, TCP
    /// ports, scoping) and seccomp, no namespaces.
    Landlock,
    Gvisor,
    AppleContainer,
    K8sJob,
}

/// Where a child may connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressPlan {
    /// Only through the per-turn proxy, to these endpoints.
    Proxy { allow: Vec<HostPort> },
    /// No network at all.
    Closed,
    /// The OS doesn't restrict the network. Only for a stdio MCP server the
    /// user enabled on this device (it is the user's own tool, filtered by
    /// the envelope before it is ever spawned); never for an agent.
    Open,
}

/// What a child spawned for a turn is allowed. A pure function of the turn's
/// inputs ([`EnvelopeEnforcer::plan`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnforcementPlan {
    /// The turn's directory (its worktree, or a scratch dir).
    pub worktree: PathBuf,
    /// Whether the child may write in `worktree` (protected dirs excepted).
    pub worktree_writable: bool,
    /// Paths inside `worktree` that stay read-only (`.git`, the envelope's
    /// protected dirs, protected root files).
    pub read_only: Vec<PathBuf>,
    /// Paths the child may not read (Ligus's data dir, `serve.token`, key
    /// files, `~/.ssh`, keychains …).
    pub deny_read: Vec<PathBuf>,
    /// Paths readable even inside a denied one (a repository checked out
    /// under the data dir).
    pub allow_read: Vec<PathBuf>,
    /// Extra writable paths: the runtime's own state dirs and the turn's
    /// scratch dir.
    pub write_extra: Vec<PathBuf>,
    /// The turn's scratch dir (`TMPDIR`).
    pub tmp: PathBuf,
    pub egress: EgressPlan,
    /// The child's whole environment, before the proxy variables (which are
    /// added once the proxy is running).
    pub env: Vec<(OsString, OsString)>,
    /// Extra CLI args the envelope adds for this runtime.
    pub args: Vec<String>,
    /// MCP servers the turn may reach (ids). The Ligus server (`ligus`) is
    /// always among them when the turn has one: it is local and needs no
    /// network, so it holds its slot under every egress policy.
    pub mcp: Vec<String>,
    /// The turn's Ligus MCP unix socket: the one extra path an agent child
    /// may connect to (D.3.2).
    pub ligus_mcp_socket: Option<PathBuf>,
}

/// The MCP server id the Ligus server holds in a plan's MCP list.
pub const LIGUS_MCP_ID: &str = "ligus";

/// What kind of child is being spawned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// A CLI agent (claude, codex, agy, copilot, aider, pi, hermes).
    Agent(ModelProviderKind),
    /// A stdio MCP server.
    McpServer,
    /// Ligus's own `run_command` tool.
    Tool,
    /// A proposal pre-flight test.
    Preflight,
}

/// This host's paths that no agent child may read, plus device-local
/// widenings. Set once at startup by each host ([`configure_host`]).
#[derive(Debug, Clone, Default)]
pub struct HostPaths {
    /// Ligus data dirs (the desktop's app data dir, `~/.ligus`, a serve or
    /// worker `LIGUS_DATA_DIR`).
    pub data_dirs: Vec<PathBuf>,
    /// Single secret files outside those dirs (a `--token-file`).
    pub secret_files: Vec<PathBuf>,
    /// Readable inside a denied dir (a worker's checked-out repos dir).
    pub allow_read: Vec<PathBuf>,
    /// Device-local extra endpoints for agents under an open envelope
    /// (`LIGUS_SANDBOX_EGRESS_ALLOW`, comma-separated `host[:port]`).
    pub extra_egress: Vec<HostPort>,
    /// Device-local egress presets on for every turn on this host
    /// (`LIGUS_SANDBOX_EGRESS_PRESETS`, comma-separated names such as
    /// `package-registries`). Same rule as `extra_egress`: only under an
    /// open envelope.
    pub egress_presets: Vec<EgressPreset>,
    /// Device-local extra writable paths (`LIGUS_SANDBOX_WRITE_ALLOW`).
    pub extra_write: Vec<PathBuf>,
    /// Paths under `~` the user lets agents read (`LIGUS_SANDBOX_READ_ALLOW`,
    /// e.g. `~/.aws` for a runtime that needs it).
    pub home_read_allow: Vec<PathBuf>,
    /// The binary that carries the Linux pre-exec helper (default: this
    /// executable).
    pub helper_exe: Option<PathBuf>,
}

fn host_slot() -> &'static RwLock<HostPaths> {
    static H: OnceLock<RwLock<HostPaths>> = OnceLock::new();
    H.get_or_init(|| RwLock::new(HostPaths::from_env()))
}

impl HostPaths {
    /// The defaults every host has: `~/.ligus` (and the legacy `~/.hive`), a
    /// `LIGUS_DATA_DIR`, and the device-local widenings from the env.
    pub fn from_env() -> Self {
        let mut h = HostPaths::default();
        if let Some(home) = home_dir() {
            h.data_dirs.push(home.join(hive_core::compat::HOME_DATA_DIR));
            h.data_dirs.push(home.join(hive_core::compat::LEGACY_HOME_DATA_DIR));
            #[cfg(target_os = "macos")]
            for id in ["ai.ligus.desktop", "ai.hive.desktop", "com.hive.desktop"] {
                h.data_dirs.push(home.join("Library/Application Support").join(id));
            }
            #[cfg(target_os = "linux")]
            for id in ["ai.ligus.desktop", "ai.hive.desktop"] {
                h.data_dirs.push(home.join(".local/share").join(id));
                h.data_dirs.push(home.join(".config").join(id));
            }
        }
        if let Ok(d) = hive_core::compat::var("LIGUS_DATA_DIR") {
            if !d.trim().is_empty() {
                h.data_dirs.push(PathBuf::from(d.trim()));
            }
        }
        let list = |name: &str| -> Vec<String> {
            hive_core::compat::var(name)
                .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
                .unwrap_or_default()
        };
        h.extra_egress = list("LIGUS_SANDBOX_EGRESS_ALLOW").iter().filter_map(|s| HostPort::parse(s)).collect();
        h.egress_presets = EgressPreset::parse_list(list("LIGUS_SANDBOX_EGRESS_PRESETS").iter().map(String::as_str));
        h.extra_write = list("LIGUS_SANDBOX_WRITE_ALLOW").into_iter().map(|p| expand_home(&p)).collect();
        h.home_read_allow = list("LIGUS_SANDBOX_READ_ALLOW").into_iter().map(|p| expand_home(&p)).collect();
        h
    }
}

fn expand_home(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), home_dir()) {
        (Some(rest), Some(h)) => h.join(rest),
        _ => PathBuf::from(p),
    }
}

/// Add this host's own data dir and secret files to the deny list (and
/// paths readable inside it). Each host calls it at startup.
pub fn configure_host(data_dir: &Path, secret_files: &[PathBuf], allow_read: &[PathBuf]) {
    let mut h = host_slot().write().unwrap_or_else(|e| e.into_inner());
    if !h.data_dirs.iter().any(|d| d == data_dir) {
        h.data_dirs.push(data_dir.to_path_buf());
    }
    for f in secret_files {
        if !h.secret_files.contains(f) {
            h.secret_files.push(f.clone());
        }
    }
    for a in allow_read {
        if !h.allow_read.contains(a) {
            h.allow_read.push(a.clone());
        }
    }
}

/// Use `exe` as the Linux pre-exec helper (tests; a host whose own binary
/// doesn't carry it).
pub fn set_helper_exe(exe: PathBuf) {
    host_slot().write().unwrap_or_else(|e| e.into_inner()).helper_exe = Some(exe);
}

pub fn host_paths() -> HostPaths {
    host_slot().read().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).filter(|p| !p.as_os_str().is_empty())
}

// ---------------------------------------------------------------------------
// The plan
// ---------------------------------------------------------------------------

/// The host asking for a plan. It must not change the plan
/// (`prop_enforcement_plan_host_independent`); it exists so the property is
/// stated, and for logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKind {
    Desktop,
    Worker,
    Serve,
    Agent,
}

/// Everything a plan is computed from.
#[derive(Debug, Clone)]
pub struct PlanInputs<'a> {
    pub host: HostKind,
    pub envelope: &'a SandboxEnvelope,
    pub purpose: Purpose,
    pub gate: &'a GatePlan,
    /// The turn's directory, and whether it is an isolated worktree the turn
    /// may write (a read-only turn's directory is never writable).
    pub worktree: &'a Path,
    pub worktree_is_isolated: bool,
    /// Whether the turn's runtime/tier can write at all (a CLI at a raised
    /// tier or one that always edits; any tool or pre-flight run).
    pub write_capable: bool,
    pub tmp: &'a Path,
    pub home: &'a Path,
    pub host_paths: &'a HostPaths,
    /// Hosts of the MCP servers this chat may reach (already filtered by the
    /// envelope).
    pub mcp_hosts: &'a [HostPort],
    pub mcp_ids: &'a [String],
    /// A configured model base URL (an API runtime behind `pi`, a gateway).
    pub model_base_url: Option<&'a str>,
    /// Caller env (git attribution, the pi config dir).
    pub extra_env: &'a [(String, String)],
    /// The runtime's Ligus-configured key, if any.
    pub provider_key: Option<&'a str>,
    /// The runtime's `model_provider_id` (which key variable aider/pi read).
    pub model_provider_id: Option<&'a str>,
    /// Extra args from the envelope for this runtime (`sandbox::enforce`).
    pub envelope_args: &'a [String],
    /// The turn's Ligus MCP socket, if the turn offers the Ligus server.
    pub ligus_mcp_socket: Option<&'a Path>,
    /// Egress presets this chat opted into (workspace or chat setting), on
    /// top of the host's own (`HostPaths::egress_presets`). Ignored unless
    /// the envelope's egress is open.
    pub egress_presets: &'a [EgressPreset],
    /// A CLI agent's per-turn home (C-9): its real state dirs are then not
    /// writable, and `HOME` points here.
    pub cli_home: Option<&'a Path>,
}

/// Model endpoints each subscription CLI must reach (its API, its sign-in
/// refresh).
pub fn provider_endpoints(kind: ModelProviderKind) -> Vec<HostPort> {
    let hosts: &[&str] = match kind {
        ModelProviderKind::ClaudeCode => &["api.anthropic.com", "console.anthropic.com", "claude.ai", "statsig.anthropic.com"],
        ModelProviderKind::Codex => &["api.openai.com", "chatgpt.com", "auth.openai.com"],
        ModelProviderKind::Copilot => &["api.githubcopilot.com", "api.individual.githubcopilot.com", "github.com", "api.github.com"],
        ModelProviderKind::Antigravity => &[
            "generativelanguage.googleapis.com",
            "cloudcode-pa.googleapis.com",
            "oauth2.googleapis.com",
            "www.googleapis.com",
            "accounts.google.com",
        ],
        ModelProviderKind::Anthropic => &["api.anthropic.com"],
        ModelProviderKind::OpenAI => &["api.openai.com"],
        ModelProviderKind::OpenRouter => &["openrouter.ai"],
        _ => &[],
    };
    hosts.iter().map(|h| HostPort::new(*h, 443)).collect()
}

/// State dirs a runtime writes (its login, session cache, config).
pub fn runtime_state_dirs(kind: ModelProviderKind, home: &Path) -> Vec<PathBuf> {
    let rel: &[&str] = match kind {
        ModelProviderKind::ClaudeCode => &[".claude", ".claude.json", ".claude.json.backup", ".cache/claude-cli-nodejs", "Library/Caches/claude-cli-nodejs"],
        ModelProviderKind::Codex => &[".codex"],
        ModelProviderKind::Copilot => &[".copilot", ".config/github-copilot", ".cache/github-copilot"],
        ModelProviderKind::Antigravity => &[".gemini", ".antigravity", ".config/agy"],
        ModelProviderKind::Aider => &[".aider", ".aider.chat.history.md", ".cache/aider"],
        ModelProviderKind::Pi => &[".pi"],
        ModelProviderKind::Hermes => &[".hermes"],
        _ => &[],
    };
    let mut v: Vec<PathBuf> = rel.iter().map(|r| home.join(r)).collect();
    let env_dir = |k: &str| std::env::var_os(k).map(PathBuf::from);
    match kind {
        ModelProviderKind::ClaudeCode => v.extend(env_dir("CLAUDE_CONFIG_DIR")),
        ModelProviderKind::Codex => v.extend(env_dir("CODEX_HOME")),
        ModelProviderKind::Copilot => v.extend(env_dir("COPILOT_HOME")),
        _ => {}
    }
    v
}

/// Secrets under `~` that agents never read unless the user allows them.
///
/// The keychain files are denied too, except to a subscription CLI whose
/// login lives in the login keychain (Claude Code, Codex, Copilot,
/// Antigravity): hiding them makes the login keychain vanish for that CLI
/// (checked with `security list-keychains` under the profile). Ligus's own
/// keychain items are protected by their item ACLs, not by the profile.
pub fn home_secret_dirs(home: &Path, purpose: Purpose) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = [".ssh", ".gnupg", ".aws", ".config/gcloud", ".kube", ".docker/config.json", ".netrc", ".git-credentials"]
        .iter()
        .map(|r| home.join(r))
        .collect();
    let keychain_login = matches!(
        purpose,
        Purpose::Agent(ModelProviderKind::ClaudeCode | ModelProviderKind::Codex | ModelProviderKind::Copilot | ModelProviderKind::Antigravity)
    );
    // Forge CLI logins: gh keeps a write-capable token in hosts.yml on a box
    // with no secret service, glab, tea and hub likewise. The forge tools
    // reach the code host for agents; the Copilot CLI may sign in through
    // the gh login, so it keeps that one.
    if !matches!(purpose, Purpose::Agent(ModelProviderKind::Copilot)) {
        v.push(home.join(".config/gh"));
    }
    v.extend([".config/glab-cli", ".config/tea", ".config/hub"].iter().map(|r| home.join(r)));
    #[cfg(target_os = "macos")]
    if !keychain_login {
        v.push(home.join("Library/Keychains"));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = keychain_login;
    #[cfg(target_os = "linux")]
    v.push(home.join(".local/share/keyrings"));
    v
}

/// The env var a CLI runtime reads its configured key from.
pub fn provider_key_var(kind: ModelProviderKind, model_provider_id: Option<&str>) -> Option<&'static str> {
    match kind {
        ModelProviderKind::ClaudeCode => Some("ANTHROPIC_API_KEY"),
        ModelProviderKind::Codex => Some("OPENAI_API_KEY"),
        ModelProviderKind::Aider | ModelProviderKind::Pi | ModelProviderKind::Hermes => {
            Some(match model_provider_id.map(str::to_ascii_lowercase).as_deref() {
                Some("anthropic") => "ANTHROPIC_API_KEY",
                Some("openrouter") => "OPENROUTER_API_KEY",
                _ => "OPENAI_API_KEY",
            })
        }
        _ => None,
    }
}

/// Builds the plan. Every field of the envelope is consumed here (an
/// exhaustive destructure: a new envelope field fails to compile until it is
/// handled; `prop_every_envelope_field_consumed`).
pub struct EnvelopeEnforcer;

impl EnvelopeEnforcer {
    pub fn plan(i: &PlanInputs<'_>) -> EnforcementPlan {
        let SandboxEnvelope { isolate_writes, protected_dirs, egress, redact_secrets: _, human_gate: _, tightened_by: _ } = i.envelope;
        // `redact_secrets` and `human_gate` act on the turn's output and its
        // proposals (chat_service), not on the process; `tightened_by` only
        // explains the rest. They are named here so a new field can't slip
        // past the enforcer.
        let host_paths = i.host_paths;
        let _ = i.host;

        // Writes: the worktree only when the gate allows writes and (under a
        // tightened envelope) it is an isolated worktree; the turn's scratch
        // dir; the runtime's state dirs; device-local extras.
        let worktree_writable = i.gate.allows_writes()
            && i.write_capable
            && !matches!(i.purpose, Purpose::McpServer)
            && (i.worktree_is_isolated || !*isolate_writes);
        let mut read_only: Vec<PathBuf> = protected_dirs.iter().map(|d| i.worktree.join(d)).collect();
        for f in ["ligus.config.toml", "hive.config.toml", ".envrc"] {
            read_only.push(i.worktree.join(f));
        }
        if !read_only.contains(&i.worktree.join(".git")) {
            read_only.push(i.worktree.join(".git"));
        }
        let mut write_extra = vec![i.tmp.to_path_buf()];
        if let Purpose::Agent(k) = i.purpose {
            // With a per-turn home (inside `tmp`) the CLI's real config is
            // read-only to it: nothing it writes there outlives the turn.
            if i.cli_home.is_none() {
                write_extra.extend(runtime_state_dirs(k, i.home));
            }
        }
        write_extra.extend(host_paths.extra_write.iter().cloned());

        // Reads: never Ligus's data, this host's secret files, or the
        // user's credential dirs (unless allowed device-locally).
        let mut deny_read: Vec<PathBuf> = host_paths.data_dirs.clone();
        deny_read.extend(host_paths.secret_files.iter().cloned());
        deny_read.extend(home_secret_dirs(i.home, i.purpose).into_iter().filter(|p| !host_paths.home_read_allow.contains(p)));
        let mut allow_read = host_paths.allow_read.clone();
        allow_read.push(i.worktree.to_path_buf());
        // Never write where reading is denied.
        write_extra.retain(|w| !deny_read.iter().any(|d| w.starts_with(d)));

        // Egress: provider endpoints plus the chat's MCP hosts; lockdown
        // keeps only the provider (the model must be reachable for the turn
        // to run at all; MCP and everything else are cut).
        let model_hosts: Vec<HostPort> = match i.purpose {
            Purpose::Agent(k) => {
                let mut v = provider_endpoints(k);
                v.extend(i.model_base_url.and_then(HostPort::from_url));
                v
            }
            _ => Vec::new(),
        };
        // Opt-in widenings (device-local hosts and presets): only under an
        // open envelope, so dearth mode and a lockdown always win over them.
        // A runtime-specific preset (claude-connectors) widens only that
        // runtime's agent children.
        let agent_kind = match i.purpose {
            Purpose::Agent(k) => Some(k),
            _ => None,
        };
        let mut widen: Vec<HostPort> = host_paths.extra_egress.clone();
        let mut presets: Vec<EgressPreset> = host_paths
            .egress_presets
            .iter()
            .chain(i.egress_presets)
            .copied()
            .filter(|p| p.only_for().is_none_or(|k| Some(k) == agent_kind))
            .collect();
        presets.sort();
        presets.dedup();
        widen.extend(presets.into_iter().flat_map(EgressPreset::hosts));
        widen.sort();
        widen.dedup();
        let egress_plan = match (i.purpose, egress) {
            (Purpose::McpServer, EgressPolicy::Closed) => EgressPlan::Closed,
            (Purpose::McpServer, _) => EgressPlan::Open,
            (Purpose::Tool | Purpose::Preflight, EgressPolicy::Open) if !widen.is_empty() => EgressPlan::Proxy { allow: widen },
            (Purpose::Tool | Purpose::Preflight, _) => EgressPlan::Closed,
            (Purpose::Agent(_), policy) => {
                let mut allow = model_hosts;
                match policy {
                    EgressPolicy::Open => {
                        allow.extend(i.mcp_hosts.iter().cloned());
                        allow.extend(widen);
                    }
                    EgressPolicy::AllowList { .. } => allow.extend(i.mcp_hosts.iter().cloned()),
                    EgressPolicy::Closed => {}
                }
                allow.sort();
                allow.dedup();
                if allow.is_empty() {
                    EgressPlan::Closed
                } else {
                    EgressPlan::Proxy { allow }
                }
            }
        };

        let provider_key = match i.purpose {
            Purpose::Agent(k) => i.provider_key.filter(|k| !k.is_empty()).and_then(|key| {
                provider_key_var(k, i.model_provider_id).map(|var| (var.to_string(), key.to_string()))
            }),
            _ => None,
        };
        let mut env = hive_core::process_util::child_env(&hive_core::process_util::ChildEnvSpec {
            tmpdir: Some(i.tmp.to_path_buf()),
            proxy: Vec::new(),
            extra: i.extra_env.to_vec(),
            provider_key,
        });
        if let (Purpose::Agent(k), Some(home)) = (i.purpose, i.cli_home) {
            for (key, val) in cli_home::env(k, home) {
                env.retain(|(x, _)| *x != key);
                env.push((key, val));
            }
            env.sort();
        }
        // Claude Code mkdirs its own scratch under `/tmp/claude-<uid>`, which
        // the sandbox doesn't let it write; point it at the turn's scratch dir.
        if let Purpose::Agent(ModelProviderKind::ClaudeCode) = i.purpose {
            env.retain(|(k, _)| k != "CLAUDE_CODE_TMPDIR");
            env.push(("CLAUDE_CODE_TMPDIR".into(), i.tmp.as_os_str().to_os_string()));
            env.sort();
        }
        let mut mcp = match egress {
            EgressPolicy::Closed => Vec::new(),
            _ => i.mcp_ids.to_vec(),
        };
        // The Ligus server is local (a unix socket): present under every
        // egress policy, including `Closed`, and always first.
        let ligus_mcp_socket = match i.purpose {
            Purpose::Agent(_) => i.ligus_mcp_socket.map(Path::to_path_buf),
            _ => None,
        };
        if ligus_mcp_socket.is_some() {
            mcp.retain(|m| m != LIGUS_MCP_ID);
            mcp.insert(0, LIGUS_MCP_ID.to_string());
        }
        EnforcementPlan {
            worktree: i.worktree.to_path_buf(),
            worktree_writable,
            read_only,
            deny_read,
            allow_read,
            write_extra,
            tmp: i.tmp.to_path_buf(),
            egress: egress_plan,
            env,
            args: i.envelope_args.to_vec(),
            mcp,
            ligus_mcp_socket,
        }
    }
}

// ---------------------------------------------------------------------------
// Executors
// ---------------------------------------------------------------------------

/// Confines a command to a plan. Implementations return the command to run
/// (possibly a wrapper such as `sandbox-exec … -- program`), with the plan's
/// environment and cwd set; the caller adds stdio and spawns it.
///
/// Deviation from the spec's `spawn(Command, &plan) -> Child`: callers keep
/// control of stdio and of sync vs async spawning (pre-flight is
/// synchronous), which a pre-built `Command` can't hand over.
pub trait TurnExecutor: Send + Sync {
    fn kind(&self) -> ExecutorKind;
    /// `proxy` is the running egress proxy (port, token, unix socket) when
    /// the plan's egress is `Proxy`.
    fn confine(&self, program: &OsStr, args: &[OsString], cwd: &Path, plan: &EnforcementPlan, proxy: Option<&EgressProxy>) -> io::Result<std::process::Command>;
}

/// No OS confinement: env allow-list and cwd checks only. Write-capable
/// tiers are refused under it ([`refuses_writes`]).
pub struct Unconfined;

impl TurnExecutor for Unconfined {
    fn kind(&self) -> ExecutorKind {
        ExecutorKind::Unconfined
    }
    fn confine(&self, program: &OsStr, args: &[OsString], cwd: &Path, plan: &EnforcementPlan, proxy: Option<&EgressProxy>) -> io::Result<std::process::Command> {
        let mut c = hive_core::process_util::command(program);
        c.args(args).current_dir(cwd).env_clear().envs(plan_env(plan, proxy));
        new_session(&mut c);
        Ok(c)
    }
}

/// Start the child in a session of its own (`setsid`), so it has no
/// controlling terminal: otherwise it could open `/dev/tty` and inject
/// keystrokes (`TIOCSTI`) into the user's shell, which would run them
/// outside any sandbox. bubblewrap does the same with `--new-session`.
pub fn new_session(c: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: `setsid` is async-signal-safe and touches no Rust state.
        unsafe {
            c.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    let _ = c;
}

/// The plan's env plus the proxy variables.
pub fn plan_env(plan: &EnforcementPlan, proxy: Option<&EgressProxy>) -> Vec<(OsString, OsString)> {
    let mut env = plan.env.clone();
    if let Some(p) = proxy {
        for (k, v) in crate::egress::proxy_env(&p.url()) {
            env.retain(|(x, _)| x != k.as_str());
            env.push((k.into(), v.into()));
        }
    }
    env
}

/// The executor for this host, probed once. `LIGUS_SANDBOX=off` forces
/// [`Unconfined`] (write tiers are then refused); on Linux,
/// `LIGUS_SANDBOX=landlock` skips bubblewrap and `LIGUS_SANDBOX=bwrap`
/// refuses the Landlock fallback.
pub fn default_executor() -> Arc<dyn TurnExecutor> {
    static E: OnceLock<Arc<dyn TurnExecutor>> = OnceLock::new();
    E.get_or_init(|| {
        let (e, status) = select_executor();
        status.log();
        let _ = STATUS.set(status);
        e
    })
    .clone()
}

fn select_executor() -> (Arc<dyn TurnExecutor>, SandboxStatus) {
    if hive_core::compat::var("LIGUS_SANDBOX").is_ok_and(|v| v.trim().eq_ignore_ascii_case("off")) {
        return (Arc::new(Unconfined), SandboxStatus::unconfined("LIGUS_SANDBOX=off".into(), Vec::new()));
    }
    #[cfg(target_os = "macos")]
    if seatbelt::available() {
        let s = SandboxStatus { kind: ExecutorKind::Seatbelt, summary: "Seatbelt (sandbox-exec)".into(), notes: Vec::new() };
        return (Arc::new(seatbelt::Seatbelt), s);
    }
    #[cfg(target_os = "linux")]
    let chosen = linux::select_executor();
    #[cfg(not(target_os = "linux"))]
    let chosen: (Arc<dyn TurnExecutor>, SandboxStatus) =
        (Arc::new(Unconfined), SandboxStatus::unconfined("this platform has no agent sandbox Ligus can use".into(), Vec::new()));
    chosen
}

/// Whether the host's executor refuses write-capable turns (it can't confine
/// them). The UI shows a persistent banner while this is true.
pub fn refuses_writes() -> bool {
    default_executor().kind() == ExecutorKind::Unconfined
}

/// Whether CLI agents run inside Ligus's own OS sandbox. A CLI that would
/// nest its own Seatbelt/bwrap sandbox (Codex) is told to use Ligus's
/// instead, because nesting fails.
pub fn os_confined() -> bool {
    !refuses_writes()
}

// ---------------------------------------------------------------------------
// Turn scope and spawning
// ---------------------------------------------------------------------------

/// The per-turn policy a host sets around a turn's dispatch.
#[derive(Debug, Clone)]
pub struct TurnPolicy {
    pub host: HostKind,
    pub envelope: SandboxEnvelope,
    pub gate: GatePlan,
    /// Whether the turn's directory is an isolated worktree.
    pub worktree_is_isolated: bool,
    /// The MCP servers this chat may reach, by id and endpoint.
    pub mcp_ids: Vec<String>,
    pub mcp_hosts: Vec<HostPort>,
    /// The turn's Ligus MCP socket (D.3.2), allowed through the sandbox.
    pub ligus_mcp_socket: Option<PathBuf>,
    /// Egress presets this chat opted into (its own setting, else the
    /// workspace's). Applied only under an open envelope.
    pub egress_presets: Vec<EgressPreset>,
    /// Endpoints the turn's egress proxies refused (host and port only),
    /// collected for the host to record as a content-free decision.
    pub denials: Arc<std::sync::Mutex<Vec<HostPort>>>,
}

impl TurnPolicy {
    pub fn new(host: HostKind, envelope: SandboxEnvelope, gate: GatePlan) -> Self {
        Self { envelope, gate, ..Self::baseline(host) }
    }

    /// Endpoints refused so far in this turn.
    pub fn egress_denials(&self) -> Vec<HostPort> {
        self.denials.lock().map(|d| d.clone()).unwrap_or_default()
    }

    /// The policy when a spawn site has none in scope: the workspace's
    /// default envelope, full gate, no MCP.
    pub fn baseline(host: HostKind) -> Self {
        Self {
            host,
            envelope: SandboxEnvelope::workspace_default(),
            gate: GatePlan::Full,
            worktree_is_isolated: false,
            mcp_ids: Vec::new(),
            mcp_hosts: Vec::new(),
            ligus_mcp_socket: None,
            egress_presets: Vec::new(),
            denials: Default::default(),
        }
    }
}

/// The runtime a turn's CLI child belongs to: its Ligus-configured key and
/// model endpoint (set by `dispatch` around a turn).
#[derive(Debug, Clone, Default)]
pub struct RuntimeScope {
    pub provider_key: Option<String>,
    pub model_provider_id: Option<String>,
    pub model_base_url: Option<String>,
}

tokio::task_local! {
    static TURN: Arc<TurnPolicy>;
    static RUNTIME: Arc<RuntimeScope>;
}

/// Run `fut` with `rt` as the runtime scope for CLI children spawned in it.
pub async fn with_runtime<F: std::future::Future>(rt: RuntimeScope, fut: F) -> F::Output {
    RUNTIME.scope(Arc::new(rt), fut).await
}

fn current_runtime() -> Option<Arc<RuntimeScope>> {
    RUNTIME.try_with(Arc::clone).ok()
}

/// Run `fut` with `policy` as the turn scope for every spawn inside it.
pub async fn with_turn<F: std::future::Future>(policy: TurnPolicy, fut: F) -> F::Output {
    TURN.scope(Arc::new(policy), fut).await
}

/// Run `f` (synchronous) with `policy` as the turn scope.
pub fn with_turn_sync<R>(policy: TurnPolicy, f: impl FnOnce() -> R) -> R {
    TURN.sync_scope(Arc::new(policy), f)
}

/// The current turn scope, if any.
pub fn current_turn() -> Option<Arc<TurnPolicy>> {
    TURN.try_with(Arc::clone).ok()
}

/// What a spawn site asks for.
#[derive(Debug, Clone)]
pub struct SpawnSpec<'a> {
    pub purpose: Purpose,
    pub program: &'a OsStr,
    pub args: Vec<OsString>,
    /// The child's cwd. `None` = the turn's fresh scratch dir.
    pub cwd: Option<&'a Path>,
    pub extra_env: &'a [(String, String)],
    pub provider_key: Option<&'a str>,
    pub model_provider_id: Option<&'a str>,
    pub model_base_url: Option<&'a str>,
    /// Whether the child may write, when its args don't say (an ACP agent,
    /// whose tier is Ligus's permission setting rather than CLI flags).
    /// `None` = derived from the args.
    pub writes: Option<bool>,
}

impl<'a> SpawnSpec<'a> {
    pub fn new(purpose: Purpose, program: &'a (impl AsRef<OsStr> + ?Sized), args: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        Self {
            purpose,
            program: program.as_ref(),
            args: args.into_iter().map(Into::into).collect(),
            cwd: None,
            extra_env: &[],
            provider_key: None,
            model_provider_id: None,
            model_base_url: None,
            writes: None,
        }
    }
    pub fn cwd(mut self, cwd: Option<&'a Path>) -> Self {
        self.cwd = cwd;
        self
    }
    pub fn env(mut self, extra: &'a [(String, String)]) -> Self {
        self.extra_env = extra;
        self
    }
}

/// Keeps a confined child's turn resources alive: the egress proxy, the
/// scratch dir, the generated profile. Hold it until the child exits.
pub struct TurnGuard {
    /// A CLI agent's per-turn home: (kind, real home, turn home).
    cli_home: Option<(ModelProviderKind, PathBuf, PathBuf)>,
    proxy: Option<EgressProxy>,
    denials: Option<Arc<std::sync::Mutex<Vec<HostPort>>>>,
    tmp: Option<TurnTmp>,
    kind: ExecutorKind,
    plan: EnforcementPlan,
}

impl TurnGuard {
    pub fn kind(&self) -> ExecutorKind {
        self.kind
    }
    pub fn plan(&self) -> &EnforcementPlan {
        &self.plan
    }
    /// Endpoints the proxy refused so far (host and port; no content).
    pub fn egress_denials(&self) -> Vec<HostPort> {
        self.proxy.as_ref().map(EgressProxy::denials).unwrap_or_default()
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        if let (Some(p), Some(d)) = (self.proxy.take(), &self.denials) {
            if let Ok(mut d) = d.lock() {
                for hp in p.denials() {
                    if !d.contains(&hp) {
                        d.push(hp);
                    }
                }
            }
        }
        #[cfg(target_os = "linux")]
        if self.kind == ExecutorKind::Bwrap && self.plan.worktree_writable {
            linux::remove_placeholders(&self.plan);
        }
        if let Some((k, real, home)) = self.cli_home.take() {
            cli_home::copy_back(k, &real, &home);
        }
        // Dropping the scratch dir removes it (after the copy-back above).
        drop(self.tmp.take());
    }
}

/// A confined command ready for stdio and spawning, plus its guard.
pub struct Prepared {
    pub command: std::process::Command,
    pub guard: TurnGuard,
}

/// Why a spawn was refused before it started.
fn refuse(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, msg.into())
}

/// Canonical form of `p` (the longest existing ancestor canonicalized, the
/// rest appended), so `/tmp` and `/private/tmp` compare equal.
pub fn canonical(p: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        return c;
    }
    let mut tail = Vec::new();
    let mut cur = p.to_path_buf();
    while let Some(parent) = cur.parent().map(Path::to_path_buf) {
        if let Some(name) = cur.file_name() {
            tail.push(name.to_os_string());
        }
        if let Ok(c) = std::fs::canonicalize(&parent) {
            let mut out = c;
            for t in tail.iter().rev() {
                out.push(t);
            }
            return out;
        }
        cur = parent;
    }
    p.to_path_buf()
}

/// Refuse a cwd that is the data dir (or inside it, unless explicitly
/// readable), `$HOME` itself, or `/`.
pub fn check_cwd(cwd: &Path, host: &HostPaths) -> io::Result<()> {
    let c = canonical(cwd);
    if c.parent().is_none() {
        return Err(refuse(format!("refusing to run an agent process with cwd {}", c.display())));
    }
    if let Some(h) = home_dir() {
        if c == canonical(&h) {
            return Err(refuse("refusing to run an agent process in your home directory; bind a repository or folder"));
        }
    }
    for d in &host.data_dirs {
        let d = canonical(d);
        if c.starts_with(&d) && !host.allow_read.iter().any(|a| c.starts_with(canonical(a))) {
            return Err(refuse(format!("refusing to run an agent process inside Ligus's data dir ({})", d.display())));
        }
    }
    Ok(())
}

/// Prefix of a turn's scratch dir under the system temp dir.
pub const TURN_DIR_PREFIX: &str = "ligus-turn-";
/// The file in a turn dir naming the process that made it (its pid).
const TURN_DIR_OWNER: &str = ".ligus-owner";

/// Turn dirs this process is using right now (never swept).
fn live_turn_dirs() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
    static L: OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// A turn's scratch dir. Dropping it (the turn ended, failed to start, or
/// its future was cancelled) removes the dir; [`sweep_stale_turn_dirs`]
/// catches what a killed process leaves behind.
#[derive(Debug)]
pub struct TurnTmp {
    path: PathBuf,
}

impl TurnTmp {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TurnTmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
        if let Ok(mut l) = live_turn_dirs().lock() {
            l.remove(&self.path);
        }
    }
}

fn make_turn_tmp() -> io::Result<TurnTmp> {
    make_turn_tmp_in(&std::env::temp_dir())
}

fn make_turn_tmp_in(parent: &Path) -> io::Result<TurnTmp> {
    let mut rnd = [0u8; 8];
    getrandom::getrandom(&mut rnd).map_err(|e| io::Error::other(e.to_string()))?;
    let id: String = rnd.iter().map(|b| format!("{b:02x}")).collect();
    let dir = canonical(parent).join(format!("{TURN_DIR_PREFIX}{id}"));
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(&dir)?;
    if let Ok(mut l) = live_turn_dirs().lock() {
        l.insert(dir.clone());
    }
    let tmp = TurnTmp { path: dir };
    std::fs::write(tmp.path.join(TURN_DIR_OWNER), std::process::id().to_string())?;
    Ok(tmp)
}

/// Whether process `pid` is running (unknown counts as running).
fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(p) = libc::pid_t::try_from(pid) else { return false };
        if p <= 0 {
            return false;
        }
        // SAFETY: signal 0 only checks that the process exists.
        let r = unsafe { libc::kill(p, 0) };
        r == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// Remove `ligus-turn-*` dirs under the system temp dir that no live
/// process owns and that are older than `older_than` (the longest a turn
/// may run). A turn killed with its host (or a host killed mid-turn) would
/// otherwise leave its scratch dir, with any CLI config copied into its
/// per-turn home, behind. Returns how many were removed.
pub fn sweep_stale_turn_dirs(older_than: std::time::Duration) -> usize {
    sweep_stale_turn_dirs_in(&std::env::temp_dir(), older_than)
}

/// [`sweep_stale_turn_dirs`] under `parent` (tests).
pub fn sweep_stale_turn_dirs_in(parent: &Path, older_than: std::time::Duration) -> usize {
    let Ok(rd) = std::fs::read_dir(parent) else { return 0 };
    let parent = canonical(parent);
    let live = live_turn_dirs().lock().map(|l| l.clone()).unwrap_or_default();
    let me = std::process::id();
    let mut removed = 0;
    for entry in rd.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(TURN_DIR_PREFIX) {
            continue;
        }
        // Never follow a link someone planted under the temp dir.
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else { continue };
        if !meta.is_dir() {
            continue;
        }
        let dir = parent.join(&name);
        if live.contains(&dir) {
            continue;
        }
        let owner_file = dir.join(TURN_DIR_OWNER);
        let owner: Option<u32> = std::fs::read_to_string(&owner_file).ok().and_then(|s| s.trim().parse().ok());
        // Another running process's turn: not ours to judge. Our own pid
        // but not live here: a leaked dir of this process.
        if owner.is_some_and(|pid| pid != me && pid_alive(pid)) {
            continue;
        }
        let made = std::fs::symlink_metadata(&owner_file).or(Ok::<_, io::Error>(meta)).and_then(|m| m.modified());
        let age = made.ok().and_then(|t| t.elapsed().ok()).unwrap_or_default();
        if age < older_than {
            continue;
        }
        if std::fs::remove_dir_all(&dir).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Sweep stale turn dirs now and every ten minutes on a background thread
/// (once per process). `max_turn` is the host's longest turn; a dir is swept
/// only once it is older than that plus a margin.
pub fn start_turn_dir_sweeper(max_turn: std::time::Duration) {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    let older_than = max_turn + std::time::Duration::from_secs(600);
    let _ = std::thread::Builder::new().name("ligus-turn-sweep".into()).spawn(move || loop {
        let n = sweep_stale_turn_dirs(older_than);
        if n > 0 {
            tracing::info!(target: "sandbox", "removed {n} stale turn scratch dir(s)");
        }
        std::thread::sleep(std::time::Duration::from_secs(600));
    });
}

/// Confine one child for the current turn: plan it, start its proxy, make
/// its scratch dir, and wrap the command for this host's executor.
pub fn prepare(spec: SpawnSpec<'_>) -> io::Result<Prepared> {
    prepare_with(&*default_executor(), spec)
}

/// [`prepare`] with an explicit executor (tests).
pub fn prepare_with(executor: &dyn TurnExecutor, spec: SpawnSpec<'_>) -> io::Result<Prepared> {
    let policy = current_turn().unwrap_or_else(|| Arc::new(TurnPolicy::baseline(HostKind::Desktop)));
    let rt = current_runtime().unwrap_or_default();
    let agent = matches!(spec.purpose, Purpose::Agent(_));
    let provider_key = spec.provider_key.or(rt.provider_key.as_deref().filter(|_| agent));
    let model_provider_id = spec.model_provider_id.or(rt.model_provider_id.as_deref().filter(|_| agent));
    let model_base_url = spec.model_base_url.or(rt.model_base_url.as_deref().filter(|_| agent));
    let host = host_paths();
    let home = home_dir().unwrap_or_else(|| PathBuf::from("/nonexistent"));
    // Removed on every early return below, and with the guard when the
    // turn ends or its future is dropped.
    let turn_tmp = make_turn_tmp()?;
    let tmp = turn_tmp.path().to_path_buf();
    let cwd = match spec.cwd {
        Some(c) if !c.as_os_str().is_empty() => {
            check_cwd(c, &host)?;
            canonical(c)
        }
        _ => tmp.clone(),
    };
    // C-9: a CLI agent runs with a throwaway home (its real config read-only).
    let cli_home_dir = match spec.purpose {
        Purpose::Agent(k) if k.is_cli_agent() && cli_home::enabled() => {
            let h = tmp.join("home");
            cli_home::populate(k, &home, &h)?;
            Some((k, h))
        }
        _ => None,
    };
    let envelope_args: Vec<String> = Vec::new();
    let arg_strings: Vec<String> = spec.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    let write_capable = match spec.purpose {
        Purpose::Agent(_) if spec.writes.is_some() => spec.writes.unwrap_or(false),
        Purpose::Agent(k) => crate::author_gate::turn_writes(k, &arg_strings),
        Purpose::Tool | Purpose::Preflight => true,
        Purpose::McpServer => false,
    };
    let inputs = PlanInputs {
        host: policy.host,
        envelope: &policy.envelope,
        purpose: spec.purpose,
        gate: &policy.gate,
        worktree: &cwd,
        // No cwd given: the turn runs in its own scratch dir.
        worktree_is_isolated: policy.worktree_is_isolated || spec.cwd.is_none(),
        write_capable,
        tmp: &tmp,
        home: &home,
        host_paths: &host,
        mcp_hosts: &policy.mcp_hosts,
        mcp_ids: &policy.mcp_ids,
        model_base_url,
        extra_env: spec.extra_env,
        provider_key,
        model_provider_id,
        envelope_args: &envelope_args,
        ligus_mcp_socket: policy.ligus_mcp_socket.as_deref(),
        egress_presets: &policy.egress_presets,
        cli_home: cli_home_dir.as_ref().map(|(_, h)| h.as_path()),
    };
    let plan = EnvelopeEnforcer::plan(&inputs);
    let kind = executor.kind();
    // `unconfined_refuses_write_tiers`: without an OS sandbox a CLI agent that
    // can write is not run at all.
    if kind == ExecutorKind::Unconfined && matches!(spec.purpose, Purpose::Agent(_)) && write_capable && policy.gate.allows_writes() {
        return Err(refuse(
            "this host has no agent sandbox (sandbox-exec on macOS; on Linux, bubblewrap with user namespaces, \
             or Landlock on Linux 6.7+), so Ligus won't run a write-capable agent turn here; use the read-only tier, \
             or install the sandbox (`ligus doctor` says why it's missing)",
        ));
    }
    let proxy = match &plan.egress {
        EgressPlan::Proxy { allow } => {
            let sock = (kind == ExecutorKind::Bwrap).then(|| tmp.join(".ligus-egress.sock"));
            Some(EgressProxy::start(allow.clone(), sock.as_deref())?)
        }
        _ => None,
    };
    let command = executor.confine(spec.program, &spec.args, &cwd, &plan, proxy.as_ref())?;
    let cli_home = cli_home_dir.map(|(k, h)| (k, home.clone(), h));
    Ok(Prepared { command, guard: TurnGuard { cli_home, proxy, denials: Some(policy.denials.clone()), tmp: Some(turn_tmp), kind, plan } })
}

/// A tokio command for `spec`, confined; the caller sets stdio. Hold the
/// guard until the child exits. `kill_on_drop` is on.
pub fn tokio_command(spec: SpawnSpec<'_>) -> io::Result<(tokio::process::Command, TurnGuard)> {
    let Prepared { command, guard } = prepare(spec)?;
    let mut cmd = tokio::process::Command::from(command);
    cmd.kill_on_drop(true);
    Ok((cmd, guard))
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

static STATUS: OnceLock<SandboxStatus> = OnceLock::new();

/// Which sandbox tier this host uses, and why: for `ligus doctor`, the logs
/// and the Settings banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxStatus {
    pub kind: ExecutorKind,
    /// One line: the tier and why it was chosen.
    pub summary: String,
    /// What this tier doesn't enforce here (residuals, kernel features
    /// missing), and what would restore a stronger tier.
    pub notes: Vec<String>,
}

impl SandboxStatus {
    pub fn unconfined(why: String, notes: Vec<String>) -> Self {
        SandboxStatus { kind: ExecutorKind::Unconfined, summary: format!("no agent sandbox: {why}; write-capable agent turns are refused"), notes }
    }

    /// The tier's short name: `seatbelt`, `bwrap`, `landlock` or `unconfined`.
    pub fn tier(&self) -> &'static str {
        match self.kind {
            ExecutorKind::Seatbelt => "seatbelt",
            ExecutorKind::Bwrap => "bwrap",
            ExecutorKind::Landlock => "landlock",
            ExecutorKind::Unconfined => "unconfined",
            ExecutorKind::Gvisor => "gvisor",
            ExecutorKind::AppleContainer => "apple-container",
            ExecutorKind::K8sJob => "k8s-job",
        }
    }

    fn log(&self) {
        match self.kind {
            ExecutorKind::Unconfined | ExecutorKind::Landlock => {
                tracing::warn!(target: "sandbox", tier = self.tier(), "agent sandbox: {}", self.summary);
                for n in &self.notes {
                    tracing::warn!(target: "sandbox", tier = self.tier(), "agent sandbox: {n}");
                }
            }
            _ => tracing::info!(target: "sandbox", tier = self.tier(), "agent sandbox: {}", self.summary),
        }
    }
}

/// This host's sandbox tier (probing it on first use).
pub fn sandbox_status() -> SandboxStatus {
    let _ = default_executor();
    STATUS.get().cloned().unwrap_or_else(|| SandboxStatus::unconfined("not probed".into(), Vec::new()))
}

/// For tests that need a real sandbox: `true` (skip) when this host has
/// none, after printing a `SKIPPED` line that the test harness doesn't
/// capture. With `LIGUS_REQUIRE_SANDBOX=1` (CI on a sandbox-capable runner)
/// a missing sandbox fails the test instead.
#[doc(hidden)]
pub fn skip_without_sandbox(test: &str) -> bool {
    if !refuses_writes() {
        return false;
    }
    let why = sandbox_status().summary;
    if std::env::var("LIGUS_REQUIRE_SANDBOX").is_ok_and(|v| v.trim() == "1") {
        panic!("{test}: LIGUS_REQUIRE_SANDBOX=1 but this host has no agent sandbox ({why})");
    }
    use std::io::Write;
    let _ = writeln!(std::io::stderr().lock(), "SKIPPED {test}: {why}");
    true
}

#[cfg(test)]
mod tests;

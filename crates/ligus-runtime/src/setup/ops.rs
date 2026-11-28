//! What the engine asks of a surface. Each method returns facts or does one
//! write; none of them returns a secret.

use ligus_core::WorkspaceRole;
use serde::Serialize;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::checks::Check;
use crate::cli_detect::LoginState;
use crate::model_catalog::ModelList;

/// Which program drives the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Surface {
    Cli,
    Desktop,
    Term,
}

/// Why an operation didn't happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpsError {
    /// This surface can't do it (the engine names one that can).
    Unsupported,
    /// The workspace's rules refused it (a role, lockdown).
    Refused(String),
    /// The network, the relay or the server didn't answer.
    Offline(String),
    /// Anything else, in one line (never a secret).
    Failed(String),
}

impl std::fmt::Display for OpsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpsError::Unsupported => f.write_str("not available here"),
            OpsError::Refused(m) | OpsError::Offline(m) | OpsError::Failed(m) => f.write_str(m),
        }
    }
}

impl From<anyhow::Error> for OpsError {
    fn from(e: anyhow::Error) -> Self {
        OpsError::Failed(crate::secret_redact::redacted(&format!("{e:#}")))
    }
}

impl From<crate::chat_service::ChatError> for OpsError {
    fn from(e: crate::chat_service::ChatError) -> Self {
        match e {
            crate::chat_service::ChatError::Unauthorized(m) => OpsError::Refused(m),
            e => OpsError::Failed(crate::secret_redact::redacted(&e.to_string())),
        }
    }
}

pub type OpsResult<T> = Result<T, OpsError>;

/// A harness (coding CLI or model) an agent can run on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessFact {
    /// `claude-code`, `codex`, `copilot`, `antigravity`, `aider`, `pi`,
    /// `local`, `gateway`.
    pub id: String,
    pub label: String,
    pub installed: bool,
    pub login: LoginState,
    /// How the sign-in was judged (never a token).
    pub login_detail: String,
    /// What to run in a terminal to sign in (empty: nothing to sign in to).
    pub login_command: String,
    pub install_command: String,
    pub version: Option<String>,
}

/// This host's agent sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxFact {
    /// `seatbelt`, `bwrap`, `landlock` or `unconfined`.
    pub tier: String,
    pub summary: String,
}

impl SandboxFact {
    pub fn confined(&self) -> bool {
        self.tier != "unconfined"
    }
    /// This host's sandbox, as the executor probed it.
    pub fn here() -> Self {
        let s = crate::executor::sandbox_status();
        SandboxFact { tier: s.tier().to_string(), summary: s.summary }
    }
}

/// A runtime an agent can name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeFact {
    pub id: String,
    /// The harness id (see [`HarnessFact::id`]; `anthropic`, `openai` and
    /// `openrouter` for a hosted API).
    pub harness: String,
    pub model: String,
    /// For `local`: the endpoint; for `gateway`: its name.
    pub endpoint: String,
    /// A workspace runtime (on the log): other hosts can run it. A
    /// device-local runtime only runs on the device that keeps it.
    pub shared: bool,
    /// The secret reference a hosted-API runtime names (never a key).
    pub secret_ref: String,
    /// The provider kind's wire name (`anthropic`, `custom`), when known.
    pub provider: String,
}

/// What a new runtime should be.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeSpec {
    /// A free id (the engine picks one no runtime has).
    pub id: String,
    pub name: String,
    pub harness: String,
    pub model: String,
    /// For `local`: the server's URL; for `gateway`: the gateway's name.
    pub endpoint: String,
    /// The provider kind's wire name (`claudeCode`, `custom`, `anthropic`):
    /// for a gateway it follows the gateway's wire, never a member's choice.
    pub provider: String,
    /// For a hosted API on a host: the secret reference the host resolves
    /// (a name its `host.toml` declares; never a key).
    pub secret_ref: String,
    /// It must be a workspace runtime: the agent runs on another host.
    pub shared: bool,
}

/// One source a host lists for agents (an offer on its record), with the
/// readiness its signed beat reports. Names and states only: never a URL,
/// an account or a key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSourceFact {
    pub host_id: String,
    pub host_label: String,
    /// `device` (a person's computer) or `worker` (a box or worker).
    pub host_kind: String,
    /// The host is this device.
    pub this_device: bool,
    /// The host is one of this person's own devices or boxes.
    pub mine: bool,
    pub online: bool,
    /// The host's Ligus build from its beat ("" when unknown).
    pub build: String,
    /// The offer's name (`claude-code`, `gw-home`).
    pub name: String,
    /// `cli`, `local`, `gateway` or `provider`.
    pub kind: String,
    /// The harness wire name (`claudeCode`, `anthropic`); empty for a gateway.
    pub harness: String,
    /// A gateway's wire: `openai` or `anthropic`.
    pub wire: String,
    /// Installed and signed in, or the server answered; `None` with no report.
    pub ready: Option<bool>,
    pub why: String,
    pub models: Vec<String>,
    /// The model list is everything it runs.
    pub complete: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentFact {
    pub id: Uuid,
    pub name: String,
    pub runtime_id: String,
    /// The host it is placed on (`""`: its owner's own devices).
    pub host_id: String,
    /// Its owner's member id (`""`: a shared agent, the workspace's).
    pub owner: String,
    /// This device's person owns it.
    pub mine: bool,
    /// What it runs when it carries its own run spec (rules 4): the source
    /// a host offers by name, its harness and model. Empty `source`: it
    /// names a runtime (`runtime_id`).
    pub source: String,
    pub harness: String,
    pub model: String,
}

/// A change to an existing agent (Change model…, Runs on…): one record,
/// one event. `None` keeps what it has.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentEdit {
    /// Name this runtime instead (rules 3, or a source no host offers by
    /// name).
    pub runtime_id: Option<String>,
    /// Carry this run spec (rules 4): `{harness, source, model}`.
    pub run: Option<(String, String, String)>,
    /// Place it here (`""`: its owner's own devices).
    pub host_id: Option<String>,
}

/// The result of a test turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestTurn {
    pub ok: bool,
    pub latency_ms: u64,
    /// One line: the reply's start or the error (redacted).
    pub detail: String,
}

/// An MCP server as the engine sees it (no token).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpFact {
    pub id: String,
    /// `stdio` or `http`.
    pub transport: String,
    /// The command line, or the URL's origin.
    pub target: String,
    pub enabled: bool,
    /// A token is saved for it on this device.
    pub has_secret: bool,
    /// Agent ids that may reach it; empty: every agent.
    pub agents: Vec<Uuid>,
}

/// What git says about a folder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderFact {
    pub path: String,
    pub exists: bool,
    pub is_checkout: bool,
    /// A linked worktree (`.git` is a file): bind its main checkout instead.
    pub linked_worktree: bool,
    /// The canonical repository it is a checkout of (`github.com/o/r`).
    pub canonical: Option<String>,
    /// The `origin` remote, without credentials.
    pub remote: Option<String>,
    pub default_branch: Option<String>,
    pub root_commit: Option<String>,
    /// Another host on this machine manages it (its label).
    pub managed_elsewhere: Option<String>,
    /// Already bound on this device (to this repo id).
    pub bound_to: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoFact {
    pub id: String,
    pub canonical: String,
    pub display_name: String,
    pub default_branch: String,
    /// The folder bound on this device, if any.
    pub bound_here: Option<String>,
}

/// This data dir as a box.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoxFact {
    /// This is the desktop app's data dir (a box must never use it).
    pub desktop_data_dir: bool,
    /// It is already a member of a workspace.
    pub joined: bool,
    pub workspace: Option<String>,
    /// A relay is configured.
    pub relay: Option<String>,
}

/// How a box's request to join through the workspace's approval mailbox
/// (code match) ended. The box only asks: an Admin's passkey decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoxAsk {
    /// The codes matched, the Admin's approval was found on the workspace
    /// log and the admission is kept. `by` is the Admin's name.
    Joined { workspace: String, by: String },
    /// The relay has no approval mailbox: paste the join request instead.
    NoMailbox,
    /// It didn't finish: its exit code, one line and how to fix it.
    Stopped { exit: i32, why: String, fix: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostFact {
    pub id: String,
    /// `device` or `worker`.
    pub kind: String,
    pub label: String,
    pub online: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteFact {
    pub id: Uuid,
    pub role: String,
    pub created_ms: i64,
    /// Someone joined with it.
    pub used: bool,
    /// Closed or past its expiry.
    pub closed: bool,
}

/// One relay probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeFact {
    Ok,
    /// Reachable, but it wants a (valid) access token.
    Unauthorized,
    HttpStatus(u16),
    Unreachable(String),
}

/// Whether `ligus serve` answers on this box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeFact {
    Running(String),
    NotRunning(String),
}

/// The surface's side of the engine. Methods default to
/// [`OpsError::Unsupported`] (or an empty list) so a surface implements only
/// what it can; the engine turns `Unsupported` into a step for the surface
/// that can.
#[allow(unused_variables)]
pub trait SetupOps {
    fn surface(&self) -> Surface;

    // context
    /// This device's role in the workspace; `None` before it has one.
    fn my_role(&self) -> Option<WorkspaceRole> {
        None
    }
    /// The workspace is in lockdown (dearth mode).
    fn lockdown(&self) -> bool {
        false
    }
    /// MCP server ids lockdown still allows.
    fn lockdown_mcp_allowed(&self) -> Vec<String> {
        Vec::new()
    }
    fn sandbox(&self) -> SandboxFact {
        SandboxFact::here()
    }
    /// The relay this workspace syncs through, if any.
    fn relay_url(&self) -> Option<String> {
        None
    }

    // agent
    fn harnesses(&self) -> Vec<HarnessFact> {
        Vec::new()
    }
    fn models(&self, harness: &str, endpoint: &str) -> ModelList {
        ModelList::none("type a model name")
    }
    fn runtimes(&self) -> Vec<RuntimeFact> {
        Vec::new()
    }
    /// Defining a runtime needs an Admin here (the CLI's workspace runtimes
    /// carry credentials); the desktop's are device-local.
    fn runtimes_need_admin(&self) -> bool {
        false
    }
    fn add_runtime(&mut self, spec: &RuntimeSpec) -> OpsResult<()> {
        Err(OpsError::Unsupported)
    }
    fn remove_runtime(&mut self, id: &str) -> OpsResult<()> {
        Err(OpsError::Unsupported)
    }
    fn agents(&self) -> Vec<AgentFact> {
        Vec::new()
    }
    /// Display names of the workspace's members (an agent may not take one).
    fn member_names(&self) -> Vec<String> {
        Vec::new()
    }
    fn add_agent(&mut self, name: &str, runtime_id: &str) -> OpsResult<Uuid> {
        Err(OpsError::Unsupported)
    }
    /// Add an agent with where it runs, in one record (one event): `host_id`
    /// empty is its owner's own devices, else that host.
    fn add_agent_placed(&mut self, name: &str, runtime_id: &str, host_id: &str) -> OpsResult<Uuid> {
        if host_id.is_empty() {
            self.add_agent(name, runtime_id)
        } else {
            Err(OpsError::Unsupported)
        }
    }
    /// Change an existing agent in one record (Change model…, Runs on…).
    fn change_agent(&mut self, agent_id: Uuid, edit: &AgentEdit) -> OpsResult<()> {
        Err(OpsError::Unsupported)
    }
    /// The workspace's rules version (0 when unknown, as on a device with
    /// no workspace yet). On rules 4 an agent carries its own run spec.
    fn rules(&self) -> u32 {
        0
    }
    /// The sources the hosts of this workspace list (their offers), one
    /// entry per host and offer, from host records their own devices signed.
    fn host_sources(&self) -> Vec<HostSourceFact> {
        Vec::new()
    }
    /// What people call this device in a list ("this Mac").
    fn this_device_label(&self) -> String {
        "this device".into()
    }
    /// Whether a new runtime is a write on the workspace's log here (the
    /// CLI's always are; the desktop keeps its own runtimes on the device
    /// unless another host must run it).
    fn runtime_on_log(&self, shared: bool) -> bool {
        shared
    }
    /// Whether this workspace holds new agents and workspace runtimes for
    /// their author's passkey (a shared workspace).
    fn passkey_writes(&self) -> bool {
        false
    }
    /// Told before each write that asks for the passkey: `step` of `of`,
    /// and what it is ("Add @coder"), so the surface can say "Confirm 1 of 2".
    fn confirming(&mut self, step: usize, of: usize, what: &str) {}
    /// Whether this device can keep a model API key in its own secret store
    /// (the desktop).
    fn keeps_model_keys(&self) -> bool {
        false
    }
    /// Whether a key for hosted API `harness` (`anthropic`, `openai`) is
    /// saved on this device already.
    fn has_model_key(&self, harness: &str) -> bool {
        false
    }
    /// Save a model API key in this device's secret store (never the log).
    fn put_model_key(&mut self, harness: &str, key: &Zeroizing<String>) -> OpsResult<()> {
        Err(OpsError::Unsupported)
    }
    /// Whether the agent's signed `localServers` grant is on.
    fn local_servers(&self, agent_id: Uuid) -> bool {
        false
    }
    /// Turn the agent's `localServers` grant on or off (the Owner's, with a
    /// passkey in a shared workspace: a signed `WorkspaceSandboxSet` keeping
    /// every other grant).
    fn set_local_servers(&mut self, agent_id: Uuid, on: bool) -> OpsResult<()> {
        Err(OpsError::Unsupported)
    }
    fn test_turn(&mut self, runtime_id: &str) -> OpsResult<TestTurn> {
        Err(OpsError::Unsupported)
    }
    /// [`SetupOps::test_turn`] for an agent that may run local servers
    /// (agy's helper server); by default the plain test turn.
    fn test_turn_with(&mut self, runtime_id: &str, local_servers: bool) -> OpsResult<TestTurn> {
        self.test_turn(runtime_id)
    }

    // tool
    fn mcp_servers(&self) -> Vec<McpFact> {
        Vec::new()
    }
    /// Start the unsaved server under this host's agent sandbox and list its
    /// tools (`token`: its bearer token, for a URL server).
    fn mcp_handshake(&mut self, spec: &crate::mcp::McpServerSpec, token: Option<&Zeroizing<String>>) -> OpsResult<Vec<String>> {
        Err(OpsError::Unsupported)
    }
    /// Save a URL server's bearer token in this device's secret store, bound
    /// to the URL's origin.
    fn put_mcp_secret(&mut self, id: &str, url: &str, secret: &Zeroizing<String>) -> OpsResult<()> {
        Err(OpsError::Unsupported)
    }
    /// Remove the token and its origin.
    fn delete_mcp_secret(&mut self, id: &str, url: &str) -> OpsResult<()> {
        Err(OpsError::Unsupported)
    }
    /// Write the server (enabled) and its reach together. `reach` empty:
    /// every agent.
    fn add_mcp_server(&mut self, spec: &crate::mcp::McpServerSpec, reach: &[Uuid]) -> OpsResult<()> {
        Err(OpsError::Unsupported)
    }
    fn remove_mcp_server(&mut self, id: &str) -> OpsResult<()> {
        Err(OpsError::Unsupported)
    }

    // folder
    fn repos(&self) -> Vec<RepoFact> {
        Vec::new()
    }
    /// Every check `repos::bind` makes, without binding.
    fn inspect_folder(&self, path: &str) -> FolderFact {
        crate::setup::validate::inspect_folder(path)
    }
    fn add_repo(&mut self, folder: &FolderFact, display_name: &str, default_branch: &str) -> OpsResult<String> {
        Err(OpsError::Unsupported)
    }
    fn bind_folder(&mut self, repo_id: &str, path: &str) -> OpsResult<()> {
        Err(OpsError::Unsupported)
    }
    /// Start with a scratch repository instead of a folder: an empty one
    /// this host creates under its scratch root, registered as a workspace
    /// repository flagged scratch and bound here (`repos::scratch_host`).
    /// Returns its repo id and folder.
    fn create_scratch(&mut self, name: &str) -> OpsResult<(String, String)> {
        Err(OpsError::Unsupported)
    }

    // box
    fn box_state(&self) -> OpsResult<BoxFact> {
        Err(OpsError::Unsupported)
    }
    /// This box's join request (`ligusjoin1:…`, public) and its fingerprint.
    fn join_request(&mut self, label: &str) -> OpsResult<(String, String)> {
        Err(OpsError::Unsupported)
    }
    /// Ask to join through the approval mailbox at `to` (`ligusaddr1:`):
    /// post this box's join request, show the match code, wait for an
    /// Admin's passkey decision, check it on the log and keep the admission.
    /// Nothing is pasted.
    fn ask_to_join(&mut self, to: &str, label: &str) -> OpsResult<BoxAsk> {
        Err(OpsError::Unsupported)
    }
    /// Keep a sealed admission code for this box.
    fn redeem(&mut self, code: &Zeroizing<String>) -> OpsResult<String> {
        Err(OpsError::Unsupported)
    }
    fn serve_running(&self) -> ServeFact {
        ServeFact::NotRunning("not checked here".into())
    }
    fn hosts(&self) -> Vec<HostFact> {
        Vec::new()
    }

    // phone
    /// The pairing precheck, as checks (ids `phone.precheck.*`).
    fn phone_precheck(&self) -> OpsResult<Vec<Check>> {
        Err(OpsError::Unsupported)
    }
    /// Labels of this person's paired phones.
    fn phones(&self) -> Vec<String> {
        Vec::new()
    }

    // invite
    fn invites(&self) -> Vec<InviteFact> {
        Vec::new()
    }

    // relay
    fn probe_relay(&self, url: &str, token: Option<&Zeroizing<String>>) -> ProbeFact {
        ProbeFact::Unreachable("can't probe from here".into())
    }
    /// Switch this device's copy of the workspace to `url` (and save the
    /// access token, when one is given, in the secret store).
    fn switch_relay(&mut self, url: &str, token: Option<&Zeroizing<String>>) -> OpsResult<()> {
        Err(OpsError::Unsupported)
    }
}

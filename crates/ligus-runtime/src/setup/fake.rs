//! An in-memory [`SetupOps`] for the engine's tests: facts to set, writes
//! recorded, failures to inject.

use std::collections::BTreeMap;

use ligus_core::WorkspaceRole;
use uuid::Uuid;
use zeroize::Zeroizing;

use super::*;
use crate::checks::Check;
use crate::cli_detect::LoginState;
use crate::mcp::{McpServerSpec, McpTransport};
use crate::model_catalog::{ModelList, ModelOption, ModelSource};

pub struct Fake {
    pub surface: Surface,
    pub role: Option<WorkspaceRole>,
    pub lockdown: bool,
    pub lockdown_allowed: Vec<String>,
    pub sandbox: SandboxFact,
    pub relay: Option<String>,
    pub harnesses: Vec<HarnessFact>,
    pub models: ModelList,
    pub runtimes: Vec<RuntimeFact>,
    pub runtimes_need_admin: bool,
    pub agents: Vec<AgentFact>,
    pub members: Vec<String>,
    pub mcp: Vec<McpFact>,
    pub handshake: Result<Vec<String>, OpsError>,
    /// Every secret value any op received (to prove where they went).
    pub secrets_seen: Vec<String>,
    pub mcp_secrets: BTreeMap<String, String>,
    pub folder: FolderFact,
    pub repos: Vec<RepoFact>,
    pub box_state: Result<BoxFact, OpsError>,
    pub serve: ServeFact,
    pub hosts: Vec<HostFact>,
    pub phones: Vec<String>,
    pub precheck: Result<Vec<Check>, OpsError>,
    pub invites: Vec<InviteFact>,
    pub probe: ProbeFact,
    /// Ops whose write fails (by name: "add_agent", "add_mcp_server", ...).
    pub fail: Vec<&'static str>,
    /// Every write, in order.
    pub writes: Vec<String>,
    pub test_turn: Result<TestTurn, OpsError>,
}

impl Default for Fake {
    fn default() -> Self {
        Fake {
            surface: Surface::Cli,
            role: Some(WorkspaceRole::Owner),
            lockdown: false,
            lockdown_allowed: Vec::new(),
            sandbox: SandboxFact { tier: "seatbelt".into(), summary: "seatbelt".into() },
            relay: Some("https://relay.example.com".into()),
            harnesses: vec![
                harness("claude-code", true, LoginState::LoggedIn),
                harness("codex", true, LoginState::NotLoggedIn),
                harness("copilot", false, LoginState::Unknown),
            ],
            models: ModelList { models: vec![ModelOption::new("sonnet", None)], source: ModelSource::Aliases, complete: false, note: None, error: None },
            runtimes: Vec::new(),
            runtimes_need_admin: false,
            agents: Vec::new(),
            members: vec!["Ana".into()],
            mcp: Vec::new(),
            handshake: Ok(vec!["search".into(), "fetch".into()]),
            secrets_seen: Vec::new(),
            mcp_secrets: BTreeMap::new(),
            folder: FolderFact::default(),
            repos: Vec::new(),
            box_state: Ok(BoxFact::default()),
            serve: ServeFact::Running("127.0.0.1:7777".into()),
            hosts: Vec::new(),
            phones: Vec::new(),
            precheck: Err(OpsError::Unsupported),
            invites: Vec::new(),
            probe: ProbeFact::Ok,
            fail: Vec::new(),
            writes: Vec::new(),
            test_turn: Ok(TestTurn { ok: true, latency_ms: 900, detail: "PINGOK".into() }),
        }
    }
}

pub fn harness(id: &str, installed: bool, login: LoginState) -> HarnessFact {
    HarnessFact {
        id: id.into(),
        label: id.into(),
        installed,
        login,
        login_detail: "checked".into(),
        login_command: format!("{id} login"),
        install_command: format!("install {id}"),
        version: Some("1.0".into()),
    }
}

impl Fake {
    fn write(&mut self, op: &'static str, what: String) -> OpsResult<()> {
        if self.fail.contains(&op) {
            return Err(OpsError::Failed(format!("{op} failed")));
        }
        self.writes.push(format!("{op} {what}"));
        Ok(())
    }
}

impl SetupOps for Fake {
    fn surface(&self) -> Surface {
        self.surface
    }
    fn my_role(&self) -> Option<WorkspaceRole> {
        self.role
    }
    fn lockdown(&self) -> bool {
        self.lockdown
    }
    fn lockdown_mcp_allowed(&self) -> Vec<String> {
        self.lockdown_allowed.clone()
    }
    fn sandbox(&self) -> SandboxFact {
        self.sandbox.clone()
    }
    fn relay_url(&self) -> Option<String> {
        self.relay.clone()
    }
    fn harnesses(&self) -> Vec<HarnessFact> {
        self.harnesses.clone()
    }
    fn models(&self, _h: &str, _e: &str) -> ModelList {
        self.models.clone()
    }
    fn runtimes(&self) -> Vec<RuntimeFact> {
        self.runtimes.clone()
    }
    fn runtimes_need_admin(&self) -> bool {
        self.runtimes_need_admin
    }
    fn add_runtime(&mut self, spec: &RuntimeSpec) -> OpsResult<()> {
        self.write("add_runtime", spec.id.clone())?;
        self.runtimes.push(RuntimeFact { id: spec.id.clone(), harness: spec.harness.clone(), model: spec.model.clone(), endpoint: spec.endpoint.clone() });
        Ok(())
    }
    fn remove_runtime(&mut self, id: &str) -> OpsResult<()> {
        self.write("remove_runtime", id.into())?;
        self.runtimes.retain(|r| r.id != id);
        Ok(())
    }
    fn agents(&self) -> Vec<AgentFact> {
        self.agents.clone()
    }
    fn member_names(&self) -> Vec<String> {
        self.members.clone()
    }
    fn add_agent(&mut self, name: &str, runtime_id: &str) -> OpsResult<Uuid> {
        self.write("add_agent", name.into())?;
        let id = Uuid::new_v4();
        self.agents.push(AgentFact { id, name: name.into(), runtime_id: runtime_id.into() });
        Ok(id)
    }
    fn test_turn(&mut self, _r: &str) -> OpsResult<TestTurn> {
        self.test_turn.clone()
    }
    fn mcp_servers(&self) -> Vec<McpFact> {
        self.mcp.clone()
    }
    fn mcp_handshake(&mut self, spec: &McpServerSpec, token: Option<&Zeroizing<String>>) -> OpsResult<Vec<String>> {
        if let Some(t) = token {
            self.secrets_seen.push(t.to_string());
        }
        // The spec is not saved yet when the handshake runs.
        assert!(!self.mcp.iter().any(|m| m.id == spec.id), "handshake runs on an unsaved spec");
        self.writes.push(format!("handshake {}", spec.id));
        self.handshake.clone()
    }
    fn put_mcp_secret(&mut self, id: &str, _url: &str, secret: &Zeroizing<String>) -> OpsResult<()> {
        self.secrets_seen.push(secret.to_string());
        self.write("put_mcp_secret", id.into())?;
        self.mcp_secrets.insert(id.into(), secret.to_string());
        Ok(())
    }
    fn delete_mcp_secret(&mut self, id: &str, _url: &str) -> OpsResult<()> {
        self.write("delete_mcp_secret", id.into())?;
        self.mcp_secrets.remove(id);
        Ok(())
    }
    fn add_mcp_server(&mut self, spec: &McpServerSpec, reach: &[Uuid]) -> OpsResult<()> {
        self.write("add_mcp_server", format!("{} enabled={} reach={}", spec.id, spec.enabled, reach.len()))?;
        let (transport, target) = match &spec.transport {
            McpTransport::Stdio { command, args } => ("stdio", std::iter::once(command.clone()).chain(args.clone()).collect::<Vec<_>>().join(" ")),
            McpTransport::Http { url } => ("http", validate::origin(url)),
        };
        self.mcp.push(McpFact {
            id: spec.id.clone(),
            transport: transport.into(),
            target,
            enabled: spec.enabled,
            has_secret: self.mcp_secrets.contains_key(&spec.id),
            agents: reach.to_vec(),
        });
        Ok(())
    }
    fn remove_mcp_server(&mut self, id: &str) -> OpsResult<()> {
        self.write("remove_mcp_server", id.into())?;
        self.mcp.retain(|m| m.id != id);
        Ok(())
    }
    fn repos(&self) -> Vec<RepoFact> {
        self.repos.clone()
    }
    fn inspect_folder(&self, _path: &str) -> FolderFact {
        self.folder.clone()
    }
    fn add_repo(&mut self, folder: &FolderFact, name: &str, branch: &str) -> OpsResult<String> {
        self.write("add_repo", format!("{} {name} {branch}", folder.canonical.clone().unwrap_or_default()))?;
        let id = Uuid::new_v4().to_string();
        self.repos.push(RepoFact {
            id: id.clone(),
            canonical: folder.canonical.clone().unwrap_or_default(),
            display_name: name.into(),
            default_branch: branch.into(),
            bound_here: None,
        });
        Ok(id)
    }
    fn bind_folder(&mut self, repo_id: &str, path: &str) -> OpsResult<()> {
        self.write("bind_folder", path.into())?;
        if let Some(r) = self.repos.iter_mut().find(|r| r.id == repo_id) {
            r.bound_here = Some(path.into());
        }
        Ok(())
    }
    fn create_scratch(&mut self, name: &str) -> OpsResult<(String, String)> {
        self.write("create_scratch", name.into())?;
        let id = Uuid::new_v4().to_string();
        let path = format!("/data/scratch/{}", if name.is_empty() { "scratch" } else { name });
        self.repos.push(RepoFact { id: id.clone(), canonical: "local:0".into(), display_name: name.into(), default_branch: "main".into(), bound_here: Some(path.clone()) });
        Ok((id, path))
    }
    fn box_state(&self) -> OpsResult<BoxFact> {
        self.box_state.clone()
    }
    fn join_request(&mut self, label: &str) -> OpsResult<(String, String)> {
        self.write("join_request", label.into())?;
        Ok(("ligusjoin1:PUBLIC".into(), "ABCD-EFGH".into()))
    }
    fn redeem(&mut self, code: &Zeroizing<String>) -> OpsResult<String> {
        self.secrets_seen.push(code.to_string());
        self.write("redeem", String::new())?;
        if let Ok(b) = &mut self.box_state {
            b.joined = true;
        }
        Ok("team".into())
    }
    fn serve_running(&self) -> ServeFact {
        self.serve.clone()
    }
    fn hosts(&self) -> Vec<HostFact> {
        self.hosts.clone()
    }
    fn phone_precheck(&self) -> OpsResult<Vec<Check>> {
        self.precheck.clone()
    }
    fn phones(&self) -> Vec<String> {
        self.phones.clone()
    }
    fn invites(&self) -> Vec<InviteFact> {
        self.invites.clone()
    }
    fn probe_relay(&self, _url: &str, _token: Option<&Zeroizing<String>>) -> ProbeFact {
        self.probe.clone()
    }
    fn switch_relay(&mut self, url: &str, token: Option<&Zeroizing<String>>) -> OpsResult<()> {
        if let Some(t) = token {
            self.secrets_seen.push(t.to_string());
        }
        self.write("switch_relay", url.into())?;
        self.relay = Some(url.into());
        Ok(())
    }
}

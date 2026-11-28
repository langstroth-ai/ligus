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
    /// How asking by code match ends (`ligus setup box --to`).
    pub code_match: Result<BoxAsk, OpsError>,
    pub serve: ServeFact,
    pub hosts: Vec<HostFact>,
    pub phones: Vec<String>,
    pub precheck: Result<Vec<Check>, OpsError>,
    pub invites: Vec<InviteFact>,
    pub probe: ProbeFact,
    /// Ops whose write fails (by name: "add_agent", "add_mcp_server", ...).
    pub fail: Vec<&'static str>,
    /// Ops the workspace's rules hold for the author's passkey.
    pub needs_presence: Vec<&'static str>,
    /// Every write, in order.
    pub writes: Vec<String>,
    pub test_turn: Result<TestTurn, OpsError>,
    /// Agents whose local-servers grant is on.
    pub local_servers: Vec<Uuid>,
    /// What the workspace's hosts list.
    pub host_sources: Vec<HostSourceFact>,
    /// The workspace holds agents and workspace runtimes for the passkey.
    pub passkey_writes: bool,
    /// Runtimes are log writes (the CLI's).
    pub runtimes_on_log: bool,
    /// "Confirm k of n: what", in order.
    pub confirms: Vec<String>,
    /// This device keeps model keys (the desktop).
    pub keeps_keys: bool,
    /// Model keys saved here, by harness.
    pub model_keys: BTreeMap<String, String>,
    /// The workspace's rules version.
    pub rules: u32,
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
            code_match: Err(OpsError::Unsupported),
            serve: ServeFact::Running("127.0.0.1:7777".into()),
            hosts: Vec::new(),
            phones: Vec::new(),
            precheck: Err(OpsError::Unsupported),
            invites: Vec::new(),
            probe: ProbeFact::Ok,
            fail: Vec::new(),
            needs_presence: Vec::new(),
            writes: Vec::new(),
            test_turn: Ok(TestTurn { ok: true, latency_ms: 900, detail: "PINGOK".into() }),
            local_servers: Vec::new(),
            host_sources: Vec::new(),
            passkey_writes: false,
            runtimes_on_log: false,
            confirms: Vec::new(),
            keeps_keys: false,
            model_keys: BTreeMap::new(),
            rules: 3,
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
        if self.needs_presence.contains(&op) {
            return Err(OpsError::Refused(ligus_core::authz::PRESENCE_REQUIRED.into()));
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
        self.runtimes.push(RuntimeFact {
            id: spec.id.clone(),
            harness: spec.harness.clone(),
            model: spec.model.clone(),
            endpoint: spec.endpoint.clone(),
            shared: spec.shared,
            secret_ref: spec.secret_ref.clone(),
            provider: spec.provider.clone(),
        });
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
        self.add_agent_placed(name, runtime_id, "")
    }
    fn add_agent_placed(&mut self, name: &str, runtime_id: &str, host_id: &str) -> OpsResult<Uuid> {
        let what = if host_id.is_empty() { name.to_string() } else { format!("{name} on {host_id}") };
        self.write("add_agent", what)?;
        let id = Uuid::new_v4();
        self.agents.push(AgentFact { id, name: name.into(), runtime_id: runtime_id.into(), host_id: host_id.into(), owner: "me".into(), mine: true, ..Default::default() });
        Ok(id)
    }
    fn change_agent(&mut self, agent_id: Uuid, edit: &AgentEdit) -> OpsResult<()> {
        let name = self.agents.iter().find(|a| a.id == agent_id).map(|a| a.name.clone()).unwrap_or_default();
        let what = format!(
            "{name} runtime={} run={} host={}",
            edit.runtime_id.clone().unwrap_or_else(|| "-".into()),
            edit.run.as_ref().map(|(h, s, m)| format!("{h}/{s}/{m}")).unwrap_or_else(|| "-".into()),
            edit.host_id.clone().unwrap_or_else(|| "-".into()),
        );
        self.write("change_agent", what)?;
        if let Some(a) = self.agents.iter_mut().find(|a| a.id == agent_id) {
            if let Some(r) = &edit.runtime_id {
                a.runtime_id = r.clone();
                a.source.clear();
                a.harness.clear();
                a.model.clear();
            }
            if let Some((h, s, m)) = &edit.run {
                a.harness = h.clone();
                a.source = s.clone();
                a.model = m.clone();
            }
            if let Some(h) = &edit.host_id {
                a.host_id = h.clone();
            }
        }
        Ok(())
    }
    fn rules(&self) -> u32 {
        self.rules
    }
    fn host_sources(&self) -> Vec<HostSourceFact> {
        self.host_sources.clone()
    }
    fn this_device_label(&self) -> String {
        "this Mac".into()
    }
    fn runtime_on_log(&self, shared: bool) -> bool {
        shared || self.runtimes_on_log
    }
    fn passkey_writes(&self) -> bool {
        self.passkey_writes
    }
    fn confirming(&mut self, step: usize, of: usize, what: &str) {
        self.confirms.push(format!("{step} of {of}: {what}"));
    }
    fn keeps_model_keys(&self) -> bool {
        self.keeps_keys
    }
    fn has_model_key(&self, harness: &str) -> bool {
        self.model_keys.contains_key(harness)
    }
    fn put_model_key(&mut self, harness: &str, key: &Zeroizing<String>) -> OpsResult<()> {
        self.secrets_seen.push(key.to_string());
        self.write("put_model_key", harness.into())?;
        self.model_keys.insert(harness.into(), key.to_string());
        Ok(())
    }
    fn local_servers(&self, agent_id: Uuid) -> bool {
        self.local_servers.contains(&agent_id)
    }
    fn set_local_servers(&mut self, agent_id: Uuid, on: bool) -> OpsResult<()> {
        let name = self.agents.iter().find(|a| a.id == agent_id).map(|a| a.name.clone()).unwrap_or_default();
        self.write("set_local_servers", format!("{name} {on}"))?;
        self.local_servers.retain(|a| *a != agent_id);
        if on {
            self.local_servers.push(agent_id);
        }
        Ok(())
    }
    fn test_turn(&mut self, _r: &str) -> OpsResult<TestTurn> {
        self.test_turn.clone()
    }
    fn test_turn_with(&mut self, r: &str, local_servers: bool) -> OpsResult<TestTurn> {
        if local_servers {
            self.writes.push(format!("test_turn_with_local_servers {r}"));
        }
        self.test_turn(r)
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
    fn ask_to_join(&mut self, to: &str, label: &str) -> OpsResult<BoxAsk> {
        self.write("ask_to_join", format!("{to} {label}"))?;
        let r = self.code_match.clone();
        if let (Ok(BoxAsk::Joined { .. }), Ok(b)) = (&r, &mut self.box_state) {
            b.joined = true;
        }
        r
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

/// One offer on a host, as `host_sources` reports it (a box unless `mine`
/// is set by the caller).
pub fn offer(host_id: &str, label: &str, kind: &str, name: &str, wire: &str, ready: Option<bool>, models: &[&str]) -> HostSourceFact {
    HostSourceFact {
        host_id: host_id.into(),
        host_label: label.into(),
        host_kind: "worker".into(),
        this_device: false,
        mine: false,
        online: true,
        build: "0.5.0".into(),
        name: name.into(),
        kind: kind.into(),
        harness: if kind == "provider" { "anthropic".into() } else { String::new() },
        wire: wire.into(),
        ready,
        why: if ready == Some(false) { "not signed in".into() } else { String::new() },
        models: models.iter().map(|m| m.to_string()).collect(),
        complete: !models.is_empty(),
    }
}

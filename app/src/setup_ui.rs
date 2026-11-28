//! The desktop's side of the guided setup flows (`ligus_runtime::setup`,
//! docs/design/setup-flows.md): [`DesktopOps`] over the app's state, and the
//! commands the wizard calls.
//!
//! Secrets reach `setup_run` in their own argument, are wrapped in
//! `Zeroizing` at once, and go only to the secret store; nothing this module
//! returns carries one. The person steps (admitting a box with a passkey,
//! pairing a phone, creating an invite) stay in their own screens: the
//! engine only checks before and after them.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use ligus_core::WorkspaceRole;
use ligus_runtime::checks::Check;
use ligus_runtime::cli_detect::DetectEnv;
use ligus_runtime::model_catalog::{ModelList, ModelOption, ModelSource};
use ligus_runtime::setup::{
    self, service, AgentEdit, AgentFact, FlowId, FolderFact, HarnessFact, HostFact, HostSourceFact, Inputs, InviteFact, McpFact, OpsError, OpsResult, ProbeFact, RepoFact, RunOpts,
    RuntimeFact, RuntimeSpec, Secrets, SetupOps, Surface, TestTurn,
};
use ligus_runtime::{McpReach, McpServerSpec, McpTransport};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::AppState;

/// The engine's side on the desktop: the active workspace of this app.
pub(crate) struct DesktopOps<'a> {
    pub app: &'a AppHandle,
    ws: Uuid,
}

impl<'a> DesktopOps<'a> {
    pub fn new(app: &'a AppHandle) -> Self {
        let ws = app.state::<AppState>().active_workspace_id();
        DesktopOps { app, ws }
    }
    fn state(&self) -> State<'a, AppState> {
        self.app.state::<AppState>()
    }
    fn local(&self) -> bool {
        self.ws == self.state().local_workspace_id
    }
}

/// Copy this device's per-agent reach onto `servers` of workspace `ws`.
/// An unreadable reach file drops every server (fail-closed: never wider
/// than the person chose).
pub(crate) fn apply_reach(data_dir: &std::path::Path, ws: Uuid, servers: &mut Vec<McpServerSpec>) {
    match McpReach::load(data_dir) {
        Ok(r) => r.apply(ws, servers),
        Err(e) => {
            tracing::warn!("MCP reach unreadable, offering no MCP servers: {e}");
            servers.clear();
        }
    }
}

/// The servers a turn of `agent` may reach: this device's reach applied,
/// then only those that name the agent (or every agent). For the paths that
/// hand servers to a CLI agent directly (Claude Code's `--mcp-config`),
/// which don't go through `tool_set`.
pub(crate) fn servers_for_agent(data_dir: &std::path::Path, ws: Uuid, agent: Option<Uuid>, servers: &[McpServerSpec]) -> Vec<McpServerSpec> {
    let mut v = servers.to_vec();
    apply_reach(data_dir, ws, &mut v);
    v.retain(|s| s.reaches(agent));
    v
}

impl SetupOps for DesktopOps<'_> {
    fn surface(&self) -> Surface {
        Surface::Desktop
    }
    fn my_role(&self) -> Option<WorkspaceRole> {
        if self.local() {
            // "My workspace" on this device only: its owner.
            return Some(WorkspaceRole::Owner);
        }
        service::my_role(&self.state().service.lock().unwrap(), self.ws)
    }
    fn lockdown(&self) -> bool {
        service::lockdown(&self.state().service.lock().unwrap(), self.ws).0
    }
    fn lockdown_mcp_allowed(&self) -> Vec<String> {
        service::lockdown(&self.state().service.lock().unwrap(), self.ws).1
    }
    fn relay_url(&self) -> Option<String> {
        if self.local() {
            return None;
        }
        let s = self.state();
        let s = s.settings.lock().unwrap();
        s.workspaces.iter().find(|w| w.id() == self.ws).map(|w| w.relay_url.clone()).filter(|u| !u.trim().is_empty())
    }

    // agent
    fn harnesses(&self) -> Vec<HarnessFact> {
        service::harnesses(&DetectEnv::from_process(), false)
    }
    fn models(&self, harness: &str, endpoint: &str) -> ModelList {
        if harness == "claude-code" {
            // The desktop knows the account's own list (Claude Code's cache).
            let models = crate::claude_code_model_options()
                .into_iter()
                .filter(|(v, _)| !v.is_empty())
                .map(|(v, l)| ModelOption::new(v, Some(l)))
                .collect();
            return ModelList { models, source: ModelSource::Aliases, complete: false, note: None, error: None };
        }
        service::models(harness, endpoint, &DetectEnv::from_process())
    }
    fn runtimes(&self) -> Vec<RuntimeFact> {
        // This device's own runtimes (only those it trusts: a cloned repo's
        // stay inert), then the workspace's (other hosts can run those).
        let mut out: Vec<RuntimeFact> = self
            .state()
            .combined_runtimes()
            .into_iter()
            .filter_map(|r| {
                let gw = r.gateway.as_deref().is_some_and(|g| !g.trim().is_empty());
                let h = service::harness_of(r.provider_kind, gw)?;
                Some(RuntimeFact {
                    id: r.id.clone(),
                    harness: h.into(),
                    model: r.model_id.clone(),
                    endpoint: if gw {
                        r.gateway.clone().unwrap_or_default()
                    } else if h == "local" || h == "openai" {
                        r.endpoint.clone()
                    } else {
                        String::new()
                    },
                    shared: false,
                    secret_ref: String::new(),
                    provider: ligus_runtime::host_offers::harness_name(r.provider_kind),
                })
            })
            .collect();
        if !self.local() {
            let shared = service::workspace_runtimes(&self.state().service.lock().unwrap(), self.ws);
            let shared: Vec<RuntimeFact> = shared.into_iter().filter(|w| !out.iter().any(|r| r.id == w.id)).collect();
            out.extend(shared);
        }
        out
    }
    fn add_runtime(&mut self, spec: &RuntimeSpec) -> OpsResult<()> {
        if spec.shared {
            // Another host runs it: a workspace runtime (names and a secret
            // reference only; never a key or this device's addresses).
            return service::add_workspace_runtime(&mut self.state().service.lock().unwrap(), self.ws, spec);
        }
        let (provider, endpoint, gateway) = local_runtime_args(spec);
        crate::add_runtime(
            self.state(),
            spec.id.clone(),
            spec.name.clone(),
            provider,
            "local".into(),
            endpoint,
            spec.model.clone(),
            true,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            gateway,
        )
        .map_err(OpsError::Failed)
    }
    fn remove_runtime(&mut self, id: &str) -> OpsResult<()> {
        let shared = !self.local() && service::workspace_runtimes(&self.state().service.lock().unwrap(), self.ws).iter().any(|r| r.id == id);
        if shared {
            return self.state().service.lock().unwrap().remove_workspace_runtime(self.ws, id).map_err(OpsError::from);
        }
        crate::remove_runtime(self.state(), id.to_string()).map_err(OpsError::Failed)
    }
    fn runtime_on_log(&self, shared: bool) -> bool {
        shared
    }
    fn passkey_writes(&self) -> bool {
        !self.local()
    }
    fn confirming(&mut self, step: usize, of: usize, what: &str) {
        use tauri::Emitter;
        let _ = self.app.emit(CONFIRMING_EVENT, json!({ "step": step, "of": of, "what": what }));
    }
    fn host_sources(&self) -> Vec<HostSourceFact> {
        let state = self.state();
        let now = ligus_core::Timestamp::now();
        let presence = &state.host_presence;
        let svc = state.service.lock().unwrap();
        service::host_sources(&svc, self.ws, |id| presence.status_of(id), |h| presence.is_online(h, &now))
    }
    fn this_device_label(&self) -> String {
        if cfg!(target_os = "macos") { "this Mac".into() } else { "this computer".into() }
    }
    fn keeps_model_keys(&self) -> bool {
        true
    }
    fn has_model_key(&self, harness: &str) -> bool {
        let Some(name) = model_key_name(harness) else { return false };
        self.state().settings.lock().unwrap().provider_keys.get(name).is_some_and(|k| !k.trim().is_empty())
    }
    fn put_model_key(&mut self, harness: &str, key: &Zeroizing<String>) -> OpsResult<()> {
        let name = model_key_name(harness).ok_or_else(|| OpsError::Failed(format!("no key for {harness} here")))?;
        let state = self.state();
        let mut s = state.settings.lock().unwrap();
        s.provider_keys.insert(name.to_string(), key.trim().to_string());
        crate::save_settings(&state.data_dir, &s);
        Ok(())
    }
    fn agents(&self) -> Vec<AgentFact> {
        service::agents(&self.state().service.lock().unwrap(), self.ws)
    }
    fn member_names(&self) -> Vec<String> {
        service::member_names(&self.state().service.lock().unwrap(), self.ws)
    }
    fn add_agent(&mut self, name: &str, runtime_id: &str) -> OpsResult<Uuid> {
        service::add_agent(&mut self.state().service.lock().unwrap(), self.ws, name, runtime_id)
    }
    fn add_agent_placed(&mut self, name: &str, runtime_id: &str, host_id: &str) -> OpsResult<Uuid> {
        service::add_agent_placed(&mut self.state().service.lock().unwrap(), self.ws, name, runtime_id, host_id)
    }
    fn change_agent(&mut self, agent_id: Uuid, edit: &AgentEdit) -> OpsResult<()> {
        service::change_agent(&mut self.state().service.lock().unwrap(), self.ws, agent_id, edit)
    }
    fn rules(&self) -> u32 {
        if self.local() {
            return 0;
        }
        service::rules(&self.state().service.lock().unwrap(), self.ws)
    }
    fn local_servers(&self, agent_id: Uuid) -> bool {
        service::local_servers(&self.state().service.lock().unwrap(), self.ws, agent_id)
    }
    fn set_local_servers(&mut self, agent_id: Uuid, on: bool) -> OpsResult<()> {
        service::set_local_servers(&mut self.state().service.lock().unwrap(), self.ws, agent_id, on)
    }
    fn test_turn(&mut self, runtime_id: &str) -> OpsResult<TestTurn> {
        self.test_turn_with(runtime_id, false)
    }
    fn test_turn_with(&mut self, runtime_id: &str, local_servers: bool) -> OpsResult<TestTurn> {
        let state = self.state();
        let rt = state.resolve_runtime(runtime_id);
        let root = Some(state.workspace_root.lock().unwrap().clone()).filter(|r| !r.trim().is_empty());
        service::block_on(async move { service::ping_with(&rt, root.as_deref(), local_servers).await }).ok_or_else(|| OpsError::Failed("couldn't run the test turn".into()))
    }

    // tool
    fn mcp_servers(&self) -> Vec<McpFact> {
        let state = self.state();
        let reach = McpReach::load(&state.data_dir).unwrap_or_default();
        let tokens: Vec<String> = state.settings.lock().unwrap().mcp_bearer_tokens.keys().cloned().collect();
        state
            .combined_mcp_servers()
            .into_iter()
            .map(|s| McpFact {
                transport: match s.transport {
                    McpTransport::Stdio { .. } => "stdio".into(),
                    McpTransport::Http { .. } => "http".into(),
                },
                target: match &s.transport {
                    McpTransport::Stdio { command, args } => std::iter::once(command.clone()).chain(args.clone()).collect::<Vec<_>>().join(" "),
                    McpTransport::Http { url } => setup::validate::origin(url),
                },
                enabled: s.enabled,
                has_secret: tokens.contains(&s.id),
                agents: reach.get(self.ws, &s.id),
                id: s.id,
            })
            .collect()
    }
    fn mcp_handshake(&mut self, spec: &McpServerSpec, token: Option<&Zeroizing<String>>) -> OpsResult<Vec<String>> {
        service::mcp_handshake(spec, token)
    }
    fn put_mcp_secret(&mut self, id: &str, url: &str, secret: &Zeroizing<String>) -> OpsResult<()> {
        // Bound to the URL's origin: the token is attached only there.
        let state = self.state();
        let mut s = state.settings.lock().unwrap();
        crate::store_mcp_bearer_token(&mut s, id, url, secret).map_err(OpsError::Failed)?;
        crate::save_settings(&state.data_dir, &s);
        Ok(())
    }
    fn delete_mcp_secret(&mut self, id: &str, url: &str) -> OpsResult<()> {
        let state = self.state();
        let mut s = state.settings.lock().unwrap();
        crate::store_mcp_bearer_token(&mut s, id, url, "").map_err(OpsError::Failed)?;
        crate::save_settings(&state.data_dir, &s);
        Ok(())
    }
    fn add_mcp_server(&mut self, spec: &McpServerSpec, reach: &[Uuid]) -> OpsResult<()> {
        let state = self.state();
        // Reach first: an orphan reach only narrows a server that isn't there.
        let mut r = McpReach::load(&state.data_dir).map_err(OpsError::Failed)?;
        r.set(self.ws, &spec.id, reach);
        r.save(&state.data_dir).map_err(OpsError::Failed)?;
        let mut server = spec.clone();
        server.auth = None;
        server.agents.clear();
        crate::add_managed_mcp_server(&state, server).map_err(OpsError::Failed)
    }
    fn remove_mcp_server(&mut self, id: &str) -> OpsResult<()> {
        crate::remove_mcp_server(self.state(), id.to_string()).map_err(OpsError::Failed)
    }

    // folder
    fn repos(&self) -> Vec<RepoFact> {
        service::repos(&self.state().service.lock().unwrap(), self.ws)
    }
    fn inspect_folder(&self, path: &str) -> FolderFact {
        service::inspect_folder(&self.state().service.lock().unwrap(), self.ws, path)
    }
    fn add_repo(&mut self, folder: &FolderFact, name: &str, branch: &str) -> OpsResult<String> {
        service::add_repo(&mut self.state().service.lock().unwrap(), self.ws, folder, name, branch)
    }
    fn bind_folder(&mut self, repo_id: &str, path: &str) -> OpsResult<()> {
        service::bind_folder(&self.state().service.lock().unwrap(), self.ws, &crate::repos_ui::host_label(), repo_id, path)
    }
    fn create_scratch(&mut self, name: &str) -> OpsResult<(String, String)> {
        let root = self.state().data_dir.join("scratch");
        service::create_scratch(&mut self.state().service.lock().unwrap(), self.ws, &crate::repos_ui::host_label(), &root, &[], name)
    }

    // box, phone, invite
    fn hosts(&self) -> Vec<HostFact> {
        service::hosts(&self.state().service.lock().unwrap(), self.ws)
    }
    fn phone_precheck(&self) -> OpsResult<Vec<Check>> {
        let reasons = crate::phone_enroll_ui::precheck_for(self.app);
        Ok(reasons
            .into_iter()
            .map(|(code, text, blocking)| {
                let id = format!("phone.precheck.{code}");
                if blocking {
                    Check::fail(&id, "phone", text).fix("Add a phone explains it; fix that first")
                } else {
                    Check::warn(&id, "phone", text)
                }
            })
            .collect())
    }
    fn phones(&self) -> Vec<String> {
        service::phones(&self.state().service.lock().unwrap(), self.ws)
    }
    fn invites(&self) -> Vec<InviteFact> {
        service::invites(&self.state().service.lock().unwrap(), self.ws)
    }

    // relay
    fn probe_relay(&self, url: &str, token: Option<&Zeroizing<String>>) -> ProbeFact {
        // The saved access token goes only to the relay it was saved for.
        let current = self.relay_url();
        let (saved, room) = {
            let state = self.state();
            let s = state.settings.lock().unwrap();
            (s.relay_access_token.clone().map(Zeroizing::new), s.sync_room.clone())
        };
        let same = current.as_deref().is_some_and(|r| setup::validate::same_origin(r, url));
        service::probe_relay(url, token.or(saved.as_ref().filter(|_| same)), &room)
    }
    fn switch_relay(&mut self, url: &str, token: Option<&Zeroizing<String>>) -> OpsResult<()> {
        if self.local() {
            return Err(OpsError::Failed("My workspace stays on this device; create a shared workspace to use a relay".into()));
        }
        // The live settings and the workspace's entry change together, in one
        // save (crate::mirror_relay_into_synced_workspace).
        let moved = !self.relay_url().as_deref().is_some_and(|r| setup::validate::same_origin(r, url));
        let state = self.state();
        let mut s = state.settings.lock().unwrap();
        s.relay_url = Some(url.to_string());
        match token {
            Some(t) => s.relay_access_token = Some(t.to_string()),
            // The old relay's token never follows the workspace to another.
            None if moved => s.relay_access_token = None,
            None => {}
        }
        let room = s.sync_room.clone();
        if !s.joined_rooms.iter().any(|r| r == &room) {
            s.joined_rooms.push(room);
        }
        crate::mirror_relay_into_synced_workspace(&mut s);
        crate::save_settings(&state.data_dir, &s);
        Ok(())
    }
}

/// The event the Add an agent sheet listens to while a run asks for the
/// passkey: `{step, of, what}` ("Confirm 1 of 2: Define gw-home for @coder").
pub(crate) const CONFIRMING_EVENT: &str = "setup-confirming";

/// A device-local runtime's provider, endpoint and gateway for `spec`. The
/// provider is the spec's (a gateway's follows its wire), never a default
/// such as Ollama: a gateway runtime has no endpoint of its own.
pub(crate) fn local_runtime_args(spec: &RuntimeSpec) -> (String, String, Option<String>) {
    let provider = if spec.provider.is_empty() {
        match spec.harness.as_str() {
            "claude-code" => "claudecode".to_string(),
            "local" => "ollama".to_string(),
            "gateway" => "custom".to_string(),
            h => h.to_string(),
        }
    } else {
        spec.provider.clone()
    };
    match spec.harness.as_str() {
        "local" | "openai" => (provider, spec.endpoint.clone(), None),
        "gateway" => (provider, String::new(), Some(spec.endpoint.clone())),
        _ => (provider, String::new(), None),
    }
}

/// This device's settings key for a hosted API's model key.
fn model_key_name(harness: &str) -> Option<&'static str> {
    match harness {
        "anthropic" => Some("anthropic"),
        "openai" => Some("openAI"),
        "openrouter" => Some("openRouter"),
        _ => None,
    }
}

// ── commands ────────────────────────────────────────────────────────────────

/// Cancel flags of running flows, by flow id.
fn cancels() -> &'static Mutex<HashMap<&'static str, Arc<AtomicBool>>> {
    static C: OnceLock<Mutex<HashMap<&'static str, Arc<AtomicBool>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn flow_of(id: &str) -> Result<FlowId, String> {
    FlowId::parse(id).ok_or_else(|| format!("unknown setup flow '{id}'"))
}

/// The seven flows (for the palette, `/setup` and the wizard).
#[tauri::command]
pub(crate) fn setup_flows() -> Value {
    json!(setup::flows())
}

/// What each flow has done in the active workspace.
#[tauri::command]
pub(crate) async fn setup_status(ligus_app: AppHandle) -> Result<Value, String> {
    crate::off_main(ligus_app, move |app| Ok(json!(setup::status(&DesktopOps::new(app))))).await
}

/// A flow's preflight and steps for the current inputs (the models of the
/// chosen harness, the agents a tool may reach).
#[tauri::command]
pub(crate) async fn setup_form(ligus_app: AppHandle, flow: String, inputs: Option<BTreeMap<String, String>>) -> Result<Value, String> {
    let flow = flow_of(&flow)?;
    crate::off_main(ligus_app, move |app| {
        let form = setup::form(flow, &DesktopOps::new(app), &Inputs(inputs.unwrap_or_default()));
        serde_json::to_value(&form).map_err(|e| e.to_string())
    })
    .await
}

/// Run a flow. `secrets` (the wizard's masked fields) are wrapped in
/// `Zeroizing` here and never returned, logged or stored outside the secret
/// store.
#[tauri::command]
pub(crate) async fn setup_run(
    ligus_app: AppHandle,
    flow: String,
    inputs: Option<BTreeMap<String, String>>,
    secrets: Option<BTreeMap<String, String>>,
    dry_run: Option<bool>,
) -> Result<Value, String> {
    let flow = flow_of(&flow)?;
    let mut s = Secrets::new();
    for (k, v) in secrets.unwrap_or_default() {
        s.set(&k, Zeroizing::new(v));
    }
    let cancel = Arc::new(AtomicBool::new(false));
    cancels().lock().unwrap().insert(flow.as_str(), cancel.clone());
    let dry_run = dry_run.unwrap_or(false);
    let out = crate::off_main(ligus_app, move |app| {
        let mut ops = DesktopOps::new(app);
        setup::run(flow, &mut ops, &Inputs(inputs.unwrap_or_default()), &s, &RunOpts { dry_run, cancel: Some(cancel), ..Default::default() })
    })
    .await;
    cancels().lock().unwrap().remove(flow.as_str());
    Ok(out.to_json(dry_run))
}

/// Stop a running flow between steps (its own writes are undone).
#[tauri::command]
pub(crate) fn setup_cancel(flow: String) -> Result<(), String> {
    let flow = flow_of(&flow)?;
    if let Some(c) = cancels().lock().unwrap().get(flow.as_str()) {
        c.store(true, Ordering::SeqCst);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_malformed_reach_file_offers_no_servers() {
        let d = tempfile::tempdir().unwrap();
        let ws = Uuid::new_v4();
        let mut servers = vec![McpServerSpec::new("a", McpTransport::Http { url: "https://m.example.com".into() })];
        apply_reach(d.path(), ws, &mut servers);
        assert_eq!(servers.len(), 1, "no file: every agent");
        std::fs::write(d.path().join(McpReach::FILE), b"{").unwrap();
        apply_reach(d.path(), ws, &mut servers);
        assert!(servers.is_empty(), "unreadable: nothing, never everything");
    }

    /// Claude Code gets its servers on the command line, not through
    /// `tool_set`: the reach must narrow that list too.
    #[test]
    fn claude_code_servers_follow_the_reach() {
        let d = tempfile::tempdir().unwrap();
        let (ws, a, b) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let servers = vec![
            McpServerSpec::new("limited", McpTransport::Stdio { command: "x".into(), args: vec![] }),
            McpServerSpec::new("open", McpTransport::Stdio { command: "y".into(), args: vec![] }),
        ];
        let mut r = McpReach::default();
        r.set(ws, "limited", &[a]);
        r.save(d.path()).unwrap();
        let ids = |agent| servers_for_agent(d.path(), ws, agent, &servers).into_iter().map(|s| s.id).collect::<Vec<_>>();
        assert_eq!(ids(Some(a)), ["limited", "open"]);
        assert_eq!(ids(Some(b)), ["open"]);
        assert_eq!(ids(None), ["open"], "the default agent gets only every-agent servers");
    }

    /// A gateway runtime's client follows the gateway's wire and has no
    /// endpoint of its own: never Ollama with localhost:11434.
    #[test]
    fn a_gateway_runtime_follows_its_wire_never_ollama() {
        let spec = |harness: &str, provider: &str, endpoint: &str| RuntimeSpec {
            id: "x".into(),
            harness: harness.into(),
            provider: provider.into(),
            endpoint: endpoint.into(),
            ..Default::default()
        };
        assert_eq!(local_runtime_args(&spec("gateway", "anthropic", "gw-claude")), ("anthropic".into(), String::new(), Some("gw-claude".into())));
        assert_eq!(local_runtime_args(&spec("gateway", "custom", "gw-home")), ("custom".into(), String::new(), Some("gw-home".into())));
        assert_eq!(local_runtime_args(&spec("gateway", "", "gw-home")).0, "custom");
        assert_eq!(local_runtime_args(&spec("local", "ollama", "http://localhost:11434")), ("ollama".into(), "http://localhost:11434".into(), None));
        assert_eq!(local_runtime_args(&spec("openai", "openAI", "https://api.openai.com/v1")).1, "https://api.openai.com/v1");
        assert_eq!(local_runtime_args(&spec("claude-code", "claudeCode", "")), ("claudeCode".into(), String::new(), None));
        for (h, p, e) in [("gateway", "custom", "gw"), ("gateway", "anthropic", "gw")] {
            assert_ne!(local_runtime_args(&spec(h, p, e)).0.to_lowercase(), "ollama");
        }
        assert_eq!(model_key_name("openai"), Some("openAI"));
        assert_eq!(model_key_name("claude-code"), None);
    }

    /// The agent flow's form for the sheet: one list, a default source and
    /// where it runs, the one button's label.
    #[test]
    fn the_agent_form_serializes_for_the_sheet() {
        struct Ops;
        impl SetupOps for Ops {
            fn surface(&self) -> Surface {
                Surface::Desktop
            }
            fn my_role(&self) -> Option<WorkspaceRole> {
                Some(WorkspaceRole::Owner)
            }
            fn keeps_model_keys(&self) -> bool {
                true
            }
            fn passkey_writes(&self) -> bool {
                true
            }
        }
        let v = serde_json::to_value(setup::form(FlowId::Agent, &Ops, &Inputs::new().with("name", "coder").with("source", "local").with("model", "llama3:8b"))).unwrap();
        let fields: Vec<Value> = v["steps"].as_array().unwrap().iter().flat_map(|s| s["fields"].as_array().cloned().unwrap_or_default()).collect();
        let ids: Vec<&str> = fields.iter().map(|f| f["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["name", "source", "endpoint", "api-key", "local-servers", "model", "runs-on", "test-turn"]);
        assert_eq!(v["action"], "Add @coder");
        assert_eq!(v["confirmations"], 1);
        let runs_on = fields.iter().find(|f| f["id"] == "runs-on").unwrap();
        assert_eq!(runs_on["default"], "here");
        assert_eq!(runs_on["note"], "Turns run on this device while Ligus is open.");
        let source = fields.iter().find(|f| f["id"] == "source").unwrap();
        assert!(source["options"].as_array().unwrap().iter().any(|o| o["value"] == "key:anthropic"));
    }

    #[test]
    fn the_flows_serialize_for_the_wizard() {
        let v = setup_flows();
        let ids: Vec<&str> = v.as_array().unwrap().iter().map(|f| f["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["agent", "tool", "folder", "box", "phone", "invite", "relay"]);
        assert!(v[0]["title"].is_string() && v[0]["why"].is_string());
        assert_eq!(v[5]["minRole"], "Admin");
    }

    /// The web's palette and `/setup` names (web/src/lib/setup.ts) are the
    /// engine's.
    #[test]
    fn web_flow_names_match_the_engine() {
        let ts = include_str!("../../web/src/lib/setup.ts");
        for f in setup::flows() {
            let want = format!("{{ id: \"{}\", title: \"{}\"", f.id.as_str(), f.title);
            assert!(ts.contains(&want), "web/src/lib/setup.ts lacks {want}");
        }
    }

    /// A secret given to `setup_run` never comes back in what it returns.
    #[test]
    fn secret_canary_absent_from_the_outcome() {
        use ligus_runtime::setup::Outcome;
        let canary = "canary-desktop-9b1e";
        struct NoOps;
        impl SetupOps for NoOps {
            fn surface(&self) -> Surface {
                Surface::Desktop
            }
            fn my_role(&self) -> Option<WorkspaceRole> {
                Some(WorkspaceRole::Owner)
            }
        }
        let mut s = Secrets::new();
        s.set("token", Zeroizing::new(canary.into()));
        for flow in FlowId::ALL {
            let o: Outcome = setup::run(flow, &mut NoOps, &Inputs::new().with("kind", "own").with("url", "https://r.example.com"), &s, &RunOpts::default());
            assert!(!o.to_json(false).to_string().contains(canary), "{flow:?}");
        }
    }
}

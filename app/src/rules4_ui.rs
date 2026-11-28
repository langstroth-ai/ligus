//! Settings → Workspace rules (workspace model v3, 15.2), and the notices
//! that come with rules 4 (5.5, 11, M13).
//!
//! - [`workspace_rules4_status`]: what switching changes and the readiness
//!   list (devices that go read-only, by name and build), the agents a
//!   person here may pin to what their runtime runs, and the runtimes whose
//!   server address is on the log.
//! - [`workspace_rules_upgrade`]: the Owner's switch, one passkey, never back.
//! - [`runtime_endpoint_move`]: "Move it to this computer": the address goes
//!   into this computer's `host.toml` as a model server it offers by name,
//!   and this person's agents on it are pointed at that name (rules 4).
//! - [`chat_old_agent_copies`]: the "old copy of @x in this chat" notice
//!   (Admins, rules 4).

use serde::Serialize;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

use crate::AppState;

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceBuildDto {
    name: String,
    person: String,
    build: Option<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EndpointRuntimeDto {
    runtime_id: String,
    name: String,
    /// Agents naming it: their ids and names.
    agents: Vec<EndpointAgentDto>,
    /// This person can move it here (rules 4, at least one of their agents
    /// runs on this computer or their own devices).
    movable_here: bool,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EndpointAgentDto {
    id: String,
    name: String,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PinOfferDto {
    agent_id: String,
    name: String,
    /// "Pin @coder to Claude Code / opus".
    label: String,
    mine: bool,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Rules4StatusDto {
    rules: u32,
    is_owner: bool,
    /// What the switch changes, one plain line each.
    changes: Vec<String>,
    /// The readiness list, one plain line each (empty: nothing to say).
    readiness: Vec<String>,
    old_devices: Vec<DeviceBuildDto>,
    unknown_devices: Vec<DeviceBuildDto>,
    endpoint_runtimes: Vec<EndpointRuntimeDto>,
    /// On rules 4: agents a person here may pin to their source.
    pins: Vec<PinOfferDto>,
}

fn device_dto(d: &ligus_runtime::chat_service::DeviceBuild) -> DeviceBuildDto {
    DeviceBuildDto { name: d.name.clone(), person: d.person.clone(), build: d.build.clone() }
}

/// The rules status of the workspace the view shows.
#[tauri::command]
pub async fn workspace_rules4_status(ligus_app: AppHandle, workspace_id: String) -> Result<Rules4StatusDto, String> {
    crate::off_main(ligus_app, move |app| {
        let state = app.state::<AppState>();
        let ws = crate::shown_workspace(&state, &workspace_id)?;
        let report = state.service.lock().unwrap().rules4_report(ws).map_err(|e| e.to_string())?;
        let pins = if report.rules >= ligus_core::authz::RULES_V4 {
            ligus_runtime::setup::pin_offers(&crate::setup_ui::DesktopOps::new(app))
                .into_iter()
                .map(|p| PinOfferDto { agent_id: p.agent_id.to_string(), name: p.name.clone(), label: p.label(), mine: p.mine })
                .collect()
        } else {
            Vec::new()
        };
        let rules4 = report.rules >= ligus_core::authz::RULES_V4;
        Ok(Rules4StatusDto {
            rules: report.rules,
            is_owner: report.is_owner,
            changes: ligus_runtime::chat_service::RULES4_CHANGES.iter().map(|s| s.to_string()).collect(),
            readiness: if rules4 { Vec::new() } else { report.lines() },
            old_devices: report.old_devices.iter().map(device_dto).collect(),
            unknown_devices: report.unknown_devices.iter().map(device_dto).collect(),
            endpoint_runtimes: report
                .endpoint_runtimes
                .iter()
                .map(|r| EndpointRuntimeDto {
                    runtime_id: r.runtime_id.clone(),
                    name: r.name.clone(),
                    agents: r.agents.iter().map(|(id, name, _, _)| EndpointAgentDto { id: id.to_string(), name: name.clone() }).collect(),
                    movable_here: rules4 && r.agents.iter().any(|(_, _, mine, here)| *mine && *here),
                })
                .collect(),
            pins,
        })
    })
    .await
}

/// Switch the workspace the view shows to rules 4: the Owner, one passkey.
#[tauri::command]
pub async fn workspace_rules_upgrade(app: AppHandle, workspace_id: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let ws = crate::shown_workspace(&state, &workspace_id)?;
        let mut svc = state.service.lock().unwrap();
        svc.upgrade_workspace_rules(ws).map(|e| e.is_some()).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A name for a model server this computer offers: the runtime's name as a
/// source name (`[a-z0-9-]`), unique among `taken`.
pub(crate) fn local_source_name(runtime_name: &str, runtime_id: &str, taken: &[String]) -> String {
    let slug = |s: &str| {
        let mut o = String::new();
        for c in s.chars() {
            if c.is_ascii_alphanumeric() {
                o.push(c.to_ascii_lowercase());
            } else if !o.is_empty() && !o.ends_with('-') {
                o.push('-');
            }
        }
        o.trim_matches('-').chars().take(40).collect::<String>().trim_matches('-').to_string()
    };
    let mut base = slug(runtime_name);
    if base.is_empty() {
        base = slug(runtime_id);
    }
    if base.is_empty() {
        base = "model-server".into();
    }
    if !taken.contains(&base) {
        return base;
    }
    (2..).map(|n| format!("{base}-{n}")).find(|c| !taken.contains(c)).unwrap_or(base)
}

/// The harness a model server's runtime speaks, for a `host.toml` entry.
pub(crate) fn local_harness(provider: ligus_core::ModelProviderKind) -> Option<&'static str> {
    match provider {
        ligus_core::ModelProviderKind::Ollama => Some("ollama"),
        ligus_core::ModelProviderKind::OpenAI | ligus_core::ModelProviderKind::Custom => Some("openai"),
        _ => None,
    }
}

/// "Move it to this computer" (5.5, rules 4): the runtime's address goes
/// into this computer's `host.toml` (it stays here), this computer offers it
/// by name, and this person's agents that name the runtime and run here (or
/// on their own devices) carry `{source: <name>, model}` instead, each with
/// a passkey (it changes what the agent names). The workspace runtime
/// itself is left for an Admin to remove. Returns how many agents moved.
#[tauri::command]
pub async fn runtime_endpoint_move(app: AppHandle, workspace_id: String, runtime_id: String) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let ws = crate::shown_workspace(&state, &workspace_id)?;
        let (rt, agents, rules, host_label) = {
            let svc = state.service.lock().unwrap();
            let rules = ligus_runtime::setup::service::rules(&svc, ws);
            let rt = svc
                .list_workspace_runtimes(ws)
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|r| r.id == runtime_id && !r.endpoint.trim().is_empty())
                .ok_or("that runtime has no server address on the workspace")?;
            let agents = svc
                .endpoint_runtimes(ws)
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|r| r.runtime_id == runtime_id)
                .map(|r| r.agents)
                .unwrap_or_default();
            let here = svc.device_id().to_string();
            let label = svc.list_hosts(ws).unwrap_or_default().into_iter().find(|h| h.id == here).map(|h| h.label).unwrap_or_default();
            (rt, agents, rules, label)
        };
        if rules < ligus_core::authz::RULES_V4 {
            return Err("upgrade the workspace rules first: on this workspace an agent's model belongs to its runtime".into());
        }
        let harness = local_harness(rt.provider).ok_or("only a model server's address can move to this computer")?;
        // Only a web address goes into host.toml (it was an Admin's word on
        // the log): http or https, nothing else.
        let scheme = rt.endpoint.trim().split("://").next().unwrap_or("").to_ascii_lowercase();
        if !matches!(scheme.as_str(), "http" | "https") || !rt.endpoint.contains("://") {
            return Err("that address isn't an http or https URL".into());
        }
        // The address stays here: one `[[local]]` entry (reused when it is
        // already listed).
        let mut cfg = ligus_runtime::host_config::HostConfig::load(&state.data_dir).map_err(|e| e.to_string())?;
        let name = match cfg.local.iter().find(|l| l.url.trim_end_matches('/') == rt.endpoint.trim_end_matches('/')) {
            Some(l) => l.name.clone(),
            None => {
                let taken: Vec<String> = cfg.local.iter().map(|l| l.name.clone()).collect();
                let name = local_source_name(&rt.name, &rt.id, &taken);
                cfg.local.push(ligus_runtime::host_config::LocalSource { name: name.clone(), harness: harness.into(), url: rt.endpoint.clone() });
                cfg.save(&state.data_dir).map_err(|e| e.to_string())?;
                name
            }
        };
        ligus_runtime::host_config::install(cfg);
        ligus_runtime::host_offers::invalidate_status();
        let mut svc = state.service.lock().unwrap();
        // This computer now offers it by name (G2 reads the offers).
        let here = svc.device_id().to_string();
        let label = if host_label.trim().is_empty() { crate::computer_name().unwrap_or_else(|| "Desktop".into()) } else { host_label };
        let mut host = ligus_core::WorkspaceHost::new(here, ligus_core::HostKind::Device, label);
        host.offers = Some(ligus_runtime::host_offers::offers_now(ligus_runtime::host_offers::HostRole::OwnComputer, Some(ws)));
        svc.ensure_host(ws, host).map_err(|e| e.to_string())?;
        let mut moved = 0;
        for (id, _, mine, runs_here) in agents {
            if !mine || !runs_here {
                continue;
            }
            let run = ligus_core::agent_run::AgentRun { harness: harness.into(), source: name.clone(), model: rt.model.clone(), resolved: None };
            svc.edit_agent(ws, id, None, Some(run), None).map_err(|e| e.to_string())?;
            moved += 1;
        }
        Ok(moved)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OldAgentCopyDto {
    id: String,
    name: String,
}

/// The agents chat `session_id` still carries an old copy of, for its
/// notice: only for an Admin, on rules 4 (where the chat runs the
/// workspace's record).
#[tauri::command]
pub async fn chat_old_agent_copies(ligus_app: AppHandle, session_id: String) -> Result<Vec<OldAgentCopyDto>, String> {
    crate::off_main(ligus_app, move |app| {
        let state = app.state::<AppState>();
        let ws = state.active_workspace_id();
        let chat = Uuid::parse_str(&session_id).map_err(|e| e.to_string())?;
        let svc = state.service.lock().unwrap();
        let rules = ligus_runtime::setup::service::rules(&svc, ws);
        let admin = svc.my_workspace_role(ws).is_some_and(|r| r.rank() >= ligus_core::WorkspaceRole::Admin.rank());
        if rules < ligus_core::authz::RULES_V4 || !admin {
            return Ok(Vec::new());
        }
        Ok(svc.old_agent_copies(ws, chat).map_err(|e| e.to_string())?.into_iter().map(|(id, name)| OldAgentCopyDto { id: id.to_string(), name }).collect())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_servers_name_is_a_source_name_and_unique() {
        assert_eq!(local_source_name("Ollama (llama3:8b)", "ollama-llama3", &[]), "ollama-llama3-8b");
        assert_eq!(local_source_name("Ollama", "x", &["ollama".into()]), "ollama-2");
        assert_eq!(local_source_name("   ", "Local_Server", &[]), "local-server");
        assert_eq!(local_source_name("", "", &[]), "model-server");
        let n = local_source_name(&"a".repeat(90), "x", &[]);
        assert!(n.len() <= 40 && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
    }

    #[test]
    fn only_a_model_servers_address_moves() {
        assert_eq!(local_harness(ligus_core::ModelProviderKind::Ollama), Some("ollama"));
        assert_eq!(local_harness(ligus_core::ModelProviderKind::Custom), Some("openai"));
        assert_eq!(local_harness(ligus_core::ModelProviderKind::ClaudeCode), None);
    }

    #[test]
    fn the_status_serializes_for_settings() {
        let dto = Rules4StatusDto {
            rules: 3,
            is_owner: true,
            changes: vec!["x".into()],
            readiness: vec!["lt on mbp (Ana) is on 0.4.1: it goes read-only here until it's updated".into()],
            old_devices: vec![DeviceBuildDto { name: "lt on mbp".into(), person: "Ana".into(), build: Some("0.4.1".into()) }],
            unknown_devices: Vec::new(),
            endpoint_runtimes: Vec::new(),
            pins: Vec::new(),
        };
        let v = serde_json::to_value(&dto).unwrap();
        assert_eq!(v["isOwner"], true);
        assert_eq!(v["oldDevices"][0]["build"], "0.4.1");
        assert!(v["readiness"][0].as_str().unwrap().contains("0.4.1"));
    }
}

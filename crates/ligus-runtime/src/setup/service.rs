//! [`SetupOps`](super::SetupOps) pieces over a [`ChatService`], shared by the
//! CLI, the desktop and lt so they read and write the workspace the same
//! way. Each takes the service and the workspace id.

use std::path::Path;
use std::time::Duration;

use ligus_core::{workspace_config_session_id, ModelProviderKind, WorkspaceAgent, WorkspaceRole};
use uuid::Uuid;
use zeroize::Zeroizing;

use super::{AgentFact, FolderFact, HarnessFact, HostFact, InviteFact, OpsError, OpsResult, ProbeFact, RepoFact};
use crate::cli_detect::{self, DetectEnv, LoginState};
use crate::model_catalog::{ModelList, ModelOption, ModelSource};
use crate::event_store::CheckoutSource;
use crate::ChatService;

fn failed(e: impl std::fmt::Display) -> OpsError {
    OpsError::Failed(crate::secret_redact::redacted(&e.to_string()))
}

/// Run `fut` to completion on a fresh current-thread runtime in its own
/// thread (safe whether or not the caller is inside a runtime).
pub fn block_on<F: std::future::Future + Send + 'static>(fut: F) -> Option<F::Output>
where
    F::Output: Send + 'static,
{
    std::thread::spawn(move || tokio::runtime::Builder::new_current_thread().enable_all().build().ok().map(|rt| rt.block_on(fut)))
        .join()
        .ok()
        .flatten()
}

// ── context ─────────────────────────────────────────────────────────────────

pub fn my_role(svc: &ChatService, ws: Uuid) -> Option<WorkspaceRole> {
    svc.my_workspace_role(ws)
}

/// Dearth mode: on, and the MCP servers it still allows.
pub fn lockdown(svc: &ChatService, ws: Uuid) -> (bool, Vec<String>) {
    match svc.workspace_guards(ws).ok().and_then(|g| g.dearth) {
        Some(d) if d.enabled => (true, d.allowed_mcp_servers),
        _ => (false, Vec::new()),
    }
}

// ── agent ───────────────────────────────────────────────────────────────────

pub fn agents(svc: &ChatService, ws: Uuid) -> Vec<AgentFact> {
    svc.load_in(ws, workspace_config_session_id(ws))
        .ok()
        .flatten()
        .map(|s| s.workspace_agents.into_iter().map(|a| AgentFact { id: a.id, name: a.name, runtime_id: a.runtime_id }).collect())
        .unwrap_or_default()
}

pub fn member_names(svc: &ChatService, ws: Uuid) -> Vec<String> {
    svc.load_in(ws, workspace_config_session_id(ws))
        .ok()
        .flatten()
        .map(|s| s.members.into_iter().map(|m| m.actor.display_name).filter(|n| !n.trim().is_empty()).collect())
        .unwrap_or_default()
}

/// Add an agent owned by this device's author.
pub fn add_agent(svc: &mut ChatService, ws: Uuid, name: &str, runtime_id: &str) -> OpsResult<Uuid> {
    let mut agent = WorkspaceAgent::new(name, runtime_id);
    agent.owner_actor_id = svc.author().id.clone();
    let id = agent.id;
    svc.add_agent(ws, ws, agent).map_err(|e| match e {
        crate::chat_service::ChatError::Unauthorized(m) => OpsError::Refused(m),
        e => failed(e),
    })?;
    Ok(id)
}

/// The provider kind a harness id runs on.
pub fn provider_of(harness: &str) -> Option<ModelProviderKind> {
    Some(match harness {
        "claude-code" => ModelProviderKind::ClaudeCode,
        "codex" => ModelProviderKind::Codex,
        "copilot" => ModelProviderKind::Copilot,
        "antigravity" => ModelProviderKind::Antigravity,
        "aider" => ModelProviderKind::Aider,
        "pi" => ModelProviderKind::Pi,
        "local" => ModelProviderKind::Ollama,
        "gateway" => ModelProviderKind::Custom,
        _ => return None,
    })
}

/// The harness id of a provider kind (the inverse of [`provider_of`] for
/// the kinds setup offers).
pub fn harness_of(kind: ModelProviderKind, gateway: bool) -> Option<&'static str> {
    Some(match kind {
        _ if gateway => "gateway",
        ModelProviderKind::ClaudeCode => "claude-code",
        ModelProviderKind::Codex => "codex",
        ModelProviderKind::Copilot => "copilot",
        ModelProviderKind::Antigravity => "antigravity",
        ModelProviderKind::Aider => "aider",
        ModelProviderKind::Pi => "pi",
        ModelProviderKind::Ollama => "local",
        _ => return None,
    })
}

/// Every harness on this host: installed, version, sign-in (never a token).
/// `probe`: also confirm agy's sign-in with `agy models` (read-only).
pub fn harnesses(env: &DetectEnv, probe: bool) -> Vec<HarnessFact> {
    let mut out = Vec::new();
    let file_cli = |id: &str, label: &str, bin: &str, kind: ModelProviderKind, login: &str, install: &str| {
        let path = cli_detect::find_on_path(bin, &env.path);
        let signed = crate::executor::cli_home::expects_file_sign_in(kind, &env.home);
        HarnessFact {
            id: id.into(),
            label: label.into(),
            installed: path.is_some(),
            login: if path.is_none() {
                LoginState::Unknown
            } else if signed {
                LoginState::LoggedIn
            } else {
                LoginState::NotLoggedIn
            },
            login_detail: if signed { "signed in (credentials found)".into() } else { "no sign-in found".into() },
            login_command: login.into(),
            install_command: install.into(),
            version: path.as_deref().and_then(|p| version_of(p)),
        }
    };
    out.push(file_cli("claude-code", "Claude Code", "claude", ModelProviderKind::ClaudeCode, "claude", "npm install -g @anthropic-ai/claude-code"));
    out.push(file_cli("codex", "Codex", "codex", ModelProviderKind::Codex, "codex login", "npm install -g @openai/codex"));
    for c in cli_detect::detect_cli_agents(env, probe) {
        if !c.supported {
            continue;
        }
        out.push(HarnessFact {
            id: c.id,
            label: c.label,
            installed: c.installed,
            login: c.login,
            login_detail: c.login_detail,
            login_command: c.login_command,
            install_command: c.install_command,
            version: c.version,
        });
    }
    // Aider and Pi take a provider key from their own config: nothing to
    // sign in to here.
    for (id, label, bin, install) in [("aider", "Aider", "aider", "pipx install aider-chat"), ("pi", "Pi", "pi", "npm install -g @mariozechner/pi-coding-agent")] {
        let path = cli_detect::find_on_path(bin, &env.path);
        out.push(HarnessFact {
            id: id.into(),
            label: label.into(),
            installed: path.is_some(),
            login: LoginState::Unknown,
            login_detail: "uses its own provider key".into(),
            login_command: String::new(),
            install_command: install.into(),
            version: path.as_deref().and_then(|p| version_of(p)),
        });
    }
    out
}

fn version_of(p: &Path) -> Option<String> {
    let out = std::process::Command::new(p).arg("--version").stdin(std::process::Stdio::null()).output().ok()?;
    cli_detect::parse_version(&String::from_utf8_lossy(&out.stdout))
}

/// The models a harness offers. Claude Code: its aliases; agy, Copilot,
/// Codex: their own lists; a local endpoint: what it serves.
pub fn models(harness: &str, endpoint: &str, env: &DetectEnv) -> ModelList {
    match harness {
        "claude-code" => ModelList {
            models: ["sonnet", "opus", "haiku"].iter().map(|m| ModelOption::new(*m, None)).collect(),
            source: ModelSource::Aliases,
            complete: false,
            note: Some("aliases Claude Code always understands; leave empty for its default".into()),
            error: None,
        },
        "codex" | "copilot" | "antigravity" => {
            let kind = provider_of(harness).unwrap_or(ModelProviderKind::Codex);
            let bin = match harness {
                "antigravity" => "agy",
                h => h,
            };
            crate::model_catalog::cli_models(kind, bin, env)
        }
        "local" => {
            let req = crate::model_catalog::HttpListing { provider: ModelProviderKind::Ollama, endpoint: endpoint.to_string(), api_key: None };
            block_on(async move { tokio::time::timeout(Duration::from_secs(8), crate::model_catalog::http_models(req)).await })
                .and_then(Result::ok)
                .unwrap_or_else(|| ModelList { error: Some("didn't answer in 8 s".into()), ..ModelList::none("type a model name") })
        }
        _ => ModelList::none("type a model name"),
    }
}

// ── tool ────────────────────────────────────────────────────────────────────

/// Start the server under this host's agent sandbox and list its tools
/// (20 s at most). The spec is used as given (enabled for the handshake).
pub fn mcp_handshake(spec: &crate::mcp::McpServerSpec, token: Option<&Zeroizing<String>>) -> OpsResult<Vec<String>> {
    let mut spec = spec.clone();
    spec.enabled = true;
    spec.agents.clear();
    if let Some(t) = token {
        spec.auth = crate::mcp::normalize_bearer_token(t).map_err(OpsError::Failed)?;
    }
    let id = spec.id.clone();
    let reg = crate::mcp::McpRegistry::new(vec![spec]);
    let out = block_on(async move { tokio::time::timeout(Duration::from_secs(20), reg.list_tools(&id)).await });
    match out {
        Some(Ok(Ok(tools))) => Ok(tools.into_iter().map(|t| t.name).collect()),
        Some(Ok(Err(e))) => {
            let m = crate::secret_redact::redacted(&e.to_string());
            Err(if m.contains("connect") || m.contains("dns") || m.contains("timed out") { OpsError::Offline(m) } else { OpsError::Failed(m) })
        }
        Some(Err(_)) => Err(OpsError::Offline("the server didn't answer in 20 s".into())),
        None => Err(OpsError::Failed("couldn't run the handshake".into())),
    }
}

// ── folder ──────────────────────────────────────────────────────────────────

pub fn repos(svc: &ChatService, ws: Uuid) -> Vec<RepoFact> {
    let Ok(roster) = svc.workspace_roster(ws) else { return Vec::new() };
    let maps = svc.store().repo_mappings(ws).unwrap_or_default();
    crate::repos::live_repos(&roster)
        .into_iter()
        .map(|r| RepoFact {
            id: r.id.to_string(),
            canonical: r.canonical.clone(),
            display_name: r.display_name.clone(),
            default_branch: r.default_branch.clone(),
            bound_here: maps.iter().find(|m| m.repo_id == r.id).map(|m| m.path.display().to_string()),
        })
        .collect()
}

/// [`super::validate::inspect_folder`] with what the store knows: the
/// canonical name through this device's host aliases, whether another host
/// manages the checkout, and whether it is bound here already.
pub fn inspect_folder(svc: &ChatService, ws: Uuid, path: &str) -> FolderFact {
    let mut f = super::validate::inspect_folder(path);
    if !f.is_checkout {
        return f;
    }
    let p = crate::repos::git::canonical(Path::new(path.trim()));
    let roster = svc.workspace_roster(ws).ok();
    let aliases = crate::repos::aliases(svc.store(), ws, roster.as_ref());
    if let Some((c, _)) = crate::repos::git::suggested_canonical(&p, &aliases.all_hosts()) {
        f.canonical = Some(c);
    }
    f.managed_elsewhere = crate::repos::marker::managed_elsewhere(&p, svc.device_id());
    f.bound_to = svc.store().repo_mappings(ws).unwrap_or_default().into_iter().find(|m| m.path == p).map(|m| m.repo_id.to_string());
    f
}

pub fn add_repo(svc: &mut ChatService, ws: Uuid, f: &FolderFact, display_name: &str, default_branch: &str) -> OpsResult<String> {
    let input = f.canonical.clone().or_else(|| f.root_commit.as_ref().map(|r| format!("local:{r}"))).ok_or_else(|| failed("that checkout has no remote and no commit"))?;
    let kind = ligus_forge::conn::known_kind(input.split('/').next().unwrap_or("")).map(|(k, _)| k.as_str().to_string()).unwrap_or_default();
    svc.add_workspace_repo(ws, &input, display_name, &kind, default_branch, f.root_commit.clone())
        .map(|r| r.id.to_string())
        .map_err(|e| match e {
            crate::chat_service::ChatError::Unauthorized(m) => OpsError::Refused(m),
            e => failed(e),
        })
}

pub fn bind_folder(svc: &ChatService, ws: Uuid, host_label: &str, repo_id: &str, path: &str) -> OpsResult<()> {
    let id = Uuid::parse_str(repo_id).map_err(failed)?;
    let roster = svc.workspace_roster(ws).map_err(failed)?;
    let repo = crate::repos::live_repos(&roster).into_iter().find(|r| r.id == id).ok_or_else(|| failed("that repository isn't in the workspace"))?;
    crate::repos::bind(svc.store(), svc.device_id(), host_label, ws, &repo, Path::new(path.trim()), CheckoutSource::Bound).map(|_| ()).map_err(failed)
}

/// "Start with a scratch repo" for a person's own device: see
/// [`crate::repos::scratch_host::create`]. `default_root` is the host's
/// `<data dir>/scratch`; `forbidden` the folders a scratch root may never be in.
pub fn create_scratch(
    svc: &mut ChatService,
    ws: Uuid,
    host_label: &str,
    default_root: &Path,
    forbidden: &[std::path::PathBuf],
    name: &str,
) -> OpsResult<(String, String)> {
    let settings = crate::repos::scratch::settings(svc.store(), default_root);
    crate::repos::scratch_host::create(svc, ws, None, &settings, forbidden, name, host_label)
        .map(|(e, r)| (r.id.to_string(), e.path.display().to_string()))
        .map_err(|e| match e {
            crate::repos::scratch_host::HostError::Refused(m) => OpsError::Refused(m),
            e => failed(e),
        })
}

// ── box, phone, invite ──────────────────────────────────────────────────────

pub fn hosts(svc: &ChatService, ws: Uuid) -> Vec<HostFact> {
    let now = time::OffsetDateTime::now_utc();
    svc.list_hosts(ws)
        .unwrap_or_default()
        .into_iter()
        .map(|h| {
            let seen = h.last_seen.inner();
            HostFact {
                id: h.id,
                kind: match h.kind {
                    ligus_core::HostKind::Worker => "worker".into(),
                    _ => "device".into(),
                },
                label: h.label,
                online: Some((now - seen).whole_seconds() < 300),
            }
        })
        .collect()
}

/// The person invites this workspace issued and whether each was used.
pub fn invites(svc: &ChatService, ws: Uuid) -> Vec<InviteFact> {
    let Some(config) = svc.load_in(ws, workspace_config_session_id(ws)).ok().flatten() else { return Vec::new() };
    config
        .guards
        .invite_keys
        .keys()
        .map(|id| InviteFact { id: *id, role: "contributor".into(), created_ms: 0, used: config.guards.used_invites.contains(id), closed: false })
        .collect()
}

// ── relay ───────────────────────────────────────────────────────────────────

/// One probe of `url` for workspace `room` (10 s at most).
pub fn probe_relay(url: &str, token: Option<&Zeroizing<String>>, room: &str) -> ProbeFact {
    let client = crate::relay_client::RelayClient::new(url.trim()).with_auth(token.map(|t| t.to_string()));
    let room = room.to_string();
    let out = block_on(async move { tokio::time::timeout(Duration::from_secs(10), client.probe(&room)).await });
    match out {
        Some(Ok(crate::relay_client::RelayProbe::Ok)) => ProbeFact::Ok,
        Some(Ok(crate::relay_client::RelayProbe::Unauthorized)) => ProbeFact::Unauthorized,
        Some(Ok(crate::relay_client::RelayProbe::HttpStatus(s))) => ProbeFact::HttpStatus(s),
        Some(Ok(crate::relay_client::RelayProbe::Unreachable(why))) => ProbeFact::Unreachable(crate::secret_redact::redacted(&why)),
        Some(Err(_)) => ProbeFact::Unreachable("no answer in 10 s".into()),
        None => ProbeFact::Unreachable("couldn't probe".into()),
    }
}

// ── test turn ───────────────────────────────────────────────────────────────

/// One short turn through the real dispatch path ("Reply with exactly
/// PINGOK"), 45 s at most (`LIGUS_RUNTIME_TEST_TIMEOUT_SECS`). The desktop's
/// runtime Test and the agent flow's test turn.
pub async fn ping(runtime: &crate::provider::ResolvedRuntime, workspace_root: Option<&str>) -> super::TestTurn {
    let turns = vec![crate::provider::ChatTurn::user("Reply with exactly the token: PINGOK")];
    let system = "You are a connectivity probe. Reply with exactly PINGOK and nothing else.";
    let timeout = Duration::from_secs(ligus_core::names::var("LIGUS_RUNTIME_TEST_TIMEOUT_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(45));
    let start = std::time::Instant::now();
    let fut = crate::provider::dispatch::stream(runtime, Some(system), &turns, workspace_root, &[], 32, |_| {}, |_| {});
    let out = tokio::time::timeout(timeout, fut).await;
    let latency_ms = start.elapsed().as_millis() as u64;
    let (ok, detail) = match out {
        Ok(Ok(reply)) if !reply.trim().is_empty() => (true, reply.trim().chars().take(80).collect()),
        Ok(Ok(_)) => (false, "the reply was empty".to_string()),
        Ok(Err(e)) => (false, crate::secret_redact::redacted(&format!("{e:#}")).chars().take(200).collect()),
        Err(_) => (false, format!("no reply in {} s", timeout.as_secs())),
    };
    super::TestTurn { ok, latency_ms, detail }
}

/// This person's paired phones in the workspace (not revoked), by
/// fingerprint.
pub fn phones(svc: &ChatService, ws: Uuid) -> Vec<String> {
    let phone_ids: std::collections::HashSet<Uuid> = svc
        .store()
        .load_config(ws, None, None)
        .map(|(_, t)| t.current().devices.iter().filter(|(_, d)| d.phone).map(|(id, _)| *id).collect())
        .unwrap_or_default();
    let mut out: Vec<String> = svc
        .own_devices(ws)
        .unwrap_or_default()
        .into_iter()
        .filter(|d| !d.revoked && phone_ids.contains(&d.device_id))
        .map(|d| d.fingerprint)
        .collect();
    out.sort();
    out.dedup();
    out
}

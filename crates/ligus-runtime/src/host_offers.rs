//! What this host offers and how ready it is (workspace model v3, 5.1).
//!
//! * **Offers** go on the host record: names and kinds only (`claude-code`
//!   is a `cli` run by `claudeCode`, `gw-home` is a `gateway` on the
//!   `openai` wire). They come from this host's own configuration: the
//!   coding CLIs on its `PATH`, `host.toml` ([`crate::host_config`]) and the
//!   gateways file ([`crate::model_gateway`]). Never a URL or a secret.
//! * **Status** rides the sealed presence beat, signed by this device:
//!   per offer whether it's ready (installed and signed in, or the server
//!   answered) with up to 64 model names, the build version and the reasons
//!   this host turns work down. Refreshed at most every
//!   [`STATUS_REFRESH_SECS`].
//!
//! Workers and boxes always list their offers. A person's own computer
//! lists them unless `host.toml` says `list_tools = false` ("Don't list this
//! computer's tools").

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};

use ligus_core::{HostModelGateway, HostOffer, HostStatus, ModelProviderKind, OfferKind, OfferStatus};
use uuid::Uuid;

use crate::cli_detect::{self, DetectEnv, LoginState};
use crate::host_config::HostConfig;
use crate::model_catalog::ModelList;

/// How often a host re-reads its readiness and model lists at most.
pub const STATUS_REFRESH_SECS: u64 = 300;

/// Who this host is to the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRole {
    /// A person's own computer (the desktop, lt, the CLI): honours the opt-out.
    OwnComputer,
    /// A worker or a `ligus serve` box: always lists.
    Box,
}

/// The coding CLIs a host can offer: (offer name, binary, harness).
pub const CLI_HARNESSES: &[(&str, &str, ModelProviderKind)] = &[
    ("claude-code", "claude", ModelProviderKind::ClaudeCode),
    ("codex", "codex", ModelProviderKind::Codex),
    ("copilot", "copilot", ModelProviderKind::Copilot),
    ("antigravity", "agy", ModelProviderKind::Antigravity),
    ("aider", "aider", ModelProviderKind::Aider),
    ("pi", "pi", ModelProviderKind::Pi),
];

/// A provider kind's wire name (`claudeCode`, `openAI`).
pub fn harness_name(kind: ModelProviderKind) -> String {
    serde_json::to_value(kind).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

/// The provider kind a harness wire name names.
pub fn harness_kind(name: &str) -> Option<ModelProviderKind> {
    serde_json::from_value(serde_json::Value::String(name.to_string())).ok()
}

/// What this host offers, from its own configuration. `installed` says
/// whether a CLI binary is on this host. Order: CLIs, local servers,
/// gateways, providers; names are unique (the first wins); at most
/// [`ligus_core::workspace_host::MAX_HOST_OFFERS`].
pub fn offers_from(
    cfg: &HostConfig,
    role: HostRole,
    workspace_id: Option<Uuid>,
    installed: impl Fn(&str) -> bool,
    gateways: &[HostModelGateway],
    gateway_exclusive: bool,
) -> Vec<HostOffer> {
    if role == HostRole::OwnComputer && !cfg.list_tools {
        return Vec::new();
    }
    let mut out: Vec<HostOffer> = Vec::new();
    let mut push = |o: HostOffer| {
        if o.well_formed() && !out.iter().any(|x| x.name == o.name) {
            out.push(o);
        }
    };
    if !gateway_exclusive {
        for (name, bin, kind) in CLI_HARNESSES {
            if installed(bin) {
                push(HostOffer { name: (*name).into(), kind: OfferKind::Cli, harness: harness_name(*kind), wire: String::new() });
            }
        }
        for l in &cfg.local {
            let kind = if l.harness == "openai" { ModelProviderKind::Custom } else { ModelProviderKind::Ollama };
            push(HostOffer { name: l.name.clone(), kind: OfferKind::Local, harness: harness_name(kind), wire: String::new() });
        }
    }
    for g in gateways {
        push(HostOffer { name: g.name.clone(), kind: OfferKind::Gateway, harness: String::new(), wire: g.api.clone() });
    }
    if !gateway_exclusive {
        if let Some(ws) = workspace_id {
            for p in cfg.providers_in(ws) {
                push(HostOffer { name: p.name.clone(), kind: OfferKind::Provider, harness: p.harness.clone(), wire: String::new() });
            }
        }
    }
    out.truncate(ligus_core::workspace_host::MAX_HOST_OFFERS);
    out
}

/// The runtime a full run spec (rules 4: `{harness, source, model}`)
/// resolves to on this host, from its own offers only (workspace model v3,
/// 5.4): a CLI it has, a local server its `host.toml` lists (the address
/// stays here), a gateway its operator listed (the harness follows the
/// gateway's wire unless the spec names one), or a hosted provider backed by
/// a secret declared for `workspace_id`. Its id is the spec's legacy id, so
/// it matches the agent's `runtime_id`. `None`: this host doesn't offer the
/// source, or offers it under another harness; the agent is refused, never
/// run on something else.
pub fn runtime_for_spec(
    cfg: &HostConfig,
    workspace_id: Uuid,
    run: &ligus_core::agent_run::AgentRun,
    offers: &[HostOffer],
    gateways: &[HostModelGateway],
) -> Option<ligus_core::WorkspaceRuntime> {
    use ligus_core::{WorkspaceCredential, WorkspaceRuntime};
    if run.source.is_empty() || run.runtime_ref().is_some() {
        return None;
    }
    let offer = offers.iter().find(|o| o.name == run.source)?;
    let id = ligus_core::agent_run::legacy_id(run);
    let wants = (!run.harness.is_empty()).then(|| harness_kind(&run.harness)).flatten();
    if !run.harness.is_empty() && wants.is_none() {
        return None;
    }
    let mut rt = match offer.kind {
        OfferKind::Cli | OfferKind::Local | OfferKind::Provider => {
            let kind = harness_kind(&offer.harness)?;
            if wants.is_some_and(|w| w != kind) {
                return None;
            }
            let mut rt = WorkspaceRuntime::new(id, run.source.clone(), kind, run.model.clone());
            match offer.kind {
                OfferKind::Local => rt.endpoint = cfg.local.iter().find(|l| l.name == run.source)?.url.clone(),
                OfferKind::Provider => {
                    let p = cfg.providers_in(workspace_id).into_iter().find(|p| p.name == run.source)?;
                    rt.credential = WorkspaceCredential::SecretRef { secret_ref: p.secret.clone() };
                }
                _ => {}
            }
            rt
        }
        OfferKind::Gateway => {
            let g = gateways.iter().find(|g| g.name == run.source)?;
            let by_wire = if g.api == "anthropic" { ModelProviderKind::Anthropic } else { ModelProviderKind::Custom };
            let mut rt = WorkspaceRuntime::new(id, run.source.clone(), wants.unwrap_or(by_wire), run.model.clone());
            rt.gateway = g.name.clone();
            rt
        }
        OfferKind::Other => return None,
    };
    rt.created_at = ligus_core::Timestamp::epoch();
    Some(rt)
}

/// This host's offers now: `host.toml`, the gateways file, the CLIs on `PATH`.
pub fn offers_now(role: HostRole, workspace_id: Option<Uuid>) -> Vec<HostOffer> {
    let gws = crate::model_gateway::host();
    let path = std::env::var_os("PATH").unwrap_or_default();
    offers_from(&crate::host_config::host(), role, workspace_id, |bin| cli_detect::find_on_path(bin, &path).is_some(), &gws.offers(), gws.exclusive())
}

/// Why this host turns work down, from its configuration.
pub fn refusals(cfg: &HostConfig, role: HostRole, gateway_exclusive: bool) -> Vec<String> {
    let mut r = Vec::new();
    if gateway_exclusive {
        r.push("runs its model gateways only".to_string());
    }
    if role == HostRole::OwnComputer && !cfg.list_tools {
        r.push("this computer's tools aren't listed".to_string());
    }
    r
}

/// One offer's status from a model listing (`ready` when it listed, or when
/// the source has no listing at all).
fn from_list(name: &str, list: &ModelList) -> OfferStatus {
    let failed = list.error.is_some();
    OfferStatus {
        name: name.to_string(),
        ready: !failed,
        why: if failed { "didn't answer".into() } else { String::new() },
        models: list.models.iter().map(|m| m.id.clone()).collect(),
        complete: list.complete && !failed,
    }
}

/// Readiness and models of every offer (slow: runs the CLIs' own listings
/// and asks servers and gateways; call off the async runtime's workers).
pub fn status_now(offers: &[HostOffer], build: &str, role: HostRole) -> HostStatus {
    let env = DetectEnv::from_process();
    let facts = crate::setup::service::harnesses(&env, false);
    let cfg = crate::host_config::host();
    let gws = crate::model_gateway::host();
    let mut out = Vec::new();
    for o in offers {
        let st = match o.kind {
            OfferKind::Cli => {
                let fact = facts.iter().find(|f| f.id == o.name);
                match fact {
                    Some(f) if !f.installed => OfferStatus { name: o.name.clone(), ready: false, why: "not installed".into(), ..Default::default() },
                    Some(f) if f.login == LoginState::NotLoggedIn => {
                        OfferStatus { name: o.name.clone(), ready: false, why: "not signed in".into(), ..Default::default() }
                    }
                    _ => {
                        let list = crate::setup::service::models(&o.name, "", &env);
                        OfferStatus { ready: true, why: String::new(), ..from_list(&o.name, &list) }
                    }
                }
            }
            OfferKind::Local => match cfg.local_source(&o.name) {
                Some(l) => {
                    let provider = if l.harness == "openai" { ModelProviderKind::Custom } else { ModelProviderKind::Ollama };
                    let req = crate::model_catalog::HttpListing { provider, endpoint: l.url.clone(), api_key: None };
                    let list = crate::setup::service::block_on(crate::model_catalog::http_models(req))
                        .unwrap_or_else(|| ModelList::failed(crate::model_catalog::ModelSource::Live, "didn't answer"));
                    from_list(&o.name, &list)
                }
                None => OfferStatus { name: o.name.clone(), ready: false, why: "not configured".into(), ..Default::default() },
            },
            OfferKind::Gateway => match gws.get(&o.name) {
                Some(g) => {
                    let list = crate::setup::service::block_on(gateway_models(g)).unwrap_or_else(|| ModelList::failed(crate::model_catalog::ModelSource::Live, "didn't answer"));
                    from_list(&o.name, &list)
                }
                None => OfferStatus { name: o.name.clone(), ready: false, why: "not configured".into(), ..Default::default() },
            },
            // A declared secret: ready when this process holds its value.
            OfferKind::Provider => {
                let held = cfg
                    .workspaces
                    .values()
                    .flat_map(|d| d.providers.iter())
                    .find(|p| p.name == o.name)
                    .is_some_and(|p| crate::host_config::is_held(&p.secret));
                OfferStatus { name: o.name.clone(), ready: held, why: if held { String::new() } else { "no key here".into() }, ..Default::default() }
            }
            OfferKind::Other => continue,
        };
        out.push(st);
    }
    HostStatus { build: build.to_string(), offers: out, refuses: refusals(&cfg, role, gws.exclusive()), refused_agents: Vec::new() }.sanitized()
}

/// A gateway's models (`GET /v1/models` through it, with its own
/// credentials). Read-only; spends no quota.
async fn gateway_models(g: std::sync::Arc<crate::model_gateway::Gateway>) -> ModelList {
    use crate::model_catalog::{parse_openai_models, ModelSource};
    let mut req = crate::provider::http::client().get(g.join("v1/models"));
    if g.api() == crate::model_gateway::Api::Anthropic {
        req = req.header("anthropic-version", "2023-06-01");
    }
    let req = match g.apply(req, None) {
        Ok(r) => r,
        Err(e) => return ModelList::failed(ModelSource::Live, e.to_string()),
    };
    let resp = match tokio::time::timeout(crate::model_catalog::HTTP_LIST_TIMEOUT, req.send()).await {
        Ok(Ok(r)) if r.status().is_success() => r,
        Ok(Ok(r)) => return ModelList::failed(ModelSource::Live, format!("answered {}", r.status())),
        _ => return ModelList::failed(ModelSource::Live, "didn't answer"),
    };
    match resp.json::<serde_json::Value>().await {
        Ok(v) => ModelList { models: parse_openai_models(&v), source: ModelSource::Live, complete: false, note: None, error: None },
        Err(e) => ModelList::failed(ModelSource::Live, e.to_string()),
    }
}

/// This host's last computed status, shared with whatever seals its beat.
#[derive(Default)]
struct Local {
    status: Option<HostStatus>,
    at: Option<Instant>,
    /// Per workspace: the agents placed on this host that it won't run
    /// (workspace model v3, 5.4), refreshed by the dispatch loop, carried in
    /// every beat (not only every five minutes, with the readiness lists).
    refused: std::collections::HashMap<Uuid, Vec<ligus_core::outcome::AgentRefusal>>,
}

/// Record the agents placed on this host that it won't run in `workspace`,
/// each with why. Replaces the last list; the next beat carries it.
pub fn set_refused_agents(workspace: Uuid, mut list: Vec<ligus_core::outcome::AgentRefusal>) {
    list.sort_by_key(|r| r.agent_id);
    list.dedup_by_key(|r| r.agent_id);
    let mut s = slot().write().unwrap_or_else(|e| e.into_inner());
    if list.is_empty() {
        s.refused.remove(&workspace);
    } else {
        s.refused.insert(workspace, list);
    }
}

/// The agents this host refuses in `workspace` ([`set_refused_agents`]).
pub fn refused_agents(workspace: Uuid) -> Vec<ligus_core::outcome::AgentRefusal> {
    slot().read().unwrap_or_else(|e| e.into_inner()).refused.get(&workspace).cloned().unwrap_or_default()
}

/// The status this host's beat for `workspace` carries: the readiness
/// report with this workspace's refused agents. `None` when there is
/// neither yet.
pub fn beat_status(workspace: Uuid) -> Option<HostStatus> {
    let refused = refused_agents(workspace);
    match local_status() {
        Some(mut st) => {
            st.refused_agents = refused;
            Some(st.sanitized())
        }
        None if refused.is_empty() => None,
        None => Some(HostStatus { refused_agents: refused, ..Default::default() }.sanitized()),
    }
}

fn slot() -> &'static RwLock<Local> {
    static SLOT: OnceLock<RwLock<Local>> = OnceLock::new();
    SLOT.get_or_init(Default::default)
}

/// The status this host's beat carries (`None` until the first refresh).
pub fn local_status() -> Option<HostStatus> {
    slot().read().unwrap_or_else(|e| e.into_inner()).status.clone()
}

/// Install `status` as this host's (also tests).
pub fn set_local_status(status: Option<HostStatus>) {
    let mut s = slot().write().unwrap_or_else(|e| e.into_inner());
    s.status = status;
    s.at = Some(Instant::now());
}

/// Make the next [`refresh_if_due`] recompute (the configuration changed).
pub fn invalidate_status() {
    slot().write().unwrap_or_else(|e| e.into_inner()).at = None;
}

/// Whether the status is older than [`STATUS_REFRESH_SECS`] (or missing).
pub fn status_due() -> bool {
    slot().read().unwrap_or_else(|e| e.into_inner()).at.is_none_or(|t| t.elapsed() >= Duration::from_secs(STATUS_REFRESH_SECS))
}

/// Refresh this host's status when due (blocking; run it off the async
/// runtime's workers). Returns the offers it was computed for.
pub fn refresh_if_due(role: HostRole, workspace_id: Option<Uuid>, build: &str) -> Vec<HostOffer> {
    let offers = offers_now(role, workspace_id);
    if status_due() {
        // Mark it first, so two callers don't both run the listings.
        set_local_status(local_status());
        set_local_status(Some(status_now(&offers, build, role)));
    }
    offers
}

/// Reader view: one host's readiness in a few words, for a Hosts row
/// ("Claude Code ready · codex not signed in · 0.5.0").
pub fn readiness_line(offers: &[HostOffer], status: Option<&HostStatus>, online: bool) -> String {
    let Some(st) = status else {
        return if offers.is_empty() { String::new() } else { offers.iter().map(|o| o.name.as_str()).collect::<Vec<_>>().join(", ") };
    };
    let mut parts: Vec<String> = Vec::new();
    let ready: Vec<&str> = offers.iter().filter(|o| st.offer(&o.name).is_some_and(|s| s.ready)).map(|o| o.name.as_str()).collect();
    if !ready.is_empty() {
        parts.push(format!("{} ready", ready.join(", ")));
    }
    for o in offers {
        if let Some(s) = st.offer(&o.name).filter(|s| !s.ready) {
            parts.push(if s.why.is_empty() { format!("{} not ready", o.name) } else { format!("{} {}", o.name, s.why) });
        }
    }
    if let Some(r) = st.refuses.first() {
        parts.push(r.clone());
    }
    if !online && !parts.is_empty() {
        parts.insert(0, "last report".into());
    }
    parts.join(" · ")
}

/// The models a source offers across hosts' verified statuses, by offer
/// name (a gateway's models, for the Add runtime sheet), deduplicated, in
/// first-seen order.
pub fn models_by_source<'a>(statuses: impl IntoIterator<Item = &'a HostStatus>) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for st in statuses {
        for o in &st.offers {
            let v = out.entry(o.name.clone()).or_default();
            for m in &o.models {
                if !v.contains(m) {
                    v.push(m.clone());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_config::{LocalSource, ProviderDecl, WorkspaceDecl};

    fn gw(name: &str, api: &str) -> HostModelGateway {
        HostModelGateway { name: name.into(), api: api.into() }
    }

    /// A full run spec resolves only through this host's own offers (the
    /// local server's address and the provider's secret from `host.toml`),
    /// with the spec's legacy id; a source it doesn't offer, or one under
    /// another harness, doesn't resolve (never a stand-in).
    #[test]
    fn a_full_run_spec_resolves_through_this_hosts_offers_only() {
        use ligus_core::agent_run::{legacy_id, AgentRun};
        let ws = Uuid::new_v4();
        let mut cfg = HostConfig::default();
        cfg.local.push(LocalSource { name: "ollama".into(), harness: "ollama".into(), url: "http://127.0.0.1:11434".into() });
        cfg.workspaces.insert(
            ws.to_string(),
            WorkspaceDecl {
                secrets: vec!["team-claude".into()],
                providers: vec![ProviderDecl { name: "team-claude".into(), harness: "anthropic".into(), secret: "team-claude".into() }],
            },
        );
        let gws = [gw("gw-home", "anthropic")];
        let offers = offers_from(&cfg, HostRole::Box, Some(ws), |b| b == "codex", &gws, false);
        let spec = |h: &str, s: &str, m: &str| AgentRun { harness: h.into(), source: s.into(), model: m.into(), resolved: None };
        let r = |run: &AgentRun| runtime_for_spec(&cfg, ws, run, &offers, &gws);
        let codex = r(&spec("", "codex", "gpt-5.3-codex")).unwrap();
        assert_eq!((codex.provider, codex.model.as_str(), codex.id.clone()), (ModelProviderKind::Codex, "gpt-5.3-codex", legacy_id(&spec("", "codex", "gpt-5.3-codex"))));
        let local = r(&spec("ollama", "ollama", "llama3:8b")).unwrap();
        assert_eq!((local.provider, local.endpoint.as_str()), (ModelProviderKind::Ollama, "http://127.0.0.1:11434"));
        let hosted = r(&spec("", "team-claude", "claude-opus")).unwrap();
        assert_eq!(hosted.credential, ligus_core::WorkspaceCredential::SecretRef { secret_ref: "team-claude".into() });
        let g = r(&spec("", "gw-home", "m")).unwrap();
        assert_eq!((g.provider, g.gateway.as_str()), (ModelProviderKind::Anthropic, "gw-home"), "the harness follows the gateway's wire");
        assert!(r(&spec("", "claude-code", "opus")).is_none(), "not installed here");
        assert!(r(&spec("codex", "ollama", "x")).is_none(), "another harness");
        assert!(r(&AgentRun::mirror("ws-claude", None)).is_none(), "a runtime reference isn't a spec");
    }

    #[test]
    fn offers_come_from_the_hosts_own_configuration() {
        let ws = Uuid::new_v4();
        let mut cfg = HostConfig::default();
        cfg.local.push(LocalSource { name: "ollama".into(), harness: "ollama".into(), url: "http://127.0.0.1:11434".into() });
        cfg.workspaces.insert(
            ws.to_string(),
            WorkspaceDecl {
                secrets: vec!["team-claude".into()],
                providers: vec![ProviderDecl { name: "team-claude".into(), harness: "anthropic".into(), secret: "team-claude".into() }],
            },
        );
        let installed = |b: &str| b == "claude" || b == "codex";
        let offers = offers_from(&cfg, HostRole::Box, Some(ws), installed, &[gw("gw-home", "openai")], false);
        let names: Vec<(&str, OfferKind, &str, &str)> = offers.iter().map(|o| (o.name.as_str(), o.kind, o.harness.as_str(), o.wire.as_str())).collect();
        assert_eq!(
            names,
            [
                ("claude-code", OfferKind::Cli, "claudeCode", ""),
                ("codex", OfferKind::Cli, "codex", ""),
                ("ollama", OfferKind::Local, "ollama", ""),
                ("gw-home", OfferKind::Gateway, "", "openai"),
                ("team-claude", OfferKind::Provider, "anthropic", ""),
            ]
        );
        // Another workspace sees no provider; a gateway-exclusive host only its gateways.
        assert!(offers_from(&cfg, HostRole::Box, Some(Uuid::new_v4()), installed, &[], false).iter().all(|o| o.kind != OfferKind::Provider));
        let excl = offers_from(&cfg, HostRole::Box, Some(ws), installed, &[gw("gw-home", "anthropic")], true);
        assert_eq!(excl.len(), 1);
        assert_eq!(excl[0].wire, "anthropic");
    }

    /// "Don't list this computer's tools": a person's computer lists
    /// nothing; a box ignores the setting.
    #[test]
    fn the_opt_out_applies_to_a_persons_computer_only() {
        let cfg = HostConfig { list_tools: false, ..Default::default() };
        assert!(offers_from(&cfg, HostRole::OwnComputer, None, |_| true, &[], false).is_empty());
        assert!(!offers_from(&cfg, HostRole::Box, None, |_| true, &[], false).is_empty());
        assert_eq!(refusals(&cfg, HostRole::OwnComputer, false), ["this computer's tools aren't listed"]);
    }

    #[test]
    fn readiness_reads_in_one_line() {
        let offers = offers_from(&HostConfig::default(), HostRole::Box, None, |b| b == "claude" || b == "codex", &[], false);
        let st = HostStatus {
            build: "0.5.0".into(),
            offers: vec![
                OfferStatus { name: "claude-code".into(), ready: true, ..Default::default() },
                OfferStatus { name: "codex".into(), ready: false, why: "not signed in".into(), ..Default::default() },
            ],
            refuses: vec![],
            refused_agents: vec![],
        };
        assert_eq!(readiness_line(&offers, Some(&st), true), "claude-code ready · codex not signed in");
        assert_eq!(readiness_line(&offers, None, true), "claude-code, codex");
        assert_eq!(readiness_line(&[], None, true), "");
    }

    #[test]
    fn harness_names_round_trip() {
        for (_, _, k) in CLI_HARNESSES {
            assert_eq!(harness_kind(&harness_name(*k)), Some(*k));
        }
        assert_eq!(harness_name(ModelProviderKind::ClaudeCode), "claudeCode");
    }
}

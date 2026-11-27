//! Workspace hosts (spec §12.4). A **host** is a machine that executes an
//! agent's turns: a member's `Device`, or an always-on `Worker` the workspace
//! owns. An agent binds to a host (`WorkspaceAgent.host_id`); a **detached**
//! agent is one bound to a worker, so it keeps running when its owner's laptop is
//! closed. Hosts ride the config log, so every member and worker sees the roster.
//!
//! A host's `id` is the machine's **device id** (from its identity) — a stable,
//! already-existing per-box identifier, so registering a host needs no new
//! provisioning: a box registers itself under its own device id.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::time_util::Timestamp;

/// A host counts as online when it beat (or, from an older build, wrote its
/// record) within this many seconds. The one window every reader uses
/// (workspace model v3, 5.10).
pub const HOST_ONLINE_WINDOW_SECS: i64 = 90;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HostKind {
    /// A member's machine — online only while their app/CLI runs.
    Device,
    /// An always-on machine the workspace owns (VM, container, office box).
    Worker,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceHost {
    /// The machine's device id (stringified). Stable per box.
    pub id: String,
    pub kind: HostKind,
    pub label: String,
    /// Actor who registered/owns the host (empty if unset).
    #[serde(default)]
    pub owner_actor_id: String,
    /// Last heartbeat (`touch_host`). A host is considered **online** when this
    /// is recent; a mention to an agent on an offline host is *queued* work
    /// (spec §12.5) until the host returns or the agent is moved to a worker.
    #[serde(default)]
    pub last_seen: Timestamp,
    #[serde(default)]
    pub created_at: Timestamp,
    /// Model gateways this host's operator lists (names and wire only; the
    /// URL and credentials stay in the host's own mounted config). An agent
    /// whose runtime names a gateway runs only on a host that offers it.
    /// Omitted when empty (docs/design/v2/model-gateways.md §5).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_gateways: Vec<HostModelGateway>,
    /// What this host can run, by name and kind (workspace model v3, 5.1): a
    /// superset of `model_gateways`, which writers keep filling for older
    /// readers. Names and kinds only, from the host's own configuration:
    /// never a URL, account, plan or secret. Readiness, model lists and the
    /// build version ride the sealed beat instead ([`HostStatus`]).
    ///
    /// Sticky: `None` (an older writer, which doesn't know the field) keeps
    /// what the host last listed; `Some(vec![])` says it lists nothing (a
    /// person's computer with "Don't list this computer's tools").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offers: Option<Vec<HostOffer>>,
}

/// One source a host offers (workspace model v3, 5.1).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostOffer {
    /// `[a-z0-9-]{1,63}` (the gateway rule): what an agent names as its source.
    pub name: String,
    pub kind: OfferKind,
    /// The harness that runs it (a `ModelProviderKind` wire name such as
    /// `claudeCode`, `codex`, `ollama`, `anthropic`); empty for a gateway.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub harness: String,
    /// A gateway's wire: `openai` or `anthropic`. Empty otherwise.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub wire: String,
}

/// What kind of source an offer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OfferKind {
    /// A coding CLI on the host (Claude Code, Codex, Copilot, agy, aider, pi).
    Cli,
    /// A local model server; its address stays in the host's `host.toml`.
    Local,
    /// An operator-listed model gateway (a `model_gateways` entry).
    Gateway,
    /// A hosted provider backed by a secret the host's `host.toml` declares.
    Provider,
    /// A kind a newer build added: kept, never offered by this build.
    #[serde(other)]
    Other,
}

impl OfferKind {
    pub fn as_str(self) -> &'static str {
        match self {
            OfferKind::Cli => "cli",
            OfferKind::Local => "local",
            OfferKind::Gateway => "gateway",
            OfferKind::Provider => "provider",
            OfferKind::Other => "other",
        }
    }
}

/// At most this many offers are taken from one host record.
pub const MAX_HOST_OFFERS: usize = 32;
/// The longest harness or wire string an offer may carry.
pub const MAX_OFFER_FIELD: usize = 32;

impl HostOffer {
    /// Whether a reader uses this offer: a well-formed name, a known kind,
    /// short `[A-Za-z0-9-]` harness and wire strings, and nothing that looks
    /// like an address or a key ([`looks_sensitive`]).
    pub fn well_formed(&self) -> bool {
        let field = |s: &str| s.len() <= MAX_OFFER_FIELD && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') && !looks_sensitive(s);
        HostModelGateway::valid_name(&self.name)
            && self.kind != OfferKind::Other
            && field(&self.harness)
            && field(&self.wire)
            && !looks_sensitive(&self.name)
            && (self.kind != OfferKind::Gateway || matches!(self.wire.as_str(), "openai" | "anthropic"))
    }
}

/// Whether `s` looks like an address, a credential or an account rather than
/// a name: a URL scheme, userinfo (`user:pass@` or `user@host`), or a common
/// API key prefix. Host records, offers and beats never carry one (H7):
/// writers drop such strings and readers ignore them.
pub fn looks_sensitive(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    const KEY_PREFIXES: &[&str] =
        &["sk-", "sk_", "ghp_", "gho_", "ghs_", "ghu_", "github_pat_", "xoxb-", "xoxp-", "akia", "aiza", "glpat-", "hf_", "eyj"];
    lower.contains("://")
        || lower.starts_with("file:")
        || lower.contains('@') && (lower.contains(':') || lower.contains('.'))
        || KEY_PREFIXES.iter().any(|p| lower.starts_with(p))
        || lower.contains("bearer ")
}

/// A host's readiness report (workspace model v3, 5.1 and 5.10): carried in
/// the sealed presence beat, signed by the host's device, never on the log.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostStatus {
    /// The host's Ligus build (`0.5.0`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub build: String,
    /// One entry per offer it lists (by name).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offers: Vec<OfferStatus>,
    /// Short reasons this host turns work down, in plain words ("runs
    /// gateways only", "tools not listed").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refuses: Vec<String>,
    /// Agents placed on this host that it won't run, each with why
    /// (workspace model v3, 5.4): a source it can't resolve, no key, or a
    /// desktop that runs only its own person's agents. Additive; older
    /// readers ignore it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refused_agents: Vec<crate::outcome::AgentRefusal>,
}

/// One offer's readiness and models.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OfferStatus {
    pub name: String,
    /// Installed and signed in (a gateway or server: it answered).
    pub ready: bool,
    /// Why it isn't ready, in a few words ("not signed in").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
    /// Up to [`MAX_STATUS_MODELS`] model names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// The list is everything this source runs (a model not on it won't work).
    #[serde(default)]
    pub complete: bool,
}

/// At most this many model names per offer in a beat.
pub const MAX_STATUS_MODELS: usize = 64;
/// The longest model name a beat keeps.
pub const MAX_MODEL_NAME: usize = 128;
/// The longest reason string (`why`, `refuses`).
pub const MAX_REASON: usize = 120;
/// At most this many refusal reasons.
pub const MAX_REFUSALS: usize = 8;
/// At most this many refused agents in one status.
pub const MAX_REFUSED_AGENTS: usize = 32;
/// The longest build string.
pub const MAX_BUILD: usize = 32;
/// A status larger than this (serialized) drops its model lists first, so a
/// beat stays well inside the relay's presence bound.
pub const MAX_STATUS_BYTES: usize = 12 * 1024;

impl HostStatus {
    /// Bring a status inside the bounds: offers capped, strings trimmed,
    /// anything that looks like an address or key dropped (H7), and when the
    /// whole is still too large, the model lists dropped (marked incomplete).
    /// Writers call it before sealing; readers call it on what they open.
    pub fn sanitized(mut self) -> Self {
        let clean = |s: &str, max: usize| -> Option<String> {
            let t = s.trim();
            (!t.is_empty() && t.len() <= max && !looks_sensitive(t) && !t.chars().any(char::is_control)).then(|| t.to_string())
        };
        self.build = clean(&self.build, MAX_BUILD).filter(|b| b.bytes().all(|c| c.is_ascii_alphanumeric() || b".-+".contains(&c))).unwrap_or_default();
        self.offers.truncate(MAX_HOST_OFFERS);
        self.offers.retain(|o| HostModelGateway::valid_name(&o.name));
        for o in &mut self.offers {
            o.why = clean(&o.why, MAX_REASON).unwrap_or_default();
            let before = o.models.len();
            o.models = o.models.iter().filter_map(|m| clean(m, MAX_MODEL_NAME)).take(MAX_STATUS_MODELS).collect();
            if o.models.len() != before {
                o.complete = false;
            }
        }
        self.refuses = self.refuses.iter().filter_map(|r| clean(r, MAX_REASON)).take(MAX_REFUSALS).collect();
        self.refused_agents.truncate(MAX_REFUSED_AGENTS);
        for r in &mut self.refused_agents {
            r.why = clean(&r.why, MAX_REASON).unwrap_or_default();
        }
        if serde_json::to_vec(&self).map_or(0, |v| v.len()) > MAX_STATUS_BYTES {
            for o in &mut self.offers {
                o.models.clear();
                o.complete = false;
            }
        }
        self
    }

    /// Why this host won't run `agent`, if it said so.
    pub fn refusal_of(&self, agent: uuid::Uuid) -> Option<&crate::outcome::AgentRefusal> {
        self.refused_agents.iter().find(|r| r.agent_id == agent)
    }

    /// The status of offer `name`.
    pub fn offer(&self, name: &str) -> Option<&OfferStatus> {
        self.offers.iter().find(|o| o.name == name)
    }
}

/// A model gateway a host offers: its operator-given name and the wire it
/// speaks. Never a URL, path or secret.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostModelGateway {
    pub name: String,
    /// `openai` or `anthropic`.
    pub api: String,
}

/// At most this many gateways are taken from one host record (a reader
/// ignores the rest), and each name at most this long.
pub const MAX_HOST_GATEWAYS: usize = 32;
pub const MAX_GATEWAY_NAME: usize = 63;

impl HostModelGateway {
    /// Whether `name` is a well-formed gateway name (`[a-z0-9-]{1,63}`, not
    /// starting or ending with `-`).
    pub fn valid_name(name: &str) -> bool {
        !name.is_empty()
            && name.len() <= MAX_GATEWAY_NAME
            && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !name.starts_with('-')
            && !name.ends_with('-')
    }
}

/// Upsert `host` into `hosts` by id. Offers are sticky: a record without
/// the field (an older writer) keeps the offers the host last listed.
pub fn upsert_sticky(hosts: &mut Vec<WorkspaceHost>, host: &WorkspaceHost) {
    if let Some(slot) = hosts.iter_mut().find(|h| h.id == host.id) {
        let kept = if host.offers.is_none() { slot.offers.take() } else { None };
        *slot = host.clone();
        if kept.is_some() {
            slot.offers = kept;
        }
    } else {
        hosts.push(host.clone());
    }
}

/// Whether `signer` is the device a host record's id names.
pub fn signed_by_own_device(host_id: &str, signer: Option<Uuid>) -> bool {
    signer.is_some_and(|d| Uuid::parse_str(host_id).is_ok_and(|id| id == d))
}

/// Note who signed each allowed host event (`AuthzState::host_signers`): a
/// record's signer by its id; a removal forgets the id.
pub fn fold_signers(signers: &mut std::collections::BTreeMap<String, Option<Uuid>>, event: &crate::events::SessionEvent, signer: Option<Uuid>) {
    use crate::events::SessionEvent;
    match event {
        SessionEvent::WorkspaceHostUpserted { host } => {
            signers.insert(host.id.clone(), signer);
        }
        SessionEvent::WorkspaceHostsUpdated { hosts } => {
            signers.clear();
            for h in hosts {
                signers.insert(h.id.clone(), signer);
            }
        }
        SessionEvent::WorkspaceHostRemoved { host_id } => {
            signers.remove(host_id);
        }
        _ => {}
    }
}

/// The hosts readers and pickers use: those whose latest record was signed
/// by the device whose id it carries (workspace model v3, 5.1, H11).
pub fn bound_hosts(hosts: &[WorkspaceHost], signers: &std::collections::BTreeMap<String, Option<Uuid>>) -> Vec<WorkspaceHost> {
    hosts.iter().filter(|h| signers.get(&h.id).is_some_and(|s| signed_by_own_device(&h.id, *s))).cloned().collect()
}

impl WorkspaceHost {
    pub fn new(id: impl Into<String>, kind: HostKind, label: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: id.into(),
            kind,
            label: label.into(),
            owner_actor_id: String::new(),
            last_seen: now,
            created_at: now,
            model_gateways: Vec::new(),
            offers: None,
        }
    }

    /// True when the host's durable `last_seen` is within the one online
    /// window ([`HOST_ONLINE_WINDOW_SECS`]) of `now`. Readers with a presence
    /// view use `HostPresence::is_online`, which also counts the beat.
    pub fn is_online(&self, now: &Timestamp) -> bool {
        now.inner().unix_timestamp() - self.last_seen.inner().unix_timestamp() <= HOST_ONLINE_WINDOW_SECS
    }

    /// The well-formed offers this host lists (bounded; malformed or unknown
    /// ones dropped). A record from an older writer lists only its gateways.
    pub fn listed_offers(&self) -> Vec<HostOffer> {
        match &self.offers {
            Some(offers) => offers.iter().take(MAX_HOST_OFFERS).filter(|o| o.well_formed()).cloned().collect(),
            None => self
                .offered_gateways()
                .map(|g| HostOffer { name: g.name.clone(), kind: OfferKind::Gateway, harness: String::new(), wire: g.api.clone() })
                .filter(HostOffer::well_formed)
                .collect(),
        }
    }

    /// Whether this host offers source `name`.
    pub fn offers_source(&self, name: &str) -> bool {
        self.listed_offers().iter().any(|o| o.name == name)
    }

    /// The well-formed gateways this host offers (bounded; malformed names
    /// dropped).
    pub fn offered_gateways(&self) -> impl Iterator<Item = &HostModelGateway> {
        self.model_gateways.iter().take(MAX_HOST_GATEWAYS).filter(|g| HostModelGateway::valid_name(&g.name))
    }

    /// Whether this host offers gateway `name`.
    pub fn offers_gateway(&self, name: &str) -> bool {
        self.offered_gateways().any(|g| g.name == name)
    }
}

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

use crate::time_util::Timestamp;

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
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !name.starts_with('-')
            && !name.ends_with('-')
    }
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
        }
    }

    /// True when the host's last heartbeat is within `window_secs` of `now`.
    /// Presence for the queue/offline handoff.
    pub fn is_online(&self, now: &Timestamp, window_secs: i64) -> bool {
        now.inner().unix_timestamp() - self.last_seen.inner().unix_timestamp() <= window_secs
    }

    /// The well-formed gateways this host offers (bounded; malformed names
    /// dropped).
    pub fn offered_gateways(&self) -> impl Iterator<Item = &HostModelGateway> {
        self.model_gateways
            .iter()
            .take(MAX_HOST_GATEWAYS)
            .filter(|g| HostModelGateway::valid_name(&g.name))
    }

    /// Whether this host offers gateway `name`.
    pub fn offers_gateway(&self, name: &str) -> bool {
        self.offered_gateways().any(|g| g.name == name)
    }
}

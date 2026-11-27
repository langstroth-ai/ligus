//! How a turn ended, and why one didn't run (workspace model v3, 5.4 and
//! 8.1).
//!
//! [`ReplyOutcome`] rides on `MessageCompleted` as an optional, additive
//! field (class R): `ok`, `failed` or `canceled`, plus the harness, source,
//! model and host that answered. It is the author's own report, display
//! only, and never feeds authz or dispatch. Older builds ignore it; a
//! completion without one (an older writer) reads as posted.
//!
//! [`TurnRoute`] is where a turn goes, computed locally on the sender's
//! device from the agent record, the host records and the hosts' beats
//! (class L, never on the log): it runs here, it runs on a host, it waits
//! for a host, or it can't run and says why.

use serde::{Deserialize, Serialize};

use crate::time_util::Timestamp;

/// The longest string an outcome keeps (harness, source, model, host).
pub const MAX_OUTCOME_FIELD: usize = 128;

/// How a reply ended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReplyStatus {
    /// Posted: the reply is what the agent wrote.
    #[default]
    Ok,
    /// The turn failed; the body is a readable error.
    Failed,
    /// Stopped by a person, swept after a crash, or overtaken by another
    /// device's answer.
    Canceled,
    /// A status a later build writes. Shown as posted.
    #[serde(other)]
    Unknown,
}

/// What a completion reports about its turn. Every field but `status` is
/// optional and skipped when empty, so a bare `{"status":"ok"}` is valid.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplyOutcome {
    #[serde(default)]
    pub status: ReplyStatus,
    /// The harness that ran it (`claudeCode`, `codex`, `openai`, ...).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub harness: String,
    /// The agent's source as the host resolved it (a runtime id, an offer).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// The model that answered (`""`: the source's default).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// The host device that ran it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
}

impl ReplyOutcome {
    /// An outcome with only a status.
    pub fn status(status: ReplyStatus) -> Self {
        Self { status, ..Default::default() }
    }

    /// The same report with `status`.
    pub fn with_status(mut self, status: ReplyStatus) -> Self {
        self.status = status;
        self
    }

    pub fn is_canceled(&self) -> bool {
        self.status == ReplyStatus::Canceled
    }

    /// Inside the bounds: each string trimmed, at most
    /// [`MAX_OUTCOME_FIELD`] bytes, free of controls, and dropped when it
    /// looks like an address, userinfo or key (an outcome is display only,
    /// but every member reads it).
    pub fn sanitized(mut self) -> Self {
        let clean = |s: &str| -> String {
            let t = s.trim();
            if t.is_empty() || t.len() > MAX_OUTCOME_FIELD || t.chars().any(char::is_control) || crate::workspace_host::looks_sensitive(t) {
                String::new()
            } else {
                t.to_string()
            }
        };
        self.harness = clean(&self.harness);
        self.source = clean(&self.source);
        self.model = clean(&self.model);
        self.host = clean(&self.host);
        self
    }
}

/// Where a turn goes, seen from one device (5.4). Local only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnRoute {
    /// This device runs it.
    RunsHere,
    /// The host `host` (a device id) runs it; it is online.
    RunsOn { host: String },
    /// It waits for `on` (host device ids; empty: the owner's own devices)
    /// since `since` (the trigger's time).
    Queued { on: Vec<String>, since: Timestamp },
    /// It doesn't run, and why.
    Unrunnable(Why),
}

/// Why a turn can't run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    /// The agent's host is gone (removed, revoked, its owner left) or has
    /// been offline past the queue window.
    HostGone { host: String, reason: GoneReason },
    /// The host doesn't have what the agent runs on; `detail` is the host's
    /// own words when it gave any.
    SourceNotOffered { host: String, source: String, detail: String },
    /// The source's complete model list doesn't have the model.
    ModelNotOffered { host: String, model: String },
    /// The host runs only its own person's agents.
    NotAllowedOnHost { host: String, detail: String },
    /// The agent's owner left the workspace.
    OwnerGone,
    /// No key for the agent's source on the device that would run it.
    NoKeyHere { source: String },
}

/// What happened to a host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoneReason {
    /// Not seen past the queue window.
    Offline,
    /// Its record is gone (or was never signed by its own device).
    Removed,
    /// Its device was revoked.
    Revoked,
    /// Its owner left the workspace.
    Left,
}

impl TurnRoute {
    /// The state a reader shows: `queued`, `hostGone`, `unrunnable`, or
    /// `None` when the turn runs (no row).
    pub fn state(&self) -> Option<&'static str> {
        match self {
            TurnRoute::RunsHere | TurnRoute::RunsOn { .. } => None,
            TurnRoute::Queued { .. } => Some("queued"),
            TurnRoute::Unrunnable(Why::HostGone { .. }) => Some("hostGone"),
            TurnRoute::Unrunnable(_) => Some("unrunnable"),
        }
    }

    /// The host this route names, if any.
    pub fn host(&self) -> Option<&str> {
        match self {
            TurnRoute::RunsOn { host } => Some(host),
            TurnRoute::Queued { on, .. } => on.first().map(String::as_str),
            TurnRoute::Unrunnable(w) => w.host(),
            TurnRoute::RunsHere => None,
        }
    }
}

impl Why {
    /// The host this reason names, if any.
    pub fn host(&self) -> Option<&str> {
        match self {
            Why::HostGone { host, .. }
            | Why::SourceNotOffered { host, .. }
            | Why::ModelNotOffered { host, .. }
            | Why::NotAllowedOnHost { host, .. } => Some(host),
            Why::OwnerGone | Why::NoKeyHere { .. } => None,
        }
    }

    /// A short machine name for readers (`hostGone`, `sourceNotOffered`, ...).
    pub fn code(&self) -> &'static str {
        match self {
            Why::HostGone { .. } => "hostGone",
            Why::SourceNotOffered { .. } => "sourceNotOffered",
            Why::ModelNotOffered { .. } => "modelNotOffered",
            Why::NotAllowedOnHost { .. } => "notAllowedOnHost",
            Why::OwnerGone => "ownerGone",
            Why::NoKeyHere { .. } => "noKeyHere",
        }
    }
}

impl GoneReason {
    pub fn code(&self) -> &'static str {
        match self {
            GoneReason::Offline => "offline",
            GoneReason::Removed => "removed",
            GoneReason::Revoked => "revoked",
            GoneReason::Left => "left",
        }
    }
}

/// Why a host turns down an agent placed on it, as its beat says
/// ([`crate::HostStatus::refused_agents`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RefusalKind {
    /// The host can't resolve what the agent runs on.
    #[default]
    SourceNotOffered,
    /// No key for the agent's runtime on the host.
    NoKey,
    /// The host runs only its own person's agents (a desktop, D16).
    NotAllowed,
    #[serde(other)]
    Other,
}

/// One agent a host won't run, and why, in the host's own words.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRefusal {
    pub agent_id: uuid::Uuid,
    #[serde(default)]
    pub kind: RefusalKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
}

impl AgentRefusal {
    /// The reason a reader shows for an agent on `host` running `source`.
    pub fn as_why(&self, host: &str, source: &str) -> Why {
        match self.kind {
            RefusalKind::NotAllowed => Why::NotAllowedOnHost { host: host.to_string(), detail: self.why.clone() },
            RefusalKind::NoKey => Why::SourceNotOffered { host: host.to_string(), source: source.to_string(), detail: self.why.clone() },
            RefusalKind::SourceNotOffered | RefusalKind::Other => {
                Why::SourceNotOffered { host: host.to_string(), source: source.to_string(), detail: self.why.clone() }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_outcome_is_additive_and_tolerant() {
        let o: ReplyOutcome = serde_json::from_str(r#"{"status":"failed"}"#).unwrap();
        assert_eq!(o.status, ReplyStatus::Failed);
        assert_eq!(serde_json::to_string(&o).unwrap(), r#"{"status":"failed"}"#);
        let later: ReplyOutcome = serde_json::from_str(r#"{"status":"paused","model":"m","extra":1}"#).unwrap();
        assert_eq!(later.status, ReplyStatus::Unknown);
        assert_eq!(later.model, "m");
        let empty: ReplyOutcome = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.status, ReplyStatus::Ok);
    }

    #[test]
    fn an_outcome_drops_anything_that_looks_like_an_address_or_key() {
        let o = ReplyOutcome {
            status: ReplyStatus::Ok,
            harness: "openai".into(),
            source: "http://user:pw@10.0.0.2:8080".into(),
            model: "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789".into(),
            host: "x\u{7}y".into(),
            tokens: Some(3),
        }
        .sanitized();
        assert_eq!(o.harness, "openai");
        assert!(o.source.is_empty() && o.model.is_empty() && o.host.is_empty());
    }

    #[test]
    fn a_refusal_reads_as_its_reason() {
        let r = AgentRefusal { agent_id: uuid::Uuid::nil(), kind: RefusalKind::NotAllowed, why: "runs only Ana's agents".into() };
        assert_eq!(r.as_why("h", "s").code(), "notAllowedOnHost");
        let unknown: AgentRefusal = serde_json::from_str(r#"{"agentId":"00000000-0000-0000-0000-000000000000","kind":"later"}"#).unwrap();
        assert_eq!(unknown.kind, RefusalKind::Other);
    }
}

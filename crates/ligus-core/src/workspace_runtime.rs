//! Workspace-owned runtimes (spec §12.5). A **detached** agent — one that keeps
//! working when its owner's laptop is closed, hosted on a workspace worker —
//! must run on a **workspace-owned credential**, never a member's personal API
//! key (personal keys never leave the owner's device). A `WorkspaceRuntime` is
//! that: a provider/model the workspace owns, with a credential the workspace
//! provides. It rides the config log, so it syncs to every member and any worker.
//!
//! Model API keys are never stored in the workspace. `SecretRef` names an
//! out-of-band secret (an env var / secret mount on each worker), so the key
//! never rides the synced log. The legacy `Inline` kind (a key carried on the
//! config log, authored by versions before 0.3.x) still decodes so old events
//! parse, but its secret is dropped on decode: it is never kept in memory,
//! used, displayed, exported or logged, and this version never authors it.

use serde::{Deserialize, Serialize};

use crate::runtime::ModelProviderKind;
use crate::time_util::Timestamp;

/// How a workspace runtime's secret is obtained. `None` for keyless runtimes
/// (e.g. a local Ollama).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum WorkspaceCredential {
    /// Resolve from a named secret the worker holds out-of-band — env
    /// `LIGUS_WS_SECRET_<secretRef>`. The raw key never rides the synced log.
    SecretRef { secret_ref: String },
    /// Legacy: a key an older version stored on the config log. Kept only so
    /// those events still decode (wire kind `inline`). The `secret` field on
    /// the wire is ignored on decode (no field here holds it), so the key is
    /// never kept, used, shown or re-serialized. Never authored: the runtime
    /// refuses to write it, and resolving it yields no key.
    Inline {},
    /// No credential (keyless local runtime).
    None,
}

impl Default for WorkspaceCredential {
    fn default() -> Self {
        WorkspaceCredential::None
    }
}

impl WorkspaceCredential {
    /// The env var name a `SecretRef` resolves from (`LIGUS_WS_SECRET_<ref>`).
    pub fn env_var(secret_ref: &str) -> String {
        format!("LIGUS_WS_SECRET_{secret_ref}")
    }
    /// True when the raw secret is NOT carried on the synced log.
    pub fn is_off_log(&self) -> bool {
        !self.is_legacy_inline()
    }
    /// True for a legacy inline credential (a key an older version stored in
    /// the workspace). It resolves as no key; the runtime warns about it.
    pub fn is_legacy_inline(&self) -> bool {
        matches!(self, WorkspaceCredential::Inline { .. })
    }
}

/// How a legacy inline credential is shown anywhere (UI, JSON, logs).
pub const LEGACY_INLINE_LABEL: &str = "stored key (unused)";

/// The calm warning for a runtime whose key an older version stored in the
/// workspace.
pub const LEGACY_INLINE_WARNING: &str =
    "This runtime's key was stored in the workspace by an older version. \
Rotate that key with the provider, then set it on each worker with --secret-ref.";

/// Why a key can't be stored in the workspace, and what to do instead.
pub const NO_WORKSPACE_KEYS: &str = "Keys aren't stored in the workspace. \
Put it on each worker as LIGUS_WS_SECRET_<NAME> and use --secret-ref <NAME>.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRuntime {
    pub id: String,
    pub name: String,
    pub provider: ModelProviderKind,
    pub model: String,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub credential: WorkspaceCredential,
    #[serde(default)]
    pub created_at: Timestamp,
    /// How a subscription CLI runtime is driven (Track D1). Omitted when
    /// `auto`, so existing events stay byte-identical.
    #[serde(default, skip_serializing_if = "crate::runtime::Driver::is_auto")]
    pub driver: crate::runtime::Driver,
    /// The operator-listed model gateway this runtime's turns go through
    /// (a name a host offers, `WorkspaceHost::model_gateways`; never a URL).
    /// Empty = the runtime's own endpoint. Rides the Admin + presence
    /// runtime events like `endpoint`. Omitted when empty, so existing
    /// events stay byte-identical (docs/design/v2/model-gateways.md §5).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub gateway: String,
}

impl WorkspaceRuntime {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        provider: ModelProviderKind,
        model: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            provider,
            model: model.into(),
            endpoint: String::new(),
            credential: WorkspaceCredential::None,
            created_at: Timestamp::now(),
            driver: crate::runtime::Driver::Auto,
            gateway: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_inline_decodes_and_drops_the_secret() {
        let wire = r#"{"kind":"inline","secret":"sk-LEGACY-123"}"#;
        let c: WorkspaceCredential = serde_json::from_str(wire).unwrap();
        assert!(c.is_legacy_inline());
        assert!(!format!("{c:?}").contains("sk-LEGACY"));
        let back = serde_json::to_string(&c).unwrap();
        assert!(!back.contains("sk-LEGACY"), "{back}");
        assert_eq!(back, r#"{"kind":"inline"}"#);
    }

    #[test]
    fn legacy_inline_runtime_never_reserializes_its_key() {
        let wire = r#"{"id":"r","name":"R","provider":"anthropic","model":"m","credential":{"kind":"inline","secret":"sk-LEGACY-456"}}"#;
        let rt: WorkspaceRuntime = serde_json::from_str(wire).unwrap();
        assert!(rt.credential.is_legacy_inline());
        assert!(!serde_json::to_string(&rt).unwrap().contains("sk-LEGACY"));
        assert!(!format!("{rt:?}").contains("sk-LEGACY"));
    }

    #[test]
    fn secret_ref_and_none_round_trip() {
        for c in [
            WorkspaceCredential::SecretRef {
                secret_ref: "a".into(),
            },
            WorkspaceCredential::None,
        ] {
            let s = serde_json::to_string(&c).unwrap();
            assert_eq!(serde_json::from_str::<WorkspaceCredential>(&s).unwrap(), c);
            assert!(!c.is_legacy_inline());
        }
    }
}

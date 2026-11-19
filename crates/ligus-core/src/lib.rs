//! `ligus-core` — pure, platform-agnostic domain logic: models, config, context
//! budgeting, git context, crypto/signing, MCP wire types, and the event-sourced
//! session projector. No IO — orchestration lives in `ligus-runtime`.

pub mod admission;
pub mod attachment;
pub mod account_keys;
pub mod agent;
pub mod authorization;
pub mod authz;
pub mod channel;
pub mod chat;
pub mod names;
pub mod config;
pub mod context_budget;
pub mod copies;
pub mod crypto;
pub mod device_link;
pub mod dispatch;
pub mod e2ee;
pub mod endpoint;
pub mod envelope_wire;
pub mod events;
pub mod flow;
pub mod git_context;
pub mod guard;
pub mod identity;
pub mod identity_link;
pub mod keys;
pub mod lamport;
pub mod lease;
pub mod memory;
#[cfg(test)]
mod lease_projection_tests;
#[cfg(test)]
mod dearth_tests;
#[cfg(test)]
mod lockdown_tests;
#[cfg(test)]
mod sec_tests;
pub mod model_profile;
pub mod phone_action;
pub mod policy;
pub mod presence;
pub mod process_util;
pub mod proposal_sets;
pub mod proposals;
pub mod runtime;
pub mod sandbox;
pub mod schedule;
pub mod session;
pub mod signal;
pub mod skills;
pub mod throttle;
pub mod time_util;
pub mod vault;
pub mod watermark;
pub mod workflow;
pub mod workspace_host;
pub mod workspace_repo;
pub mod workspace_runtime;
#[doc(hidden)]
pub mod testkit;

// Flat re-exports of the core domain types for ergonomic downstream use.
pub use agent::WorkspaceAgent;
pub use authorization::{evaluate as authorize, AuthzDecision, AuthzReason};
pub use channel::{room_workspace_id, workspace_config_session_id, Channel};
pub use device_link::DeviceLinks;
pub use workspace_host::{HostKind, HostModelGateway, WorkspaceHost};
pub use workspace_runtime::{WorkspaceCredential, WorkspaceRuntime};
pub use chat::{
    ChatMessage, ChatToolCall, ChatToolResult, MessageReaction, MessageRole, TranscriptEventKind,
};
pub use crypto::{
    sign_envelope, verify, verify_envelope, AgentKeyCertificate, CryptoError, DeviceCertificate, DeviceIdentity,
    HumanAccount, Platform, SigningKeypair,
};
pub use e2ee::{
    derive_workspace_key, generate_workspace_key, open as seal_open, open_symmetric, seal,
    seal_symmetric, seal_symmetric_kid, KeyAgreementKeypair, SealedBlob, SealedEnvelope, WorkspaceKeyRotation,
};
pub use events::{
    fold_copy_decisions, fold_decisions, project, project_authorized, project_config, CopyVerdicts, Verdict, project_with_founder, project_with_options, WorkspaceAuthz, EventScope, MemberRoleChange, SessionEvent,
    SessionEventEnvelope,
};
pub use git_context::{GitContextReader, GitFileChange, GitFileDiff, GitSnapshot};
pub use identity::{
    ActorIdentity, ActorKind, ActorStamp, CfgFrontier, LigusUserProfile, WorkspaceMember, WorkspaceRole,
};
pub use policy::{PermissionPolicy, PermissionScope, Workspace};
pub use proposal_sets::{ProposalSet, ScoutAssessment, SetPhase};
pub use proposals::{ActionProposal, ProposalApproval, ProposalKind, ProposalStatus, ProposedCommand};
pub use runtime::{Driver, ModelProviderKind, RuntimeCapabilities, RuntimeLocation, RuntimeTarget};
pub use schedule::ScheduleTrigger;
pub use session::{turn_claim_key, ChatSession, MAX_CASCADE_DEPTH};
pub use skills::SkillProfile;
pub use time_util::Timestamp;
pub use vault::{MountedVault, VaultSource};
pub use workflow::{
    GateRejectPolicy, NodeRunState, NodeRunStatus, WorkflowDefinition, WorkflowNode,
    WorkflowNodeKind, WorkflowRun, WorkflowRunStatus,
};

/// Crate version string, surfaced through the IPC `get_app_info` command.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_non_empty() {
        assert!(!VERSION.is_empty());
    }
}

/// The community relay: a free, best-effort relay run by the Langstroth
/// project. Only ever a preset the person picks (onboarding, a new workspace,
/// Settings → Team sync); never a silent fallback when no relay is set.
/// Mirrored in `web/src/lib/communityRelay.ts`.
pub const COMMUNITY_RELAY_URL: &str = "https://relay.langstroth.ai";

#[cfg(test)]
mod community_relay_tests {
    /// The desktop UI's preset must be the same URL as the Rust constant.
    #[test]
    fn ts_mirror_matches() {
        let ts = include_str!("../../../web/src/lib/communityRelay.ts");
        let want = format!("export const COMMUNITY_RELAY_URL = \"{}\";", super::COMMUNITY_RELAY_URL);
        assert!(ts.contains(&want), "web/src/lib/communityRelay.ts must declare {want}");
    }

    #[test]
    fn is_an_https_origin() {
        let u = super::COMMUNITY_RELAY_URL;
        assert!(u.starts_with("https://") && !u.ends_with('/') && !u.contains("/v1"));
    }
}

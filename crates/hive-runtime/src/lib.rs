//! `hive-runtime` — orchestration ported from the Swift `HiveRuntime` target:
//! the SQLite event store, identity store, provider adapters, MCP manager,
//! subprocess bridges, and the disk/relay/LAN/P2P transports.
//!
//! Phase 1 lands the SQLite event store and a file-based identity store.
//! Crypto/signing, providers, MCP, and transports arrive in later phases.

pub mod agent_tools;
pub mod agent_turn;
pub mod attachment_blob;
pub mod author_gate;
pub mod builtin_tools;
pub mod chat_service;
pub mod cli_detect;
pub mod context;
pub mod device_enroll;
pub mod directives;
pub mod effects;
pub mod egress;
pub mod facts;
pub mod finalize;
pub mod envelope_verifier;
pub mod event_store;
pub mod executor;
pub mod git_attribution;
pub mod git_worktree;
pub mod github;
pub mod guard_scan;
pub mod host_presence;
pub mod hygiene;
pub mod identity_store;
pub mod identity_verifier;
pub mod keyring;
pub mod lease_presence;
pub mod ligus_auth;
pub mod ligus_mcp;
pub mod mcp;
pub mod mcp_oauth;
pub mod mentions;
pub mod patch_leaving;
pub mod peer;
#[cfg(feature = "p2p")]
pub mod peer_iroh;
pub mod private_file;
pub mod prompt;
pub mod proposal_preflight;
pub mod provider;
pub mod relay_client;
pub mod remote_manifest;
pub mod retrieval;
pub mod review;
pub mod sandbox;
pub mod secret_redact;
pub mod secret_store;
pub mod sse;
pub mod sync_engine;
pub mod tool_loop;
pub mod vault_fetcher;
pub mod workflow_directive;
pub mod workflow_tool;
pub mod workspace_trust;

#[cfg(test)]
mod test_relay;
#[cfg(test)]
mod dearth_roster_tests;
#[cfg(test)]
mod ligus_auth_membership_tests;
#[cfg(test)]
mod keyring_tests;

pub use builtin_tools::{BuiltinTools, Policy as BuiltinToolPolicy};
pub use chat_service::{
    cascade_target, empty_reply_notice, pending_mentions, pending_turns, resolve_workspace_credential, turns_for, turns_from, turns_with_summary,
    ChatService, PendingMention, SUMMARY_TURN_HEADER,
};
pub use context::{Compactor, CompactionResult, Summarizer};
pub use mcp::{
    normalize_bearer_token, resolve_bearer, McpRegistry, McpServerSpec, McpTool, McpTransport,
};
pub use mentions::{parse_mentions, MentionTargets};
pub use relay_client::{
    AccountDevice, AccountIdentity, EnvelopePage, FetchedEnvelope, Friend, FriendPresence, FriendRequestOutcome,
    IncomingFriendRequest, InboxEvent, InviteEntry, IssuedInvite, IssuedRelayToken, MemberEntry,
    RelayClient, RelayError, RelayProbe, RelayTokenEntry, RelayUserEntry, ENVELOPE_PAGE_LIMIT,
};
pub use remote_manifest::resolve_manifest_url;
pub use sse::{next_backoff, parse_seq, SseConnectError, SseDecoder, SseStream};
pub use host_presence::HostPresence;
pub use sync_engine::{SyncEngine, SyncError};
pub use provider::dispatch::{self, ResolvedRuntime};
pub use provider::{AnthropicClient, ChatTurn, OpenAiClient, ProviderError};

pub use envelope_verifier::{
    verify_stream, DeviceKeyResolver, DeviceRoster, QuarantineReason, VerificationOutcome,
};
pub use identity_verifier::{
    gate_ingest, shared_cache, FetchOutcome, IdentityCache, IdentityMode, IdentityVerdict,
    IngestAction, SharedIdentityCache,
};
pub use event_store::{EventStore, EventStoreError, StoredCompaction};
pub use identity_store::{
    FileKeyVault, IdentityError, IdentityStore, KeyVault, StoredIdentity,
};

use hive_proto::AppInfo;

/// Assemble the [`AppInfo`] payload returned by the `get_app_info` IPC command.
pub fn app_info() -> AppInfo {
    AppInfo {
        name: "Ligus".to_string(),
        core_version: hive_core::VERSION.to_string(),
        build_profile: if cfg!(debug_assertions) { "debug" } else { "release" }.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_info_reports_core_version() {
        let info = app_info();
        assert_eq!(info.name, "Ligus");
        assert_eq!(info.core_version, hive_core::VERSION);
    }
}

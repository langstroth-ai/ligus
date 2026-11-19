//! `hive-runtime` — orchestration ported from the Swift `HiveRuntime` target:
//! the SQLite event store, identity store, provider adapters, MCP manager,
//! subprocess bridges, and the disk/relay/LAN/P2P transports.
//!
//! Phase 1 lands the SQLite event store and a file-based identity store.
//! Crypto/signing, providers, MCP, and transports arrive in later phases.

// `test-presence` answers every passkey prompt with a recovery key derived
// from the account id. Tests reach it through dev-dependencies only; an
// optimized build that has it is a mistake this refuses to compile
// (scripts/check-no-test-presence.sh checks the shipped graphs in CI).
#[cfg(all(feature = "test-presence", not(debug_assertions)))]
compile_error!("hive-runtime's `test-presence` feature must never be in an optimized (release) build");

pub mod agent_tools;
pub mod agent_turn;
pub mod apply_host;
pub mod attachment_blob;
pub mod author_gate;
pub mod box_admission;
pub mod builtin_tools;
pub mod chat_service;
pub mod cli_detect;
pub mod connection_settings;
pub mod context;
pub mod device_enroll;
pub mod directory_trust;
pub mod dm;
pub mod dm_delivery;
pub mod contacts;
pub mod directives;
pub mod effects;
pub mod egress;
pub mod facts;
pub mod finalize;
pub mod forge_secrets;
pub mod envelope_verifier;
pub mod event_store;
pub mod executor;
pub mod git_attribution;
pub mod git_worktree;
pub mod frozen_ns;
pub mod guard_scan;
pub mod host_presence;
pub mod revocation_status;
pub mod human_presence;
pub mod hygiene;
pub mod identity_links;
pub mod identity_store;
pub mod identity_verifier;
pub mod invite_sig;
pub mod keyring;
pub mod lease_presence;
pub mod lockdown_view;
pub mod ligus_auth;
pub mod ligus_mcp;
pub mod mcp;
pub mod mcp_oauth;
pub mod mentions;
pub mod model_gateway;
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
pub mod safe_fs;
pub mod sandbox;
pub mod spend;
pub mod secret_redact;
pub mod secret_store;
pub mod sse;
pub mod sync_engine;
pub mod sync_service;
pub mod tool_loop;
pub mod turn_dispatch;
pub mod vault_fetcher;
pub mod workflow_directive;
pub mod workflow_tool;
pub mod workspace_invite;
pub mod workspace_trust;

#[cfg(test)]
mod test_relay;
#[cfg(test)]
mod dearth_roster_tests;
#[cfg(test)]
mod ligus_auth_membership_tests;
#[cfg(test)]
mod keyring_tests;
#[cfg(test)]
mod presence_tests;

pub use builtin_tools::{BuiltinTools, Policy as BuiltinToolPolicy};
pub use chat_service::{
    cascade_target, empty_reply_notice, pending_mentions, pending_turns, resolve_workspace_credential, turns_for, turns_from, turns_with_summary,
    ChatService, PendingMention, SUMMARY_TURN_HEADER,
};
pub use context::{Compactor, CompactionResult, Summarizer};
pub use mcp::{
    normalize_bearer_token, resolve_bearer, McpRegistry, McpServerSpec, McpTool, McpTransport,
};
pub use mentions::{message_mentions, parse_mentions, requester_from, requester_of, MentionTargets, Requester};
pub use relay_client::{
    EnvelopePage, FetchedEnvelope, InviteEntry, IssuedInvite, MemberEntry, RelayClient,
    RelayError, RelayProbe, ENVELOPE_PAGE_LIMIT,
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
    let sandbox = executor::sandbox_status();
    AppInfo {
        name: "Ligus".to_string(),
        core_version: hive_core::VERSION.to_string(),
        build_profile: if cfg!(debug_assertions) { "debug" } else { "release" }.to_string(),
        sandbox_tier: sandbox.tier().to_string(),
        sandbox_summary: sandbox.summary,
        sandbox_notes: sandbox.notes,
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

/// This machine's host name, for a friendly default label ("mac-mini", not
/// "mac-mini.local"): the OS's own name, else `$HOSTNAME`/`$COMPUTERNAME`.
pub fn machine_hostname() -> Option<String> {
    let clean = |s: String| {
        let s = s.trim().trim_end_matches(".local").trim_end_matches(".lan").to_string();
        (!s.is_empty() && s != "localhost").then_some(s)
    };
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer is valid for its length; gethostname NUL-terminates
        // within it on success (we also cap at the first NUL).
        if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            if let Some(h) = std::str::from_utf8(&buf[..end]).ok().map(str::to_string).and_then(clean) {
                return Some(h);
            }
        }
    }
    std::env::var("HOSTNAME").ok().or_else(|| std::env::var("COMPUTERNAME").ok()).and_then(clean)
}

#[cfg(test)]
mod hostname_tests {
    #[test]
    fn machine_hostname_is_a_plain_name() {
        if let Some(h) = super::machine_hostname() {
            assert!(!h.is_empty() && !h.ends_with(".local") && h != "localhost", "{h}");
        }
    }
}

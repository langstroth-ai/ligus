//! P0.8: nothing that decides which events fold, or how, reads the
//! environment. Every `names` environment read in ligus-core and ligus-runtime
//! is listed below and reviewed by hand; the test fails when a new one
//! appears, and checks that setting every listed variable to a non-default
//! value leaves the fold, the trust roster and ingest unchanged.
//!
//! Runs in its own test binary because it sets process environment variables.

use std::path::Path;

use ligus_core::crypto::{DeviceCertificate, SigningKeypair};
use ligus_core::{
    project_authorized, project_config, sign_envelope, workspace_config_session_id, ActionProposal,
    ActorIdentity, ActorKind, ActorStamp, ChatSession, ProposalApproval, ProposalKind,
    SessionEvent, SessionEventEnvelope, Timestamp, WorkspaceAuthz, WorkspaceMember, WorkspaceRole,
};
use ligus_runtime::envelope_verifier::{roster_with_batch, verdict_for};
use ligus_runtime::event_store::EventStore;
use ligus_runtime::relay_client::RelayClient;
use ligus_runtime::sync_engine::SyncEngine;
use uuid::Uuid;

/// Every environment read (call argument as written) in ligus-core and
/// ligus-runtime, with the variable it reads and why it can't affect a fold.
const FOLD_ENV_DENYLIST: &[(&str, &str, &str)] = &[
    (
        "\"LIGUS_IDENTITY_MODE\"",
        "LIGUS_IDENTITY_MODE",
        "ignored since P0.8; only logs a warning",
    ),
    (
        "\"LIGUS_OLLAMA_WIRE\"",
        "LIGUS_OLLAMA_WIRE",
        "provider wire format for a turn",
    ),
    (
        "GATEWAYS_FILE_ENV",
        "LIGUS_MODEL_GATEWAYS_FILE",
        "the host operator's model gateways (a turn's model route, never the fold)",
    ),
    (
        "EXCLUSIVE_ENV",
        "LIGUS_MODEL_GATEWAYS_EXCLUSIVE",
        "whether a gateway host refuses other model routes",
    ),
    (
        "ATTRIBUTION_KEY_ENV",
        "LIGUS_MODEL_GATEWAY_ATTRIBUTION_KEY_FILE",
        "the key of the pseudonymous gateway attribution ids",
    ),
    (
        "\"LIGUS_LIVE_MODEL\"",
        "LIGUS_LIVE_MODEL",
        "live provider tests",
    ),
    (
        "\"LIGUS_LIVE_CLI\"",
        "LIGUS_LIVE_CLI",
        "live provider tests",
    ),
    (
        "\"LIGUS_LIVE_DRIVER\"",
        "LIGUS_LIVE_DRIVER",
        "live provider tests",
    ),
    (
        "\"LIGUS_LIVE_KEY\"",
        "LIGUS_LIVE_KEY",
        "live provider tests",
    ),
    (
        "\"LIGUS_AGENT_TIMEOUT_SECS\"",
        "LIGUS_AGENT_TIMEOUT_SECS",
        "subprocess turn timeout",
    ),
    (
        "\"LIGUS_PATCH_LEAVING_CALLS\"",
        "LIGUS_PATCH_LEAVING_CALLS",
        "agent tool-loop stall limit",
    ),
    (
        "IDLE_TIMEOUT_ENV",
        "LIGUS_TURN_IDLE_TIMEOUT_SECS",
        "provider stream idle timeout",
    ),
    (
        "\"LIGUS_TURN_TIMEOUT_SECS\"",
        "LIGUS_TURN_TIMEOUT_SECS",
        "the turn engine's wall-clock backstop for a turn",
    ),
    (
        "\"LIGUS_RUNTIME_TEST_TIMEOUT_SECS\"",
        "LIGUS_RUNTIME_TEST_TIMEOUT_SECS",
        "how long a setup test turn waits for its reply (never the fold)",
    ),
    (
        "PREFLIGHT_TEST_ENV",
        "LIGUS_PREFLIGHT_TEST",
        "pre-flight test command (a side effect, recorded as an event)",
    ),
    (
        "&WorkspaceCredential::env_var(secret_ref)",
        "LIGUS_CRED_FOLD_ENV",
        "a runtime credential for a turn",
    ),
    (
        "MIN_ROLE_ENV",
        "LIGUS_MCP_MIN_ROLE",
        "this host's tool floor for a turn (H1 author gate)",
    ),
    (
        "\"LIGUS_DATA_DIR\"",
        "LIGUS_DATA_DIR",
        "a directory the OS sandbox denies to agent processes",
    ),
    (
        "\"LIGUS_SANDBOX\"",
        "LIGUS_SANDBOX",
        "which executor spawns agent processes",
    ),
    (
        "name",
        "LIGUS_SANDBOX_EGRESS_ALLOW",
        "extra egress hosts for a turn's proxy",
    ),
    (
        "name",
        "LIGUS_SANDBOX_EGRESS_PRESETS",
        "opt-in egress presets for a turn's proxy",
    ),
    (
        "name",
        "LIGUS_SANDBOX_WRITE_ALLOW",
        "extra sandbox write paths for a turn",
    ),
    (
        "name",
        "LIGUS_SANDBOX_READ_ALLOW",
        "extra sandbox read paths for a turn",
    ),
    (
        "\"LIGUS_ACP_INIT_TIMEOUT_SECS\"",
        "LIGUS_ACP_INIT_TIMEOUT_SECS",
        "how long an ACP agent may take to answer initialize",
    ),
    (
        "\"LIGUS_ACP_ASK_TIMEOUT_SECS\"",
        "LIGUS_ACP_ASK_TIMEOUT_SECS",
        "how long an ACP permission escalation waits for a person",
    ),
    (
        "\"LIGUS_CLI_HOME\"",
        "LIGUS_CLI_HOME",
        "whether CLI agents get a per-turn home (C-9)",
    ),
    (
        "\"LIGUS_MCP_BRIDGE\"",
        "LIGUS_MCP_BRIDGE",
        "which binary bridges an agent to the Ligus MCP socket",
    ),
    (
        "k",
        "LIGUS_MCP_MAX_TOOL_CALLS",
        "per-turn Ligus tool budget (device-local host policy)",
    ),
    (
        "k",
        "LIGUS_MCP_MAX_PROPOSALS",
        "per-turn Ligus tool budget (device-local host policy)",
    ),
    (
        "k",
        "LIGUS_MCP_MAX_HANDOFFS",
        "per-turn Ligus tool budget (device-local host policy)",
    ),
    (
        "k",
        "LIGUS_MCP_MAX_ASKS",
        "per-turn Ligus tool budget (device-local host policy)",
    ),
    (
        "k",
        "LIGUS_MCP_MAX_READ_MESSAGES",
        "per-turn Ligus tool budget (device-local host policy)",
    ),
    (
        "\"LIGUS_MCP_MAX_SPEND_USD\"",
        "LIGUS_MCP_MAX_SPEND_USD",
        "per-turn Ligus spend budget (device-local host policy)",
    ),
    (
        "\"LIGUS_SECRET_BACKEND\"",
        "LIGUS_SECRET_BACKEND",
        "which OS secret store keeps this device's secrets (B2)",
    ),
    (
        "\"LIGUS_SECRETS_DIR\"",
        "LIGUS_SECRETS_DIR",
        "where a chart mounts its Secret files (B2)",
    ),
    (
        "k",
        "LIGUS_MCP_MAX_CASCADE_TOOL_CALLS",
        "per-cascade Ligus tool budget (device-local host policy)",
    ),
    (
        "k",
        "LIGUS_MCP_MAX_CASCADE_PROPOSALS",
        "per-cascade Ligus tool budget (device-local host policy)",
    ),
    (
        "k",
        "LIGUS_MCP_MAX_CASCADE_HANDOFFS",
        "per-cascade Ligus tool budget (device-local host policy)",
    ),
    (
        "k",
        "LIGUS_MCP_MAX_CASCADE_ASKS",
        "per-cascade Ligus tool budget (device-local host policy)",
    ),
    (
        "\"LIGUS_SANDBOX_UNIX_SOCKETS\"",
        "LIGUS_SANDBOX_UNIX_SOCKETS",
        "whether sandboxed children may create unix sockets (C-10)",
    ),
    (
        "\"LIGUS_MODEL_PRICES\"",
        "LIGUS_MODEL_PRICES",
        "token prices for a turn's spend budget (device-local host policy)",
    ),
    (
        "name",
        "LIGUS_AGENT_MAX_LINE_BYTES",
        "cap on one line of an agent's output",
    ),
    (
        "name",
        "LIGUS_AGENT_MAX_OUTPUT_BYTES",
        "cap on an agent turn's output",
    ),
    (
        "GRANTS_ENV",
        "LIGUS_SANDBOX_GRANTS",
        "whether this host applies the workspace's signed sandbox grants (device-local)",
    ),
];

fn rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

/// The argument text of every `names::{var,var_os,env_opt,is_set}(…)` call.
fn env_reads() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut files = Vec::new();
    rs_files(&root.join("ligus-core/src"), &mut files);
    rs_files(&root.join("ligus-runtime/src"), &mut files);
    let mut out = Vec::new();
    for f in files {
        if f.ends_with("names.rs") {
            continue; // the accessors themselves
        }
        let src = std::fs::read_to_string(&f).unwrap();
        for call in [
            "names::var(",
            "names::var_os(",
            "names::env_opt(",
            "names::is_set(",
        ] {
            for (i, _) in src.match_indices(call) {
                let rest = &src[i + call.len()..];
                // Up to the matching close paren.
                let mut depth = 1;
                let mut end = 0;
                for (j, c) in rest.char_indices() {
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = j;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                out.push((f.display().to_string(), rest[..end].trim().to_string()));
            }
        }
    }
    out
}

#[test]
fn every_env_read_is_reviewed() {
    let reads = env_reads();
    assert!(reads.len() >= 5, "the scanner found the known reads");
    for (file, arg) in reads {
        assert!(
            FOLD_ENV_DENYLIST.iter().any(|(a, _, _)| *a == arg),
            "{file} reads the environment ({arg}); review it and list it in FOLD_ENV_DENYLIST"
        );
    }
}

fn member(id: &str, role: WorkspaceRole, account: Uuid) -> WorkspaceMember {
    let mut actor = ActorIdentity::new(id, id, ActorKind::Human);
    actor.account_id = Some(account);
    WorkspaceMember {
        id: id.into(),
        actor,
        role,
        title: String::new(),
        index: 0,
        joined_at: Timestamp::epoch(),
    }
}

/// A small signed workspace: config log with the owner and a Contributor,
/// a chat with a proposal and votes, trust events, and one forged vote.
fn fixture() -> (Vec<SessionEventEnvelope>, Vec<SessionEventEnvelope>) {
    let ws = Uuid::from_u128(1);
    let owner_acct = SigningKeypair::from_seed(&[1; 32]).unwrap();
    let owner_dev = SigningKeypair::from_seed(&[2; 32]).unwrap();
    let (owner_id, dev_id) = (
        Uuid::from_u128(10),
        ligus_core::crypto::device_id_for(
            ligus_core::crypto::SigAlg::Ed25519,
            &owner_dev.public_key_bytes(),
        ),
    );
    let owner = owner_id.to_string();
    let stamp = |f: Option<ligus_core::CfgFrontier>| {
        let mut a = ActorIdentity::new(owner.clone(), "o", ActorKind::Human);
        a.account_id = Some(owner_id);
        Some(ActorStamp {
            actor: a,
            recorded_at: Timestamp::epoch(),
            cfg_frontier: f,
            presence: None,
            runtime_effect: false,
            prev: None,
            sig_alg: Default::default(),
            log_head: None,
        })
    };
    let mut n = 0u64;
    // Each log's newest event: the next one there counts from it.
    let mut heads: std::collections::HashMap<Uuid, ligus_core::CfgFrontier> =
        std::collections::HashMap::new();
    let mut make = |sid: Uuid, payload: SessionEvent, f: Option<ligus_core::CfgFrontier>| {
        n += 1;
        let mut e = SessionEventEnvelope::new(sid, ws, n as i64, payload);
        e.event_id = Uuid::from_u128(1000 + n as u128);
        e.id = Uuid::from_u128(2000 + n as u128);
        e.timestamp = Timestamp::epoch();
        e.actor_stamp = stamp(f);
        ligus_core::lamport::stamp_next(&mut e, heads.get(&sid).copied());
        heads.insert(
            sid,
            ligus_core::CfgFrontier {
                lamport: e.lamport,
                event_id: e.event_id,
            },
        );
        sign_envelope(&mut e, dev_id, &owner_dev);
        e
    };
    let cfg_id = workspace_config_session_id(ws);
    let mut seed = ChatSession::new("", ws, "");
    seed.id = cfg_id;
    seed.creator_actor_id = owner.clone();
    seed.members = vec![member(&owner, WorkspaceRole::Owner, owner_id)];
    seed.created_at = Timestamp::epoch();
    let mut cfg = vec![make(
        cfg_id,
        SessionEvent::SessionSnapshot {
            session: Box::new(seed),
        },
        None,
    )];
    cfg.push(make(
        cfg_id,
        SessionEvent::AccountKeyRegistered {
            account_id: owner_id,
            signing_public_key: owner_acct.public_key_bytes().to_vec(),
        },
        None,
    ));
    let cert = DeviceCertificate::issue(
        &owner_acct,
        owner_id,
        &owner_dev.public_key_bytes(),
        Timestamp::epoch(),
    );
    cfg.push(make(
        cfg_id,
        SessionEvent::DeviceCertificateAdded { certificate: cert },
        None,
    ));
    cfg.push(make(
        cfg_id,
        SessionEvent::MemberAdded {
            invite: None,
            proof: None,
            member: member("mara", WorkspaceRole::Contributor, Uuid::from_u128(12)),
        },
        None,
    ));
    let head = Some(ligus_core::CfgFrontier {
        lamport: cfg.last().unwrap().lamport,
        event_id: cfg.last().unwrap().event_id,
    });

    let sid = Uuid::from_u128(20);
    let mut chat = ChatSession::new("c", ws, "anthropic");
    chat.id = sid;
    chat.creator_actor_id = owner.clone();
    chat.members = vec![member(&owner, WorkspaceRole::Owner, owner_id)];
    chat.created_at = Timestamp::epoch();
    let mut p = ActionProposal::new("p", ProposalKind::Decision, "mara");
    p.id = Uuid::from_u128(30);
    p.created_at = Timestamp::epoch();
    let mut log = vec![make(
        sid,
        SessionEvent::SessionSnapshot {
            session: Box::new(chat),
        },
        head,
    )];
    log.push(make(
        sid,
        SessionEvent::ProposalUpserted { proposal: p },
        head,
    ));
    let vote = |actor: &str| SessionEvent::ProposalVoteCast {
        proposal_id: Uuid::from_u128(30),
        approval: ProposalApproval {
            actor_id: actor.into(),
            role: WorkspaceRole::Owner,
            approved: true,
            created_at: Timestamp::epoch(),
            diff_digest: None,
        },
    };
    log.push(make(sid, vote("mara"), head)); // names someone else: refused
    log.push(make(sid, vote(&owner), head));
    (cfg, log)
}

/// Everything ingest and the fold decide, as one comparable string.
fn decide() -> String {
    let (cfg, chat) = fixture();
    let (cfg_state, timeline) = project_config(&cfg, None, None);
    let chat_state =
        project_authorized(&chat, None, None, WorkspaceAuthz::Timeline(&timeline)).unwrap();
    let all: Vec<&SessionEventEnvelope> = cfg.iter().chain(chat.iter()).collect();
    let roster = roster_with_batch(Vec::new(), &all, None);
    let verdicts: Vec<String> = all
        .iter()
        .map(|e| format!("{:?}", verdict_for(&roster, e)))
        .collect();

    // The relay ingest path end to end.
    let mut store = EventStore::open_in_memory().unwrap();
    let mut eng = SyncEngine::new(RelayClient::new("http://127.0.0.1:9"), "room")
        .with_room_workspace(Uuid::from_u128(1));
    let batch: Vec<(u64, serde_json::Value)> = all
        .iter()
        .enumerate()
        .map(|(i, e)| (i as u64 + 1, serde_json::to_value(e).unwrap()))
        .collect();
    let applied = eng.apply_fetched(&mut store, &batch).unwrap();
    let stored = store.list(chat[0].session_id).unwrap().len();

    format!(
        "{:?}|{:?}|{:?}|{:?}|{:?}|{applied}|{stored}",
        cfg_state.map(|c| (c.members, c.authz)),
        chat_state.proposals,
        chat_state.authz,
        verdicts,
        roster.certified_devices(),
    )
}

#[test]
fn fold_ignores_env() {
    let clean = decide();
    for (_, var, _) in FOLD_ENV_DENYLIST {
        for value in ["github-signing-keys", "1", "0", "garbage"] {
            std::env::set_var(var, value);
            assert_eq!(decide(), clean, "{var}={value} changed the fold");
        }
    }
    for (_, var, _) in FOLD_ENV_DENYLIST {
        std::env::set_var(var, "option-c");
    }
    assert_eq!(decide(), clean);
}

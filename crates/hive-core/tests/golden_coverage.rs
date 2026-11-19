//! Every `SessionEvent` kind has a golden `hive-envelope-v3` vector (Phase A,
//! item 7: the 0.1 wire freeze). `golden/envelopes_v3.jsonl` holds the
//! vectors; this file checks that every kind a 0.1 build knows has one, and
//! holds the generator for the Phase A additions (run `phase_a_goldens`
//! with `LIGUS_GOLDEN_OUT=<path> --ignored`, then append its lines).

use hive_core::crypto::{DeviceCertificate, SigAlg};
use hive_core::presence::{PasskeyCredential, PresencePolicy};
use hive_core::watermark::Watermark;
use hive_core::workspace_repo::{repo_id, ApplyRequest, WorkspaceRepo};
use hive_core::{
    sign_envelope, ActionProposal, ActorIdentity, ActorKind, ActorStamp, CfgFrontier, MemberRoleChange, SessionEvent,
    SessionEventEnvelope, SigningKeypair, Timestamp, WorkspaceMember, WorkspaceRole,
};
use uuid::Uuid;

const GOLDEN: &str = include_str!("golden/envelopes_v3.jsonl");

/// The kind tag of every variant. An exhaustive match: a new kind doesn't
/// compile until it is listed here, and then needs a golden vector.
fn tag(e: &SessionEvent) -> &'static str {
    use SessionEvent as E;
    match e {
        E::SessionSnapshot { .. } => "sessionSnapshot",
        E::MessageAppended { .. } => "messageAppended",
        E::MessageChunkReceived { .. } => "messageChunkReceived",
        E::MessageCompleted { .. } => "messageCompleted",
        E::MessageRemoved { .. } => "messageRemoved",
        E::TurnClaimed { .. } => "turnClaimed",
        E::MemberAdded { .. } => "memberAdded",
        E::MemberRemoved { .. } => "memberRemoved",
        E::MemberRoleChanged { .. } => "memberRoleChanged",
        E::AgentRosterUpdated { .. } => "agentRosterUpdated",
        E::AgentUpserted { .. } => "agentUpserted",
        E::AgentRemoved { .. } => "agentRemoved",
        E::SessionRuntimeChanged { .. } => "sessionRuntimeChanged",
        E::SessionTitleChanged { .. } => "sessionTitleChanged",
        E::SessionArchivedChanged { .. } => "sessionArchivedChanged",
        E::SkillsUpdated { .. } => "skillsUpdated",
        E::ProposalUpserted { .. } => "proposalUpserted",
        E::ProposalVoteCast { .. } => "proposalVoteCast",
        E::ProposalDismissed { .. } => "proposalDismissed",
        E::ProposalRefined { .. } => "proposalRefined",
        E::ProposalSetOpened { .. } => "proposalSetOpened",
        E::ProposalSetAlternativeAdded { .. } => "proposalSetAlternativeAdded",
        E::ScoutAssessmentPosted { .. } => "scoutAssessmentPosted",
        E::ProposalSetRoundAdvanced { .. } => "proposalSetRoundAdvanced",
        E::ProposalSetQuorumReached { .. } => "proposalSetQuorumReached",
        E::ProposalSetAlternativeStopped { .. } => "proposalSetAlternativeStopped",
        E::ProposalSetTuned { .. } => "proposalSetTuned",
        E::ProposalSetPreflightRecorded { .. } => "proposalSetPreflightRecorded",
        E::ProposalSetDecided { .. } => "proposalSetDecided",
        E::ProposalReverted { .. } => "proposalReverted",
        E::VaultSourcesUpdated { .. } => "vaultSourcesUpdated",
        E::WorkflowDefinitionsUpdated { .. } => "workflowDefinitionsUpdated",
        E::WorkflowRunUpserted { .. } => "workflowRunUpserted",
        E::MessageReactionAdded { .. } => "messageReactionAdded",
        E::MessageReactionRemoved { .. } => "messageReactionRemoved",
        E::AccountKeyRegistered { .. } => "accountKeyRegistered",
        E::AccountKeyRotated { .. } => "accountKeyRotated",
        E::DeviceCertificateAdded { .. } => "deviceCertificateAdded",
        E::DeviceRevoked { .. } => "deviceRevoked",
        E::ChannelCreated { .. } => "channelCreated",
        E::ChannelRenamed { .. } => "channelRenamed",
        E::ChannelReordered { .. } => "channelReordered",
        E::ChannelArchived { .. } => "channelArchived",
        E::ChatChannelChanged { .. } => "chatChannelChanged",
        E::WorkspaceRuntimesUpdated { .. } => "workspaceRuntimesUpdated",
        E::WorkspaceRuntimeUpserted { .. } => "workspaceRuntimeUpserted",
        E::WorkspaceRuntimeRemoved { .. } => "workspaceRuntimeRemoved",
        E::WorkspaceHostsUpdated { .. } => "workspaceHostsUpdated",
        E::WorkspaceHostUpserted { .. } => "workspaceHostUpserted",
        E::WorkspaceHostRemoved { .. } => "workspaceHostRemoved",
        E::CoordinatorLeaseClaimed { .. } => "coordinatorLeaseClaimed",
        E::CoordinatorLeaseReleased { .. } => "coordinatorLeaseReleased",
        E::DearthModeChanged { .. } => "dearthModeChanged",
        E::JoinApproved { .. } => "joinApproved",
        E::AdmissionClosed { .. } => "admissionClosed",
        E::WorkspaceSandboxSet { .. } => "workspaceSandboxSet",
        E::ChatLockdownRaised { .. } => "chatLockdownRaised",
        E::ChatLockdownCleared { .. } => "chatLockdownCleared",
        E::ProposalMarkedStale { .. } => "proposalMarkedStale",
        E::FlowRecorded { .. } => "flowRecorded",
        E::WorkspaceRulesSet { .. } => "workspaceRulesSet",
        E::ProposalApplied { .. } => "proposalApplied",
        E::AgentKeyCertified { .. } => "agentKeyCertified",
        E::WorkspaceKeyRotated { .. } => "workspaceKeyRotated",
        E::DeviceKeyAgreementPublished { .. } => "deviceKeyAgreementPublished",
        E::PasskeyRegistered { .. } => "passkeyRegistered",
        E::PasskeyRemoved { .. } => "passkeyRemoved",
        E::IdentityLinkPublished { .. } => "identityLinkPublished",
        E::IdentityLinkRemoved { .. } => "identityLinkRemoved",
        E::ProposalApplyRequested { .. } => "proposalApplyRequested",
        E::ProposalApplyFailed { .. } => "proposalApplyFailed",
        E::ProposalApplyWithdrawn { .. } => "proposalApplyWithdrawn",
        E::WorkspaceRepoAdded { .. } => "workspaceRepoAdded",
        E::WorkspaceRepoUpdated { .. } => "workspaceRepoUpdated",
        E::WorkspaceRepoRemoved { .. } => "workspaceRepoRemoved",
        E::ChatRepoSet { .. } => "chatRepoSet",
        E::ChatBasePinned { .. } => "chatBasePinned",
        E::ChatBaseRepinned { .. } => "chatBaseRepinned",
        E::AttachmentPolicySet { .. } => "attachmentPolicySet",
        E::AttachmentHashBlocked { .. } => "attachmentHashBlocked",
        E::AttachmentHashUnblocked { .. } => "attachmentHashUnblocked",
        E::AttachmentShared { .. } => "attachmentShared",
        E::AttachmentDeleted { .. } => "attachmentDeleted",
        E::Unknown => "unknown",
    }
}

/// Every tag `tag` can return (kept next to it; the coverage test checks
/// each has a vector, and `tag` agrees with `kind_str`).
const ALL: &[&str] = &[
    "sessionSnapshot", "messageAppended", "messageChunkReceived", "messageCompleted", "messageRemoved", "turnClaimed",
    "memberAdded", "memberRemoved", "memberRoleChanged", "agentRosterUpdated", "agentUpserted", "agentRemoved",
    "sessionRuntimeChanged", "sessionTitleChanged", "sessionArchivedChanged", "skillsUpdated", "proposalUpserted",
    "proposalVoteCast", "proposalDismissed", "proposalRefined", "proposalSetOpened", "proposalSetAlternativeAdded",
    "scoutAssessmentPosted", "proposalSetRoundAdvanced", "proposalSetQuorumReached", "proposalSetAlternativeStopped",
    "proposalSetTuned", "proposalSetPreflightRecorded", "proposalSetDecided", "proposalReverted", "vaultSourcesUpdated",
    "workflowDefinitionsUpdated", "workflowRunUpserted", "messageReactionAdded", "messageReactionRemoved",
    "accountKeyRegistered", "accountKeyRotated", "deviceCertificateAdded", "deviceRevoked", "channelCreated",
    "channelRenamed", "channelReordered", "channelArchived", "chatChannelChanged", "workspaceRuntimesUpdated",
    "workspaceRuntimeUpserted", "workspaceRuntimeRemoved", "workspaceHostsUpdated", "workspaceHostUpserted",
    "workspaceHostRemoved", "coordinatorLeaseClaimed", "coordinatorLeaseReleased", "dearthModeChanged", "joinApproved", "admissionClosed",
    "chatLockdownRaised", "chatLockdownCleared", "proposalMarkedStale", "flowRecorded", "workspaceRulesSet",
    "proposalApplied", "agentKeyCertified", "workspaceKeyRotated", "deviceKeyAgreementPublished", "passkeyRegistered",
    "passkeyRemoved", "identityLinkPublished", "identityLinkRemoved", "proposalApplyRequested", "workspaceRepoAdded",
    "workspaceRepoUpdated", "workspaceRepoRemoved", "chatRepoSet", "chatBasePinned", "proposalApplyFailed",
    "proposalApplyWithdrawn", "workspaceSandboxSet", "chatBaseRepinned", "attachmentPolicySet", "attachmentHashBlocked", "attachmentHashUnblocked", "attachmentShared",
    "attachmentDeleted",
];

#[test]
fn every_kind_has_a_golden_vector() {
    let mut have = std::collections::BTreeSet::new();
    for line in GOLDEN.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        have.insert(v["envelope"]["payload"]["kind"].as_str().unwrap().to_string());
    }
    let missing: Vec<&&str> = ALL.iter().filter(|k| !have.contains(**k)).collect();
    assert!(missing.is_empty(), "kinds without a golden vector: {missing:?}");
    // The list and the decoder agree: each vector decodes to its own kind.
    for line in GOLDEN.lines().filter(|l| !l.trim().is_empty()) {
        let start = line.find("\"envelope\":").unwrap() + "\"envelope\":".len();
        let env: SessionEventEnvelope = serde_json::from_str(&line[start..line.len() - 1]).unwrap();
        assert_eq!(tag(&env.payload), env.payload.kind_str());
        assert!(ALL.contains(&tag(&env.payload)), "{} isn't listed", tag(&env.payload));
    }
}

// ---- the generator for the Phase A vectors ----

fn u(n: u128) -> Uuid {
    Uuid::from_u128(0x3333_0000_0000_4000_8000_0000_0000_0000 | n)
}

fn device() -> SigningKeypair {
    SigningKeypair::from_seed(&[0x5a; 32]).unwrap()
}

fn cases() -> Vec<(&'static str, Uuid, SessionEvent)> {
    let ws = u(2);
    let cfg = hive_core::workspace_config_session_id(ws);
    let chat = u(3);
    let acct_kp = SigningKeypair::from_seed(&[0x6b; 32]).unwrap();
    let wm = vec![Watermark { signer: u(0x51), log: chat, heads: vec![u(0x61), u(0x62)], gaps: vec![] }];
    let wm_gaps = vec![Watermark { signer: u(0x51), log: chat, heads: vec![u(0x61)], gaps: vec![u(0x63)] }];
    let repo = WorkspaceRepo {
        id: repo_id("github.com/zazulam/wefit"),
        canonical: "github.com/zazulam/wefit".into(),
        display_name: "WeFit".into(),
        forge_kind: "github".into(),
        default_branch: "main".into(),
        root_commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
    };
    let mut prop = ActionProposal::file_diff("Fix", "agent", "diff --git a/f b/f\n+1\n".into(), vec!["f".into()]);
    prop.id = u(0x71);
    prop.created_at = Timestamp::epoch();
    prop.repo_id = Some(repo.id);
    prop.base_commit = Some("0123456789abcdef0123456789abcdef01234567".into());
    prop.base_ref = "main".into();
    prop.produced_on = Some(u(1));
    let mut svc = ActorIdentity::new(u(0x91).to_string(), "box", ActorKind::Service);
    svc.account_id = Some(u(0x91));
    let mut link = hive_core::identity_link::IdentityLink {
        link_id: u(0xa1),
        account_id: u(9),
        provider: "github".into(),
        host: "github.com".into(),
        subject_id: "583231".into(),
        handle_at_link: "octocat".into(),
        method: hive_core::identity_link::LinkMethod::KeyPublished,
        statement: String::new(),
        proof: String::new(),
        key_fingerprint: String::new(),
        nonce: String::new(),
        issued_at: Timestamp::epoch(),
        expires_at: Timestamp::epoch(),
    };
    link.statement = "ligus identity link (golden)".into();
    let agent_kp = SigningKeypair::from_seed(&[0x7c; 32]).unwrap();
    let phone_key = p256::ecdsa::SigningKey::from_slice(&[0x11; 32]).unwrap();
    let mut sealed = std::collections::BTreeMap::new();
    sealed.insert(u(1).to_string(), hive_core::SealedBlob { ephemeral_public: vec![7; 32], ciphertext: vec![8; 48] });
    vec![
        ("workspaceRulesSet", cfg, SessionEvent::WorkspaceRulesSet { version: 2, presence: Some(PresencePolicy::ligus()) }),
        ("workspaceRulesSet/self-hosted", cfg, SessionEvent::WorkspaceRulesSet {
            version: 2,
            presence: Some(PresencePolicy { rp_id: "example.org".into(), origins: vec!["https://example.org".into(), "https://id.example.org".into()] }),
        }),
        ("workspaceKeyRotated", cfg, SessionEvent::WorkspaceKeyRotated {
            rotation: hive_core::keys::SignedRotation { version: 1, prev_version: 0, sealed, key_commitment: "ab".repeat(32) },
        }),
        ("deviceKeyAgreementPublished", cfg, SessionEvent::DeviceKeyAgreementPublished { device_id: u(1), ka_public: vec![9; 32] }),
        ("passkeyRegistered", cfg, SessionEvent::PasskeyRegistered {
            credential: PasskeyCredential {
                account_id: u(9),
                credential_id: "AAECAwQFBgcICQoLDA0ODw".into(),
                alg: -7,
                public_key: vec![4; 65],
                label: "MacBook Touch ID".into(),
                recovery: false,
                registered_at: Timestamp::epoch(),
            },
        }),
        ("passkeyRegistered/recovery", cfg, SessionEvent::PasskeyRegistered {
            credential: PasskeyCredential {
                account_id: u(9),
                credential_id: "cmVjb3Zlcnk".into(),
                alg: -8,
                public_key: vec![5; 32],
                label: "Recovery key".into(),
                recovery: true,
                registered_at: Timestamp::epoch(),
            },
        }),
        ("passkeyRemoved", cfg, SessionEvent::PasskeyRemoved { account_id: u(9), credential_id: "AAECAwQFBgcICQoLDA0ODw".into() }),
        ("agentKeyCertified", cfg, SessionEvent::AgentKeyCertified {
            cert: hive_core::AgentKeyCertificate::issue(&device(), u(1), u(0x81), &agent_kp.public_key_bytes(), Timestamp::epoch()),
        }),
        ("identityLinkPublished", cfg, SessionEvent::IdentityLinkPublished { link }),
        ("identityLinkRemoved", cfg, SessionEvent::IdentityLinkRemoved { account_id: u(9), link_id: u(0xa1) }),
        ("proposalApplied", chat, SessionEvent::ProposalApplied {
            proposal_id: u(0x71),
            diff_digest: "cd".repeat(32),
            commit: Some("89abcdef0123456789abcdef0123456789abcdef".into()),
            at: Timestamp::epoch(),
            repo_id: Some(repo.id),
            applied_on: Some(u(1)),
            base_commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
            applied_tree_hash: Some("fedcba9876543210fedcba9876543210fedcba98".into()),
            request_id: Some(u(0xb1)),
        }),
        ("proposalApplyRequested", chat, SessionEvent::ProposalApplyRequested {
            request: ApplyRequest {
                id: u(0xb1),
                proposal_id: u(0x71),
                diff_digest: "cd".repeat(32),
                host_device_id: u(1),
                expected_base_commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
                repo_id: Some(repo.id),
            },
        }),
        ("proposalUpserted/repo", chat, SessionEvent::ProposalUpserted { proposal: prop }),
        ("workspaceRepoAdded", cfg, SessionEvent::WorkspaceRepoAdded { repo: repo.clone() }),
        ("workspaceRepoUpdated", cfg, SessionEvent::WorkspaceRepoUpdated { repo_id: repo.id, display_name: "WeFit app".into(), default_branch: "main".into() }),
        ("workspaceRepoRemoved", cfg, SessionEvent::WorkspaceRepoRemoved { repo_id: repo.id }),
        ("chatRepoSet", chat, SessionEvent::ChatRepoSet { repo_id: Some(repo.id), base_branch: "main".into() }),
        ("chatBasePinned", chat, SessionEvent::ChatBasePinned { repo_id: repo.id, commit: "0123456789abcdef0123456789abcdef01234567".into() }),
        ("memberRemoved/watermark", cfg, SessionEvent::MemberRemoved { member_id: u(9).to_string(), watermark: wm.clone() }),
        ("memberRoleChanged/watermark", cfg, SessionEvent::MemberRoleChanged {
            change: MemberRoleChange { member_id: u(9).to_string(), old_role: WorkspaceRole::Admin, new_role: WorkspaceRole::Contributor },
            watermark: wm.clone(),
        }),
        ("deviceRevoked/watermark", cfg, SessionEvent::DeviceRevoked { device_id: u(0x51), watermark: wm }),
        ("memberAdded/service", cfg, SessionEvent::MemberAdded {
            member: WorkspaceMember { id: svc.id.clone(), actor: svc, role: WorkspaceRole::Contributor, title: String::new(), index: 3, joined_at: Timestamp::epoch() },
            invite: None,
            proof: None,
        }),
        ("deviceCertificateAdded/phone-p256", cfg, SessionEvent::DeviceCertificateAdded {
            certificate: DeviceCertificate::issue_p256(&acct_kp, u(9), u(0x52), &hive_core::crypto::p256_public_key(&phone_key), Timestamp::epoch()),
        }),
        ("memberRemoved/watermark-gaps", cfg, SessionEvent::MemberRemoved { member_id: u(9).to_string(), watermark: wm_gaps }),
    ]
}

/// Writes the Phase A golden lines (signed by the fixed device key, with a
/// stamp that names a frontier and a chain link).
#[test]
#[ignore]
fn phase_a_goldens() {
    let Ok(path) = std::env::var("LIGUS_GOLDEN_OUT") else { return };
    let kp = device();
    let pk: String = kp.public_key_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let mut out = String::new();
    for (i, (label, sid, payload)) in cases().into_iter().enumerate() {
        let mut e = SessionEventEnvelope::new(sid, u(2), 100 + i as i64, payload);
        e.id = u(0x1000 + i as u128);
        e.event_id = u(0x2000 + i as u128);
        e.lamport = 500 + i as u64;
        e.timestamp = Timestamp::epoch();
        let mut actor = ActorIdentity::new(u(9).to_string(), "Mara", ActorKind::Human);
        actor.account_id = Some(u(9));
        actor.device_id = Some(u(1));
        e.actor_stamp = Some(ActorStamp {
            actor,
            recorded_at: Timestamp::epoch(),
            cfg_frontier: Some(CfgFrontier { lamport: 499, event_id: u(0x1fff) }),
            presence: None,
            runtime_effect: false,
            prev: Some(u(0x2fff)),
            sig_alg: SigAlg::Ed25519, log_head: None,
        });
        sign_envelope(&mut e, u(1), &kp);
        let line = format!("{{\"label\":\"{label}\",\"pk\":\"{pk}\",\"envelope\":{}}}", serde_json::to_string(&e).unwrap());
        out.push_str(&line);
        out.push('\n');
    }
    std::fs::write(path, out).unwrap();
}

/// Writes the Phase B golden lines: a box admission (`JoinApproved` with its
/// optional `admission` terms), and, after review, an admission with its
/// redeem key, a redemption with its proof, and an admission's closing. Run
/// with `LIGUS_GOLDEN_OUT=<path> --ignored`, then append the lines.
#[test]
#[ignore]
fn phase_b_goldens() {
    let Ok(path) = std::env::var("LIGUS_GOLDEN_OUT") else { return };
    let kp = device();
    let pk: String = kp.public_key_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let ws = u(2);
    let redeem = hive_core::admission::RedeemKey::from_seed(&[0x5a; 32]).unwrap();
    let svc = {
        let mut a = ActorIdentity::new(u(0x73).to_string(), "build-1", ActorKind::Service);
        a.account_id = Some(u(0x72));
        a
    };
    let cases = [
        ("joinApprovedAdmission/redeemKey", 0, SessionEvent::JoinApproved {
            subject: format!("invite:{}", u(0x71)),
            admission: Some(hive_core::admission::Admission::service(3_600_000, Some(u(0x72)), redeem.public()).for_device(&[0x33; 32])),
        }),
        ("memberAdded/redemption", 1, SessionEvent::MemberAdded {
            member: WorkspaceMember { id: svc.id.clone(), actor: svc.clone(), role: WorkspaceRole::Contributor, title: String::new(), index: 4, joined_at: Timestamp::epoch() },
            invite: Some(u(0x71)),
            proof: Some(redeem.prove(ws, u(0x71), &svc.id, u(1))),
        }),
        ("admissionClosed", 2, SessionEvent::AdmissionClosed { invite: u(0x71), redeemed: Some(u(0x3011)) }),
    ];
    let mut out = String::new();
    for (label, i, payload) in cases {
        let mut e = SessionEventEnvelope::new(hive_core::workspace_config_session_id(ws), ws, 201 + i, payload);
        e.id = u(0x3010 + 2 * i as u128);
        e.event_id = u(0x3011 + 2 * i as u128);
        e.lamport = 601 + i as u64;
        e.timestamp = Timestamp::epoch();
        let mut actor = ActorIdentity::new(u(9).to_string(), "Mara", ActorKind::Human);
        actor.account_id = Some(u(9));
        actor.device_id = Some(u(1));
        e.actor_stamp = Some(ActorStamp {
            actor,
            recorded_at: Timestamp::epoch(),
            cfg_frontier: Some(CfgFrontier { lamport: 600 + i as u64, event_id: u(0x3001) }),
            presence: None,
            runtime_effect: false,
            prev: Some(u(0x3001)),
            sig_alg: SigAlg::Ed25519, log_head: None,
        });
        sign_envelope(&mut e, u(1), &kp);
        out.push_str(&format!("{{\"label\":\"{label}\",\"pk\":\"{pk}\",\"envelope\":{}}}\n", serde_json::to_string(&e).unwrap()));
    }
    std::fs::write(path, out).unwrap();
}

/// Writes the Phase B apply-on-host lines: a host's failure report on an
/// apply request and a person's withdrawal of one. Run with
/// `LIGUS_GOLDEN_OUT=<path> --ignored`, then append the lines.
#[test]
#[ignore]
fn phase_b_apply_goldens() {
    let Ok(path) = std::env::var("LIGUS_GOLDEN_OUT") else { return };
    let kp = device();
    let pk: String = kp.public_key_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let ws = u(2);
    let chat = u(3);
    let cases = [
        ("proposalApplyFailed", 0, SessionEvent::ProposalApplyFailed {
            request_id: u(0xb1),
            proposal_id: u(0x71),
            reason: "the checkout moved since the request (expected 01234567, at 89abcdef)".into(),
        }),
        ("proposalApplyWithdrawn", 1, SessionEvent::ProposalApplyWithdrawn { request_id: u(0xb1), proposal_id: u(0x71) }),
    ];
    let mut out = String::new();
    for (label, i, payload) in cases {
        let mut e = SessionEventEnvelope::new(chat, ws, 301 + i, payload);
        e.id = u(0x4010 + 2 * i as u128);
        e.event_id = u(0x4011 + 2 * i as u128);
        e.lamport = 701 + i as u64;
        e.timestamp = Timestamp::epoch();
        let mut actor = ActorIdentity::new(u(9).to_string(), "Mara", ActorKind::Human);
        actor.account_id = Some(u(9));
        actor.device_id = Some(u(1));
        e.actor_stamp = Some(ActorStamp {
            actor,
            recorded_at: Timestamp::epoch(),
            cfg_frontier: Some(CfgFrontier { lamport: 700, event_id: u(0x4001) }),
            presence: None,
            runtime_effect: false,
            prev: Some(u(0x4002)),
            sig_alg: SigAlg::Ed25519, log_head: None,
        });
        sign_envelope(&mut e, u(1), &kp);
        out.push_str(&format!("{{\"label\":\"{label}\",\"pk\":\"{pk}\",\"envelope\":{}}}\n", serde_json::to_string(&e).unwrap()));
    }
    std::fs::write(path, out).unwrap();
}

/// Writes the lamport-bounds lines (`hive_core::lamport`): an event whose
/// stamp names its log head (`logHead`, `{l, e}`), its lamport one past it,
/// and a lockdown clear naming the raises it clears.
/// Run with `LIGUS_GOLDEN_OUT=<path> --ignored`, then append the lines.
#[test]
#[ignore]
fn lamport_bounds_goldens() {
    let Ok(path) = std::env::var("LIGUS_GOLDEN_OUT") else { return };
    let kp = device();
    let pk: String = kp.public_key_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let ws = u(2);
    let chat = u(3);
    let mut m = hive_core::ChatMessage::new(hive_core::MessageRole::User, "Mara", "after the head");
    m.id = u(0x5004);
    m.created_at = Timestamp::epoch();
    let cfg = hive_core::workspace_config_session_id(ws);
    let cases = [
        ("messageAppended/logHead", 0u64, chat, SessionEvent::MessageAppended { message: m }),
        ("chatLockdownCleared/raises", 1, cfg, SessionEvent::ChatLockdownCleared { chat_id: chat, raises: vec![u(0x5020), u(0x5021)] }),
    ];
    let mut out = String::new();
    for (label, i, sid, payload) in cases {
        let mut e = SessionEventEnvelope::new(sid, ws, 401 + i as i64, payload);
        e.id = u(0x5010 + 2 * i as u128);
        e.event_id = u(0x5011 + 2 * i as u128);
        e.timestamp = Timestamp::epoch();
        let mut actor = ActorIdentity::new(u(9).to_string(), "Mara", ActorKind::Human);
        actor.account_id = Some(u(9));
        actor.device_id = Some(u(1));
        e.actor_stamp = Some(ActorStamp {
            actor,
            recorded_at: Timestamp::epoch(),
            cfg_frontier: Some(CfgFrontier { lamport: 800, event_id: u(0x5001) }),
            presence: None,
            runtime_effect: false,
            prev: Some(u(0x5002)),
            sig_alg: SigAlg::Ed25519,
            log_head: None,
        });
        hive_core::lamport::stamp_next(&mut e, Some(CfgFrontier { lamport: 41, event_id: u(0x5003) }));
        sign_envelope(&mut e, u(1), &kp);
        out.push_str(&format!("{{\"label\":\"{label}\",\"pk\":\"{pk}\",\"envelope\":{}}}\n", serde_json::to_string(&e).unwrap()));
    }
    std::fs::write(path, out).unwrap();
}

/// Writes the re-pin line (review residual 3): `chatBaseRepinned`, a
/// chat's pinned base moved from the pin in force to another commit. Run
/// with `LIGUS_GOLDEN_OUT=<path> --ignored`, then append the line.
#[test]
#[ignore]
fn repin_goldens() {
    let Ok(path) = std::env::var("LIGUS_GOLDEN_OUT") else { return };
    let kp = device();
    let pk: String = kp.public_key_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let ws = u(2);
    let chat = u(3);
    let payload = SessionEvent::ChatBaseRepinned {
        repo_id: repo_id("github.com/zazulam/wefit"),
        commit: "89abcdef0123456789abcdef0123456789abcdef".into(),
        previous: "0123456789abcdef0123456789abcdef01234567".into(),
    };
    let mut e = SessionEventEnvelope::new(chat, ws, 501, payload);
    e.id = u(0x6010);
    e.event_id = u(0x6011);
    e.lamport = 901;
    e.timestamp = Timestamp::epoch();
    let mut actor = ActorIdentity::new(u(9).to_string(), "Mara", ActorKind::Human);
    actor.account_id = Some(u(9));
    actor.device_id = Some(u(1));
    e.actor_stamp = Some(ActorStamp {
        actor,
        recorded_at: Timestamp::epoch(),
        cfg_frontier: Some(CfgFrontier { lamport: 900, event_id: u(0x6001) }),
        presence: None,
        runtime_effect: false,
        prev: Some(u(0x6002)),
        sig_alg: SigAlg::Ed25519,
        log_head: None,
    });
    sign_envelope(&mut e, u(1), &kp);
    std::fs::write(path, format!("{{\"label\":\"chatBaseRepinned\",\"pk\":\"{pk}\",\"envelope\":{}}}\n", serde_json::to_string(&e).unwrap())).unwrap();
}

/// Writes the file-attachment lines (issue #118): the policy, the blocklist,
/// a share, an agent's approved offer carried out, and a deletion. Run with
/// `LIGUS_GOLDEN_OUT=<path> --ignored`, then append the lines.
#[test]
#[ignore]
fn attachment_goldens() {
    use hive_core::attachment::{AttachmentPolicy, AttachmentRef};
    let Ok(path) = std::env::var("LIGUS_GOLDEN_OUT") else { return };
    let kp = device();
    let pk: String = kp.public_key_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let ws = u(2);
    let chat = u(3);
    let cfg = hive_core::workspace_config_session_id(ws);
    let file = AttachmentRef {
        id: u(0x6001),
        blob_id: "q2Xk7d0Rk1n3V6t1yq0gq8Bv8m8i2C0u4K0z0sVQ5aE".into(),
        sha256: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".into(),
        size: 4,
        content_type: "text/plain".into(),
        name: "notes v2.txt".into(),
        message_id: Some(u(0x6002)),
        proposal_id: None,
    };
    let offered = AttachmentRef { id: u(0x6003), proposal_id: Some(u(0x6004)), message_id: None, name: "report.pdf".into(), content_type: "application/pdf".into(), ..file.clone() };
    let cases = [
        ("attachmentPolicySet", 0u64, cfg, SessionEvent::AttachmentPolicySet { policy: AttachmentPolicy { agents: true, ..AttachmentPolicy::recommended() } }),
        ("attachmentPolicySet/off", 1, cfg, SessionEvent::AttachmentPolicySet { policy: AttachmentPolicy::default() }),
        ("attachmentHashBlocked", 2, cfg, SessionEvent::AttachmentHashBlocked { sha256: "ab".repeat(32) }),
        ("attachmentHashUnblocked", 3, cfg, SessionEvent::AttachmentHashUnblocked { sha256: "ab".repeat(32) }),
        ("attachmentShared", 4, chat, SessionEvent::AttachmentShared { attachment: file }),
        ("attachmentShared/proposal", 5, chat, SessionEvent::AttachmentShared { attachment: offered }),
        ("attachmentDeleted", 6, chat, SessionEvent::AttachmentDeleted { attachment_id: u(0x6001) }),
    ];
    let mut out = String::new();
    for (label, i, sid, payload) in cases {
        let mut e = SessionEventEnvelope::new(sid, ws, 501 + i as i64, payload);
        e.id = u(0x6010 + 2 * i as u128);
        e.event_id = u(0x6011 + 2 * i as u128);
        e.timestamp = Timestamp::epoch();
        let mut actor = ActorIdentity::new(u(9).to_string(), "Mara", ActorKind::Human);
        actor.account_id = Some(u(9));
        actor.device_id = Some(u(1));
        e.actor_stamp = Some(ActorStamp {
            actor,
            recorded_at: Timestamp::epoch(),
            cfg_frontier: Some(CfgFrontier { lamport: 900, event_id: u(0x6000) }),
            presence: None,
            runtime_effect: false,
            prev: None,
            sig_alg: SigAlg::Ed25519,
            log_head: None,
        });
        hive_core::lamport::stamp_next(&mut e, Some(CfgFrontier { lamport: 50 + i, event_id: u(0x6005) }));
        sign_envelope(&mut e, u(1), &kp);
        out.push_str(&format!("{{\"label\":\"{label}\",\"pk\":\"{pk}\",\"envelope\":{}}}\n", serde_json::to_string(&e).unwrap()));
    }
    std::fs::write(path, out).unwrap();
}

/// Writes the workspace-memory and task-state lines
/// (`docs/design/v2/workspace-memory.md`): one `flowRecorded` vector per new
/// record type. Run with `LIGUS_GOLDEN_OUT=<path> --ignored`, then append.
#[test]
#[ignore]
fn memory_goldens() {
    use hive_core::flow::FlowRecord;
    use hive_core::memory::*;
    let Ok(path) = std::env::var("LIGUS_GOLDEN_OUT") else { return };
    let kp = device();
    let pk: String = kp.public_key_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let ws = u(2);
    let chat = u(3);
    let cfg = hive_core::workspace_config_session_id(ws);
    let source = MemorySource { chat_id: Some(chat), message_id: Some(u(0x6031)), turn_id: Some(u(0x6032)), egress: None };
    let cases = [
        ("flowRecorded/memoryWritten", cfg, FlowRecord::MemoryWritten(MemoryWrite {
            entry_id: u(0x6001),
            base: Some(u(0x6011)),
            title: "Relay ports".into(),
            body: "The team relay listens on 8443.".into(),
            tags: vec!["infra".into()],
            source: source.clone(),
            resolves: vec![u(0x6012)],
        })),
        ("flowRecorded/memoryRemoved", cfg, FlowRecord::MemoryRemoved(MemoryRemoval { entry_id: u(0x6001), base: u(0x6013), source: MemorySource::default(), resolves: vec![] })),
        ("flowRecorded/memoryProposed", cfg, FlowRecord::MemoryProposed(MemoryProposal {
            proposal_id: u(0x6021),
            op: MemoryOp::Edit { entry_id: u(0x6001), base: u(0x6011), title: "Relay ports".into(), body: "8443, and 443 behind the proxy.".into(), tags: vec![] },
            proposer: u(0x81).to_string(),
            source,
            rationale: "seen in the deploy chat".into(),
        })),
        ("flowRecorded/memoryProposalDecided", cfg, FlowRecord::MemoryProposalDecided(MemoryDecision { proposal_id: u(0x6021), accepted: true, note: "thanks".into(), resolves: vec![] })),
        ("flowRecorded/taskState", chat, FlowRecord::TaskState(TaskStateUpdate {
            task_id: chat,
            agent_key: u(0x81).to_string(),
            turn_id: u(0x6032),
            status: TaskStatus::Blocked,
            summary: "Waiting on the schema decision".into(),
            progress: Some(40),
            open_questions: vec!["Keep the v1 route?".into()],
            partial_results: vec!["Drafted the migration".into()],
            ttl_ms: Some(3_600_000),
        })),
    ];
    let mut out = String::new();
    for (i, (label, sid, record)) in cases.into_iter().enumerate() {
        let mut e = SessionEventEnvelope::new(sid, ws, 501 + i as i64, SessionEvent::FlowRecorded { record });
        e.id = u(0x6100 + 2 * i as u128);
        e.event_id = u(0x6101 + 2 * i as u128);
        e.timestamp = Timestamp::epoch();
        let mut actor = ActorIdentity::new(u(9).to_string(), "Mara", ActorKind::Human);
        actor.account_id = Some(u(9));
        actor.device_id = Some(u(1));
        e.actor_stamp = Some(ActorStamp {
            actor,
            recorded_at: Timestamp::epoch(),
            cfg_frontier: Some(CfgFrontier { lamport: 900, event_id: u(0x6050) }),
            presence: None,
            runtime_effect: false,
            prev: Some(u(0x6051)),
            sig_alg: SigAlg::Ed25519,
            log_head: None,
        });
        hive_core::lamport::stamp_next(&mut e, Some(CfgFrontier { lamport: 60, event_id: u(0x6052) }));
        sign_envelope(&mut e, u(1), &kp);
        out.push_str(&format!("{{\"label\":\"{label}\",\"pk\":\"{pk}\",\"envelope\":{}}}\n", serde_json::to_string(&e).unwrap()));
    }
    std::fs::write(path, out).unwrap();
}

/// The memory and task-state vectors are present and decode to their types.
#[test]
fn memory_goldens_are_present() {
    for t in ["memoryWritten", "memoryRemoved", "memoryProposed", "memoryProposalDecided", "taskState"] {
        let line = GOLDEN
            .lines()
            .find(|l| l.contains(&format!("\"label\":\"flowRecorded/{t}\"")))
            .unwrap_or_else(|| panic!("no golden vector for {t}"));
        let start = line.find("\"envelope\":").unwrap() + "\"envelope\":".len();
        let env: SessionEventEnvelope = serde_json::from_str(&line[start..line.len() - 1]).unwrap();
        let SessionEvent::FlowRecorded { record } = env.payload else { panic!("{t}") };
        assert_eq!(record.type_str(), t);
    }
}

/// Writes the Phase C line: the sandbox grants (`WorkspaceSandboxSet`) with a
/// workspace path grant and one agent's paths and shell grant. Run with
/// `LIGUS_GOLDEN_OUT=<path> --ignored`, then append the line.
#[test]
#[ignore]
fn phase_c_goldens() {
    let Ok(path) = std::env::var("LIGUS_GOLDEN_OUT") else { return };
    let kp = device();
    let pk: String = kp.public_key_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let ws = u(2);
    let mut grants = hive_core::sandbox::SandboxGrants::default();
    grants.workspace.read_only.push("~/.m2/repository".into());
    grants.agents.insert(
        u(0xa1),
        hive_core::sandbox::AgentGrant { read_only: vec!["/opt/datasets".into()], read_write: vec!["~/scratch/agent".into()], shell: true },
    );
    let cfg = hive_core::workspace_config_session_id(ws);
    let mut e = SessionEventEnvelope::new(cfg, ws, 501, SessionEvent::WorkspaceSandboxSet { grants });
    e.id = u(0x6010);
    e.event_id = u(0x6011);
    e.timestamp = Timestamp::epoch();
    let mut actor = ActorIdentity::new(u(9).to_string(), "Mara", ActorKind::Human);
    actor.account_id = Some(u(9));
    actor.device_id = Some(u(1));
    e.actor_stamp = Some(ActorStamp {
        actor,
        recorded_at: Timestamp::epoch(),
        cfg_frontier: Some(CfgFrontier { lamport: 900, event_id: u(0x6001) }),
        presence: None,
        runtime_effect: false,
        prev: Some(u(0x6002)),
        sig_alg: SigAlg::Ed25519,
        log_head: None,
    });
    hive_core::lamport::stamp_next(&mut e, Some(CfgFrontier { lamport: 900, event_id: u(0x6001) }));
    sign_envelope(&mut e, u(1), &kp);
    std::fs::write(path, format!("{{\"label\":\"workspaceSandboxSet\",\"pk\":\"{pk}\",\"envelope\":{}}}\n", serde_json::to_string(&e).unwrap())).unwrap();
}

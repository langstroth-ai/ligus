//! Golden vectors for phone-signed actions (Phase B): what a phone client
//! (the iOS app) must accept and refuse before it signs a box's draft.
//!
//! `golden/phone_actions_v1.jsonl`: one line per case, with `label`,
//! `workspaceId`, `phoneDeviceId`, `accountId`, the `action` the phone asked
//! for (the `POST /v1/phone-actions/prepare` JSON), the passkey `challenge`
//! (base64url) and `assertion` when the phone ran its passkey, the box's
//! `preimage` (base64url), the phone's `publicKey` (SEC1 uncompressed, hex),
//! its `signature` (base64url, ECDSA P-256 / SHA-256, `r || s`, low S) over
//! the preimage for the accepted cases, and `expect`: `"ok"` or the exact
//! refusal of `hive_core::phone_action::check_phone_action_preimage`.
//! Regenerate with `LIGUS_GOLDEN_OUT=<path> --ignored phone_action_goldens`
//! (only when adding cases: existing lines are frozen).

use hive_core::crypto::{envelope_preimage, sign_p256, verify_p256, SigAlg};
use hive_core::flow::{FlowRecord, HumanAnswered};
use hive_core::phone_action::{check_phone_action_preimage, Confirmed, PhoneAction};
use hive_core::presence::{b64url, unb64url, PresenceAssertion};
use hive_core::workspace_repo::ApplyRequest;
use hive_core::{
    workspace_config_session_id, ActorIdentity, ActorKind, ActorStamp, CfgFrontier, ChatMessage, MessageRole, ProposalApproval, SessionEvent,
    SessionEventEnvelope, Timestamp, WorkspaceRole,
};
use uuid::Uuid;

const GOLDEN: &str = include_str!("golden/phone_actions_v1.jsonl");

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn phone_action_vectors_check_as_frozen() {
    let mut n = 0;
    for line in GOLDEN.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let label = v["label"].as_str().unwrap();
        let uuid = |k: &str| Uuid::parse_str(v[k].as_str().unwrap()).unwrap();
        let action: PhoneAction = serde_json::from_value(v["action"].clone()).unwrap();
        let preimage = unb64url(v["preimage"].as_str().unwrap()).unwrap();
        let challenge: Option<[u8; 32]> = v["challenge"].as_str().map(|c| unb64url(c).unwrap().try_into().unwrap());
        let assertion: Option<PresenceAssertion> = v.get("assertion").filter(|a| !a.is_null()).map(|a| serde_json::from_value(a.clone()).unwrap());
        let confirmed = challenge.as_ref().map(|c| Confirmed { challenge: c, assertion: assertion.as_ref() });
        let got = check_phone_action_preimage(&preimage, confirmed, uuid("workspaceId"), uuid("phoneDeviceId"), uuid("accountId"), &action);
        let expect = v["expect"].as_str().unwrap();
        match got {
            Ok(_) => {
                assert_eq!(expect, "ok", "{label}");
                let pk = unhex(v["publicKey"].as_str().unwrap());
                let sig = unb64url(v["signature"].as_str().unwrap()).unwrap();
                verify_p256(&pk, &preimage, &sig).unwrap_or_else(|e| panic!("{label}: {e:?}"));
            }
            Err(why) => assert_eq!(why, expect, "{label}"),
        }
        n += 1;
    }
    assert!(n >= 14, "{n} vectors");
}

// ---- the generator ----

fn u(n: u128) -> Uuid {
    Uuid::from_u128(0x5555_0000_0000_4000_8000_0000_0000_0000 | n)
}

struct Ids {
    ws: Uuid,
    chat: Uuid,
    phone: Uuid,
    account: Uuid,
    proposal: Uuid,
    host: Uuid,
}

fn ids() -> Ids {
    Ids { ws: u(1), chat: u(2), phone: u(3), account: u(4), proposal: u(5), host: u(6) }
}

fn actor(i: &Ids) -> ActorIdentity {
    let mut a = ActorIdentity::new(i.account.to_string(), "Mara", ActorKind::Human);
    a.account_id = Some(i.account);
    a.device_id = Some(i.phone);
    a
}

const DIGEST: &str = "1f6c3a3c62b9f0a4f2a7f8f7a1a5c6d3e2b1a0f9e8d7c6b5a4f3e2d1c0b9a8f7";
const BASE: &str = "0123456789abcdef0123456789abcdef01234567";

fn action(i: &Ids, kind: &str) -> PhoneAction {
    match kind {
        "post" => PhoneAction::Post { chat_id: i.chat, text: "Ship it after lunch.".into() },
        "vote" => PhoneAction::Vote { chat_id: i.chat, proposal_id: i.proposal, diff_digest: DIGEST.into(), approved: true },
        "answer" => PhoneAction::Answer { chat_id: i.chat, ask_id: u(7), answer: "yes".into(), option_index: Some(0) },
        "clearLockdown" => PhoneAction::ClearLockdown { chat_id: i.chat },
        "requestApply" => PhoneAction::RequestApply {
            chat_id: i.chat,
            proposal_id: i.proposal,
            diff_digest: DIGEST.into(),
            host_device_id: i.host,
            expected_base_commit: Some(BASE.into()),
            repo_id: None,
        },
        _ => PhoneAction::WithdrawApply { chat_id: i.chat, proposal_id: i.proposal, request_id: u(8) },
    }
}

/// The event a box drafts for `a` (box-chosen ids fixed for the vector).
fn payload(i: &Ids, a: &PhoneAction) -> SessionEvent {
    match a {
        PhoneAction::Post { text, .. } => {
            let mut m = ChatMessage::new(MessageRole::User, "Mara", text.clone());
            m.id = u(0x10);
            m.created_at = Timestamp::epoch();
            m.actor_identity = Some(actor(i));
            SessionEvent::MessageAppended { message: m }
        }
        PhoneAction::Vote { proposal_id, diff_digest, approved, .. } => SessionEvent::ProposalVoteCast {
            proposal_id: *proposal_id,
            approval: ProposalApproval {
                actor_id: i.account.to_string(),
                role: WorkspaceRole::Contributor,
                approved: *approved,
                created_at: Timestamp::epoch(),
                diff_digest: Some(diff_digest.clone()),
            },
        },
        PhoneAction::Answer { ask_id, answer, option_index, .. } => SessionEvent::FlowRecorded {
            record: FlowRecord::HumanAnswered(HumanAnswered { ask_id: *ask_id, answer: answer.clone(), option_index: *option_index, by: "Mara".into(), at_ms: 1_700_000_000_000 }),
        },
        PhoneAction::ClearLockdown { chat_id } => SessionEvent::ChatLockdownCleared { chat_id: *chat_id },
        PhoneAction::RequestApply { proposal_id, diff_digest, host_device_id, expected_base_commit, repo_id, .. } => SessionEvent::ProposalApplyRequested {
            request: ApplyRequest {
                id: u(0x11),
                proposal_id: *proposal_id,
                diff_digest: diff_digest.clone(),
                host_device_id: *host_device_id,
                expected_base_commit: expected_base_commit.clone(),
                repo_id: *repo_id,
            },
        },
        PhoneAction::WithdrawApply { proposal_id, request_id, .. } => SessionEvent::ProposalApplyWithdrawn { request_id: *request_id, proposal_id: *proposal_id },
    }
}

fn draft(i: &Ids, a: &PhoneAction, n: u128, ev: SessionEvent) -> SessionEventEnvelope {
    let mut e = SessionEventEnvelope::new(a.session_id(i.ws), i.ws, 40 + n as i64, ev);
    e.id = u(0x100 + n);
    e.event_id = u(0x200 + n);
    e.lamport = 900 + n as u64;
    e.timestamp = Timestamp::epoch();
    e.actor_stamp = Some(ActorStamp {
        actor: actor(i),
        recorded_at: Timestamp::epoch(),
        cfg_frontier: Some(CfgFrontier { lamport: 899, event_id: u(0x300) }),
        presence: None,
        runtime_effect: false,
        prev: Some(u(0x301)),
        sig_alg: SigAlg::P256,
    });
    e
}

#[test]
#[ignore]
fn phone_action_goldens() {
    let Ok(path) = std::env::var("LIGUS_GOLDEN_OUT") else { return };
    let i = ids();
    let key = p256::ecdsa::SigningKey::from_slice(&[0x11; 32]).unwrap();
    let pk = hex(&hive_core::crypto::p256_public_key(&key));
    let mut out = String::new();
    let mut n = 0u128;
    // (label, kind, with presence, tweak) — tweak 0: as drafted.
    let cases: &[(&str, &str, bool, u8)] = &[
        ("post", "post", false, 0),
        ("vote", "vote", false, 0),
        ("vote/presence", "vote", true, 0),
        ("answer", "answer", false, 0),
        ("clearLockdown/presence", "clearLockdown", true, 0),
        ("requestApply", "requestApply", false, 0),
        ("requestApply/presence", "requestApply", true, 0),
        ("withdrawApply", "withdrawApply", false, 0),
        ("refuse/vote-flipped", "vote", false, 1),
        ("refuse/post-extra-field", "post", false, 2),
        ("refuse/presence-not-run", "vote", true, 3),
        ("refuse/trailing-byte", "post", false, 4),
        ("refuse/other-host", "requestApply", false, 5),
        ("refuse/clearLockdown-on-chat-log", "clearLockdown", false, 6),
        ("refuse/vote-for-someone-else", "vote", false, 7),
    ];
    for (label, kind, presence, tweak) in cases {
        n += 1;
        let asked = action(&i, kind);
        let mut ev = payload(&i, &asked);
        match tweak {
            1 => {
                if let SessionEvent::ProposalVoteCast { approval, .. } = &mut ev {
                    approval.approved = false;
                }
            }
            5 => {
                if let SessionEvent::ProposalApplyRequested { request } = &mut ev {
                    request.host_device_id = u(0x66);
                }
            }
            7 => {
                if let SessionEvent::ProposalVoteCast { approval, .. } = &mut ev {
                    approval.actor_id = "someone-else".into();
                }
            }
            _ => {}
        }
        let mut env = draft(&i, &asked, n, ev);
        if *tweak == 6 {
            env.session_id = i.chat;
        }
        let mut challenge = None;
        let mut assertion = None;
        if *presence {
            let c = hive_core::presence::challenge(&env);
            let a = hive_core::testkit::test_recovery_key(i.account).assert(&c);
            env.actor_stamp.as_mut().unwrap().presence = Some(a.clone());
            env.clear_raw();
            challenge = Some(c);
            if *tweak != 3 {
                assertion = Some(a);
            } else {
                challenge = None;
            }
        }
        let mut preimage = envelope_preimage(&env);
        if *tweak == 2 {
            // `"zz": 1` in the payload, lengths fixed up.
            preimage = with_extra_payload_field(&preimage);
        }
        if *tweak == 4 {
            preimage.push(0);
        }
        let confirmed = challenge.as_ref().map(|c| Confirmed { challenge: c, assertion: assertion.as_ref() });
        let expect = match check_phone_action_preimage(&preimage, confirmed, i.ws, i.phone, i.account, &asked) {
            Ok(_) => "ok".to_string(),
            Err(why) => why.to_string(),
        };
        assert_eq!(expect == "ok", *tweak == 0, "{label}: {expect}");
        let mut line = serde_json::json!({
            "label": label,
            "workspaceId": i.ws,
            "phoneDeviceId": i.phone,
            "accountId": i.account,
            "action": asked,
            "preimage": b64url(&preimage),
            "publicKey": pk,
            "expect": expect,
        });
        if let Some(c) = challenge {
            line["challenge"] = serde_json::json!(b64url(&c));
        }
        if let Some(a) = assertion {
            line["assertion"] = serde_json::to_value(a).unwrap();
        }
        if *tweak == 0 {
            line["signature"] = serde_json::json!(b64url(&sign_p256(&key, &preimage)));
        }
        out.push_str(&serde_json::to_string(&line).unwrap());
        out.push('\n');
    }
    let _ = workspace_config_session_id(i.ws);
    std::fs::write(path, out).unwrap();
}

fn with_extra_payload_field(preimage: &[u8]) -> Vec<u8> {
    let head = b"hive-envelope-v3\0".len() + 16 + 16 + 8 + 8 + 16;
    let mut out = preimage[..head].to_vec();
    let mut rest = &preimage[head..];
    for k in 0..4 {
        let len = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
        let mut json = rest[4..4 + len].to_vec();
        rest = &rest[4 + len..];
        if k == 3 {
            let mut v: serde_json::Value = serde_json::from_slice(&json).unwrap();
            v.as_object_mut().unwrap().insert("zz".into(), serde_json::json!(1));
            json = serde_json::to_vec(&v).unwrap();
        }
        out.extend_from_slice(&(json.len() as u32).to_le_bytes());
        out.extend_from_slice(&json);
    }
    out
}

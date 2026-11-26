//! P-256 (Secure Enclave) signatures for phone device keys (Phase A, item 6),
//! cross-checked against Apple CryptoKit.
//!
//! `golden/p256_cryptokit.jsonl` was produced by
//! `scripts/p256-vectors/sign.swift` (CryptoKit `P256.Signing`, the API the
//! iOS app's Secure Enclave key uses, with S normalized to the low half, as
//! the phone must): each line holds a 65-byte public key, the
//! `hive-envelope-v3` preimage of an envelope tagged `sigAlg: p256` and the
//! 64-byte `r || s` signature over it, plus the envelope itself. The inputs
//! come from `p256_vector_inputs` below (run with `LIGUS_P256_INPUTS=<path>`
//! and `--ignored`). Every build must keep verifying them.

use ligus_core::crypto::{envelope_preimage, verify_envelope, verify_p256, SigAlg};
use ligus_core::{
    ActorIdentity, ActorKind, ActorStamp, ChatMessage, MessageRole, ProposalApproval, SessionEvent,
    SessionEventEnvelope, Timestamp, WorkspaceRole,
};
use uuid::Uuid;

const GOLDEN: &str = include_str!("golden/p256_cryptokit.jsonl");

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn cryptokit_p256_vectors_verify() {
    let mut n = 0;
    for line in GOLDEN.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let label = v["label"].as_str().unwrap();
        let pk = unhex(v["pk"].as_str().unwrap());
        let preimage = unhex(v["preimage"].as_str().unwrap());
        let sig = unhex(v["sig"].as_str().unwrap());
        verify_p256(&pk, &preimage, &sig).unwrap_or_else(|e| panic!("{label}: {e:?}"));
        // Parse the envelope from its exact bytes (a `Value` would reorder keys).
        let start = line.find("\"envelope\":").unwrap() + "\"envelope\":".len();
        let mut env: SessionEventEnvelope =
            serde_json::from_str(&line[start..line.len() - 1]).unwrap();
        assert_eq!(
            env.actor_stamp.as_ref().unwrap().sig_alg,
            SigAlg::P256,
            "{label}"
        );
        assert_eq!(
            envelope_preimage(&env),
            preimage,
            "{label}: the preimage moved"
        );
        env.signature = Some(sig.clone());
        verify_envelope(&env, &pk).unwrap_or_else(|e| panic!("{label}: {e:?}"));
        // Any flipped bit fails.
        let mut bad = sig.clone();
        bad[10] ^= 1;
        assert!(verify_p256(&pk, &preimage, &bad).is_err(), "{label}");
        n += 1;
    }
    assert!(n >= 3);
}

/// The unsigned envelopes the vectors sign (deterministic).
fn inputs() -> Vec<(&'static str, SessionEventEnvelope)> {
    let ws = Uuid::from_u128(0x2222_0000_0000_4000_8000_0000_0000_0002);
    let chat = Uuid::from_u128(0x2222_0000_0000_4000_8000_0000_0000_0003);
    let phone = Uuid::from_u128(0x2222_0000_0000_4000_8000_0000_0000_0007);
    let account = Uuid::from_u128(0x2222_0000_0000_4000_8000_0000_0000_0009);
    let mut actor = ActorIdentity::new(account.to_string(), "Mara", ActorKind::Human);
    actor.account_id = Some(account);
    actor.device_id = Some(phone);
    let mut msg = ChatMessage::new(MessageRole::User, "Mara", "from my phone");
    msg.id = Uuid::from_u128(0x2222_0000_0000_4000_8000_0000_0000_00a1);
    msg.created_at = Timestamp::epoch();
    let approval = ProposalApproval {
        actor_id: account.to_string(),
        role: WorkspaceRole::Contributor,
        approved: true,
        created_at: Timestamp::epoch(),
        diff_digest: Some("ab".repeat(32)),
    };
    let cases = vec![
        (
            "phone/messageAppended",
            chat,
            SessionEvent::MessageAppended { message: msg },
        ),
        (
            "phone/proposalVoteCast",
            chat,
            SessionEvent::ProposalVoteCast {
                proposal_id: Uuid::from_u128(0xb1),
                approval,
            },
        ),
        (
            "phone/deviceRevoked-self",
            ligus_core::workspace_config_session_id(ws),
            SessionEvent::DeviceRevoked {
                device_id: phone,
                watermark: vec![],
            },
        ),
    ];
    cases
        .into_iter()
        .enumerate()
        .map(|(i, (label, sid, payload))| {
            let mut e = SessionEventEnvelope::new(sid, ws, 1 + i as i64, payload);
            e.id = Uuid::from_u128(0x2222_0000_0000_4000_8000_0000_0000_1000 + i as u128);
            e.event_id = Uuid::from_u128(0x2222_0000_0000_4000_8000_0000_0000_2000 + i as u128);
            e.lamport = 300 + i as u64;
            e.timestamp = Timestamp::epoch();
            e.actor_stamp = Some(ActorStamp {
                actor: actor.clone(),
                recorded_at: Timestamp::epoch(),
                cfg_frontier: None,
                presence: None,
                runtime_effect: false,
                prev: None,
                sig_alg: SigAlg::P256,
                log_head: None,
            });
            e.signer_device_id = Some(phone);
            (label, e)
        })
        .collect()
}

/// Writes `{label, preimage, envelopeText}` lines for the Swift signer.
#[test]
#[ignore]
fn p256_vector_inputs() {
    let Ok(path) = std::env::var("LIGUS_P256_INPUTS") else {
        return;
    };
    let mut out = String::new();
    for (label, e) in inputs() {
        // The envelope travels as text so the signer copies its exact bytes.
        let line = serde_json::json!({ "label": label, "preimage": hex(&envelope_preimage(&e)), "envelopeText": serde_json::to_string(&e).unwrap() });
        out.push_str(&line.to_string());
        out.push('\n');
    }
    std::fs::write(path, out).unwrap();
}

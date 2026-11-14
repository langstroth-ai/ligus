//! Signatures are checked over the bytes a device received, and those bytes
//! survive storage and re-sync.
//!
//! `golden/envelopes_v3.jsonl` holds signed envelopes generated with the
//! 534c8d0b build (one per event kind that has a payload, plus legacy
//! shapes: no `scope`, no `lamport`, no actor stamp, a float-bearing flow
//! record, a workspace-scoped event). Each line is
//! `{"label":…,"pk":<hex device key>,"envelope":<exact wire JSON>}`. They pin
//! the `hive-envelope-v3` preimage: every build must keep verifying them.

use hive_core::{verify_envelope, SessionEventEnvelope};

const GOLDEN: &str = include_str!("golden/envelopes_v3.jsonl");

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

/// (label, public key, the envelope's exact wire text) per golden line.
fn golden() -> Vec<(String, Vec<u8>, String)> {
    GOLDEN
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            let label = v["label"].as_str().unwrap().to_string();
            let pk = unhex(v["pk"].as_str().unwrap());
            let start = line.find("\"envelope\":").unwrap() + "\"envelope\":".len();
            let wire = line[start..line.len() - 1].to_string();
            (label, pk, wire)
        })
        .collect()
}

#[test]
fn golden_envelopes_verify() {
    let vectors = golden();
    assert!(vectors.len() >= 24);
    for (label, pk, wire) in vectors {
        let env: SessionEventEnvelope = serde_json::from_str(&wire).unwrap_or_else(|e| panic!("{label}: {e}"));
        verify_envelope(&env, &pk).unwrap_or_else(|e| panic!("{label} no longer verifies: {e}"));
    }
}

/// Every golden vector re-serializes to exactly the bytes it was received as
/// and still verifies over them.
#[test]
fn golden_envelopes_verify_byte_for_byte() {
    for (label, pk, wire) in golden() {
        let env: SessionEventEnvelope = serde_json::from_str(&wire).unwrap();
        assert_eq!(serde_json::to_string(&env).unwrap(), wire, "{label}: re-emitted bytes differ");
        let again: SessionEventEnvelope = serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        verify_envelope(&again, &pk).unwrap_or_else(|e| panic!("{label}: {e}"));
        assert!(env.raw().is_some());
    }
}

use hive_core::{sign_envelope, ChatSession, SessionEvent, SigningKeypair};
use proptest::prelude::*;
use serde_json::{json, Value};
use uuid::Uuid;

/// A small random JSON value (what a newer build might add).
fn arb_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| json!(n)),
        (-1.0e9f64..1.0e9).prop_map(|f| json!(f)),
        "[a-zA-Z0-9 ✓\"\\\\]{0,12}".prop_map(Value::String),
    ];
    leaf.prop_recursive(3, 16, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::btree_map("[a-z]{1,6}", inner, 0..4)
                .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

/// Insert `extra` into every `nth`-selected object inside `v` (any depth),
/// under keys no build knows.
fn inject(v: &mut Value, extra: &Value, pick: &mut impl Iterator<Item = bool>, depth: usize) {
    match v {
        Value::Object(map) => {
            for (_, child) in map.iter_mut() {
                inject(child, extra, pick, depth + 1);
            }
            if pick.next().unwrap_or(false) {
                map.insert(format!("zzFuture{depth}"), extra.clone());
            }
        }
        Value::Array(items) => {
            for item in items {
                inject(item, extra, pick, depth + 1);
            }
        }
        _ => {}
    }
}

fn sample_events() -> Vec<SessionEvent> {
    let mut msg = hive_core::ChatMessage::new(hive_core::MessageRole::User, "Mara", "hi");
    msg.actor_identity = Some(hive_core::ActorIdentity::new("m", "Mara", hive_core::ActorKind::Human));
    vec![
        SessionEvent::MessageAppended { message: msg },
        SessionEvent::ProposalUpserted {
            proposal: hive_core::ActionProposal::file_diff("p", "m", "diff".into(), vec!["a".into()]),
        },
        SessionEvent::SessionTitleChanged { title: "t".into() },
    ]
}

/// Sign `payload` (arbitrary JSON) the way a newer encoder would: over the
/// bytes it puts on the wire. Returns the wire text.
fn newer_encoder(kp: &SigningKeypair, sid: Uuid, payload: &Value, top_extra: &Value, lamport: u64) -> String {
    let base = SessionEventEnvelope::new(sid, Uuid::nil(), lamport as i64, SessionEvent::SessionTitleChanged { title: String::new() });
    let mut v = serde_json::to_value(&base).unwrap();
    v["payload"] = payload.clone();
    v["lamport"] = json!(lamport);
    v["zzTopLevel"] = top_extra.clone();
    let mut env: SessionEventEnvelope = serde_json::from_str(&v.to_string()).unwrap();
    sign_envelope(&mut env, Uuid::from_u128(5), kp);
    serde_json::to_string(&env).unwrap()
}

proptest! {
    /// A newer build adds fields at any depth and new event kinds. This build
    /// verifies them, keeps them byte for byte, and folds an unknown kind as a
    /// no-op and a known kind exactly as without the extras.
    #[test]
    fn prop_verify_roundtrip_with_unknown_fields(
        extra in arb_json(),
        picks in prop::collection::vec(any::<bool>(), 1..32),
        which in 0usize..4,
    ) {
        let kp = SigningKeypair::from_seed(&[7u8; 32]).unwrap();
        let session = ChatSession::new("p", Uuid::nil(), "anthropic");
        let sid = session.id;
        let seed = SessionEventEnvelope::new(sid, Uuid::nil(), 0, SessionEvent::SessionSnapshot { session: Box::new(session) });
        let events = sample_events();
        let (mut payload, plain) = if which < events.len() {
            let p = serde_json::to_value(&events[which]).unwrap();
            (p.clone(), Some(p))
        } else {
            (json!({"kind": "zzFutureKind", "body": extra.clone()}), None)
        };
        let mut pick = picks.iter().copied().cycle().take(64);
        inject(&mut payload, &extra, &mut pick, 0);
        let wire = newer_encoder(&kp, sid, &payload, &extra, 1);

        let env: SessionEventEnvelope = serde_json::from_str(&wire).unwrap();
        prop_assert!(verify_envelope(&env, &kp.public_key_bytes()).is_ok());
        prop_assert_eq!(serde_json::to_string(&env).unwrap(), wire.clone());
        // The kept bytes still verify after another hop.
        let hop: SessionEventEnvelope = serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        prop_assert!(verify_envelope(&hop, &kp.public_key_bytes()).is_ok());

        let folded = hive_core::project(&[seed.clone(), env.clone()]).unwrap();
        match plain {
            None => {
                prop_assert_eq!(&env.payload, &SessionEvent::Unknown);
                let alone = hive_core::project(std::slice::from_ref(&seed)).unwrap();
                prop_assert_eq!(folded.messages, alone.messages);
                prop_assert_eq!(folded.title, alone.title);
                prop_assert_eq!(folded.proposals, alone.proposals);
            }
            Some(p) => {
                let wire_plain = newer_encoder(&kp, sid, &p, &Value::Null, 1);
                let env_plain: SessionEventEnvelope = serde_json::from_str(&wire_plain).unwrap();
                let expected = hive_core::project(&[seed, env_plain]).unwrap();
                prop_assert_eq!(folded.messages, expected.messages);
                prop_assert_eq!(folded.title, expected.title);
                prop_assert_eq!(folded.proposals, expected.proposals);
            }
        }
    }

    /// Tampering with any received byte of a signed field breaks the signature.
    #[test]
    fn prop_received_bytes_are_what_is_signed(extra in arb_json(), flip in any::<prop::sample::Index>()) {
        let kp = SigningKeypair::from_seed(&[8u8; 32]).unwrap();
        let payload = json!({"kind": "sessionTitleChanged", "title": "x", "zz": extra});
        let wire = newer_encoder(&kp, Uuid::nil(), &payload, &Value::Null, 3);
        let env: SessionEventEnvelope = serde_json::from_str(&wire).unwrap();
        let p = env.raw().unwrap().payload().to_string();
        // Change one character inside the payload to another valid JSON text.
        let mut changed = p.clone();
        let i = flip.index(p.len());
        if !p.is_char_boundary(i) { return Ok(()); }
        if let Some(c) = p[i..].chars().next().filter(|c| c.is_ascii_alphanumeric()) {
            let replacement = if c == 'a' { "b" } else { "a" };
            changed.replace_range(i..i + 1, replacement);
            let tampered = wire.replacen(&p, &changed, 1);
            if let Ok(t) = serde_json::from_str::<SessionEventEnvelope>(&tampered) {
                prop_assert!(verify_envelope(&t, &kp.public_key_bytes()).is_err());
            }
        }
    }
}

/// Bytes that went through a `serde_json::Value` (keys reordered) still verify
/// when they mean exactly the signed value; with an added field they do not.
#[test]
fn value_reencoded_envelopes_verify_only_without_additions() {
    let kp = SigningKeypair::from_seed(&[9u8; 32]).unwrap();
    let mut env = SessionEventEnvelope::new(Uuid::nil(), Uuid::nil(), 1, sample_events().remove(0));
    sign_envelope(&mut env, Uuid::from_u128(1), &kp);
    let v = serde_json::to_value(&env).unwrap();
    let back: SessionEventEnvelope = serde_json::from_value(v.clone()).unwrap();
    assert!(verify_envelope(&back, &kp.public_key_bytes()).is_ok());
    let mut smuggled = v;
    smuggled["payload"]["message"]["zzInjected"] = json!("unsigned");
    let back: SessionEventEnvelope = serde_json::from_value(smuggled).unwrap();
    assert!(verify_envelope(&back, &kp.public_key_bytes()).is_err());
}

proptest! {
    /// The hand-written envelope serde never panics on mutated input, and
    /// whatever it accepts it re-emits exactly (a stand-in for the
    /// `envelope_roundtrip` fuzz target, which needs a nightly toolchain).
    #[test]
    fn prop_envelope_parser_is_total_and_round_trips(
        which in 0usize..64,
        edits in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..6),
    ) {
        let vectors = golden();
        let (_, _, wire) = &vectors[which % vectors.len()];
        let mut bytes = wire.clone().into_bytes();
        for (at, b) in edits {
            let i = at.index(bytes.len());
            bytes[i] = b" {}[],:\"0123456789abcdefnulltrue"[b as usize % 32];
        }
        let Ok(text) = String::from_utf8(bytes) else { return Ok(()) };
        if let Ok(env) = serde_json::from_str::<SessionEventEnvelope>(&text) {
            let out = serde_json::to_string(&env).unwrap();
            prop_assert_eq!(out.as_str(), text.trim(), "accepted text is re-emitted exactly");
            let back: SessionEventEnvelope = serde_json::from_str(&out).unwrap();
            prop_assert_eq!(serde_json::to_string(&back).unwrap(), out);
        }
    }
}

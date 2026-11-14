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

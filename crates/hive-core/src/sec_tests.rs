//! Adversarial regression tests from the 2.0 core security review. Each test
//! reproduces one finding; see the commit that introduced it.

use uuid::Uuid;

use crate::crypto::{sign_envelope, verify_envelope, SigningKeypair};
use crate::events::{SessionEvent, SessionEventEnvelope};
use crate::flow::{FlowRecord, ProgressMark};

/// Tiny deterministic generator for property-style loops (no extra deps).
pub(crate) struct Rng(u64);
impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    pub(crate) fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

/// A signed event survives the wire: serialize the envelope, parse it back
/// (as a receiving device does) and its signature still verifies. Floats in
/// flow records (cost, progress units, scores) must round-trip exactly, or
/// every peer quarantines the author's event as a bad signature.
#[test]
fn signed_flow_records_with_floats_verify_after_a_json_round_trip() {
    let kp = SigningKeypair::from_seed(&[5u8; 32]).unwrap();
    let device = Uuid::from_u128(7);
    let mut rng = Rng::new(0x5eed);
    let mut failures = 0;
    for i in 0..2_000u64 {
        // Mix money-like values and arbitrary finite bit patterns.
        let units = if i % 2 == 0 {
            (rng.next() % 1_000_000) as f64 * 0.000137
        } else {
            let f = f64::from_bits(rng.next());
            if f.is_finite() { f } else { 0.5 }
        };
        let rec = FlowRecord::Progress(ProgressMark {
            message_id: Uuid::from_u128(i as u128),
            agent: "primary".into(),
            units,
            kind: "tests_passing".into(),
            at_ms: 0,
        });
        let mut env = SessionEventEnvelope::new(Uuid::nil(), Uuid::nil(), 1, SessionEvent::FlowRecorded { record: rec });
        sign_envelope(&mut env, device, &kp);
        let wire = serde_json::to_string(&env).unwrap();
        let back: SessionEventEnvelope = serde_json::from_str(&wire).unwrap();
        if verify_envelope(&back, &kp.public_key_bytes()).is_err() {
            failures += 1;
        }
    }
    assert_eq!(failures, 0, "signed events with floats failed to verify after a JSON round trip");
}

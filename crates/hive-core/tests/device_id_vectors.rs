//! Golden vectors for device ids (`hive_core::crypto::device_id_for`): every
//! device id is a UUIDv8 of `SHA-256("ligus-device-id-v1\0" || alg || "\0" ||
//! public_key)` (first 16 bytes, version 8, RFC 9562 variant), with `alg` the
//! key's wire name (`ed25519` or `p256`). A phone (the iOS app) derives its id
//! from its Secure Enclave key this way.
//!
//! `golden/device_id_v1.jsonl`: one line per case, `label`, `alg`,
//! `publicKey` (hex: 32 bytes for Ed25519, the 65-byte SEC1 uncompressed
//! point for P-256) and `deviceId`. The values were computed independently
//! (Python `hashlib` and `uuid`). Existing lines are frozen.

use hive_core::crypto::{device_id_for, DeviceCertificate, SigAlg, SigningKeypair, DEVICE_ID_CONTEXT};
use hive_core::Timestamp;
use uuid::Uuid;

const GOLDEN: &str = include_str!("golden/device_id_v1.jsonl");

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

#[test]
fn device_id_vectors_derive_as_frozen() {
    let mut n = 0;
    for line in GOLDEN.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let label = v["label"].as_str().unwrap();
        let alg = match v["alg"].as_str().unwrap() {
            "ed25519" => SigAlg::Ed25519,
            "p256" => SigAlg::P256,
            other => panic!("{label}: alg {other}"),
        };
        let key = unhex(v["publicKey"].as_str().unwrap());
        let want = Uuid::parse_str(v["deviceId"].as_str().unwrap()).unwrap();
        let got = device_id_for(alg, &key);
        assert_eq!(got, want, "{label}");
        assert_eq!(got.get_version_num(), 8, "{label}");
        assert_eq!(got.get_variant(), uuid::Variant::RFC4122, "{label}");
        n += 1;
    }
    assert!(n >= 4, "{n} vectors");
}

#[test]
fn the_device_id_context_is_frozen() {
    // b"ligus-device-id-v1\0"
    let hex: String = DEVICE_ID_CONTEXT.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex, "6c696775732d6465766963652d69642d763100");
}

/// The algorithm is part of the derivation: the same bytes under the other
/// algorithm name another device.
#[test]
fn the_algorithm_is_bound() {
    let key: Vec<u8> = (0u8..32).collect();
    assert_ne!(device_id_for(SigAlg::Ed25519, &key), device_id_for(SigAlg::P256, &key));
}

/// A certificate names the id its key derives; one under any other id never
/// verifies, whoever signed it and wherever it sorts.
#[test]
fn a_certificate_under_another_id_never_verifies() {
    let account = SigningKeypair::generate().unwrap();
    let device = SigningKeypair::generate().unwrap();
    let a = Uuid::new_v4();
    let good = DeviceCertificate::issue(&account, a, &device.public_key_bytes(), Timestamp::epoch());
    assert_eq!(good.device_id, device_id_for(SigAlg::Ed25519, &device.public_key_bytes()));
    good.verify(&account.public_key_bytes()).unwrap();
    let squat = DeviceCertificate::issue_for_id(&account, a, Uuid::new_v4(), &device.public_key_bytes(), Timestamp::epoch());
    assert!(matches!(squat.verify(&account.public_key_bytes()), Err(hive_core::crypto::CryptoError::DeviceIdMismatch)));
    // Someone else's key under a victim's (derived) id.
    let victim = SigningKeypair::generate().unwrap();
    let victim_id = device_id_for(SigAlg::Ed25519, &victim.public_key_bytes());
    let squat = DeviceCertificate::issue_for_id(&account, a, victim_id, &device.public_key_bytes(), Timestamp::epoch());
    assert!(squat.verify(&account.public_key_bytes()).is_err());
}

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

/// Alg confusion and key encodings: a certificate verifies only for the one
/// encoding of the key its algorithm names (32 bytes for Ed25519, the
/// 65-byte uncompressed point for P-256) under the id that encoding and
/// algorithm derive. The same bytes under the other algorithm, a P-256 key's
/// compressed form, or an Ed25519 tag over a P-256 key never verify.
#[test]
fn only_one_encoding_and_algorithm_names_a_device() {
    let account = SigningKeypair::generate().unwrap();
    let a = Uuid::new_v4();
    let ok = |c: &DeviceCertificate| c.verify(&account.public_key_bytes()).is_ok();
    let ed = SigningKeypair::generate().unwrap().public_key_bytes().to_vec();
    let phone = hive_core::testkit::Phone::new();
    let p256 = phone.public_key();
    assert_eq!(p256.len(), 65);
    assert!(ok(&DeviceCertificate::issue(&account, a, &ed, Timestamp::epoch())));
    assert!(ok(&DeviceCertificate::issue_p256(&account, a, &p256, Timestamp::epoch())));
    // Ed25519 bytes certified as P-256, and a P-256 key certified as Ed25519.
    assert!(!ok(&DeviceCertificate::issue_p256(&account, a, &ed, Timestamp::epoch())));
    assert!(!ok(&DeviceCertificate::issue(&account, a, &p256, Timestamp::epoch())));
    // The compressed form of the phone's key: another id, and refused.
    let mut compressed = vec![if p256[64] & 1 == 0 { 2u8 } else { 3u8 }];
    compressed.extend_from_slice(&p256[1..33]);
    assert_ne!(device_id_for(SigAlg::P256, &compressed), phone.device_id);
    assert!(!ok(&DeviceCertificate::issue_p256(&account, a, &compressed, Timestamp::epoch())));
    // The uncompressed key under the Ed25519 derivation (alg confusion on
    // the id alone): refused.
    let confused = DeviceCertificate::issue_p256_for_id(&account, a, device_id_for(SigAlg::Ed25519, &p256), &p256, Timestamp::epoch());
    assert!(!ok(&confused));
}

/// A device id is compared as 16 bytes, never as text: every spelling of a
/// derived id parses to it (upper case, braces, no hyphens), and the bytes
/// of another version or variant are another id, refused.
#[test]
fn a_device_id_is_its_bytes_whatever_its_spelling() {
    let device = SigningKeypair::generate().unwrap();
    let id = device_id_for(SigAlg::Ed25519, &device.public_key_bytes());
    let text = id.to_string();
    assert_eq!(text, text.to_lowercase(), "shown lower case");
    for spelled in [text.to_uppercase(), format!("{{{text}}}"), id.simple().to_string(), format!("urn:uuid:{text}")] {
        assert_eq!(Uuid::parse_str(&spelled).unwrap(), id, "{spelled}");
    }
    let account = SigningKeypair::generate().unwrap();
    for (version, variant) in [(0x40u8, 0x80u8), (0x50, 0x80), (0x80, 0xc0), (0x80, 0x00)] {
        let mut b = *id.as_bytes();
        b[6] = (b[6] & 0x0f) | version;
        b[8] = (b[8] & 0x3f) | variant;
        let other = Uuid::from_bytes(b);
        if other == id {
            continue;
        }
        let c = DeviceCertificate::issue_for_id(&account, Uuid::new_v4(), other, &device.public_key_bytes(), Timestamp::epoch());
        assert!(c.verify(&account.public_key_bytes()).is_err(), "version {version:#x} variant {variant:#x}");
    }
}

//! Known-answer tests pinning the bytes Ligus's crypto primitives produce, so
//! a dependency upgrade (ed25519-dalek, x25519-dalek, sha2, hkdf, hmac,
//! chacha20poly1305, argon2, bip39) can never silently change what is signed,
//! sealed, derived or stored. Expected values come from the published
//! standard where one exists (RFC 8032, RFC 7748, FIPS 180-2, BIP-39) and
//! otherwise were produced by the pre-upgrade crates (ed25519-dalek 2,
//! x25519-dalek 2, sha2 0.10, hkdf 0.12, chacha20poly1305 0.10, argon2 0.5,
//! bip39 2). The relay-tunnel vectors (Noise, push AEAD, HMAC) live in
//! hive-cli's tunnel tests.

use hive_core::crypto::{verify, SigningKeypair};
use hive_core::e2ee::{self, KeyAgreementKeypair, SealedBlob, SealedEnvelope};
use hive_core::flow::keys::{file_key, repo_key};
use hive_core::presence::RecoveryKey;
use sha2::{Digest, Sha256, Sha512};

fn hx(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

/// RFC 8032 §7.1 test 1 (empty message), plus a pinned Ligus message.
#[test]
fn ed25519_signatures_are_pinned() {
    let kp = SigningKeypair::from_seed(&unhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")).unwrap();
    assert_eq!(hx(&kp.public_key_bytes()), "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
    let empty = kp.sign(b"");
    assert_eq!(
        hx(&empty),
        "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
    );
    let sig = kp.sign(b"ligus kat: ed25519");
    assert_eq!(
        hx(&sig),
        "c0811a9fb206cd12ee3ee47e2d0c0555bc53cb078f910af25cce6cdf6a33862cd2d228f8185ae77d21959fe1de988bd6edbcec1f73a79cb6302d1560c1ae320c"
    );
    verify(&kp.public_key_bytes(), b"", &empty).unwrap();
    verify(&kp.public_key_bytes(), b"ligus kat: ed25519", &sig).unwrap();
    assert!(verify(&kp.public_key_bytes(), b"ligus kat: ed25519!", &sig).is_err());
    let mut bad = sig;
    bad[63] ^= 0x10;
    assert!(verify(&kp.public_key_bytes(), b"ligus kat: ed25519", &bad).is_err());
}

/// RFC 7748 §6.1 (Alice's key pair).
#[test]
fn x25519_public_key_is_pinned() {
    let a = KeyAgreementKeypair::from_seed(&unhex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a")).unwrap();
    assert_eq!(hx(&a.public_key_bytes()), "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a");
    assert_eq!(hx(&a.seed_bytes()), "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
}

/// A workspace key sealed by the pre-upgrade crates (X25519 + HKDF-SHA256 +
/// ChaCha20-Poly1305) still opens, and tampering is still refused.
#[test]
fn sealed_blob_from_before_the_upgrade_opens() {
    let a = KeyAgreementKeypair::from_seed(&unhex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a")).unwrap();
    let blob = SealedBlob {
        ephemeral_public: unhex("425cad310abcf98fcd72bfda58f799c6570b9ffe7998f469f7a42f2ad46c0a30"),
        ciphertext: unhex("768d7ff499b5c5099aa43ea4f932aeaf9d829ef984a8907d979b90d27a3f987f95fa6475b55f86c3684de9eaefbe26"),
    };
    assert_eq!(e2ee::open(&a, &blob).unwrap(), b"ligus kat: sealed workspace key");
    let mut bad = blob.clone();
    bad.ciphertext[0] ^= 1;
    assert!(e2ee::open(&a, &bad).is_err());
    // And a fresh seal round-trips.
    let fresh = e2ee::seal(&a.public_key_bytes(), b"round trip").unwrap();
    assert_eq!(e2ee::open(&a, &fresh).unwrap(), b"round trip");
}

/// A symmetric envelope sealed by the pre-upgrade chacha20poly1305 still opens.
#[test]
fn sealed_envelope_from_before_the_upgrade_opens() {
    let env = SealedEnvelope {
        version: 3,
        nonce: unhex("925645b9a7754129c9f748a9"),
        ciphertext: unhex("2a8c5990423468e93acb19353c28e877fa7d300e3f88f4be3d5698e00e9f982c03e5f9de"),
        kid: None,
    };
    assert_eq!(e2ee::open_symmetric(&[0x42; 32], &env).unwrap(), b"ligus kat: symmetric");
    assert!(e2ee::open_symmetric(&[0x43; 32], &env).is_err());
    let fresh = e2ee::seal_symmetric(&[0x42; 32], 3, b"round trip").unwrap();
    assert_eq!(e2ee::open_symmetric(&[0x42; 32], &fresh).unwrap(), b"round trip");
}

/// Argon2id passphrase workspace key (19 MiB, t=2, p=1, fixed salt).
#[test]
fn argon2id_passphrase_key_is_pinned() {
    assert_eq!(
        hx(&e2ee::derive_workspace_key("correct horse battery staple")),
        "1bc87a0ef9bdaf108c5cc869fa766c14d97468c7e07b6c62ec241e84237d7895"
    );
}

/// HKDF-Extract (HMAC-SHA256) flow keys.
#[test]
fn flow_keys_are_pinned() {
    assert_eq!(file_key(&[7u8; 32], "./src/main.rs"), "f_cb6ba83e97da7ad7a8c9187da904c25c");
    assert_eq!(repo_key(&[7u8; 32], "ligus"), "r_2142c57baee7b8bc38046513d35b5e24");
}

/// FIPS 180-2 "abc".
#[test]
fn sha2_is_pinned() {
    assert_eq!(hx(&Sha256::digest(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert_eq!(
        hx(&Sha512::digest(b"abc")),
        "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
    );
}

/// BIP-39 all-zero 256-bit entropy, and the recovery key it seeds (the
/// Ed25519 public key of the all-zero seed).
#[test]
fn recovery_words_are_pinned() {
    let words = ["abandon"; 23].join(" ") + " art";
    let rk = RecoveryKey::from_words(&words).unwrap();
    assert_eq!(rk.words(), words);
    assert_eq!(hx(&rk.public_key()), "3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29");
    // Case and spacing are normalised; a bad checksum is refused.
    assert!(RecoveryKey::from_words(&format!("  {}  ", words.to_uppercase())).is_ok());
    assert!(RecoveryKey::from_words(&(["abandon"; 24].join(" "))).is_err());
}

//! Known-answer and compatibility tests for the RustCrypto digest generation
//! (sha2 0.11, hkdf 0.13). The standard vectors are RFC 5869 (HKDF-SHA256)
//! and FIPS 180-4 (SHA-256, SHA-512, including the multi-block and
//! million-character messages). The Ligus vectors derive the bytes the
//! protocol stores, signs over or sends (device ids, the workspace-key
//! commitment, account-key fingerprints, the proposal digest, the sealing
//! HKDF) on fixed inputs; every expected value was produced by the previous
//! generation (sha2 0.10.9, hkdf 0.12.4, digest 0.10.7) over the same
//! preimages, so a dependency change can never silently move them.

use hkdf::Hkdf;
use ligus_core::account_keys::key_fingerprint;
use ligus_core::authz::votes::diff_digest;
use ligus_core::crypto::{device_id_for, SigAlg};
use ligus_core::keys::{key_commitment, key_id};
use ligus_core::proposals::ActionProposal;
use sha2::{Digest, Sha256, Sha512};
use uuid::Uuid;

fn hx(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn id(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap()
}

/// RFC 5869 Appendix A, test cases 1 to 3 (HKDF-SHA256): PRK and OKM.
#[test]
fn hkdf_sha256_rfc5869_vectors() {
    let cases = [
        (
            "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b",
            "000102030405060708090a0b0c",
            "f0f1f2f3f4f5f6f7f8f9",
            "077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5",
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865",
        ),
        (
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f404142434445464748494a4b4c4d4e4f",
            "606162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9fa0a1a2a3a4a5a6a7a8a9aaabacadaeaf",
            "b0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedfe0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff",
            "06a6b88c5853361a06104c9ceb35b45cef760014904671014a193f40c15fc244",
            "b11e398dc80327a1c8e7f78c596a49344f012eda2d4efad8a050cc4c19afa97c59045a99cac7827271cb41c65e590e09da3275600c2f09b8367793a9aca3db71cc30c58179ec3e87c14c01d5c1f3434f1d87",
        ),
        (
            "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b",
            "",
            "",
            "19ef24a32c717b167f33a91d6f648bdf96596776afdb6377ac434c1c293ccb04",
            "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8",
        ),
    ];
    for (ikm, salt, info, prk_want, okm_want) in cases {
        let salt = unhex(salt);
        let (prk, hk) = Hkdf::<Sha256>::extract(
            if salt.is_empty() { None } else { Some(&salt) },
            &unhex(ikm),
        );
        assert_eq!(hx(&prk), prk_want);
        let mut okm = vec![0u8; okm_want.len() / 2];
        hk.expand(&unhex(info), &mut okm).unwrap();
        assert_eq!(hx(&okm), okm_want);
    }
}

/// FIPS 180-4 example messages (empty, one block, two blocks, one million 'a').
#[test]
fn sha2_fips_180_4_vectors() {
    let m448: &[u8] = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
    let m896: &[u8] = b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu";
    let million = vec![b'a'; 1_000_000];
    let cases: [(&[u8], &str, &str); 5] = [
        (
            b"",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e",
        ),
        (
            b"abc",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
        ),
        (
            m448,
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            "204a8fc6dda82f0a0ced7beb8e08a41657c16ef468b228a8279be331a703c33596fd15c13b1b07f9aa1d3bea57789ca031ad85c7a71dd70354ec631238ca3445",
        ),
        (
            m896,
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1",
            "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909",
        ),
        (
            &million,
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
            "e718483d0ce769644e2e42c7bc15b4638e1f98b13b2044285632a803afa973ebde0ff244877ea60a4cb0432ce577c31beb009c5c2c49aa2e4eadb217ad8cc09b",
        ),
    ];
    for (msg, s256, s512) in cases {
        assert_eq!(hx(&Sha256::digest(msg)), s256);
        assert_eq!(hx(&Sha512::digest(msg)), s512);
        // Incremental hashing in uneven chunks gives the same digest.
        let mut h = Sha256::new();
        for c in msg.chunks(97) {
            h.update(c);
        }
        assert_eq!(hx(&h.finalize()), s256);
    }
}

/// Device ids (Ed25519 and P-256) derive exactly as before.
#[test]
fn device_ids_are_unchanged() {
    assert_eq!(
        device_id_for(SigAlg::Ed25519, &[0x11; 32]),
        id("100eeb14-b187-811f-ab90-19aaf699bf3d")
    );
    let mut p256 = vec![0x04u8];
    p256.extend([0x22u8; 64]);
    assert_eq!(
        device_id_for(SigAlg::P256, &p256),
        id("8e2e5b4b-3fd2-8f40-8d12-5ef0dce60f7d")
    );
}

/// The workspace-key commitment and the key id cut from it.
#[test]
fn workspace_key_commitment_is_unchanged() {
    let ws = id("6f1c2a4e-8b3d-4c5e-9f70-112233445566");
    let want = "692aea98e218f68826d81a624355f5e3a75092b229b09d7c32a636da9a9e50e1";
    assert_eq!(key_commitment(ws, 3, &[0x42; 32]), want);
    assert_eq!(key_id(ws, 3, &[0x42; 32]), &want[..16]);
}

/// The account-key fingerprint (`fk`).
#[test]
fn account_key_fingerprint_is_unchanged() {
    assert_eq!(
        hx(&key_fingerprint(&[0x11; 32])),
        "02d449a31fbb267c8f352e9968a79e3e"
    );
}

/// The proposal digest a vote binds to, with every optional part set and
/// with none set.
#[test]
fn proposal_digest_is_unchanged() {
    let full: ActionProposal = serde_json::from_value(serde_json::json!({
        "id": "0a0b0c0d-0e0f-4011-8213-141516171819",
        "title": "t",
        "kind": "fileDiff",
        "status": "open",
        "diff": "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-old\n+new\n",
        "changedFiles": ["src/main.rs", "README.md"],
        "repoId": "a1a2a3a4-b1b2-4c1c-8d1d-e1e2e3e4e5e6",
        "baseCommit": "0123456789abcdef0123456789abcdef01234567",
    }))
    .unwrap();
    assert_eq!(
        diff_digest(&full),
        "5ffcebefdc1eef76c06762fe31f057b3e658097b18c14bef655a4bb2f3ac81db"
    );

    let bare: ActionProposal = serde_json::from_value(serde_json::json!({
        "id": "0a0b0c0d-0e0f-4011-8213-141516171819",
        "title": "t",
        "kind": "decision",
        "status": "open",
    }))
    .unwrap();
    assert_eq!(
        diff_digest(&bare),
        "f6a796bba2f2d7dbd596658ba751da3247ca241fb7643cd3209175b6589546e9"
    );
}

/// HKDF-SHA256 with a salt and the workspace-key sealing info string, as
/// the X25519 sealing derives its AEAD key.
#[test]
fn sealing_hkdf_is_unchanged() {
    let hk = Hkdf::<Sha256>::new(Some(&[0x33; 64]), &[0x44; 32]);
    let mut okm = [0u8; 32];
    hk.expand(b"hive-workspace-key-seal-v1", &mut okm).unwrap();
    assert_eq!(
        hx(&okm),
        "387f38c116aae59264fbda6ca2e19d59349cd9797f9856bb677f4d61c7f8f7a2"
    );
}

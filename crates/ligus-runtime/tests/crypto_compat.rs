//! Compatibility tests for the RustCrypto digest generation (sha2 0.11,
//! hkdf 0.13, hmac 0.13) over the runtime's derivations: RFC 4231 HMAC
//! vectors, then the relay write key, the phone-enrollment rendezvous id,
//! the push idempotency key, the worktree signal key, the gateway member
//! pseudonym and the MCP turn-credential MAC, each on fixed inputs. Every
//! expected Ligus value was produced by the previous generation (sha2
//! 0.10.9, hkdf 0.12.4, hmac 0.12.1) over the same preimages.

use hmac::{KeyInit, Mac};
use ligus_runtime::contacts::relay_write_key;
use ligus_runtime::hygiene::opaque_key;
use ligus_runtime::ligus_mcp::cred::{hmac_sha256, TURN_CRED_TAG};
use ligus_runtime::model_gateway::member_pseudonym;
use ligus_runtime::phone_enroll_qr::EnrollSecret;
use ligus_runtime::relay_client::idempotency_key;
use uuid::Uuid;

fn hx(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn id(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap()
}

/// RFC 4231 test cases 1 and 2 (HMAC-SHA256), through the hmac crate and
/// through the hand-rolled HMAC the MCP credential uses.
#[test]
fn hmac_sha256_rfc4231_vectors() {
    let mut m = hmac::Hmac::<sha2::Sha256>::new_from_slice(&[0x0b; 20]).unwrap();
    m.update(b"Hi There");
    assert_eq!(
        hx(&m.finalize().into_bytes()),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
    let mut m = hmac::Hmac::<sha2::Sha256>::new_from_slice(b"Jefe").unwrap();
    m.update(b"what do ya want for nothing?");
    assert_eq!(
        hx(&m.finalize().into_bytes()),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );

    // The hand-rolled HMAC (32-byte key, zero-padded to the block) agrees
    // with the crate on the same key and message.
    let mut k = [0u8; 32];
    k[..20].copy_from_slice(&[0x0b; 20]);
    assert_eq!(
        hx(&hmac_sha256(&k, &[b"Hi ", b"There"])),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
}

#[test]
fn relay_write_key_is_unchanged() {
    // The relay base is normalised (trimmed, no trailing slash, lower case).
    assert_eq!(
        relay_write_key("master-write-key", " https://Relay.Example.com/ "),
        "AJbN6CTKZU2ZtoMiRJBaSpumCv22MGUKijNf1OygiHo"
    );
}

#[test]
fn phone_enroll_rid_is_unchanged() {
    assert_eq!(
        hx(&EnrollSecret::from_bytes([0x07; 32]).keys().rid.0),
        "35555adb6e003fdd12edb0c7186a0f15"
    );
}

#[test]
fn push_idempotency_key_is_unchanged() {
    assert_eq!(
        idempotency_key(&[9; 32], id("c0c1c2c3-d0d1-4e1e-8f1f-a0a1a2a3a4a5")),
        "cace6abbd8ccfb2a4b0c6748aa677d31"
    );
}

#[test]
fn worktree_signal_key_is_unchanged() {
    assert_eq!(
        opaque_key(b"device-secret", "chat-slug"),
        "171205cdc2b80596a2702a9d"
    );
}

#[test]
fn gateway_member_pseudonym_is_unchanged() {
    assert_eq!(
        member_pseudonym(
            b"attribution-key",
            id("6f1c2a4e-8b3d-4c5e-9f70-112233445566"),
            "member-1"
        ),
        "551ff05749725b45cf878f5ac8169b16"
    );
}

#[test]
fn mcp_turn_credential_mac_is_unchanged() {
    let (t, c, a) = (
        id("11111111-1111-4111-8111-111111111111"),
        id("22222222-2222-4222-8222-222222222222"),
        id("33333333-3333-4333-8333-333333333333"),
    );
    let mac = hmac_sha256(
        &[0x5a; 32],
        &[
            TURN_CRED_TAG,
            t.as_bytes(),
            c.as_bytes(),
            a.as_bytes(),
            &1_900_000_000u64.to_le_bytes(),
        ],
    );
    assert_eq!(
        hx(&mac),
        "2896b6c570a62826a2fac67143cf0bff77581d441607cd95339aa13a6d24aa26"
    );
}

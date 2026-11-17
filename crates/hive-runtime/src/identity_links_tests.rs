//! Identity-link verification: golden SSHSIG vectors, and every forge against
//! a fake HTTP server on loopback (no real network).
//!
//! `testdata/identity_links/` holds signatures made with OpenSSH 9.9's
//! `ssh-keygen -Y sign` over `statement.txt` (throwaway keys; the private
//! halves were deleted), and, prefixed `openssh_`, the security-key vectors
//! from OpenSSH's own regression suite
//! (`regress/unittests/sshsig/testdata`, OpenSSH licence), made with real
//! FIDO authenticators under the namespace `unittest`.
//!
//! The RSA vectors (`rsa*.sig`) are `ssh-keygen -Y sign` output too, except
//! `rsa_sha2_256.sig`, `rsa_sha1.sig` and `rsa_leading_zero.sig`, which were
//! assembled by hand per PROTOCOL.sshsig and signed with `openssl dgst` using
//! a throwaway `ssh-keygen` key (deleted): a genuine rsa-sha2-256 and ssh-rsa
//! signature over the sha512 SSHSIG blob, and an rsa-sha2-512 signature over
//! `rsa_leading_zero.txt` whose first byte is zero. All verify with
//! `ssh-keygen -Y check-novalidate` except ssh-rsa, which OpenSSH refuses.
//! `rsa_bb06_e3.sig` is a synthetic Bleichenbacher-2006 forgery attempt
//! (e = 3, modulus 2^3072 - 1) that OpenSSH also refuses.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;
use hive_core::identity_link::{render_statement, IdentityLink, LinkMethod};
use ssh_key::private::Ed25519Keypair;
use ssh_key::{HashAlg, LineEnding, PrivateKey};

use super::*;

const W: &str = "11111111-1111-4111-8111-111111111111";
const ACCOUNT: &str = "6f1c2d3e-0000-4000-8000-000000000001";
const ACCOUNT_KEY: [u8; 32] = [7u8; 32];

fn vector(name: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/identity_links").join(name);
    std::fs::read_to_string(p).unwrap()
}

fn ts(s: &str) -> Timestamp {
    serde_json::from_value(serde_json::json!(s)).unwrap()
}

fn ws() -> Uuid {
    Uuid::parse_str(W).unwrap()
}

fn now() -> Timestamp {
    ts("2026-12-01T00:00:00Z")
}

fn ctx() -> LinkContext<'static> {
    LinkContext { workspace_id: ws(), account_key: Some(&ACCOUNT_KEY) }
}

/// The link `statement.txt` is the rendering of.
fn golden_link() -> IdentityLink {
    IdentityLink {
        link_id: Uuid::from_u128(42),
        account_id: Uuid::parse_str(ACCOUNT).unwrap(),
        provider: "github".into(),
        host: "github.com".into(),
        subject_id: "1234567".into(),
        handle_at_link: "alice".into(),
        method: LinkMethod::SshSig,
        statement: String::new(),
        proof: String::new(),
        key_fingerprint: String::new(),
        nonce: "AAECAwQFBgcICQoLDA0ODw".into(),
        issued_at: ts("2026-11-14T12:00:00Z"),
        expires_at: ts("2027-11-14T12:00:00Z"),
    }
}

fn key(seed: u8) -> PrivateKey {
    PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
}

fn openssh_pub(k: &PrivateKey) -> String {
    k.public_key().to_openssh().unwrap()
}

/// A link for `provider/host/subject`, SSH-signed by `k` in this test.
fn signed(provider: &str, host: &str, subject: &str, handle: &str, k: &PrivateKey) -> IdentityLink {
    let mut l = golden_link();
    l.provider = provider.into();
    l.host = host.into();
    l.subject_id = subject.into();
    l.handle_at_link = handle.into();
    l.statement = render_statement(&l, ws(), &ACCOUNT_KEY);
    l.proof = k.sign(LINK_NAMESPACE, HashAlg::Sha512, l.statement.as_bytes()).unwrap().to_pem(LineEnding::LF).unwrap();
    l.key_fingerprint = k.public_key().fingerprint(HashAlg::Sha256).to_string();
    l
}

// ---------------------------------------------------------------------------
// Golden SSHSIG vectors

#[test]
fn golden_statement_matches_the_renderer() {
    assert_eq!(render_statement(&golden_link(), ws(), &ACCOUNT_KEY), vector("statement.txt"));
}

#[test]
fn golden_vectors_from_ssh_keygen_verify() {
    let msg = vector("statement.txt");
    for k in ["ed25519", "ecdsa", "rsa"] {
        let ok = verify_sshsig(&vector(&format!("{k}.sig")), LINK_NAMESPACE, msg.as_bytes())
            .unwrap_or_else(|e| panic!("{k}: {e}"));
        let pubkey = parse_openssh(&vector(&format!("{k}.pub"))).unwrap();
        assert_eq!(ok.public_key.key_data(), pubkey.key_data(), "{k}");
        assert_eq!(ok.fingerprint, pubkey.fingerprint(HashAlg::Sha256).to_string());
        // One byte of the statement changed: no longer valid.
        let tampered = msg.replace("subject: 1234567", "subject: 7654321");
        assert!(verify_sshsig(&vector(&format!("{k}.sig")), LINK_NAMESPACE, tampered.as_bytes()).is_err(), "{k}");
    }
    // The ssh-keygen signature over the golden statement makes a full link.
    let mut l = golden_link();
    l.statement = msg;
    l.proof = vector("ed25519.sig");
    l.key_fingerprint = parse_openssh(&vector("ed25519.pub")).unwrap().fingerprint(HashAlg::Sha256).to_string();
    assert!(offline_check(&l, ctx(), now()).unwrap().is_some());
}

/// Hardware keys (pre-check): `sk-ssh-ed25519@openssh.com` and
/// `sk-ecdsa-sha2-nistp256@openssh.com` signatures made by real
/// authenticators verify; the WebAuthn variant is refused.
#[test]
fn golden_security_key_vectors_verify() {
    let ns = vector("openssh_namespace");
    let msg = vector("openssh_signed-data");
    for k in ["ed25519_sk", "ecdsa_sk"] {
        let ok = verify_sshsig(&vector(&format!("openssh_{k}.sig")), ns.trim(), msg.as_bytes())
            .unwrap_or_else(|e| panic!("{k}: {e}"));
        let pubkey = parse_openssh(&vector(&format!("openssh_{k}.pub"))).unwrap();
        assert_eq!(ok.public_key.key_data(), pubkey.key_data());
        assert!(verify_sshsig(&vector(&format!("openssh_{k}.sig")), LINK_NAMESPACE, msg.as_bytes()).is_err());
    }
    assert!(verify_sshsig(&vector("openssh_ecdsa_sk_webauthn.sig"), ns.trim(), msg.as_bytes()).is_err());
}

/// A security-key signature without the user-presence flag is refused.
#[test]
fn security_key_without_user_presence_is_refused() {
    let sig = SshSig::from_pem(vector("openssh_ed25519_sk.sig")).unwrap();
    let mut data = sig.signature().as_bytes().to_vec();
    let flags = data.len() - 5;
    data[flags] &= !0x01;
    let s = ssh_key::Signature::new(sig.signature().algorithm(), data).unwrap();
    let forged = SshSig::new(sig.public_key().clone(), sig.namespace(), sig.hash_alg(), s).unwrap();
    let err = verify_sshsig(&forged.to_pem(LineEnding::LF).unwrap(), "unittest", vector("openssh_signed-data").as_bytes())
        .unwrap_err();
    assert!(err.contains("user presence"), "{err}");
}

/// ssh-key's reader doesn't check that a length-prefixed field is used up,
/// so an inflated public-key or signature length still parsed and verified:
/// non-canonical encodings are refused (CRLF line ends are fine).
#[test]
fn sshsig_non_canonical_encoding_is_refused() {
    use base64::Engine as _;
    let msg = vector("statement.txt");
    let armor = |b: &[u8], width: usize, nl: &str| {
        let b64 = base64::engine::general_purpose::STANDARD.encode(b);
        let lines: Vec<&str> = b64.as_bytes().chunks(width).map(|c| std::str::from_utf8(c).unwrap()).collect();
        format!("-----BEGIN SSH SIGNATURE-----{nl}{}{nl}-----END SSH SIGNATURE-----{nl}", lines.join(nl))
    };
    for f in ["ed25519.sig", "ecdsa.sig", "rsa.sig", "rsa4096.sig"] {
        let body: String = vector(f).lines().filter(|l| !l.starts_with("-----")).collect();
        let blob = base64::engine::general_purpose::STANDARD.decode(body).unwrap();
        assert!(verify_sshsig(&armor(&blob, 70, "\r\n"), LINK_NAMESPACE, msg.as_bytes()).is_ok(), "{f} crlf");
        let mut inflated = blob.clone();
        inflated[11] ^= 1; // public key length + 65536
        assert!(SshSig::from_pem(armor(&inflated, 70, "\n")).is_ok(), "{f}: ssh-key still parses it");
        let err = verify_sshsig(&armor(&inflated, 70, "\n"), LINK_NAMESPACE, msg.as_bytes()).unwrap_err();
        assert!(err.contains("non-canonical"), "{f}: {err}");
    }
}

/// The SSHSIG version isn't covered by the signature; only version 1 (the
/// only one OpenSSH writes) is accepted, for every key type.
#[test]
fn sshsig_version_other_than_1_is_refused() {
    use base64::Engine as _;
    let msg = vector("statement.txt");
    for f in ["ed25519.sig", "ecdsa.sig", "rsa.sig"] {
        let body: String = vector(f).lines().filter(|l| !l.starts_with("-----")).collect();
        let mut blob = base64::engine::general_purpose::STANDARD.decode(body).unwrap();
        assert_eq!(&blob[6..10], &[0, 0, 0, 1]);
        blob[9] = 0;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&blob);
        let lines: Vec<&str> = b64.as_bytes().chunks(70).map(|c| std::str::from_utf8(c).unwrap()).collect();
        let armored = format!("-----BEGIN SSH SIGNATURE-----\n{}\n-----END SSH SIGNATURE-----\n", lines.join("\n"));
        assert_eq!(SshSig::from_pem(&armored).unwrap().version(), 0);
        let err = verify_sshsig(&armored, LINK_NAMESPACE, msg.as_bytes()).unwrap_err();
        assert!(err.contains("version"), "{f}: {err}");
    }
}

#[test]
fn small_rsa_keys_are_refused() {
    let err = verify_sshsig(&vector("rsa2048.sig"), LINK_NAMESPACE, vector("statement.txt").as_bytes()).unwrap_err();
    assert!(err.contains("3072"), "{err}");
}

// ---------------------------------------------------------------------------
// RSA (verified with rsa 0.10, not ssh-key's rsa 0.9 feature)

fn rsa_sig(vector_name: &str) -> SshSig {
    SshSig::from_pem(vector(vector_name)).unwrap()
}

/// Re-armor `sig` with its signature bytes replaced (same algorithm).
fn rsa_with_sig_bytes(sig: &SshSig, alg: ssh_key::Algorithm, data: Vec<u8>) -> String {
    let s = ssh_key::Signature::new(alg, data).unwrap();
    SshSig::new(sig.public_key().clone(), sig.namespace(), sig.hash_alg(), s).unwrap().to_pem(LineEnding::LF).unwrap()
}

/// An RSA key with the given modulus and exponent bytes, and a signature of
/// `sig_len` bytes, armored.
fn rsa_synthetic(n: &[u8], e: &[u8], sig_len: usize) -> String {
    let key = ssh_key::public::KeyData::Rsa(ssh_key::public::RsaPublicKey {
        e: ssh_key::Mpint::from_positive_bytes(e).unwrap(),
        n: ssh_key::Mpint::from_positive_bytes(n).unwrap(),
    });
    let s = ssh_key::Signature::new(ssh_key::Algorithm::Rsa { hash: Some(HashAlg::Sha512) }, vec![1u8; sig_len]).unwrap();
    SshSig::new(key, LINK_NAMESPACE, HashAlg::Sha512, s).unwrap().to_pem(LineEnding::LF).unwrap()
}

const RSA_SHA512: ssh_key::Algorithm = ssh_key::Algorithm::Rsa { hash: Some(HashAlg::Sha512) };

/// `ssh-keygen -Y sign` RSA-3072 (two keys, one the pre-rsa-0.10 fixture),
/// RSA-4096 and RSA-8192 signatures over the statement verify, bind to the
/// key's fingerprint, and fail for a tampered statement.
#[test]
fn rsa_ssh_keygen_vectors_verify() {
    let msg = vector("statement.txt");
    for (k, bits) in [("rsa", 3072), ("rsa3072b", 3072), ("rsa4096", 4096), ("rsa8192", 8192)] {
        let ok = verify_sshsig(&vector(&format!("{k}.sig")), LINK_NAMESPACE, msg.as_bytes())
            .unwrap_or_else(|e| panic!("{k}: {e}"));
        let pubkey = parse_openssh(&vector(&format!("{k}.pub"))).unwrap();
        assert_eq!(ok.public_key.key_data(), pubkey.key_data(), "{k}");
        assert_eq!(ok.fingerprint, pubkey.fingerprint(HashAlg::Sha256).to_string(), "{k}");
        assert_eq!(ok.algorithm, "ssh-rsa", "{k}");
        let n = pubkey.key_data().rsa().unwrap().n.as_positive_bytes().unwrap().len();
        assert_eq!(n * 8, bits, "{k}");
        let tampered = msg.replace("subject: 1234567", "subject: 7654321");
        assert!(verify_sshsig(&vector(&format!("{k}.sig")), LINK_NAMESPACE, tampered.as_bytes()).is_err(), "{k}");
        let mut extra = msg.clone();
        extra.push('\n');
        assert!(verify_sshsig(&vector(&format!("{k}.sig")), LINK_NAMESPACE, extra.as_bytes()).is_err(), "{k}");
    }
    // An RSA link proof makes a full offline-verified link.
    let mut l = golden_link();
    l.statement = msg;
    l.proof = vector("rsa4096.sig");
    l.key_fingerprint = parse_openssh(&vector("rsa4096.pub")).unwrap().fingerprint(HashAlg::Sha256).to_string();
    assert!(offline_check(&l, ctx(), now()).unwrap().is_some());
}

#[test]
fn rsa_wrong_namespace_is_refused() {
    let msg = vector("statement.txt");
    let sig = vector("rsa4096.git.sig");
    assert!(verify_sshsig(&sig, "git", msg.as_bytes()).is_ok());
    assert!(verify_sshsig(&sig, LINK_NAMESPACE, msg.as_bytes()).is_err());
    // A link-namespace signature checked under another namespace.
    assert!(verify_sshsig(&vector("rsa4096.sig"), "git", msg.as_bytes()).is_err());
}

/// `ssh-keygen -Y sign -O hashalg=sha256`: a valid signature, but the SSHSIG
/// hash must be sha512.
#[test]
fn rsa_sha256_sshsig_hash_is_refused() {
    let err = verify_sshsig(&vector("rsa_sha256hash.sig"), LINK_NAMESPACE, vector("statement.txt").as_bytes())
        .unwrap_err();
    assert!(err.contains("sha512"), "{err}");
}

/// Genuine signatures (OpenSSH accepts the rsa-sha2-256 one) over the right
/// SSHSIG blob, but with rsa-sha2-256 or ssh-rsa (SHA-1): refused.
#[test]
fn rsa_sha2_256_and_sha1_signature_algorithms_are_refused() {
    let msg = vector("statement.txt");
    let err = verify_sshsig(&vector("rsa_sha2_256.sig"), LINK_NAMESPACE, msg.as_bytes()).unwrap_err();
    assert!(err.contains("rsa-sha2-512"), "{err}");
    // ssh-key won't even parse a SHA-1 ssh-rsa SSHSIG.
    assert!(verify_sshsig(&vector("rsa_sha1.sig"), LINK_NAMESPACE, msg.as_bytes()).is_err());
    // Relabelling a good rsa-sha2-512 signature as rsa-sha2-256 doesn't help.
    let good = rsa_sig("rsa3072b.sig");
    let relabelled =
        rsa_with_sig_bytes(&good, ssh_key::Algorithm::Rsa { hash: Some(HashAlg::Sha256) }, good.signature().as_bytes().to_vec());
    assert!(verify_sshsig(&relabelled, LINK_NAMESPACE, msg.as_bytes()).is_err());
}

#[test]
fn rsa_small_and_oversized_moduli_are_refused() {
    let msg = vector("statement.txt");
    let err = verify_sshsig(&vector("rsa2048.sig"), LINK_NAMESPACE, msg.as_bytes()).unwrap_err();
    assert!(err.contains("3072"), "{err}");
    // 3071 bits: one short.
    let mut n = vec![0xffu8; 384];
    n[0] = 0x7f;
    let err = verify_sshsig(&rsa_synthetic(&n, &[1, 0, 1], 384), LINK_NAMESPACE, msg.as_bytes()).unwrap_err();
    assert!(err.contains("3072"), "{err}");
    // 16392 bits, and a huge 64 KiB-class modulus: refused before any math.
    for len in [2049usize, 8192] {
        let n = vec![0xffu8; len];
        let armored = rsa_synthetic(&n, &[1, 0, 1], len);
        let err = verify_sshsig(&armored, LINK_NAMESPACE, msg.as_bytes()).unwrap_err();
        assert!(err.contains("16384") || err.contains("too large"), "{len}: {err}");
    }
}

/// Exponents RSA verification must not accept: 1, even, larger than the
/// ceiling, at least the modulus.
#[test]
fn rsa_exponent_edge_cases_are_refused() {
    let msg = vector("statement.txt");
    let n = rsa_sig("rsa3072b.sig").public_key().rsa().unwrap().n.as_positive_bytes().unwrap().to_vec();
    let too_big = [0xffu8; 9];
    let huge = vec![0x01u8; 384];
    for e in [&[1u8][..], &[2], &[1, 0, 0], &too_big, &[0x02, 0, 0, 0, 1], &huge, &n] {
        let armored = rsa_synthetic(&n, e, 384);
        assert!(verify_sshsig(&armored, LINK_NAMESPACE, msg.as_bytes()).is_err(), "{e:?}");
    }
    // e = 3 is a legal exponent: refused only because the signature is junk.
    let err = verify_sshsig(&rsa_synthetic(&n, &[3], 384), LINK_NAMESPACE, msg.as_bytes()).unwrap_err();
    assert!(err.contains("doesn't verify"), "{err}");
}

/// The signature must be exactly the modulus length (RFC 8017 8.2.2), as
/// with rsa 0.9 before: a valid signature whose first byte is zero verifies
/// at full length; stripped of that byte, or with an extra leading zero,
/// it doesn't. Signatures not below the modulus are refused.
#[test]
fn rsa_signature_length_and_range_are_strict() {
    let msg = vector("rsa_leading_zero.txt");
    let good = rsa_sig("rsa_leading_zero.sig");
    let bytes = good.signature().as_bytes().to_vec();
    assert_eq!(bytes.len(), 384);
    assert_eq!(bytes[0], 0);
    verify_sshsig(&vector("rsa_leading_zero.sig"), LINK_NAMESPACE, msg.as_bytes()).unwrap();
    let short = rsa_with_sig_bytes(&good, RSA_SHA512, bytes[1..].to_vec());
    assert!(verify_sshsig(&short, LINK_NAMESPACE, msg.as_bytes()).is_err());
    let mut long = vec![0u8];
    long.extend_from_slice(&bytes);
    assert!(verify_sshsig(&rsa_with_sig_bytes(&good, RSA_SHA512, long), LINK_NAMESPACE, msg.as_bytes()).is_err());
    // s = n, s = n + good (mod 2^k): not below the modulus.
    let n = good.public_key().rsa().unwrap().n.as_positive_bytes().unwrap().to_vec();
    assert!(verify_sshsig(&rsa_with_sig_bytes(&good, RSA_SHA512, n.clone()), LINK_NAMESPACE, msg.as_bytes()).is_err());
    let mut sum = vec![0u8; n.len()];
    let mut carry = 0u16;
    for i in (0..n.len()).rev() {
        let v = n[i] as u16 + bytes[i] as u16 + carry;
        sum[i] = v as u8;
        carry = v >> 8;
    }
    assert_eq!(carry, 0, "n + s fits (s has a leading zero byte)");
    assert!(verify_sshsig(&rsa_with_sig_bytes(&good, RSA_SHA512, sum), LINK_NAMESPACE, msg.as_bytes()).is_err());
    for junk in [vec![0u8; 384], vec![0xffu8; 384], { let mut v = vec![0u8; 384]; v[383] = 1; v }] {
        assert!(verify_sshsig(&rsa_with_sig_bytes(&good, RSA_SHA512, junk), LINK_NAMESPACE, msg.as_bytes()).is_err());
    }
}

/// Bleichenbacher 2006 against e = 3: `00 01 FF 00 DigestInfo(SHA-512)
/// hash` followed by garbage, made a perfect cube (`rsa_bb06_e3.sig`, a
/// synthetic 3072-bit modulus). A lenient PKCS#1 v1.5 parser accepts it;
/// OpenSSH and this verifier don't.
#[test]
fn rsa_e3_garbage_after_digest_forgery_is_refused() {
    let sig = rsa_sig("rsa_bb06_e3.sig");
    assert_eq!(sig.public_key().rsa().unwrap().e.as_positive_bytes().unwrap(), &[3]);
    let err = verify_sshsig(&vector("rsa_bb06_e3.sig"), LINK_NAMESPACE, vector("statement.txt").as_bytes()).unwrap_err();
    assert!(err.contains("doesn't verify"), "{err}");
}

/// An even modulus (not an RSA key) is refused without panicking.
#[test]
fn rsa_even_modulus_is_refused() {
    let mut n = vec![0xffu8; 384];
    n[383] = 0xfe;
    assert!(verify_sshsig(&rsa_synthetic(&n, &[1, 0, 1], 384), LINK_NAMESPACE, vector("statement.txt").as_bytes()).is_err());
}

/// OpenSSH signs and verifies with an empty reserved field; an RSA SSHSIG
/// carrying one is refused.
#[test]
fn rsa_nonempty_reserved_field_is_refused() {
    use base64::Engine as _;
    let pem = vector("rsa3072b.sig");
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let mut blob = base64::engine::general_purpose::STANDARD.decode(body).unwrap();
    let be = |b: &[u8]| u32::from_be_bytes(b.try_into().unwrap()) as usize;
    let mut at = 6 + 4;
    at += 4 + be(&blob[at..at + 4]); // public key
    at += 4 + be(&blob[at..at + 4]); // namespace
    assert_eq!(&blob[at..at + 4], &[0, 0, 0, 0]);
    blob.splice(at..at + 4, [0, 0, 0, 1, b'x']);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&blob);
    let lines: Vec<&str> = b64.as_bytes().chunks(70).map(|c| std::str::from_utf8(c).unwrap()).collect();
    let armored = format!("-----BEGIN SSH SIGNATURE-----\n{}\n-----END SSH SIGNATURE-----\n", lines.join("\n"));
    assert_eq!(SshSig::from_pem(&armored).unwrap().reserved(), b"x");
    assert!(verify_sshsig(&armored, LINK_NAMESPACE, vector("statement.txt").as_bytes()).is_err());
}

/// Every one-byte corruption and truncation of an RSA SSHSIG is refused
/// without panicking, including the unsigned version field (1 -> 0).
#[test]
fn rsa_hostile_sshsig_bytes_never_verify_or_panic() {
    use base64::Engine as _;
    let msg = vector("statement.txt");
    let pem = vector("rsa3072b.sig");
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let blob = base64::engine::general_purpose::STANDARD.decode(body).unwrap();
    let armor = |b: &[u8]| {
        let b64 = base64::engine::general_purpose::STANDARD.encode(b);
        let lines: Vec<&str> = b64.as_bytes().chunks(70).map(|c| std::str::from_utf8(c).unwrap()).collect();
        format!("-----BEGIN SSH SIGNATURE-----\n{}\n-----END SSH SIGNATURE-----\n", lines.join("\n"))
    };
    assert!(verify_sshsig(&armor(&blob), LINK_NAMESPACE, msg.as_bytes()).is_ok());
    // Every framing/key byte; every 16th byte of the 384-byte signature value
    // (each of those costs a full RSA operation in a debug build).
    for i in (0..blob.len()).filter(|&i| i + 384 < blob.len() || i % 16 == 0) {
        for x in [0x01u8, 0x80, 0xff] {
            let mut b = blob.clone();
            b[i] ^= x;
            assert!(verify_sshsig(&armor(&b), LINK_NAMESPACE, msg.as_bytes()).is_err(), "byte {i} ^ {x:#x}");
        }
    }
    for len in 0..blob.len() {
        assert!(verify_sshsig(&armor(&blob[..len]), LINK_NAMESPACE, msg.as_bytes()).is_err(), "truncated to {len}");
    }
}

/// S3 `sshsig_wrong_namespace_rejected`: a signature made for `git`,
/// `file`, or checked with an empty namespace never passes as a link proof.
#[test]
fn sshsig_wrong_namespace_rejected() {
    let msg = vector("statement.txt");
    for ns in ["git", "file"] {
        let sig = vector(&format!("ed25519.{ns}.sig"));
        // Valid under its own namespace...
        assert!(verify_sshsig(&sig, ns, msg.as_bytes()).is_ok(), "{ns}");
        // ...never as an identity link.
        assert!(verify_sshsig(&sig, LINK_NAMESPACE, msg.as_bytes()).is_err(), "{ns}");
        let mut l = golden_link();
        l.statement = msg.clone();
        l.proof = sig;
        l.key_fingerprint = parse_openssh(&vector("ed25519.pub")).unwrap().fingerprint(HashAlg::Sha256).to_string();
        assert!(matches!(offline_check(&l, ctx(), now()), Err(LinkStatus::Invalid { .. })), "{ns}");
    }
    assert!(verify_sshsig(&vector("ed25519.sig"), "", msg.as_bytes()).is_err(), "empty namespace");
}

#[test]
fn signature_by_another_key_than_named_is_invalid() {
    let mut l = signed("github", "github.com", "1234567", "alice", &key(1));
    l.key_fingerprint = key(2).public_key().fingerprint(HashAlg::Sha256).to_string();
    assert!(matches!(offline_check(&l, ctx(), now()), Err(LinkStatus::Invalid { .. })));
}

/// S2 `link_replayed_by_other_account_invalid` (view side), and cross
/// workspace replay: a statement checked against another workspace, or
/// another account, fails.
#[test]
fn link_replayed_into_another_workspace_or_account_is_invalid() {
    let l = signed("github", "github.com", "1234567", "alice", &key(1));
    let other_ws = LinkContext { workspace_id: Uuid::from_u128(9), account_key: Some(&ACCOUNT_KEY) };
    assert!(matches!(offline_check(&l, other_ws, now()), Err(LinkStatus::Invalid { .. })));
    let mut mallory = l.clone();
    mallory.account_id = Uuid::from_u128(666);
    assert!(offline_check(&mallory, ctx(), now()).is_err());
}

#[test]
fn rotated_account_key_turns_stale_and_expiry_expires() {
    let l = signed("github", "github.com", "1234567", "alice", &key(1));
    let rotated = LinkContext { workspace_id: ws(), account_key: Some(&[8u8; 32]) };
    assert!(matches!(offline_check(&l, rotated, now()), Err(LinkStatus::Stale { .. })));
    assert_eq!(offline_check(&l, ctx(), ts("2027-11-14T12:00:00Z")).unwrap_err(), LinkStatus::Expired);
}

/// S13 `self_asserted_never_shown_verified`.
#[tokio::test]
async fn self_asserted_never_shown_verified() {
    let mut l = golden_link();
    l.provider = "bitbucket".into();
    l.host = "bitbucket.org".into();
    l.method = LinkMethod::SelfAsserted;
    l.nonce.clear();
    let f = LinkFetcher::new(ForgeConfig { check_forges: true, ..Default::default() });
    let s = check_link(&l, ctx(), &f, now(), None).await;
    assert_eq!(s, LinkStatus::SelfAsserted);
    assert!(!s.is_verified());
}

// ---------------------------------------------------------------------------
// Fake forges

#[derive(Default)]
struct Fake {
    routes: BTreeMap<String, VecDeque<(u16, String)>>,
    hits: usize,
    authorization_seen: bool,
}

#[derive(Clone)]
struct Server {
    url: String,
    st: Arc<Mutex<Fake>>,
}

async fn handle(State(st): State<Arc<Mutex<Fake>>>, req: Request) -> Response {
    let mut f = st.lock().unwrap();
    f.hits += 1;
    if req.headers().contains_key("authorization") || req.headers().contains_key("cookie") {
        f.authorization_seen = true;
    }
    let key = req.uri().path_and_query().map(|p| p.as_str().to_string()).unwrap_or_default();
    let (status, body) = match f.routes.get_mut(&key) {
        Some(q) if q.len() > 1 => q.pop_front().unwrap(),
        Some(q) => q.front().cloned().unwrap(),
        None => (404, "{\"message\":\"Not Found\"}".into()),
    };
    let mut r = Response::new(Body::from(body.clone()));
    *r.status_mut() = StatusCode::from_u16(status).unwrap();
    if (300..400).contains(&status) {
        r.headers_mut().insert("location", HeaderValue::from_str(&body).unwrap());
    }
    r
}

async fn server() -> Server {
    let st = Arc::new(Mutex::new(Fake::default()));
    let app = axum::Router::new().fallback(handle).with_state(st.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Server { url, st }
}

impl Server {
    fn set(&self, path: &str, status: u16, body: impl Into<String>) {
        self.st.lock().unwrap().routes.insert(path.into(), VecDeque::from([(status, body.into())]));
    }
    /// Answers in turn (the last one repeats).
    fn seq(&self, path: &str, answers: Vec<(u16, String)>) {
        self.st.lock().unwrap().routes.insert(path.into(), answers.into());
    }
    fn hits(&self) -> usize {
        self.st.lock().unwrap().hits
    }
    fn saw_credentials(&self) -> bool {
        self.st.lock().unwrap().authorization_seen
    }
}

fn fetcher(apis: &[(&str, &str, &Server)]) -> LinkFetcher {
    let mut cfg = ForgeConfig { check_forges: true, ..Default::default() };
    for (p, h, s) in apis {
        cfg.test_api.insert((p.to_string(), h.to_string()), s.url.clone());
    }
    LinkFetcher::new(cfg)
}

fn gh_user(id: &str, login: &str) -> String {
    serde_json::json!({"id": id.parse::<u64>().unwrap(), "login": login}).to_string()
}

fn key_json(keys: &[String]) -> String {
    serde_json::Value::Array(keys.iter().enumerate().map(|(i, k)| serde_json::json!({"id": i, "key": k})).collect()).to_string()
}

/// GitHub: `/user/{id}` → login, `/users/{login}/keys` and
/// `/ssh_signing_keys`, then `/user/{id}` again.
fn github_alice(s: &Server, login: &str, auth: &[String], signing: &[String]) {
    s.set("/user/1234567", 200, gh_user("1234567", login));
    s.set(&format!("/users/{login}/keys"), 200, key_json(auth));
    s.set(&format!("/users/{login}/ssh_signing_keys"), 200, key_json(signing));
}

#[tokio::test]
async fn github_verifies_by_stable_id_without_credentials() {
    let gh = server().await;
    let k = key(1);
    github_alice(&gh, "alice", &[openssh_pub(&k)], &[]);
    gh.set("/users/alice", 200, gh_user("1234567", "alice"));
    let f = fetcher(&[("github", "github.com", &gh)]);
    assert_eq!(f.resolve_handle("github", "github.com", "@alice").await.unwrap(), ("1234567".into(), "alice".into()));
    let l = signed("github", "github.com", "1234567", "alice", &k);
    let s = check_link(&l, ctx(), &f, now(), None).await;
    assert!(matches!(&s, LinkStatus::Verified { handle: Some(h), .. } if h == "alice"), "{s:?}");
    assert!(!gh.saw_credentials(), "never a token or cookie");
}

/// S1 `link_pins_subject_id_not_handle`: Alice renamed to `alice2` and
/// Mallory registered `alice`. Alice's link follows her id; Mallory's keys
/// under the old name never make it verify, and her own link (signed by
/// her key, naming the handle "alice") is bound to her own id.
#[tokio::test]
async fn link_pins_subject_id_not_handle() {
    let gh = server().await;
    let (alice, mallory) = (key(1), key(2));
    github_alice(&gh, "alice2", &[openssh_pub(&alice)], &[]);
    gh.set("/users/alice/keys", 200, key_json(&[openssh_pub(&mallory)]));
    gh.set("/users/alice/ssh_signing_keys", 200, "[]");
    gh.set("/user/999", 200, gh_user("999", "alice"));
    let f = fetcher(&[("github", "github.com", &gh)]);
    let a = signed("github", "github.com", "1234567", "alice", &alice);
    let s = check_link(&a, ctx(), &f, now(), None).await;
    assert!(matches!(&s, LinkStatus::Verified { handle: Some(h), .. } if h == "alice2"), "shows the current handle: {s:?}");
    // Mallory claims Alice's id with her own key: not on Alice's list.
    let m = signed("github", "github.com", "1234567", "alice", &mallory);
    assert!(matches!(check_link(&m, ctx(), &f, now(), None).await, LinkStatus::Stale { .. }));
    // Mallory's honest link is her own id, whatever handle it shows.
    let m = signed("github", "github.com", "999", "alice", &mallory);
    assert!(check_link(&m, ctx(), &f, now(), None).await.is_verified());
}

/// S1 `rename_between_resolve_and_fetch_is_rejected` (TOCTOU).
#[tokio::test]
async fn rename_between_resolve_and_fetch_is_rejected() {
    let gh = server().await;
    let k = key(1);
    github_alice(&gh, "alice", &[openssh_pub(&k)], &[]);
    gh.seq("/user/1234567", vec![(200, gh_user("1234567", "alice")), (200, gh_user("1234567", "alice-renamed"))]);
    let l = signed("github", "github.com", "1234567", "alice", &k);
    let s = check_link(&l, ctx(), &fetcher(&[("github", "github.com", &gh)]), now(), None).await;
    assert!(matches!(&s, LinkStatus::Unreachable { reason, .. } if reason.contains("renamed")), "{s:?}");
}

/// S4 `removed_forge_key_turns_stale` and `cache_ttl_bounds_staleness`.
#[tokio::test]
async fn removed_forge_key_turns_stale_within_the_cache_ttl() {
    let gh = server().await;
    let k = key(1);
    github_alice(&gh, "alice", &[openssh_pub(&k)], &[]);
    let f = fetcher(&[("github", "github.com", &gh)]);
    let l = signed("github", "github.com", "1234567", "alice", &k);
    let cache = MemoryCache::default();
    let t0 = now();
    assert!(check_cached(&cache, &l, ctx(), &f, t0, false).await.is_verified());
    let hits = gh.hits();
    // The key is removed on the forge.
    github_alice(&gh, "alice", &[], &[]);
    let t1 = Timestamp(t0.0 + time::Duration::hours(1));
    assert!(check_cached(&cache, &l, ctx(), &f, t1, false).await.is_verified(), "cached within the TTL");
    assert_eq!(gh.hits(), hits, "no request inside the TTL");
    let t2 = Timestamp(t0.0 + time::Duration::hours(24) + time::Duration::seconds(1));
    let s = check_cached(&cache, &l, ctx(), &f, t2, false).await;
    assert!(matches!(s, LinkStatus::Stale { .. }), "stale once the TTL passed: {s:?}");
    // A forced re-check (opening the profile) sees it at once.
    let cache = MemoryCache::default();
    github_alice(&gh, "alice", &[openssh_pub(&k)], &[]);
    assert!(check_cached(&cache, &l, ctx(), &f, t0, false).await.is_verified());
    github_alice(&gh, "alice", &[], &[]);
    assert!(matches!(check_cached(&cache, &l, ctx(), &f, t1, true).await, LinkStatus::Stale { .. }));
}

#[tokio::test]
async fn deleted_forge_account_is_stale_and_errors_are_unreachable() {
    let gh = server().await;
    let k = key(1);
    let l = signed("github", "github.com", "1234567", "alice", &k);
    let f = fetcher(&[("github", "github.com", &gh)]);
    assert!(matches!(check_link(&l, ctx(), &f, now(), None).await, LinkStatus::Stale { .. }), "404");
    gh.set("/user/1234567", 503, "busy");
    let last = Some(ts("2026-11-20T00:00:00Z"));
    let s = check_link(&l, ctx(), &f, now(), last).await;
    assert!(matches!(s, LinkStatus::Unreachable { last_verified_at, .. } if last_verified_at == last));
}

/// S10 `forge_checks_disabled_makes_no_requests`.
#[tokio::test]
async fn forge_checks_disabled_makes_no_requests() {
    let gh = server().await;
    let k = key(1);
    github_alice(&gh, "alice", &[openssh_pub(&k)], &[]);
    let mut f = fetcher(&[("github", "github.com", &gh)]);
    f.cfg.check_forges = false;
    let l = signed("github", "github.com", "1234567", "alice", &k);
    let cache = MemoryCache::default();
    assert_eq!(check_cached(&cache, &l, ctx(), &f, now(), true).await, LinkStatus::SignatureOnly);
    assert_eq!(gh.hits(), 0);
}

#[tokio::test]
async fn github_key_published_uses_the_signing_list_only() {
    let gh = server().await;
    let mine = account_key_openssh(&ACCOUNT_KEY).unwrap();
    let mut l = golden_link();
    l.method = LinkMethod::KeyPublished;
    l.nonce.clear();
    let f = fetcher(&[("github", "github.com", &gh)]);
    // On the auth list only (that would grant push): not a published key.
    github_alice(&gh, "alice", std::slice::from_ref(&mine), &[]);
    assert!(matches!(check_link(&l, ctx(), &f, now(), None).await, LinkStatus::Stale { .. }));
    github_alice(&gh, "alice", &[], &[mine]);
    assert!(check_link(&l, ctx(), &f, now(), None).await.is_verified());
}

#[tokio::test]
async fn gitlab_lists_keys_by_id() {
    let gl = server().await;
    let k = key(3);
    let mine = account_key_openssh(&ACCOUNT_KEY).unwrap();
    gl.set(
        "/users/77/keys",
        200,
        serde_json::json!([
            {"id": 1, "key": openssh_pub(&k), "usage_type": "auth"},
            {"id": 2, "key": mine, "usage_type": "signing"},
        ])
        .to_string(),
    );
    gl.set("/users/77", 403, "{\"message\":\"403 Forbidden\"}");
    gl.set("/users?username=bob", 200, serde_json::json!([{"id": 77, "username": "bob"}]).to_string());
    let f = fetcher(&[("gitlab", "gitlab.com", &gl)]);
    assert_eq!(f.resolve_handle("gitlab", "gitlab.com", "@bob").await.unwrap(), ("77".into(), "bob".into()));
    let l = signed("gitlab", "gitlab.com", "77", "bob", &k);
    let s = check_link(&l, ctx(), &f, now(), None).await;
    assert!(matches!(&s, LinkStatus::Verified { handle: None, .. }), "verified by id, handle unknown: {s:?}");
    let mut kp = golden_link();
    kp.provider = "gitlab".into();
    kp.host = "gitlab.com".into();
    kp.subject_id = "77".into();
    kp.method = LinkMethod::KeyPublished;
    kp.nonce.clear();
    assert!(check_link(&kp, ctx(), &f, now(), None).await.is_verified(), "signing usage");
}

#[tokio::test]
async fn forgejo_codeberg_and_gitea_use_the_anonymous_keys_listing_with_an_id_cross_check() {
    for (provider, host) in [("forgejo", "codeberg.org"), ("gitea", "gitea.com")] {
        let fj = server().await;
        let k = key(4);
        let carol = serde_json::json!({"ok": true, "data": [{"id": 55, "login": "carol"}]}).to_string();
        fj.set("/api/v1/users/search?uid=55", 200, carol.clone());
        fj.set("/carol.keys", 200, format!("{}\n", openssh_pub(&k)));
        fj.set("/api/v1/users/search?q=carol&limit=50", 200, carol);
        let f = fetcher(&[(provider, host, &fj)]);
        assert_eq!(f.resolve_handle(provider, host, "carol").await.unwrap().0, "55");
        let l = signed(provider, host, "55", "carol", &k);
        let s = check_link(&l, ctx(), &f, now(), None).await;
        assert!(matches!(&s, LinkStatus::Verified { handle: Some(h), .. } if h == "carol"), "{provider}: {s:?}");
        // The search answering for another uid is not this account.
        fj.set("/api/v1/users/search?uid=55", 200, serde_json::json!({"ok": true, "data": [{"id": 56, "login": "carol"}]}).to_string());
        assert!(matches!(check_link(&l, ctx(), &f, now(), None).await, LinkStatus::Stale { .. }), "{provider}");
    }
}

/// SSRF guards: a host this device wasn't told about is never contacted, a
/// redirect is never followed, an oversized answer is dropped, a
/// self-hosted forge is HTTPS only.
#[tokio::test]
async fn ssrf_guards_unconfigured_hosts_redirects_and_size() {
    let gh = server().await;
    let elsewhere = server().await;
    let k = key(1);
    // A co-member's link naming an internal host: not asked.
    let l = signed("gitlab", "gitlab.internal.example", "1", "x", &k);
    let s = check_link(&l, ctx(), &fetcher(&[("github", "github.com", &gh)]), now(), None).await;
    assert!(matches!(s, LinkStatus::NotChecked { .. }), "{s:?}");
    assert_eq!(gh.hits(), 0);
    // A redirect to another host is not followed.
    elsewhere.set("/user/1234567", 200, gh_user("1234567", "alice"));
    gh.set("/user/1234567", 302, format!("{}/user/1234567", elsewhere.url));
    let l = signed("github", "github.com", "1234567", "alice", &k);
    let f = fetcher(&[("github", "github.com", &gh)]);
    assert!(matches!(check_link(&l, ctx(), &f, now(), None).await, LinkStatus::Unreachable { .. }));
    assert_eq!(elsewhere.hits(), 0, "redirect not followed");
    // An answer over the cap.
    github_alice(&gh, "alice", &[openssh_pub(&k)], &[]);
    gh.set("/users/alice/keys", 200, "[".to_string() + &"0,".repeat(MAX_RESPONSE_BYTES) + "0]");
    assert!(matches!(check_link(&l, ctx(), &f, now(), None).await, LinkStatus::Unreachable { .. }));
    // A self-hosted forge must be HTTPS.
    let cfg = ForgeConfig {
        check_forges: true,
        self_hosted: vec![SelfHostedForge { provider: "gitlab".into(), host: "gl.example".into(), base_url: "http://gl.example".into() }],
        ..Default::default()
    };
    assert!(cfg.api_root("gitlab", "gl.example").is_none());
    let cfg = ForgeConfig {
        self_hosted: vec![SelfHostedForge { provider: "gitlab".into(), host: "gl.example".into(), base_url: "https://gl.example".into() }],
        ..Default::default()
    };
    assert_eq!(cfg.api_root("gitlab", "gl.example").as_deref(), Some("https://gl.example/api/v4"));
}

#[test]
fn handles_are_strict() {
    assert!(!safe_segment(".."));
    assert!(!safe_segment("a/b"));
    assert!(!safe_segment("a?b"));
}

/// The relay is never involved: a link naming the relay's host isn't a
/// forge this device asks, and the status comes only from the forge.
#[tokio::test]
async fn relay_cannot_forge_or_suppress_verification() {
    let relay = server().await;
    let gh = server().await;
    let k = key(1);
    github_alice(&gh, "alice", &[openssh_pub(&k)], &[]);
    // A relay that also pretends to be a forge for the host it serves.
    relay.set("/user/1234567", 200, gh_user("1234567", "alice"));
    relay.set("/users/alice/keys", 200, key_json(&[openssh_pub(&key(9))]));
    let host = relay.url.trim_start_matches("http://").replace(':', "-");
    let fake = signed("github", &host, "1234567", "alice", &key(9));
    let f = fetcher(&[("github", "github.com", &gh)]);
    assert!(matches!(check_link(&fake, ctx(), &f, now(), None).await, LinkStatus::NotChecked { .. }));
    assert_eq!(relay.hits(), 0, "the relay is never asked");
    // The genuine link verifies from the forge alone.
    let l = signed("github", "github.com", "1234567", "alice", &k);
    assert!(check_link(&l, ctx(), &f, now(), None).await.is_verified());
}

/// S14 (extends `fold_ignores_env`): forges answering differently change
/// the device-local status only; the fold and the roster are identical.
#[tokio::test]
async fn fold_ignores_forge_answers() {
    use hive_core::{ActorIdentity, ActorKind, SigningKeypair};
    let store = crate::EventStore::open_in_memory().unwrap();
    let account = Uuid::new_v4();
    let mut author = ActorIdentity::new(account.to_string(), "Ana", ActorKind::Human);
    author.account_id = Some(account);
    let mut svc = crate::ChatService::new(store, Uuid::new_v4(), SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), author);
    let wid = Uuid::new_v4();
    svc.ensure_workspace_config(wid).unwrap();
    let k = key(1);
    let d = crate::chat_service::LinkDraft::new("github", "github.com", "1234567", "alice", LinkMethod::SshSig);
    let mut l = svc.draft_identity_link(wid, &d).unwrap();
    l.proof = k.sign(LINK_NAMESPACE, HashAlg::Sha512, l.statement.as_bytes()).unwrap().to_pem(LineEnding::LF).unwrap();
    l.key_fingerprint = k.public_key().fingerprint(HashAlg::Sha256).to_string();
    svc.publish_identity_link(wid, l.clone()).unwrap();

    let fold = |svc: &crate::ChatService| {
        let (_, t) = svc.store().load_config(wid, None, None).unwrap();
        (t.fingerprint(), t.current().clone())
    };
    let before = fold(&svc);
    let (yes, no) = (server().await, server().await);
    github_alice(&yes, "alice", &[openssh_pub(&k)], &[]);
    github_alice(&no, "alice", &[], &[]);
    let key = svc.account_public_key();
    let c = LinkContext { workspace_id: wid, account_key: Some(&key) };
    let a = check_link(&l, c, &fetcher(&[("github", "github.com", &yes)]), Timestamp::now(), None).await;
    let b = check_link(&l, c, &fetcher(&[("github", "github.com", &no)]), Timestamp::now(), None).await;
    assert!(a.is_verified() && matches!(b, LinkStatus::Stale { .. }));
    let after = fold(&svc);
    assert_eq!(before.0, after.0);
    assert_eq!(before.1, after.1);
}

// ---------------------------------------------------------------------------
// Adversarial review (linked identities, 2.0 residuals)

/// A forge is not trusted to hand back a clean name: a GitLab username
/// carrying bidi overrides or zero-width characters is shown without them.
#[tokio::test]
async fn forge_supplied_handles_lose_bidi_controls() {
    let gl = server().await;
    let k = key(3);
    gl.set("/users/77/keys", 200, serde_json::json!([{"id": 1, "key": openssh_pub(&k), "usage_type": "auth"}]).to_string());
    gl.set("/users/77", 200, serde_json::json!({"id": 77, "username": "b\u{200B}ob\u{202E}"}).to_string());
    let f = fetcher(&[("gitlab", "gitlab.com", &gl)]);
    let l = signed("gitlab", "gitlab.com", "77", "bob", &k);
    let s = check_link(&l, ctx(), &f, now(), None).await;
    assert!(matches!(&s, LinkStatus::Verified { handle: Some(h), .. } if h == "bob"), "{s:?}");
}

/// A published-key link has no statement: what it proves depends on the
/// account key at check time. A verified check cached under one account key
/// must not answer for the link once the account key changed (rotation, or
/// another workspace registering another key for the same account).
#[tokio::test]
async fn cached_check_is_bound_to_the_account_key() {
    let gh = server().await;
    let mine = account_key_openssh(&ACCOUNT_KEY).unwrap();
    github_alice(&gh, "alice", &[], &[mine]);
    let mut l = golden_link();
    l.method = LinkMethod::KeyPublished;
    l.nonce.clear();
    let f = fetcher(&[("github", "github.com", &gh)]);
    let cache = MemoryCache::default();
    assert!(check_cached(&cache, &l, ctx(), &f, now(), false).await.is_verified());
    let other = [9u8; 32];
    let rotated = LinkContext { workspace_id: ws(), account_key: Some(&other) };
    let later = Timestamp(now().0 + time::Duration::hours(1));
    let s = check_cached(&cache, &l, rotated, &f, later, false).await;
    assert!(matches!(s, LinkStatus::Stale { .. }), "the forge lists the old key, not this one: {s:?}");
    assert_ne!(cache_key(&l, ws(), &ACCOUNT_KEY), cache_key(&l, ws(), &other));
}

/// D15: the published-key fallback is offered only where the Ligus key
/// grants no push access. GitLab's default usage type, "Authentication &
/// Signing", grants push; only a "Signing" key counts.
#[tokio::test]
async fn gitlab_key_published_needs_a_signing_only_key() {
    let gl = server().await;
    let mine = account_key_openssh(&ACCOUNT_KEY).unwrap();
    gl.set("/users/77/keys", 200, serde_json::json!([{"id": 2, "key": mine, "usage_type": "auth_and_signing"}]).to_string());
    let f = fetcher(&[("gitlab", "gitlab.com", &gl)]);
    let mut kp = golden_link();
    kp.provider = "gitlab".into();
    kp.host = "gitlab.com".into();
    kp.subject_id = "77".into();
    kp.method = LinkMethod::KeyPublished;
    kp.nonce.clear();
    let s = check_link(&kp, ctx(), &f, now(), None).await;
    assert!(matches!(s, LinkStatus::Stale { .. }), "a key that also grants push: {s:?}");
    gl.set("/users/77/keys", 200, serde_json::json!([{"id": 2, "key": mine, "usage_type": "signing"}]).to_string());
    assert!(check_link(&kp, ctx(), &f, now(), None).await.is_verified());
}

/// People shows chips from the cache without asking the forge. A cached
/// "verified" must not outlive the link (expiry), the 24-hour TTL, the
/// account key it was made under, or forge checks being turned off.
#[test]
fn cached_chip_status_is_only_shown_while_it_holds() {
    let k = key(1);
    let l = signed("github", "github.com", "1234567", "alice", &k);
    let t0 = now();
    let verified = CachedCheck {
        status: LinkStatus::Verified { checked_at: t0, handle: Some("alice".into()) },
        checked_at: t0,
        last_verified_at: Some(t0),
    };
    let soon = Timestamp(t0.0 + time::Duration::hours(1));
    assert!(cached_status(Some(&verified), &l, ctx(), soon, true).is_some_and(|s| s.is_verified()));
    let day_later = Timestamp(t0.0 + time::Duration::hours(30));
    assert_eq!(cached_status(Some(&verified), &l, ctx(), day_later, true), None, "past the TTL: not checked");
    assert_eq!(cached_status(Some(&verified), &l, ctx(), ts("2027-11-15T00:00:00Z"), true), Some(LinkStatus::Expired));
    assert_eq!(cached_status(Some(&verified), &l, ctx(), soon, false), Some(LinkStatus::SignatureOnly), "checks off");
    let other = [9u8; 32];
    let rotated = LinkContext { workspace_id: ws(), account_key: Some(&other) };
    assert!(!cached_status(Some(&verified), &l, rotated, soon, true).is_some_and(|s| s.is_verified()));
    assert_eq!(cached_status(None, &l, ctx(), soon, true), None);
}

/// 2.0 dropped the SourceHut and Tangled verifiers (owner decision
/// 2026-11-16). A link of either kind made by a 2.0 pre-release still folds
/// (the wire shape is unchanged), but it is shown as self-asserted: never
/// verified, never checked against a forge, never served from a cached
/// "verified", and no new one can be prepared.
#[tokio::test]
async fn dropped_forge_links_are_self_asserted_and_never_verify() {
    let k = key(5);
    let forge = server().await;
    forge.set("/~dave.keys", 200, format!("{}\n", openssh_pub(&k)));
    let f = fetcher(&[("sourcehut", "meta.sr.ht", &forge), ("tangled", "atproto", &forge)]);
    let cache = MemoryCache::default();
    for l in [
        signed("sourcehut", "meta.sr.ht", "~dave", "~dave", &k),
        signed("tangled", "atproto", "did:plc:abcdefghijklmnopqrstuvwx", "erin.example.com", &k),
    ] {
        assert_eq!(offline_check(&l, ctx(), now()).unwrap_err(), LinkStatus::SelfAsserted, "{}", l.provider);
        assert_eq!(check_link(&l, ctx(), &f, now(), None).await, LinkStatus::SelfAsserted);
        // A "verified" cached by a pre-release build is not served.
        let key = cache_key(&l, ws(), &ACCOUNT_KEY);
        let stale = CachedCheck {
            status: serde_json::from_value(serde_json::json!({"state": "verified", "checkedAt": now(), "handle": "~dave", "nameBound": true})).unwrap(),
            checked_at: now(),
            last_verified_at: Some(now()),
        };
        cache.put_check(&key, &stale);
        assert_eq!(check_cached(&cache, &l, ctx(), &f, now(), false).await, LinkStatus::SelfAsserted);
        assert_eq!(cached_status(cache.get_check(&key).as_ref(), &l, ctx(), now(), true), Some(LinkStatus::SelfAsserted));
        // A forged signature is still invalid, not self-asserted.
        let mut bad = l.clone();
        bad.proof = signed(&l.provider, &l.host, &l.subject_id, "someone-else", &k).proof;
        assert!(matches!(offline_check(&bad, ctx(), now()), Err(LinkStatus::Invalid { .. })));
        assert!(matches!(f.resolve_handle(&l.provider, &l.host, "dave").await, Err(FetchError::NotConfigured(_))));
    }
    assert_eq!(forge.hits(), 0, "no forge is asked about a dropped kind");
}

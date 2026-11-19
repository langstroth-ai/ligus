//! End-to-end encryption for the workspace key — the sealing half of the crypto
//! story (the signing half is in `crypto`).
//!
//! Each device has an X25519 **key-agreement** keypair (distinct from its
//! Ed25519 signing key). The symmetric workspace key is sealed *per device*
//! using an HPKE-base-style scheme: ephemeral X25519 → ECDH → HKDF-SHA256 →
//! ChaCha20-Poly1305. The relay only ever forwards ciphertext; only a holder of
//! a recipient device's secret can open the seal. Key **rotation** issues a new
//! version sealed only for the still-trusted devices, so a removed device — even
//! with old blobs — cannot read anything sealed after its removal.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

use crate::crypto::CryptoError;

// FROZEN cryptographic constant. Do NOT rename with the product (Ligus -> Ligus):
// changing the HKDF info breaks every sealed workspace key already on disk or on
// a relay. Pinned by `crypto_constants_are_frozen` below.
const HKDF_INFO: &[u8] = b"hive-workspace-key-seal-v1";
// FROZEN: the Argon2id salt for passphrase-derived workspace keys. Changing it
// re-derives every passphrase workspace key, so every existing encrypted
// workspace becomes unreadable. Never rename it with the product.
const ARGON2_SALT: &[u8] = b"hive-workspace-key-argon2id";

/// An X25519 key-agreement keypair for receiving sealed workspace keys.
pub struct KeyAgreementKeypair {
    secret: StaticSecret,
}

impl KeyAgreementKeypair {
    pub fn generate() -> Result<Self, CryptoError> {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|_| CryptoError::Random)?;
        Ok(Self {
            secret: StaticSecret::from(seed),
        })
    }

    pub fn from_seed(seed: &[u8]) -> Result<Self, CryptoError> {
        let seed: [u8; 32] = seed.try_into().map_err(|_| CryptoError::BadKey)?;
        Ok(Self {
            secret: StaticSecret::from(seed),
        })
    }

    pub fn seed_bytes(&self) -> [u8; 32] {
        self.secret.to_bytes()
    }

    pub fn public_key_bytes(&self) -> [u8; 32] {
        PublicKey::from(&self.secret).to_bytes()
    }
}

/// A workspace key sealed for one recipient device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SealedBlob {
    /// Ephemeral X25519 public key for this seal.
    pub ephemeral_public: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

/// Derive a 32-byte key + 12-byte nonce from the ECDH shared secret, bound to
/// both public keys so the same shared secret can't be replayed across pairs.
fn derive_key_nonce(shared: &[u8], ephemeral_pub: &[u8], recipient_pub: &[u8]) -> ([u8; 32], [u8; 12]) {
    let mut salt = Vec::with_capacity(64);
    salt.extend_from_slice(ephemeral_pub);
    salt.extend_from_slice(recipient_pub);
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared);
    let mut okm = [0u8; 44];
    hk.expand(HKDF_INFO, &mut okm).expect("hkdf expand");
    let mut key = [0u8; 32];
    let mut nonce = [0u8; 12];
    key.copy_from_slice(&okm[..32]);
    nonce.copy_from_slice(&okm[32..]);
    (key, nonce)
}

/// Seal `plaintext` (the workspace key) for `recipient_public` (an X25519
/// public key).
///
/// A low-order recipient key (one that forces the all-zero shared secret,
/// which anyone can compute) is refused with [`CryptoError::BadKey`], as
/// [`open`] refuses a low-order ephemeral key: the same check the tunnel's
/// Noise handshake makes on every DH output.
pub fn seal(recipient_public: &[u8], plaintext: &[u8]) -> Result<SealedBlob, CryptoError> {
    let recipient: [u8; 32] = recipient_public.try_into().map_err(|_| CryptoError::BadKey)?;
    let recipient_pk = PublicKey::from(recipient);

    let mut eph_seed = [0u8; 32];
    getrandom::fill(&mut eph_seed).map_err(|_| CryptoError::Random)?;
    let ephemeral = StaticSecret::from(eph_seed);
    let ephemeral_pub = PublicKey::from(&ephemeral).to_bytes();

    let shared = ephemeral.diffie_hellman(&recipient_pk);
    if !shared.was_contributory() {
        return Err(CryptoError::BadKey);
    }
    let (key, nonce) = derive_key_nonce(shared.as_bytes(), &ephemeral_pub, &recipient);

    let cipher = ChaCha20Poly1305::new(&Key::from(key));
    let ciphertext = cipher
        .encrypt(&Nonce::from(nonce), plaintext)
        .map_err(|_| CryptoError::VerifyFailed)?;

    Ok(SealedBlob {
        ephemeral_public: ephemeral_pub.to_vec(),
        ciphertext,
    })
}

/// Open a [`SealedBlob`] with the recipient device's keypair.
pub fn open(recipient: &KeyAgreementKeypair, blob: &SealedBlob) -> Result<Vec<u8>, CryptoError> {
    let eph: [u8; 32] = blob
        .ephemeral_public
        .as_slice()
        .try_into()
        .map_err(|_| CryptoError::BadKey)?;
    let ephemeral_pub = PublicKey::from(eph);
    let recipient_pub = recipient.public_key_bytes();

    let shared = recipient.secret.diffie_hellman(&ephemeral_pub);
    if !shared.was_contributory() {
        return Err(CryptoError::BadKey);
    }
    let (key, nonce) = derive_key_nonce(shared.as_bytes(), &eph, &recipient_pub);

    let cipher = ChaCha20Poly1305::new(&Key::from(key));
    cipher
        .decrypt(&Nonce::from(nonce), blob.ciphertext.as_ref())
        .map_err(|_| CryptoError::VerifyFailed)
}

/// A versioned symmetric workspace key, sealed to each trusted device.
///
/// **Forward-secrecy limitation (by design, documented — P3-3):** the key is
/// sealed to each device's *long-term static* X25519 key, and within an epoch all
/// events share one symmetric key (no per-message ratchet). So compromising a
/// device's static secret later opens every past rotation sealed to it, and
/// compromising one epoch key exposes every event in that epoch. This is an
/// accepted trade for the product requirement that any member can decrypt history
/// at rest on any of their devices. A ratcheting group-key scheme (MLS /
/// double-ratchet) would add forward secrecy + cheaper membership churn, at the
/// cost of that at-rest-history property — tracked as a possible future direction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceKeyRotation {
    pub version: u32,
    /// device id → the workspace key sealed for that device's X25519 key.
    pub sealed: std::collections::BTreeMap<String, SealedBlob>,
}

/// Generate a fresh 32-byte symmetric workspace key.
pub fn generate_workspace_key() -> Result<[u8; 32], CryptoError> {
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|_| CryptoError::Random)?;
    Ok(key)
}

/// Derive a stable 32-byte workspace key from a shared passphrase (Argon2id).
/// Lets peers agree on a key out-of-band without exchanging raw bytes — the
/// relay-forwarding E2EE path.
///
/// Argon2id (memory-hard), NOT HKDF: a human-chosen room passphrase must resist an
/// offline dictionary attack by anyone holding ciphertext (the content-blind relay,
/// or any reader on an open relay). HKDF is a cheap hash — millions of guesses/sec;
/// Argon2id makes each guess cost real memory + time. The salt is a fixed
/// application constant because both peers must derive the same key from the
/// passphrase alone; the memory/time cost is the dominant defense (per-workspace
/// salting would only stop one precomputed table spanning many workspaces — a
/// marginal add over the memory-hardness, and left as a possible follow-up).
pub fn derive_workspace_key(passphrase: &str) -> [u8; 32] {
    use argon2::{Algorithm, Argon2, Params, Version};
    // ~19 MiB, 2 passes, 1 lane (OWASP-recommended floor), 32-byte output.
    let params = Params::new(19 * 1024, 2, 1, Some(32)).expect("valid argon2 params");
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    argon
        .hash_password_into(passphrase.as_bytes(), ARGON2_SALT, &mut key)
        .expect("argon2 derive");
    key
}

/// A symmetrically-sealed blob: what travels over the wire so the relay only
/// ever sees ciphertext. Sealed with the workspace key (ChaCha20-Poly1305) and
/// a fresh random nonce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SealedEnvelope {
    /// Key epoch this ciphertext was sealed under — the [`WorkspaceKeyRotation`]
    /// version, or `0` for the base/passphrase key. A client keyed with multiple
    /// epochs uses this to pick the right key, so a rotation no longer strands
    /// history: events sealed under an older epoch stay readable. Absent on
    /// pre-epoch bodies (`#[serde(default)]` → 0).
    #[serde(default)]
    pub version: u32,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    /// The sealing key's id (Phase 0, B1): the first 16 hex characters of its
    /// commitment (see [`crate::keys::kid_of`]). Picks the key when two keys
    /// share a version (a rotation that lost a tie). Absent on older bodies,
    /// which a reader opens with every key it holds for `version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kid: Option<String>,
}

/// Seal arbitrary bytes (a serialized envelope) with the workspace key for epoch
/// `version` (stamped onto the output so a keyed reader can pick the right key).
pub fn seal_symmetric(
    key: &[u8; 32],
    version: u32,
    plaintext: &[u8],
) -> Result<SealedEnvelope, CryptoError> {
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|_| CryptoError::Random)?;
    let cipher = ChaCha20Poly1305::new(&Key::from(*key));
    let ciphertext = cipher
        .encrypt(&Nonce::from(nonce), plaintext)
        .map_err(|_| CryptoError::VerifyFailed)?;
    Ok(SealedEnvelope {
        version,
        nonce: nonce.to_vec(),
        ciphertext,
        kid: None,
    })
}

/// [`seal_symmetric`], stamping the sealing key's id (see [`SealedEnvelope::kid`]).
pub fn seal_symmetric_kid(
    key: &[u8; 32],
    version: u32,
    kid: &str,
    plaintext: &[u8],
) -> Result<SealedEnvelope, CryptoError> {
    let mut sealed = seal_symmetric(key, version, plaintext)?;
    sealed.kid = Some(kid.to_string());
    Ok(sealed)
}

/// Open a [`SealedEnvelope`] with the workspace key. Also authenticates: only a
/// holder of the key could have produced openable ciphertext.
pub fn open_symmetric(key: &[u8; 32], sealed: &SealedEnvelope) -> Result<Vec<u8>, CryptoError> {
    if sealed.nonce.len() != 12 {
        return Err(CryptoError::BadKey);
    }
    let nonce = Nonce::try_from(sealed.nonce.as_slice()).map_err(|_| CryptoError::BadKey)?;
    let cipher = ChaCha20Poly1305::new(&Key::from(*key));
    cipher
        .decrypt(&nonce, sealed.ciphertext.as_ref())
        .map_err(|_| CryptoError::VerifyFailed)
}

impl WorkspaceKeyRotation {
    /// Seal `key` for each `(device_id, x25519_public)` recipient.
    pub fn seal_for_devices(
        version: u32,
        key: &[u8],
        recipients: &[(String, Vec<u8>)],
    ) -> Result<Self, CryptoError> {
        let mut sealed = std::collections::BTreeMap::new();
        for (device_id, public) in recipients {
            sealed.insert(device_id.clone(), seal(public, key)?);
        }
        Ok(Self { version, sealed })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip() {
        let device = KeyAgreementKeypair::generate().unwrap();
        let secret = b"the-32-byte-symmetric-workspace!"; // 32 bytes
        let blob = seal(&device.public_key_bytes(), secret).unwrap();
        let opened = open(&device, &blob).unwrap();
        assert_eq!(opened, secret);
    }

    /// Low-order X25519 points (RFC 7748 §6.1; the all-zero shared secret
    /// they force is what the tunnel's Noise `CheckedResolver` refuses).
    const LOW_ORDER: [&str; 4] = [
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0100000000000000000000000000000000000000000000000000000000000000",
        "e0eb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b800",
        "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
    ];

    fn unhex(s: &str) -> [u8; 32] {
        let v: Vec<u8> = (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect();
        v.try_into().unwrap()
    }

    #[test]
    fn seal_refuses_a_low_order_recipient() {
        for p in LOW_ORDER {
            assert!(seal(&unhex(p), b"workspace-key-payload-1234567890").is_err(), "sealed to low-order point {p}");
        }
    }

    /// Anyone can compute the all-zero shared secret a low-order ephemeral
    /// forces, so such a blob is forgeable without the recipient's key:
    /// `open` must refuse it rather than authenticate it.
    #[test]
    fn open_refuses_a_low_order_ephemeral() {
        let device = KeyAgreementKeypair::generate().unwrap();
        let recipient = device.public_key_bytes();
        for p in LOW_ORDER {
            let eph = unhex(p);
            let (key, nonce) = derive_key_nonce(&[0u8; 32], &eph, &recipient);
            let ciphertext = ChaCha20Poly1305::new(&Key::from(key)).encrypt(&Nonce::from(nonce), &b"forged"[..]).unwrap();
            let blob = SealedBlob { ephemeral_public: eph.to_vec(), ciphertext };
            assert!(open(&device, &blob).is_err(), "opened a blob with low-order ephemeral {p}");
        }
    }

    #[test]
    fn wrong_device_cannot_open() {
        let alice = KeyAgreementKeypair::generate().unwrap();
        let bob = KeyAgreementKeypair::generate().unwrap();
        let blob = seal(&alice.public_key_bytes(), b"workspace-key-payload-1234567890").unwrap();
        assert!(open(&bob, &blob).is_err());
    }

    #[test]
    fn symmetric_seal_open_roundtrip_and_tamper() {
        let key = derive_workspace_key("team-alpha secret");
        let plaintext = br#"{"kind":"messageAppended"}"#;
        let sealed = seal_symmetric(&key, 0, plaintext).unwrap();
        assert_ne!(sealed.ciphertext, plaintext);
        assert_eq!(open_symmetric(&key, &sealed).unwrap(), plaintext);

        // wrong key fails
        let other = derive_workspace_key("different");
        assert!(open_symmetric(&other, &sealed).is_err());

        // tampered ciphertext fails (AEAD)
        let mut bad = sealed.clone();
        bad.ciphertext[0] ^= 0xff;
        assert!(open_symmetric(&key, &bad).is_err());
    }

    #[test]
    fn sealed_envelope_stamps_and_defaults_epoch() {
        let key = derive_workspace_key("epoch test");
        let sealed = seal_symmetric(&key, 3, b"payload").unwrap();
        assert_eq!(sealed.version, 3, "epoch is stamped on the sealed body");

        // A legacy pre-epoch body (no `version`) deserializes to epoch 0.
        let legacy = serde_json::json!({ "nonce": sealed.nonce, "ciphertext": sealed.ciphertext });
        let parsed: SealedEnvelope = serde_json::from_value(legacy).unwrap();
        assert_eq!(parsed.version, 0, "missing version defaults to base epoch");
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Pins the exact bytes of the E2EE domain-separation constants, written as
    /// hex so a textual find-and-replace of the product name cannot also
    /// "update" the expectation, plus known-answer vectors for the derivations
    /// that consume them. If this fails, a rename touched a frozen constant:
    /// revert it. Changing these makes every existing workspace unreadable.
    #[test]
    fn crypto_constants_are_frozen() {
        // b"hive-workspace-key-seal-v1"
        assert_eq!(hex(HKDF_INFO), "686976652d776f726b73706163652d6b65792d7365616c2d7631");
        // b"hive-workspace-key-argon2id"
        assert_eq!(hex(ARGON2_SALT), "686976652d776f726b73706163652d6b65792d6172676f6e326964");
        assert_eq!(hex(&derive_workspace_key("pinned passphrase")), "60cc65fc19c112e3699959d5a56a708fc6ac23f4362d761718dfd7d3067f3ec2");
        let (key, nonce) = derive_key_nonce(&[7u8; 32], &[1u8; 32], &[2u8; 32]);
        assert_eq!(hex(&key), "2362b77b54fa70c7b4e1c7db9cb7efdefacc3840aaeee8cd64fd4a0d8f52d58f");
        assert_eq!(hex(&nonce), "642b977119bafd3df8edc1f0");
    }

    #[test]
    fn derive_workspace_key_is_stable() {
        assert_eq!(derive_workspace_key("x"), derive_workspace_key("x"));
        assert_ne!(derive_workspace_key("x"), derive_workspace_key("y"));
    }

    #[test]
    fn rotation_excludes_removed_device() {
        let alice = KeyAgreementKeypair::generate().unwrap();
        let bob = KeyAgreementKeypair::generate().unwrap();
        let key_v1 = generate_workspace_key().unwrap();

        // v1 sealed for both
        let v1 = WorkspaceKeyRotation::seal_for_devices(
            1,
            &key_v1,
            &[
                ("alice".into(), alice.public_key_bytes().to_vec()),
                ("bob".into(), bob.public_key_bytes().to_vec()),
            ],
        )
        .unwrap();
        assert_eq!(open(&bob, &v1.sealed["bob"]).unwrap(), key_v1);

        // rotate: new key sealed only for alice (bob removed)
        let key_v2 = generate_workspace_key().unwrap();
        let v2 = WorkspaceKeyRotation::seal_for_devices(
            2,
            &key_v2,
            &[("alice".into(), alice.public_key_bytes().to_vec())],
        )
        .unwrap();
        assert!(v2.sealed.get("bob").is_none(), "bob must not receive v2");
        assert_eq!(open(&alice, &v2.sealed["alice"]).unwrap(), key_v2);
        assert_ne!(key_v1, key_v2);
    }
}

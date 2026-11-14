//! Human presence (Phase 0, B3): WebAuthn passkey assertions over the event
//! digest, verified offline on every peer inside the fold.
//!
//! An event that needs a human (membership, keys, guards, zones, workflow
//! definitions, agent hosts, gates, human-gated votes; see
//! [`crate::authz::rules::requires_presence`]) carries a [`PresenceAssertion`]
//! in its actor stamp. The stamp is inside the v3 signature preimage, so the
//! assertion can't be stripped or moved without breaking the device
//! signature. The WebAuthn challenge is the event digest ([`challenge`]),
//! which excludes the stamp itself, so there is no circularity.
//!
//! * Relying party `ligus.ai`; ES256 (COSE -7) and EdDSA (COSE -8); any
//!   authenticator (synced iCloud/Google passkeys, Windows Hello, security
//!   keys). User presence **and** user verification are required.
//! * `signCount` is not enforced: synced passkeys report 0, and a monotonic
//!   counter would brick them.
//! * A recovery key (24 BIP-39 words, Ed25519) is a credential with
//!   `recovery: true`; its assertion is `Ed25519(sk, "ligus-recovery-v1\0" ||
//!   challenge)` with empty authenticator data and client data.

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::events::SessionEventEnvelope;
use crate::time_util::Timestamp;

/// Domain string of the presence challenge (added by Phase 0; never change).
pub const PRESENCE_CONTEXT: &[u8] = b"ligus-presence-v1\0";
/// Domain string of a recovery-key assertion (added by Phase 0; never change).
pub const RECOVERY_CONTEXT: &[u8] = b"ligus-recovery-v1\0";
/// The WebAuthn relying party.
pub const RP_ID: &str = "ligus.ai";
/// Origins a `clientDataJSON` may name (the hand-off page and the iOS app,
/// which builds its client data with this origin).
pub const DEFAULT_ORIGINS: &[&str] = &["https://ligus.ai"];
/// COSE algorithm ids.
pub const ES256: i32 = -7;
pub const EDDSA: i32 = -8;
/// The serialized presence (and a registration proof) is at most this big.
pub const MAX_PRESENCE_BYTES: usize = 4096;
/// Largest credential id (decoded).
pub const MAX_CREDENTIAL_ID: usize = 1023;

/// A registered passkey (or recovery key) of one account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyCredential {
    pub account_id: Uuid,
    /// base64url, at most 1023 bytes decoded.
    pub credential_id: String,
    /// -7 (ES256) or -8 (EdDSA).
    pub alg: i32,
    /// ES256: SEC1 uncompressed (65 bytes); EdDSA: 32 bytes.
    pub public_key: Vec<u8>,
    /// "MacBook Touch ID", "YubiKey 5C" (at most 64 characters).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// An offline recovery key (alg -8, no WebAuthn wrapper).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recovery: bool,
    pub registered_at: Timestamp,
}

/// A WebAuthn assertion (or recovery signature) carried in the actor stamp.
/// For a registration's own proof, `authenticator_data` holds the whole
/// `attestationObject` and `signature` is empty (see [`verify_create`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceAssertion {
    /// base64url.
    pub credential_id: String,
    /// base64url (empty for a recovery key).
    pub authenticator_data: String,
    /// base64url (empty for a recovery key).
    pub client_data_json: String,
    /// base64url (ES256: DER; EdDSA: 64 bytes).
    pub signature: String,
}

/// The part of a credential verification needs (kept in the roster).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PasskeyKey {
    pub alg: i32,
    pub public_key: Vec<u8>,
    pub recovery: bool,
}

impl From<&PasskeyCredential> for PasskeyKey {
    fn from(c: &PasskeyCredential) -> Self {
        Self { alg: c.alg, public_key: c.public_key.clone(), recovery: c.recovery }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PresenceError(pub String);

fn err(m: impl Into<String>) -> PresenceError {
    PresenceError(m.into())
}

pub fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn unb64url(s: &str) -> Result<Vec<u8>, PresenceError> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .map_err(|_| err("not base64url"))
}

/// What a verifier accepts (production: [`Policy::ligus`]).
#[derive(Debug, Clone)]
pub struct Policy<'a> {
    pub rp_id: &'a str,
    pub origins: &'a [&'a str],
    pub require_uv: bool,
}

impl Policy<'static> {
    pub const fn ligus() -> Self {
        Self { rp_id: RP_ID, origins: DEFAULT_ORIGINS, require_uv: true }
    }
}

// ---------------------------------------------------------------------------
// The challenge (B.4.3)
// ---------------------------------------------------------------------------

/// The event digest a presence assertion signs:
/// `SHA-256("ligus-presence-v1\0" || workspace_id || session_id || event_id ||
/// u64le(lamport) || u32le(len) || payload || u32le(len) || cfg_frontier_json
/// || signer_account_id)`. The payload is the bytes received (or, for an event
/// authored here, its serialization); the frontier is `""` when absent; the
/// account is the stamped account (nil when none).
pub fn challenge(env: &SessionEventEnvelope) -> [u8; 32] {
    let payload = match env.raw() {
        Some(r) => r.payload().as_bytes().to_vec(),
        None => serde_json::to_vec(&env.payload).expect("serialize payload"),
    };
    let stamp = env.actor_stamp.as_ref();
    let frontier = stamp
        .and_then(|s| s.cfg_frontier)
        .map(|f| serde_json::to_vec(&f).expect("serialize frontier"))
        .unwrap_or_default();
    let account = stamp.and_then(|s| s.actor.account_id).unwrap_or_else(Uuid::nil);
    let mut h = Sha256::new();
    h.update(PRESENCE_CONTEXT);
    h.update(env.workspace_id.as_bytes());
    h.update(env.session_id.as_bytes());
    h.update(env.event_id.as_bytes());
    h.update(env.lamport.to_le_bytes());
    h.update((payload.len() as u32).to_le_bytes());
    h.update(&payload);
    h.update((frontier.len() as u32).to_le_bytes());
    h.update(&frontier);
    h.update(account.as_bytes());
    h.finalize().into()
}

// ---------------------------------------------------------------------------
// A small CBOR reader (attestation objects and COSE keys only)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Cbor {
    Int(i128),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Cbor>),
    Map(Vec<(Cbor, Cbor)>),
    Simple(u8),
}

impl Cbor {
    fn get(&self, key: &Cbor) -> Option<&Cbor> {
        match self {
            Cbor::Map(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn int(&self) -> Option<i128> {
        if let Cbor::Int(i) = self {
            Some(*i)
        } else {
            None
        }
    }
    fn bytes(&self) -> Option<&[u8]> {
        if let Cbor::Bytes(b) = self {
            Some(b)
        } else {
            None
        }
    }
}

/// Decode one CBOR item from the front of `data`: (item, bytes consumed).
/// Definite lengths only, nesting depth at most 16.
pub fn cbor_decode(data: &[u8]) -> Result<(Cbor, usize), PresenceError> {
    fn arg(data: &[u8], pos: &mut usize, info: u8) -> Result<u64, PresenceError> {
        let take = |pos: &mut usize, n: usize| -> Result<u64, PresenceError> {
            let s = data.get(*pos..*pos + n).ok_or_else(|| err("truncated CBOR"))?;
            *pos += n;
            Ok(s.iter().fold(0u64, |a, b| (a << 8) | *b as u64))
        };
        match info {
            0..=23 => Ok(info as u64),
            24 => take(pos, 1),
            25 => take(pos, 2),
            26 => take(pos, 4),
            27 => take(pos, 8),
            _ => Err(err("unsupported CBOR length")),
        }
    }
    fn item(data: &[u8], pos: &mut usize, depth: u8) -> Result<Cbor, PresenceError> {
        if depth > 16 {
            return Err(err("CBOR nested too deep"));
        }
        let b = *data.get(*pos).ok_or_else(|| err("truncated CBOR"))?;
        *pos += 1;
        let (major, info) = (b >> 5, b & 0x1f);
        let n = if major == 7 { info as u64 } else { arg(data, pos, info)? };
        let len = |n: u64| -> Result<usize, PresenceError> {
            let n = usize::try_from(n).map_err(|_| err("CBOR length"))?;
            if n > data.len() {
                return Err(err("CBOR length past the end"));
            }
            Ok(n)
        };
        Ok(match major {
            0 => Cbor::Int(n as i128),
            1 => Cbor::Int(-1 - n as i128),
            2 | 3 => {
                let n = len(n)?;
                let s = data.get(*pos..*pos + n).ok_or_else(|| err("truncated CBOR"))?.to_vec();
                *pos += n;
                if major == 2 {
                    Cbor::Bytes(s)
                } else {
                    Cbor::Text(String::from_utf8(s).map_err(|_| err("CBOR text isn't UTF-8"))?)
                }
            }
            4 => {
                let n = len(n)?;
                let mut v = Vec::with_capacity(n.min(64));
                for _ in 0..n {
                    v.push(item(data, pos, depth + 1)?);
                }
                Cbor::Array(v)
            }
            5 => {
                let n = len(n)?;
                let mut v = Vec::with_capacity(n.min(64));
                for _ in 0..n {
                    let k = item(data, pos, depth + 1)?;
                    let val = item(data, pos, depth + 1)?;
                    v.push((k, val));
                }
                Cbor::Map(v)
            }
            6 => item(data, pos, depth + 1)?, // a tag: its content
            _ => match info {
                20..=23 => Cbor::Simple(info),
                _ => return Err(err("unsupported CBOR simple value")),
            },
        })
    }
    let mut pos = 0;
    let v = item(data, &mut pos, 0)?;
    Ok((v, pos))
}

/// The algorithm and raw public key of a COSE key (EC2 P-256 or OKP Ed25519).
pub fn cose_public_key(key: &Cbor) -> Result<(i32, Vec<u8>), PresenceError> {
    let kty = key.get(&Cbor::Int(1)).and_then(Cbor::int).ok_or_else(|| err("COSE key without kty"))?;
    let alg = key.get(&Cbor::Int(3)).and_then(Cbor::int).ok_or_else(|| err("COSE key without alg"))?;
    let crv = key.get(&Cbor::Int(-1)).and_then(Cbor::int);
    let x = key.get(&Cbor::Int(-2)).and_then(Cbor::bytes);
    match (kty, alg) {
        (2, -7) => {
            let y = key.get(&Cbor::Int(-3)).and_then(Cbor::bytes);
            let (Some(x), Some(y), Some(1)) = (x, y, crv) else { return Err(err("bad P-256 COSE key")) };
            if x.len() != 32 || y.len() != 32 {
                return Err(err("bad P-256 coordinates"));
            }
            let mut pk = vec![0x04];
            pk.extend_from_slice(x);
            pk.extend_from_slice(y);
            p256::ecdsa::VerifyingKey::from_sec1_bytes(&pk).map_err(|_| err("P-256 point not on the curve"))?;
            Ok((ES256, pk))
        }
        (1, -8) => {
            let (Some(x), Some(6)) = (x, crv) else { return Err(err("bad Ed25519 COSE key")) };
            if x.len() != 32 {
                return Err(err("bad Ed25519 key length"));
            }
            Ok((EDDSA, x.to_vec()))
        }
        _ => Err(err(format!("unsupported COSE key (kty {kty}, alg {alg}): ES256 or EdDSA only"))),
    }
}

// ---------------------------------------------------------------------------
// Authenticator data and client data
// ---------------------------------------------------------------------------

/// Parsed authenticator data.
#[derive(Debug, Clone)]
pub struct AuthData {
    pub rp_id_hash: [u8; 32],
    pub flags: u8,
    pub sign_count: u32,
    /// (credential id, COSE public key) when attested credential data is present.
    pub attested: Option<(Vec<u8>, Cbor)>,
}

pub const FLAG_UP: u8 = 0x01;
pub const FLAG_UV: u8 = 0x04;
const FLAG_AT: u8 = 0x40;

pub fn parse_auth_data(data: &[u8]) -> Result<AuthData, PresenceError> {
    if data.len() < 37 {
        return Err(err("authenticator data too short"));
    }
    let mut rp_id_hash = [0u8; 32];
    rp_id_hash.copy_from_slice(&data[..32]);
    let flags = data[32];
    let sign_count = u32::from_be_bytes([data[33], data[34], data[35], data[36]]);
    let mut attested = None;
    if flags & FLAG_AT != 0 {
        let rest = &data[37..];
        if rest.len() < 18 {
            return Err(err("attested credential data too short"));
        }
        let id_len = u16::from_be_bytes([rest[16], rest[17]]) as usize;
        let id = rest.get(18..18 + id_len).ok_or_else(|| err("credential id past the end"))?.to_vec();
        let (key, _) = cbor_decode(&rest[18 + id_len..])?;
        attested = Some((id, key));
    }
    Ok(AuthData { rp_id_hash, flags, sign_count, attested })
}

#[derive(Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    ty: String,
    challenge: String,
    origin: String,
    #[serde(default, rename = "crossOrigin")]
    cross_origin: Option<bool>,
}

fn check_client_data(raw: &[u8], ty: &str, challenge: &[u8], policy: &Policy<'_>) -> Result<(), PresenceError> {
    let cd: ClientData = serde_json::from_slice(raw).map_err(|_| err("clientDataJSON isn't the expected JSON"))?;
    if cd.ty != ty {
        return Err(err(format!("clientDataJSON type is {}, expected {ty}", cd.ty)));
    }
    if unb64url(&cd.challenge)? != challenge {
        return Err(err("the assertion signs a different challenge"));
    }
    if !policy.origins.contains(&cd.origin.as_str()) {
        return Err(err(format!("origin {} isn't allowed", cd.origin)));
    }
    if cd.cross_origin == Some(true) {
        return Err(err("cross-origin ceremony"));
    }
    Ok(())
}

fn check_rp_and_flags(ad: &AuthData, policy: &Policy<'_>) -> Result<(), PresenceError> {
    let want: [u8; 32] = Sha256::digest(policy.rp_id.as_bytes()).into();
    if ad.rp_id_hash != want {
        return Err(err("authenticator data is for another relying party"));
    }
    if ad.flags & FLAG_UP == 0 {
        return Err(err("user presence (UP) not set"));
    }
    if policy.require_uv && ad.flags & FLAG_UV == 0 {
        return Err(err("user verification (UV) not set"));
    }
    Ok(())
}

/// Verify a signature by a credential key.
pub fn verify_signature(alg: i32, public_key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), PresenceError> {
    match alg {
        ES256 => {
            use p256::ecdsa::signature::Verifier;
            let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(public_key).map_err(|_| err("bad ES256 key"))?;
            let sig = p256::ecdsa::Signature::from_der(signature).map_err(|_| err("bad ES256 signature encoding"))?;
            key.verify(message, &sig).map_err(|_| err("ES256 signature doesn't verify"))
        }
        EDDSA => crate::crypto::verify(public_key, message, signature).map_err(|_| err("EdDSA signature doesn't verify")),
        other => Err(err(format!("unsupported algorithm {other}"))),
    }
}

/// Verify a `webauthn.get` assertion by `key` over `challenge`.
pub fn verify_get(a: &PresenceAssertion, key: &PasskeyKey, challenge: &[u8], policy: &Policy<'_>) -> Result<(), PresenceError> {
    if key.recovery {
        return verify_recovery(a, key, challenge);
    }
    let auth_data = unb64url(&a.authenticator_data)?;
    let client_data = unb64url(&a.client_data_json)?;
    let signature = unb64url(&a.signature)?;
    let ad = parse_auth_data(&auth_data)?;
    check_rp_and_flags(&ad, policy)?;
    check_client_data(&client_data, "webauthn.get", challenge, policy)?;
    let mut msg = auth_data;
    msg.extend_from_slice(&Sha256::digest(&client_data));
    verify_signature(key.alg, &key.public_key, &msg, &signature)
}

/// A recovery-key assertion: `Ed25519(sk, "ligus-recovery-v1\0" || challenge)`.
pub fn verify_recovery(a: &PresenceAssertion, key: &PasskeyKey, challenge: &[u8]) -> Result<(), PresenceError> {
    if !key.recovery || key.alg != EDDSA {
        return Err(err("not a recovery key"));
    }
    if !a.authenticator_data.is_empty() || !a.client_data_json.is_empty() {
        return Err(err("a recovery assertion carries no WebAuthn data"));
    }
    let mut msg = RECOVERY_CONTEXT.to_vec();
    msg.extend_from_slice(challenge);
    verify_signature(EDDSA, &key.public_key, &msg, &unb64url(&a.signature)?)
}

/// Verify a registration's own proof (`webauthn.create` over the registration
/// event's digest; `authenticator_data` is the `attestationObject`, whose
/// statement is not checked: Ligus asks for `attestation: "none"`). Returns the
/// attested credential: (credential id, alg, public key).
pub fn verify_create(a: &PresenceAssertion, challenge: &[u8], policy: &Policy<'_>) -> Result<(Vec<u8>, i32, Vec<u8>), PresenceError> {
    let att = unb64url(&a.authenticator_data)?;
    let client_data = unb64url(&a.client_data_json)?;
    let (obj, _) = cbor_decode(&att)?;
    let auth_data = obj
        .get(&Cbor::Text("authData".into()))
        .and_then(Cbor::bytes)
        .ok_or_else(|| err("attestationObject without authData"))?;
    let ad = parse_auth_data(auth_data)?;
    check_rp_and_flags(&ad, policy)?;
    check_client_data(&client_data, "webauthn.create", challenge, policy)?;
    let (id, key) = ad.attested.ok_or_else(|| err("no attested credential"))?;
    if unb64url(&a.credential_id)? != id {
        return Err(err("the attested credential isn't the one named"));
    }
    let (alg, pk) = cose_public_key(&key)?;
    Ok((id, alg, pk))
}

/// Whether a credential's own fields are well formed.
pub fn credential_well_formed(c: &PasskeyCredential) -> Result<(), PresenceError> {
    let id = unb64url(&c.credential_id)?;
    if id.is_empty() || id.len() > MAX_CREDENTIAL_ID {
        return Err(err("credential id length"));
    }
    if c.label.chars().count() > 64 {
        return Err(err("label longer than 64 characters"));
    }
    match (c.alg, c.public_key.len(), c.recovery) {
        (ES256, 65, false) => {
            p256::ecdsa::VerifyingKey::from_sec1_bytes(&c.public_key).map_err(|_| err("bad ES256 key"))?;
        }
        (EDDSA, 32, _) => {}
        _ => return Err(err("credential: ES256 (65-byte SEC1 key) or EdDSA (32 bytes); a recovery key is EdDSA")),
    }
    Ok(())
}

/// The credential id of a recovery key: base64url of its public key.
pub fn recovery_credential_id(public_key: &[u8; 32]) -> String {
    b64url(public_key)
}

/// The stamped presence of `env`, if any, within the size cap.
pub fn presence_of(env: &SessionEventEnvelope) -> Result<Option<&PresenceAssertion>, PresenceError> {
    let Some(p) = env.actor_stamp.as_ref().and_then(|s| s.presence.as_ref()) else { return Ok(None) };
    let size = serde_json::to_vec(p).map(|v| v.len()).unwrap_or(usize::MAX);
    if size > MAX_PRESENCE_BYTES {
        return Err(err("presence larger than 4 KiB"));
    }
    Ok(Some(p))
}

// ---------------------------------------------------------------------------
// Recovery key words (BIP-39, 24 words = 32 bytes of entropy)
// ---------------------------------------------------------------------------

/// A recovery key: an Ed25519 key whose seed is 32 bytes of BIP-39 entropy.
pub struct RecoveryKey {
    seed: [u8; 32],
}

impl RecoveryKey {
    pub fn generate() -> Result<Self, PresenceError> {
        let mut seed = [0u8; 32];
        getrandom::getrandom(&mut seed).map_err(|_| err("random source"))?;
        Ok(Self { seed })
    }
    /// The 24 words (shown once).
    pub fn words(&self) -> String {
        bip39::Mnemonic::from_entropy(&self.seed).expect("32 bytes of entropy").to_string()
    }
    /// From the 24 words (checksum verified).
    pub fn from_words(words: &str) -> Result<Self, PresenceError> {
        let m = bip39::Mnemonic::parse_normalized(&words.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase())
            .map_err(|_| err("those aren't 24 valid recovery words"))?;
        let e = m.to_entropy();
        let seed: [u8; 32] = e.as_slice().try_into().map_err(|_| err("a recovery key is 24 words"))?;
        Ok(Self { seed })
    }
    fn keypair(&self) -> crate::crypto::SigningKeypair {
        crate::crypto::SigningKeypair::from_seed(&self.seed).expect("32-byte seed")
    }
    pub fn public_key(&self) -> [u8; 32] {
        self.keypair().public_key_bytes()
    }
    /// The credential to register for `account_id`.
    pub fn credential(&self, account_id: Uuid, label: &str) -> PasskeyCredential {
        let pk = self.public_key();
        PasskeyCredential {
            account_id,
            credential_id: recovery_credential_id(&pk),
            alg: EDDSA,
            public_key: pk.to_vec(),
            label: label.chars().take(64).collect(),
            recovery: true,
            registered_at: Timestamp::now(),
        }
    }
    /// The assertion over `challenge`.
    pub fn assert(&self, challenge: &[u8; 32]) -> PresenceAssertion {
        let mut msg = RECOVERY_CONTEXT.to_vec();
        msg.extend_from_slice(challenge);
        PresenceAssertion {
            credential_id: recovery_credential_id(&self.public_key()),
            authenticator_data: String::new(),
            client_data_json: String::new(),
            signature: b64url(&self.keypair().sign(&msg)),
        }
    }
}

// ---------------------------------------------------------------------------
// A software authenticator (tests and the demo host)
// ---------------------------------------------------------------------------

/// A WebAuthn authenticator in software: what a platform authenticator
/// returns, for tests. Not used by any production path.
pub mod soft {
    use super::*;

    pub enum Key {
        Es256(p256::ecdsa::SigningKey),
        Ed25519(crate::crypto::SigningKeypair),
    }

    pub struct SoftAuthenticator {
        pub credential_id: Vec<u8>,
        pub key: Key,
        pub rp_id: String,
        pub origin: String,
        /// Flags to set (UP|UV by default).
        pub flags: u8,
    }

    impl SoftAuthenticator {
        pub fn es256() -> Self {
            let mut id = [0u8; 16];
            let _ = getrandom::getrandom(&mut id);
            Self {
                credential_id: id.to_vec(),
                key: Key::Es256(random_es256_key()),
                rp_id: RP_ID.into(),
                origin: DEFAULT_ORIGINS[0].into(),
                flags: FLAG_UP | FLAG_UV,
            }
        }
        pub fn ed25519() -> Self {
            Self { key: Key::Ed25519(crate::crypto::SigningKeypair::generate().unwrap()), ..Self::es256() }
        }
        pub fn alg(&self) -> i32 {
            match self.key {
                Key::Es256(_) => ES256,
                Key::Ed25519(_) => EDDSA,
            }
        }
        pub fn public_key(&self) -> Vec<u8> {
            match &self.key {
                Key::Es256(k) => k.verifying_key().to_sec1_point(false).as_bytes().to_vec(),
                Key::Ed25519(k) => k.public_key_bytes().to_vec(),
            }
        }
        pub fn credential(&self, account_id: Uuid, label: &str) -> PasskeyCredential {
            PasskeyCredential {
                account_id,
                credential_id: b64url(&self.credential_id),
                alg: self.alg(),
                public_key: self.public_key(),
                label: label.into(),
                recovery: false,
                registered_at: Timestamp::epoch(),
            }
        }
        fn auth_data(&self, attested: bool) -> Vec<u8> {
            let mut ad = Sha256::digest(self.rp_id.as_bytes()).to_vec();
            ad.push(self.flags | if attested { FLAG_AT } else { 0 });
            ad.extend_from_slice(&0u32.to_be_bytes());
            if attested {
                ad.extend_from_slice(&[0u8; 16]);
                ad.extend_from_slice(&(self.credential_id.len() as u16).to_be_bytes());
                ad.extend_from_slice(&self.credential_id);
                ad.extend_from_slice(&self.cose_key());
            }
            ad
        }
        fn cose_key(&self) -> Vec<u8> {
            match &self.key {
                Key::Es256(k) => {
                    let p = k.verifying_key().to_sec1_point(false);
                    let mut v = vec![0xa5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21, 0x58, 0x20];
                    v.extend_from_slice(p.x().unwrap());
                    v.extend_from_slice(&[0x22, 0x58, 0x20]);
                    v.extend_from_slice(p.y().unwrap());
                    v
                }
                Key::Ed25519(k) => {
                    let mut v = vec![0xa4, 0x01, 0x01, 0x03, 0x27, 0x20, 0x06, 0x21, 0x58, 0x20];
                    v.extend_from_slice(&k.public_key_bytes());
                    v
                }
            }
        }
        fn client_data(&self, ty: &str, challenge: &[u8]) -> Vec<u8> {
            format!(
                r#"{{"type":"{ty}","challenge":"{}","origin":"{}","crossOrigin":false}}"#,
                b64url(challenge),
                self.origin
            )
            .into_bytes()
        }
        fn sign(&self, msg: &[u8]) -> Vec<u8> {
            match &self.key {
                Key::Es256(k) => {
                    use p256::ecdsa::signature::Signer;
                    let s: p256::ecdsa::Signature = k.sign(msg);
                    s.to_der().as_bytes().to_vec()
                }
                Key::Ed25519(k) => k.sign(msg).to_vec(),
            }
        }
        /// `navigator.credentials.get` over `challenge`.
        pub fn get(&self, challenge: &[u8]) -> PresenceAssertion {
            let ad = self.auth_data(false);
            let cd = self.client_data("webauthn.get", challenge);
            let mut msg = ad.clone();
            msg.extend_from_slice(&Sha256::digest(&cd));
            PresenceAssertion {
                credential_id: b64url(&self.credential_id),
                authenticator_data: b64url(&ad),
                client_data_json: b64url(&cd),
                signature: b64url(&self.sign(&msg)),
            }
        }
        /// `navigator.credentials.create` over `challenge` (`attestation: "none"`).
        pub fn create(&self, challenge: &[u8]) -> PresenceAssertion {
            let ad = self.auth_data(true);
            let cd = self.client_data("webauthn.create", challenge);
            // {"fmt":"none","attStmt":{},"authData":h'…'}
            let mut att = vec![0xa3, 0x63];
            att.extend_from_slice(b"fmt");
            att.extend_from_slice(&[0x64]);
            att.extend_from_slice(b"none");
            att.extend_from_slice(&[0x67]);
            att.extend_from_slice(b"attStmt");
            att.push(0xa0);
            att.extend_from_slice(&[0x68]);
            att.extend_from_slice(b"authData");
            if ad.len() < 256 {
                att.extend_from_slice(&[0x58, ad.len() as u8]);
            } else {
                att.push(0x59);
                att.extend_from_slice(&(ad.len() as u16).to_be_bytes());
            }
            att.extend_from_slice(&ad);
            PresenceAssertion {
                credential_id: b64url(&self.credential_id),
                authenticator_data: b64url(&att),
                client_data_json: b64url(&cd),
                signature: String::new(),
            }
        }
    }

    /// A random P-256 signing key from OS randomness (rejection-sampled: a
    /// 32-byte draw is a valid scalar unless it is zero or at least the
    /// group order, which almost never happens).
    fn random_es256_key() -> p256::ecdsa::SigningKey {
        loop {
            let mut b = [0u8; 32];
            getrandom::getrandom(&mut b).expect("random");
            if let Ok(k) = p256::ecdsa::SigningKey::from_slice(&b) {
                return k;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn domain_strings_are_pinned() {
        // As hex, so a rename can't "update" the expectation.
        assert_eq!(hex(PRESENCE_CONTEXT), "6c696775732d70726573656e63652d763100");
        assert_eq!(hex(RECOVERY_CONTEXT), "6c696775732d7265636f766572792d763100");
    }

    #[test]
    fn soft_authenticator_round_trips_both_algorithms() {
        for a in [soft::SoftAuthenticator::es256(), soft::SoftAuthenticator::ed25519()] {
            let c = [7u8; 32];
            let key = PasskeyKey::from(&a.credential(Uuid::nil(), "t"));
            verify_get(&a.get(&c), &key, &c, &Policy::ligus()).unwrap();
            let (id, alg, pk) = verify_create(&a.create(&c), &c, &Policy::ligus()).unwrap();
            assert_eq!((id, alg, pk), (a.credential_id.clone(), a.alg(), a.public_key()));
            assert!(verify_get(&a.get(&[8u8; 32]), &key, &c, &Policy::ligus()).is_err(), "wrong challenge");
        }
    }

    #[test]
    fn recovery_words_round_trip() {
        let k = RecoveryKey::generate().unwrap();
        let words = k.words();
        assert_eq!(words.split_whitespace().count(), 24);
        let back = RecoveryKey::from_words(&format!("  {}  ", words.to_uppercase())).unwrap();
        assert_eq!(back.public_key(), k.public_key());
        let c = [3u8; 32];
        let cred = k.credential(Uuid::nil(), "Recovery");
        verify_recovery(&back.assert(&c), &PasskeyKey::from(&cred), &c).unwrap();
        // A word swapped breaks the checksum.
        let mut w: Vec<&str> = words.split_whitespace().collect();
        w.swap(0, 1);
        if w[0] != w[1] {
            assert!(RecoveryKey::from_words(&w.join(" ")).is_err() || RecoveryKey::from_words(&w.join(" ")).unwrap().public_key() != k.public_key());
        }
    }
}

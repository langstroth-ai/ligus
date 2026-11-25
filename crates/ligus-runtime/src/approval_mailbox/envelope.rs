//! Sealed blobs, the signed envelope and the message bodies (wire note 3).

use ligus_core::crypto::{device_id_for, SigAlg, SigningKeypair};
use ligus_core::e2ee::{KeyAgreementKeypair, SealedBlob};
use ligus_core::match_code::{self as mc, hex, MatchKind};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use uuid::Uuid;

use super::{b64, hex_fixed};
use crate::box_admission::JoinRequest;
use crate::computer_link::LinkRequest;
use crate::phone_enroll::PhoneCode;

/// How far ahead of this device's clock a request may be dated.
pub const CLOCK_SKEW_MS: i64 = 5 * 60 * 1000;
/// The longest plaintext envelope read.
const MAX_ENVELOPE: usize = 64 * 1024;

/// Why a message was refused.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum MsgError {
    #[error("the message is damaged or incomplete")]
    Malformed,
    #[error("the message's signature doesn't verify")]
    BadSignature,
    #[error("the message is a {got}, not a {want}")]
    WrongType { want: &'static str, got: &'static str },
    #[error("the message answers another request")]
    WrongRid,
    #[error("the request was read from another mailbox than it names")]
    WrongMailbox,
    #[error("the request is signed by a key its code doesn't carry")]
    WrongKey,
    #[error("the request's kind doesn't match its code")]
    KindMismatch,
    #[error("the request's key-agreement key isn't the one its code carries")]
    WrongRequesterKa,
    #[error("the request's code doesn't verify: {0}")]
    BadCode(String),
    #[error("this request is too old (over 15 minutes): make a new one")]
    Expired,
    #[error("this request is dated in the future: check the requesting device's clock")]
    FromTheFuture,
    #[error("the message's device id isn't the one its key derives")]
    WrongDeviceId,
}

/// The device key that signs a message: Ed25519 (desktops, computers, boxes)
/// or P-256 (phones).
pub enum Signer {
    Ed25519(SigningKeypair),
    P256(p256::ecdsa::SigningKey),
}

impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Signer({:?}, {})", self.alg(), self.device_id())
    }
}

impl Clone for Signer {
    fn clone(&self) -> Self {
        match self {
            Self::Ed25519(k) => Self::Ed25519(SigningKeypair::from_seed(&k.seed_bytes()).expect("32-byte seed")),
            Self::P256(k) => Self::P256(k.clone()),
        }
    }
}

impl Signer {
    pub fn alg(&self) -> SigAlg {
        match self {
            Self::Ed25519(_) => SigAlg::Ed25519,
            Self::P256(_) => SigAlg::P256,
        }
    }

    /// 32 bytes (Ed25519) or the 65-byte SEC1 uncompressed point (P-256).
    pub fn public_key(&self) -> Vec<u8> {
        match self {
            Self::Ed25519(k) => k.public_key_bytes().to_vec(),
            Self::P256(k) => ligus_core::crypto::p256_public_key(k),
        }
    }

    /// The device id the key derives.
    pub fn device_id(&self) -> Uuid {
        device_id_for(self.alg(), &self.public_key())
    }

    pub fn sign(&self, msg: &[u8]) -> Vec<u8> {
        match self {
            Self::Ed25519(k) => k.sign(msg).to_vec(),
            Self::P256(k) => ligus_core::crypto::sign_p256(k, msg).to_vec(),
        }
    }
}

fn alg_name(alg: SigAlg) -> &'static str {
    match alg {
        SigAlg::Ed25519 => "ed25519",
        SigAlg::P256 => "p256",
    }
}

fn verify_sig(alg: SigAlg, key: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    match alg {
        SigAlg::Ed25519 => key.len() == 32 && ligus_core::crypto::verify(key, msg, sig).is_ok(),
        SigAlg::P256 => ligus_core::crypto::verify_p256(key, msg, sig).is_ok(),
    }
}

/// A message type (`t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MsgType {
    Request,
    Claim,
    Open,
    Reveal,
    Result,
    Abort,
    /// A computer link: the sealed grant's digest before the grant.
    Offer,
    /// The requester's acknowledgement of an offer.
    Ack,
}

impl MsgType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Claim => "claim",
            Self::Open => "open",
            Self::Reveal => "reveal",
            Self::Result => "result",
            Self::Abort => "abort",
            Self::Offer => "offer",
            Self::Ack => "ack",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "request" => Self::Request,
            "claim" => Self::Claim,
            "open" => Self::Open,
            "reveal" => Self::Reveal,
            "result" => Self::Result,
            "abort" => Self::Abort,
            "offer" => Self::Offer,
            "ack" => Self::Ack,
            _ => return None,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct EnvelopeWire {
    t: String,
    alg: String,
    key: String,
    body: String,
    sig: String,
}

/// Sign `body` as a message of type `t`: the plaintext envelope (wire note
/// 3.2), as a string, ready to seal.
pub(crate) fn sign_message(t: MsgType, body: &impl Serialize, signer: &Signer) -> String {
    let body = serde_json::to_string(body).expect("serialize a message body");
    let pre = mc::message_preimage(t.as_str(), &body).expect("a known message type");
    let w = EnvelopeWire {
        t: t.as_str().into(),
        alg: alg_name(signer.alg()).into(),
        key: b64::enc(&signer.public_key()),
        body,
        sig: b64::enc(&signer.sign(&pre)),
    };
    serde_json::to_string(&w).expect("serialize an envelope")
}

/// A plaintext envelope whose signature verified over its exact `body`
/// string. Who the key belongs to is the caller's check.
#[derive(Debug, Clone)]
pub struct Verified {
    pub t: MsgType,
    pub alg: SigAlg,
    /// The signer's public key.
    pub key: Vec<u8>,
    /// The body, exactly as signed.
    pub body: String,
    /// The whole plaintext envelope.
    pub raw: String,
}

impl Verified {
    /// Verify a plaintext envelope.
    pub fn parse(plain: &[u8]) -> Result<Self, MsgError> {
        if plain.len() > MAX_ENVELOPE {
            return Err(MsgError::Malformed);
        }
        let raw = std::str::from_utf8(plain).map_err(|_| MsgError::Malformed)?;
        let w: EnvelopeWire = serde_json::from_str(raw).map_err(|_| MsgError::Malformed)?;
        let t = MsgType::parse(&w.t).ok_or(MsgError::Malformed)?;
        let alg = match w.alg.as_str() {
            "ed25519" => SigAlg::Ed25519,
            "p256" => SigAlg::P256,
            _ => return Err(MsgError::Malformed),
        };
        let key = b64::dec(&w.key).ok_or(MsgError::Malformed)?;
        let want = if alg == SigAlg::Ed25519 { 32 } else { 65 };
        if key.len() != want {
            return Err(MsgError::Malformed);
        }
        let sig = b64::dec(&w.sig).ok_or(MsgError::Malformed)?;
        let pre = mc::message_preimage(t.as_str(), &w.body).ok_or(MsgError::Malformed)?;
        if !verify_sig(alg, &key, &pre, &sig) {
            return Err(MsgError::BadSignature);
        }
        Ok(Self { t, alg, key, body: w.body, raw: raw.to_string() })
    }

    /// The device id the signer's key derives.
    pub fn signer_device_id(&self) -> Uuid {
        device_id_for(self.alg, &self.key)
    }

    fn expect(&self, t: MsgType) -> Result<(), MsgError> {
        if self.t != t {
            return Err(MsgError::WrongType { want: t.as_str(), got: self.t.as_str() });
        }
        Ok(())
    }

    fn body_as<T: DeserializeOwned>(&self) -> Result<T, MsgError> {
        serde_json::from_str(&self.body).map_err(|_| MsgError::Malformed)
    }
}

fn check_rid(body_rid: &str, rid: &[u8; 16]) -> Result<(), MsgError> {
    if hex_fixed::<16>(body_rid) != Some(*rid) {
        return Err(MsgError::WrongRid);
    }
    Ok(())
}

/// `b64u(ephemeralPublic(32) ‖ ciphertext)` sealed to `recipient`.
pub fn seal_blob(recipient: &[u8; 32], plain: &[u8]) -> Result<String, MsgError> {
    let s = ligus_core::e2ee::seal(recipient, plain).map_err(|_| MsgError::Malformed)?;
    let mut out = s.ephemeral_public;
    out.extend_from_slice(&s.ciphertext);
    Ok(b64::enc(&out))
}

/// Open a blob sealed to `ka`; `None` means "not for me" (or damaged).
pub fn open_blob(ka: &KeyAgreementKeypair, blob: &str) -> Option<Vec<u8>> {
    let b = b64::dec(blob)?;
    if b.len() <= 32 {
        return None;
    }
    let sealed = SealedBlob { ephemeral_public: b[..32].to_vec(), ciphertext: b[32..].to_vec() };
    ligus_core::e2ee::open(ka, &sealed).ok()
}

// ---- Bodies (wire note 3.3) ----

/// A request's body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestBody {
    pub v: u32,
    pub kind: MatchKind,
    pub rid: String,
    pub mailbox: String,
    pub code: String,
    pub requester_ka: String,
    pub read_key: String,
    pub commit: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimBody {
    pub rid: String,
    pub approver_device_id: Uuid,
    /// A display hint only.
    pub approver_name: String,
    pub at: i64,
}

/// Who the approver says they are (unverified until the log says so).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApproverPerson {
    pub account_id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenBody {
    pub rid: String,
    pub approver_device_id: Uuid,
    pub approver_sig_key: String,
    pub approver_ka: String,
    pub approver_person: ApproverPerson,
    pub n_a: String,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevealBody {
    pub rid: String,
    pub n_r: String,
}

/// How an approver ended a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Approved,
    Mismatch,
    Declined,
    Expired,
}

/// A result's body. `grant` (a `ligusadmit1:`/`ligusgrant1:` text) is a
/// sealed secret for the requester only: never log it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultBody {
    pub rid: String,
    pub outcome: Outcome,
    pub grant: Option<String>,
    pub grant_digest: Option<String>,
}

impl std::fmt::Debug for ResultBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResultBody")
            .field("rid", &self.rid)
            .field("outcome", &self.outcome)
            .field("grant", &self.grant.as_ref().map(|_| ".."))
            .field("grant_digest", &self.grant_digest)
            .finish()
    }
}

/// Why a request was ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AbortReason {
    Mismatch,
    Cancelled,
    Contested,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbortBody {
    pub rid: String,
    pub reason: AbortReason,
}

/// A computer link's offer (identity model v2, 8.1): the approver built and
/// sealed the grant and names its digest; the grant follows only after the
/// requester's [`AckBody`] and the approver's passkey.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OfferBody {
    pub rid: String,
    pub grant_digest: String,
    pub person: ApproverPerson,
    pub approver_device_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AckBody {
    pub rid: String,
    pub grant_digest: String,
}

/// The code a request carries, decoded and verified as today.
#[derive(Debug, Clone)]
pub enum RequestSubject {
    Box(JoinRequest),
    Computer(LinkRequest),
    Phone(PhoneCode),
}

impl RequestSubject {
    /// The label the requester gave (a display hint).
    pub fn label(&self) -> String {
        let l = match self {
            Self::Box(r) => &r.label,
            Self::Computer(r) => &r.label,
            Self::Phone(c) => &c.label,
        };
        crate::display::shown(l)
    }

    /// The 80-bit fingerprint the paste path compares (Details).
    pub fn fingerprint(&self) -> String {
        match self {
            Self::Box(r) => r.fingerprint(),
            Self::Computer(r) => r.fingerprint(),
            Self::Phone(c) => c.fingerprint(),
        }
    }

    /// The requesting device's id.
    pub fn device_id(&self) -> Uuid {
        match self {
            Self::Box(r) => r.device_id,
            Self::Computer(r) => r.device_id,
            Self::Phone(c) => c.device_id,
        }
    }

    /// The requesting device's signing key.
    pub fn device_key(&self) -> &[u8] {
        match self {
            Self::Box(r) => &r.device_public_key,
            Self::Computer(r) => &r.device_public_key,
            Self::Phone(c) => &c.public_key,
        }
    }

    fn alg(&self) -> SigAlg {
        match self {
            Self::Phone(_) => SigAlg::P256,
            _ => SigAlg::Ed25519,
        }
    }

    /// The key-agreement key the code carries (a box or a computer).
    fn ka(&self) -> Option<&[u8]> {
        match self {
            Self::Box(r) => Some(&r.ka_public),
            Self::Computer(r) => Some(&r.ka_public),
            Self::Phone(_) => None,
        }
    }

    /// Decode `code` as a code of `kind`, verified as today.
    pub fn decode(kind: MatchKind, code: &str, now_ms: i64) -> Result<Self, MsgError> {
        let prefix = match kind {
            MatchKind::Box => crate::box_admission::JOIN_PREFIX,
            MatchKind::Computer => crate::computer_link::REQUEST_PREFIX,
            MatchKind::Phone => crate::phone_enroll::CODE_PREFIX,
        };
        if !code.trim_start().starts_with(prefix) {
            return Err(MsgError::KindMismatch);
        }
        Ok(match kind {
            MatchKind::Box => Self::Box(JoinRequest::decode(code).map_err(MsgError::BadCode)?),
            MatchKind::Computer => {
                let r = LinkRequest::decode(code).map_err(|e| MsgError::BadCode(e.to_string()))?;
                r.check_fresh(now_ms).map_err(|e| MsgError::BadCode(e.to_string()))?;
                Self::Computer(r)
            }
            MatchKind::Phone => Self::Phone(PhoneCode::decode(code).map_err(|e| MsgError::BadCode(e.to_string()))?),
        })
    }
}

/// A request that passed every check of wire note 3.3.
#[derive(Debug, Clone)]
pub struct VerifiedRequest {
    pub rid: [u8; 16],
    pub kind: MatchKind,
    pub mailbox: [u8; 16],
    /// The request's code text, exactly as carried (the transcript hashes it).
    pub code: String,
    pub subject: RequestSubject,
    pub requester_ka: [u8; 32],
    pub commit: [u8; 32],
    pub created_at: i64,
    pub signer_alg: SigAlg,
    pub signer_key: Vec<u8>,
    /// The plaintext envelope, as a string (the serve API hands it to a phone).
    pub envelope: String,
}

impl VerifiedRequest {
    pub fn rid_hex(&self) -> String {
        hex(&self.rid)
    }

    /// Check a plaintext request read from `mailbox` under `rid`.
    pub fn verify(plain: &[u8], rid: &[u8; 16], mailbox: &[u8; 16], now_ms: i64) -> Result<Self, MsgError> {
        let v = Verified::parse(plain)?;
        v.expect(MsgType::Request)?;
        let b: RequestBody = v.body_as()?;
        if b.v != 1 {
            return Err(MsgError::Malformed);
        }
        check_rid(&b.rid, rid)?;
        if hex_fixed::<16>(&b.mailbox) != Some(*mailbox) {
            return Err(MsgError::WrongMailbox);
        }
        let requester_ka = b64::fixed::<32>(&b.requester_ka).ok_or(MsgError::Malformed)?;
        b64::fixed::<16>(&b.read_key).ok_or(MsgError::Malformed)?;
        let commit = b64::fixed::<32>(&b.commit).ok_or(MsgError::Malformed)?;
        if b.created_at > now_ms.saturating_add(CLOCK_SKEW_MS) {
            return Err(MsgError::FromTheFuture);
        }
        if now_ms.saturating_sub(b.created_at) > mc::REQUEST_TTL_MS {
            return Err(MsgError::Expired);
        }
        let subject = RequestSubject::decode(b.kind, &b.code, now_ms)?;
        if subject.alg() != v.alg || subject.device_key() != v.key.as_slice() {
            return Err(MsgError::WrongKey);
        }
        if subject.ka().is_some_and(|ka| ka != requester_ka) {
            return Err(MsgError::WrongRequesterKa);
        }
        Ok(Self {
            rid: *rid,
            kind: b.kind,
            mailbox: *mailbox,
            code: b.code,
            subject,
            requester_ka,
            commit,
            created_at: b.created_at,
            signer_alg: v.alg,
            signer_key: v.key.clone(),
            envelope: v.raw,
        })
    }
}

/// An `open` that verified.
#[derive(Debug, Clone)]
pub struct VerifiedOpen {
    pub approver_device_id: Uuid,
    pub approver_alg: SigAlg,
    pub approver_sig_key: Vec<u8>,
    pub approver_ka: [u8; 32],
    pub person: ApproverPerson,
    pub n_a: [u8; 32],
    pub at: i64,
}

pub(crate) fn verify_open(v: &Verified, rid: &[u8; 16]) -> Result<VerifiedOpen, MsgError> {
    v.expect(MsgType::Open)?;
    let b: OpenBody = v.body_as()?;
    check_rid(&b.rid, rid)?;
    let sig_key = b64::dec(&b.approver_sig_key).ok_or(MsgError::Malformed)?;
    if sig_key != v.key {
        return Err(MsgError::WrongKey);
    }
    if b.approver_device_id != v.signer_device_id() {
        return Err(MsgError::WrongDeviceId);
    }
    Ok(VerifiedOpen {
        approver_device_id: b.approver_device_id,
        approver_alg: v.alg,
        approver_sig_key: sig_key,
        approver_ka: b64::fixed::<32>(&b.approver_ka).ok_or(MsgError::Malformed)?,
        person: b.approver_person,
        n_a: b64::fixed::<32>(&b.n_a).ok_or(MsgError::Malformed)?,
        at: b.at,
    })
}

/// `nR` from a reveal signed by the requester (`key`, `alg`).
pub(crate) fn verify_reveal(v: &Verified, rid: &[u8; 16], alg: SigAlg, key: &[u8]) -> Result<[u8; 32], MsgError> {
    v.expect(MsgType::Reveal)?;
    if v.alg != alg || v.key != key {
        return Err(MsgError::WrongKey);
    }
    let b: RevealBody = v.body_as()?;
    check_rid(&b.rid, rid)?;
    b64::fixed::<32>(&b.n_r).ok_or(MsgError::Malformed)
}

/// A result signed by `approver_sig_key`, its digest well formed.
pub(crate) fn verify_result(v: &Verified, rid: &[u8; 16], alg: SigAlg, approver_sig_key: &[u8]) -> Result<ResultBody, MsgError> {
    v.expect(MsgType::Result)?;
    if v.alg != alg || v.key != approver_sig_key {
        return Err(MsgError::WrongKey);
    }
    let b: ResultBody = v.body_as()?;
    check_rid(&b.rid, rid)?;
    if b.grant.is_some() != b.grant_digest.is_some() || b.grant_digest.as_deref().is_some_and(|d| hex_fixed::<32>(d).is_none()) {
        return Err(MsgError::Malformed);
    }
    Ok(b)
}

pub(crate) fn verify_abort(v: &Verified, rid: &[u8; 16]) -> Result<AbortReason, MsgError> {
    v.expect(MsgType::Abort)?;
    let b: AbortBody = v.body_as()?;
    check_rid(&b.rid, rid)?;
    Ok(b.reason)
}

pub(crate) fn verify_claim(v: &Verified, rid: &[u8; 16]) -> Result<ClaimBody, MsgError> {
    v.expect(MsgType::Claim)?;
    let b: ClaimBody = v.body_as()?;
    check_rid(&b.rid, rid)?;
    if b.approver_device_id != v.signer_device_id() {
        return Err(MsgError::WrongDeviceId);
    }
    Ok(b)
}

/// An offer signed by the open's approver key, naming the open's person and
/// device, with a well-formed digest.
pub(crate) fn verify_offer(v: &Verified, rid: &[u8; 16], open: &VerifiedOpen) -> Result<OfferBody, MsgError> {
    v.expect(MsgType::Offer)?;
    if v.alg != open.approver_alg || v.key != open.approver_sig_key {
        return Err(MsgError::WrongKey);
    }
    let b: OfferBody = v.body_as()?;
    check_rid(&b.rid, rid)?;
    if b.approver_device_id != open.approver_device_id || b.person != open.person {
        return Err(MsgError::WrongDeviceId);
    }
    hex_fixed::<32>(&b.grant_digest).ok_or(MsgError::Malformed)?;
    Ok(b)
}

/// The digest an ack signed by the requester (`alg`, `key`) acknowledges.
pub(crate) fn verify_ack(v: &Verified, rid: &[u8; 16], alg: SigAlg, key: &[u8]) -> Result<String, MsgError> {
    v.expect(MsgType::Ack)?;
    if v.alg != alg || v.key != key {
        return Err(MsgError::WrongKey);
    }
    let b: AckBody = v.body_as()?;
    check_rid(&b.rid, rid)?;
    hex_fixed::<32>(&b.grant_digest).ok_or(MsgError::Malformed)?;
    Ok(b.grant_digest)
}

/// SHA-256 of a grant's UTF-8 bytes, hex.
pub(crate) fn grant_digest(grant: &str) -> String {
    use sha2::{Digest, Sha256};
    hex(&Sha256::digest(grant.as_bytes()))
}

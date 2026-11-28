//! Reverse-QR phone enrollment: "Add a phone" by scanning the desktop's QR
//! code (`docs/design/v2/phone-enroll-reverse-qr.md`, revision 3). This
//! module is the codec and the checks only: the three wire formats, the keys
//! derived from the QR's secret, the presence-slot payloads, and every check
//! the design assigns to the box, the desktop and the phone, as pure
//! functions. It does no I/O: the serve routes, the desktop's Tauri commands,
//! the tunnel's provisional enrollment and the iOS app call it.
//!
//! ```text
//! offer   = "ligusenroll1:"    + b64u( ws(16) ‖ desktop(16) ‖ secret(32) ‖ exp(u64 LE) )             72 bytes
//! request = "ligusenrollreq1:" + b64u( rid(16) ‖ ws(16) ‖ desktop(16) ‖ exp(u64 LE) ‖ phone_sig(64)
//!                                      ‖ mac(32) ‖ code_body(145..209) )                       297..361 bytes
//! request = "ligusenrollreq2:" + b64u( rid(16) ‖ ws(16) ‖ desktop(16) ‖ exp(u64 LE) ‖ phone_sig(64)
//!                                      ‖ mac(32) ‖ ext_len(u8) ‖ ext(1..64) ‖ code_body )       (v2: extensions)
//! answer  = "ligusenrollres1:" + b64u( rid(16) ‖ outcome(u8) ‖ device_id(16) ‖ tag(32) )             65 bytes
//!
//! prk   = HKDF-Extract(salt = "ligus-enroll-qr-v1", ikm = secret)            (HKDF-SHA256)
//! rid   = HKDF-Expand(prk, "rid", 16)
//! k_req = HKDF-Expand(prk, "request-mac", 32)
//! k_ans = HKDF-Expand(prk, "answer-mac", 32)
//!
//! signed    = "ligus-enroll-request-v1\0" ‖ rid ‖ ws ‖ desktop ‖ exp ‖ code_body
//! signed v2 = "ligus-enroll-request-v2\0" ‖ rid ‖ ws ‖ desktop ‖ exp ‖ ext_len ‖ ext ‖ code_body
//! phone_sig = ECDSA P-256 / SHA-256 over signed, r ‖ s, low S (the phone's device key)
//! mac       = HMAC-SHA256(k_req, signed ‖ phone_sig)
//! tag       = HMAC-SHA256(k_ans, "ligus-enroll-answer-v1\0" ‖ rid ‖ outcome ‖ device_id)
//! ```
//!
//! `ext` (v2 only, workspace model v3, 10.2) is a list of `type(u8) ‖ len(u8)
//! ‖ value` entries, each type at most once, filling `ext_len` exactly. Type
//! 1 is `previousDeviceId` (16 bytes): after a key reset, the phone's old
//! device id, so the desktop that certifies the new key can offer to revoke
//! the old one in the same step. An unknown type below 0x80 is ignored; an
//! unknown type from 0x80 up is refused (a "critical" extension). A phone
//! sends v1 when it has no extension, so older desktops and boxes keep
//! working.
//!
//! `code_body` is the phone's `ligusphone1:` code bytes, unchanged
//! ([`PhoneCode::body`]); it passes every check of [`PhoneCode::decode`],
//! and the desktop certifies exactly that code. `b64u` is base64url without
//! padding; decoders refuse padding, `+`, `/`, whitespace and non-canonical
//! trailing bits. Every `now_ms` below is Unix milliseconds; `exp` is Unix
//! seconds.
//!
//! # API for the next steps
//!
//! Each piece calls only these for the formats and the checks; nothing else
//! may build or parse a `ligusenroll*` string.
//!
//! **Box (`ligus serve`, step 2 of the design's build order):**
//! - `POST /v1/phone-enroll/check`: [`box_check_offer`]`(&OfferFields,
//!   box_workspace, now_ms)`, then the roster checks, refused with
//!   [`BoxRefusal::DesktopUnknown`], [`BoxRefusal::DeviceRevoked`],
//!   [`BoxRefusal::SomeoneElses`] or [`BoxRefusal::RelayUnavailable`]
//!   ([`BoxRefusal::status`] and [`BoxRefusal::error`] are the HTTP status
//!   and the `error` field).
//! - `POST /v1/phone-enroll/requests`: [`box_check_request`]`(text,
//!   box_workspace, caller_device, now_ms) -> EnrollRequest`, then the same
//!   roster checks on [`EnrollRequest::fields`] and the phone's
//!   [`EnrollRequest::code`]; then [`RelayedRequest::new`] →
//!   [`RelayedRequest::sign`] (its device key) → [`seal_relayed_request`],
//!   published at [`request_slot`]`(rid, box_device_id, phone_device_id)`
//!   (one slot per phone, so every pending request shows at once).
//! - `GET /v1/phone-enroll/requests/{rid}`: [`open_relayed_answers`] then
//!   [`check_relayed_answer`]`(slot, &answer, offer_desktop, now_ms,
//!   desktop_key)`, relaying [`RelayedAnswer::answer`] as is.
//! - [`Rid::from_b64u`] parses the path's `rid`.
//!
//! **Desktop (Tauri commands, step 4):**
//! - `phone_enroll_offer_start`: [`DesktopOffer::start`]`(ws, desktop,
//!   now_ms)`, then [`DesktopOffer::qr_text`] (only into the QR renderer),
//!   [`DesktopOffer::rid`], [`DesktopOffer::expires_at`].
//! - `phone_enroll_offer_poll`: [`DesktopOffer::request_slot_prefix`] to pick
//!   the slots, [`open_relayed_requests`], [`check_relayed_request`]`(slot,
//!   &relayed, now_ms, service_device_key)`, [`DesktopOffer::check_request`]
//!   `(text, now_ms)`, then `ChatService::phone_standing` on the code, then
//!   [`DesktopOffer::observe`]`(&request, now_ms) -> Seen` (first phone,
//!   same phone, two phones = abort, tripwire).
//! - `phone_enroll_offer_approve`: [`DesktopOffer::check_approve`]`(rid,
//!   fingerprint, now_ms) -> &PhoneCode` (the 5-second hold, the lock, the
//!   fingerprint, the expiry), then `confirm_matches_preview` and
//!   `certify_phone_code`, then [`DesktopOffer::approved`] and
//!   [`DesktopOffer::answer`]`(Outcome::Certified, device)`.
//! - `phone_enroll_offer_decline`: [`DesktopOffer::answer`]`(Outcome::Declined, ..)`.
//! - Answers go out as [`RelayedAnswer::new`] → [`RelayedAnswer::sign`] →
//!   [`seal_relayed_answer`] at [`answer_slot`]`(rid)`, until
//!   [`DesktopOffer::is_over`].
//!
//! **Phone (iOS, step 5; the Swift port must match the vectors):**
//! - Scan: [`PhoneOffer::scan`]`(text, now_ms)`; [`PhoneOffer::fields`] for
//!   `check`; [`PhoneOffer::build_request`]`(&code, sign)` →
//!   [`EnrollRequest::encode`]; [`PhoneOffer::check_answer`]`(text,
//!   my_device) -> PhoneVerdict` ([`PhoneVerdict::AnotherPhone`] with
//!   [`Outcome::Certified`] is the tripwire alarm).
//!
//! **Tunnel (E3b, step 3):** nothing here; it calls the box routes.
//!
//! Golden vectors: `crates/ligus-core/tests/golden/phone_enroll_qr_v1.jsonl`;
//! the iOS differential corpus:
//! `crates/ligus-core/tests/golden/rust_parity_enroll_v1.jsonl`. Both are
//! generated and checked by `crates/ligus-runtime/tests/phone_enroll_qr_vectors.rs`.
//!
//! Secrets: the offer's secret, `k_req` and `k_ans` are wiped on drop, and
//! no `Debug` or error message here prints them or the offer text.

use std::collections::HashMap;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hmac::{KeyInit, Mac};
use ligus_core::SigningKeypair;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

use crate::keyring::KeyringEngine;
use crate::phone_enroll::{PhoneCode, PhoneCodeError, MAX_LABEL, P256_KEY_LEN};

// ---- FROZEN wire identifiers (new in 0.1.1, never renamed) ----

/// Prefix of the desktop's QR. The prefix is the version.
pub const OFFER_PREFIX: &str = "ligusenroll1:";
/// Prefix of the phone's request.
pub const REQUEST_PREFIX: &str = "ligusenrollreq1:";
/// Prefix of the phone's request with extensions (v2).
pub const REQUEST_V2_PREFIX: &str = "ligusenrollreq2:";
/// Prefix of the desktop's answer.
pub const ANSWER_PREFIX: &str = "ligusenrollres1:";
/// HKDF-SHA256 salt for every key derived from the offer's secret.
pub const HKDF_SALT: &[u8] = b"ligus-enroll-qr-v1";
/// HKDF info for the rendezvous id.
pub const INFO_RID: &[u8] = b"rid";
/// HKDF info for the request's MAC key.
pub const INFO_REQUEST_MAC: &[u8] = b"request-mac";
/// HKDF info for the answer's MAC key.
pub const INFO_ANSWER_MAC: &[u8] = b"answer-mac";
/// Context of the phone's signature and the request MAC.
pub const REQUEST_CONTEXT: &[u8] = b"ligus-enroll-request-v1\0";
/// The same for a v2 request (with extensions).
pub const REQUEST_V2_CONTEXT: &[u8] = b"ligus-enroll-request-v2\0";
/// v2 extension type: the phone's previous device id (16 bytes).
pub const EXT_PREVIOUS_DEVICE: u8 = 1;
/// v2 extensions are at most this many bytes.
pub const MAX_EXT: usize = 64;
/// Context of the answer's tag.
pub const ANSWER_CONTEXT: &[u8] = b"ligus-enroll-answer-v1\0";
/// Context of a box's signature over a request slot.
pub const RELAY_CONTEXT: &[u8] = b"ligus-enroll-relay-v1\0";
/// Context of the desktop's signature over an answer slot.
pub const ANSWER_RELAY_CONTEXT: &[u8] = b"ligus-enroll-answer-relay-v1\0";
/// Presence slot name prefix of a relayed request:
/// `phone-enroll:<b64u rid>:<box device id>:<phone tag>` ([`request_slot`]).
pub const REQUEST_SLOT_PREFIX: &str = "phone-enroll:";
/// Context of a request slot's phone tag ([`phone_slot_tag`]).
pub const SLOT_TAG_CONTEXT: &[u8] = b"ligus-enroll-slot-v1\0";
/// Presence slot name prefix of an answer: `phone-enroll-answer:<b64u rid>`.
pub const ANSWER_SLOT_PREFIX: &str = "phone-enroll-answer:";
/// Sealed-blob field of a request slot.
pub const REQUEST_SLOT_FIELD: &str = "phoneEnrollRequest";
/// Sealed-blob field of an answer slot.
pub const ANSWER_SLOT_FIELD: &str = "phoneEnrollAnswer";

// ---- timing (revision 3) ----

/// The desktop issues `exp = now + 300`.
pub const OFFER_TTL_SECS: u64 = 300;
/// The box and the phone accept an `exp` up to this far in the past.
pub const PAST_TOLERANCE_SECS: i64 = 120;
/// The box refuses an `exp` further ahead than this (300 + 120).
pub const BOX_MAX_AHEAD_SECS: i64 = 420;
/// The phone refuses an `exp` further ahead than this (10 minutes).
pub const PHONE_MAX_AHEAD_SECS: i64 = 600;
/// A slot whose `at` is further ahead than this is dropped.
pub const SLOT_MAX_AHEAD_SECS: i64 = 30;
/// A slot whose `at` is older than this is dropped.
pub const SLOT_MAX_AGE_SECS: i64 = 60;
/// **Add this phone** stays disabled this long after a request first appears.
pub const HOLD_MS: i64 = 5_000;
/// The status route's long poll is capped at this.
pub const MAX_WAIT_SECS: u64 = 25;
/// The box and the desktop re-publish their slots this often.
pub const REPUBLISH_SECS: u64 = 10;

const SECRET_LEN: usize = 32;
const OFFER_LEN: usize = 16 + 16 + SECRET_LEN + 8;
const REQUEST_FIXED: usize = 16 + 16 + 16 + 8 + 64 + 32;
const CODE_MIN: usize = 16 + P256_KEY_LEN + 64;
const CODE_MAX: usize = CODE_MIN + MAX_LABEL;
const ANSWER_LEN: usize = 16 + 1 + 16 + 32;

type HmacSha256 = hmac::Hmac<sha2::Sha256>;

// ---- errors ----

/// Why a format or a check refused. [`EnrollError::code`] is the stable
/// reason the golden vectors and the iOS parity corpus pin; the `Display`
/// text is for people and never carries a secret.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnrollError {
    #[error("not a Ligus add-a-phone code (expected {OFFER_PREFIX}...)")]
    NotAnOffer,
    #[error("not an add-a-phone request (expected {REQUEST_PREFIX}...)")]
    NotARequest,
    #[error("not an add-a-phone answer (expected {ANSWER_PREFIX}...)")]
    NotAnAnswer,
    #[error("the code is damaged or incomplete")]
    Malformed,
    #[error("the phone's code in the request isn't valid: {0}")]
    BadCode(PhoneCodeError),
    #[error("the phone's signature over the request doesn't match its key")]
    BadPhoneSignature,
    #[error("this is for another add-a-phone code")]
    WrongOffer,
    #[error("the request wasn't made from this code")]
    BadMac,
    #[error("the request names another workspace, desktop or expiry than this code")]
    OfferMismatch,
    #[error("this code has expired: show a new one")]
    Expired,
    #[error("this code expires too far ahead: check the clock")]
    TooFarAhead,
    #[error("the answer wasn't made by the desktop that showed this code")]
    BadTag,
    #[error("this code was answered by two phones: start again")]
    OfferClosed,
    #[error("no phone has answered this code yet")]
    NoRequest,
    #[error("wait a few seconds: still listening for other phones")]
    Hold,
    #[error("the fingerprint doesn't match the phone that answered")]
    FingerprintMismatch,
    #[error("the relayed request or answer is in the wrong slot")]
    WrongSlot,
    #[error("the relayed request or answer is stale")]
    StaleSlot,
    #[error("the relayed request or answer isn't signed by a device allowed to relay it")]
    BadSlotSignature,
    #[error("the answer comes from another desktop than the one that showed this code")]
    WrongDesktop,
    #[error("the request comes from another device than the phone in it")]
    DeviceMismatch,
}

impl EnrollError {
    /// The stable reason (golden vectors, parity corpus, logs).
    pub fn code(&self) -> &'static str {
        match self {
            EnrollError::NotAnOffer => "not_an_offer",
            EnrollError::NotARequest => "not_a_request",
            EnrollError::NotAnAnswer => "not_an_answer",
            EnrollError::Malformed => "malformed",
            EnrollError::BadCode(_) => "bad_code",
            EnrollError::BadPhoneSignature => "bad_phone_signature",
            EnrollError::WrongOffer => "wrong_offer",
            EnrollError::BadMac => "bad_mac",
            EnrollError::OfferMismatch => "offer_mismatch",
            EnrollError::Expired => "expired",
            EnrollError::TooFarAhead => "too_far_ahead",
            EnrollError::BadTag => "bad_tag",
            EnrollError::OfferClosed => "offer_closed",
            EnrollError::NoRequest => "no_request",
            EnrollError::Hold => "hold",
            EnrollError::FingerprintMismatch => "fingerprint_mismatch",
            EnrollError::WrongSlot => "wrong_slot",
            EnrollError::StaleSlot => "stale_slot",
            EnrollError::BadSlotSignature => "bad_slot_signature",
            EnrollError::WrongDesktop => "wrong_desktop",
            EnrollError::DeviceMismatch => "device_mismatch",
        }
    }
}

// ---- strict base64url ----

/// Decode `body` (after its prefix) as canonical base64url without padding:
/// no `=`, `+`, `/`, whitespace or non-zero trailing bits, and it must be
/// exactly what [`URL_SAFE_NO_PAD`] would encode.
fn b64u_strict(body: &str) -> Option<Zeroizing<Vec<u8>>> {
    let b = Zeroizing::new(URL_SAFE_NO_PAD.decode(body.as_bytes()).ok()?);
    let again = Zeroizing::new(URL_SAFE_NO_PAD.encode(&*b));
    (*again == body).then_some(b)
}

fn b64u(b: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(b)
}

fn uuid_at(b: &[u8], at: usize) -> Uuid {
    Uuid::from_bytes(b[at..at + 16].try_into().expect("16 bytes"))
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

fn secs_ms(secs: i64) -> i128 {
    secs as i128 * 1000
}

// ---- the secret and its keys ----

/// The offer's one-time 32-byte secret. Travels only optically (the QR);
/// wiped on drop; never printed.
pub struct EnrollSecret([u8; SECRET_LEN]);

impl EnrollSecret {
    /// 32 bytes from the OS CSPRNG.
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut s = [0u8; SECRET_LEN];
        getrandom::fill(&mut s)?;
        Ok(Self(s))
    }

    /// A fixed secret (golden vectors and tests).
    pub fn from_bytes(bytes: [u8; SECRET_LEN]) -> Self {
        Self(bytes)
    }

    /// HKDF-SHA256 with salt [`HKDF_SALT`]: the rendezvous id and the two MAC keys.
    pub fn keys(&self) -> OfferKeys {
        let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(HKDF_SALT), &self.0);
        let mut rid = [0u8; 16];
        let mut k_req = Zeroizing::new([0u8; 32]);
        let mut k_ans = Zeroizing::new([0u8; 32]);
        hk.expand(INFO_RID, &mut rid)
            .expect("16 bytes is a valid HKDF-SHA256 length");
        hk.expand(INFO_REQUEST_MAC, &mut *k_req)
            .expect("32 bytes is a valid HKDF-SHA256 length");
        hk.expand(INFO_ANSWER_MAC, &mut *k_ans)
            .expect("32 bytes is a valid HKDF-SHA256 length");
        OfferKeys {
            rid: Rid(rid),
            k_req,
            k_ans,
        }
    }
}

impl Clone for EnrollSecret {
    fn clone(&self) -> Self {
        Self(self.0)
    }
}

impl Drop for EnrollSecret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for EnrollSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EnrollSecret(..)")
    }
}

/// The rendezvous id: names the offer on the wire (box, relay, slot names,
/// logs). Reveals nothing about the secret.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Rid(pub [u8; 16]);

impl Rid {
    /// base64url, no padding (22 characters): the slot names and the status route's path.
    pub fn to_b64u(&self) -> String {
        b64u(&self.0)
    }

    /// Parse [`Rid::to_b64u`] (canonical only).
    pub fn from_b64u(s: &str) -> Option<Self> {
        let b = b64u_strict(s)?;
        Some(Self(b.as_slice().try_into().ok()?))
    }
}

impl std::fmt::Debug for Rid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Rid({})", self.to_b64u())
    }
}

impl std::fmt::Display for Rid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_b64u())
    }
}

impl Serialize for Rid {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_b64u())
    }
}

impl<'de> Deserialize<'de> for Rid {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Rid::from_b64u(&s).ok_or_else(|| serde::de::Error::custom("not a rendezvous id"))
    }
}

/// What the secret derives. `k_req` and `k_ans` never leave the desktop and
/// the phone; wiped on drop.
pub struct OfferKeys {
    pub rid: Rid,
    k_req: Zeroizing<[u8; 32]>,
    k_ans: Zeroizing<[u8; 32]>,
}

impl OfferKeys {
    /// `k_req` (golden vectors only: never log or send it).
    pub fn request_mac_key(&self) -> &[u8; 32] {
        &self.k_req
    }

    /// `k_ans` (golden vectors only: never log or send it).
    pub fn answer_mac_key(&self) -> &[u8; 32] {
        &self.k_ans
    }

    /// `HMAC-SHA256(k_req, signed ‖ phone_sig)`.
    pub fn request_mac(&self, signed: &[u8], phone_sig: &[u8; 64]) -> [u8; 32] {
        let mut m = <HmacSha256 as KeyInit>::new_from_slice(&*self.k_req).expect("any key length");
        m.update(signed);
        m.update(phone_sig);
        m.finalize().into_bytes().into()
    }

    /// Constant-time check of a request's MAC.
    fn verify_request_mac(&self, signed: &[u8], phone_sig: &[u8; 64], mac: &[u8; 32]) -> bool {
        let mut m = <HmacSha256 as KeyInit>::new_from_slice(&*self.k_req).expect("any key length");
        m.update(signed);
        m.update(phone_sig);
        m.verify_slice(mac).is_ok()
    }

    /// `HMAC-SHA256(k_ans, "ligus-enroll-answer-v1\0" ‖ rid ‖ outcome ‖ device_id)`.
    pub fn answer_tag(&self, outcome: Outcome, device_id: Uuid) -> [u8; 32] {
        let mut m = <HmacSha256 as KeyInit>::new_from_slice(&*self.k_ans).expect("any key length");
        m.update(&answer_tag_preimage(self.rid, outcome, device_id));
        m.finalize().into_bytes().into()
    }

    fn verify_answer_tag(&self, outcome: Outcome, device_id: Uuid, tag: &[u8; 32]) -> bool {
        let mut m = <HmacSha256 as KeyInit>::new_from_slice(&*self.k_ans).expect("any key length");
        m.update(&answer_tag_preimage(self.rid, outcome, device_id));
        m.verify_slice(tag).is_ok()
    }
}

impl std::fmt::Debug for OfferKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfferKeys")
            .field("rid", &self.rid)
            .finish_non_exhaustive()
    }
}

// ---- the offer: `ligusenroll1:` ----

/// The desktop's QR.
#[derive(Clone)]
pub struct Offer {
    pub workspace_id: Uuid,
    /// The desktop's workspace device id.
    pub desktop_device_id: Uuid,
    pub secret: EnrollSecret,
    /// Unix seconds.
    pub exp: u64,
}

impl std::fmt::Debug for Offer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Offer")
            .field("workspace_id", &self.workspace_id)
            .field("desktop_device_id", &self.desktop_device_id)
            .field("exp", &self.exp)
            .finish_non_exhaustive()
    }
}

/// An offer's public part: what the phone sends to `POST
/// /v1/phone-enroll/check`, and what a request names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OfferFields {
    pub workspace_id: Uuid,
    pub desktop_device_id: Uuid,
    pub rid: Rid,
    pub exp: u64,
}

impl Offer {
    /// `ligusenroll1:` + b64u(ws ‖ desktop ‖ secret ‖ exp). Holds the
    /// secret: only into the QR renderer, never a log or the webview.
    pub fn encode(&self) -> Zeroizing<String> {
        let mut b = Zeroizing::new(Vec::with_capacity(OFFER_LEN));
        b.extend_from_slice(self.workspace_id.as_bytes());
        b.extend_from_slice(self.desktop_device_id.as_bytes());
        b.extend_from_slice(&self.secret.0);
        b.extend_from_slice(&self.exp.to_le_bytes());
        let body = Zeroizing::new(b64u(&b));
        let mut out = Zeroizing::new(String::with_capacity(OFFER_PREFIX.len() + body.len()));
        out.push_str(OFFER_PREFIX);
        out.push_str(&body);
        out
    }

    /// Parse the format only (no clock): exact prefix, canonical
    /// base64url, exactly 72 bytes.
    pub fn decode(text: &str) -> Result<Self, EnrollError> {
        let body = text
            .strip_prefix(OFFER_PREFIX)
            .ok_or(EnrollError::NotAnOffer)?;
        let b = b64u_strict(body).ok_or(EnrollError::Malformed)?;
        if b.len() != OFFER_LEN {
            return Err(EnrollError::Malformed);
        }
        let mut secret = [0u8; SECRET_LEN];
        secret.copy_from_slice(&b[32..64]);
        let secret = EnrollSecret(secret);
        Ok(Self {
            workspace_id: uuid_at(&b, 0),
            desktop_device_id: uuid_at(&b, 16),
            secret,
            exp: u64_at(&b, 64),
        })
    }

    pub fn keys(&self) -> OfferKeys {
        self.secret.keys()
    }

    pub fn fields(&self) -> OfferFields {
        OfferFields {
            workspace_id: self.workspace_id,
            desktop_device_id: self.desktop_device_id,
            rid: self.keys().rid,
            exp: self.exp,
        }
    }
}

// ---- the request: `ligusenrollreq1:` ----

/// `"ligus-enroll-request-v1\0" ‖ rid ‖ ws ‖ desktop ‖ exp(u64 LE) ‖
/// code_body`: what the phone signs, and (with the signature) what the MAC
/// covers.
pub fn request_signed_preimage(
    rid: Rid,
    workspace_id: Uuid,
    desktop_device_id: Uuid,
    exp: u64,
    code_body: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(REQUEST_CONTEXT.len() + 56 + code_body.len());
    out.extend_from_slice(REQUEST_CONTEXT);
    out.extend_from_slice(&rid.0);
    out.extend_from_slice(workspace_id.as_bytes());
    out.extend_from_slice(desktop_device_id.as_bytes());
    out.extend_from_slice(&exp.to_le_bytes());
    out.extend_from_slice(code_body);
    out
}

/// The v2 form of [`request_signed_preimage`]: `"ligus-enroll-request-v2\0"
/// ‖ rid ‖ ws ‖ desktop ‖ exp ‖ ext_len(u8) ‖ ext ‖ code_body`.
pub fn request_signed_preimage_v2(
    rid: Rid,
    workspace_id: Uuid,
    desktop_device_id: Uuid,
    exp: u64,
    ext: &[u8],
    code_body: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(REQUEST_V2_CONTEXT.len() + 57 + ext.len() + code_body.len());
    out.extend_from_slice(REQUEST_V2_CONTEXT);
    out.extend_from_slice(&rid.0);
    out.extend_from_slice(workspace_id.as_bytes());
    out.extend_from_slice(desktop_device_id.as_bytes());
    out.extend_from_slice(&exp.to_le_bytes());
    out.push(ext.len() as u8);
    out.extend_from_slice(ext);
    out.extend_from_slice(code_body);
    out
}

/// What a request's v2 extensions say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestExt {
    /// The phone's device id before a key reset ([`EXT_PREVIOUS_DEVICE`]).
    pub previous_device_id: Option<Uuid>,
    /// The exact bytes a decoded request carried (unknown entries kept), so
    /// it encodes and signs as it came.
    wire: Vec<u8>,
}

impl RequestExt {
    /// Extensions naming the phone's previous device id.
    pub fn previous(device: Uuid) -> Self {
        Self { previous_device_id: Some(device), wire: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.encode().is_empty()
    }

    /// The `ext` bytes (empty when there is nothing to say).
    pub fn encode(&self) -> Vec<u8> {
        if !self.wire.is_empty() {
            return self.wire.clone();
        }
        let mut out = Vec::new();
        if let Some(d) = self.previous_device_id {
            out.push(EXT_PREVIOUS_DEVICE);
            out.push(16);
            out.extend_from_slice(d.as_bytes());
        }
        out
    }

    /// Parse `ext`: well-formed entries filling it exactly, each type once,
    /// no unknown critical type (0x80 and up), a previous device id of 16
    /// bytes that isn't nil.
    pub fn decode(ext: &[u8]) -> Result<Self, EnrollError> {
        let mut out = Self::default();
        let mut seen = [false; 256];
        let mut i = 0;
        while i < ext.len() {
            let (ty, len) = (ext[i], *ext.get(i + 1).ok_or(EnrollError::Malformed)? as usize);
            let value = ext.get(i + 2..i + 2 + len).ok_or(EnrollError::Malformed)?;
            if std::mem::replace(&mut seen[ty as usize], true) {
                return Err(EnrollError::Malformed);
            }
            match ty {
                EXT_PREVIOUS_DEVICE => {
                    let b: [u8; 16] = value.try_into().map_err(|_| EnrollError::Malformed)?;
                    let d = Uuid::from_bytes(b);
                    if d.is_nil() {
                        return Err(EnrollError::Malformed);
                    }
                    out.previous_device_id = Some(d);
                }
                t if t >= 0x80 => return Err(EnrollError::Malformed),
                _ => {}
            }
            i += 2 + len;
        }
        out.wire = ext.to_vec();
        Ok(out)
    }
}

/// A request's bytes, split but not yet checked.
struct RawRequest {
    fields: OfferFields,
    phone_sig: [u8; 64],
    mac: [u8; 32],
    /// `None` for v1; the v2 `ext` bytes otherwise (never empty).
    ext: Option<Vec<u8>>,
    code_body: Vec<u8>,
}

impl RawRequest {
    fn parse(text: &str) -> Result<Self, EnrollError> {
        let (body, v2) = match (text.strip_prefix(REQUEST_PREFIX), text.strip_prefix(REQUEST_V2_PREFIX)) {
            (Some(b), _) => (b, false),
            (None, Some(b)) => (b, true),
            (None, None) => return Err(EnrollError::NotARequest),
        };
        let b = b64u_strict(body).ok_or(EnrollError::Malformed)?;
        let (ext, code_at) = if v2 {
            let n = *b.get(REQUEST_FIXED).ok_or(EnrollError::Malformed)? as usize;
            if n == 0 || n > MAX_EXT {
                return Err(EnrollError::Malformed);
            }
            let ext = b.get(REQUEST_FIXED + 1..REQUEST_FIXED + 1 + n).ok_or(EnrollError::Malformed)?.to_vec();
            RequestExt::decode(&ext)?;
            (Some(ext), REQUEST_FIXED + 1 + n)
        } else {
            (None, REQUEST_FIXED)
        };
        if b.len() < code_at + CODE_MIN || b.len() > code_at + CODE_MAX {
            return Err(EnrollError::Malformed);
        }
        let fields = OfferFields {
            rid: Rid(b[0..16].try_into().expect("16 bytes")),
            workspace_id: uuid_at(&b, 16),
            desktop_device_id: uuid_at(&b, 32),
            exp: u64_at(&b, 48),
        };
        Ok(Self {
            fields,
            phone_sig: b[56..120].try_into().expect("64 bytes"),
            mac: b[120..152].try_into().expect("32 bytes"),
            ext,
            code_body: b[code_at..].to_vec(),
        })
    }

    fn signed(&self) -> Vec<u8> {
        let f = &self.fields;
        match &self.ext {
            None => request_signed_preimage(f.rid, f.workspace_id, f.desktop_device_id, f.exp, &self.code_body),
            Some(ext) => request_signed_preimage_v2(f.rid, f.workspace_id, f.desktop_device_id, f.exp, ext, &self.code_body),
        }
    }

    /// `phone_sig` under the key inside `code_body` (P-256, low S).
    fn verify_phone_sig(&self) -> Result<(), EnrollError> {
        let key = &self.code_body[16..16 + P256_KEY_LEN];
        ligus_core::crypto::verify_p256(key, &self.signed(), &self.phone_sig)
            .map_err(|_| EnrollError::BadPhoneSignature)
    }
}

/// A phone's request to be added, decoded and with its phone signature and
/// code checked (the MAC is checked only by [`DesktopOffer::check_request`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollRequest {
    pub rid: Rid,
    pub workspace_id: Uuid,
    pub desktop_device_id: Uuid,
    pub exp: u64,
    /// ECDSA P-256 over [`EnrollRequest::signed`], `r ‖ s`, low S.
    pub phone_sig: [u8; 64],
    /// `HMAC-SHA256(k_req, signed ‖ phone_sig)`.
    pub mac: [u8; 32],
    /// The phone's `ligusphone1:` code, carried unchanged.
    pub code: PhoneCode,
    /// The v2 extensions (empty: a v1 request).
    pub ext: RequestExt,
}

impl EnrollRequest {
    /// The phone's side: sign the request for `offer` with the phone's
    /// device key (`sign`: ECDSA P-256 / SHA-256, 64 bytes `r ‖ s`, low S,
    /// over the bytes it is given) and MAC it with the offer's `k_req`.
    pub fn build(offer: &Offer, code: &PhoneCode, sign: impl FnOnce(&[u8]) -> [u8; 64]) -> Self {
        Self::build_with(offer, code, RequestExt::default(), sign)
    }

    /// [`EnrollRequest::build`] with extensions (a v2 request when `ext`
    /// says anything; a previous device id equal to this phone's is dropped).
    pub fn build_with(offer: &Offer, code: &PhoneCode, mut ext: RequestExt, sign: impl FnOnce(&[u8]) -> [u8; 64]) -> Self {
        if ext.previous_device_id == Some(code.device_id) {
            ext = RequestExt::default();
        }
        let keys = offer.keys();
        let signed = Self::preimage(keys.rid, offer.workspace_id, offer.desktop_device_id, offer.exp, &ext, &code.body());
        let phone_sig = sign(&signed);
        let mac = keys.request_mac(&signed, &phone_sig);
        Self {
            rid: keys.rid,
            workspace_id: offer.workspace_id,
            desktop_device_id: offer.desktop_device_id,
            exp: offer.exp,
            phone_sig,
            mac,
            code: code.clone(),
            ext,
        }
    }

    fn preimage(rid: Rid, ws: Uuid, desktop: Uuid, exp: u64, ext: &RequestExt, code_body: &[u8]) -> Vec<u8> {
        if ext.is_empty() {
            request_signed_preimage(rid, ws, desktop, exp, code_body)
        } else {
            request_signed_preimage_v2(rid, ws, desktop, exp, &ext.encode(), code_body)
        }
    }

    /// What the phone signed.
    pub fn signed(&self) -> Vec<u8> {
        Self::preimage(self.rid, self.workspace_id, self.desktop_device_id, self.exp, &self.ext, &self.code.body())
    }

    /// The phone's device id before a key reset, when the request says so
    /// (signed by the new key, inside the request; never on the log).
    pub fn previous_device_id(&self) -> Option<Uuid> {
        self.ext.previous_device_id
    }

    /// `ligusenrollreq1:` + b64u(rid ‖ ws ‖ desktop ‖ exp ‖ phone_sig ‖ mac ‖
    /// code_body), or `ligusenrollreq2:` with `ext_len ‖ ext` before the code
    /// when there are extensions.
    pub fn encode(&self) -> String {
        let code = self.code.body();
        let ext = self.ext.encode();
        let mut b = Vec::with_capacity(REQUEST_FIXED + 1 + ext.len() + code.len());
        b.extend_from_slice(&self.rid.0);
        b.extend_from_slice(self.workspace_id.as_bytes());
        b.extend_from_slice(self.desktop_device_id.as_bytes());
        b.extend_from_slice(&self.exp.to_le_bytes());
        b.extend_from_slice(&self.phone_sig);
        b.extend_from_slice(&self.mac);
        if ext.is_empty() {
            b.extend_from_slice(&code);
            return format!("{REQUEST_PREFIX}{}", b64u(&b));
        }
        b.push(ext.len() as u8);
        b.extend_from_slice(&ext);
        b.extend_from_slice(&code);
        format!("{REQUEST_V2_PREFIX}{}", b64u(&b))
    }

    /// Everything a party without the secret can check (the box's steps 1
    /// to 3, §7.3): the encoding, the phone's code (every
    /// [`PhoneCode::decode`] check), then `phone_sig` under the code's key.
    pub fn decode(text: &str) -> Result<Self, EnrollError> {
        let raw = RawRequest::parse(text)?;
        let code = PhoneCode::from_body(&raw.code_body).map_err(EnrollError::BadCode)?;
        raw.verify_phone_sig()?;
        Self::from_raw(raw, code)
    }

    fn from_raw(raw: RawRequest, code: PhoneCode) -> Result<Self, EnrollError> {
        let ext = match &raw.ext {
            Some(e) => RequestExt::decode(e)?,
            None => RequestExt::default(),
        };
        // A phone never names itself as its own previous device.
        if ext.previous_device_id == Some(code.device_id) {
            return Err(EnrollError::Malformed);
        }
        let f = raw.fields;
        Ok(Self {
            rid: f.rid,
            workspace_id: f.workspace_id,
            desktop_device_id: f.desktop_device_id,
            exp: f.exp,
            phone_sig: raw.phone_sig,
            mac: raw.mac,
            code,
            ext,
        })
    }

    /// The offer this request names.
    pub fn fields(&self) -> OfferFields {
        OfferFields {
            workspace_id: self.workspace_id,
            desktop_device_id: self.desktop_device_id,
            rid: self.rid,
            exp: self.exp,
        }
    }

    /// The phone's device id (the derived id of its key).
    pub fn device_id(&self) -> Uuid {
        self.code.device_id
    }

    /// Whether `other` comes from the same phone: the same device id and
    /// key. ECDSA signatures differ every time, so a phone that restarted and
    /// scanned again sends other bytes but is still the same phone (review
    /// round 2, R11).
    pub fn same_phone(&self, other: &EnrollRequest) -> bool {
        self.code.device_id == other.code.device_id && self.code.public_key == other.code.public_key
    }
}

// ---- the answer: `ligusenrollres1:` ----

/// What the desktop answered about a phone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    Certified = 1,
    Declined = 2,
    /// Standing: a conflict, or revoked.
    Refused = 3,
}

impl Outcome {
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            1 => Some(Self::Certified),
            2 => Some(Self::Declined),
            3 => Some(Self::Refused),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Certified => "certified",
            Self::Declined => "declined",
            Self::Refused => "refused",
        }
    }
}

/// `"ligus-enroll-answer-v1\0" ‖ rid ‖ outcome(u8) ‖ device_id`: what the
/// answer's tag covers.
pub fn answer_tag_preimage(rid: Rid, outcome: Outcome, device_id: Uuid) -> Vec<u8> {
    let mut out = Vec::with_capacity(ANSWER_CONTEXT.len() + 33);
    out.extend_from_slice(ANSWER_CONTEXT);
    out.extend_from_slice(&rid.0);
    out.push(outcome as u8);
    out.extend_from_slice(device_id.as_bytes());
    out
}

/// The desktop's answer about one phone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollAnswer {
    pub rid: Rid,
    pub outcome: Outcome,
    /// The phone the desktop answered about.
    pub device_id: Uuid,
    pub tag: [u8; 32],
}

impl EnrollAnswer {
    pub fn build(keys: &OfferKeys, outcome: Outcome, device_id: Uuid) -> Self {
        Self {
            rid: keys.rid,
            outcome,
            device_id,
            tag: keys.answer_tag(outcome, device_id),
        }
    }

    /// `ligusenrollres1:` + b64u(rid ‖ outcome ‖ device_id ‖ tag).
    pub fn encode(&self) -> String {
        let mut b = Vec::with_capacity(ANSWER_LEN);
        b.extend_from_slice(&self.rid.0);
        b.push(self.outcome as u8);
        b.extend_from_slice(self.device_id.as_bytes());
        b.extend_from_slice(&self.tag);
        format!("{ANSWER_PREFIX}{}", b64u(&b))
    }

    /// Parse the format only (the tag is checked by [`PhoneOffer::check_answer`]).
    pub fn decode(text: &str) -> Result<Self, EnrollError> {
        let body = text
            .strip_prefix(ANSWER_PREFIX)
            .ok_or(EnrollError::NotAnAnswer)?;
        let b = b64u_strict(body).ok_or(EnrollError::Malformed)?;
        if b.len() != ANSWER_LEN {
            return Err(EnrollError::Malformed);
        }
        let outcome = Outcome::from_byte(b[16]).ok_or(EnrollError::Malformed)?;
        Ok(Self {
            rid: Rid(b[0..16].try_into().expect("16 bytes")),
            outcome,
            device_id: uuid_at(&b, 17),
            tag: b[33..65].try_into().expect("32 bytes"),
        })
    }
}

// ---- the box ----

/// A box route's refusal (§7.2 and §7.3): [`BoxRefusal::status`] and
/// [`BoxRefusal::error`] are the HTTP status and the `error` field.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BoxRefusal {
    #[error("this code is for another workspace")]
    WrongWorkspace,
    #[error("the desktop that showed this code isn't a device of a person here")]
    DesktopUnknown,
    #[error("this code has expired: show a new one on the desktop")]
    OfferExpired,
    #[error("this phone's device is revoked here")]
    DeviceRevoked,
    #[error("this phone belongs to someone else here")]
    SomeoneElses,
    #[error("this box can't reach the workspace's relay")]
    RelayUnavailable,
    /// The request's own channel bound another device (§7.3 step 4).
    #[error("this request comes from another device than the phone in it")]
    DeviceMismatch,
    /// The request didn't decode or verify (§7.3 steps 1 to 3).
    #[error("{0}")]
    BadRequest(EnrollError),
}

impl BoxRefusal {
    pub fn status(&self) -> u16 {
        match self {
            BoxRefusal::WrongWorkspace
            | BoxRefusal::DesktopUnknown
            | BoxRefusal::OfferExpired
            | BoxRefusal::SomeoneElses => 409,
            BoxRefusal::DeviceRevoked | BoxRefusal::DeviceMismatch => 403,
            BoxRefusal::RelayUnavailable => 503,
            BoxRefusal::BadRequest(_) => 400,
        }
    }

    pub fn error(&self) -> &'static str {
        match self {
            BoxRefusal::WrongWorkspace => "wrong_workspace",
            BoxRefusal::DesktopUnknown => "desktop_unknown",
            BoxRefusal::OfferExpired => "offer_expired",
            BoxRefusal::DeviceRevoked => "device_revoked",
            BoxRefusal::SomeoneElses => "someone_elses",
            BoxRefusal::RelayUnavailable => "relay_unavailable",
            BoxRefusal::DeviceMismatch => "device_mismatch",
            BoxRefusal::BadRequest(_) => "bad_request",
        }
    }
}

/// The box's clock window: `now − 120 ≤ exp ≤ now + 420` (seconds).
pub fn box_exp_ok(exp: u64, now_ms: i64) -> bool {
    let exp = exp as i128 * 1000;
    exp >= now_ms as i128 - secs_ms(PAST_TOLERANCE_SECS)
        && exp <= now_ms as i128 + secs_ms(BOX_MAX_AHEAD_SECS)
}

/// `POST /v1/phone-enroll/check`, the checks that need no roster: the
/// workspace, then the expiry window. The roster checks
/// (`desktop_unknown`, `device_revoked`, `someone_elses`) and
/// `relay_unavailable` are the caller's.
pub fn box_check_offer(
    fields: &OfferFields,
    box_workspace: Uuid,
    now_ms: i64,
) -> Result<(), BoxRefusal> {
    if fields.workspace_id != box_workspace {
        return Err(BoxRefusal::WrongWorkspace);
    }
    if !box_exp_ok(fields.exp, now_ms) {
        return Err(BoxRefusal::OfferExpired);
    }
    Ok(())
}

/// `POST /v1/phone-enroll/requests`, the checks that need no roster, in the
/// design's order: decode and verify (steps 1 to 3), the channel's device
/// (step 4: `caller_device` is the device the channel binds, a provisional
/// or paired tunnel peer; `None` on the bearer and operator paths), then
/// [`box_check_offer`]. The MAC can't be checked here (the box never has
/// the secret).
pub fn box_check_request(
    text: &str,
    box_workspace: Uuid,
    caller_device: Option<Uuid>,
    now_ms: i64,
) -> Result<EnrollRequest, BoxRefusal> {
    let req = EnrollRequest::decode(text).map_err(BoxRefusal::BadRequest)?;
    if caller_device.is_some_and(|d| d != req.device_id()) {
        return Err(BoxRefusal::DeviceMismatch);
    }
    box_check_offer(&req.fields(), box_workspace, now_ms)?;
    Ok(req)
}

// ---- the desktop ----

/// What a valid request meant for the offer ([`DesktopOffer::observe`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// The first phone: the offer is now locked to it. Show the request
    /// card; **Add this phone** waits for the hold.
    First,
    /// The same phone again (the same device id and key): nothing changes.
    SamePhone,
    /// Another phone before approval: the offer is aborted ("Two phones
    /// answered this code").
    TwoPhones,
    /// Another phone after approval: raise the tripwire alert (§8.3).
    Tripwire,
}

#[derive(Debug, Clone)]
enum Phase {
    Open,
    Locked {
        request: Box<EnrollRequest>,
        first_seen_ms: i64,
    },
    Watching {
        request: Box<EnrollRequest>,
    },
    Aborted,
}

/// The desktop's offer, in memory only (one per desktop). Holds the secret
/// and both MAC keys; wiped on drop.
pub struct DesktopOffer {
    offer: Offer,
    keys: OfferKeys,
    phase: Phase,
}

impl std::fmt::Debug for DesktopOffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopOffer")
            .field("offer", &self.offer)
            .field("rid", &self.keys.rid)
            .finish_non_exhaustive()
    }
}

impl DesktopOffer {
    /// A new offer: a secret from the OS CSPRNG, `exp = now + 300`.
    pub fn start(
        workspace_id: Uuid,
        desktop_device_id: Uuid,
        now_ms: i64,
    ) -> Result<Self, getrandom::Error> {
        let exp = (now_ms.max(0) / 1000) as u64 + OFFER_TTL_SECS;
        Ok(Self::from_offer(Offer {
            workspace_id,
            desktop_device_id,
            secret: EnrollSecret::generate()?,
            exp,
        }))
    }

    /// An offer with a fixed secret and expiry (tests, vectors).
    pub fn from_offer(offer: Offer) -> Self {
        let keys = offer.keys();
        Self {
            offer,
            keys,
            phase: Phase::Open,
        }
    }

    /// The QR's text. Holds the secret: only into the QR renderer.
    pub fn qr_text(&self) -> Zeroizing<String> {
        self.offer.encode()
    }

    pub fn rid(&self) -> Rid {
        self.keys.rid
    }

    /// The offer's `exp` (Unix seconds).
    pub fn expires_at(&self) -> u64 {
        self.offer.exp
    }

    pub fn fields(&self) -> OfferFields {
        OfferFields {
            workspace_id: self.offer.workspace_id,
            desktop_device_id: self.offer.desktop_device_id,
            rid: self.keys.rid,
            exp: self.offer.exp,
        }
    }

    /// `phone-enroll:<b64u rid>:`: every relaying box's slot for this offer starts so.
    pub fn request_slot_prefix(&self) -> String {
        format!("{REQUEST_SLOT_PREFIX}{}:", self.keys.rid.to_b64u())
    }

    /// The offer's own `exp` has passed (desktop clock): stop polling, close.
    pub fn is_over(&self, now_ms: i64) -> bool {
        now_ms as i128 > self.offer.exp as i128 * 1000
    }

    pub fn is_aborted(&self) -> bool {
        matches!(self.phase, Phase::Aborted)
    }

    /// The phone the offer is locked to (before or after approval).
    pub fn locked_request(&self) -> Option<&EnrollRequest> {
        match &self.phase {
            Phase::Locked { request, .. } | Phase::Watching { request } => Some(request),
            _ => None,
        }
    }

    /// Whether the offer was approved and is now watched until `exp`.
    pub fn is_watching(&self) -> bool {
        matches!(self.phase, Phase::Watching { .. })
    }

    /// Every check of a request the desktop makes, in the design's order
    /// (§8.1): the format, `rid`, the MAC (constant time), workspace,
    /// desktop and `exp` equal to the offer's, `exp` not passed (desktop
    /// clock), `phone_sig`, then the phone's code ([`PhoneCode::from_body`]).
    /// The slot's signature ([`check_relayed_request`]) comes before, and
    /// `phone_standing` after. Stateless; [`DesktopOffer::observe`] is the
    /// one-use lock.
    pub fn check_request(&self, text: &str, now_ms: i64) -> Result<EnrollRequest, EnrollError> {
        let raw = RawRequest::parse(text)?;
        if raw.fields.rid != self.keys.rid {
            return Err(EnrollError::WrongOffer);
        }
        if !self
            .keys
            .verify_request_mac(&raw.signed(), &raw.phone_sig, &raw.mac)
        {
            return Err(EnrollError::BadMac);
        }
        if raw.fields != self.fields() {
            return Err(EnrollError::OfferMismatch);
        }
        if self.is_over(now_ms) {
            return Err(EnrollError::Expired);
        }
        raw.verify_phone_sig()?;
        let code = PhoneCode::from_body(&raw.code_body).map_err(EnrollError::BadCode)?;
        EnrollRequest::from_raw(raw, code)
    }

    /// The one-use lock (§8.1, §8.3) for a request that passed
    /// [`DesktopOffer::check_request`] and `phone_standing`. The first phone
    /// locks the offer; the same phone again changes nothing; another phone
    /// aborts the offer before approval, or trips the alert after it.
    /// Refused once the offer is aborted or over.
    pub fn observe(&mut self, request: &EnrollRequest, now_ms: i64) -> Result<Seen, EnrollError> {
        if request.rid != self.keys.rid {
            return Err(EnrollError::WrongOffer);
        }
        if self.is_over(now_ms) {
            return Err(EnrollError::Expired);
        }
        let seen = match &self.phase {
            Phase::Aborted => return Err(EnrollError::OfferClosed),
            Phase::Open => Seen::First,
            Phase::Locked {
                request: locked, ..
            } if locked.same_phone(request) => Seen::SamePhone,
            Phase::Locked { .. } => Seen::TwoPhones,
            Phase::Watching { request: locked } if locked.same_phone(request) => Seen::SamePhone,
            Phase::Watching { .. } => Seen::Tripwire,
        };
        match seen {
            Seen::First => {
                self.phase = Phase::Locked {
                    request: Box::new(request.clone()),
                    first_seen_ms: now_ms,
                }
            }
            Seen::TwoPhones => self.phase = Phase::Aborted,
            Seen::SamePhone | Seen::Tripwire => {}
        }
        Ok(seen)
    }

    /// Before certifying (`phone_enroll_offer_approve`): the offer is
    /// locked to a phone and not aborted, `rid` is this offer's, the
    /// 5-second hold has passed since the phone first appeared, the
    /// fingerprint is that phone's, and the offer isn't over. Returns the
    /// code to certify.
    pub fn check_approve(
        &self,
        rid: Rid,
        fingerprint: &str,
        now_ms: i64,
    ) -> Result<&PhoneCode, EnrollError> {
        if rid != self.keys.rid {
            return Err(EnrollError::WrongOffer);
        }
        let (request, first_seen_ms) = match &self.phase {
            Phase::Aborted => return Err(EnrollError::OfferClosed),
            Phase::Open | Phase::Watching { .. } => return Err(EnrollError::NoRequest),
            Phase::Locked {
                request,
                first_seen_ms,
            } => (request, *first_seen_ms),
        };
        if self.is_over(now_ms) {
            return Err(EnrollError::Expired);
        }
        if (now_ms as i128) < first_seen_ms as i128 + HOLD_MS as i128 {
            return Err(EnrollError::Hold);
        }
        if request.code.fingerprint() != fingerprint {
            return Err(EnrollError::FingerprintMismatch);
        }
        Ok(&request.code)
    }

    /// After the certificate was written: the offer is watched until `exp`
    /// (the tripwire). No-op unless locked.
    pub fn approved(&mut self) {
        if let Phase::Locked { request, .. } = &self.phase {
            self.phase = Phase::Watching {
                request: request.clone(),
            };
        }
    }

    /// The answer about `device_id`, tagged with `k_ans`.
    pub fn answer(&self, outcome: Outcome, device_id: Uuid) -> EnrollAnswer {
        EnrollAnswer::build(&self.keys, outcome, device_id)
    }
}

// ---- the phone ----

/// What the phone makes of the desktop's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhoneVerdict {
    /// The answer is about this phone.
    Mine(Outcome),
    /// The answer is about another phone. With [`Outcome::Certified`] it is
    /// the tripwire alarm: "Another phone was added with this code. Tell the
    /// person at the desktop to revoke it." Don't retry.
    AnotherPhone(Outcome),
}

/// The phone's scanned offer, in memory only (one at a time). Holds the
/// secret and both MAC keys; wiped on drop.
pub struct PhoneOffer {
    offer: Offer,
    keys: OfferKeys,
}

impl std::fmt::Debug for PhoneOffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhoneOffer")
            .field("offer", &self.offer)
            .field("rid", &self.keys.rid)
            .finish_non_exhaustive()
    }
}

/// The phone's clock window: `now − 120 ≤ exp ≤ now + 600` (seconds;
/// review round 2, R12).
pub fn phone_exp_check(exp: u64, now_ms: i64) -> Result<(), EnrollError> {
    let exp = exp as i128 * 1000;
    if exp < now_ms as i128 - secs_ms(PAST_TOLERANCE_SECS) {
        return Err(EnrollError::Expired);
    }
    if exp > now_ms as i128 + secs_ms(PHONE_MAX_AHEAD_SECS) {
        return Err(EnrollError::TooFarAhead);
    }
    Ok(())
}

impl PhoneOffer {
    /// After a scan: decode the offer and check its expiry on the phone's clock.
    pub fn scan(text: &str, now_ms: i64) -> Result<Self, EnrollError> {
        let offer = Offer::decode(text)?;
        phone_exp_check(offer.exp, now_ms)?;
        let keys = offer.keys();
        Ok(Self { offer, keys })
    }

    /// For `POST /v1/phone-enroll/check` and to pick the host profile.
    pub fn fields(&self) -> OfferFields {
        OfferFields {
            workspace_id: self.offer.workspace_id,
            desktop_device_id: self.offer.desktop_device_id,
            rid: self.keys.rid,
            exp: self.offer.exp,
        }
    }

    /// Past the phone's tolerance: show "Show a new code".
    pub fn is_expired(&self, now_ms: i64) -> bool {
        phone_exp_check(self.offer.exp, now_ms) == Err(EnrollError::Expired)
    }

    /// The request for this offer, signed by the phone's key (`sign`, see
    /// [`EnrollRequest::build`]).
    pub fn build_request(
        &self,
        code: &PhoneCode,
        sign: impl FnOnce(&[u8]) -> [u8; 64],
    ) -> EnrollRequest {
        EnrollRequest::build(&self.offer, code, sign)
    }

    /// [`PhoneOffer::build_request`] with v2 extensions (a key reset names
    /// the previous device).
    pub fn build_request_with(&self, code: &PhoneCode, ext: RequestExt, sign: impl FnOnce(&[u8]) -> [u8; 64]) -> EnrollRequest {
        EnrollRequest::build_with(&self.offer, code, ext, sign)
    }

    /// Check an answer the box relayed: the format, `rid`, the tag
    /// (constant time), then whose phone it is about.
    pub fn check_answer(&self, text: &str, my_device: Uuid) -> Result<PhoneVerdict, EnrollError> {
        let a = EnrollAnswer::decode(text)?;
        if a.rid != self.keys.rid {
            return Err(EnrollError::WrongOffer);
        }
        if !self.keys.verify_answer_tag(a.outcome, a.device_id, &a.tag) {
            return Err(EnrollError::BadTag);
        }
        Ok(if a.device_id == my_device {
            PhoneVerdict::Mine(a.outcome)
        } else {
            PhoneVerdict::AnotherPhone(a.outcome)
        })
    }
}

// ---- presence slots (§5.5) ----

/// `phone-enroll:<b64u rid>:<box device id>:<phone tag>`: one slot per
/// offer, relaying box and phone, so a box publishes every request it holds
/// for an offer at once and the desktop sees each one within the hold
/// (§8.3). `phone_device_id` is the device id of the request's code.
pub fn request_slot(rid: Rid, box_device_id: Uuid, phone_device_id: Uuid) -> String {
    format!(
        "{REQUEST_SLOT_PREFIX}{}:{box_device_id}:{}",
        rid.to_b64u(),
        phone_slot_tag(rid, phone_device_id)
    )
}

/// The phone's part of a request slot's name: `b64u(SHA-256(
/// "ligus-enroll-slot-v1\0" ‖ rid ‖ phone_device_id)[..16])`. Keyed by
/// `rid`, so the relay can't link a phone's slots across offers or to its
/// device id.
pub fn phone_slot_tag(rid: Rid, phone_device_id: Uuid) -> String {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    h.update(SLOT_TAG_CONTEXT);
    h.update(rid.0);
    h.update(phone_device_id.as_bytes());
    URL_SAFE_NO_PAD.encode(&h.finalize()[..16])
}

/// `phone-enroll-answer:<b64u rid>`.
pub fn answer_slot(rid: Rid) -> String {
    format!("{ANSWER_SLOT_PREFIX}{}", rid.to_b64u())
}

/// `context ‖ canonical JSON without the signature` (as `revocation_status`).
fn slot_preimage(context: &[u8], v: &impl Serialize) -> Vec<u8> {
    let mut out = context.to_vec();
    out.extend_from_slice(&serde_json::to_vec(v).unwrap_or_default());
    out
}

fn slot_fresh(at: i64, now_ms: i64) -> bool {
    let at = secs_ms(at);
    at <= now_ms as i128 + secs_ms(SLOT_MAX_AHEAD_SECS)
        && at >= now_ms as i128 - secs_ms(SLOT_MAX_AGE_SECS)
}

/// A box's request slot: `{request, relay, at}`, signed by the relaying
/// box's device key (Ed25519) over [`RELAY_CONTEXT`] and the rest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayedRequest {
    /// The `ligusenrollreq1:` text, as the phone sent it.
    pub request: String,
    /// The relaying box's device id.
    pub relay: Uuid,
    /// Unix seconds.
    pub at: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signature: Vec<u8>,
}

impl RelayedRequest {
    /// Unsigned; `now_ms` becomes `at` (seconds).
    pub fn new(request: &str, relay: Uuid, now_ms: i64) -> Self {
        Self {
            request: request.to_string(),
            relay,
            at: now_ms.div_euclid(1000),
            signature: Vec::new(),
        }
    }

    fn unsigned(&self) -> Self {
        Self {
            signature: Vec::new(),
            ..self.clone()
        }
    }

    /// The bytes the box signs.
    pub fn preimage(&self) -> Vec<u8> {
        slot_preimage(RELAY_CONTEXT, &self.unsigned())
    }

    pub fn sign(mut self, keypair: &SigningKeypair) -> Self {
        self.signature = keypair.sign(&self.preimage()).to_vec();
        self
    }

    pub fn verify(&self, device_public_key: &[u8]) -> bool {
        !self.signature.is_empty()
            && ligus_core::verify(device_public_key, &self.preimage(), &self.signature).is_ok()
    }
}

/// The desktop's answer slot: `{answer, desktop, at}`, signed by the
/// offer's desktop device key (Ed25519) over [`ANSWER_RELAY_CONTEXT`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayedAnswer {
    /// The `ligusenrollres1:` text.
    pub answer: String,
    /// The desktop's device id (the offer's `desktop_device_id`).
    pub desktop: Uuid,
    /// Unix seconds.
    pub at: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signature: Vec<u8>,
}

impl RelayedAnswer {
    pub fn new(answer: &EnrollAnswer, desktop: Uuid, now_ms: i64) -> Self {
        Self {
            answer: answer.encode(),
            desktop,
            at: now_ms.div_euclid(1000),
            signature: Vec::new(),
        }
    }

    fn unsigned(&self) -> Self {
        Self {
            signature: Vec::new(),
            ..self.clone()
        }
    }

    pub fn preimage(&self) -> Vec<u8> {
        slot_preimage(ANSWER_RELAY_CONTEXT, &self.unsigned())
    }

    pub fn sign(mut self, keypair: &SigningKeypair) -> Self {
        self.signature = keypair.sign(&self.preimage()).to_vec();
        self
    }

    pub fn verify(&self, device_public_key: &[u8]) -> bool {
        !self.signature.is_empty()
            && ligus_core::verify(device_public_key, &self.preimage(), &self.signature).is_ok()
    }
}

/// A request slot is taken only if it sits in the slot its own `rid`,
/// `relay` and phone (the device id of the request's code) name, its `at` is within 30 s ahead and 60 s old, and it is
/// signed by `relay`'s key. `service_device_key` returns a key only for a
/// certified, unrevoked device of a current **service member** (a box);
/// `None` for anyone else, so a person's device can't inject a request
/// (review R2). Returns the request's `rid`. The request itself is then
/// checked by [`DesktopOffer::check_request`].
pub fn check_relayed_request(
    slot: &str,
    r: &RelayedRequest,
    now_ms: i64,
    service_device_key: impl Fn(Uuid) -> Option<Vec<u8>>,
) -> Result<Rid, EnrollError> {
    let raw = RawRequest::parse(&r.request)?;
    let rid = raw.fields.rid;
    if slot != request_slot(rid, r.relay, uuid_at(&raw.code_body, 0)) {
        return Err(EnrollError::WrongSlot);
    }
    if !slot_fresh(r.at, now_ms) {
        return Err(EnrollError::StaleSlot);
    }
    if !service_device_key(r.relay).is_some_and(|pk| r.verify(&pk)) {
        return Err(EnrollError::BadSlotSignature);
    }
    Ok(rid)
}

/// An answer slot is taken only if it sits in the slot its own `rid`
/// names, comes from the offer's desktop (`offer_desktop`), is fresh, and
/// is signed by that desktop's key (`desktop_key`: a certified, unrevoked
/// device's key). The box checks this against its roster; the phone checks
/// the answer's tag ([`PhoneOffer::check_answer`]).
pub fn check_relayed_answer(
    slot: &str,
    a: &RelayedAnswer,
    offer_desktop: Uuid,
    now_ms: i64,
    desktop_key: impl Fn(Uuid) -> Option<Vec<u8>>,
) -> Result<Rid, EnrollError> {
    let rid = EnrollAnswer::decode(&a.answer)?.rid;
    if slot != answer_slot(rid) {
        return Err(EnrollError::WrongSlot);
    }
    if a.desktop != offer_desktop {
        return Err(EnrollError::WrongDesktop);
    }
    if !slot_fresh(a.at, now_ms) {
        return Err(EnrollError::StaleSlot);
    }
    if !desktop_key(a.desktop).is_some_and(|pk| a.verify(&pk)) {
        return Err(EnrollError::BadSlotSignature);
    }
    Ok(rid)
}

/// The request slot's blob, sealed under the keyring's seal key at the
/// authorized head (`{"phoneEnrollRequest": <sealed>}`).
pub fn seal_relayed_request(ring: &KeyringEngine, r: &RelayedRequest) -> Option<Value> {
    crate::revocation_status::seal(ring, REQUEST_SLOT_FIELD, r)
}

/// Every request slot in a presence listing that opens at or after the
/// authorized head, with its slot name (check each with
/// [`check_relayed_request`]).
pub fn open_relayed_requests(
    blobs: &HashMap<String, Value>,
    ring: &KeyringEngine,
) -> Vec<(String, RelayedRequest)> {
    crate::revocation_status::open_all(blobs, ring, REQUEST_SLOT_FIELD, REQUEST_SLOT_PREFIX)
}

/// The answer slot's blob (`{"phoneEnrollAnswer": <sealed>}`).
pub fn seal_relayed_answer(ring: &KeyringEngine, a: &RelayedAnswer) -> Option<Value> {
    crate::revocation_status::seal(ring, ANSWER_SLOT_FIELD, a)
}

/// Every answer slot in a presence listing (check each with [`check_relayed_answer`]).
pub fn open_relayed_answers(
    blobs: &HashMap<String, Value>,
    ring: &KeyringEngine,
) -> Vec<(String, RelayedAnswer)> {
    crate::revocation_status::open_all(blobs, ring, ANSWER_SLOT_FIELD, ANSWER_SLOT_PREFIX)
}

#[cfg(test)]
mod tests;

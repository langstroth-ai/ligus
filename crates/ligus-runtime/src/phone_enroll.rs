//! "Add this phone": certifying a phone's P-256 Secure Enclave key as one of
//! its person's devices.
//!
//! 1. The phone shows its **phone code** (`ligusphone1:…`, as text to copy
//!    and as a QR code): its workspace device id, its P-256 public key (65
//!    bytes, SEC1 uncompressed), a label, and a P-256 signature by that key
//!    over all of it (proof of possession: a code can't name a key its
//!    sender doesn't hold), plus the key's fingerprint on screen.
//! 2. On the person's desktop (or `ligus phones certify` on a CLI identity of
//!    theirs), the code is pasted or scanned, the fingerprint is compared
//!    with the phone's screen, and the person confirms with their passkey
//!    (the presence the rules require for every phone enrollment).
//! 3. The desktop appends `DeviceCertificateAdded` with a P-256 certificate
//!    ([`crate::ChatService::certify_phone`]): the phone tier, capped by its
//!    person's role. The phone's box sees it after its next pull, and the
//!    phone can then pair with the box (an uncertified phone's pairing is
//!    refused with `device_not_certified`, see `docs/relay-tunnel.md`).
//!
//! The device id is never chosen: it is the one the phone's key derives
//! ([`ligus_core::crypto::device_id_for`] over `p256` and the 65-byte SEC1
//! uncompressed key), and a code naming any other id is refused on decode
//! ([`check_device_id`]), before anything is shown or certified.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use uuid::Uuid;

/// Prefix of a phone code.
pub const CODE_PREFIX: &str = "ligusphone1:";
/// Longest label carried in a code, in bytes.
pub const MAX_LABEL: usize = 64;
/// Length of a P-256 public key (SEC1 uncompressed).
pub const P256_KEY_LEN: usize = 65;
/// device(16) ‖ key(65) ‖ sig(64).
const FIXED: usize = 16 + P256_KEY_LEN + 64;
// FROZEN signature context (new in 0.1, never renamed): every phone code is
// signed over it. Pinned in the tests below and in the iOS signer.
const PREIMAGE_TAG: &[u8] = b"ligus-phone-code-v1\0";

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum PhoneCodeError {
    #[error("not a Ligus phone code (expected {CODE_PREFIX}...)")]
    NotACode,
    #[error("the phone code is damaged or incomplete")]
    Malformed,
    #[error("the phone code's key isn't a P-256 public key")]
    BadKey,
    #[error("the phone code's signature doesn't match its key")]
    BadSignature,
    #[error("the phone's device id doesn't belong to its key: {0}")]
    DeviceIdMismatch(String),
}

/// What a phone asks to be certified as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneCode {
    pub device_id: Uuid,
    /// 65-byte SEC1 uncompressed P-256 public key.
    pub public_key: Vec<u8>,
    pub label: String,
    /// ECDSA P-256 over [`preimage`], 64 bytes `r ‖ s`, low S.
    pub signature: [u8; 64],
}

/// Characters a label never carries: controls, and the bidi overrides,
/// isolates and marks and invisible format characters that can reorder or
/// hide what a person reads next to the fingerprint (on the desktop's sheet,
/// in a terminal).
pub(crate) fn hidden_or_reordering(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}'
        )
}

pub(crate) fn clamp_label(label: &str) -> String {
    // Drop what a label never carries first, then trim, so what is left is
    // already trimmed (a dropped character can't leave a space at an end);
    // trim again after the cut. Idempotent: a code's label is refused unless
    // it is its own clamp.
    let kept: String = label.chars().filter(|c| !hidden_or_reordering(*c)).collect();
    let mut out = String::new();
    for c in kept.trim().chars() {
        if out.len() + c.len_utf8() > MAX_LABEL {
            break;
        }
        out.push(c);
    }
    out.trim_end().to_string()
}

/// What the phone signs: `"ligus-phone-code-v1\0" ‖ device_id(16) ‖ key(65)
/// ‖ label (UTF-8)`.
pub fn preimage(device_id: Uuid, public_key: &[u8], label: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(PREIMAGE_TAG.len() + 16 + public_key.len() + label.len());
    out.extend_from_slice(PREIMAGE_TAG);
    out.extend_from_slice(device_id.as_bytes());
    out.extend_from_slice(public_key);
    out.extend_from_slice(label.as_bytes());
    out
}

/// The derived-id rule: a phone's device id is the one its P-256 key
/// derives (`ligus_core::crypto::device_id_for(SigAlg::P256, key)`, over the
/// 65-byte SEC1 uncompressed point, `0x04 ‖ X ‖ Y`). Any other id (chosen,
/// another device's, the Ed25519 derivation of the same bytes, the id of the
/// compressed form) is refused: a certificate for it would never verify.
pub fn check_device_id(device_id: Uuid, public_key: &[u8]) -> Result<(), String> {
    if public_key.len() != P256_KEY_LEN || public_key[0] != 0x04 {
        return Err("a phone's key is a 65-byte uncompressed P-256 point".into());
    }
    let derived = ligus_core::crypto::device_id_for(ligus_core::crypto::SigAlg::P256, public_key);
    if device_id != derived {
        return Err(format!("{device_id} isn't the id its key derives ({derived})"));
    }
    Ok(())
}

impl PhoneCode {
    /// Build a code, signing [`preimage`] with `sign` (ECDSA P-256 by the
    /// key `public_key` names, 64 bytes low S): the reference for the iOS
    /// app's Secure Enclave signer.
    pub fn new_signed(device_id: Uuid, public_key: Vec<u8>, label: &str, sign: impl FnOnce(&[u8]) -> [u8; 64]) -> Self {
        let label = clamp_label(label);
        let signature = sign(&preimage(device_id, &public_key, &label));
        Self { device_id, public_key, label, signature }
    }

    /// A test phone's code.
    #[cfg(test)]
    pub fn new(device_id: Uuid, key: &ligus_core::testkit::Phone, label: &str) -> Self {
        Self::new_signed(device_id, key.public_key(), label, |m| ligus_core::crypto::sign_p256(&key.key, m))
    }

    /// The `ligusphone1:` code: base64url (no padding) of device(16) ‖
    /// key(65) ‖ sig(64) ‖ label (UTF-8, at most 64 bytes).
    pub fn encode(&self) -> String {
        format!("{CODE_PREFIX}{}", URL_SAFE_NO_PAD.encode(self.body()))
    }

    /// The code's bytes without the prefix or the base64url: device(16) ‖
    /// key(65) ‖ sig(64) ‖ label. Carried unchanged inside a reverse-QR
    /// request (`crate::phone_enroll_qr`).
    pub fn body(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(FIXED + self.label.len());
        b.extend_from_slice(self.device_id.as_bytes());
        b.extend_from_slice(&self.public_key);
        b.extend_from_slice(&self.signature);
        b.extend_from_slice(self.label.as_bytes());
        b
    }

    /// Parse and verify a code: its signature, its label, and the
    /// derived-id rule ([`check_device_id`]). Whitespace and line breaks are
    /// ignored.
    pub fn decode(code: &str) -> Result<Self, PhoneCodeError> {
        let compact: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        let body = compact.strip_prefix(CODE_PREFIX).ok_or(PhoneCodeError::NotACode)?;
        let b = URL_SAFE_NO_PAD.decode(body.as_bytes()).map_err(|_| PhoneCodeError::Malformed)?;
        Self::from_body(&b)
    }

    /// [`Self::decode`] of a code's bytes ([`Self::body`]): every check
    /// `decode` makes (length, label clamp, signature, derived id).
    pub fn from_body(b: &[u8]) -> Result<Self, PhoneCodeError> {
        if b.len() < FIXED || b.len() > FIXED + MAX_LABEL {
            return Err(PhoneCodeError::Malformed);
        }
        let device_id = Uuid::from_slice(&b[..16]).map_err(|_| PhoneCodeError::Malformed)?;
        let public_key = b[16..16 + P256_KEY_LEN].to_vec();
        let signature: [u8; 64] = b[16 + P256_KEY_LEN..FIXED].try_into().map_err(|_| PhoneCodeError::Malformed)?;
        let label = std::str::from_utf8(&b[FIXED..]).map_err(|_| PhoneCodeError::Malformed)?.to_string();
        if label != clamp_label(&label) {
            return Err(PhoneCodeError::Malformed);
        }
        let c = Self { device_id, public_key, label, signature };
        c.verify()?;
        check_device_id(c.device_id, &c.public_key).map_err(PhoneCodeError::DeviceIdMismatch)?;
        Ok(c)
    }

    /// The key is a P-256 key and signed this code.
    pub fn verify(&self) -> Result<(), PhoneCodeError> {
        if self.public_key.len() != P256_KEY_LEN {
            return Err(PhoneCodeError::BadKey);
        }
        ligus_core::crypto::verify_p256(&self.public_key, &preimage(self.device_id, &self.public_key, &self.label), &self.signature).map_err(|e| match e {
            ligus_core::CryptoError::BadKey => PhoneCodeError::BadKey,
            _ => PhoneCodeError::BadSignature,
        })
    }

    /// `XXXX-XXXX-XXXX-XXXX-XXXX` of the key (the first 10 bytes of
    /// SHA-256 of the 65 key bytes, upper-case hex): what the phone shows and
    /// the person compares.
    pub fn fingerprint(&self) -> String {
        crate::device_enroll::fingerprint(&self.public_key)
    }
}

/// Where a phone stands before certifying it, for the confirmation sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhoneStanding {
    /// Not on this workspace's roster: certify it.
    New,
    /// Already certified for this account with this key: nothing to do.
    AlreadyMine,
    /// The id is certified with another key, or for another account, or as
    /// a desktop or box: refused (a phone code can't take over a device).
    Conflict(String),
    /// Revoked here: the phone needs a new device key and id.
    Revoked,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_round_trips_and_verifies() {
        let p = ligus_core::testkit::Phone::new();
        let c = PhoneCode::new(p.device_id, &p, "  Mara's iPhone\n");
        assert_eq!(c.label, "Mara's iPhone");
        let text = c.encode();
        assert!(text.starts_with(CODE_PREFIX));
        // A copy with line breaks still parses.
        let wrapped: String = text.chars().enumerate().flat_map(|(i, ch)| if i % 20 == 19 { vec![ch, '\n'] } else { vec![ch] }).collect();
        let d = PhoneCode::decode(&wrapped).unwrap();
        assert_eq!(d, c);
        assert_eq!(d.public_key, p.public_key());
        assert_eq!(d.fingerprint(), crate::device_enroll::fingerprint(&p.public_key()));
    }

    /// The golden fingerprint the iOS app's own computation must match
    /// (docs/ios-app.md): SHA-256 of the 65-byte key 04 01 02 .. 40, first
    /// 10 bytes, upper-case hex in five groups of four.
    #[test]
    fn the_fingerprint_is_80_bits_in_five_groups() {
        let key: Vec<u8> = std::iter::once(4u8).chain(1..=64u8).collect();
        let c = PhoneCode::new_signed(Uuid::nil(), key, "p", |_| [0u8; 64]);
        assert_eq!(c.fingerprint(), "0ED3-A6AB-957F-F6F5-9A96");
    }

    #[test]
    fn a_forged_or_damaged_code_is_refused() {
        let p = ligus_core::testkit::Phone::new();
        let mallory = ligus_core::testkit::Phone::new();
        let c = PhoneCode::new(p.device_id, &p, "phone");
        // Another key, same signature: refused.
        let mut forged = c.clone();
        forged.public_key = mallory.public_key();
        assert_eq!(PhoneCode::decode(&forged.encode()), Err(PhoneCodeError::BadSignature));
        // Another device id under the same signature: refused.
        let mut moved = c.clone();
        moved.device_id = Uuid::new_v4();
        assert_eq!(PhoneCode::decode(&moved.encode()), Err(PhoneCodeError::BadSignature));
        // A relabelled code: refused.
        let mut relabelled = c.clone();
        relabelled.label = "Boss's phone".into();
        assert_eq!(PhoneCode::decode(&relabelled.encode()), Err(PhoneCodeError::BadSignature));
        // Not a P-256 key.
        let mut bad = c.clone();
        bad.public_key = vec![4u8; 65];
        assert_eq!(PhoneCode::decode(&bad.encode()), Err(PhoneCodeError::BadKey));
        assert_eq!(PhoneCode::decode("hivedev1:abc"), Err(PhoneCodeError::NotACode));
        assert_eq!(PhoneCode::decode("ligusphone1:!!"), Err(PhoneCodeError::Malformed));
        assert_eq!(PhoneCode::decode(&format!("{CODE_PREFIX}{}", URL_SAFE_NO_PAD.encode([0u8; 40]))), Err(PhoneCodeError::Malformed));
    }

    /// A code's device id is the one its key derives. A chosen id, another
    /// device's id, the Ed25519 derivation of the same key bytes or the id
    /// of the key's compressed form: each refused on decode and by
    /// `certify_phone_code`, even though the phone's signature covers it.
    #[test]
    fn a_code_names_only_the_id_its_key_derives() {
        use ligus_core::crypto::{device_id_for, SigAlg};
        let p = ligus_core::testkit::Phone::new();
        let key = p.public_key();
        assert_eq!(p.device_id, device_id_for(SigAlg::P256, &key));
        assert!(PhoneCode::decode(&PhoneCode::new(p.device_id, &p, "phone").encode()).is_ok());
        let other = ligus_core::testkit::Phone::new();
        let compressed = {
            let mut c = vec![if key[64] & 1 == 0 { 2u8 } else { 3u8 }];
            c.extend_from_slice(&key[1..33]);
            c
        };
        for (why, id) in [
            ("a chosen id", Uuid::new_v4()),
            ("the nil id", Uuid::nil()),
            ("another phone's id", other.device_id),
            ("the Ed25519 derivation of the same bytes", device_id_for(SigAlg::Ed25519, &key)),
            ("the compressed key's id", device_id_for(SigAlg::P256, &compressed)),
            ("the derived id with the version cleared", {
                let mut b = *p.device_id.as_bytes();
                b[6] &= 0x0f;
                Uuid::from_bytes(b)
            }),
        ] {
            let code = PhoneCode::new(id, &p, "phone");
            assert!(code.verify().is_ok(), "{why}: the phone signed it");
            assert!(matches!(PhoneCode::decode(&code.encode()), Err(PhoneCodeError::DeviceIdMismatch(_))), "{why}");
            let (mut svc, wid) = desktop();
            assert!(svc.certify_phone_code(wid, &code).is_err(), "{why}");
            assert!(certs(&svc, wid).is_empty(), "{why}");
        }
        assert!(check_device_id(device_id_for(SigAlg::P256, &compressed), &compressed).is_err(), "a compressed key is refused");
    }

    /// A label can't carry bidi overrides, isolates or invisible format
    /// characters: on the confirmation sheet (and in a terminal) they can
    /// reorder or hide what the person reads next to the fingerprint.
    #[test]
    fn a_label_carries_no_bidi_or_invisible_characters() {
        let p = ligus_core::testkit::Phone::new();
        for bad in ["\u{202E}", "\u{2066}", "\u{2069}", "\u{200B}", "\u{200F}", "\u{FEFF}", "\u{061C}", "\u{2060}"] {
            let c = PhoneCode::new(p.device_id, &p, &format!("Ana{bad}enohp"));
            assert_eq!(c.label, "Anaenohp", "{bad:?} dropped when signing");
            // A phone that signs one anyway: refused, not shown.
            let label = format!("Ana{bad}enohp");
            let raw = PhoneCode {
                device_id: p.device_id,
                public_key: p.public_key(),
                signature: ligus_core::crypto::sign_p256(&p.key, &preimage(p.device_id, &p.public_key(), &label)),
                label,
            };
            assert_eq!(PhoneCode::decode(&raw.encode()), Err(PhoneCodeError::Malformed), "{bad:?}");
        }
        // Whatever a phone is named, the code it signs decodes.
        let long = format!("{} tail", "x".repeat(MAX_LABEL - 1));
        for name in ["\u{200B} Ana", "\u{7} Ana", "Ana \u{202E}", "Ana\u{7} ", long.as_str(), "\u{2066}\u{2069}"] {
            let c = PhoneCode::new(p.device_id, &p, name);
            assert_eq!(PhoneCode::decode(&c.encode()).map(|d| d.label), Ok(c.label.clone()), "{name:?}");
        }
    }

    #[test]
    fn preimage_layout_is_pinned() {
        let id = Uuid::from_bytes([7u8; 16]);
        let m = preimage(id, &[4u8; 65], "L");
        let mut want = b"ligus-phone-code-v1\0".to_vec();
        want.extend_from_slice(&[7u8; 16]);
        want.extend_from_slice(&[4u8; 65]);
        want.push(b'L');
        assert_eq!(m, want);
    }

    // ── Certifying through ChatService ─────────────────────────────────────

    use crate::chat_service::{ChatError, ChatService, PresenceProvider, PresenceRequest};
    use ligus_core::{workspace_config_session_id, ActorIdentity, ActorKind, SessionEvent, SigningKeypair};

    /// A person's desktop (test presence: the account's test recovery key)
    /// on a fresh workspace.
    fn desktop() -> (ChatService, Uuid) {
        let store = crate::EventStore::open_in_memory().unwrap();
        let account = Uuid::new_v4();
        let mut author = ActorIdentity::new(account.to_string(), "Ana", ActorKind::Human);
        author.account_id = Some(account);
        let mut svc = ChatService::new(store, SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), author);
        let wid = Uuid::new_v4();
        svc.ensure_workspace_config(wid).unwrap();
        (svc, wid)
    }

    fn certs(svc: &ChatService, wid: Uuid) -> Vec<ligus_core::crypto::DeviceCertificate> {
        svc.store()
            .list(workspace_config_session_id(wid))
            .unwrap()
            .into_iter()
            .filter_map(|e| match e.payload {
                SessionEvent::DeviceCertificateAdded { certificate } if certificate.alg == ligus_core::crypto::SigAlg::P256 => Some(certificate),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn add_this_phone_certifies_the_presented_id_at_the_phone_tier() {
        let (mut svc, wid) = desktop();
        let phone = ligus_core::testkit::Phone::new();
        let code = PhoneCode::decode(&PhoneCode::new(phone.device_id, &phone, "Ana's iPhone").encode()).unwrap();
        assert_eq!(svc.phone_standing(wid, &code).unwrap(), PhoneStanding::New);
        assert_eq!(svc.certify_phone_code(wid, &code).unwrap(), phone.device_id);
        let c = certs(&svc, wid);
        assert_eq!(c.len(), 1);
        assert_eq!((c[0].device_id, &c[0].device_public_key), (phone.device_id, &phone.public_key()));
        assert_eq!(c[0].account_id, svc.author().account_id.unwrap());
        // The fold took it, as a phone (the capped tier), with presence.
        let (_, t) = svc.store().load_config(wid, None, None).unwrap();
        let d = t.current().devices.get(&phone.device_id).expect("on the roster");
        assert!(d.phone && !d.revoked);
        let env = svc
            .store()
            .list(workspace_config_session_id(wid))
            .unwrap()
            .into_iter()
            .find(|e| matches!(&e.payload, SessionEvent::DeviceCertificateAdded { certificate } if certificate.device_id == phone.device_id))
            .unwrap();
        assert!(env.actor_stamp.as_ref().and_then(|s| s.presence.as_ref()).is_some(), "confirmed with the person's presence");
        // U5: named in the same step, a synced name every device shows.
        let me = svc.author().account_id.unwrap();
        assert_eq!(svc.device_label_of(wid, phone.device_id, me).as_deref(), Some("Ana's iPhone"));
        // Again: nothing new.
        assert_eq!(svc.phone_standing(wid, &code).unwrap(), PhoneStanding::AlreadyMine);
        svc.certify_phone_code(wid, &code).unwrap();
        assert_eq!(certs(&svc, wid).len(), 1);
    }

    /// U5: a phone that chose no name is "Phone · added 24 Nov", never
    /// "Device 1a2b3c4d", and renameable afterwards.
    #[test]
    fn a_phone_without_a_name_gets_a_dated_one_at_enrolment() {
        let (mut svc, wid) = desktop();
        let phone = ligus_core::testkit::Phone::new();
        let code = PhoneCode::decode(&PhoneCode::new(phone.device_id, &phone, "  ").encode()).unwrap();
        svc.certify_phone_code(wid, &code).unwrap();
        let me = svc.author().account_id.unwrap();
        let label = svc.device_label_of(wid, phone.device_id, me).expect("named at enrolment");
        assert!(label.starts_with("Phone \u{00B7} added "), "{label}");
        assert!(!label.contains("Device"));
        assert!(svc.set_device_label(wid, phone.device_id, "Work phone").unwrap());
        assert_eq!(svc.device_label_of(wid, phone.device_id, me).as_deref(), Some("Work phone"));
        let at = ligus_core::Timestamp::from_unix_seconds(1_795_521_600); // 24 Nov 2026
        assert_eq!(crate::chat_service::phone_label("", at), "Phone \u{00B7} added 24 Nov");
        assert_eq!(crate::chat_service::phone_label("Ana's iPhone", at), "Ana's iPhone");
    }

    #[test]
    fn a_phone_code_cannot_take_over_a_device() {
        let (mut svc, wid) = desktop();
        let phone = ligus_core::testkit::Phone::new();
        svc.certify_phone_code(wid, &PhoneCode::new(phone.device_id, &phone, "p")).unwrap();
        // Same id, another key.
        let other = ligus_core::testkit::Phone::new();
        let swap = PhoneCode::new(phone.device_id, &other, "p");
        assert!(matches!(svc.phone_standing(wid, &swap).unwrap(), PhoneStanding::Conflict(_)));
        assert!(svc.certify_phone_code(wid, &swap).is_err());
        // This desktop's own id.
        let me = PhoneCode::new(svc.device_id(), &other, "p");
        assert!(svc.certify_phone_code(wid, &me).is_err());
        assert_eq!(certs(&svc, wid).len(), 1);
        // A revoked phone needs a new key and id.
        svc.revoke_device(Uuid::nil(), wid, phone.device_id).unwrap();
        let again = PhoneCode::new(phone.device_id, &phone, "p");
        assert_eq!(svc.phone_standing(wid, &again).unwrap(), PhoneStanding::Revoked);
        assert!(svc.certify_phone_code(wid, &again).unwrap_err().to_string().contains("revoked"));
    }

    struct Cancel;
    impl PresenceProvider for Cancel {
        fn assert(&self, _: &PresenceRequest) -> std::result::Result<ligus_core::presence::PresenceAssertion, String> {
            Err("cancelled".into())
        }
    }

    #[test]
    fn no_passkey_no_phone() {
        let (mut svc, wid) = desktop();
        // Register the account's recovery key first (test presence), then
        // put a provider that cancels in its place.
        let first = ligus_core::testkit::Phone::new();
        svc.certify_phone_code(wid, &PhoneCode::new(first.device_id, &first, "p")).unwrap();
        svc.set_presence_provider(Some(std::sync::Arc::new(Cancel)));
        let phone = ligus_core::testkit::Phone::new();
        let err = svc.certify_phone_code(wid, &PhoneCode::new(phone.device_id, &phone, "p")).unwrap_err();
        assert!(matches!(err, ChatError::Unauthorized(_)), "{err}");
        assert_eq!(certs(&svc, wid).len(), 1, "nothing appended without presence");
    }

    /// The passkey sheet names the key being certified, by the fingerprint
    /// the person compared with the phone's screen: what the passkey
    /// confirms reads as that phone, not just an id.
    #[test]
    fn the_passkey_sheet_names_the_fingerprint() {
        struct Rec(ligus_core::presence::RecoveryKey, std::sync::Mutex<Vec<String>>);
        impl PresenceProvider for Rec {
            fn assert(&self, req: &PresenceRequest) -> std::result::Result<ligus_core::presence::PresenceAssertion, String> {
                self.1.lock().unwrap().push(req.summary.clone());
                Ok(self.0.assert(&req.challenge))
            }
        }
        let (mut svc, wid) = desktop();
        let first = ligus_core::testkit::Phone::new();
        svc.certify_phone_code(wid, &PhoneCode::new(first.device_id, &first, "p")).unwrap();
        let account = svc.author().account_id.unwrap();
        let rec = std::sync::Arc::new(Rec(ligus_core::testkit::test_recovery_key(account), Default::default()));
        svc.set_presence_provider(Some(rec.clone()));
        let phone = ligus_core::testkit::Phone::new();
        let code = PhoneCode::new(phone.device_id, &phone, "p");
        svc.certify_phone_code(wid, &code).unwrap();
        let asked = rec.1.lock().unwrap().clone();
        assert_eq!(asked.len(), 1);
        assert!(asked[0].contains(&code.fingerprint()), "{}", asked[0]);
    }

    /// A P-256 certificate for a device id already certified here (as a
    /// desktop or box, or as a phone with another key) is refused, with or
    /// without presence. Before ids were derived it counted as re-publishing
    /// a certificate in force, so it needed no passkey, and it turned the
    /// device into a phone with the new key: a holder of the account key (a
    /// copied data dir) could swap a key of their own onto another of the
    /// person's devices and keep it past the copied device's revocation. Now
    /// such a certificate names an id its key doesn't derive, refused here
    /// and by the fold wherever it sorts (ligus-core
    /// `a_phone_certificate_never_takes_over_a_device_in_the_fold`).
    #[test]
    fn the_fold_refuses_a_phone_certificate_over_an_existing_device() {
        let (mut svc, wid) = desktop();
        let first = ligus_core::testkit::Phone::new();
        svc.certify_phone_code(wid, &PhoneCode::new(first.device_id, &first, "p")).unwrap();
        let me = svc.device_id();
        let mallory = ligus_core::testkit::Phone::new();
        let roster = |svc: &ChatService| svc.store().load_config(wid, None, None).unwrap().1.current().devices.clone();
        let before = roster(&svc);
        // No passkey at hand.
        svc.set_presence_provider(Some(std::sync::Arc::new(Cancel)));
        assert!(svc.certify_phone(wid, me, &mallory.public_key()).is_err(), "this desktop's id, another key, no passkey");
        assert!(svc.certify_phone(wid, first.device_id, &mallory.public_key()).is_err(), "a phone's id, another key, no passkey");
        // Even with the person's passkey.
        svc.set_presence_provider(None);
        assert!(svc.certify_phone(wid, me, &mallory.public_key()).is_err(), "this desktop's id, with presence");
        assert!(svc.certify_phone(wid, first.device_id, &mallory.public_key()).is_err(), "a phone's id, with presence");
        let after = roster(&svc);
        assert_eq!(after.get(&me).map(|d| (d.phone, d.public_key.clone())), before.get(&me).map(|d| (d.phone, d.public_key.clone())));
        assert_eq!(after.get(&first.device_id).map(|d| d.public_key.clone()), Some(first.public_key()));
        // Re-publishing the phone's own certificate still needs nothing.
        svc.set_presence_provider(Some(std::sync::Arc::new(Cancel)));
        svc.certify_phone(wid, first.device_id, &first.public_key()).unwrap();
    }

    #[test]
    fn a_box_never_certifies_a_phone() {
        let store = crate::EventStore::open_in_memory().unwrap();
        let account = Uuid::new_v4();
        let mut author = ActorIdentity::new(account.to_string(), "box", ActorKind::Service);
        author.account_id = Some(account);
        let mut svc = ChatService::new(store, SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), author);
        let wid = Uuid::new_v4();
        let phone = ligus_core::testkit::Phone::new();
        assert!(svc.certify_phone_code(wid, &PhoneCode::new(phone.device_id, &phone, "p")).is_err());
    }
}

use super::*;
use ligus_core::testkit::Phone;
use proptest::prelude::*;

const NOW_MS: i64 = 1_800_000_000_000;
const NOW: u64 = 1_800_000_000;

fn phone_code(p: &Phone, label: &str) -> PhoneCode {
    PhoneCode::new(p.device_id, p, label)
}

fn signer(p: &Phone) -> impl FnOnce(&[u8]) -> [u8; 64] + '_ {
    |m| ligus_core::crypto::sign_p256(&p.key, m)
}

fn offer_with(secret: u8, exp: u64) -> Offer {
    Offer {
        workspace_id: Uuid::from_u128(0xa1),
        desktop_device_id: Uuid::from_u128(0xd1),
        secret: EnrollSecret::from_bytes([secret; 32]),
        exp,
    }
}

fn desk(secret: u8) -> DesktopOffer {
    DesktopOffer::from_offer(offer_with(secret, NOW + OFFER_TTL_SECS))
}

fn req_text(offer: &Offer, p: &Phone, label: &str) -> String {
    EnrollRequest::build(offer, &phone_code(p, label), signer(p)).encode()
}

fn body_of(text: &str, prefix: &str) -> Vec<u8> {
    URL_SAFE_NO_PAD
        .decode(text.strip_prefix(prefix).unwrap())
        .unwrap()
}

fn with_body(prefix: &str, b: &[u8]) -> String {
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(b))
}

/// `(r, n - s)`: the same signature with a high S.
pub(crate) fn high_s(sig: &[u8; 64]) -> [u8; 64] {
    const N: [u8; 32] = [
        0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xbc, 0xe6, 0xfa, 0xad, 0xa7, 0x17, 0x9e, 0x84, 0xf3, 0xb9, 0xca, 0xc2, 0xfc, 0x63,
        0x25, 0x51,
    ];
    let mut out = *sig;
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let d = N[i] as i16 - sig[32 + i] as i16 - borrow;
        out[32 + i] = d.rem_euclid(256) as u8;
        borrow = (d < 0) as i16;
    }
    out
}

/// The wire identifiers are frozen: a change here is a protocol break.
#[test]
fn wire_constants_are_pinned() {
    assert_eq!(OFFER_PREFIX, "ligusenroll1:");
    assert_eq!(REQUEST_PREFIX, "ligusenrollreq1:");
    assert_eq!(ANSWER_PREFIX, "ligusenrollres1:");
    assert_eq!(HKDF_SALT, b"ligus-enroll-qr-v1");
    assert_eq!(INFO_RID, b"rid");
    assert_eq!(INFO_REQUEST_MAC, b"request-mac");
    assert_eq!(INFO_ANSWER_MAC, b"answer-mac");
    assert_eq!(REQUEST_CONTEXT, b"ligus-enroll-request-v1\0");
    assert_eq!(ANSWER_CONTEXT, b"ligus-enroll-answer-v1\0");
    assert_eq!(RELAY_CONTEXT, b"ligus-enroll-relay-v1\0");
    assert_eq!(ANSWER_RELAY_CONTEXT, b"ligus-enroll-answer-relay-v1\0");
    assert_eq!(REQUEST_SLOT_PREFIX, "phone-enroll:");
    assert_eq!(SLOT_TAG_CONTEXT, b"ligus-enroll-slot-v1\0");
    assert_eq!(ANSWER_SLOT_PREFIX, "phone-enroll-answer:");
    assert_eq!(REQUEST_SLOT_FIELD, "phoneEnrollRequest");
    assert_eq!(ANSWER_SLOT_FIELD, "phoneEnrollAnswer");
    assert_eq!(
        (
            OFFER_TTL_SECS,
            PAST_TOLERANCE_SECS,
            BOX_MAX_AHEAD_SECS,
            PHONE_MAX_AHEAD_SECS
        ),
        (300, 120, 420, 600)
    );
    assert_eq!(
        (
            SLOT_MAX_AHEAD_SECS,
            SLOT_MAX_AGE_SECS,
            HOLD_MS,
            MAX_WAIT_SECS,
            REPUBLISH_SECS
        ),
        (30, 60, 5_000, 25, 10)
    );
    assert_eq!(
        (OFFER_LEN, REQUEST_FIXED, CODE_MIN, CODE_MAX, ANSWER_LEN),
        (72, 152, 145, 209, 65)
    );
    // The outcome bytes.
    assert_eq!(
        (
            Outcome::Certified as u8,
            Outcome::Declined as u8,
            Outcome::Refused as u8
        ),
        (1, 2, 3)
    );
    // A slot prefix never captures the other kind.
    assert!(!answer_slot(Rid([0; 16])).starts_with(REQUEST_SLOT_PREFIX));
    // The existing identifiers this builds on are untouched.
    assert_eq!(crate::phone_enroll::CODE_PREFIX, "ligusphone1:");
}

#[test]
fn the_offer_is_96_characters_after_its_prefix() {
    let o = offer_with(7, NOW + 300);
    let text = o.encode();
    assert_eq!(text.len(), OFFER_PREFIX.len() + 96);
    let back = Offer::decode(&text).unwrap();
    assert_eq!(
        (
            back.workspace_id,
            back.desktop_device_id,
            back.exp,
            back.secret.0
        ),
        (o.workspace_id, o.desktop_device_id, o.exp, o.secret.0)
    );
}

#[test]
fn a_request_round_trips_and_every_side_accepts_it() {
    let p = Phone::new();
    let d = desk(1);
    let text = req_text(&d.offer, &p, "Ana's iPhone");
    let req = EnrollRequest::decode(&text).unwrap();
    assert_eq!(req.encode(), text);
    assert_eq!(req.code, phone_code(&p, "Ana's iPhone"));
    assert_eq!(req.fields(), d.fields());
    assert_eq!(
        box_check_request(&text, d.offer.workspace_id, Some(p.device_id), NOW_MS).unwrap(),
        req
    );
    assert_eq!(d.check_request(&text, NOW_MS).unwrap(), req);
    // The byte length is 152 + the code's.
    let n = body_of(&text, REQUEST_PREFIX).len();
    assert_eq!(
        n,
        REQUEST_FIXED + phone_code(&p, "Ana's iPhone").body().len()
    );
}

#[test]
fn the_phones_side_builds_what_the_desktop_checks() {
    let p = Phone::new();
    let d = desk(2);
    let po = PhoneOffer::scan(&d.qr_text(), NOW_MS).unwrap();
    assert_eq!(po.fields(), d.fields());
    let text = po
        .build_request(&phone_code(&p, "phone"), signer(&p))
        .encode();
    let req = d.check_request(&text, NOW_MS).unwrap();
    assert_eq!(req.device_id(), p.device_id);
    let answer = d.answer(Outcome::Certified, p.device_id).encode();
    assert_eq!(
        po.check_answer(&answer, p.device_id),
        Ok(PhoneVerdict::Mine(Outcome::Certified))
    );
    assert_eq!(
        po.check_answer(&answer, Uuid::from_u128(9)),
        Ok(PhoneVerdict::AnotherPhone(Outcome::Certified))
    );
    for o in [Outcome::Declined, Outcome::Refused] {
        assert_eq!(
            po.check_answer(&d.answer(o, p.device_id).encode(), p.device_id),
            Ok(PhoneVerdict::Mine(o))
        );
    }
}

#[test]
fn every_bit_flip_truncation_and_extension_of_a_request_is_refused() {
    let p = Phone::new();
    let d = desk(3);
    let b = body_of(&req_text(&d.offer, &p, "flip"), REQUEST_PREFIX);
    for i in 0..b.len() * 8 {
        let mut m = b.clone();
        m[i / 8] ^= 1 << (i % 8);
        assert!(
            d.check_request(&with_body(REQUEST_PREFIX, &m), NOW_MS)
                .is_err(),
            "bit {i}"
        );
    }
    for n in 0..b.len() {
        assert!(
            d.check_request(&with_body(REQUEST_PREFIX, &b[..n]), NOW_MS)
                .is_err(),
            "truncated to {n}"
        );
        assert!(
            EnrollRequest::decode(&with_body(REQUEST_PREFIX, &b[..n])).is_err(),
            "truncated to {n}"
        );
    }
    for extra in 1..=3 {
        let mut m = b.clone();
        m.extend(std::iter::repeat_n(b'a', extra));
        assert!(
            d.check_request(&with_body(REQUEST_PREFIX, &m), NOW_MS)
                .is_err(),
            "extended by {extra}"
        );
        assert!(
            EnrollRequest::decode(&with_body(REQUEST_PREFIX, &m)).is_err(),
            "extended by {extra}"
        );
    }
}

#[test]
fn every_bit_flip_truncation_and_extension_of_an_answer_is_refused() {
    let d = desk(4);
    let po = PhoneOffer::scan(&d.qr_text(), NOW_MS).unwrap();
    let me = Uuid::from_u128(0xbeef);
    let b = body_of(&d.answer(Outcome::Certified, me).encode(), ANSWER_PREFIX);
    for i in 0..b.len() * 8 {
        let mut m = b.clone();
        m[i / 8] ^= 1 << (i % 8);
        assert!(
            po.check_answer(&with_body(ANSWER_PREFIX, &m), me).is_err(),
            "bit {i}"
        );
    }
    for n in 0..b.len() {
        assert_eq!(
            po.check_answer(&with_body(ANSWER_PREFIX, &b[..n]), me),
            Err(EnrollError::Malformed)
        );
    }
    let mut m = b.clone();
    m.push(0);
    assert_eq!(
        po.check_answer(&with_body(ANSWER_PREFIX, &m), me),
        Err(EnrollError::Malformed)
    );
}

/// The offer carries no integrity of its own (the QR's error correction
/// covers the optics): a flipped bit is another offer, and the request it
/// leads to is refused by the desktop that showed the real one.
#[test]
fn a_damaged_offer_leads_to_a_request_the_desktop_refuses() {
    let p = Phone::new();
    let d = desk(5);
    let b = body_of(&d.qr_text(), OFFER_PREFIX);
    for i in 0..b.len() * 8 {
        let mut m = b.clone();
        m[i / 8] ^= 1 << (i % 8);
        let Ok(other) = Offer::decode(&with_body(OFFER_PREFIX, &m)) else {
            continue;
        };
        let text = EnrollRequest::build(&other, &phone_code(&p, "x"), signer(&p)).encode();
        assert!(d.check_request(&text, NOW_MS).is_err(), "bit {i}");
    }
    for n in 0..b.len() {
        assert_eq!(
            Offer::decode(&with_body(OFFER_PREFIX, &b[..n])).unwrap_err(),
            EnrollError::Malformed
        );
    }
    let mut m = b.clone();
    m.push(0);
    assert_eq!(
        Offer::decode(&with_body(OFFER_PREFIX, &m)).unwrap_err(),
        EnrollError::Malformed
    );
}

#[test]
fn non_canonical_base64url_and_prefixes_are_refused() {
    let p = Phone::new();
    // 0xfb: the offer's base64url has both `-` and `_`.
    let d = desk(0xfb);
    let offer = d.qr_text().to_string();
    assert!(offer.contains('-') && offer.contains('_'));
    let request = req_text(&d.offer, &p, "x");
    let answer = d.answer(Outcome::Declined, p.device_id).encode();
    for (text, prefix) in [
        (&offer, OFFER_PREFIX),
        (&request, REQUEST_PREFIX),
        (&answer, ANSWER_PREFIX),
    ] {
        let body = text.strip_prefix(prefix).unwrap();
        let bad = [
            format!("{text}="),
            format!("{text}=="),
            format!(" {text}"),
            format!("{text}\n"),
            format!("{prefix} {body}"),
            format!("{}{body}", prefix.to_uppercase()),
            format!("{}{body}", prefix.trim_end_matches(':')),
            text.replacen('-', "+", 1),
            text.replacen('_', "/", 1),
            // A non-zero trailing bit in the last character (when the
            // length leaves a partial group; otherwise any change is a value).
            if body_of(text, prefix).len().is_multiple_of(3) {
                text.clone()
            } else {
                let mut s = text.clone();
                let last = s.pop().unwrap();
                let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
                let i = alphabet.iter().position(|c| *c as char == last).unwrap();
                s.push(alphabet[i | 1] as char);
                if s == *text {
                    s.pop();
                    s.push(alphabet[(i | 1) + 1] as char);
                }
                s
            },
        ];
        for b in bad.iter().filter(|b| *b != text) {
            let refused = match prefix {
                OFFER_PREFIX => Offer::decode(b).is_err(),
                REQUEST_PREFIX => {
                    EnrollRequest::decode(b).is_err() && d.check_request(b, NOW_MS).is_err()
                }
                _ => EnrollAnswer::decode(b).is_err(),
            };
            assert!(refused, "{prefix} accepted {b:?}");
        }
    }
    // Each decoder refuses the other formats' prefixes.
    assert_eq!(
        Offer::decode(&request).unwrap_err(),
        EnrollError::NotAnOffer
    );
    assert_eq!(
        EnrollRequest::decode(&offer).unwrap_err(),
        EnrollError::NotARequest
    );
    assert_eq!(
        EnrollAnswer::decode(&offer).unwrap_err(),
        EnrollError::NotAnAnswer
    );
    assert_eq!(
        Offer::decode(&phone_code(&p, "x").encode()).unwrap_err(),
        EnrollError::NotAnOffer
    );
}

#[test]
fn a_request_under_another_offer_or_key_is_refused() {
    let p = Phone::new();
    let d = desk(7);
    let other = offer_with(8, d.offer.exp);
    // Another offer's request (its own rid): not this offer.
    assert_eq!(
        d.check_request(&req_text(&other, &p, "x"), NOW_MS)
            .unwrap_err(),
        EnrollError::WrongOffer
    );
    // This offer's rid, MAC'd under another secret (a box guessing).
    let mut r = EnrollRequest::decode(&req_text(&d.offer, &p, "x")).unwrap();
    r.mac = other.keys().request_mac(&r.signed(), &r.phone_sig);
    assert_eq!(
        d.check_request(&r.encode(), NOW_MS).unwrap_err(),
        EnrollError::BadMac
    );
    // ... which the box can't tell (it never has the secret).
    assert!(box_check_request(&r.encode(), d.offer.workspace_id, None, NOW_MS).is_ok());
}

/// T11: someone who holds the secret swaps in another phone's code; the
/// MAC is right but the phone signature isn't that code's key's.
#[test]
fn a_valid_mac_over_another_phones_code_is_refused() {
    let (a, b) = (Phone::new(), Phone::new());
    let d = desk(9);
    let keys = d.offer.keys();
    let code_b = phone_code(&b, "victim");
    let signed = request_signed_preimage(
        keys.rid,
        d.offer.workspace_id,
        d.offer.desktop_device_id,
        d.offer.exp,
        &code_b.body(),
    );
    let phone_sig = ligus_core::crypto::sign_p256(&a.key, &signed);
    let mac = keys.request_mac(&signed, &phone_sig);
    let forged = EnrollRequest {
        rid: keys.rid,
        workspace_id: d.offer.workspace_id,
        desktop_device_id: d.offer.desktop_device_id,
        exp: d.offer.exp,
        phone_sig,
        mac,
        code: code_b,
        ext: Default::default(),
    };
    assert_eq!(
        d.check_request(&forged.encode(), NOW_MS).unwrap_err(),
        EnrollError::BadPhoneSignature
    );
    assert_eq!(
        EnrollRequest::decode(&forged.encode()).unwrap_err(),
        EnrollError::BadPhoneSignature
    );
}

#[test]
fn a_high_s_phone_signature_is_refused() {
    let p = Phone::new();
    let d = desk(10);
    let mut r = EnrollRequest::decode(&req_text(&d.offer, &p, "x")).unwrap();
    r.phone_sig = high_s(&r.phone_sig);
    r.mac = d.offer.keys().request_mac(&r.signed(), &r.phone_sig);
    assert_eq!(
        EnrollRequest::decode(&r.encode()).unwrap_err(),
        EnrollError::BadPhoneSignature
    );
    assert_eq!(
        d.check_request(&r.encode(), NOW_MS).unwrap_err(),
        EnrollError::BadPhoneSignature
    );
}

/// The code inside a request gets every `PhoneCode::decode` check: here,
/// a label that isn't its own clamp.
#[test]
fn the_label_clamp_is_inherited() {
    let p = Phone::new();
    let d = desk(11);
    let label = " padded ";
    let mut code = phone_code(&p, "x");
    code.label = label.into();
    code.signature = ligus_core::crypto::sign_p256(
        &p.key,
        &crate::phone_enroll::preimage(p.device_id, &code.public_key, label),
    );
    let r = EnrollRequest::build(&d.offer, &code, signer(&p));
    assert_eq!(
        EnrollRequest::decode(&r.encode()).unwrap_err(),
        EnrollError::BadCode(PhoneCodeError::Malformed)
    );
    assert_eq!(
        d.check_request(&r.encode(), NOW_MS).unwrap_err(),
        EnrollError::BadCode(PhoneCodeError::Malformed)
    );
}

#[test]
fn the_box_checks_workspace_expiry_and_channel() {
    let p = Phone::new();
    let o = offer_with(12, NOW + 300);
    let text = req_text(&o, &p, "x");
    let ws = o.workspace_id;
    assert_eq!(
        box_check_request(&text, Uuid::from_u128(0xff), None, NOW_MS).unwrap_err(),
        BoxRefusal::WrongWorkspace
    );
    assert_eq!(
        box_check_request(&text, ws, Some(Uuid::from_u128(1)), NOW_MS).unwrap_err(),
        BoxRefusal::DeviceMismatch
    );
    assert!(box_check_request(&text, ws, Some(p.device_id), NOW_MS).is_ok());
    let r = box_check_request("ligusenrollreq1:AAAA", ws, None, NOW_MS).unwrap_err();
    assert_eq!((r.status(), r.error()), (400, "bad_request"));
    // The window: exp in [now - 120, now + 420].
    let at = |exp: u64| box_check_offer(&OfferFields { exp, ..o.fields() }, ws, NOW_MS);
    assert!(at(NOW - 120).is_ok() && at(NOW + 420).is_ok());
    assert_eq!(at(NOW - 121), Err(BoxRefusal::OfferExpired));
    assert_eq!(at(NOW + 421), Err(BoxRefusal::OfferExpired));
    assert!(box_check_offer(
        &OfferFields {
            exp: NOW - 120,
            ..o.fields()
        },
        ws,
        NOW_MS + 1
    )
    .is_err());
    // The route's status codes and error names (§7.2).
    for (r, s, e) in [
        (BoxRefusal::WrongWorkspace, 409, "wrong_workspace"),
        (BoxRefusal::DesktopUnknown, 409, "desktop_unknown"),
        (BoxRefusal::OfferExpired, 409, "offer_expired"),
        (BoxRefusal::DeviceRevoked, 403, "device_revoked"),
        (BoxRefusal::SomeoneElses, 409, "someone_elses"),
        (BoxRefusal::RelayUnavailable, 503, "relay_unavailable"),
        (BoxRefusal::DeviceMismatch, 403, "device_mismatch"),
    ] {
        assert_eq!((r.status(), r.error()), (s, e));
    }
}

/// T16: the phone accepts `exp` up to 120 s past and 10 minutes ahead.
#[test]
fn the_phones_clock_window() {
    let text = |exp: u64| offer_with(13, exp).encode().to_string();
    assert!(PhoneOffer::scan(&text(NOW - 60), NOW_MS).is_ok());
    assert!(PhoneOffer::scan(&text(NOW - 120), NOW_MS).is_ok());
    assert_eq!(
        PhoneOffer::scan(&text(NOW - 180), NOW_MS).unwrap_err(),
        EnrollError::Expired
    );
    assert_eq!(
        PhoneOffer::scan(&text(NOW - 121), NOW_MS).unwrap_err(),
        EnrollError::Expired
    );
    assert!(PhoneOffer::scan(&text(NOW + 600), NOW_MS).is_ok());
    assert_eq!(
        PhoneOffer::scan(&text(NOW + 601), NOW_MS).unwrap_err(),
        EnrollError::TooFarAhead
    );
    let po = PhoneOffer::scan(&text(NOW), NOW_MS).unwrap();
    assert!(!po.is_expired(NOW_MS + 120_000) && po.is_expired(NOW_MS + 120_001));
}

#[test]
fn the_desktop_refuses_a_request_after_its_own_expiry() {
    let p = Phone::new();
    let d = desk(14);
    let text = req_text(&d.offer, &p, "x");
    let end = d.expires_at() as i64 * 1000;
    assert!(d.check_request(&text, end).is_ok());
    assert_eq!(
        d.check_request(&text, end + 1).unwrap_err(),
        EnrollError::Expired
    );
    assert!(d.is_over(end + 1) && !d.is_over(end));
    let started =
        DesktopOffer::start(Uuid::from_u128(1), Uuid::from_u128(2), NOW_MS + 999).unwrap();
    assert_eq!(started.expires_at(), NOW + 300);
    assert_ne!(
        started.rid(),
        DesktopOffer::start(Uuid::from_u128(1), Uuid::from_u128(2), NOW_MS)
            .unwrap()
            .rid(),
        "a fresh secret every time"
    );
}

/// One use (§8.1), the hold and the tripwire (§8.3), and T15: the same
/// phone's fresh request is the same phone.
#[test]
fn one_use_hold_and_tripwire() {
    let (a, b) = (Phone::new(), Phone::new());
    let mut d = desk(15);
    let ra = d
        .check_request(&req_text(&d.offer, &a, "A"), NOW_MS)
        .unwrap();
    // The same phone after a restart: the Secure Enclave's signatures are
    // randomized, so its request has other bytes (here, its label changed too).
    let ra2 = d
        .check_request(&req_text(&d.offer, &a, "A again"), NOW_MS)
        .unwrap();
    let rb = d
        .check_request(&req_text(&d.offer, &b, "B"), NOW_MS)
        .unwrap();
    let fp = ra.code.fingerprint();
    assert_eq!(
        d.check_approve(d.rid(), &fp, NOW_MS).unwrap_err(),
        EnrollError::NoRequest
    );
    assert_eq!(d.observe(&ra, NOW_MS), Ok(Seen::First));
    assert_ne!(ra.encode(), ra2.encode());
    assert_eq!(
        d.observe(&ra, NOW_MS + 500),
        Ok(Seen::SamePhone),
        "the same bytes again"
    );
    assert_eq!(d.observe(&ra2, NOW_MS + 1_000), Ok(Seen::SamePhone));
    assert_eq!(
        d.check_approve(d.rid(), &fp, NOW_MS + 4_999).unwrap_err(),
        EnrollError::Hold
    );
    assert_eq!(
        d.check_approve(d.rid(), &rb.code.fingerprint(), NOW_MS + 5_000)
            .unwrap_err(),
        EnrollError::FingerprintMismatch
    );
    assert_eq!(
        d.check_approve(Rid([0; 16]), &fp, NOW_MS + 5_000)
            .unwrap_err(),
        EnrollError::WrongOffer
    );
    assert_eq!(
        d.check_approve(d.rid(), &fp, NOW_MS + 5_000).unwrap(),
        &ra.code
    );
    d.approved();
    assert!(d.is_watching());
    assert_eq!(
        d.check_approve(d.rid(), &fp, NOW_MS + 6_000).unwrap_err(),
        EnrollError::NoRequest,
        "approved once"
    );
    assert_eq!(d.observe(&ra2, NOW_MS + 7_000), Ok(Seen::SamePhone));
    assert_eq!(d.observe(&rb, NOW_MS + 8_000), Ok(Seen::Tripwire));
    assert_eq!(
        d.locked_request().unwrap().device_id(),
        a.device_id,
        "the answer keeps naming the first phone"
    );
    let end = d.expires_at() as i64 * 1000;
    assert_eq!(d.observe(&rb, end + 1), Err(EnrollError::Expired));

    // Before approval, a second phone aborts the offer.
    let mut d = desk(16);
    let ra = d
        .check_request(&req_text(&d.offer, &a, "A"), NOW_MS)
        .unwrap();
    let rb = d
        .check_request(&req_text(&d.offer, &b, "B"), NOW_MS)
        .unwrap();
    assert_eq!(d.observe(&ra, NOW_MS), Ok(Seen::First));
    assert_eq!(d.observe(&rb, NOW_MS + 2_000), Ok(Seen::TwoPhones));
    assert!(d.is_aborted());
    assert_eq!(
        d.check_approve(d.rid(), &ra.code.fingerprint(), NOW_MS + 10_000)
            .unwrap_err(),
        EnrollError::OfferClosed
    );
    assert_eq!(
        d.observe(&ra, NOW_MS + 3_000),
        Err(EnrollError::OfferClosed)
    );
    // A request for another offer can't touch this one.
    let other = desk(17);
    let ro = other
        .check_request(&req_text(&other.offer, &a, "A"), NOW_MS)
        .unwrap();
    assert_eq!(desk(16).observe(&ro, NOW_MS), Err(EnrollError::WrongOffer));
}

/// T12: an answer for another offer, or tagged with the wrong key, is ignored.
#[test]
fn an_answer_for_another_offer_or_under_another_key_is_refused() {
    let me = Uuid::from_u128(0x77);
    let d = desk(18);
    let other = desk(19);
    let po = PhoneOffer::scan(&d.qr_text(), NOW_MS).unwrap();
    assert_eq!(
        po.check_answer(&other.answer(Outcome::Certified, me).encode(), me),
        Err(EnrollError::WrongOffer)
    );
    let mut a = d.answer(Outcome::Certified, me);
    a.tag = other.keys.answer_tag(Outcome::Certified, me);
    assert_eq!(po.check_answer(&a.encode(), me), Err(EnrollError::BadTag));
    let mut a = d.answer(Outcome::Declined, me);
    a.outcome = Outcome::Certified;
    assert_eq!(po.check_answer(&a.encode(), me), Err(EnrollError::BadTag));
    let mut b = body_of(&d.answer(Outcome::Certified, me).encode(), ANSWER_PREFIX);
    for bad in [0u8, 4, 0xff] {
        b[16] = bad;
        assert_eq!(
            po.check_answer(&with_body(ANSWER_PREFIX, &b), me),
            Err(EnrollError::Malformed)
        );
    }
}

fn ring(s: &str) -> KeyringEngine {
    KeyringEngine::passphrase(Uuid::nil(), ligus_core::derive_workspace_key(s))
}

/// A request slot's name: `phone-enroll:<rid>:<box>:<tag>`, the tag
/// 16 bytes of SHA-256 over the context, `rid` and the phone's device id
/// (22 characters), so two phones on one offer never share a slot, and the
/// same phone's tag differs between offers. Well under the relay's 256
/// characters for a presence name.
#[test]
fn request_slots_are_per_phone_and_unlinkable_across_offers() {
    use sha2::Digest as _;
    let (bx, a, b) = (
        Uuid::from_u128(0xb0),
        Uuid::from_u128(0xa1),
        Uuid::from_u128(0xa2),
    );
    let rid = Rid([7; 16]);
    let mut pre = SLOT_TAG_CONTEXT.to_vec();
    pre.extend_from_slice(&rid.0);
    pre.extend_from_slice(a.as_bytes());
    let want = URL_SAFE_NO_PAD.encode(&sha2::Sha256::digest(&pre)[..16]);
    assert_eq!(phone_slot_tag(rid, a), want);
    assert_eq!(
        request_slot(rid, bx, a),
        format!("phone-enroll:{}:{bx}:{want}", rid.to_b64u())
    );
    assert_ne!(request_slot(rid, bx, a), request_slot(rid, bx, b));
    assert_ne!(phone_slot_tag(rid, a), phone_slot_tag(Rid([8; 16]), a));
    assert!(
        !request_slot(rid, bx, a).contains(&a.to_string()),
        "the relay never sees the phone's device id"
    );
    assert_eq!(request_slot(rid, bx, a).len(), 13 + 22 + 1 + 36 + 1 + 22);
}

/// T2 and §5.5: request slots are sealed, bound to their slot, fresh, and
/// signed by a service member's device; a person's device is refused.
#[test]
fn request_slots_are_sealed_bound_fresh_and_box_signed() {
    let p = Phone::new();
    let d = desk(20);
    let k = ring("room");
    let (bx, person) = (Uuid::from_u128(0xb0), Uuid::from_u128(0xc0));
    let (bkp, pkp) = (
        SigningKeypair::generate().unwrap(),
        SigningKeypair::generate().unwrap(),
    );
    let service_key = |id: Uuid| (id == bx).then(|| bkp.public_key_bytes().to_vec());
    let text = req_text(&d.offer, &p, "x");
    let r = RelayedRequest::new(&text, bx, NOW_MS).sign(&bkp);
    let slot = request_slot(d.rid(), bx, p.device_id);
    assert!(slot.starts_with(&d.request_slot_prefix()));
    assert_eq!(
        check_relayed_request(&slot, &r, NOW_MS, service_key),
        Ok(d.rid())
    );
    // Sealed: the relay sees no request.
    let mut blobs = HashMap::new();
    blobs.insert(slot.clone(), seal_relayed_request(&k, &r).unwrap());
    assert!(
        !serde_json::to_string(&blobs)
            .unwrap()
            .contains(&text[text.len() - 40..]),
        "the relay sees no request"
    );
    blobs.insert("host:box".into(), serde_json::json!({ "hostBeat": 1 }));
    blobs.insert(
        answer_slot(d.rid()),
        seal_relayed_answer(
            &k,
            &RelayedAnswer::new(
                &d.answer(Outcome::Declined, p.device_id),
                d.offer.desktop_device_id,
                NOW_MS,
            ),
        )
        .unwrap(),
    );
    blobs.insert(
        request_slot(d.rid(), person, p.device_id),
        seal_relayed_request(&ring("other"), &r).unwrap(),
    );
    assert_eq!(
        open_relayed_requests(&blobs, &k),
        vec![(slot.clone(), r.clone())]
    );
    // Wrong slot: another box's name, another rid's.
    assert_eq!(
        check_relayed_request(
            &request_slot(d.rid(), person, p.device_id),
            &r,
            NOW_MS,
            service_key
        ),
        Err(EnrollError::WrongSlot)
    );
    assert_eq!(
        check_relayed_request(
            &request_slot(Rid([1; 16]), bx, p.device_id),
            &r,
            NOW_MS,
            service_key
        ),
        Err(EnrollError::WrongSlot)
    );
    // Another phone's slot (one slot per offer, box and phone).
    assert_eq!(
        check_relayed_request(
            &request_slot(d.rid(), bx, Uuid::from_u128(0xa2)),
            &r,
            NOW_MS,
            service_key
        ),
        Err(EnrollError::WrongSlot)
    );
    // The old one-slot-per-box name isn't taken either.
    assert_eq!(
        check_relayed_request(
            &format!("{}{bx}", d.request_slot_prefix()),
            &r,
            NOW_MS,
            service_key
        ),
        Err(EnrollError::WrongSlot)
    );
    // Freshness: 30 s ahead, 60 s old.
    assert!(check_relayed_request(&slot, &r, NOW_MS - 30_000, service_key).is_ok());
    assert_eq!(
        check_relayed_request(&slot, &r, NOW_MS - 30_001, service_key),
        Err(EnrollError::StaleSlot)
    );
    assert!(check_relayed_request(&slot, &r, NOW_MS + 60_000, service_key).is_ok());
    assert_eq!(
        check_relayed_request(&slot, &r, NOW_MS + 60_001, service_key),
        Err(EnrollError::StaleSlot)
    );
    // A person's device (no service key), a forged signature, a changed field.
    let by_person = RelayedRequest::new(&text, person, NOW_MS).sign(&pkp);
    assert_eq!(
        check_relayed_request(
            &request_slot(d.rid(), person, p.device_id),
            &by_person,
            NOW_MS,
            service_key
        ),
        Err(EnrollError::BadSlotSignature)
    );
    let forged = RelayedRequest::new(&text, bx, NOW_MS).sign(&pkp);
    assert_eq!(
        check_relayed_request(&slot, &forged, NOW_MS, service_key),
        Err(EnrollError::BadSlotSignature)
    );
    let unsigned = RelayedRequest::new(&text, bx, NOW_MS);
    assert_eq!(
        check_relayed_request(&slot, &unsigned, NOW_MS, service_key),
        Err(EnrollError::BadSlotSignature)
    );
    let moved = RelayedRequest {
        at: r.at + 1,
        ..r.clone()
    };
    assert_eq!(
        check_relayed_request(&slot, &moved, NOW_MS, service_key),
        Err(EnrollError::BadSlotSignature)
    );
    // A slot holding junk.
    let junk = RelayedRequest {
        request: "ligusenrollreq1:AAAA".into(),
        ..r
    };
    assert_eq!(
        check_relayed_request(&slot, &junk, NOW_MS, service_key),
        Err(EnrollError::Malformed)
    );
}

#[test]
fn answer_slots_come_only_from_the_offers_desktop() {
    let d = desk(21);
    let k = ring("room");
    let me = Uuid::from_u128(0x99);
    let desktop = d.offer.desktop_device_id;
    let (dkp, okp) = (
        SigningKeypair::generate().unwrap(),
        SigningKeypair::generate().unwrap(),
    );
    let other_desk = Uuid::from_u128(0xd2);
    let key = |id: Uuid| match id {
        id if id == desktop => Some(dkp.public_key_bytes().to_vec()),
        id if id == other_desk => Some(okp.public_key_bytes().to_vec()),
        _ => None,
    };
    let a = RelayedAnswer::new(&d.answer(Outcome::Certified, me), desktop, NOW_MS).sign(&dkp);
    let slot = answer_slot(d.rid());
    assert_eq!(
        check_relayed_answer(&slot, &a, desktop, NOW_MS, key),
        Ok(d.rid())
    );
    let mut blobs = HashMap::new();
    blobs.insert(slot.clone(), seal_relayed_answer(&k, &a).unwrap());
    assert_eq!(
        open_relayed_answers(&blobs, &k),
        vec![(slot.clone(), a.clone())]
    );
    assert!(open_relayed_requests(&blobs, &k).is_empty());
    let from_other =
        RelayedAnswer::new(&d.answer(Outcome::Certified, me), other_desk, NOW_MS).sign(&okp);
    assert_eq!(
        check_relayed_answer(&slot, &from_other, desktop, NOW_MS, key),
        Err(EnrollError::WrongDesktop)
    );
    let forged = RelayedAnswer::new(&d.answer(Outcome::Certified, me), desktop, NOW_MS).sign(&okp);
    assert_eq!(
        check_relayed_answer(&slot, &forged, desktop, NOW_MS, key),
        Err(EnrollError::BadSlotSignature)
    );
    assert_eq!(
        check_relayed_answer(&answer_slot(Rid([2; 16])), &a, desktop, NOW_MS, key),
        Err(EnrollError::WrongSlot)
    );
    assert_eq!(
        check_relayed_answer(&slot, &a, desktop, NOW_MS + 60_001, key),
        Err(EnrollError::StaleSlot)
    );
    // The desktop's device revoked (no key): dropped.
    assert_eq!(
        check_relayed_answer(&slot, &a, desktop, NOW_MS, |_| None),
        Err(EnrollError::BadSlotSignature)
    );
}

/// T10 (codec part): nothing here prints the secret, the MAC keys or the
/// offer text.
#[test]
fn debug_and_errors_never_show_the_secret() {
    let p = Phone::new();
    let mut d = desk(0x5a);
    let po = PhoneOffer::scan(&d.qr_text(), NOW_MS).unwrap();
    let keys = d.offer.keys();
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let offer_text = d.qr_text().to_string();
    let secret_b64 = &offer_text[OFFER_PREFIX.len() + 43..OFFER_PREFIX.len() + 85];
    let req = d
        .check_request(&req_text(&d.offer, &p, "x"), NOW_MS)
        .unwrap();
    d.observe(&req, NOW_MS).unwrap();
    let mut shown = format!(
        "{:?} {:?} {:?} {:?} {:?} {:?}",
        d.offer, d, po, keys, d.offer.secret, req
    );
    for e in [
        EnrollError::BadMac,
        EnrollError::BadTag,
        EnrollError::Expired,
        EnrollError::BadCode(PhoneCodeError::Malformed),
    ] {
        shown.push_str(&format!(" {e} {e:?}"));
    }
    for needle in [
        hex(&[0x5a; 32]),
        hex(keys.request_mac_key()),
        hex(keys.answer_mac_key()),
        secret_b64.to_string(),
        offer_text.clone(),
    ] {
        assert!(!shown.contains(&needle), "{needle} in {shown}");
    }
    assert!(
        shown.contains(&d.rid().to_b64u()),
        "logs name an offer by its rid"
    );
}

#[test]
fn rids_parse_only_canonically() {
    let r = desk(22).rid();
    assert_eq!(Rid::from_b64u(&r.to_b64u()), Some(r));
    assert_eq!(r.to_b64u().len(), 22);
    assert_eq!(
        serde_json::from_value::<Rid>(serde_json::to_value(r).unwrap()).unwrap(),
        r
    );
    for bad in [
        format!("{r}="),
        format!("{r}A"),
        r.to_string()[..21].to_string(),
        String::new(),
    ] {
        assert_eq!(Rid::from_b64u(&bad), None, "{bad}");
    }
}

proptest! {
    #[test]
    fn prop_offers_round_trip(ws in any::<u128>(), dd in any::<u128>(), secret in any::<[u8; 32]>(), exp in any::<u64>()) {
        let o = Offer { workspace_id: Uuid::from_u128(ws), desktop_device_id: Uuid::from_u128(dd), secret: EnrollSecret::from_bytes(secret), exp };
        let back = Offer::decode(&o.encode()).unwrap();
        prop_assert_eq!((back.workspace_id, back.desktop_device_id, back.secret.0, back.exp), (o.workspace_id, o.desktop_device_id, secret, exp));
        prop_assert_eq!(back.fields(), o.fields());
    }

    #[test]
    fn prop_answers_round_trip(secret in any::<[u8; 32]>(), dev in any::<u128>(), o in 1u8..=3) {
        let keys = EnrollSecret::from_bytes(secret).keys();
        let a = EnrollAnswer::build(&keys, Outcome::from_byte(o).unwrap(), Uuid::from_u128(dev));
        prop_assert_eq!(EnrollAnswer::decode(&a.encode()).unwrap(), a);
    }

    #[test]
    fn prop_requests_round_trip(secret in any::<[u8; 32]>(), ws in any::<u128>(), label in "\\PC{0,70}", ahead in 0u64..=300) {
        let p = Phone::new();
        let o = Offer { workspace_id: Uuid::from_u128(ws), desktop_device_id: Uuid::from_u128(3), secret: EnrollSecret::from_bytes(secret), exp: NOW + ahead };
        let d = DesktopOffer::from_offer(o.clone());
        let text = req_text(&o, &p, &label);
        let req = d.check_request(&text, NOW_MS).unwrap();
        prop_assert_eq!(req.encode(), text.clone());
        prop_assert_eq!(&EnrollRequest::decode(&text).unwrap(), &req);
        prop_assert!(box_check_request(&text, o.workspace_id, Some(p.device_id), NOW_MS).is_ok());
    }

    /// Arbitrary text after each prefix never panics and is refused unless
    /// canonical and the right length.
    #[test]
    fn prop_junk_never_panics(body in "[A-Za-z0-9_=+/ -]{0,600}") {
        let d = desk(23);
        let po = PhoneOffer::scan(&d.qr_text(), NOW_MS).unwrap();
        let (offer, request, answer) = (format!("{OFFER_PREFIX}{body}"), format!("{REQUEST_PREFIX}{body}"), format!("{ANSWER_PREFIX}{body}"));
        let _ = Offer::decode(&offer);
        prop_assert!(EnrollRequest::decode(&request).is_err());
        prop_assert!(d.check_request(&request, NOW_MS).is_err());
        prop_assert!(po.check_answer(&answer, Uuid::nil()).is_err());
    }
}

// ---- v2: extensions (workspace model v3, 10.2) ----

fn v2_text(offer: &Offer, p: &Phone, label: &str, prev: Uuid) -> String {
    EnrollRequest::build_with(offer, &phone_code(p, label), RequestExt::previous(prev), signer(p)).encode()
}

/// After a key reset the phone names its old device inside the request:
/// a v2 request every side accepts, signed and MACed over the extension.
#[test]
fn a_v2_request_carries_the_previous_device_and_every_side_accepts_it() {
    let p = Phone::new();
    let d = desk(41);
    let prev = Uuid::from_u128(0xabc);
    let text = v2_text(&d.offer, &p, "Ana's iPhone", prev);
    assert!(text.starts_with(REQUEST_V2_PREFIX), "{text}");
    let req = d.check_request(&text, NOW_MS).unwrap();
    assert_eq!(req.previous_device_id(), Some(prev));
    assert_eq!(req.encode(), text);
    assert_eq!(EnrollRequest::decode(&text).unwrap(), req);
    assert!(box_check_request(&text, d.offer.workspace_id, Some(p.device_id), NOW_MS).is_ok());
    assert!(req.signed().starts_with(REQUEST_V2_CONTEXT));
    // Without a previous device (or naming itself) a phone sends v1.
    let v1 = EnrollRequest::build_with(&d.offer, &phone_code(&p, "x"), RequestExt::previous(p.device_id), signer(&p)).encode();
    assert!(v1.starts_with(REQUEST_PREFIX));
    // A nil previous id is never sent (the decoder refuses it).
    let nil = EnrollRequest::build_with(&d.offer, &phone_code(&p, "x"), RequestExt::previous(Uuid::nil()), signer(&p)).encode();
    assert!(nil.starts_with(REQUEST_PREFIX));
    assert!(d.check_request(&nil, NOW_MS).is_ok());
    assert_eq!(d.check_request(&v1, NOW_MS).unwrap().previous_device_id(), None);
}

/// The extension can't be stripped (a downgrade to v1), added, or changed
/// by whoever carries the request: the phone's signature covers it.
#[test]
fn the_extension_can_be_neither_stripped_nor_added_nor_changed() {
    let p = Phone::new();
    let d = desk(42);
    let prev = Uuid::from_u128(0xabc);
    let text = v2_text(&d.offer, &p, "phone", prev);
    let b = body_of(&text, REQUEST_V2_PREFIX);
    let n = b[REQUEST_FIXED] as usize;
    // Stripped: the same bytes without the extension, as v1.
    let stripped = [&b[..REQUEST_FIXED], &b[REQUEST_FIXED + 1 + n..]].concat();
    assert!(d.check_request(&with_body(REQUEST_PREFIX, &stripped), NOW_MS).is_err());
    assert!(EnrollRequest::decode(&with_body(REQUEST_PREFIX, &stripped)).is_err());
    // Added: a v1 request re-wrapped as v2 with an extension.
    let v1 = body_of(&req_text(&d.offer, &p, "phone"), REQUEST_PREFIX);
    let ext = RequestExt::previous(prev).encode();
    let added = [&v1[..REQUEST_FIXED], &[ext.len() as u8], &ext, &v1[REQUEST_FIXED..]].concat();
    assert!(d.check_request(&with_body(REQUEST_V2_PREFIX, &added), NOW_MS).is_err());
    assert!(EnrollRequest::decode(&with_body(REQUEST_V2_PREFIX, &added)).is_err());
    // Changed: every bit of the whole v2 body.
    for i in 0..b.len() * 8 {
        let mut m = b.clone();
        m[i / 8] ^= 1 << (i % 8);
        assert!(d.check_request(&with_body(REQUEST_V2_PREFIX, &m), NOW_MS).is_err(), "bit {i}");
    }
    for k in 0..b.len() {
        assert!(EnrollRequest::decode(&with_body(REQUEST_V2_PREFIX, &b[..k])).is_err(), "truncated to {k}");
    }
}

/// Malformed extensions are refused before any signature check; an unknown
/// ordinary entry is carried (and signed) as it came, an unknown critical
/// one refused.
#[test]
fn malformed_and_critical_extensions_are_refused() {
    let prev = Uuid::from_u128(7);
    let ok = RequestExt::previous(prev).encode();
    assert_eq!(RequestExt::decode(&ok).unwrap().previous_device_id, Some(prev));
    for (what, ext) in [
        ("duplicate", [ok.clone(), ok.clone()].concat()),
        ("short value", vec![EXT_PREVIOUS_DEVICE, 15].into_iter().chain([1u8; 15]).collect()),
        ("overrun", vec![EXT_PREVIOUS_DEVICE, 16, 1, 2]),
        ("dangling type", vec![9]),
        ("nil device", [vec![EXT_PREVIOUS_DEVICE, 16], vec![0u8; 16]].concat()),
        ("critical unknown", vec![0x80, 0]),
    ] {
        assert_eq!(RequestExt::decode(&ext), Err(EnrollError::Malformed), "{what}");
    }
    let unknown = [ok.clone(), vec![0x22, 2, 9, 9]].concat();
    let e = RequestExt::decode(&unknown).unwrap();
    assert_eq!((e.previous_device_id, e.encode()), (Some(prev), unknown));

    // A request whose extension is empty, too long, or names the phone
    // itself is refused.
    let p = Phone::new();
    let d = desk(43);
    let b = body_of(&req_text(&d.offer, &p, "phone"), REQUEST_PREFIX);
    let wrap = |ext: &[u8]| with_body(REQUEST_V2_PREFIX, &[&b[..REQUEST_FIXED], &[ext.len() as u8], ext, &b[REQUEST_FIXED..]].concat());
    assert!(EnrollRequest::decode(&wrap(&[])).is_err());
    assert!(EnrollRequest::decode(&wrap(&vec![0x22; MAX_EXT + 1][..])).is_err());
    let own = EnrollRequest::build_with(&d.offer, &phone_code(&p, "phone"), RequestExt::default(), signer(&p));
    let ext_self = RequestExt::previous(p.device_id).encode();
    let signed = request_signed_preimage_v2(own.rid, own.workspace_id, own.desktop_device_id, own.exp, &ext_self, &own.code.body());
    let sig = ligus_core::crypto::sign_p256(&p.key, &signed);
    let mac = d.offer.keys().request_mac(&signed, &sig);
    let self_named = [&b[..56], &sig, &mac, &[ext_self.len() as u8], &ext_self, &b[REQUEST_FIXED..]].concat();
    assert_eq!(d.check_request(&with_body(REQUEST_V2_PREFIX, &self_named), NOW_MS).unwrap_err(), EnrollError::Malformed);
}

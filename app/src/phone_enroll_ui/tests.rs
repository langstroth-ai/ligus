//! The desktop side of reverse-QR enrollment, without a window: a real
//! `ChatService` (test presence), a box keypair that relays, simulated
//! phones (`ligus_core::testkit::Phone`), and presence listings built with
//! the codec's own seal helpers. Review tests: T1 (tripwire), T2 (who may
//! relay), T7 (shown only after Show code, hidden, content protection),
//! T10 (logs), T11/T12 (substitution, replay), T15 (same phone again).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ligus_core::testkit::Phone;
use ligus_core::{ActorIdentity, ActorKind, SigningKeypair};
use ligus_runtime::keyring::KeyringEngine;
use ligus_runtime::phone_enroll::{PhoneCode, PhoneStanding};
use ligus_runtime::phone_enroll_qr::{
    check_relayed_answer, open_relayed_answers, request_slot, seal_relayed_request, EnrollSecret, Offer, Outcome, PhoneOffer,
    PhoneVerdict, RelayedRequest, HOLD_MS,
};
use ligus_runtime::{ChatService, EventStore};
use serde_json::Value;
use uuid::Uuid;

use super::*;

const NOW: i64 = 1_900_000_000_000;

fn ring() -> KeyringEngine {
    KeyringEngine::passphrase(Uuid::nil(), ligus_core::derive_workspace_key("enroll-room"))
}

/// A person's desktop on a fresh workspace, answering presence prompts with
/// a test recovery key (no passkey sheet, no keychain).
fn desktop() -> (ChatService, Uuid) {
    let account = Uuid::new_v4();
    let mut author = ActorIdentity::new(account.to_string(), "Ana", ActorKind::Human);
    author.account_id = Some(account);
    let mut svc = ChatService::new(EventStore::open_in_memory().unwrap(), SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), author);
    svc.enable_test_presence();
    let ws = Uuid::new_v4();
    svc.ensure_workspace_config(ws).unwrap();
    (svc, ws)
}

fn code_of(p: &Phone, label: &str) -> PhoneCode {
    PhoneCode::new_signed(p.device_id, p.public_key(), label, |m| ligus_core::crypto::sign_p256(&p.key, m))
}

/// What the phone sends after scanning `qr` (a fresh signature every call).
fn request_text(qr: &str, p: &Phone, label: &str, now_ms: i64) -> String {
    let offer = PhoneOffer::scan(qr, now_ms).unwrap();
    offer.build_request(&code_of(p, label), |m| ligus_core::crypto::sign_p256(&p.key, m)).encode()
}

struct Box_ {
    id: Uuid,
    kp: SigningKeypair,
}

impl Box_ {
    fn new() -> Self {
        Self { id: Uuid::new_v4(), kp: SigningKeypair::generate().unwrap() }
    }
    fn key(&self) -> Vec<u8> {
        self.kp.public_key_bytes().to_vec()
    }
    /// The box relays `text` into a presence listing.
    fn relay(&self, blobs: &mut HashMap<String, Value>, ring: &KeyringEngine, text: &str, now_ms: i64) {
        let r = RelayedRequest::new(text, self.id, now_ms).sign(&self.kp);
        let rid = ligus_runtime::phone_enroll_qr::EnrollRequest::decode(text).unwrap().rid;
        blobs.insert(request_slot(rid, self.id), seal_relayed_request(ring, &r).unwrap());
    }
}

/// Records every content-protection change.
#[derive(Default)]
struct Shield(RefCell<Vec<bool>>);

impl Shield {
    fn f(&self) -> impl Fn(bool) + '_ {
        |on| self.0.borrow_mut().push(on)
    }
    fn last(&self) -> Option<bool> {
        self.0.borrow().last().copied()
    }
}

struct Flow {
    svc: ChatService,
    ws: Uuid,
    desk: EnrollDesk,
    ring: KeyringEngine,
    bx: Box_,
    qr: String,
    rid: String,
    blobs: HashMap<String, Value>,
}

impl Flow {
    fn new(shield: &Shield) -> Self {
        let (svc, ws) = desktop();
        let desk = EnrollDesk::default();
        let offer = desk.start(ws, svc.device_id(), EnrollAccountDto::default(), NOW, &shield.f()).unwrap();
        let qr = desk.current.lock().unwrap().as_ref().unwrap().offer.qr_text().to_string();
        Self { svc, ws, desk, ring: ring(), bx: Box_::new(), qr, rid: offer.rid, blobs: HashMap::new() }
    }

    fn poll(&self, now_ms: i64, shield: &Shield) -> PollDto {
        let key = self.bx.key();
        let bx = self.bx.id;
        self.desk.poll(
            &self.blobs,
            &self.ring,
            now_ms,
            |d| (d == bx).then(|| key.clone()),
            |c| self.svc.phone_standing(self.ws, c).ok(),
            |_| "Home box".into(),
            |_| false,
            &shield.f(),
        )
    }

    fn phone_answers(&mut self, p: &Phone, label: &str, now_ms: i64) {
        let text = request_text(&self.qr, p, label, now_ms);
        let ring = ring();
        self.bx.relay(&mut self.blobs, &ring, &text, now_ms);
    }

    /// Approve as the command does: the checks, the passkey (test presence),
    /// then the watch.
    fn approve(&mut self, fingerprint: &str, now_ms: i64, shield: &Shield) -> Result<(Watch, Option<TripwireDto>), String> {
        let (ws, code) = self.desk.begin_approve(&self.rid, fingerprint, self.ws, now_ms)?;
        self.svc.certify_phone_code(ws, &code).map_err(|e| e.to_string())?;
        Ok(self.desk.finish_approve(&shield.f()).expect("a watch"))
    }
}

// ---- the QR ----

#[test]
fn the_qr_is_an_svg_image_without_the_offer_text() {
    let offer = DesktopOffer::start(Uuid::new_v4(), Uuid::new_v4(), NOW).unwrap();
    let text = offer.qr_text();
    let svg = qr_svg(&text).unwrap();
    assert!(svg.starts_with("<svg ") && svg.ends_with("</svg>"));
    assert!(svg.contains("aria-hidden=\"true\""), "the page labels the image, not the SVG");
    assert!(!svg.contains(text.as_str()) && !svg.contains("ligusenroll1:"), "no offer text in the image");
    assert_eq!(svg, qr_svg(&text).unwrap(), "deterministic");
    // 109 characters at level M fits a small code (version 7 or so: 45 modules).
    let qr = qrcodegen::QrCode::encode_text(&text, qrcodegen::QrCodeEcc::Medium).unwrap();
    assert!(qr.size() <= 57, "version ≤ 10, size {}", qr.size());
}

// ---- T7: shown only after Show code, hidden, content protection ----

#[test]
fn content_protection_follows_the_qr_on_screen() {
    let shield = Shield::default();
    let f = Flow::new(&shield);
    assert_eq!(shield.last(), Some(true), "protected as soon as the code shows");
    assert!(f.desk.is_shown());
    f.desk.hide(&shield.f());
    assert_eq!(shield.last(), Some(false));
    assert!(!f.desk.is_shown(), "hidden on blur, the offer kept");
    let again = f.desk.show(NOW + 1_000, &shield.f()).unwrap();
    assert_eq!(again.rid, f.rid, "the same offer comes back");
    assert!(again.qr_svg.starts_with("<svg"));
    assert_eq!(shield.last(), Some(true));
    f.desk.cancel(&shield.f());
    assert_eq!(shield.last(), Some(false));
    assert!(f.desk.show(NOW + 2_000, &shield.f()).is_err(), "nothing to show after Cancel");
}

#[test]
fn an_expired_offer_closes_and_unprotects() {
    let shield = Shield::default();
    let f = Flow::new(&shield);
    let exp_ms = (NOW / 1000 + ligus_runtime::phone_enroll_qr::OFFER_TTL_SECS as i64) * 1000;
    assert_eq!(f.poll(exp_ms, &shield).state, "waiting", "still open at exp");
    assert_eq!(f.poll(exp_ms + 1, &shield).state, "expired");
    assert_eq!(shield.last(), Some(false));
    assert_eq!(f.poll(exp_ms + 2, &shield).state, "none");
}

// ---- the request card, the hold, approval ----

#[test]
fn one_phone_waits_out_the_hold_then_is_certified_and_answered() {
    let shield = Shield::default();
    let mut f = Flow::new(&shield);
    assert_eq!(f.poll(NOW, &shield).state, "waiting");
    let p = Phone::new();
    f.phone_answers(&p, "Ana's iPhone", NOW + 1_000);
    let got = f.poll(NOW + 1_000, &shield);
    assert_eq!(got.state, "request");
    let card = got.phone.clone().unwrap();
    assert_eq!(card.label, "Ana's iPhone");
    assert_eq!(card.fingerprint, code_of(&p, "x").fingerprint());
    assert_eq!(card.standing, "new");
    assert_eq!(card.via, "Home box");
    assert_eq!(got.hold_remaining_ms, Some(HOLD_MS));
    // The hold: Add this phone is refused for 5 s after the phone appeared.
    let err = f.desk.begin_approve(&f.rid, &card.fingerprint, f.ws, NOW + 1_000 + HOLD_MS - 1).unwrap_err();
    assert!(err.contains("still listening for other phones"), "{err}");
    assert_eq!(f.poll(NOW + 4_000, &shield).hold_remaining_ms, Some(2_000));
    // Wrong fingerprint, wrong workspace.
    assert!(f.desk.begin_approve(&f.rid, "0000-0000-0000-0000-0000", f.ws, NOW + 7_000).is_err());
    assert!(f.desk.begin_approve(&f.rid, &card.fingerprint, Uuid::new_v4(), NOW + 7_000).unwrap_err().contains("workspace changed"));
    f.desk.approve_failed();
    // Approve.
    let (mut watch, alert) = f.approve(&card.fingerprint, NOW + 7_000, &shield).unwrap();
    assert!(alert.is_none());
    assert_eq!(shield.last(), Some(false), "the QR leaves the screen");
    assert_eq!(f.desk.current_ws(), None);
    assert_eq!(f.svc.phone_standing(f.ws, &code_of(&p, "x")).unwrap(), PhoneStanding::AlreadyMine);
    // The answer: signed by this device, tagged for the phone.
    let desktop = f.svc.device_id();
    let pk = f.svc.device_public_key().to_vec();
    let (slot, blob) = watch.answer_due(NOW + 7_000, &f.ring, desktop, |a| f.svc.sign_relayed_answer(a)).unwrap();
    assert!(watch.answer_due(NOW + 8_000, &f.ring, desktop, |a| f.svc.sign_relayed_answer(a)).is_none(), "every 10 s");
    assert!(watch.answer_due(NOW + 17_000, &f.ring, desktop, |a| f.svc.sign_relayed_answer(a)).is_some());
    let mut listing = HashMap::new();
    listing.insert(slot.clone(), blob);
    let (s, a) = open_relayed_answers(&listing, &f.ring).pop().unwrap();
    check_relayed_answer(&s, &a, desktop, NOW + 7_000, |_| Some(pk.clone())).unwrap();
    let phone = PhoneOffer::scan(&f.qr, NOW + 7_000).unwrap();
    assert_eq!(phone.check_answer(&a.answer, p.device_id).unwrap(), PhoneVerdict::Mine(Outcome::Certified));
    // The phone's name is kept for My devices.
    assert_eq!(watch.added_code().label, "Ana's iPhone");
}

/// T15: the same phone again (re-published, or rescanned after a restart:
/// another signature) neither aborts nor restarts the hold.
#[test]
fn the_same_phone_again_is_the_same_phone() {
    let shield = Shield::default();
    let mut f = Flow::new(&shield);
    let p = Phone::new();
    f.phone_answers(&p, "iPhone", NOW);
    assert_eq!(f.poll(NOW, &shield).state, "request");
    let other_box = Box_::new();
    let text = request_text(&f.qr, &p, "Renamed iPhone", NOW + 2_000);
    let r = ring();
    other_box.relay(&mut f.blobs, &r, &text, NOW + 2_000); // dropped: not a known box
    f.phone_answers(&p, "iPhone", NOW + 2_000);
    let got = f.poll(NOW + 2_000, &shield);
    assert_eq!(got.state, "request");
    assert_eq!(got.hold_remaining_ms, Some(HOLD_MS - 2_000), "the hold runs from the first sighting");
    assert_eq!(f.desk.hold_first_seen(), Some(NOW));
}

/// Two phones answer one code before approval: the offer aborts and nothing
/// can be approved.
#[test]
fn two_phones_abort_the_offer() {
    let shield = Shield::default();
    let mut f = Flow::new(&shield);
    let (a, b) = (Phone::new(), Phone::new());
    f.phone_answers(&a, "Ana's iPhone", NOW);
    let fp = f.poll(NOW, &shield).phone.unwrap().fingerprint;
    // Another box slot so both requests are listed.
    let text = request_text(&f.qr, &b, "Ana's iPhone", NOW + 1_000);
    let key = f.bx.key();
    let bx = f.bx.id;
    let second = Box_ { id: Uuid::new_v4(), kp: SigningKeypair::generate().unwrap() };
    second.relay(&mut f.blobs, &f.ring, &text, NOW + 1_000);
    let second_key = second.key();
    let got = f.desk.poll(
        &f.blobs,
        &f.ring,
        NOW + 1_000,
        |d| (d == bx).then(|| key.clone()).or_else(|| (d == second.id).then(|| second_key.clone())),
        |c| f.svc.phone_standing(f.ws, c).ok(),
        |_| String::new(),
        |_| false,
        &shield.f(),
    );
    assert_eq!(got.state, "aborted");
    assert_eq!(shield.last(), Some(false));
    assert!(f.desk.begin_approve(&f.rid, &fp, f.ws, NOW + 9_000).is_err());
}

/// T1: after approval, another phone answering the same code raises the
/// tripwire once, naming both phones; the added phone again doesn't.
#[test]
fn another_phone_after_approval_trips_the_alert() {
    let shield = Shield::default();
    let mut f = Flow::new(&shield);
    let (mallory, ana) = (Phone::new(), Phone::new());
    f.phone_answers(&mallory, "Ana's iPhone", NOW);
    let fp = f.poll(NOW, &shield).phone.unwrap().fingerprint;
    let (mut watch, _) = f.approve(&fp, NOW + HOLD_MS, &shield).unwrap();
    let key = f.bx.key();
    let bx = f.bx.id;
    let relay_key = |d: Uuid| (d == bx).then(|| key.clone());
    // The added phone again: nothing.
    assert!(watch.step(&f.blobs, &f.ring, NOW + 6_000, relay_key).is_empty());
    // The real phone arrives.
    let mut blobs = HashMap::new();
    f.bx.relay(&mut blobs, &f.ring, &request_text(&f.qr, &ana, "Ana's iPhone", NOW + 20_000), NOW + 20_000);
    let alerts = watch.step(&blobs, &f.ring, NOW + 20_000, relay_key);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].added.device_id, mallory.device_id.to_string());
    assert_eq!(alerts[0].other.device_id, ana.device_id.to_string());
    assert_eq!(alerts[0].rid, f.rid);
    assert!(watch.step(&blobs, &f.ring, NOW + 22_000, relay_key).is_empty(), "once per phone");
    // The answer keeps naming the added phone, so the real one raises its alarm.
    let (_, blob) = watch.answer_due(NOW + 22_000, &f.ring, f.svc.device_id(), |a| f.svc.sign_relayed_answer(a)).unwrap();
    let mut listing = HashMap::new();
    listing.insert(answer_slot(watch.rid()), blob);
    let (_, a) = open_relayed_answers(&listing, &f.ring).pop().unwrap();
    let phone = PhoneOffer::scan(&f.qr, NOW + 22_000).unwrap();
    assert_eq!(phone.check_answer(&a.answer, ana.device_id).unwrap(), PhoneVerdict::AnotherPhone(Outcome::Certified));
}

/// A second phone that aborts the offer while the passkey runs: the phone is
/// still certified (the passkey said yes), so the alert comes at once.
#[test]
fn a_second_phone_during_the_passkey_alerts_at_once() {
    let shield = Shield::default();
    let mut f = Flow::new(&shield);
    let (a, b) = (Phone::new(), Phone::new());
    f.phone_answers(&a, "Ana's iPhone", NOW);
    let fp = f.poll(NOW, &shield).phone.unwrap().fingerprint;
    let (ws, code) = f.desk.begin_approve(&f.rid, &fp, f.ws, NOW + HOLD_MS).unwrap();
    // While the passkey sheet is up, a second phone answers.
    let mut blobs = HashMap::new();
    f.bx.relay(&mut blobs, &f.ring, &request_text(&f.qr, &b, "Ana's iPhone", NOW + 6_000), NOW + 6_000);
    f.blobs = blobs;
    assert_eq!(f.poll(NOW + 6_000, &shield).state, "aborted");
    assert_eq!(f.desk.current_ws(), Some(f.ws), "kept while the passkey runs");
    // Closing the panel or a new code doesn't drop it either.
    f.desk.cancel(&shield.f());
    assert!(f.desk.start(f.ws, f.svc.device_id(), EnrollAccountDto::default(), NOW + 6_000, &shield.f()).is_err());
    assert_eq!(f.desk.current_ws(), Some(f.ws));
    f.svc.certify_phone_code(ws, &code).unwrap();
    let (_, alert) = f.desk.finish_approve(&shield.f()).unwrap();
    let alert = alert.expect("the second phone is named at once");
    assert_eq!(alert.other.device_id, b.device_id.to_string());
    assert_eq!(alert.added.device_id, a.device_id.to_string());
}

#[test]
fn not_my_phone_answers_declined_and_closes() {
    let shield = Shield::default();
    let mut f = Flow::new(&shield);
    let p = Phone::new();
    f.phone_answers(&p, "iPhone", NOW);
    f.poll(NOW, &shield);
    assert!(f.desk.decline("AAAAAAAAAAAAAAAAAAAAAA", &shield.f()).is_err(), "another rid");
    let mut watch = f.desk.decline(&f.rid, &shield.f()).unwrap().expect("a phone was locked");
    assert_eq!(f.desk.current_ws(), None);
    assert_eq!(shield.last(), Some(false));
    let (_, blob) = watch.answer_due(NOW + 1_000, &f.ring, f.svc.device_id(), |a| f.svc.sign_relayed_answer(a)).unwrap();
    let mut listing = HashMap::new();
    listing.insert(answer_slot(watch.rid()), blob);
    let (_, a) = open_relayed_answers(&listing, &f.ring).pop().unwrap();
    let phone = PhoneOffer::scan(&f.qr, NOW + 1_000).unwrap();
    assert_eq!(phone.check_answer(&a.answer, p.device_id).unwrap(), PhoneVerdict::Mine(Outcome::Declined));
    assert_eq!(f.svc.phone_standing(f.ws, &code_of(&p, "x")).unwrap(), PhoneStanding::New, "nothing certified");
    // A decline doesn't watch for other phones.
    let mut blobs = HashMap::new();
    f.bx.relay(&mut blobs, &f.ring, &request_text(&f.qr, &Phone::new(), "x", NOW + 2_000), NOW + 2_000);
    let key = f.bx.key();
    assert!(watch.step(&blobs, &f.ring, NOW + 2_000, |_| Some(key.clone())).is_empty());
}

#[test]
fn a_phone_that_cant_be_added_is_refused_not_approved() {
    let shield = Shield::default();
    let mut f = Flow::new(&shield);
    let p = Phone::new();
    // Certified here with another key under the same id: a conflict.
    let other = Phone::new();
    let swapped = PhoneCode::new_signed(p.device_id, other.public_key(), "x", |m| ligus_core::crypto::sign_p256(&other.key, m));
    let _ = f.svc.certify_phone_code(f.ws, &swapped); // refused: the id isn't the key's
    // A revoked phone: certify, then revoke.
    f.svc.certify_phone_code(f.ws, &code_of(&p, "x")).unwrap();
    f.svc.revoke_device(ligus_core::workspace_config_session_id(f.ws), f.ws, p.device_id).unwrap();
    f.phone_answers(&p, "iPhone", NOW);
    let got = f.poll(NOW, &shield);
    let card = got.phone.unwrap();
    assert_eq!(card.standing, "revoked");
    assert!(card.problem.is_some());
    assert!(f.desk.begin_approve(&f.rid, &card.fingerprint, f.ws, NOW + HOLD_MS).is_err());
    f.desk.approve_failed();
    let mut watch = f.desk.decline(&f.rid, &shield.f()).unwrap().unwrap();
    let (_, blob) = watch.answer_due(NOW + 6_000, &f.ring, f.svc.device_id(), |a| f.svc.sign_relayed_answer(a)).unwrap();
    let mut listing = HashMap::new();
    listing.insert(answer_slot(watch.rid()), blob);
    let (_, a) = open_relayed_answers(&listing, &f.ring).pop().unwrap();
    let phone = PhoneOffer::scan(&f.qr, NOW + 6_000).unwrap();
    assert_eq!(phone.check_answer(&a.answer, p.device_id).unwrap(), PhoneVerdict::Mine(Outcome::Refused));
}

// ---- T2, T11, T12: what doesn't count as a request ----

#[test]
fn requests_not_relayed_by_a_box_or_not_made_from_this_code_are_ignored() {
    let shield = Shield::default();
    let mut f = Flow::new(&shield);
    let p = Phone::new();
    let r = ring();
    // T2: signed by a person's device (the relay key resolver says no).
    let person = Box_::new();
    person.relay(&mut f.blobs, &r, &request_text(&f.qr, &p, "x", NOW), NOW);
    assert_eq!(f.poll(NOW, &shield).state, "waiting");
    // T11/T12: made from another code (another secret, same workspace and desktop).
    let other = Offer {
        workspace_id: f.ws,
        desktop_device_id: f.svc.device_id(),
        secret: EnrollSecret::from_bytes([9; 32]),
        exp: (NOW / 1000) as u64 + 300,
    };
    f.phone_answers(&p, "x", NOW); // keep a valid one aside to compare
    let valid = f.blobs.clone();
    f.blobs.clear();
    let replay = request_text(&other.encode(), &p, "x", NOW);
    f.bx.relay(&mut f.blobs, &r, &replay, NOW);
    assert_eq!(f.poll(NOW, &shield).state, "waiting", "another offer's request isn't in this offer's slots");
    // A request sealed under another workspace key doesn't open.
    f.blobs.clear();
    let foreign = KeyringEngine::passphrase(Uuid::nil(), ligus_core::derive_workspace_key("other-room"));
    f.bx.relay(&mut f.blobs, &foreign, &request_text(&f.qr, &p, "x", NOW), NOW);
    assert_eq!(f.poll(NOW, &shield).state, "waiting");
    // A stale slot (more than 60 s old) is dropped.
    f.blobs = valid.clone();
    assert_eq!(f.poll(NOW + 61_000, &shield).state, "waiting");
    f.blobs = valid;
    assert_eq!(f.poll(NOW + 1_000, &shield).state, "request");
}

/// T2 through the service's own signer rule: this person's own desktop (a
/// certified human device) can't relay a request, even holding the
/// workspace key and the code.
#[test]
fn the_desktops_own_device_cannot_relay_a_request() {
    let shield = Shield::default();
    let f = Flow::new(&shield);
    let me = f.svc.device_id();
    assert!(f.svc.enroll_relay_key(f.ws, me).is_none());
    let mut blobs = HashMap::new();
    let text = request_text(&f.qr, &Phone::new(), "x", NOW);
    let rid = ligus_runtime::phone_enroll_qr::EnrollRequest::decode(&text).unwrap().rid;
    let r = f.svc.sign_relayed_request(RelayedRequest::new(&text, me, NOW));
    blobs.insert(request_slot(rid, me), seal_relayed_request(&f.ring, &r).unwrap());
    let got = f.desk.poll(&blobs, &f.ring, NOW, |d| f.svc.enroll_relay_key(f.ws, d), |c| f.svc.phone_standing(f.ws, c).ok(), |_| String::new(), |_| false, &shield.f());
    assert_eq!(got.state, "waiting");
}

// ---- the precheck (review R8) ----

#[test]
fn the_precheck_names_what_is_missing() {
    let (svc, ws) = desktop();
    assert!(precheck_reasons(&svc, ws, true, false).iter().any(|r| r.contains("shared workspace")));
    assert!(precheck_reasons(&svc, ws, false, false).iter().any(|r| r.contains("relay connection")));
    // No passkey or recovery key yet on this workspace.
    assert!(precheck_reasons(&svc, ws, false, true).iter().any(|r| r.contains("passkey")));
    let mut nobody = ChatService::new(
        EventStore::open_in_memory().unwrap(),
        SigningKeypair::generate().unwrap(),
        SigningKeypair::generate().unwrap(),
        ActorIdentity::new("x", "X", ActorKind::Human),
    );
    nobody.ensure_workspace_config(ws).ok();
    assert!(precheck_reasons(&nobody, ws, false, true).iter().any(|r| r.contains("no account")));
    let mut boxed = ChatService::new(
        EventStore::open_in_memory().unwrap(),
        SigningKeypair::generate().unwrap(),
        SigningKeypair::generate().unwrap(),
        ActorIdentity::new("b", "Box", ActorKind::Service),
    );
    boxed.ensure_workspace_config(ws).ok();
    assert!(precheck_reasons(&boxed, ws, false, true).iter().any(|r| r.contains("your own desktop")));
}

#[test]
fn the_precheck_passes_once_the_account_has_a_credential() {
    let (mut svc, ws) = desktop();
    // The test presence registers a recovery key on first use: certify a phone.
    let p = Phone::new();
    svc.certify_phone_code(ws, &code_of(&p, "x")).unwrap();
    assert!(precheck_reasons(&svc, ws, false, true).is_empty(), "{:?}", precheck_reasons(&svc, ws, false, true));
}

// ---- T10: log hygiene ----

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn forms(bytes: &[u8]) -> Vec<String> {
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    vec![hex.clone(), hex.to_uppercase(), URL_SAFE_NO_PAD.encode(bytes), format!("{bytes:?}")]
}

/// Run the whole flow (start, poll, a bad request, approve, answer, decline
/// path, the tripwire) at TRACE with a capturing subscriber: the offer text,
/// its secret, `k_req`, `k_ans`, the MACs, the request and answer texts and
/// the phone's code never appear, and neither do they in the errors.
#[test]
fn secrets_macs_and_codes_never_reach_the_logs() {
    let out = Captured::default();
    let subscriber = tracing_subscriber::fmt().with_max_level(tracing::Level::TRACE).with_writer(out.clone()).with_ansi(false).finish();
    let mut errors = Vec::new();
    let mut secrets: Vec<String> = Vec::new();
    tracing::subscriber::with_default(subscriber, || {
        // Other tests run the same callsites without a subscriber, in parallel.
        tracing::callsite::rebuild_interest_cache();
        let shield = Shield::default();
        let mut f = Flow::new(&shield);
        let offer = Offer::decode(&f.qr).unwrap();
        let raw = URL_SAFE_NO_PAD.decode(&f.qr["ligusenroll1:".len()..]).unwrap();
        let keys = offer.keys();
        secrets.push(f.qr.clone());
        secrets.push(f.qr["ligusenroll1:".len()..].to_string());
        secrets.extend(forms(&raw[32..64]));
        secrets.extend(forms(keys.request_mac_key()));
        secrets.extend(forms(keys.answer_mac_key()));
        let (a, b) = (Phone::new(), Phone::new());
        let text = request_text(&f.qr, &a, "Ana's iPhone", NOW);
        let req = ligus_runtime::phone_enroll_qr::EnrollRequest::decode(&text).unwrap();
        secrets.push(text.clone());
        secrets.extend(forms(&req.mac));
        secrets.push(code_of(&a, "Ana's iPhone").encode());
        // A bad request first: a wrong MAC, in this offer's slot.
        let mut tampered = req.clone();
        tampered.mac = [0u8; 32];
        let bad = tampered.encode();
        secrets.push(bad.clone());
        f.bx.relay(&mut f.blobs, &f.ring, &bad, NOW);
        assert_eq!(f.poll(NOW, &shield).state, "waiting");
        f.bx.relay(&mut f.blobs, &f.ring, &text, NOW);
        let card = f.poll(NOW, &shield).phone.unwrap();
        errors.extend(f.desk.begin_approve(&f.rid, &card.fingerprint, f.ws, NOW + 1).err());
        errors.extend(f.desk.begin_approve(&f.rid, "nope", f.ws, NOW + HOLD_MS).err());
        let (mut watch, _) = f.approve(&card.fingerprint, NOW + HOLD_MS, &shield).unwrap();
        let (_, blob) = watch.answer_due(NOW + HOLD_MS, &f.ring, f.svc.device_id(), |x| f.svc.sign_relayed_answer(x)).unwrap();
        let mut listing = HashMap::new();
        listing.insert(answer_slot(watch.rid()), blob);
        let (_, ans) = open_relayed_answers(&listing, &f.ring).pop().unwrap();
        secrets.push(ans.answer.clone());
        let mut blobs = HashMap::new();
        f.bx.relay(&mut blobs, &f.ring, &request_text(&f.qr, &b, "x", NOW + 9_000), NOW + 9_000);
        let key = f.bx.key();
        assert_eq!(watch.step(&blobs, &f.ring, NOW + 9_000, |_| Some(key.clone())).len(), 1);
        errors.extend(f.desk.show(NOW + 10_000, &shield.f()).err());
        // Debug of the state never prints a secret either.
        tracing::trace!(target: "identity", "watch {:?}", watch.rid());
    });
    let logs = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
    // Under `cargo test` other threads share the callsites, so a line can be
    // missed here; nextest (CI) runs each test in its own process. Only the
    // absence of secrets is load-bearing.
    assert!(logs.contains("rid="), "the subscriber captured nothing: {logs}");
    for s in &secrets {
        assert!(!logs.contains(s.as_str()), "a secret reached the logs: {s}");
        for e in &errors {
            assert!(!e.contains(s.as_str()), "a secret reached an error: {e}");
        }
    }
    assert!(!logs.contains("ligusenroll1:") && !logs.contains("ligusenrollreq1:") && !logs.contains("ligusenrollres1:") && !logs.contains("ligusphone1:"));
}

// ---- the phone's name under My devices ----

#[test]
fn my_devices_shows_the_name_a_phone_gave_itself_when_it_was_added() {
    let (here, phone, other) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let mut labels = HashMap::new();
    labels.insert(phone.to_string(), "Ana's\u{202E} iPhone".to_string());
    assert_eq!(crate::device_label(phone, here, None, &labels), "Ana's iPhone", "cleaned like every name");
    assert_eq!(crate::device_label(phone, here, Some("Studio box"), &labels), "Studio box", "a host's own label wins");
    assert_eq!(crate::device_label(here, here, None, &labels), "This device");
    assert_eq!(crate::device_label(other, here, None, &labels), format!("Device {}", &other.to_string()[..8]));
}

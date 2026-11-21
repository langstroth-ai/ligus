//! Reverse-QR enrollment end to end (`docs/design/v2/phone-enroll-reverse-qr.md`
//! §13): a real `ligus serve` box, the fake tunnel relay and the test sync
//! relay, the desktop's own `ChatService` driving the codec's desktop API
//! (`DesktopOffer`), and a simulated phone (`PhoneKeys`, `PhoneOffer`).
//! Each attack test of the review is marked with its T number.

use ligus_runtime::phone_enroll::PhoneCode;
use ligus_runtime::phone_enroll_qr::{
    self as qr, answer_slot, check_relayed_request, open_relayed_requests, request_slot, seal_relayed_answer, seal_relayed_request,
    DesktopOffer, EnrollError, EnrollRequest, EnrollSecret, Offer, Outcome, PhoneOffer, PhoneVerdict, RelayedAnswer, RelayedRequest, Rid,
    Seen,
};

use super::*;

fn now_ms() -> i64 {
    Timestamp::now().inner().unix_timestamp_nanos().div_euclid(1_000_000) as i64
}

/// A phone's `ligusphone1:` code, signed by its own key.
fn code_of(p: &ligus_core::testkit::Phone, label: &str) -> PhoneCode {
    PhoneCode::new_signed(p.device_id, p.public_key(), label, |m| ligus_core::crypto::sign_p256(&p.key, m))
}

/// The phone's request for `offer`, signed by its key.
fn request_for(offer: &PhoneOffer, p: &ligus_core::testkit::Phone, label: &str) -> EnrollRequest {
    offer.build_request(&code_of(p, label), |m| ligus_core::crypto::sign_p256(&p.key, m))
}

/// The person's desktop: its own service, keyring and relay (the Tauri
/// commands' job, done here with the codec's desktop API directly).
struct Desk {
    cfg: Config,
    svc: ChatService,
    ws: Uuid,
    id: Uuid,
}

impl Desk {
    fn open(h: &Harness) -> Self {
        let svc = crate::open_service(&h.desktop).unwrap();
        let id = svc.device_id();
        Self { cfg: h.desktop.clone(), svc, ws: h.handle.shared.ws, id }
    }

    /// A new offer with a known secret (so the log check knows what to look for).
    fn offer(&self, secret: [u8; 32]) -> (Offer, DesktopOffer) {
        let offer = Offer { workspace_id: self.ws, desktop_device_id: self.id, secret: EnrollSecret::from_bytes(secret), exp: (now_ms() / 1000) as u64 + qr::OFFER_TTL_SECS };
        (offer.clone(), DesktopOffer::from_offer(offer))
    }

    fn ring(&self) -> ligus_runtime::keyring::KeyringEngine {
        crate::box_ring(&self.cfg).unwrap()
    }

    /// One `phone_enroll_offer_poll`: every request slot for the offer, each
    /// checked in the design's order (§8.1), then observed.
    async fn poll(&self, offer: &mut DesktopOffer) -> Vec<Result<(Seen, EnrollRequest), EnrollError>> {
        let ring = self.ring();
        let blobs = crate::relay_client(&self.cfg).unwrap().list_presence(&self.cfg.room).await.unwrap();
        let now = now_ms();
        let mut out = Vec::new();
        let prefix = offer.request_slot_prefix();
        for (slot, r) in open_relayed_requests(&blobs, &ring).into_iter().filter(|(s, _)| s.starts_with(&prefix)) {
            let res = check_relayed_request(&slot, &r, now, |d| self.svc.enroll_relay_key(self.ws, d))
                .and_then(|_| offer.check_request(&r.request, now))
                .and_then(|req| {
                    use ligus_runtime::phone_enroll::PhoneStanding;
                    match self.svc.phone_standing(self.ws, &req.code).unwrap() {
                        PhoneStanding::New | PhoneStanding::AlreadyMine => {}
                        other => panic!("standing {other:?}"),
                    }
                    offer.observe(&req, now).map(|seen| (seen, req))
                });
            out.push(res);
        }
        out
    }

    /// Poll until a request for the offer passes every check.
    async fn wait_request(&self, offer: &mut DesktopOffer, want: Seen) -> EnrollRequest {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            crate::sync_once(&self.cfg).await.unwrap();
            for r in self.poll(offer).await {
                if let Ok((seen, req)) = r {
                    if seen == want {
                        return req;
                    }
                }
            }
            assert!(tokio::time::Instant::now() < deadline, "the desktop never saw {want:?}");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Publish the desktop's answer slot.
    async fn answer(&self, offer: &DesktopOffer, outcome: Outcome, device: Uuid) {
        let a = self.svc.sign_relayed_answer(RelayedAnswer::new(&offer.answer(outcome, device), self.id, now_ms()));
        let blob = seal_relayed_answer(&self.ring(), &a).unwrap();
        crate::relay_client(&self.cfg).unwrap().publish_presence(&self.cfg.room, &answer_slot(offer.rid()), &blob).await.unwrap();
    }
}

/// Until the box knows the desktop's person and the desktop knows the box
/// as a service member (both through sync).
async fn rosters_ready(h: &Harness, desk: &Desk) {
    let shared = h.handle.shared.clone();
    let box_id = shared.lock().device_id();
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        crate::sync_once(&desk.cfg).await.unwrap();
        shared.sync_kick.notify_one();
        let box_ok = {
            let svc = shared.lock();
            svc.enroll_desktop(desk.ws, desk.id).is_ok() && svc.enroll_relay_key(desk.ws, box_id).is_some()
        };
        if box_ok && desk.svc.enroll_relay_key(desk.ws, box_id).is_some() {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "rosters never caught up");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn check_body(f: &qr::OfferFields) -> Value {
    json!({ "workspaceId": f.workspace_id, "desktopDeviceId": f.desktop_device_id, "rid": f.rid, "exp": f.exp })
}

/// Design §13 "End to end", extending `add_this_phone_then_pair_and_ask_who_am_i`:
/// the desktop shows its QR → the phone scans → E3b provisional enrollment
/// → `check` → `requests` → the box's relay slot → the desktop checks it and
/// approves (`certify_phone_code`, after the 5-second hold) → its answer →
/// the box syncs the certificate → the phone succeeds on `certified: true`,
/// pairs with E3a on the same window and asks `/v1/me`. Also T10 (no secret
/// in the box's log), T12 (a re-submit is idempotent) and T14 (an old phone
/// still gets 4040).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scan_the_desktops_qr_then_pair_and_ask_who_am_i() {
    let relay = FakeRelay::start().await;
    let (endpoint, _) = spawn_model(vec![]).await;
    let r = relay.url.clone();
    let h = harness_with(&endpoint, "m", &[("README.md", "hi\n")], move |o| {
        o.remote = Some(RelaySettings::new(&r, None, true).unwrap());
    })
    .await;
    relay.wait_connected(WAIT).await.unwrap();
    let t = h.handle.shared.tunnel.clone().unwrap();
    let ws = h.handle.shared.ws;
    let box_id = h.handle.shared.lock().device_id();
    let info = host_info(&h, &relay.url);
    let keys = PhoneKeys::generate();
    let phone = keys.device.clone();
    let desk = Desk::open(&h);
    rosters_ready(&h, &desk).await;

    // The desktop shows its QR; the phone scans it.
    let secret: [u8; 32] = tun::random_bytes();
    let (raw_offer, mut offer) = desk.offer(secret);
    let qr_text = offer.qr_text().to_string();
    let scanned = PhoneOffer::scan(&qr_text, now_ms()).unwrap();
    let f = scanned.fields();
    assert_eq!(f, offer.fields());

    // T14: an old phone (no enrollKey) is still refused with 4040.
    let ps = open_window(&h, &relay).await;
    let e = testkit::phone_enroll(&info, &keys, &ps, "Ana's iPhone").await.err().unwrap();
    assert_eq!(testkit::close_code(&e), Some(4040), "{e:#}");

    // E3b: the provisional channel.
    let ps = open_window(&h, &relay).await;
    let mut c = testkit::phone_enroll_provisional(&info, &keys, &ps, "Ana's iPhone").await.unwrap();
    assert!(t.registry.list().iter().all(|r| !r.is_active()), "no pairing yet");
    for path in ["/v1/chats", "/v1/me", "/v1/health"] {
        assert_eq!(c.request("GET", path, None).await.unwrap().status, 404, "{path}: the provisional router has the enroll routes only");
    }

    // check: who the offer's desktop is here.
    let v = c.request("POST", "/v1/phone-enroll/check", Some(check_body(&f))).await.unwrap();
    assert_eq!(v.status, 200, "{:?}", v.json());
    let v = v.json();
    assert_eq!(v["workspaceId"], json!(ws));
    assert_eq!(v["person"]["accountId"], json!(desk.svc.author().account_id.unwrap()));
    assert_eq!(v["person"]["memberId"], json!(desk.svc.author().id));
    assert_eq!(v["person"]["nameCollision"], json!(false));
    assert_eq!(v["phone"], json!({ "standing": "new" }));
    assert!(v["person"]["role"].is_string() && v["desktopLabel"].is_string() && v["workspaceName"].is_string(), "{v}");

    // A request carrying another phone's code from this channel: 403 device_mismatch.
    let stranger = ligus_core::testkit::Phone::new();
    let other = request_for(&scanned, &stranger, "not mine").encode();
    let r = c.request("POST", "/v1/phone-enroll/requests", Some(json!({ "request": other }))).await.unwrap();
    assert_eq!((r.status, r.json()["error"].clone()), (403, json!("device_mismatch")));

    // The phone's request.
    let req = request_for(&scanned, &phone, "Ana's iPhone");
    let text = req.encode();
    let r = c.request("POST", "/v1/phone-enroll/requests", Some(json!({ "request": text }))).await.unwrap();
    assert_eq!((r.status, r.json()), (202, json!({ "rid": f.rid, "state": "relayed" })));
    // T12: the same bytes again are idempotent.
    let r = c.request("POST", "/v1/phone-enroll/requests", Some(json!({ "request": text }))).await.unwrap();
    assert_eq!(r.status, 202);
    assert_eq!(h.handle.shared.phone_enroll.pending(), 1);

    // The desktop sees the phone, relayed by the box.
    let seen = desk.wait_request(&mut offer, Seen::First).await;
    assert_eq!(seen.code, code_of(&phone, "Ana's iPhone"));
    // The slot is sealed (the relay sees no request text) and box-signed.
    let raw = crate::relay_client(&desk.cfg).unwrap().list_presence(&desk.cfg.room).await.unwrap();
    let blob = raw.get(&request_slot(f.rid, box_id)).expect("the box's slot").to_string();
    assert!(!blob.contains(&text[qr::REQUEST_PREFIX.len()..]) && !blob.contains("ligusenrollreq1"), "sealed");

    // Waiting.
    let s = c.request("GET", &format!("/v1/phone-enroll/requests/{}?wait=1", f.rid), None).await.unwrap();
    assert_eq!((s.status, s.json()), (200, json!({ "rid": f.rid, "state": "waiting", "certified": false })));

    // The hold, then approval with the passkey (the paste flow's certification).
    let fp = code_of(&phone, "x").fingerprint();
    assert_eq!(offer.check_approve(offer.rid(), &fp, now_ms()).unwrap_err(), EnrollError::Hold);
    let code = offer.check_approve(offer.rid(), &fp, now_ms() + qr::HOLD_MS).unwrap().clone();
    let mut svc = crate::open_service(&h.desktop).unwrap();
    assert_eq!(svc.certify_phone_code(ws, &code).unwrap(), phone.device_id);
    offer.approved();
    desk.answer(&offer, Outcome::Certified, phone.device_id).await;
    crate::sync_once(&h.desktop).await.unwrap();

    // The phone: success is the box's `certified: true`; the answer is checked too.
    let deadline = tokio::time::Instant::now() + WAIT;
    let done = loop {
        h.handle.shared.sync_kick.notify_one();
        let s = c.request("GET", &format!("/v1/phone-enroll/requests/{}?wait=3", f.rid), None).await.unwrap().json();
        if s["certified"] == json!(true) && s["state"] == "answered" {
            break s;
        }
        assert!(tokio::time::Instant::now() < deadline, "never certified: {s}");
    };
    let answer = done["answer"].as_str().unwrap();
    assert_eq!(scanned.check_answer(answer, phone.device_id).unwrap(), PhoneVerdict::Mine(Outcome::Certified));
    // The box stops relaying the request once the answer names its phone.
    let relay_client = crate::relay_client(&desk.cfg).unwrap();
    let deadline = tokio::time::Instant::now() + WAIT;
    while relay_client.list_presence(&desk.cfg.room).await.unwrap().get(&request_slot(f.rid, box_id)) != Some(&json!({})) {
        assert!(tokio::time::Instant::now() < deadline, "the box never cleared its slot");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    drop(c);

    // P6: the same window, E3a binds now; then who am I.
    let (mut c, pid, _) = testkit::phone_enroll(&info, &keys, &ps, "Ana's iPhone").await.unwrap();
    assert_eq!(t.registry.get(&pid).unwrap().device_id, Some(phone.device_id));
    let me = c.request("GET", "/v1/me", None).await.unwrap();
    assert_eq!(me.status, 200, "{:?}", me.json());
    assert_eq!(me.json()["device"]["id"], json!(phone.device_id));
    assert_eq!(me.json()["account"]["id"], json!(desk.svc.author().account_id.unwrap()));

    // T10: the box's log names the request by rid, never its secret parts.
    let log = crate::serve::phone_enroll_api::test_log().lock().unwrap().join("\n");
    assert!(log.contains(&f.rid.to_b64u()), "{log}");
    let keys_ = raw_offer.keys();
    let hexes = |b: &[u8]| [hex::encode(b), hex::encode_upper(b), tun::b64u(b)];
    let mut needles = vec![qr_text.clone(), text.clone(), text[qr::REQUEST_PREFIX.len()..].to_string(), code.encode()];
    for b in [&secret[..], &keys_.request_mac_key()[..], &keys_.answer_mac_key()[..], &req.mac[..]] {
        needles.extend(hexes(b));
    }
    for n in needles {
        assert!(!log.contains(&n), "the log holds a secret part: {n}");
    }
    h.handle.shutdown();
}

/// The box's refusals (§7.2, §7.3) and the attacks that go through it, on
/// the direct path (bearer): T1 (stolen QR: two phones abort the offer
/// before approval, trip the wire after it, and the second phone sees an
/// answer about another device), T2 (only a service member's slot counts;
/// an answer not signed by the offer's desktop isn't relayed), T4 (no `exp`
/// past now + 420), T6 (lost answer: `certified: true` is success), T11
/// (substitution: another code under the phone's signature, a wrong MAC),
/// T12 (replay into another offer), T13 (revoked phone, unknown desktop)
/// and T15 (a restarted phone is the same phone).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_box_refuses_and_relays_and_the_desktop_catches_two_phones() {
    let (endpoint, _) = spawn_model(vec![]).await;
    let h = harness(&endpoint, "m", &[("README.md", "hi\n")]).await;
    let ws = h.handle.shared.ws;
    let box_id = h.handle.shared.lock().device_id();
    let desk = Desk::open(&h);
    rosters_ready(&h, &desk).await;
    let hr = &h;
    let post = move |path: &'static str, body: Value| status_of(hr, reqwest::Method::POST, path, Some(body));
    let (_, mut offer) = desk.offer(tun::random_bytes());
    let scanned = PhoneOffer::scan(&offer.qr_text(), now_ms()).unwrap();
    let f = scanned.fields();

    // Anonymous: 401.
    let r = h.client.post(format!("{}/v1/phone-enroll/check", h.base)).json(&check_body(&f)).send().await.unwrap();
    assert_eq!(r.status(), 401);
    // Wrong workspace, expiry window (T4: nothing past now + 420), unknown desktops.
    let mut b = check_body(&f);
    b["workspaceId"] = json!(Uuid::new_v4());
    assert_eq!(post("/v1/phone-enroll/check", b).await, (409, json!({ "error": "wrong_workspace", "message": qr::BoxRefusal::WrongWorkspace.to_string() })));
    let now = (now_ms() / 1000) as u64;
    for exp in [now - 200, now + 600] {
        let mut b = check_body(&f);
        b["exp"] = json!(exp);
        assert_eq!(post("/v1/phone-enroll/check", b).await.1["error"], json!("offer_expired"), "exp {exp}");
    }
    // T4: a well-formed, signed request whose offer expires too far ahead.
    let fp = ligus_core::testkit::Phone::new();
    let far = Offer { workspace_id: ws, desktop_device_id: desk.id, secret: EnrollSecret::from_bytes([5; 32]), exp: now + 600 };
    let far_req = EnrollRequest::build(&far, &code_of(&fp, "x"), |m| ligus_core::crypto::sign_p256(&fp.key, m));
    assert_eq!(post("/v1/phone-enroll/requests", json!({ "request": far_req.encode() })).await.1["error"], json!("offer_expired"));
    // T13: a desktop id that isn't a person's device here.
    for d in [Uuid::new_v4(), box_id] {
        let mut b = check_body(&f);
        b["desktopDeviceId"] = json!(d);
        assert_eq!(post("/v1/phone-enroll/check", b).await.1["error"], json!("desktop_unknown"), "{d}");
    }
    // A fresh phone: 200, standing new.
    let a = ligus_core::testkit::Phone::new();
    let mut b = check_body(&f);
    b["deviceId"] = json!(a.device_id);
    let (code, v) = post("/v1/phone-enroll/check", b).await;
    assert_eq!((code, v["phone"].clone()), (200, json!({ "standing": "new" })), "{v}");

    // Malformed, and T11: another phone's code under this phone's signature.
    assert_eq!(post("/v1/phone-enroll/requests", json!({ "request": "ligusenrollreq1:AAAA" })).await.1["error"], json!("bad_request"));
    let good = request_for(&scanned, &a, "Ana's iPhone");
    let swapped = EnrollRequest { code: code_of(&ligus_core::testkit::Phone::new(), "Mallory"), ..good.clone() };
    assert_eq!(post("/v1/phone-enroll/requests", json!({ "request": swapped.encode() })).await.1["error"], json!("bad_request"));
    // T11: a wrong MAC passes the box (it can't check it) but not the desktop.
    let bad_mac = EnrollRequest { mac: [7; 32], ..request_for(&scanned, &ligus_core::testkit::Phone::new(), "M") };
    assert_eq!(post("/v1/phone-enroll/requests", json!({ "request": bad_mac.encode() })).await.0, 202);
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let res = desk.poll(&mut offer).await;
        if res.iter().any(|r| matches!(r, Err(EnrollError::BadMac))) {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{res:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(offer.locked_request().is_none(), "a wrong MAC never locks the offer");

    // T2: a slot sealed with the workspace key and signed by a person's
    // device (the desktop's own) is dropped; the box's is taken.
    let now = now_ms();
    let forged = desk.svc.sign_relayed_request(RelayedRequest::new(&good.encode(), desk.id, now));
    assert!(desk.svc.enroll_relay_key(ws, desk.id).is_none());
    assert_eq!(check_relayed_request(&request_slot(f.rid, desk.id), &forged, now, |d| desk.svc.enroll_relay_key(ws, d)), Err(EnrollError::BadSlotSignature));
    let boxed = h.handle.shared.lock().sign_relayed_request(RelayedRequest::new(&good.encode(), box_id, now));
    assert_eq!(check_relayed_request(&request_slot(f.rid, box_id), &boxed, now, |d| desk.svc.enroll_relay_key(ws, d)), Ok(f.rid));
    let _ = seal_relayed_request(&desk.ring(), &forged).expect("any key holder can seal; the signature is what counts");

    // The honest phone A, then T15: A again after a restart (a new signature).
    assert_eq!(post("/v1/phone-enroll/requests", json!({ "request": good.encode() })).await.0, 202);
    desk.wait_request(&mut offer, Seen::First).await;
    // (Test signatures are deterministic, RFC 6979; a renamed phone gives
    // other bytes the same way the Secure Enclave's random nonces do.)
    let again = request_for(&scanned, &a, "Ana's iPhone (2)");
    assert_ne!(again.encode(), good.encode());
    assert_eq!(post("/v1/phone-enroll/requests", json!({ "request": again.encode() })).await.0, 202);
    assert_eq!(h.handle.shared.phone_enroll.pending_for(a.device_id), 1, "replaced, not added");
    assert_eq!(offer.observe(&again, now_ms()).unwrap(), Seen::SamePhone);
    // T1 before approval: a second phone with the same QR aborts the offer.
    let m = ligus_core::testkit::Phone::new();
    assert_eq!(post("/v1/phone-enroll/requests", json!({ "request": request_for(&scanned, &m, "Ana's iPhone").encode() })).await.0, 202);
    desk.wait_request(&mut offer, Seen::TwoPhones).await;
    assert!(offer.is_aborted());

    // T12: a request for that offer presented to a new one is refused.
    let (_, mut offer2) = desk.offer(tun::random_bytes());
    assert_eq!(offer2.check_request(&good.encode(), now_ms()).unwrap_err(), EnrollError::WrongOffer);
    let scanned2 = PhoneOffer::scan(&offer2.qr_text(), now_ms()).unwrap();
    let f2 = scanned2.fields();
    let stale = offer.answer(Outcome::Certified, a.device_id).encode();
    assert_eq!(scanned2.check_answer(&stale, a.device_id).unwrap_err(), EnrollError::WrongOffer);

    // T1 after approval, with T6: phone C is approved, the answer is lost.
    let c = ligus_core::testkit::Phone::new();
    let status = |device: Uuid, wait: u64| {
        let path = format!("/v1/phone-enroll/requests/{}?wait={wait}&deviceId={device}", f2.rid);
        async move { status_of(hr, reqwest::Method::GET, &path, None).await }
    };
    assert_eq!(post("/v1/phone-enroll/requests", json!({ "request": request_for(&scanned2, &c, "C").encode() })).await.0, 202);
    let locked = desk.wait_request(&mut offer2, Seen::First).await;
    let code = offer2.check_approve(offer2.rid(), &locked.code.fingerprint(), now_ms() + qr::HOLD_MS).unwrap().clone();
    crate::open_service(&h.desktop).unwrap().certify_phone_code(ws, &code).unwrap();
    offer2.approved();
    crate::sync_once(&h.desktop).await.unwrap();
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        h.handle.shared.sync_kick.notify_one();
        let (_, s) = status(c.device_id, 2).await;
        if s["certified"] == json!(true) {
            assert_eq!(s["state"], "waiting", "T6: no answer, still a success");
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{s}");
    }
    // T2: an answer slot not signed by the offer's desktop isn't relayed.
    let fake = h.handle.shared.lock().sign_relayed_answer(RelayedAnswer::new(&offer2.answer(Outcome::Declined, c.device_id), desk.id, now_ms()));
    let blob = seal_relayed_answer(&desk.ring(), &fake).unwrap();
    crate::relay_client(&desk.cfg).unwrap().publish_presence(&desk.cfg.room, &answer_slot(offer2.rid()), &blob).await.unwrap();
    // The stolen QR's phone D answers after approval: the tripwire.
    let d = ligus_core::testkit::Phone::new();
    assert_eq!(post("/v1/phone-enroll/requests", json!({ "request": request_for(&scanned2, &d, "Ana's iPhone").encode() })).await.0, 202);
    let tripped = desk.wait_request(&mut offer2, Seen::Tripwire).await;
    assert_eq!(tripped.device_id(), d.device_id);
    // The box has looked at the answer slots since the forged one went up.
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert_eq!(status(d.device_id, 0).await.1["state"], "waiting", "the forged answer wasn't taken");
    // The desktop keeps answering "certified" for C: D's status carries it, D's phone alarms.
    desk.answer(&offer2, Outcome::Certified, c.device_id).await;
    let deadline = tokio::time::Instant::now() + WAIT;
    let ans = loop {
        let (_, s) = status(d.device_id, 3).await;
        if let Some(a) = s["answer"].as_str() {
            assert_eq!((s["state"].clone(), s["certified"].clone()), (json!("answered"), json!(false)));
            break a.to_string();
        }
        assert!(tokio::time::Instant::now() < deadline, "{s}");
    };
    assert_eq!(scanned2.check_answer(&ans, d.device_id).unwrap(), PhoneVerdict::AnotherPhone(Outcome::Certified));
    assert_eq!(scanned2.check_answer(&ans, c.device_id).unwrap(), PhoneVerdict::Mine(Outcome::Certified));

    // T13: a revoked phone.
    let rv = ligus_core::testkit::Phone::new();
    {
        let mut owner = crate::open_service(&h.desktop).unwrap();
        owner.certify_phone_code(ws, &code_of(&rv, "old")).unwrap();
        owner.revoke_device(ws, ws, rv.device_id).unwrap();
    }
    crate::sync_once(&h.desktop).await.unwrap();
    wait_until("the box folds the revocation", WAIT, || {
        h.handle.shared.sync_kick.notify_one();
        matches!(h.handle.shared.device_lookup(rv.device_id), tun::device_bind::DeviceLookup::Revoked)
    })
    .await;
    let (_, mut offer3) = desk.offer(tun::random_bytes());
    let scanned3 = PhoneOffer::scan(&offer3.qr_text(), now_ms()).unwrap();
    let mut b = check_body(&scanned3.fields());
    b["deviceId"] = json!(rv.device_id);
    assert_eq!(post("/v1/phone-enroll/check", b).await.0, 403);
    let (code, v) = post("/v1/phone-enroll/requests", json!({ "request": request_for(&scanned3, &rv, "old").encode() })).await;
    assert_eq!((code, v["error"].clone()), (403, json!("device_revoked")));
    assert!(desk.poll(&mut offer3).await.is_empty(), "nothing relayed");

    // The status route: an unknown or malformed rid is 404.
    for rid in [Rid([9; 16]).to_b64u(), "nope".to_string()] {
        assert_eq!(status_of(&h, reqwest::Method::GET, &format!("/v1/phone-enroll/requests/{rid}"), None).await.0, 404);
    }
    h.handle.shutdown();
}

/// T3 (a co-member's QR) and the roster checks a co-member makes possible:
/// `check` names the desktop's account and flags a display name another
/// member shares; a phone certified for someone else is `someone_elses`.
/// And T5: a box that can't relay (here: it joined as a person, not as a
/// service member, so desktops would drop its slots) answers 503
/// `relay_unavailable` instead of taking requests it can't deliver.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_co_members_qr_is_named_and_a_box_that_cant_relay_says_so() {
    let (endpoint, _) = spawn_model(vec![]).await;
    let desktop_name = crate::box_name(crate::env_opt("LIGUS_SERVE_LABEL"), ligus_runtime::machine_hostname());
    let h = harness_with(&endpoint, "m", &[("README.md", "hi\n")], move |o| {
        o.test_person = true;
        o.name = Some(desktop_name.to_uppercase());
    })
    .await;
    let ws = h.handle.shared.ws;
    let desk = Desk::open(&h);
    let shared = h.handle.shared.clone();
    wait_until("the box knows the desktop's person", WAIT, || {
        shared.sync_kick.notify_one();
        shared.lock().enroll_desktop(ws, desk.id).is_ok()
    })
    .await;
    let person = shared.lock().enroll_desktop(ws, desk.id).unwrap();
    assert_eq!(person.account_id, desk.svc.author().account_id.unwrap());
    assert!(person.name_collision, "{person:?}: two members are called {}", person.display_name);

    // A phone certified here for the box's person is someone else's for the desktop.
    let p = ligus_core::testkit::Phone::new();
    shared.lock().certify_phone_code(ws, &code_of(&p, "theirs")).unwrap();
    let (_, offer) = desk.offer(tun::random_bytes());
    let scanned = PhoneOffer::scan(&offer.qr_text(), now_ms()).unwrap();
    let mut b = check_body(&scanned.fields());
    b["deviceId"] = json!(p.device_id);
    let (code, v) = status_of(&h, reqwest::Method::POST, "/v1/phone-enroll/check", Some(b)).await;
    assert_eq!((code, v["error"].clone()), (409, json!("someone_elses")), "{v}");
    let (code, v) = status_of(&h, reqwest::Method::POST, "/v1/phone-enroll/requests", Some(json!({ "request": request_for(&scanned, &p, "theirs").encode() }))).await;
    assert_eq!((code, v["error"].clone()), (409, json!("someone_elses")), "{v}");

    // T5: this box isn't a service member: it can't relay.
    let fresh = ligus_core::testkit::Phone::new();
    let (code, v) = status_of(&h, reqwest::Method::POST, "/v1/phone-enroll/check", Some(check_body(&scanned.fields()))).await;
    assert_eq!((code, v["error"].clone()), (503, json!("relay_unavailable")), "{v}");
    let (code, v) = status_of(&h, reqwest::Method::POST, "/v1/phone-enroll/requests", Some(json!({ "request": request_for(&scanned, &fresh, "mine").encode() }))).await;
    assert_eq!((code, v["error"].clone()), (503, json!("relay_unavailable")), "{v}");
    assert_eq!(shared.phone_enroll.pending(), 0);
    h.handle.shutdown();
}

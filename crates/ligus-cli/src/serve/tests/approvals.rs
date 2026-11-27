//! Code-match approval through `ligus serve` (`docs/design/code-match-wire.md`
//! section 5): a box requester posts to the workspace's approval mailbox, the
//! serve box lists it for a paired phone, and a simulated phone does the
//! approver's side by hand (P-256 key, its own `nA` and ephemeral key, its own
//! seals) through the box's API. The box only relays bytes (design note 11,
//! M6c box side), and the commit hands back the sealed admission the phone
//! signs into its result.

use base64::Engine as _;
use ligus_core::crypto::{device_id_for, SigAlg, SigningKeypair};
use ligus_core::e2ee::KeyAgreementKeypair;
use ligus_core::match_code::{self as mc, hex, MailboxKeys, MatchKind, Transcript};
use ligus_runtime::approval_mailbox::{self as am, ApprovalAddress, MailboxAuth, MailboxClient, RequestSpec, Requester, RequesterEvent, Signer, Verified};
use ligus_runtime::box_admission::JoinRequest;
use sha2::Digest as _;

use super::*;

pub(super) fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

pub(super) fn unb64(s: &str) -> Vec<u8> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).unwrap()
}

fn rand32() -> [u8; 32] {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).unwrap();
    b
}

/// A box asking to join: its device key, key-agreement key and join request.
struct BoxDev {
    key: SigningKeypair,
    ka: KeyAgreementKeypair,
    request: JoinRequest,
}

impl BoxDev {
    fn new(label: &str) -> Self {
        let key = SigningKeypair::generate().unwrap();
        let ka = KeyAgreementKeypair::generate().unwrap();
        let pk = key.public_key_bytes();
        let request = JoinRequest::new(Uuid::new_v4(), device_id_for(SigAlg::Ed25519, &pk), &pk, &ka.public_key_bytes(), label);
        Self { key, ka, request }
    }
    fn spec(&self) -> RequestSpec {
        RequestSpec {
            kind: MatchKind::Box,
            code: self.request.encode(),
            signer: Signer::Ed25519(SigningKeypair::from_seed(&self.key.seed_bytes()).unwrap()),
            ka: Some(KeyAgreementKeypair::from_seed(&self.ka.seed_bytes()).unwrap()),
        }
    }
}

/// A signed envelope (wire note 3.2) by a phone's P-256 key.
pub(super) fn phone_envelope(t: &str, body: &Value, phone: &ligus_core::testkit::Phone) -> String {
    let body = body.to_string();
    let pre = mc::message_preimage(t, &body).unwrap();
    let sig = ligus_core::crypto::sign_p256(&phone.key, &pre);
    json!({ "t": t, "alg": "p256", "key": b64(&phone.public_key()), "body": body, "sig": b64(&sig) }).to_string()
}

pub(super) struct World {
    pub(super) h: Harness,
    pub(super) ws: Uuid,
    /// A member's mailbox client on the relay (to read what it stores).
    pub(super) relay: MailboxClient,
    pub(super) epoch: am::EpochKey,
    pub(super) address: ApprovalAddress,
    pub(super) phone: ligus_core::testkit::Phone,
    pub(super) account: Uuid,
}

pub(super) async fn kick_until<F: Fn() -> bool>(h: &Harness, what: &str, done: F) {
    for _ in 0..300 {
        if done() {
            return;
        }
        h.handle.shared.sync_kick.notify_one();
        h.handle.shared.approvals.kick.notify_one();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

/// A serve box (a service member) in a desktop's workspace, a phone the
/// desktop certified for its person (an Admin), and the box's approval
/// mailbox registered by the box itself.
pub(super) async fn world() -> World {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let mut h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    // Through the tunnel in real life; the operator on loopback here (the
    // routes need a secret channel).
    h.as_operator = true;
    let ws = h.handle.shared.ws;
    let phone = ligus_core::testkit::Phone::new();
    let account = {
        let mut desk = open_service(&h.desktop).unwrap();
        desk.certify_phone(ws, phone.device_id, &phone.public_key()).unwrap();
        desk.author().account_id.unwrap()
    };
    sync_once(&h.desktop).await.unwrap();
    kick_until(&h, "the phone's certificate on the box", || {
        h.handle.shared.lock().workspace_roster(ws).is_ok_and(|r| r.devices.get(&phone.device_id).is_some_and(|d| d.phone && !d.revoked))
    })
    .await;
    let keys = h.handle.shared.lock().approval_epoch_keys(ws, &crate::key_source(&h.handle.shared.cfg).unwrap()).unwrap();
    let epoch = keys.iter().max_by_key(|e| e.epoch).unwrap().clone();
    let relay = MailboxClient::new(h.desktop.relay_url.as_deref().unwrap());
    let mailbox = MailboxKeys::derive(&epoch.key).mailbox();
    // The box registers the mailbox the relay doesn't know (yet).
    let mut counter = None;
    for _ in 0..300 {
        counter = relay.counter(&mailbox).await.unwrap();
        if counter.is_some() {
            break;
        }
        h.handle.shared.approvals.kick.notify_one();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let counter = counter.expect("the box registered the approval mailbox");
    let fk = h.handle.shared.lock().founder_pin_hex(ws).unwrap().unwrap();
    let address = ApprovalAddress::derive(relay.relay(), ws, &fk, epoch.epoch, &epoch.key, counter);
    World { h, ws, relay, epoch, address, phone, account }
}

impl World {
    async fn post(&self, dev: &BoxDev) -> Requester {
        Requester::post(self.relay.clone(), &self.address, dev.spec(), None).await.unwrap()
    }

    async fn approvals(&self) -> Vec<Value> {
        self.h.get("/v1/approvals").await["approvals"].as_array().unwrap().clone()
    }

    async fn approval(&self, rid: &str) -> Value {
        for _ in 0..100 {
            if let Some(a) = self.approvals().await.into_iter().find(|a| a["rid"] == rid) {
                return a;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("request {rid} isn't listed");
    }

    /// The row after the box's listing went stale (it reuses one for a
    /// second), so it shows what was posted since.
    async fn fresh_approval(&self, rid: &str) -> Value {
        tokio::time::sleep(Duration::from_millis(1100)).await;
        self.approval(rid).await
    }

    async fn msgs(&self, rid: &str, after: u64) -> Vec<Value> {
        self.h.get(&format!("/v1/approvals/{rid}/msgs?after={after}")).await["msgs"].as_array().unwrap().clone()
    }

    async fn post_msg(&self, rid: &str, what: &str, blob: &str, to: &str) -> (u16, Value) {
        status_of(&self.h, reqwest::Method::POST, &format!("/v1/approvals/{rid}/{what}"), Some(json!({ "blob": blob, "to": to }))).await
    }

    fn member_keys(&self) -> MailboxKeys {
        MailboxKeys::derive(&self.epoch.key)
    }
}

/// The phone's side of one exchange, by hand.
struct PhoneApprover {
    rid: String,
    request: am::RequestBody,
    ka: KeyAgreementKeypair,
    n_a: [u8; 32],
}

impl PhoneApprover {
    /// Parse and verify the listed request's envelope itself (never the
    /// box's preview).
    fn read(row: &Value) -> Self {
        let v = Verified::parse(row["request"].as_str().unwrap().as_bytes()).expect("the request's signature verifies on the phone");
        assert_eq!(v.t, am::MsgType::Request);
        let request: am::RequestBody = serde_json::from_str(&v.body).unwrap();
        let code = JoinRequest::decode(&request.code).unwrap();
        assert_eq!(code.device_public_key, v.key, "signed by the key the code carries");
        Self { rid: request.rid.clone(), request, ka: KeyAgreementKeypair::generate().unwrap(), n_a: rand32() }
    }

    fn requester_ka(&self) -> [u8; 32] {
        unb64(&self.request.requester_ka).try_into().unwrap()
    }

    fn claim(&self, phone: &ligus_core::testkit::Phone, seal_key: &[u8; 32]) -> String {
        let body = json!({ "rid": self.rid, "approverDeviceId": phone.device_id, "approverName": "Ana's phone", "at": ligus_core::flow::now_ms() });
        am::seal_blob(seal_key, phone_envelope("claim", &body, phone).as_bytes()).unwrap()
    }

    fn open(&self, phone: &ligus_core::testkit::Phone, account: Uuid) -> String {
        let body = json!({
            "rid": self.rid,
            "approverDeviceId": phone.device_id,
            "approverSigKey": b64(&phone.public_key()),
            "approverKa": b64(&self.ka.public_key_bytes()),
            "approverPerson": { "accountId": account, "name": "Ana" },
            "nA": b64(&self.n_a),
            "at": ligus_core::flow::now_ms(),
        });
        am::seal_blob(&self.requester_ka(), phone_envelope("open", &body, phone).as_bytes()).unwrap()
    }

    /// The reveal's `nR`, checked against the request's commit.
    fn reveal(&self, blob: &str) -> [u8; 32] {
        let plain = am::open_blob(&self.ka, blob).expect("the reveal is sealed to the phone's ephemeral key");
        let v = Verified::parse(&plain).unwrap();
        assert_eq!(v.t, am::MsgType::Reveal);
        assert_eq!(b64(&v.key), b64(&JoinRequest::decode(&self.request.code).unwrap().device_public_key), "signed by the requesting box");
        let body: am::RevealBody = serde_json::from_str(&v.body).unwrap();
        let n_r: [u8; 32] = unb64(&body.n_r).try_into().unwrap();
        let rid: [u8; 16] = mc::unhex(&self.rid).unwrap().try_into().unwrap();
        assert_eq!(b64(&mc::commit(&rid, &n_r)), self.request.commit, "the reveal matches the commit");
        n_r
    }

    fn code(&self, phone: &ligus_core::testkit::Phone, n_r: &[u8; 32]) -> u64 {
        let rid: [u8; 16] = mc::unhex(&self.rid).unwrap().try_into().unwrap();
        let mailbox: [u8; 16] = mc::unhex(&self.request.mailbox).unwrap().try_into().unwrap();
        let commit: [u8; 32] = unb64(&self.request.commit).try_into().unwrap();
        let t = Transcript {
            kind: MatchKind::Box,
            mailbox: &mailbox,
            rid: &rid,
            request_code: self.request.code.as_bytes(),
            requester_ka: &self.requester_ka(),
            commit: &commit,
            approver_device_id: phone.device_id,
            approver_sig_key: &phone.public_key(),
            approver_ka: &self.ka.public_key_bytes(),
            n_a: &self.n_a,
        };
        mc::code40(&t.digest(), n_r)
    }

    fn result(&self, phone: &ligus_core::testkit::Phone, outcome: &str, grant: Option<&str>) -> String {
        let digest = grant.map(|g| hex(&sha2::Sha256::digest(g.as_bytes())));
        let body = json!({ "rid": self.rid, "outcome": outcome, "grant": grant, "grantDigest": digest });
        am::seal_blob(&self.requester_ka(), phone_envelope("result", &body, phone).as_bytes()).unwrap()
    }

    fn abort(&self, phone: &ligus_core::testkit::Phone, to: &[u8; 32]) -> String {
        let body = json!({ "rid": self.rid, "reason": "mismatch" });
        am::seal_blob(to, phone_envelope("abort", &body, phone).as_bytes()).unwrap()
    }
}

/// The phone's admission of the requesting box through the existing
/// prepare, presence and commit: the sealed `ligusadmit1:` text.
async fn admit(w: &World, request: &JoinRequest) -> String {
    let prep = w
        .h
        .send(reqwest::Method::POST, "/v1/admissions/prepare", Some(json!({ "deviceId": w.phone.device_id, "request": request.encode() })))
        .await;
    let challenge: [u8; 32] = unb64(prep["challenge"].as_str().unwrap()).try_into().unwrap();
    let a = ligus_core::testkit::test_recovery_key(w.account).assert(&challenge);
    let id = prep["prepareId"].as_str().unwrap().to_string();
    let pre = w
        .h
        .send(
            reqwest::Method::POST,
            "/v1/admissions/presence",
            Some(json!({ "prepareId": id, "assertion": { "credentialId": a.credential_id, "authenticatorData": a.authenticator_data, "clientDataJSON": a.client_data_json, "signature": a.signature } })),
        )
        .await;
    let sig = ligus_core::crypto::sign_p256(&w.phone.key, &unb64(pre["preimage"].as_str().unwrap()));
    let done = w
        .h
        .send(reqwest::Method::POST, "/v1/admissions/commit", Some(json!({ "prepareId": id, "signature": b64(&sig) })))
        .await;
    assert_eq!(done["sealed"], json!(true));
    let code = done["code"].as_str().unwrap().to_string();
    assert!(code.starts_with(ligus_runtime::box_admission::SEALED_PREFIX), "the commit hands back the sealed admission");
    code
}

/// The whole exchange: listed with the right fields, the phone claims and
/// opens through the box, reads the reveal, checks the commit, both sides
/// show the same code, and the requester takes the phone's signed result
/// carrying the sealed admission the commit handed back. What the relay
/// stores is what the phone posted, byte for byte; the box opens only the
/// members' blobs; one push went out for the request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_phone_approves_a_box_through_serve_by_code() {
    let w = world().await;
    let dev = BoxDev::new("build-1");
    let mut req = w.post(&dev).await;
    let rid = req.rid_hex();

    let row = w.approval(&rid).await;
    assert_eq!(row["workspaceId"], json!(w.ws));
    assert_eq!(row["kind"], "box");
    assert_eq!(row["label"], "build-1");
    assert_eq!(row["mailbox"], json!(w.address.mailbox_hex()));
    assert_eq!(row["epoch"], json!(w.epoch.epoch));
    let seal_key: [u8; 32] = unb64(row["sealKey"].as_str().unwrap()).try_into().unwrap();
    assert_eq!(seal_key, w.address.seal_key);
    assert!(row["putAt"].as_i64().unwrap() > 0 && row["expiresAt"].as_i64().unwrap() > row["putAt"].as_i64().unwrap());
    assert!(row["workspaceName"].is_string());
    assert_eq!(row["claims"], json!([]));
    let p = PhoneApprover::read(&row);
    assert_eq!(p.rid, rid);
    assert_eq!(p.request.code, dev.request.encode(), "the request's code, unchanged");

    // One push for the request, however often it is listed.
    kick_until(&w.h, "the push", || w.h.handle.shared.approvals.pushed().contains(&rid)).await;
    w.approvals().await;
    w.h.handle.shared.approvals.kick.notify_one();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(w.h.handle.shared.approvals.pushed().iter().filter(|r| **r == rid).count(), 1);

    // Claim (to the members) and open (to the requester), sealed by the phone.
    let claim = p.claim(&w.phone, &seal_key);
    let (c, v) = w.post_msg(&rid, "claim", &claim, "members").await;
    assert_eq!((c, v["seq"].as_u64()), (201, Some(1)), "{v}");
    let open = p.open(&w.phone, w.account);
    let (c, v) = w.post_msg(&rid, "open", &open, "requester").await;
    assert_eq!((c, v["seq"].as_u64()), (201, Some(2)), "{v}");

    // The relay holds the phone's bytes exactly.
    let stored = w.relay.get_msgs(MailboxAuth::Member(&w.member_keys()), &w.address.mailbox, &req.rid(), 0, 0).await.unwrap();
    assert_eq!(stored.iter().map(|m| (m.blob.as_str(), m.to.as_str())).collect::<Vec<_>>(), vec![(claim.as_str(), "members"), (open.as_str(), "requester")]);

    // The claim shows, verified (the phone is a certified device).
    let row = w.fresh_approval(&rid).await;
    assert_eq!(row["claims"][0]["approverDeviceId"], json!(w.phone.device_id));
    assert_eq!(row["claims"][0]["approverName"], "Ana's phone");
    assert_eq!(row["claims"][0]["verified"], true);

    // The requester answers the open with its reveal and shows the code.
    let events = req.poll(0).await.unwrap();
    let Some(RequesterEvent::Opened { code: box_code, claimed }) = events.into_iter().next() else { panic!("no open") };
    assert_eq!(claimed.device_id, w.phone.device_id);

    // The phone reads everything through the box: the claim opened for it
    // (members), the open and the reveal passed through sealed.
    let msgs = w.msgs(&rid, 0).await;
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0]["blob"], claim);
    let plain = msgs[0]["plain"].as_str().expect("a members' blob is opened for the phone");
    assert_eq!(Verified::parse(plain.as_bytes()).unwrap().t, am::MsgType::Claim);
    assert_eq!(msgs[1]["blob"], open);
    assert!(msgs[1]["plain"].is_null(), "the open is sealed to the requester");
    assert_eq!(msgs[2]["to"], "approver");
    assert!(msgs[2]["plain"].is_null(), "the box can't open the reveal");
    assert_eq!(msgs[2]["from"], "requester");
    assert!(msgs[2]["at"].as_i64().unwrap() > 0 && msgs[2]["seq"] == 3);
    assert_eq!(w.msgs(&rid, 2).await.len(), 1, "after=N");
    let n_r = p.reveal(msgs[2]["blob"].as_str().unwrap());
    assert_eq!(p.code(&w.phone, &n_r), box_code.code40, "both screens show the same code");

    // Codes match: the phone admits the box and signs the result with the
    // admission's digest; the requester's checks 1 to 4 take it.
    let grant = admit(&w, &dev.request).await;
    let result = p.result(&w.phone, "approved", Some(&grant));
    let (c, _) = w.post_msg(&rid, "result", &result, "requester").await;
    assert_eq!(c, 201);
    let events = req.poll(0).await.unwrap();
    let Some(RequesterEvent::Result(r)) = events.into_iter().next() else { panic!("no result") };
    let checked = req.check_grant(&r, am::GrantTarget::Box { ka: &dev.ka }, ligus_core::flow::now_ms()).expect("the grant passes checks 1 to 4");
    assert!(matches!(checked, am::CheckedGrant::Box { .. }));
    w.h.handle.shutdown();
}

/// The box relays bytes and nothing more: a tampered open reaches the relay
/// as posted and the requester can't open it (no code, no reveal), the box
/// refuses a wrong routing hint or a blob that isn't sealed, a claim by an
/// uncertified device shows unverified, the phone's two aborts end the
/// request (listed no more, its members' abort opened for the phone), and an
/// unknown request is a 404.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_box_only_relays_what_the_phone_sealed() {
    let w = world().await;
    let dev = BoxDev::new("build-2");
    let mut req = w.post(&dev).await;
    let rid = req.rid_hex();
    let row = w.approval(&rid).await;
    let seal_key: [u8; 32] = unb64(row["sealKey"].as_str().unwrap()).try_into().unwrap();
    let p = PhoneApprover::read(&row);

    // Wrong hints and non-blobs are refused before the relay.
    let claim = p.claim(&w.phone, &seal_key);
    assert_eq!(w.post_msg(&rid, "claim", &claim, "requester").await.0, 400);
    assert_eq!(w.post_msg(&rid, "open", &claim, "members").await.0, 400);
    assert_eq!(w.post_msg(&rid, "result", &claim, "approver").await.0, 400);
    assert_eq!(w.post_msg(&rid, "claim", "", "members").await.0, 400);
    assert_eq!(w.post_msg(&rid, "claim", "not base64!", "members").await.0, 400);
    assert_eq!(w.post_msg(&rid, "claim", &"A".repeat(am::MAX_MSG_BLOB + 4), "members").await.0, 400);
    assert_eq!(w.post_msg(&"0".repeat(32), "claim", &claim, "members").await.0, 404, "an unknown request");
    assert_eq!(w.post_msg(&rid, "reveal", &claim, "approver").await.0, 404, "no such route");
    assert_eq!(status_of(&w.h, reqwest::Method::GET, &format!("/v1/approvals/{}/msgs", "0".repeat(32)), None).await.0, 404);

    // A claim by a device the workspace never certified: unverified.
    let stranger = ligus_core::testkit::Phone::new();
    let (c, _) = w.post_msg(&rid, "claim", &p.claim(&stranger, &seal_key), "members").await;
    assert_eq!(c, 201);
    let row = w.fresh_approval(&rid).await;
    assert_eq!(row["claims"][0]["approverDeviceId"], json!(stranger.device_id));
    assert_eq!(row["claims"][0]["verified"], false);

    // A tampered open: stored as posted, opened by nobody.
    let open = p.open(&w.phone, w.account);
    let mut bytes = unb64(&open);
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    let tampered = b64(&bytes);
    assert_eq!(w.post_msg(&rid, "open", &tampered, "requester").await.0, 201);
    let stored = w.relay.get_msgs(MailboxAuth::Member(&w.member_keys()), &w.address.mailbox, &req.rid(), 1, 0).await.unwrap();
    assert_eq!(stored[0].blob, tampered);
    assert!(req.poll(0).await.unwrap().is_empty(), "a tampered open is nothing to the requester");
    assert!(req.code().is_none());

    // The codes didn't match: the phone ends it to the members and the requester.
    let (c, _) = w.post_msg(&rid, "abort", &p.abort(&w.phone, &seal_key), "members").await;
    assert_eq!(c, 201);
    let (c, _) = w.post_msg(&rid, "abort", &p.abort(&w.phone, &p.requester_ka()), "requester").await;
    assert_eq!(c, 201);
    let msgs = w.msgs(&rid, 0).await;
    let aborts: Vec<&Value> = msgs.iter().filter(|m| m["plain"].as_str().is_some_and(|p| Verified::parse(p.as_bytes()).is_ok_and(|v| v.t == am::MsgType::Abort))).collect();
    assert_eq!(aborts.len(), 1, "only the members' abort opens on the box");
    assert!(matches!(req.poll(0).await.unwrap().first(), Some(RequesterEvent::Aborted { .. })));
    // Ended: listed no more (the listing is fresh after a second).
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(!w.approvals().await.iter().any(|a| a["rid"] == rid));
    w.h.handle.shutdown();
}

/// A relay without mailbox routes: the feature is off, the list is empty and
/// nothing is an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_relay_without_mailboxes_lists_nothing() {
    let (endpoint, _) = spawn_model(vec![vec![delta(json!({"content": "hi"}))]]).await;
    let mut h = harness(&endpoint, "m", &[("README.md", "x\n")]).await;
    h.as_operator = true;
    let bare = spawn(Router::new()).await;
    h.handle.shared.approvals.set_relay_for_test(&bare);
    // (A poll of the real relay may be in flight; the next looks at the bare one.)
    kick_until(&h, "the box to see no mailbox routes", || h.handle.shared.approvals.no_routes()).await;
    let v = h.get("/v1/approvals").await;
    assert_eq!(v, json!({ "approvals": [] }));
    assert_eq!(h.get("/v1/approvals").await, json!({ "approvals": [] }));
    let (c, _) = status_of(&h, reqwest::Method::GET, &format!("/v1/approvals/{}/msgs", "a".repeat(32)), None).await;
    assert_eq!(c, 404);
    h.handle.shutdown();
}

/// The `approval.request` push: its kind, payload (`workspaceId`, `rid`,
/// `label`, `requestKind`) and title, and no chat id.
#[test]
fn the_approval_push_carries_the_request_not_a_chat() {
    let dev = BoxDev::new("build-1");
    let ws = Uuid::new_v4();
    let key = rand32();
    let keys = MailboxKeys::derive(&key);
    let rid = [3u8; 16];
    let plain = {
        let body = json!({
            "v": 1, "kind": "box", "rid": hex(&rid), "mailbox": hex(&keys.mailbox()), "code": dev.request.encode(),
            "requesterKa": b64(&dev.ka.public_key_bytes()), "readKey": b64(&[1; 16]), "commit": b64(&[2; 32]),
            "createdAt": ligus_core::flow::now_ms(),
        })
        .to_string();
        let pre = mc::message_preimage("request", &body).unwrap();
        json!({ "t": "request", "alg": "ed25519", "key": b64(&dev.key.public_key_bytes()), "body": body, "sig": b64(&dev.key.sign(&pre)) }).to_string()
    };
    let request = am::VerifiedRequest::verify(plain.as_bytes(), &rid, &keys.mailbox(), ligus_core::flow::now_ms()).unwrap();
    let live = am::LiveRequest {
        relay: "https://relay.example".into(),
        epoch: 0,
        mailbox: keys.mailbox(),
        request,
        put_at: 1,
        expires_at: 2,
        msg_count: 0,
        claims: Vec::new(),
        opened_without_claim: false,
        ended: None,
        requester_msgs_before_reveal: 0,
        revealed: false,
        listed_at: 0,
    };
    let content = crate::serve::approvals_api::push_content(ws, "Acme", &live);
    let pt = crate::serve::tunnel::push_plaintext(&content, false, 7, 9);
    assert_eq!(pt["kind"], "approval.request");
    assert_eq!(pt["workspaceId"], json!(ws));
    assert_eq!(pt["rid"], json!(hex(&rid)));
    assert_eq!(pt["label"], "build-1");
    assert_eq!(pt["requestKind"], "box");
    assert_eq!(pt["title"], "build-1 wants to join Acme");
    assert!(pt.get("chatId").is_none() && pt.get("preview").is_none());
    assert_eq!(content.collapse.as_deref(), Some(format!("approvals:{ws}").as_str()), "collapses per workspace");
}

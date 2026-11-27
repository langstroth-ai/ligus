//! The passkey hand-off through `ligus serve` to a phone
//! (`docs/design/code-match-wire.md` section 8): a terminal of Ana's (her
//! desktop's identity stands in for it) posts a hand-off; the box lists it,
//! with what the phone shows and signs, only to a phone it places as Ana,
//! never to another person's phone, one it can't place, or the operator;
//! and only Ana's phone can read or answer it through the box. A simulated
//! phone does its side by hand (P-256 key, its own seals) and the terminal
//! takes its answer and stores the draft.

use axum::http::Request;
use ligus_core::match_code::{self as mc, MatchKind};
use ligus_runtime::approval_mailbox::{
    self as am, PresenceAsk, PresenceOutcome, PresenceRequester, Signer, Verified,
};
use ligus_runtime::secret_store::StoreKeyVault;
use ligus_runtime::IdentityStore;
use sha2::Digest as _;
use tower::ServiceExt as _;

use super::approvals::{b64, phone_envelope, unb64, world, World};
use super::*;
use crate::serve::principal::{Principal, Standing};

/// The approval routes as a request acting for `who` would reach them.
async fn call(
    w: &World,
    who: &Principal,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let app = crate::serve::approvals_api::routes(axum::Router::new())
        .with_state(w.h.handle.shared.clone());
    let mut req = Request::builder().method(method).uri(path);
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let resp = crate::serve::api::acting_as(who.clone(), app.oneshot(req.body(body).unwrap()))
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Ana's phone, as the box places it (`principal_for_phone`).
fn ana(w: &World) -> Principal {
    let person =
        w.h.handle
            .shared
            .lock()
            .phone_person(w.ws, w.phone.device_id)
            .unwrap();
    assert_eq!(person.account_id, Some(w.account));
    Principal {
        person: Some(person),
        device: Some(w.phone.device_id),
        via_host: None,
        standing: Standing::Person,
    }
}

/// Another person's phone on the same box.
fn ben(w: &World) -> Principal {
    let mut p = ana(w);
    let person = p.person.as_mut().unwrap();
    person.id = Uuid::new_v4().to_string();
    person.account_id = Some(Uuid::new_v4());
    p.device = Some(Uuid::new_v4());
    p
}

/// A drafted workspace runtime of Ana's, posted as a hand-off by her
/// desktop's device key (what `ligus setup` on her terminal does).
async fn post_handoff(w: &World, name: &str) -> PresenceRequester {
    let mut desk = open_service(&w.h.desktop).unwrap();
    desk.capture_presence(true);
    let rt = WorkspaceRuntime::new(
        format!("rt-{name}"),
        name,
        ModelProviderKind::ClaudeCode,
        "sonnet",
    );
    assert!(
        desk.add_workspace_runtime(w.ws, rt).is_err(),
        "captured, not written"
    );
    let draft = desk.take_presence_draft().expect("a draft").env;
    desk.capture_presence(false);
    let ids = IdentityStore::new(
        &w.h.desktop.data_dir,
        StoreKeyVault::for_data_dir(&w.h.desktop.data_dir),
    );
    let key = ids.device_keypair(desk.device_id()).unwrap();
    let ask = PresenceAsk::new("laptop", &key.public_key_bytes(), &draft).unwrap();
    let summary = ligus_runtime::chat_service::summarize(&draft.payload);
    PresenceRequester::post(
        w.relay.clone(),
        &w.address,
        Signer::Ed25519(key),
        ask,
        &summary,
        None,
    )
    .await
    .unwrap()
}

/// The listed rows `who` gets (a fresh listing each time).
async fn rows(w: &World, who: &Principal) -> Vec<Value> {
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let (status, v) = call(w, who, "GET", "/v1/approvals", None).await;
    assert_eq!(status, 200, "{v}");
    v["approvals"].as_array().unwrap().clone()
}

/// Ana's phone's row for `rid`, once the box lists it.
async fn row_for(w: &World, rid: &str) -> Value {
    for _ in 0..50 {
        let (_, v) = call(w, &ana(w), "GET", "/v1/approvals", None).await;
        if let Some(r) = v["approvals"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["rid"] == json!(rid))
        {
            return r.clone();
        }
        w.h.handle.shared.approvals.kick.notify_one();
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("hand-off {rid} never listed for Ana's phone");
}

fn listed(rows: &[Value], rid: &str) -> bool {
    rows.iter().any(|r| r["rid"] == json!(rid))
}

/// Ana's own devices, as her terminal's log has them (who may answer).
fn anas_devices(w: &World) -> std::collections::HashSet<Uuid> {
    open_service(&w.h.desktop)
        .unwrap()
        .my_device_ids(w.ws)
        .unwrap()
        .into_iter()
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_handoff_is_its_persons_phone_alone_and_the_phone_confirms_it() {
    let w = world().await;
    let mut req = post_handoff(&w, "coder").await;
    let rid = req.rid_hex();

    // Ana's phone: the row carries what it shows and signs, rendered on the
    // box from the draft.
    let row = row_for(&w, &rid).await;
    assert_eq!(row["kind"], json!("presence"));
    let p = &row["presence"];
    assert_eq!(p["summary"], json!("Define workspace runtime \u{201C}coder\u{201D} (ClaudeCode, model \u{201C}sonnet\u{201D})"));
    assert_eq!(p["eventKind"], json!("workspaceRuntimeUpserted"));
    assert_eq!(p["accountId"], json!(w.account));
    assert_eq!(
        p["code"],
        json!(req.code().shown),
        "the code the terminal shows"
    );
    assert_eq!(
        unb64(p["challenge"].as_str().unwrap()),
        ligus_core::presence::challenge(req.draft()).to_vec()
    );
    assert!(p["rpId"].as_str().is_some_and(|r| !r.is_empty()), "{p}");
    assert!(!p["deviceName"].as_str().unwrap().is_empty());

    // Nobody else: another person's phone, a phone the box can't place, the
    // operator (the box itself).
    assert!(
        !listed(&rows(&w, &ben(&w)).await, &rid),
        "another person's phone never sees it"
    );
    assert!(
        !listed(&rows(&w, &Principal::unresolved()).await, &rid),
        "an unplaced phone never sees it"
    );
    assert!(
        !listed(
            &w.h.get("/v1/approvals").await["approvals"]
                .as_array()
                .unwrap()
                .clone(),
            &rid
        ),
        "the operator never sees it"
    );
    // ... nor reads or answers it through the box.
    let msgs = format!("/v1/approvals/{rid}/msgs?after=0");
    assert_eq!(call(&w, &ben(&w), "GET", &msgs, None).await.0, 404);
    assert_eq!(
        call(&w, &Principal::unresolved(), "GET", &msgs, None)
            .await
            .0,
        404
    );
    assert_eq!(call(&w, &ana(&w), "GET", &msgs, None).await.0, 200);

    // The phone checks the request itself: its signature, the code from the
    // request alone, and the commit it will sign.
    let v = Verified::parse(row["request"].as_str().unwrap().as_bytes())
        .expect("the request verifies on the phone");
    let body: am::RequestBody = serde_json::from_str(&v.body).unwrap();
    assert_eq!(body.kind, MatchKind::Presence);
    let ask = PresenceAsk::decode(&body.code).unwrap();
    assert_eq!(
        ask.device_key, v.key,
        "signed by the device the draft names"
    );
    let mailbox: [u8; 16] = mc::unhex(&body.mailbox).unwrap().try_into().unwrap();
    let rid_b: [u8; 16] = mc::unhex(&rid).unwrap().try_into().unwrap();
    assert_eq!(
        mc::encode(mc::presence_code40(&mailbox, &rid_b, body.code.as_bytes())),
        req.code().shown
    );
    let commit: [u8; 32] = unb64(&body.commit).try_into().unwrap();
    assert_eq!(b64(&commit), p["challenge"].as_str().unwrap());
    let requester_ka: [u8; 32] = unb64(&body.requester_ka).try_into().unwrap();
    let seal_key: [u8; 32] = unb64(row["sealKey"].as_str().unwrap()).try_into().unwrap();

    // A claim to the members, then the result with the passkey's assertion
    // (the test recovery key stands in for the phone's passkey) over the
    // commit, signed by the phone's own key and sealed to the terminal.
    let claim = json!({ "rid": rid, "approverDeviceId": w.phone.device_id, "approverName": "Ana's iPhone", "at": ligus_core::flow::now_ms() });
    let claim = am::seal_blob(
        &seal_key,
        phone_envelope("claim", &claim, &w.phone).as_bytes(),
    )
    .unwrap();
    let path = format!("/v1/approvals/{rid}/claim");
    assert_eq!(
        call(
            &w,
            &ben(&w),
            "POST",
            &path,
            Some(json!({ "blob": claim, "to": "members" }))
        )
        .await
        .0,
        404
    );
    assert_eq!(
        call(
            &w,
            &ana(&w),
            "POST",
            &path,
            Some(json!({ "blob": claim, "to": "members" }))
        )
        .await
        .0,
        201
    );
    let assertion = ligus_core::testkit::test_recovery_key(w.account).assert(&commit);
    let grant = format!(
        "{}{}",
        am::ASSERTION_PREFIX,
        b64(&serde_json::to_vec(&assertion).unwrap())
    );
    let result = json!({ "rid": rid, "outcome": "approved", "grant": grant, "grantDigest": mc::hex(&sha2::Sha256::digest(grant.as_bytes())) });
    let result = am::seal_blob(
        &requester_ka,
        phone_envelope("result", &result, &w.phone).as_bytes(),
    )
    .unwrap();
    let path = format!("/v1/approvals/{rid}/result");
    assert_eq!(
        call(
            &w,
            &Principal::unresolved(),
            "POST",
            &path,
            Some(json!({ "blob": result, "to": "requester" }))
        )
        .await
        .0,
        404
    );
    assert_eq!(
        call(
            &w,
            &ana(&w),
            "POST",
            &path,
            Some(json!({ "blob": result, "to": "requester" }))
        )
        .await
        .0,
        201
    );

    // The terminal takes it (the phone is one of Ana's devices on its log)
    // and stores exactly the draft; its fold accepts the presence.
    let mine = anas_devices(&w);
    assert!(
        mine.contains(&w.phone.device_id),
        "a certified phone answers for its person"
    );
    let mut outcome = None;
    for _ in 0..20 {
        if let Some(o) = req.poll(1, &|d| mine.contains(&d)).await.unwrap() {
            outcome = Some(o);
            break;
        }
    }
    let Some(PresenceOutcome::Approved {
        assertion: got,
        by_device,
    }) = outcome
    else {
        panic!("{outcome:?}")
    };
    assert_eq!(by_device, w.phone.device_id);
    let mut desk = open_service(&w.h.desktop).unwrap();
    let draft = desk.presence_draft_of(req.draft().clone()).unwrap();
    desk.commit_presence(draft, got).unwrap();
    assert!(desk
        .list_workspace_runtimes(w.ws)
        .unwrap()
        .iter()
        .any(|r| r.name == "coder"));
    req.settle().await.unwrap();
    w.h.handle.shutdown();
}

/// A phone's Not me: a declined result to the terminal and an abort to the
/// members; nothing is written. A stranger's answer is ignored.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn not_me_from_the_phone_ends_it_and_a_strangers_answer_counts_for_nothing() {
    let w = world().await;
    let mut req = post_handoff(&w, "nope").await;
    let rid = req.rid_hex();
    let row = row_for(&w, &rid).await;
    let v = Verified::parse(row["request"].as_str().unwrap().as_bytes()).unwrap();
    let body: am::RequestBody = serde_json::from_str(&v.body).unwrap();
    let requester_ka: [u8; 32] = unb64(&body.requester_ka).try_into().unwrap();
    let commit: [u8; 32] = unb64(&body.commit).try_into().unwrap();
    let mine = anas_devices(&w);

    // A phone that isn't Ana's, answering through Ana's own channel: the
    // terminal ignores it (its key isn't one of Ana's devices).
    let stranger = ligus_core::testkit::Phone::new();
    let assertion = ligus_core::testkit::test_recovery_key(w.account).assert(&commit);
    let grant = format!(
        "{}{}",
        am::ASSERTION_PREFIX,
        b64(&serde_json::to_vec(&assertion).unwrap())
    );
    let forged = json!({ "rid": rid, "outcome": "approved", "grant": grant, "grantDigest": mc::hex(&sha2::Sha256::digest(grant.as_bytes())) });
    let forged = am::seal_blob(
        &requester_ka,
        phone_envelope("result", &forged, &stranger).as_bytes(),
    )
    .unwrap();
    let path = format!("/v1/approvals/{rid}/result");
    assert_eq!(
        call(
            &w,
            &ana(&w),
            "POST",
            &path,
            Some(json!({ "blob": forged, "to": "requester" }))
        )
        .await
        .0,
        201
    );
    assert_eq!(
        req.poll(1, &|d| mine.contains(&d)).await.unwrap(),
        None,
        "a stranger's approval counts for nothing"
    );

    // Ana's phone says Not me.
    let declined = json!({ "rid": rid, "outcome": "declined", "grant": null, "grantDigest": null });
    let declined = am::seal_blob(
        &requester_ka,
        phone_envelope("result", &declined, &w.phone).as_bytes(),
    )
    .unwrap();
    assert_eq!(
        call(
            &w,
            &ana(&w),
            "POST",
            &path,
            Some(json!({ "blob": declined, "to": "requester" }))
        )
        .await
        .0,
        201
    );
    let seal_key: [u8; 32] = unb64(row["sealKey"].as_str().unwrap()).try_into().unwrap();
    let abort = am::seal_blob(
        &seal_key,
        phone_envelope(
            "abort",
            &json!({ "rid": rid, "reason": "cancelled" }),
            &w.phone,
        )
        .as_bytes(),
    )
    .unwrap();
    assert_eq!(
        call(
            &w,
            &ana(&w),
            "POST",
            &format!("/v1/approvals/{rid}/abort"),
            Some(json!({ "blob": abort, "to": "members" }))
        )
        .await
        .0,
        201
    );
    let mut outcome = None;
    for _ in 0..20 {
        if let Some(o) = req.poll(1, &|d| mine.contains(&d)).await.unwrap() {
            outcome = Some(o);
            break;
        }
    }
    assert_eq!(
        outcome,
        Some(PresenceOutcome::Declined {
            by_device: w.phone.device_id
        })
    );
    assert!(!open_service(&w.h.desktop)
        .unwrap()
        .list_workspace_runtimes(w.ws)
        .unwrap()
        .iter()
        .any(|r| r.name == "nope"));
    // Ended: the box lists it no more, for anyone.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(!listed(&rows(&w, &ana(&w)).await, &rid));
    w.h.handle.shutdown();
}

/// A device that isn't Ana's on the box's fold: a hand-off it asks never
/// lists for Ana's phone (the box checks the asking device on its own log).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_handoff_from_a_device_that_isnt_hers_never_lists() {
    let w = world().await;
    let mut desk = open_service(&w.h.desktop).unwrap();
    desk.capture_presence(true);
    let rt = WorkspaceRuntime::new("rt-x", "x", ModelProviderKind::ClaudeCode, "sonnet");
    assert!(desk.add_workspace_runtime(w.ws, rt).is_err());
    let mut draft = desk.take_presence_draft().unwrap().env;
    drop(desk);
    // Re-signed by a key nobody certified, as that device.
    let rogue = ligus_core::crypto::SigningKeypair::generate().unwrap();
    let rogue_id = ligus_core::crypto::device_id_for(
        ligus_core::crypto::SigAlg::Ed25519,
        &rogue.public_key_bytes(),
    );
    ligus_core::crypto::sign_envelope(&mut draft, rogue_id, &rogue);
    let ask = PresenceAsk::new("laptop", &rogue.public_key_bytes(), &draft).unwrap();
    let req = PresenceRequester::post(
        w.relay.clone(),
        &w.address,
        Signer::Ed25519(rogue),
        ask,
        "x",
        None,
    )
    .await
    .unwrap();
    let rid = req.rid_hex();
    for _ in 0..5 {
        assert!(
            !listed(&rows(&w, &ana(&w)).await, &rid),
            "a device that isn't Ana's never lists"
        );
    }
    assert_eq!(
        call(
            &w,
            &ana(&w),
            "GET",
            &format!("/v1/approvals/{rid}/msgs?after=0"),
            None
        )
        .await
        .0,
        404
    );
    w.h.handle.shutdown();
}

#[test]
fn a_handoff_pushes_once_outside_the_box_cap() {
    let mut t = crate::serve::approvals_api::PushTracker::default();
    let t0 = 1_000_000;
    // Three box requests fill the box cap; a hand-off still pushes, once.
    for (i, rid) in ["a", "b", "c"].iter().enumerate() {
        assert!(t.decide(rid, MatchKind::Box, false, t0 + i as i64));
    }
    assert!(!t.decide("d", MatchKind::Box, false, t0 + 4));
    assert!(t.decide("p", MatchKind::Presence, false, t0 + 5));
    assert!(
        !t.decide("p", MatchKind::Presence, false, t0 + 6),
        "once per request"
    );
    assert!(
        !t.decide("q", MatchKind::Presence, true, t0 + 7),
        "someone is already checking it"
    );
    let c = crate::serve::approvals_api::presence_push_content(Uuid::nil(), "p", "laptop");
    assert_eq!(c.kind, "approval.presence");
    assert_eq!(
        c.title,
        "Your passkey, asked by your device: \u{201C}laptop\u{201D}"
    );
    assert!(c
        .fields
        .iter()
        .any(|(k, v)| *k == "requestKind" && v == "presence"));
}

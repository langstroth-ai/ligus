//! End to end: a real `ligus serve` with remote access on, a minimal fake
//! relay, and a fake phone (snow initiator) pairing, then GET + SSE through
//! the tunnel, renewal, and revocation (included from `serve/tests.rs` for
//! its harness).
//!
//! Set `LIGUS_TUNNEL_INTEROP_RELAY=http://127.0.0.1:<port>` to run
//! `interop_against_a_real_relay` against a real relay binary started with
//! `LIGUS_RELAY_TUNNEL=open`.

use std::time::Duration;

use serde_json::json;

use super::*;
use crate::serve::tunnel::frame::{self, Frame, ResponseHead};
use crate::serve::tunnel::testkit::{self, FakeRelay, HostInfo, PhoneKeys};
use crate::serve::tunnel::{self as tun, cred, PairingSecret, RelaySettings};

/// The overall deadline for anything a test awaits. Every wait returns as
/// soon as its condition holds, so this only bounds a genuinely stuck test;
/// latency bounds the spec makes are asserted separately (in watch passes,
/// see [`watch_caught_up`]), never by how fast a loaded machine is.
const WAIT: Duration = Duration::from_secs(30);

async fn serve_with_relay(relay: &str) -> Harness {
    let (endpoint, _) = spawn_model(vec![]).await;
    let relay = relay.to_string();
    let h = harness_with(&endpoint, "m", &[("README.md", "hi\n")], move |o| {
        o.remote = Some(RelaySettings::new(&relay, None, true).unwrap());
    })
    .await;
    trust_test_phones(&h);
    h
}

/// The box's real roster decides what is revoked (or known); every test
/// phone's device otherwise counts as a certified phone (certifying one for
/// real needs its person's passkey, which the serve harness doesn't have).
fn trust_test_phones(h: &Harness) {
    let t = h.handle.shared.tunnel.clone().unwrap();
    let weak = Arc::downgrade(&h.handle.shared);
    let real: tun::DeviceLookupFn = Arc::new(move |d| match weak.upgrade() {
        Some(s) => s.device_lookup(d),
        None => tun::device_bind::DeviceLookup::Unknown,
    });
    t.set_device_lookup(testkit::test_device_lookup(Some(real)));
}

fn host_info(h: &Harness, relay: &str) -> HostInfo {
    let t = h.handle.shared.tunnel.as_ref().expect("tunnel on");
    HostInfo { relay: relay.to_string(), host_id: t.host_id().to_string(), hs: *t.keys.static_pub() }
}

/// The phone's side of a v2 link: (relay, hid, hs, ps).
fn parse_link(link: &str) -> (String, String, [u8; 32], PairingSecret) {
    let param = |k: &str| link.split(['?', '&']).find_map(|kv| kv.strip_prefix(&format!("{k}="))).unwrap().to_string();
    assert_eq!(param("v"), "2");
    let relay = String::from_utf8(tun::b64u_decode(&param("relay")).unwrap()).unwrap();
    (relay, param("hid"), tun::b64u_fixed(&param("hs")).unwrap(), PairingSecret::from_bytes(tun::b64u_fixed(&param("ps")).unwrap()))
}

/// A v2 link the way `ligus pair` makes one: window file + link.
fn pair_link(h: &Harness) -> String {
    let t = h.handle.shared.tunnel.as_ref().unwrap();
    let (secret, exp) = tun::create_window(&t.dir, tun::PAIR_WINDOW_SECS).unwrap();
    tun::pairing_link_v2(Some(("http://100.101.102.104:8787", &h.token)), &t.keys, &t.settings.relay, &secret.ps, exp)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tunnel_end_to_end_pair_get_sse_renew_revoke() {
    let relay = FakeRelay::start().await;
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(WAIT).await.unwrap();
    let t = h.handle.shared.tunnel.clone().unwrap();

    // Pair: `ligus pair` leaves a window in pending/, serve announces it.
    let link = pair_link(&h);
    let (relay_url, hid, hs, ps) = parse_link(&link);
    assert_eq!((relay_url.as_str(), hid.as_str(), hs), (relay.url.as_str(), t.host_id(), *t.keys.static_pub()));
    let announced = relay.wait_for("enroll", |m| m["eid"] == ps.eid, WAIT).await.unwrap();
    assert_eq!(announced["tagHash"], json!(ps.tag_hash()));
    let info = host_info(&h, &relay.url);
    let phone = PhoneKeys::generate();
    let (mut c, pid, credential) = testkit::phone_enroll(&info, &phone, &ps, "Test iPhone").await.unwrap();
    assert!(t.registry.is_active(&pid));
    relay.wait_for("enrollClose", |m| m["eid"] == ps.eid, WAIT).await.unwrap();
    assert!(!tun::pending_dir(&t.dir).join(format!("enroll-{}.json", ps.eid)).exists());
    // The enrollment connection already carries the API.
    let health = c.request("GET", "/v1/health", None).await.unwrap();
    assert_eq!(health.status, 200);
    assert_eq!(health.json()["workspace"], json!(h.handle.shared.cfg.room), "tunnel peer counts as authorized");
    drop(c);

    // The same ps can't enroll twice (T-qr-reuse).
    assert!(testkit::phone_enroll(&info, &PhoneKeys::generate(), &ps, "again").await.is_err());

    // Session: GET.
    let mut c = testkit::phone_session(&info, &phone, &credential).await.unwrap();
    let chats = c.request("GET", "/v1/chats", None).await.unwrap();
    assert_eq!(chats.status, 200, "{}", String::from_utf8_lossy(&chats.body));
    assert!(chats.json()["chats"].is_array());

    // SSE: the hub's events arrive as DATA on an open stream.
    let sse = c.open("GET", "/v1/events", &[("accept", "text/event-stream")], None, true).await.unwrap();
    let head = c.recv_timeout(WAIT).await.unwrap();
    assert_eq!((head.ty, head.stream), (frame::RESPONSE, sse));
    let head: ResponseHead = serde_json::from_slice(&head.payload).unwrap();
    assert_eq!(head.status, 200);
    assert!(head.headers.iter().any(|(k, v)| k == "content-type" && v.starts_with("text/event-stream")));
    h.handle.shared.hub.emit("turn.status", json!({ "canary": "tunnel-sse" }));
    let mut text = String::new();
    while !text.contains("tunnel-sse") {
        let f = c.recv_timeout(WAIT).await.unwrap();
        if f.ty == frame::DATA && f.stream == sse {
            text.push_str(&String::from_utf8_lossy(&f.payload));
        }
    }
    assert!(text.contains("event: turn.status"), "{text}");
    c.send(Frame::reset(sse, frame::reset::CANCEL, "")).await.unwrap();

    // Renewal (tunnel-only): a fresh credential for the same pid and key.
    let renewed = c.request("POST", "/v1/tunnel/credential", None).await.unwrap();
    assert_eq!(renewed.status, 200);
    let new_cred = renewed.json()["credential"].as_str().unwrap().to_string();
    let claims = cred::verify(&new_cred, t.host_id(), Some((t.host_id(), &t.keys.sign_pub())), tun::now()).unwrap();
    assert_eq!((claims.pid.as_str(), claims.pk), (pid.as_str(), phone.auth.verifying_key().to_bytes()));
    // Push preference: previews default on; "previews": false turns them off;
    // a body without the field keeps the stored value.
    let r = c.request("PUT", "/v1/tunnel/push", Some(json!({ "enabled": true }))).await.unwrap();
    assert_eq!((r.status, r.json()), (200, json!({ "enabled": true, "previews": true })));
    assert!(t.registry.get(&pid).unwrap().push_enabled && t.registry.get(&pid).unwrap().push_previews);
    let r = c.request("PUT", "/v1/tunnel/push", Some(json!({ "enabled": true, "previews": false }))).await.unwrap();
    assert_eq!(r.json(), json!({ "enabled": true, "previews": false }));
    let r = c.request("PUT", "/v1/tunnel/push", Some(json!({ "enabled": true }))).await.unwrap();
    assert_eq!(r.json(), json!({ "enabled": true, "previews": false }), "absent field keeps the stored value");
    // A push to this phone is sender-only now; with previews back on it
    // carries the first 120 characters, end-to-end encrypted.
    t.push_available.store(true, std::sync::atomic::Ordering::SeqCst);
    let key: [u8; 32] = tun::b64u_fixed(&t.registry.get(&pid).unwrap().push_key).unwrap();
    let content = tun::PushContent {
        kind: "message.done".into(),
        chat_id: Some("c1".into()),
        title: "coder replied".into(),
        sender: Some("coder".into()),
        preview: Some(format!("secret plan {}", "z".repeat(300))),
        ..Default::default()
    };
    // The SSE stream above was cancelled: the app is "in the background".
    wait_background(&t, &pid).await;
    for (previews, want_preview) in [(false, false), (true, true)] {
        c.request("PUT", "/v1/tunnel/push", Some(json!({ "enabled": true, "previews": previews }))).await.unwrap();
        let seq = t.registry.get(&pid).unwrap().push_seq;
        // A different chat each time: the same (kind, chat) within 10 s is
        // coalesced by the host-side limiter.
        let content = tun::PushContent { chat_id: Some(format!("c-{previews}")), ..content.clone() };
        assert_eq!(t.push_all(&content), 1);
        let m = relay
            .wait_for(
                "push",
                |m| {
                    let ct = tun::b64u_decode(m["ct"].as_str().unwrap_or_default()).unwrap_or_default();
                    m["pid"] == json!(pid) && ct.len() > 28 && testkit::open_push(&key, t.host_id(), &pid, &ct)["seq"] == json!(seq + 1)
                },
                WAIT,
            )
            .await
            .unwrap();
        let ct = tun::b64u_decode(m["ct"].as_str().unwrap()).unwrap();
        assert!(ct.len() <= tun::PUSH_CT_MAX);
        let pt = testkit::open_push(&key, t.host_id(), &pid, &ct);
        assert_eq!(pt["sender"], "coder");
        if want_preview {
            assert!(pt["body"].as_str().unwrap().starts_with("secret plan zzz") && pt["body"].as_str().unwrap().ends_with('…'));
            assert_eq!(pt["body"].as_str().unwrap().chars().count(), tun::PREVIEW_CHARS + 1);
        } else {
            assert_eq!(pt["body"], "");
            assert!(!pt.to_string().contains("secret"));
        }
    }
    t.push_available.store(false, std::sync::atomic::Ordering::SeqCst);
    // …and 404 on the direct path, even with the right bearer token.
    let direct = h.client.post(format!("{}/v1/tunnel/credential", h.base)).bearer_auth(&h.token).send().await.unwrap();
    assert_eq!(direct.status(), 404);

    // Revocation from another process (`ligus phones revoke`): the live SSE
    // session gets GOAWAY revoked within ~1 s of serve noticing (≤ 2 s poll).
    let sse = c.open("GET", "/v1/events", &[], None, true).await.unwrap();
    let _ = c.recv_timeout(WAIT).await.unwrap();
    let data_dir = h.handle.shared.cfg.data_dir.clone();
    tun::cli::phones_revoke(&data_dir, Some(&pid), &h.handle.shared.opts.token_file, false).unwrap();
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        // GOAWAY from the box, or the relay's 4003 close if its revocation
        // (control or HTTP) lands first: both end the session as revoked.
        let f = match tokio::time::timeout_at(deadline, c.recv()).await.expect("GOAWAY") {
            Ok(f) => f,
            Err(e) => {
                assert_eq!(testkit::close_code(&e), Some(4003), "{e:#}");
                break;
            }
        };
        if f.ty == frame::GOAWAY {
            assert_eq!(f.code(), Some(frame::goaway::REVOKED));
            break;
        }
        assert_eq!(f.stream, sse);
    }
    let rv = relay.wait_for("revoke", |m| m["pid"] == json!(pid), WAIT).await.unwrap();
    assert!(rv["exp"].as_u64().unwrap() >= claims.exp, "revocation lasts until the latest credential's exp");
    // Acked → the pending file is gone; reconnect is refused at the relay.
    let pending = tun::pending_dir(&t.dir).join(format!("revoke-{pid}.json"));
    wait_until("the relay acks the revocation (pending file removed)", WAIT, || !pending.exists()).await;
    let err = testkit::phone_session(&info, &phone, &new_cred).await.err().expect("revoked");
    assert_eq!(testkit::close_code(&err), Some(4003));
    // The direct-path token was rotated, and serve picked it up.
    let old_token = h.token.clone();
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let r = h.client.get(format!("{}/v1/chats", h.base)).bearer_auth(&old_token).send().await.unwrap();
        if r.status() == 401 {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "old bearer token still accepted after revoke");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let new_token = h.handle.shared.token();
    assert_ne!(new_token, old_token);
    assert_eq!(h.client.get(format!("{}/v1/chats", h.base)).bearer_auth(&new_token).send().await.unwrap().status(), 200);
}

/// Until the phone's cancelled `/v1/events` streams are gone.
async fn wait_background(t: &tun::TunnelHost, pid: &str) {
    wait_until("the events stream stops counting as foreground", WAIT, || t.foreground_streams(pid) == 0).await;
}

/// The four notification kinds, end to end: a real serve projection change
/// (agent reply, mention, proposal, workflow gate) → push on the fake relay →
/// decrypts with the phone's push key; and none while the app is open.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pushes_reach_the_relay_encrypted_and_skip_a_foreground_phone() {
    use hive_core::{workflow, ActionProposal, NodeRunStatus, ProposalKind};
    let relay = FakeRelay::start().await;
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(WAIT).await.unwrap();
    let t = h.handle.shared.tunnel.clone().unwrap();
    let (_, _, _, ps) = parse_link(&pair_link(&h));
    relay.wait_for("enroll", |m| m["eid"] == ps.eid, WAIT).await.unwrap();
    let info = host_info(&h, &relay.url);
    let phone = PhoneKeys::generate();
    let (_c, pid, credential) = testkit::phone_enroll(&info, &phone, &ps, "Push phone").await.unwrap();
    drop(_c);
    let mut c = testkit::phone_session(&info, &phone, &credential).await.unwrap();
    assert_eq!(c.request("PUT", "/v1/tunnel/push", Some(json!({ "enabled": true }))).await.unwrap().status, 200);
    t.push_available.store(true, std::sync::atomic::Ordering::SeqCst);
    let key: [u8; 32] = tun::b64u_fixed(&t.registry.get(&pid).unwrap().push_key).unwrap();
    let shared = h.handle.shared.clone();
    let ws = shared.ws;
    let chat = h.send(reqwest::Method::POST, "/v1/chats", None).await;
    let chat_id = uuid::Uuid::parse_str(chat["chat"]["id"].as_str().unwrap()).unwrap();

    // Foreground: an open events stream → a finished reply pushes nothing.
    let sse = c.open("GET", "/v1/events", &[("accept", "text/event-stream")], None, true).await.unwrap();
    let _ = c.recv_timeout(WAIT).await.unwrap();
    assert_eq!(t.foreground_streams(&pid), 1);
    let seq0 = t.registry.get(&pid).unwrap().push_seq;
    let mid = shared.lock().begin_assistant_message(chat_id, ws, "coder".to_string(), "gw".to_string(), Some(h.agent_id), None).unwrap();
    shared.lock().complete_assistant_message(chat_id, ws, mid, "all done").unwrap();
    shared.refresh();
    assert_eq!(t.registry.get(&pid).unwrap().push_seq, seq0, "no push while the app is open");

    // Background: the app cancelled the stream.
    c.send(Frame::reset(sse, frame::reset::CANCEL, "")).await.unwrap();
    wait_background(&t, &pid).await;
    let next = |seq: u64| {
        let relay = &relay;
        let pid = pid.clone();
        let hid = t.host_id().to_string();
        async move {
            let m = relay
                .wait_for(
                    "push",
                    |m| {
                        let ct = tun::b64u_decode(m["ct"].as_str().unwrap_or_default()).unwrap_or_default();
                        m["pid"] == json!(pid) && ct.len() > 28 && testkit::open_push(&key, &hid, &pid, &ct)["seq"] == json!(seq)
                    },
                    WAIT,
                )
                .await
                .unwrap();
            assert_eq!(m["prio"], "high");
            assert_eq!(m["collapse"], json!(tun::push_collapse(&key, &chat_id.to_string())), "collapse is the HMAC, never the chat id");
            testkit::open_push(&key, &hid, &pid, &tun::b64u_decode(m["ct"].as_str().unwrap()).unwrap())
        }
    };

    // An agent reply that mentions the user → one `mention` push, not two.
    let me = shared.lock().author().display_name.clone();
    let mid = shared.lock().begin_assistant_message(chat_id, ws, "coder".to_string(), "gw".to_string(), Some(h.agent_id), None).unwrap();
    shared.lock().complete_assistant_message(chat_id, ws, mid, format!("@{me} the build is green")).unwrap();
    shared.refresh();
    let pt = next(seq0 + 1).await;
    assert_eq!((pt["kind"].as_str(), pt["title"].as_str(), pt["sender"].as_str()), (Some("mention"), Some("coder mentioned you"), Some("coder")));
    assert_eq!(pt["chatId"], json!(chat_id.to_string()));
    assert_eq!(pt["body"], json!(format!("@{me} the build is green")));
    assert_eq!(t.registry.get(&pid).unwrap().push_seq, seq0 + 1, "exactly one push for the mentioning reply");

    // A proposal waiting for review.
    let p = ActionProposal::new("coder proposed changes to 1 file", ProposalKind::FileDiff, "agent-x");
    let proposal_id = p.id;
    shared.lock().upsert_proposal(chat_id, ws, p).unwrap();
    shared.refresh();
    let pt = next(seq0 + 2).await;
    assert_eq!((pt["kind"].as_str(), pt["title"].as_str()), (Some("proposal.created"), Some("Change ready for review")));
    assert_eq!((pt["chatId"].clone(), pt["proposalId"].clone()), (json!(chat_id.to_string()), json!(proposal_id.to_string())));

    // A workflow gate waiting for approval (the desktop's run engine writes
    // the gate's proposal, then the run): one `workflow.approval` push.
    let def = workflow::preset_review_gate();
    let mut run = workflow::new_run(&def, "build it", "u1");
    let gate = ActionProposal::new("Approve?", ProposalKind::Decision, "");
    let st = run.node_state_mut("approval").unwrap();
    st.status = NodeRunStatus::AwaitingApproval;
    st.proposal_id = Some(gate.id);
    let gate_id = gate.id;
    shared.lock().upsert_proposal(chat_id, ws, gate).unwrap();
    shared.refresh();
    shared.lock().upsert_workflow_run(chat_id, ws, run.clone()).unwrap();
    shared.refresh();
    let pt = next(seq0 + 3).await;
    assert_eq!(pt["kind"], "workflow.approval");
    assert_eq!(pt["title"], json!(format!("{} needs approval", def.name)));
    assert_eq!(pt["workflowRunId"], json!(run.id.to_string()));
    assert_eq!(pt["workflowId"], json!(def.id.to_string()));
    assert_eq!(pt["proposalId"], json!(gate_id.to_string()));
    assert_eq!(pt["body"], "Approval");
    assert_eq!(t.registry.get(&pid).unwrap().push_seq, seq0 + 3, "the gate's proposal didn't push on its own");
    t.push_available.store(false, std::sync::atomic::Ordering::SeqCst);
    h.handle.shutdown();
}

/// A paired phone with push on, through the fake relay: (relay, harness,
/// host, phone session, pid, push key).
async fn push_phone() -> (FakeRelay, Harness, Arc<tun::TunnelHost>, testkit::PhoneConn<testkit::ClientWs>, String, [u8; 32]) {
    let relay = FakeRelay::start().await;
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(WAIT).await.unwrap();
    let t = h.handle.shared.tunnel.clone().unwrap();
    let (_, _, _, ps) = parse_link(&pair_link(&h));
    relay.wait_for("enroll", |m| m["eid"] == ps.eid, WAIT).await.unwrap();
    let info = host_info(&h, &relay.url);
    let phone = PhoneKeys::generate();
    let (_c, pid, credential) = testkit::phone_enroll(&info, &phone, &ps, "Push phone").await.unwrap();
    drop(_c);
    let mut c = testkit::phone_session(&info, &phone, &credential).await.unwrap();
    assert_eq!(c.request("PUT", "/v1/tunnel/push", Some(json!({ "enabled": true }))).await.unwrap().status, 200);
    t.push_available.store(true, std::sync::atomic::Ordering::SeqCst);
    let key: [u8; 32] = tun::b64u_fixed(&t.registry.get(&pid).unwrap().push_key).unwrap();
    (relay, h, t, c, pid, key)
}

/// iOS suspends the app (screen locked) before the RESET for its events
/// stream leaves, and the relay keeps the dead phone connection up for up to
/// a minute: the stream stays "open" on the box. Once the phone has been
/// quiet for FOREGROUND_QUIET it is in the background, and pushes go again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_suspended_phones_stale_events_stream_stops_suppressing_pushes() {
    let (relay, h, t, mut c, pid, key) = push_phone().await;
    let chat = h.send(reqwest::Method::POST, "/v1/chats", None).await;
    let chat_id = chat["chat"]["id"].as_str().unwrap().to_string();
    let _sse = c.open("GET", "/v1/events", &[("accept", "text/event-stream")], None, true).await.unwrap();
    let _ = c.recv_timeout(WAIT).await.unwrap();
    assert_eq!(t.foreground_streams(&pid), 1);
    let content = tun::PushContent { kind: "message.done".into(), chat_id: Some(chat_id.clone()), title: "coder replied".into(), ..Default::default() };
    // While the phone is talking, the app is open: no push.
    assert_eq!(t.push_all_at(&content, std::time::Instant::now()), 0);
    // The phone goes silent (suspended, no RESET): the stream is still open…
    assert_eq!(t.foreground_streams(&pid), 1);
    // …but past the quiet window the push goes out.
    let later = std::time::Instant::now() + tun::push::FOREGROUND_QUIET + Duration::from_secs(1);
    assert_eq!(t.push_all_at(&content, later), 1, "a quiet phone's stale stream must not suppress pushes");
    let m = relay.wait_for("push", |m| m["pid"] == json!(pid), WAIT).await.unwrap();
    let pt = testkit::open_push(&key, t.host_id(), &pid, &tun::b64u_decode(m["ct"].as_str().unwrap()).unwrap());
    assert_eq!(pt["kind"], "message.done");
    assert!(m.get("diag").is_none(), "a real push never asks for diagnostics");
    t.push_available.store(false, std::sync::atomic::Ordering::SeqCst);
    h.handle.shutdown();
}

/// `ligus remote push-test`: the CLI hands the request to the running serve
/// through `pending/`, serve sends a `test` push (even with the app open)
/// asking the relay for details, and the CLI reports each hop; a refusal
/// fails the command with the reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_test_reports_each_hop_through_the_running_serve() {
    let (relay, h, t, mut c, pid, key) = push_phone().await;
    let _sse = c.open("GET", "/v1/events", &[("accept", "text/event-stream")], None, true).await.unwrap();
    let _ = c.recv_timeout(WAIT).await.unwrap();
    *relay.state.push_reply.lock().unwrap() =
        Some(json!({ "ok": true, "detail": "apns sandbox topic com.zazulam.ligus.lab: 200 (apns-id A1)" }));

    // In process: the report.
    let r = t.push_test(None, WAIT).await;
    assert!(r.connected && r.push_available && r.error.is_none(), "{r:?}");
    let p = &r.phones[0];
    assert!(p.push_enabled && p.app_open && p.sent, "{p:?}");
    let ack = p.relay.as_ref().expect("relay answered");
    assert!(ack.ok && ack.detail.as_deref() == Some("apns sandbox topic com.zazulam.ligus.lab: 200 (apns-id A1)"), "{ack:?}");
    let m = relay.wait_for("push", |m| m["pid"] == json!(pid) && m["diag"] == json!(true), WAIT).await.unwrap();
    let pt = testkit::open_push(&key, t.host_id(), &pid, &tun::b64u_decode(m["ct"].as_str().unwrap()).unwrap());
    assert_eq!((pt["kind"].as_str(), pt["title"].as_str()), (Some("test"), Some("Test notification")));

    // Through the CLI and pending/, as `ligus remote push-test` runs it.
    let data_dir = h.handle.shared.cfg.data_dir.clone();
    let dd = data_dir.clone();
    tokio::task::spawn_blocking(move || tun::cli::remote_push_test(&dd, None)).await.unwrap().expect("push-test succeeds");
    // A refusal: the command fails, and names the hop and the fix.
    *relay.state.push_reply.lock().unwrap() =
        Some(json!({ "ok": false, "code": "push_failed", "detail": "apns production topic com.zazulam.ligus: 400 BadDeviceToken" }));
    let r = t.push_test(Some("Push phone"), WAIT).await;
    let ack = r.phones[0].relay.clone().unwrap();
    assert_eq!((ack.ok, ack.code.as_deref()), (false, Some("push_failed")));
    let (text, ok) = tun::cli::describe_push_test(&r);
    assert!(!ok && text.contains("BadDeviceToken") && text.contains("sandbox (development) token"), "{text}");
    let dd = data_dir.clone();
    let err = tokio::task::spawn_blocking(move || tun::cli::remote_push_test(&dd, Some("Push phone"))).await.unwrap().unwrap_err();
    assert!(err.to_string().contains("didn't get through"), "{err}");
    // Push-tests never trip the host's gates or real pushes' limits.
    assert_eq!(t.foreground_streams(&pid), 1);
    // A phone that never turned notifications on isn't sent anything.
    c.request("PUT", "/v1/tunnel/push", Some(json!({ "enabled": false }))).await.unwrap();
    let before = relay.messages("push").len();
    let r = t.push_test(None, Duration::from_secs(2)).await;
    assert!(!r.phones[0].push_enabled && !r.phones[0].sent);
    assert_eq!(relay.messages("push").len(), before);
    assert!(tun::cli::describe_push_test(&r).0.contains("notifications OFF"));
    t.push_available.store(false, std::sync::atomic::Ordering::SeqCst);
    h.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tunnel_request_after_revocation_on_open_session_gets_401() {
    // The per-request re-check (§9), independent of the GOAWAY: a request
    // racing the revocation is refused by the auth middleware.
    let relay = FakeRelay::start().await;
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(WAIT).await.unwrap();
    let t = h.handle.shared.tunnel.clone().unwrap();
    let (_, _, _, ps) = parse_link(&pair_link(&h));
    relay.wait_for("enroll", |m| m["eid"] == ps.eid, WAIT).await.unwrap();
    let info = host_info(&h, &relay.url);
    let phone = PhoneKeys::generate();
    let (_c, pid, _) = testkit::phone_enroll(&info, &phone, &ps, "p").await.unwrap();
    // Mark revoked in the registry only (no broadcast yet), then call the
    // router the way the connector does.
    t.registry.revoke(&pid).unwrap();
    let req = axum::http::Request::builder().uri("/v1/chats").body(axum::body::Body::empty()).unwrap();
    let mut req = req;
    req.extensions_mut().insert(tun::TunnelPeer { pid });
    use tower::ServiceExt;
    let resp = crate::serve::api::router(h.handle.shared.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 401);
}

/// Interop with a real relay (the Go `langstroth-ai/langstroth-relay` with
/// `LIGUS_RELAY_TUNNEL=open`). Ignored unless `LIGUS_TUNNEL_INTEROP_RELAY` is set.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interop_against_a_real_relay() {
    let Ok(relay) = hive_core::compat::var("LIGUS_TUNNEL_INTEROP_RELAY") else {
        eprintln!("skipped: set LIGUS_TUNNEL_INTEROP_RELAY=http://127.0.0.1:<port>");
        return;
    };
    let h = serve_with_relay(&relay).await;
    let t = h.handle.shared.tunnel.clone().unwrap();
    for _ in 0..100 {
        if t.live_status().connected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(t.live_status().connected, "host never connected: {:?}", t.live_status());
    let (_, _, _, ps) = parse_link(&pair_link(&h));
    // Let serve pick up and announce the window.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let info = host_info(&h, &relay);
    let phone = PhoneKeys::generate();
    let (mut c, pid, credential) = testkit::phone_enroll(&info, &phone, &ps, "Interop").await.expect("enroll via the real relay");
    assert_eq!(c.request("GET", "/v1/health", None).await.unwrap().status, 200);
    drop(c);
    let mut c = testkit::phone_session(&info, &phone, &credential).await.expect("session via the real relay");
    assert_eq!(c.request("GET", "/v1/chats", None).await.unwrap().status, 200);
    let sse = c.open("GET", "/v1/events", &[], None, true).await.unwrap();
    let _ = c.recv_timeout(Duration::from_secs(5)).await.unwrap();
    h.handle.shared.hub.emit("turn.status", json!({ "canary": "interop" }));
    let mut text = String::new();
    while !text.contains("interop") {
        let f = c.recv_timeout(Duration::from_secs(5)).await.unwrap();
        if f.ty == frame::DATA && f.stream == sse {
            text.push_str(&String::from_utf8_lossy(&f.payload));
        }
    }
    t.revoke(&pid).unwrap();
    loop {
        let f = c.recv_timeout(Duration::from_secs(5)).await.unwrap();
        if f.ty == frame::GOAWAY {
            assert_eq!(f.code(), Some(frame::goaway::REVOKED));
            break;
        }
    }
    for _ in 0..50 {
        if t.pending_revocations().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(t.pending_revocations().is_empty(), "relay never acked the revoke");
    let err = testkit::phone_session(&info, &phone, &credential).await.err().expect("revoked at the relay");
    assert_eq!(testkit::close_code(&err), Some(4003), "{err:#}");
}

/// A token set through the tunnel: the router called the way the connector
/// calls it, for a paired phone. (The LAN-bind refusal is
/// `forges_api::tests::tokens_travel_only_through_the_tunnel_or_loopback`.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forge_tokens_can_be_set_through_the_tunnel() {
    let relay = FakeRelay::start().await;
    let h = serve_with_relay(&relay.url).await;
    // Forge changes need the owner's presence on every box (5.7).
    h.handle.shared.test_presence.store(true, std::sync::atomic::Ordering::SeqCst);
    relay.wait_connected(WAIT).await.unwrap();
    h.send(reqwest::Method::POST, "/v1/forges", Some(json!({"kind": "gitlab", "baseUrl": "https://gitlab.example.com", "auth": "token"}))).await;
    let t = h.handle.shared.tunnel.clone().unwrap();
    let (_, _, _, ps) = parse_link(&pair_link(&h));
    relay.wait_for("enroll", |m| m["eid"] == ps.eid, WAIT).await.unwrap();
    let info = host_info(&h, &relay.url);
    let phone = PhoneKeys::generate();
    let (_c, pid, _) = testkit::phone_enroll(&info, &phone, &ps, "p").await.unwrap();
    assert!(t.registry.is_active(&pid));
    let body = json!({"purpose": "write", "token": "glpat-tunnel-0123456789"}).to_string();
    let mut req = axum::http::Request::builder()
        .method("PUT")
        .uri("/v1/forges/gitlab:gitlab.example.com/token")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body))
        .unwrap();
    req.extensions_mut().insert(tun::TunnelPeer { pid });
    use tower::ServiceExt;
    let resp = crate::serve::api::router(h.handle.shared.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("glpat-tunnel"), "the answer never echoes the token");
    let store = crate::forge_cmd::token_store(&h.handle.shared.cfg.data_dir);
    let id = hive_forge::ForgeId::parse("gitlab:gitlab.example.com").unwrap();
    assert_eq!(store.get(&id, hive_forge::Purpose::Write).unwrap().unwrap().expose(), "glpat-tunnel-0123456789");
    h.handle.shutdown();
}

/// Phase A, item 6: phone revocation is three-part. A workspace
/// `DeviceRevoked` of the phone's own device, synced to the box, also
/// revokes the phone's tunnel pairing: the relay is told to drop it and a
/// request on its session gets 401.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoked_phone_device_cuts_its_tunnel() {
    let relay = FakeRelay::start().await;
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(WAIT).await.unwrap();
    let t = h.handle.shared.tunnel.clone().unwrap();
    let (_, _, _, ps) = parse_link(&pair_link(&h));
    relay.wait_for("enroll", |m| m["eid"] == ps.eid, WAIT).await.unwrap();
    let info = host_info(&h, &relay.url);
    let phone = PhoneKeys::generate();
    let (_c, pid, _) = testkit::phone_enroll(&info, &phone, &ps, "p").await.unwrap();
    let phone_device = phone.device_id();
    assert_eq!(t.registry.get(&pid).unwrap().device_id, Some(phone_device));
    // Nothing revoked yet: the hook cuts nothing.
    assert!(h.handle.shared.revoke_phones_of_revoked_devices().is_empty());
    // The workspace owner revokes the phone's device on their desktop.
    {
        let mut owner = crate::open_service(&h.desktop).unwrap();
        let ws = h.handle.shared.ws;
        owner.revoke_device(ws, ws, phone_device).unwrap();
    }
    crate::sync_once(&h.desktop).await.unwrap();
    // The box pulls it and revokes the pairing.
    let deadline = tokio::time::Instant::now() + WAIT;
    while t.registry.is_active(&pid) {
        assert!(tokio::time::Instant::now() < deadline, "the revoked phone's pairing stayed active");
        h.handle.shared.sync_kick.notify_one();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    relay.wait_for("revoke", |m| m["pid"] == json!(pid), WAIT).await.unwrap();
    let mut req = axum::http::Request::builder().uri("/v1/chats").body(axum::body::Body::empty()).unwrap();
    req.extensions_mut().insert(tun::TunnelPeer { pid });
    use tower::ServiceExt;
    let resp = crate::serve::api::router(h.handle.shared.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 401);
}

// ── Phase B: phone revocation is complete (tunnel, push, session) ───────────

use hive_runtime::revocation_status::StepState;

/// `phone`'s device claim for the pairing with `keys`'s static on `t`.
fn device_claim(t: &tun::TunnelHost, phone: &PhoneKeys, keys: &PhoneKeys) -> Value {
    let sig = hive_core::crypto::sign_p256(&phone.device.key, &tun::device_bind::bind_message(t.host_id(), &keys.static_pub()));
    json!({ "id": phone.device_id(), "sig": tun::b64u(sig) })
}

/// A fresh pairing window the relay has heard of.
async fn open_window(h: &Harness, relay: &FakeRelay) -> PairingSecret {
    let (_, _, _, ps) = parse_link(&pair_link(h));
    relay.wait_for("enroll", |m| m["eid"] == ps.eid, WAIT).await.unwrap();
    ps
}

/// Pair a phone that proves its workspace device at enrollment.
async fn pair_bound(h: &Harness, relay: &FakeRelay) -> (Arc<tun::TunnelHost>, HostInfo, PhoneKeys, String, String, Arc<hive_core::testkit::Phone>) {
    let t = h.handle.shared.tunnel.clone().unwrap();
    let ps = open_window(h, relay).await;
    let info = host_info(h, &relay.url);
    let keys = PhoneKeys::generate();
    let (_c, pid, credential) = testkit::phone_enroll(&info, &keys, &ps, "Mara's iPhone").await.unwrap();
    assert_eq!(t.registry.get(&pid).unwrap().device_id, Some(keys.device_id()), "bound at pairing");
    let phone = keys.device.clone();
    (t, info, keys, pid, credential, phone)
}

/// The owner revokes the device on their desktop (not synced yet).
fn revoke_on_desktop(h: &Harness, device: Uuid) {
    let mut owner = crate::open_service(&h.desktop).unwrap();
    let ws = h.handle.shared.ws;
    owner.revoke_device(ws, ws, device).unwrap();
}

async fn wait_until(what: &str, within: Duration, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Full passes the box's revocation watch has completed.
fn watch_passes(h: &Harness) -> u64 {
    h.handle.shared.revocation_passes.load(std::sync::atomic::Ordering::SeqCst)
}

/// Until the revocation watch has completed two full passes after this call.
/// The first may have read the store just before the call; the second began
/// after it, so it saw every store and registry change made before the call
/// and acted on it. "Cut by then" is the spec's latency bound (at most one
/// [`crate::serve::REVOCATION_WATCH`] sleep plus one roster fold after the
/// change) stated in the watch's own passes: load can slow the passes
/// without failing the test, but a cut that waits for anything else (a sync
/// round, a later store change, a slower timer) still fails it.
async fn watch_caught_up(h: &Harness) {
    assert!(crate::serve::REVOCATION_WATCH <= Duration::from_millis(500), "the spec: the roster is re-checked within 0.5 s of a store change");
    let p0 = watch_passes(h);
    wait_until("two full revocation-watch passes", WAIT, || watch_passes(h) >= p0 + 2).await;
}

/// Wait for the phone's session to end (GOAWAY, or the socket closing).
async fn session_ends<S>(c: &mut testkit::PhoneConn<S>, within: Duration) -> bool
where
    S: futures_util::Stream<Item = Result<tokio_tungstenite::tungstenite::Message, tokio_tungstenite::tungstenite::Error>>
        + futures_util::Sink<tokio_tungstenite::tungstenite::Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin,
{
    tokio::time::timeout(within, async {
        loop {
            match c.recv().await {
                Ok(f) if f.ty == frame::GOAWAY => return,
                Ok(_) => {}
                Err(_) => return,
            }
        }
    })
    .await
    .is_ok()
}

fn pushes_to(relay: &FakeRelay, pid: &str) -> usize {
    relay.messages("push").iter().filter(|m| m["pid"] == json!(pid)).count()
}

/// The full cut: once the box pulls the revocation, the phone's live tunnel
/// closes (the sync round wakes the revocation watch, so the cut doesn't
/// wait for its poll), the relay drops the pairing and the push token, no
/// push goes out, the old credential is refused, and each step's status is
/// reported. Waits on the events themselves (the close, the ledger), not on
/// wall-clock bounds a loaded machine can miss.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoking_a_phone_device_cuts_tunnel_and_push_and_reports_each_step() {
    let relay = FakeRelay::start().await;
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(WAIT).await.unwrap();
    let (t, info, keys, pid, credential, phone) = pair_bound(&h, &relay).await;

    // A live session that registered an APNs token and turned pushes on.
    let mut c = testkit::phone_session_with(&info, &keys, &credential, true).await.unwrap();
    assert_eq!(c.request("PUT", "/v1/tunnel/push", Some(json!({ "enabled": true }))).await.unwrap().status, 200);
    assert!(relay.state.push_tokens.lock().unwrap().contains(&pid));
    // Pushes reach the phone before (so the "none after" check can fail).
    let before = t.push_test(Some(&pid), WAIT).await;
    assert!(before.phones[0].sent, "{before:?}");
    assert_eq!(relay.state.delivered.lock().unwrap().iter().filter(|p| **p == pid).count(), 1);

    revoke_on_desktop(&h, phone.device_id);
    crate::sync_once(&h.desktop).await.unwrap();
    h.handle.shared.sync_kick.notify_one();
    // Bound: from the moment the box's roster has the revocation.
    let shared = h.handle.shared.clone();
    wait_until("the box folds the revocation", WAIT, || {
        shared.device_lookup(phone.device_id) == tun::device_bind::DeviceLookup::Revoked
    })
    .await;
    // Bound: the watch's first full pass after the box's roster has the
    // revocation cuts the pairing (the cut ends the live session at once).
    watch_caught_up(&h).await;
    assert!(!t.registry.is_active(&pid), "the first watch pass after the fold didn't cut the pairing");
    assert!(session_ends(&mut c, WAIT).await, "the revoked phone's live tunnel stayed open");

    // The relay is told (HTTP route), drops the push token, and the steps settle.
    wait_until("revocation steps settle", WAIT, || t.ledger.list().first().is_some_and(|e| e.status.complete())).await;
    let e = t.ledger.list().remove(0);
    assert_eq!(e.status.device_id, phone.device_id.to_string());
    assert_eq!((e.status.tunnel.state, e.status.push.state, e.status.session.state), (StepState::Done, StepState::Done, StepState::NotApplicable));
    // (Confirmed by the HTTP route or the control `revoke` ack, whichever lands first.)
    assert!(relay.state.revoked.lock().unwrap().contains(&pid));
    assert!(!relay.state.push_tokens.lock().unwrap().contains(&pid), "push token survived");
    let listed = h.send(reqwest::Method::GET, "/v1/revocations", None).await;
    assert_eq!(listed["revocations"][0]["summary"], json!("revoked: tunnel cut, push removed, session: not applicable"));
    assert_eq!(listed["revocations"][0]["complete"], json!(true));

    // No push after revocation: neither the gated path nor push-test sends one.
    let sent_before = pushes_to(&relay, &pid);
    t.push_available.store(true, std::sync::atomic::Ordering::SeqCst);
    // Both report what they handed to the relay connection: nothing.
    assert_eq!(t.push_all(&tun::PushContent { kind: "test".into(), title: "x".into(), ..Default::default() }), 0, "a push for the revoked phone was sent");
    let after = t.push_test(Some(&pid), Duration::from_secs(1)).await;
    assert!(!after.phones[0].sent);
    assert_eq!(pushes_to(&relay, &pid), sent_before, "a push for the revoked phone reached the relay");
    assert_eq!(relay.state.delivered.lock().unwrap().iter().filter(|p| **p == pid).count(), 1);

    // Its credential no longer opens a session.
    let err = testkit::phone_session(&info, &keys, &credential).await.err().expect("refused");
    assert_eq!(testkit::close_code(&err), Some(4003), "{err:#}");
    h.handle.shutdown();
}

/// A revoked phone can't pair again: not with its old keys, not with a claim
/// of the revoked device, and a fresh unbound pairing that later claims it is
/// revoked on the spot.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoked_phone_device_cannot_pair_again() {
    let relay = FakeRelay::start().await;
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(WAIT).await.unwrap();
    let (t, info, keys, pid, _, phone) = pair_bound(&h, &relay).await;
    revoke_on_desktop(&h, phone.device_id);
    crate::sync_once(&h.desktop).await.unwrap();
    wait_until("pairing cut", WAIT, || !t.registry.is_active(&pid)).await;
    let active = || t.registry.list().into_iter().filter(|r| r.is_active()).count();

    // Same keys, with the claim.
    let ps = open_window(&h, &relay).await;
    assert!(testkit::phone_enroll(&info, &keys, &ps, "again").await.is_err());
    // Same keys, no claim (the revoked pairing's static).
    let ps = open_window(&h, &relay).await;
    assert!(testkit::phone_enroll_with_device(&info, &keys, &ps, "again", None).await.is_err());
    // New keys claiming the revoked device.
    let ps = open_window(&h, &relay).await;
    let fresh = PhoneKeys::generate();
    assert!(testkit::phone_enroll_with_device(&info, &fresh, &ps, "again", Some(device_claim(&t, &keys, &fresh))).await.is_err());
    assert_eq!(active(), 0, "a refused enrollment left a pairing behind");
    let _ = phone;
    h.handle.shutdown();
}

/// Residual 1: every pairing proves its phone's workspace device. Fresh keys
/// with no claim (what a revoked phone would try), a claim this box doesn't
/// know, a claim made for another pairing or another box, and a claim of a
/// device that isn't a phone are all refused; the old "pair unbound, bind
/// later" route is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_pairing_must_prove_its_phone_device() {
    let relay = FakeRelay::start().await;
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(WAIT).await.unwrap();
    let t = h.handle.shared.tunnel.clone().unwrap();
    let info = host_info(&h, &relay.url);
    let active = || t.registry.list().into_iter().filter(|r| r.is_active()).count();

    // No claim at all.
    let ps = open_window(&h, &relay).await;
    assert!(testkit::phone_enroll_with_device(&info, &PhoneKeys::generate(), &ps, "unbound", None).await.is_err());
    // A device this box has never heard of (a validly signed claim).
    let stranger = hive_core::testkit::Phone::new();
    let keys = PhoneKeys::generate();
    let sig = hive_core::crypto::sign_p256(&stranger.key, &tun::device_bind::bind_message(t.host_id(), &keys.static_pub()));
    let ps = open_window(&h, &relay).await;
    assert!(testkit::phone_enroll_with_device(&info, &keys, &ps, "stranger", Some(json!({ "id": stranger.device_id, "sig": tun::b64u(sig) }))).await.is_err());
    // A claim lifted from another pairing (bound to that pairing's static).
    let victim = PhoneKeys::generate();
    let lifted = victim.device_claim(t.host_id());
    let attacker = PhoneKeys::generate();
    let ps = open_window(&h, &relay).await;
    assert!(testkit::phone_enroll_with_device(&info, &attacker, &ps, "lifted", Some(lifted)).await.is_err());
    // A claim made for another box (its host id), same static.
    let other_box = attacker.device_claim("AAAAAAAAAAAAAAAAAAAAAA");
    let ps = open_window(&h, &relay).await;
    assert!(testkit::phone_enroll_with_device(&info, &attacker, &ps, "other box", Some(other_box)).await.is_err());
    assert_eq!(active(), 0, "a refused enrollment left a pairing behind");

    // The bind-later route no longer exists.
    let ps = open_window(&h, &relay).await;
    let ok = PhoneKeys::generate();
    let (mut c, pid, _) = testkit::phone_enroll(&info, &ok, &ps, "ok").await.unwrap();
    assert_eq!(t.registry.get(&pid).unwrap().device_id, Some(ok.device_id()));
    let r = c.request("PUT", "/v1/tunnel/device", Some(ok.device_claim(t.host_id()))).await.unwrap();
    assert!(r.status == 404 || r.status == 405, "{}", r.status);
    h.handle.shutdown();
}

/// Whatever path the revocation arrives by (here: another process writing
/// the box's `hive.db`, the way a phone's own `DeviceRevoked` would land),
/// the box cuts the pairing without a sync round: the desktop never syncs,
/// so only the revocation watch reading the store can see it. Waits on the
/// close event and the report, not a wall-clock bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revocation_arriving_by_any_path_cuts_promptly() {
    let relay = FakeRelay::start().await;
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(WAIT).await.unwrap();
    let (t, info, keys, pid, credential, phone) = pair_bound(&h, &relay).await;
    let mut c = testkit::phone_session(&info, &keys, &credential).await.unwrap();
    assert_eq!(c.request("GET", "/v1/health", None).await.unwrap().status, 200);

    revoke_on_desktop(&h, phone.device_id);
    let ws = h.handle.shared.ws;
    let revocation = hive_runtime::event_store::EventStore::open(h.desktop.data_dir.join("hive.db"))
        .unwrap()
        .list(workspace_config_session_id(ws))
        .unwrap()
        .into_iter()
        .rev()
        .find(|e| matches!(&e.payload, hive_core::SessionEvent::DeviceRevoked { device_id, .. } if *device_id == phone.device_id))
        .expect("the desktop wrote it");
    let mut other_process = hive_runtime::event_store::EventStore::open(h.handle.shared.cfg.data_dir.join("hive.db")).unwrap();
    assert!(other_process.ingest(&revocation).unwrap());
    // Bound: the watch's first full pass after the event landed cuts the
    // pairing; no sync round runs in between (only the watch cuts).
    watch_caught_up(&h).await;
    assert!(!t.registry.is_active(&pid), "the first watch pass after the event landed didn't cut the pairing");
    assert!(session_ends(&mut c, WAIT).await, "live tunnel stayed open");
    // Reported: the ledger has this device's revocation.
    wait_until("revocation reported", WAIT, || {
        t.ledger.list().iter().any(|e| e.status.device_id == phone.device_id.to_string())
    })
    .await;
    // Told by the HTTP route or the control `revoke`, whichever lands first
    // (under load the control ack can settle the step before the HTTP call).
    wait_until("relay told", WAIT, || {
        relay.state.http_revokes.lock().unwrap().iter().any(|(s, b)| *s == 200 && b["pid"] == json!(pid)) || relay.state.revoked.lock().unwrap().contains(&pid)
    })
    .await;
    h.handle.shutdown();
}

/// A relay failure shows as a failed step and is retried (on request, too);
/// a relay without the HTTP route confirms through its control `revoke` ack.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_failures_are_reported_and_retried() {
    let relay = FakeRelay::start().await;
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(WAIT).await.unwrap();
    let (t, _, _, pid, _, phone) = pair_bound(&h, &relay).await;
    *relay.state.http_revoke_fail.lock().unwrap() = Some(503);
    relay.state.control_revoke_fail.store(true, std::sync::atomic::Ordering::SeqCst);
    revoke_on_desktop(&h, phone.device_id);
    crate::sync_once(&h.desktop).await.unwrap();
    // The tunnel and push steps settle separately; wait for both.
    wait_until("tunnel and push steps failed", WAIT, || {
        t.ledger.list().first().is_some_and(|e| e.status.tunnel.state == StepState::Failed && e.status.push.state == StepState::Failed)
    })
    .await;
    let e = t.ledger.list().remove(0);
    assert_eq!(e.status.push.state, StepState::Failed);
    assert!(e.status.tunnel.detail.contains("503"), "{}", e.status.tunnel.detail);
    // Both relay paths failed; the box kept the local cut regardless.
    assert!(!t.registry.is_active(&pid));
    *relay.state.http_revoke_fail.lock().unwrap() = None;
    h.send(reqwest::Method::POST, &format!("/v1/revocations/{}/retry", phone.device_id), None).await;
    wait_until("steps settle after retry", WAIT, || t.ledger.list().first().is_some_and(|e| e.status.complete())).await;
    assert!(!t.registry.is_active(&pid));
    h.handle.shutdown();

    // An older relay (no HTTP route): the control connection's ack confirms.
    let relay = FakeRelay::start().await;
    relay.state.http_revoke_off.store(true, std::sync::atomic::Ordering::SeqCst);
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(WAIT).await.unwrap();
    let (t, _, _, pid, _, phone) = pair_bound(&h, &relay).await;
    revoke_on_desktop(&h, phone.device_id);
    crate::sync_once(&h.desktop).await.unwrap();
    wait_until("steps settle via control ack", WAIT, || t.ledger.list().first().is_some_and(|e| e.status.complete())).await;
    relay.wait_for("revoke", |m| m["pid"] == json!(pid), WAIT).await.unwrap();
    assert!(relay.state.http_revokes.lock().unwrap().iter().all(|(s, _)| *s == 404));
    h.handle.shutdown();
}

/// With langstroth-auth operator access configured, the device's sign-in
/// session is ended too.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_session_step_ends_the_langstroth_auth_session() {
    let calls: Arc<Mutex<Vec<(String, String)>>> = Default::default();
    let c2 = calls.clone();
    let auth = Router::new().route(
        "/admin/v1/tenants/{tenant}/devices/{device}/revoke",
        post(move |axum::extract::Path((tenant, device)): axum::extract::Path<(String, String)>, headers: HeaderMap| {
            let c2 = c2.clone();
            async move {
                if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some("Bearer admin-test-token") {
                    return (axum::http::StatusCode::UNAUTHORIZED, axum::Json(json!({})));
                }
                c2.lock().unwrap().push((tenant, device));
                (axum::http::StatusCode::OK, axum::Json(json!({ "revoked": 2 })))
            }
        }),
    );
    let auth_url = spawn(auth).await;
    let relay = FakeRelay::start().await;
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(WAIT).await.unwrap();
    let t = h.handle.shared.tunnel.clone().unwrap();
    t.set_auth_admin(Some(tun::revocation::AuthAdmin { url: auth_url, tenant: "acme".into(), token: "admin-test-token".into() }));
    let (_, _, _, _, _, phone) = pair_bound(&h, &relay).await;
    revoke_on_desktop(&h, phone.device_id);
    crate::sync_once(&h.desktop).await.unwrap();
    wait_until("steps settle", WAIT, || t.ledger.list().first().is_some_and(|e| e.status.complete())).await;
    let e = t.ledger.list().remove(0);
    assert_eq!(e.status.session.state, StepState::Done);
    assert_eq!(e.status.summary(), "revoked: tunnel cut, push removed, session ended");
    assert_eq!(calls.lock().unwrap().clone(), vec![("acme".to_string(), phone.device_id.to_string())]);
    h.handle.shutdown();
}

/// A revocation folded between the claim check (msg1) and storing the
/// pairing (E4) must still refuse it: the box's view of the device is stale
/// at msg1, the `DeviceRevoked` has already landed and been acted on, and no
/// later store change would wake the revocation watch again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revocation_racing_an_enrollment_still_refuses_it() {
    let relay = FakeRelay::start().await;
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(WAIT).await.unwrap();
    let t = h.handle.shared.tunnel.clone().unwrap();
    let keys = PhoneKeys::generate();
    revoke_on_desktop(&h, keys.device_id());
    crate::sync_once(&h.desktop).await.unwrap();
    h.handle.shared.sync_kick.notify_one();
    let shared = h.handle.shared.clone();
    wait_until("the box folds the revocation", WAIT, || {
        shared.device_lookup(keys.device_id()) == tun::device_bind::DeviceLookup::Revoked
    })
    .await;
    // Let the watch see that change (there is no pairing to cut yet).
    watch_caught_up(&h).await;
    // The claim check at msg1 still sees the device as certified (a stale
    // view); every later lookup sees the real roster.
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (c2, weak) = (calls.clone(), Arc::downgrade(&h.handle.shared));
    let (id, pk) = (keys.device_id(), keys.device.public_key());
    t.set_device_lookup(Arc::new(move |d| {
        if d == id && c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            return tun::device_bind::DeviceLookup::Certified { public_key: pk.clone(), phone: true };
        }
        weak.upgrade().map(|s| s.device_lookup(d)).unwrap_or(tun::device_bind::DeviceLookup::Unknown)
    }));
    let ps = open_window(&h, &relay).await;
    let info = host_info(&h, &relay.url);
    // Returns once the box decided E4 (PONG after storing the pairing, or
    // the session ended on a refusal); a stored pairing bumps the registry
    // generation, which the watch's next pass re-checks.
    let _ = testkit::phone_enroll(&info, &keys, &ps, "racer").await;
    watch_caught_up(&h).await;
    let active: Vec<_> = t.registry.list().into_iter().filter(|r| r.is_active()).collect();
    assert!(active.is_empty(), "a pairing of a revoked device stayed active: {:?}", active.iter().map(|r| &r.pid).collect::<Vec<_>>());
    h.handle.shutdown();
}

/// Residual 2: a revoked phone also held the direct-path bearer token
/// (`serve.token`, from its pairing link). Revoking its device rotates the
/// token once, ends direct streams opened with the old one, and the phones
/// that remain learn the new token over their tunnel.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoking_a_phone_device_rotates_the_direct_token() {
    let relay = FakeRelay::start().await;
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(WAIT).await.unwrap();
    let (t, info, _, pid, _, phone) = pair_bound(&h, &relay).await;
    let (_, _, keep_keys, keep_pid, keep_cred, _) = pair_bound(&h, &relay).await;
    let old = h.token.clone();
    // The revoked phone's direct stream, opened with the old token.
    let mut stream = h.events(None).await;

    revoke_on_desktop(&h, phone.device_id);
    crate::sync_once(&h.desktop).await.unwrap();
    h.handle.shared.sync_kick.notify_one();
    wait_until("pairing cut", WAIT, || !t.registry.is_active(&pid)).await;
    wait_until("token rotated", WAIT, || h.handle.shared.token() != old).await;
    let on_disk = std::fs::read_to_string(&h.handle.shared.opts.token_file).unwrap();
    assert_eq!(on_disk.trim(), h.handle.shared.token());

    // The old token no longer opens anything on the direct path …
    let r = h.client.get(format!("{}/v1/chats", h.base)).bearer_auth(&old).send().await.unwrap();
    assert_eq!(r.status(), 401);
    // … and the stream it opened has ended.
    let ended = tokio::time::timeout(WAIT, async {
        while stream.recv().await.is_some() {}
    })
    .await;
    assert!(ended.is_ok(), "a direct stream opened with the revoked token stayed open");

    // The phone that remains fetches the new token over its tunnel.
    assert!(t.registry.is_active(&keep_pid));
    let mut c = testkit::phone_session(&info, &keep_keys, &keep_cred).await.unwrap();
    let r = c.request("GET", "/v1/tunnel/direct", None).await.unwrap();
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(r.json()["token"], json!(h.handle.shared.token()));
    // The direct path itself never hands the token out.
    let r = h.client.get(format!("{}/v1/tunnel/direct", h.base)).bearer_auth(h.handle.shared.token()).send().await.unwrap();
    assert_eq!(r.status(), 404);

    // Once only: later store changes don't rotate it again.
    let now = h.handle.shared.token();
    let r = h.client.post(format!("{}/v1/chats", h.base)).bearer_auth(&now).json(&json!({ "title": "later" })).send().await.unwrap();
    assert!(r.status().is_success());
    watch_caught_up(&h).await;
    assert_eq!(h.handle.shared.token(), now);
    h.handle.shutdown();
}

/// Residual 3, end to end on the real roster: the box signs its report with
/// its device key and the desktop verifies it; the box takes a retry only
/// from a member's certified desktop device, never from a mere key holder,
/// a revoked device or a phone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revocation_presence_is_signed_and_checked_against_the_roster() {
    use hive_runtime::revocation_status as rs;
    let relay = FakeRelay::start().await;
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(WAIT).await.unwrap();
    let (t, _, _, _, _, phone) = pair_bound(&h, &relay).await;
    revoke_on_desktop(&h, phone.device_id);
    crate::sync_once(&h.desktop).await.unwrap();
    wait_until("ledger entry", WAIT, || !t.ledger.list().is_empty()).await;
    let ws = h.handle.shared.ws;
    let k = hive_runtime::keyring::KeyringEngine::passphrase(ws, hive_core::derive_workspace_key("serve-test"));

    // The box's report, as the presence loop seals it.
    let report = h.handle.shared.lock().sign_revocation_report(t.ledger.report(&h.handle.shared.host_id));
    let mut blobs = std::collections::HashMap::new();
    blobs.insert(rs::report_slot(&report.host_id), rs::seal_report(&k, &report).unwrap());
    crate::sync_once(&h.desktop).await.unwrap();
    let desk = crate::open_service(&h.desktop).unwrap();
    let shown = rs::open_reports(&blobs, &k, |d| desk.certified_device_key(ws, d).ok().flatten());
    assert_eq!(shown.len(), 1, "the desktop rejected the box's own signed report");
    // The same report, tampered by a key holder: dropped.
    let mut forged = report.clone();
    forged.devices[0].tunnel.state = hive_runtime::revocation_status::StepState::Done;
    forged.devices[0].tunnel.detail = "all good".into();
    blobs.insert(rs::report_slot(&report.host_id), rs::seal_report(&k, &forged).unwrap());
    assert!(rs::open_reports(&blobs, &k, |d| desk.certified_device_key(ws, d).ok().flatten()).is_empty());

    // Retries: the box accepts the desktop's device, nothing else.
    crate::sync_once(&h.desktop).await.unwrap();
    h.handle.shared.sync_kick.notify_one();
    let desk_id = desk.device_id();
    let shared = h.handle.shared.clone();
    wait_until("box knows the desktop device", WAIT, || shared.lock().retry_requester_key(ws, desk_id).is_some()).await;
    let svc = h.handle.shared.lock();
    assert!(svc.retry_requester_key(ws, Uuid::new_v4()).is_none(), "an unknown device");
    assert!(svc.retry_requester_key(ws, phone.device_id).is_none(), "a revoked phone");
    drop(svc);
    let now = Timestamp::now().inner().unix_timestamp();
    let good = desk.sign_revocation_retry(rs::retry_request(&phone.device_id.to_string(), desk_id));
    let holder = hive_core::SigningKeypair::generate().unwrap();
    let bad = rs::retry_request(&phone.device_id.to_string(), desk_id).sign(&holder);
    for (req, want) in [(good, 1usize), (bad, 0)] {
        let mut b = std::collections::HashMap::new();
        b.insert(rs::retry_slot(&req.device_id), rs::seal_retry(&k, &req).unwrap());
        let svc = h.handle.shared.lock();
        assert_eq!(rs::open_retries(&b, &k, 0, now + 1, |d| svc.retry_requester_key(ws, d)).len(), want);
    }
    h.handle.shutdown();
}

/// Residual 4: the revocation watch reads only new rows; chat traffic on a
/// busy workspace never re-checks the roster, a config-log change does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_traffic_does_not_recheck_the_roster() {
    let relay = FakeRelay::start().await;
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(WAIT).await.unwrap();
    let (t, _, _, pid, _, phone) = pair_bound(&h, &relay).await;
    let checks = || h.handle.shared.revocation_checks.load(std::sync::atomic::Ordering::Relaxed);
    watch_caught_up(&h).await;
    let chat = h.send(reqwest::Method::POST, "/v1/chats", Some(json!({ "title": "busy" }))).await["chat"]["id"].as_str().unwrap().to_string();
    // (events row, equivocations row, registry generation) right now.
    let config_id = workspace_config_session_id(h.handle.shared.ws);
    let mark = || {
        let svc = h.handle.shared.lock();
        (svc.store().max_row_id().unwrap(), svc.store().max_equivocation_row_id().unwrap(), t.registry.generation())
    };
    // Whether anything that legitimately re-checks (a config-log row, a
    // registry change) happened since `m`: the box's own start-up writes and
    // the pairing's bookkeeping can land late on a loaded machine.
    let disturbed = |m: (i64, i64, u64)| {
        let svc = h.handle.shared.lock();
        svc.store().session_rows_after(config_id, m.0, m.1).unwrap() || t.registry.generation() != m.2
    };
    let mut quiet_round = false;
    for _ in 0..10 {
        // Marked before the watch catches up: anything that landed before
        // the mark is behind the watch by the time `before` is read.
        let m = mark();
        watch_caught_up(&h).await;
        let before = checks();
        // Each message is a store change the watch has fully looked at (a
        // pass that began after it) before the next one, without re-checking.
        for i in 0..8 {
            h.send(reqwest::Method::POST, &format!("/v1/chats/{chat}/messages"), Some(json!({ "body": format!("msg {i}") }))).await;
            watch_caught_up(&h).await;
        }
        assert!(mark().0 > m.0, "the chat messages were stored");
        if disturbed(m) {
            continue; // not only chat traffic in this round: try again
        }
        assert_eq!(checks(), before, "chat messages re-checked the roster");
        quiet_round = true;
        break;
    }
    assert!(quiet_round, "never had a round of only chat traffic");
    let before = checks();
    // A revocation on the config log is still picked up.
    revoke_on_desktop(&h, phone.device_id);
    crate::sync_once(&h.desktop).await.unwrap();
    wait_until("pairing cut", WAIT, || !t.registry.is_active(&pid)).await;
    assert!(checks() > before);
    h.handle.shutdown();
}

/// A box whose clock is far off the relay's still gets its revocation
/// confirmed: the relay says its time, the box corrects and retries.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_skewed_clock_still_confirms_the_revocation() {
    let relay = FakeRelay::start().await;
    relay.state.clock_offset.store(1_000, std::sync::atomic::Ordering::SeqCst);
    // Only the HTTP route can confirm.
    relay.state.control_revoke_fail.store(true, std::sync::atomic::Ordering::SeqCst);
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(WAIT).await.unwrap();
    let (t, _, _, pid, _, phone) = pair_bound(&h, &relay).await;
    revoke_on_desktop(&h, phone.device_id);
    crate::sync_once(&h.desktop).await.unwrap();
    wait_until("pairing cut", WAIT, || !t.registry.is_active(&pid)).await;
    wait_until("relay confirms despite the skew", WAIT, || {
        relay.state.http_revokes.lock().unwrap().iter().any(|(s, b)| *s == 200 && b["pid"] == json!(pid))
    })
    .await;
    wait_until("steps settle", WAIT, || t.ledger.list().first().is_some_and(|e| e.status.tunnel.state == StepState::Done)).await;
    h.handle.shutdown();
}

/// Phase B integration: the serve box is its own service member, and an
/// Admin revokes its device like any other. A revoked box can no longer act
/// in the workspace, so it cuts every pairing it holds (the relay is told,
/// requests on their sessions get 401), rotates the direct-path token once,
/// and cuts a pairing made after the revocation too. Who may revoke a box's
/// device at all is the fold's (`service_admission`, `phones_and_services`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoking_the_boxs_own_device_cuts_every_pairing_it_holds() {
    let relay = FakeRelay::start().await;
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(WAIT).await.unwrap();
    let (t, _, _, a, _, _) = pair_bound(&h, &relay).await;
    let (_, _, _, b, _, _) = pair_bound(&h, &relay).await;
    let old = h.token.clone();
    let own: Uuid = h.handle.shared.host_id.parse().unwrap();
    // A phone's revocation elsewhere cuts nothing here yet.
    assert!(h.handle.shared.revoke_phones_of_revoked_devices().is_empty());

    revoke_on_desktop(&h, own);
    crate::sync_once(&h.desktop).await.unwrap();
    h.handle.shared.sync_kick.notify_one();
    wait_until("every pairing cut", WAIT, || !t.registry.is_active(&a) && !t.registry.is_active(&b)).await;
    for pid in [&a, &b] {
        relay.wait_for("revoke", |m| m["pid"] == json!(pid), WAIT).await.unwrap();
        let mut req = axum::http::Request::builder().uri("/v1/chats").body(axum::body::Body::empty()).unwrap();
        req.extensions_mut().insert(tun::TunnelPeer { pid: pid.clone() });
        use tower::ServiceExt;
        let resp = crate::serve::api::router(h.handle.shared.clone()).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 401);
    }
    wait_until("token rotated", WAIT, || h.handle.shared.token() != old).await;
    let r = h.client.get(format!("{}/v1/chats", h.base)).bearer_auth(&old).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let rotated = h.handle.shared.token();

    // A pairing made after the revocation is cut as it appears; the token
    // isn't rotated again.
    let (_, _, _, c, _, _) = pair_bound(&h, &relay).await;
    wait_until("the later pairing cut", WAIT, || !t.registry.is_active(&c)).await;
    assert_eq!(h.handle.shared.token(), rotated);
    h.handle.shutdown();
}

// ── "Add this phone" (certify, then pair, then GET /v1/me) ─────────────────

/// End to end, with the box's real roster (no test trust): a phone that
/// isn't certified yet is refused at pairing with the typed close
/// `4040 device_not_certified`, not a silent close; its person certifies the
/// code the phone shows on their desktop (with their passkey); once the box
/// synced the certificate the same phone pairs, and `GET /v1/me` through the
/// tunnel tells it its workspace, config log, account, role, tier and
/// relying party with no manual setup.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn add_this_phone_then_pair_and_ask_who_am_i() {
    use hive_runtime::phone_enroll::PhoneCode;
    let relay = FakeRelay::start().await;
    let (endpoint, _) = spawn_model(vec![]).await;
    let r = relay.url.clone();
    let h = harness_with(&endpoint, "m", &[("README.md", "hi\n")], move |o| {
        o.remote = Some(RelaySettings::new(&r, None, true).unwrap());
    })
    .await;
    relay.wait_connected(Duration::from_secs(10)).await.unwrap();
    let t = h.handle.shared.tunnel.clone().unwrap();
    let ws = h.handle.shared.ws;
    let info = host_info(&h, &relay.url);
    let keys = PhoneKeys::generate();

    // Not certified: the refusal says so.
    let ps = open_window(&h, &relay).await;
    let err = testkit::phone_enroll(&info, &keys, &ps, "Ana's iPhone").await.err().expect("refused");
    let closed = err.downcast_ref::<testkit::Closed>().unwrap_or_else(|| panic!("{err:#}"));
    assert_eq!((closed.0, closed.1.as_str()), (4040, "device_not_certified"));
    assert!(t.registry.list().iter().all(|r| !r.is_active()), "no pairing left behind");

    // The phone shows its code; the person certifies it on the desktop.
    let phone = keys.device.clone();
    let code = PhoneCode::new_signed(phone.device_id, phone.public_key(), "Ana's iPhone", |m| hive_core::crypto::sign_p256(&phone.key, m)).encode();
    let (account, member_id) = {
        let mut desk = crate::open_service(&h.desktop).unwrap();
        let c = PhoneCode::decode(&code).unwrap();
        assert_eq!(desk.certify_phone_code(ws, &c).unwrap(), phone.device_id, "the id the phone presents");
        (desk.author().account_id.unwrap(), desk.author().id.clone())
    };
    crate::sync_once(&h.desktop).await.unwrap();
    for _ in 0..200 {
        h.handle.shared.sync_kick.notify_one();
        if matches!(h.handle.shared.device_lookup(phone.device_id), tun::device_bind::DeviceLookup::Certified { phone: true, .. }) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Now it pairs, and asks who it is.
    let ps = open_window(&h, &relay).await;
    let (mut c, pid, _) = testkit::phone_enroll(&info, &keys, &ps, "Ana's iPhone").await.unwrap();
    assert_eq!(t.registry.get(&pid).unwrap().device_id, Some(phone.device_id));
    let me = c.request("GET", "/v1/me", None).await.unwrap();
    assert_eq!(me.status, 200, "{:?}", me.json());
    let v = me.json();
    assert_eq!(v["workspaceId"], json!(ws));
    assert_eq!(v["configLogId"], json!(workspace_config_session_id(ws)));
    assert_eq!(v["device"]["id"], json!(phone.device_id));
    assert_eq!(v["device"]["tier"], "phone");
    assert_eq!(v["device"]["fingerprint"], json!(hive_runtime::device_enroll::fingerprint(&phone.public_key())));
    assert_eq!(v["account"]["id"], json!(account));
    assert_eq!(v["account"]["memberId"], json!(member_id));
    let role = h.handle.shared.lock().load(workspace_config_session_id(ws)).unwrap().unwrap().role_of(&member_id);
    assert_eq!(v["account"]["role"], json!(role));
    let rp = h.handle.shared.lock().workspace_presence_policy(ws).unwrap();
    assert_eq!(v["rpId"], json!(rp.rp_id));
    assert_eq!(v["origins"], json!(rp.origins));
    // A query can't make the tunnel describe another device.
    let other = c.request("GET", &format!("/v1/me?deviceId={}", Uuid::new_v4()), None).await.unwrap();
    assert_eq!(other.json()["device"]["id"], json!(phone.device_id));
    h.handle.shutdown();
}

/// Every msg1 refusal has its own documented close code (relay-tunnel.md
/// §12): no claim and a claim for another pairing are `4042
/// device_claim_invalid`, a revoked device is `4041 device_revoked`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refused_pairings_say_why() {
    let relay = FakeRelay::start().await;
    let h = serve_with_relay(&relay.url).await;
    relay.wait_connected(Duration::from_secs(10)).await.unwrap();
    let t = h.handle.shared.tunnel.clone().unwrap();
    let info = host_info(&h, &relay.url);
    let code_of = |e: anyhow::Error| e.downcast_ref::<testkit::Closed>().map(|c| (c.0, c.1.clone())).unwrap_or_else(|| panic!("{e:#}"));

    let ps = open_window(&h, &relay).await;
    let e = testkit::phone_enroll_with_device(&info, &PhoneKeys::generate(), &ps, "x", None).await.err().unwrap();
    assert_eq!(code_of(e), (4042, "device_claim_invalid".to_string()));
    let victim = PhoneKeys::generate();
    let ps = open_window(&h, &relay).await;
    let e = testkit::phone_enroll_with_device(&info, &PhoneKeys::generate(), &ps, "x", Some(victim.device_claim(t.host_id()))).await.err().unwrap();
    assert_eq!(code_of(e), (4042, "device_claim_invalid".to_string()));

    let (t, info, keys, pid, _, phone) = pair_bound(&h, &relay).await;
    revoke_on_desktop(&h, phone.device_id);
    crate::sync_once(&h.desktop).await.unwrap();
    wait_until("pairing cut", Duration::from_secs(10), || !t.registry.is_active(&pid)).await;
    let ps = open_window(&h, &relay).await;
    let e = testkit::phone_enroll(&info, &keys, &ps, "again").await.err().unwrap();
    assert_eq!(code_of(e), (4041, "device_revoked".to_string()));
    h.handle.shutdown();
}

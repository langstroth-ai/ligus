//! Short link codes through the CLI, end to end on the in-process test
//! relay: `ligus devices link` shows a code, `ligus devices request --code`
//! types it and links after the code match; relay restarts on both sides;
//! the clear errors; and the fallback to the approval address.

use std::sync::mpsc;
use std::time::Duration;

use crate::approvals_cmd::{self as ac, ToOptions};
use crate::approvals_cmd_tests::{address, approve, first_rid, fresh, spawn, world, world_on, World};
use crate::link_code_cmd as lc;
use crate::problem::{classify, Problem, EXIT_FIXABLE, EXIT_HUMAN, EXIT_NOT_FOUND, EXIT_USAGE};
use crate::serve::tests::test_relay;

fn opts(tx: Option<mpsc::Sender<String>>) -> ToOptions {
    let mut o = ToOptions::new(String::new(), "laptop".into(), Duration::from_secs(60), false, true);
    o.code_tx = tx;
    o.log_retry = Duration::from_secs(15);
    o
}

/// The desk shows a code; returns it once it is posted.
fn show(w: &World, wait: Duration) -> (tokio::task::JoinHandle<Result<(), (i32, String)>>, String) {
    let (tx, rx) = mpsc::channel();
    let desk = w.desk.clone();
    let shown = tokio::spawn(async move { lc::link(&desk, wait, true, Some(tx)).await.map_err(|e| (classify(&e).0, e.to_string())) });
    let code = rx.recv_timeout(Duration::from_secs(30)).expect("a code is shown");
    (shown, code)
}

/// A new computer types `code` and confirms every question.
fn type_code(w: &World, dir: &str, code: &str, tx: mpsc::Sender<String>) -> tokio::task::JoinHandle<Result<(), (i32, String)>> {
    let comp = fresh(&w.root.join(dir));
    let (code, relay) = (code.to_string(), w.relay.clone());
    tokio::spawn(async move {
        let mut ask = |_: &str| Ok(true);
        lc::cmd_request_code(&comp, &code, Some(relay), opts(Some(tx)), false, Some(&mut ask)).await.map_err(|e| (classify(&e).0, e.to_string()))
    })
}

async fn approve_first(w: &World, rx: &mpsc::Receiver<String>) {
    let rid = first_rid(w).await;
    let mut typed = || Ok(rx.recv_timeout(Duration::from_secs(30))?);
    ac::cmd_approve(&w.desk, approve(w, &rid), Some(&mut typed)).await.unwrap();
}

fn linked(w: &World, dir: &str) -> bool {
    ligus_runtime::computer_link::LinkState::load(&w.root.join(dir)).unwrap().linked_in(w.ws).is_some()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_computer_links_by_typing_the_code_the_desk_shows() {
    let w = world().await;
    let (shown, code) = show(&w, Duration::from_secs(60));
    assert_eq!(code.len(), 7, "K7Q-4MX: {code}");
    let (tx, rx) = mpsc::channel();
    // Typed in lower case, without the dash.
    let linking = type_code(&w, "laptop", &code.replace('-', "").to_lowercase(), tx);
    approve_first(&w, &rx).await;
    linking.await.unwrap().unwrap();
    assert!(linked(&w, "laptop"));
    // The desk saw the code used and stopped (exit 0).
    shown.await.unwrap().unwrap();
    // Single-use: typing it again finds nothing (exit 5, a clear error).
    let (tx2, _rx2) = mpsc::channel();
    let again = type_code(&w, "other", &code, tx2).await.unwrap().unwrap_err();
    assert_eq!(again.0, EXIT_NOT_FOUND, "{}", again.1);
    assert!(again.1.contains("no device is showing"), "{}", again.1);
}

/// The relay restarts while the desk shows its code: the desk posts the same
/// code again and says so, and the computer then types it and links. The
/// relay restarts again while the computer waits for approval: it asks
/// again, and the second request is approved.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_restarts_on_both_sides_are_survived() {
    let (router, _clock, restart) = test_relay::router_with_restart(true);
    let w = world_on(router).await;
    let (shown, code) = show(&w, Duration::from_secs(60));
    restart.restart();
    // The desk notices within a check or two and posts the code again.
    let comp = fresh(&w.root.join("laptop"));
    let mut found = None;
    for _ in 0..8 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        match lc::resolve_code(&comp, &code, &w.relay).await {
            Ok(a) => {
                found = Some(a);
                break;
            }
            Err(e) => assert_eq!(classify(&e).0, EXIT_NOT_FOUND, "{e}"),
        }
    }
    let found = found.expect("the code came back after the restart");
    assert_eq!(found, address(&w).await);
    shown.await.unwrap().unwrap();

    // The computer asks with the address the code found; the relay restarts
    // under its request; it asks again.
    let (tx, rx) = mpsc::channel();
    let cc = comp.clone();
    let mut o = opts(Some(tx));
    o.to = found.encode();
    let linking = tokio::spawn(async move {
        let mut ask = |_: &str| Ok(true);
        ac::cmd_devices_request_to(&cc, o, false, Some(&mut ask)).await.map_err(|e| (classify(&e).0, e.to_string()))
    });
    let first = first_rid(&w).await;
    restart.restart();
    // A member's device registers the mailbox again (the desktop does when
    // it polls); the computer's request comes back under a new rid.
    let _ = address(&w).await;
    let mut rid = first.clone();
    for _ in 0..100 {
        rid = first_rid(&w).await;
        if rid != first {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_ne!(rid, first, "asked again after the restart");
    let mut typed = || Ok(rx.recv_timeout(Duration::from_secs(30))?);
    ac::cmd_approve(&w.desk, approve(&w, &rid), Some(&mut typed)).await.unwrap();
    linking.await.unwrap().unwrap();
    assert!(linked(&w, "laptop"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clear_errors_for_a_bad_an_unknown_or_an_unattended_code() {
    let w = world().await;
    let comp = fresh(&w.root.join("laptop"));
    let e = lc::resolve_code(&comp, "K7Q-4M", &w.relay).await.unwrap_err();
    assert_eq!(classify(&e).0, EXIT_USAGE);
    let e = lc::resolve_code(&comp, "K7Q-4MX", &w.relay).await.unwrap_err();
    assert_eq!(classify(&e).0, EXIT_NOT_FOUND);
    let p = e.downcast_ref::<Problem>().unwrap();
    assert!(p.message.contains("K7Q-4MX") && p.fix.as_deref().unwrap_or_default().contains("--relay"), "{p:?}");
    // No terminal: exit 4 before the code is used up.
    let (_shown, code) = show(&w, Duration::from_secs(5));
    let e = lc::cmd_request_code(&comp, &code, Some(w.relay.clone()), opts(None), false, None).await.unwrap_err();
    assert_eq!(classify(&e).0, EXIT_HUMAN);
    lc::resolve_code(&comp, &code, &w.relay).await.expect("still unused");
    // Nobody types it: the desk exits 4 when --wait passes.
    let e = lc::link(&w.desk, Duration::ZERO, true, None).await.unwrap_err();
    assert_eq!(classify(&e).0, EXIT_HUMAN);
    assert!(e.downcast_ref::<Problem>().unwrap().fix.as_deref().unwrap_or_default().contains("ligus devices request --code"), "{e}");
}

/// A relay with mailboxes but no short codes: the desk exits 3 and its fix
/// carries the approval address to use instead; the computer typing a code
/// there exits 3 and is told to use the address.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relay_without_short_codes_falls_back_to_the_address() {
    let (router, _clock, _restart) = test_relay::router_with_restart(false);
    let w = world_on(router).await;
    let e = lc::link(&w.desk, Duration::from_secs(5), true, None).await.unwrap_err();
    let p = e.downcast_ref::<Problem>().unwrap();
    assert_eq!(p.exit, EXIT_FIXABLE);
    let addr = address(&w).await.encode();
    assert!(p.fix.as_deref().unwrap_or_default().contains(&format!("ligus devices request --to {addr}")), "{p:?}");
    let e = lc::resolve_code(&fresh(&w.root.join("laptop")), "K7Q-4MX", &w.relay).await.unwrap_err();
    let p = e.downcast_ref::<Problem>().unwrap();
    assert_eq!(p.exit, EXIT_FIXABLE);
    assert!(p.fix.as_deref().unwrap_or_default().contains("--to"), "{p:?}");
    // An older relay with no mailbox at all: the desk names the paste path.
    let bare = spawn(axum::Router::new().route("/v1/health", axum::routing::get(|| async { "ok" }))).await;
    let mut desk = w.desk.clone();
    desk.relay_url = Some(bare);
    let e = lc::link(&desk, Duration::from_secs(5), true, None).await.unwrap_err();
    assert_eq!(classify(&e).0, EXIT_FIXABLE);
}

#[test]
fn the_code_relay_is_the_flag_then_the_data_dir_then_the_community_relay() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = fresh(tmp.path());
    assert_eq!(lc::code_relay(&cfg, None), ligus_core::COMMUNITY_RELAY_URL);
    cfg.relay_url = Some("https://relay.example.org/".into());
    assert_eq!(lc::code_relay(&cfg, None), "https://relay.example.org");
    assert_eq!(lc::code_relay(&cfg, Some("https://other.example".into())), "https://other.example");
}

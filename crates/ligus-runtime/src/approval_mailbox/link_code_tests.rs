//! Short link codes against the in-process relay: the showing side posts a
//! code, the typing side resolves it to the approval address once, a relay
//! restart is survived with the same code, and a relay without the routes
//! reads as "no routes" (the caller falls back to the pasted address).

use ligus_core::link_code::ShortCode;
use uuid::Uuid;

use super::*;
use crate::test_relay::{router_with_restart, MailboxRestart, TestClock};

async fn spawn(router: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

struct Env {
    base: String,
    clock: TestClock,
    restart: MailboxRestart,
    client: MailboxClient,
    ws: Uuid,
    pin: String,
    epochs: Vec<EpochKey>,
}

async fn env_with(codes: bool) -> Env {
    let (router, clock, restart) = router_with_restart(codes);
    let base = spawn(router).await;
    let client = MailboxClient::new(&base).with_clock(clock.clock());
    Env { base, clock, restart, client, ws: Uuid::new_v4(), pin: "ab".repeat(16), epochs: vec![EpochKey { epoch: 2, key: rand_bytes() }] }
}

async fn env() -> Env {
    env_with(true).await
}

impl Env {
    async fn offer(&self) -> LinkCodeOffer {
        LinkCodeOffer::post(&self.client, self.ws, &self.pin, &self.epochs).await.unwrap()
    }
    async fn address(&self) -> ApprovalAddress {
        current_address(&self.client, self.ws, &self.pin, &self.epochs).await.unwrap()
    }
    /// A new device: its own client, no token, typing the code.
    fn newcomer(&self) -> MailboxClient {
        MailboxClient::new(&self.base)
    }
}

#[tokio::test]
async fn a_typed_code_resolves_once_to_the_approval_address() {
    let e = env().await;
    let mut offer = e.offer().await;
    assert_eq!(offer.code().as_str().len(), 6);
    assert!(offer.expires_at() > e.clock.now() + 9 * 60_000);
    assert_eq!(offer.check(&e.client, &e.pin, &e.epochs).await.unwrap(), CodeState::Live);

    // Typed with a dash, in lower case.
    let typed = ShortCode::parse(&offer.code().shown().to_lowercase()).unwrap();
    let got = resolve_link_code(&e.newcomer(), &typed, true).await.unwrap();
    assert_eq!(got, e.address().await, "the code carries the address and nothing else");

    // Single-use: a second resolve is a plain miss; the shower sees Used.
    assert_eq!(resolve_link_code(&e.newcomer(), &typed, true).await.unwrap_err(), MailboxError::CodeUnknown);
    assert_eq!(offer.check(&e.client, &e.pin, &e.epochs).await.unwrap(), CodeState::Used);
}

#[tokio::test]
async fn a_mistyped_or_expired_code_is_a_clear_miss() {
    let e = env().await;
    let mut offer = e.offer().await;
    let other = loop {
        let c = ShortCode::generate();
        if &c != offer.code() {
            break c;
        }
    };
    let err = resolve_link_code(&e.newcomer(), &other, true).await.unwrap_err();
    assert_eq!(err, MailboxError::CodeUnknown);
    assert_eq!(err.exit_code(), 5);
    assert!(err.to_string().contains("ask your other device for a new one"), "{err}");

    e.clock.advance(10 * 60_000 + 1_000);
    assert_eq!(offer.check(&e.client, &e.pin, &e.epochs).await.unwrap(), CodeState::Expired);
    assert_eq!(resolve_link_code(&e.newcomer(), offer.code(), true).await.unwrap_err(), MailboxError::CodeUnknown);
}

/// The code is bound to its relay: the same code typed for another relay
/// (here the same server under another name) finds nothing.
#[tokio::test]
async fn a_code_is_for_its_own_relay() {
    let e = env().await;
    let offer = e.offer().await;
    let elsewhere = MailboxClient::new(&e.base.replace("127.0.0.1", "localhost"));
    assert_eq!(resolve_link_code(&elsewhere, offer.code(), true).await.unwrap_err(), MailboxError::CodeUnknown);
    resolve_link_code(&e.newcomer(), offer.code(), true).await.unwrap();
}

#[tokio::test]
async fn a_withdrawn_code_resolves_to_nothing() {
    let e = env().await;
    let offer = e.offer().await;
    offer.withdraw(&e.client).await.unwrap();
    offer.withdraw(&e.client).await.unwrap();
    assert_eq!(resolve_link_code(&e.newcomer(), offer.code(), true).await.unwrap_err(), MailboxError::CodeUnknown);
}

/// The relay keeps codes in memory: after a restart the shower's next check
/// registers the mailbox again and posts the same code for the rest of its
/// life, and the person's code still works.
#[tokio::test]
async fn after_a_relay_restart_the_same_code_is_posted_again() {
    let e = env().await;
    let mut offer = e.offer().await;
    let until = offer.expires_at();
    e.clock.advance(2 * 60_000);
    e.restart.restart();
    assert_eq!(offer.check(&e.client, &e.pin, &e.epochs).await.unwrap(), CodeState::Reposted);
    assert!(offer.expires_at() <= until + 1_000, "a re-post doesn't stretch the code's life");
    assert_eq!(offer.check(&e.client, &e.pin, &e.epochs).await.unwrap(), CodeState::Live);
    let got = resolve_link_code(&e.newcomer(), offer.code(), true).await.unwrap();
    assert_eq!(got, e.address().await);
    // Restarted with under a minute left: it expires instead.
    let mut late = e.offer().await;
    e.clock.advance(9 * 60_000 + 30_000);
    e.restart.restart();
    assert_eq!(late.check(&e.client, &e.pin, &e.epochs).await.unwrap(), CodeState::Expired);
}

/// A relay from before the code routes, one with no mailboxes at all, and an
/// older token-gated relay (its access gate answers first): all "no routes",
/// so the device falls back to the pasted address.
#[tokio::test]
async fn a_relay_without_the_routes_reads_as_no_routes() {
    let e = env_with(false).await;
    assert_eq!(LinkCodeOffer::post(&e.client, e.ws, &e.pin, &e.epochs).await.unwrap_err(), MailboxError::NoRoutes);
    assert_eq!(resolve_link_code(&e.newcomer(), &ShortCode::generate(), true).await.unwrap_err(), MailboxError::NoRoutes);
    let bare = spawn(axum::Router::new().route("/v1/health", axum::routing::get(|| async { "ok" }))).await;
    assert_eq!(resolve_link_code(&MailboxClient::new(&bare), &ShortCode::generate(), true).await.unwrap_err(), MailboxError::NoRoutes);
    let gated = spawn(axum::Router::new().fallback(|| async { (axum::http::StatusCode::UNAUTHORIZED, "relay requires a valid access token") })).await;
    assert_eq!(resolve_link_code(&MailboxClient::new(&gated), &ShortCode::generate(), true).await.unwrap_err(), MailboxError::NoRoutes);
}

/// A relay that hands back someone else's blob, or names another mailbox
/// than the address inside, gets nothing past the resolver.
#[tokio::test]
async fn a_lying_relay_cant_substitute_an_address() {
    let e = env().await;
    let offer = e.offer().await;
    let real = e.address().await;
    let code = offer.code().clone();
    let relay_for = |mailbox: String, blob: String| {
        axum::Router::new().route(
            "/v1/link-codes/{code}",
            axum::routing::post(move || {
                let (m, b) = (mailbox.clone(), blob.clone());
                async move { axum::Json(serde_json::json!({ "mailbox": m, "blob": b, "expiresAt": 0 })) }
            }),
        )
    };
    // A blob sealed under the right code for this fake relay, naming the real
    // mailbox, but holding an address on another relay.
    for (mailbox, blob) in [
        (hex_mb(&[9; 16]), code.stretch(&e.base).seal(&real.mailbox, &real.encode())),
        (real.mailbox_hex(), "AAAA".to_string()),
    ] {
        let lying = spawn(relay_for(mailbox, blob)).await;
        let err = resolve_link_code(&MailboxClient::new(&lying), &code, true).await.unwrap_err();
        assert_eq!(err, MailboxError::Malformed);
    }
    // Sealed for the lying relay itself but naming an address on the real one.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lying = format!("http://{}", listener.local_addr().unwrap());
    let blob = code.stretch(&lying).seal(&real.mailbox, &real.encode());
    let router = relay_for(real.mailbox_hex(), blob);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    assert_eq!(resolve_link_code(&MailboxClient::new(&lying), &code, true).await.unwrap_err(), MailboxError::Malformed);
}

fn hex_mb(b: &[u8; 16]) -> String {
    ligus_core::match_code::hex(b)
}

#[test]
fn relay_forgot_is_a_lost_request_or_mailbox() {
    assert!(relay_forgot(&MailboxError::RequestUnknown));
    assert!(relay_forgot(&MailboxError::MailboxUnknown));
    assert!(!relay_forgot(&MailboxError::NoRoutes));
    assert!(!relay_forgot(&MailboxError::Unreachable("x".into())));
}

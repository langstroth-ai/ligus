//! The host's control connection to the relay (§6.2) and the accept
//! connections it dials per `open` (§6.3).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use super::session::{self, Mode, Timeouts};
use super::{b64u, b64u_fixed, TunnelHost, SUBPROTOCOL};

/// `hello` / `welcome` deadline after connecting (§10).
const HANDSHAKE: Duration = Duration::from_secs(10);
/// The relay pings every 20 s; a control connection silent this long is dead.
const SILENT: Duration = Duration::from_secs(60);
/// Backoff cap (§10), and the minimum wait after `superseded` (4009).
const MAX_BACKOFF: Duration = Duration::from_secs(60);

pub type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Why a control connection ended.
#[derive(Debug)]
enum End {
    /// Closed with a relay close code (§12).
    Code(u16, String),
    /// Our side stopped.
    Stopped,
    Other(anyhow::Error),
}

/// Dial `url` offering `hive-tunnel.1`; the relay must select it.
///
/// No message or frame may exceed the spec's 65 535 bytes (§5: text ≤ 8 KiB,
/// binary ≤ 65 535; §7.1: one Noise message per binary message), so the
/// relay can't make the host buffer tungstenite's defaults (64 MiB / 16 MiB).
pub async fn dial(url: &str) -> Result<Ws> {
    use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
    let mut req = url
        .into_client_request()
        .with_context(|| format!("bad relay URL {url}"))?;
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(SUBPROTOCOL),
    );
    let limit = Some(super::noise::MAX_MSG);
    let config = WebSocketConfig::default()
        .max_message_size(limit)
        .max_frame_size(limit);
    let (ws, resp) = tokio::time::timeout(
        HANDSHAKE,
        tokio_tungstenite::connect_async_tls_with_config(
            req,
            Some(config),
            false,
            Some(tls_connector()),
        ),
    )
    .await
    .map_err(|_| anyhow!("connecting to {url} timed out"))?
    .map_err(|e| match e {
        tokio_tungstenite::tungstenite::Error::Http(r) => {
            anyhow!("relay refused the upgrade: HTTP {}", r.status())
        }
        e => anyhow!("connecting to the relay: {e}"),
    })?;
    let proto = resp
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok());
    if proto != Some(SUBPROTOCOL) {
        bail!("relay did not select the {SUBPROTOCOL} subprotocol");
    }
    Ok(ws)
}

/// The TLS connector for `wss://` relay URLs: the same roots as Ligus's HTTP
/// clients (the OS trust store, or the bundled Mozilla roots when the OS has
/// none), so a relay reachable for sync is reachable for remote access too,
/// including behind a TLS-inspecting proxy whose root the OS trusts.
fn tls_connector() -> tokio_tungstenite::Connector {
    tokio_tungstenite::Connector::Rustls(Arc::new(ligus_forge::tls_client_config()))
}

/// ±20 % jitter.
fn jitter(d: Duration) -> Duration {
    let r = u16::from_le_bytes(super::random_bytes::<2>()) as f64 / u16::MAX as f64;
    d.mul_f64(0.8 + 0.4 * r)
}

/// What the control loop does after the relay closed the connection with a
/// close code (§12).
#[derive(Debug, Clone, PartialEq, Eq)]
enum After {
    /// Stop remote access, with the operator-facing reason.
    Stop(String),
    /// Reconnect after this long.
    Wait(Duration),
}

/// How long a host the relay doesn't serve (4006 `not_entitled`: pending
/// approval by its operator; 4003 `revoked`) waits between tries: either can
/// change on the relay's side, but not within seconds.
const NOT_ENTITLED_WAIT: Duration = Duration::from_secs(10 * 60);

/// The relay's close codes, classified. Retrying a code that can't change
/// by itself only hammers the relay: credentials it rejects (4001) and a
/// protocol version it doesn't speak (4026) stop remote access; a host it
/// doesn't serve (4006, and 4003 should a relay send it here) tries again
/// rarely;
/// another process on this host's key (4009) waits at least a minute;
/// a rate limit (4029) waits at least a minute (retrying sooner can't lift
/// it); anything else backs off.
fn after_close(code: u16, why: &str, backoff: Duration) -> After {
    match code {
        4001 => After::Stop(format!(
            "the relay rejected this host's credentials (4001 {why}); remote access is stopped. \
             Check `ligus remote status`, or run `ligus remote reset` if the host key changed."
        )),
        4026 => After::Stop(format!(
            "the relay doesn't speak this version of the remote-access protocol (4026 {why}); remote access is stopped. Update Ligus, or point it at a newer relay."
        )),
        4003 | 4006 => After::Wait(jitter(NOT_ENTITLED_WAIT).max(NOT_ENTITLED_WAIT)),
        4009 | 4029 => After::Wait(jitter(MAX_BACKOFF).max(MAX_BACKOFF)),
        _ => After::Wait(jitter(backoff)),
    }
}

/// Keep the control connection up until `stop`. Reconnects with backoff
/// 1 … 60 s (±20 %); waits ≥ 60 s after 4009 and ≥ 10 min after 4003 or
/// 4006; stops on 4001 and 4026 (§12, [`after_close`]).
pub async fn run(
    host: Arc<TunnelHost>,
    router: impl Into<session::Routers>,
    mut stop: watch::Receiver<bool>,
    t: Timeouts,
) {
    let router: session::Routers = router.into();
    let mut backoff = Duration::from_secs(1);
    loop {
        if *stop.borrow() {
            return;
        }
        let (end, welcomed) = connect_once(&host, &router, &mut stop, t).await;
        host.set_control(None);
        if welcomed {
            backoff = Duration::from_secs(1);
        }
        let wait = match &end {
            End::Stopped => return,
            End::Code(code, why) => {
                host.update_status(|s| s.last_error = Some(format!("{code} {why}")));
                match after_close(*code, why, backoff) {
                    After::Stop(msg) => {
                        eprintln!("remote: {msg}");
                        return;
                    }
                    After::Wait(wait) => {
                        match code {
                            4009 => eprintln!(
                                "remote: SUPERSEDED (4009): another process connected to the relay with this host's key. \
                                 Two `ligus serve`s share one data dir? Waiting {}s.",
                                wait.as_secs()
                            ),
                            4003 | 4006 => eprintln!(
                                "remote: the relay doesn't serve this host ({code} {why}: ask its operator); trying again in {} min",
                                wait.as_secs() / 60
                            ),
                            4029 => eprintln!(
                                "remote: the relay is rate limiting this network ({code} {why}). It counts failed attempts per \
                                 IP address (per /64 on IPv6), so a phone on the same network retrying an old pairing link \
                                 counts too; the block lifts a few minutes after the failures stop. Retrying in {}s.",
                                wait.as_secs()
                            ),
                            _ => eprintln!("remote: relay closed the control connection ({code} {why}); retrying in {}s", wait.as_secs()),
                        }
                        wait
                    }
                }
            }
            End::Other(e) => {
                eprintln!("remote: {e:#}; retrying in {}s", backoff.as_secs());
                host.update_status(|s| s.last_error = Some(format!("{e:#}")));
                jitter(backoff)
            }
        };
        backoff = (backoff * 2).min(MAX_BACKOFF);
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = super::stopped(&mut stop) => return,
        }
    }
}

fn close_code(m: &Message) -> Option<(u16, String)> {
    match m {
        Message::Close(Some(f)) => Some((u16::from(f.code), f.reason.to_string())),
        Message::Close(None) => Some((1005, String::new())),
        _ => None,
    }
}

/// The relay's `{"t":"error","code","message"}` frame as text, e.g.
/// `rate_limited: too many attempts`.
fn relay_error_text(m: &Value) -> String {
    let code = m["code"].as_str().unwrap_or("error");
    match m["message"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(msg) => format!("{code}: {msg}"),
        None => code.to_string(),
    }
}

/// The relay refused with an error frame (§6): it follows it with a close
/// frame whose code says what to do. Wait briefly for that code, so the
/// refusal is classified like any other close (and its text is logged).
async fn refusal(ws: &mut Ws, m: &Value) -> End {
    let text = relay_error_text(m);
    match tokio::time::timeout(Duration::from_secs(2), next_json(ws)).await {
        Ok(Err(End::Code(code, _))) => End::Code(code, text),
        _ => End::Other(anyhow!("the relay refused the control connection ({text})")),
    }
}

/// The relay's opening `challenge`, or the refusal it sent instead.
async fn read_challenge(ws: &mut Ws) -> Result<Value, End> {
    let ch = next_json(ws).await?;
    match ch["t"].as_str() {
        Some("challenge") => Ok(ch),
        Some("error") => Err(refusal(ws, &ch).await),
        _ => Err(End::Other(anyhow!("expected challenge, got {}", ch["t"]))),
    }
}

/// Read the next text message as JSON (pings are answered by tungstenite).
async fn next_json(ws: &mut Ws) -> Result<Value, End> {
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(t))) => {
                return serde_json::from_str(&t)
                    .map_err(|e| End::Other(anyhow!("relay sent bad JSON: {e}")));
            }
            Some(Ok(m)) => {
                if let Some((c, r)) = close_code(&m) {
                    return Err(End::Code(c, r));
                }
            }
            Some(Err(e)) => return Err(End::Other(anyhow!("control connection: {e}"))),
            None => return Err(End::Other(anyhow!("control connection closed"))),
        }
    }
}

async fn connect_once(
    host: &Arc<TunnelHost>,
    router: &session::Routers,
    stop: &mut watch::Receiver<bool>,
    t: Timeouts,
) -> (End, bool) {
    let url = host.settings.ws_url("/v1/tunnel/host");
    let mut ws = match dial(&url).await {
        Ok(ws) => ws,
        Err(e) => return (End::Other(e), false),
    };
    // challenge → hello → welcome
    let welcome = async {
        let ch = read_challenge(&mut ws).await?;
        if ch["v"] != 1 {
            return Err(End::Other(anyhow!(
                "relay speaks tunnel version {}",
                ch["v"]
            )));
        }
        let nonce: [u8; 32] =
            b64u_fixed(ch["nonce"].as_str().unwrap_or_default()).map_err(End::Other)?;
        let mut hello = json!({
            "t": "hello",
            "v": 1,
            "hostId": host.host_id(),
            "hostPub": b64u(host.keys.sign_pub().as_bytes()),
            "sig": b64u(host.keys.sign_challenge(&nonce)),
            "agent": format!("ligus-serve/{}", env!("CARGO_PKG_VERSION")),
        });
        if let Some(r) = &host.settings.reg_token {
            hello["reg"] = json!(r);
        }
        ws.send(Message::Text(hello.to_string().into()))
            .await
            .map_err(|e| End::Other(e.into()))?;
        loop {
            let m = next_json(&mut ws).await?;
            match m["t"].as_str() {
                Some("welcome") => return Ok(m),
                Some("error") => return Err(refusal(&mut ws, &m).await),
                _ => {}
            }
        }
    };
    let welcome = match tokio::time::timeout(HANDSHAKE, welcome).await {
        Ok(Ok(w)) => w,
        Ok(Err(e)) => return (e, false),
        Err(_) => return (End::Other(anyhow!("relay handshake timed out")), false),
    };
    let status = welcome["status"].as_str().unwrap_or("active").to_string();
    if let Some(n) = welcome["maxSessions"].as_u64() {
        host.max_sessions
            .store(n.clamp(1, 1024) as usize, Ordering::SeqCst);
    }
    let push = welcome["push"].as_bool().unwrap_or(false);
    host.push_available.store(push, Ordering::SeqCst);
    println!(
        "remote: connected to {} as host {} (status {status})",
        host.settings.relay,
        host.host_id()
    );
    if status == "pending" {
        println!("remote: the relay registered this host as pending: pairing works, sessions wait until the relay operator admits it.");
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    host.set_control(Some(tx));
    host.update_status(|s| {
        s.status = status.clone();
        s.push = push;
        s.last_error = None;
    });
    // Re-announce windows and unacked revocations on every connection.
    host.announce_windows();
    host.send_revocations();

    let session_stop = stop.clone();
    let end = loop {
        tokio::select! {
            m = tokio::time::timeout(SILENT, ws.next()) => {
                let m = match m {
                    Err(_) => break End::Other(anyhow!("relay silent for {}s", SILENT.as_secs())),
                    Ok(None) => break End::Other(anyhow!("control connection closed")),
                    Ok(Some(Err(e))) => break End::Other(anyhow!("control connection: {e}")),
                    Ok(Some(Ok(m))) => m,
                };
                if let Some((c, r)) = close_code(&m) {
                    break End::Code(c, r);
                }
                let Message::Text(text) = m else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                handle(host, router, &v, session_stop.clone(), t);
            }
            Some(out) = rx.recv() => {
                if let Err(e) = ws.send(Message::Text(out.into())).await {
                    break End::Other(anyhow!("control send: {e}"));
                }
            }
            _ = super::stopped(stop) => {
                let _ = ws.close(None).await;
                break End::Stopped;
            }
        }
    };
    (end, true)
}

fn handle(
    host: &Arc<TunnelHost>,
    router: &session::Routers,
    v: &Value,
    stop: watch::Receiver<bool>,
    t: Timeouts,
) {
    match v["t"].as_str() {
        Some("open") => {
            let Some(sid) = v["sid"].as_str().map(str::to_string) else {
                return;
            };
            let mode = match (v["mode"].as_str(), v["pid"].as_str(), v["eid"].as_str()) {
                (Some("session"), Some(pid), _) => Mode::Session {
                    pid: pid.to_string(),
                },
                (Some("enroll"), _, Some(eid)) => Mode::Enroll {
                    eid: eid.to_string(),
                },
                _ => {
                    reject(host, &sid);
                    return;
                }
            };
            // Cheap early refusals: unknown/revoked pid, closed window, capacity.
            let admissible = match &mode {
                Mode::Session { pid } => host.registry.is_active(pid),
                Mode::Enroll { eid } => host.window_psk(eid).is_some(),
            };
            if !admissible
                || host.sessions.load(Ordering::SeqCst) >= host.max_sessions.load(Ordering::SeqCst)
            {
                reject(host, &sid);
                return;
            }
            host.sessions.fetch_add(1, Ordering::SeqCst);
            let host = host.clone();
            let router = router.clone();
            tokio::spawn(async move {
                let url = host
                    .settings
                    .ws_url(&format!("/v1/tunnel/accept?sid={sid}"));
                let res = match dial(&url).await {
                    Ok(ws) => session::run(host.clone(), router, ws, mode.clone(), stop, t).await,
                    Err(e) => Err(e.context("dialing accept")),
                };
                host.sessions.fetch_sub(1, Ordering::SeqCst);
                if let Err(e) = res {
                    eprintln!("remote: session {mode:?} ended: {e:#}");
                }
            });
        }
        Some("status") => {
            let status = v["status"].as_str().unwrap_or_default().to_string();
            println!("remote: relay status for this host is now {status}");
            host.update_status(|s| s.status = status);
        }
        Some("ack") => {
            host.on_ack(
                v["id"].as_str().unwrap_or_default(),
                v["ok"].as_bool().unwrap_or(false),
                v["code"].as_str(),
                v["detail"].as_str(),
            );
        }
        Some("error") => eprintln!("remote: relay error: {} {}", v["code"], v["message"]),
        _ => {}
    }
}

fn reject(host: &TunnelHost, sid: &str) {
    host.send_plain(json!({ "t": "reject", "sid": sid, "code": "host_rejected" }));
}

#[cfg(test)]
mod tls_tests {
    /// The tunnel's WebSockets trust what relay HTTP trusts (it used to be
    /// the bundled Mozilla roots only, so sync worked behind a
    /// TLS-inspecting proxy and remote access did not).
    #[test]
    fn tunnel_tls_uses_the_http_root_policy() {
        let tokio_tungstenite::Connector::Rustls(cfg) = super::tls_connector() else {
            panic!("not rustls")
        };
        assert_eq!(
            format!("{:?}", cfg.as_ref()),
            format!("{:?}", ligus_forge::tls_client_config())
        );
        if ligus_forge::uses_platform_roots() {
            assert!(format!("{cfg:?}").contains("verifier: Verifier"));
        }
    }
}

#[cfg(test)]
mod close_code_tests {
    use super::*;

    /// A close code that can't change by itself stops remote access instead
    /// of being retried forever; a pending host tries again rarely.
    #[test]
    fn close_codes_stop_or_back_off_by_what_they_mean() {
        let b = Duration::from_secs(2);
        for code in [4001, 4026] {
            assert!(
                matches!(after_close(code, "x", b), After::Stop(_)),
                "{code} is retried"
            );
        }
        for code in [4003, 4006] {
            assert!(
                matches!(after_close(code, "x", b), After::Wait(w) if w >= NOT_ENTITLED_WAIT),
                "{code} retried at the reconnect pace"
            );
        }
        assert!(matches!(after_close(4009, "superseded", b), After::Wait(w) if w >= MAX_BACKOFF));
        assert!(
            matches!(after_close(4029, "rate_limited: too many attempts", b), After::Wait(w) if w >= MAX_BACKOFF)
        );
        assert!(matches!(after_close(4011, "shutting_down", b), After::Wait(w) if w <= b * 2));
    }
}

#[cfg(test)]
mod refusal_tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;

    /// A relay that answers the upgrade with `frames` and then closes with `close`.
    async fn relay_sending(frames: Vec<Value>, close: Option<(u16, &'static str)>) -> Ws {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            for f in frames {
                ws.send(Message::Text(f.to_string().into())).await.unwrap();
            }
            if let Some((code, reason)) = close {
                let _ = ws
                    .close(Some(CloseFrame {
                        code: CloseCode::from(code),
                        reason: reason.into(),
                    }))
                    .await;
            }
            while ws.next().await.is_some() {}
        });
        tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap()
            .0
    }

    /// The relay refusing the control connection (an error frame instead of
    /// the challenge) is reported with its code and message, not as
    /// `expected challenge, got "error"`, and is classified by its close code.
    #[tokio::test]
    async fn a_refused_control_connection_says_why() {
        let err = json!({ "t": "error", "code": "rate_limited", "message": "too many attempts" });
        let mut ws = relay_sending(vec![err.clone()], Some((4029, "rate_limited"))).await;
        match read_challenge(&mut ws).await {
            Err(End::Code(4029, why)) => assert_eq!(why, "rate_limited: too many attempts"),
            other => panic!("{other:?}"),
        }
        // No close frame: the refusal text still reaches the log.
        let mut ws = relay_sending(vec![err], None).await;
        match read_challenge(&mut ws).await {
            Err(End::Other(e)) => assert!(
                format!("{e:#}").contains("rate_limited: too many attempts"),
                "{e:#}"
            ),
            other => panic!("{other:?}"),
        }
        let mut ws = relay_sending(
            vec![json!({ "t": "challenge", "v": 1, "nonce": "x" })],
            None,
        )
        .await;
        assert!(matches!(read_challenge(&mut ws).await, Ok(v) if v["t"] == "challenge"));
    }

    #[test]
    fn relay_error_text_reads_code_and_message() {
        assert_eq!(
            relay_error_text(&json!({ "t": "error", "code": "not_entitled", "message": " " })),
            "not_entitled"
        );
        assert_eq!(relay_error_text(&json!({ "t": "error" })), "error");
    }
}

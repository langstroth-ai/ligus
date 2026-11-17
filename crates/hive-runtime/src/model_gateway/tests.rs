use super::*;
use std::io::{Read, Write};
use std::sync::Mutex;

/// What a fake gateway received: request heads and bodies.
#[derive(Default)]
pub(crate) struct Seen {
    pub heads: Vec<String>,
    pub bodies: Vec<Vec<u8>>,
}

/// A plain-HTTP fake gateway on 127.0.0.1 that records every request and
/// answers with an OpenAI-style streamed reply. Returns its port.
pub(crate) fn fake_gateway() -> (u16, Arc<Mutex<Seen>>) {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let s2 = seen.clone();
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut c) = conn else { continue };
            let seen = s2.clone();
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let head_end = loop {
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                    match c.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                let len: usize = head
                    .lines()
                    .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0)))
                    .unwrap_or(0);
                while buf.len() < head_end + len {
                    match c.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let body = buf[head_end..(head_end + len).min(buf.len())].to_vec();
                {
                    let mut s = seen.lock().unwrap();
                    s.heads.push(head);
                    s.bodies.push(body);
                }
                let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"pong from gateway\"}}]}\n\ndata: [DONE]\n\n";
                let _ = c.write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}", sse.len()).as_bytes(),
                );
            });
        }
    });
    (port, seen)
}

fn header<'a>(head: &'a str, name: &str) -> Vec<&'a str> {
    head.lines()
        .skip(1)
        .filter_map(|l| l.split_once(':'))
        .filter(|(k, _)| k.trim().eq_ignore_ascii_case(name))
        .map(|(_, v)| v.trim())
        .collect()
}

fn cfg(json: &str) -> Result<Vec<Gateway>, GatewayError> {
    ModelGateways::parse(json)
}

fn api_key_gateway(port: u16, key_file: &Path, attribution: bool) -> Gateway {
    Gateway::new(GatewayConfig {
        name: "test-gw".into(),
        url: format!("http://127.0.0.1:{port}"),
        api: Api::Openai,
        allow_plaintext: true,
        attribution,
        auth: AuthConfig::ApiKey { key_file: key_file.to_path_buf(), header: None },
        ca_file: None,
    })
    .unwrap()
}

#[test]
fn config_validation() {
    let ok = r#"{"gateways":[{"name":"litellm","url":"https://litellm.gw.svc:4000","api":"openai",
        "auth":{"mode":"serviceAccountToken","tokenFile":"/etc/ligus/model-gateways/litellm/token"}}]}"#;
    let g = cfg(ok).unwrap();
    assert_eq!(g[0].name(), "litellm");
    assert_eq!(g[0].chat_endpoint(), "https://litellm.gw.svc:4000/v1/chat/completions");
    let bad = [
        // plaintext without the opt-in
        r#"{"gateways":[{"name":"a","url":"http://gw:4000","auth":{"mode":"none"}}]}"#,
        // credentials in the URL
        r#"{"gateways":[{"name":"a","url":"https://u:p@gw","auth":{"mode":"none"}}]}"#,
        // mtls over http
        r#"{"gateways":[{"name":"a","url":"http://gw","allowPlaintext":true,"auth":{"mode":"mtls","certFile":"/c","clientKeyFile":"/k"}}]}"#,
        // metadata addresses
        r#"{"gateways":[{"name":"a","url":"https://169.254.169.254","auth":{"mode":"none"}}]}"#,
        r#"{"gateways":[{"name":"a","url":"https://[fd00:ec2::254]","auth":{"mode":"none"}}]}"#,
        // bad names, duplicates
        r#"{"gateways":[{"name":"A b","url":"https://gw","auth":{"mode":"none"}}]}"#,
        r#"{"gateways":[{"name":"-a","url":"https://gw","auth":{"mode":"none"}}]}"#,
        r#"{"gateways":[{"name":"a","url":"https://gw","auth":{"mode":"none"}},{"name":"a","url":"https://gw2","auth":{"mode":"none"}}]}"#,
        // unknown field, unknown mode, query
        r#"{"gateways":[{"name":"a","url":"https://gw","auth":{"mode":"none"},"extra":1}]}"#,
        r#"{"gateways":[{"name":"a","url":"https://gw","auth":{"mode":"oauth"}}]}"#,
        r#"{"gateways":[{"name":"a","url":"https://gw?x=1","auth":{"mode":"none"}}]}"#,
    ];
    for b in bad {
        assert!(cfg(b).is_err(), "accepted: {b}");
    }
}

#[test]
fn routes_named_covered_direct_and_exclusive() {
    let gws = cfg(r#"{"gateways":[{"name":"litellm","url":"https://litellm.gw.svc:4000/","auth":{"mode":"none"}},
        {"name":"claude-gw","url":"https://ai.example.com/anthropic","api":"anthropic","auth":{"mode":"none"}}]}"#)
    .unwrap();
    let excl = ModelGateways::new(gws, true, None);
    let open = ModelGateways::new(cfg(r#"{"gateways":[{"name":"litellm","url":"https://litellm.gw.svc:4000","auth":{"mode":"none"}}]}"#).unwrap(), false, None);
    let sel = |n: &str| GatewaySelection::named(n);
    let r = excl.route(ModelProviderKind::Custom, "", Some(&sel("litellm"))).unwrap().unwrap();
    assert_eq!(r.gateway.name(), "litellm");
    // Named but not offered here: refused on every host.
    assert!(excl.route(ModelProviderKind::Custom, "", Some(&sel("other"))).unwrap_err().contains("doesn't offer"));
    assert!(ModelGateways::default().route(ModelProviderKind::Custom, "", Some(&sel("litellm"))).is_err());
    // An endpoint on a listed gateway goes through it (credential injected).
    let r = excl.route(ModelProviderKind::Custom, "https://LITELLM.gw.svc:4000/v1/chat/completions", None).unwrap().unwrap();
    assert_eq!(r.gateway.name(), "litellm");
    let r = excl.route(ModelProviderKind::Anthropic, "https://ai.example.com/anthropic/v1/messages", None).unwrap().unwrap();
    assert_eq!(r.gateway.name(), "claude-gw");
    // Lookalikes are not on the gateway.
    for e in [
        "https://litellm.gw.svc:4001/v1/chat/completions",
        "http://litellm.gw.svc:4000/v1/chat/completions",
        "https://ai.example.com/anthropic-evil/v1/messages",
        "https://ai.example.com.evil.net/anthropic/v1/messages",
        "https://user@litellm.gw.svc:4000/v1",
    ] {
        assert!(excl.route(ModelProviderKind::Custom, e, None).is_err(), "{e} counted as the gateway");
    }
    // An unlisted endpoint: refused on an exclusive host, direct otherwise.
    assert!(excl.route(ModelProviderKind::OpenAI, "https://api.openai.com/v1/chat/completions", None).unwrap_err().contains("only through"));
    assert!(excl.route(ModelProviderKind::ClaudeCode, "", None).is_err(), "a CLI's vendor endpoint on an exclusive host");
    assert!(open.route(ModelProviderKind::OpenAI, "https://api.openai.com/v1/chat/completions", None).unwrap().is_none());
    assert!(ModelGateways::default().route(ModelProviderKind::ClaudeCode, "", None).unwrap().is_none());
    // CLIs that can't take a base URL, or speak the other wire.
    assert!(excl.route(ModelProviderKind::Copilot, "", Some(&sel("litellm"))).is_err());
    assert!(excl.route(ModelProviderKind::ClaudeCode, "", Some(&sel("litellm"))).is_err(), "claude speaks the anthropic wire");
    assert!(excl.route(ModelProviderKind::ClaudeCode, "", Some(&sel("claude-gw"))).is_ok());
    assert!(excl.route(ModelProviderKind::Codex, "", Some(&sel("litellm"))).is_ok());
    assert_eq!(
        excl.offers().iter().map(|o| (o.name.as_str(), o.api.as_str())).collect::<Vec<_>>(),
        vec![("claude-gw", "anthropic"), ("litellm", "openai")]
    );
}

#[test]
fn token_rotation_is_picked_up_per_request() {
    let dir = tempfile::tempdir().unwrap();
    let tok = dir.path().join("token");
    std::fs::write(&tok, "jwt-one\n").unwrap();
    let g = Gateway::new(GatewayConfig {
        name: "sa".into(),
        url: "https://gw.example".into(),
        api: Api::Openai,
        allow_plaintext: false,
        attribution: false,
        auth: AuthConfig::ServiceAccountToken { token_file: tok.clone() },
        ca_file: None,
    })
    .unwrap();
    assert_eq!(g.auth_headers().unwrap()[0].1.as_str(), "Bearer jwt-one");
    // The kubelet swaps the file atomically; the next request sends the new one.
    std::fs::write(dir.path().join("token.new"), "jwt-two").unwrap();
    std::fs::rename(dir.path().join("token.new"), &tok).unwrap();
    assert_eq!(g.auth_headers().unwrap()[0].1.as_str(), "Bearer jwt-two");
    std::fs::write(&tok, "has space").unwrap();
    assert!(g.auth_headers().is_err(), "a malformed token is refused, not sent");
    std::fs::remove_file(&tok).unwrap();
    let e = g.auth_headers().unwrap_err().to_string();
    assert!(e.contains("ServiceAccount token") && !e.contains("jwt"), "{e}");
    let key = dir.path().join("k");
    std::fs::write(&key, "sk-gw").unwrap();
    for (h, want) in [(None, ("authorization", "Bearer sk-gw")), (Some("x-api-key"), ("x-api-key", "sk-gw")), (Some("api-key"), ("api-key", "sk-gw"))] {
        let g = Gateway::new(GatewayConfig {
            name: "k".into(),
            url: "https://gw.example".into(),
            api: Api::Anthropic,
            allow_plaintext: false,
            attribution: false,
            auth: AuthConfig::ApiKey { key_file: key.clone(), header: h.map(Into::into) },
            ca_file: None,
        })
        .unwrap();
        let hs = g.auth_headers().unwrap();
        assert_eq!((hs[0].0, hs[0].1.as_str()), want);
    }
}

#[test]
fn attribution_is_pseudonymous() {
    let w = Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888);
    let a = Uuid::from_u128(0x9999);
    let k1 = [7u8; 32];
    let k2 = [8u8; 32];
    let at = Attribution::new(&k1, Some(w), Some(a));
    let (ws, ag) = (at.workspace.clone().unwrap(), at.agent.clone().unwrap());
    assert_eq!(ws.len(), 32);
    assert!(ws.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(ws, pseudonym(&k1, b"w", w), "stable");
    assert_ne!(ws, pseudonym(&k2, b"w", w), "keyed");
    assert_ne!(pseudonym(&k1, b"w", a), ag, "workspace and agent ids are separated");
    for v in [&ws, &ag] {
        assert!(!v.contains(&w.simple().to_string()[..8]) && !v.contains(&a.simple().to_string()[24..]));
    }
    assert_eq!(pseudonym(&[0u8; 32], b"w", Uuid::nil()), pseudonym_vector());
    assert_eq!(ATTRIBUTION_CONTEXT, b"ligus-gateway-attribution-v1\0", "frozen once released (phase0-specs 0.1)");
}

/// The attribution vector, computed independently of `pseudonym` (HMAC by
/// hand over SHA-256), so a change to either side shows.
fn pseudonym_vector() -> String {
    use sha2::{Digest, Sha256};
    let key = [0u8; 64];
    let ipad: Vec<u8> = key.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = key.iter().map(|b| b ^ 0x5c).collect();
    let mut msg = ATTRIBUTION_CONTEXT.to_vec();
    msg.extend(b"w");
    msg.push(0);
    msg.extend([0u8; 16]);
    let inner = Sha256::digest([ipad, msg].concat());
    let outer = Sha256::digest([opad, inner.to_vec()].concat());
    outer[..16].iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn client_credential_headers_and_forbidden_addresses() {
    for h in ["Authorization", "proxy-authorization", "X-Api-Key", "api-key", "Cookie", "X-Ligus-Agent", "x-ligus-workspace"] {
        assert!(is_client_credential_header(h), "{h}");
    }
    for h in ["content-type", "anthropic-version", "x-request-id"] {
        assert!(!is_client_credential_header(h), "{h}");
    }
    assert!(addr_forbidden("169.254.169.254".parse().unwrap(), true));
    assert!(addr_forbidden("100.100.100.200".parse().unwrap(), true));
    assert!(addr_forbidden("::ffff:169.254.169.254".parse().unwrap(), true));
    assert!(addr_forbidden("127.0.0.1".parse().unwrap(), false));
    assert!(!addr_forbidden("127.0.0.1".parse().unwrap(), true));
    assert!(!addr_forbidden("10.96.0.10".parse().unwrap(), false));
}

async fn raw_request(port: u16, raw: String) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(raw.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), s.read_to_end(&mut out)).await;
    String::from_utf8_lossy(&out).into_owned()
}

/// The proxy's gateway route: the client's credentials are stripped, the
/// gateway's (and the attribution ids) are added, the body and answer pass
/// through, and a wrong token or unknown gateway is refused.
#[tokio::test]
async fn gateway_route_injects_and_strips() {
    let (port, seen) = fake_gateway();
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("api-key");
    std::fs::write(&key, "sk-REAL-GATEWAY-KEY").unwrap();
    let g = Arc::new(api_key_gateway(port, &key, true));
    let w = Uuid::from_u128(42);
    let a = Uuid::from_u128(43);
    let attr = Attribution::new(&[1u8; 32], Some(w), Some(a));
    let proxy = crate::egress::EgressProxy::start_with(Vec::new(), None, vec![ResolvedGateway { gateway: g, attribution: Some(attr.clone()) }]).unwrap();
    let pp = proxy.port();
    let base = proxy.gateway_url("test-gw");
    let path = base.strip_prefix(&format!("http://127.0.0.1:{pp}")).unwrap().to_string();

    let body = r#"{"model":"m","messages":[]}"#;
    let raw = format!(
        "POST {path}/v1/chat/completions?x=1 HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer FORGED-BY-CHILD\r\nX-Api-Key: FORGED2\r\nX-Ligus-Agent: forged-agent\r\nCookie: c=1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let resp = raw_request(pp, raw).await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    assert!(resp.contains("pong from gateway"), "{resp}");
    {
        let s = seen.lock().unwrap();
        let head = &s.heads[0];
        assert!(head.starts_with("POST /v1/chat/completions?x=1 "), "{head}");
        assert_eq!(header(head, "authorization"), vec!["Bearer sk-REAL-GATEWAY-KEY"], "{head}");
        assert!(header(head, "x-api-key").is_empty() && header(head, "cookie").is_empty(), "{head}");
        assert_eq!(header(head, "x-ligus-agent"), vec![attr.agent.as_deref().unwrap()]);
        assert_eq!(header(head, "x-ligus-workspace"), vec![attr.workspace.as_deref().unwrap()]);
        assert!(!head.contains(&w.to_string()) && !head.contains("FORGED") && !head.contains("forged"), "{head}");
        assert_eq!(s.bodies[0], body.as_bytes());
    }
    // Chunked request bodies are decoded and forwarded whole.
    let raw = format!("POST {path}/v1/chat/completions HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n");
    assert!(raw_request(pp, raw).await.starts_with("HTTP/1.1 200"));
    assert_eq!(seen.lock().unwrap().bodies[1], b"hello world");
    // A wrong token, or a gateway the turn doesn't have: refused, nothing forwarded.
    let wrong = path.replacen(proxy.token(), "0000", 1);
    assert!(raw_request(pp, format!("GET {wrong}/v1/models HTTP/1.1\r\n\r\n")).await.starts_with("HTTP/1.1 403"));
    let other = path.replace("/test-gw", "/other-gw");
    assert!(raw_request(pp, format!("GET {other}/v1/models HTTP/1.1\r\n\r\n")).await.starts_with("HTTP/1.1 404"));
    assert_eq!(seen.lock().unwrap().heads.len(), 2);
}

/// A hosted-API turn (Ligus makes the call) sends the same credential, never
/// the runtime's own key, to the gateway's chat endpoint.
#[tokio::test]
async fn hosted_turn_uses_the_gateway_credential() {
    let (port, seen) = fake_gateway();
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("api-key");
    std::fs::write(&key, "sk-REAL-GATEWAY-KEY").unwrap();
    let g = Arc::new(api_key_gateway(port, &key, false));
    let http = g.client().await.unwrap();
    let client = crate::provider::OpenAiClient::new(g.chat_endpoint()).with_gateway(g.clone(), None, http);
    let out = client
        .stream_reply(Some("sk-RUNTIME-OWN-KEY"), "m", None, &[crate::provider::ChatTurn::user("ping")], |_| {})
        .await
        .unwrap();
    assert_eq!(out, "pong from gateway");
    let s = seen.lock().unwrap();
    let head = &s.heads[0];
    assert!(head.starts_with("POST /v1/chat/completions "), "{head}");
    assert_eq!(header(head, "authorization"), vec!["Bearer sk-REAL-GATEWAY-KEY"]);
    assert!(!head.contains("RUNTIME-OWN-KEY"), "{head}");
}

/// Item 4: a sandboxed CLI child reaches the model only through the route;
/// the credential is absent from its env, argv and readable files, a
/// credential it forges is replaced, and it can't reach the gateway directly.
#[test]
fn gateway_credential_absent_from_child() {
    use crate::executor::{self, Purpose, RuntimeScope, SpawnSpec};
    let Some(e) = executor::tests::real_executor() else { return };
    if !std::path::Path::new("/usr/bin/curl").exists() {
        eprintln!("SKIPPED gateway_credential_absent_from_child: no /usr/bin/curl");
        return;
    }
    let f = executor::tests::Fixture::new();
    let (port, seen) = fake_gateway();
    let secrets = f.outside.join("gw-secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    let key = secrets.join("api-key");
    std::fs::write(&key, "sk-CHILD-MUST-NOT-SEE").unwrap();
    let name = format!("child-gw-{port}");
    let g = Gateway::new(GatewayConfig {
        name: name.clone(),
        url: format!("http://127.0.0.1:{port}"),
        api: Api::Openai,
        allow_plaintext: true,
        attribution: false,
        auth: AuthConfig::ApiKey { key_file: key.clone(), header: None },
        ca_file: None,
    })
    .unwrap();
    // Not exclusive: other tests in this process keep their direct routes.
    install(Arc::new(ModelGateways::new(vec![g], false, None)));
    let script = format!(
        "env; echo ARGV:$0 \"$@\"; cat '{key}' && echo READ-OK; \
         /usr/bin/curl -s -m 10 -H 'Authorization: Bearer FORGED' -H 'Content-Type: application/json' -d '{{\"model\":\"m\"}}' \"$OPENAI_BASE_URL/chat/completions\"; echo; \
         /usr/bin/curl -s -m 5 http://127.0.0.1:{port}/direct && echo DIRECT-OK",
        key = key.display()
    );
    let rt = RuntimeScope { gateway: Some(GatewaySelection::named(&name)), ..Default::default() };
    let out = executor::with_runtime_sync(rt, || {
        let policy = executor::TurnPolicy { worktree_is_isolated: true, ..executor::TurnPolicy::baseline(executor::HostKind::Serve) };
        executor::with_turn_sync(policy, || {
            let spec = SpawnSpec::new(Purpose::Agent(ModelProviderKind::Hermes), "/bin/sh", ["-c", script.as_str()]).cwd(Some(&f.worktree));
            let mut p = executor::prepare_with(&*e, spec).expect("prepare");
            let argv = format!("{:?}", p.command);
            assert!(!argv.contains("CHILD-MUST-NOT-SEE"), "credential in the command: {argv}");
            let o = p.command.output().expect("spawn");
            drop(p.guard);
            format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
        })
    });
    assert!(!out.contains("CHILD-MUST-NOT-SEE") && !out.contains("READ-OK"), "the child saw the credential: {out}");
    assert!(out.contains(&format!("OPENAI_API_KEY={DUMMY_KEY}")), "{out}");
    assert!(out.contains("/ligus-gw/"), "base URL is the proxy route: {out}");
    assert!(out.contains("pong from gateway"), "the turn reached the gateway through the route: {out}");
    assert!(!out.contains("DIRECT-OK"), "the child reached the gateway directly: {out}");
    let s = seen.lock().unwrap();
    assert_eq!(s.heads.len(), 1, "only the routed request arrived: {:?}", s.heads);
    assert_eq!(header(&s.heads[0], "authorization"), vec!["Bearer sk-CHILD-MUST-NOT-SEE"]);
    assert!(!s.heads[0].contains("FORGED"));
}

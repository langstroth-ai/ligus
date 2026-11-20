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
    // Only the model API: the gateway's admin routes (LiteLLM's key minting,
    // say) never get the credential, and no path climbs out of a prefix.
    for (m, bad) in [
        ("POST", "/key/generate"),
        ("GET", "/v1/chat/completions/../../key/list"),
        ("POST", "/v1/chat/%2e%2e/%2e%2e/key/generate"),
        ("POST", "/v1//chat/completions"),
        ("DELETE", "/v1/models/x"),
        ("POST", "/v1/messages"),
    ] {
        let resp = raw_request(pp, format!("{m} {path}{bad} HTTP/1.1\r\nContent-Length: 0\r\n\r\n")).await;
        assert!(resp.starts_with("HTTP/1.1 403"), "{m} {bad}: {resp}");
    }
    assert_eq!(seen.lock().unwrap().heads.len(), 2);
}

#[test]
fn route_paths_are_the_model_api_only() {
    for (api, m, p) in [
        (Api::Openai, "POST", "/v1/chat/completions"),
        (Api::Openai, "POST", "/v1/chat/completions?x=1"),
        (Api::Openai, "POST", "/v1/responses"),
        (Api::Openai, "GET", "/v1/models"),
        (Api::Anthropic, "POST", "/v1/messages?beta=true"),
        (Api::Anthropic, "POST", "/v1/messages/count_tokens"),
    ] {
        assert!(route_path_allowed(api, m, p), "{m} {p}");
    }
    for (api, m, p) in [
        (Api::Openai, "POST", "/v1/messages"),
        (Api::Anthropic, "POST", "/v1/chat/completions"),
        (Api::Openai, "POST", "/key/generate"),
        (Api::Openai, "GET", "/v1/chat/completions"),
        (Api::Openai, "POST", "/v1/chat/completions/../../key/generate"),
        (Api::Openai, "POST", "/v1/%2E%2E/key"),
        (Api::Openai, "POST", "v1/chat/completions"),
    ] {
        assert!(!route_path_allowed(api, m, p), "{m} {p}");
    }
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

/// A server that answers every request with `307` to `location` and counts
/// the requests it got.
fn redirecting_server(location: String) -> (u16, Arc<Mutex<usize>>) {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = l.local_addr().unwrap().port();
    let hits = Arc::new(Mutex::new(0usize));
    let h2 = hits.clone();
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut c) = conn else { continue };
            *h2.lock().unwrap() += 1;
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                match c.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let _ = c.write_all(format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes());
        }
    });
    (port, hits)
}

/// A gateway that answers with a redirect never gets its credential carried
/// to the redirect target: not through the proxy route, not from a hosted
/// turn. `x-api-key` and `api-key` are not among the headers reqwest drops on
/// a cross-origin redirect, and the target is never vetted (it could be a
/// metadata address or plain HTTP).
#[tokio::test]
async fn gateway_redirects_are_not_followed() {
    let (elsewhere, seen) = fake_gateway();
    let (gw_port, hits) = redirecting_server(format!("http://127.0.0.1:{elsewhere}/v1/chat/completions"));
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("api-key");
    std::fs::write(&key, "sk-MUST-STAY-ON-THE-GATEWAY").unwrap();
    let g = Arc::new(
        Gateway::new(GatewayConfig {
            name: "redir-gw".into(),
            url: format!("http://127.0.0.1:{gw_port}"),
            api: Api::Openai,
            allow_plaintext: true,
            attribution: false,
            auth: AuthConfig::ApiKey { key_file: key.clone(), header: Some("x-api-key".into()) },
            ca_file: None,
        })
        .unwrap(),
    );
    // Through the proxy route.
    let proxy = crate::egress::EgressProxy::start_with(Vec::new(), None, vec![ResolvedGateway { gateway: g.clone(), attribution: None }]).unwrap();
    let pp = proxy.port();
    let path = proxy.gateway_url("redir-gw").strip_prefix(&format!("http://127.0.0.1:{pp}")).unwrap().to_string();
    let body = r#"{"model":"m","messages":[]}"#;
    let resp = raw_request(pp, format!("POST {path}/v1/chat/completions HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}", body.len())).await;
    assert!(resp.starts_with("HTTP/1.1 307"), "the redirect goes back to the client, unfollowed: {resp}");
    // A hosted-API turn.
    let http = g.client().await.unwrap();
    let client = crate::provider::OpenAiClient::new(g.chat_endpoint()).with_gateway(g.clone(), None, http);
    let _ = client.stream_reply(None, "m", None, &[crate::provider::ChatTurn::user("ping")], |_| {}).await;
    assert!(*hits.lock().unwrap() >= 2, "both requests reached the gateway");
    let s = seen.lock().unwrap();
    assert!(s.heads.is_empty(), "a redirect was followed with the gateway's credential: {:?}", s.heads);
}

/// An HTTP runtime whose endpoint is on a gateway goes through it only for
/// the gateway wire's model API: an endpoint on the gateway's admin routes
/// (LiteLLM's key minting) would otherwise get the injected credential, and
/// the reply would come back to whoever set the endpoint.
#[test]
fn covered_endpoint_must_be_the_model_api() {
    let gws = ModelGateways::new(
        cfg(r#"{"gateways":[{"name":"litellm","url":"https://litellm.gw.svc:4000","auth":{"mode":"none"}},
            {"name":"claude-gw","url":"https://ai.example.com/anthropic","api":"anthropic","auth":{"mode":"none"}}]}"#)
        .unwrap(),
        false,
        None,
    );
    for (p, e) in [
        (ModelProviderKind::Custom, "https://litellm.gw.svc:4000/key/generate"),
        (ModelProviderKind::OpenAI, "https://litellm.gw.svc:4000/user/new"),
        (ModelProviderKind::Custom, "https://litellm.gw.svc:4000/v1/chat/completions/../../key/generate"),
        (ModelProviderKind::Custom, "https://litellm.gw.svc:4000/v1/%2e%2e/key/generate"),
        (ModelProviderKind::Custom, "https://litellm.gw.svc:4000"),
        (ModelProviderKind::Anthropic, "https://ai.example.com/anthropic/admin/keys"),
        (ModelProviderKind::Anthropic, "https://litellm.gw.svc:4000/v1/chat/completions"),
    ] {
        assert!(gws.route(p, e, None).is_err(), "{p:?} {e} got the gateway's credential");
    }
    assert!(gws.route(ModelProviderKind::Custom, "https://litellm.gw.svc:4000/v1/chat/completions", None).unwrap().is_some());
    assert!(gws.route(ModelProviderKind::Anthropic, "https://ai.example.com/anthropic/v1/messages", None).unwrap().is_some());
}

/// On a gateway-exclusive host, the `/props` context probe of a Custom
/// runtime must not reach an endpoint that isn't a gateway (with the
/// runtime's own key) before the turn is refused.
#[tokio::test]
async fn context_probe_respects_the_gateway_route() {
    let l = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = l.local_addr().unwrap().port();
    let excl = ModelGateways::new(cfg(r#"{"gateways":[{"name":"litellm","url":"https://litellm.gw.svc:4000","auth":{"mode":"none"}}]}"#).unwrap(), true, None);
    let rt = crate::provider::ResolvedRuntime {
        provider: ModelProviderKind::Custom,
        model: "m".into(),
        endpoint: format!("http://127.0.0.1:{port}/v1/chat/completions"),
        api_key: Some("sk-RUNTIME".into()),
        args: Vec::new(),
        model_provider_id: None,
        model_base_url: None,
        context_window_tokens: None,
        keep_alive: None,
        think: None,
        tools: false,
        max_tool_rounds: None,
        allow_remote_file_tools: false,
        driver: Default::default(),
        gateway: None,
    };
    crate::provider::dispatch::warm_context_probe_on(&excl, &rt).await;
    let got = tokio::time::timeout(std::time::Duration::from_millis(300), l.accept()).await;
    assert!(got.is_err(), "the probe connected to an unlisted endpoint on an exclusive host");
}

/// `openssl` for the TLS tests (skipped without it).
fn openssl() -> Option<&'static str> {
    ["/opt/homebrew/bin/openssl", "/usr/bin/openssl", "/usr/local/bin/openssl"].into_iter().find(|p| Path::new(p).exists())
}

fn run_openssl(bin: &str, dir: &Path, args: &[&str]) {
    let o = std::process::Command::new(bin).current_dir(dir).args(args).output().unwrap();
    assert!(o.status.success(), "openssl {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

/// A CA (`<ca>.crt`/`<ca>.key`) and a `localhost` server cert it signed
/// (`<ca>-srv.crt`/`<ca>-srv.key`), all decoys in `dir`.
fn ca_and_server_cert(bin: &str, dir: &Path, ca: &str) {
    run_openssl(bin, dir, &["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2", "-subj", &format!("/CN={ca}"), "-keyout", &format!("{ca}.key"), "-out", &format!("{ca}.crt")]);
    run_openssl(bin, dir, &["req", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=localhost", "-keyout", &format!("{ca}-srv.key"), "-out", &format!("{ca}-srv.csr")]);
    std::fs::write(dir.join("ext.cnf"), "subjectAltName=DNS:localhost\nbasicConstraints=CA:FALSE\n").unwrap();
    run_openssl(bin, dir, &["x509", "-req", "-in", &format!("{ca}-srv.csr"), "-CA", &format!("{ca}.crt"), "-CAkey", &format!("{ca}.key"), "-CAcreateserial", "-days", "2", "-extfile", "ext.cnf", "-out", &format!("{ca}-srv.crt")]);
}

/// A TLS server (`openssl s_server -www`) on localhost with `cert`/`key`.
struct TlsServer(std::process::Child, u16);
impl Drop for TlsServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn tls_server(bin: &str, dir: &Path, cert: &str, key: &str) -> TlsServer {
    let port = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port();
    let child = std::process::Command::new(bin)
        .current_dir(dir)
        .args(["s_server", "-quiet", "-www", "-accept", &port.to_string(), "-cert", cert, "-key", key])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    TlsServer(child, port)
}

/// The pinned CA holds whichever roots the host has: on a host without
/// system CA certificates (Ligus then preconfigures reqwest with its bundled
/// Mozilla roots, which overrides every other TLS setting) a gateway with a
/// `caFile` must still trust exactly that CA, and nothing else.
#[tokio::test]
async fn pinned_ca_holds_without_system_roots() {
    let Some(bin) = openssl() else {
        eprintln!("SKIPPED pinned_ca_holds_without_system_roots: no openssl");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    ca_and_server_cert(bin, dir.path(), "ca-a");
    ca_and_server_cert(bin, dir.path(), "ca-b");
    let gw = |port: u16| {
        Gateway::new(GatewayConfig {
            name: "pinned".into(),
            url: format!("https://localhost:{port}"),
            api: Api::Openai,
            allow_plaintext: false,
            attribution: false,
            auth: AuthConfig::None,
            ca_file: Some(dir.path().join("ca-a.crt")),
        })
        .unwrap()
    };
    let good = tls_server(bin, dir.path(), "ca-a-srv.crt", "ca-a-srv.key");
    let bad = tls_server(bin, dir.path(), "ca-b-srv.crt", "ca-b-srv.key");
    for platform_roots in [true, false] {
        let g = gw(good.1);
        let c = g.client_with_roots(platform_roots).await.unwrap();
        let r = c.get(g.join("/")).send().await;
        assert!(r.is_ok(), "platform_roots={platform_roots}: the pinned CA wasn't trusted: {r:?}");
        let g = gw(bad.1);
        let c = g.client_with_roots(platform_roots).await.unwrap();
        assert!(c.get(g.join("/")).send().await.is_err(), "platform_roots={platform_roots}: a cert from another CA was accepted");
    }
    // An mTLS identity would be dropped the same way: without a pinned CA
    // on such a host the gateway is refused, not dialed without it.
    let m = Gateway::new(GatewayConfig {
        name: "mtls".into(),
        url: format!("https://localhost:{}", good.1),
        api: Api::Openai,
        allow_plaintext: false,
        attribution: false,
        auth: AuthConfig::Mtls { cert_file: dir.path().join("ca-a-srv.crt"), client_key_file: dir.path().join("ca-a-srv.key") },
        ca_file: None,
    })
    .unwrap();
    assert!(m.client_with_roots(false).await.is_err());
}

/// The exclusive host's one exemption, a local Ligus daemon, is a loopback
/// endpoint: a `ligus-daemon` runtime pointed anywhere else is model traffic
/// leaving the host like any other and is refused.
#[test]
fn ligus_daemon_is_exempt_only_on_loopback() {
    let excl = ModelGateways::new(cfg(r#"{"gateways":[{"name":"litellm","url":"https://litellm.gw.svc:4000","auth":{"mode":"none"}}]}"#).unwrap(), true, None);
    for e in ["https://attacker.example/v1/chat/completions", "http://10.0.0.5:8080/v1/chat/completions", "http://localhost.attacker.example/v1", ""] {
        assert!(excl.route(ModelProviderKind::LigusDaemon, e, None).is_err(), "ligus-daemon at {e:?} went around the gateways");
    }
    for e in ["http://127.0.0.1:8080/v1/chat/completions", "http://localhost:8080/v1/chat/completions", "http://[::1]:8080/v1/chat/completions"] {
        assert!(excl.route(ModelProviderKind::LigusDaemon, e, None).unwrap().is_none(), "{e}");
    }
}

/// Spoofable gateway identity, budget and routing headers a sandboxed agent
/// might send: none of them reaches a gateway.
const SPOOFED: &[&str] = &[
    "x-litellm-customer-id",
    "x-litellm-api-key",
    "x-litellm-tags",
    "x-litellm-end-user-id",
    "x-litellm-timeout",
    "x-envoy-original-path",
    "x-envoy-max-retries",
    "x-envoy-upstream-rq-timeout-ms",
    "x-envoy-external-address",
    "x-ai-eg-model",
    "x-gateway-model-name",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-forwarded-client-cert",
    "forwarded",
    "x-real-ip",
    "x-original-url",
    "x-request-id",
    "x-user-id",
    "x-budget-id",
    "x-portkey-api-key",
    "helicone-auth",
    "traceparent",
    "openai-organization",
    "openai-project",
    "anthropic-dangerous-direct-browser-access",
    "x-stainless-lang",
    "x-app",
    "x-ligus-agent",
    "x-ligus-workspace",
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "api-key",
    "cookie",
];

/// The gateway route forwards an allow-list per wire: the content and
/// version headers the wire's model API needs, never anything else.
#[test]
fn request_header_allow_list() {
    for api in [Api::Openai, Api::Anthropic] {
        for h in ["content-type", "Content-Type", "accept", "accept-encoding", "user-agent", "idempotency-key"] {
            assert!(forwardable_request_header(api, h), "{api:?} {h}");
        }
        for h in SPOOFED {
            assert!(!forwardable_request_header(api, h), "{api:?} forwards {h}");
        }
    }
    for h in ["anthropic-version", "anthropic-beta", "Anthropic-Beta"] {
        assert!(forwardable_request_header(Api::Anthropic, h), "{h}");
        assert!(!forwardable_request_header(Api::Openai, h), "{h} on the OpenAI wire");
    }
    assert!(forwardable_request_header(Api::Openai, "OpenAI-Beta"));
    assert!(!forwardable_request_header(Api::Anthropic, "openai-beta"));
    // The allow-list never names a credential or attribution header.
    for api in [Api::Openai, Api::Anthropic] {
        for h in request_header_allow_list_of(api) {
            assert!(!is_client_credential_header(h), "{h}");
        }
    }
}

/// A recorded request head (fixtures/*.http) as the CLI sent it, through the
/// proxy route to a fake gateway, with every spoofed header added. Returns
/// what the gateway received.
async fn replay_fixture(api: Api, fixture: &str) -> (String, Attribution) {
    let (port, seen) = fake_gateway();
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("api-key");
    std::fs::write(&key, "sk-REAL-GATEWAY-KEY").unwrap();
    let mut g = api_key_gateway(port, &key, true);
    g.cfg.api = api;
    let attr = Attribution::new(&[2u8; 32], Some(Uuid::from_u128(7)), Some(Uuid::from_u128(8)));
    let proxy = crate::egress::EgressProxy::start_with(Vec::new(), None, vec![ResolvedGateway { gateway: Arc::new(g), attribution: Some(attr.clone()) }]).unwrap();
    let pp = proxy.port();
    let path = proxy.gateway_url("test-gw").strip_prefix(&format!("http://127.0.0.1:{pp}")).unwrap().to_string();
    let mut lines = fixture.lines();
    let (method, rest) = lines.next().unwrap().split_once(' ').unwrap();
    let target = rest.split(' ').next().unwrap();
    let body = r#"{"model":"m","stream":true}"#;
    let mut raw = format!("{method} {path}{target} HTTP/1.1\r\nHost: 127.0.0.1:{pp}\r\n");
    for l in lines.filter(|l| !l.is_empty()) {
        raw.push_str(l);
        raw.push_str("\r\n");
    }
    for h in SPOOFED {
        raw.push_str(&format!("{h}: SPOOFED-{h}\r\n"));
    }
    raw.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    let resp = raw_request(pp, raw).await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    let s = seen.lock().unwrap();
    assert_eq!(s.bodies[0], body.as_bytes());
    (s.heads[0].clone(), attr)
}

/// The gateway's view of a replayed request: no spoofed header, every
/// header the wire needs, the gateway's own credential and Ligus's own
/// attribution ids.
fn assert_clean(head: &str, attr: &Attribution) {
    assert!(!head.contains("SPOOFED") && !head.contains("dummy-capture"), "{head}");
    assert_eq!(header(head, "authorization"), vec!["Bearer sk-REAL-GATEWAY-KEY"], "{head}");
    assert_eq!(header(head, "x-ligus-agent"), vec![attr.agent.as_deref().unwrap()]);
    assert_eq!(header(head, "x-ligus-workspace"), vec![attr.workspace.as_deref().unwrap()]);
    for h in head.lines().skip(1).filter_map(|l| l.split_once(':')).map(|(k, _)| k.trim().to_ascii_lowercase()) {
        assert!(
            matches!(h.as_str(), "host" | "content-length" | "authorization" | "x-ligus-agent" | "x-ligus-workspace")
                || request_header_allow_list_of(Api::Openai).any(|a| a == h)
                || request_header_allow_list_of(Api::Anthropic).any(|a| a == h),
            "forwarded {h}: {head}"
        );
    }
}

/// Claude Code's recorded `/v1/messages` request still works through the
/// route: its version and beta headers arrive, the rest is dropped.
#[tokio::test]
async fn claude_code_request_shape_through_the_route() {
    let (head, attr) = replay_fixture(Api::Anthropic, include_str!("fixtures/claude-code-2.1.312-messages.http")).await;
    assert!(head.starts_with("POST /v1/messages?beta=true "), "{head}");
    assert_clean(&head, &attr);
    assert_eq!(header(&head, "anthropic-version"), vec!["2023-06-01"]);
    assert!(header(&head, "anthropic-beta")[0].starts_with("claude-code-20250219,"), "{head}");
    assert_eq!(header(&head, "content-type"), vec!["application/json"]);
    assert_eq!(header(&head, "accept"), vec!["application/json"]);
    assert!(header(&head, "user-agent")[0].starts_with("claude-cli/"), "{head}");
    assert!(header(&head, "x-stainless-lang").is_empty() && header(&head, "x-claude-code-session-id").is_empty() && header(&head, "x-app").is_empty());
}

/// Codex CLI's recorded `/v1/responses` request still works through the
/// route: content negotiation arrives, its session and metadata headers
/// don't.
#[tokio::test]
async fn codex_request_shape_through_the_route() {
    let (head, attr) = replay_fixture(Api::Openai, include_str!("fixtures/codex-0.145.0-responses.http")).await;
    assert!(head.starts_with("POST /v1/responses "), "{head}");
    assert_clean(&head, &attr);
    assert_eq!(header(&head, "accept"), vec!["text/event-stream"]);
    assert_eq!(header(&head, "content-type"), vec!["application/json"]);
    assert!(header(&head, "user-agent")[0].starts_with("codex_exec/"), "{head}");
    assert!(header(&head, "x-codex-turn-metadata").is_empty() && header(&head, "session-id").is_empty() && header(&head, "originator").is_empty());
}

/// Review: the allow-list holds against header tricks a sandboxed agent
/// can send the route: odd case, a space before the colon, an obs-fold
/// continuation line, a `Connection:`-listed header, an HTTP/2 pseudo
/// header, `TE: trailers` with a trailer, a request smuggled in a chunked
/// body next to a conflicting `Content-Length`, and a bare LF inside an
/// allowed header's value. The gateway sees only allowed headers and the
/// decoded body, framed by the proxy itself, in one request.
#[tokio::test]
async fn allow_list_survives_header_tricks() {
    let (port, seen) = fake_gateway();
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("api-key");
    std::fs::write(&key, "sk-REAL-GATEWAY-KEY").unwrap();
    let mut g = api_key_gateway(port, &key, true);
    g.cfg.api = Api::Anthropic;
    let attr = Attribution::new(&[3u8; 32], Some(Uuid::from_u128(9)), Some(Uuid::from_u128(10)));
    let proxy = crate::egress::EgressProxy::start_with(Vec::new(), None, vec![ResolvedGateway { gateway: Arc::new(g), attribution: Some(attr.clone()) }]).unwrap();
    let pp = proxy.port();
    let path = proxy.gateway_url("test-gw").strip_prefix(&format!("http://127.0.0.1:{pp}")).unwrap().to_string();
    let smuggled = "POST /key/generate HTTP/1.1\r\nHost: gw\r\nContent-Length: 0\r\n\r\n";
    let chunk = format!("{{\"model\":\"m\"}}{smuggled}");
    let raw = format!(
        "POST {path}/v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         X-LiteLLM-API-Key: EVIL-case\r\n\
         x-litellm-tags : EVIL-space\r\n\
         User-Agent: agent/1\r\n x-litellm-customer-id: EVIL-fold\r\n\tx-envoy-original-path: EVIL-tab\r\n\
         Connection: keep-alive, x-forwarded-for\r\nx-forwarded-for: EVIL-conn\r\n\
         :authority: EVIL-pseudo\r\n\
         TE: trailers\r\nTrailer: x-litellm-api-key\r\n\
         Content-Type: application/json\r\nanthropic-version: 2023-06-01\r\n\
         Content-Length: 3\r\nTransfer-Encoding: chunked\r\n\r\n\
         {:x}\r\n{chunk}\r\n0\r\nx-litellm-api-key: EVIL-trailer\r\n\r\n",
        chunk.len()
    );
    let resp = raw_request(pp, raw).await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    {
        let s = seen.lock().unwrap();
        assert_eq!(s.heads.len(), 1, "one request reaches the gateway");
        let head = &s.heads[0];
        assert!(!head.contains("EVIL"), "{head}");
        assert_clean(head, &attr);
        assert!(header(head, "transfer-encoding").is_empty() && header(head, "te").is_empty() && header(head, "trailer").is_empty(), "{head}");
        assert_eq!(header(head, "content-length"), vec![chunk.len().to_string().as_str()], "{head}");
        assert_eq!(header(head, "user-agent"), vec!["agent/1"]);
        assert_eq!(header(head, "anthropic-version"), vec!["2023-06-01"]);
        assert_eq!(s.bodies[0], chunk.as_bytes(), "the smuggled request is only body bytes");
    }
    // A bare LF can't split an allowed header into a second one: the proxy
    // refuses to build the request rather than forward it.
    let raw = format!("POST {path}/v1/messages HTTP/1.1\r\nHost: x\r\nUser-Agent: a\nx-litellm-api-key: EVIL-lf\r\nContent-Length: 2\r\n\r\n{{}}");
    let resp = raw_request(pp, raw).await;
    assert!(!resp.starts_with("HTTP/1.1 200"), "{resp}");
    let s = seen.lock().unwrap();
    assert!(s.heads.iter().all(|h| !h.contains("EVIL")), "{:?}", s.heads);
}

/// Agent access controls: the requesting member rides as a keyed pseudonym
/// in `x-ligus-member` (never the member id, a name or an email), only when
/// the gateway's attribution is on and a member is known.
#[test]
fn attribution_names_the_requesting_member_pseudonymously() {
    let key = [7u8; 32];
    let w = Uuid::from_u128(1);
    let member = "3f6c1a2e-5b7d-4c11-9a0b-2d4e6f8a1c3e";
    let at = Attribution::new(&key, Some(w), None).with_member(&key, Some(w), Some(member));
    let m = at.member.clone().unwrap();
    assert_eq!(m.len(), 32);
    assert!(!m.contains(&member[..8]));
    assert_eq!(m, member_pseudonym(&key, w, member), "stable");
    assert_ne!(m, member_pseudonym(&[8u8; 32], w, member), "keyed");
    assert!(at.headers().iter().any(|(h, v)| *h == HEADER_MEMBER && v == &m));
    assert_eq!(HEADER_MEMBER, "x-ligus-member");
    // Inside the reserved set: a caller can't set it.
    assert!(at.headers().iter().all(|(h, _)| h.starts_with("x-ligus-")));
    // No member: no header.
    let none = Attribution::new(&key, Some(w), None).with_member(&key, Some(w), Some(""));
    assert!(none.headers().iter().all(|(h, _)| *h != HEADER_MEMBER));
    // A UUID member id hashes the same way as its string form, not as raw
    // bytes: separated from an agent id of the same value.
    let u = Uuid::from_u128(9);
    assert_ne!(pseudonym_bytes(&key, b"m", u.to_string().as_bytes()), pseudonym(&key, b"a", u));
}

/// The member pseudonym is scoped to its workspace: one person's turns in
/// two workspaces can't be linked at the gateway by the member header
/// (account ids are the same across workspaces), and none is sent without
/// a workspace to scope it to.
#[test]
fn the_member_pseudonym_is_unlinkable_across_workspaces() {
    let key = [7u8; 32];
    let member = "3f6c1a2e-5b7d-4c11-9a0b-2d4e6f8a1c3e";
    let (w1, w2) = (Uuid::from_u128(1), Uuid::from_u128(2));
    let a = Attribution::new(&key, Some(w1), None).with_member(&key, Some(w1), Some(member)).member.unwrap();
    let b = Attribution::new(&key, Some(w2), None).with_member(&key, Some(w2), Some(member)).member.unwrap();
    assert_ne!(a, b);
    assert_eq!(a, Attribution::new(&key, Some(w1), None).with_member(&key, Some(w1), Some(member)).member.unwrap(), "stable within a workspace");
    assert!(Attribution::new(&key, None, None).with_member(&key, None, Some(member)).member.is_none());
}

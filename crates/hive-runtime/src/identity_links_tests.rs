//! Identity-link verification: golden SSHSIG vectors, and every forge against
//! a fake HTTP server on loopback (no real network).
//!
//! `testdata/identity_links/` holds signatures made with OpenSSH 9.9's
//! `ssh-keygen -Y sign` over `statement.txt` (throwaway keys; the private
//! halves were deleted), and, prefixed `openssh_`, the security-key vectors
//! from OpenSSH's own regression suite
//! (`regress/unittests/sshsig/testdata`, OpenSSH licence), made with real
//! FIDO authenticators under the namespace `unittest`.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;
use hive_core::identity_link::{render_statement, IdentityLink, LinkMethod};
use ssh_key::private::Ed25519Keypair;
use ssh_key::{HashAlg, LineEnding, PrivateKey};

use super::*;

const W: &str = "11111111-1111-4111-8111-111111111111";
const ACCOUNT: &str = "6f1c2d3e-0000-4000-8000-000000000001";
const ACCOUNT_KEY: [u8; 32] = [7u8; 32];

fn vector(name: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/identity_links").join(name);
    std::fs::read_to_string(p).unwrap()
}

fn ts(s: &str) -> Timestamp {
    serde_json::from_value(serde_json::json!(s)).unwrap()
}

fn ws() -> Uuid {
    Uuid::parse_str(W).unwrap()
}

fn now() -> Timestamp {
    ts("2026-12-01T00:00:00Z")
}

fn ctx() -> LinkContext<'static> {
    LinkContext { workspace_id: ws(), account_key: Some(&ACCOUNT_KEY) }
}

/// The link `statement.txt` is the rendering of.
fn golden_link() -> IdentityLink {
    IdentityLink {
        link_id: Uuid::from_u128(42),
        account_id: Uuid::parse_str(ACCOUNT).unwrap(),
        provider: "github".into(),
        host: "github.com".into(),
        subject_id: "1234567".into(),
        handle_at_link: "alice".into(),
        method: LinkMethod::SshSig,
        statement: String::new(),
        proof: String::new(),
        key_fingerprint: String::new(),
        nonce: "AAECAwQFBgcICQoLDA0ODw".into(),
        issued_at: ts("2026-11-14T12:00:00Z"),
        expires_at: ts("2027-11-14T12:00:00Z"),
    }
}

fn key(seed: u8) -> PrivateKey {
    PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
}

fn openssh_pub(k: &PrivateKey) -> String {
    k.public_key().to_openssh().unwrap()
}

/// A link for `provider/host/subject`, SSH-signed by `k` in this test.
fn signed(provider: &str, host: &str, subject: &str, handle: &str, k: &PrivateKey) -> IdentityLink {
    let mut l = golden_link();
    l.provider = provider.into();
    l.host = host.into();
    l.subject_id = subject.into();
    l.handle_at_link = handle.into();
    l.statement = render_statement(&l, ws(), &ACCOUNT_KEY);
    l.proof = k.sign(LINK_NAMESPACE, HashAlg::Sha512, l.statement.as_bytes()).unwrap().to_pem(LineEnding::LF).unwrap();
    l.key_fingerprint = k.public_key().fingerprint(HashAlg::Sha256).to_string();
    l
}

// ---------------------------------------------------------------------------
// Golden SSHSIG vectors

#[test]
fn golden_statement_matches_the_renderer() {
    assert_eq!(render_statement(&golden_link(), ws(), &ACCOUNT_KEY), vector("statement.txt"));
}

#[test]
fn golden_vectors_from_ssh_keygen_verify() {
    let msg = vector("statement.txt");
    for k in ["ed25519", "ecdsa", "rsa"] {
        let ok = verify_sshsig(&vector(&format!("{k}.sig")), LINK_NAMESPACE, msg.as_bytes())
            .unwrap_or_else(|e| panic!("{k}: {e}"));
        let pubkey = parse_openssh(&vector(&format!("{k}.pub"))).unwrap();
        assert_eq!(ok.public_key.key_data(), pubkey.key_data(), "{k}");
        assert_eq!(ok.fingerprint, pubkey.fingerprint(HashAlg::Sha256).to_string());
        // One byte of the statement changed: no longer valid.
        let tampered = msg.replace("subject: 1234567", "subject: 7654321");
        assert!(verify_sshsig(&vector(&format!("{k}.sig")), LINK_NAMESPACE, tampered.as_bytes()).is_err(), "{k}");
    }
    // The ssh-keygen signature over the golden statement makes a full link.
    let mut l = golden_link();
    l.statement = msg;
    l.proof = vector("ed25519.sig");
    l.key_fingerprint = parse_openssh(&vector("ed25519.pub")).unwrap().fingerprint(HashAlg::Sha256).to_string();
    assert!(offline_check(&l, ctx(), now()).unwrap().is_some());
}

/// Hardware keys (pre-check): `sk-ssh-ed25519@openssh.com` and
/// `sk-ecdsa-sha2-nistp256@openssh.com` signatures made by real
/// authenticators verify; the WebAuthn variant is refused.
#[test]
fn golden_security_key_vectors_verify() {
    let ns = vector("openssh_namespace");
    let msg = vector("openssh_signed-data");
    for k in ["ed25519_sk", "ecdsa_sk"] {
        let ok = verify_sshsig(&vector(&format!("openssh_{k}.sig")), ns.trim(), msg.as_bytes())
            .unwrap_or_else(|e| panic!("{k}: {e}"));
        let pubkey = parse_openssh(&vector(&format!("openssh_{k}.pub"))).unwrap();
        assert_eq!(ok.public_key.key_data(), pubkey.key_data());
        assert!(verify_sshsig(&vector(&format!("openssh_{k}.sig")), LINK_NAMESPACE, msg.as_bytes()).is_err());
    }
    assert!(verify_sshsig(&vector("openssh_ecdsa_sk_webauthn.sig"), ns.trim(), msg.as_bytes()).is_err());
}

/// A security-key signature without the user-presence flag is refused.
#[test]
fn security_key_without_user_presence_is_refused() {
    let sig = SshSig::from_pem(vector("openssh_ed25519_sk.sig")).unwrap();
    let mut data = sig.signature().as_bytes().to_vec();
    let flags = data.len() - 5;
    data[flags] &= !0x01;
    let s = ssh_key::Signature::new(sig.signature().algorithm(), data).unwrap();
    let forged = SshSig::new(sig.public_key().clone(), sig.namespace(), sig.hash_alg(), s).unwrap();
    let err = verify_sshsig(&forged.to_pem(LineEnding::LF).unwrap(), "unittest", vector("openssh_signed-data").as_bytes())
        .unwrap_err();
    assert!(err.contains("user presence"), "{err}");
}

#[test]
fn small_rsa_keys_are_refused() {
    let err = verify_sshsig(&vector("rsa2048.sig"), LINK_NAMESPACE, vector("statement.txt").as_bytes()).unwrap_err();
    assert!(err.contains("3072"), "{err}");
}

/// S3 `sshsig_wrong_namespace_rejected`: a signature made for `git`,
/// `file`, or checked with an empty namespace never passes as a link proof.
#[test]
fn sshsig_wrong_namespace_rejected() {
    let msg = vector("statement.txt");
    for ns in ["git", "file"] {
        let sig = vector(&format!("ed25519.{ns}.sig"));
        // Valid under its own namespace...
        assert!(verify_sshsig(&sig, ns, msg.as_bytes()).is_ok(), "{ns}");
        // ...never as an identity link.
        assert!(verify_sshsig(&sig, LINK_NAMESPACE, msg.as_bytes()).is_err(), "{ns}");
        let mut l = golden_link();
        l.statement = msg.clone();
        l.proof = sig;
        l.key_fingerprint = parse_openssh(&vector("ed25519.pub")).unwrap().fingerprint(HashAlg::Sha256).to_string();
        assert!(matches!(offline_check(&l, ctx(), now()), Err(LinkStatus::Invalid { .. })), "{ns}");
    }
    assert!(verify_sshsig(&vector("ed25519.sig"), "", msg.as_bytes()).is_err(), "empty namespace");
}

#[test]
fn signature_by_another_key_than_named_is_invalid() {
    let mut l = signed("github", "github.com", "1234567", "alice", &key(1));
    l.key_fingerprint = key(2).public_key().fingerprint(HashAlg::Sha256).to_string();
    assert!(matches!(offline_check(&l, ctx(), now()), Err(LinkStatus::Invalid { .. })));
}

/// S2 `link_replayed_by_other_account_invalid` (view side), and cross
/// workspace replay: a statement checked against another workspace, or
/// another account, fails.
#[test]
fn link_replayed_into_another_workspace_or_account_is_invalid() {
    let l = signed("github", "github.com", "1234567", "alice", &key(1));
    let other_ws = LinkContext { workspace_id: Uuid::from_u128(9), account_key: Some(&ACCOUNT_KEY) };
    assert!(matches!(offline_check(&l, other_ws, now()), Err(LinkStatus::Invalid { .. })));
    let mut mallory = l.clone();
    mallory.account_id = Uuid::from_u128(666);
    assert!(offline_check(&mallory, ctx(), now()).is_err());
}

#[test]
fn rotated_account_key_turns_stale_and_expiry_expires() {
    let l = signed("github", "github.com", "1234567", "alice", &key(1));
    let rotated = LinkContext { workspace_id: ws(), account_key: Some(&[8u8; 32]) };
    assert!(matches!(offline_check(&l, rotated, now()), Err(LinkStatus::Stale { .. })));
    assert_eq!(offline_check(&l, ctx(), ts("2027-11-14T12:00:00Z")).unwrap_err(), LinkStatus::Expired);
}

/// S13 `self_asserted_never_shown_verified`.
#[tokio::test]
async fn self_asserted_never_shown_verified() {
    let mut l = golden_link();
    l.provider = "bitbucket".into();
    l.host = "bitbucket.org".into();
    l.method = LinkMethod::SelfAsserted;
    l.nonce.clear();
    let f = LinkFetcher::new(ForgeConfig { check_forges: true, ..Default::default() });
    let s = check_link(&l, ctx(), &f, now(), None).await;
    assert_eq!(s, LinkStatus::SelfAsserted);
    assert!(!s.is_verified());
}

// ---------------------------------------------------------------------------
// Fake forges

#[derive(Default)]
struct Fake {
    routes: BTreeMap<String, VecDeque<(u16, String)>>,
    hits: usize,
    authorization_seen: bool,
}

#[derive(Clone)]
struct Server {
    url: String,
    st: Arc<Mutex<Fake>>,
}

async fn handle(State(st): State<Arc<Mutex<Fake>>>, req: Request) -> Response {
    let mut f = st.lock().unwrap();
    f.hits += 1;
    if req.headers().contains_key("authorization") || req.headers().contains_key("cookie") {
        f.authorization_seen = true;
    }
    let key = req.uri().path_and_query().map(|p| p.as_str().to_string()).unwrap_or_default();
    let (status, body) = match f.routes.get_mut(&key) {
        Some(q) if q.len() > 1 => q.pop_front().unwrap(),
        Some(q) => q.front().cloned().unwrap(),
        None => (404, "{\"message\":\"Not Found\"}".into()),
    };
    let mut r = Response::new(Body::from(body.clone()));
    *r.status_mut() = StatusCode::from_u16(status).unwrap();
    if (300..400).contains(&status) {
        r.headers_mut().insert("location", HeaderValue::from_str(&body).unwrap());
    }
    r
}

async fn server() -> Server {
    let st = Arc::new(Mutex::new(Fake::default()));
    let app = axum::Router::new().fallback(handle).with_state(st.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Server { url, st }
}

impl Server {
    fn set(&self, path: &str, status: u16, body: impl Into<String>) {
        self.st.lock().unwrap().routes.insert(path.into(), VecDeque::from([(status, body.into())]));
    }
    /// Answers in turn (the last one repeats).
    fn seq(&self, path: &str, answers: Vec<(u16, String)>) {
        self.st.lock().unwrap().routes.insert(path.into(), answers.into());
    }
    fn hits(&self) -> usize {
        self.st.lock().unwrap().hits
    }
    fn saw_credentials(&self) -> bool {
        self.st.lock().unwrap().authorization_seen
    }
}

fn fetcher(apis: &[(&str, &str, &Server)]) -> LinkFetcher {
    let mut cfg = ForgeConfig { check_forges: true, ..Default::default() };
    for (p, h, s) in apis {
        cfg.test_api.insert((p.to_string(), h.to_string()), s.url.clone());
    }
    LinkFetcher::new(cfg)
}

fn gh_user(id: &str, login: &str) -> String {
    serde_json::json!({"id": id.parse::<u64>().unwrap(), "login": login}).to_string()
}

fn key_json(keys: &[String]) -> String {
    serde_json::Value::Array(keys.iter().enumerate().map(|(i, k)| serde_json::json!({"id": i, "key": k})).collect()).to_string()
}

/// GitHub: `/user/{id}` → login, `/users/{login}/keys` and
/// `/ssh_signing_keys`, then `/user/{id}` again.
fn github_alice(s: &Server, login: &str, auth: &[String], signing: &[String]) {
    s.set("/user/1234567", 200, gh_user("1234567", login));
    s.set(&format!("/users/{login}/keys"), 200, key_json(auth));
    s.set(&format!("/users/{login}/ssh_signing_keys"), 200, key_json(signing));
}

#[tokio::test]
async fn github_verifies_by_stable_id_without_credentials() {
    let gh = server().await;
    let k = key(1);
    github_alice(&gh, "alice", &[openssh_pub(&k)], &[]);
    gh.set("/users/alice", 200, gh_user("1234567", "alice"));
    let f = fetcher(&[("github", "github.com", &gh)]);
    assert_eq!(f.resolve_handle("github", "github.com", "@alice").await.unwrap(), ("1234567".into(), "alice".into()));
    let l = signed("github", "github.com", "1234567", "alice", &k);
    let s = check_link(&l, ctx(), &f, now(), None).await;
    assert!(matches!(&s, LinkStatus::Verified { handle: Some(h), name_bound: false, .. } if h == "alice"), "{s:?}");
    assert!(!gh.saw_credentials(), "never a token or cookie");
}

/// S1 `link_pins_subject_id_not_handle`: Alice renamed to `alice2` and
/// Mallory registered `alice`. Alice's link follows her id; Mallory's keys
/// under the old name never make it verify, and her own link (signed by
/// her key, naming the handle "alice") is bound to her own id.
#[tokio::test]
async fn link_pins_subject_id_not_handle() {
    let gh = server().await;
    let (alice, mallory) = (key(1), key(2));
    github_alice(&gh, "alice2", &[openssh_pub(&alice)], &[]);
    gh.set("/users/alice/keys", 200, key_json(&[openssh_pub(&mallory)]));
    gh.set("/users/alice/ssh_signing_keys", 200, "[]");
    gh.set("/user/999", 200, gh_user("999", "alice"));
    let f = fetcher(&[("github", "github.com", &gh)]);
    let a = signed("github", "github.com", "1234567", "alice", &alice);
    let s = check_link(&a, ctx(), &f, now(), None).await;
    assert!(matches!(&s, LinkStatus::Verified { handle: Some(h), .. } if h == "alice2"), "shows the current handle: {s:?}");
    // Mallory claims Alice's id with her own key: not on Alice's list.
    let m = signed("github", "github.com", "1234567", "alice", &mallory);
    assert!(matches!(check_link(&m, ctx(), &f, now(), None).await, LinkStatus::Stale { .. }));
    // Mallory's honest link is her own id, whatever handle it shows.
    let m = signed("github", "github.com", "999", "alice", &mallory);
    assert!(check_link(&m, ctx(), &f, now(), None).await.is_verified());
}

/// S1 `rename_between_resolve_and_fetch_is_rejected` (TOCTOU).
#[tokio::test]
async fn rename_between_resolve_and_fetch_is_rejected() {
    let gh = server().await;
    let k = key(1);
    github_alice(&gh, "alice", &[openssh_pub(&k)], &[]);
    gh.seq("/user/1234567", vec![(200, gh_user("1234567", "alice")), (200, gh_user("1234567", "alice-renamed"))]);
    let l = signed("github", "github.com", "1234567", "alice", &k);
    let s = check_link(&l, ctx(), &fetcher(&[("github", "github.com", &gh)]), now(), None).await;
    assert!(matches!(&s, LinkStatus::Unreachable { reason, .. } if reason.contains("renamed")), "{s:?}");
}

/// S4 `removed_forge_key_turns_stale` and `cache_ttl_bounds_staleness`.
#[tokio::test]
async fn removed_forge_key_turns_stale_within_the_cache_ttl() {
    let gh = server().await;
    let k = key(1);
    github_alice(&gh, "alice", &[openssh_pub(&k)], &[]);
    let f = fetcher(&[("github", "github.com", &gh)]);
    let l = signed("github", "github.com", "1234567", "alice", &k);
    let cache = MemoryCache::default();
    let t0 = now();
    assert!(check_cached(&cache, &l, ctx(), &f, t0, false).await.is_verified());
    let hits = gh.hits();
    // The key is removed on the forge.
    github_alice(&gh, "alice", &[], &[]);
    let t1 = Timestamp(t0.0 + time::Duration::hours(1));
    assert!(check_cached(&cache, &l, ctx(), &f, t1, false).await.is_verified(), "cached within the TTL");
    assert_eq!(gh.hits(), hits, "no request inside the TTL");
    let t2 = Timestamp(t0.0 + time::Duration::hours(24) + time::Duration::seconds(1));
    let s = check_cached(&cache, &l, ctx(), &f, t2, false).await;
    assert!(matches!(s, LinkStatus::Stale { .. }), "stale once the TTL passed: {s:?}");
    // A forced re-check (opening the profile) sees it at once.
    let cache = MemoryCache::default();
    github_alice(&gh, "alice", &[openssh_pub(&k)], &[]);
    assert!(check_cached(&cache, &l, ctx(), &f, t0, false).await.is_verified());
    github_alice(&gh, "alice", &[], &[]);
    assert!(matches!(check_cached(&cache, &l, ctx(), &f, t1, true).await, LinkStatus::Stale { .. }));
}

#[tokio::test]
async fn deleted_forge_account_is_stale_and_errors_are_unreachable() {
    let gh = server().await;
    let k = key(1);
    let l = signed("github", "github.com", "1234567", "alice", &k);
    let f = fetcher(&[("github", "github.com", &gh)]);
    assert!(matches!(check_link(&l, ctx(), &f, now(), None).await, LinkStatus::Stale { .. }), "404");
    gh.set("/user/1234567", 503, "busy");
    let last = Some(ts("2026-11-20T00:00:00Z"));
    let s = check_link(&l, ctx(), &f, now(), last).await;
    assert!(matches!(s, LinkStatus::Unreachable { last_verified_at, .. } if last_verified_at == last));
}

/// S10 `forge_checks_disabled_makes_no_requests`.
#[tokio::test]
async fn forge_checks_disabled_makes_no_requests() {
    let gh = server().await;
    let k = key(1);
    github_alice(&gh, "alice", &[openssh_pub(&k)], &[]);
    let mut f = fetcher(&[("github", "github.com", &gh)]);
    f.cfg.check_forges = false;
    let l = signed("github", "github.com", "1234567", "alice", &k);
    let cache = MemoryCache::default();
    assert_eq!(check_cached(&cache, &l, ctx(), &f, now(), true).await, LinkStatus::SignatureOnly);
    assert_eq!(gh.hits(), 0);
}

#[tokio::test]
async fn github_key_published_uses_the_signing_list_only() {
    let gh = server().await;
    let mine = account_key_openssh(&ACCOUNT_KEY).unwrap();
    let mut l = golden_link();
    l.method = LinkMethod::KeyPublished;
    l.nonce.clear();
    let f = fetcher(&[("github", "github.com", &gh)]);
    // On the auth list only (that would grant push): not a published key.
    github_alice(&gh, "alice", std::slice::from_ref(&mine), &[]);
    assert!(matches!(check_link(&l, ctx(), &f, now(), None).await, LinkStatus::Stale { .. }));
    github_alice(&gh, "alice", &[], &[mine]);
    assert!(check_link(&l, ctx(), &f, now(), None).await.is_verified());
}

#[tokio::test]
async fn gitlab_lists_keys_by_id() {
    let gl = server().await;
    let k = key(3);
    let mine = account_key_openssh(&ACCOUNT_KEY).unwrap();
    gl.set(
        "/users/77/keys",
        200,
        serde_json::json!([
            {"id": 1, "key": openssh_pub(&k), "usage_type": "auth"},
            {"id": 2, "key": mine, "usage_type": "signing"},
        ])
        .to_string(),
    );
    gl.set("/users/77", 403, "{\"message\":\"403 Forbidden\"}");
    gl.set("/users?username=bob", 200, serde_json::json!([{"id": 77, "username": "bob"}]).to_string());
    let f = fetcher(&[("gitlab", "gitlab.com", &gl)]);
    assert_eq!(f.resolve_handle("gitlab", "gitlab.com", "@bob").await.unwrap(), ("77".into(), "bob".into()));
    let l = signed("gitlab", "gitlab.com", "77", "bob", &k);
    let s = check_link(&l, ctx(), &f, now(), None).await;
    assert!(matches!(&s, LinkStatus::Verified { handle: None, .. }), "verified by id, handle unknown: {s:?}");
    let mut kp = golden_link();
    kp.provider = "gitlab".into();
    kp.host = "gitlab.com".into();
    kp.subject_id = "77".into();
    kp.method = LinkMethod::KeyPublished;
    kp.nonce.clear();
    assert!(check_link(&kp, ctx(), &f, now(), None).await.is_verified(), "signing usage");
}

#[tokio::test]
async fn forgejo_codeberg_and_gitea_use_the_anonymous_keys_listing_with_an_id_cross_check() {
    for (provider, host) in [("forgejo", "codeberg.org"), ("gitea", "gitea.com")] {
        let fj = server().await;
        let k = key(4);
        let carol = serde_json::json!({"ok": true, "data": [{"id": 55, "login": "carol"}]}).to_string();
        fj.set("/api/v1/users/search?uid=55", 200, carol.clone());
        fj.set("/carol.keys", 200, format!("{}\n", openssh_pub(&k)));
        fj.set("/api/v1/users/search?q=carol&limit=50", 200, carol);
        let f = fetcher(&[(provider, host, &fj)]);
        assert_eq!(f.resolve_handle(provider, host, "carol").await.unwrap().0, "55");
        let l = signed(provider, host, "55", "carol", &k);
        let s = check_link(&l, ctx(), &f, now(), None).await;
        assert!(matches!(&s, LinkStatus::Verified { handle: Some(h), .. } if h == "carol"), "{provider}: {s:?}");
        // The search answering for another uid is not this account.
        fj.set("/api/v1/users/search?uid=55", 200, serde_json::json!({"ok": true, "data": [{"id": 56, "login": "carol"}]}).to_string());
        assert!(matches!(check_link(&l, ctx(), &f, now(), None).await, LinkStatus::Stale { .. }), "{provider}");
    }
}

/// S1 `sourcehut_link_marked_name_bound`.
#[tokio::test]
async fn sourcehut_link_marked_name_bound() {
    let sr = server().await;
    let k = key(5);
    sr.set("/~dave.keys", 200, format!("# comment\n{}\n", openssh_pub(&k)));
    let f = fetcher(&[("sourcehut", "meta.sr.ht", &sr)]);
    assert_eq!(f.resolve_handle("sourcehut", "meta.sr.ht", "dave").await.unwrap().0, "~dave");
    let l = signed("sourcehut", "meta.sr.ht", "~dave", "~dave", &k);
    let s = check_link(&l, ctx(), &f, now(), None).await;
    assert!(matches!(s, LinkStatus::Verified { name_bound: true, .. }), "{s:?}");
}

#[tokio::test]
async fn tangled_resolves_the_did_then_reads_the_pds() {
    let plc = server().await;
    let pds = server().await;
    let k = key(6);
    let did = "did:plc:abcdefghijklmnopqrstuvwx";
    plc.set(
        &format!("/{did}"),
        200,
        serde_json::json!({
            "id": did,
            "alsoKnownAs": ["at://erin.example.com"],
            "service": [{"id": "#atproto_pds", "type": "AtprotoPersonalDataServer", "serviceEndpoint": pds.url}],
        })
        .to_string(),
    );
    pds.set(
        &format!("/xrpc/com.atproto.repo.listRecords?repo={did}&collection=org.tangled.key.sshKey&limit=100"),
        200,
        serde_json::json!({"records": [{"uri": "at://x", "value": {"name": "laptop", "publicKey": openssh_pub(&k), "createdAt": "2026-01-01T00:00:00Z"}}]}).to_string(),
    );
    // erin.example.com names the DID back, so the handle is shown.
    let erin = server().await;
    erin.set("/.well-known/atproto-did", 200, did);
    let mut cfg = ForgeConfig {
        check_forges: true,
        ask_owner_chosen_hosts: true,
        test_plc: Some(plc.url.clone()),
        test_allow_loopback: true,
        ..Default::default()
    };
    cfg.test_https.insert("erin.example.com".into(), erin.url.clone());
    let f = LinkFetcher::new(cfg);
    let l = signed("tangled", "atproto", did, "erin.example.com", &k);
    let s = check_link(&l, ctx(), &f, now(), None).await;
    assert!(matches!(&s, LinkStatus::Verified { handle: Some(h), .. } if h == "erin.example.com"), "{s:?}");

    // Without the test loopback allowance, a DID document pointing the PDS
    // at a private address is refused (SSRF).
    let cfg = ForgeConfig { check_forges: true, ask_owner_chosen_hosts: true, test_plc: Some(plc.url.clone()), ..Default::default() };
    let before = pds.hits();
    let s = check_link(&l, ctx(), &LinkFetcher::new(cfg), now(), None).await;
    assert!(!s.is_verified(), "{s:?}");
    assert_eq!(pds.hits(), before, "the private PDS is never contacted");
}

/// SSRF guards: a host this device wasn't told about is never contacted, a
/// redirect is never followed, an oversized answer is dropped, a
/// self-hosted forge is HTTPS only.
#[tokio::test]
async fn ssrf_guards_unconfigured_hosts_redirects_and_size() {
    let gh = server().await;
    let elsewhere = server().await;
    let k = key(1);
    // A co-member's link naming an internal host: not asked.
    let l = signed("gitlab", "gitlab.internal.example", "1", "x", &k);
    let s = check_link(&l, ctx(), &fetcher(&[("github", "github.com", &gh)]), now(), None).await;
    assert!(matches!(s, LinkStatus::NotChecked { .. }), "{s:?}");
    assert_eq!(gh.hits(), 0);
    // A redirect to another host is not followed.
    elsewhere.set("/user/1234567", 200, gh_user("1234567", "alice"));
    gh.set("/user/1234567", 302, format!("{}/user/1234567", elsewhere.url));
    let l = signed("github", "github.com", "1234567", "alice", &k);
    let f = fetcher(&[("github", "github.com", &gh)]);
    assert!(matches!(check_link(&l, ctx(), &f, now(), None).await, LinkStatus::Unreachable { .. }));
    assert_eq!(elsewhere.hits(), 0, "redirect not followed");
    // An answer over the cap.
    github_alice(&gh, "alice", &[openssh_pub(&k)], &[]);
    gh.set("/users/alice/keys", 200, "[".to_string() + &"0,".repeat(MAX_RESPONSE_BYTES) + "0]");
    assert!(matches!(check_link(&l, ctx(), &f, now(), None).await, LinkStatus::Unreachable { .. }));
    // A self-hosted forge must be HTTPS.
    let cfg = ForgeConfig {
        check_forges: true,
        self_hosted: vec![SelfHostedForge { provider: "gitlab".into(), host: "gl.example".into(), base_url: "http://gl.example".into() }],
        ..Default::default()
    };
    assert!(cfg.api_root("gitlab", "gl.example").is_none());
    let cfg = ForgeConfig {
        self_hosted: vec![SelfHostedForge { provider: "gitlab".into(), host: "gl.example".into(), base_url: "https://gl.example".into() }],
        ..Default::default()
    };
    assert_eq!(cfg.api_root("gitlab", "gl.example").as_deref(), Some("https://gl.example/api/v4"));
}

#[test]
fn public_ip_filter() {
    for bad in ["127.0.0.1", "10.1.2.3", "172.16.0.1", "192.168.1.1", "169.254.169.254", "100.64.0.1", "0.0.0.0", "::1", "fc00::1", "fe80::1", "::ffff:10.0.0.1"] {
        assert!(!is_public_ip(bad.parse().unwrap()), "{bad}");
    }
    for good in ["140.82.112.3", "2606:4700::1111"] {
        assert!(is_public_ip(good.parse().unwrap()), "{good}");
    }
}

#[test]
fn handles_and_dids_are_strict() {
    assert!(valid_did("did:plc:abcdefghijklmnopqrstuvwx"));
    assert!(!valid_did("did:plc:ABC"));
    assert!(valid_did("did:web:example.com"));
    assert!(!valid_did("did:web:127.0.0.1:8080/x"));
    assert!(!safe_segment(".."));
    assert!(!safe_segment("a/b"));
    assert!(!safe_segment("a?b"));
}

/// The relay is never involved: a link naming the relay's host isn't a
/// forge this device asks, and the status comes only from the forge.
#[tokio::test]
async fn relay_cannot_forge_or_suppress_verification() {
    let relay = server().await;
    let gh = server().await;
    let k = key(1);
    github_alice(&gh, "alice", &[openssh_pub(&k)], &[]);
    // A relay that also pretends to be a forge for the host it serves.
    relay.set("/user/1234567", 200, gh_user("1234567", "alice"));
    relay.set("/users/alice/keys", 200, key_json(&[openssh_pub(&key(9))]));
    let host = relay.url.trim_start_matches("http://").replace(':', "-");
    let fake = signed("github", &host, "1234567", "alice", &key(9));
    let f = fetcher(&[("github", "github.com", &gh)]);
    assert!(matches!(check_link(&fake, ctx(), &f, now(), None).await, LinkStatus::NotChecked { .. }));
    assert_eq!(relay.hits(), 0, "the relay is never asked");
    // The genuine link verifies from the forge alone.
    let l = signed("github", "github.com", "1234567", "alice", &k);
    assert!(check_link(&l, ctx(), &f, now(), None).await.is_verified());
}

/// S14 (extends `fold_ignores_env`): forges answering differently change
/// the device-local status only; the fold and the roster are identical.
#[tokio::test]
async fn fold_ignores_forge_answers() {
    use hive_core::{ActorIdentity, ActorKind, SigningKeypair};
    let store = crate::EventStore::open_in_memory().unwrap();
    let account = Uuid::new_v4();
    let mut author = ActorIdentity::new(account.to_string(), "Ana", ActorKind::Human);
    author.account_id = Some(account);
    let mut svc = crate::ChatService::new(store, Uuid::new_v4(), SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), author);
    let wid = Uuid::new_v4();
    svc.ensure_workspace_config(wid).unwrap();
    let k = key(1);
    let d = crate::chat_service::LinkDraft::new("github", "github.com", "1234567", "alice", LinkMethod::SshSig);
    let mut l = svc.draft_identity_link(wid, &d).unwrap();
    l.proof = k.sign(LINK_NAMESPACE, HashAlg::Sha512, l.statement.as_bytes()).unwrap().to_pem(LineEnding::LF).unwrap();
    l.key_fingerprint = k.public_key().fingerprint(HashAlg::Sha256).to_string();
    svc.publish_identity_link(wid, l.clone()).unwrap();

    let fold = |svc: &crate::ChatService| {
        let (_, t) = svc.store().load_config(wid, None, None).unwrap();
        (t.fingerprint(), t.current().clone())
    };
    let before = fold(&svc);
    let (yes, no) = (server().await, server().await);
    github_alice(&yes, "alice", &[openssh_pub(&k)], &[]);
    github_alice(&no, "alice", &[], &[]);
    let key = svc.account_public_key();
    let c = LinkContext { workspace_id: wid, account_key: Some(&key) };
    let a = check_link(&l, c, &fetcher(&[("github", "github.com", &yes)]), Timestamp::now(), None).await;
    let b = check_link(&l, c, &fetcher(&[("github", "github.com", &no)]), Timestamp::now(), None).await;
    assert!(a.is_verified() && matches!(b, LinkStatus::Stale { .. }));
    let after = fold(&svc);
    assert_eq!(before.0, after.0);
    assert_eq!(before.1, after.1);
}

// ---------------------------------------------------------------------------
// Adversarial review (linked identities, 2.0 residuals)

/// A forge (or a DID document) is not trusted to hand back a clean name: a
/// GitLab username or an atproto handle carrying bidi overrides or
/// zero-width characters is shown without them.
#[tokio::test]
async fn forge_supplied_handles_lose_bidi_controls() {
    let gl = server().await;
    let k = key(3);
    gl.set("/users/77/keys", 200, serde_json::json!([{"id": 1, "key": openssh_pub(&k), "usage_type": "auth"}]).to_string());
    gl.set("/users/77", 200, serde_json::json!({"id": 77, "username": "b\u{200B}ob\u{202E}"}).to_string());
    let f = fetcher(&[("gitlab", "gitlab.com", &gl)]);
    let l = signed("gitlab", "gitlab.com", "77", "bob", &k);
    let s = check_link(&l, ctx(), &f, now(), None).await;
    assert!(matches!(&s, LinkStatus::Verified { handle: Some(h), .. } if h == "bob"), "{s:?}");
}


/// Privacy: a Tangled link's owner chooses its PDS (and a `did:web` host,
/// and a handle's domain). An automatic check (opening People) must not
/// make every viewer's device contact a server the link's owner runs, which
/// would tell them who looked and from where; only an explicit check does.
/// The big hosted PDSes are asked automatically.
#[tokio::test]
async fn automatic_checks_never_contact_hosts_the_link_owner_chose() {
    let plc = server().await;
    let pds = server().await;
    let k = key(6);
    let did = "did:plc:abcdefghijklmnopqrstuvwx";
    plc.set(
        &format!("/{did}"),
        200,
        serde_json::json!({"id": did, "service": [{"id": "#atproto_pds", "serviceEndpoint": pds.url}]}).to_string(),
    );
    pds.set(
        &format!("/xrpc/com.atproto.repo.listRecords?repo={did}&collection=org.tangled.key.sshKey&limit=100"),
        200,
        serde_json::json!({"records": [{"value": {"publicKey": openssh_pub(&k)}}]}).to_string(),
    );
    let l = signed("tangled", "atproto", did, "erin.example.com", &k);
    let auto = ForgeConfig { check_forges: true, test_plc: Some(plc.url.clone()), test_allow_loopback: true, ..Default::default() };
    let s = check_link(&l, ctx(), &LinkFetcher::new(auto.clone()), now(), None).await;
    assert!(matches!(s, LinkStatus::NotChecked { .. }), "{s:?}");
    assert_eq!(pds.hits(), 0, "the owner's server never hears of an automatic check");
    let did_web = "did:web:erin.example.com";
    let l_web = signed("tangled", "atproto", did_web, "erin.example.com", &k);
    assert!(matches!(check_link(&l_web, ctx(), &LinkFetcher::new(auto.clone()), now(), None).await, LinkStatus::NotChecked { .. }));
    let explicit = ForgeConfig { ask_owner_chosen_hosts: true, ..auto };
    assert!(check_link(&l, ctx(), &LinkFetcher::new(explicit), now(), None).await.is_verified());
    assert!(pds.hits() > 0);
    for host in ["bsky.social", "morel.us-east.host.bsky.network"] {
        assert!(well_known_pds(host), "{host}");
    }
    for host in ["erin.example.com", "bsky.network.evil.example", "evilbsky.network", "127.0.0.1"] {
        assert!(!well_known_pds(host), "{host}");
    }
}
/// A DID document for `did` naming `handle` and a PDS.
fn did_doc(did: &str, handle: &str, pds: &str) -> String {
    serde_json::json!({
        "id": did,
        "alsoKnownAs": [format!("at://{handle}")],
        "service": [{"id": "#atproto_pds", "type": "AtprotoPersonalDataServer", "serviceEndpoint": pds}],
    })
    .to_string()
}

fn tangled_keys(pds: &Server, did: &str, k: &PrivateKey) {
    pds.set(
        &format!("/xrpc/com.atproto.repo.listRecords?repo={did}&collection=org.tangled.key.sshKey&limit=100"),
        200,
        serde_json::json!({"records": [{"uri": "at://x", "value": {"publicKey": openssh_pub(k)}}]}).to_string(),
    );
}


/// An atproto handle is only shown as the account's when the handle's
/// domain names the DID back (`/.well-known/atproto-did`): a DID document
/// claiming someone else's handle in `alsoKnownAs` verifies (the keys are
/// the DID's), but the chip shows the DID, never the borrowed handle.
#[tokio::test]
async fn tangled_handle_shown_only_when_its_domain_names_the_did() {
    let plc = server().await;
    let pds = server().await;
    let famous = server().await;
    let k = key(6);
    let did = "did:plc:abcdefghijklmnopqrstuvwx";
    plc.set(&format!("/{did}"), 200, did_doc(did, "famous.example.com", &pds.url));
    tangled_keys(&pds, did, &k);
    famous.set("/.well-known/atproto-did", 200, "did:plc:zzzzzzzzzzzzzzzzzzzzzzzz");
    let mut cfg = ForgeConfig {
        check_forges: true,
        ask_owner_chosen_hosts: true,
        test_plc: Some(plc.url.clone()),
        test_allow_loopback: true,
        ..Default::default()
    };
    cfg.test_https.insert("famous.example.com".into(), famous.url.clone());
    let f = LinkFetcher::new(cfg);
    let l = signed("tangled", "atproto", did, "famous.example.com", &k);
    let s = check_link(&l, ctx(), &f, now(), None).await;
    assert!(matches!(&s, LinkStatus::Verified { handle: None, .. }), "a borrowed handle is not shown: {s:?}");
    // The same DID entering the link flow by DID gets no borrowed handle either.
    assert_eq!(f.resolve_handle("tangled", "atproto", did).await.unwrap(), (did.to_string(), did.to_string()));
    // When the domain does name the DID, the handle is shown.
    famous.set("/.well-known/atproto-did", 200, did);
    let s = check_link(&l, ctx(), &f, now(), None).await;
    assert!(matches!(&s, LinkStatus::Verified { handle: Some(h), .. } if h == "famous.example.com"), "{s:?}");
}

/// SSRF: IPv6 forms that carry an IPv4 address (NAT64, 6to4, the old
/// IPv4-compatible form, Teredo) and the deprecated site-local range reach
/// the same internal hosts as the IPv4 address itself.
#[test]
fn embedded_private_ipv4_in_ipv6_is_not_public() {
    for bad in [
        "64:ff9b::a9fe:a9fe",
        "64:ff9b::7f00:1",
        "64:ff9b:1::a00:1",
        "2002:7f00:1::",
        "2002:a9fe:a9fe::1",
        "::7f00:1",
        "::a9fe:a9fe",
        "2001:0:4136:e378:8000:63bf:3fff:fdd2",
        "fec0::1",
        "100::1",
    ] {
        assert!(!is_public_ip(bad.parse().unwrap()), "{bad}");
    }
    for good in ["64:ff9b::8c52:7003", "2002:8c52:7003::1", "2606:4700::1111"] {
        assert!(is_public_ip(good.parse().unwrap()), "{good}");
    }
}

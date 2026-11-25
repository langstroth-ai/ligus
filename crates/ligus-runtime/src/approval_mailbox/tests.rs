//! Code-match approval against the in-process relay: the exchange, its
//! refusals (design note 11, M4, M6 checks 1 to 4, M7, and the relay
//! routes of M7b and M10), and check 5 on real folded workspaces.

use std::sync::Arc;

use ligus_core::crypto::{device_id_for, SigAlg, SigningKeypair};
use ligus_core::e2ee::KeyAgreementKeypair;
use ligus_core::match_code::{self as mc, hex, put_cap, MailboxKeys, MatchKind};
use ligus_core::{ActorIdentity, ActorKind};
use uuid::Uuid;

use super::envelope::{seal_blob, sign_message};
use super::*;
use crate::box_admission::JoinRequest;
use crate::computer_link::{Grant, LinkRequest};
use crate::keyring::DeliveredKey;
use crate::test_relay::TestClock;
use crate::workspace_invite::{encode_workspace_invite, WorkspaceConn};

const MIN: i64 = 60_000;

struct Env {
    base: String,
    clock: TestClock,
    client: MailboxClient,
    ek: EpochKey,
    address: ApprovalAddress,
    room: String,
    fk: String,
    tmp: tempfile::TempDir,
}

async fn spawn(router: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

/// A relay, a workspace key at `epoch`, its registered mailbox and address.
async fn env_at(epoch: u32, key: [u8; 32], room: &str, fk: &str) -> Env {
    let (router, clock) = crate::test_relay::router_with_clock();
    let base = spawn(router).await;
    let client = MailboxClient::new(&base).with_clock(clock.clock());
    let counter = client.ensure_registered(&key).await.unwrap();
    let ws = ligus_core::room_workspace_id(room);
    let address = ApprovalAddress::derive(&base, ws, fk, epoch, &key, counter);
    Env { base, clock, client, ek: EpochKey { epoch, key }, address, room: room.into(), fk: fk.into(), tmp: tempfile::tempdir().unwrap() }
}

async fn env() -> Env {
    let fk = crate::envelope_verifier::FounderPin::fingerprint_hex(&SigningKeypair::generate().unwrap().public_key_bytes());
    env_at(3, rand_bytes(), &format!("acme-{}", Uuid::new_v4().simple()), &fk).await
}

fn clone_ka(ka: &KeyAgreementKeypair) -> KeyAgreementKeypair {
    KeyAgreementKeypair::from_seed(&ka.seed_bytes()).unwrap()
}

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
            ka: Some(clone_ka(&self.ka)),
        }
    }
}

struct Computer {
    key: SigningKeypair,
    ka: KeyAgreementKeypair,
    request: LinkRequest,
}

impl Computer {
    fn new(now: i64) -> Self {
        let key = SigningKeypair::generate().unwrap();
        let ka = KeyAgreementKeypair::generate().unwrap();
        let request = LinkRequest::new(&key, &ka.public_key_bytes(), "lt on mbp", now, None);
        Self { key, ka, request }
    }
    fn spec(&self) -> RequestSpec {
        RequestSpec {
            kind: MatchKind::Computer,
            code: self.request.encode(),
            signer: Signer::Ed25519(SigningKeypair::from_seed(&self.key.seed_bytes()).unwrap()),
            ka: Some(clone_ka(&self.ka)),
        }
    }
}

fn approver(name: &str) -> (ApproverIdentity, SigningKeypair) {
    let k = SigningKeypair::generate().unwrap();
    let id = ApproverIdentity {
        signer: Signer::Ed25519(SigningKeypair::from_seed(&k.seed_bytes()).unwrap()),
        device_name: format!("{name}'s desktop"),
        person: ApproverPerson { account_id: Uuid::new_v4(), name: name.into() },
    };
    (id, k)
}

async fn live(env: &Env, rid: [u8; 16]) -> LiveRequest {
    let l = list_live(&env.client, std::slice::from_ref(&env.ek)).await.unwrap();
    l.live.into_iter().find(|r| r.request.rid == rid).expect("the request is listed")
}

/// Post, open, reveal: both codes.
async fn exchange(env: &Env, spec: RequestSpec, me: ApproverIdentity, seen: &mut SeenSet) -> (Requester, ApproverSession, MatchCode, MatchCode) {
    let mut req = Requester::post(env.client.clone(), &env.address, spec, Some(env.tmp.path())).await.unwrap();
    let l = live(env, req.rid()).await;
    let mut session = ApproverSession::begin(env.client.clone(), &env.ek, &l.request, me, seen).await.unwrap();
    let events = req.poll(0).await.unwrap();
    let RequesterEvent::Opened { code: rc, .. } = events.into_iter().next().expect("opened") else { panic!("not an open") };
    let ac = session.await_reveal(0).await.unwrap().expect("revealed");
    (req, session, rc, ac)
}

fn conn(env: &Env, room: &str, fk: &str, inv: Option<Uuid>, epoch: u32) -> WorkspaceConn {
    WorkspaceConn {
        name: "Acme".into(),
        relay_url: env.base.clone(),
        room: room.into(),
        key: Some("passphrase".into()),
        icon: None,
        dm_account: None,
        founder_actor_id: Some(Uuid::new_v4().to_string()),
        fk: Some(fk.into()),
        inv: inv.map(|i| i.to_string()),
        ek: vec![DeliveredKey::new(epoch, &[5; 32])],
        sig: None,
    }
}

fn box_grant(env: &Env, to: &JoinRequest, room: &str, fk: &str, epoch: u32) -> String {
    let code = encode_workspace_invite(&conn(env, room, fk, Some(Uuid::new_v4()), epoch));
    to.seal(&crate::box_admission::with_redeem_key(&code, &[9; 32]).unwrap()).unwrap()
}

fn person() -> ActorIdentity {
    let acct = Uuid::new_v4();
    let mut p = ActorIdentity::new(acct.to_string(), "Ana", ActorKind::Human);
    p.account_id = Some(acct);
    p
}

fn computer_grant(env: &Env, to: &LinkRequest, approver: &SigningKeypair, room: &str, fk: &str, epoch: u32, now: i64) -> String {
    let code = encode_workspace_invite(&conn(env, room, fk, None, epoch));
    let id = device_id_for(SigAlg::Ed25519, &approver.public_key_bytes());
    Grant::seal(to, ligus_core::room_workspace_id(room), &code, &person(), id, approver, now + 30 * MIN).unwrap()
}

fn approved(rid: &str, grant: &str) -> ResultBody {
    ResultBody { rid: rid.into(), outcome: Outcome::Approved, grant: Some(grant.into()), grant_digest: Some(super::envelope::grant_digest(grant)) }
}

#[test]
fn an_address_round_trips_and_refuses_what_it_should() {
    let a = ApprovalAddress::derive("https://relay.example/", Uuid::new_v4(), &"ab".repeat(16), 3, &[1; 32], 2);
    assert_eq!(a.relay, "https://relay.example");
    let text = a.encode();
    assert!(text.starts_with(ADDRESS_PREFIX));
    assert_eq!(ApprovalAddress::decode(&text).unwrap(), a);
    assert_eq!(a.mailbox, MailboxKeys::derive(&[1; 32]).mailbox());
    assert_eq!(a.put_cap, put_cap(&[1; 32], 2));
    let http = ApprovalAddress::derive("http://127.0.0.1:1", Uuid::new_v4(), &"ab".repeat(16), 0, &[1; 32], 0).encode();
    assert_eq!(ApprovalAddress::decode(&http).unwrap_err(), AddressError::InsecureRelay);
    assert!(ApprovalAddress::decode_with(&http, true).is_ok());
    let tamper = |f: &dyn Fn(&mut serde_json::Value)| {
        let body = b64::dec(text.strip_prefix(ADDRESS_PREFIX).unwrap()).unwrap();
        let mut v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        f(&mut v);
        ApprovalAddress::decode(&format!("{ADDRESS_PREFIX}{}", b64::enc(&serde_json::to_vec(&v).unwrap())))
    };
    assert_eq!(tamper(&|v| v["v"] = 2.into()).unwrap_err(), AddressError::Version(2));
    assert_eq!(tamper(&|v| v["sealKey"] = b64::enc(&[1; 31]).into()).unwrap_err(), AddressError::Malformed);
    assert_eq!(tamper(&|v| v["putCap"] = b64::enc(&[1; 17]).into()).unwrap_err(), AddressError::Malformed);
    assert_eq!(tamper(&|v| v["mailbox"] = "abc".into()).unwrap_err(), AddressError::Malformed);
    assert_eq!(tamper(&|v| v["founderPin"] = "zz".into()).unwrap_err(), AddressError::Malformed);
    assert_eq!(ApprovalAddress::decode("ligusjoin1:x").unwrap_err(), AddressError::NotAnAddress);
}

/// The happy path for a box: equal codes on both sides, a grant that passes
/// checks 1 to 4, and the request deleted on settle (D1).
#[tokio::test]
async fn a_box_and_its_admin_see_the_same_code_and_the_grant_checks_out() {
    let env = env().await;
    let b = BoxDev::new("build-1");
    let (me, _) = approver("Ana");
    let (mut req, mut session, rc, ac) = exchange(&env, b.spec(), me, &mut SeenSet::in_memory()).await;
    assert_eq!(rc.code40, ac.code40);
    assert_eq!(rc.shown.len(), 9);
    assert_eq!(req.claimed().unwrap().name, "Ana");
    assert!(env.tmp.path().join(PENDING_FILE).exists());
    let pending = PendingStore::new(env.tmp.path()).load().unwrap();
    assert!(pending[0].open_seen);

    let grant = box_grant(&env, &b.request, &env.room, &env.fk, 3);
    session.finish_approved(Some(&grant)).await.unwrap();
    let ev = req.poll(0).await.unwrap();
    let RequesterEvent::Result(r) = &ev[0] else { panic!("{ev:?}") };
    let checked = req.check_grant(r, GrantTarget::Box { ka: &b.ka }, env.clock.now()).unwrap();
    let CheckedGrant::Box { invite_id, conn, .. } = &checked else { panic!() };
    assert_eq!(conn.id(), env.address.workspace_id);
    let pk = b.key.public_key_bytes();
    let la = req.log_approval(&checked, &pk, 0).unwrap();
    assert!(matches!(la.subject, LogSubject::Box { invite_id: i, .. } if i == *invite_id));
    assert!(!format!("{checked:?}").contains("hivews1"), "no secret in Debug");

    req.settle().await.unwrap();
    assert!(!env.tmp.path().join(PENDING_FILE).exists(), "the pending entry is gone");
    let keys = MailboxKeys::derive(&env.ek.key);
    assert!(env.client.list_requests(&keys).await.unwrap().is_empty(), "the request is deleted on settle");
}

#[tokio::test]
async fn a_computer_and_its_persons_desktop_see_the_same_code() {
    let env = env().await;
    let c = Computer::new(env.clock.now());
    let (me, desk) = approver("Ana");
    let (req, mut session, rc, ac) = exchange(&env, c.spec(), me, &mut SeenSet::in_memory()).await;
    assert_eq!(rc.code40, ac.code40);
    let grant = computer_grant(&env, &c.request, &desk, &env.room, &env.fk, 3, env.clock.now());
    let mut req = req;
    // Identity model v2, 8.1: offer, ack, then (the caller's passkey and
    // publish) the result.
    assert_eq!(session.finish_approved(Some(&grant)).await.unwrap_err(), ApproverError::NotAcknowledged);
    session.offer(grant.clone()).await.unwrap();
    let ev = req.poll(0).await.unwrap();
    assert!(matches!(&ev[0], RequesterEvent::Offered { grant_digest, person_name } if *grant_digest == super::envelope::grant_digest(&grant) && person_name == "Ana"), "{ev:?}");
    req.ack().await.unwrap();
    assert!(session.await_ack(0).await.unwrap());
    session.finish_approved(None).await.unwrap();
    let ev = req.poll(0).await.unwrap();
    let RequesterEvent::Result(r) = &ev[0] else { panic!("{ev:?}") };
    let pk = c.key.public_key_bytes();
    let target = || GrantTarget::Computer { request: &c.request, ka: &c.ka, device_key: &pk[..] };
    let checked = req.check_grant(r, target(), env.clock.now()).unwrap();
    assert!(matches!(checked, CheckedGrant::Computer { ref person, .. } if person.display_name == "Ana"));
}

/// Identity model v2 K1: for a computer, nothing leaves the desktop before
/// a valid ack: no grant in the mailbox, no result; an ack by another key or
/// for another digest is refused; past 2 minutes the link didn't finish and
/// nothing is sent; with a valid ack the grant hashes to the acknowledged
/// digest.
#[tokio::test]
async fn a_computer_gets_nothing_before_a_valid_ack() {
    let env = env().await;
    let c = Computer::new(env.clock.now());
    let (me, desk) = approver("Ana");
    let (mut req, mut session, _, _) = exchange(&env, c.spec(), me, &mut SeenSet::in_memory()).await;
    let grant = computer_grant(&env, &c.request, &desk, &env.room, &env.fk, 3, env.clock.now());
    session.offer(grant.clone()).await.unwrap();
    assert!(matches!(req.poll(0).await.unwrap()[0], RequesterEvent::Offered { .. }));
    // Nothing in the mailbox carries the grant.
    let keys = MailboxKeys::derive(&env.ek.key);
    let msgs = env.client.get_msgs(MailboxAuth::Member(&keys), &env.address.mailbox, &req.rid(), 0, 0).await.unwrap();
    for m in &msgs {
        if let Some(p) = open_blob(&c.ka, &m.blob) {
            assert!(!String::from_utf8_lossy(&p).contains(crate::computer_link::GRANT_PREFIX), "the grant left before the ack");
        }
    }
    assert_eq!(session.finish_approved(None).await.unwrap_err(), ApproverError::NotAcknowledged);
    // Forged acks: another key, another digest.
    let open = msgs.iter().find_map(|m| open_blob(&c.ka, &m.blob).and_then(|p| Verified::parse(&p).ok()).filter(|v| v.t == MsgType::Open)).unwrap();
    let open = super::envelope::verify_open(&open, &req.rid()).unwrap();
    let post_ack = |signer: Signer, digest: String| {
        let msg = sign_message(MsgType::Ack, &AckBody { rid: req.rid_hex(), grant_digest: digest }, &signer);
        let blob = seal_blob(&open.approver_ka, msg.as_bytes()).unwrap();
        let (client, mailbox, rid) = (env.client.clone(), env.address.mailbox, req.rid());
        let keys = MailboxKeys::derive(&env.ek.key);
        async move { client.post_msg(MailboxAuth::Member(&keys), &mailbox, &rid, &blob, "approver").await.unwrap() }
    };
    post_ack(Signer::Ed25519(SigningKeypair::generate().unwrap()), super::envelope::grant_digest(&grant)).await;
    post_ack(c.spec().signer, hex(&[7; 32])).await;
    assert!(!session.await_ack(0).await.unwrap(), "neither forged ack counts");
    // Two minutes pass: the link didn't finish, and nothing is sent.
    env.clock.advance(ACK_WINDOW_MS + 1_000);
    req.ack().await.unwrap();
    assert_eq!(session.await_ack(0).await.unwrap_err(), ApproverError::DidntFinish, "a late ack counts for nothing");
    assert_eq!(session.finish_approved(None).await.unwrap_err(), ApproverError::Ended);
    assert!(!req.poll(0).await.unwrap().iter().any(|e| matches!(e, RequesterEvent::Result(_))));
}

/// The valid path: the grant that arrives hashes to the acknowledged digest.
#[tokio::test]
async fn a_computer_receives_the_grant_it_acknowledged() {
    let env = env().await;
    let c = Computer::new(env.clock.now());
    let (me, desk) = approver("Ana");
    let (mut req, mut session, _, _) = exchange(&env, c.spec(), me, &mut SeenSet::in_memory()).await;
    let grant = computer_grant(&env, &c.request, &desk, &env.room, &env.fk, 3, env.clock.now());
    session.offer(grant.clone()).await.unwrap();
    let RequesterEvent::Offered { grant_digest, .. } = req.poll(0).await.unwrap().remove(0) else { panic!() };
    req.ack().await.unwrap();
    assert!(session.await_ack(0).await.unwrap());
    // The caller's passkey and publish happen here; then the result. A
    // different grant is refused.
    let other = computer_grant(&env, &c.request, &desk, &env.room, &env.fk, 3, env.clock.now());
    assert_eq!(session.finish_approved(Some(&other)).await.unwrap_err(), ApproverError::GrantChanged);
    session.finish_approved(Some(&grant)).await.unwrap();
    let RequesterEvent::Result(r) = req.poll(0).await.unwrap().remove(0) else { panic!() };
    assert_eq!(r.grant_digest.as_deref(), Some(grant_digest.as_str()));
    let pk = c.key.public_key_bytes();
    req.check_grant(&r, GrantTarget::Computer { request: &c.request, ka: &c.ka, device_key: &pk }, env.clock.now()).unwrap();
}

/// A phone (P-256) approves a box.
#[tokio::test]
async fn a_phone_approves_a_box_with_its_p256_key() {
    let env = env().await;
    let b = BoxDev::new("build-1");
    let phone = ligus_core::testkit::Phone::new();
    let me = ApproverIdentity {
        signer: Signer::P256(phone.key.clone()),
        device_name: "Ana's phone".into(),
        person: ApproverPerson { account_id: Uuid::new_v4(), name: "Ana".into() },
    };
    let (mut req, mut session, rc, ac) = exchange(&env, b.spec(), me, &mut SeenSet::in_memory()).await;
    assert_eq!(rc.code40, ac.code40);
    let claimed = req.claimed().unwrap();
    assert_eq!((claimed.alg, claimed.device_id), (SigAlg::P256, phone.device_id));
    session.finish_approved(Some(&box_grant(&env, &b.request, &env.room, &env.fk, 3))).await.unwrap();
    let ev = req.poll(0).await.unwrap();
    let RequesterEvent::Result(r) = &ev[0] else { panic!("{ev:?}") };
    req.check_grant(r, GrantTarget::Box { ka: &b.ka }, env.clock.now()).unwrap();
}

/// A man in the middle who holds the workspace key runs two exchanges: it
/// plays the approver to the honest box, and a box of its own to the honest
/// Admin. The two screens show different codes.
#[tokio::test]
async fn a_man_in_the_middle_substituting_keys_gives_different_codes() {
    let env = env().await;
    let honest = BoxDev::new("build-1");
    let mut req = Requester::post(env.client.clone(), &env.address, honest.spec(), None).await.unwrap();
    let l = live(&env, req.rid()).await;
    let (mallory, _) = approver("Ana");
    let mut fake_approver = ApproverSession::begin(env.client.clone(), &env.ek, &l.request, mallory, &mut SeenSet::in_memory()).await.unwrap();
    let RequesterEvent::Opened { code: box_code, .. } = req.poll(0).await.unwrap().remove(0) else { panic!() };
    fake_approver.await_reveal(0).await.unwrap().unwrap();

    let fake_box = BoxDev::new("build-1");
    let mut fake_req = Requester::post(env.client.clone(), &env.address, fake_box.spec(), None).await.unwrap();
    let l2 = live(&env, fake_req.rid()).await;
    let (ana, _) = approver("Ana");
    let mut admin = ApproverSession::begin(env.client.clone(), &env.ek, &l2.request, ana, &mut SeenSet::in_memory()).await.unwrap();
    fake_req.poll(0).await.unwrap();
    let admin_code = admin.await_reveal(0).await.unwrap().unwrap();
    assert_ne!(box_code.code40, admin_code.code40, "substituted keys show different codes");
    assert_eq!(l.label(), l2.label(), "the labels alone can't tell them apart");
    assert_ne!(l.fingerprint(), l2.fingerprint());
}

/// M4: a reveal that doesn't match the commit aborts.
#[tokio::test]
async fn a_reveal_that_misses_the_commit_aborts() {
    let env = env().await;
    let b = BoxDev::new("build-1");
    let signer = b.spec().signer;
    let (rid, n_r, read_key): ([u8; 16], [u8; 32], [u8; 16]) = (rand_bytes(), rand_bytes(), rand_bytes());
    let body = RequestBody {
        v: 1,
        kind: MatchKind::Box,
        rid: hex(&rid),
        mailbox: env.address.mailbox_hex(),
        code: b.request.encode(),
        requester_ka: b64::enc(&b.ka.public_key_bytes()),
        read_key: b64::enc(&read_key),
        commit: b64::enc(&mc::commit(&rid, &n_r)),
        created_at: env.clock.now(),
    };
    let blob = seal_blob(&env.address.seal_key, sign_message(MsgType::Request, &body, &signer).as_bytes()).unwrap();
    env.client.put_request(&env.address.mailbox, &rid, &env.address.put_cap, &blob, &read_key).await.unwrap();
    let l = live(&env, rid).await;
    let (me, _) = approver("Ana");
    let mut session = ApproverSession::begin(env.client.clone(), &env.ek, &l.request, me, &mut SeenSet::in_memory()).await.unwrap();
    // The requester reads the open and reveals another nR.
    let msgs = env.client.get_msgs(MailboxAuth::Requester(&read_key), &env.address.mailbox, &rid, 0, 0).await.unwrap();
    let open = msgs.iter().find_map(|m| open_blob(&b.ka, &m.blob)).unwrap();
    let open = super::envelope::verify_open(&Verified::parse(&open).unwrap(), &rid).unwrap();
    let wrong: [u8; 32] = rand_bytes();
    let reveal = sign_message(MsgType::Reveal, &RevealBody { rid: hex(&rid), n_r: b64::enc(&wrong) }, &signer);
    let blob = seal_blob(&open.approver_ka, reveal.as_bytes()).unwrap();
    env.client.post_msg(MailboxAuth::Requester(&read_key), &env.address.mailbox, &rid, &blob, "approver").await.unwrap();
    assert_eq!(session.await_reveal(0).await.unwrap_err(), ApproverError::RevealMismatch);
    let after = live(&env, rid).await;
    assert_eq!(after.ended.map(|e| e.0), Some(AbortReason::Mismatch), "the members see the abort");
}

/// M4: a second open is ignored; M4: a reused rid is refused.
#[tokio::test]
async fn a_second_open_is_ignored_and_a_reused_rid_is_refused() {
    let env = env().await;
    let b = BoxDev::new("build-1");
    let mut req = Requester::post(env.client.clone(), &env.address, b.spec(), None).await.unwrap();
    let l = live(&env, req.rid()).await;
    let (ana, ana_key) = approver("Ana");
    let (ben, _) = approver("Ben");
    let mut seen = SeenSet::in_memory();
    let mut first = ApproverSession::begin(env.client.clone(), &env.ek, &l.request, ana, &mut seen).await.unwrap();
    let mut second = ApproverSession::begin(env.client.clone(), &env.ek, &l.request, ben, &mut SeenSet::in_memory()).await.unwrap();
    let ev = req.poll(0).await.unwrap();
    assert_eq!(ev.iter().filter(|e| matches!(e, RequesterEvent::Opened { .. })).count(), 1);
    assert_eq!(req.claimed().unwrap().device_id, device_id_for(SigAlg::Ed25519, &ana_key.public_key_bytes()));
    assert!(first.await_reveal(0).await.unwrap().is_some());
    assert!(second.await_reveal(0).await.unwrap().is_none(), "the second approver never gets the reveal");
    // The same device can't open it again.
    let (ana2, _) = approver("Ana");
    let again = ApproverSession::begin(env.client.clone(), &env.ek, &l.request, ana2, &mut seen).await.unwrap_err();
    assert_eq!(again, ApproverError::ReusedRid);
    // Other members see two claims.
    assert_eq!(live(&env, req.rid()).await.claims.len(), 2);
}

/// The seen-set persists, per mailbox, for a day.
#[test]
fn the_seen_set_persists_for_a_day() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = SeenSet::load(tmp.path()).unwrap();
    s.mark(&[1; 16], &[2; 16], 1_000).unwrap();
    let s = SeenSet::load(tmp.path()).unwrap();
    assert!(s.contains(&[1; 16], &[2; 16], 1_000 + SEEN_TTL_MS));
    assert!(!s.contains(&[9; 16], &[2; 16], 1_000), "per mailbox");
    assert!(!s.contains(&[1; 16], &[2; 16], 2_000 + SEEN_TTL_MS));
}

/// M4: a request older than 15 minutes is refused (by the approver, before
/// the relay's own expiry), and one from the future too.
#[tokio::test]
async fn a_request_older_than_15_minutes_is_refused() {
    let env = env().await;
    let b = BoxDev::new("build-1");
    let req = Requester::post(env.client.clone(), &env.address, b.spec(), None).await.unwrap();
    let l = live(&env, req.rid()).await;
    let now = env.clock.now();
    let plain = l.request.envelope.as_bytes();
    assert_eq!(VerifiedRequest::verify(plain, &l.request.rid, &l.request.mailbox, now + 16 * MIN).unwrap_err(), MsgError::Expired);
    assert_eq!(VerifiedRequest::verify(plain, &l.request.rid, &l.request.mailbox, now - 6 * MIN).unwrap_err(), MsgError::FromTheFuture);
    assert_eq!(VerifiedRequest::verify(plain, &[0; 16], &l.request.mailbox, now).unwrap_err(), MsgError::WrongRid);
    assert_eq!(VerifiedRequest::verify(plain, &l.request.rid, &[0; 16], now).unwrap_err(), MsgError::WrongMailbox);
    let late = MailboxClient::new(&env.base).with_clock(Arc::new({
        let c = env.clock.clone();
        move || c.now() + 15 * MIN + 1_000
    }));
    let (me, _) = approver("Ana");
    let r = ApproverSession::begin(late, &env.ek, &l.request, me, &mut SeenSet::in_memory()).await.unwrap_err();
    assert_eq!(r, ApproverError::RequestExpired);
}

/// Request checks: a request signed by another key than its code's, or
/// whose key-agreement key isn't the code's, or whose kind isn't its code's.
#[tokio::test]
async fn a_request_whose_keys_dont_match_its_code_is_refused() {
    let env = env().await;
    let b = BoxDev::new("build-1");
    let rid: [u8; 16] = rand_bytes();
    let now = env.clock.now();
    let body = |kind: MatchKind, ka: [u8; 32]| RequestBody {
        v: 1,
        kind,
        rid: hex(&rid),
        mailbox: env.address.mailbox_hex(),
        code: b.request.encode(),
        requester_ka: b64::enc(&ka),
        read_key: b64::enc(&[1u8; 16]),
        commit: b64::enc(&[2u8; 32]),
        created_at: now,
    };
    let check = |plain: String| VerifiedRequest::verify(plain.as_bytes(), &rid, &env.address.mailbox, now).map(|_| ());
    let own = b.spec().signer;
    let other = Signer::Ed25519(SigningKeypair::generate().unwrap());
    assert!(check(sign_message(MsgType::Request, &body(MatchKind::Box, b.ka.public_key_bytes()), &own)).is_ok());
    assert_eq!(check(sign_message(MsgType::Request, &body(MatchKind::Box, b.ka.public_key_bytes()), &other)), Err(MsgError::WrongKey));
    assert_eq!(check(sign_message(MsgType::Request, &body(MatchKind::Box, [7; 32]), &own)), Err(MsgError::WrongRequesterKa));
    assert_eq!(check(sign_message(MsgType::Request, &body(MatchKind::Computer, b.ka.public_key_bytes()), &own)), Err(MsgError::KindMismatch));
    // Signed as another message type: never a request.
    assert!(matches!(check(sign_message(MsgType::Claim, &body(MatchKind::Box, b.ka.public_key_bytes()), &own)), Err(MsgError::WrongType { .. })));
}

/// M4: the code expires 5 minutes after the open; no grant is sent after.
#[tokio::test]
async fn the_code_expires_five_minutes_after_the_open() {
    let env = env().await;
    let b = BoxDev::new("build-1");
    let (me, _) = approver("Ana");
    let (mut req, mut session, rc, ac) = exchange(&env, b.spec(), me, &mut SeenSet::in_memory()).await;
    assert!(ac.expires_at - env.clock.now() <= mc::CODE_TTL_MS && rc.expires_at - env.clock.now() <= mc::CODE_TTL_MS);
    env.clock.advance(5 * MIN + 1_000);
    assert!(ac.expired(env.clock.now()));
    let grant = box_grant(&env, &b.request, &env.room, &env.fk, 3);
    assert_eq!(session.finish_approved(Some(&grant)).await.unwrap_err(), ApproverError::CodeExpired);
    let ev = req.poll(0).await.unwrap();
    assert!(matches!(&ev[0], RequesterEvent::Result(r) if r.outcome == Outcome::Expired && r.grant.is_none()), "{ev:?}");
    assert!(req.ended());
    // A late approved result would still be refused by the requester.
    assert_eq!(req.check_grant(&approved(&req.rid_hex(), &grant), GrantTarget::Box { ka: &b.ka }, env.clock.now()).unwrap_err(), GrantError::CodeExpired);
}

/// M7: two requests each get their own code; settling one leaves the other.
#[tokio::test]
async fn two_requests_get_their_own_codes_and_settling_one_leaves_the_other() {
    let env = env().await;
    let (b1, b2) = (BoxDev::new("build-1"), BoxDev::new("build-2"));
    let (a1, _) = approver("Ana");
    let (a2, _) = approver("Ana");
    let mut seen = SeenSet::in_memory();
    let (mut r1, _s1, c1, d1) = exchange(&env, b1.spec(), a1, &mut seen).await;
    let (r2, _s2, c2, d2) = exchange(&env, b2.spec(), a2, &mut seen).await;
    assert_eq!((c1.code40, c2.code40), (d1.code40, d2.code40));
    assert_ne!(c1.code40, c2.code40);
    assert_eq!(PendingStore::new(env.tmp.path()).load().unwrap().len(), 2);
    r1.settle().await.unwrap();
    let left = PendingStore::new(env.tmp.path()).load().unwrap();
    assert_eq!(left.iter().map(|e| e.rid.clone()).collect::<Vec<_>>(), vec![r2.rid_hex()]);
    let listing = list_live(&env.client, std::slice::from_ref(&env.ek)).await.unwrap();
    assert_eq!(listing.live.iter().map(|l| l.rid_hex()).collect::<Vec<_>>(), vec![r2.rid_hex()]);
}

/// 6.4: three dead requests in 15 minutes stop the requester.
#[tokio::test]
async fn three_dead_requests_stop_the_requester() {
    let env = env().await;
    let b = BoxDev::new("build-1");
    for _ in 0..DEAD_LIMIT {
        let mut r = Requester::post(env.client.clone(), &env.address, b.spec(), Some(env.tmp.path())).await.unwrap();
        r.abort(AbortReason::Cancelled).await.unwrap();
        r.settle().await.unwrap();
    }
    let e = Requester::post(env.client.clone(), &env.address, b.spec(), Some(env.tmp.path())).await.unwrap_err();
    assert_eq!(e, RequesterError::TooManyDead);
    assert_eq!(e.exit_code(), 1);
    env.clock.advance(16 * MIN);
    assert!(Requester::post(env.client.clone(), &env.address, b.spec(), Some(env.tmp.path())).await.is_ok());
}

/// A requester's abort reaches the approver; members see it.
#[tokio::test]
async fn a_requester_abort_ends_the_approvers_session() {
    let env = env().await;
    let b = BoxDev::new("build-1");
    let mut req = Requester::post(env.client.clone(), &env.address, b.spec(), None).await.unwrap();
    let l = live(&env, req.rid()).await;
    let (me, _) = approver("Ana");
    let mut s = ApproverSession::begin(env.client.clone(), &env.ek, &l.request, me, &mut SeenSet::in_memory()).await.unwrap();
    req.poll(0).await.unwrap();
    req.abort(AbortReason::Mismatch).await.unwrap();
    // The reveal came first; the abort ends it after the code.
    assert!(s.await_reveal(0).await.unwrap().is_some());
    s.mismatch().await.unwrap();
    assert_eq!(live(&env, req.rid()).await.ended.map(|e| e.0), Some(AbortReason::Mismatch));
}

/// Design 6.3 checks 1 to 4 for a computer: wrong approver key, wrong
/// digest, a grant for another request, a fork (another founder pin or
/// workspace), an older key epoch.
#[tokio::test]
async fn a_computer_refuses_every_grant_that_fails_checks_1_to_4() {
    let env = env().await;
    let c = Computer::new(env.clock.now());
    let (me, desk) = approver("Ana");
    let (req, _s, _, _) = exchange(&env, c.spec(), me, &mut SeenSet::in_memory()).await;
    let now = env.clock.now();
    let rid = req.rid_hex();
    let key = c.key.public_key_bytes();
    let mut req = req;
    let mut check = |g: &str| {
        req.set_acked_for_tests(&super::envelope::grant_digest(g));
        req.check_grant(&approved(&rid, g), GrantTarget::Computer { request: &c.request, ka: &c.ka, device_key: &key }, now)
    };
    assert!(check(&computer_grant(&env, &c.request, &desk, &env.room, &env.fk, 3, now)).is_ok());
    let other_desk = SigningKeypair::generate().unwrap();
    assert_eq!(check(&computer_grant(&env, &c.request, &other_desk, &env.room, &env.fk, 3, now)).unwrap_err(), GrantError::WrongApprover);
    let good = computer_grant(&env, &c.request, &desk, &env.room, &env.fk, 3, now);
    // Forks.
    let fork_fk = "cd".repeat(16);
    assert_eq!(check(&computer_grant(&env, &c.request, &desk, &env.room, &fork_fk, 3, now)).unwrap_err(), GrantError::WrongFounder);
    assert_eq!(check(&computer_grant(&env, &c.request, &desk, "another-room", &env.fk, 3, now)).unwrap_err(), GrantError::WrongWorkspace);
    // An older key epoch than the address's (3).
    assert_eq!(check(&computer_grant(&env, &c.request, &desk, &env.room, &env.fk, 2, now)).unwrap_err(), GrantError::OldEpoch);
    // Another request (same key-agreement key, another device key).
    let other_dev = SigningKeypair::generate().unwrap();
    let other = LinkRequest::new(&other_dev, &c.ka.public_key_bytes(), "lt", now, None);
    assert!(matches!(check(&computer_grant(&env, &other, &desk, &env.room, &env.fk, 3, now)).unwrap_err(), GrantError::NotForThisRequest(_)));
    // Sealed to another computer.
    let elsewhere = Computer::new(now);
    assert!(matches!(check(&computer_grant(&env, &elsewhere.request, &desk, &env.room, &env.fk, 3, now)).unwrap_err(), GrantError::NotForThisRequest(_)));
    // A grant other than the acknowledged one.
    req.set_acked_for_tests(&hex(&[1; 32]));
    assert_eq!(
        req.check_grant(&approved(&rid, &good), GrantTarget::Computer { request: &c.request, ka: &c.ka, device_key: &key }, now).unwrap_err(),
        GrantError::NotAcknowledged
    );
    req.set_acked_for_tests(&super::envelope::grant_digest(&good));
    let mut bad_digest = approved(&rid, &good);
    bad_digest.grant_digest = Some(hex(&[0; 32]));
    req.set_acked_for_tests(&hex(&[0; 32]));
    assert_eq!(
        req.check_grant(&bad_digest, GrantTarget::Computer { request: &c.request, ka: &c.ka, device_key: &key }, now).unwrap_err(),
        GrantError::DigestMismatch
    );
    req.set_acked_for_tests(&super::envelope::grant_digest(&good));
    let mut no_grant = approved(&rid, &good);
    no_grant.grant = None;
    no_grant.grant_digest = None;
    assert_eq!(
        req.check_grant(&no_grant, GrantTarget::Computer { request: &c.request, ka: &c.ka, device_key: &key }, now).unwrap_err(),
        GrantError::NotAcknowledged, "a computer's result must carry the acknowledged grant"
    );
    // Not approved.
    let mut declined = approved(&rid, &good);
    declined.outcome = Outcome::Declined;
    assert_eq!(
        req.check_grant(&declined, GrantTarget::Computer { request: &c.request, ka: &c.ka, device_key: &key }, now).unwrap_err(),
        GrantError::NotApproved(Outcome::Declined)
    );
}

/// The same checks for a box, and a result signed by another key than the
/// open's is never read.
#[tokio::test]
async fn a_box_refuses_every_grant_that_fails_checks_1_to_3() {
    let env = env().await;
    let b = BoxDev::new("build-1");
    let (me, _) = approver("Ana");
    let (mut req, _s, _, _) = exchange(&env, b.spec(), me, &mut SeenSet::in_memory()).await;
    let now = env.clock.now();
    let rid = req.rid_hex();
    let check = |g: &str| req.check_grant(&approved(&rid, g), GrantTarget::Box { ka: &b.ka }, now);
    assert!(check(&box_grant(&env, &b.request, &env.room, &env.fk, 3)).is_ok());
    // Sealed to another box.
    let other = BoxDev::new("build-2");
    assert!(matches!(check(&box_grant(&env, &other.request, &env.room, &env.fk, 3)).unwrap_err(), GrantError::NotForThisRequest(_)));
    // A bearer code answers no request.
    let bearer = crate::box_admission::with_redeem_key(&encode_workspace_invite(&conn(&env, &env.room, &env.fk, Some(Uuid::new_v4()), 3)), &[9; 32]).unwrap();
    assert!(matches!(check(&bearer).unwrap_err(), GrantError::NotForThisRequest(_)));
    assert_eq!(check(&box_grant(&env, &b.request, &env.room, &"ef".repeat(16), 3)).unwrap_err(), GrantError::WrongFounder);
    assert_eq!(check(&box_grant(&env, &b.request, "fork-room", &env.fk, 3)).unwrap_err(), GrantError::WrongWorkspace);
    assert_eq!(check(&box_grant(&env, &b.request, &env.room, &env.fk, 1)).unwrap_err(), GrantError::OldEpoch);

    // A result signed by another key (sealed to the box all the same) is ignored.
    let rogue = Signer::Ed25519(SigningKeypair::generate().unwrap());
    let body = approved(&rid, &box_grant(&env, &b.request, &env.room, &env.fk, 3));
    let blob = seal_blob(&b.ka.public_key_bytes(), sign_message(MsgType::Result, &body, &rogue).as_bytes()).unwrap();
    let keys = MailboxKeys::derive(&env.ek.key);
    env.client.post_msg(MailboxAuth::Member(&keys), &env.address.mailbox, &req.rid(), &blob, "requester").await.unwrap();
    assert!(req.poll(0).await.unwrap().is_empty(), "a result not signed by the open's key is never read");
}

struct Classes(Uuid);
impl ApproverClassifier for Classes {
    fn classify(&self, _: MatchKind, device: Uuid, _: &[u8]) -> Approvability {
        if device == self.0 {
            Approvability::CanApprove
        } else {
            Approvability::Cannot
        }
    }
}

/// 6.4: the contested sheet.
#[tokio::test]
async fn claims_read_as_mine_claimed_or_contested() {
    let env = env().await;
    let b = BoxDev::new("build-1");
    let req = Requester::post(env.client.clone(), &env.address, b.spec(), None).await.unwrap();
    let l = live(&env, req.rid()).await;
    let me = Uuid::new_v4();
    assert_eq!(claim_view(&l, me, &Classes(me)), ClaimView::Unclaimed);
    let (ben, ben_key) = approver("Ben");
    let ben_id = device_id_for(SigAlg::Ed25519, &ben_key.public_key_bytes());
    ApproverSession::begin(env.client.clone(), &env.ek, &l.request, ben, &mut SeenSet::in_memory()).await.unwrap();
    let l = live(&env, req.rid()).await;
    assert_eq!(claim_view(&l, ben_id, &Classes(me)), ClaimView::Mine);
    assert!(matches!(claim_view(&l, me, &Classes(ben_id)), ClaimView::ClaimedBy { ref name, .. } if name == "Ben's desktop"));
    assert!(matches!(claim_view(&l, me, &Classes(me)), ClaimView::Contested { by: Some(_) }));

    // An open with no claim before it.
    let req2 = Requester::post(env.client.clone(), &env.address, b.spec(), None).await.unwrap();
    let keys = MailboxKeys::derive(&env.ek.key);
    env.client.post_msg(MailboxAuth::Member(&keys), &env.address.mailbox, &req2.rid(), &seal_blob(&[9; 32], b"x").unwrap(), "requester").await.unwrap();
    let l2 = live(&env, req2.rid()).await;
    assert_eq!(claim_view(&l2, me, &Classes(me)), ClaimView::Contested { by: None });
    // End this request.
    let (ana, _) = approver("Ana");
    end_request(&env.client, &env.ek, &l2.request, &ana.signer, AbortReason::Contested).await.unwrap();
    assert_eq!(live(&env, req2.rid()).await.ended.map(|e| e.0), Some(AbortReason::Contested));
}

/// The previous two epochs' mailboxes are polled too; a third-oldest isn't.
#[tokio::test]
async fn approvers_poll_the_current_and_the_previous_two_epochs() {
    let env = env().await;
    let keys: Vec<EpochKey> = (0..4).map(|e| EpochKey { epoch: e, key: rand_bytes() }).collect();
    for k in &keys {
        let n = env.client.ensure_registered(&k.key).await.unwrap();
        let a = ApprovalAddress::derive(&env.base, env.address.workspace_id, &env.fk, k.epoch, &k.key, n);
        Requester::post(env.client.clone(), &a, BoxDev::new(&format!("b{}", k.epoch)).spec(), None).await.unwrap();
    }
    let l = list_live(&env.client, &keys).await.unwrap();
    let mut epochs: Vec<u32> = l.live.iter().map(|r| r.epoch).collect();
    epochs.sort();
    assert_eq!(epochs, vec![1, 2, 3]);
    let unknown = list_live(&env.client, &[EpochKey { epoch: 9, key: rand_bytes() }]).await.unwrap();
    assert_eq!(unknown.unregistered, vec![9]);
}

// ---- Relay routes (M7b, M10) ----

#[tokio::test]
async fn puts_need_the_current_cap_and_reset_cuts_the_old_one_off() {
    let env = env().await;
    let http = reqwest::Client::new();
    let url = format!("{}/v1/mailboxes/{}/requests/{}", env.base, env.address.mailbox_hex(), hex(&[1u8; 16]));
    let body = serde_json::json!({ "blob": "abc", "readKeyHash": b64::enc(&[0; 32]) });
    let r = http.put(&url).json(&body).send().await.unwrap();
    assert_eq!(r.status(), 403, "no put cap");
    let wrong = env.client.put_request(&env.address.mailbox, &rand_bytes(), &[0; 16], "abc", &[1; 16]).await.unwrap_err();
    assert_eq!(wrong, MailboxError::BadCap);
    assert_eq!(wrong.exit_code(), 6);
    // Register is idempotent; reset moves the counter; a stale register is refused.
    assert_eq!(env.client.register(&env.ek.key, 0).await.unwrap(), 0);
    assert_eq!(env.client.reset_address(&env.ek.key).await.unwrap(), 1);
    assert_eq!(env.client.register(&env.ek.key, 0).await.unwrap_err(), MailboxError::StaleCounter);
    assert_eq!(env.client.register(&env.ek.key, 3).await.unwrap_err(), MailboxError::StaleCounter, "a reset skips no counter");
    assert_eq!(env.client.ensure_registered(&env.ek.key).await.unwrap(), 1);
    let old = env.client.put_request(&env.address.mailbox, &rand_bytes(), &env.address.put_cap, "abc", &[1; 16]).await;
    assert_eq!(old.unwrap_err(), MailboxError::BadCap, "the old address is cut off");
    let new = ApprovalAddress::derive(&env.base, env.address.workspace_id, &env.fk, 3, &env.ek.key, 1);
    env.client.put_request(&new.mailbox, &rand_bytes(), &new.put_cap, "abc", &[1; 16]).await.unwrap();
    // A register not signed by the mailbox key, or naming another mailbox, is refused.
    let other = MailboxKeys::derive(&[7; 32]);
    let bad = serde_json::json!({
        "authPub": b64::enc(&other.auth().public_key_bytes()),
        "putCapHash": b64::enc(&[0; 32]),
        "counter": 5,
        "sig": b64::enc(&other.auth().sign(b"not the preimage")),
    });
    let r = http.put(format!("{}/v1/mailboxes/{}", env.base, hex(&other.mailbox()))).json(&bad).send().await.unwrap();
    assert!(r.status() == 403 || r.status() == 401, "{}", r.status());
    let r = http.put(format!("{}/v1/mailboxes/{}", env.base, env.address.mailbox_hex())).json(&bad).send().await.unwrap();
    assert_eq!(r.status(), 400, "mailbox mismatch");
}

#[tokio::test]
async fn a_read_key_reads_only_its_own_request_and_members_need_a_fresh_signature() {
    let env = env().await;
    let a = &env.address;
    let (ra, rb): ([u8; 16], [u8; 16]) = (rand_bytes(), rand_bytes());
    let (ka, kb): ([u8; 16], [u8; 16]) = (rand_bytes(), rand_bytes());
    env.client.put_request(&a.mailbox, &ra, &a.put_cap, "abc", &ka).await.unwrap();
    env.client.put_request(&a.mailbox, &rb, &a.put_cap, "abc", &kb).await.unwrap();
    assert!(env.client.get_msgs(MailboxAuth::Requester(&ka), &a.mailbox, &ra, 0, 0).await.is_ok());
    assert_eq!(env.client.get_msgs(MailboxAuth::Requester(&ka), &a.mailbox, &rb, 0, 0).await.unwrap_err(), MailboxError::Unauthorized);
    assert_eq!(env.client.post_msg(MailboxAuth::Requester(&ka), &a.mailbox, &rb, "abc", "members").await.unwrap_err(), MailboxError::Unauthorized);
    assert_eq!(env.client.delete_request(&a.mailbox, &rb, &ka).await.unwrap_err(), MailboxError::Unauthorized);
    // A read key can't list.
    let keys = MailboxKeys::derive(&env.ek.key);
    let wrong_keys = MailboxKeys::derive(&[8; 32]);
    assert_eq!(env.client.list_requests(&keys).await.unwrap().len(), 2);
    let r = reqwest::get(format!("{}/v1/mailboxes/{}/requests", env.base, a.mailbox_hex())).await.unwrap();
    assert_eq!(r.status(), 401);
    // A member signature from 6 minutes ago is refused.
    let skewed = MailboxClient::new(&env.base).with_clock(Arc::new({
        let c = env.clock.clone();
        move || c.now() - 6 * MIN
    }));
    assert_eq!(skewed.list_requests(&keys).await.unwrap_err(), MailboxError::Unauthorized);
    let _ = wrong_keys;
    // Delete with the right key; then it is gone and can't be put again.
    env.client.delete_request(&a.mailbox, &ra, &ka).await.unwrap();
    assert_eq!(env.client.get_msgs(MailboxAuth::Requester(&ka), &a.mailbox, &ra, 0, 0).await.unwrap_err(), MailboxError::RequestUnknown);
    assert_eq!(env.client.put_request(&a.mailbox, &ra, &a.put_cap, "abc", &ka).await.unwrap_err(), MailboxError::RidExists);
}

#[tokio::test]
async fn the_relay_enforces_its_limits() {
    let env = env().await;
    let a = &env.address;
    let keys = MailboxKeys::derive(&env.ek.key);
    // 10 puts a minute per client.
    for _ in 0..10 {
        env.client.put_request(&a.mailbox, &rand_bytes(), &a.put_cap, "abc", &[1; 16]).await.unwrap();
    }
    let r = env.client.put_request(&a.mailbox, &rand_bytes(), &a.put_cap, "abc", &[1; 16]).await.unwrap_err();
    assert!(matches!(r, MailboxError::RateLimited(_)), "{r:?}");
    assert_eq!(r.exit_code(), 7);
    // 64 live requests.
    let mut n = 10;
    while n < 64 {
        env.clock.advance(61_000);
        for _ in 0..10.min(64 - n) {
            env.client.put_request(&a.mailbox, &rand_bytes(), &a.put_cap, "abc", &[1; 16]).await.unwrap();
            n += 1;
        }
    }
    env.clock.advance(61_000);
    assert_eq!(env.client.put_request(&a.mailbox, &rand_bytes(), &a.put_cap, "abc", &[1; 16]).await.unwrap_err(), MailboxError::MailboxFull);
    // No overwrite.
    let first = env.client.list_requests(&keys).await.unwrap()[0].clone();
    let rid: [u8; 16] = hex_fixed(&first.rid).unwrap();
    // 10 messages per request.
    for i in 0..10 {
        env.client.post_msg(MailboxAuth::Member(&keys), &a.mailbox, &rid, &format!("abc{i}"), "members").await.unwrap();
    }
    assert_eq!(env.client.post_msg(MailboxAuth::Member(&keys), &a.mailbox, &rid, "abc99", "members").await.unwrap_err(), MailboxError::TooManyMessages);
    let msgs = env.client.get_msgs(MailboxAuth::Member(&keys), &a.mailbox, &rid, 0, 0).await.unwrap();
    assert_eq!(msgs.iter().map(|m| m.seq).collect::<Vec<_>>(), (1..=10).collect::<Vec<_>>());
    let other: [u8; 16] = hex_fixed(&env.client.list_requests(&keys).await.unwrap()[1].rid).unwrap();
    env.client.post_msg(MailboxAuth::Member(&keys), &a.mailbox, &other, "dup", "members").await.unwrap();
    assert_eq!(env.client.post_msg(MailboxAuth::Member(&keys), &a.mailbox, &other, "dup", "members").await.unwrap_err(), MailboxError::DuplicateMessage);
    assert_eq!(env.client.get_msgs(MailboxAuth::Member(&keys), &a.mailbox, &rid, 7, 0).await.unwrap().len(), 3);
    // Size caps and empty blobs, sent raw past the client's own guard.
    let http = reqwest::Client::new();
    env.clock.advance(61_000);
    let big = "a".repeat(MAX_REQUEST_BLOB + 1);
    let r = http
        .put(format!("{}/v1/mailboxes/{}/requests/{}", env.base, a.mailbox_hex(), hex(&[3u8; 16])))
        .header("X-Ligus-Put-Cap", b64::enc(&a.put_cap))
        .json(&serde_json::json!({ "blob": big, "readKeyHash": b64::enc(&[0; 32]) }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);
    assert_eq!(env.client.put_request(&a.mailbox, &rand_bytes(), &a.put_cap, &big, &[1; 16]).await.unwrap_err(), MailboxError::TooLarge);
    // Expiry after 15 minutes: nothing is listed, and an expired rid can't be put again.
    env.clock.advance(15 * MIN);
    assert!(env.client.list_requests(&keys).await.unwrap().is_empty());
    assert_eq!(env.client.get_msgs(MailboxAuth::Member(&keys), &a.mailbox, &rid, 0, 0).await.unwrap_err(), MailboxError::RequestUnknown);
    assert_eq!(env.client.put_request(&a.mailbox, &rid, &a.put_cap, "abc", &[1; 16]).await.unwrap_err(), MailboxError::RidExists);
}

#[tokio::test]
async fn a_long_poll_returns_when_a_message_arrives() {
    let env = env().await;
    let a = env.address.clone();
    let rid: [u8; 16] = rand_bytes();
    let rk: [u8; 16] = rand_bytes();
    env.client.put_request(&a.mailbox, &rid, &a.put_cap, "abc", &rk).await.unwrap();
    let c = env.client.clone();
    let keys = MailboxKeys::derive(&env.ek.key);
    let waiter = tokio::spawn(async move { c.get_msgs(MailboxAuth::Requester(&rk), &a.mailbox, &rid, 0, 10).await });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let t = std::time::Instant::now();
    env.client.post_msg(MailboxAuth::Member(&keys), &env.address.mailbox, &rid, "abc", "requester").await.unwrap();
    let got = waiter.await.unwrap().unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].from, "member");
    assert!(t.elapsed() < std::time::Duration::from_secs(5));
}

/// A relay without the routes is exit 3 (paste); an unreachable one exit 7.
#[tokio::test]
async fn no_routes_and_offline_are_told_apart() {
    let base = spawn(axum::Router::new().route("/v1/health", axum::routing::get(|| async { "ok" }))).await;
    let c = MailboxClient::new(&base);
    let e = c.ensure_registered(&[1; 32]).await.unwrap_err();
    assert_eq!(e, MailboxError::NoRoutes);
    assert_eq!(e.exit_code(), 3);
    assert_eq!(c.register(&[1; 32], 0).await.unwrap_err(), MailboxError::NoRoutes);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let e = MailboxClient::new(&dead).counter(&[0; 16]).await.unwrap_err();
    assert!(matches!(e, MailboxError::Unreachable(_)), "{e:?}");
    assert_eq!(e.exit_code(), 7);
}

// ---- Check 5 on real workspaces ----

mod on_the_log {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use ligus_core::presence::PresenceAssertion;
    use ligus_core::workspace_config_session_id;

    use super::*;
    use crate::chat_service::{PresenceProvider, PresenceRequest};
    use crate::{ChatService, EventStore, FileKeyVault, IdentityStore, RelayClient, SyncEngine};

    const PASSPHRASE: &str = "code match passphrase";

    struct TestPasskey(Uuid);
    impl PresenceProvider for TestPasskey {
        fn assert(&self, req: &PresenceRequest) -> Result<PresenceAssertion, String> {
            Ok(ligus_core::testkit::test_recovery_key(self.0).assert(&req.challenge))
        }
    }

    struct Dev {
        svc: ChatService,
        dir: PathBuf,
        engine: SyncEngine,
    }

    impl Dev {
        fn open(dir: &Path, name: &str, relay: &str, ws: Uuid) -> Self {
            std::fs::create_dir_all(dir).unwrap();
            let ids = IdentityStore::new(dir, FileKeyVault::new(dir));
            let me = ids.bootstrap(name, &name.to_lowercase(), name).unwrap();
            let mut svc = ChatService::new(
                EventStore::open(dir.join("ligus.db")).unwrap(),
                ids.device_keypair(me.device.id).unwrap(),
                ids.account_keypair(me.account.id).unwrap(),
                me.account.actor(),
            );
            svc.enable_test_presence();
            let key = ligus_core::derive_workspace_key(PASSPHRASE);
            let engine = SyncEngine::new(RelayClient::new(relay), ws.to_string()).with_room_workspace(ws).with_key(key);
            Self { svc, dir: dir.to_path_buf(), engine }
        }
        fn ids(&self) -> IdentityStore<FileKeyVault> {
            IdentityStore::new(&self.dir, FileKeyVault::new(&self.dir))
        }
        fn device_key(&self) -> SigningKeypair {
            self.ids().device_keypair(self.svc.device_id()).unwrap()
        }
        fn ka(&self) -> KeyAgreementKeypair {
            self.ids().key_agreement(self.svc.device_id()).unwrap()
        }
        async fn sync(&mut self) {
            let mut store = EventStore::open(self.dir.join("ligus.db")).unwrap();
            self.engine.push_new(&store).await.unwrap();
            self.engine.pull(&mut store).await.unwrap();
        }
        fn found(&mut self, ws: Uuid) {
            self.svc.ensure_self_member(workspace_config_session_id(ws), ws).unwrap();
            let acct = self.svc.author().account_id.unwrap();
            let key = ligus_core::testkit::test_recovery_key(acct);
            self.svc.register_passkey(ws, key.credential(acct, "Test recovery key"), &|req| Ok(key.assert(&req.challenge))).unwrap();
            self.svc.set_presence_provider(Some(Arc::new(TestPasskey(acct))));
        }
        fn fk(&self) -> String {
            crate::envelope_verifier::FounderPin::fingerprint_hex(&self.svc.account_public_key())
        }
        fn conn(&self, relay: &str, room: &str, inv: Option<Uuid>) -> WorkspaceConn {
            WorkspaceConn {
                name: "Acme".into(),
                relay_url: relay.into(),
                room: room.into(),
                key: Some(PASSPHRASE.into()),
                icon: None,
                dm_account: None,
                founder_actor_id: Some(self.svc.author().id.clone()),
                fk: Some(self.fk()),
                inv: inv.map(|i| i.to_string()),
                ek: Vec::new(),
                sig: None,
            }
        }
        fn identity(&self) -> ApproverIdentity {
            ApproverIdentity {
                signer: Signer::Ed25519(self.device_key()),
                device_name: "Alex's desktop".into(),
                person: ApproverPerson { account_id: self.svc.author().account_id.unwrap(), name: "Alex".into() },
            }
        }
    }

    /// The mailbox at the relay, for workspace key epoch 0 (the passphrase).
    async fn setup(relay: &str, ws: Uuid, fk: &str) -> (MailboxClient, EpochKey, ApprovalAddress) {
        let client = MailboxClient::new(relay);
        let ek = EpochKey { epoch: 0, key: ligus_core::derive_workspace_key(PASSPHRASE) };
        let n = client.ensure_registered(&ek.key).await.unwrap();
        let address = ApprovalAddress::derive(relay, ws, fk, 0, &ek.key, n);
        (client, ek, address)
    }

    /// Check 5 for a box: the Admin's admission, bound to the box, signed by
    /// the device that showed the code, is on the log; another device is refused.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_box_finds_the_admins_admission_on_the_log() {
        let relay = spawn(crate::test_relay::router()).await;
        let tmp = tempfile::tempdir().unwrap();
        let room = "acme";
        let ws = ligus_core::room_workspace_id(room);
        let mut mac = Dev::open(&tmp.path().join("mac"), "Alex", &relay, ws);
        mac.found(ws);
        mac.sync().await;
        let (client, ek, address) = setup(&relay, ws, &mac.fk()).await;

        let mut bx = Dev::open(&tmp.path().join("box"), "build-1", &relay, ws);
        let dk = bx.device_key();
        let ka = bx.ka();
        let jr = JoinRequest::new(bx.svc.author().account_id.unwrap(), bx.svc.device_id(), &dk.public_key_bytes(), &ka.public_key_bytes(), "build-1");
        let spec = RequestSpec { kind: MatchKind::Box, code: jr.encode(), signer: Signer::Ed25519(bx.device_key()), ka: Some(bx.ka()) };
        let mut req = Requester::post(client.clone(), &address, spec, Some(&bx.dir)).await.unwrap();
        let l = list_live(&client, std::slice::from_ref(&ek)).await.unwrap().live.remove(0);
        let mut session = ApproverSession::begin(client.clone(), &ek, &l.request, mac.identity(), &mut SeenSet::in_memory()).await.unwrap();
        let RequesterEvent::Opened { code, .. } = req.poll(0).await.unwrap().remove(0) else { panic!() };
        assert_eq!(session.await_reveal(0).await.unwrap().unwrap().code40, code.code40);
        // The Admin confirms: the admission for the request's keys, with the passkey.
        let RequestSubject::Box(parsed) = &session.request().subject else { panic!() };
        let (inv, seed) = mac.svc.issue_box_admission(ws, crate::box_admission::DEFAULT_TTL_MS, Some(parsed)).unwrap();
        let invite = crate::box_admission::with_redeem_key(&encode_workspace_invite(&mac.conn(&relay, room, Some(inv))), &seed).unwrap();
        session.finish_approved(Some(&parsed.seal(&invite).unwrap())).await.unwrap();
        let RequesterEvent::Result(r) = req.poll(0).await.unwrap().remove(0) else { panic!() };
        let checked = req.check_grant(&r, GrantTarget::Box { ka: &ka }, crate::human_presence::now_ms()).unwrap();
        let CheckedGrant::Box { conn, .. } = &checked else { panic!() };
        let (founder, fk) = conn.founder_pin().unwrap();
        bx.svc.store().set_workspace_pin(ws, &founder, &fk).unwrap();
        let pk = dk.public_key_bytes();
        let la = req.log_approval(&checked, &pk, crate::human_presence::now_ms()).unwrap();
        // Before the Admin syncs, it isn't there yet.
        bx.sync().await;
        assert!(matches!(bx.svc.check_approval_on_log(&la), LogVerdict::NotYet(_)), "{:?}", bx.svc.check_approval_on_log(&la));
        mac.sync().await;
        bx.sync().await;
        assert_eq!(bx.svc.check_approval_on_log(&la), LogVerdict::Verified { account_id: mac.svc.author().account_id.unwrap(), name: "Alex".into() });
        // Claimed by another device (a racing open): refused.
        let other = SigningKeypair::generate().unwrap();
        let racing = LogApproval {
            approver_device_id: device_id_for(SigAlg::Ed25519, &other.public_key_bytes()),
            approver_sig_key: &other.public_key_bytes(),
            ..req.log_approval(&checked, &pk, 0).unwrap()
        };
        assert!(matches!(bx.svc.check_approval_on_log(&racing), LogVerdict::Refused(_)));
        // Bound to another device: refused.
        let wrong_box = LogApproval { subject: LogSubject::Box { invite_id: inv, device_key: &[3; 32] }, ..req.log_approval(&checked, &pk, 0).unwrap() };
        assert!(matches!(bx.svc.check_approval_on_log(&wrong_box), LogVerdict::Refused(_)));
        // A fork's founder pin: refused.
        let fork = LogApproval { founder_pin: "00000000000000000000000000000000", ..req.log_approval(&checked, &pk, 0).unwrap() };
        assert!(matches!(bx.svc.check_approval_on_log(&fork), LogVerdict::Refused(_)));
        req.settle().await.unwrap();
    }

    /// Check 5 for a computer: the certificate the desktop published, under
    /// the grant whose approver showed the code.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_computer_finds_its_certificate_on_the_log() {
        let relay = spawn(crate::test_relay::router()).await;
        let tmp = tempfile::tempdir().unwrap();
        let room = "acme";
        let ws = ligus_core::room_workspace_id(room);
        let mut mac = Dev::open(&tmp.path().join("mac"), "Alex", &relay, ws);
        mac.found(ws);
        mac.sync().await;
        let (client, ek, address) = setup(&relay, ws, &mac.fk()).await;

        let mut lt = Dev::open(&tmp.path().join("lt"), "lt on mbp", &relay, ws);
        let now = crate::human_presence::now_ms();
        let link = LinkRequest::new(&lt.device_key(), &lt.ka().public_key_bytes(), "lt on mbp", now, None);
        let spec = RequestSpec { kind: MatchKind::Computer, code: link.encode(), signer: Signer::Ed25519(lt.device_key()), ka: Some(lt.ka()) };
        let mut req = Requester::post(client.clone(), &address, spec, Some(&lt.dir)).await.unwrap();
        let l = list_live(&client, std::slice::from_ref(&ek)).await.unwrap().live.remove(0);
        let mut session = ApproverSession::begin(client.clone(), &ek, &l.request, mac.identity(), &mut SeenSet::in_memory()).await.unwrap();
        let RequesterEvent::Opened { code, claimed } = req.poll(0).await.unwrap().remove(0) else { panic!() };
        assert_eq!(claimed.name, "Alex");
        assert_eq!(session.await_reveal(0).await.unwrap().unwrap().code40, code.code40);
        let RequestSubject::Computer(parsed) = session.request().subject.clone() else { panic!() };
        let parsed = &parsed;
        let conn_code = encode_workspace_invite(&mac.conn(&relay, room, None));
        let grant = mac.svc.prepare_computer_link(ws, parsed, &conn_code, now).unwrap();
        session.offer(grant).await.unwrap();
        assert!(matches!(req.poll(0).await.unwrap()[0], RequesterEvent::Offered { .. }));
        req.ack().await.unwrap();
        assert!(session.await_ack(0).await.unwrap());
        // Only now: the passkey and the certificate.
        mac.svc.publish_computer_link(ws, parsed, &conn_code, now).unwrap();
        session.finish_approved(None).await.unwrap();
        let RequesterEvent::Result(r) = req.poll(0).await.unwrap().remove(0) else { panic!() };
        let ka = lt.ka();
        let pk = lt.device_key().public_key_bytes();
        let checked = req.check_grant(&r, GrantTarget::Computer { request: &link, ka: &ka, device_key: &pk }, now).unwrap();
        let CheckedGrant::Computer { grant: g, .. } = &checked else { panic!() };
        let (founder, fk) = g.conn.founder_pin().unwrap();
        lt.svc.store().set_workspace_pin(ws, &founder, &fk).unwrap();
        lt.svc.set_trusted_founder(Some(founder));
        let la = req.log_approval(&checked, &pk, now).unwrap();
        lt.sync().await;
        assert!(matches!(lt.svc.check_approval_on_log(&la), LogVerdict::NotYet(_)));
        mac.sync().await;
        lt.sync().await;
        assert!(matches!(lt.svc.check_approval_on_log(&la), LogVerdict::Verified { ref name, .. } if name == "Alex"));
        let other = SigningKeypair::generate().unwrap();
        let racing = LogApproval { approver_sig_key: &other.public_key_bytes(), ..req.log_approval(&checked, &pk, now).unwrap() };
        assert!(matches!(lt.svc.check_approval_on_log(&racing), LogVerdict::Refused(_)));
        let v = retry_log_check(|| async { lt.svc.check_approval_on_log(&la) }, std::time::Duration::from_secs(1), std::time::Duration::from_millis(10)).await;
        assert!(matches!(v, LogVerdict::Verified { .. }));
        req.settle().await.unwrap();
    }
}

// ---- Regressions from the security review ----
mod review_regressions {
    use super::*;

    /// A request's label is bounded and clean everywhere: the box codec
    /// refuses a label that isn't its own clamp, so neither the requester
    /// nor an approver takes one; every name hint is cut to 64 characters.
    #[tokio::test]
    async fn a_requests_label_and_name_hints_are_bounded() {
        let env = env().await;
        let mut b = BoxDev::new("x");
        b.request.label = format!("build-1 is asking. Check the box shows the same code: K7Q4 M9X2. {}", "A".repeat(3000));
        assert!(matches!(Requester::post(env.client.clone(), &env.address, b.spec(), None).await, Err(RequesterError::BadSpec(_))));
        // A hand-built request with that label: the approver refuses it.
        let rid: [u8; 16] = rand_bytes();
        let body = RequestBody {
            v: 1,
            kind: MatchKind::Box,
            rid: hex(&rid),
            mailbox: env.address.mailbox_hex(),
            code: b.request.encode(),
            requester_ka: b64::enc(&b.ka.public_key_bytes()),
            read_key: b64::enc(&[1u8; 16]),
            commit: b64::enc(&[2u8; 32]),
            created_at: env.clock.now(),
        };
        let plain = sign_message(MsgType::Request, &body, &b.spec().signer);
        assert!(matches!(VerifiedRequest::verify(plain.as_bytes(), &rid, &env.address.mailbox, env.clock.now()), Err(MsgError::BadCode(_))));
        assert!(hint(&"A".repeat(3000)).chars().count() <= MAX_HINT_CHARS);
        assert_eq!(hint("Ana\u{202E}s desktop"), "Anas desktop");
        // A claim's approver name is a bounded hint too.
        let ok = BoxDev::new("build-1");
        let req = Requester::post(env.client.clone(), &env.address, ok.spec(), None).await.unwrap();
        let l = live(&env, req.rid()).await;
        let (mut me, _) = approver("x");
        me.device_name = "B".repeat(500);
        ApproverSession::begin(env.client.clone(), &env.ek, &l.request, me, &mut SeenSet::in_memory()).await.unwrap();
        let l = live(&env, req.rid()).await;
        assert!(l.claims[0].approver_name.chars().count() <= MAX_HINT_CHARS);
    }

    /// A key holder who can't approve races its own open in after an
    /// honest claim: every sheet reads contested.
    #[tokio::test]
    async fn a_racing_open_after_an_honest_claim_is_contested() {
        let env = env().await;
        let b = BoxDev::new("build-1");
        let mut req = Requester::post(env.client.clone(), &env.address, b.spec(), None).await.unwrap();
        let l = live(&env, req.rid()).await;
        let keys = MailboxKeys::derive(&env.ek.key);
        let (admin, _) = approver("Alex");
        let admin_dev = admin.signer.device_id();
        let claim = sign_message(
            MsgType::Claim,
            &ClaimBody { rid: hex(&req.rid()), approver_device_id: admin_dev, approver_name: "Alex's desktop".into(), at: env.client.now() },
            &admin.signer,
        );
        env.client
            .post_msg(MailboxAuth::Member(&keys), &env.address.mailbox, &req.rid(), &seal_blob(&keys.seal().public_key_bytes(), claim.as_bytes()).unwrap(), "members")
            .await
            .unwrap();
        // The Admin's own open, then the Viewer's racing one (no claim).
        let (viewer, _) = approver("Vic");
        let vka = KeyAgreementKeypair::generate().unwrap();
        let open = sign_message(
            MsgType::Open,
            &OpenBody {
                rid: hex(&req.rid()),
                approver_device_id: viewer.signer.device_id(),
                approver_sig_key: b64::enc(&viewer.signer.public_key()),
                approver_ka: b64::enc(&vka.public_key_bytes()),
                approver_person: viewer.person.clone(),
                n_a: b64::enc(&[9u8; 32]),
                at: env.client.now(),
            },
            &viewer.signer,
        );
        env.client
            .post_msg(MailboxAuth::Member(&keys), &env.address.mailbox, &req.rid(), &seal_blob(&l.request.requester_ka, open.as_bytes()).unwrap(), "requester")
            .await
            .unwrap();
        env.client
            .post_msg(MailboxAuth::Member(&keys), &env.address.mailbox, &req.rid(), &seal_blob(&l.request.requester_ka, b"admin's open").unwrap(), "requester")
            .await
            .unwrap();
        let ev = req.poll(0).await.unwrap();
        let RequesterEvent::Opened { claimed, .. } = &ev[0] else { panic!() };
        assert_eq!(claimed.device_id, viewer.signer.device_id());
        let l = live(&env, req.rid()).await;
        let view = claim_view(&l, Uuid::new_v4(), &Classes(admin_dev));
        assert_eq!(view, ClaimView::Contested { by: None });
        // The claimer's own view too, once no reveal reached it in 30 s.
        let mut mine = l.clone();
        mine.requester_msgs_before_reveal = 1;
        mine.revealed = false;
        mine.listed_at = mine.claims[0].at + REVEAL_WAIT_MS + 1;
        assert_eq!(claim_view(&mine, admin_dev, &Classes(admin_dev)), ClaimView::Contested { by: None });
        mine.listed_at = mine.claims[0].at + 1_000;
        assert_eq!(claim_view(&mine, admin_dev, &Classes(admin_dev)), ClaimView::Mine);
    }

    /// Check 3 pins the relay: a grant naming another relay than the
    /// address is refused (box and computer), unless the operator overrides it.
    #[tokio::test]
    async fn a_grant_pointing_at_another_relay_is_refused() {
        let env = env().await;
        let b = BoxDev::new("build-1");
        let (me, _) = approver("Ana");
        let (mut req, _s, _, _) = exchange(&env, b.spec(), me, &mut SeenSet::in_memory()).await;
        let now = env.clock.now();
        let rid = req.rid_hex();
        let mut c = conn(&env, &env.room, &env.fk, Some(Uuid::new_v4()), 3);
        c.relay_url = "https://attacker.example".into();
        let grant = b.request.seal(&crate::box_admission::with_redeem_key(&encode_workspace_invite(&c), &[9; 32]).unwrap()).unwrap();
        assert_eq!(req.check_grant(&approved(&rid, &grant), GrantTarget::Box { ka: &b.ka }, now).unwrap_err(), GrantError::WrongRelay);
        req.allow_relay_override(true);
        assert!(req.check_grant(&approved(&rid, &grant), GrantTarget::Box { ka: &b.ka }, now).is_ok());
        // A trailing slash or another case is the same relay.
        assert!(super::super::requester::same_relay("HTTPS://Relay.Example/", "https://relay.example"));
        assert!(!super::super::requester::same_relay("https://relay.example", "https://relay.example:8443"));

        let c2 = Computer::new(env.clock.now());
        let (me, desk) = approver("Ana");
        let (mut req, _s, _, _) = exchange(&env, c2.spec(), me, &mut SeenSet::in_memory()).await;
        let mut wc = conn(&env, &env.room, &env.fk, None, 3);
        wc.relay_url = "https://attacker.example".into();
        let id = device_id_for(SigAlg::Ed25519, &desk.public_key_bytes());
        let g = Grant::seal(&c2.request, ligus_core::room_workspace_id(&env.room), &encode_workspace_invite(&wc), &person(), id, &desk, now + 30 * MIN).unwrap();
        req.set_acked_for_tests(&super::super::envelope::grant_digest(&g));
        let pk = c2.key.public_key_bytes();
        let target = GrantTarget::Computer { request: &c2.request, ka: &c2.ka, device_key: &pk };
        assert_eq!(req.check_grant(&approved(&req.rid_hex(), &g), target, now).unwrap_err(), GrantError::WrongRelay);
    }

    /// Nothing may be written once the code expired or the request was
    /// ended: `ready_to_commit` says so before the passkey.
    #[tokio::test]
    async fn ready_to_commit_refuses_an_expired_code_an_abort_and_a_late_ack() {
        let env = env().await;
        let (me, _) = approver("Ana");
        let (_req, mut s, _, _) = exchange(&env, BoxDev::new("build-1").spec(), me, &mut SeenSet::in_memory()).await;
        s.ready_to_commit().await.unwrap();
        env.clock.advance(mc::CODE_TTL_MS + 1_000);
        assert_eq!(s.ready_to_commit().await.unwrap_err(), ApproverError::CodeExpired);

        let env = super::env().await;
        let (me, _) = approver("Ana");
        let (mut req, mut s, _, _) = exchange(&env, BoxDev::new("build-1").spec(), me, &mut SeenSet::in_memory()).await;
        req.abort(AbortReason::Cancelled).await.unwrap();
        assert_eq!(s.ready_to_commit().await.unwrap_err(), ApproverError::Aborted(AbortReason::Cancelled));

        let env = super::env().await;
        let c = Computer::new(env.clock.now());
        let (me, desk) = approver("Ana");
        let (mut req, mut s, _, _) = exchange(&env, c.spec(), me, &mut SeenSet::in_memory()).await;
        s.offer(computer_grant(&env, &c.request, &desk, &env.room, &env.fk, 3, env.clock.now())).await.unwrap();
        assert_eq!(s.ready_to_commit().await.unwrap_err(), ApproverError::NotAcknowledged);
        req.poll(0).await.unwrap();
        req.ack().await.unwrap();
        s.ready_to_commit().await.unwrap();
        assert!(s.acknowledged(), "ready_to_commit reads the ack too");
        env.clock.advance(ACK_WINDOW_MS + 1_000);
        assert_eq!(s.ready_to_commit().await.unwrap_err(), ApproverError::DidntFinish);
    }

    /// The serve box between a phone and the relay (M6c, box side): it can't
    /// rewrite the request's bytes, its commit or the phone's sealed open
    /// without the phone or the requester noticing.
    #[tokio::test]
    async fn a_box_in_the_middle_cant_rewrite_what_it_relays() {
        let env = env().await;
        let b = BoxDev::new("build-1");
        let req = Requester::post(env.client.clone(), &env.address, b.spec(), None).await.unwrap();
        let l = live(&env, req.rid()).await;
        let now = env.clock.now();
        let plain = l.request.envelope.clone();
        // The request the box hands the phone: any rewritten byte of the
        // signed body (the code, the commit, the key-agreement key) fails.
        let v: serde_json::Value = serde_json::from_str(&plain).unwrap();
        let body = v["body"].as_str().unwrap().to_string();
        for (from, to) in [(l.request.code.as_str(), BoxDev::new("build-1").request.encode()), (&b64::enc(&l.request.commit), b64::enc(&[0u8; 32]))] {
            let mut forged = v.clone();
            forged["body"] = serde_json::Value::String(body.replace(from, &to));
            assert_eq!(VerifiedRequest::verify(forged.to_string().as_bytes(), &l.request.rid, &l.request.mailbox, now).unwrap_err(), MsgError::BadSignature);
        }
        // An open the phone sealed to the requester: a flipped byte doesn't open.
        let (phone, _) = approver("Ana");
        let open = sign_message(
            MsgType::Open,
            &OpenBody {
                rid: l.rid_hex(),
                approver_device_id: phone.signer.device_id(),
                approver_sig_key: b64::enc(&phone.signer.public_key()),
                approver_ka: b64::enc(&[5u8; 32]),
                approver_person: phone.person.clone(),
                n_a: b64::enc(&[6u8; 32]),
                at: now,
            },
            &phone.signer,
        );
        let blob = seal_blob(&l.request.requester_ka, open.as_bytes()).unwrap();
        let mut raw = b64::dec(&blob).unwrap();
        raw[40] ^= 1;
        assert!(open_blob(&b.ka, &b64::enc(&raw)).is_none());
        assert!(open_blob(&b.ka, &blob).is_some());
        // And a re-signed open (the box's own key, another nA) is the box's,
        // not the phone's: its device id names the box's key.
        let (boxkey, _) = approver("Ana");
        let mut body: OpenBody = serde_json::from_str(serde_json::from_str::<serde_json::Value>(&open).unwrap()["body"].as_str().unwrap()).unwrap();
        body.n_a = b64::enc(&[7u8; 32]);
        let forged = sign_message(MsgType::Open, &body, &boxkey.signer);
        assert_eq!(super::super::envelope::verify_open(&Verified::parse(forged.as_bytes()).unwrap(), &l.request.rid).unwrap_err(), MsgError::WrongKey);
    }
}

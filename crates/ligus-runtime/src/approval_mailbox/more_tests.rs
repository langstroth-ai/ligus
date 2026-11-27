//! A computer link that brings all the person's workspaces ([`super::more`]):
//! the wire stays byte-for-byte what it was for one workspace, older peers on
//! either side link one workspace, and through the test relay a person in
//! three workspaces links a computer into all three as the same member, or
//! into the ones whose passkey went through.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ligus_core::crypto::SigningKeypair;
use ligus_core::e2ee::KeyAgreementKeypair;
use ligus_core::match_code::MatchKind;
use ligus_core::presence::PresenceAssertion;
use ligus_core::workspace_config_session_id;
use uuid::Uuid;

use super::envelope::sign_message;
use super::more::{AckWire, OfferWire, ResultWire};
use super::*;
use crate::chat_service::{PresenceProvider, PresenceRequest};
use crate::computer_link::LinkRequest;
use crate::workspace_invite::{encode_workspace_invite, WorkspaceConn};
use crate::{ChatService, EventStore, FileKeyVault, IdentityStore, RelayClient, SyncEngine};

const PASSPHRASE: &str = "more workspaces passphrase";
const ROOMS: [&str; 3] = ["acme", "beta", "gamma"];

fn now() -> i64 {
    crate::human_presence::now_ms()
}

// ---- The wire ----

/// With nothing more, the offer, the ack and the result are the bytes an
/// older build signs; with more, an older build still reads its own fields.
#[test]
fn one_workspace_keeps_its_bytes_and_older_builds_read_the_new_fields() {
    let signer = || Signer::Ed25519(SigningKeypair::from_seed(&[7; 32]).unwrap());
    let offer = OfferBody {
        rid: "ab".repeat(16),
        grant_digest: "cd".repeat(32),
        person: ApproverPerson { account_id: Uuid::from_u128(1), name: "Ana".into() },
        approver_device_id: Uuid::from_u128(2),
    };
    assert_eq!(sign_message(MsgType::Offer, &OfferWire { base: &offer, more: &[] }, &signer()), sign_message(MsgType::Offer, &offer, &signer()));
    let ack = AckBody { rid: "ab".repeat(16), grant_digest: "cd".repeat(32) };
    assert_eq!(sign_message(MsgType::Ack, &AckWire { base: &ack, more: &[] }, &signer()), sign_message(MsgType::Ack, &ack, &signer()));
    let result = ResultBody { rid: "ab".repeat(16), outcome: Outcome::Approved, grant: Some("ligusgrant1:x".into()), grant_digest: Some("cd".repeat(32)) };
    assert_eq!(
        sign_message(MsgType::Result, &ResultWire { base: &result, more_grants: &[] }, &signer()),
        sign_message(MsgType::Result, &result, &signer())
    );

    // An older build parses the bodies it knows and ignores the rest.
    let more = [MoreOffer { workspace_id: Uuid::from_u128(3), name: "Beta".into(), grant_digest: "ef".repeat(32) }];
    let v = Verified::parse(sign_message(MsgType::Offer, &OfferWire { base: &offer, more: &more }, &signer()).as_bytes()).unwrap();
    assert!(v.body.contains("\"more\":[{\"workspaceId\""), "{}", v.body);
    assert_eq!(serde_json::from_str::<OfferBody>(&v.body).unwrap(), offer);
    assert_eq!(super::more::offer_more(&v, None), more);
    assert!(super::more::offer_more(&v, Some(Uuid::from_u128(3))).is_empty(), "the request's own workspace is never one more");
    let digests = ["ef".repeat(32), "not hex".into()];
    let v = Verified::parse(sign_message(MsgType::Ack, &AckWire { base: &ack, more: &digests }, &signer()).as_bytes()).unwrap();
    assert_eq!(serde_json::from_str::<AckBody>(&v.body).unwrap(), ack);
    assert_eq!(super::more::ack_more(&v), vec!["ef".repeat(32)], "a malformed digest is dropped");
    // A grant travels compact and comes back as the exact text its digest names.
    let link = LinkRequest::new(&SigningKeypair::generate().unwrap(), &KeyAgreementKeypair::generate().unwrap().public_key_bytes(), "lt", now(), None);
    let mut ana = ligus_core::ActorIdentity::new(Uuid::from_u128(9).to_string(), "Ana", ligus_core::ActorKind::Human);
    ana.account_id = Some(Uuid::from_u128(9));
    let desk = SigningKeypair::from_seed(&[7; 32]).unwrap();
    let id = ligus_core::crypto::device_id_for(ligus_core::crypto::SigAlg::Ed25519, &desk.public_key_bytes());
    let grant = crate::computer_link::Grant::seal(&link, Uuid::from_u128(3), "hivews1:x", &ana, id, &desk, 1).unwrap();
    let small = super::more::compact(&grant).unwrap();
    assert!(small.len() * 3 < grant.len(), "{} vs {}", small.len(), grant.len());
    assert_eq!(super::more::expand(&small).as_deref(), Some(grant.as_str()));
    assert_eq!(super::more::compact("ligusgrant1:nope"), None);
    let grants = [small, "%%".to_string()];
    let v = Verified::parse(sign_message(MsgType::Result, &ResultWire { base: &result, more_grants: &grants }, &signer()).as_bytes()).unwrap();
    assert!(v.body.contains("\"moreGrants\":["));
    assert_eq!(serde_json::from_str::<ResultBody>(&v.body).unwrap(), result);
    assert_eq!(super::more::result_more(&v), vec![grant], "a damaged entry is dropped");
}

// ---- Through the test relay, with real folded workspaces ----

/// A passkey the test can make the person cancel.
struct Passkey {
    account: Uuid,
    refuse: Arc<AtomicBool>,
}

impl PresenceProvider for Passkey {
    fn assert(&self, req: &PresenceRequest) -> Result<PresenceAssertion, String> {
        if self.refuse.load(Ordering::SeqCst) {
            return Err("the passkey sheet was cancelled".into());
        }
        Ok(ligus_core::testkit::test_recovery_key(self.account).assert(&req.challenge))
    }
}

struct Dev {
    svc: ChatService,
    dir: PathBuf,
    relay: String,
    refuse: Arc<AtomicBool>,
}

impl Dev {
    fn open(dir: &Path, name: &str, relay: &str) -> Self {
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
        Self { svc, dir: dir.to_path_buf(), relay: relay.into(), refuse: Arc::new(AtomicBool::new(false)) }
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
    /// Found `room` with a passkey (rules ask presence for a new device).
    fn found(&mut self, room: &str, passkey: bool) {
        let ws = ligus_core::room_workspace_id(room);
        self.svc.ensure_self_member(workspace_config_session_id(ws), ws).unwrap();
        let acct = self.svc.author().account_id.unwrap();
        if passkey {
            let key = ligus_core::testkit::test_recovery_key(acct);
            self.svc.register_passkey(ws, key.credential(acct, "Test recovery key"), &|req| Ok(key.assert(&req.challenge))).unwrap();
        }
        self.svc.set_presence_provider(Some(Arc::new(Passkey { account: acct, refuse: self.refuse.clone() })));
    }
    fn fk(&self) -> String {
        crate::envelope_verifier::FounderPin::fingerprint_hex(&self.svc.account_public_key())
    }
    fn conn(&self, room: &str) -> WorkspaceConn {
        WorkspaceConn {
            name: room[..1].to_uppercase() + &room[1..],
            relay_url: self.relay.clone(),
            room: room.into(),
            key: Some(PASSPHRASE.into()),
            icon: None,
            dm_account: None,
            founder_actor_id: Some(self.svc.author().id.clone()),
            fk: Some(self.fk()),
            inv: None,
            ek: Vec::new(),
            sig: None,
        }
    }
    fn conn_code(&self, room: &str) -> String {
        encode_workspace_invite(&self.conn(room))
    }
    async fn sync(&self, room: &str) {
        let ws = ligus_core::room_workspace_id(room);
        let mut engine = SyncEngine::new(RelayClient::new(&self.relay), room.to_string())
            .with_room_workspace(ws)
            .with_key(ligus_core::derive_workspace_key(PASSPHRASE));
        let mut store = EventStore::open(self.dir.join("ligus.db")).unwrap();
        engine.push_new(&store).await.unwrap();
        engine.pull(&mut store).await.unwrap();
    }
    fn identity(&self) -> ApproverIdentity {
        ApproverIdentity {
            signer: Signer::Ed25519(self.device_key()),
            device_name: "Ana's desktop".into(),
            person: ApproverPerson { account_id: self.svc.author().account_id.unwrap(), name: "Ana".into() },
        }
    }
    fn human_members(&self, room: &str) -> Vec<String> {
        let ws = ligus_core::room_workspace_id(room);
        let cfg = self.svc.load_in(ws, workspace_config_session_id(ws)).unwrap().unwrap();
        cfg.members.iter().filter(|m| m.actor.kind == ligus_core::ActorKind::Human).map(|m| m.id.clone()).collect()
    }
}

async fn spawn() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, crate::test_relay::router()).await.unwrap() });
    format!("http://{addr}")
}

/// The person's desktop with `rooms` (each with a passkey unless listed in
/// `no_passkey`), synced; the first room's mailbox and address.
async fn desktop(relay: &str, dir: &Path, rooms: &[&str], no_passkey: &[&str]) -> (Dev, MailboxClient, EpochKey, ApprovalAddress) {
    let mut mac = Dev::open(dir, "Ana", relay);
    for room in rooms {
        mac.found(room, !no_passkey.contains(room));
        mac.sync(room).await;
    }
    let client = MailboxClient::new(relay);
    let ek = EpochKey { epoch: 0, key: ligus_core::derive_workspace_key(PASSPHRASE) };
    let n = client.ensure_registered(&ek.key).await.unwrap();
    let address = ApprovalAddress::derive(relay, ligus_core::room_workspace_id(rooms[0]), &mac.fk(), 0, &ek.key, n);
    (mac, client, ek, address)
}

struct Linking {
    req: Requester,
    session: ApproverSession,
    link: LinkRequest,
}

/// The computer posts to the first room's address, the desktop opens it and
/// both see the code.
async fn open_link(lt: &Dev, mac: &Dev, client: &MailboxClient, ek: &EpochKey, address: &ApprovalAddress) -> Linking {
    let link = LinkRequest::new(&lt.device_key(), &lt.ka().public_key_bytes(), "lt on mbp", now(), None);
    let spec = RequestSpec { kind: MatchKind::Computer, code: link.encode(), signer: Signer::Ed25519(lt.device_key()), ka: Some(lt.ka()) };
    let mut req = Requester::post(client.clone(), address, spec, Some(&lt.dir)).await.unwrap();
    let l = list_live(client, std::slice::from_ref(ek)).await.unwrap().live.remove(0);
    let mut session = ApproverSession::begin(client.clone(), ek, &l.request, mac.identity(), &mut SeenSet::in_memory()).await.unwrap();
    let RequesterEvent::Opened { code, .. } = req.poll(0).await.unwrap().remove(0) else { panic!("opened") };
    assert_eq!(session.await_reveal(0).await.unwrap().unwrap().code40, code.code40);
    Linking { req, session, link }
}

/// The desktop's offer: the first room's grant and one more for every other
/// room it can add the computer to. Returns the refusals by room.
async fn offer_all(mac: &mut Dev, l: &mut Linking, rooms: &[&str]) -> Vec<(Uuid, String)> {
    let first = ligus_core::room_workspace_id(rooms[0]);
    let grant = mac.svc.prepare_computer_link(first, &l.link, &mac.conn_code(rooms[0]), now()).unwrap();
    let others: Vec<(Uuid, String, String)> =
        rooms[1..].iter().map(|r| (ligus_core::room_workspace_id(r), mac.conn(r).name, mac.conn_code(r))).collect();
    let prepared = mac.svc.prepare_more_computer_links(&l.link, &others, now());
    let mut refused = Vec::new();
    let mut more = Vec::new();
    for (ws, r) in prepared {
        match r {
            Ok(g) => more.push(g),
            Err(why) => refused.push((ws, why)),
        }
    }
    let left_out = l.session.offer_with(grant, more).await.unwrap();
    assert!(left_out.is_empty(), "three workspaces fit in one result");
    refused
}

/// The computer's checks 1 to 4 on every grant, then check 5 on each
/// workspace's own log; the verified workspaces and the refusals.
async fn redeem_all(lt: &mut Dev, mac: &Dev, l: &mut Linking, rooms: &[&str]) -> (Vec<(Uuid, ligus_core::ActorIdentity)>, Vec<(Uuid, GrantError)>) {
    let RequesterEvent::Result(r) = l.req.poll(0).await.unwrap().remove(0) else { panic!("a result") };
    let ka = lt.ka();
    let pk = lt.device_key().public_key_bytes();
    let target = || GrantTarget::Computer { request: &l.link, ka: &ka, device_key: &pk };
    let first = l.req.check_grant(&r, target(), now()).unwrap();
    let more = l.req.check_more_grants(&first, target(), now());
    let mut grants = vec![first];
    let mut failed = Vec::new();
    for m in more {
        match m.outcome {
            Ok(g) => grants.push(g),
            Err(e) => failed.push((m.workspace_id, e)),
        }
    }
    for room in rooms {
        mac.sync(room).await;
    }
    let mut verified = Vec::new();
    for g in &grants {
        let CheckedGrant::Computer { grant, .. } = g else { panic!() };
        let (founder, fk) = grant.conn.founder_pin().unwrap();
        lt.svc.store().set_workspace_pin(grant.workspace_id, &founder, &fk).unwrap();
        lt.sync(&grant.conn.room).await;
        lt.svc.set_trusted_founder(Some(founder));
        let person = lt.svc.check_computer_link(grant, now()).unwrap();
        verified.push((grant.workspace_id, person));
    }
    (verified, failed)
}

/// The e2e: one link, three workspaces, the same member in each.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_link_brings_all_three_workspaces_as_the_same_member() {
    let relay = spawn().await;
    let tmp = tempfile::tempdir().unwrap();
    let (mut mac, client, ek, address) = desktop(&relay, &tmp.path().join("mac"), &ROOMS, &[]).await;
    let ana = mac.svc.author().clone();
    let mut lt = Dev::open(&tmp.path().join("lt"), "lt on mbp", &relay);
    let mut l = open_link(&lt, &mac, &client, &ek, &address).await;

    assert!(offer_all(&mut mac, &mut l, &ROOMS).await.is_empty(), "the desktop can add the computer to all three");
    assert!(matches!(l.req.poll(0).await.unwrap()[0], RequesterEvent::Offered { .. }));
    let offered: Vec<&str> = l.req.offered_more().iter().map(|m| m.name.as_str()).collect();
    assert_eq!(offered, ["Beta", "Gamma"], "the computer can say: Linking 3 workspaces");
    l.req.ack_all().await.unwrap();
    assert!(l.session.await_ack(0).await.unwrap());
    let acked = l.session.acked_more();
    assert_eq!(acked.len(), 2);

    // The first workspace's passkey, then one for each more (1 of 3, 2 of 3, 3 of 3).
    let first = ligus_core::room_workspace_id(ROOMS[0]);
    mac.svc.publish_computer_link(first, &l.link, &mac.conn_code(ROOMS[0]), now()).unwrap();
    let mut published = Vec::new();
    for m in &acked {
        l.session.still_open().await.unwrap();
        let room = ROOMS.iter().find(|r| ligus_core::room_workspace_id(r) == m.workspace_id).unwrap();
        mac.svc.publish_computer_link_in(m.workspace_id, &l.link, &mac.conn_code(room), now()).unwrap();
        published.push(m.workspace_id);
    }
    l.session.finish_approved_with(&published).await.unwrap();

    let (verified, failed) = redeem_all(&mut lt, &mac, &mut l, &ROOMS).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(verified.len(), 3);
    for (_, person) in &verified {
        assert_eq!((person.id.as_str(), person.account_id), (ana.id.as_str(), ana.account_id), "the same person everywhere");
    }
    // The computer acts as Ana and accepts its certificate in each.
    lt.svc.act_as_owner_device(verified[0].1.clone());
    let lt_device = lt.svc.device_id();
    for room in ROOMS {
        let ws = ligus_core::room_workspace_id(room);
        lt.svc.set_trusted_founder(Some(ana.id.clone()));
        assert!(lt.svc.accept_device_certificate(ws).unwrap(), "{room}: accepted");
        lt.sync(room).await;
        mac.sync(room).await;
        assert_eq!(mac.human_members(room), vec![ana.id.clone()], "{room}: never a second member");
        let mine = mac.svc.own_devices(ws).unwrap();
        assert!(mine.iter().any(|d| d.device_id == lt_device && !d.revoked), "{room}: one of Ana's devices: {mine:?}");
    }
    l.req.settle().await.unwrap();
}

/// A passkey cancelled in one workspace and no passkey in another: the
/// computer links the rest, and knows which didn't come and why.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancelled_passkey_or_a_workspace_the_desktop_cant_add_to_leaves_the_rest_linked() {
    let relay = spawn().await;
    let tmp = tempfile::tempdir().unwrap();
    let rooms = ["acme", "beta", "gamma", "delta"];
    let (mut mac, client, ek, address) = desktop(&relay, &tmp.path().join("mac"), &rooms, &["delta"]).await;
    let mut lt = Dev::open(&tmp.path().join("lt"), "lt on mbp", &relay);
    let mut l = open_link(&lt, &mac, &client, &ek, &address).await;

    let refused = offer_all(&mut mac, &mut l, &rooms).await;
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0].0, ligus_core::room_workspace_id("delta"));
    assert!(refused[0].1.contains("passkey"), "said plainly: {}", refused[0].1);
    l.req.poll(0).await.unwrap();
    assert_eq!(l.req.offered_more().len(), 2, "only what the desktop can add is offered");
    l.req.ack_all().await.unwrap();
    assert!(l.session.await_ack(0).await.unwrap());

    let first = ligus_core::room_workspace_id(rooms[0]);
    mac.svc.publish_computer_link(first, &l.link, &mac.conn_code(rooms[0]), now()).unwrap();
    let beta = ligus_core::room_workspace_id("beta");
    let gamma = ligus_core::room_workspace_id("gamma");
    mac.svc.publish_computer_link_in(beta, &l.link, &mac.conn_code("beta"), now()).unwrap();
    // The person cancels gamma's passkey: nothing is written there.
    mac.refuse.store(true, Ordering::SeqCst);
    assert!(mac.svc.publish_computer_link_in(gamma, &l.link, &mac.conn_code("gamma"), now()).is_err());
    mac.refuse.store(false, Ordering::SeqCst);
    // A workspace that wasn't acknowledged can't be sent.
    assert_eq!(l.session.finish_approved_with(&[ligus_core::room_workspace_id("delta")]).await.unwrap_err(), ApproverError::NotAcknowledged);
    l.session.finish_approved_with(&[beta]).await.unwrap();

    let (verified, failed) = redeem_all(&mut lt, &mac, &mut l, &rooms[..3]).await;
    assert_eq!(verified.iter().map(|(w, _)| *w).collect::<Vec<_>>(), vec![first, beta]);
    assert_eq!(failed, vec![(gamma, GrantError::NotSent)]);
    // The open workspace's founder is the desktop's own after the more ones.
    assert_eq!(mac.svc.trusted_founder(), None);
    l.req.settle().await.unwrap();
}

/// An older approver offers no more: a new computer acknowledges all and
/// links one workspace. An older computer acknowledges only its own: a new
/// approver publishes nothing more.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn older_approvers_and_older_computers_link_one_workspace() {
    let relay = spawn().await;
    let tmp = tempfile::tempdir().unwrap();
    let (mut mac, client, ek, address) = desktop(&relay, &tmp.path().join("mac"), &ROOMS, &[]).await;
    let first = ligus_core::room_workspace_id(ROOMS[0]);

    // An older approver: offer, then finish without more.
    let mut lt = Dev::open(&tmp.path().join("lt"), "lt on mbp", &relay);
    let mut l = open_link(&lt, &mac, &client, &ek, &address).await;
    let grant = mac.svc.prepare_computer_link(first, &l.link, &mac.conn_code(ROOMS[0]), now()).unwrap();
    l.session.offer(grant).await.unwrap();
    l.req.poll(0).await.unwrap();
    assert!(l.req.offered_more().is_empty());
    l.req.ack_all().await.unwrap();
    assert!(l.session.await_ack(0).await.unwrap());
    mac.svc.publish_computer_link(first, &l.link, &mac.conn_code(ROOMS[0]), now()).unwrap();
    l.session.finish_approved(None).await.unwrap();
    let (verified, failed) = redeem_all(&mut lt, &mac, &mut l, &ROOMS[..1]).await;
    assert_eq!((verified.len(), failed.len()), (1, 0));
    l.req.settle().await.unwrap();

    // An older computer: it acknowledges only the digest it knows.
    let lt2 = Dev::open(&tmp.path().join("lt2"), "lt on mini", &relay);
    let mut l = open_link(&lt2, &mac, &client, &ek, &address).await;
    offer_all(&mut mac, &mut l, &ROOMS).await;
    l.req.poll(0).await.unwrap();
    assert_eq!(l.req.offered_more().len(), 2);
    l.req.ack().await.unwrap();
    assert!(l.session.await_ack(0).await.unwrap());
    assert!(l.session.acked_more().is_empty(), "no certificate goes where the computer didn't ask");
    assert_eq!(l.session.offered_more().len(), 2);
    l.session.finish_approved_with(&[]).await.unwrap();
    let RequesterEvent::Result(r) = l.req.poll(0).await.unwrap().remove(0) else { panic!() };
    assert!(r.grant.is_some());
    l.req.settle().await.unwrap();
}

/// An extra grant must answer this request, from the device that showed the
/// code, for the workspace the offer named, and the same person.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_extra_grant_for_another_request_person_or_workspace_is_refused() {
    let relay = spawn().await;
    let tmp = tempfile::tempdir().unwrap();
    let (mut mac, client, ek, address) = desktop(&relay, &tmp.path().join("mac"), &ROOMS[..2], &[]).await;
    let lt = Dev::open(&tmp.path().join("lt"), "lt on mbp", &relay);
    let mut l = open_link(&lt, &mac, &client, &ek, &address).await;
    offer_all(&mut mac, &mut l, &ROOMS[..2]).await;
    l.req.poll(0).await.unwrap();
    l.req.ack_all().await.unwrap();
    assert!(l.session.await_ack(0).await.unwrap());
    let first = ligus_core::room_workspace_id(ROOMS[0]);
    mac.svc.publish_computer_link(first, &l.link, &mac.conn_code(ROOMS[0]), now()).unwrap();
    l.session.finish_approved_with(&[]).await.unwrap();
    let RequesterEvent::Result(r) = l.req.poll(0).await.unwrap().remove(0) else { panic!() };
    let ka = lt.ka();
    let pk = lt.device_key().public_key_bytes();
    let target = GrantTarget::Computer { request: &l.link, ka: &ka, device_key: &pk };
    let first_checked = l.req.check_grant(&r, target, now()).unwrap();
    let offered = l.req.acked_more()[0].clone();
    let open = l.req.open_for_tests().unwrap();

    let check = |grant: &str, offered: &MoreOffer| {
        super::more::check_more_grant(offered, Some(grant), &open, &l.link, &ka, &pk, &first_checked, now())
    };
    let digest_of = |g: &str| MoreOffer { grant_digest: super::envelope::grant_digest(g), ..offered.clone() };
    let beta = ligus_core::room_workspace_id(ROOMS[1]);
    let seal = |req: &LinkRequest, person: &ligus_core::ActorIdentity, ws: Uuid, room: &str, signer: &SigningKeypair| {
        let id = ligus_core::crypto::device_id_for(ligus_core::crypto::SigAlg::Ed25519, &signer.public_key_bytes());
        crate::computer_link::Grant::seal(req, ws, &mac.conn_code(room), person, id, signer, now() + 60_000).unwrap()
    };
    let ana = crate::computer_link::device_local_cleared(mac.svc.author());
    let desk = mac.device_key();
    let good = seal(&l.link, &ana, beta, ROOMS[1], &desk);
    assert!(check(&good, &digest_of(&good)).is_ok());
    assert_eq!(check(&good, &offered).unwrap_err(), GrantError::DigestMismatch, "not the grant the computer acknowledged");
    let other_req = LinkRequest::new(&SigningKeypair::generate().unwrap(), &ka.public_key_bytes(), "x", now(), None);
    let g = seal(&other_req, &ana, beta, ROOMS[1], &desk);
    assert!(matches!(check(&g, &digest_of(&g)).unwrap_err(), GrantError::NotForThisRequest(_)), "another request");
    let g = seal(&l.link, &ana, beta, ROOMS[1], &SigningKeypair::generate().unwrap());
    assert_eq!(check(&g, &digest_of(&g)).unwrap_err(), GrantError::WrongApprover, "another device than the one that showed the code");
    let mut sam = ana.clone();
    sam.account_id = Some(Uuid::new_v4());
    sam.id = sam.account_id.unwrap().to_string();
    let g = seal(&l.link, &sam, beta, ROOMS[1], &desk);
    assert!(matches!(check(&g, &digest_of(&g)).unwrap_err(), GrantError::NotForThisRequest(_)), "another person");
    let g = seal(&l.link, &ana, first, ROOMS[0], &desk);
    assert_eq!(check(&g, &MoreOffer { workspace_id: first, ..digest_of(&g) }).unwrap_err(), GrantError::WrongWorkspace, "the first workspace again");
    let g = seal(&l.link, &ana, ligus_core::room_workspace_id("gamma"), "gamma", &desk);
    assert_eq!(check(&g, &digest_of(&g)).unwrap_err(), GrantError::WrongWorkspace, "not the workspace the offer named");
    assert_eq!(super::more::check_more_grant(&offered, None, &open, &l.link, &ka, &pk, &first_checked, now()).unwrap_err(), GrantError::NotSent);
    l.req.settle().await.unwrap();
}

/// More workspaces than one result carries: the rest are left out, said so,
/// and what is offered fits.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn more_grants_than_fit_are_left_out_and_the_result_fits() {
    let relay = spawn().await;
    let tmp = tempfile::tempdir().unwrap();
    let (mac, client, ek, address) = desktop(&relay, &tmp.path().join("mac"), &ROOMS[..1], &[]).await;
    let lt = Dev::open(&tmp.path().join("lt"), "lt on mbp", &relay);
    let mut l = open_link(&lt, &mac, &client, &ek, &address).await;
    let desk = mac.device_key();
    let id = ligus_core::crypto::device_id_for(ligus_core::crypto::SigAlg::Ed25519, &desk.public_key_bytes());
    let ana = crate::computer_link::device_local_cleared(mac.svc.author());
    let grant_for = |room: &str| {
        crate::computer_link::Grant::seal(&l.link, ligus_core::room_workspace_id(room), &mac.conn_code(room), &ana, id, &desk, now() + 60_000).unwrap()
    };
    let first = grant_for(ROOMS[0]);
    let rooms: Vec<String> = (0..40).map(|i| format!("room-{i}")).collect();
    let more: Vec<MoreGrant> = rooms.iter().map(|r| MoreGrant { workspace_id: ligus_core::room_workspace_id(r), name: r.clone(), grant: grant_for(r) }).collect();
    let one = first.len();
    let left_out = l.session.offer_with(first, more).await.unwrap();
    assert!(!left_out.is_empty() && left_out.len() < 40, "{} left out (a grant is {one} bytes)", left_out.len());
    let offered = l.session.offered_more();
    eprintln!("a grant is {one} bytes; {} more fit", offered.len());
    assert!(offered.len() >= 10, "a person's usual handful fits: {}", offered.len());
    assert_eq!(offered.len() + left_out.len(), 40);
    l.req.poll(0).await.unwrap();
    l.req.ack_all().await.unwrap();
    assert!(l.session.await_ack(0).await.unwrap());
    let all: Vec<Uuid> = l.session.acked_more().iter().map(|m| m.workspace_id).collect();
    l.session.finish_approved_with(&all).await.expect("the result fits the relay's message limit");
    let RequesterEvent::Result(_) = l.req.poll(0).await.unwrap().remove(0) else { panic!() };
    l.req.settle().await.unwrap();
}

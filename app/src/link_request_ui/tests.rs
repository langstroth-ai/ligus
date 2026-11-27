//! The desktop's link flow: its pure parts, and end to end through the
//! in-process test relay (device 2 links to device 1's account and ends up
//! as the same member, never a second one).

use std::sync::Arc;
use std::time::Duration;

use ligus_core::presence::PresenceAssertion;
use ligus_core::{workspace_config_session_id, ActorKind};
use ligus_runtime::approval_mailbox::{list_live, ApproverIdentity, ApproverPerson, ApproverSession, EpochKey, RequestSubject, SeenSet};
use ligus_runtime::chat_service::{PresenceProvider, PresenceRequest};
use ligus_runtime::workspace_invite::encode_workspace_invite;
use ligus_runtime::{EventStore, FileKeyVault, IdentityStore, RelayClient, SyncEngine};


use super::*;

const PASSPHRASE: &str = "desktop link passphrase";

// ── Pure parts ───────────────────────────────────────────────────────────

#[test]
fn an_address_must_be_one_and_on_https_or_this_computer() {
    assert!(parse_address("  ").unwrap_err().text.contains("Paste the approval address"));
    assert!(parse_address("hivews1:abc").unwrap_err().text.contains("isn't an approval address"));
    let key = [9u8; 32];
    let ws = Uuid::new_v4();
    let local = ApprovalAddress::derive("http://127.0.0.1:8080", ws, &"ab".repeat(16), 1, &key, 1).encode();
    assert!(parse_address(&format!("\n{local}\n")).is_ok(), "a loopback relay, pasted with blank lines");
    let plain = ApprovalAddress::derive("http://relay.example", ws, &"ab".repeat(16), 1, &key, 1).encode();
    let e = parse_address(&plain).unwrap_err();
    assert!(e.text.contains("https://") && !e.retry, "{e:?}");
    let tls = ApprovalAddress::derive("https://relay.example", ws, &"ab".repeat(16), 1, &key, 1).encode();
    assert_eq!(parse_address(&tls).unwrap().relay, "https://relay.example");
}

#[test]
fn mailbox_errors_read_plainly_and_say_whether_to_retry() {
    let f = mailbox_failed(&MailboxError::NoRoutes);
    assert!(f.no_mailbox && !f.retry && f.text.contains("request code"), "{f:?}");
    let mut v = LinkFlowDto::default();
    set_failed(&mut v, &f);
    assert_eq!(v.phase, "noMailbox");
    let f = mailbox_failed(&MailboxError::Unreachable("timed out".into()));
    assert!(f.retry && !f.no_mailbox && f.text.contains("Couldn't reach the relay"));
    set_failed(&mut v, &f);
    assert_eq!((v.phase.as_str(), v.can_retry), ("failed", true));
    assert!(mailbox_failed(&MailboxError::MailboxUnknown).text.contains("Copy it again"));
    assert!(mailbox_failed(&MailboxError::RequestUnknown).text.contains("restarted"));
    assert!(!requester_failed(&RequesterError::TooManyDead).retry);
    assert!(outcome_failed(Outcome::Declined).text.contains("declined"));
    assert!(abort_failed(AbortReason::Contested).text.contains("Another device"));
}

#[test]
fn a_cancelled_flow_ignores_its_old_driver() {
    let slot = Slot::default();
    assert_eq!(slot.view().phase, "idle");
    let (gen, mut rx) = slot.begin(view("waiting"));
    slot.answer(Answer::CodesMatch(true)).unwrap();
    assert_eq!(rx.try_recv().unwrap(), Answer::CodesMatch(true));
    assert_eq!(slot.reset().phase, "idle");
    assert_eq!(rx.try_recv().unwrap(), Answer::Cancel, "the driver hears the cancel");
    assert!(slot.set(gen, |v| v.phase = "linked".into()).is_none(), "a stale driver changes nothing");
    assert_eq!(slot.view().phase, "idle");
    assert!(slot.answer(Answer::LinkAs(true)).is_err(), "nothing waits after a cancel");
}

#[test]
fn a_linked_desktop_refuses_to_found_or_join_and_says_why() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(refuse_if_linked(tmp.path(), "found workspaces").is_ok());
    let mut link = LinkState::default();
    let mut person = ActorIdentity::new("ana", "Ana", ActorKind::Human);
    person.account_id = Some(Uuid::new_v4());
    link.record_link(&person, Uuid::new_v4()).unwrap();
    link.save(tmp.path()).unwrap();
    let e = refuse_if_linked(tmp.path(), "found workspaces").unwrap_err();
    assert!(e.contains("one of Ana's devices") && e.contains("Link this computer"), "{e}");
    assert_eq!(linked_person(tmp.path()).as_deref(), Some("Ana"));
}

// ── End to end through the test relay ────────────────────────────────────

struct TestPasskey(Uuid);
impl PresenceProvider for TestPasskey {
    fn assert(&self, req: &PresenceRequest) -> Result<PresenceAssertion, String> {
        Ok(ligus_core::testkit::test_recovery_key(self.0).assert(&req.challenge))
    }
}

fn open_svc(dir: &Path, name: &str) -> ChatService {
    std::fs::create_dir_all(dir).unwrap();
    let ids = IdentityStore::new(dir, FileKeyVault::new(dir));
    let me = ids.bootstrap(name, &name.to_lowercase(), name).unwrap();
    ChatService::new(
        EventStore::open(dir.join("ligus.db")).unwrap(),
        ids.device_keypair(me.device.id).unwrap(),
        ids.account_keypair(me.account.id).unwrap(),
        me.account.actor(),
    )
}

async fn sync_dir(dir: &Path, conn: &WorkspaceConn) -> Result<(), String> {
    let key = ligus_core::derive_workspace_key(conn.key.as_deref().unwrap_or_default());
    let engine = SyncEngine::new(RelayClient::new(&conn.relay_url), conn.room.clone()).with_room_workspace(conn.id()).with_key(key);
    sync_with(dir.join("ligus.db"), engine, false).await
}

/// Device 1: the person's first desktop, which founded the workspace.
struct First {
    svc: ChatService,
    dir: PathBuf,
    relay: String,
    room: String,
}

impl First {
    fn found(dir: &Path, relay: &str, room: &str) -> Self {
        let mut svc = open_svc(dir, "Ana");
        svc.enable_test_presence();
        let ws = ligus_core::room_workspace_id(room);
        svc.ensure_self_member(workspace_config_session_id(ws), ws).unwrap();
        let acct = svc.author().account_id.unwrap();
        let key = ligus_core::testkit::test_recovery_key(acct);
        svc.register_passkey(ws, key.credential(acct, "Test recovery key"), &|req| Ok(key.assert(&req.challenge))).unwrap();
        svc.set_presence_provider(Some(Arc::new(TestPasskey(acct))));
        Self { svc, dir: dir.to_path_buf(), relay: relay.into(), room: room.into() }
    }
    fn ws(&self) -> Uuid {
        ligus_core::room_workspace_id(&self.room)
    }
    fn conn(&self) -> WorkspaceConn {
        WorkspaceConn {
            name: "Acme".into(),
            relay_url: self.relay.clone(),
            room: self.room.clone(),
            key: Some(PASSPHRASE.into()),
            icon: None,
            dm_account: None,
            founder_actor_id: Some(self.svc.author().id.clone()),
            fk: Some(ligus_runtime::envelope_verifier::FounderPin::fingerprint_hex(&self.svc.account_public_key())),
            inv: None,
            ek: Vec::new(),
            sig: None,
        }
    }
    async fn sync(&self) {
        sync_dir(&self.dir, &self.conn()).await.unwrap();
    }
    fn identity(&self) -> ApproverIdentity {
        let ids = IdentityStore::new(&self.dir, FileKeyVault::new(&self.dir));
        ApproverIdentity {
            signer: Signer::Ed25519(ids.device_keypair(self.svc.device_id()).unwrap()),
            device_name: "Ana's desktop".into(),
            person: ApproverPerson { account_id: self.svc.author().account_id.unwrap(), name: "Ana".into() },
        }
    }
    fn human_members(&self) -> Vec<String> {
        let cfg = self.svc.load_in(self.ws(), workspace_config_session_id(self.ws())).unwrap().unwrap();
        cfg.members.iter().filter(|m| m.actor.kind == ActorKind::Human).map(|m| m.id.clone()).collect()
    }
}

/// Device 2: the new desktop, as the link flow sees it.
struct NewDesktop {
    dir: PathBuf,
    svc: Mutex<ChatService>,
    ka: KeyAgreementKeypair,
    own: usize,
    adopted: Mutex<Vec<WorkspaceConn>>,
}

impl NewDesktop {
    fn open(dir: &Path) -> Self {
        let svc = open_svc(dir, "You");
        Self { dir: dir.to_path_buf(), svc: Mutex::new(svc), ka: KeyAgreementKeypair::generate().unwrap(), own: 0, adopted: Mutex::new(Vec::new()) }
    }
}

impl LinkHost for NewDesktop {
    fn data_dir(&self) -> PathBuf {
        self.dir.clone()
    }
    fn with_svc<R>(&self, f: impl FnOnce(&mut ChatService) -> R) -> R {
        f(&mut self.svc.lock().unwrap())
    }
    fn keys(&self) -> Result<(ligus_core::crypto::SigningKeypair, KeyAgreementKeypair), String> {
        let ids = IdentityStore::new(&self.dir, FileKeyVault::new(&self.dir));
        let device = ids.device_keypair(self.svc.lock().unwrap().device_id()).map_err(|e| e.to_string())?;
        Ok((device, clone_ka(&self.ka)))
    }
    fn label(&self) -> String {
        "Ana's MacBook Air".into()
    }
    fn token(&self, _: &str) -> Option<String> {
        None
    }
    fn own_workspaces(&self, _: &LinkState) -> usize {
        self.own
    }
    fn sync(&self, conn: &WorkspaceConn) -> impl Future<Output = Result<(), String>> + Send {
        let (dir, conn) = (self.dir.clone(), conn.clone());
        async move { sync_dir(&dir, &conn).await }
    }
    fn adopt_workspace(&self, conn: &WorkspaceConn) -> Result<(), String> {
        self.adopted.lock().unwrap().push(conn.clone());
        Ok(())
    }
    fn changed(&self, _: &LinkFlowDto) {}
}

async fn spawn_relay() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, ligus_runtime::test_relay::router()).await.unwrap() });
    format!("http://{addr}")
}

async fn wait_for(slot: &Slot, phase: &str) -> LinkFlowDto {
    for _ in 0..300 {
        let v = slot.view();
        if v.phase == phase {
            return v;
        }
        assert!(v.phase != "failed" && v.phase != "noMailbox", "the flow failed waiting for {phase}: {v:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("never reached {phase}: {:?}", slot.view());
}

/// The workspace's approval mailbox at the relay (key epoch 0, the
/// passphrase): what My devices > Add a computer shows as the address.
async fn mailbox(first: &First) -> (MailboxClient, EpochKey, ApprovalAddress) {
    let client = MailboxClient::new(&first.relay);
    let ek = EpochKey { epoch: 0, key: ligus_core::derive_workspace_key(PASSPHRASE) };
    let n = client.ensure_registered(&ek.key).await.unwrap();
    let fk = first.conn().fk.unwrap();
    let address = ApprovalAddress::derive(&first.relay, first.ws(), &fk, 0, &ek.key, n);
    (client, ek, address)
}

/// Device 1's side, as its Inbox runs it: open the request, show the code,
/// Codes match, the offer, wait for the ack, then the passkey and the
/// certificate, synced, and the result.
async fn approve_in_inbox(first: &mut First, client: &MailboxClient, ek: &EpochKey, slot: &Slot) {
    let ws = first.ws();
    // Device 1 opens it from its Inbox: both screens show the same code.
    let live = list_live(&client, std::slice::from_ref(&ek)).await.unwrap().live.remove(0);
    let mut session = ApproverSession::begin(client.clone(), &ek, &live.request, first.identity(), &mut SeenSet::in_memory()).await.unwrap();
    let RequestSubject::Computer(parsed) = session.request().subject.clone() else { panic!("a computer's request") };
    assert_eq!(parsed.label, "Ana's MacBook Air");
    let shown = wait_for(&slot, "code").await;
    let approver_code = session.await_reveal(5).await.unwrap().expect("the reveal arrives");
    assert_eq!(shown.code.as_deref(), Some(approver_code.shown.as_str()), "both screens show the same code");
    assert_eq!(shown.claimed_name.as_deref(), Some("Ana"));

    // Codes match on both: the offer, the ack, then the passkey and the certificate.
    slot.answer(Answer::CodesMatch(true)).unwrap();
    let conn_code = encode_workspace_invite(&first.conn());
    let now = now_ms();
    let grant = first.svc.prepare_computer_link(ws, &parsed, &conn_code, now).unwrap();
    session.offer(grant).await.unwrap();
    let mut acked = false;
    for _ in 0..20 {
        if session.await_ack(1).await.unwrap() {
            acked = true;
            break;
        }
    }
    assert!(acked, "device 2 acknowledged the offer");
    first.svc.publish_computer_link(ws, &parsed, &conn_code, now).unwrap();
    first.sync().await;
    session.finish_approved(None).await.unwrap();

}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_desktop_links_by_matching_codes_and_is_the_same_member() {
    let relay = spawn_relay().await;
    let tmp = tempfile::tempdir().unwrap();
    let first = First::found(&tmp.path().join("one"), &relay, "acme");
    first.sync().await;
    let (client, ek, address) = mailbox(&first).await;
    let ws = first.ws();
    let ana = first.svc.author().clone();

    // Device 2 pastes the address: no name, no workspace.
    let two = Arc::new(NewDesktop::open(&tmp.path().join("two")));
    let own_id = two.svc.lock().unwrap().author().id.clone();
    let slot = Arc::new(Slot::default());
    let v = start(two.clone(), slot.clone(), &address.encode()).await;
    assert_eq!(v.phase, "waiting", "{v:?}");
    assert!(v.request.as_deref().is_some_and(|r| r.starts_with("liguslink1:")), "the paste path's request is ready too");

    let mut first = first;
    approve_in_inbox(&mut first, &client, &ek, &slot).await;

    // Device 2 finds the certificate on the log and asks who it links as.
    let v = wait_for(&slot, "confirmPerson").await;
    assert_eq!(v.person_name.as_deref(), Some("Ana"));
    assert!(LinkState::load(&two.dir).unwrap().person.is_none(), "nothing is saved before the person says yes");
    slot.answer(Answer::LinkAs(true)).unwrap();
    let v = wait_for(&slot, "linked").await;
    assert_eq!(v.workspace_id.as_deref(), Some(ws.to_string().as_str()));

    // Device 2 is one of Ana's devices: it acts as Ana, never as its own account.
    {
        let svc = two.svc.lock().unwrap();
        assert_eq!(svc.author().id, ana.id);
        assert_eq!(svc.author().account_id, ana.account_id);
        assert!(svc.is_delegated());
        assert_ne!(svc.author().id, own_id);
    }
    let link = LinkState::load(&two.dir).unwrap();
    assert_eq!(link.person.as_ref().and_then(|p| p.account_id), ana.account_id);
    assert_eq!(link.account_in(ws), ana.account_id);
    assert_eq!(two.adopted.lock().unwrap().iter().map(|c| c.room.as_str()).collect::<Vec<_>>(), ["acme"]);
    assert!(two.adopted.lock().unwrap()[0].inv.is_none(), "the workspace is kept with no invite");

    // On device 1: still one member, and device 2 is one of Ana's devices,
    // its name (written by device 2 as Ana) accepted.
    first.sync().await;
    assert_eq!(first.human_members(), vec![ana.id.clone()], "never a second member");
    let device2 = two.svc.lock().unwrap().device_id();
    let mine = first.svc.own_devices(ws).unwrap();
    assert!(mine.iter().any(|d| d.device_id == device2 && !d.revoked), "{mine:?}");
    assert_eq!(first.svc.device_label_of(ws, device2, ana.account_id.unwrap()).as_deref(), Some("Ana's MacBook Air"));

    // At the next launch device 2 acts as Ana again, and the workspace binds.
    let mut relaunched = open_svc(&two.dir, "You");
    assert!(adopt_at_launch(&mut relaunched, &two.dir).unwrap());
    assert_eq!(relaunched.author().account_id, ana.account_id);
    bind_workspace(&mut relaunched, &two.dir, ws, None);
    assert_eq!(relaunched.write_block(), None, "it counts here");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn codes_that_dont_match_grant_nothing_and_save_nothing() {
    let relay = spawn_relay().await;
    let tmp = tempfile::tempdir().unwrap();
    let first = First::found(&tmp.path().join("one"), &relay, "acme");
    first.sync().await;
    let (client, ek, address) = mailbox(&first).await;
    let two = Arc::new(NewDesktop::open(&tmp.path().join("two")));
    let slot = Arc::new(Slot::default());
    start(two.clone(), slot.clone(), &address.encode()).await;
    let live = list_live(&client, std::slice::from_ref(&ek)).await.unwrap().live.remove(0);
    let mut session = ApproverSession::begin(client.clone(), &ek, &live.request, first.identity(), &mut SeenSet::in_memory()).await.unwrap();
    wait_for(&slot, "code").await;
    let _ = session.await_reveal(5).await.unwrap();
    slot.answer(Answer::CodesMatch(false)).unwrap();
    let mut v = slot.view();
    for _ in 0..100 {
        v = slot.view();
        if v.phase == "failed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(v.phase, "failed");
    assert!(v.error.as_deref().is_some_and(|e| e.contains("don't match")) && v.can_retry, "{v:?}");
    assert!(!LinkState::load(&two.dir).unwrap().is_linked());
    assert!(!two.svc.lock().unwrap().is_delegated());
    assert!(two.adopted.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_paste_path_links_when_the_relay_has_no_mailbox() {
    let relay = spawn_relay().await;
    let tmp = tempfile::tempdir().unwrap();
    let mut first = First::found(&tmp.path().join("one"), &relay, "acme");
    first.sync().await;
    let ws = first.ws();
    let ana = first.svc.author().clone();
    let two = Arc::new(NewDesktop::open(&tmp.path().join("two")));
    let slot = Arc::new(Slot::default());

    let v = paste(&*two, &slot);
    assert_eq!(v.phase, "paste");
    let request = LinkRequest::decode(v.request.as_deref().unwrap()).unwrap();
    assert_eq!(v.fingerprint.as_deref(), Some(request.fingerprint().as_str()));
    // Device 1: Add a computer, paste, compare, passkey: the grant.
    let grant = first.svc.issue_computer_link(ws, &request, &encode_workspace_invite(&first.conn()), now_ms()).unwrap();
    first.sync().await;
    assert!(open_grant(&*two, &slot, "hivews1:nope").unwrap_err().contains("isn't the code"));
    assert!(open_grant(&*two, &slot, v.request.as_deref().unwrap()).unwrap_err().contains("That's this computer's request"));
    let v = open_grant(&*two, &slot, &format!("ligus devices redeem \"{grant}\"\n")).unwrap();
    assert_eq!(v.phase, "confirmApprover");
    assert_eq!(v.person_name.as_deref(), Some("Ana"));
    assert_eq!(v.workspace_name.as_deref(), Some("Acme"));
    assert!(!LinkState::load(&two.dir).unwrap().is_linked(), "nothing saved before the person confirms");
    let v = redeem(two.clone(), slot.clone()).await;
    assert_eq!(v.phase, "linked", "{v:?}");
    assert_eq!(two.svc.lock().unwrap().author().id, ana.id);
    first.sync().await;
    assert_eq!(first.human_members(), vec![ana.id.clone()], "never a second member");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_install_with_its_own_workspaces_is_never_linked() {
    let relay = spawn_relay().await;
    let tmp = tempfile::tempdir().unwrap();
    let first = First::found(&tmp.path().join("one"), &relay, "acme");
    first.sync().await;
    let (_, _, address) = mailbox(&first).await;
    let mut two = NewDesktop::open(&tmp.path().join("two"));
    two.own = 2;
    let two = Arc::new(two);
    let slot = Arc::new(Slot::default());
    let v = start(two.clone(), slot.clone(), &address.encode()).await;
    assert_eq!(v.phase, "failed");
    assert!(v.error.as_deref().is_some_and(|e| e.contains("its own member (2 workspaces)")) && !v.can_retry, "{v:?}");
    assert!(LinkState::load(&two.dir).unwrap().pending.is_empty(), "no request was made");
    let v = paste(&*two, &slot);
    assert_eq!(v.phase, "failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saying_no_to_link_as_leaves_nothing_behind() {
    let relay = spawn_relay().await;
    let tmp = tempfile::tempdir().unwrap();
    let mut first = First::found(&tmp.path().join("one"), &relay, "acme");
    first.sync().await;
    let (client, ek, address) = mailbox(&first).await;
    let ws = first.ws();
    let two = Arc::new(NewDesktop::open(&tmp.path().join("two")));
    let slot = Arc::new(Slot::default());
    start(two.clone(), slot.clone(), &address.encode()).await;
    approve_in_inbox(&mut first, &client, &ek, &slot).await;
    wait_for(&slot, "confirmPerson").await;
    assert!(two.svc.lock().unwrap().store().workspace_pin(ws).unwrap().is_some(), "pinned while checking");
    slot.answer(Answer::LinkAs(false)).unwrap();
    let mut v = slot.view();
    for _ in 0..100 {
        v = slot.view();
        if v.phase == "failed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(v.error.as_deref().is_some_and(|e| e.contains("Not linked")), "{v:?}");
    let svc = two.svc.lock().unwrap();
    assert!(svc.store().workspace_pin(ws).unwrap().is_none(), "the workspace isn't kept");
    assert!(!svc.is_delegated());
    assert!(!LinkState::load(&two.dir).unwrap().is_linked());
    assert!(two.adopted.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_stops_a_pasted_link_that_is_still_checking() {
    let relay = spawn_relay().await;
    let tmp = tempfile::tempdir().unwrap();
    let mut first = First::found(&tmp.path().join("one"), &relay, "acme");
    first.sync().await;
    let ws = first.ws();
    let two = Arc::new(NewDesktop::open(&tmp.path().join("two")));
    let slot = Arc::new(Slot::default());
    let v = paste(&*two, &slot);
    let request = LinkRequest::decode(v.request.as_deref().unwrap()).unwrap();
    // The grant, but the certificate hasn't reached the relay yet.
    let grant = first.svc.issue_computer_link(ws, &request, &encode_workspace_invite(&first.conn()), now_ms()).unwrap();
    open_grant(&*two, &slot, &grant).unwrap();
    let running = tokio::spawn(redeem(two.clone(), slot.clone()));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(slot.view().phase, "linking");
    // The person cancels; then the certificate arrives.
    slot.reset();
    first.sync().await;
    let _ = tokio::time::timeout(Duration::from_secs(10), running).await.expect("the redeem stops");
    assert!(!LinkState::load(&two.dir).unwrap().is_linked(), "cancelled: nothing linked");
    assert!(!two.svc.lock().unwrap().is_delegated());
    assert!(two.adopted.lock().unwrap().is_empty());
    assert_eq!(slot.view().phase, "idle");
}

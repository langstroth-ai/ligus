//! Code-match approval through the CLI, end to end on the in-process test
//! relay with real folded workspaces (design note 11: M5, M6b, M6d, M8, M9,
//! M12; identity model v2 K1 for computers).

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use ligus_core::workspace_config_session_id;
use ligus_runtime::approval_mailbox as am;
use uuid::Uuid;

use crate::approvals_cmd::{self as ac, ApproveOptions, ToOptions};
use crate::problem::{classify, EXIT_ERROR, EXIT_FIXABLE, EXIT_HUMAN};
use crate::{open_service, sync_once, Config};

use crate::serve::tests::test_relay;

pub(crate) const PASSPHRASE: &str = "code match cli passphrase";

pub(crate) async fn spawn(router: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

pub(crate) fn config(dir: &Path, relay: &str, room: &str) -> Config {
    std::fs::create_dir_all(dir).unwrap();
    Config { data_dir: dir.to_path_buf(), relay_url: Some(relay.into()), token: None, room: room.into(), key: Some(PASSPHRASE.into()), epoch_keys: Vec::new() }
}

pub(crate) fn fresh(dir: &Path) -> Config {
    std::fs::create_dir_all(dir).unwrap();
    Config { data_dir: dir.to_path_buf(), relay_url: None, token: None, room: "default".into(), key: None, epoch_keys: Vec::new() }
}

pub(crate) struct World {
    _tmp: tempfile::TempDir,
    pub(crate) root: std::path::PathBuf,
    pub(crate) relay: String,
    pub(crate) room: String,
    pub(crate) ws: Uuid,
    pub(crate) desk: Config,
    pub(crate) words: String,
}

/// Alex founds a workspace with a recovery key registered (an Admin's presence).
pub(crate) async fn world() -> World {
    world_on(test_relay::router()).await
}

pub(crate) async fn world_on(router: axum::Router) -> World {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let relay = spawn(router).await;
    let room = format!("cm-{}", Uuid::new_v4().simple());
    let ws = crate::uuid_of_room(&room);
    let desk = config(&root.join("desk"), &relay, &room);
    let acct = {
        let mut svc = open_service(&desk).unwrap();
        svc.ensure_self_member(workspace_config_session_id(ws), ws).unwrap();
        let acct = svc.author().account_id.unwrap();
        let key = ligus_core::testkit::test_recovery_key(acct);
        svc.register_passkey(ws, key.credential(acct, "Recovery key"), &|req| Ok(key.assert(&req.challenge))).unwrap();
        acct
    };
    sync_once(&desk).await.unwrap();
    let words = root.join("words");
    ligus_runtime::private_file::write_private_atomic(&words, ligus_core::testkit::test_recovery_key(acct).words().as_bytes()).unwrap();
    World { relay, room, ws, desk, words: words.display().to_string(), root, _tmp: tmp }
}

/// The workspace's approval address, as `ligus admit address` prints it.
pub(crate) async fn address(w: &World) -> am::ApprovalAddress {
    let svc = open_service(&w.desk).unwrap();
    let keys = svc.approval_epoch_keys(w.ws, &crate::key_source(&w.desk).unwrap()).unwrap();
    let pin = svc.founder_pin_hex(w.ws).unwrap().unwrap();
    am::current_address(&am::MailboxClient::new(&w.relay), w.ws, &pin, &keys).await.unwrap()
}

/// The rid of the first live request, once it is there.
pub(crate) async fn first_rid(w: &World) -> String {
    let svc = open_service(&w.desk).unwrap();
    let keys = svc.approval_epoch_keys(w.ws, &crate::key_source(&w.desk).unwrap()).unwrap();
    drop(svc);
    let client = am::MailboxClient::new(&w.relay);
    for _ in 0..200 {
        let l = am::list_live(&client, &keys).await.unwrap();
        if let Some(r) = l.live.first() {
            return r.rid_hex();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no request arrived");
}

async fn live_count(w: &World) -> usize {
    let svc = open_service(&w.desk).unwrap();
    let keys = svc.approval_epoch_keys(w.ws, &crate::key_source(&w.desk).unwrap()).unwrap();
    am::list_live(&am::MailboxClient::new(&w.relay), &keys).await.unwrap().live.len()
}

pub(crate) fn to(addr: &am::ApprovalAddress, label: &str, tx: Option<mpsc::Sender<String>>) -> ToOptions {
    let mut o = ToOptions::new(addr.encode(), label.into(), Duration::from_secs(60), false, true);
    o.code_tx = tx;
    o.log_retry = Duration::from_secs(15);
    o
}

pub(crate) fn approve(w: &World, rid: &str) -> ApproveOptions {
    ApproveOptions { rid: rid.into(), code: None, words: Some(w.words.clone()), json: true, end: false }
}

/// The config log's admissions and members (what a mismatch must not change).
fn config_state(w: &World) -> (usize, usize) {
    let svc = open_service(&w.desk).unwrap();
    let c = svc.load_in(w.ws, workspace_config_session_id(w.ws)).unwrap().unwrap();
    (c.guards.service_invites.len(), c.members.len())
}

/// M8 headless box: the box asks through the mailbox with no terminal, the
/// Admin approves with the code it shows, and the box keeps the admission
/// only after finding it on the workspace log; then it joins. The request is
/// deleted on settle.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_headless_box_joins_through_the_mailbox_after_check_5() {
    let w = world().await;
    let addr = address(&w).await;
    let boxc = fresh(&w.root.join("box"));
    let (tx, rx) = mpsc::channel();
    let o = to(&addr, "build-1", Some(tx));
    let bc = boxc.clone();
    let boxed = tokio::spawn(async move { ac::cmd_admit_request_to(&bc, o, None).await.map_err(|e| (classify(&e).0, e.to_string())) });
    let rid = first_rid(&w).await;
    let mut typed = || Ok(rx.recv_timeout(Duration::from_secs(30))?);
    ac::cmd_approve(&w.desk, approve(&w, &rid), Some(&mut typed)).await.unwrap();
    boxed.await.unwrap().unwrap();
    // The box kept the admission and joins as its own member.
    let f = crate::load_file_config(&boxc.data_dir);
    assert_eq!(f.room.as_deref(), Some(w.room.as_str()));
    let store = ligus_runtime::EventStore::open(boxc.data_dir.join("ligus.db")).unwrap();
    assert!(store.join_invite(w.ws).unwrap().is_some());
    assert_eq!(live_count(&w).await, 0, "settled: the request is gone from the relay");
    let joined = Config { room: w.room.clone(), relay_url: f.relay_url.clone(), key: f.key.clone(), epoch_keys: f.ek.clone(), ..boxc.clone() };
    sync_once(&joined).await.unwrap();
    let mut svc = open_service(&joined).unwrap();
    svc.ensure_self_member(workspace_config_session_id(w.ws), w.ws).unwrap();
    let c = svc.load_in(w.ws, workspace_config_session_id(w.ws)).unwrap().unwrap();
    assert!(c.members.iter().any(|m| m.id == svc.author().id), "the box joined as its own member");
    // M8: nothing printed carries a secret.
    for line in ac::PRINTED.lock().unwrap().iter() {
        for secret in ["hivews1:", "ligusadmit1:", "ligusgrant1:", PASSPHRASE] {
            assert!(!line.contains(secret), "printed a secret ({secret}): {line}");
        }
    }
}

/// M5: a wrong code on the approver ends the request; nothing reaches the
/// log, the approver keeps a notice, and the box exits 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mismatch_grants_nothing_and_leaves_a_notice() {
    let w = world().await;
    let addr = address(&w).await;
    let before = config_state(&w);
    let boxc = fresh(&w.root.join("box"));
    let (tx, rx) = mpsc::channel();
    let o = to(&addr, "build-1", Some(tx));
    let bc = boxc.clone();
    let boxed = tokio::spawn(async move { ac::cmd_admit_request_to(&bc, o, None).await.map_err(|e| (classify(&e).0, e.to_string())) });
    let rid = first_rid(&w).await;
    let mut typed = || {
        let real = rx.recv_timeout(Duration::from_secs(30))?;
        // Any other code.
        Ok(if real.starts_with('0') { "1111 1111".to_string() } else { "0000 0000".to_string() })
    };
    let e = ac::cmd_approve(&w.desk, approve(&w, &rid), Some(&mut typed)).await.unwrap_err();
    assert_eq!(classify(&e).0, EXIT_ERROR);
    let (code, why) = boxed.await.unwrap().unwrap_err();
    assert_eq!(code, EXIT_ERROR, "{why}");
    assert!(why.contains("match"), "{why}");
    sync_once(&w.desk).await.unwrap();
    assert_eq!(config_state(&w), before, "nothing on the log");
    let notices = am::Notices::new(&w.desk.data_dir).live(ligus_core::flow::now_ms()).unwrap();
    assert_eq!(notices.len(), 1);
    assert!(notices[0].text().ends_with("didn't match. Nothing was granted."), "{}", notices[0].text());
    assert!(notices[0].text().contains("build-1"));
    assert!(ligus_runtime::EventStore::open(boxc.data_dir.join("ligus.db")).unwrap().join_invite(w.ws).unwrap().is_none());
}

/// M6b: a key holder who isn't an Admin races an open and sends a grant
/// built with the workspace key: the box never joins (check 5).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_racing_key_holder_cant_make_a_headless_box_join() {
    let w = world().await;
    let addr = address(&w).await;
    let boxc = fresh(&w.root.join("box"));
    let mut o = to(&addr, "build-1", None);
    o.log_retry = Duration::from_secs(3);
    let bc = boxc.clone();
    let boxed = tokio::spawn(async move { ac::cmd_admit_request_to(&bc, o, None).await.map_err(|e| (classify(&e).0, e.to_string())) });
    let rid = first_rid(&w).await;
    // Mallory holds the workspace key (as a device revoked before a
    // rotation would) but no Admin's device.
    let (keys, pin) = {
        let svc = open_service(&w.desk).unwrap();
        (svc.approval_epoch_keys(w.ws, &crate::key_source(&w.desk).unwrap()).unwrap(), svc.founder_pin_hex(w.ws).unwrap().unwrap())
    };
    let client = am::MailboxClient::new(&w.relay);
    let live = am::list_live(&client, &keys).await.unwrap().live.into_iter().find(|l| l.rid_hex() == rid).unwrap();
    let mallory = ligus_core::crypto::SigningKeypair::generate().unwrap();
    let me = am::ApproverIdentity {
        signer: am::Signer::Ed25519(mallory),
        device_name: "Alex's desktop".into(),
        person: am::ApproverPerson { account_id: Uuid::new_v4(), name: "Alex".into() },
    };
    let mut s = am::ApproverSession::begin(client, &keys[0], &live.request, me, &mut am::SeenSet::in_memory()).await.unwrap();
    while s.await_reveal(5).await.unwrap().is_none() {}
    let am::RequestSubject::Box(join) = &live.request.subject else { panic!() };
    // A well-formed admission for the real workspace and founder, for an
    // invite that was never issued.
    let conn = ligus_runtime::workspace_invite::WorkspaceConn {
        name: String::new(),
        relay_url: w.relay.clone(),
        room: w.room.clone(),
        key: Some(PASSPHRASE.into()),
        icon: None,
        dm_account: None,
        founder_actor_id: Some(Uuid::new_v4().to_string()),
        fk: Some(pin),
        inv: Some(Uuid::new_v4().to_string()),
        ek: Vec::new(),
        sig: None,
    };
    let invite = ligus_runtime::box_admission::with_redeem_key(&ligus_runtime::workspace_invite::encode_workspace_invite(&conn), &[4; 32]).unwrap();
    s.finish_approved(Some(&join.seal(&invite).unwrap())).await.unwrap();
    let (code, why) = boxed.await.unwrap().unwrap_err();
    assert_eq!(code, EXIT_ERROR, "{why}");
    assert!(why.contains("workspace log"), "{why}");
    assert!(crate::load_file_config(&boxc.data_dir).room.is_none(), "nothing was kept");
}

/// M6d and identity model v2 K1 through the CLI: the computer confirms the
/// code, acknowledges the offer, and links only after "Link this computer as
/// Alex?"; the desktop publishes the certificate only after the ack. A No at
/// the end links nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_computer_links_through_the_mailbox_and_a_no_links_nothing() {
    for answer in [true, false] {
        let w = world().await;
        let addr = address(&w).await;
        let comp = fresh(&w.root.join("laptop"));
        let (tx, rx) = mpsc::channel();
        let o = to(&addr, "laptop", Some(tx));
        let cc = comp.clone();
        let prompts = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let p2 = prompts.clone();
        let linking = tokio::spawn(async move {
            let mut ask = move |q: &str| {
                p2.lock().unwrap().push(q.to_string());
                Ok(!q.starts_with("Link this computer") || answer)
            };
            ac::cmd_devices_request_to(&cc, o, false, Some(&mut ask)).await.map_err(|e| (classify(&e).0, e.to_string()))
        });
        let rid = first_rid(&w).await;
        let mut typed = || Ok(rx.recv_timeout(Duration::from_secs(30))?);
        ac::cmd_approve(&w.desk, approve(&w, &rid), Some(&mut typed)).await.unwrap();
        let r = linking.await.unwrap();
        let asked = prompts.lock().unwrap().clone();
        assert!(asked.iter().any(|q| q.starts_with("Link this computer as") && q.ends_with("(verified on the workspace log)?")), "{asked:?}");
        let state = ligus_runtime::computer_link::LinkState::load(&comp.data_dir).unwrap();
        if answer {
            r.unwrap();
            assert!(state.linked_in(w.ws).is_some());
        } else {
            assert_eq!(r.unwrap_err().0, EXIT_ERROR);
            assert!(state.linked_in(w.ws).is_none(), "a No links nothing");
        }
    }
}

/// M8: a computer with no terminal exits 4 before it asks anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_computer_without_a_terminal_exits_4() {
    let w = world().await;
    let addr = address(&w).await;
    let e = ac::cmd_devices_request_to(&fresh(&w.root.join("laptop")), to(&addr, "laptop", None), false, None).await.unwrap_err();
    assert_eq!(classify(&e).0, EXIT_HUMAN);
    assert_eq!(live_count(&w).await, 0, "nothing was posted");
    // And a headless approver without a code.
    let e = ac::cmd_approve(&w.desk, approve(&w, "00"), None).await.unwrap_err();
    assert_eq!(classify(&e).0, EXIT_HUMAN);
}

/// M9: a relay without the mailbox routes is exit 3, and the fix names the
/// paste path; an older desktop that never polls leaves the box at exit 4
/// past --wait, its paste text already printed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_routes_is_exit_3_and_an_older_desktop_is_exit_4() {
    let w = world().await;
    let addr = address(&w).await;
    let bare = spawn(axum::Router::new().route("/v1/health", axum::routing::get(|| async { "ok" }))).await;
    let mut old = addr.clone();
    old.relay = bare;
    let e = ac::cmd_admit_request_to(&fresh(&w.root.join("box1")), to(&old, "build-1", None), None).await.unwrap_err();
    let p = e.downcast_ref::<crate::problem::Problem>().unwrap();
    assert_eq!(p.exit, EXIT_FIXABLE);
    assert!(p.fix.as_deref().unwrap_or_default().contains("ligus admit request"), "{:?}", p.fix);
    let mut o = to(&addr, "build-1", None);
    o.wait = Duration::from_secs(2);
    let e = ac::cmd_admit_request_to(&fresh(&w.root.join("box2")), o, None).await.unwrap_err();
    assert_eq!(classify(&e).0, EXIT_HUMAN);
    assert_eq!(live_count(&w).await, 0, "cancelled and settled");
}

/// M12: the passkey sheet names the label and the code for a code-match
/// approval, and nothing about the code reaches the log.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_passkey_sheet_names_the_label_and_the_code() {
    let w = world().await;
    let mut svc = open_service(&w.desk).unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    struct Rec(std::sync::Arc<std::sync::Mutex<Vec<String>>>, ligus_core::presence::RecoveryKey);
    impl ligus_runtime::chat_service::PresenceProvider for Rec {
        fn assert(&self, req: &ligus_runtime::chat_service::PresenceRequest) -> Result<ligus_core::presence::PresenceAssertion, String> {
            self.0.lock().unwrap().push(req.summary.clone());
            Ok(self.1.assert(&req.challenge))
        }
    }
    let acct = svc.author().account_id.unwrap();
    svc.set_presence_provider(Some(std::sync::Arc::new(Rec(seen.clone(), ligus_core::testkit::test_recovery_key(acct)))));
    let k = ligus_core::crypto::SigningKeypair::generate().unwrap();
    let ka = ligus_core::KeyAgreementKeypair::generate().unwrap();
    let pk = k.public_key_bytes();
    let join = ligus_runtime::box_admission::JoinRequest::new(
        Uuid::new_v4(),
        ligus_core::crypto::device_id_for(ligus_core::crypto::SigAlg::Ed25519, &pk),
        &pk,
        &ka.public_key_bytes(),
        "build-1",
    );
    svc.set_match_context(Some(ligus_runtime::chat_service::MatchContext { label: "build-1".into(), code: "k7q4m9x2".into() }));
    svc.issue_box_admission(w.ws, ligus_runtime::box_admission::DEFAULT_TTL_MS, Some(&join)).unwrap();
    // Without a code match the summary is the generic one again.
    svc.issue_box_admission(w.ws, ligus_runtime::box_admission::DEFAULT_TTL_MS, Some(&join)).unwrap();
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen[0], "Admit box \u{201C}build-1\u{201D}, code K7Q4 M9X2");
    assert_eq!(seen[1], "Let someone join with a single-use invite");
    let envs = svc.store().config_envelopes(w.ws).unwrap();
    assert!(!format!("{envs:?}").contains("K7Q4"), "nothing about the code on the log");
}

// ---- Regressions from the security review ----

/// Another member joins (through an invite from Alex), with a recovery key.
async fn join_member(w: &World, name: &str) -> (Config, String) {
    let cfg = config(&w.root.join(name), &w.relay, &w.room);
    let (inv, seed, pin) = {
        let mut svc = open_service(&w.desk).unwrap();
        let inv = svc.issue_invite(w.ws).unwrap();
        let seed = svc.take_invite_redeem_seed(w.ws, inv).unwrap();
        (inv, seed, svc.store().workspace_pin(w.ws).unwrap().unwrap())
    };
    sync_once(&w.desk).await.unwrap();
    {
        let svc = open_service(&cfg).unwrap();
        svc.store().set_workspace_pin(w.ws, &pin.founder_account.to_string(), &pin.fk.iter().map(|b| format!("{b:02x}")).collect::<String>()).unwrap();
        svc.store().set_join_invite(w.ws, inv).unwrap();
        svc.store().set_join_redeem(w.ws, seed.as_ref()).unwrap();
    }
    sync_once(&cfg).await.unwrap();
    let acct = {
        let mut svc = open_service(&cfg).unwrap();
        svc.ensure_self_member(workspace_config_session_id(w.ws), w.ws).unwrap();
        let acct = svc.author().account_id.unwrap();
        let key = ligus_core::testkit::test_recovery_key(acct);
        svc.register_passkey(w.ws, key.credential(acct, "Recovery key"), &|req| Ok(key.assert(&req.challenge))).unwrap();
        acct
    };
    sync_once(&cfg).await.unwrap();
    sync_once(&w.desk).await.unwrap();
    let words = w.root.join(format!("{name}-words"));
    ligus_runtime::private_file::write_private_atomic(&words, ligus_core::testkit::test_recovery_key(acct).words().as_bytes()).unwrap();
    (cfg, words.display().to_string())
}

fn device_key(cfg: &Config) -> ligus_core::crypto::SigningKeypair {
    let svc = open_service(cfg).unwrap();
    let ids = ligus_runtime::IdentityStore::new(&cfg.data_dir, ligus_runtime::secret_store::StoreKeyVault::for_data_dir(&cfg.data_dir));
    ids.device_keypair(svc.device_id()).unwrap()
}

async fn live_of(w: &World, rid: &str) -> am::LiveRequest {
    let svc = open_service(&w.desk).unwrap();
    let keys = svc.approval_epoch_keys(w.ws, &crate::key_source(&w.desk).unwrap()).unwrap();
    am::list_live(&am::MailboxClient::new(&w.relay), &keys).await.unwrap().live.into_iter().find(|l| l.rid_hex() == rid).unwrap()
}

/// `racer` (a device key that isn't an Admin's) opens the box's request
/// first and sends a well-formed admission for an invite never issued.
async fn race_a_fake_admission(w: &World, rid: &str, racer: ligus_core::crypto::SigningKeypair) {
    let (keys, pin) = {
        let svc = open_service(&w.desk).unwrap();
        (svc.approval_epoch_keys(w.ws, &crate::key_source(&w.desk).unwrap()).unwrap(), svc.founder_pin_hex(w.ws).unwrap().unwrap())
    };
    let live = live_of(w, rid).await;
    let me = am::ApproverIdentity {
        signer: am::Signer::Ed25519(racer),
        device_name: "Alex's desktop".into(),
        person: am::ApproverPerson { account_id: Uuid::new_v4(), name: "Alex".into() },
    };
    let mut s = am::ApproverSession::begin(am::MailboxClient::new(&w.relay), &keys[0], &live.request, me, &mut am::SeenSet::in_memory()).await.unwrap();
    while s.await_reveal(5).await.unwrap().is_none() {}
    let am::RequestSubject::Box(join) = &live.request.subject else { panic!() };
    let conn = ligus_runtime::workspace_invite::WorkspaceConn {
        name: String::new(),
        relay_url: w.relay.clone(),
        room: w.room.clone(),
        key: Some(PASSPHRASE.into()),
        icon: None,
        dm_account: None,
        founder_actor_id: Some(Uuid::new_v4().to_string()),
        fk: Some(pin),
        inv: Some(Uuid::new_v4().to_string()),
        ek: Vec::new(),
        sig: None,
    };
    let invite = ligus_runtime::box_admission::with_redeem_key(&ligus_runtime::workspace_invite::encode_workspace_invite(&conn), &[4; 32]).unwrap();
    s.finish_approved(Some(&join.seal(&invite).unwrap())).await.unwrap();
}

/// M6b: a member who isn't an Admin, a service member (a box), and a device
/// revoked (the workspace key it still holds) each race an open and a
/// grant: the box never joins, and Alex's sheet reads contested.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn headless_box_capture_attempts_all_fail() {
    let w = world().await;
    let addr = address(&w).await;
    let (member, _) = join_member(&w, "member").await;
    // A box that joined properly, through the mailbox.
    let svc_box = fresh(&w.root.join("svc-box"));
    {
        let (tx, rx) = mpsc::channel();
        let o = to(&addr, "svc-box", Some(tx));
        let bc = svc_box.clone();
        let boxed = tokio::spawn(async move { ac::cmd_admit_request_to(&bc, o, None).await.map_err(|e| e.to_string()) });
        let rid = first_rid(&w).await;
        let mut typed = || Ok(rx.recv_timeout(Duration::from_secs(30))?);
        ac::cmd_approve(&w.desk, approve(&w, &rid), Some(&mut typed)).await.unwrap();
        boxed.await.unwrap().unwrap();
    }
    let f = crate::load_file_config(&svc_box.data_dir);
    let svc_box = Config { room: w.room.clone(), relay_url: f.relay_url.clone(), key: f.key.clone(), epoch_keys: f.ek.clone(), ..svc_box };
    sync_once(&svc_box).await.unwrap();
    open_service(&svc_box).unwrap().ensure_self_member(workspace_config_session_id(w.ws), w.ws).unwrap();
    sync_once(&svc_box).await.unwrap();
    // A device revoked by Alex (no rotation yet: it still holds the key).
    let (revoked, _) = join_member(&w, "revoked").await;
    let revoked_id = open_service(&revoked).unwrap().device_id();
    {
        let mut svc = open_service(&w.desk).unwrap();
        let acct = svc.author().account_id.unwrap();
        svc.set_presence_provider(Some(std::sync::Arc::new(crate::admit::RecoveryPresence(ligus_core::testkit::test_recovery_key(acct)))));
        svc.revoke_device(workspace_config_session_id(w.ws), w.ws, revoked_id).unwrap();
    }
    sync_once(&w.desk).await.unwrap();

    for (who, racer) in [("a member", device_key(&member)), ("a box", device_key(&svc_box)), ("a revoked device", device_key(&revoked))] {
        let boxc = fresh(&w.root.join(format!("victim-{}", who.replace(' ', "-"))));
        let mut o = to(&addr, "build-1", None);
        o.log_retry = Duration::from_secs(3);
        let bc = boxc.clone();
        let boxed = tokio::spawn(async move { ac::cmd_admit_request_to(&bc, o, None).await.map_err(|e| (classify(&e).0, e.to_string())) });
        let rid = first_rid(&w).await;
        race_a_fake_admission(&w, &rid, racer).await;
        // Alex's sheet, meanwhile: contested.
        let live = live_of(&w, &rid).await;
        let svc = open_service(&w.desk).unwrap();
        let view = am::claim_view(&live, svc.device_id(), &svc.approval_classifier(w.ws).unwrap());
        assert!(matches!(view, am::ClaimView::Contested { .. }), "{who}: {view:?}");
        drop(svc);
        let (code, why) = boxed.await.unwrap().unwrap_err();
        assert_eq!(code, EXIT_ERROR, "{who}: {why}");
        assert!(why.contains("workspace log"), "{who}: {why}");
        assert!(crate::load_file_config(&boxc.data_dir).room.is_none(), "{who}: nothing was kept");
    }
}

/// M6d: another member (Mallory) races Ana's computer request and
/// approves it from their own desktop: the computer shows Mallory as the
/// verified person, and a No links nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_racing_my_computer_request_is_named_and_a_no_links_nothing() {
    let w = world().await;
    let addr = address(&w).await;
    let (mallory, mallory_words) = join_member(&w, "mallory").await;
    let mallory_name = open_service(&mallory).unwrap().author().display_name.clone();
    let comp = fresh(&w.root.join("laptop"));
    let (tx, rx) = mpsc::channel();
    let o = to(&addr, "laptop", Some(tx));
    let cc = comp.clone();
    let prompts = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let p2 = prompts.clone();
    let linking = tokio::spawn(async move {
        let mut ask = move |q: &str| {
            p2.lock().unwrap().push(q.to_string());
            Ok(!q.starts_with("Link this computer"))
        };
        ac::cmd_devices_request_to(&cc, o, false, Some(&mut ask)).await.map_err(|e| (classify(&e).0, e.to_string()))
    });
    let rid = first_rid(&w).await;
    let mut typed = || Ok(rx.recv_timeout(Duration::from_secs(30))?);
    let o = ApproveOptions { rid: rid.clone(), code: None, words: Some(mallory_words), json: true, end: false };
    ac::cmd_approve(&mallory, o, Some(&mut typed)).await.unwrap();
    let (code, _) = linking.await.unwrap().unwrap_err();
    assert_eq!(code, EXIT_ERROR);
    let asked = prompts.lock().unwrap().clone();
    let want = format!("Link this computer as {} (verified on the workspace log)?", ligus_runtime::display::shown(&mallory_name));
    assert!(asked.contains(&want), "{asked:?}");
    let state = ligus_runtime::computer_link::LinkState::load(&comp.data_dir).unwrap();
    assert!(state.person.is_none() && state.linked_in(w.ws).is_none(), "nothing linked");
}

/// An expired code writes nothing: past 5 minutes, typing the right code
/// signs and publishes no admission (the clock is moved, not waited).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_expired_code_writes_nothing_on_the_log() {
    let (router, clock) = test_relay::router_with_clock();
    let w = world_on(router).await;
    let addr = address(&w).await;
    let before = config_state(&w);
    let boxc = fresh(&w.root.join("box"));
    let (tx, rx) = mpsc::channel();
    let mut o = to(&addr, "build-1", Some(tx));
    o.wait = Duration::from_secs(60);
    let bc = boxc.clone();
    let boxed = tokio::spawn(async move { ac::cmd_admit_request_to(&bc, o, None).await.map_err(|e| (classify(&e).0, e.to_string())) });
    let rid = first_rid(&w).await;
    let desk_dir = w.desk.data_dir.clone();
    let mut typed = || {
        let real = rx.recv_timeout(Duration::from_secs(30))?;
        // The person took longer than the code lives.
        let late = ligus_core::match_code::CODE_TTL_MS + 5_000;
        clock.advance(late);
        ac::CLOCK_SKEW.lock().unwrap().insert(desk_dir.clone(), late);
        Ok(real)
    };
    let e = ac::cmd_approve(&w.desk, approve(&w, &rid), Some(&mut typed)).await.unwrap_err();
    ac::CLOCK_SKEW.lock().unwrap().remove(&w.desk.data_dir);
    assert_eq!(classify(&e).1, "expired", "{e}");
    let (code, _) = boxed.await.unwrap().unwrap_err();
    assert_eq!(code, EXIT_ERROR);
    sync_once(&w.desk).await.unwrap();
    assert_eq!(config_state(&w), before, "nothing on the log");
}

/// A request another Admin is checking is refused (exit 6); a contested
/// one isn't approved (exit 4) and `--end` ends it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approve_respects_claims_and_ends_contested_requests() {
    let w = world().await;
    let addr = address(&w).await;
    let (ben, _) = join_member(&w, "ben").await;
    // Ben is an Admin.
    {
        let mut svc = open_service(&w.desk).unwrap();
        let acct = svc.author().account_id.unwrap();
        svc.set_presence_provider(Some(std::sync::Arc::new(crate::admit::RecoveryPresence(ligus_core::testkit::test_recovery_key(acct)))));
        let ben_id = open_service(&ben).unwrap().author().id.clone();
        svc.set_member_role(workspace_config_session_id(w.ws), w.ws, ben_id, ligus_core::WorkspaceRole::Admin).unwrap();
    }
    sync_once(&w.desk).await.unwrap();
    let (vic, _) = join_member(&w, "vic").await;
    for (racer, want) in [(&ben, crate::problem::EXIT_REFUSED), (&vic, EXIT_HUMAN)] {
        let boxc = fresh(&w.root.join(format!("box-{want}")));
        let mut o = to(&addr, "build-1", None);
        o.wait = Duration::from_secs(5);
        let bc = boxc.clone();
        let boxed = tokio::spawn(async move { ac::cmd_admit_request_to(&bc, o, None).await.map_err(|e| classify(&e).0) });
        let rid = first_rid(&w).await;
        // The racer claims (and opens) first.
        let (keys, live) = {
            let svc = open_service(&w.desk).unwrap();
            (svc.approval_epoch_keys(w.ws, &crate::key_source(&w.desk).unwrap()).unwrap(), live_of(&w, &rid).await)
        };
        let me = am::ApproverIdentity {
            signer: am::Signer::Ed25519(device_key(racer)),
            device_name: "racer".into(),
            person: am::ApproverPerson { account_id: Uuid::new_v4(), name: "racer".into() },
        };
        let _s = am::ApproverSession::begin(am::MailboxClient::new(&w.relay), &keys[0], &live.request, me, &mut am::SeenSet::in_memory()).await.unwrap();
        let mut typed = || Ok("0000 0000".to_string());
        let e = ac::cmd_approve(&w.desk, approve(&w, &rid), Some(&mut typed)).await.unwrap_err();
        assert_eq!(classify(&e).0, want, "{e}");
        if want == EXIT_HUMAN {
            let o = ApproveOptions { rid: rid.clone(), code: None, words: None, json: true, end: true };
            ac::cmd_approve(&w.desk, o, None).await.unwrap();
            // The box sees the abort and ends too (and deletes its request).
            assert_eq!(boxed.await.unwrap().unwrap_err(), EXIT_ERROR, "the box was told it ended");
            continue;
        }
        let _ = boxed.await;
    }
}

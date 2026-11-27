//! The passkey hand-off end to end on the in-process test relay (workspace
//! model v3, test S3): a CLI computer linked as Alex's asks Alex's desktop
//! (a desktop-side approver stub on the desk's real folded workspace) to
//! confirm a workspace runtime under rules 3; it waits, is declined, or exits
//! with --no-wait and finishes with `ligus handoff wait`. A relay without
//! the mailbox keeps today's answer.

use std::sync::mpsc;
use std::time::Duration;

use ligus_core::authz::PRESENCE_REQUIRED;
use ligus_core::{workspace_config_session_id, ModelProviderKind, WorkspaceRuntime};
use ligus_runtime::approval_mailbox as am;
use uuid::Uuid;

use crate::approvals_cmd as ac;
use crate::approvals_cmd_tests::{address, approve, first_rid, fresh, spawn, to, world, World};
use crate::handoff::{self, HandoffEnd, HandoffPresence};
use crate::problem::classify;
use crate::{open_service, sync_once, Config};

/// Alex links a laptop (a CLI computer) through the mailbox, as
/// `ligus devices request --to` does; both sides sync.
pub(crate) async fn linked_laptop(w: &World) -> Config {
    let addr = address(w).await;
    let comp = fresh(&w.root.join("laptop"));
    let (tx, rx) = mpsc::channel();
    let o = to(&addr, "laptop", Some(tx));
    let cc = comp.clone();
    let linking = tokio::spawn(async move {
        let mut ask = |_: &str| Ok(true);
        ac::cmd_devices_request_to(&cc, o, false, Some(&mut ask)).await.map_err(|e| e.to_string())
    });
    let rid = first_rid(w).await;
    let mut typed = || Ok(rx.recv_timeout(Duration::from_secs(30))?);
    ac::cmd_approve(&w.desk, approve(w, &rid), Some(&mut typed)).await.unwrap();
    linking.await.unwrap().unwrap();
    let f = crate::load_file_config(&comp.data_dir);
    let lap = Config { room: w.room.clone(), relay_url: f.relay_url.clone(), key: f.key.clone(), epoch_keys: f.ek.clone(), ..comp };
    sync_once(&lap).await.unwrap();
    sync_once(&w.desk).await.unwrap();
    sync_once(&lap).await.unwrap();
    lap
}

fn runtime(name: &str) -> WorkspaceRuntime {
    WorkspaceRuntime::new(format!("rt-{name}"), name, ModelProviderKind::ClaudeCode, "sonnet")
}

/// What the laptop's `ligus setup` does for a workspace runtime: the write,
/// with the hand-off as its passkey. Runs on its own thread (it blocks).
fn add_runtime(lap: &Config, name: &str, timeout: Option<Duration>) -> std::thread::JoinHandle<(Result<(), String>, Option<HandoffEnd>)> {
    let lap = lap.clone();
    let name = name.to_string();
    std::thread::spawn(move || {
        let mut svc = open_service(&lap).unwrap();
        let ws = crate::uuid_of_room(&lap.room);
        let (p, end) = HandoffPresence::for_service(&lap, &svc, "laptop", timeout).expect("the hand-off runs here");
        svc.set_presence_provider(Some(std::sync::Arc::new(p)));
        let r = svc.add_workspace_runtime(ws, runtime(&name)).map_err(|e| e.to_string());
        let end = end.lock().unwrap().clone();
        (r, end)
    })
}

/// The desktop's side: the first hand-off in the mailbox, checked against
/// the desk's own fold and rendered from the draft. Returns it with its view.
async fn desk_sees(w: &World) -> (am::EpochKey, am::LiveRequest, am::PresenceView) {
    let svc = open_service(&w.desk).unwrap();
    let keys = svc.approval_epoch_keys(w.ws, &crate::key_source(&w.desk).unwrap()).unwrap();
    let acct = svc.author().account_id.unwrap();
    let client = am::MailboxClient::new(&w.relay);
    for _ in 0..400 {
        let l = am::list_live(&client, &keys).await.unwrap();
        if let Some(r) = l.live.into_iter().find(|r| r.kind() == ligus_core::match_code::MatchKind::Presence && r.ended.is_none() && r.claims.is_empty()) {
            let view = am::check_presence_ask(&r.request, w.ws, acct, &|d| svc.is_my_device(w.ws, d).unwrap()).expect("the desk may confirm it");
            let epoch = keys.iter().find(|k| k.epoch == r.epoch).unwrap().clone();
            return (epoch, r, view);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no hand-off arrived");
}

fn desk_identity(w: &World) -> am::ApproverIdentity {
    let svc = open_service(&w.desk).unwrap();
    let ids = ligus_runtime::IdentityStore::new(&w.desk.data_dir, ligus_runtime::secret_store::StoreKeyVault::for_data_dir(&w.desk.data_dir));
    am::ApproverIdentity {
        signer: am::Signer::Ed25519(ids.device_keypair(svc.device_id()).unwrap()),
        device_name: "Alex's desktop".into(),
        person: am::ApproverPerson { account_id: svc.author().account_id.unwrap(), name: "Alex".into() },
    }
}

/// Confirm (`yes`) or say Not me on the desk, with the desk's passkey (the
/// test recovery key stands in for the browser ceremony).
async fn desk_answers(w: &World, yes: bool) -> am::PresenceView {
    let (epoch, live, view) = desk_sees(w).await;
    let mut a = am::PresenceApproval::begin(am::MailboxClient::new(&w.relay), &epoch, &live.request, desk_identity(w), &mut am::SeenSet::in_memory())
        .await
        .unwrap();
    if !yes {
        a.decline().await.unwrap();
        return view;
    }
    let svc = open_service(&w.desk).unwrap();
    let req = svc.presence_request_for(&a.ask().draft).unwrap();
    assert_eq!(req.challenge, view.challenge, "the desk signs the challenge it computed from the draft");
    let acct = svc.author().account_id.unwrap();
    a.approve(&ligus_core::testkit::test_recovery_key(acct).assert(&req.challenge)).await.unwrap();
    view
}

fn runtimes(cfg: &Config, ws: Uuid) -> Vec<String> {
    open_service(cfg).unwrap().list_workspace_runtimes(ws).unwrap().into_iter().map(|r| r.name).collect()
}

/// S3 through the CLI: wait and confirm; Not me; --no-wait then `ligus
/// handoff wait`. The desk renders the exact draft and the change reaches the
/// desk's log only with the desk's passkey.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cli_step_is_confirmed_on_the_desktop() {
    let w = world().await;
    let lap = linked_laptop(&w).await;
    let ws = w.ws;

    // 1. Wait, confirm on the desk.
    let t = add_runtime(&lap, "coder", Some(Duration::from_secs(60)));
    let view = desk_answers(&w, true).await;
    assert_eq!(view.summary, "Define workspace runtime \u{201C}coder\u{201D} (ClaudeCode, model \u{201C}sonnet\u{201D})");
    let (r, end) = t.join().unwrap();
    r.unwrap();
    assert_eq!(end, Some(HandoffEnd::Approved));
    sync_once(&lap).await.unwrap();
    sync_once(&w.desk).await.unwrap();
    assert!(runtimes(&w.desk, ws).contains(&"coder".to_string()), "on the desk's log, accepted by its fold");
    let env = open_service(&w.desk)
        .unwrap()
        .store()
        .list(workspace_config_session_id(ws))
        .unwrap()
        .into_iter()
        .find(|e| matches!(&e.payload, ligus_core::SessionEvent::WorkspaceRuntimeUpserted { runtime } if runtime.name == "coder"))
        .unwrap();
    assert_eq!(ligus_core::presence::challenge(&env), view.challenge, "stored exactly as the desk saw it");

    // 2. Not me: nothing is written; the laptop stays a device of Alex's.
    let t = add_runtime(&lap, "nope", Some(Duration::from_secs(60)));
    desk_answers(&w, false).await;
    let (r, end) = t.join().unwrap();
    let why = r.unwrap_err();
    assert!(why.contains("declined on your desktop or phone"), "{why}");
    assert_eq!(end, Some(HandoffEnd::Declined));
    assert!(!runtimes(&lap, ws).contains(&"nope".to_string()));

    // 3. --no-wait: exit as a person's step with the id and code; the desk
    //    confirms meanwhile; `ligus handoff wait` writes it.
    let t = add_runtime(&lap, "later", None);
    let (r, end) = t.join().unwrap();
    assert!(r.unwrap_err().contains(PRESENCE_REQUIRED));
    let Some(HandoffEnd::Pending { rid, code, .. }) = end.clone() else { panic!("{end:?}") };
    let next = handoff::next_lines(end.as_ref().unwrap(), "ligus setup agent").unwrap();
    assert!(next[0].contains(&code) && next[1] == format!("ligus handoff wait {rid}"), "{next:?}");
    let view = desk_answers(&w, true).await;
    assert_eq!(view.code.shown, code, "the desk shows the code the terminal printed");
    handoff::cmd_wait(&lap, Some(&rid), Duration::from_secs(30), true).await.unwrap();
    assert!(runtimes(&lap, ws).contains(&"later".to_string()));
    // Nothing is left waiting, and a second wait finds nothing.
    let e = handoff::cmd_wait(&lap, Some(&rid), Duration::from_secs(1), true).await.unwrap_err();
    assert_eq!(classify(&e).0, crate::problem::EXIT_NOT_FOUND);
}

/// A relay without the mailbox routes: the step keeps today's answer (a
/// person's step that says to use the desktop app or a recovery-words file).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_mailbox_routes_keep_the_desktop_only_answer() {
    let w = world().await;
    let lap = linked_laptop(&w).await;
    let bare = spawn(axum::Router::new().route("/v1/health", axum::routing::get(|| async { "ok" }))).await;
    let old = Config { relay_url: Some(bare), ..lap.clone() };
    let (r, end) = add_runtime(&old, "coder", Some(Duration::from_secs(10))).join().unwrap();
    let why = r.unwrap_err();
    assert!(why.contains(PRESENCE_REQUIRED), "{why}");
    assert!(matches!(end, Some(HandoffEnd::Unavailable(_))), "{end:?}");
    assert!(handoff::next_lines(end.as_ref().unwrap(), "x").is_none(), "the flow's own fix line stays");
    assert!(!runtimes(&lap, w.ws).contains(&"coder".to_string()));
}

// ---- The commands that took only --recovery-words-file ----

/// A phone of Alex's (certified with the recovery key, as Add a phone does).
async fn alexs_phone(w: &World) -> ligus_core::testkit::Phone {
    let phone = ligus_core::testkit::Phone::new();
    let mut svc = open_service(&w.desk).unwrap();
    let acct = svc.author().account_id.unwrap();
    svc.set_presence_provider(Some(std::sync::Arc::new(crate::admit::RecoveryPresence(ligus_core::testkit::test_recovery_key(acct)))));
    svc.certify_phone(w.ws, phone.device_id, &phone.public_key()).unwrap();
    drop(svc);
    sync_once(&w.desk).await.unwrap();
    phone
}

/// The phone's side, as the phone app does it through its box: the box's
/// checks on its fold (here the desk's), then a claim and an answer signed
/// with the phone's own P-256 key (the test recovery key stands in for the
/// passkey ceremony over the request's commit).
async fn phone_answers(w: &World, phone: &ligus_core::testkit::Phone, yes: bool) -> am::PresenceView {
    let (epoch, live, view) = desk_sees(w).await;
    let acct = open_service(&w.desk).unwrap().author().account_id.unwrap();
    let me = am::ApproverIdentity {
        signer: am::Signer::P256(phone.key.clone()),
        device_name: "Alex's iPhone".into(),
        person: am::ApproverPerson { account_id: acct, name: "Alex".into() },
    };
    let mut a = am::PresenceApproval::begin(am::MailboxClient::new(&w.relay), &epoch, &live.request, me, &mut am::SeenSet::in_memory()).await.unwrap();
    if yes {
        a.approve(&ligus_core::testkit::test_recovery_key(acct).assert(&live.request.commit)).await.unwrap();
    } else {
        a.decline().await.unwrap();
    }
    view
}

/// A computer's link request (as `ligus devices request` prints it) and its fingerprint.
fn link_request(w: &World, name: &str) -> (String, String) {
    let dir = w.root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let comp = Config { data_dir: dir.clone(), relay_url: None, token: None, room: "default".into(), key: None, epoch_keys: Vec::new() };
    crate::devices_cmd::cmd_request(&comp, Some(name.into()), false, true, None).unwrap();
    let state = ligus_runtime::computer_link::LinkState::load(&dir).unwrap();
    let request = state.latest_pending(ligus_core::flow::now_ms()).unwrap().code.clone();
    let fp = ligus_runtime::computer_link::LinkRequest::decode(&request).unwrap().fingerprint();
    (request, fp)
}

/// `ligus devices approve` without a recovery-words file, on its own thread
/// (it blocks while it waits), with the hand-off as its passkey.
fn devices_approve(cfg: &Config, request: &str, fp: &str, out: &std::path::Path, wait: Duration) -> std::thread::JoinHandle<anyhow::Result<()>> {
    let (cfg, request, fp, out) = (cfg.clone(), request.to_string(), fp.to_string(), out.display().to_string());
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new().unwrap().block_on(crate::devices_cmd::cmd_approve(&cfg, Some(&request), None, Some(fp), None, Some(out), true, None, Some(wait)))
    })
}

/// Whether the desk's log holds a certificate for `device`, with presence.
fn certificate_of(w: &World, device: Uuid) -> bool {
    open_service(&w.desk).unwrap().store().list(workspace_config_session_id(w.ws)).unwrap().into_iter().any(|e| {
        matches!(&e.payload, ligus_core::SessionEvent::DeviceCertificateAdded { certificate } if certificate.device_id == device)
            && e.actor_stamp.as_ref().is_some_and(|s| s.presence.is_some())
    })
}

fn phone_code(phone: &ligus_core::testkit::Phone) -> ligus_runtime::phone_enroll::PhoneCode {
    ligus_runtime::phone_enroll::PhoneCode::new_signed(phone.device_id, phone.public_key(), "Alex's iPad", |m| ligus_core::crypto::sign_p256(&phone.key, m))
}

/// `ligus devices approve`, `ligus phones certify` and `ligus admit issue`
/// with no recovery-words file: each sends its one write to Alex's own
/// devices and waits; Alex's phone confirms it with the passkey and the
/// command finishes (the grant, the certificate, the admission code).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_recovery_word_commands_wait_for_your_desktop_or_phone() {
    let w = world().await;
    let phone = alexs_phone(&w).await;

    // devices approve
    let (request, fp) = link_request(&w, "laptop");
    let grant = w.root.join("grant");
    let t = devices_approve(&w.desk, &request, &fp, &grant, Duration::from_secs(60));
    let view = phone_answers(&w, &phone, true).await;
    assert!(view.summary.starts_with("Add device"), "the change names the computer: {}", view.summary);
    t.join().unwrap().unwrap();
    assert!(std::fs::read_to_string(&grant).unwrap().trim().len() > 20, "the grant for the computer");
    let comp = ligus_runtime::computer_link::LinkRequest::decode(&request).unwrap().device_id;
    assert!(certificate_of(&w, comp), "certified with the phone's confirmation");

    // phones certify
    let ipad = ligus_core::testkit::Phone::new();
    let (desk, code, f) = (w.desk.clone(), phone_code(&ipad).encode(), phone_code(&ipad).fingerprint());
    let t = std::thread::spawn(move || crate::admit::cmd_phones_certify(&desk, Some(&code), None, None, Some(f), None, Some(Duration::from_secs(60))).map(|_| ()));
    let view = phone_answers(&w, &phone, true).await;
    assert!(view.summary.starts_with("Enroll phone"), "{}", view.summary);
    t.join().unwrap().unwrap();
    assert!(certificate_of(&w, ipad.device_id), "the phone certified with the other phone's confirmation");

    // admit issue
    let out = w.root.join("admission");
    let (desk, o) = (w.desk.clone(), out.display().to_string());
    let t = std::thread::spawn(move || {
        let issue = "issue".to_string();
        crate::admit::cmd_admit(&desk, &[&issue], |f| match f {
            "--out" => Some(o.clone()),
            "--timeout" => Some("60s".into()),
            _ => None,
        })
    });
    phone_answers(&w, &phone, true).await;
    t.join().unwrap().unwrap();
    assert!(!std::fs::read_to_string(&out).unwrap().trim().is_empty(), "the admission code");
}

/// Not me on the phone, no answer in time, and a relay without the mailbox:
/// nothing is written, each in its own words and exit code.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn not_me_a_timeout_and_no_mailbox_on_the_recovery_word_commands() {
    let w = world().await;
    let phone = alexs_phone(&w).await;
    let (request, fp) = link_request(&w, "laptop");
    let comp = ligus_runtime::computer_link::LinkRequest::decode(&request).unwrap().device_id;
    let certified = || certificate_of(&w, comp);

    // Not me.
    let t = devices_approve(&w.desk, &request, &fp, &w.root.join("g1"), Duration::from_secs(60));
    phone_answers(&w, &phone, false).await;
    let e = t.join().unwrap().unwrap_err();
    assert_eq!(classify(&e), (crate::problem::EXIT_ERROR, "declined"), "{e:#}");
    assert!(!certified());

    // Nobody answers in time.
    let e = devices_approve(&w.desk, &request, &fp, &w.root.join("g2"), Duration::from_secs(2)).join().unwrap().unwrap_err();
    assert_eq!(classify(&e), (crate::problem::EXIT_HUMAN, "not_confirmed"), "{e:#}");
    assert!(!certified());

    // A relay without the mailbox routes: the old answer (use the desktop,
    // or a recovery-words file).
    let bare = spawn(axum::Router::new().route("/v1/health", axum::routing::get(|| async { "ok" }))).await;
    let old = Config { relay_url: Some(bare), ..w.desk.clone() };
    let e = devices_approve(&old, &request, &fp, &w.root.join("g3"), Duration::from_secs(10)).join().unwrap().unwrap_err();
    assert_eq!(classify(&e), (crate::problem::EXIT_HUMAN, "presence_needed"), "{e:#}");
    let fix = e.chain().find_map(|c| c.downcast_ref::<crate::problem::Problem>()).and_then(|p| p.fix.clone());
    assert!(fix.is_some_and(|f| f.contains("--recovery-words-file")), "{e:#}");
    assert!(!certified());
}

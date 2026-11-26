//! The native path with a mock authenticator (a software authenticator
//! behind the [`NativeAuthenticator`] trait): no platform API, no prompt.

use super::*;
use ligus_core::presence::soft::SoftAuthenticator;
use ligus_core::presence::{self, b64url, PasskeyCredential, PasskeyKey};
use std::sync::atomic::AtomicUsize;
use uuid::Uuid;

/// What the mock does when called.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Answer,
    /// Answer, but sign another challenge.
    WrongChallenge,
    Unavailable,
    Failed,
    Refused,
    /// The person closes the system dialog.
    Dismiss,
    /// Never answers until cancelled.
    Hang,
}

struct Mock {
    soft: SoftAuthenticator,
    mode: Mode,
    /// The origin rule: `None` = this authenticator can't serve the RP.
    serves: bool,
    calls: AtomicUsize,
    cancels: AtomicUsize,
    cancelled: AtomicBool,
    seen_get: Mutex<Option<GetRequest>>,
    seen_create: Mutex<Option<CreateRequest>>,
}

impl Mock {
    fn new(mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            soft: SoftAuthenticator::es256(),
            mode,
            serves: true,
            calls: AtomicUsize::new(0),
            cancels: AtomicUsize::new(0),
            cancelled: AtomicBool::new(false),
            seen_get: Mutex::new(None),
            seen_create: Mutex::new(None),
        })
    }
    fn not_serving() -> Arc<Self> {
        let mut m = Arc::into_inner(Self::new(Mode::Answer)).unwrap();
        m.serves = false;
        Arc::new(m)
    }
    fn outcome(&self) -> Result<(), NativeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.mode {
            Mode::Answer | Mode::WrongChallenge => Ok(()),
            Mode::Unavailable => Err(NativeError::Unavailable(
                "not associated with the domain".into(),
            )),
            Mode::Failed => Err(NativeError::Failed("authenticator error".into())),
            Mode::Refused => Err(NativeError::Refused("already registered here".into())),
            Mode::Dismiss => Err(NativeError::Cancelled),
            Mode::Hang => {
                while !self.cancelled.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(NativeError::Cancelled)
            }
        }
    }
    fn challenge<'a>(&self, c: &'a [u8; 32]) -> std::borrow::Cow<'a, [u8]> {
        if self.mode == Mode::WrongChallenge {
            let mut o = *c;
            o[0] ^= 1;
            std::borrow::Cow::Owned(o.to_vec())
        } else {
            std::borrow::Cow::Borrowed(&c[..])
        }
    }
}

fn raw(a: &PresenceAssertion) -> RawAssertion {
    RawAssertion {
        credential_id: unb64url(&a.credential_id).unwrap(),
        authenticator_data: unb64url(&a.authenticator_data).unwrap(),
        client_data_json: unb64url(&a.client_data_json).unwrap(),
        signature: unb64url(&a.signature).unwrap(),
    }
}

impl NativeAuthenticator for Mock {
    fn origin(&self, policy: &PresencePolicy) -> Option<String> {
        self.serves.then(|| policy.page_origin())
    }
    fn get(&self, req: &GetRequest) -> Result<RawAssertion, NativeError> {
        *self.seen_get.lock().unwrap() = Some(req.clone());
        self.outcome()?;
        Ok(raw(&self.soft.get(&self.challenge(&req.challenge))))
    }
    fn create(&self, req: &CreateRequest) -> Result<RawRegistration, NativeError> {
        *self.seen_create.lock().unwrap() = Some(req.clone());
        self.outcome()?;
        let a = self.soft.create(&self.challenge(&req.challenge));
        Ok(RawRegistration {
            credential_id: unb64url(&a.credential_id).unwrap(),
            attestation_object: unb64url(&a.authenticator_data).unwrap(),
            client_data_json: unb64url(&a.client_data_json).unwrap(),
        })
    }
    fn cancel(&self) {
        self.cancels.fetch_add(1, Ordering::SeqCst);
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct Log(Mutex<Vec<String>>);
impl Events for Log {
    fn stage(&self, stage: Stage) {
        self.0.lock().unwrap().push(format!("{stage:?}"));
    }
    fn done(&self, ok: bool) {
        self.0.lock().unwrap().push(format!("done:{ok}"));
    }
}
impl Log {
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

fn request(cred_ids: Vec<String>) -> PresenceRequest {
    let mut challenge = [0u8; 32];
    challenge[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    PresenceRequest {
        challenge,
        kind: "memberRemoved".into(),
        account_id: Uuid::from_u128(77),
        credential_ids: cred_ids,
        digest_short: ligus_core::keys::hex(&challenge[..8]),
        summary: "Remove Bea".into(),
        policy: PresencePolicy::ligus(),
    }
}

const WAIT: Duration = Duration::from_secs(5);

fn browser_marker() -> Result<PresenceAssertion, String> {
    Ok(PresenceAssertion {
        credential_id: "from-the-browser".into(),
        authenticator_data: String::new(),
        client_data_json: String::new(),
        signature: String::new(),
    })
}

fn as_native(m: &Arc<Mock>) -> Option<Arc<dyn NativeAuthenticator>> {
    Some(m.clone() as Arc<dyn NativeAuthenticator>)
}

/// The native result is the hand-off's assertion, over the requested
/// challenge, and it verifies with the very verifier the fold uses.
#[test]
fn native_get_verifies_like_the_hand_off() {
    let m = Mock::new(Mode::Answer);
    let req = request(vec![b64url(&m.soft.credential_id)]);
    let sig = Signals::new();
    let log = Log::default();
    let a = run_get(as_native(&m), &req, &sig, WAIT, &log, || {
        panic!("no browser")
    })
    .unwrap();
    let key = PasskeyKey::from(&m.soft.credential(req.account_id, "t"));
    req.policy
        .with(|p| presence::verify_get(&a, &key, &req.challenge, p))
        .unwrap();
    assert_eq!(log.take(), ["Native", "done:true"]);
    let seen = m.seen_get.lock().unwrap().clone().unwrap();
    assert_eq!(seen.rp_id, "ligus.ai");
    assert_eq!(seen.origin, "https://ligus.ai");
    assert_eq!(seen.challenge, req.challenge);
    assert_eq!(seen.allow, vec![m.soft.credential_id.clone()]);
}

/// No native authenticator, or one that can't serve the workspace's RP (a
/// self-hosted RP without the app association): the hand-off, untouched.
#[test]
fn no_native_or_unserved_rp_uses_the_hand_off() {
    let req = request(vec![]);
    let sig = Signals::new();
    let log = Log::default();
    assert_eq!(
        run_get(None, &req, &sig, WAIT, &log, browser_marker)
            .unwrap()
            .credential_id,
        "from-the-browser"
    );
    let m = Mock::not_serving();
    assert_eq!(
        run_get(as_native(&m), &req, &sig, WAIT, &log, browser_marker)
            .unwrap()
            .credential_id,
        "from-the-browser"
    );
    assert_eq!(
        run_create(
            as_native(&m),
            &req,
            req.account_id.as_bytes(),
            "Ana",
            &sig,
            WAIT,
            &log,
            browser_marker
        )
        .unwrap()
        .credential_id,
        "from-the-browser"
    );
    assert_eq!(m.calls.load(Ordering::SeqCst), 0);
    assert!(log.take().is_empty(), "the hand-off emits its own events");
}

/// An unavailable or failing native API falls back to the hand-off.
#[test]
fn native_failure_falls_back_to_the_hand_off() {
    for mode in [Mode::Unavailable, Mode::Failed] {
        let m = Mock::new(mode);
        let req = request(vec![]);
        let log = Log::default();
        let out = run_get(
            as_native(&m),
            &req,
            &Signals::new(),
            WAIT,
            &log,
            browser_marker,
        )
        .unwrap();
        assert_eq!(out.credential_id, "from-the-browser", "{mode:?}");
        let out = run_create(
            as_native(&m),
            &req,
            req.account_id.as_bytes(),
            "Ana",
            &Signals::new(),
            WAIT,
            &log,
            browser_marker,
        )
        .unwrap();
        assert_eq!(out.credential_id, "from-the-browser", "{mode:?}");
        assert_eq!(m.calls.load(Ordering::SeqCst), 2);
    }
}

/// A refusal the browser would hit too is an error, not a fallback.
#[test]
fn a_refusal_is_an_error() {
    let m = Mock::new(Mode::Refused);
    let req = request(vec![]);
    let log = Log::default();
    let e = run_create(
        as_native(&m),
        &req,
        req.account_id.as_bytes(),
        "Ana",
        &Signals::new(),
        WAIT,
        &log,
        || panic!("no browser"),
    )
    .unwrap_err();
    assert!(e.contains("already registered"), "{e}");
    assert_eq!(log.take(), ["Native", "done:false"]);
}

/// A result over another challenge is refused, never used.
#[test]
fn a_wrong_challenge_is_refused() {
    let m = Mock::new(Mode::WrongChallenge);
    let req = request(vec![]);
    let log = Log::default();
    let e = run_get(as_native(&m), &req, &Signals::new(), WAIT, &log, || {
        panic!("no browser")
    })
    .unwrap_err();
    assert!(e.contains("different challenge"), "{e}");
    assert_eq!(log.take(), ["Native", "done:false"]);
    let e = run_create(
        as_native(&m),
        &req,
        req.account_id.as_bytes(),
        "Ana",
        &Signals::new(),
        WAIT,
        &log,
        || panic!("no browser"),
    )
    .unwrap_err();
    assert!(e.contains("different challenge"), "{e}");
}

/// While the system dialog is up, the sheet's Cancel, "Use the browser" and
/// the recovery key each end it: the native call is cancelled and the
/// choice is honoured.
#[test]
fn sheet_choices_cancel_the_pending_native_call() {
    let req = request(vec![]);
    let log = Log::default();
    // Cancel.
    let m = Mock::new(Mode::Hang);
    let sig = Arc::new(Signals::new());
    let s2 = sig.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        s2.cancel();
    });
    assert_eq!(
        run_get(as_native(&m), &req, &sig, WAIT, &log, || panic!(
            "no browser"
        ))
        .unwrap_err(),
        "cancelled"
    );
    t.join().unwrap();
    assert_eq!(m.cancels.load(Ordering::SeqCst), 1);
    // The browser instead.
    let m = Mock::new(Mode::Hang);
    let s2 = sig.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        s2.use_browser();
    });
    assert_eq!(
        run_get(as_native(&m), &req, &sig, WAIT, &log, browser_marker)
            .unwrap()
            .credential_id,
        "from-the-browser"
    );
    t.join().unwrap();
    assert_eq!(m.cancels.load(Ordering::SeqCst), 1);
    // The recovery key (a get only), which signs the requested challenge.
    let m = Mock::new(Mode::Hang);
    let k = RecoveryKey::generate().unwrap();
    let pk = k.public_key();
    let s2 = sig.clone();
    let req2_digest = req.digest_short.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        s2.set_recovery(k, &req2_digest);
    });
    let a = run_get(as_native(&m), &req, &sig, WAIT, &log, || {
        panic!("no browser")
    })
    .unwrap();
    t.join().unwrap();
    let rk = PasskeyKey {
        alg: presence::EDDSA,
        public_key: pk.to_vec(),
        recovery: true,
    };
    presence::verify_recovery(&a, &rk, &req.challenge).unwrap();
    assert_eq!(m.cancels.load(Ordering::SeqCst), 1);
}

/// A choice left over from an earlier ceremony doesn't end the next one.
#[test]
fn stale_choices_are_reset() {
    let m = Mock::new(Mode::Answer);
    let req = request(vec![]);
    let sig = Signals::new();
    sig.cancel();
    sig.use_browser();
    sig.set_recovery(RecoveryKey::generate().unwrap(), &req.digest_short);
    let a = run_get(as_native(&m), &req, &sig, WAIT, &Log::default(), || {
        panic!("no browser")
    })
    .unwrap();
    assert_eq!(a.credential_id, b64url(&m.soft.credential_id));
}

/// Closing the system dialog keeps the sheet up (Dismissed) until the
/// person picks the browser, the recovery key or Cancel, or time runs out.
#[test]
fn a_dismissed_dialog_waits_for_the_sheet() {
    let req = request(vec![]);
    let m = Mock::new(Mode::Dismiss);
    let sig = Arc::new(Signals::new());
    let log = Log::default();
    let s2 = sig.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        s2.use_browser();
    });
    assert_eq!(
        run_get(as_native(&m), &req, &sig, WAIT, &log, browser_marker)
            .unwrap()
            .credential_id,
        "from-the-browser"
    );
    t.join().unwrap();
    assert_eq!(log.take(), ["Native", "Dismissed"]);
    assert_eq!(
        m.cancels.load(Ordering::SeqCst),
        0,
        "nothing pending to cancel"
    );
    // Timeout.
    let e = run_get(
        as_native(&m),
        &req,
        &Signals::new(),
        Duration::from_millis(200),
        &log,
        || panic!("no browser"),
    )
    .unwrap_err();
    assert!(e.contains("timed out"), "{e}");
    // A create can't be answered by a recovery key: it's ignored there.
    let sig = Arc::new(Signals::new());
    let s2 = sig.clone();
    let req_digest = req.digest_short.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        s2.set_recovery(RecoveryKey::generate().unwrap(), &req_digest);
        std::thread::sleep(Duration::from_millis(100));
        s2.cancel();
    });
    assert_eq!(
        run_create(
            as_native(&m),
            &req,
            req.account_id.as_bytes(),
            "Ana",
            &sig,
            WAIT,
            &log,
            || panic!("no browser")
        )
        .unwrap_err(),
        "cancelled"
    );
    t.join().unwrap();
}

/// A native call that never returns doesn't hang the ceremony past its
/// deadline, and is cancelled.
#[test]
fn a_hung_native_call_times_out() {
    let m = Mock::new(Mode::Hang);
    let req = request(vec![]);
    let started = Instant::now();
    let e = run_get(
        as_native(&m),
        &req,
        &Signals::new(),
        Duration::from_millis(200),
        &Log::default(),
        || panic!("no browser"),
    )
    .unwrap_err();
    assert!(e.contains("timed out"), "{e}");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(m.cancels.load(Ordering::SeqCst), 1);
}

/// The create request carries what the page's `navigator.credentials.create`
/// does: the account id as user handle, the name, the existing credentials
/// to exclude; its result reads out with the same `verify_create`.
#[test]
fn one_account_gets_a_passkey_per_workspace() {
    let account = uuid::Uuid::new_v4();
    let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    assert_ne!(
        user_handle(account, a),
        user_handle(account, b),
        "a passkey in B must not replace A's"
    );
    assert_eq!(user_handle(account, a), user_handle(account, a));
    assert!(
        user_handle(account, a).len() <= 64,
        "WebAuthn caps the user handle at 64 bytes"
    );
    let m = Mock::new(Mode::Answer);
    let req = request(vec![]);
    let h = user_handle(req.account_id, b);
    run_create(
        as_native(&m),
        &req,
        &h,
        "Ana",
        &Signals::new(),
        WAIT,
        &Log::default(),
        || panic!("no browser"),
    )
    .unwrap();
    assert_eq!(m.seen_create.lock().unwrap().clone().unwrap().user_id, h);
}

#[test]
fn native_create_matches_the_page() {
    let m = Mock::new(Mode::Answer);
    let existing = SoftAuthenticator::es256();
    let req = request(vec![b64url(&existing.credential_id)]);
    let log = Log::default();
    let att = run_create(
        as_native(&m),
        &req,
        req.account_id.as_bytes(),
        "Ana",
        &Signals::new(),
        WAIT,
        &log,
        || panic!("no browser"),
    )
    .unwrap();
    let (id, alg, pk) = req
        .policy
        .with(|p| presence::verify_create(&att, &req.challenge, p))
        .unwrap();
    assert_eq!(
        (id, alg, pk),
        (
            m.soft.credential_id.clone(),
            presence::ES256,
            m.soft.public_key()
        )
    );
    let c = m.seen_create.lock().unwrap().clone().unwrap();
    assert_eq!(c.user_id, req.account_id.as_bytes().to_vec());
    assert_eq!(c.user_name, "Ana");
    assert_eq!(c.exclude, vec![existing.credential_id.clone()]);
    assert_eq!(
        (c.rp_id.as_str(), c.origin.as_str()),
        ("ligus.ai", "https://ligus.ai")
    );
    assert_eq!(log.take(), ["Native", "done:true"]);
}

/// End to end through the real fold: a first passkey made natively, its
/// registration confirmed natively by the new credential, then a
/// presence-gated event confirmed natively. Every signature is over the
/// event digest the fold recomputes.
#[test]
fn native_ceremonies_pass_the_fold() {
    use ligus_core::{ActorIdentity, ActorKind, SigningKeypair, Verdict, WorkspaceAuthz};
    let store = ligus_runtime::EventStore::open_in_memory().unwrap();
    let account = Uuid::new_v4();
    let mut author = ActorIdentity::new(account.to_string(), "Ana", ActorKind::Human);
    author.account_id = Some(account);
    let mut svc = ligus_runtime::chat_service::ChatService::new(
        store,
        SigningKeypair::generate().unwrap(),
        SigningKeypair::generate().unwrap(),
        author,
    );
    svc.disable_test_presence();
    let ws = Uuid::new_v4();
    svc.ensure_workspace_config(ws).unwrap();
    let m = Mock::new(Mode::Answer);
    let policy = svc.workspace_presence_policy(ws).unwrap();
    let create_req = PresenceRequest {
        challenge: [5u8; 32],
        kind: "passkeyRegistered".into(),
        account_id: account,
        credential_ids: vec![],
        digest_short: String::new(),
        summary: String::new(),
        policy: policy.clone(),
    };
    let att = run_create(
        as_native(&m),
        &create_req,
        create_req.account_id.as_bytes(),
        "Ana",
        &Signals::new(),
        WAIT,
        &Log::default(),
        || panic!("no browser"),
    )
    .unwrap();
    let (id, alg, public_key) = policy
        .with(|p| presence::verify_create(&att, &[5u8; 32], p))
        .unwrap();
    let new_id = b64url(&id);
    let cred = PasskeyCredential {
        account_id: account,
        credential_id: new_id.clone(),
        alg,
        public_key,
        label: "Touch ID".into(),
        recovery: false,
        registered_at: ligus_core::Timestamp::now(),
    };
    let m2 = m.clone();
    let env = svc
        .register_passkey(ws, cred, &move |req| {
            let mut req = req.clone();
            req.credential_ids = vec![new_id.clone()];
            run_get(
                as_native(&m2),
                &req,
                &Signals::new(),
                WAIT,
                &Log::default(),
                || panic!("no browser"),
            )
        })
        .unwrap();
    let log = svc
        .store()
        .list(ligus_core::workspace_config_session_id(ws))
        .unwrap();
    let verdicts = ligus_core::fold_decisions(&log, None, None, WorkspaceAuthz::Unchecked);
    assert_eq!(verdicts[&env.event_id], Verdict::Allowed);
    assert_eq!(svc.passkeys_of(ws, account).unwrap().len(), 1);
    // The digest the native authenticator signed is the event's own.
    let a = ligus_core::presence::presence_of(&env).unwrap().unwrap();
    assert_eq!(
        presence::returned_challenge(&a.client_data_json).unwrap(),
        presence::challenge(&env).to_vec()
    );
}

/// The RPs the macOS app serves natively are exactly the entitlements
/// file's `webcredentials:` domains, and ligus.ai is among them.
#[test]
fn webcredential_domains_come_from_the_entitlements() {
    let ent = include_str!("../../Ligus.entitlements");
    assert_eq!(webcredential_domains(ent), vec!["ligus.ai".to_string()]);
    let more = "<array><string>applinks:ligus.ai</string><string>webcredentials:Auth.Example.com?mode=developer</string>\n<string> webcredentials:ligus.ai </string></array>";
    assert_eq!(
        webcredential_domains(more),
        vec!["auth.example.com".to_string(), "ligus.ai".to_string()]
    );
    assert!(
        ent.contains("H5H3NNR456.ai.ligus.desktop"),
        "the application identifier is the team's"
    );
}

/// A stand-in for the app's main thread: runs queued closures in order,
/// unless `blocked` (a synchronous command waiting for the service lock).
type Task = Box<dyn FnOnce() + Send>;

struct FakeMain {
    tx: Mutex<Option<mpsc::Sender<Task>>>,
    blocked: Arc<AtomicBool>,
}

impl FakeMain {
    fn new(blocked: bool) -> Self {
        let (tx, rx) = mpsc::channel::<Task>();
        let b = Arc::new(AtomicBool::new(blocked));
        let b2 = b.clone();
        std::thread::spawn(move || {
            for f in rx {
                while b2.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                f();
            }
        });
        Self {
            tx: Mutex::new(Some(tx)),
            blocked: b,
        }
    }
    fn queue(&self, f: Task) -> bool {
        self.tx
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|t| t.send(f).is_ok())
    }
}

impl Drop for FakeMain {
    fn drop(&mut self) {
        self.blocked.store(false, Ordering::SeqCst);
        self.tx.lock().unwrap().take();
    }
}

const FAST: MainThreadWatch = MainThreadWatch {
    start_wait: Duration::from_millis(150),
    stall: Duration::from_millis(150),
    beat_every: Duration::from_millis(30),
    total: Duration::from_secs(5),
};

/// Queue a ceremony on `main` the way the macOS bridge does: claim the
/// state, then (after `answer_after`) reply.
fn queue_ceremony(
    main: &FakeMain,
    state: &Arc<std::sync::atomic::AtomicU8>,
    tx: mpsc::Sender<Result<u32, NativeError>>,
    answer_after: Duration,
    presented: Arc<AtomicBool>,
) {
    let st = state.clone();
    main.queue(Box::new(move || {
        if st
            .compare_exchange(QUEUED, STARTED, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        presented.store(true, Ordering::SeqCst);
        std::thread::spawn(move || {
            std::thread::sleep(answer_after);
            let _ = tx.send(Ok(7));
        });
    }));
}

/// A responsive main thread: the reply comes through, heartbeats and all.
#[test]
fn main_thread_watch_passes_a_reply_through() {
    let main = FakeMain::new(false);
    let state = Arc::new(std::sync::atomic::AtomicU8::new(QUEUED));
    let (tx, rx) = mpsc::channel();
    let presented = Arc::new(AtomicBool::new(false));
    queue_ceremony(
        &main,
        &state,
        tx,
        Duration::from_millis(400),
        presented.clone(),
    );
    let cancels = AtomicUsize::new(0);
    let r = await_main_thread(&rx, &state, &FAST, &|f| main.queue(f), &|| {
        cancels.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(r, Ok(7));
    assert_eq!(cancels.load(Ordering::SeqCst), 0);
}

/// The main thread is blocked before it presents: give up, and it never
/// presents later (no orphan dialog), so the browser is used.
#[test]
fn main_thread_watch_gives_up_before_presenting() {
    let main = FakeMain::new(true);
    let state = Arc::new(std::sync::atomic::AtomicU8::new(QUEUED));
    let (tx, rx) = mpsc::channel();
    let presented = Arc::new(AtomicBool::new(false));
    queue_ceremony(
        &main,
        &state,
        tx,
        Duration::from_millis(1),
        presented.clone(),
    );
    let r = await_main_thread(&rx, &state, &FAST, &|f| main.queue(f), &|| {});
    assert!(matches!(r, Err(NativeError::Unavailable(_))), "{r:?}");
    assert_eq!(state.load(Ordering::SeqCst), ABANDONED);
    main.blocked.store(false, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !presented.load(Ordering::SeqCst),
        "presented after the waiter gave up"
    );
}

/// The main thread blocks while the dialog is up: the waiter notices the
/// unanswered heartbeat, cancels, and falls back.
#[test]
fn main_thread_watch_notices_a_stall() {
    let main = FakeMain::new(false);
    let state = Arc::new(std::sync::atomic::AtomicU8::new(QUEUED));
    let (tx, rx) = mpsc::channel();
    let presented = Arc::new(AtomicBool::new(false));
    queue_ceremony(
        &main,
        &state,
        tx,
        Duration::from_secs(30),
        presented.clone(),
    );
    let blocked = main.blocked.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        blocked.store(true, Ordering::SeqCst);
    });
    let cancels = AtomicUsize::new(0);
    let started = Instant::now();
    let r = await_main_thread(&rx, &state, &FAST, &|f| main.queue(f), &|| {
        cancels.fetch_add(1, Ordering::SeqCst);
    });
    assert!(matches!(r, Err(NativeError::Unavailable(_))), "{r:?}");
    assert!(presented.load(Ordering::SeqCst));
    assert_eq!(cancels.load(Ordering::SeqCst), 1);
    assert!(started.elapsed() < Duration::from_secs(3));
}

/// Review: two ceremonies never run at once. A `create` (Add a passkey runs
/// it without the service lock) and a presence `get` share the sheet's
/// signals and the platform's one pending-dialog slot, so overlapping them
/// let the sheet's Cancel, browser choice or reset of one land on the other
/// (and on macOS the second dialog cancelled the first). The second waits
/// for the first to end, then starts with fresh choices.
#[test]
fn ceremonies_never_overlap() {
    let req = request(vec![]);
    let sig = Arc::new(Signals::new());
    let creating = Mock::new(Mode::Hang);
    let (c2, s2, r2) = (creating.clone(), sig.clone(), req.clone());
    let create = std::thread::spawn(move || {
        run_create(
            as_native(&c2),
            &r2,
            r2.account_id.as_bytes(),
            "Ana",
            &s2,
            WAIT,
            &Log::default(),
            || panic!("no browser"),
        )
    });
    while creating.calls.load(Ordering::SeqCst) == 0 {
        std::thread::sleep(Duration::from_millis(5));
    }
    let getting = Mock::new(Mode::Answer);
    let (g2, s3, r3) = (getting.clone(), sig.clone(), req.clone());
    let get = std::thread::spawn(move || {
        run_get(as_native(&g2), &r3, &s3, WAIT, &Log::default(), || {
            panic!("no browser")
        })
    });
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        getting.calls.load(Ordering::SeqCst),
        0,
        "the get started while the create's dialog was up"
    );
    // The person cancels the create on the sheet: only the create ends.
    sig.cancel();
    assert_eq!(create.join().unwrap().unwrap_err(), "cancelled");
    let a = get
        .join()
        .unwrap()
        .expect("the get runs after the create, with its own choices");
    assert_eq!(a.credential_id, b64url(&getting.soft.credential_id));
    assert_eq!(creating.cancels.load(Ordering::SeqCst), 1);
    assert_eq!(getting.cancels.load(Ordering::SeqCst), 0);
}

/// Review: the hand-off is serialized the same way (no native API, or an
/// RP the platform doesn't serve).
#[test]
fn hand_offs_never_overlap() {
    let req = request(vec![]);
    let sig = Arc::new(Signals::new());
    let inside = Arc::new(AtomicUsize::new(0));
    let most = Arc::new(AtomicUsize::new(0));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let (s, r, inside, most) = (sig.clone(), req.clone(), inside.clone(), most.clone());
            std::thread::spawn(move || {
                run_get(None, &r, &s, WAIT, &Log::default(), || {
                    let n = inside.fetch_add(1, Ordering::SeqCst) + 1;
                    most.fetch_max(n, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(30));
                    inside.fetch_sub(1, Ordering::SeqCst);
                    browser_marker()
                })
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap().unwrap();
    }
    assert_eq!(most.load(Ordering::SeqCst), 1);
}

/// Review: an abandoned call (its waiter gave up: a timeout, or Cancel
/// before Windows took the cancellation id) that returns while the next
/// ceremony's call is running must not forget the running call, or the
/// sheet's Cancel and browser choice could no longer close its dialog.
#[test]
fn an_abandoned_call_ending_late_keeps_the_running_one_cancellable() {
    let p = PendingOp::<u32>::new();
    let old = p.begin(1);
    let new = p.begin(2);
    p.end(old);
    assert_eq!(
        p.current(),
        Some(2),
        "the running call is still cancellable"
    );
    p.end(new);
    assert_eq!(p.current(), None);
    // In order, each call clears its own.
    let t = p.begin(3);
    assert_eq!(p.current(), Some(3));
    p.end(t);
    assert_eq!(p.current(), None);
}

/// Review: recovery words the person typed on the sheet for one ceremony
/// must not confirm another. Ceremonies run one after another (a second
/// presence write waits for the first), so words submitted as the first
/// one times out land on the next: the sheet showed the first ceremony's
/// event and digest, the key would sign the second's. They are bound to the
/// digest the sheet showed and dropped when it isn't this ceremony's.
#[test]
fn recovery_words_for_another_ceremony_are_dropped() {
    let shown = request(vec![]);
    let pending = request(vec![]);
    assert_ne!(shown.digest_short, pending.digest_short);
    let m = Mock::new(Mode::Hang);
    let sig = Arc::new(Signals::new());
    let s2 = sig.clone();
    let shown_digest = shown.digest_short.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        s2.set_recovery(RecoveryKey::generate().unwrap(), &shown_digest);
        std::thread::sleep(Duration::from_millis(200));
        s2.cancel();
    });
    let r = run_get(as_native(&m), &pending, &sig, WAIT, &Log::default(), || {
        panic!("no browser")
    });
    t.join().unwrap();
    assert_eq!(
        r.map(|a| a.signature.is_empty()).unwrap_err(),
        "cancelled",
        "the words confirmed a ceremony the sheet didn't show"
    );
}

/// Final review P2: Windows ran a native ceremony for whatever RP the
/// workspace rules named.
#[test]
fn windows_runs_natively_only_for_ligus_own_rp() {
    assert!(windows_serves(&PresencePolicy::default()));
    for rp in ["google.com", "github.com", "com", "ligus.ai.evil.example"] {
        assert!(!windows_serves(&PresencePolicy::for_rp(rp)), "{rp}");
    }
}

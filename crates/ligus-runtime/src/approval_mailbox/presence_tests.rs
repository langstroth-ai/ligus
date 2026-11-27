//! The passkey hand-off against the in-process relay (workspace model v3,
//! test S3): the summary comes from the draft, only the person's own devices
//! count, an assertion is good for its exact draft only, and expiry, replay,
//! cancel, Not me and the rate limit hold.

use std::sync::Arc;

use ligus_core::match_code::{hex, MatchKind};
use ligus_core::presence::soft::SoftAuthenticator;
use ligus_core::presence::{PresenceAssertion, RecoveryKey};
use ligus_core::{
    fold_decisions, workspace_config_session_id, ActorIdentity, ActorKind, ModelProviderKind, SessionEventEnvelope, SigningKeypair, Verdict, WorkspaceAuthz,
    WorkspaceRuntime,
};
use uuid::Uuid;

use super::envelope::{seal_blob, sign_message};
use super::*;
use crate::chat_service::{ChatService, PresenceProvider, PresenceRequest};
use crate::test_relay::TestClock;
use crate::EventStore;

const MIN: i64 = 60_000;

struct Soft(Arc<SoftAuthenticator>);
impl PresenceProvider for Soft {
    fn assert(&self, req: &PresenceRequest) -> Result<PresenceAssertion, String> {
        Ok(self.0.get(&req.challenge))
    }
}

struct Desk {
    svc: ChatService,
    ws: Uuid,
    key: Arc<SoftAuthenticator>,
    device: SigningKeypair,
    client: MailboxClient,
    clock: TestClock,
    epoch: EpochKey,
    address: ApprovalAddress,
    tmp: tempfile::TempDir,
}

async fn spawn(router: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

/// Ana founds a workspace on rules 3 with a passkey and a recovery key; her
/// device's key signs drafts; the workspace's mailbox is on a test relay.
async fn desk() -> Desk {
    let store = EventStore::open_in_memory().unwrap();
    let account = Uuid::new_v4();
    let mut author = ActorIdentity::new(account.to_string(), "Ana", ActorKind::Human);
    author.account_id = Some(account);
    let device = SigningKeypair::generate().unwrap();
    let mut svc = ChatService::new(store, SigningKeypair::from_seed(&device.seed_bytes()).unwrap(), SigningKeypair::generate().unwrap(), author);
    svc.disable_test_presence();
    let ws = Uuid::new_v4();
    svc.ensure_workspace_config(ws).unwrap();
    let key = Arc::new(SoftAuthenticator::es256());
    let k1 = key.clone();
    svc.register_passkey(ws, key.credential(account, "test key"), &move |req| Ok(k1.get(&req.challenge))).unwrap();
    let rec = RecoveryKey::generate().unwrap();
    let k2 = key.clone();
    svc.register_passkey(ws, rec.credential(account, "Recovery key"), &move |req| Ok(k2.get(&req.challenge))).unwrap();
    svc.set_presence_provider(Some(Arc::new(Soft(key.clone()))));
    svc.set_workspace_rules(ws, ligus_core::authz::RULES_NEW_WORKSPACE).unwrap();
    svc.set_presence_provider(None);
    let (router, clock) = crate::test_relay::router_with_clock();
    let base = spawn(router).await;
    let client = MailboxClient::new(&base).with_clock(clock.clock());
    let ek = [9u8; 32];
    let counter = client.ensure_registered(&ek).await.unwrap();
    let address = ApprovalAddress::derive(&base, ws, &"ab".repeat(16), 1, &ek, counter);
    Desk { svc, ws, key, device, client, clock, epoch: EpochKey { epoch: 1, key: ek }, address, tmp: tempfile::tempdir().unwrap() }
}

fn runtime(name: &str) -> WorkspaceRuntime {
    WorkspaceRuntime::new(format!("rt-{name}"), name, ModelProviderKind::ClaudeCode, "sonnet")
}

impl Desk {
    /// A draft that needs Ana's passkey (a workspace runtime under rules 3),
    /// as the terminal would post it: captured, nothing written.
    fn draft(&mut self, name: &str) -> SessionEventEnvelope {
        self.svc.capture_presence(true);
        assert!(self.svc.add_workspace_runtime(self.ws, runtime(name)).is_err(), "captured, not written");
        self.svc.take_presence_draft().expect("a draft").env
    }
    fn signer(&self) -> Signer {
        Signer::Ed25519(SigningKeypair::from_seed(&self.device.seed_bytes()).unwrap())
    }
    fn ask(&mut self, name: &str) -> PresenceAsk {
        let d = self.draft(name);
        PresenceAsk::new("build-1", &self.device.public_key_bytes(), &d).unwrap()
    }
    async fn post(&mut self, name: &str) -> PresenceRequester {
        let ask = self.ask(name);
        let summary = crate::chat_service::summarize(&ask.draft.payload);
        PresenceRequester::post(self.client.clone(), &self.address, self.signer(), ask, &summary, Some(self.tmp.path())).await.unwrap()
    }
    async fn listing(&self) -> Listing {
        list_live(&self.client, std::slice::from_ref(&self.epoch)).await.unwrap()
    }
    async fn live(&self, rid: &str) -> LiveRequest {
        self.listing().await.live.into_iter().find(|l| l.rid_hex() == rid).expect("listed")
    }
    fn me(&self) -> ApproverIdentity {
        ApproverIdentity {
            signer: self.signer(),
            device_name: "Ana's desktop".into(),
            person: ApproverPerson { account_id: self.svc.author().account_id.unwrap(), name: "Ana".into() },
        }
    }
    fn check(&self, l: &LiveRequest) -> Result<PresenceView, PresenceRefusal> {
        let acct = self.svc.author().account_id.unwrap();
        check_presence_ask(&l.request, self.ws, acct, &|d| self.svc.is_my_device(self.ws, d).unwrap())
    }
    /// Ana's own devices, as her desk's log has them.
    fn mine(&self) -> std::collections::HashSet<Uuid> {
        self.svc.my_device_ids(self.ws).unwrap().into_iter().collect()
    }
    fn verdict(&self, id: Uuid) -> Verdict {
        let log = self.svc.store().list(workspace_config_session_id(self.ws)).unwrap();
        fold_decisions(&log, None, None, WorkspaceAuthz::Unchecked)[&id]
    }
}

/// S3, the happy path: the approver's card is rendered from the draft, both
/// screens show the same code, the passkey signs that draft's challenge, and
/// the terminal stores exactly the bytes the approver saw.
#[tokio::test]
async fn a_handoff_confirms_the_exact_draft_and_both_screens_show_one_code() {
    let mut d = desk().await;
    let mut req = d.post("coder").await;
    let l = d.live(&req.rid_hex()).await;
    assert_eq!(l.kind(), MatchKind::Presence);
    let view = d.check(&l).unwrap();
    assert_eq!(view.summary, "Define workspace runtime \u{201C}coder\u{201D} (ClaudeCode, model \u{201C}sonnet\u{201D})");
    assert_eq!(view.kind, "workspaceRuntimeUpserted");
    assert_eq!(view.code.shown, req.code().shown, "one code on both screens, before anyone answers");
    assert_eq!(view.challenge, ligus_core::presence::challenge(req.draft()));
    // Desktop: claim, passkey over the request's own challenge, result.
    let mut a = PresenceApproval::begin(d.client.clone(), &d.epoch, &l.request, d.me(), &mut SeenSet::in_memory()).await.unwrap();
    let request = d.svc.presence_request_for(&a.ask().draft).unwrap();
    assert_eq!(request.challenge, view.challenge);
    assert_eq!(request.summary, view.summary);
    a.approve(&d.key.get(&request.challenge)).await.unwrap();
    // Other devices of Ana's see who is checking it.
    let l2 = d.live(&req.rid_hex()).await;
    assert_eq!(l2.claims.len(), 1);
    // Terminal: the assertion, then the draft goes on the log as drafted.
    let out = req.poll(0, &{ let m = d.mine(); move |x| m.contains(&x) }).await.unwrap().expect("answered");
    let PresenceOutcome::Approved { assertion, by_device } = out else { panic!("{out:?}") };
    assert_eq!(by_device, d.svc.device_id());
    let draft = req.draft().clone();
    let payload_before = serde_json::to_string(&draft.payload).unwrap();
    req.settle().await.unwrap();
    let stored = d.svc.commit_presence(d.svc.presence_draft_of(draft.clone()).unwrap(), assertion.clone()).unwrap();
    assert_eq!(serde_json::to_string(&stored.payload).unwrap(), payload_before, "the payload the approver rendered");
    assert_eq!(ligus_core::presence::challenge(&stored), view.challenge, "the challenge the passkey signed");
    assert_eq!(d.verdict(stored.event_id), Verdict::Allowed);
    assert!(d.svc.list_workspace_runtimes(d.ws).unwrap().iter().any(|r| r.name == "coder"));
    // Replay: the same assertion can't be stored twice, and the rid can't be answered twice.
    assert!(d.svc.commit_presence(d.svc.presence_draft_of(draft).unwrap(), assertion).is_err(), "single use");
    let mut seen = SeenSet::in_memory();
    seen.mark(&l.request.mailbox, &l.request.rid, d.client.now()).unwrap();
    let again = PresenceApproval::begin(d.client.clone(), &d.epoch, &l.request, d.me(), &mut seen).await.unwrap_err();
    assert_eq!(again, ApproverError::ReusedRid);
    assert!(d.listing().await.live.is_empty(), "settled: gone from the relay");
}

/// S3: a swapped draft. An assertion over one draft doesn't confirm another
/// (the fold refuses it), and an approval carrying another draft's
/// assertion is ignored by the terminal.
#[tokio::test]
async fn an_assertion_is_good_for_its_exact_draft_only() {
    let mut d = desk().await;
    let mut req = d.post("coder").await;
    let other = d.draft("other");
    let l = d.live(&req.rid_hex()).await;
    // An approver that signs another draft: refused before anything is sent.
    let mut a = PresenceApproval::begin(d.client.clone(), &d.epoch, &l.request, d.me(), &mut SeenSet::in_memory()).await.unwrap();
    let wrong = d.key.get(&ligus_core::presence::challenge(&other));
    assert!(matches!(a.approve(&wrong).await, Err(ApproverError::Msg(_))), "the passkey signed something else");
    // A forged approval with that assertion (sent by Ana's own key): the terminal ignores it.
    let body = ResultBody {
        rid: req.rid_hex(),
        outcome: Outcome::Approved,
        grant: Some(format!("{ASSERTION_PREFIX}{}", super::b64::enc(&serde_json::to_vec(&wrong).unwrap()))),
        grant_digest: None,
    };
    let body = ResultBody { grant_digest: body.grant.as_deref().map(super::envelope::grant_digest), ..body };
    let msg = sign_message(MsgType::Result, &body, &d.signer());
    let blob = seal_blob(&l.request.requester_ka, msg.as_bytes()).unwrap();
    let keys = ligus_core::match_code::MailboxKeys::derive(&d.epoch.key);
    d.client.post_msg(MailboxAuth::Member(&keys), &l.request.mailbox, &l.request.rid, &blob, "requester").await.unwrap();
    assert_eq!(req.poll(0, &{ let m = d.mine(); move |x| m.contains(&x) }).await.unwrap(), None, "not over this draft: still waiting");
    // And the fold: the right draft with the other draft's assertion is refused.
    let r = d.svc.commit_presence(d.svc.presence_draft_of(req.draft().clone()).unwrap(), wrong);
    assert!(r.is_err(), "{r:?}");
    let good = d.key.get(&ligus_core::presence::challenge(&other));
    d.svc.commit_presence(d.svc.presence_draft_of(other).unwrap(), good).unwrap();
}

/// S3: a request that names another draft than it carries (its commit), or
/// a tampered draft (its device signature), never lists; the label is a hint
/// and never the summary.
#[tokio::test]
async fn a_tampered_or_swapped_request_is_refused() {
    let mut d = desk().await;
    let ask = d.ask("coder");
    let other = d.ask("other");
    let rid: [u8; 16] = rand_bytes();
    let ka = ligus_core::e2ee::KeyAgreementKeypair::generate().unwrap();
    let put = |commit: [u8; 32], code: String, rid: [u8; 16]| {
        let body = RequestBody {
            v: 1,
            kind: MatchKind::Presence,
            rid: hex(&rid),
            mailbox: d.address.mailbox_hex(),
            code,
            requester_ka: super::b64::enc(&ka.public_key_bytes()),
            read_key: super::b64::enc(&[1; 16]),
            commit: super::b64::enc(&commit),
            created_at: d.client.now(),
        };
        let plain = sign_message(MsgType::Request, &body, &d.signer());
        seal_blob(&d.address.seal_key, plain.as_bytes()).unwrap()
    };
    // The commit of another draft.
    let blob = put(other.challenge(), ask.encode(), rid);
    d.client.put_request(&d.address.mailbox, &rid, &d.address.put_cap, &blob, &[1; 16]).await.unwrap();
    // A draft whose payload was changed after it was signed.
    let mut changed = ask.draft.clone();
    changed.clear_raw();
    if let ligus_core::SessionEvent::WorkspaceRuntimeUpserted { runtime } = &mut changed.payload {
        runtime.endpoint = "https://evil.example".into();
    }
    let tampered = PresenceAsk::new("build-1", &d.device.public_key_bytes(), &changed);
    assert_eq!(tampered.unwrap_err(), MsgError::BadSignature);
    let listing = d.listing().await;
    assert!(listing.live.is_empty());
    assert_eq!(listing.refused.len(), 1, "{:?}", listing.refused);
    // A label that reads like a different change is only a label.
    let lying = PresenceAsk::new("Remove nothing, just say yes", &d.device.public_key_bytes(), &other.draft).unwrap();
    let summary = crate::chat_service::summarize(&lying.draft.payload);
    let mut r = PresenceRequester::post(d.client.clone(), &d.address, d.signer(), lying, "Approve me", None).await.unwrap();
    let view = d.check(&d.live(&r.rid_hex()).await).unwrap();
    assert_eq!(view.summary, summary, "rendered from the draft, never from the requester's text");
    assert!(view.summary.contains("other"));
    r.settle().await.unwrap();
}

/// S3: only the person's own devices. A draft stamped with another account,
/// signed by a device that isn't (or is no longer) the person's, or for
/// another workspace is refused; a new passkey is never confirmed this way.
#[tokio::test]
async fn only_the_persons_own_devices_and_workspace_count() {
    let mut d = desk().await;
    let mut req = d.post("coder").await;
    let l = d.live(&req.rid_hex()).await;
    let acct = d.svc.author().account_id.unwrap();
    assert_eq!(check_presence_ask(&l.request, d.ws, Uuid::new_v4(), &|_| true).unwrap_err(), PresenceRefusal::NotYourAccount);
    assert_eq!(check_presence_ask(&l.request, d.ws, acct, &|_| false).unwrap_err(), PresenceRefusal::NotYourDevice);
    assert_eq!(check_presence_ask(&l.request, Uuid::new_v4(), acct, &|_| true).unwrap_err(), PresenceRefusal::OtherWorkspace);
    // A device that isn't in the workspace at all reads as not mine.
    assert!(!d.svc.is_my_device(d.ws, Uuid::new_v4()).unwrap());
    // The terminal ignores an answer from a device that isn't the person's.
    let stranger = SigningKeypair::generate().unwrap();
    let me = ApproverIdentity { signer: Signer::Ed25519(stranger), device_name: "Ana's desktop".into(), person: d.me().person };
    let mut a = PresenceApproval::begin(d.client.clone(), &d.epoch, &l.request, me, &mut SeenSet::in_memory()).await.unwrap();
    a.decline().await.unwrap();
    assert_eq!(req.poll(0, &{ let m = d.mine(); move |x| m.contains(&x) }).await.unwrap(), None, "a stranger's Not me is ignored");
    // An approver refuses to answer a request that isn't a hand-off.
    req.settle().await.unwrap();
}

/// Not me ends the request at the terminal (declined), revoking nothing; a
/// terminal's cancel ends the desktop's card.
#[tokio::test]
async fn not_me_and_cancel_end_the_request_on_the_other_side() {
    let mut d = desk().await;
    let mut req = d.post("coder").await;
    let l = d.live(&req.rid_hex()).await;
    let mut a = PresenceApproval::begin(d.client.clone(), &d.epoch, &l.request, d.me(), &mut SeenSet::in_memory()).await.unwrap();
    a.decline().await.unwrap();
    assert!(matches!(req.poll(0, &{ let m = d.mine(); move |x| m.contains(&x) }).await.unwrap(), Some(PresenceOutcome::Declined { .. })));
    assert!(d.svc.is_my_device(d.ws, d.svc.device_id()).unwrap(), "Not me revokes nothing by itself");
    req.settle().await.unwrap();

    let mut req = d.post("again").await;
    let l = d.live(&req.rid_hex()).await;
    let mut a = PresenceApproval::begin(d.client.clone(), &d.epoch, &l.request, d.me(), &mut SeenSet::in_memory()).await.unwrap();
    req.cancel().await.unwrap();
    assert_eq!(a.ready().await.unwrap_err(), ApproverError::Aborted(AbortReason::Cancelled));
    assert!(a.approve(&d.key.get(&[0; 32])).await.is_err(), "nothing is sent after a cancel");
    req.settle().await.unwrap();
}

/// Expiry: past 15 minutes the terminal stops and the desktop refuses; a
/// card opened in time can't confirm once it expired. Three dead hand-offs
/// stop the terminal.
#[tokio::test]
async fn a_handoff_expires_and_dead_ones_stop_the_terminal() {
    let mut d = desk().await;
    let mut req = d.post("coder").await;
    let l = d.live(&req.rid_hex()).await;
    let mut a = PresenceApproval::begin(d.client.clone(), &d.epoch, &l.request, d.me(), &mut SeenSet::in_memory()).await.unwrap();
    d.clock.advance(16 * MIN);
    assert_eq!(a.ready().await.unwrap_err(), ApproverError::RequestExpired);
    let late = PresenceApproval::begin(d.client.clone(), &d.epoch, &l.request, d.me(), &mut SeenSet::in_memory()).await.unwrap_err();
    assert_eq!(late, ApproverError::RequestExpired);
    assert_eq!(req.poll(0, &{ let m = d.mine(); move |x| m.contains(&x) }).await.unwrap(), Some(PresenceOutcome::Expired));
    let _ = req.settle().await;
    for name in ["b", "c"] {
        let mut r = d.post(name).await;
        r.cancel().await.unwrap();
        r.settle().await.unwrap();
    }
    let ask = d.ask("d");
    let e = PresenceRequester::post(d.client.clone(), &d.address, d.signer(), ask, "", Some(d.tmp.path())).await.unwrap_err();
    assert_eq!(e, RequesterError::TooManyDead);
}

/// `--no-wait`: the pending entry resumes with the same code and reads the
/// answer that came meanwhile.
#[tokio::test]
async fn a_pending_handoff_resumes_with_its_code() {
    let mut d = desk().await;
    let req = d.post("coder").await;
    let code = req.code().shown.clone();
    let rid = req.rid_hex();
    drop(req);
    let store = PresencePendingStore::new(d.tmp.path());
    let entry = store.live(d.client.now()).unwrap().into_iter().find(|e| e.rid == rid).unwrap();
    assert_eq!(entry.code, code);
    let l = d.live(&rid).await;
    let mut a = PresenceApproval::begin(d.client.clone(), &d.epoch, &l.request, d.me(), &mut SeenSet::in_memory()).await.unwrap();
    a.approve(&d.key.get(&a.ask().challenge())).await.unwrap();
    let mut back = PresenceRequester::resume(d.client.clone(), &entry, d.signer(), Some(d.tmp.path())).unwrap();
    assert_eq!(back.code().shown, code);
    assert!(matches!(back.poll(0, &{ let m = d.mine(); move |x| m.contains(&x) }).await.unwrap(), Some(PresenceOutcome::Approved { .. })));
    back.settle().await.unwrap();
    assert!(store.load().unwrap().is_empty(), "settled: the pending entry is gone");
    // Another device's key can't pick it up.
    let other = Signer::Ed25519(SigningKeypair::generate().unwrap());
    assert!(PresenceRequester::resume(d.client.clone(), &entry, other, None).is_err());
}

/// Rate limit: an approver shows at most six hand-offs from one device in an
/// hour (the same request stays shown); another device has its own count.
#[test]
fn an_approver_shows_at_most_six_handoffs_per_device_and_hour() {
    let mut rate = AskRate::default();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    for i in 0..ASK_RATE_PER_HOUR {
        assert!(rate.admit(a, &format!("r{i}"), 1_000));
    }
    assert!(!rate.admit(a, "r-next", 2_000));
    assert!(rate.admit(a, "r0", 2_000), "an admitted request stays admitted");
    assert!(rate.admit(b, "r-next", 2_000));
    assert!(rate.admit(a, "r-later", 1_000 + 61 * MIN), "an hour later");
}

/// A draft too large for the relay's request blob is refused before it is
/// posted (the terminal then says to do it on the desktop).
#[tokio::test]
async fn a_draft_too_large_to_hand_off_is_refused() {
    let mut d = desk().await;
    d.svc.capture_presence(true);
    let mut big = runtime("big");
    big.endpoint = format!("https://example.com/{}", "a".repeat(5_000));
    assert!(d.svc.add_workspace_runtime(d.ws, big).is_err());
    let draft = d.svc.take_presence_draft().unwrap().env;
    let e = PresenceAsk::new("build-1", &d.device.public_key_bytes(), &draft).unwrap_err();
    assert!(matches!(e, MsgError::BadCode(_)), "{e:?}");
    // A typical runtime draft fits with room to spare.
    let ask = d.ask("coder");
    assert!(ask.draft_text().len() < MAX_DRAFT_BYTES / 2, "{}", ask.draft_text().len());
}

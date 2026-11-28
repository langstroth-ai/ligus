//! "I already use Ligus": this desktop links itself to the person's account,
//! as one more of their devices (never a second member). The other side,
//! approving, is `approvals_ui` (the Inbox) and My devices > Add a computer.
//!
//! The runtime already has every piece (`ligus_runtime::computer_link`,
//! `ligus_runtime::approval_mailbox::Requester`); this module drives them for
//! the desktop, the way `ligus devices request --to` does for the CLI:
//!
//! 1. The person pastes the approval address their other device shows (My
//!    devices > Add a computer). This desktop makes a link request
//!    (`liguslink1:`: its device key, key-agreement key and name, nothing
//!    secret) and posts it to that address ([`link_request_start`]). No name
//!    is asked and no workspace is made.
//! 2. When the other device opens the request, both screens show the same
//!    code. The person says whether they match here
//!    ([`link_request_codes_match`]); the other device confirms with the
//!    passkey and publishes this desktop's certificate.
//! 3. The grant comes back sealed to this desktop. It is checked (checks 1
//!    to 4 in the runtime), the workspace is pulled, and the certificate must
//!    be on its log (check 5). The person confirms who they link as
//!    ([`link_request_link_as`], check 6), and only then is anything saved:
//!    the link (`computer-link.json`), the workspace, this desktop's own
//!    acceptance of its certificate and its name. From then on the desktop
//!    acts as the person ([`adopt_at_launch`], [`bind_workspace`]).
//!
//! A relay without mailbox routes falls back to the paste path
//! ([`link_request_paste`], [`link_request_open_grant`],
//! [`link_request_redeem`]): the request is pasted into Add a computer on the
//! other device and its grant pasted back here.
//!
//! An install that already has workspaces of its own can't be linked (it
//! would become a bridge between two members); one already linked to the
//! same person can be linked into another of their workspaces.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use ligus_core::match_code::MatchKind;
use ligus_core::{ActorIdentity, KeyAgreementKeypair};
use ligus_runtime::approval_mailbox::{
    self as am, AbortReason, ApprovalAddress, CheckedGrant, GrantTarget, MailboxClient, MailboxError, Outcome, RequestSpec, Requester, RequesterError,
    RequesterEvent, Signer,
};
use ligus_runtime::computer_link::{Grant, LinkRequest, LinkState, OpenedGrant, NOT_ON_RELAY_YET};
use ligus_runtime::workspace_invite::WorkspaceConn;
use ligus_runtime::ChatService;
use serde::Serialize;
use tokio::sync::mpsc;
use uuid::Uuid;

/// The flow moved on. Payload: [`LinkFlowDto`].
pub(crate) const CHANGED_EVENT: &str = "link://changed";
/// How long one poll of the mailbox waits for a message.
const POLL_SECS: u64 = 2;
/// How long the certificate may take to reach this desktop after approval.
#[cfg(not(test))]
const LOG_BUDGET: Duration = Duration::from_millis(am::LOG_CHECK_RETRY_MS as u64);
#[cfg(test)]
const LOG_BUDGET: Duration = Duration::from_secs(3);

fn now_ms() -> i64 {
    ligus_core::flow::now_ms()
}

// ── What the screen shows ────────────────────────────────────────────────

/// Where the flow stands, for the screen. `phase` is one of:
///
/// * `idle`: nothing started;
/// * `waiting`: the request is in the mailbox, waiting for the other device
///   to open it;
/// * `code`: both screens show `code`: do they match?
/// * `approving`: the person said they match; the other device confirms
///   with the passkey;
/// * `checking`: the grant came back; the workspace is being pulled and the
///   approval looked up on its log;
/// * `confirmPerson`: verified: link as `personName`?
/// * `paste`: the paste path: `request` goes to the other device, its code
///   comes back here;
/// * `confirmApprover`: a pasted grant opened: approved by
///   `approverFingerprint` for `workspaceName`: is that your device?
/// * `linking`: saving the link;
/// * `linked`: done: this desktop is one of `personName`'s devices;
/// * `noMailbox`: the relay can't carry requests: use the paste path;
/// * `failed`: `error` says why; `canRetry` whether trying again can help.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LinkFlowDto {
    pub phase: String,
    /// This desktop's request (`liguslink1:…`, no secret), for the paste path.
    pub request: Option<String>,
    /// The request's fingerprint, which Add a computer shows too.
    pub fingerprint: Option<String>,
    /// The name this desktop asks with.
    pub label: Option<String>,
    /// The match code both screens show.
    pub code: Option<String>,
    /// The code spelled out, for a screen reader.
    pub code_spelled: Option<String>,
    /// Who the other device says it is (checked after approval).
    pub claimed_name: Option<String>,
    pub claimed_fingerprint: Option<String>,
    /// The person, once verified (or as a pasted grant names them).
    pub person_name: Option<String>,
    pub workspace_name: Option<String>,
    pub workspace_id: Option<String>,
    pub approver_fingerprint: Option<String>,
    /// A passing problem while waiting (the relay can't be reached; still trying).
    pub notice: Option<String>,
    pub error: Option<String>,
    pub can_retry: bool,
    /// When the other device brings more than one workspace: each one, the
    /// first one first ("Linking 3 workspaces…").
    pub workspaces: Vec<LinkWsDto>,
}

/// One workspace of a link that brings several. `state` is one of
/// `checking`, `ready` (verified, waiting for the person's yes), `linking`,
/// `linked`, `failed` (`error`; `canRetry` when trying again can help, with
/// [`retry`]) or `notIncluded` (the other device didn't add this computer
/// there; `error` says what to do).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LinkWsDto {
    pub workspace_id: String,
    pub name: String,
    pub state: String,
    pub error: Option<String>,
    pub can_retry: bool,
}

fn ws_row(view: &mut LinkFlowDto, ws: Uuid, f: impl FnOnce(&mut LinkWsDto)) {
    if let Some(r) = view.workspaces.iter_mut().find(|r| r.workspace_id == ws.to_string()) {
        f(r);
    }
}

fn view(phase: &str) -> LinkFlowDto {
    LinkFlowDto { phase: phase.into(), ..LinkFlowDto::default() }
}

/// A failure, as the screen says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Failed {
    pub text: String,
    pub retry: bool,
    /// The relay has no mailbox routes: offer the paste path.
    pub no_mailbox: bool,
}

impl Failed {
    fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), retry: true, no_mailbox: false }
    }
    fn final_(text: impl Into<String>) -> Self {
        Self { text: text.into(), retry: false, no_mailbox: false }
    }
}

/// A mailbox error in words a person reads.
pub(crate) fn mailbox_failed(e: &MailboxError) -> Failed {
    match e {
        MailboxError::NoRoutes => Failed {
            text: "Your relay can't pass requests between devices. Use a request code instead.".into(),
            retry: false,
            no_mailbox: true,
        },
        MailboxError::Unreachable(_) | MailboxError::RateLimited(_) => Failed::new(format!("Couldn't reach the relay ({e}). Check the network, then try again.")),
        MailboxError::BadCap | MailboxError::MailboxUnknown => {
            Failed::new("That approval address is out of date. Copy it again from your other device (My devices > Add a computer), then try again.")
        }
        MailboxError::RequestUnknown => Failed::new("The relay lost the request (it may have restarted). Try again."),
        MailboxError::MailboxFull => Failed { text: format!("{e}."), retry: true, no_mailbox: true },
        other => Failed::new(format!("The relay refused the request: {other}.")),
    }
}

fn requester_failed(e: &RequesterError) -> Failed {
    match e {
        RequesterError::Mailbox(m) => mailbox_failed(m),
        RequesterError::TooManyDead => Failed::final_(format!("{e}.")),
        other => Failed::new(format!("{other}.")),
    }
}

fn outcome_failed(o: Outcome) -> Failed {
    match o {
        Outcome::Mismatch => Failed::new("The other device said the codes don't match. Nothing was granted."),
        Outcome::Declined => Failed::new("The other device declined. Nothing was granted."),
        Outcome::Expired | Outcome::Approved => Failed::new("The request expired before it was approved. Try again."),
    }
}

fn abort_failed(r: AbortReason) -> Failed {
    match r {
        AbortReason::Mismatch => Failed::new("The codes didn't match. Nothing was granted."),
        AbortReason::Cancelled => Failed::new("The request was cancelled. Nothing was granted."),
        AbortReason::Contested => Failed::new(
            "Another device answered the request too, so it was ended. Nothing was granted. Try again on a network you trust.",
        ),
    }
}

/// What a person answers on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Answer {
    CodesMatch(bool),
    LinkAs(bool),
    Cancel,
}

// ── The flow's state ─────────────────────────────────────────────────────

#[derive(Default)]
struct SlotInner {
    /// Bumped by every start and cancel: an older driver's updates are dropped.
    gen: u64,
    view: LinkFlowDto,
    answers: Option<mpsc::UnboundedSender<Answer>>,
    /// The paste path: the grant pasted and opened, waiting for "that's my device".
    pasted: Option<(OpenedGrant, Uuid)>,
    /// Workspaces of a linked flow that failed and can be tried again: their
    /// grant (valid 30 minutes) and this computer's name.
    retry: Vec<(OpenedGrant, String)>,
}

/// One link flow (the app has one; tests make their own).
#[derive(Default)]
pub(crate) struct Slot(Mutex<SlotInner>);

impl Slot {
    pub(crate) fn view(&self) -> LinkFlowDto {
        let v = self.0.lock().unwrap().view.clone();
        if v.phase.is_empty() {
            return view("idle");
        }
        v
    }

    fn begin(&self, v: LinkFlowDto) -> (u64, mpsc::UnboundedReceiver<Answer>) {
        let mut s = self.0.lock().unwrap();
        if let Some(tx) = s.answers.take() {
            let _ = tx.send(Answer::Cancel);
        }
        s.gen += 1;
        s.view = v;
        s.pasted = None;
        s.retry.clear();
        let (tx, rx) = mpsc::unbounded_channel();
        s.answers = Some(tx);
        (s.gen, rx)
    }

    /// Change the view if `gen` is still the flow's.
    fn set(&self, gen: u64, f: impl FnOnce(&mut LinkFlowDto)) -> Option<LinkFlowDto> {
        let mut s = self.0.lock().unwrap();
        if s.gen != gen {
            return None;
        }
        f(&mut s.view);
        Some(s.view.clone())
    }

    /// Whether `gen` is still the flow's (not cancelled or replaced).
    fn current(&self, gen: u64) -> bool {
        self.0.lock().unwrap().gen == gen
    }

    fn answer(&self, a: Answer) -> Result<(), String> {
        let s = self.0.lock().unwrap();
        match &s.answers {
            Some(tx) if tx.send(a).is_ok() => Ok(()),
            _ => Err("Nothing is waiting for that answer. Start again.".into()),
        }
    }

    /// Cancel whatever runs and go back to `idle`.
    fn reset(&self) -> LinkFlowDto {
        let mut s = self.0.lock().unwrap();
        if let Some(tx) = s.answers.take() {
            let _ = tx.send(Answer::Cancel);
        }
        s.gen += 1;
        s.view = view("idle");
        s.pasted = None;
        s.retry.clear();
        s.view.clone()
    }
}

// ── What the flow needs from the desktop (the app, or a test) ────────────

/// This desktop as the link flow uses it.
pub(crate) trait LinkHost: Send + Sync + 'static {
    fn data_dir(&self) -> PathBuf;
    /// Run `f` on the chat service (under its lock).
    fn with_svc<R>(&self, f: impl FnOnce(&mut ChatService) -> R) -> R;
    /// This desktop's device signing key and key-agreement key.
    fn keys(&self) -> Result<(ligus_core::crypto::SigningKeypair, KeyAgreementKeypair), String>;
    /// The name this desktop asks with.
    fn label(&self) -> String;
    /// The relay access token for `relay`, if any.
    fn token(&self, relay: &str) -> Option<String>;
    /// Whether this install already has workspaces of its own (not the
    /// private Home one, not ones it was linked into).
    fn own_workspaces(&self, link: &LinkState) -> usize;
    /// One push and pull of `conn`'s workspace.
    fn sync(&self, conn: &WorkspaceConn) -> impl Future<Output = Result<(), String>> + Send;
    /// Keep `conn` as one of this desktop's workspaces and make it active
    /// (after the link is recorded).
    fn adopt_workspace(&self, conn: &WorkspaceConn) -> Result<(), String>;
    /// Keep `conn` as one of this desktop's workspaces, leaving the active
    /// one as it is (a link's more workspaces).
    fn keep_workspace(&self, conn: &WorkspaceConn) -> Result<(), String>;
    /// The view changed.
    fn changed(&self, view: &LinkFlowDto);
}

/// Whether this install may make a link request, and what it is linked as.
fn may_link<H: LinkHost>(host: &H) -> Result<LinkState, Failed> {
    let link = LinkState::load(&host.data_dir()).map_err(|e| Failed::final_(format!("This computer's link record is unreadable ({e}).")))?;
    let own = host.own_workspaces(&link);
    if own > 0 {
        return Err(Failed::final_(format!(
            "This computer already uses Ligus as its own member ({own} workspace{}), so it can't become another device of yours. Link a fresh install instead, or reset this one (Settings → Advanced).",
            if own == 1 { "" } else { "s" }
        )));
    }
    Ok(link)
}

/// A link request for this desktop, remembered as pending. A fresh one
/// replaces any other this install made.
fn make_request<H: LinkHost>(host: &H) -> Result<(LinkRequest, ligus_core::crypto::SigningKeypair, KeyAgreementKeypair), Failed> {
    let mut link = may_link(host)?;
    let (device, ka) = host.keys().map_err(Failed::final_)?;
    let now = now_ms();
    link.prune_expired(now);
    let request = LinkRequest::new(&device, &ka.public_key_bytes(), &host.label(), now, None);
    link.add_pending(&request, now);
    link.save(&host.data_dir()).map_err(|e| Failed::final_(format!("Couldn't keep the request ({e}).")))?;
    Ok((request, device, ka))
}

/// Parse a pasted approval address. Only `https://` relays, or a loopback
/// `http://` one (a relay on this computer).
pub(crate) fn parse_address(text: &str) -> Result<ApprovalAddress, Failed> {
    let t = text.trim();
    if t.is_empty() {
        return Err(Failed::new("Paste the approval address from your other device (My devices > Add a computer)."));
    }
    let a = ApprovalAddress::decode_with(t, true).map_err(|e| Failed::new(format!("That isn't an approval address ({e}). Copy it again from your other device.")))?;
    let url = url::Url::parse(&a.relay).map_err(|_| Failed::new("That approval address is damaged. Copy it again from your other device."))?;
    let host = url.host_str().unwrap_or_default().trim_start_matches('[').trim_end_matches(']');
    let loopback = host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if url.scheme() == "http" && !loopback {
        return Err(Failed::final_("That approval address's relay isn't an https:// address, so this computer won't use it."));
    }
    Ok(a)
}

fn shown_ws(conn: &WorkspaceConn) -> String {
    crate::shown(&conn.display_name())
}

// ── Starting, answering, cancelling ──────────────────────────────────────

/// A short code that didn't find an address, in words a person reads.
fn code_failed(e: &MailboxError) -> Failed {
    match e {
        MailboxError::NoRoutes => Failed::final_(
            "This relay doesn't know short codes yet. On your other device, copy the approval address instead (My devices > Add a computer) and paste it here.",
        ),
        MailboxError::CodeUnknown => Failed::final_(
            "No device is showing that code. Check it, or get a new one on your other device: a code lasts 10 minutes and works once. A self-hosted relay needs its address under Relay.",
        ),
        MailboxError::Malformed => Failed::final_("The relay's answer for that code didn't check out, so nothing was sent. Get a new code and try again."),
        other => mailbox_failed(other),
    }
}

/// The approval address `input` names: pasted as is, or behind a short code
/// (`K7Q-4MX`) typed for `relay` (the community relay when none is named).
async fn address_of<H: LinkHost>(host: &H, input: &str, relay: Option<&str>) -> Result<ApprovalAddress, Failed> {
    let Some(code) = ligus_core::link_code::ShortCode::parse(input) else { return parse_address(input) };
    let relay = relay.map(str::trim).filter(|r| !r.is_empty()).unwrap_or(ligus_core::COMMUNITY_RELAY_URL).trim_end_matches('/').to_string();
    let a = crate::link_code_ui::resolve_typed(&code, &relay, host.token(&relay)).await.map_err(|e| code_failed(&e))?;
    // The same checks as a pasted address (an https relay, or this computer's own).
    parse_address(&a.encode())
}

/// Post a link request to `address` (an approval address, or a short code
/// on `relay`) and drive it in the background. Returns the first view
/// (`waiting`, or `failed`/`noMailbox`).
pub(crate) async fn start<H: LinkHost>(host: Arc<H>, slot: Arc<Slot>, address: &str, relay: Option<&str>) -> LinkFlowDto {
    let (gen, rx) = slot.begin(view("waiting"));
    let fail = |f: Failed| {
        let v = slot.set(gen, |v| set_failed(v, &f)).unwrap_or_else(|| slot.view());
        host.changed(&v);
        v
    };
    let address = match address_of(&*host, address, relay).await {
        Ok(a) => a,
        Err(f) => return fail(f),
    };
    let (request, device, ka) = match make_request(&*host) {
        Ok(r) => r,
        Err(f) => return fail(f),
    };
    let code = request.encode();
    let device_key = device.public_key_bytes();
    let spec = RequestSpec { kind: MatchKind::Computer, code: code.clone(), signer: Signer::Ed25519(device), ka: Some(clone_ka(&ka)) };
    let client = MailboxClient::new(&address.relay).with_auth(host.token(&address.relay));
    let req = match Requester::post(client, &address, spec, Some(&host.data_dir())).await {
        Ok(r) => r,
        Err(e) => {
            let mut f = requester_failed(&e);
            // The paste path still works with this request.
            if f.no_mailbox {
                f.retry = false;
            }
            let v = slot
                .set(gen, |v| {
                    set_failed(v, &f);
                    v.request = Some(code.clone());
                    v.fingerprint = Some(request.fingerprint());
                    v.label = Some(request.label.clone());
                })
                .unwrap_or_else(|| slot.view());
            host.changed(&v);
            return v;
        }
    };
    let v = slot
        .set(gen, |v| {
            v.request = Some(code.clone());
            v.fingerprint = Some(request.fingerprint());
            v.label = Some(request.label.clone());
        })
        .unwrap_or_else(|| slot.view());
    host.changed(&v);
    let ctx = Ctx { request, ka, device_key };
    let (h, s) = (host.clone(), slot.clone());
    tokio::spawn(async move { drive(h, s, gen, req, ctx, rx).await });
    v
}

fn set_failed(v: &mut LinkFlowDto, f: &Failed) {
    v.phase = if f.no_mailbox && !f.retry { "noMailbox".into() } else { "failed".into() };
    v.error = Some(f.text.clone());
    v.can_retry = f.retry;
    v.notice = None;
}

fn clone_ka(ka: &KeyAgreementKeypair) -> KeyAgreementKeypair {
    KeyAgreementKeypair::from_seed(&ka.seed_bytes()).expect("a key-agreement seed round-trips")
}

struct Ctx {
    request: LinkRequest,
    ka: KeyAgreementKeypair,
    device_key: [u8; 32],
}

/// Run a posted request to its end, then settle it on the relay.
async fn drive<H: LinkHost>(host: Arc<H>, slot: Arc<Slot>, gen: u64, mut req: Requester, ctx: Ctx, mut rx: mpsc::UnboundedReceiver<Answer>) {
    let out = drive_inner(&*host, &slot, gen, &mut req, &ctx, &mut rx).await;
    let _ = req.settle().await;
    let v = match out {
        Ok(()) => slot.view(),
        Err(f) => slot.set(gen, |v| set_failed(v, &f)).unwrap_or_else(|| slot.view()),
    };
    host.changed(&v);
}

async fn drive_inner<H: LinkHost>(
    host: &H,
    slot: &Slot,
    gen: u64,
    req: &mut Requester,
    ctx: &Ctx,
    rx: &mut mpsc::UnboundedReceiver<Answer>,
) -> Result<(), Failed> {
    let update = |f: &dyn Fn(&mut LinkFlowDto)| {
        if let Some(v) = slot.set(gen, |v| f(v)) {
            host.changed(&v);
        }
    };
    let mut confirmed = false;
    let mut offered = false;
    let mut restarts = 0;
    loop {
        // The person's answers, between polls (a poll is never cut short).
        while let Ok(a) = rx.try_recv() {
            match a {
                Answer::Cancel => {
                    let _ = req.abort(AbortReason::Cancelled).await;
                    return Err(Failed::new("Cancelled. Nothing was granted."));
                }
                Answer::CodesMatch(false) => {
                    let _ = req.abort(AbortReason::Mismatch).await;
                    return Err(Failed::new(
                        "The codes don't match, so nothing was granted. Start again; if it happens again, someone else may be answering your request.",
                    ));
                }
                Answer::CodesMatch(true) if req.code().is_some() && !confirmed => {
                    confirmed = true;
                    update(&|v| v.phase = "approving".into());
                    if offered {
                        req.ack_all().await.map_err(|e| requester_failed(&e))?;
                    }
                }
                _ => {}
            }
        }
        let events = match req.poll(POLL_SECS).await {
            Ok(e) => {
                // "Asking again" stays until the other device answers.
                update(&|v| {
                    if v.notice.as_deref() != Some(RESTARTED) {
                        v.notice = None;
                    }
                });
                e
            }
            Err(RequesterError::Mailbox(m)) if m.retryable() => {
                let note = format!("Can't reach the relay right now ({m}). Still trying…");
                update(&|v| v.notice = Some(note.clone()));
                tokio::time::sleep(Duration::from_secs(POLL_SECS)).await;
                continue;
            }
            // The relay forgot the request (it restarted): ask again, once
            // its mailbox is back (a member's device registers it again).
            Err(RequesterError::Mailbox(m)) if am::relay_forgot(&m) && restarts < MAX_REPOSTS => {
                restarts += 1;
                update(&|v| {
                    v.phase = "waiting".into();
                    v.code = None;
                    v.code_spelled = None;
                    v.notice = Some(RESTARTED.into());
                });
                let fresh = repost(host, req, ctx, rx).await?;
                let _ = std::mem::replace(req, fresh).settle().await;
                confirmed = false;
                offered = false;
                continue;
            }
            Err(e) => return Err(requester_failed(&e)),
        };
        for ev in events {
            match ev {
                RequesterEvent::Opened { code, claimed } => {
                    let name = crate::shown(&claimed.name);
                    update(&|v| {
                        v.notice = None;
                        v.phase = "code".into();
                        v.code = Some(code.shown.clone());
                        v.code_spelled = Some(code.spelled.clone());
                        v.claimed_name = Some(name.clone());
                        v.claimed_fingerprint = Some(claimed.fingerprint.clone());
                    });
                }
                RequesterEvent::Offered { .. } => {
                    offered = true;
                    // The other device brings more of the person's workspaces.
                    let more: Vec<LinkWsDto> = req
                        .offered_more()
                        .iter()
                        .map(|m| LinkWsDto { workspace_id: m.workspace_id.to_string(), name: crate::shown(&m.name), state: "waiting".into(), ..LinkWsDto::default() })
                        .collect();
                    if !more.is_empty() {
                        update(&|v| v.workspaces = more.clone());
                    }
                    if confirmed {
                        req.ack_all().await.map_err(|e| requester_failed(&e))?;
                    }
                }
                RequesterEvent::Result(res) if res.outcome == Outcome::Approved => {
                    let target = || GrantTarget::Computer { request: &ctx.request, ka: &ctx.ka, device_key: &ctx.device_key };
                    let checked = req
                        .check_grant(&res, target(), now_ms())
                        .map_err(|e| Failed::new(format!("The approval didn't check out ({e}). Nothing was linked; start again.")))?;
                    let more = req.check_more_grants(&checked, target(), now_ms());
                    let CheckedGrant::Computer { grant, .. } = checked else {
                        return Err(Failed::new("The approval wasn't for a computer. Nothing was linked."));
                    };
                    return link_all(host, slot, gen, rx, *grant, more).await;
                }
                RequesterEvent::Result(res) => return Err(outcome_failed(res.outcome)),
                RequesterEvent::Aborted { reason, .. } => return Err(abort_failed(reason)),
                RequesterEvent::CodeExpired => return Err(Failed::new("The code expired (after 5 minutes). Nothing was granted; try again.")),
                RequesterEvent::RequestExpired => {
                    return Err(Failed::new("No device opened the request in 15 minutes. Open the Inbox on your other device, then try again."))
                }
            }
        }
    }
}

/// Times the request is asked again after relay restarts.
const MAX_REPOSTS: usize = 3;
pub(crate) const RESTARTED: &str = "The relay restarted; asking again.";

/// Post this desktop's request again to the same address, retrying while
/// the relay's mailbox comes back. Cancel stops it.
async fn repost<H: LinkHost>(host: &H, req: &Requester, ctx: &Ctx, rx: &mut mpsc::UnboundedReceiver<Answer>) -> Result<Requester, Failed> {
    let address = req.address().clone();
    let until = Instant::now() + Duration::from_millis(ligus_core::match_code::REQUEST_TTL_MS as u64);
    loop {
        if let Ok(Answer::Cancel) = rx.try_recv() {
            return Err(Failed::new("Cancelled. Nothing was granted."));
        }
        let (device, _) = host.keys().map_err(Failed::final_)?;
        let spec = RequestSpec { kind: MatchKind::Computer, code: ctx.request.encode(), signer: Signer::Ed25519(device), ka: Some(clone_ka(&ctx.ka)) };
        let client = MailboxClient::new(&address.relay).with_auth(host.token(&address.relay));
        match Requester::post(client, &address, spec, Some(&host.data_dir())).await {
            Ok(r) => return Ok(r),
            Err(RequesterError::Mailbox(m)) if (am::relay_forgot(&m) || m.retryable()) && Instant::now() < until => {
                tokio::time::sleep(Duration::from_secs(POLL_SECS)).await;
            }
            Err(e) => return Err(requester_failed(&e)),
        }
    }
}

/// Pin the grant's founder, pull its workspace and find this desktop's
/// certificate on the log (check 5), retrying while it syncs. Writes
/// nothing to the workspace.
async fn verify_on_log<H: LinkHost>(host: &H, grant: &OpenedGrant, budget: Duration, live: &Live<'_>) -> Result<(ActorIdentity, bool), Failed> {
    refuse_other_person(host, grant)?;
    let (founder, fk) = grant.conn.founder_pin().ok_or_else(|| Failed::final_("The approval names no workspace founder. Nothing was linked."))?;
    if ligus_runtime::workspace_invite::invite_relay_problem(&grant.conn.relay_url).is_some() {
        return Err(Failed::final_("The workspace has no relay this computer can use. Nothing was linked."));
    }
    let ws = grant.workspace_id;
    let had_pin = host
        .with_svc(|svc| {
            let had = svc.store().workspace_pin(ws)?.is_some();
            svc.store().set_workspace_pin(ws, &founder, &fk)?;
            Ok::<_, ligus_runtime::EventStoreError>(had)
        })
        .map_err(|e| Failed::new(format!("Couldn't pin the workspace ({e}).")))?;
    let r = check_on_log(host, grant, &founder, budget, live).await;
    if r.is_err() {
        forget_unlinked(host, ws, had_pin, live);
    }
    r.map(|p| (p, had_pin))
}

/// Whether the flow is still the one the person sees (not cancelled or
/// replaced by a new start).
type Live<'a> = dyn Fn() -> bool + Sync + 'a;

fn cancelled() -> Failed {
    Failed::new("Cancelled. Nothing was granted.")
}

/// An install already linked to someone stops before it pulls another
/// person's workspace (`LinkState::record_link` would refuse it anyway).
fn refuse_other_person<H: LinkHost>(host: &H, grant: &OpenedGrant) -> Result<(), Failed> {
    let link = LinkState::load(&host.data_dir()).map_err(|e| Failed::final_(format!("This computer's link record is unreadable ({e}).")))?;
    match link.person {
        Some(p) if p.account_id != grant.person.account_id => Err(Failed::final_(format!(
            "This computer is already one of {}'s devices, so it can't be someone else's too.",
            crate::shown(&p.display_name)
        ))),
        _ => Ok(()),
    }
}

/// A link that didn't happen leaves no copy of a workspace this computer
/// didn't have before. A flow that was cancelled or replaced leaves it: a
/// newer flow may be linking the same workspace with that pin.
fn forget_unlinked<H: LinkHost>(host: &H, ws: Uuid, had_pin: bool, live: &Live<'_>) {
    if !had_pin && live() {
        if let Err(e) = host.with_svc(|svc| svc.store().forget_workspace(ws)) {
            tracing::warn!(target: "link", "forgetting the workspace of a link that didn't happen failed: {e}");
        }
    }
}

async fn check_on_log<H: LinkHost>(host: &H, grant: &OpenedGrant, founder: &str, budget: Duration, live: &Live<'_>) -> Result<ActorIdentity, Failed> {
    let start = Instant::now();
    let mut last;
    loop {
        if !live() {
            return Err(cancelled());
        }
        last = match host.sync(&grant.conn).await {
            Err(e) => format!("couldn't sync the workspace: {e}"),
            Ok(()) => {
                // The grant's founder only for this check: the workspace that
                // is open meanwhile keeps its own.
                let r = host.with_svc(|svc| {
                    let before = svc.trusted_founder().map(str::to_string);
                    svc.set_trusted_founder(Some(founder.to_string()));
                    let r = svc.check_computer_link(grant, now_ms());
                    svc.set_trusted_founder(before);
                    r
                });
                match r {
                    Ok(p) => return Ok(p),
                    Err(e) => {
                        let msg = e.to_string();
                        let waiting = msg.contains(NOT_ON_RELAY_YET) || msg.contains("hasn't synced");
                        if !waiting {
                            return Err(Failed::new(format!("The workspace doesn't agree with the approval: {msg}. Nothing was linked.")));
                        }
                        msg
                    }
                }
            }
        };
        if start.elapsed() >= budget {
            return Err(Failed::new(format!(
                "The approval didn't reach this computer ({last}). Make sure your other device is online and synced, then try again."
            )));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// The name this computer asked with (its pending request's label), read
/// before the link is recorded (recording uses the request up).
fn pending_label<H: LinkHost>(host: &H, grant: &OpenedGrant) -> String {
    LinkState::load(&host.data_dir()).ok().and_then(|l| l.pending_for(&grant.request, now_ms()).map(|p| p.label.trim().to_string())).unwrap_or_default()
}

/// The writes of a link, once the person confirmed: record it, act as the
/// person, accept the certificate, name this desktop (`label`), keep the
/// workspace: made the active one (`activate`), or kept beside it, the open
/// workspace's founder, author and write block put back (a link's more
/// workspaces).
async fn commit<H: LinkHost>(
    host: &H,
    grant: &OpenedGrant,
    person: &ActorIdentity,
    had_pin: bool,
    live: &Live<'_>,
    label: &str,
    activate: bool,
) -> Result<ActorIdentity, Failed> {
    let dir = host.data_dir();
    let ws = grant.workspace_id;
    // Cancelled while it was checking: nothing is saved.
    if !live() {
        return Err(cancelled());
    }
    let recorded = (|| {
        let mut link = LinkState::load(&dir).map_err(|e| Failed::final_(format!("This computer's link record is unreadable ({e}).")))?;
        link.record_link(person, ws).map_err(|e| Failed::final_(e.to_string()))?;
        link.save(&dir).map_err(|e| Failed::final_(format!("Couldn't save the link ({e}).")))?;
        Ok(label.to_string())
    })();
    let label = match recorded {
        Ok(l) => l,
        Err(f) => {
            forget_unlinked(host, ws, had_pin, live);
            return Err(f);
        }
    };
    let ka_pub = host.keys().ok().map(|(_, ka)| ka.public_key_bytes());
    let before = (!activate).then(|| {
        host.with_svc(|svc| (svc.trusted_founder().map(str::to_string), svc.author().clone(), svc.is_delegated(), svc.write_block().map(str::to_string)))
    });
    let (accepted, named) = host.with_svc(|svc| {
        // The workspace opens right after (`adopt_workspace`), with this pin.
        svc.set_trusted_founder(grant.conn.founder_pin().map(|(f, _)| f));
        let p = person.account_id.and_then(|a| svc.person_actor(ws, a, Some(person))).unwrap_or_else(|| person.clone());
        svc.act_as_owner_device(p);
        svc.set_write_block(None);
        // Rules 3: this desktop's own copy of the certificate, without which
        // it counts for nothing there.
        let accepted = svc.accept_device_certificate(ws).map_err(|e| e.to_string());
        let named = if label.is_empty() {
            Ok(false)
        } else {
            let device = svc.device_id();
            svc.set_device_label(ws, device, &label).map_err(|e| e.to_string())
        };
        if let Some(k) = ka_pub {
            if let Err(e) = svc.publish_key_agreement(ws, k) {
                tracing::debug!(target: "link", "key-agreement key not published yet: {e}");
            }
        }
        (accepted, named)
    });
    // A more workspace: the open one is as it was.
    if let Some((founder, author, delegated, block)) = before {
        host.with_svc(|svc| {
            svc.set_trusted_founder(founder);
            if delegated {
                svc.act_as_owner_device(author);
            }
            svc.set_write_block(block);
        });
    }
    // A failed acceptance is retried when the workspace opens
    // ([`bind_workspace`]); it is never silently dropped.
    if let Err(e) = &accepted {
        tracing::warn!(target: "link", "accepting this computer's certificate failed (retried when the workspace opens): {e}");
    }
    if let Err(e) = named {
        tracing::warn!(target: "link", "naming this computer failed (it keeps working): {e}");
    }
    let mut conn = grant.conn.clone();
    conn.inv = None;
    conn.sig = None;
    if activate {
        host.adopt_workspace(&conn).map_err(|e| Failed::new(format!("Linked, but opening the workspace failed: {e}")))?;
    } else {
        host.keep_workspace(&conn).map_err(|e| Failed::new(format!("Linked, but keeping the workspace failed: {e}")))?;
    }
    let _ = host.sync(&conn).await;
    Ok(host.with_svc(|svc| svc.author().clone()))
}

/// What the screen says about a more workspace whose grant didn't come or
/// didn't check out (nothing was written there).
fn not_included(e: &am::GrantError) -> String {
    match e {
        am::GrantError::NotSent => {
            "Your other device didn't add this computer here (its passkey was cancelled, or it couldn't). Link it later: ＋ → Join → Link this computer.".into()
        }
        other => format!("The approval for this workspace didn't check out ({other}). Link it later: ＋ → Join → Link this computer."),
    }
}

/// A link's grants, the first one first: check 5 on each workspace's own
/// log, then "Link this computer as Ana?" once, then each workspace's
/// writes. One that fails leaves the others linked; it is shown, with Try
/// again when that can help ([`retry`]). With no more workspaces this is
/// exactly the one-workspace link.
async fn link_all<H: LinkHost>(
    host: &H,
    slot: &Slot,
    gen: u64,
    rx: &mut mpsc::UnboundedReceiver<Answer>,
    first: OpenedGrant,
    more: Vec<am::MoreGrantCheck>,
) -> Result<(), Failed> {
    let update = |f: &dyn Fn(&mut LinkFlowDto)| {
        if let Some(v) = slot.set(gen, |v| f(v)) {
            host.changed(&v);
        }
    };
    let live = || slot.current(gen);
    let several = !more.is_empty();
    let row = |g: &OpenedGrant| LinkWsDto { workspace_id: g.workspace_id.to_string(), name: shown_ws(&g.conn), state: "checking".into(), ..LinkWsDto::default() };
    let mut rows = vec![row(&first)];
    let label = pending_label(host, &first);
    let mut grants = vec![first];
    for m in more {
        match m.outcome {
            Ok(CheckedGrant::Computer { grant, .. }) => {
                rows.push(row(&grant));
                grants.push(*grant);
            }
            Ok(_) => {}
            Err(e) => rows.push(LinkWsDto {
                workspace_id: m.workspace_id.to_string(),
                name: crate::shown(&m.name),
                state: "notIncluded".into(),
                error: Some(not_included(&e)),
                can_retry: false,
            }),
        }
    }
    let first_name = shown_ws(&grants[0].conn);
    update(&|v| {
        v.phase = "checking".into();
        v.workspace_name = Some(first_name.clone());
        if several {
            v.workspaces = rows.clone();
        }
    });

    // Check 5, on each workspace's own log.
    let mut verified: Vec<(OpenedGrant, ActorIdentity, bool)> = Vec::new();
    let mut later: Vec<OpenedGrant> = Vec::new();
    let mut first_failure = None;
    let forget_all = |verified: &[(OpenedGrant, ActorIdentity, bool)]| {
        for (g, _, had) in verified {
            forget_unlinked(host, g.workspace_id, *had, &live);
        }
    };
    for g in grants {
        match verify_on_log(host, &g, LOG_BUDGET, &live).await {
            Ok((p, had)) => {
                update(&|v| ws_row(v, g.workspace_id, |r| r.state = "ready".into()));
                verified.push((g, p, had));
            }
            Err(f) => {
                if !live() {
                    forget_all(&verified);
                    return Err(cancelled());
                }
                update(&|v| {
                    ws_row(v, g.workspace_id, |r| {
                        r.state = "failed".into();
                        r.error = Some(f.text.clone());
                        r.can_retry = f.retry;
                    })
                });
                if f.retry {
                    later.push(g);
                }
                first_failure.get_or_insert(f);
            }
        }
    }
    let Some((_, person, _)) = verified.first() else {
        return Err(first_failure.unwrap_or_else(|| Failed::new("Nothing was linked. Start again.")));
    };

    // Check 6: the person says who they link as. Only an answer given after
    // the name is on screen counts.
    while let Ok(a) = rx.try_recv() {
        if a == Answer::Cancel {
            forget_all(&verified);
            return Err(cancelled());
        }
    }
    let name = crate::shown(&person.display_name);
    update(&|v| {
        v.phase = "confirmPerson".into();
        v.person_name = Some(name.clone());
    });
    loop {
        match rx.recv().await {
            Some(Answer::LinkAs(true)) => break,
            Some(Answer::LinkAs(false)) | Some(Answer::Cancel) | None => {
                forget_all(&verified);
                return Err(Failed::new("Not linked. Nothing was saved on this computer."));
            }
            Some(_) => {}
        }
    }
    update(&|v| v.phase = "linking".into());

    // The first workspace that checked out opens; the others are kept beside it.
    let mut linked: Option<(ActorIdentity, Uuid)> = None;
    for (i, (g, p, had)) in verified.iter().enumerate() {
        update(&|v| ws_row(v, g.workspace_id, |r| r.state = "linking".into()));
        match commit(host, g, p, *had, &live, &label, i == 0).await {
            Ok(person) => {
                update(&|v| ws_row(v, g.workspace_id, |r| r.state = "linked".into()));
                linked.get_or_insert((person, g.workspace_id));
            }
            Err(f) if i == 0 => {
                forget_all(&verified[1..]);
                return Err(f);
            }
            Err(f) => {
                update(&|v| {
                    ws_row(v, g.workspace_id, |r| {
                        r.state = "failed".into();
                        r.error = Some(f.text.clone());
                        r.can_retry = f.retry;
                    })
                });
                if f.retry {
                    later.push(g.clone());
                }
            }
        }
    }
    let Some((person, ws)) = linked else { return Err(Failed::new("Nothing was linked. Start again.")) };
    {
        let mut s = slot.0.lock().unwrap();
        if s.gen == gen {
            s.retry = later.into_iter().map(|g| (g, label.clone())).collect();
        }
    }
    let pn = crate::shown(&person.display_name);
    update(&|v| {
        v.phase = "linked".into();
        v.person_name = Some(pn.clone());
        v.workspace_id = Some(ws.to_string());
    });
    Ok(())
}

/// Try one workspace of a link again (`failed` with Try again): check 5 on
/// its log, then its writes, beside the open workspace.
pub(crate) async fn retry<H: LinkHost>(host: Arc<H>, slot: Arc<Slot>, workspace_id: Uuid) -> LinkFlowDto {
    let (grant, label, gen) = {
        let mut s = slot.0.lock().unwrap();
        let Some(i) = s.retry.iter().position(|(g, _)| g.workspace_id == workspace_id) else { return s.view.clone() };
        let (g, l) = s.retry.remove(i);
        ws_row(&mut s.view, workspace_id, |r| {
            r.state = "checking".into();
            r.error = None;
            r.can_retry = false;
        });
        (g, l, s.gen)
    };
    host.changed(&slot.view());
    let live = || slot.current(gen);
    let r = async {
        may_link(&*host)?;
        let (person, had_pin) = verify_on_log(&*host, &grant, LOG_BUDGET, &live).await?;
        commit(&*host, &grant, &person, had_pin, &live, &label, false).await
    }
    .await;
    let v = match r {
        Ok(_) => slot.set(gen, |v| ws_row(v, workspace_id, |r| r.state = "linked".into())),
        Err(f) => {
            let again = f.retry && live();
            {
                let mut s = slot.0.lock().unwrap();
                if again && s.gen == gen {
                    s.retry.push((grant, label));
                }
            }
            slot.set(gen, |v| {
                ws_row(v, workspace_id, |r| {
                    r.state = "failed".into();
                    r.error = Some(f.text.clone());
                    r.can_retry = again;
                })
            })
        }
    }
    .unwrap_or_else(|| slot.view());
    host.changed(&v);
    v
}

// ── The paste path ───────────────────────────────────────────────────────

/// Make a request for the paste path: `paste`, with the request to copy.
pub(crate) fn paste<H: LinkHost>(host: &H, slot: &Slot) -> LinkFlowDto {
    let (gen, _rx) = slot.begin(view("paste"));
    let v = match make_request(host) {
        Ok((r, _, _)) => slot.set(gen, |v| {
            v.request = Some(r.encode());
            v.fingerprint = Some(r.fingerprint());
            v.label = Some(r.label.clone());
        }),
        Err(f) => slot.set(gen, |v| set_failed(v, &f)),
    }
    .unwrap_or_else(|| slot.view());
    host.changed(&v);
    v
}

/// Open a pasted grant (`ligusgrant1:…`) and show who approved it, for the
/// person to confirm (`confirmApprover`). Nothing is saved.
pub(crate) fn open_grant<H: LinkHost>(host: &H, slot: &Slot, text: &str) -> Result<LinkFlowDto, String> {
    let code = text.split_whitespace().find(|w| w.trim_matches(|c| c == '"' || c == '\'').starts_with(ligus_runtime::computer_link::GRANT_PREFIX));
    let Some(code) = code.map(|w| w.trim_matches(|c| c == '"' || c == '\'').to_string()) else {
        return Err(if text.trim().starts_with(ligus_runtime::computer_link::REQUEST_PREFIX) {
            "That's this computer's request. Paste it on your other device, then paste the code it gives back here.".into()
        } else {
            "That isn't the code from your other device. After you confirm there, it shows a code to copy (Copy for lt).".into()
        });
    };
    let (_, ka) = host.keys()?;
    let link = LinkState::load(&host.data_dir()).map_err(|e| e.to_string())?;
    let grant = Grant::open(&code, &ka).map_err(|e| match e {
        ligus_runtime::computer_link::LinkError::NotForThisDevice => "That code was made for another computer's request.".to_string(),
        other => format!("{other}."),
    })?;
    let pending = link.pending_for(&grant.request, now_ms()).ok_or("That code answers a request this computer no longer has (it expired or was used). Make a new request.")?;
    let device_id = pending.device_id;
    let (device, _) = host.keys()?;
    let person = grant.check(&device.public_key_bytes(), now_ms()).map_err(|e| format!("{e}."))?;
    refuse_other_person(host, &grant).map_err(|f| f.text)?;
    let (person_name, ws_name, fp) = (crate::shown(&person.display_name), shown_ws(&grant.conn), grant.approver_fingerprint());
    let mut s = slot.0.lock().unwrap();
    s.pasted = Some((grant, device_id));
    s.view.phase = "confirmApprover".into();
    s.view.person_name = Some(person_name);
    s.view.workspace_name = Some(ws_name);
    s.view.approver_fingerprint = Some(fp);
    s.view.error = None;
    let v = s.view.clone();
    drop(s);
    host.changed(&v);
    Ok(v)
}

/// The person confirmed the approving device of a pasted grant: link.
pub(crate) async fn redeem<H: LinkHost>(host: Arc<H>, slot: Arc<Slot>) -> LinkFlowDto {
    let (grant, gen) = {
        let mut s = slot.0.lock().unwrap();
        let Some((grant, _)) = s.pasted.take() else { return s.view.clone() };
        s.view.phase = "linking".into();
        (grant, s.gen)
    };
    host.changed(&slot.view());
    let live = || slot.current(gen);
    let r = async {
        may_link(&*host)?;
        let (person, had_pin) = verify_on_log(&*host, &grant, LOG_BUDGET, &live).await?;
        commit(&*host, &grant, &person, had_pin, &live, &pending_label(&*host, &grant), true).await
    }
    .await;
    let v = match r {
        Ok(p) => slot.set(gen, |v| {
            v.phase = "linked".into();
            v.person_name = Some(crate::shown(&p.display_name));
            v.workspace_id = Some(grant.workspace_id.to_string());
        }),
        Err(f) => slot.set(gen, |v| set_failed(v, &f)),
    }
    .unwrap_or_else(|| slot.view());
    host.changed(&v);
    v
}

// ── A linked desktop at launch and per workspace ─────────────────────────

/// At launch: a desktop linked as a person's device acts as them. An
/// unreadable link record stops the launch (acting as no one instead would
/// write as the wrong identity).
pub(crate) fn adopt_at_launch(svc: &mut ChatService, data_dir: &Path) -> Result<bool, String> {
    let link = LinkState::load(data_dir).map_err(|e| format!("{} is unreadable ({e}): fix or remove it", LinkState::path(data_dir).display()))?;
    match link.person {
        Some(p) if link.is_linked() => {
            svc.act_as_owner_device(p);
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// When a team workspace opens on a linked desktop: act as the person there
/// (their current profile from the log), accept the certificate if an
/// earlier link couldn't, and refuse writes where this desktop doesn't
/// count. A desktop that isn't linked is left as it is.
pub(crate) fn bind_workspace(svc: &mut ChatService, data_dir: &Path, ws: Uuid, local_account: Option<Uuid>) {
    use ligus_runtime::device_binding::Binding;
    let link = match LinkState::load(data_dir) {
        Ok(l) => l,
        Err(e) => {
            svc.set_write_block(Some(format!("this computer's link record is unreadable ({e})")));
            return;
        }
    };
    if !link.is_linked() {
        return;
    }
    let local = local_account.unwrap_or_default();
    let binding = svc.binding(ws, &link, local).unwrap_or(Binding::Pending { account: link.account_in(ws).unwrap_or_default() });
    let act_as = |svc: &mut ChatService, account: Uuid| {
        if let Some(p) = svc.person_actor(ws, account, link.person.as_ref()) {
            svc.act_as_owner_device(p);
        }
    };
    match binding {
        Binding::Linked { account } => {
            act_as(svc, account);
            svc.set_write_block(None);
        }
        Binding::Own | Binding::NotLinkedHere => svc.set_write_block(Some(
            "this computer is one of your devices, but isn't linked into this workspace: link it here (＋ → Join → Link this computer)".into(),
        )),
        other => {
            if let Some(account) = link.account_in(ws) {
                act_as(svc, account);
            }
            let healed = matches!(other, Binding::Pending { .. })
                && svc.accept_device_certificate(ws).unwrap_or(false)
                && svc.binding(ws, &link, local).is_ok_and(|b| b.writable());
            svc.set_write_block(if healed { None } else { other.why_read_only() });
        }
    }
}

/// Whether this install is linked as someone's device (the person's name).
pub(crate) fn linked_person(data_dir: &Path) -> Option<String> {
    LinkState::load(data_dir).ok().filter(|l| l.is_linked()).and_then(|l| l.person).map(|p| crate::shown(&p.display_name))
}

/// What a linked desktop says when asked to found or join a workspace.
pub(crate) fn refuse_if_linked(data_dir: &Path, what: &str) -> Result<(), String> {
    match linked_person(data_dir) {
        Some(name) => Err(format!(
            "This computer is one of {name}'s devices, so it doesn't {what}. Do it on your first device, then link this computer to that workspace (＋ → Join → Link this computer)."
        )),
        None => Ok(()),
    }
}

/// One push and pull of `engine`'s workspace into the store at `db`. The
/// store isn't shared across threads, so the round runs on a blocking
/// thread of its own.
pub(crate) async fn sync_with(db: PathBuf, mut engine: ligus_runtime::SyncEngine, refresh_keyring: bool) -> Result<(), String> {
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        handle.block_on(async move {
            let mut store = ligus_runtime::EventStore::open(&db).map_err(|e| e.to_string())?;
            if refresh_keyring {
                let _ = engine.refresh_keyring(&store);
            }
            engine.push_new(&store).await.map_err(|e| e.to_string())?;
            engine.pull(&mut store).await.map_err(|e| e.to_string())?;
            Ok(())
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// One push and pull of `conn`'s workspace, open or not (the store, its
/// keys and the relay's access token from the app).
pub(crate) fn sync_conn(st: &crate::AppState, conn: &WorkspaceConn) -> impl Future<Output = Result<(), String>> + Send {
    let src = crate::key_source_for(st, Some(conn));
    let static_token = st.settings.lock().unwrap().relay_access_token.clone();
    let token = crate::ligus_auth_ui::effective(Some(&conn.relay_url), static_token);
    let db = st.db_path.clone();
    let engine = ligus_runtime::SyncEngine::new(ligus_runtime::RelayClient::new(&conn.relay_url).with_auth(token), conn.room.clone()).with_key_source(src);
    sync_with(db, engine, true)
}

// ── The app's host and commands ──────────────────────────────────────────

static FLOW: LazyLock<Arc<Slot>> = LazyLock::new(|| Arc::new(Slot::default()));

struct AppHost(tauri::AppHandle);

impl AppHost {
    fn state(&self) -> tauri::State<'_, crate::AppState> {
        tauri::Manager::state::<crate::AppState>(&self.0)
    }
}

impl LinkHost for AppHost {
    fn data_dir(&self) -> PathBuf {
        self.state().data_dir.clone()
    }

    fn with_svc<R>(&self, f: impl FnOnce(&mut ChatService) -> R) -> R {
        let st = self.state();
        let mut svc = st.service.lock().unwrap();
        f(&mut svc)
    }

    fn keys(&self) -> Result<(ligus_core::crypto::SigningKeypair, KeyAgreementKeypair), String> {
        let st = self.state();
        let device_id = st.service.lock().unwrap().device_id();
        let device = st.identity.device_keypair(device_id).map_err(|e| e.to_string())?;
        let ka = st
            .settings
            .lock()
            .unwrap()
            .ka_secret
            .as_deref()
            .and_then(crate::parse_hex32)
            .and_then(|seed| KeyAgreementKeypair::from_seed(&seed).ok())
            .ok_or("this computer has no key-agreement key yet: restart Ligus and try again")?;
        Ok((device, ka))
    }

    fn label(&self) -> String {
        crate::computer_name().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Desktop".into())
    }

    fn token(&self, relay: &str) -> Option<String> {
        let static_token = self.state().settings.lock().unwrap().relay_access_token.clone();
        crate::ligus_auth_ui::effective(Some(relay), static_token)
    }

    fn own_workspaces(&self, link: &LinkState) -> usize {
        let st = self.state();
        let s = st.settings.lock().unwrap();
        s.workspaces.iter().filter(|w| w.dm_account.is_none() && !link.workspaces.contains(&w.id())).count()
    }

    fn sync(&self, conn: &WorkspaceConn) -> impl Future<Output = Result<(), String>> + Send {
        sync_conn(&self.state(), conn)
    }

    fn adopt_workspace(&self, conn: &WorkspaceConn) -> Result<(), String> {
        let st = self.state();
        {
            let mut s = st.settings.lock().unwrap();
            if !s.workspaces.iter().any(|w| w.room == conn.room) {
                s.workspaces.push(conn.clone());
            }
            // The first run's answer is settled: this install is linked.
            s.first_run_path = Some(crate::first_run_ui::PATH_LINK.to_string());
            crate::save_settings(&st.data_dir, &s);
        }
        crate::set_active_workspace_sync(st, conn.id().to_string())
    }

    fn keep_workspace(&self, conn: &WorkspaceConn) -> Result<(), String> {
        let st = self.state();
        let mut s = st.settings.lock().unwrap();
        if !s.workspaces.iter().any(|w| w.room == conn.room) {
            s.workspaces.push(conn.clone());
        }
        crate::save_settings(&st.data_dir, &s);
        Ok(())
    }

    fn changed(&self, view: &LinkFlowDto) {
        let _ = tauri::Emitter::emit(&self.0, CHANGED_EVENT, view.clone());
    }
}

/// Where the link flow stands.
#[tauri::command]
pub(crate) fn link_request_status() -> LinkFlowDto {
    FLOW.view()
}

/// Post this desktop's link request to the approval address the other
/// device shows, and wait for it there.
#[tauri::command]
pub(crate) async fn link_request_start(app: tauri::AppHandle, address: String, relay: Option<String>) -> Result<LinkFlowDto, String> {
    Ok(start(Arc::new(AppHost(app)), FLOW.clone(), &address, relay.as_deref()).await)
}

/// The person's answer to "Does your other device show this code?".
#[tauri::command]
pub(crate) fn link_request_codes_match(matches: bool) -> Result<LinkFlowDto, String> {
    FLOW.answer(Answer::CodesMatch(matches))?;
    Ok(FLOW.view())
}

/// The person's answer to "Link this computer as …?".
#[tauri::command]
pub(crate) fn link_request_link_as(yes: bool) -> Result<LinkFlowDto, String> {
    FLOW.answer(Answer::LinkAs(yes))?;
    Ok(FLOW.view())
}

/// Stop: the request is ended on the relay and nothing is granted.
#[tauri::command]
pub(crate) fn link_request_cancel(app: tauri::AppHandle) -> LinkFlowDto {
    let v = FLOW.reset();
    AppHost(app).changed(&v);
    v
}

/// The paste path: a request to copy to the other device.
#[tauri::command]
pub(crate) async fn link_request_paste(app: tauri::AppHandle) -> Result<LinkFlowDto, String> {
    crate::off_main(app, move |app| Ok(paste(&AppHost(app.clone()), &FLOW))).await
}

/// The paste path: the code the other device gave back.
#[tauri::command]
pub(crate) async fn link_request_open_grant(app: tauri::AppHandle, grant: String) -> Result<LinkFlowDto, String> {
    crate::off_main(app, move |app| open_grant(&AppHost(app.clone()), &FLOW, &grant)).await
}

/// The paste path: "That's my device": link.
#[tauri::command]
pub(crate) async fn link_request_redeem(app: tauri::AppHandle) -> Result<LinkFlowDto, String> {
    Ok(redeem(Arc::new(AppHost(app)), FLOW.clone()).await)
}

/// Try one workspace of a link that brought several again.
#[tauri::command]
pub(crate) async fn link_request_retry(app: tauri::AppHandle, workspace_id: String) -> Result<LinkFlowDto, String> {
    let ws = Uuid::parse_str(&workspace_id).map_err(|e| e.to_string())?;
    Ok(retry(Arc::new(AppHost(app)), FLOW.clone(), ws).await)
}

#[cfg(test)]
mod tests;

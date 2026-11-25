//! Code-match approval on the desktop (`docs/design/code-match-approval.md`
//! 7.2; bytes in `docs/design/code-match-wire.md`): the approver's side.
//!
//! A background loop ([`run_poll_loop`]) keeps, for every team workspace,
//! its approval mailbox registered (the current key epoch's, and the
//! previous two epochs' when the relay forgot them) and lists the live
//! requests every few seconds, backing off on errors and going quiet on a
//! relay without mailbox routes. The Inbox lists them ([`list_approvals`]);
//! opening one claims it and runs the commit-then-reveal exchange
//! ([`approval_open`]), and once the code shows the person confirms the
//! match ([`approval_codes_match`]):
//!
//! * a box: the existing box admission over the keys of **this request's**
//!   `ligusjoin1` text (never a second copy), confirmed with the passkey,
//!   then the sealed `ligusadmit1:` in the result;
//! * a computer (identity model v2, 8.1): the grant is built and sealed now
//!   and only its digest is offered; after the computer's ack (2 minutes at
//!   most) the passkey and the certificate, then the result with the grant.
//!   No ack: nothing is signed and the Inbox says so for a day;
//! * a phone: the existing phone certification of the request's phone
//!   code, then the result.
//!
//! The passkey sheet's summary names the label and the code (the runtime's
//! [`MatchContext`]). Nothing about the code is logged or written to the
//! workspace log (decision D4); the only things kept on disk are the
//! seen-set (`approval-seen.json`) and the Inbox notices of the last day
//! (`approval-notices.json`), both device-local.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use ligus_core::match_code::MatchKind;
use ligus_core::WorkspaceRole;
use ligus_runtime::approval_mailbox::{
    claim_view, current_address, end_request, list_live, AbortReason, ApprovalAddress, ApproverError, ApproverIdentity, ApproverPerson, ApproverSession,
    ClaimView, EpochKey, LiveRequest, MailboxClient, MailboxError, Notice, NoticeKind, Notices, RequestSubject, SeenSet, Signer, VerifiedRequest,
};
use ligus_runtime::chat_service::{MatchContext, WorkspaceApprovers};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;

use crate::AppState;

/// The listing changed (no payload): the Inbox refetches [`list_approvals`].
pub(crate) const CHANGED_EVENT: &str = "approvals://changed";
/// An open approval moved on. Payload: [`SessionDto`].
pub(crate) const SESSION_EVENT: &str = "approvals://session";
/// How often the mailboxes are listed while all is well.
const POLL: Duration = Duration::from_secs(5);
/// The longest backoff after errors.
const MAX_BACKOFF: Duration = Duration::from_secs(120);
/// How long a relay without mailbox routes is left alone.
const NO_ROUTES_RECHECK: Duration = Duration::from_secs(600);
/// The long-poll each wait step uses (the session is locked meanwhile).
const STEP_WAIT_SECS: u64 = 5;

fn now_ms() -> i64 {
    ligus_core::flow::now_ms()
}

// ── Pure helpers (tested) ────────────────────────────────────────────────

/// The live request whose code the pasted `text` is, byte for byte after
/// trimming (wire note 7). Only join and link requests: a phone's code has
/// its own flow.
pub(crate) fn paste_match<'a>(text: &str, live: impl IntoIterator<Item = (&'a str, &'a str)>) -> Option<&'a str> {
    let t = text.trim();
    if !(t.starts_with(ligus_runtime::box_admission::JOIN_PREFIX) || t.starts_with(ligus_runtime::computer_link::REQUEST_PREFIX)) {
        return None;
    }
    live.into_iter().find(|(_, code)| code.trim() == t).map(|(rid, _)| rid)
}

fn kind_str(k: MatchKind) -> &'static str {
    match k {
        MatchKind::Box => "box",
        MatchKind::Computer => "computer",
        MatchKind::Phone => "phone",
    }
}

/// Same group for "2 devices are asking": boxes count with boxes, a
/// person's computers and phones together.
fn same_group(a: MatchKind, b: MatchKind) -> bool {
    (a == MatchKind::Box) == (b == MatchKind::Box)
}

// ── Inbox notices (device-local, a day: the runtime's `Notices`) ──────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FactDto {
    /// The request's rid (what dismisses it).
    pub key: String,
    /// `mismatch` or `didntFinish`.
    pub kind: String,
    pub workspace_id: String,
    pub label: String,
    pub at: i64,
    /// The Inbox line and its "?" ([`Notice::text`], [`Notice::help`]).
    pub text: String,
    pub help: String,
}

fn fact_dto(n: &Notice) -> FactDto {
    FactDto {
        key: n.rid.clone(),
        kind: match n.kind {
            NoticeKind::Mismatch => "mismatch",
            NoticeKind::DidntFinish => "didntFinish",
        }
        .into(),
        workspace_id: n.workspace_id.to_string(),
        label: n.label.clone(),
        at: n.at,
        text: n.text(),
        help: n.help().into(),
    }
}

fn add_fact(app: &AppHandle, kind: NoticeKind, slot: &Slot) {
    let dir = app.state::<AppState>().data_dir.clone();
    let n = Notice {
        kind,
        workspace_id: slot.ws,
        rid: slot.request.rid_hex(),
        request_kind: slot.kind,
        label: slot.label.clone(),
        at: now_ms(),
        by: None,
    };
    if let Err(e) = Notices::new(&dir).record(n) {
        tracing::warn!(error = %e, "couldn't keep the approval notice");
    }
    let _ = app.emit(CHANGED_EVENT, ());
}

// ── The desk: what the loop found, and the sessions open here ────────────

/// One team workspace, as the loop polls it.
#[derive(Clone)]
struct Target {
    id: Uuid,
    name: String,
    relay: String,
    token: Option<String>,
    founder_pin: String,
    epochs: Vec<EpochKey>,
    approvers: WorkspaceApprovers,
    can_box: bool,
    can_devices: bool,
    is_admin: bool,
}

impl Target {
    fn client(&self) -> MailboxClient {
        MailboxClient::new(&self.relay).with_auth(self.token.clone())
    }
    fn current(&self) -> Option<&EpochKey> {
        self.epochs.iter().max_by_key(|e| e.epoch)
    }
}

#[derive(Clone)]
struct Found {
    target: Target,
    live: Vec<LiveRequest>,
}

#[derive(Default)]
struct DeskState {
    found: HashMap<Uuid, Found>,
    no_routes: HashSet<Uuid>,
    /// The listing last sent, to emit only on a change.
    last_sig: String,
}

struct Slot {
    ws: Uuid,
    kind: MatchKind,
    label: String,
    request: VerifiedRequest,
    session: tokio::sync::Mutex<ApproverSession>,
    view: Mutex<SessionDto>,
}

#[derive(Default)]
struct Desk {
    state: Mutex<DeskState>,
    sessions: Mutex<HashMap<String, Arc<Slot>>>,
}

static DESK: LazyLock<Desk> = LazyLock::new(Desk::default);

fn slot(rid: &str) -> Result<Arc<Slot>, String> {
    DESK.sessions.lock().unwrap().get(rid).cloned().ok_or_else(|| "This request isn't open on this device.".to_string())
}

fn found_request(ws: Uuid, rid: &str) -> Option<(Target, LiveRequest)> {
    let st = DESK.state.lock().unwrap();
    let f = st.found.get(&ws)?;
    f.live.iter().find(|l| l.rid_hex() == rid).map(|l| (f.target.clone(), l.clone()))
}

// ── DTOs ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub(crate) enum ClaimDto {
    Unclaimed,
    Mine,
    ClaimedBy { name: String },
    Contested { name: Option<String> },
}

impl From<ClaimView> for ClaimDto {
    fn from(v: ClaimView) -> Self {
        match v {
            ClaimView::Unclaimed => Self::Unclaimed,
            ClaimView::Mine => Self::Mine,
            ClaimView::ClaimedBy { name, .. } => Self::ClaimedBy { name },
            ClaimView::Contested { by } => Self::Contested { name: by.map(|(n, _)| n) },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApprovalDto {
    pub workspace_id: String,
    pub workspace_name: String,
    pub rid: String,
    /// `box`, `computer` or `phone`.
    pub kind: String,
    pub label: String,
    /// The 80-bit fingerprint (Details).
    pub fingerprint: String,
    pub created_at: i64,
    pub expires_at: i64,
    /// The approval address's relay (Details).
    pub relay: String,
    pub claim: ClaimDto,
    /// How many live requests of the same group this workspace has.
    pub parallel: usize,
    /// The phase of this device's session for it, if one is open.
    pub phase: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApprovalsDto {
    pub requests: Vec<ApprovalDto>,
    pub facts: Vec<FactDto>,
}

/// An approval open on this device. `phase`: `waiting` (for the other
/// device's answer), `code`, `confirming` (a computer's ack), `passkey`,
/// `approved`, `mismatch`, `declined`, `expired`, `didntFinish`, `ended`,
/// `failed`.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionDto {
    pub rid: String,
    pub workspace_id: String,
    pub kind: String,
    pub label: String,
    pub fingerprint: String,
    pub relay: String,
    pub created_at: i64,
    pub phase: String,
    /// `K7Q4 M9X2`, once both ends know it. Not a secret.
    pub code: Option<String>,
    /// `K, 7, Q, 4. M, 9, X, 2.`
    pub spelled: Option<String>,
    pub expires_at: Option<i64>,
    /// When the code appeared (the 3-second hold starts here).
    pub code_at: Option<i64>,
    pub message: Option<String>,
}

fn set_view(app: &AppHandle, slot: &Slot, f: impl FnOnce(&mut SessionDto)) -> SessionDto {
    let v = {
        let mut v = slot.view.lock().unwrap();
        f(&mut v);
        v.clone()
    };
    let _ = app.emit(SESSION_EVENT, v.clone());
    let _ = app.emit(CHANGED_EVENT, ());
    v
}

fn phase(slot: &Slot) -> String {
    slot.view.lock().unwrap().phase.clone()
}

/// Where an error leaves the session, in words for the sheet.
fn ended_by(e: &ApproverError) -> (&'static str, String) {
    match e {
        ApproverError::Aborted(AbortReason::Mismatch) | ApproverError::RevealMismatch => ("mismatch", "The codes didn't match. Nothing was granted.".into()),
        ApproverError::Aborted(AbortReason::Cancelled) => ("ended", "The other device cancelled the request. Nothing was granted.".into()),
        ApproverError::Aborted(AbortReason::Contested) => ("ended", "The request was ended as possibly intercepted. Nothing was granted.".into()),
        ApproverError::CodeExpired | ApproverError::RequestExpired => ("expired", "The code expired. Nothing was granted. Ask for a new request.".into()),
        ApproverError::DidntFinish => ("didntFinish", "The computer didn't confirm in time. Nothing was added.".into()),
        ApproverError::Mailbox(MailboxError::RequestUnknown) | ApproverError::Ended => ("ended", "The request is over. Nothing was granted.".into()),
        other => ("failed", other.to_string()),
    }
}

// ── The loop ─────────────────────────────────────────────────────────────

/// Every team workspace this device can approve in, with its epoch keys.
fn targets(state: &AppState) -> Vec<Target> {
    let (conns, static_token) = {
        let s = state.settings.lock().unwrap();
        (s.workspaces.clone(), s.relay_access_token.clone())
    };
    let mut out = Vec::new();
    for conn in conns {
        let ws = conn.id();
        if conn.dm_account.is_some() || ws == state.local_workspace_id || !ligus_runtime::workspace_invite::usable_relay_url(&conn.relay_url) {
            continue;
        }
        let src = crate::key_source_for(state, Some(&conn));
        let svc = state.service.lock().unwrap();
        let epochs = svc.approval_epoch_keys(ws, &src).unwrap_or_default();
        let Some(founder_pin) = svc.founder_pin_hex(ws).ok().flatten() else { continue };
        if epochs.is_empty() {
            continue;
        }
        let role = svc.my_workspace_role(ws);
        let is_admin = role.is_some_and(|r| r.rank() >= WorkspaceRole::Admin.rank());
        let human = svc.author().kind == ligus_core::ActorKind::Human && svc.author().account_id.is_some();
        let approvers = svc.approval_classifier(ws).unwrap_or_default();
        out.push(Target {
            id: ws,
            name: crate::shown(&conn.display_name()),
            relay: conn.relay_url.trim().trim_end_matches('/').to_string(),
            token: crate::ligus_auth_ui::effective(Some(&conn.relay_url), static_token.clone()),
            founder_pin,
            epochs,
            approvers,
            can_box: is_admin,
            can_devices: human && !svc.is_delegated(),
            is_admin,
        });
    }
    out
}

async fn poll_one(t: &Target) -> Result<Vec<LiveRequest>, MailboxError> {
    let client = t.client();
    if let Some(cur) = t.current() {
        client.ensure_registered(&cur.key).await?;
    }
    let listing = list_live(&client, &t.epochs).await?;
    // The relay keeps mailboxes in memory: register the older epochs again
    // when it forgot them, so a stale address still reaches us.
    for e in &listing.unregistered {
        if let Some(k) = t.epochs.iter().find(|k| k.epoch == *e) {
            let _ = client.ensure_registered(&k.key).await;
        }
    }
    Ok(listing.live)
}

/// The background loop: list every team workspace's approval mailboxes,
/// emit [`CHANGED_EVENT`] when the listing changes.
pub(crate) async fn run_poll_loop(app: AppHandle) {
    let mut backoff: HashMap<Uuid, (Instant, u32)> = HashMap::new();
    loop {
        let targets = crate::off_main(app.clone(), |app| targets(&app.state::<AppState>())).await;
        let ids: HashSet<Uuid> = targets.iter().map(|t| t.id).collect();
        for t in targets {
            if backoff.get(&t.id).is_some_and(|(next, _)| Instant::now() < *next) {
                continue;
            }
            match poll_one(&t).await {
                Ok(live) => {
                    backoff.remove(&t.id);
                    let mut st = DESK.state.lock().unwrap();
                    st.no_routes.remove(&t.id);
                    st.found.insert(t.id, Found { target: t, live });
                }
                Err(MailboxError::NoRoutes) => {
                    // Quiet: paste stays the way in. Look again later.
                    backoff.insert(t.id, (Instant::now() + NO_ROUTES_RECHECK, 0));
                    let mut st = DESK.state.lock().unwrap();
                    st.no_routes.insert(t.id);
                    st.found.remove(&t.id);
                }
                Err(e) => {
                    let fails = backoff.get(&t.id).map_or(0, |(_, n)| *n) + 1;
                    let wait = POLL.saturating_mul(1 << fails.min(5)).min(MAX_BACKOFF);
                    if fails == 1 {
                        tracing::debug!(target: "approvals", workspace = %t.id, error = %e, "couldn't list the approval mailbox");
                    }
                    backoff.insert(t.id, (Instant::now() + wait, fails));
                }
            }
        }
        let changed = {
            let mut st = DESK.state.lock().unwrap();
            st.found.retain(|id, _| ids.contains(id));
            let sig = listing_sig(&st);
            let changed = sig != st.last_sig;
            st.last_sig = sig;
            changed
        };
        if changed {
            let _ = app.emit(CHANGED_EVENT, ());
        }
        tokio::time::sleep(POLL).await;
    }
}

fn listing_sig(st: &DeskState) -> String {
    let mut parts: Vec<String> = st
        .found
        .values()
        .flat_map(|f| f.live.iter().map(|l| format!("{}:{}:{}:{}", l.rid_hex(), l.claims.len(), l.opened_without_claim, l.ended.is_some())))
        .collect();
    parts.sort();
    parts.join(",")
}

// ── Commands ─────────────────────────────────────────────────────────────

/// The live requests this device can approve (every team workspace; the
/// web shows the active one's), plus the Inbox facts of the last day.
#[tauri::command]
pub(crate) fn list_approvals(state: tauri::State<AppState>) -> ApprovalsDto {
    let me = state.service.lock().unwrap().device_id();
    let sessions = DESK.sessions.lock().unwrap();
    let st = DESK.state.lock().unwrap();
    let mut requests = Vec::new();
    for f in st.found.values() {
        let t = &f.target;
        let open: Vec<&LiveRequest> = f.live.iter().filter(|l| l.ended.is_none()).collect();
        for l in &f.live {
            let rid = l.rid_hex();
            let session = sessions.get(&rid);
            let allowed = if l.kind() == MatchKind::Box { t.can_box } else { t.can_devices };
            if !allowed {
                continue;
            }
            let claim: ClaimDto = claim_view(l, me, &t.approvers).into();
            // Ended, or opened here before a restart (it can't be answered
            // twice): only a session still on screen shows it.
            if session.is_none() && (l.ended.is_some() || claim == ClaimDto::Mine) {
                continue;
            }
            requests.push(ApprovalDto {
                workspace_id: t.id.to_string(),
                workspace_name: t.name.clone(),
                rid,
                kind: kind_str(l.kind()).into(),
                label: l.label(),
                fingerprint: l.fingerprint(),
                created_at: l.created_at(),
                expires_at: l.expires_at,
                relay: l.relay.clone(),
                claim,
                parallel: open.iter().filter(|o| same_group(o.kind(), l.kind())).count(),
                phase: session.map(|s| phase(s)),
            });
        }
    }
    requests.sort_by_key(|r| r.created_at);
    let facts = Notices::new(&state.data_dir).live(now_ms()).unwrap_or_default().iter().map(fact_dto).collect();
    ApprovalsDto { requests, facts }
}

/// Forget an Inbox notice (its row's Dismiss; `key` is its rid).
#[tauri::command]
pub(crate) fn approval_dismiss_fact(state: tauri::State<AppState>, key: String) {
    if let Err(e) = Notices::new(&state.data_dir).dismiss(&key) {
        tracing::warn!(error = %e, "couldn't dismiss the approval notice");
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AddressDto {
    /// `ligusaddr1:…`: not a secret, grants nothing.
    pub address: String,
    pub relay: String,
    /// Reset address is offered to Admins only (a client convention).
    pub can_reset: bool,
}

const NO_ROUTES_TEXT: &str = "This workspace's relay has no approval mailbox. Paste the request instead.";

fn target_for(app: &AppHandle, ws: Uuid) -> Result<Target, String> {
    targets(&app.state::<AppState>())
        .into_iter()
        .find(|t| t.id == ws)
        .ok_or_else(|| "Requests reach a team workspace with a relay and its key on this device.".to_string())
}

fn mailbox_text(e: MailboxError) -> String {
    match e {
        MailboxError::NoRoutes => NO_ROUTES_TEXT.into(),
        other => other.to_string(),
    }
}

/// The workspace's approval address (Admit a box, My devices: Copy).
#[tauri::command]
pub(crate) async fn get_approval_address(app: AppHandle, workspace_id: String) -> Result<AddressDto, String> {
    let ws = Uuid::parse_str(&workspace_id).map_err(|e| e.to_string())?;
    let t = crate::off_main(app.clone(), move |app| target_for(app, ws)).await?;
    let a = current_address(&t.client(), t.id, &t.founder_pin, &t.epochs).await.map_err(mailbox_text)?;
    Ok(AddressDto { address: a.encode(), relay: a.relay, can_reset: t.is_admin })
}

/// Reset address: the old one stops working (Admins only, by convention).
#[tauri::command]
pub(crate) async fn reset_approval_address(app: AppHandle, workspace_id: String) -> Result<AddressDto, String> {
    let ws = Uuid::parse_str(&workspace_id).map_err(|e| e.to_string())?;
    let t = crate::off_main(app.clone(), move |app| target_for(app, ws)).await?;
    if !t.is_admin {
        return Err("Only an Admin resets the approval address.".into());
    }
    let cur = t.current().ok_or("No workspace key on this device yet.")?;
    let counter = t.client().reset_address(&cur.key).await.map_err(mailbox_text)?;
    tracing::info!(target: "approvals", workspace = %ws, "reset the approval address");
    let a = ApprovalAddress::derive(&t.relay, t.id, &t.founder_pin, cur.epoch, &cur.key, counter);
    Ok(AddressDto { address: a.encode(), relay: a.relay, can_reset: true })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PasteMatchDto {
    pub workspace_id: String,
    pub rid: String,
}

/// Pasted request text that is a live request's code: run the code match
/// for it instead of the fingerprint compare. `None`: compare as today.
#[tauri::command]
pub(crate) fn approval_match_paste(state: tauri::State<AppState>, text: String) -> Option<PasteMatchDto> {
    let ws = state.active_workspace_id();
    let st = DESK.state.lock().unwrap();
    let f = st.found.get(&ws)?;
    let codes: Vec<(String, &str)> = f.live.iter().filter(|l| l.ended.is_none()).map(|l| (l.rid_hex(), l.request.code.as_str())).collect();
    let rid = paste_match(&text, codes.iter().map(|(r, c)| (r.as_str(), *c)))?;
    Some(PasteMatchDto { workspace_id: ws.to_string(), rid: rid.to_string() })
}

fn identity(state: &AppState) -> Result<ApproverIdentity, String> {
    let (device_id, author) = {
        let svc = state.service.lock().unwrap();
        (svc.device_id(), svc.author().clone())
    };
    let kp = state.identity.device_keypair(device_id).map_err(|e| e.to_string())?;
    let account_id = author.account_id.ok_or("This device has no account identity.")?;
    let name = crate::shown(&author.display_name);
    Ok(ApproverIdentity {
        signer: Signer::Ed25519(kp),
        device_name: format!("{name}'s desktop"),
        person: ApproverPerson { account_id, name },
    })
}

/// The open session's view, if this device has one for `rid`.
#[tauri::command]
pub(crate) fn approval_session(rid: String) -> Option<SessionDto> {
    DESK.sessions.lock().unwrap().get(&rid).map(|s| s.view.lock().unwrap().clone())
}

/// Open a request: claim it, open the exchange, and wait in the background
/// for the other device's answer (the code arrives as [`SESSION_EVENT`]).
/// Reopening a request already open here returns its session.
#[tauri::command]
pub(crate) async fn approval_open(app: AppHandle, workspace_id: String, rid: String) -> Result<SessionDto, String> {
    if let Some(v) = approval_session(rid.clone()) {
        return Ok(v);
    }
    let ws = Uuid::parse_str(&workspace_id).map_err(|e| e.to_string())?;
    let (t, live) = found_request(ws, &rid).ok_or("This request is gone (expired or ended).")?;
    let active = app.state::<AppState>().active_workspace_id();
    if active != ws {
        return Err(format!("Switch to {} to approve this request.", t.name));
    }
    let allowed = if live.kind() == MatchKind::Box { t.can_box } else { t.can_devices };
    if !allowed {
        return Err(if live.kind() == MatchKind::Box {
            "Only a workspace Admin can admit a box.".into()
        } else {
            "Add devices from a desktop that holds your account key.".into()
        });
    }
    let epoch = t.epochs.iter().find(|e| e.epoch == live.epoch).cloned().ok_or("That request's key epoch isn't on this device.")?;
    let (me, data_dir) = {
        let st = app.state::<AppState>();
        (identity(&st)?, st.data_dir.clone())
    };
    let mut seen = SeenSet::load(&data_dir).map_err(|e| e.to_string())?;
    let session = ApproverSession::begin(t.client(), &epoch, &live.request, me, &mut seen).await.map_err(|e| e.to_string())?;
    tracing::info!(target: "approvals", rid = %rid, kind = kind_str(live.kind()), "opened an approval request");
    let view = SessionDto {
        rid: rid.clone(),
        workspace_id: ws.to_string(),
        kind: kind_str(live.kind()).into(),
        label: live.label(),
        fingerprint: live.fingerprint(),
        relay: live.relay.clone(),
        created_at: live.created_at(),
        phase: "waiting".into(),
        ..SessionDto::default()
    };
    let slot = Arc::new(Slot {
        ws,
        kind: live.kind(),
        label: live.label(),
        request: live.request.clone(),
        session: tokio::sync::Mutex::new(session),
        view: Mutex::new(view.clone()),
    });
    DESK.sessions.lock().unwrap().insert(rid, slot.clone());
    tauri::async_runtime::spawn(await_code(app.clone(), slot));
    let _ = app.emit(CHANGED_EVENT, ());
    Ok(view)
}

async fn await_code(app: AppHandle, slot: Arc<Slot>) {
    loop {
        if phase(&slot) != "waiting" {
            return;
        }
        let r = slot.session.lock().await.await_reveal(STEP_WAIT_SECS).await;
        match r {
            Ok(Some(code)) => {
                set_view(&app, &slot, |v| {
                    v.phase = "code".into();
                    v.code = Some(code.shown.clone());
                    v.spelled = Some(code.spelled.clone());
                    v.expires_at = Some(code.expires_at);
                    v.code_at = Some(now_ms());
                });
                return;
            }
            Ok(None) => continue,
            Err(ApproverError::Mailbox(e)) if e.retryable() => tokio::time::sleep(Duration::from_secs(2)).await,
            Err(e) => {
                let (p, msg) = ended_by(&e);
                if p == "mismatch" {
                    add_fact(&app, NoticeKind::Mismatch, &slot);
                }
                set_view(&app, &slot, |v| {
                    v.phase = p.into();
                    v.message = Some(msg);
                });
                return;
            }
        }
    }
}

fn fail(app: &AppHandle, slot: &Slot, e: &ApproverError) -> SessionDto {
    let (p, msg) = ended_by(e);
    match p {
        "mismatch" => add_fact(app, NoticeKind::Mismatch, slot),
        "didntFinish" => add_fact(app, NoticeKind::DidntFinish, slot),
        _ => {}
    }
    set_view(app, slot, |v| {
        v.phase = p.into();
        v.message = Some(msg);
    })
}

/// Back to the code after a refused or cancelled passkey: nothing was
/// written, the person may try again while the code lasts.
fn back_to_code(app: &AppHandle, slot: &Slot, why: String) -> SessionDto {
    set_view(app, slot, |v| {
        v.phase = "code".into();
        v.message = Some(why);
    })
}

/// **Codes match**: write the approval exactly as today over this request's
/// keys, with the passkey (its sheet names the label and the code), and
/// send the result. For a computer, the offer and its ack come first.
#[tauri::command]
pub(crate) async fn approval_codes_match(app: AppHandle, rid: String) -> Result<SessionDto, String> {
    let slot = slot(&rid)?;
    let (ph, code) = {
        let v = slot.view.lock().unwrap();
        (v.phase.clone(), v.code.clone())
    };
    if ph != "code" {
        return Err("There is no code to confirm.".into());
    }
    let code = code.ok_or("There is no code to confirm.")?;
    if app.state::<AppState>().active_workspace_id() != slot.ws {
        return Err("Switch back to this request's workspace to approve it.".into());
    }
    let ctx = MatchContext { label: slot.label.clone(), code };
    let ws = slot.ws;
    match slot.kind {
        MatchKind::Box => {
            set_view(&app, &slot, |v| {
                v.phase = "passkey".into();
                v.message = None;
            });
            let req_code = slot.request.code.clone();
            let issued = crate::off_main(app.clone(), move |app| {
                let st = app.state::<AppState>();
                // The passkey sheet names the label and the code.
                st.service.lock().unwrap().set_match_context(Some(ctx));
                let r = crate::issue_box_admission_code(&st, ws, Some(req_code), None);
                st.service.lock().unwrap().set_match_context(None);
                r
            })
            .await;
            let (dto, _) = match issued {
                Ok(d) => d,
                Err(e) => return Ok(back_to_code(&app, &slot, e)),
            };
            tracing::info!(target: "approvals", rid = %rid, "admitted a box by code match");
            let r = slot.session.lock().await.finish_approved(Some(&dto.code)).await;
            Ok(match r {
                Ok(()) => set_view(&app, &slot, |v| v.phase = "approved".into()),
                Err(e) => fail(&app, &slot, &e),
            })
        }
        MatchKind::Phone => {
            let RequestSubject::Phone(pc) = slot.request.subject.clone() else { return Err("Not a phone request.".into()) };
            set_view(&app, &slot, |v| {
                v.phase = "passkey".into();
                v.message = None;
            });
            let certified = crate::off_main(app.clone(), move |app| {
                let st = app.state::<AppState>();
                let r = {
                    let mut svc = st.service.lock().unwrap();
                    svc.set_match_context(Some(ctx));
                    let r = svc.certify_phone_code(ws, &pc).map_err(|e| e.to_string());
                    svc.set_match_context(None);
                    r
                };
                if let Ok(id) = &r {
                    crate::remember_phone_label(&st, *id, &pc.label);
                }
                r
            })
            .await;
            if let Err(e) = certified {
                return Ok(back_to_code(&app, &slot, e));
            }
            tracing::info!(target: "approvals", rid = %rid, "certified a phone by code match");
            let r = slot.session.lock().await.finish_approved(None).await;
            Ok(match r {
                Ok(()) => set_view(&app, &slot, |v| v.phase = "approved".into()),
                Err(e) => fail(&app, &slot, &e),
            })
        }
        MatchKind::Computer => computer_match(app, slot, ctx).await,
    }
}

/// Identity model v2, 8.1: grant sealed now, offer, wait for the ack (2
/// minutes), only then the passkey and the certificate, then the result.
async fn computer_match(app: AppHandle, slot: Arc<Slot>, ctx: MatchContext) -> Result<SessionDto, String> {
    let RequestSubject::Computer(req) = slot.request.subject.clone() else { return Err("Not a computer request.".into()) };
    let ws = slot.ws;
    let acked = slot.session.lock().await.acknowledged();
    if !acked {
        let req2 = req.clone();
        let grant = crate::off_main(app.clone(), move |app| {
            let st = app.state::<AppState>();
            let conn = crate::computer_link_conn(&st, ws)?;
            let svc = st.service.lock().unwrap();
            svc.prepare_computer_link(ws, &req2, &conn, now_ms()).map_err(|e| e.to_string())
        })
        .await;
        let grant = match grant {
            Ok(g) => g,
            Err(e) => return Ok(back_to_code(&app, &slot, e)),
        };
        if let Err(e) = slot.session.lock().await.offer(grant).await {
            return Ok(fail(&app, &slot, &e));
        }
        set_view(&app, &slot, |v| {
            v.phase = "confirming".into();
            v.message = None;
        });
        loop {
            if phase(&slot) != "confirming" {
                // Ended from the sheet meanwhile.
                return Ok(slot.view.lock().unwrap().clone());
            }
            let r = slot.session.lock().await.await_ack(STEP_WAIT_SECS).await;
            match r {
                Ok(true) => break,
                Ok(false) => continue,
                Err(ApproverError::Mailbox(e)) if e.retryable() => tokio::time::sleep(Duration::from_secs(2)).await,
                Err(e) => return Ok(fail(&app, &slot, &e)),
            }
        }
    }
    set_view(&app, &slot, |v| {
        v.phase = "passkey".into();
        v.message = None;
    });
    let published = crate::off_main(app.clone(), move |app| {
        let st = app.state::<AppState>();
        let conn = crate::computer_link_conn(&st, ws)?;
        let mut svc = st.service.lock().unwrap();
        svc.set_match_context(Some(ctx));
        let r = svc.publish_computer_link(ws, &req, &conn, now_ms()).map_err(|e| e.to_string());
        svc.set_match_context(None);
        r
    })
    .await;
    if let Err(e) = published {
        // Acknowledged, nothing written: Codes match again runs the passkey.
        return Ok(back_to_code(&app, &slot, e));
    }
    tracing::info!(target: "approvals", rid = %slot.request.rid_hex(), "added a computer by code match");
    let r = slot.session.lock().await.finish_approved(None).await;
    Ok(match r {
        Ok(()) => set_view(&app, &slot, |v| v.phase = "approved".into()),
        Err(e) => fail(&app, &slot, &e),
    })
}

/// **Don't match**: end the request; nothing is granted; the Inbox keeps
/// the fact for a day.
#[tauri::command]
pub(crate) async fn approval_dont_match(app: AppHandle, rid: String) -> Result<SessionDto, String> {
    let slot = slot(&rid)?;
    set_view(&app, &slot, |v| v.phase = "mismatch".into());
    let r = slot.session.lock().await.mismatch().await;
    add_fact(&app, NoticeKind::Mismatch, &slot);
    if let Err(e) = r {
        tracing::debug!(target: "approvals", rid = %rid, error = %e, "couldn't post the mismatch");
    }
    Ok(set_view(&app, &slot, |v| {
        v.phase = "mismatch".into();
        v.message = Some("Nothing was granted.".into());
    }))
}

/// The code ran out on screen.
#[tauri::command]
pub(crate) async fn approval_expire(app: AppHandle, rid: String) -> Result<SessionDto, String> {
    let slot = slot(&rid)?;
    if !matches!(phase(&slot).as_str(), "waiting" | "code") {
        return Ok(slot.view.lock().unwrap().clone());
    }
    set_view(&app, &slot, |v| v.phase = "expired".into());
    let _ = slot.session.lock().await.expire().await;
    Ok(set_view(&app, &slot, |v| {
        v.phase = "expired".into();
        v.message = Some("The code expired. Nothing was granted.".into());
    }))
}

async fn end_without_session(app: &AppHandle, ws: Uuid, rid: &str, reason: AbortReason) -> Result<(), String> {
    let (t, live) = found_request(ws, rid).ok_or("This request is gone (expired or ended).")?;
    let epoch = t.epochs.iter().find(|e| e.epoch == live.epoch).cloned().ok_or("That request's key epoch isn't on this device.")?;
    let me = identity(&app.state::<AppState>())?;
    end_request(&t.client(), &epoch, &live.request, &me.signer, reason).await.map_err(|e| e.to_string())?;
    let mut st = DESK.state.lock().unwrap();
    if let Some(f) = st.found.get_mut(&ws) {
        f.live.retain(|l| l.rid_hex() != rid);
    }
    Ok(())
}

/// **End this request** (the contested sheet): an abort as contested.
#[tauri::command]
pub(crate) async fn approval_end(app: AppHandle, workspace_id: String, rid: String) -> Result<(), String> {
    if let Ok(slot) = slot(&rid) {
        set_view(&app, &slot, |v| v.phase = "ended".into());
        slot.session.lock().await.contested().await.map_err(|e| e.to_string())?;
        set_view(&app, &slot, |v| v.message = Some("The request was ended. Nothing was granted.".into()));
        return Ok(());
    }
    let ws = Uuid::parse_str(&workspace_id).map_err(|e| e.to_string())?;
    end_without_session(&app, ws, &rid, AbortReason::Contested).await?;
    let _ = app.emit(CHANGED_EVENT, ());
    Ok(())
}

/// Decline (the row menu, or the sheet before a code): nothing is granted.
#[tauri::command]
pub(crate) async fn approval_decline(app: AppHandle, workspace_id: String, rid: String) -> Result<(), String> {
    if let Ok(slot) = slot(&rid) {
        set_view(&app, &slot, |v| v.phase = "declined".into());
        slot.session.lock().await.decline().await.map_err(|e| e.to_string())?;
        return Ok(());
    }
    let ws = Uuid::parse_str(&workspace_id).map_err(|e| e.to_string())?;
    end_without_session(&app, ws, &rid, AbortReason::Cancelled).await?;
    let _ = app.emit(CHANGED_EVENT, ());
    Ok(())
}

/// The sheet closed on a finished session: forget it.
#[tauri::command]
pub(crate) fn approval_forget(app: AppHandle, rid: String) {
    let mut s = DESK.sessions.lock().unwrap();
    if s.get(&rid).is_some_and(|slot| !matches!(phase(slot).as_str(), "waiting" | "code" | "confirming" | "passkey")) {
        s.remove(&rid);
        drop(s);
        let _ = app.emit(CHANGED_EVENT, ());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_runs_the_exchange_only_for_a_byte_equal_live_request() {
        let live = [("r1", "ligusjoin1:AAAA"), ("r2", "liguslink1:BBBB")];
        assert_eq!(paste_match("  ligusjoin1:AAAA\n", live), Some("r1"));
        assert_eq!(paste_match("liguslink1:BBBB", live), Some("r2"));
        assert_eq!(paste_match("ligusjoin1:AAAB", live), None, "one byte off: the fingerprint compare");
        assert_eq!(paste_match("ligusjoin1:aaaa", live), None, "case matters");
        assert_eq!(paste_match("ligusphone1:AAAA", [("r3", "ligusphone1:AAAA")]), None, "phones have their own flow");
        assert_eq!(paste_match("", live), None);
    }

    #[test]
    fn notices_reach_the_inbox_with_the_runtimes_copy() {
        let n = Notice {
            kind: NoticeKind::Mismatch,
            workspace_id: Uuid::nil(),
            rid: "ab".repeat(16),
            request_kind: MatchKind::Box,
            label: "build-1".into(),
            at: 5,
            by: None,
        };
        let d = fact_dto(&n);
        assert_eq!(d.kind, "mismatch");
        assert_eq!(d.key, n.rid);
        assert_eq!(d.text, "Request from build-1 didn't match. Nothing was granted.");
        assert_eq!(d.help, n.help());
        let d = fact_dto(&Notice { kind: NoticeKind::DidntFinish, ..n });
        assert_eq!((d.kind.as_str(), d.text.as_str()), ("didntFinish", "Linking build-1 didn't finish. Nothing was added."));
    }

    #[test]
    fn groups_for_several_asking() {
        assert!(same_group(MatchKind::Computer, MatchKind::Phone));
        assert!(same_group(MatchKind::Box, MatchKind::Box));
        assert!(!same_group(MatchKind::Box, MatchKind::Computer));
    }
}

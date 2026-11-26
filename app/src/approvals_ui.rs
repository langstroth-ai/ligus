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
    claim_view, current_address, end_request, hint, list_live, AbortReason, ApprovalAddress, ApproverError, ApproverIdentity, ApproverPerson, ApproverSession,
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
/// Phone requests in the mailbox are shown, never approved from it.
const PHONE_READ_ONLY: &str = "Add phones from My devices > Add a phone.";
/// How long after the code appears Codes match is refused (design 7.2): the
/// sheet holds the button as long, and the backend enforces it too.
pub(crate) const HOLD_MS: i64 = 3_000;
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

/// A name folded for "looks the same": lower case, confusable letters and
/// digits mapped to one form, everything but letters and digits dropped. A
/// requester's label whose skeleton equals an existing device or member
/// name gets one quiet Details line.
pub(crate) fn skeleton(name: &str) -> String {
    let lower: String = name.to_lowercase().replace("rn", "m").replace("vv", "w");
    lower
        .chars()
        .filter_map(|c| {
            let c = match c {
                '0' | 'ο' | 'о' => 'o',
                '1' | 'i' | 'l' | '|' | 'ı' | 'і' => 'l',
                '5' => 's',
                '3' => 'e',
                '4' => 'a',
                'а' => 'a',
                'е' => 'e',
                'с' => 'c',
                'р' => 'p',
                'х' => 'x',
                'у' => 'y',
                c => c,
            };
            c.is_alphanumeric().then_some(c)
        })
        .collect()
}

/// Run `write` (the passkey ceremony and the log write) only when the
/// session may still commit (UI-1): not ended, the code not expired, a
/// computer's ack in time, no abort. Otherwise nothing is written.
pub(crate) async fn commit_if_ready<T, F: std::future::Future<Output = T>>(
    session: &tokio::sync::Mutex<ApproverSession>,
    write: impl FnOnce() -> F,
) -> Result<T, ApproverError> {
    session.lock().await.ready_to_commit().await?;
    Ok(write().await)
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
    /// Folded names: a device id's "<member>'s device", from the log.
    device_names: HashMap<Uuid, String>,
    /// Skeletons of the existing member and device names.
    name_skeletons: HashSet<String>,
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

impl ClaimDto {
    /// The view, naming a claimer by its folded name where the log knows
    /// the device, else by its own (bounded) hint.
    fn from_view(v: ClaimView, names: &HashMap<Uuid, String>) -> Self {
        let name = |hinted: String, id: Uuid| names.get(&id).cloned().unwrap_or_else(|| hint(&hinted));
        match v {
            ClaimView::Unclaimed => Self::Unclaimed,
            ClaimView::Mine => Self::Mine,
            ClaimView::ClaimedBy { name: n, device_id } => Self::ClaimedBy { name: name(n, device_id) },
            ClaimView::Contested { by } => Self::Contested { name: by.map(|(n, id)| name(n, id)) },
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
    /// The label looks like an existing device or member name.
    pub name_clash: bool,
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
        ApproverError::CodeExpired | ApproverError::RequestExpired => ("expired", "Expired. Nothing was granted.".into()),
        ApproverError::DidntFinish => ("didntFinish", "The computer didn't confirm in time. Nothing was added.".into()),
        ApproverError::Aborted(_)
        | ApproverError::Mailbox(MailboxError::RequestUnknown)
        | ApproverError::Ended
        | ApproverError::NoCode
        | ApproverError::NotAcknowledged => ("ended", "The request was ended. Nothing was granted.".into()),
        other => ("failed", other.to_string()),
    }
}

// ── The loop ─────────────────────────────────────────────────────────────

/// The log's names: each certified device as "<member>'s device" (or
/// phone), and the skeletons of the member names.
fn folded_names(svc: &ligus_runtime::ChatService, ws: Uuid, local: Uuid) -> (HashMap<Uuid, String>, HashSet<String>) {
    let mut names = HashMap::new();
    let mut skeletons = HashSet::new();
    let Some(pin) = svc.store().workspace_pin(ws).ok().flatten() else { return (names, skeletons) };
    let Ok((config, timeline)) = svc.store().load_config(ws, Some(&pin.founder_account.to_string()), Some(local)) else { return (names, skeletons) };
    let members: HashMap<Uuid, String> = config
        .map(|c| c.members.iter().filter_map(|m| Some((m.actor.account_id?, crate::shown(&m.actor.display_name)))).collect())
        .unwrap_or_default();
    for n in members.values() {
        skeletons.insert(skeleton(n));
    }
    for (id, d) in &timeline.current().devices {
        if let Some(n) = d.accounts.iter().find_map(|a| members.get(a)) {
            names.insert(*id, hint(&format!("{n}'s {}", if d.phone { "phone" } else { "device" })));
        }
    }
    (names, skeletons)
}

/// Every team workspace this device can approve in, with its epoch keys.
fn targets(state: &AppState) -> Vec<Target> {
    let (conns, static_token, phone_labels) = {
        let s = state.settings.lock().unwrap();
        (s.workspaces.clone(), s.relay_access_token.clone(), s.phone_labels.values().cloned().collect::<Vec<String>>())
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
        let (device_names, mut name_skeletons) = folded_names(&svc, ws, state.local_workspace_id);
        for h in svc.list_hosts(ws).unwrap_or_default() {
            name_skeletons.insert(skeleton(&h.label));
        }
        name_skeletons.extend(phone_labels.iter().map(|l| skeleton(l)));
        name_skeletons.remove("");
        out.push(Target {
            id: ws,
            name: crate::shown(&conn.display_name()),
            relay: conn.relay_url.trim().trim_end_matches('/').to_string(),
            token: crate::ligus_auth_ui::effective(Some(&conn.relay_url), static_token.clone()),
            founder_pin,
            epochs,
            approvers,
            device_names,
            name_skeletons,
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
            let claim = ClaimDto::from_view(claim_view(l, me, &t.approvers), &t.device_names);
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
                name_clash: t.name_skeletons.contains(&skeleton(&l.label())),
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
    // A phone is added from My devices > Add a phone until phones send
    // code-match requests themselves: never certified from the mailbox.
    if live.kind() == MatchKind::Phone {
        return Err(PHONE_READ_ONLY.into());
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
/// send the result. For a computer, the offer and its ack come first. The
/// session is checked right after the person chose and again right before
/// the passkey and the log write ([`commit_if_ready`]); anything else ends
/// it with nothing written.
#[tauri::command]
pub(crate) async fn approval_codes_match(app: AppHandle, rid: String) -> Result<SessionDto, String> {
    let slot = slot(&rid)?;
    if app.state::<AppState>().active_workspace_id() != slot.ws {
        return Err("Switch back to this request's workspace to approve it.".into());
    }
    if slot.kind == MatchKind::Phone {
        return Err(PHONE_READ_ONLY.into());
    }
    // One Codes match at a time, never inside the hold: the phase moves on
    // under the same lock that checks it.
    let code = {
        let mut v = slot.view.lock().unwrap();
        claim_codes_match(&mut v, now_ms())?
    };
    let _ = app.emit(SESSION_EVENT, slot.view.lock().unwrap().clone());
    if let Err(e) = slot.session.lock().await.ready_to_commit().await {
        return Ok(fail(&app, &slot, &e));
    }
    let ctx = MatchContext { label: slot.label.clone(), code };
    let ws = slot.ws;
    if slot.kind == MatchKind::Computer {
        return computer_match(app, slot, ctx).await;
    }
    let req_code = slot.request.code.clone();
    let app2 = app.clone();
    let issued = commit_if_ready(&slot.session, move || {
        crate::off_main(app2, move |app| {
            let st = app.state::<AppState>();
            // The passkey sheet names the label and the code.
            st.service.lock().unwrap().set_match_context(Some(ctx));
            let r = crate::issue_box_admission_code(&st, ws, Some(req_code), None);
            st.service.lock().unwrap().set_match_context(None);
            r
        })
    })
    .await;
    let (dto, _) = match issued {
        Err(e) => return Ok(fail(&app, &slot, &e)),
        Ok(Err(e)) => return Ok(back_to_code(&app, &slot, e)),
        Ok(Ok(d)) => d,
    };
    tracing::info!(target: "approvals", rid = %rid, "admitted a box by code match");
    let r = slot.session.lock().await.finish_approved(Some(&dto.code)).await;
    Ok(match r {
        Ok(()) => set_view(&app, &slot, |v| v.phase = "approved".into()),
        Err(e) => fail(&app, &slot, &e),
    })
}

/// The check-and-move of Codes match on the session's view: only from the
/// code, only once the hold passed. Returns the code.
pub(crate) fn claim_codes_match(v: &mut SessionDto, now: i64) -> Result<String, String> {
    if v.phase != "code" {
        return Err("There is no code to confirm.".into());
    }
    let code = v.code.clone().ok_or("There is no code to confirm.")?;
    if now.saturating_sub(v.code_at.unwrap_or(i64::MAX)) < HOLD_MS {
        return Err("Compare the codes first.".into());
    }
    v.phase = if v.kind == "computer" { "confirming" } else { "passkey" }.into();
    v.message = None;
    Ok(code)
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
    let app2 = app.clone();
    let published = commit_if_ready(&slot.session, move || {
        crate::off_main(app2, move |app| {
            let st = app.state::<AppState>();
            let conn = crate::computer_link_conn(&st, ws)?;
            let mut svc = st.service.lock().unwrap();
            svc.set_match_context(Some(ctx));
            let r = svc.publish_computer_link(ws, &req, &conn, now_ms()).map_err(|e| e.to_string());
            svc.set_match_context(None);
            r
        })
    })
    .await;
    match published {
        Err(e) => return Ok(fail(&app, &slot, &e)),
        // Acknowledged, nothing written: Codes match again runs the passkey.
        Ok(Err(e)) => return Ok(back_to_code(&app, &slot, e)),
        Ok(Ok(())) => {}
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

    fn view(phase: &str, code_at: i64) -> SessionDto {
        SessionDto { phase: phase.into(), kind: "box".into(), code: Some("K7Q4 M9X2".into()), code_at: Some(code_at), ..SessionDto::default() }
    }

    #[test]
    fn codes_match_is_refused_inside_the_hold_and_runs_once() {
        let mut v = view("code", 10_000);
        assert!(claim_codes_match(&mut v, 10_000 + HOLD_MS - 1).is_err(), "inside the 3-second hold");
        assert_eq!(v.phase, "code");
        assert_eq!(claim_codes_match(&mut v, 10_000 + HOLD_MS).unwrap(), "K7Q4 M9X2");
        assert_eq!(v.phase, "passkey");
        assert!(claim_codes_match(&mut v, 10_000 + HOLD_MS + 1).is_err(), "a second Codes match");
        let mut c = SessionDto { kind: "computer".into(), ..view("code", 0) };
        claim_codes_match(&mut c, HOLD_MS).unwrap();
        assert_eq!(c.phase, "confirming");
        assert!(claim_codes_match(&mut view("waiting", 0), HOLD_MS).is_err());
        assert!(claim_codes_match(&mut SessionDto { code_at: None, ..view("code", 0) }, i64::MAX).is_err());
    }

    #[test]
    fn look_alike_names_share_a_skeleton() {
        assert_eq!(skeleton("Ana's MacBook"), skeleton("ana s rnacbook"));
        assert_eq!(skeleton("build-1"), skeleton("BUILD-l"));
        assert_eq!(skeleton("lt on mbp"), skeleton("1t 0n mbp"));
        assert_ne!(skeleton("build-1"), skeleton("build-2"));
        assert_eq!(skeleton("Аna"), skeleton("Ana"), "Cyrillic A");
    }

    /// UI-1: past the code's 5 minutes, Codes match writes nothing.
    #[tokio::test]
    async fn past_the_code_ttl_codes_match_writes_nothing() {
        use ligus_core::crypto::{device_id_for, SigAlg, SigningKeypair};
        use ligus_core::e2ee::KeyAgreementKeypair;
        use ligus_runtime::approval_mailbox::{Requester, RequestSpec};
        let (router, clock) = ligus_runtime::test_relay::router_with_clock();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = MailboxClient::new(&base).with_clock(clock.clock());
        let key = [7u8; 32];
        let counter = client.ensure_registered(&key).await.unwrap();
        let address = ApprovalAddress::derive(&base, Uuid::new_v4(), &"ab".repeat(16), 1, &key, counter);
        let epoch = EpochKey { epoch: 1, key };
        // A box asks.
        let bk = SigningKeypair::generate().unwrap();
        let ka = KeyAgreementKeypair::generate().unwrap();
        let pk = bk.public_key_bytes();
        let join = ligus_runtime::box_admission::JoinRequest::new(Uuid::new_v4(), device_id_for(SigAlg::Ed25519, &pk), &pk, &ka.public_key_bytes(), "build-1");
        let spec = RequestSpec { kind: MatchKind::Box, code: join.encode(), signer: Signer::Ed25519(bk), ka: Some(ka) };
        let tmp = tempfile::tempdir().unwrap();
        let mut req = Requester::post(client.clone(), &address, spec, Some(tmp.path())).await.unwrap();
        let live = list_live(&client, std::slice::from_ref(&epoch)).await.unwrap().live.remove(0);
        let ak = SigningKeypair::generate().unwrap();
        let me = ApproverIdentity {
            signer: Signer::Ed25519(ak),
            device_name: "Ana's desktop".into(),
            person: ApproverPerson { account_id: Uuid::new_v4(), name: "Ana".into() },
        };
        let session = ApproverSession::begin(client.clone(), &epoch, &live.request, me, &mut SeenSet::in_memory()).await.unwrap();
        let session = tokio::sync::Mutex::new(session);
        req.poll(0).await.unwrap();
        assert!(session.lock().await.await_reveal(0).await.unwrap().is_some(), "the code shows");
        // Ready while the code lasts.
        let wrote = std::sync::atomic::AtomicBool::new(false);
        assert!(commit_if_ready(&session, || async {}).await.is_ok());
        // Six minutes later the person chooses Codes match.
        clock.advance(6 * 60_000);
        let r = commit_if_ready(&session, || async {
            wrote.store(true, std::sync::atomic::Ordering::SeqCst);
        })
        .await;
        assert_eq!(r.unwrap_err(), ApproverError::CodeExpired);
        assert!(!wrote.load(std::sync::atomic::Ordering::SeqCst), "no passkey, nothing on the log");
        assert_eq!(ended_by(&ApproverError::CodeExpired).1, "Expired. Nothing was granted.");
    }

    #[test]
    fn groups_for_several_asking() {
        assert!(same_group(MatchKind::Computer, MatchKind::Phone));
        assert!(same_group(MatchKind::Box, MatchKind::Box));
        assert!(!same_group(MatchKind::Box, MatchKind::Computer));
    }
}

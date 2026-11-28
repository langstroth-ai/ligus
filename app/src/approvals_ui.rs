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
//! * a passkey hand-off (workspace model v3, 17.3): another of the person's
//!   own devices (the CLI, lt) sent one drafted change for the passkey. The
//!   card shows what it does, rendered from the draft, and the code the
//!   terminal printed; **Confirm with passkey** runs the passkey over that
//!   exact draft's challenge and seals the assertion back; **Not me** writes
//!   nothing and leaves an Inbox row offering to revoke the device. A native
//!   notification says one is waiting.
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
    check_presence_ask, claim_view, current_address, end_request, hint, list_live, AbortReason, ApprovalAddress, ApproverError, ApproverIdentity, ApproverPerson,
    ApproverSession, AskRate, ClaimView, EpochKey, LiveRequest, MailboxClient, MailboxError, Notice, NoticeKind, Notices, PresenceApproval, PresenceView,
    RequestSubject, SeenSet, Signer, VerifiedRequest,
};
use ligus_runtime::chat_service::PresenceProvider as _;
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
        MatchKind::Presence => "presence",
    }
}

/// Same group for "2 devices are asking": boxes count with boxes, a
/// person's computers and phones together, hand-offs with hand-offs.
fn same_group(a: MatchKind, b: MatchKind) -> bool {
    let g = |k: MatchKind| match k {
        MatchKind::Box => 0,
        MatchKind::Computer | MatchKind::Phone => 1,
        MatchKind::Presence => 2,
    };
    g(a) == g(b)
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
            NoticeKind::NotMe => "notMe",
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
        device_id: None,
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
    /// This device's account, and that account's own devices here with their
    /// names (who may ask this desktop for a passkey).
    account: Option<Uuid>,
    my_devices: HashMap<Uuid, String>,
}

impl Target {
    /// A passkey hand-off this desktop may confirm: checked against the log
    /// (its account's own device, this workspace), rendered from the draft.
    fn presence(&self, l: &LiveRequest) -> Option<PresenceView> {
        if !self.can_devices {
            return None;
        }
        check_presence_ask(&l.request, self.id, self.account?, &|d| self.my_devices.contains_key(&d)).ok()
    }
    /// The asking device's name: the log's, else its own hint.
    fn device_name(&self, v: &PresenceView) -> String {
        self.my_devices.get(&v.device_id).filter(|n| !n.is_empty()).cloned().unwrap_or_else(|| v.label.clone())
    }
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
    /// Hand-offs shown per asking device and hour.
    rate: AskRate,
    /// Hand-offs a notification was shown for.
    notified: HashSet<String>,
}

struct Slot {
    ws: Uuid,
    kind: MatchKind,
    label: String,
    request: VerifiedRequest,
    session: tokio::sync::Mutex<ApproverSession>,
    view: Mutex<SessionDto>,
}

/// A passkey hand-off open on this device.
struct PresenceSlot {
    ws: Uuid,
    view: PresenceView,
    session: tokio::sync::Mutex<PresenceApproval>,
    dto: Mutex<SessionDto>,
}

#[derive(Default)]
struct Desk {
    state: Mutex<DeskState>,
    sessions: Mutex<HashMap<String, Arc<Slot>>>,
    presence: Mutex<HashMap<String, Arc<PresenceSlot>>>,
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
    /// A passkey hand-off: what it does, from the draft.
    pub summary: Option<String>,
    /// A passkey hand-off: the code the terminal printed (known at once).
    pub code: Option<String>,
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
    /// A passkey hand-off: what it does, from the draft, and its event kind.
    pub summary: Option<String>,
    pub change_kind: Option<String>,
    /// A computer link: the person's other workspaces it brings
    /// ([`crate::link_more`]).
    pub more: Vec<crate::link_more::MoreWsDto>,
    /// Which passkey is up when there is more than one (`2 of 3`).
    pub step: Option<String>,
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
        let account = svc.author().account_id;
        let my_devices = account
            .map(|a| {
                svc.my_device_ids(ws)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|d| (d, svc.device_label_of(ws, d, a).map(|l| hint(&l)).unwrap_or_default()))
                    .collect()
            })
            .unwrap_or_default();
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
            account,
            my_devices,
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
                    let fresh = new_handoffs(&t, &live);
                    let mut st = DESK.state.lock().unwrap();
                    st.no_routes.remove(&t.id);
                    st.found.insert(t.id, Found { target: t, live });
                    let lines: Vec<String> = fresh.into_iter().filter(|(rid, _)| st.notified.insert(rid.clone())).map(|(_, l)| l).collect();
                    drop(st);
                    for line in lines {
                        crate::notices::post(&app, crate::notices::Notice::new(crate::notices::NoticeKind::NeedsYou, None, line));
                    }
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

/// The hand-offs in `live` this desktop may confirm, with the line a
/// notification shows ("“laptop” asks for your passkey: …").
fn new_handoffs(t: &Target, live: &[LiveRequest]) -> Vec<(String, String)> {
    live.iter()
        .filter(|l| l.kind() == MatchKind::Presence && l.ended.is_none() && l.claims.is_empty())
        .filter_map(|l| {
            let v = t.presence(l)?;
            Some((l.rid_hex(), format!("\u{201C}{}\u{201D} asks for your passkey: {}", t.device_name(&v), v.summary)))
        })
        .collect()
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
    let presence = DESK.presence.lock().unwrap();
    let mut st = DESK.state.lock().unwrap();
    let st = &mut *st;
    let mut requests = Vec::new();
    let now = now_ms();
    for f in st.found.values() {
        let t = &f.target;
        let open: Vec<&LiveRequest> = f.live.iter().filter(|l| l.ended.is_none()).collect();
        for l in &f.live {
            let rid = l.rid_hex();
            if l.kind() == MatchKind::Presence {
                let slot = presence.get(&rid);
                let Some(v) = t.presence(l) else { continue };
                // At most a few an hour from one device; the rest wait unseen.
                if slot.is_none() && !st.rate.admit(v.device_id, &rid, now) {
                    continue;
                }
                let claim = ClaimDto::from_view(claim_view(l, me, &t.approvers), &t.device_names);
                if slot.is_none() && (l.ended.is_some() || claim == ClaimDto::Mine) {
                    continue;
                }
                requests.push(ApprovalDto {
                    workspace_id: t.id.to_string(),
                    workspace_name: t.name.clone(),
                    rid,
                    kind: kind_str(l.kind()).into(),
                    label: t.device_name(&v),
                    fingerprint: l.fingerprint(),
                    created_at: l.created_at(),
                    expires_at: l.expires_at,
                    relay: l.relay.clone(),
                    claim,
                    parallel: open.iter().filter(|o| same_group(o.kind(), l.kind())).count(),
                    phase: slot.map(|s| s.dto.lock().unwrap().phase.clone()),
                    name_clash: false,
                    summary: Some(v.summary.clone()),
                    code: Some(v.code.shown.clone()),
                });
                continue;
            }
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
                summary: None,
                code: None,
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

/// What a short link code for workspace `ws` needs (`link_code_ui`): the
/// relay client, the founder pin and the epoch keys. Only a person's own
/// device shows one (not a delegated box).
pub(crate) fn link_code_target(app: &AppHandle, ws: Uuid) -> Result<(MailboxClient, String, Vec<EpochKey>), String> {
    let t = target_for(app, ws)?;
    if !t.can_devices {
        return Err("Only your own desktop shows a code to link a device.".into());
    }
    if t.epochs.is_empty() {
        return Err("No workspace key on this device yet.".into());
    }
    Ok((t.client(), t.founder_pin.clone(), t.epochs.clone()))
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
    if let Some(p) = DESK.presence.lock().unwrap().get(&rid) {
        return Some(p.dto.lock().unwrap().clone());
    }
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
    if live.kind() == MatchKind::Presence {
        return presence_open(app, t, live).await;
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
    if let Some(p) = presence_slot(&rid) {
        return presence_confirm(app, p).await;
    }
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
            let grant = st.service.lock().unwrap().prepare_computer_link(ws, &req2, &conn, now_ms()).map_err(|e| e.to_string())?;
            // The person's other workspaces come along (each its own grant).
            let (more, skipped) = crate::link_more::prepare(&st, ws, &req2, now_ms());
            Ok::<_, String>((grant, more, skipped))
        })
        .await;
        let (grant, more, skipped) = match grant {
            Ok(g) => g,
            Err(e) => return Ok(back_to_code(&app, &slot, e)),
        };
        let names: Vec<(Uuid, String)> = more.iter().map(|m| (m.workspace_id, m.name.clone())).collect();
        let left_out = match slot.session.lock().await.offer_with(grant, more).await {
            Ok(l) => l,
            Err(e) => return Ok(fail(&app, &slot, &e)),
        };
        set_view(&app, &slot, |v| {
            v.phase = "confirming".into();
            v.message = None;
            v.more = crate::link_more::offered_rows(&names, &left_out);
            v.more.extend(skipped);
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
    // The computer took these more workspaces: one passkey each, after this one.
    let acked_more = slot.session.lock().await.acked_more();
    let total = 1 + acked_more.len();
    let first_name = {
        let st = app.state::<AppState>();
        let s = st.settings.lock().unwrap();
        s.workspaces.iter().find(|w| w.id() == ws).map(|w| crate::shown(&w.display_name())).unwrap_or_default()
    };
    set_view(&app, &slot, |v| {
        v.phase = "passkey".into();
        v.message = None;
        v.step = (total > 1).then(|| format!("1 of {total}"));
        for r in &mut v.more {
            if r.state == "offered" && !acked_more.iter().any(|m| m.workspace_id.to_string() == r.workspace_id) {
                r.state = "notTaken".into();
                r.message = Some("This computer links one workspace at a time: link it there afterwards.".into());
            }
        }
    });
    let app2 = app.clone();
    let (req1, ctx1) = (req.clone(), ctx.clone());
    let published = commit_if_ready(&slot.session, move || {
        crate::off_main(app2, move |app| {
            let st = app.state::<AppState>();
            let conn = crate::computer_link_conn(&st, ws)?;
            crate::link_more::with_step(app, &first_name, 1, total, |svc| {
                svc.set_match_context(Some(ctx1));
                let r = svc.publish_computer_link(ws, &req1, &conn, now_ms()).map_err(|e| e.to_string());
                svc.set_match_context(None);
                r
            })
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
    let mut added = Vec::new();
    for (i, m) in acked_more.iter().enumerate() {
        let id = m.workspace_id.to_string();
        let mark = |state: &str, message: Option<String>| {
            set_view(&app, &slot, |v| {
                v.step = Some(format!("{} of {total}", i + 2));
                if let Some(r) = v.more.iter_mut().find(|r| r.workspace_id == id) {
                    r.state = state.into();
                    r.message = message;
                }
            })
        };
        // The computer may have ended the request meanwhile: stop, nothing more is written.
        if let Err(e) = slot.session.lock().await.still_open().await {
            return Ok(fail(&app, &slot, &e));
        }
        mark("adding", None);
        match crate::link_more::publish_one(app.clone(), m.workspace_id, m.name.clone(), req.clone(), ctx.clone(), i + 2, total).await {
            Ok(()) => {
                added.push(m.workspace_id);
                mark("added", None);
            }
            Err(e) => {
                mark("notAdded", Some(e));
            }
        }
    }
    tracing::info!(target: "approvals", rid = %slot.request.rid_hex(), more = added.len(), of = acked_more.len(), "added the computer to more workspaces");
    let r = slot.session.lock().await.finish_approved_with(&added).await;
    Ok(match r {
        Ok(()) => set_view(&app, &slot, |v| v.phase = "approved".into()),
        Err(e) => fail(&app, &slot, &e),
    })
}

/// **Don't match**: end the request; nothing is granted; the Inbox keeps
/// the fact for a day.
#[tauri::command]
pub(crate) async fn approval_dont_match(app: AppHandle, rid: String) -> Result<SessionDto, String> {
    if let Some(p) = presence_slot(&rid) {
        return Ok(presence_not_me(&app, &p).await);
    }
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
    if let Some(p) = presence_slot(&rid) {
        if p.dto.lock().unwrap().phase != "code" {
            return Ok(p.dto.lock().unwrap().clone());
        }
        return Ok(presence_view_set(&app, &p, |v| {
            v.phase = "expired".into();
            v.message = Some("Expired. Nothing was confirmed.".into());
        }));
    }
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
    if let Some(p) = presence_slot(&rid) {
        presence_decline(&app, &p).await;
        return Ok(());
    }
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
    if let Some(p) = presence_slot(&rid) {
        presence_decline(&app, &p).await;
        return Ok(());
    }
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
    {
        let mut p = DESK.presence.lock().unwrap();
        if p.get(&rid).is_some_and(|slot| !matches!(slot.dto.lock().unwrap().phase.as_str(), "code" | "passkey")) {
            p.remove(&rid);
            drop(p);
            let _ = app.emit(CHANGED_EVENT, ());
            return;
        }
    }
    let mut s = DESK.sessions.lock().unwrap();
    if s.get(&rid).is_some_and(|slot| !matches!(phase(slot).as_str(), "waiting" | "code" | "confirming" | "passkey")) {
        s.remove(&rid);
        drop(s);
        let _ = app.emit(CHANGED_EVENT, ());
    }
}

// ── Passkey hand-off (workspace model v3, 17.3) ───────────────────────────

fn presence_slot(rid: &str) -> Option<Arc<PresenceSlot>> {
    DESK.presence.lock().unwrap().get(rid).cloned()
}

fn presence_view_set(app: &AppHandle, slot: &PresenceSlot, f: impl FnOnce(&mut SessionDto)) -> SessionDto {
    let v = {
        let mut v = slot.dto.lock().unwrap();
        f(&mut v);
        v.clone()
    };
    let _ = app.emit(SESSION_EVENT, v.clone());
    let _ = app.emit(CHANGED_EVENT, ());
    v
}

/// Open a hand-off: claim it (the person's other devices show who is
/// checking it) and show its card: what it does, from the draft, and the
/// code the terminal printed.
async fn presence_open(app: AppHandle, t: Target, live: LiveRequest) -> Result<SessionDto, String> {
    let view = t.presence(&live).ok_or("This request isn't from one of your devices here (or it was revoked).")?;
    let epoch = t.epochs.iter().find(|e| e.epoch == live.epoch).cloned().ok_or("That request's key epoch isn't on this device.")?;
    let (me, data_dir) = {
        let st = app.state::<AppState>();
        (identity(&st)?, st.data_dir.clone())
    };
    let mut seen = SeenSet::load(&data_dir).map_err(|e| e.to_string())?;
    let session = PresenceApproval::begin(t.client(), &epoch, &live.request, me, &mut seen).await.map_err(|e| e.to_string())?;
    let rid = live.rid_hex();
    tracing::info!(target: "approvals", rid = %rid, kind = %view.kind, "opened a passkey hand-off");
    let dto = SessionDto {
        rid: rid.clone(),
        workspace_id: t.id.to_string(),
        kind: "presence".into(),
        label: t.device_name(&view),
        fingerprint: live.fingerprint(),
        relay: live.relay.clone(),
        created_at: live.created_at(),
        phase: "code".into(),
        code: Some(view.code.shown.clone()),
        spelled: Some(view.code.spelled.clone()),
        expires_at: Some(view.code.expires_at),
        code_at: Some(now_ms()),
        message: None,
        summary: Some(view.summary.clone()),
        change_kind: Some(view.kind.clone()),
        ..SessionDto::default()
    };
    let slot = Arc::new(PresenceSlot { ws: t.id, view, session: tokio::sync::Mutex::new(session), dto: Mutex::new(dto.clone()) });
    DESK.presence.lock().unwrap().insert(rid, slot);
    let _ = app.emit(CHANGED_EVENT, ());
    Ok(dto)
}

/// **Confirm with passkey**: still live and still one of my devices (read
/// again from the log), then the passkey over this exact draft's challenge
/// (the summary and the relying party are this desktop's own), then the
/// assertion sealed back to the terminal. A cancelled passkey goes back to
/// the card; nothing is sent.
async fn presence_confirm(app: AppHandle, slot: Arc<PresenceSlot>) -> Result<SessionDto, String> {
    if app.state::<AppState>().active_workspace_id() != slot.ws {
        return Err("Switch back to this request's workspace to confirm it.".into());
    }
    {
        let mut v = slot.dto.lock().unwrap();
        claim_codes_match(&mut v, now_ms())?;
    }
    let _ = app.emit(SESSION_EVENT, slot.dto.lock().unwrap().clone());
    if let Err(e) = slot.session.lock().await.ready().await {
        return Ok(presence_failed(&app, &slot, &e));
    }
    let draft = slot.session.lock().await.ask().draft.clone();
    let (ws, device, challenge) = (slot.ws, slot.view.device_id, slot.view.challenge);
    let app2 = app.clone();
    let asserted = crate::off_main(app.clone(), move |app| {
        let st = app.state::<AppState>();
        let req = {
            let svc = st.service.lock().unwrap();
            if !svc.is_my_device(ws, device).unwrap_or(false) {
                return Err("That device isn't one of yours any more.".to_string());
            }
            svc.presence_request_for(&draft).map_err(|e| e.to_string())?
        };
        if req.challenge != challenge {
            return Err("The change isn't the one shown.".to_string());
        }
        crate::passkey_ui::DesktopPresence(app2).assert(&req)
    })
    .await;
    let assertion = match asserted {
        Ok(a) => a,
        Err(why) => {
            return Ok(presence_view_set(&app, &slot, |v| {
                v.phase = "code".into();
                v.message = Some(why);
            }))
        }
    };
    let r = slot.session.lock().await.approve(&assertion).await;
    Ok(match r {
        Ok(()) => {
            tracing::info!(target: "approvals", rid = %slot.view.rid, "confirmed a passkey hand-off");
            presence_view_set(&app, &slot, |v| {
                v.phase = "approved".into();
                v.message = None;
            })
        }
        Err(e) => presence_failed(&app, &slot, &e),
    })
}

fn presence_failed(app: &AppHandle, slot: &PresenceSlot, e: &ApproverError) -> SessionDto {
    let (phase, msg) = match e {
        ApproverError::RequestExpired | ApproverError::CodeExpired => ("expired", "Expired. Nothing was confirmed.".to_string()),
        ApproverError::Aborted(_) | ApproverError::Ended | ApproverError::Mailbox(MailboxError::RequestUnknown) => {
            ("ended", "The other device cancelled it. Nothing was confirmed.".to_string())
        }
        other => ("failed", other.to_string()),
    };
    presence_view_set(app, slot, |v| {
        v.phase = phase.into();
        v.message = Some(msg);
    })
}

/// **Not me**: a declined answer (nothing is written), and an Inbox row for
/// a day offering to revoke the device that asked.
async fn presence_not_me(app: &AppHandle, slot: &PresenceSlot) -> SessionDto {
    presence_decline(app, slot).await;
    let n = Notice {
        kind: NoticeKind::NotMe,
        workspace_id: slot.ws,
        rid: slot.view.rid.clone(),
        request_kind: MatchKind::Presence,
        label: slot.dto.lock().unwrap().label.clone(),
        at: now_ms(),
        by: None,
        device_id: Some(slot.view.device_id),
    };
    if let Err(e) = Notices::new(&app.state::<AppState>().data_dir).record(n) {
        tracing::warn!(error = %e, "couldn't keep the Not me notice");
    }
    presence_view_set(app, slot, |v| {
        v.phase = "declined".into();
        v.message = Some("Nothing was written.".into());
    })
}

async fn presence_decline(app: &AppHandle, slot: &PresenceSlot) {
    if let Err(e) = slot.session.lock().await.decline().await {
        tracing::debug!(target: "approvals", rid = %slot.view.rid, error = %e, "couldn't post the decline");
    }
    presence_view_set(app, slot, |v| v.phase = "declined".into());
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
            device_id: None,
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
        assert!(same_group(MatchKind::Presence, MatchKind::Presence));
        assert!(!same_group(MatchKind::Presence, MatchKind::Computer));
    }
}

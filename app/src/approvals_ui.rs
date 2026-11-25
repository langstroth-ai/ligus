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
//! The passkey sheet's summary names the label and the code. Nothing about
//! the code is logged or written to the workspace log (decision D4); the
//! only things kept on disk are the seen-set (`approval-seen.json`) and the
//! Inbox facts of the last day (`approval-facts.json`), both device-local.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use ligus_core::match_code::MatchKind;
use ligus_core::WorkspaceRole;
use ligus_runtime::approval_mailbox::{
    claim_view, end_request, list_live, AbortReason, ApprovalAddress, Approvability, ApproverClassifier, ApproverError, ApproverIdentity, ApproverPerson,
    ApproverSession, ClaimView, EpochKey, LiveRequest, MailboxClient, MailboxError, RequestSubject, SeenSet, Signer, VerifiedRequest,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;

use crate::AppState;

/// The listing changed (no payload): the Inbox refetches [`list_approvals`].
pub(crate) const CHANGED_EVENT: &str = "approvals://changed";
/// An open approval moved on. Payload: [`SessionDto`].
pub(crate) const SESSION_EVENT: &str = "approvals://session";
/// The Inbox facts of the last day (a mismatch, a link that didn't finish).
const FACTS_FILE: &str = "approval-facts.json";
const FACT_TTL_MS: i64 = 24 * 60 * 60 * 1000;
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

// ── Who may approve what (pure, tested) ──────────────────────────────────

/// One certified device, as the classifier needs it.
#[derive(Debug, Clone, Default)]
pub(crate) struct DeviceFacts {
    pub accounts: BTreeSet<Uuid>,
    pub revoked: bool,
    pub phone: bool,
    pub public_key: Vec<u8>,
    pub delegated_for: BTreeSet<Uuid>,
}

/// The folded workspace, reduced to what decides a claim (design 6.4): an
/// Admin's device for a box, the person's own device for a computer or a
/// phone; revoked, re-keyed or unknown devices can't approve.
#[derive(Debug, Clone, Default)]
pub(crate) struct Roster {
    /// This device's person.
    pub me: Option<Uuid>,
    /// Accounts of human members who are Admins (or the Owner).
    pub admins: HashSet<Uuid>,
    pub devices: HashMap<Uuid, DeviceFacts>,
}

impl ApproverClassifier for Roster {
    fn classify(&self, kind: MatchKind, device_id: Uuid, key: &[u8]) -> Approvability {
        let Some(d) = self.devices.get(&device_id) else { return Approvability::Unknown };
        if d.revoked || d.public_key != key || d.accounts.is_empty() {
            return Approvability::Cannot;
        }
        let ok = match kind {
            MatchKind::Box => d.accounts.iter().any(|a| self.admins.contains(a)),
            // A computer or a phone is certified by a non-delegated,
            // non-phone device of the person (link-a-computer 3).
            MatchKind::Computer | MatchKind::Phone => self.me.is_some_and(|me| d.accounts.contains(&me) && !d.phone && !d.delegated_for.contains(&me)),
        };
        if ok {
            Approvability::CanApprove
        } else {
            Approvability::Cannot
        }
    }
}

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

/// The passkey sheet's summary for an approval (design 6.2, test M12).
pub(crate) fn presence_summary(kind: MatchKind, label: &str, code: &str) -> String {
    let l = if label.trim().is_empty() { "unnamed".to_string() } else { label.to_string() };
    match kind {
        MatchKind::Box => format!("Admit box \u{201C}{l}\u{201D}, code {code}"),
        MatchKind::Computer => format!("Add computer \u{201C}{l}\u{201D} to your devices, code {code}"),
        MatchKind::Phone => format!("Add phone \u{201C}{l}\u{201D} to your devices, code {code}"),
    }
}

/// The event kind each approval drafts (the presence request's `kind`).
fn drafted_kind(kind: MatchKind) -> &'static str {
    match kind {
        MatchKind::Box => "joinApproved",
        MatchKind::Computer | MatchKind::Phone => "deviceCertificateAdded",
    }
}

// ── Facts (device-local, a day) ──────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FactDto {
    pub key: String,
    /// `mismatch` or `didntFinish`.
    pub kind: String,
    pub workspace_id: String,
    pub label: String,
    pub at: i64,
}

fn load_facts(dir: &Path, now: i64) -> Vec<FactDto> {
    let mut v: Vec<FactDto> = std::fs::read(dir.join(FACTS_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    v.retain(|f| now.saturating_sub(f.at) <= FACT_TTL_MS);
    v
}

fn save_facts(dir: &Path, facts: &[FactDto]) {
    if let Ok(b) = serde_json::to_vec(facts) {
        if let Err(e) = ligus_runtime::private_file::write_private_atomic(&dir.join(FACTS_FILE), &b) {
            tracing::warn!(error = %e, "couldn't keep the approval facts");
        }
    }
}

fn add_fact(app: &AppHandle, kind: &str, ws: Uuid, label: &str, rid: &str) {
    let dir = app.state::<AppState>().data_dir.clone();
    let now = now_ms();
    let mut facts = load_facts(&dir, now);
    let key = format!("{kind}:{rid}");
    if !facts.iter().any(|f| f.key == key) {
        facts.push(FactDto { key, kind: kind.into(), workspace_id: ws.to_string(), label: label.into(), at: now });
        save_facts(&dir, &facts);
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
    roster: Roster,
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

fn roster_of(svc: &ligus_runtime::ChatService, ws: Uuid, founder: &str, local: Uuid) -> Roster {
    let mut r = Roster { me: svc.author().account_id, ..Roster::default() };
    let Ok((config, timeline)) = svc.store().load_config(ws, Some(founder), Some(local)) else { return r };
    if let Some(config) = config {
        for m in &config.members {
            if m.actor.kind == ligus_core::ActorKind::Human && config.role_of(&m.actor.id).rank() >= WorkspaceRole::Admin.rank() {
                if let Some(a) = m.actor.account_id {
                    r.admins.insert(a);
                }
            }
        }
    }
    for (id, d) in &timeline.current().devices {
        r.devices.insert(
            *id,
            DeviceFacts { accounts: d.accounts.clone(), revoked: d.revoked, phone: d.phone, public_key: d.public_key.clone(), delegated_for: d.delegated_for.clone() },
        );
    }
    r
}

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
        let epochs: Vec<EpochKey> = svc
            .delivered_epoch_keys(ws, &src)
            .unwrap_or_default()
            .iter()
            .filter_map(|d| d.key().map(|key| EpochKey { epoch: d.v, key }))
            .collect();
        let Some(pin) = svc.store().workspace_pin(ws).ok().flatten() else { continue };
        if epochs.is_empty() {
            continue;
        }
        let role = svc.my_workspace_role(ws);
        let is_admin = role.is_some_and(|r| r.rank() >= WorkspaceRole::Admin.rank());
        let human = svc.author().kind == ligus_core::ActorKind::Human && svc.author().account_id.is_some();
        let roster = roster_of(&svc, ws, &pin.founder_account.to_string(), state.local_workspace_id);
        out.push(Target {
            id: ws,
            name: crate::shown(&conn.display_name()),
            relay: conn.relay_url.trim().trim_end_matches('/').to_string(),
            token: crate::ligus_auth_ui::effective(Some(&conn.relay_url), static_token.clone()),
            founder_pin: ligus_core::keys::hex(&pin.fk),
            epochs,
            roster,
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
            let claim: ClaimDto = claim_view(l, me, &t.roster).into();
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
    ApprovalsDto { requests, facts: load_facts(&state.data_dir, now_ms()) }
}

/// Forget an Inbox fact (its row's Dismiss).
#[tauri::command]
pub(crate) fn approval_dismiss_fact(state: tauri::State<AppState>, key: String) {
    let mut facts = load_facts(&state.data_dir, now_ms());
    facts.retain(|f| f.key != key);
    save_facts(&state.data_dir, &facts);
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
    let cur = t.current().ok_or("No workspace key on this device yet.")?;
    let counter = t.client().ensure_registered(&cur.key).await.map_err(mailbox_text)?;
    let a = ApprovalAddress::derive(&t.relay, t.id, &t.founder_pin, cur.epoch, &cur.key, counter);
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
                    add_fact(&app, "mismatch", slot.ws, &slot.label, &slot.request.rid_hex());
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
    if p == "mismatch" || p == "didntFinish" {
        add_fact(app, p, slot.ws, &slot.label, &slot.request.rid_hex());
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
    let summary = presence_summary(slot.kind, &slot.label, &code);
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
                crate::passkey_ui::with_approval_summary(drafted_kind(MatchKind::Box), summary, || crate::issue_box_admission_code(&st, ws, Some(req_code), None))
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
                let r = crate::passkey_ui::with_approval_summary(drafted_kind(MatchKind::Phone), summary, || {
                    st.service.lock().unwrap().certify_phone_code(ws, &pc).map_err(|e| e.to_string())
                });
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
        MatchKind::Computer => computer_match(app, slot, summary).await,
    }
}

/// Identity model v2, 8.1: grant sealed now, offer, wait for the ack (2
/// minutes), only then the passkey and the certificate, then the result.
async fn computer_match(app: AppHandle, slot: Arc<Slot>, summary: String) -> Result<SessionDto, String> {
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
        crate::passkey_ui::with_approval_summary(drafted_kind(MatchKind::Computer), summary, || {
            st.service.lock().unwrap().publish_computer_link(ws, &req, &conn, now_ms()).map_err(|e| e.to_string())
        })
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
    add_fact(&app, "mismatch", slot.ws, &slot.label, &rid);
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

    fn dev(accounts: &[Uuid], key: &[u8]) -> DeviceFacts {
        DeviceFacts { accounts: accounts.iter().copied().collect(), public_key: key.to_vec(), ..DeviceFacts::default() }
    }

    #[test]
    fn a_box_is_approved_by_an_admins_device_only() {
        let (admin, member) = (Uuid::new_v4(), Uuid::new_v4());
        let (d1, d2, d3) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut r = Roster { me: Some(member), admins: [admin].into(), ..Roster::default() };
        r.devices.insert(d1, dev(&[admin], b"k1"));
        r.devices.insert(d2, dev(&[member], b"k2"));
        let mut revoked = dev(&[admin], b"k3");
        revoked.revoked = true;
        r.devices.insert(d3, revoked);
        assert_eq!(r.classify(MatchKind::Box, d1, b"k1"), Approvability::CanApprove);
        assert_eq!(r.classify(MatchKind::Box, d2, b"k2"), Approvability::Cannot, "a member's device");
        assert_eq!(r.classify(MatchKind::Box, d3, b"k3"), Approvability::Cannot, "revoked");
        assert_eq!(r.classify(MatchKind::Box, d1, b"other"), Approvability::Cannot, "another key under the id");
        assert_eq!(r.classify(MatchKind::Box, Uuid::new_v4(), b"k"), Approvability::Unknown);
    }

    #[test]
    fn a_device_of_mine_is_approved_by_my_own_full_device_only() {
        let (me, other) = (Uuid::new_v4(), Uuid::new_v4());
        let (mine, phone, lt, theirs) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut r = Roster { me: Some(me), admins: [other].into(), ..Roster::default() };
        r.devices.insert(mine, dev(&[me], b"a"));
        let mut p = dev(&[me], b"b");
        p.phone = true;
        r.devices.insert(phone, p);
        let mut d = dev(&[me], b"c");
        d.delegated_for = [me].into();
        r.devices.insert(lt, d);
        r.devices.insert(theirs, dev(&[other], b"d"));
        for k in [MatchKind::Computer, MatchKind::Phone] {
            assert_eq!(r.classify(k, mine, b"a"), Approvability::CanApprove);
            assert_eq!(r.classify(k, phone, b"b"), Approvability::Cannot, "a phone can't certify devices");
            assert_eq!(r.classify(k, lt, b"c"), Approvability::Cannot, "a delegated device can't either");
            assert_eq!(r.classify(k, theirs, b"d"), Approvability::Cannot, "an Admin's device isn't mine");
        }
    }

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
    fn the_passkey_summary_names_the_label_and_the_code() {
        let s = presence_summary(MatchKind::Box, "\u{201C}build-1\u{201D}".trim_matches(['\u{201C}', '\u{201D}']), "K7Q4 M9X2");
        assert_eq!(s, "Admit box \u{201C}build-1\u{201D}, code K7Q4 M9X2");
        assert!(presence_summary(MatchKind::Computer, "lt on mbp", "K7Q4 M9X2").contains("lt on mbp"));
        assert!(presence_summary(MatchKind::Phone, "Ana's iPhone", "K7Q4 M9X2").ends_with("code K7Q4 M9X2"));
        assert_eq!(drafted_kind(MatchKind::Box), "joinApproved");
        assert_eq!(drafted_kind(MatchKind::Computer), "deviceCertificateAdded");
    }

    #[test]
    fn the_note_replaces_only_its_own_kind() {
        let req = ligus_runtime::chat_service::PresenceRequest {
            challenge: [0; 32],
            kind: "joinApproved".into(),
            account_id: Uuid::nil(),
            credential_ids: vec![],
            digest_short: String::new(),
            summary: "Let someone join with a single-use invite".into(),
            policy: Default::default(),
        };
        let note = ("joinApproved".to_string(), "Admit box \u{201C}b\u{201D}, code K7Q4 M9X2".to_string());
        assert_eq!(crate::passkey_ui::summary_with_note(&req, Some(&note)).as_deref(), Some(note.1.as_str()));
        let other = ("deviceCertificateAdded".to_string(), "x".to_string());
        assert_eq!(crate::passkey_ui::summary_with_note(&req, Some(&other)), None);
        assert_eq!(crate::passkey_ui::summary_with_note(&req, None), None);
    }

    #[test]
    fn facts_last_a_day() {
        let dir = tempfile::tempdir().unwrap();
        let now = 10 * FACT_TTL_MS;
        let f = |key: &str, at| FactDto { key: key.into(), kind: "mismatch".into(), workspace_id: Uuid::nil().to_string(), label: "build-1".into(), at };
        save_facts(dir.path(), &[f("old", now - FACT_TTL_MS - 1), f("new", now - 1000)]);
        let got = load_facts(dir.path(), now);
        assert_eq!(got.iter().map(|f| f.key.as_str()).collect::<Vec<_>>(), ["new"]);
    }

    #[test]
    fn groups_for_several_asking() {
        assert!(same_group(MatchKind::Computer, MatchKind::Phone));
        assert!(same_group(MatchKind::Box, MatchKind::Box));
        assert!(!same_group(MatchKind::Box, MatchKind::Computer));
    }
}

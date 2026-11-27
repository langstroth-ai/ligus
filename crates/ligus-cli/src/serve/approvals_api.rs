//! Code-match approval for a paired phone (`docs/design/code-match-wire.md`
//! section 5; design note 5.2 and 7.3).
//!
//! The **phone** is the approver: it picks `nA` and its own ephemeral key,
//! signs with its P-256 device key and seals every message it sends itself.
//! This box only carries bytes. As a member it can read the workspace's
//! approval mailboxes (the current key epoch and the two before it), so it:
//!
//! * polls them ([`poll_loop`]), re-registering a mailbox the relay forgot
//!   (the relay keeps mailboxes in memory), and pushes `approval.request`
//!   for a new box request (at most [`PUSH_LIMIT`] in [`PUSH_WINDOW_MS`]);
//! * lists the live requests with each one's plaintext envelope
//!   (`GET /v1/approvals`), which the phone parses and verifies itself;
//! * hands over a request's messages (`GET /v1/approvals/{rid}/msgs`),
//!   opening only what is sealed to the members (claims, aborts), which the
//!   phone can't open because it can't derive the seal key;
//! * posts the phone's sealed `claim`, `open`, `result` and `abort` with its
//!   member auth (`POST /v1/approvals/{rid}/claim|open|result|abort`),
//!   unchanged.
//!
//! A rewritten byte changes the phone's code or fails a phone check, and the
//! box never sees `nR` before `nA` is fixed (it can't open the reveal). The
//! box admission itself goes through `/v1/admissions/prepare|presence|commit`,
//! whose `code` (sealed to the requesting box) the phone signs into its
//! `result` as `grantDigest`.
//!
//! A passkey hand-off (`presence`, wire note section 8) is one person's
//! alone: it is listed, pushed and carried only for a phone this box places
//! as the person the draft is stamped with ([`super::principal`]), once the
//! box's own fold says the asking device is one of that person's devices
//! ([`presence_view`]). The row adds what the phone shows and signs: the
//! summary rendered here from the draft, the code, the challenge (the
//! request's own `commit`), and the relying party and credentials. The
//! phone checks the request's signature and the code itself, runs the
//! passkey over `commit`, and signs and seals its `result`; the requesting
//! terminal takes it only from one of its person's devices. Any other phone,
//! and one this box can't place, never sees it (its routes answer 404).
//!
//! A computer's or a phone's request is listed too (the phone shows it
//! read-only, "Approve on your desktop") but never pushed. Its code can't be
//! shown there: the `open` and the reveal are sealed to the requester and
//! the approver, so neither this box nor the phone can compute it.
//!
//! Every route needs the tunnel (or the operator on a loopback listener): a
//! request's envelope carries the requester's read key for its `rid`.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use ligus_core::match_code::{hex, MailboxKeys, MatchKind};
use ligus_runtime::approval_mailbox::{self as am, AskRate, EpochKey, LiveRequest, MailboxAuth, MailboxClient, MailboxError, RequestSubject};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{watch, Notify};
use uuid::Uuid;

use super::principal::{Principal, Standing};
use super::tunnel::{PushContent, TunnelPeer};
use super::Shared;

/// How often the box polls the mailboxes.
pub const POLL_EVERY: Duration = Duration::from_secs(5);
/// How often it looks again at a relay without mailbox routes (the feature
/// is off there: no errors, no load).
pub const NO_ROUTES_EVERY: Duration = Duration::from_secs(300);
/// How long it waits after a relay error.
const ERROR_EVERY: Duration = Duration::from_secs(30);
/// A listing this fresh answers `GET /v1/approvals` (the phone polls).
const FRESH_FOR: Duration = Duration::from_secs(1);
/// `approval.request` pushes per workspace in [`PUSH_WINDOW_MS`]; past it
/// the phone's Inbox shows "Several join requests" instead.
pub const PUSH_LIMIT: usize = 3;
pub const PUSH_WINDOW_MS: i64 = 10 * 60 * 1000;
/// How long a request id is remembered as seen (requests live 15 minutes).
const SEEN_KEEP_MS: i64 = 30 * 60 * 1000;

/// Which pushes a workspace's new requests get: one per request id, only for
/// a box request nobody claimed yet, at most [`PUSH_LIMIT`] in
/// [`PUSH_WINDOW_MS`] (design 7.3: a flood can't wear the person down).
#[derive(Debug, Default)]
pub struct PushTracker {
    seen: HashMap<String, i64>,
    sent: VecDeque<i64>,
}

impl PushTracker {
    /// Whether request `rid` (of `kind`, `claimed` or not) pushes at `now_ms`.
    /// Every request is seen once: a later call for it is `false`.
    pub fn decide(&mut self, rid: &str, kind: MatchKind, claimed: bool, now_ms: i64) -> bool {
        self.seen.retain(|_, at| now_ms.saturating_sub(*at) <= SEEN_KEEP_MS);
        if self.seen.contains_key(rid) {
            return false;
        }
        self.seen.insert(rid.to_string(), now_ms);
        // A hand-off goes to its person's phones once ([`push`]); the
        // approver's per-device rate applies, not the box cap.
        if kind == MatchKind::Presence {
            return !claimed;
        }
        if kind != MatchKind::Box || claimed {
            return false;
        }
        while self.sent.front().is_some_and(|t| now_ms.saturating_sub(*t) >= PUSH_WINDOW_MS) {
            self.sent.pop_front();
        }
        if self.sent.len() >= PUSH_LIMIT {
            return false;
        }
        self.sent.push_back(now_ms);
        true
    }
}

#[derive(Default)]
struct Inner {
    /// The last listing (live requests of the polled epochs).
    live: Vec<LiveRequest>,
    /// The epoch keys it was read with (secrets: never logged).
    epochs: Vec<EpochKey>,
    listed_at: Option<Instant>,
    /// The relay has no mailbox routes: look again after this.
    no_routes_until: Option<Instant>,
    /// The last relay error, logged once per change.
    last_error: Option<String>,
    pushes: PushTracker,
    /// Hand-offs shown per asking device and hour (listing and pushes).
    rate: AskRate,
    /// Tests: every request id an `approval.request` went out for.
    #[cfg(test)]
    pushed: Vec<String>,
    /// Tests: another relay for the mailbox routes.
    #[cfg(test)]
    relay_override: Option<String>,
}

/// The box's view of the approval mailboxes.
#[derive(Default)]
pub struct Approvals {
    inner: std::sync::Mutex<Inner>,
    /// One listing at a time (the poll loop and the phone's GETs).
    refreshing: tokio::sync::Mutex<()>,
    /// Wakes [`poll_loop`].
    pub kick: Notify,
}

impl Approvals {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether the last look found a relay without mailbox routes.
    pub fn no_routes(&self) -> bool {
        self.lock().no_routes_until.is_some_and(|t| Instant::now() < t)
    }

    /// Tests: the request ids pushed so far.
    #[cfg(test)]
    pub fn pushed(&self) -> Vec<String> {
        self.lock().pushed.clone()
    }

    /// Tests: read the mailboxes on `relay` instead of the workspace's.
    #[cfg(test)]
    pub fn set_relay_for_test(&self, relay: &str) {
        let mut g = self.lock();
        g.relay_override = Some(relay.to_string());
        g.no_routes_until = None;
        g.listed_at = None;
    }
}

fn now_ms() -> i64 {
    ligus_core::flow::now_ms()
}

/// A mailbox client for the workspace's relay, with the access token the
/// box's relay client sends (a langstroth-auth token bound to that relay,
/// else the static one).
fn client(s: &Shared) -> Option<MailboxClient> {
    #[cfg(test)]
    if let Some(r) = s.approvals.lock().relay_override.clone() {
        return Some(MailboxClient::new(&r));
    }
    crate::relay_client(&s.cfg).ok()?;
    let url = s.cfg.relay_url.as_deref()?.trim().to_string();
    let token = crate::auth::cell().and_then(|c| c.get_for(&url)).filter(|t| !t.is_empty()).or_else(|| s.cfg.token.clone());
    Some(MailboxClient::new(&url).with_auth(token))
}

/// The workspace keys this box holds, one per authorized epoch.
fn epoch_keys(s: &Shared) -> Result<Vec<EpochKey>, String> {
    let src = crate::key_source(&s.cfg).map_err(|e| format!("{e:#}"))?;
    s.lock().approval_epoch_keys(s.ws, &src).map_err(|e| e.to_string())
}

/// Why a listing failed.
#[derive(Debug)]
enum Refresh {
    /// No relay, or no workspace key yet.
    Unavailable(String),
    Mailbox(MailboxError),
}

/// List the live requests (unless a listing is fresher than [`FRESH_FOR`]
/// and `force` is off), re-register forgotten mailboxes, and push what is
/// new. A relay without routes lists nothing and isn't asked again for
/// [`NO_ROUTES_EVERY`].
async fn refresh(s: &Shared, force: bool) -> Result<(), Refresh> {
    let _one = s.approvals.refreshing.lock().await;
    {
        let g = s.approvals.lock();
        if g.no_routes_until.is_some_and(|t| Instant::now() < t) {
            return Ok(());
        }
        if !force && g.listed_at.is_some_and(|t| t.elapsed() < FRESH_FOR) {
            return Ok(());
        }
    }
    let client = client(s).ok_or_else(|| Refresh::Unavailable("this box has no relay to read approval requests from".into()))?;
    let epochs = epoch_keys(s).map_err(Refresh::Unavailable)?;
    if epochs.is_empty() {
        return Err(Refresh::Unavailable("this box holds no workspace key yet".into()));
    }
    let listing = match am::list_live(&client, &epochs).await {
        Ok(l) => l,
        Err(MailboxError::NoRoutes) => {
            let mut g = s.approvals.lock();
            g.live.clear();
            g.listed_at = Some(Instant::now());
            g.no_routes_until = Some(Instant::now() + NO_ROUTES_EVERY);
            g.last_error = None;
            return Ok(());
        }
        Err(e) => {
            note_error(s, &e.to_string());
            return Err(Refresh::Mailbox(e));
        }
    };
    // The relay forgot these mailboxes (a restart): register them again, so
    // requesters holding the address can put.
    for epoch in &listing.unregistered {
        if let Some(k) = epochs.iter().find(|e| e.epoch == *epoch) {
            match client.ensure_registered(&k.key).await {
                Ok(_) | Err(MailboxError::StaleCounter) => {}
                Err(MailboxError::NoRoutes) => break,
                Err(e) => note_error(s, &format!("registering the approval mailbox of key epoch {epoch}: {e}")),
            }
        }
    }
    if !listing.refused.is_empty() {
        // Counted, shown nowhere (the bytes are anyone's).
        note_error(s, &format!("{} approval request(s) didn't verify and were skipped", listing.refused.len()));
    }
    let now = now_ms();
    let new: Vec<LiveRequest> = {
        let mut g = s.approvals.lock();
        g.last_error = None;
        g.no_routes_until = None;
        g.listed_at = Some(Instant::now());
        g.epochs = epochs;
        g.live = listing.live;
        let live = g.live.clone();
        live.into_iter().filter(|r| r.ended.is_none() && g.pushes.decide(&r.rid_hex(), r.kind(), !r.claims.is_empty() || r.opened_without_claim, now)).collect()
    };
    for r in new {
        push(s, &r);
    }
    Ok(())
}

fn note_error(s: &Shared, why: &str) {
    let mut g = s.approvals.lock();
    if g.last_error.as_deref() != Some(why) {
        eprintln!("approvals: {why}");
        g.last_error = Some(why.to_string());
    }
}

/// The `approval.request` push for `r` (design 7.3): "<label> wants to
/// join <workspace>", collapsing per workspace.
pub fn push_content(ws: Uuid, workspace_name: &str, r: &LiveRequest) -> PushContent {
    let label = r.label();
    let title = if workspace_name.is_empty() { format!("{label} wants to join") } else { format!("{label} wants to join {workspace_name}") };
    PushContent {
        kind: "approval.request".into(),
        title,
        fixed_body: "Open Ligus to compare the code the box shows.".into(),
        collapse: Some(format!("approvals:{ws}")),
        coalesce: Some(r.rid_hex()),
        fields: vec![("workspaceId", ws.to_string()), ("rid", r.rid_hex()), ("label", label), ("requestKind", r.kind().as_str().to_string())],
        ..Default::default()
    }
}

fn push(s: &Shared, r: &LiveRequest) {
    if r.kind() == MatchKind::Presence {
        return push_presence(s, r);
    }
    #[cfg(test)]
    s.approvals.lock().pushed.push(r.rid_hex());
    let Some(t) = &s.tunnel else { return };
    let content = push_content(s.ws, &super::identity_api::workspace_name(s), r);
    t.push_all(&content);
}

/// The `approval.presence` push for hand-off `rid`: "Your passkey, asked by
/// your device: “laptop”", for the phones of the person it is for only.
pub fn presence_push_content(ws: Uuid, rid: &str, device_name: &str) -> PushContent {
    PushContent {
        kind: "approval.presence".into(),
        title: format!("Your passkey, asked by your device: {}", quoted(device_name)),
        fixed_body: "Open Ligus to check the change and its code.".into(),
        collapse: Some(format!("approvals:{ws}")),
        coalesce: Some(rid.to_string()),
        fields: vec![("workspaceId", ws.to_string()), ("rid", rid.to_string()), ("label", device_name.to_string()), ("requestKind", "presence".into())],
        ..Default::default()
    }
}

fn quoted(label: &str) -> String {
    if label.is_empty() {
        "(no name)".into()
    } else {
        format!("\u{201C}{label}\u{201D}")
    }
}

/// Push hand-off `r` to the phones this box places as the draft's person,
/// once it checks out for that person ([`presence_view`]).
fn push_presence(s: &Shared, r: &LiveRequest) {
    let RequestSubject::Presence(ask) = &r.request.subject else { return };
    let account = ask.account_id();
    let Some(view) = presence_view(s, r, account, now_ms()) else { return };
    #[cfg(test)]
    s.approvals.lock().pushed.push(r.rid_hex());
    let Some(t) = &s.tunnel else { return };
    let pids: Vec<String> = {
        let svc = s.lock();
        t.registry
            .list()
            .into_iter()
            .filter(|rec| rec.is_active())
            .filter(|rec| rec.device_id.and_then(|d| svc.phone_person(s.ws, d).ok()).is_some_and(|p| p.account_id == Some(account)))
            .map(|rec| rec.pid)
            .collect()
    };
    if !pids.is_empty() {
        t.push_to(&presence_push_content(s.ws, &r.rid_hex(), &view.device_name), &pids);
    }
}

/// Poll the approval mailboxes while the box can write (a revoked box reads
/// nothing for anyone).
pub async fn poll_loop(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    loop {
        let writable = shared.lock().write_block().is_none();
        let wait = if !writable {
            POLL_EVERY
        } else {
            match refresh(&shared, true).await {
                Ok(()) if shared.approvals.no_routes() => NO_ROUTES_EVERY,
                Ok(()) => POLL_EVERY,
                Err(_) => ERROR_EVERY,
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = shared.approvals.kick.notified() => {}
            _ = stop.wait_for(|s| *s) => return,
        }
    }
}

// ── HTTP ────────────────────────────────────────────────────────────────────

/// A problem answer: `{"error": <message>, "code": <code>}`.
fn problem(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into(), "code": code }))).into_response()
}

/// How a relay failure reads to the phone.
fn mailbox_problem(e: &MailboxError) -> Response {
    let msg = e.to_string();
    match e {
        MailboxError::RequestUnknown => problem(StatusCode::NOT_FOUND, "request_unknown", msg),
        MailboxError::MailboxUnknown => problem(StatusCode::NOT_FOUND, "mailbox_unknown", msg),
        MailboxError::NoRoutes => problem(StatusCode::NOT_FOUND, "no_mailbox", msg),
        MailboxError::RateLimited(secs) => {
            let mut r = problem(StatusCode::TOO_MANY_REQUESTS, "rate_limited", msg);
            if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
                r.headers_mut().insert(axum::http::header::RETRY_AFTER, v);
            }
            r
        }
        MailboxError::TooManyMessages => problem(StatusCode::TOO_MANY_REQUESTS, "too_many_messages", msg),
        MailboxError::MailboxFull => problem(StatusCode::TOO_MANY_REQUESTS, "mailbox_full", msg),
        MailboxError::TooLarge => problem(StatusCode::PAYLOAD_TOO_LARGE, "too_large", msg),
        MailboxError::Unreachable(_) => problem(StatusCode::BAD_GATEWAY, "relay_unreachable", msg),
        // The relay refused this box's member credentials, or answered
        // something else: the box's trouble, not the phone's.
        _ => problem(StatusCode::BAD_GATEWAY, "relay_refused", msg),
    }
}

fn refresh_problem(e: Refresh) -> Response {
    match e {
        Refresh::Unavailable(why) => problem(StatusCode::SERVICE_UNAVAILABLE, "unavailable", why),
        Refresh::Mailbox(e) => mailbox_problem(&e),
    }
}

/// The refusal when the call didn't come through a secret channel.
fn guard(s: &Shared, peer: Option<&TunnelPeer>) -> Option<Response> {
    super::identity_api::require_secret_channel(s, peer).err().map(IntoResponse::into_response)
}

/// `rid` as 32 lowercase hex, else `None` (an unknown request).
fn parse_rid(raw: &str) -> Option<[u8; 16]> {
    if raw.len() != 32 || raw.bytes().any(|c| c.is_ascii_uppercase()) {
        return None;
    }
    ligus_core::match_code::unhex(raw)?.try_into().ok()
}

/// The certified, unrevoked device `id` holding `key` in this workspace.
fn certified(roster: Option<&ligus_core::authz::RosterState>, id: Uuid, key: &[u8]) -> bool {
    roster.and_then(|r| r.devices.get(&id)).is_some_and(|d| !d.revoked && !d.accounts.is_empty() && d.public_key == key)
}

/// The account a request acts for when it may see hand-offs: a phone this
/// box places as its person. The operator (the box itself), the shared
/// token and an unplaced phone see none.
fn handoff_account(p: &Principal) -> Option<Uuid> {
    (p.standing == Standing::Person).then(|| p.account()).flatten()
}

/// A hand-off as the phone of `account` may see it.
pub struct PresenceRowView {
    pub device_id: Uuid,
    /// The asking device's name on the log, else its cleaned label hint.
    pub device_name: String,
    pub json: Value,
}

/// Hand-off `r` for `account`'s phone, or `None` when it isn't theirs to
/// confirm: on this box's fold the draft is for this workspace, stamped
/// with `account`, signed by a device certified for `account` and not
/// revoked, and a kind that can be confirmed elsewhere
/// ([`am::check_presence_ask`]); and its asking device is within the
/// per-device hourly rate. The summary is rendered from the draft, never
/// from the request's label; the relying party and credentials come from
/// this box's fold.
pub fn presence_view(s: &Shared, r: &LiveRequest, account: Uuid, now: i64) -> Option<PresenceRowView> {
    let RequestSubject::Presence(ask) = &r.request.subject else { return None };
    let (view, req, name) = {
        let svc = s.lock();
        let mine: std::collections::HashSet<Uuid> = svc.account_device_ids(s.ws, account).ok()?.into_iter().collect();
        let view = am::check_presence_ask(&r.request, s.ws, account, &|d| mine.contains(&d)).ok()?;
        let req = svc.presence_request_as(&ask.draft, account).ok()?;
        let name = svc.device_label_of(s.ws, view.device_id, account).map(|l| am::hint(&l)).filter(|l| !l.is_empty());
        (view, req, name)
    };
    if req.challenge != r.request.commit {
        return None;
    }
    if !s.approvals.lock().rate.admit(view.device_id, &view.rid, now) {
        return None;
    }
    let device_name = name.unwrap_or_else(|| view.label.clone());
    let json = json!({
        "accountId": account,
        "deviceId": view.device_id,
        "deviceName": device_name,
        "eventKind": view.kind,
        "summary": view.summary,
        "code": view.code.shown,
        "challenge": base64url(&view.challenge),
        "rpId": req.policy.rp_id,
        "origins": req.policy.origins,
        "allowCredentials": req.credential_ids,
    });
    Some(PresenceRowView { device_id: view.device_id, device_name, json })
}

fn row(ws: Uuid, workspace_name: &str, r: &LiveRequest, roster: Option<&ligus_core::authz::RosterState>, seal_key: &[u8; 32]) -> Value {
    let claims: Vec<Value> = r
        .claims
        .iter()
        .map(|c| {
            json!({
                "approverDeviceId": c.approver_device_id,
                "approverName": c.approver_name,
                "at": c.at,
                "verified": certified(roster, c.approver_device_id, &c.key),
            })
        })
        .collect();
    json!({
        "rid": r.rid_hex(),
        "workspaceId": ws,
        "workspaceName": workspace_name,
        "kind": r.kind().as_str(),
        "label": r.label(),
        "mailbox": hex(&r.mailbox),
        "epoch": r.epoch,
        "sealKey": base64url(seal_key),
        "putAt": r.put_at,
        "expiresAt": r.expires_at,
        "request": r.request.envelope,
        "claims": claims,
    })
}

fn base64url(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// `GET /v1/approvals` → `{"approvals":[...]}`: the live, unended requests.
pub async fn list(State(s): State<Arc<Shared>>, peer: Option<Extension<TunnelPeer>>) -> Response {
    if let Some(r) = guard(&s, peer.as_deref()) {
        return r;
    }
    if let Err(e) = refresh(&s, false).await {
        return refresh_problem(e);
    }
    let (live, epochs) = {
        let g = s.approvals.lock();
        (g.live.clone(), g.epochs.clone())
    };
    let now = now_ms();
    let name = super::identity_api::workspace_name(&s);
    let roster = s.lock().workspace_roster(s.ws).ok();
    let account = handoff_account(&super::api::principal());
    let rows: Vec<Value> = live
        .iter()
        .filter(|r| r.ended.is_none() && r.expires_at > now)
        .filter_map(|r| {
            let key = epochs.iter().find(|e| e.epoch == r.epoch)?;
            // A passkey hand-off is its person's alone: listed only for a
            // phone placed as that person, and only once it checks out.
            let presence = match r.kind() {
                MatchKind::Presence => Some(presence_view(&s, r, account?, now)?.json),
                _ => None,
            };
            let mut v = row(s.ws, &name, r, roster.as_ref(), &MailboxKeys::derive(&key.key).seal().public_key_bytes());
            if let Some(p) = presence {
                v["presence"] = p;
            }
            Some(v)
        })
        .collect();
    Json(json!({ "approvals": rows })).into_response()
}

/// The mailbox keys of the listed request `rid` (a fresh listing when it
/// isn't in the last one).
///
/// A hand-off is found only for the phone of its person (else it is as
/// unknown as any other id): the box's member auth carries nobody else's
/// claim, answer or abort for it, and nobody else reads its messages.
async fn find(s: &Shared, rid: &[u8; 16]) -> Result<(MailboxKeys, [u8; 16]), Box<Response>> {
    let account = handoff_account(&super::api::principal());
    let lookup = |s: &Shared| {
        let (r, k) = {
            let g = s.approvals.lock();
            let r = g.live.iter().find(|r| r.request.rid == *rid)?.clone();
            let k = g.epochs.iter().find(|e| e.epoch == r.epoch)?.clone();
            (r, k)
        };
        if r.kind() == MatchKind::Presence {
            presence_view(s, &r, account?, now_ms())?;
        }
        Some((MailboxKeys::derive(&k.key), r.mailbox))
    };
    if let Some(found) = lookup(s) {
        return Ok(found);
    }
    refresh(s, true).await.map_err(|e| Box::new(refresh_problem(e)))?;
    lookup(s).ok_or_else(|| Box::new(problem(StatusCode::NOT_FOUND, "request_unknown", "no live approval request with that id")))
}

fn forget(s: &Shared, rid: &[u8; 16]) {
    s.approvals.lock().live.retain(|r| r.request.rid != *rid);
}

#[derive(Deserialize)]
pub struct MsgsQuery {
    #[serde(default)]
    after: u64,
}

/// `GET /v1/approvals/{rid}/msgs?after=N` → `{"msgs":[{seq, to, from, at,
/// blob, plain}]}`. `plain` is the plaintext of a blob sealed to the
/// members (claims, aborts), else null; no long poll (the phone polls).
pub async fn msgs(State(s): State<Arc<Shared>>, peer: Option<Extension<TunnelPeer>>, Path(rid): Path<String>, Query(q): Query<MsgsQuery>) -> Response {
    if let Some(r) = guard(&s, peer.as_deref()) {
        return r;
    }
    let Some(rid) = parse_rid(&rid) else {
        return problem(StatusCode::NOT_FOUND, "request_unknown", "no live approval request with that id");
    };
    let (keys, mailbox) = match find(&s, &rid).await {
        Ok(k) => k,
        Err(r) => return *r,
    };
    let Some(client) = client(&s) else {
        return problem(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "this box has no relay");
    };
    let msgs = match client.get_msgs(MailboxAuth::Member(&keys), &mailbox, &rid, q.after, 0).await {
        Ok(m) => m,
        Err(e) => {
            if e == MailboxError::RequestUnknown {
                forget(&s, &rid);
            }
            return mailbox_problem(&e);
        }
    };
    let seal = keys.seal();
    let out: Vec<Value> = msgs
        .iter()
        .map(|m| {
            let plain = am::open_blob(&seal, &m.blob).and_then(|p| String::from_utf8(p).ok());
            json!({ "seq": m.seq, "to": m.to, "from": m.from, "at": m.at, "blob": m.blob, "plain": plain })
        })
        .collect();
    Json(json!({ "msgs": out })).into_response()
}

/// What the phone posts through the box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostKind {
    Claim,
    Open,
    Result,
    Abort,
}

impl PostKind {
    /// The routing hints a message of this kind may carry (wire note 3.3):
    /// a claim goes to the members; an open and a result to the requester;
    /// an abort to both (the phone posts it twice).
    pub fn allows(self, to: &str) -> bool {
        match self {
            Self::Claim => to == "members",
            Self::Open | Self::Result => to == "requester",
            Self::Abort => to == "members" || to == "requester",
        }
    }
}

/// A non-empty base64url blob (no padding) the relay takes, long enough to
/// be a sealed message.
pub fn blob_ok(blob: &str) -> bool {
    use base64::Engine;
    !blob.is_empty()
        && blob.len() <= am::MAX_MSG_BLOB
        && blob.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        && base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(blob.as_bytes()).is_ok_and(|b| b.len() > 32)
}

#[derive(Deserialize)]
pub struct PostReq {
    blob: String,
    to: String,
}

async fn post_msg(s: Arc<Shared>, peer: Option<Extension<TunnelPeer>>, rid: String, req: PostReq, kind: PostKind) -> Response {
    if let Some(r) = guard(&s, peer.as_deref()) {
        return r;
    }
    let Some(rid) = parse_rid(&rid) else {
        return problem(StatusCode::NOT_FOUND, "request_unknown", "no live approval request with that id");
    };
    if !kind.allows(&req.to) {
        return problem(StatusCode::BAD_REQUEST, "invalid_request", format!("a {kind:?} doesn't go to '{}'", req.to.chars().take(16).collect::<String>()));
    }
    if !blob_ok(&req.blob) {
        return problem(StatusCode::BAD_REQUEST, "invalid_request", "blob must be a sealed message: non-empty base64url, at most 32 KiB");
    }
    let (keys, mailbox) = match find(&s, &rid).await {
        Ok(k) => k,
        Err(r) => return *r,
    };
    let Some(client) = client(&s) else {
        return problem(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "this box has no relay");
    };
    // The phone's bytes, unchanged.
    match client.post_msg(MailboxAuth::Member(&keys), &mailbox, &rid, &req.blob, &req.to).await {
        Ok(seq) => (StatusCode::CREATED, Json(json!({ "seq": seq }))).into_response(),
        Err(e) => {
            if e == MailboxError::RequestUnknown {
                forget(&s, &rid);
            }
            mailbox_problem(&e)
        }
    }
}

/// `POST /v1/approvals/{rid}/claim` `{blob, to:"members"}` → 201 `{seq}`.
pub async fn claim(State(s): State<Arc<Shared>>, peer: Option<Extension<TunnelPeer>>, Path(rid): Path<String>, Json(req): Json<PostReq>) -> Response {
    post_msg(s, peer, rid, req, PostKind::Claim).await
}

/// `POST /v1/approvals/{rid}/open` `{blob, to:"requester"}` → 201 `{seq}`.
pub async fn open(State(s): State<Arc<Shared>>, peer: Option<Extension<TunnelPeer>>, Path(rid): Path<String>, Json(req): Json<PostReq>) -> Response {
    post_msg(s, peer, rid, req, PostKind::Open).await
}

/// `POST /v1/approvals/{rid}/result` `{blob, to:"requester"}` → 201 `{seq}`.
pub async fn result(State(s): State<Arc<Shared>>, peer: Option<Extension<TunnelPeer>>, Path(rid): Path<String>, Json(req): Json<PostReq>) -> Response {
    post_msg(s, peer, rid, req, PostKind::Result).await
}

/// `POST /v1/approvals/{rid}/abort` `{blob, to:"members"|"requester"}` → 201 `{seq}`.
pub async fn abort(State(s): State<Arc<Shared>>, peer: Option<Extension<TunnelPeer>>, Path(rid): Path<String>, Json(req): Json<PostReq>) -> Response {
    post_msg(s, peer, rid, req, PostKind::Abort).await
}

/// The routes, under the API router's auth.
pub fn routes(r: axum::Router<Arc<Shared>>) -> axum::Router<Arc<Shared>> {
    use axum::routing::{get, post};
    r.route("/v1/approvals", get(list))
        .route("/v1/approvals/{rid}/msgs", get(msgs))
        .route("/v1/approvals/{rid}/claim", post(claim))
        .route("/v1/approvals/{rid}/open", post(open))
        .route("/v1/approvals/{rid}/result", post(result))
        .route("/v1/approvals/{rid}/abort", post(abort))
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn one_push_per_box_request_and_three_in_ten_minutes() {
        let mut t = PushTracker::default();
        let t0 = 1_000_000;
        assert!(t.decide("a", MatchKind::Box, false, t0));
        assert!(!t.decide("a", MatchKind::Box, false, t0 + 1), "a request pushes once");
        // Device requests are listed, never pushed; a claimed box request
        // (someone is already checking it) neither.
        assert!(!t.decide("c", MatchKind::Computer, false, t0 + 2));
        assert!(!t.decide("p", MatchKind::Phone, false, t0 + 3));
        assert!(!t.decide("claimed", MatchKind::Box, true, t0 + 4));
        assert!(t.decide("b", MatchKind::Box, false, t0 + 5));
        assert!(t.decide("d", MatchKind::Box, false, t0 + 6));
        // The fourth box request in 10 minutes: no push.
        assert!(!t.decide("e", MatchKind::Box, false, t0 + 7));
        assert!(!t.decide("f", MatchKind::Box, false, t0 + PUSH_WINDOW_MS - 1));
        // The window moved past the first: one more goes.
        assert!(t.decide("g", MatchKind::Box, false, t0 + PUSH_WINDOW_MS));
        assert!(!t.decide("h", MatchKind::Box, false, t0 + PUSH_WINDOW_MS + 1));
    }

    #[test]
    fn routing_hints_follow_the_wire_note() {
        assert!(PostKind::Claim.allows("members"));
        assert!(!PostKind::Claim.allows("requester"));
        assert!(PostKind::Open.allows("requester") && !PostKind::Open.allows("members") && !PostKind::Open.allows("approver"));
        assert!(PostKind::Result.allows("requester") && !PostKind::Result.allows("members"));
        assert!(PostKind::Abort.allows("members") && PostKind::Abort.allows("requester") && !PostKind::Abort.allows("approver"));
    }

    #[test]
    fn blobs_must_be_sealed_base64url() {
        let sealed = am::seal_blob(&[7; 32], b"x").unwrap();
        assert!(blob_ok(&sealed));
        assert!(!blob_ok(""));
        assert!(!blob_ok(&format!("{sealed}=")), "padding");
        assert!(!blob_ok(&format!("{}+", "A".repeat(100))), "standard base64");
        assert!(!blob_ok("abc"), "too short to be sealed");
        assert!(!blob_ok(&"A".repeat(am::MAX_MSG_BLOB + 4)));
    }

    #[test]
    fn rids_are_32_lowercase_hex() {
        assert!(parse_rid(&"ab".repeat(16)).is_some());
        assert!(parse_rid(&"AB".repeat(16)).is_none());
        assert!(parse_rid("ab").is_none());
        assert!(parse_rid(&"zz".repeat(16)).is_none());
    }
}

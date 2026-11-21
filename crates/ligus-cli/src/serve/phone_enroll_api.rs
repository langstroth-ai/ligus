//! Reverse-QR phone enrollment, the box's side
//! (`docs/design/v2/phone-enroll-reverse-qr.md` §5.5, §7): a phone that
//! scanned its person's desktop QR asks this box to relay its request to
//! that desktop, and waits here for the answer.
//!
//! - `POST /v1/phone-enroll/check` `{workspaceId, desktopDeviceId, rid, exp,
//!   deviceId?}`: who the offer's desktop belongs to here. Writes nothing.
//! - `POST /v1/phone-enroll/requests` `{request: "ligusenrollreq1:…"}`: every
//!   check the box can make (not the MAC: it never has the QR's secret), the
//!   limits, then the request is published on the relay's presence channel
//!   at `phone-enroll:<rid>:<this box's device id>`, sealed under the
//!   workspace key and signed by this box's device key. `202 {rid, state:
//!   "relayed"}`.
//! - `GET /v1/phone-enroll/requests/{rid}?wait=N`: long-polls (N ≤ 25 s) for
//!   the desktop's answer, relayed as is when the offer's own desktop signed
//!   it, and says whether this box's roster now has the phone's certificate
//!   (`certified`, the phone's success, review R7).
//!
//! Callers: a provisional tunnel peer (E3b, its own router:
//! [`provisional_router`]), a paired tunnel peer, the bearer and the
//! operator. Nothing here is secret (no keys, no tokens), and the box writes
//! nothing to any log; it relays. Pending requests live in memory until
//! their `exp` and are dropped on restart (the phone re-submits on a 404).
//!
//! Log hygiene (review R10): lines name a request by its `rid` and the
//! phone's device id only, never the request text, its MAC or a code.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use ligus_runtime::phone_enroll_qr::{
    self as qr, box_check_offer, box_check_request, check_relayed_answer, open_relayed_answers, request_slot, seal_relayed_request,
    BoxRefusal, EnrollAnswer, OfferFields, RelayedAnswer, RelayedRequest, Rid,
};
use ligus_runtime::chat_service::EnrollPhoneStanding;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{watch, Notify};
use uuid::Uuid;

use super::api::{caller, Caller};
use super::tunnel::{ProvisionalPeer, TunnelPeer};
use super::Shared;

/// Pending requests on one box, all callers together (review R5).
pub const MAX_PENDING: usize = 16;
/// Requests accepted per minute, all callers together.
pub const MAX_PER_MINUTE: usize = 30;
/// Pending requests per bearer or operator caller.
pub const PER_BEARER: usize = 2;
/// Pending requests per paired tunnel peer, and per provisional connection.
pub const PER_PEER: usize = 1;
/// Requests one provisional connection may submit (E3b, P5).
pub const PROVISIONAL_MAX_REQUESTS: usize = 8;
/// How often the box looks for answers while a request is pending.
const ANSWER_POLL: Duration = Duration::from_secs(2);
/// With two phones on one offer, the slot shows each in turn this often (it
/// holds one request): the desktop, polling every 2 s, sees both.
const ROTATE: Duration = Duration::from_secs(3);
/// How long the loop reuses its keyring.
const RING_TTL: Duration = Duration::from_secs(ligus_runtime::host_presence::HOST_BEAT_EVERY_SECS as u64);
/// A request past its `exp` still answers `expired` this long, then goes.
const KEEP_EXPIRED_SECS: i64 = qr::PAST_TOLERANCE_SECS;

fn now_ms() -> i64 {
    ligus_core::Timestamp::now().inner().unix_timestamp_nanos().div_euclid(1_000_000) as i64
}

/// One line in the box's log, never with a request's text, MAC or code.
fn note(line: String) {
    eprintln!("phone-enroll: {line}");
    #[cfg(test)]
    test_log().lock().unwrap_or_else(|e| e.into_inner()).push(line);
}

/// Every [`note`] so far (tests: the log-hygiene check, review T10).
#[cfg(test)]
pub fn test_log() -> &'static Mutex<Vec<String>> {
    static LOG: std::sync::OnceLock<Mutex<Vec<String>>> = std::sync::OnceLock::new();
    LOG.get_or_init(Default::default)
}

// ── Callers ────────────────────────────────────────────────────────────────

/// Who submitted a request (§7.1): what its limits are counted under, and
/// the device the channel binds, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Channel {
    Provisional { conn: String, device: Uuid },
    Paired { pid: String, device: Option<Uuid> },
    Bearer,
    Operator,
}

impl Channel {
    fn key(&self) -> String {
        match self {
            Channel::Provisional { conn, .. } => format!("provisional:{conn}"),
            Channel::Paired { pid, .. } => format!("tunnel:{pid}"),
            Channel::Bearer => "bearer".into(),
            Channel::Operator => "operator".into(),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Channel::Provisional { .. } => "provisional tunnel",
            Channel::Paired { .. } => "paired tunnel",
            Channel::Bearer => "bearer",
            Channel::Operator => "operator",
        }
    }

    /// The device the channel proved (E3b's key, a pairing's device).
    fn device(&self) -> Option<Uuid> {
        match self {
            Channel::Provisional { device, .. } => Some(*device),
            Channel::Paired { device, .. } => *device,
            Channel::Bearer | Channel::Operator => None,
        }
    }

    fn cap(&self) -> usize {
        match self {
            Channel::Provisional { .. } | Channel::Paired { .. } => PER_PEER,
            Channel::Bearer | Channel::Operator => PER_BEARER,
        }
    }
}

fn channel(s: &Shared, prov: Option<&ProvisionalPeer>, peer: Option<&TunnelPeer>) -> Option<Channel> {
    if let Some(p) = prov {
        return Some(Channel::Provisional { conn: p.conn.clone(), device: p.device_id });
    }
    if let Some(p) = peer {
        let device = s.tunnel.as_ref().and_then(|t| t.registry.get(&p.pid)).and_then(|r| r.device_id);
        return Some(Channel::Paired { pid: p.pid.clone(), device });
    }
    match caller() {
        Caller::Operator => Some(Channel::Operator),
        Caller::Bearer => Some(Channel::Bearer),
        Caller::Tunnel(pid) => {
            let device = s.tunnel.as_ref().and_then(|t| t.registry.get(&pid)).and_then(|r| r.device_id);
            Some(Channel::Paired { pid, device })
        }
        Caller::Anonymous => None,
    }
}

// ── Answers ────────────────────────────────────────────────────────────────

fn refuse(status: StatusCode, error: &str, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": error, "message": message.into() }))).into_response()
}

fn refusal(r: &BoxRefusal) -> Response {
    let status = StatusCode::from_u16(r.status()).unwrap_or(StatusCode::BAD_REQUEST);
    refuse(status, r.error(), r.to_string())
}

fn unauthorized() -> Response {
    refuse(StatusCode::UNAUTHORIZED, "unauthorized", "missing or invalid credential")
}

fn rate_limited(message: &str) -> Response {
    refuse(StatusCode::TOO_MANY_REQUESTS, "rate_limited", message)
}

// ── The pending store ──────────────────────────────────────────────────────

/// One relayed request.
#[derive(Debug, Clone)]
struct Entry {
    rid: Rid,
    /// The `ligusenrollreq1:` text, as the phone sent it (never logged).
    text: String,
    device: Uuid,
    public_key: Vec<u8>,
    desktop: Uuid,
    /// The desktop's person (the account the phone is to be certified for).
    account: Uuid,
    exp: u64,
    channel: String,
    seq: u64,
    /// The desktop's answer slot text, once one signed by `desktop` arrived.
    answer: Option<String>,
}

impl Entry {
    fn expired(&self, now_ms: i64) -> bool {
        now_ms as i128 > self.exp as i128 * 1000
    }

    /// The answer names this entry's phone: nothing more to relay for it.
    fn answered_for_me(&self) -> bool {
        self.answer.as_deref().and_then(|a| EnrollAnswer::decode(a).ok()).is_some_and(|a| a.device_id == self.device)
    }
}

/// What one offer's slot shows.
#[derive(Debug, Default)]
struct Slot {
    shown: Option<u64>,
    published_ms: i64,
}

#[derive(Default)]
struct Inner {
    entries: Vec<Entry>,
    accepted: VecDeque<i64>,
    slots: HashMap<Rid, Slot>,
    /// Slots to clear (`{}`): their offer has nothing left to relay.
    clear: HashSet<Rid>,
    seq: u64,
}

/// Why the store refused a request.
#[derive(Debug, PartialEq, Eq)]
enum Full {
    RateLimited,
}

impl Inner {
    fn remove(&mut self, pred: impl Fn(&Entry) -> bool) -> Vec<Entry> {
        let (gone, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.entries).into_iter().partition(|e| pred(e));
        self.entries = keep;
        for e in &gone {
            if !self.entries.iter().any(|x| x.rid == e.rid) {
                self.slots.remove(&e.rid);
                self.clear.insert(e.rid);
            }
        }
        gone
    }

    fn oldest_of(&self, channel: &str) -> Option<u64> {
        self.entries.iter().filter(|e| e.channel == channel).map(|e| e.seq).min()
    }

    /// Add `e` under the limits (§7.3 step 6). Re-submitting the same bytes
    /// is idempotent; a phone's new request replaces its old one; a channel
    /// over its cap, or a full box, evicts the oldest of the heaviest caller,
    /// so a flood evicts itself. Returns the entries evicted.
    fn submit(&mut self, mut e: Entry, cap: usize, now_ms: i64) -> Result<Vec<Entry>, Full> {
        if self.entries.iter().any(|x| x.text == e.text) {
            return Ok(Vec::new());
        }
        while self.accepted.front().is_some_and(|t| *t < now_ms - 60_000) {
            self.accepted.pop_front();
        }
        if self.accepted.len() >= MAX_PER_MINUTE {
            return Err(Full::RateLimited);
        }
        let device = e.device;
        let mut evicted = self.remove(|x| x.device == device);
        evicted.clear(); // the same phone's older request: replaced, not evicted
        while self.entries.iter().filter(|x| x.channel == e.channel).count() >= cap {
            let Some(seq) = self.oldest_of(&e.channel) else { break };
            evicted.extend(self.remove(|x| x.seq == seq));
        }
        while self.entries.len() >= MAX_PENDING {
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for x in &self.entries {
                *counts.entry(x.channel.as_str()).or_default() += 1;
            }
            let heaviest = counts
                .into_iter()
                .max_by_key(|(c, n)| (*n, std::cmp::Reverse(self.oldest_of(c).unwrap_or(u64::MAX))))
                .map(|(c, _)| c.to_string());
            let Some(seq) = heaviest.and_then(|c| self.oldest_of(&c)) else { break };
            evicted.extend(self.remove(|x| x.seq == seq));
        }
        self.seq += 1;
        e.seq = self.seq;
        self.clear.remove(&e.rid);
        self.entries.push(e);
        self.accepted.push_back(now_ms);
        Ok(evicted)
    }

    /// Drop what is past its offer's `exp` (kept a while to answer `expired`).
    fn purge(&mut self, now_ms: i64) {
        let limit = |e: &Entry| now_ms as i128 > (e.exp as i128 + KEEP_EXPIRED_SECS as i128) * 1000;
        self.remove(limit);
        let live: HashSet<Rid> = self.entries.iter().filter(|e| !e.expired(now_ms)).map(|e| e.rid).collect();
        let dead: Vec<Rid> = self.slots.keys().filter(|r| !live.contains(r)).copied().collect();
        for r in dead {
            self.slots.remove(&r);
            self.clear.insert(r);
        }
    }

    /// The slots to (re)publish now: per offer, the request to show (the
    /// phones the answer doesn't name yet, in turn), and the slots to clear.
    fn plan(&mut self, now_ms: i64) -> (Vec<(Rid, String)>, Vec<Rid>) {
        self.purge(now_ms);
        let mut by_rid: HashMap<Rid, Vec<&Entry>> = HashMap::new();
        for e in self.entries.iter().filter(|e| !e.expired(now_ms) && !e.answered_for_me()) {
            by_rid.entry(e.rid).or_default().push(e);
        }
        let mut publish = Vec::new();
        let mut settled: Vec<Rid> = self.slots.keys().filter(|r| !by_rid.contains_key(r)).copied().collect();
        for (rid, mut list) in by_rid {
            list.sort_by_key(|e| e.seq);
            let slot = self.slots.entry(rid).or_default();
            let age = now_ms - slot.published_ms;
            let current = list.iter().position(|e| Some(e.seq) == slot.shown);
            let next = match current {
                None => Some(0),
                Some(i) if list.len() > 1 && age >= ROTATE.as_millis() as i64 => Some((i + 1) % list.len()),
                Some(_) if age >= qr::REPUBLISH_SECS as i64 * 1000 => current,
                Some(_) => None,
            };
            if let Some(i) = next {
                slot.shown = Some(list[i].seq);
                slot.published_ms = now_ms;
                publish.push((rid, list[i].text.clone()));
            }
        }
        for r in &settled {
            self.slots.remove(r);
        }
        settled.extend(self.clear.drain());
        settled.sort();
        settled.dedup();
        (publish, settled)
    }

    /// The offers whose answer the box still watches for (until `exp`).
    fn watched(&self, now_ms: i64) -> Vec<(Rid, Uuid)> {
        let mut out: Vec<(Rid, Uuid)> = self.entries.iter().filter(|e| !e.expired(now_ms)).map(|e| (e.rid, e.desktop)).collect();
        out.sort();
        out.dedup();
        out
    }

    /// Take the answer slots (opened, not yet checked) that the offers'
    /// own desktops signed (§5.5): each is set on the requests it answers.
    /// Returns the offers that got a new answer.
    fn take_answers(&mut self, opened: Vec<(String, RelayedAnswer)>, now_ms: i64, desktop_key: impl Fn(Uuid) -> Option<Vec<u8>>) -> Vec<Rid> {
        let watched = self.watched(now_ms);
        let mut out = Vec::new();
        for (slot, a) in opened {
            let Ok(rid) = EnrollAnswer::decode(&a.answer).map(|x| x.rid) else { continue };
            // Only the requests sent to the desktop that signed it: an
            // answer from another desktop (a co-member's, for a request
            // naming it) never answers a request for this one.
            if !watched.contains(&(rid, a.desktop)) {
                continue;
            }
            if check_relayed_answer(&slot, &a, a.desktop, now_ms, &desktop_key).is_err() {
                continue;
            }
            if self.set_answer(rid, a.desktop, &a.answer) && !out.contains(&rid) {
                out.push(rid);
            }
        }
        out
    }

    fn set_answer(&mut self, rid: Rid, desktop: Uuid, answer: &str) -> bool {
        let mut changed = false;
        for e in self.entries.iter_mut().filter(|e| e.rid == rid && e.desktop == desktop) {
            if e.answer.as_deref() != Some(answer) {
                e.answer = Some(answer.to_string());
                changed = true;
            }
        }
        changed
    }
}

/// The box's pending requests and the loop's wake-ups.
#[derive(Default)]
pub struct EnrollBox {
    inner: Mutex<Inner>,
    /// Wakes [`relay_loop`]: a new request to publish.
    kick: Notify,
    /// Wakes the long polls: an answer arrived, or the loop looked again.
    changed: Notify,
}

impl EnrollBox {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Pending requests (tests).
    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.lock().entries.len()
    }

    /// Pending requests from one phone (tests).
    #[cfg(test)]
    pub fn pending_for(&self, device: Uuid) -> usize {
        self.lock().entries.iter().filter(|e| e.device == device).count()
    }
}

// ── Relay plumbing ─────────────────────────────────────────────────────────

/// Whether this box can relay at all: a relay to publish presence on, and
/// its own device a service member's (the only signer a desktop accepts).
fn relay_refusal(s: &Shared) -> Option<Response> {
    let relay = s.cfg.relay_url.as_deref().is_some_and(|u| !u.trim().is_empty()) && crate::relay_client(&s.cfg).is_ok();
    if !relay {
        return Some(refusal(&BoxRefusal::RelayUnavailable));
    }
    let svc = s.lock();
    if svc.enroll_relay_key(s.ws, svc.device_id()).is_none() {
        return Some(refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            BoxRefusal::RelayUnavailable.error(),
            "this box isn't (yet) a certified service member of the workspace, so desktops wouldn't take a request it relays",
        ));
    }
    None
}

fn keyring(s: &Shared) -> Option<ligus_runtime::keyring::KeyringEngine> {
    match crate::key_source(&s.cfg) {
        Ok(src) => Some(ligus_runtime::keyring::KeyringEngine::from_store(s.lock().store(), s.ws, &src)),
        Err(_) => s.cfg.key.as_deref().map(|p| ligus_runtime::keyring::KeyringEngine::passphrase(s.ws, ligus_core::derive_workspace_key(p))),
    }
}

/// Publish the pending requests' slots (at once for a new one, then every
/// 10 s; clearing settled ones with `{}`), and watch for the desktops'
/// answers while any offer is open.
pub async fn relay_loop(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let Ok(relay) = crate::relay_client(&shared.cfg) else { return };
    let room = shared.cfg.room.clone();
    // The keyring, rebuilt as often as the presence loop rebuilds its own.
    let mut ring: Option<(tokio::time::Instant, ligus_runtime::keyring::KeyringEngine)> = None;
    loop {
        let now = now_ms();
        let (publish, clear, watched) = {
            let mut inner = shared.phone_enroll.lock();
            let (p, c) = inner.plan(now);
            (p, c, inner.watched(now))
        };
        if !(publish.is_empty() && clear.is_empty() && watched.is_empty()) {
            if ring.as_ref().is_none_or(|(at, _)| at.elapsed() >= RING_TTL) {
                ring = keyring(&shared).map(|r| (tokio::time::Instant::now(), r));
            }
            if let Some((_, ring)) = &ring {
                let me = shared.lock().device_id();
                for (rid, text) in publish {
                    let r = shared.lock().sign_relayed_request(RelayedRequest::new(&text, me, now_ms()));
                    let Some(blob) = seal_relayed_request(ring, &r) else {
                        note(format!("request {rid}: no workspace key to seal it under"));
                        continue;
                    };
                    if let Err(e) = relay.publish_presence(&room, &request_slot(rid, me), &blob).await {
                        note(format!("request {rid}: publishing failed: {e}"));
                    }
                }
                for rid in clear {
                    let _ = relay.publish_presence(&room, &request_slot(rid, me), &json!({})).await;
                }
                if !watched.is_empty() {
                    watch_answers(&shared, &relay, &room, ring).await;
                }
            }
        }
        shared.phone_enroll.changed.notify_waiters();
        tokio::select! {
            _ = tokio::time::sleep(ANSWER_POLL) => {}
            _ = shared.phone_enroll.kick.notified() => {}
            _ = stop.wait_for(|s| *s) => return,
        }
    }
}

/// Take the answers the offers' own desktops signed (§5.5); drop the rest.
async fn watch_answers(
    shared: &Shared,
    relay: &ligus_runtime::RelayClient,
    room: &str,
    ring: &ligus_runtime::keyring::KeyringEngine,
) {
    let Ok(blobs) = relay.list_presence(room).await else { return };
    let opened = open_relayed_answers(&blobs, ring);
    // The signers' keys first (the service's lock), then the store's lock:
    // never both at once.
    let keys: HashMap<Uuid, Option<Vec<u8>>> = {
        let svc = shared.lock();
        opened.iter().map(|(_, a)| a.desktop).collect::<HashSet<_>>().into_iter().map(|d| (d, svc.enroll_desktop_key(shared.ws, d))).collect()
    };
    let answered = shared.phone_enroll.lock().take_answers(opened, now_ms(), |d| keys.get(&d).cloned().flatten());
    for rid in &answered {
        note(format!("request {rid}: the desktop answered"));
    }
    if !answered.is_empty() {
        // Stop re-publishing what the answer covers now, not in 10 s.
        shared.phone_enroll.kick.notify_one();
    }
}

// ── Routes ─────────────────────────────────────────────────────────────────

type S = State<Arc<Shared>>;

/// The routes, behind the API's auth (bearer, operator, paired tunnel peer).
pub fn routes(r: Router<Arc<Shared>>) -> Router<Arc<Shared>> {
    r.route("/v1/phone-enroll/check", post(check))
        .route("/v1/phone-enroll/requests", post(submit))
        .route("/v1/phone-enroll/requests/{rid}", get(status))
}

/// E3b (§6.1, P4): the router a provisional tunnel connection gets. Only
/// the three enroll routes; it never reaches the API router or its auth.
pub fn provisional_router(shared: Arc<Shared>) -> Router {
    routes(Router::new())
        .layer(axum::middleware::from_fn(|req: axum::extract::Request, next: axum::middleware::Next| async move {
            if req.extensions().get::<ProvisionalPeer>().is_none() {
                return unauthorized();
            }
            next.run(req).await
        }))
        .with_state(shared)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckBody {
    workspace_id: Uuid,
    desktop_device_id: Uuid,
    rid: Rid,
    exp: u64,
    /// Off the tunnel only: the phone's device, for its standing.
    #[serde(default)]
    device_id: Option<Uuid>,
}

fn bad_json(e: impl std::fmt::Display) -> Response {
    refuse(StatusCode::BAD_REQUEST, "bad_request", format!("invalid JSON body: {e}"))
}

/// `POST /v1/phone-enroll/check` (§7.2).
async fn check(State(s): S, prov: Option<Extension<ProvisionalPeer>>, peer: Option<Extension<TunnelPeer>>, body: axum::body::Bytes) -> Response {
    let Some(ch) = channel(&s, prov.as_deref(), peer.as_deref()) else { return unauthorized() };
    let b: CheckBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => return bad_json(e),
    };
    let fields = OfferFields { workspace_id: b.workspace_id, desktop_device_id: b.desktop_device_id, rid: b.rid, exp: b.exp };
    if let Err(r) = box_check_offer(&fields, s.ws, now_ms()) {
        return refusal(&r);
    }
    let device = ch.device().or(b.device_id);
    let key = prov.as_deref().map(|p| p.public_key.clone());
    let answer = {
        let svc = s.lock();
        let desktop = match svc.enroll_desktop(s.ws, fields.desktop_device_id) {
            Ok(d) => d,
            Err(r) => return refusal(&r),
        };
        let standing = match device {
            Some(d) => match svc.enroll_phone_standing(s.ws, d, key.as_deref(), desktop.account_id) {
                Ok(st) => json!({ "standing": st.as_str() }),
                Err(r) => return refusal(&r),
            },
            None => Value::Null,
        };
        let name = svc
            .load(ligus_core::workspace_config_session_id(s.ws))
            .ok()
            .flatten()
            .map(|c| ligus_runtime::display::shown(&c.title))
            .unwrap_or_default();
        json!({
            "workspaceId": s.ws,
            "workspaceName": name,
            "person": {
                "accountId": desktop.account_id,
                "memberId": desktop.member_id,
                "displayName": desktop.display_name,
                "role": desktop.role,
                "nameCollision": desktop.name_collision,
            },
            "desktopLabel": desktop.label,
            "phone": standing,
        })
    };
    if let Some(r) = relay_refusal(&s) {
        return r;
    }
    Json(answer).into_response()
}

#[derive(Deserialize)]
pub struct SubmitBody {
    request: String,
}

/// `POST /v1/phone-enroll/requests` (§7.3).
async fn submit(State(s): S, prov: Option<Extension<ProvisionalPeer>>, peer: Option<Extension<TunnelPeer>>, body: axum::body::Bytes) -> Response {
    let Some(ch) = channel(&s, prov.as_deref(), peer.as_deref()) else { return unauthorized() };
    if let Some(p) = prov.as_deref() {
        if p.requests.fetch_add(1, Ordering::SeqCst) >= PROVISIONAL_MAX_REQUESTS {
            return rate_limited("this connection has sent as many requests as it may; enroll again");
        }
    }
    let b: SubmitBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => return bad_json(e),
    };
    let now = now_ms();
    // Steps 1 to 4 and the offer's workspace and expiry.
    let req = match box_check_request(&b.request, s.ws, ch.device(), now) {
        Ok(r) => r,
        Err(r) => return refusal(&r),
    };
    // Step 5: the roster.
    let account = {
        let svc = s.lock();
        let desktop = match svc.enroll_desktop(s.ws, req.desktop_device_id) {
            Ok(d) => d,
            Err(r) => return refusal(&r),
        };
        if let Err(r) = svc.enroll_phone_standing(s.ws, req.device_id(), Some(&req.code.public_key), desktop.account_id) {
            return refusal(&r);
        }
        desktop.account_id
    };
    if let Some(r) = relay_refusal(&s) {
        return r;
    }
    // Step 6: the limits.
    let entry = Entry {
        rid: req.rid,
        text: b.request,
        device: req.device_id(),
        public_key: req.code.public_key.clone(),
        desktop: req.desktop_device_id,
        account,
        exp: req.exp,
        channel: ch.key(),
        seq: 0,
        answer: None,
    };
    let evicted = match s.phone_enroll.lock().submit(entry, ch.cap(), now) {
        Ok(ev) => ev,
        Err(Full::RateLimited) => return rate_limited("this box is taking too many add-a-phone requests; try again in a minute"),
    };
    for e in evicted {
        note(format!("request {} from device {} evicted (limits)", e.rid, e.device));
    }
    note(format!("relaying request {} from device {} ({}) to desktop {}", req.rid, req.device_id(), ch.kind(), req.desktop_device_id));
    s.phone_enroll.kick.notify_one();
    (StatusCode::ACCEPTED, Json(json!({ "rid": req.rid, "state": "relayed" }))).into_response()
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StatusQuery {
    #[serde(default)]
    wait: Option<u64>,
    /// Off the tunnel: which phone's request, when one caller has several.
    #[serde(default)]
    device_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq)]
struct Snapshot {
    state: &'static str,
    answer: Option<String>,
    certified: bool,
}

fn snapshot(s: &Shared, rid: Rid, ch: &Channel, device: Option<Uuid>) -> Option<Snapshot> {
    let now = now_ms();
    let e = {
        let mut inner = s.phone_enroll.lock();
        inner.purge(now);
        let mine = |e: &&Entry| {
            e.rid == rid
                && match (ch.device(), device) {
                    (Some(d), _) => e.device == d,
                    (None, Some(d)) => e.device == d && e.channel == ch.key(),
                    (None, None) => e.channel == ch.key(),
                }
        };
        inner.entries.iter().filter(mine).max_by_key(|e| e.seq)?.clone()
    };
    let certified = s.lock().enroll_phone_standing(s.ws, e.device, Some(&e.public_key), e.account) == Ok(EnrollPhoneStanding::AlreadyMine);
    let state = if e.answer.is_some() {
        "answered"
    } else if e.expired(now) {
        "expired"
    } else {
        "waiting"
    };
    Some(Snapshot { state, answer: e.answer, certified })
}

/// `GET /v1/phone-enroll/requests/{rid}?wait=N` (§7.4). While the request
/// waits, the call returns when an answer arrives, the offer expires, or
/// `certified` changes from what it was when the call began; at `N`
/// seconds (≤ 25) at the latest.
async fn status(
    State(s): S,
    prov: Option<Extension<ProvisionalPeer>>,
    peer: Option<Extension<TunnelPeer>>,
    Path(rid): Path<String>,
    Query(q): Query<StatusQuery>,
) -> Response {
    let Some(ch) = channel(&s, prov.as_deref(), peer.as_deref()) else { return unauthorized() };
    let not_found = || refuse(StatusCode::NOT_FOUND, "not_found", "no pending request with that id on this box; submit it again");
    let Some(rid) = Rid::from_b64u(&rid) else { return not_found() };
    let Some(first) = snapshot(&s, rid, &ch, q.device_id) else { return not_found() };
    let wait = Duration::from_secs(q.wait.unwrap_or(0).min(qr::MAX_WAIT_SECS));
    let deadline = tokio::time::Instant::now() + wait;
    let mut cur = first.clone();
    while cur.state == "waiting" && cur.certified == first.certified && tokio::time::Instant::now() < deadline {
        let changed = s.phone_enroll.changed.notified();
        tokio::select! {
            _ = changed => {}
            _ = tokio::time::sleep_until(deadline.min(tokio::time::Instant::now() + Duration::from_millis(500))) => {}
        }
        match snapshot(&s, rid, &ch, q.device_id) {
            Some(n) => cur = n,
            None => return not_found(),
        }
    }
    let mut v = json!({ "rid": rid, "state": cur.state, "certified": cur.certified });
    if let Some(a) = cur.answer {
        v["answer"] = json!(a);
    }
    Json(v).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(rid: u8, device: u128, channel: &str, text: &str) -> Entry {
        Entry {
            rid: Rid([rid; 16]),
            text: text.into(),
            device: Uuid::from_u128(device),
            public_key: vec![4; 65],
            desktop: Uuid::from_u128(1),
            account: Uuid::from_u128(2),
            exp: 4_000_000_000,
            channel: channel.into(),
            seq: 0,
            answer: None,
        }
    }

    /// T4 (pending flood, review C4/R5): one caller's flood evicts only its
    /// own requests; an honest phone on another channel always gets in, and
    /// stays in while the flood goes on.
    #[test]
    fn a_flood_evicts_itself_and_an_honest_phone_stays() {
        let mut inner = Inner::default();
        let mut now = 1_000_000;
        inner.submit(entry(9, 900, "tunnel:honest", "honest"), PER_PEER, now).unwrap();
        // Many provisional connections, each one request.
        for i in 0..200u128 {
            now += 2_100; // under 30 a minute
            let r = inner.submit(entry(i as u8, 1000 + i, &format!("provisional:{}", i % 5), &format!("flood-{i}")), PER_PEER, now);
            assert!(r.is_ok());
            assert!(inner.entries.len() <= MAX_PENDING);
            assert!(inner.entries.iter().any(|e| e.text == "honest"), "the honest phone was evicted at {i}");
            for c in 0..5 {
                assert!(inner.entries.iter().filter(|e| e.channel == format!("provisional:{c}")).count() <= PER_PEER);
            }
        }
        // A bearer caller: at most 2 pending.
        for i in 0..10u128 {
            now += 2_100;
            inner.submit(entry(50 + i as u8, 5000 + i, "bearer", &format!("bearer-{i}")), PER_BEARER, now).unwrap();
            assert!(inner.entries.iter().filter(|e| e.channel == "bearer").count() <= PER_BEARER);
        }
        // Global cap with many distinct callers: the heaviest is evicted.
        let mut inner = Inner::default();
        for i in 0..MAX_PENDING as u128 {
            inner.submit(entry(i as u8, 100 + i, &format!("tunnel:{i}"), &format!("t{i}")), PER_PEER, now + i as i64).unwrap();
        }
        inner.submit(entry(99, 9999, "bearer", "b1"), PER_BEARER, now + 100).unwrap();
        assert_eq!(inner.entries.len(), MAX_PENDING);
        assert!(inner.entries.iter().any(|e| e.text == "b1"));
    }

    #[test]
    fn rate_limit_idempotence_and_replacement() {
        let mut inner = Inner::default();
        let now = 10_000_000;
        inner.submit(entry(1, 1, "bearer", "a"), PER_BEARER, now).unwrap();
        // The same bytes again: idempotent, not counted.
        for _ in 0..100 {
            inner.submit(entry(1, 1, "bearer", "a"), PER_BEARER, now).unwrap();
        }
        assert_eq!((inner.entries.len(), inner.accepted.len()), (1, 1));
        // The same phone's fresh request replaces its old one.
        inner.submit(entry(2, 1, "bearer", "b"), PER_BEARER, now).unwrap();
        assert_eq!(inner.entries.len(), 1);
        assert_eq!(inner.entries[0].text, "b");
        assert!(inner.clear.contains(&Rid([1; 16])), "the old offer's slot is cleared");
        // 30 a minute.
        let mut inner = Inner::default();
        for i in 0..MAX_PER_MINUTE as u128 {
            inner.submit(entry(i as u8, 100 + i, &format!("tunnel:{i}"), &format!("r{i}")), PER_PEER, now).unwrap();
        }
        assert_eq!(inner.submit(entry(200, 999, "tunnel:x", "late"), PER_PEER, now + 1).unwrap_err(), Full::RateLimited);
        assert!(inner.submit(entry(200, 999, "tunnel:x", "late"), PER_PEER, now + 61_000).is_ok());
    }

    /// An answer slot counts only for the requests whose own desktop signed
    /// it (§5.5). A co-member who sends the box a request naming their own
    /// desktop for someone else's offer (the box can't check its MAC) must
    /// not get the box to take their desktop's answer for the other
    /// requests of that offer: an answer naming the real phone would stop
    /// the box relaying it, so the offer's desktop would never see the
    /// second phone (no abort, no tripwire), and the real desktop's own
    /// answer would be refused for the whole offer.
    #[test]
    fn an_answer_counts_only_for_requests_to_the_desktop_that_signed_it() {
        let mut inner = Inner::default();
        let now = 30_000_000;
        let rid = Rid([7; 16]);
        let (kx, ky) = (ligus_core::SigningKeypair::generate().unwrap(), ligus_core::SigningKeypair::generate().unwrap());
        // X is the co-member's desktop, Y the offer's real desktop.
        let (x, y) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let key = |d: Uuid| {
            if d == x {
                Some(kx.public_key_bytes().to_vec())
            } else if d == y {
                Some(ky.public_key_bytes().to_vec())
            } else {
                None
            }
        };
        let mut decoy = entry(7, 100, "tunnel:mallory", "decoy");
        decoy.desktop = x;
        let mut real = entry(7, 200, "tunnel:ana", "real phone");
        real.desktop = y;
        inner.submit(decoy, PER_PEER, now).unwrap();
        inner.submit(real, PER_PEER, now).unwrap();
        // X's answer about the real phone (its tag can't be right: X has no secret).
        let forged = qr::EnrollAnswer { rid, outcome: qr::Outcome::Declined, device_id: Uuid::from_u128(200), tag: [0; 32] };
        let signed = RelayedAnswer::new(&forged, x, now).sign(&kx);
        let taken = inner.take_answers(vec![(qr::answer_slot(rid), signed.clone())], now, key);
        assert_eq!(taken, vec![rid], "it answers the decoy, sent to X");
        let real = inner.entries.iter().find(|e| e.text == "real phone").unwrap();
        assert_eq!(real.answer, None, "X's answer must not answer a request for Y");
        let shown: HashSet<String> = (0..6).flat_map(|t| inner.plan(now + t * 4_000).0).map(|(_, text)| text).collect();
        assert!(shown.contains("real phone"), "the box still relays the real phone: {shown:?}");
        // Y's own answer is still taken for Y's request.
        let real_answer = qr::EnrollAnswer { rid, outcome: qr::Outcome::Certified, device_id: Uuid::from_u128(999), tag: [1; 32] };
        let ya = RelayedAnswer::new(&real_answer, y, now).sign(&ky);
        assert_eq!(inner.take_answers(vec![(qr::answer_slot(rid), ya.clone())], now, key), vec![rid]);
        let real = inner.entries.iter().find(|e| e.text == "real phone").unwrap();
        assert_eq!(real.answer.as_deref(), Some(ya.answer.as_str()));
        let decoy = inner.entries.iter().find(|e| e.text == "decoy").unwrap();
        assert_eq!(decoy.answer.as_deref(), Some(signed.answer.as_str()), "and not on X's");
        // An answer signed by a desktop no request names is ignored.
        let kz = ligus_core::SigningKeypair::generate().unwrap();
        let za = RelayedAnswer::new(&real_answer, Uuid::from_u128(3), now).sign(&kz);
        assert!(inner.take_answers(vec![(qr::answer_slot(rid), za)], now, |d| (d == Uuid::from_u128(3)).then(|| kz.public_key_bytes().to_vec())).is_empty());
    }

    /// Two phones on one offer share its one slot: it shows each in turn,
    /// and once the answer names one, only the other.
    #[test]
    fn two_phones_on_one_offer_take_turns_in_the_slot() {
        let mut inner = Inner::default();
        let now = 20_000_000;
        inner.submit(entry(7, 1, "tunnel:a", "first"), PER_PEER, now).unwrap();
        let (p, c) = inner.plan(now);
        assert_eq!((p.len(), c.len()), (1, 0));
        assert_eq!(p[0].1, "first");
        // Nothing new within 10 s.
        assert!(inner.plan(now + 1_000).0.is_empty());
        assert_eq!(inner.plan(now + 10_500).0[0].1, "first", "re-published every 10 s");
        inner.submit(entry(7, 2, "bearer", "second"), PER_BEARER, now + 11_000).unwrap();
        let mut seen = HashSet::new();
        for t in 0..10 {
            for (_, text) in inner.plan(now + 11_000 + t * 3_100).0 {
                seen.insert(text);
            }
        }
        assert_eq!(seen, ["first".to_string(), "second".to_string()].into_iter().collect());
        // The desktop answered about phone 1: only phone 2 is shown now.
        let keys = ligus_runtime::phone_enroll_qr::EnrollSecret::from_bytes([3; 32]).keys();
        let answer = EnrollAnswer::build(&keys, qr::Outcome::Certified, Uuid::from_u128(1)).encode();
        inner.set_answer(Rid([7; 16]), Uuid::from_u128(1), &answer);
        for t in 0..5 {
            for (_, text) in inner.plan(now + 60_000 + t * 3_100).0 {
                assert_eq!(text, "second");
            }
        }
        // Past exp: the slot is cleared.
        for e in inner.entries.iter_mut() {
            e.exp = 1;
        }
        let (p, c) = inner.plan(now + 70_000);
        assert!(p.is_empty());
        assert_eq!(c, vec![Rid([7; 16])]);
    }
}

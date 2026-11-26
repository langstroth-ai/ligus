//! "Add a phone" by showing a QR code the phone scans: the desktop side of
//! reverse-QR enrollment (`docs/design/v2/phone-enroll-reverse-qr.md` §8;
//! the codec is `ligus_runtime::phone_enroll_qr`).
//!
//! The offer lives here, in memory only: never persisted, never sent to the
//! web page as text (the page gets the QR as an SVG drawn here, and only
//! after **Show code**). While the QR shows, the window is content protected
//! (best effort: it keeps it out of screenshots and most screen sharing).
//!
//! A phone answers through a `ligus serve` box, which relays its request on
//! the relay's presence channel (`phone-enroll:<rid>:<box>:<phone tag>`, one
//! slot per request, sealed under the workspace key and signed by the box).
//! The page polls
//! [`phone_enroll_offer_poll`]; every request goes through the codec's checks
//! in the design's order (slot signature by a service member's device, then
//! `rid`, MAC, offer fields, expiry, phone signature, the phone's code), then
//! `phone_standing`, then the one-use lock ([`DesktopOffer::observe`]).
//! Approval holds for 5 s after the phone first appears, takes one last look
//! at presence itself (another phone there aborts, even if the page stopped
//! polling), then certifies with the person's passkey through the same
//! `certify_phone_code` as paste. After an answer the offer is handed to a
//! background watch that re-publishes the answer (`phone-enroll-answer:<rid>`, signed by this device) until the
//! offer's expiry and, after an approval, raises the tripwire
//! ([`TRIPWIRE_EVENT`]) when another phone answers the same code.
//!
//! Log hygiene (review R10): the offer text, its secret, `k_req`, `k_ans`,
//! the request and answer texts, MACs and the phone's code are never logged,
//! returned in an error or written to disk. Logs name an offer by its `rid`
//! only.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use ligus_core::ActorKind;
use ligus_runtime::keyring::KeyringEngine;
use ligus_runtime::phone_enroll::{PhoneCode, PhoneStanding};
use ligus_runtime::phone_enroll_qr::{
    answer_slot, check_relayed_request, open_relayed_requests, seal_relayed_answer, DesktopOffer, EnrollRequest, Outcome,
    RelayedAnswer, Rid, Seen,
};
use ligus_runtime::ChatService;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;

use crate::AppState;

/// Emitted when another phone answers a code after its phone was added
/// (the tripwire, design §8.3). Payload: [`TripwireDto`].
pub(crate) const TRIPWIRE_EVENT: &str = "phone-enroll://tripwire";

/// How often the background watch lists presence.
const WATCH_POLL_MS: u64 = 2_000;

/// Approval refused: another phone answered the code (the poll's `aborted`).
pub(crate) const TWO_PHONES: &str = "Two phones answered this code. Someone else may have seen it. Nothing was added. Start again.";

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---- what the page sees ----

/// Who the phone would act as, shown on the request card.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EnrollAccountDto {
    pub display_name: String,
    /// `Owner`, `Admin`, `Contributor`, `Viewer` (empty if unknown).
    pub role: String,
    pub workspace_name: String,
    /// Another current member here has the same display name: the phone
    /// warns about it too (review R4).
    pub name_collision: bool,
}

/// A shown offer: the QR as an SVG image (never its text).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OfferDto {
    pub qr_svg: String,
    pub rid: String,
    /// Unix seconds.
    pub expires_at: u64,
    pub account: EnrollAccountDto,
}

/// The phone that answered, as the request card shows it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EnrollPhoneDto {
    pub device_id: String,
    /// The name the phone gives itself (cleaned for display).
    pub label: String,
    pub fingerprint: String,
    /// `new`, `alreadyMine`, `revoked` or `conflict`.
    pub standing: String,
    pub problem: Option<String>,
    /// The box that relayed the request.
    pub via: String,
    /// One of your devices already has this name.
    pub duplicate_label: bool,
}

/// What a poll found.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PollDto {
    /// `none` (no offer), `waiting`, `request`, `aborted` (two phones) or `expired`.
    pub state: &'static str,
    pub rid: Option<String>,
    pub expires_at: Option<u64>,
    pub phone: Option<EnrollPhoneDto>,
    /// **Add this phone** is enabled once this reaches 0 (the hold).
    pub hold_remaining_ms: Option<i64>,
}

impl PollDto {
    fn bare(state: &'static str) -> Self {
        Self { state, rid: None, expires_at: None, phone: None, hold_remaining_ms: None }
    }
}

/// After **Add this phone**.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApprovedDto {
    pub device_id: String,
    pub label: String,
    /// The watch for other phones runs until then (Unix seconds).
    pub watching_until: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TripwirePhoneDto {
    pub device_id: String,
    pub label: String,
    pub fingerprint: String,
}

/// "Another phone answered this code after you added *label*."
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TripwireDto {
    pub rid: String,
    pub added: TripwirePhoneDto,
    pub other: TripwirePhoneDto,
}

/// Whether this desktop can finish an enrollment, before drawing a secret
/// (design §8.1, review R8).
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PrecheckDto {
    pub ready: bool,
    /// What's missing, in words; the panel opens on the paste flow then.
    /// The blocking reasons' texts (kept for older pages).
    pub missing: Vec<String>,
    /// Every finding, blocking or not, with a stable code the panel turns
    /// into a link to the fix ("No box reachable → Admit a box").
    pub reasons: Vec<PrecheckReason>,
}

/// One precheck finding.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PrecheckReason {
    /// `localWorkspace`, `noRelay`, `notYourDevice`, `noAccount`,
    /// `noPasskey`, `noBox` or `noBoxOnline`.
    pub code: &'static str,
    pub text: String,
    /// False for a warning that doesn't stop Show code (a box that hasn't
    /// been seen lately may only be slow to report in).
    pub blocking: bool,
}

impl PrecheckReason {
    fn blocking(code: &'static str, text: impl Into<String>) -> Self {
        Self { code, text: text.into(), blocking: true }
    }
}

impl PrecheckDto {
    pub(crate) fn from_reasons(reasons: Vec<PrecheckReason>) -> Self {
        let missing: Vec<String> = reasons.iter().filter(|r| r.blocking).map(|r| r.text.clone()).collect();
        Self { ready: missing.is_empty(), missing, reasons }
    }
}

/// Whether a box (an always-on `ligus serve` host) can carry the phone's
/// request to this desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BoxReach {
    /// At least one box was seen recently.
    Online,
    /// Boxes are registered but none was seen lately: the newest one's label
    /// and how many minutes ago it was seen.
    Quiet { label: String, minutes: i64 },
    /// The workspace has no box at all.
    None,
}

/// Where the workspace's boxes stand, from the host roster and presence.
pub(crate) fn box_reach(hosts: &[ligus_core::WorkspaceHost], online: impl Fn(&ligus_core::WorkspaceHost) -> bool, now_s: i64, last_seen_s: impl Fn(&ligus_core::WorkspaceHost) -> i64) -> BoxReach {
    let boxes: Vec<_> = hosts.iter().filter(|h| h.kind == ligus_core::HostKind::Worker).collect();
    if boxes.is_empty() {
        return BoxReach::None;
    }
    if boxes.iter().any(|h| online(h)) {
        return BoxReach::Online;
    }
    let newest = boxes.iter().max_by_key(|h| last_seen_s(h)).expect("non-empty");
    let label = crate::shown(&newest.label);
    BoxReach::Quiet {
        label: if label.trim().is_empty() { "your box".into() } else { label },
        minutes: ((now_s - last_seen_s(newest)) / 60).max(1),
    }
}

fn phone_dto(code: &PhoneCode, standing: &PhoneStanding, via: String, duplicate_label: bool) -> EnrollPhoneDto {
    let (standing, problem) = crate::phones_ui::standing_dto(standing.clone());
    EnrollPhoneDto {
        device_id: code.device_id.to_string(),
        label: crate::shown(&code.label),
        fingerprint: code.fingerprint(),
        standing,
        problem,
        via,
        duplicate_label,
    }
}

fn tripwire_phone(code: &PhoneCode) -> TripwirePhoneDto {
    TripwirePhoneDto { device_id: code.device_id.to_string(), label: crate::shown(&code.label), fingerprint: code.fingerprint() }
}

// ---- the QR ----

/// The QR as an SVG (black modules on white, a 4-module quiet zone). Drawn
/// here so the page never holds the offer as text.
pub(crate) fn qr_svg(text: &str) -> Result<String, String> {
    use qrcodegen::{QrCode, QrCodeEcc};
    let qr = QrCode::encode_text(text, QrCodeEcc::Medium).map_err(|_| "Couldn't draw the code.".to_string())?;
    const QUIET: i32 = 4;
    let n = qr.size();
    let dim = n + 2 * QUIET;
    let mut path = String::new();
    for y in 0..n {
        for x in 0..n {
            if qr.get_module(x, y) {
                let _ = write!(path, "M{},{}h1v1h-1z", x + QUIET, y + QUIET);
            }
        }
    }
    Ok(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {dim} {dim}\" shape-rendering=\"crispEdges\" aria-hidden=\"true\" focusable=\"false\"><rect width=\"{dim}\" height=\"{dim}\" fill=\"#fff\"/><path d=\"{path}\" fill=\"#000\"/></svg>"
    ))
}

// ---- who may relay a request (review R2) ----
//
// Only a box may sign a request slot: `ChatService::enroll_relay_key` (the
// box's module, `chat_service/phone_enroll_relay.rs`) returns a key only for
// a certified, unrevoked, non-phone device held by a current service member.

/// Every request for `offer` in a presence listing that passes the slot and
/// request checks, with the box that relayed it, in slot order. Refusals
/// are logged by `rid` and reason code only.
pub(crate) fn valid_requests(
    offer: &DesktopOffer,
    blobs: &HashMap<String, Value>,
    ring: &KeyringEngine,
    now_ms: i64,
    relay_key: impl Fn(Uuid) -> Option<Vec<u8>>,
) -> Vec<(Uuid, EnrollRequest)> {
    let prefix = offer.request_slot_prefix();
    let mut slots: Vec<_> = open_relayed_requests(blobs, ring).into_iter().filter(|(slot, _)| slot.starts_with(&prefix)).collect();
    slots.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = Vec::new();
    for (slot, relayed) in slots {
        if let Err(e) = check_relayed_request(&slot, &relayed, now_ms, &relay_key) {
            tracing::debug!(target: "identity", rid = %offer.rid(), reason = e.code(), "dropped an add-a-phone request slot");
            continue;
        }
        match offer.check_request(&relayed.request, now_ms) {
            Ok(req) => out.push((relayed.relay, req)),
            Err(e) => tracing::debug!(target: "identity", rid = %offer.rid(), reason = e.code(), "refused an add-a-phone request"),
        }
    }
    out
}

// ---- the offer on screen ----

struct Locked {
    request: EnrollRequest,
    standing: PhoneStanding,
    card: EnrollPhoneDto,
    first_seen_ms: i64,
}

struct Current {
    offer: DesktopOffer,
    ws: Uuid,
    account: EnrollAccountDto,
    /// The QR is on screen (the window is content protected).
    shown: bool,
    /// The phone the offer is locked to.
    phone: Option<Locked>,
    /// The second phone that aborted the offer.
    other: Option<EnrollRequest>,
    /// A passkey ceremony for this offer is in flight.
    approving: bool,
}

/// One look at a presence listing for the offer on screen: every valid
/// request, through the one-use lock. The first phone locks the offer;
/// another phone before approval aborts it.
#[allow(clippy::too_many_arguments)]
fn look(
    c: &mut Current,
    blobs: &HashMap<String, Value>,
    ring: &KeyringEngine,
    now_ms: i64,
    relay_key: impl Fn(Uuid) -> Option<Vec<u8>>,
    standing: impl Fn(&PhoneCode) -> Option<PhoneStanding>,
    via: impl Fn(Uuid) -> String,
    duplicate: impl Fn(&PhoneCode) -> bool,
) {
    for (relay, req) in valid_requests(&c.offer, blobs, ring, now_ms, &relay_key) {
        let Some(st) = standing(&req.code) else { continue };
        match c.offer.observe(&req, now_ms) {
            Ok(Seen::First) => {
                tracing::info!(target: "identity", rid = %c.offer.rid(), "a phone answered an add-a-phone code");
                let card = phone_dto(&req.code, &st, via(relay), duplicate(&req.code));
                c.phone = Some(Locked { request: req, standing: st, card, first_seen_ms: now_ms });
            }
            Ok(Seen::SamePhone) => {
                // The same phone again (re-published, or rescanned after a
                // restart): refresh its standing, keep when it first appeared.
                if let Some(l) = c.phone.as_mut() {
                    let (standing, problem) = crate::phones_ui::standing_dto(st.clone());
                    l.card.standing = standing;
                    l.card.problem = problem;
                    l.standing = st;
                }
            }
            Ok(Seen::TwoPhones) => {
                tracing::warn!(target: "identity", rid = %c.offer.rid(), "two phones answered one add-a-phone code; offer aborted");
                c.other = Some(req);
                break;
            }
            // Only after approval, and an approved offer isn't on screen.
            Ok(Seen::Tripwire) => {}
            Err(_) => break,
        }
    }
}

/// The desktop's add-a-phone state: at most one offer on screen. Answered
/// offers move to a [`Watch`].
#[derive(Default)]
pub(crate) struct EnrollDesk {
    current: Mutex<Option<Current>>,
}

impl EnrollDesk {
    /// A new offer for `ws`, shown at once (the page calls this from **Show
    /// code**). Replaces the offer on screen, if any.
    pub(crate) fn start(
        &self,
        ws: Uuid,
        desktop: Uuid,
        account: EnrollAccountDto,
        now_ms: i64,
        shield: &dyn Fn(bool),
    ) -> Result<OfferDto, String> {
        if self.current.lock().unwrap().as_ref().is_some_and(|c| c.approving) {
            return Err("Finish adding the phone first: your passkey is waiting.".into());
        }
        let offer = DesktopOffer::start(ws, desktop, now_ms).map_err(|_| "Couldn't draw a secret for the code.".to_string())?;
        let qr_svg = qr_svg(&offer.qr_text())?;
        let dto = OfferDto { qr_svg, rid: offer.rid().to_string(), expires_at: offer.expires_at(), account: account.clone() };
        tracing::info!(target: "identity", rid = %offer.rid(), "add-a-phone offer started");
        *self.current.lock().unwrap() = Some(Current { offer, ws, account, shown: true, phone: None, other: None, approving: false });
        shield(true);
        Ok(dto)
    }

    /// Show the offer's QR again (after it was hidden on blur).
    pub(crate) fn show(&self, now_ms: i64, shield: &dyn Fn(bool)) -> Result<OfferDto, String> {
        let mut cur = self.current.lock().unwrap();
        let c = cur.as_mut().ok_or("This code was closed. Show a new code.")?;
        if c.offer.is_over(now_ms) {
            *cur = None;
            shield(false);
            return Err("This code expired. Show a new code.".into());
        }
        let qr_svg = qr_svg(&c.offer.qr_text())?;
        c.shown = true;
        shield(true);
        Ok(OfferDto { qr_svg, rid: c.offer.rid().to_string(), expires_at: c.offer.expires_at(), account: c.account.clone() })
    }

    /// Hide the QR (the window lost focus, the panel closed); the offer stays.
    pub(crate) fn hide(&self, shield: &dyn Fn(bool)) {
        if let Some(c) = self.current.lock().unwrap().as_mut() {
            c.shown = false;
        }
        shield(false);
    }

    /// Drop the offer on screen (Cancel). A watch keeps running, and an
    /// offer whose passkey ceremony is in flight stays (its certificate may
    /// be written; the approval then hands it to a watch).
    pub(crate) fn cancel(&self, shield: &dyn Fn(bool)) {
        let mut cur = self.current.lock().unwrap();
        if cur.as_ref().is_some_and(|c| !c.approving) {
            if let Some(c) = cur.take() {
                tracing::info!(target: "identity", rid = %c.offer.rid(), "add-a-phone offer cancelled");
            }
        } else if let Some(c) = cur.as_mut() {
            c.shown = false;
        }
        shield(false);
    }

    /// The workspace of the offer on screen.
    pub(crate) fn current_ws(&self) -> Option<Uuid> {
        self.current.lock().unwrap().as_ref().map(|c| c.ws)
    }

    #[cfg(test)]
    fn is_shown(&self) -> bool {
        self.current.lock().unwrap().as_ref().is_some_and(|c| c.shown)
    }

    /// The offer's expiry passed: close it.
    pub(crate) fn expire_if_over(&self, now_ms: i64, shield: &dyn Fn(bool)) -> bool {
        let mut cur = self.current.lock().unwrap();
        if cur.as_ref().is_some_and(|c| c.offer.is_over(now_ms) && !c.approving) {
            *cur = None;
            shield(false);
            return true;
        }
        false
    }

    /// Run one poll's checks over a presence listing. `standing` is
    /// `phone_standing` (an `Err` drops the request), `via` names the box,
    /// `duplicate` tells whether one of your devices already has the name.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn poll(
        &self,
        blobs: &HashMap<String, Value>,
        ring: &KeyringEngine,
        now_ms: i64,
        relay_key: impl Fn(Uuid) -> Option<Vec<u8>>,
        standing: impl Fn(&PhoneCode) -> Option<PhoneStanding>,
        via: impl Fn(Uuid) -> String,
        duplicate: impl Fn(&PhoneCode) -> bool,
        shield: &dyn Fn(bool),
    ) -> PollDto {
        let mut cur = self.current.lock().unwrap();
        let Some(c) = cur.as_mut() else { return PollDto::bare("none") };
        if c.offer.is_aborted() {
            return PollDto::bare("aborted");
        }
        if c.offer.is_over(now_ms) && !c.approving {
            *cur = None;
            shield(false);
            return PollDto::bare("expired");
        }
        look(c, blobs, ring, now_ms, relay_key, standing, via, duplicate);
        if c.offer.is_aborted() {
            if !c.approving {
                *cur = None;
            }
            shield(false);
            return PollDto::bare("aborted");
        }
        let rid = Some(c.offer.rid().to_string());
        let expires_at = Some(c.offer.expires_at());
        match &c.phone {
            None => PollDto { state: "waiting", rid, expires_at, phone: None, hold_remaining_ms: None },
            Some(l) => PollDto {
                state: "request",
                rid,
                expires_at,
                phone: Some(l.card.clone()),
                hold_remaining_ms: Some((l.first_seen_ms + ligus_runtime::phone_enroll_qr::HOLD_MS - now_ms).max(0)),
            },
        }
    }

    /// **Add this phone**, before the passkey. First one last look at the
    /// presence listing `blobs`, fetched for this call, exactly as a poll
    /// looks (`standing` is `phone_standing`): another phone there aborts the
    /// offer and refuses with [`TWO_PHONES`], so a page that stopped polling
    /// after the first sighting can't skip the two-phones abort. Then the
    /// hold, the lock, the fingerprint and the expiry (the codec's
    /// `check_approve`), and that the active workspace is still the offer's.
    /// Returns the code to certify and marks the approval in flight.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn begin_approve(
        &self,
        rid: &str,
        fingerprint: &str,
        active_ws: Uuid,
        now_ms: i64,
        blobs: &HashMap<String, Value>,
        ring: &KeyringEngine,
        relay_key: impl Fn(Uuid) -> Option<Vec<u8>>,
        standing: impl Fn(&PhoneCode) -> Option<PhoneStanding>,
    ) -> Result<(Uuid, PhoneCode), String> {
        let mut cur = self.current.lock().unwrap();
        let c = cur.as_mut().ok_or("This code was closed. Show a new code.")?;
        let rid = Rid::from_b64u(rid).ok_or("This is for another code.")?;
        if rid != c.offer.rid() {
            return Err("This is for another code.".into());
        }
        if !c.offer.is_aborted() {
            look(c, blobs, ring, now_ms, relay_key, standing, |_| "a serve box".into(), |_| false);
        }
        if c.offer.is_aborted() {
            // Kept (aborted) so the page's next poll shows the two-phones panel.
            tracing::warn!(target: "identity", rid = %c.offer.rid(), "approval refused: two phones answered one add-a-phone code");
            return Err(TWO_PHONES.into());
        }
        let code = c.offer.check_approve(rid, fingerprint.trim(), now_ms).map_err(|e| e.to_string())?.clone();
        crate::phones_ui::confirm_matches_preview(&code, fingerprint, &c.ws.to_string(), active_ws)?;
        if c.phone.as_ref().is_some_and(|l| !matches!(l.standing, PhoneStanding::New | PhoneStanding::AlreadyMine)) {
            return Err("This phone can't be added here.".into());
        }
        c.approving = true;
        Ok((c.ws, code))
    }

    /// The passkey ceremony failed or was cancelled: the offer stays.
    pub(crate) fn approve_failed(&self) {
        if let Some(c) = self.current.lock().unwrap().as_mut() {
            c.approving = false;
        }
    }

    /// The certificate was written: the offer leaves the screen for a
    /// watch until its expiry. If another phone answered while the passkey
    /// ran (the offer aborted), the watch starts with that phone's alert.
    pub(crate) fn finish_approve(&self, shield: &dyn Fn(bool)) -> Option<(Watch, Option<TripwireDto>)> {
        let c = self.current.lock().unwrap().take()?;
        shield(false);
        // Kept here, not read back from the offer: an aborted offer has no lock.
        let added = c.phone?.request;
        let mut offer = c.offer;
        offer.approved();
        let mut watch = Watch::new(offer, c.ws, Outcome::Certified, added, true);
        let alert = c.other.as_ref().and_then(|other| watch.alert_for(other));
        Some((watch, alert))
    }

    /// **Not my phone** (or **Close** on a phone that can't be added): the
    /// answer is `declined` (`refused` when its standing refuses it), and
    /// the offer leaves the screen. Returns the watch that publishes the
    /// answer until the offer's expiry, if a phone was locked.
    pub(crate) fn decline(&self, rid: &str, shield: &dyn Fn(bool)) -> Result<Option<Watch>, String> {
        let mut cur = self.current.lock().unwrap();
        let Some(c) = cur.as_ref() else { return Ok(None) };
        if Rid::from_b64u(rid) != Some(c.offer.rid()) {
            return Err("This is for another code.".into());
        }
        if c.approving {
            return Err("Wait for the passkey to finish.".into());
        }
        let c = cur.take().expect("checked");
        shield(false);
        let Some(locked) = c.phone else { return Ok(None) };
        let refused = matches!(locked.standing, PhoneStanding::Revoked | PhoneStanding::Conflict(_));
        let req = locked.request;
        tracing::info!(target: "identity", rid = %c.offer.rid(), refused, "declined an add-a-phone request");
        let outcome = if refused { Outcome::Refused } else { Outcome::Declined };
        Ok(Some(Watch::new(c.offer, c.ws, outcome, req, false)))
    }

    #[cfg(test)]
    fn hold_first_seen(&self) -> Option<i64> {
        self.current.lock().unwrap().as_ref().and_then(|c| c.phone.as_ref().map(|p| p.first_seen_ms))
    }
}

// ---- after the answer ----

/// An answered offer: its answer is re-published until the offer's expiry,
/// and after an approval every other phone that answers trips the alert
/// (design §8.3). In memory only.
pub(crate) struct Watch {
    offer: DesktopOffer,
    ws: Uuid,
    outcome: Outcome,
    added: EnrollRequest,
    tripwire: bool,
    alerted: HashSet<(Uuid, Vec<u8>)>,
    last_published_ms: Option<i64>,
}

impl Watch {
    fn new(offer: DesktopOffer, ws: Uuid, outcome: Outcome, added: EnrollRequest, tripwire: bool) -> Self {
        Self { offer, ws, outcome, added, tripwire, alerted: HashSet::new(), last_published_ms: None }
    }

    pub(crate) fn ws(&self) -> Uuid {
        self.ws
    }

    pub(crate) fn rid(&self) -> Rid {
        self.offer.rid()
    }

    pub(crate) fn expires_at(&self) -> u64 {
        self.offer.expires_at()
    }

    pub(crate) fn is_over(&self, now_ms: i64) -> bool {
        self.offer.is_over(now_ms)
    }

    pub(crate) fn added_code(&self) -> &PhoneCode {
        &self.added.code
    }

    fn alert_for(&mut self, other: &EnrollRequest) -> Option<TripwireDto> {
        if !self.tripwire || other.same_phone(&self.added) {
            return None;
        }
        if !self.alerted.insert((other.code.device_id, other.code.public_key.clone())) {
            return None;
        }
        tracing::warn!(target: "identity", rid = %self.offer.rid(), "another phone answered an add-a-phone code after its phone was added");
        Some(TripwireDto { rid: self.offer.rid().to_string(), added: tripwire_phone(&self.added.code), other: tripwire_phone(&other.code) })
    }

    /// One look at a presence listing: an alert for each new other phone.
    pub(crate) fn step(
        &mut self,
        blobs: &HashMap<String, Value>,
        ring: &KeyringEngine,
        now_ms: i64,
        relay_key: impl Fn(Uuid) -> Option<Vec<u8>>,
    ) -> Vec<TripwireDto> {
        if !self.tripwire || self.offer.is_over(now_ms) {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (_, req) in valid_requests(&self.offer, blobs, ring, now_ms, &relay_key) {
            let other = match self.offer.observe(&req, now_ms) {
                Ok(Seen::Tripwire) => true,
                Ok(_) => false,
                // An offer aborted during the passkey has no lock left: compare
                // with the phone that was added.
                Err(_) => !req.same_phone(&self.added),
            };
            if other {
                out.extend(self.alert_for(&req));
            }
        }
        out
    }

    /// The signed, sealed answer blob, when it is due (now, then every
    /// [`REPUBLISH_SECS`](ligus_runtime::phone_enroll_qr::REPUBLISH_SECS)).
    pub(crate) fn answer_due(
        &mut self,
        now_ms: i64,
        ring: &KeyringEngine,
        desktop: Uuid,
        sign: impl FnOnce(RelayedAnswer) -> RelayedAnswer,
    ) -> Option<(String, Value)> {
        if self.offer.is_over(now_ms) {
            return None;
        }
        let every = ligus_runtime::phone_enroll_qr::REPUBLISH_SECS as i64 * 1000;
        if self.last_published_ms.is_some_and(|t| now_ms - t < every) {
            return None;
        }
        let answer = self.offer.answer(self.outcome, self.added.code.device_id);
        let blob = seal_relayed_answer(ring, &sign(RelayedAnswer::new(&answer, desktop, now_ms)))?;
        self.last_published_ms = Some(now_ms);
        Some((answer_slot(self.offer.rid()), blob))
    }
}

// ---- the Tauri side ----

fn shield_for(app: &AppHandle) -> impl Fn(bool) + '_ {
    move |on| {
        // Best effort (review RR4): not every platform or capture path honours it.
        if let Some(w) = app.get_webview_window("main") {
            if let Err(e) = w.set_content_protected(on) {
                tracing::debug!(target: "identity", "content protection unavailable: {e}");
            }
        }
    }
}

/// The reasons this desktop can't finish an enrollment in `ws`, and the
/// warnings worth knowing first. Each names what's missing; the panel adds
/// the link to the fix.
pub(crate) fn precheck_reasons(svc: &ChatService, ws: Uuid, local: bool, presence: bool, boxes: &BoxReach) -> Vec<PrecheckReason> {
    let mut out = Vec::new();
    if local {
        out.push(PrecheckReason::blocking(
            "localWorkspace",
            "This workspace is only on this device. Phones join a workspace you share through a box: switch to a shared one.",
        ));
    }
    if !presence {
        out.push(PrecheckReason::blocking(
            "noRelay",
            "No relay connection, so the phone's answer can't reach this desktop.",
        ));
    }
    if svc.is_delegated() || svc.author().kind == ActorKind::Service {
        out.push(PrecheckReason::blocking(
            "notYourDevice",
            "This device acts for an account it doesn't hold. Add the phone from your own desktop.",
        ));
    }
    match svc.author().account_id {
        None => out.push(PrecheckReason::blocking("noAccount", "This device has no account yet. Sign in first.")),
        Some(a) if !local && svc.passkeys_of(ws, a).unwrap_or_default().is_empty() => out.push(PrecheckReason::blocking(
            "noPasskey",
            "No passkey or recovery key for your account here. Adding a phone is approved with one.",
        )),
        Some(_) => {}
    }
    if !local {
        match boxes {
            BoxReach::None => out.push(PrecheckReason::blocking(
                "noBox",
                "No box in this workspace. The phone reaches you through an always-on box (ligus serve).",
            )),
            // Without a relay, presence can't be read: don't guess.
            BoxReach::Quiet { label, minutes } if presence => out.push(PrecheckReason {
                code: "noBoxOnline",
                text: format!(
                    "No box reachable right now: {label} was last seen {minutes} min ago. The phone's request goes through it, so start it first."
                ),
                blocking: false,
            }),
            _ => {}
        }
    }
    out
}

fn account_for(svc: &ChatService, ws: Uuid, workspace_name: String) -> EnrollAccountDto {
    let me = svc.author();
    let mine = ligus_proto::names::clean_display_name(&me.display_name);
    let members = svc
        .load_in(ws, ligus_core::workspace_config_session_id(ws))
        .ok()
        .flatten()
        .map(|c| c.members)
        .unwrap_or_default();
    let role = members.iter().find(|m| m.id == me.id).map(|m| format!("{:?}", m.role)).unwrap_or_default();
    let name_collision = !mine.is_empty()
        && members.iter().any(|m| {
            m.id != me.id
                && m.actor.kind == ActorKind::Human
                && ligus_proto::names::clean_display_name(&m.actor.display_name).eq_ignore_ascii_case(&mine)
        });
    EnrollAccountDto { display_name: crate::shown(&me.display_name), role, workspace_name, name_collision }
}

fn workspace_name(state: &AppState, ws: Uuid) -> String {
    let s = state.settings.lock().unwrap();
    s.workspaces
        .iter()
        .find(|w| crate::room_workspace_id(&w.room) == ws)
        .map(|w| crate::shown(&w.name))
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "this workspace".into())
}

/// The active workspace's presence channel, when it is `ws`'s.
fn conn_for(state: &AppState, ws: Uuid) -> Option<(ligus_runtime::RelayClient, String, KeyringEngine)> {
    crate::active_presence_conn(state).filter(|(_, room, _)| crate::room_workspace_id(room) == ws)
}

/// [`box_reach`] for `ws`, with this app's host presence.
fn box_reach_for(state: &AppState, svc: &ChatService, ws: Uuid) -> BoxReach {
    let hosts = svc.list_hosts(ws).unwrap_or_default();
    let now = ligus_core::Timestamp::now();
    box_reach(
        &hosts,
        |h| state.host_presence.is_online(h, &now),
        now.inner().unix_timestamp(),
        |h| state.host_presence.last_seen(h).inner().unix_timestamp(),
    )
}

/// The precheck's reasons as (code, text, blocking), for the setup flow.
pub(crate) fn precheck_for(app: &AppHandle) -> Vec<(&'static str, String, bool)> {
    let state = app.state::<AppState>();
    let ws = state.active_workspace_id();
    let presence = conn_for(&state, ws).is_some();
    let local = ws == state.local_workspace_id;
    let svc = state.service.lock().unwrap();
    let boxes = box_reach_for(&state, &svc, ws);
    precheck_reasons(&svc, ws, local, presence, &boxes).into_iter().map(|r| (r.code, r.text, r.blocking)).collect()
}

/// Can this desktop add a phone by QR right now? Otherwise the panel opens
/// on the paste flow and says what's missing.
#[tauri::command]
pub(crate) async fn phone_enroll_precheck(ligus_app: AppHandle) -> Result<PrecheckDto, String> {
    crate::off_main(ligus_app, move |app| {
        let state = app.state::<AppState>();
        let ws = state.active_workspace_id();
        let presence = conn_for(&state, ws).is_some();
        let local = ws == state.local_workspace_id;
        let svc = state.service.lock().unwrap();
        let boxes = box_reach_for(&state, &svc, ws);
        Ok(PrecheckDto::from_reasons(precheck_reasons(&svc, ws, local, presence, &boxes)))
    })
    .await
}

/// **Show code**: a new offer for the active workspace, its QR as an SVG.
#[tauri::command]
pub(crate) async fn phone_enroll_offer_start(ligus_app: AppHandle) -> Result<OfferDto, String> {
    crate::off_main(ligus_app, move |app| {
        let state = app.state::<AppState>();
        let ws = state.active_workspace_id();
        let presence = conn_for(&state, ws).is_some();
        let local = ws == state.local_workspace_id;
        let name = workspace_name(&state, ws);
        let (account, desktop) = {
            let svc = state.service.lock().unwrap();
            let boxes = box_reach_for(&state, &svc, ws);
            if let Some(first) = precheck_reasons(&svc, ws, local, presence, &boxes).into_iter().find(|r| r.blocking) {
                return Err(first.text);
            }
            (account_for(&svc, ws, name), svc.device_id())
        };
        state.phone_enroll.start(ws, desktop, account, now_ms(), &shield_for(app))
    })
    .await
}

/// **Show code** again, after the QR was hidden.
#[tauri::command]
pub(crate) fn phone_enroll_offer_show(ligus_app: AppHandle) -> Result<OfferDto, String> {
    let state = ligus_app.state::<AppState>();
    state.phone_enroll.show(now_ms(), &shield_for(&ligus_app))
}

/// Hide the QR (the window lost focus); the offer stays.
#[tauri::command]
pub(crate) fn phone_enroll_offer_hide(ligus_app: AppHandle) {
    ligus_app.state::<AppState>().phone_enroll.hide(&shield_for(&ligus_app));
}

/// Close the offer on screen.
#[tauri::command]
pub(crate) fn phone_enroll_offer_cancel(ligus_app: AppHandle) {
    ligus_app.state::<AppState>().phone_enroll.cancel(&shield_for(&ligus_app));
}

/// Look for the phone's request (the page calls this every 2 s, one at a
/// time, while the panel is open).
#[tauri::command]
pub(crate) async fn phone_enroll_offer_poll(ligus_app: AppHandle) -> Result<PollDto, String> {
    let state = ligus_app.state::<AppState>();
    let Some(ws) = state.phone_enroll.current_ws() else { return Ok(PollDto::bare("none")) };
    if state.phone_enroll.expire_if_over(now_ms(), &shield_for(&ligus_app)) {
        return Ok(PollDto::bare("expired"));
    }
    let Some((relay, room, ring)) = conn_for(&state, ws) else {
        return Err("No relay connection for this code's workspace. Close this and open Add a phone again.".into());
    };
    let blobs = relay.list_presence(&room).await.map_err(|e| format!("Couldn't reach the relay: {e}"))?;
    crate::off_main(ligus_app, move |app| {
        let state = app.state::<AppState>();
        let phone_labels = state.settings.lock().unwrap().phone_labels.clone();
        let svc = state.service.lock().unwrap();
        let hosts = svc.list_hosts(ws).unwrap_or_default();
        let mine = crate::my_device_labels(&svc, ws, &phone_labels);
        Ok(state.phone_enroll.poll(
            &blobs,
            &ring,
            now_ms(),
            |d| svc.enroll_relay_key(ws, d),
            |code| svc.phone_standing(ws, code).ok(),
            |relay| {
                hosts
                    .iter()
                    .find(|h| h.id == relay.to_string())
                    .map(|h| crate::shown(&h.label))
                    .filter(|l| !l.trim().is_empty())
                    .unwrap_or_else(|| "a serve box".into())
            },
            |code| {
                let label = crate::shown(&code.label);
                !label.trim().is_empty()
                    && mine.iter().any(|(id, l)| *id != code.device_id && l.eq_ignore_ascii_case(&label))
            },
            &shield_for(app),
        ))
    })
    .await
}

/// **Add this phone**: after the hold, with the compared fingerprint, and
/// after a last presence look finds no other phone; certifies with the
/// person's passkey, answers `certified`, and keeps
/// watching for other phones until the offer expires.
#[tauri::command(async)]
pub(crate) async fn phone_enroll_offer_approve(ligus_app: AppHandle, rid: String, fingerprint: String) -> Result<ApprovedDto, String> {
    let app = ligus_app.clone();
    // A last look for other phones, right before certifying (fails closed:
    // no listing, no approval).
    let (ws, relay, room, ring) = {
        let state = ligus_app.state::<AppState>();
        let ws = state.phone_enroll.current_ws().ok_or("This code was closed. Show a new code.")?;
        let (relay, room, ring) = conn_for(&state, ws)
            .ok_or("No relay connection for this code's workspace, so Ligus can't check that no other phone answered it.")?;
        (ws, relay, room, ring)
    };
    let blobs = relay.list_presence(&room).await.map_err(|e| format!("Couldn't check that no other phone answered this code: {e}"))?;
    let (watch, alert) = crate::off_main(ligus_app, move |app| {
        let state = app.state::<AppState>();
        let (ws, code) = {
            let svc = state.service.lock().unwrap();
            state.phone_enroll.begin_approve(
                &rid,
                &fingerprint,
                state.active_workspace_id(),
                now_ms(),
                &blobs,
                &ring,
                |d| svc.enroll_relay_key(ws, d),
                |code| svc.phone_standing(ws, code).ok(),
            )?
        };
        let certified = {
            let mut svc = state.service.lock().unwrap();
            svc.certify_phone_code(ws, &code).map_err(|e| e.to_string())
        };
        let id = match certified {
            Ok(id) => id,
            Err(e) => {
                state.phone_enroll.approve_failed();
                return Err(e);
            }
        };
        tracing::info!(target: "identity", device = %id, label = %code.label, "certified a phone as this account's");
        crate::remember_phone_label(&state, id, &code.label);
        state.phone_enroll.finish_approve(&shield_for(app)).ok_or_else(|| "The code closed while you confirmed.".to_string())
    })
    .await?;
    let code = watch.added_code().clone();
    let dto = ApprovedDto { device_id: code.device_id.to_string(), label: crate::shown(&code.label), watching_until: watch.expires_at() };
    if let Some(a) = alert {
        let _ = app.emit(TRIPWIRE_EVENT, a);
    }
    spawn_watch(app, watch);
    Ok(dto)
}

/// **Not my phone**: answer `declined` (or `refused`) and close the offer.
#[tauri::command]
pub(crate) fn phone_enroll_offer_decline(ligus_app: AppHandle, rid: String) -> Result<(), String> {
    let state = ligus_app.state::<AppState>();
    if let Some(watch) = state.phone_enroll.decline(&rid, &shield_for(&ligus_app))? {
        spawn_watch(ligus_app.clone(), watch);
    }
    Ok(())
}

/// Re-publish an answer until the offer's expiry and, after an approval,
/// raise the tripwire; then clear the answer slot.
fn spawn_watch(app: AppHandle, watch: Watch) {
    let watch = Arc::new(Mutex::new(watch));
    tauri::async_runtime::spawn(async move {
        let (ws, rid) = {
            let w = watch.lock().unwrap();
            (w.ws(), w.rid())
        };
        loop {
            if watch.lock().unwrap().is_over(now_ms()) {
                break;
            }
            let state = app.state::<AppState>();
            if let Some((relay, room, ring)) = conn_for(&state, ws) {
                // Due answer, signed by this device.
                let due = {
                    let svc = state.service.lock().unwrap();
                    let desktop = svc.device_id();
                    watch.lock().unwrap().answer_due(now_ms(), &ring, desktop, |a| svc.sign_relayed_answer(a))
                };
                if let Some((slot, blob)) = due {
                    if let Err(e) = relay.publish_presence(&room, &slot, &blob).await {
                        tracing::debug!(target: "identity", rid = %rid, "add-a-phone answer publish failed: {e}");
                    }
                }
                if let Ok(blobs) = relay.list_presence(&room).await {
                    let alerts = {
                        let svc = state.service.lock().unwrap();
                        watch.lock().unwrap().step(&blobs, &ring, now_ms(), |d| svc.enroll_relay_key(ws, d))
                    };
                    for a in alerts {
                        let _ = app.emit(TRIPWIRE_EVENT, a);
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(WATCH_POLL_MS)).await;
        }
        let state = app.state::<AppState>();
        if let Some((relay, room, _)) = conn_for(&state, ws) {
            let _ = relay.publish_presence(&room, &answer_slot(rid), &serde_json::json!({})).await;
        }
        tracing::info!(target: "identity", rid = %rid, "add-a-phone offer closed");
    });
}

#[cfg(test)]
mod tests;

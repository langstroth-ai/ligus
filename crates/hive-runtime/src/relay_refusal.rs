//! Relay refusals: what a `429` or `507` from the relay asks this client to do.
//!
//! Every `429` and `507` a relay sends (v0.3 and later) carries a
//! `Retry-After` header in whole seconds and a JSON body
//! `{"error": "<code>", "message": "<text>", "retryAfter": <secs>}`. The
//! `error` code is the contract (`message` is for people and may change), so
//! a client switches on it and treats an unknown code by its status. See the
//! relay README, "Refusals: 429 and 507".
//!
//! | Class | Codes | What the client does |
//! |---|---|---|
//! | [`RefusalClass::RateLimited`] | `ip_rate`, `room_rate`, `tenant_rate`, `write_rate`, `read_rate`, `room_writer_rate`, `contact_*_rate`, `rate_limited`, any other `429` | wait `Retry-After`, retry the same request (same body, same idempotency key); the outbox is kept |
//! | [`RefusalClass::StreamLimit`] | `sse_streams` | back the event stream off until `Retry-After` (polling meanwhile) |
//! | [`RefusalClass::PairLockout`] | `pair_lockout` | send no pairing-code lookup to that relay until `Retry-After` |
//! | [`RefusalClass::ClientQuota`] | `client_rooms`, `too_many_pending` | no retry right away: tell the user; a write pause rechecks once per [`CLIENT_QUOTA_RECHECK`] (at least), so it can't last until a restart |
//! | [`RefusalClass::RoomFull`] | `507` `room_bytes`, `room_envelopes`, `keyring` | pause writes to that room only ("workspace full"); reads go on; one try per `Retry-After` |
//! | [`RefusalClass::OrgQuota`] | `507` `tenant_bytes`, `tenant_rooms` | as `RoomFull`, worded "organisation quota reached" |
//! | [`RefusalClass::RelayFull`] | `507` `store_bytes`, `total_bytes`, `total_rooms`, `contacts_full`, any other `507` | back off until `Retry-After` |
//!
//! A refused write stored nothing, so retrying it later is always safe.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The longest `Retry-After` honoured. A relay asking for more is capped, so
/// a bad header can't park a client for days.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(24 * 3600);
/// The longest wait a single request sleeps in place before retrying a rate
/// limit. A longer `Retry-After` ends the request with the refusal, and the
/// caller (the sync loop) waits it out between rounds instead.
pub const MAX_INLINE_WAIT: Duration = Duration::from_secs(30);
/// The least a write pause for a client quota lasts before one quiet
/// recheck.
pub const CLIENT_QUOTA_RECHECK: Duration = Duration::from_secs(3600);
/// How many times one request is retried in place after a rate limit.
pub const MAX_INLINE_RETRIES: u32 = 3;

/// What a refusal asks of the client (see the module table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefusalClass {
    RateLimited,
    StreamLimit,
    PairLockout,
    ClientQuota,
    RoomFull,
    OrgQuota,
    RelayFull,
}

impl RefusalClass {
    /// A refusal the client may retry by itself once `Retry-After` passed.
    pub fn auto_retry(self) -> bool {
        !matches!(self, RefusalClass::ClientQuota)
    }

    /// How long a write pause for this refusal lasts: its `Retry-After`,
    /// and for a refusal that isn't retried by itself at least
    /// [`CLIENT_QUOTA_RECHECK`]. Every pause ends: a single refusal (from a
    /// hostile or confused relay) never stops a room's writes until restart.
    pub fn write_pause(self, retry_after: Duration) -> Duration {
        if self.auto_retry() {
            retry_after
        } else {
            retry_after.max(CLIENT_QUOTA_RECHECK).min(MAX_RETRY_AFTER)
        }
    }

    /// A refusal of writes that pauses writing (to this room, this tenant,
    /// this relay) rather than ending the request with an error.
    pub fn pauses_writes(self) -> bool {
        matches!(
            self,
            RefusalClass::RoomFull | RefusalClass::OrgQuota | RefusalClass::RelayFull | RefusalClass::ClientQuota | RefusalClass::RateLimited
        )
    }
}

/// A `429` or `507` from the relay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayRefusal {
    pub status: u16,
    /// The body's `error` code; empty when the relay sent none (an older
    /// relay, a proxy).
    pub code: String,
    /// The body's `message` (for people; may change).
    pub message: String,
    /// `Retry-After` (header first, then the body's `retryAfter`), at least
    /// one second and at most [`MAX_RETRY_AFTER`].
    pub retry_after: Duration,
}

impl RelayRefusal {
    /// Whether `status` is a refusal this module handles.
    pub fn is_refusal_status(status: u16) -> bool {
        matches!(status, 429 | 507)
    }

    /// Parse a refusal from its status, `Retry-After` header and body. `None`
    /// for any other status.
    pub fn parse(status: u16, retry_after_header: Option<&str>, body: &[u8]) -> Option<Self> {
        if !Self::is_refusal_status(status) {
            return None;
        }
        let v: serde_json::Value = serde_json::from_slice(body).unwrap_or(serde_json::Value::Null);
        let text = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().chars().take(200).collect::<String>();
        let code: String = text("error").chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(64).collect();
        let header = retry_after_header.and_then(|h| h.trim().parse::<u64>().ok());
        let body_secs = v.get("retryAfter").and_then(|x| x.as_u64());
        let default = if status == 429 { 1 } else { 60 };
        let secs = header.or(body_secs).unwrap_or(default).max(1);
        let retry_after = Duration::from_secs(secs).min(MAX_RETRY_AFTER);
        Some(Self { status, code, message: text("message"), retry_after })
    }

    /// Read a refusal off a response (consumes the body). `Err(resp)` hands
    /// back any other response untouched.
    pub async fn from_response(resp: reqwest::Response) -> Result<Self, reqwest::Response> {
        let status = resp.status().as_u16();
        if !Self::is_refusal_status(status) {
            return Err(resp);
        }
        let header = resp.headers().get(reqwest::header::RETRY_AFTER).and_then(|h| h.to_str().ok()).map(str::to_owned);
        // A refusal body is small; never read more than 16 KiB of it.
        let mut resp = resp;
        let mut body = Vec::new();
        while let Ok(Some(chunk)) = resp.chunk().await {
            body.extend_from_slice(&chunk);
            if body.len() > 16 * 1024 {
                body.truncate(16 * 1024);
                break;
            }
        }
        Ok(Self::parse(status, header.as_deref(), &body).expect("a refusal status"))
    }

    /// What this refusal asks of the client. An unknown code goes by its
    /// status: a `429` is a rate limit, a `507` a full relay.
    pub fn class(&self) -> RefusalClass {
        match (self.status, self.code.as_str()) {
            (429, "sse_streams") => RefusalClass::StreamLimit,
            (429, "pair_lockout") => RefusalClass::PairLockout,
            (429, "client_rooms" | "too_many_pending") => RefusalClass::ClientQuota,
            (429, _) => RefusalClass::RateLimited,
            (507, "room_bytes" | "room_envelopes" | "keyring") => RefusalClass::RoomFull,
            (507, "tenant_bytes" | "tenant_rooms") => RefusalClass::OrgQuota,
            (_, _) => RefusalClass::RelayFull,
        }
    }

    /// The code as shown to a person (the status when the relay sent none).
    fn code_shown(&self) -> String {
        if self.code.is_empty() {
            format!("HTTP {}", self.status)
        } else {
            self.code.clone()
        }
    }

    /// A sentence for the person: what happened and what happens next.
    pub fn user_message(&self) -> String {
        let wait = human_duration(self.retry_after);
        let code = self.code_shown();
        match self.class() {
            RefusalClass::RateLimited => format!("the relay is rate limiting this device ({code}); retrying in {wait}"),
            RefusalClass::StreamLimit => format!("the relay has too many live streams open from this device ({code}); polling instead, retrying in {wait}"),
            RefusalClass::PairLockout => format!("too many wrong pairing codes were tried on this relay ({code}); try again in {wait}"),
            RefusalClass::ClientQuota if self.code == "too_many_pending" => {
                format!("the relay refused: too many pending requests from this account ({code}). Not retried right away; clear some first")
            }
            RefusalClass::ClientQuota => format!(
                "the relay refused: this device created as many workspaces as the relay allows ({code}). Not retried right away; it clears when one of them expires or is deleted"
            ),
            RefusalClass::RoomFull => format!(
                "workspace full: the relay is not taking new writes for this workspace ({code}). It stays readable; this device's new events wait here and are sent once there is room (next try in {wait})"
            ),
            RefusalClass::OrgQuota => format!(
                "organisation quota reached on the relay ({code}): ask your admin. New events wait on this device (next try in {wait})"
            ),
            RefusalClass::RelayFull => format!("the relay is full ({code}); new events wait on this device, retrying in {wait}"),
        }
    }
}

impl std::fmt::Display for RelayRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.user_message())
    }
}

/// `45 s`, `10 min`, `1 h`.
pub fn human_duration(d: Duration) -> String {
    let s = d.as_secs().max(1);
    if s < 120 {
        format!("{s} s")
    } else if s < 7200 {
        format!("{} min", s.div_ceil(60))
    } else {
        format!("{} h", s.div_ceil(3600))
    }
}

/// Pairing-code lockouts by relay base URL (process-wide: every client for a
/// relay shares it, since the relay locks out the address, not a client).
fn lockouts() -> &'static Mutex<HashMap<String, Instant>> {
    static L: std::sync::OnceLock<Mutex<HashMap<String, Instant>>> = std::sync::OnceLock::new();
    L.get_or_init(Default::default)
}

/// Remember a `pair_lockout` from `base` until its `Retry-After` passes.
pub fn note_pair_lockout(base: &str, refusal: &RelayRefusal) {
    let mut l = lockouts().lock().unwrap_or_else(|e| e.into_inner());
    l.insert(base.to_string(), Instant::now() + refusal.retry_after);
}

/// The time left on `base`'s pairing lockout, if any.
pub fn pair_lockout_left(base: &str) -> Option<Duration> {
    let mut l = lockouts().lock().unwrap_or_else(|e| e.into_inner());
    let until = *l.get(base)?;
    let now = Instant::now();
    if until <= now {
        l.remove(base);
        return None;
    }
    Some(until - now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(status: u16, code: &str) -> RelayRefusal {
        let body = serde_json::json!({ "error": code, "message": "m", "retryAfter": 7 }).to_string();
        RelayRefusal::parse(status, Some("5"), body.as_bytes()).unwrap()
    }

    #[test]
    fn every_documented_code_has_its_class() {
        use RefusalClass::*;
        for c in [
            "ip_rate",
            "room_rate",
            "tenant_rate",
            "write_rate",
            "read_rate",
            "room_writer_rate",
            "contact_create_rate",
            "contact_write_rate",
            "contact_drop_rate",
            "rate_limited",
        ] {
            assert_eq!(r(429, c).class(), RateLimited, "{c}");
        }
        assert_eq!(r(429, "sse_streams").class(), StreamLimit);
        assert_eq!(r(429, "pair_lockout").class(), PairLockout);
        assert_eq!(r(429, "client_rooms").class(), ClientQuota);
        assert_eq!(r(429, "too_many_pending").class(), ClientQuota);
        for c in ["room_bytes", "room_envelopes", "keyring"] {
            assert_eq!(r(507, c).class(), RoomFull, "{c}");
        }
        for c in ["tenant_bytes", "tenant_rooms"] {
            assert_eq!(r(507, c).class(), OrgQuota, "{c}");
        }
        for c in ["store_bytes", "total_bytes", "total_rooms", "contacts_full"] {
            assert_eq!(r(507, c).class(), RelayFull, "{c}");
        }
        // Unknown codes go by status.
        assert_eq!(r(429, "brand_new_limit").class(), RateLimited);
        assert_eq!(r(507, "brand_new_quota").class(), RelayFull);
        assert_eq!(r(429, "").class(), RateLimited);
        assert!(!ClientQuota.auto_retry() && RoomFull.auto_retry());
    }

    #[test]
    fn retry_after_prefers_the_header_then_the_body_and_is_bounded() {
        assert_eq!(r(429, "ip_rate").retry_after, Duration::from_secs(5));
        let body = br#"{"error":"room_bytes","retryAfter":3600}"#;
        assert_eq!(RelayRefusal::parse(507, None, body).unwrap().retry_after, Duration::from_secs(3600));
        // Garbage header, no body: the status's default, never zero.
        assert_eq!(RelayRefusal::parse(429, Some("soon"), b"").unwrap().retry_after, Duration::from_secs(1));
        assert_eq!(RelayRefusal::parse(507, Some("0"), b"not json").unwrap().retry_after, Duration::from_secs(1));
        assert_eq!(RelayRefusal::parse(507, None, b"").unwrap().retry_after, Duration::from_secs(60));
        assert_eq!(RelayRefusal::parse(429, Some("99999999"), b"").unwrap().retry_after, MAX_RETRY_AFTER);
        assert!(RelayRefusal::parse(500, Some("5"), b"").is_none());
    }

    #[test]
    fn a_hostile_body_is_bounded_and_cleaned() {
        let long = "x".repeat(5000);
        let body = serde_json::json!({ "error": format!("room_bytes\u{202e}{long}"), "message": long }).to_string();
        let r = RelayRefusal::parse(507, None, body.as_bytes()).unwrap();
        assert!(r.code.len() <= 64 && r.code.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        assert!(r.message.chars().count() <= 200);
    }

    #[test]
    fn user_messages_name_the_situation() {
        assert!(r(507, "room_bytes").user_message().starts_with("workspace full"));
        assert!(r(507, "tenant_rooms").user_message().starts_with("organisation quota reached"));
        assert!(r(429, "client_rooms").user_message().contains("Not retried right away"));
        assert!(RelayRefusal::parse(507, None, b"").unwrap().user_message().contains("HTTP 507"));
    }

    #[test]
    fn a_pair_lockout_lasts_until_retry_after() {
        let base = format!("http://lockout-{}", uuid::Uuid::new_v4());
        assert!(pair_lockout_left(&base).is_none());
        note_pair_lockout(&base, &r(429, "pair_lockout"));
        let left = pair_lockout_left(&base).unwrap();
        assert!(left <= Duration::from_secs(5) && left > Duration::from_secs(3));
        // Another relay isn't locked.
        assert!(pair_lockout_left("http://other.example").is_none());
    }
}

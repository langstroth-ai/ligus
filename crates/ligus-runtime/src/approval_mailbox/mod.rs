//! Code-match approval over the relay's approval mailbox
//! (`docs/design/code-match-approval.md`; bytes in
//! `docs/design/code-match-wire.md`).
//!
//! A requester (a box asking to join, a computer asking to be one of a
//! person's devices, a phone) posts a sealed, signed request to the
//! workspace's mailbox. An approver (a desktop, a phone through its box, a
//! headless CLI) opens it, and the two run a commit-then-reveal exchange that
//! gives both screens the same 8-symbol code (`K7Q4 M9X2`). The person
//! confirms the match with their passkey, the approval is written to the log
//! exactly as today, and the existing grant (`ligusadmit1:` or
//! `ligusgrant1:`) travels back sealed through the mailbox.
//!
//! The pieces, in the order a flow uses them:
//!
//! * [`ApprovalAddress`] (`ligusaddr1:`): where requests go. Not a secret.
//! * [`MailboxClient`]: every relay route, with member auth
//!   (`X-Ligus-Mailbox-Auth`), the requester's read key and the put cap.
//!   [`MailboxError::exit_code`] maps failures onto the shared exit-code
//!   table (3: no mailbox routes, fall back to paste; 7: offline).
//! * [`Requester`]: post a request, poll, show the code once an approver
//!   opened it, then [`Requester::check_grant`] (design 6.3 checks 1 to 4)
//!   and, after syncing, [`LogApprovalCheck`] (check 5), then
//!   [`Requester::settle`] (delete on settle).
//! * [`list_live`] and [`ApproverSession`]: the approver's side, with the
//!   device-local [`SeenSet`].
//!
//! * [`PresenceRequester`] and [`PresenceApproval`]: the passkey hand-off
//!   (0.5), where a terminal asks its person's desktop to confirm one
//!   drafted event with the passkey.
//!
//! Pure functions (the code, the keys, the preimages) are in
//! [`ligus_core::match_code`].

mod address;
mod approver;
mod client;
mod envelope;
mod log_check;
mod notices;
mod presence_ask;
mod requester;
#[cfg(test)]
mod presence_tests;
#[cfg(test)]
mod tests;

pub use address::{AddressError, ApprovalAddress, ADDRESS_PREFIX};
pub use approver::{
    claim_view, end_request, list_live, ApproverError, ApproverIdentity, ApproverSession, Approvability, ApproverClassifier, ClaimView, EpochKey,
    LiveRequest, Listing, SeenClaim, SeenSet, ACK_WINDOW_MS, REVEAL_WAIT_MS, SEEN_FILE, SEEN_TTL_MS,
};
pub use client::{current_address, ListedRequest, MailboxAuth, MailboxClient, MailboxError, MailboxMsg, PutAck, MAX_MSG_BLOB, MAX_REQUEST_BLOB, MAX_WAIT_SECS};
pub use envelope::{
    hint, open_blob, seal_blob, AbortBody, AckBody, OfferBody, AbortReason, ApproverPerson, ClaimBody, MsgError, MsgType, OpenBody, Outcome, RequestBody, RequestSubject,
    ResultBody, RevealBody, Signer, Verified, VerifiedOpen, VerifiedRequest, CLOCK_SKEW_MS, MAX_HINT_CHARS,
};
pub use notices::{Notice, NoticeKind, Notices, NOTICES_FILE, NOTICE_TTL_MS};
pub use log_check::{retry_log_check, LogApproval, LogApprovalCheck, LogSubject, LogVerdict, LOG_CHECK_RETRY_MS};
pub use presence_ask::{
    check_presence_ask, presence_code, AskRate, PresenceAsk, PresenceApproval, PresenceOutcome, PresencePending, PresencePendingStore, PresenceRefusal,
    PresenceRequester, PresenceView, ASK_RATE_PER_HOUR, ASSERTION_PREFIX, MAX_DRAFT_BYTES, PRESENCE_PENDING_FILE, PRESENCE_PREFIX,
};
pub use requester::{
    CheckedGrant, ClaimedApprover, DeadLog, GrantError, GrantTarget, PendingEntry, PendingStore, Requester, RequesterError, RequesterEvent,
    RequestSpec, DEAD_FILE, DEAD_LIMIT, DEAD_WINDOW_MS, PENDING_FILE,
};

/// A code both screens show, with when it stops being valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchCode {
    /// The 40-bit value.
    pub code40: u64,
    /// `K7Q4 M9X2`.
    pub shown: String,
    /// `K7Q4M9X2` (what `--code` takes).
    pub compact: String,
    /// What a screen reader reads (`K, 7, Q, 4. M, 9, X, 2.`).
    pub spelled: String,
    /// Unix ms after which neither side shows or accepts it.
    pub expires_at: i64,
}

impl MatchCode {
    pub(crate) fn new(code40: u64, expires_at: i64) -> Self {
        use ligus_core::match_code as mc;
        Self { code40, shown: mc::encode(code40), compact: mc::encode_compact(code40), spelled: mc::spell(code40), expires_at }
    }

    /// Whether the code has expired at `now_ms`.
    pub fn expired(&self, now_ms: i64) -> bool {
        now_ms > self.expires_at
    }

    /// Whether `typed` (a person's input: case, spaces, I/L/O are lenient)
    /// is this code.
    pub fn matches_typed(&self, typed: &str) -> bool {
        ligus_core::match_code::decode(typed) == Some(self.code40)
    }
}

/// The clock every piece reads: unix ms. Tests inject their own.
pub type Clock = std::sync::Arc<dyn Fn() -> i64 + Send + Sync>;

/// The system clock.
pub fn system_clock() -> Clock {
    std::sync::Arc::new(|| {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
    })
}

pub(crate) fn rand_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("OS randomness");
    b
}

pub(crate) mod b64 {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;

    pub fn enc(b: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(b)
    }

    pub fn dec(s: &str) -> Option<Vec<u8>> {
        URL_SAFE_NO_PAD.decode(s.as_bytes()).ok()
    }

    /// Exactly `N` bytes of base64url.
    pub fn fixed<const N: usize>(s: &str) -> Option<[u8; N]> {
        dec(s)?.try_into().ok()
    }
}

/// Exactly `N` bytes of hex.
pub(crate) fn hex_fixed<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != 2 * N || s.bytes().any(|c| c.is_ascii_uppercase()) {
        return None;
    }
    ligus_core::match_code::unhex(s)?.try_into().ok()
}

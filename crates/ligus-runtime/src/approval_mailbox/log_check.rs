//! Design note 6.3 check 5: after syncing with the pinned founder, the
//! approval must be on the workspace log, signed by the device whose key
//! showed the code (`approverSigKey`), a certified and unrevoked device of an
//! Admin (a box) or of the person (a computer).
//!
//! The check needs the workspace's folded state, so it is a trait the
//! caller's service implements ([`crate::ChatService`] does). It is kept
//! narrow on purpose: a later redeem step (for example certifying the new
//! device on redeem) adds a [`LogSubject`] variant without changing the
//! callers of the others.

use std::future::Future;
use std::time::Duration;

use uuid::Uuid;

use crate::computer_link::OpenedGrant;

/// How long a requester retries check 5 while the approval syncs.
pub const LOG_CHECK_RETRY_MS: i64 = 2 * 60 * 1000;

/// What the approval answers.
#[non_exhaustive]
pub enum LogSubject<'a> {
    /// A box admission: the invite the opened admission names, and this
    /// box's device key the admission must be bound to.
    Box {
        invite_id: Uuid,
        device_key: &'a [u8],
    },
    /// A computer link: the opened grant (its certificate for this device).
    Computer { grant: &'a OpenedGrant },
}

/// Check 5's inputs: the address's pins and the `open`'s approver.
pub struct LogApproval<'a> {
    pub workspace_id: Uuid,
    /// The address's founder pin (32 lowercase hex).
    pub founder_pin: &'a str,
    pub approver_device_id: Uuid,
    pub approver_sig_key: &'a [u8],
    pub subject: LogSubject<'a>,
    pub now_ms: i64,
}

/// Check 5's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogVerdict {
    /// On the log as required. `account_id` and `name` are the verified
    /// approver (check 6 shows "Link this computer as Ana (verified on the
    /// workspace log)?").
    Verified { account_id: Uuid, name: String },
    /// Not there yet: sync and retry (up to [`LOG_CHECK_RETRY_MS`]).
    NotYet(String),
    /// There, but not as required: redeem nothing.
    Refused(String),
}

/// The folded workspace's answer to check 5.
pub trait LogApprovalCheck {
    fn check_approval_on_log(&self, approval: &LogApproval<'_>) -> LogVerdict;
}

/// Run `attempt` (a sync, then check 5) until it is
/// [`LogVerdict::Verified`] or [`LogVerdict::Refused`], or `budget` passed
/// (then the last `NotYet`: "the approval wasn't found on the workspace
/// log", exit 1).
pub async fn retry_log_check<F, Fut>(
    mut attempt: F,
    budget: Duration,
    interval: Duration,
) -> LogVerdict
where
    F: FnMut() -> Fut,
    Fut: Future<Output = LogVerdict>,
{
    let start = tokio::time::Instant::now();
    loop {
        let v = attempt().await;
        if !matches!(v, LogVerdict::NotYet(_)) || start.elapsed() + interval > budget {
            return v;
        }
        tokio::time::sleep(interval).await;
    }
}

//! Device-local notices an approval leaves behind (design note 6.5; identity
//! model v2, 8.1): "Request from build-1 didn't match. Nothing was granted."
//! and "Linking build-1 didn't finish. Nothing was added." One row per
//! request, kept a day, in `approval-notices.json` (owner-only). Nothing here
//! goes on any log.

use std::path::{Path, PathBuf};

use ligus_core::match_code::MatchKind;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The notices file in a data dir.
pub const NOTICES_FILE: &str = "approval-notices.json";
/// How long a notice is shown.
pub const NOTICE_TTL_MS: i64 = 24 * 60 * 60 * 1000;

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NoticeKind {
    /// The codes didn't match (this device's own mismatch, or an abort
    /// signed by a named workspace device).
    Mismatch,
    /// A computer never acknowledged the offer within 2 minutes.
    DidntFinish,
    /// The person said Not me to a passkey hand-off from one of their
    /// devices: nothing was written, and the Inbox offers to revoke it.
    NotMe,
}

/// One notice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    pub kind: NoticeKind,
    pub workspace_id: Uuid,
    /// The request's rid (hex).
    pub rid: String,
    pub request_kind: MatchKind,
    /// The request's label (a hint).
    pub label: String,
    /// Unix ms.
    pub at: i64,
    /// Who ended it, when it wasn't this device: a workspace device's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// The device that asked (Not me: the one to revoke).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<Uuid>,
}

impl Notice {
    /// The Inbox line.
    pub fn text(&self) -> String {
        let label = super::envelope::hint(&self.label);
        match self.kind {
            NoticeKind::Mismatch => format!("Request from {label} didn't match. Nothing was granted."),
            NoticeKind::DidntFinish => format!("Linking {label} didn't finish. Nothing was added."),
            NoticeKind::NotMe => format!("You said Not me to a request from {label}. Nothing was written."),
        }
    }

    /// The "?" beside it.
    pub fn help(&self) -> &'static str {
        match self.kind {
            NoticeKind::Mismatch => {
                "The two screens showed different codes, so someone may have tried to intercept the request. Check the \
                 device you're adding and start again; tell an Admin if it happens twice."
            }
            NoticeKind::DidntFinish => {
                "The computer didn't confirm in time, so nothing was signed or published and no key left this device. \
                 Start again from the computer."
            }
            NoticeKind::NotMe => {
                "Not me revokes nothing by itself. If you didn't ask for this on that device, someone else may be using \
                 it: revoke it in My devices, and rotate the workspace key if it held one."
            }
        }
    }
}

/// `approval-notices.json` in a data dir.
#[derive(Debug, Clone)]
pub struct Notices {
    path: PathBuf,
}

impl Notices {
    pub fn new(data_dir: &Path) -> Self {
        Self { path: data_dir.join(NOTICES_FILE) }
    }

    fn load_all(&self) -> std::io::Result<Vec<Notice>> {
        match std::fs::read(&self.path) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// The notices younger than a day at `now_ms`, newest first.
    pub fn live(&self, now_ms: i64) -> std::io::Result<Vec<Notice>> {
        let mut all: Vec<Notice> = self.load_all()?.into_iter().filter(|n| now_ms.saturating_sub(n.at) <= NOTICE_TTL_MS).collect();
        all.sort_by_key(|n| std::cmp::Reverse(n.at));
        Ok(all)
    }

    /// Record `n` (one row per request: a later notice for the same rid
    /// replaces it), dropping rows older than a day.
    pub fn record(&self, n: Notice) -> std::io::Result<()> {
        let mut all = self.load_all()?;
        all.retain(|x| x.rid != n.rid && n.at.saturating_sub(x.at) <= NOTICE_TTL_MS);
        all.push(n);
        let bytes = serde_json::to_vec_pretty(&all).map_err(std::io::Error::other)?;
        crate::private_file::write_private_atomic(&self.path, &bytes)
    }

    /// Dismiss the notice for `rid`.
    pub fn dismiss(&self, rid: &str) -> std::io::Result<()> {
        let mut all = self.load_all()?;
        let before = all.len();
        all.retain(|x| x.rid != rid);
        if all.len() == before {
            return Ok(());
        }
        let bytes = serde_json::to_vec_pretty(&all).map_err(std::io::Error::other)?;
        crate::private_file::write_private_atomic(&self.path, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(kind: NoticeKind, rid: &str, at: i64) -> Notice {
        Notice { kind, workspace_id: Uuid::nil(), rid: rid.into(), request_kind: MatchKind::Box, label: "build-1".into(), at, by: None, device_id: None }
    }

    #[test]
    fn notices_keep_one_row_per_request_for_a_day() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Notices::new(tmp.path());
        s.record(n(NoticeKind::Mismatch, "a", 1_000)).unwrap();
        s.record(n(NoticeKind::Mismatch, "a", 2_000)).unwrap();
        s.record(n(NoticeKind::DidntFinish, "b", 3_000)).unwrap();
        let live = s.live(3_000).unwrap();
        assert_eq!(live.len(), 2);
        assert_eq!(live[0].text(), "Linking build-1 didn't finish. Nothing was added.");
        assert_eq!(live[1].text(), "Request from build-1 didn't match. Nothing was granted.");
        assert!(s.live(3_000 + NOTICE_TTL_MS + 2_000).unwrap().is_empty(), "a day later");
        let not_me = Notice { device_id: Some(Uuid::nil()), ..n(NoticeKind::NotMe, "c", 3_000) };
        assert_eq!(not_me.text(), "You said Not me to a request from build-1. Nothing was written.");
        assert!(not_me.help().contains("revokes nothing by itself"));
        s.dismiss("b").unwrap();
        assert_eq!(s.live(3_000).unwrap().len(), 1);
    }
}

//! The requester's side: post a request, wait for an approver, show the
//! code, check the grant (design note 6.3 checks 1 to 4), settle.

use std::path::{Path, PathBuf};

use ligus_core::crypto::SigAlg;
use ligus_core::e2ee::KeyAgreementKeypair;
use ligus_core::match_code::{self as mc, hex, MatchKind, Transcript};
use ligus_core::ActorIdentity;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::client::{MailboxAuth, MailboxClient, MailboxError};
use super::envelope::{
    grant_digest, open_blob, seal_blob, sign_message, verify_abort, verify_offer, verify_open,
    verify_result, AbortBody, AbortReason, AckBody, MsgError, MsgType, Outcome, RequestBody,
    ResultBody, RevealBody, Signer, Verified, VerifiedOpen,
};
use super::{b64, hex_fixed, rand_bytes, ApprovalAddress, MatchCode};
use crate::computer_link::{Grant, LinkRequest, OpenedGrant};
use crate::workspace_invite::WorkspaceConn;

/// The requester's pending state in a data dir (wire note 6).
pub const PENDING_FILE: &str = "approval-pending.json";
/// The requester's dead-request log in a data dir (design note 6.4).
pub const DEAD_FILE: &str = "approval-dead.json";
/// Dead requests within [`DEAD_WINDOW_MS`] after which a requester stops.
pub const DEAD_LIMIT: usize = 3;
pub const DEAD_WINDOW_MS: i64 = 15 * 60 * 1000;

/// One pending request (wire note 6). Nothing here is a workspace secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingEntry {
    pub rid: String,
    pub mailbox: String,
    pub relay: String,
    pub kind: MatchKind,
    pub n_r: String,
    pub read_key: String,
    pub created_at: i64,
    pub address_epoch: u32,
    pub workspace_id: Uuid,
    pub founder_pin: String,
    pub open_seen: bool,
}

fn load_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> std::io::Result<T> {
    match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e),
    }
}

fn save_json<T: Serialize>(path: &Path, v: &T) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(v).map_err(std::io::Error::other)?;
    crate::private_file::write_private_atomic(path, &bytes)
}

/// `approval-pending.json` in a data dir (owner-only).
#[derive(Debug, Clone)]
pub struct PendingStore {
    path: PathBuf,
}

impl PendingStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(PENDING_FILE),
        }
    }

    pub fn load(&self) -> std::io::Result<Vec<PendingEntry>> {
        load_json(&self.path)
    }

    /// Add or replace the entry for `e.rid`.
    pub fn upsert(&self, e: &PendingEntry) -> std::io::Result<()> {
        let mut all = self.load()?;
        all.retain(|x| x.rid != e.rid);
        all.push(e.clone());
        save_json(&self.path, &all)
    }

    /// Drop the entry for `rid` (the file goes when it empties).
    pub fn remove(&self, rid: &str) -> std::io::Result<()> {
        let mut all = self.load()?;
        let before = all.len();
        all.retain(|x| x.rid != rid);
        if all.is_empty() {
            return match std::fs::remove_file(&self.path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        if all.len() != before {
            save_json(&self.path, &all)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DeadRow {
    rid: String,
    at: i64,
}

/// `approval-dead.json`: the requests that ended without an approval, so a
/// requester stops after [`DEAD_LIMIT`] in [`DEAD_WINDOW_MS`].
#[derive(Debug, Clone)]
pub struct DeadLog {
    path: PathBuf,
}

impl DeadLog {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(DEAD_FILE),
        }
    }

    /// Dead requests in the window ending at `now_ms`.
    pub fn recent(&self, now_ms: i64) -> std::io::Result<usize> {
        let rows: Vec<DeadRow> = load_json(&self.path)?;
        Ok(rows
            .iter()
            .filter(|r| now_ms.saturating_sub(r.at) <= DEAD_WINDOW_MS)
            .count())
    }

    pub fn record(&self, rid: &str, now_ms: i64) -> std::io::Result<()> {
        let mut rows: Vec<DeadRow> = load_json(&self.path)?;
        rows.retain(|r| now_ms.saturating_sub(r.at) <= DEAD_WINDOW_MS && r.rid != rid);
        rows.push(DeadRow {
            rid: rid.to_string(),
            at: now_ms,
        });
        save_json(&self.path, &rows)
    }
}

/// Why the requester's side failed.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum RequesterError {
    #[error(transparent)]
    Mailbox(#[from] MailboxError),
    #[error("3 requests ended without an approval in the last 15 minutes: stopping. Check the device you're adding, start again on a trusted network, and tell an Admin if it keeps happening")]
    TooManyDead,
    #[error("the request's code doesn't match this device: {0}")]
    BadSpec(String),
    #[error("couldn't keep the pending request: {0}")]
    Io(String),
}

impl RequesterError {
    /// The shared exit code.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Mailbox(e) => e.exit_code(),
            Self::BadSpec(_) => 2,
            _ => 1,
        }
    }
}

fn io(e: std::io::Error) -> RequesterError {
    RequesterError::Io(e.to_string())
}

/// What a requester asks with.
pub struct RequestSpec {
    pub kind: MatchKind,
    /// The unchanged `ligusjoin1:`/`liguslink1:`/`ligusphone1:` text.
    pub code: String,
    /// The device key inside `code`.
    pub signer: Signer,
    /// The key-agreement key inside `code` (a box or a computer); `None` for
    /// a phone, which gets a fresh ephemeral one.
    pub ka: Option<KeyAgreementKeypair>,
}

/// Who an `open` says approved: **unverified** until the log says so
/// ("Says it's Ana's desktop (checked after approval)").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedApprover {
    pub device_id: Uuid,
    pub account_id: Uuid,
    pub name: String,
    pub alg: SigAlg,
    pub sig_key: Vec<u8>,
    /// The approving device's fingerprint.
    pub fingerprint: String,
}

/// What a poll saw, in order.
#[derive(Debug, Clone)]
pub enum RequesterEvent {
    /// The first valid `open`: the reveal went out and the code is ready.
    Opened {
        code: MatchCode,
        claimed: ClaimedApprover,
    },
    /// A computer link's offer (identity model v2, 8.1): the approver built
    /// the grant with this digest. Nothing is granted yet; the caller
    /// answers with [`Requester::ack`] (after its person confirmed the code).
    Offered {
        grant_digest: String,
        person_name: String,
    },
    /// The approver's signed result (any outcome). An approved one goes
    /// through [`Requester::check_grant`].
    Result(ResultBody),
    /// Someone ended the request (unauthenticated: a claim only).
    Aborted {
        reason: AbortReason,
        by_device: Uuid,
    },
    /// Five minutes passed since the open with no result.
    CodeExpired,
    /// Fifteen minutes passed with no open.
    RequestExpired,
}

/// A request in flight.
pub struct Requester {
    client: MailboxClient,
    address: ApprovalAddress,
    rid: [u8; 16],
    n_r: [u8; 32],
    read_key: [u8; 16],
    commit: [u8; 32],
    kind: MatchKind,
    code: String,
    ka: KeyAgreementKeypair,
    signer: Signer,
    created_at: i64,
    cursor: u64,
    open: Option<VerifiedOpen>,
    match_code: Option<MatchCode>,
    /// A computer's offered digest, and whether it was acknowledged.
    offered: Option<String>,
    acked: Option<String>,
    other_relay: bool,
    ended: bool,
    pending: Option<PendingStore>,
    dead: Option<DeadLog>,
}

impl std::fmt::Debug for Requester {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Requester")
            .field("rid", &hex(&self.rid))
            .field("kind", &self.kind)
            .field("ended", &self.ended)
            .finish()
    }
}

impl Requester {
    /// Post a request to `address`. With `data_dir`, the request is kept in
    /// [`PENDING_FILE`] until settled and the [`DEAD_FILE`] limit applies.
    pub async fn post(
        client: MailboxClient,
        address: &ApprovalAddress,
        spec: RequestSpec,
        data_dir: Option<&Path>,
    ) -> Result<Self, RequesterError> {
        let now = client.now();
        let dead = data_dir.map(DeadLog::new);
        if let Some(d) = &dead {
            if d.recent(now).map_err(io)? >= DEAD_LIMIT {
                return Err(RequesterError::TooManyDead);
            }
        }
        let subject = super::envelope::RequestSubject::decode(spec.kind, &spec.code, now)
            .map_err(|e| RequesterError::BadSpec(e.to_string()))?;
        if subject.device_key() != spec.signer.public_key().as_slice() {
            return Err(RequesterError::BadSpec(
                "the signer isn't the code's device key".into(),
            ));
        }
        let ka = match (spec.kind, spec.ka) {
            (MatchKind::Phone, None) => KeyAgreementKeypair::generate()
                .map_err(|e| RequesterError::BadSpec(e.to_string()))?,
            (MatchKind::Phone, Some(k)) => k,
            (_, Some(k)) => k,
            (_, None) => {
                return Err(RequesterError::BadSpec(
                    "a box or a computer uses its code's key-agreement key".into(),
                ))
            }
        };
        let rid: [u8; 16] = rand_bytes();
        let n_r: [u8; 32] = rand_bytes();
        let read_key: [u8; 16] = rand_bytes();
        let commit = mc::commit(&rid, &n_r);
        let body = RequestBody {
            v: 1,
            kind: spec.kind,
            rid: hex(&rid),
            mailbox: address.mailbox_hex(),
            code: spec.code.clone(),
            requester_ka: b64::enc(&ka.public_key_bytes()),
            read_key: b64::enc(&read_key),
            commit: b64::enc(&commit),
            created_at: now,
        };
        // The request must verify the way an approver checks it.
        let plain = sign_message(MsgType::Request, &body, &spec.signer);
        super::envelope::VerifiedRequest::verify(plain.as_bytes(), &rid, &address.mailbox, now)
            .map_err(|e| RequesterError::BadSpec(e.to_string()))?;
        let blob = seal_blob(&address.seal_key, plain.as_bytes())
            .map_err(|e| RequesterError::BadSpec(e.to_string()))?;
        let pending = data_dir.map(PendingStore::new);
        let entry = PendingEntry {
            rid: hex(&rid),
            mailbox: address.mailbox_hex(),
            relay: address.relay.clone(),
            kind: spec.kind,
            n_r: b64::enc(&n_r),
            read_key: b64::enc(&read_key),
            created_at: now,
            address_epoch: address.epoch,
            workspace_id: address.workspace_id,
            founder_pin: address.founder_pin.clone(),
            open_seen: false,
        };
        if let Some(p) = &pending {
            p.upsert(&entry).map_err(io)?;
        }
        if let Err(e) = client
            .put_request(&address.mailbox, &rid, &address.put_cap, &blob, &read_key)
            .await
        {
            if let Some(p) = &pending {
                let _ = p.remove(&entry.rid);
            }
            return Err(e.into());
        }
        Ok(Self {
            client,
            address: address.clone(),
            rid,
            n_r,
            read_key,
            commit,
            kind: spec.kind,
            code: spec.code,
            ka,
            signer: spec.signer,
            created_at: now,
            cursor: 0,
            open: None,
            match_code: None,
            offered: None,
            acked: None,
            other_relay: false,
            ended: false,
            pending,
            dead,
        })
    }

    pub fn rid(&self) -> [u8; 16] {
        self.rid
    }

    /// The approval address the request went to.
    pub fn address(&self) -> &ApprovalAddress {
        &self.address
    }

    pub fn rid_hex(&self) -> String {
        hex(&self.rid)
    }

    pub fn kind(&self) -> MatchKind {
        self.kind
    }

    /// The key-agreement key results are sealed to (a phone's ephemeral one).
    pub fn requester_ka(&self) -> &KeyAgreementKeypair {
        &self.ka
    }

    /// The code, once an approver opened the request.
    pub fn code(&self) -> Option<&MatchCode> {
        self.match_code.as_ref()
    }

    /// Who the accepted `open` says approved (unverified).
    pub fn claimed(&self) -> Option<ClaimedApprover> {
        self.open.as_ref().map(claimed)
    }

    /// Whether the request is over (approved, dead or expired).
    pub fn ended(&self) -> bool {
        self.ended
    }

    fn mark_dead(&mut self) {
        if !self.ended {
            self.ended = true;
            if let Some(d) = &self.dead {
                let _ = d.record(&hex(&self.rid), self.client.now());
            }
        }
    }

    /// Read new messages (long-polling up to `wait_secs`) and act on them:
    /// the first valid `open` gets the reveal and makes the code; later ones
    /// are ignored.
    pub async fn poll(&mut self, wait_secs: u64) -> Result<Vec<RequesterEvent>, RequesterError> {
        let mut out = Vec::new();
        if self.ended {
            return Ok(out);
        }
        let msgs = self
            .client
            .get_msgs(
                MailboxAuth::Requester(&self.read_key),
                &self.address.mailbox,
                &self.rid,
                self.cursor,
                wait_secs,
            )
            .await?;
        for m in msgs {
            self.cursor = self.cursor.max(m.seq);
            let Some(plain) = open_blob(&self.ka, &m.blob) else {
                continue;
            };
            let Ok(v) = Verified::parse(&plain) else {
                continue;
            };
            match v.t {
                MsgType::Open if self.open.is_none() => {
                    let Ok(open) = verify_open(&v, &self.rid) else {
                        continue;
                    };
                    self.accept_open(open, &mut out).await?;
                }
                MsgType::Offer if self.kind == MatchKind::Computer && self.offered.is_none() => {
                    let Some(open) = &self.open else { continue };
                    let Ok(o) = verify_offer(&v, &self.rid, open) else {
                        continue;
                    };
                    self.offered = Some(o.grant_digest.clone());
                    out.push(RequesterEvent::Offered {
                        grant_digest: o.grant_digest,
                        person_name: super::envelope::hint(&o.person.name),
                    });
                }
                MsgType::Result => {
                    let Some(open) = &self.open else { continue };
                    let Ok(r) =
                        verify_result(&v, &self.rid, open.approver_alg, &open.approver_sig_key)
                    else {
                        continue;
                    };
                    if r.outcome != Outcome::Approved {
                        self.mark_dead();
                    } else {
                        self.ended = true;
                    }
                    out.push(RequesterEvent::Result(r));
                    return Ok(out);
                }
                MsgType::Abort => {
                    let Ok(reason) = verify_abort(&v, &self.rid) else {
                        continue;
                    };
                    self.mark_dead();
                    out.push(RequesterEvent::Aborted {
                        reason,
                        by_device: v.signer_device_id(),
                    });
                    return Ok(out);
                }
                _ => {}
            }
        }
        let now = self.client.now();
        match &self.match_code {
            // An acknowledged offer waits for the approver's passkey.
            Some(_) if self.acked.is_some() => {}
            Some(c) if c.expired(now) => {
                self.mark_dead();
                out.push(RequesterEvent::CodeExpired);
            }
            None if now.saturating_sub(self.created_at) > mc::REQUEST_TTL_MS => {
                self.mark_dead();
                out.push(RequesterEvent::RequestExpired);
            }
            _ => {}
        }
        Ok(out)
    }

    async fn accept_open(
        &mut self,
        open: VerifiedOpen,
        out: &mut Vec<RequesterEvent>,
    ) -> Result<(), RequesterError> {
        let reveal = sign_message(
            MsgType::Reveal,
            &RevealBody {
                rid: hex(&self.rid),
                n_r: b64::enc(&self.n_r),
            },
            &self.signer,
        );
        let blob = seal_blob(&open.approver_ka, reveal.as_bytes())
            .map_err(|e| RequesterError::BadSpec(e.to_string()))?;
        self.client
            .post_msg(
                MailboxAuth::Requester(&self.read_key),
                &self.address.mailbox,
                &self.rid,
                &blob,
                "approver",
            )
            .await?;
        let t = Transcript {
            kind: self.kind,
            mailbox: &self.address.mailbox,
            rid: &self.rid,
            request_code: self.code.as_bytes(),
            requester_ka: &self.ka.public_key_bytes(),
            commit: &self.commit,
            approver_device_id: open.approver_device_id,
            approver_sig_key: &open.approver_sig_key,
            approver_ka: &open.approver_ka,
            n_a: &open.n_a,
        };
        let code = MatchCode::new(
            mc::code40(&t.digest(), &self.n_r),
            self.client.now().saturating_add(mc::CODE_TTL_MS),
        );
        if let Some(p) = &self.pending {
            if let Ok(mut all) = p.load() {
                if let Some(e) = all.iter_mut().find(|e| e.rid == hex(&self.rid)) {
                    e.open_seen = true;
                    let e = e.clone();
                    p.upsert(&e).map_err(io)?;
                }
            }
        }
        out.push(RequesterEvent::Opened {
            code: code.clone(),
            claimed: claimed(&open),
        });
        self.match_code = Some(code);
        self.open = Some(open);
        Ok(())
    }

    /// A computer acknowledges the offer the approver made: an `ack` of its
    /// digest, signed by this device and sealed to the approver. Call it once
    /// the person confirmed the code on this computer. The result must then
    /// carry a grant that hashes to this digest.
    pub async fn ack(&mut self) -> Result<(), RequesterError> {
        let (Some(open), Some(digest)) = (&self.open, &self.offered) else {
            return Err(RequesterError::BadSpec(
                "there is no offer to acknowledge".into(),
            ));
        };
        if self
            .match_code
            .as_ref()
            .is_none_or(|c| c.expired(self.client.now()))
        {
            return Err(RequesterError::BadSpec(
                "the code expired: start again".into(),
            ));
        }
        let msg = sign_message(
            MsgType::Ack,
            &AckBody {
                rid: hex(&self.rid),
                grant_digest: digest.clone(),
            },
            &self.signer,
        );
        let blob = seal_blob(&open.approver_ka, msg.as_bytes())
            .map_err(|e| RequesterError::BadSpec(e.to_string()))?;
        self.client
            .post_msg(
                MailboxAuth::Requester(&self.read_key),
                &self.address.mailbox,
                &self.rid,
                &blob,
                "approver",
            )
            .await?;
        self.acked = Some(digest.clone());
        Ok(())
    }

    /// Take a grant whose connection names another relay than the address
    /// (check 3 pins the relay otherwise). Only for an operator's explicit
    /// relay override (the CLI's `LIGUS_RELAY_URL`); check 5 and the redeem
    /// then use that override, never the grant's relay.
    pub fn allow_relay_override(&mut self, yes: bool) {
        self.other_relay = yes;
    }

    /// Tests: acknowledge `digest` without an offer (to reach checks 1 to 4).
    #[cfg(test)]
    pub(crate) fn set_acked_for_tests(&mut self, digest: &str) {
        self.acked = Some(digest.to_string());
    }

    /// End the request from this side (the person said the codes don't
    /// match, or cancelled): an abort to the members and, after an open, to
    /// the approver.
    pub async fn abort(&mut self, reason: AbortReason) -> Result<(), RequesterError> {
        let msg = sign_message(
            MsgType::Abort,
            &AbortBody {
                rid: hex(&self.rid),
                reason,
            },
            &self.signer,
        );
        self.mark_dead();
        let auth = MailboxAuth::Requester(&self.read_key);
        let to_members = seal_blob(&self.address.seal_key, msg.as_bytes())
            .map_err(|e| RequesterError::BadSpec(e.to_string()))?;
        self.client
            .post_msg(
                auth,
                &self.address.mailbox,
                &self.rid,
                &to_members,
                "members",
            )
            .await?;
        if let Some(open) = &self.open {
            let to_approver = seal_blob(&open.approver_ka, msg.as_bytes())
                .map_err(|e| RequesterError::BadSpec(e.to_string()))?;
            self.client
                .post_msg(
                    auth,
                    &self.address.mailbox,
                    &self.rid,
                    &to_approver,
                    "approver",
                )
                .await?;
        }
        Ok(())
    }

    /// The request is over (whatever the outcome): delete it and its
    /// messages from the relay (decision D1: the result holds a sealed grant)
    /// and drop the pending entry.
    pub async fn settle(&mut self) -> Result<(), RequesterError> {
        self.ended = true;
        let r = self
            .client
            .delete_request(&self.address.mailbox, &self.rid, &self.read_key)
            .await;
        if let Some(p) = &self.pending {
            p.remove(&hex(&self.rid)).map_err(io)?;
        }
        r.map_err(Into::into)
    }

    /// Check 5's inputs for a grant that passed checks 1 to 4
    /// ([`super::LogApprovalCheck`]); `device_key` is this device's signing key.
    pub fn log_approval<'a>(
        &'a self,
        checked: &'a CheckedGrant,
        device_key: &'a [u8],
        now_ms: i64,
    ) -> Option<super::LogApproval<'a>> {
        let open = self.open.as_ref()?;
        let subject = match checked {
            CheckedGrant::Box { invite_id, .. } => super::LogSubject::Box {
                invite_id: *invite_id,
                device_key,
            },
            CheckedGrant::Computer { grant, .. } => super::LogSubject::Computer { grant },
        };
        Some(super::LogApproval {
            workspace_id: self.address.workspace_id,
            founder_pin: &self.address.founder_pin,
            approver_device_id: open.approver_device_id,
            approver_sig_key: &open.approver_sig_key,
            subject,
            now_ms,
        })
    }

    /// Design note 6.3 checks 1 to 4 on an approved result, before anything
    /// is written. Check 5 (the approval on the log) follows after a sync
    /// ([`super::LogApprovalCheck`]).
    pub fn check_grant(
        &self,
        result: &ResultBody,
        target: GrantTarget<'_>,
        now_ms: i64,
    ) -> Result<CheckedGrant, GrantError> {
        let open = self.open.as_ref().ok_or(GrantError::NoOpen)?;
        if result.rid != hex(&self.rid) {
            return Err(GrantError::NotForThisRequest(
                "the result answers another request".into(),
            ));
        }
        if result.outcome != Outcome::Approved {
            return Err(GrantError::NotApproved(result.outcome));
        }
        if self.kind == MatchKind::Computer {
            // The grant must be the one this computer acknowledged.
            if self.acked.is_none() || result.grant_digest != self.acked {
                return Err(GrantError::NotAcknowledged);
            }
        } else if self.match_code.as_ref().is_none_or(|c| c.expired(now_ms)) {
            return Err(GrantError::CodeExpired);
        }
        check_grant(
            &self.address,
            self.kind,
            open,
            result,
            target,
            now_ms,
            self.other_relay,
        )
    }
}

fn claimed(open: &VerifiedOpen) -> ClaimedApprover {
    ClaimedApprover {
        device_id: open.approver_device_id,
        account_id: open.person.account_id,
        name: super::envelope::hint(&open.person.name),
        alg: open.approver_alg,
        sig_key: open.approver_sig_key.clone(),
        fingerprint: crate::device_enroll::fingerprint(&open.approver_sig_key),
    }
}

/// What the grant must answer.
pub enum GrantTarget<'a> {
    /// This box: its key-agreement key (the admission is sealed to it).
    Box { ka: &'a KeyAgreementKeypair },
    /// This computer: its link request, key-agreement key and device key.
    Computer {
        request: &'a LinkRequest,
        ka: &'a KeyAgreementKeypair,
        device_key: &'a [u8],
    },
}

/// A grant that passed checks 1 to 4. Holds workspace secrets: never log it.
pub enum CheckedGrant {
    /// The opened `hivews1:` admission (with its redeem key) and what it names.
    Box {
        invite: zeroize::Zeroizing<String>,
        invite_id: Uuid,
        conn: Box<WorkspaceConn>,
    },
    /// The opened computer grant and the person it names (unverified until
    /// check 5).
    Computer {
        grant: Box<OpenedGrant>,
        person: ActorIdentity,
    },
}

impl std::fmt::Debug for CheckedGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Box {
                invite_id, conn, ..
            } => write!(
                f,
                "CheckedGrant::Box(invite {invite_id}, workspace {})",
                conn.id()
            ),
            Self::Computer { grant, .. } => write!(
                f,
                "CheckedGrant::Computer(workspace {})",
                grant.workspace_id
            ),
        }
    }
}

/// Why a grant was refused (nothing is written).
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum GrantError {
    #[error("no approver opened this request")]
    NoOpen,
    #[error("the approver ended the request: {0:?}")]
    NotApproved(Outcome),
    #[error("the code expired before the approval arrived: start again")]
    CodeExpired,
    #[error("the grant isn't the one this computer acknowledged")]
    NotAcknowledged,
    #[error("the result carries no grant")]
    NoGrant,
    #[error("the grant's digest doesn't match the grant")]
    DigestMismatch,
    #[error("the grant wasn't issued by the device that showed the code")]
    WrongApprover,
    #[error("the grant doesn't answer this request: {0}")]
    NotForThisRequest(String),
    #[error("the grant is for another workspace than the approval address names")]
    WrongWorkspace,
    #[error("the grant names another founder than the approval address pins (a copy of the workspace, not the real one)")]
    WrongFounder,
    #[error("the grant's workspace keys are older than the approval address")]
    OldEpoch,
    #[error("the grant names another relay than the approval address")]
    WrongRelay,
}

fn newest_epoch(conn: &WorkspaceConn) -> u32 {
    conn.ek.iter().map(|k| k.v).max().unwrap_or(0)
}

/// The same relay: scheme, host (any case), port and path, ignoring a
/// trailing slash and the default port.
pub(crate) fn same_relay(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        url::Url::parse(s.trim()).ok().map(|u| {
            let port = u.port_or_known_default();
            (
                u.scheme().to_string(),
                u.host_str().map(str::to_ascii_lowercase),
                port,
                u.path().trim_end_matches('/').to_string(),
            )
        })
    };
    matches!((norm(a), norm(b)), (Some(x), Some(y)) if x == y)
}

fn check_conn(
    address: &ApprovalAddress,
    conn: &WorkspaceConn,
    other_relay: bool,
) -> Result<(), GrantError> {
    if conn.id() != address.workspace_id || conn.dm_account.is_some() {
        return Err(GrantError::WrongWorkspace);
    }
    // A grant names the relay its connection syncs with: check 5 reads the
    // log there, so it must be the address's relay.
    if !other_relay && !same_relay(&conn.relay_url, &address.relay) {
        return Err(GrantError::WrongRelay);
    }
    let fk = conn.fk.as_deref().map(|s| s.trim().to_ascii_lowercase());
    if fk.as_deref() != Some(address.founder_pin.as_str())
        || hex_fixed::<16>(&address.founder_pin).is_none()
    {
        return Err(GrantError::WrongFounder);
    }
    if newest_epoch(conn) < address.epoch {
        return Err(GrantError::OldEpoch);
    }
    Ok(())
}

/// Checks 1 to 4 of design note 6.3 (the result's signature by
/// `approverSigKey` was checked when it was read).
pub(crate) fn check_grant(
    address: &ApprovalAddress,
    kind: MatchKind,
    open: &VerifiedOpen,
    result: &ResultBody,
    target: GrantTarget<'_>,
    now_ms: i64,
    other_relay: bool,
) -> Result<CheckedGrant, GrantError> {
    let (Some(grant), Some(digest)) = (&result.grant, &result.grant_digest) else {
        return Err(GrantError::NoGrant);
    };
    // Check 1.
    if grant_digest(grant) != *digest {
        return Err(GrantError::DigestMismatch);
    }
    match (kind, target) {
        (MatchKind::Box, GrantTarget::Box { ka }) => {
            // Check 2: sealed to this box (a bearer code answers no request).
            if !grant
                .trim_start()
                .starts_with(crate::box_admission::SEALED_PREFIX)
            {
                return Err(GrantError::NotForThisRequest(
                    "a box answering a request takes only a sealed admission".into(),
                ));
            }
            let invite = zeroize::Zeroizing::new(
                crate::box_admission::open_admission(grant, ka)
                    .map_err(|e| GrantError::NotForThisRequest(e.to_string()))?,
            );
            let conn = crate::workspace_invite::decode_workspace_invite(&invite)
                .map_err(GrantError::NotForThisRequest)?;
            let (Some(invite_id), Some(_)) = (
                conn.invite_id(),
                crate::box_admission::redeem_key_of(&invite),
            ) else {
                return Err(GrantError::NotForThisRequest(
                    "that code isn't a box admission".into(),
                ));
            };
            // Check 3.
            check_conn(address, &conn, other_relay)?;
            Ok(CheckedGrant::Box {
                invite,
                invite_id,
                conn: Box::new(conn),
            })
        }
        (
            MatchKind::Computer,
            GrantTarget::Computer {
                request,
                ka,
                device_key,
            },
        ) => {
            let g =
                Grant::open(grant, ka).map_err(|e| GrantError::NotForThisRequest(e.to_string()))?;
            // Check 2: it names this request.
            if g.request != request.digest() {
                return Err(GrantError::NotForThisRequest(
                    "it names another link request".into(),
                ));
            }
            // Check 4: the approver key in the grant is the one that showed the code.
            if g.approver_key != open.approver_sig_key
                || g.approver_device_id != open.approver_device_id
            {
                return Err(GrantError::WrongApprover);
            }
            let person = g
                .check(device_key, now_ms)
                .map_err(|e| GrantError::NotForThisRequest(e.to_string()))?;
            // Check 3.
            if g.workspace_id != address.workspace_id {
                return Err(GrantError::WrongWorkspace);
            }
            check_conn(address, &g.conn, other_relay)?;
            Ok(CheckedGrant::Computer {
                grant: Box::new(g),
                person,
            })
        }
        _ => Err(GrantError::NotForThisRequest(
            "the grant is for another kind of request".into(),
        )),
    }
}

impl From<MsgError> for RequesterError {
    fn from(e: MsgError) -> Self {
        Self::BadSpec(e.to_string())
    }
}

//! The approver's side: list live requests, claim and open one, check the
//! reveal, show the code, post the result.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ligus_core::crypto::SigAlg;
use ligus_core::e2ee::KeyAgreementKeypair;
use ligus_core::match_code::{self as mc, hex, MailboxKeys, MatchKind, Transcript};
use uuid::Uuid;

use super::client::{MailboxAuth, MailboxClient, MailboxError};
use super::envelope::{
    grant_digest, open_blob, seal_blob, sign_message, verify_abort, verify_ack, verify_claim,
    verify_reveal, AbortBody, AbortReason, ApproverPerson, ClaimBody, MsgError, MsgType, OfferBody,
    OpenBody, Outcome, ResultBody, Signer, Verified, VerifiedRequest,
};
use super::{b64, hex_fixed, rand_bytes, MatchCode};

/// The approver's seen-set in a data dir.
pub const SEEN_FILE: &str = "approval-seen.json";
/// How long an approver remembers a `rid` it opened.
pub const SEEN_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// How long an approver waits for a computer's ack after its offer
/// (identity model v2, 8.1): past it, nothing is signed or published.
pub const ACK_WINDOW_MS: i64 = 2 * 60 * 1000;
/// How many epochs an approver polls: the current one and the previous two.
const EPOCHS_POLLED: usize = 3;

/// One epoch's workspace key (a secret: never log it).
#[derive(Clone)]
pub struct EpochKey {
    pub epoch: u32,
    pub key: [u8; 32],
}

impl std::fmt::Debug for EpochKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EpochKey({})", self.epoch)
    }
}

/// The `rid`s this device opened, per mailbox, for 24 hours (design 6.4).
#[derive(Debug, Clone, Default)]
pub struct SeenSet {
    path: Option<PathBuf>,
    seen: BTreeMap<String, BTreeMap<String, i64>>,
}

impl SeenSet {
    /// The seen-set kept in `data_dir` ([`SEEN_FILE`], owner-only).
    pub fn load(data_dir: &Path) -> std::io::Result<Self> {
        let path = data_dir.join(SEEN_FILE);
        let seen = match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e),
        };
        Ok(Self {
            path: Some(path),
            seen,
        })
    }

    /// A seen-set that lives in memory only (tests, a one-shot process).
    pub fn in_memory() -> Self {
        Self::default()
    }

    pub fn contains(&self, mailbox: &[u8; 16], rid: &[u8; 16], now_ms: i64) -> bool {
        self.seen
            .get(&hex(mailbox))
            .and_then(|m| m.get(&hex(rid)))
            .is_some_and(|at| now_ms.saturating_sub(*at) <= SEEN_TTL_MS)
    }

    /// Remember `rid` (and forget what is older than a day).
    pub fn mark(&mut self, mailbox: &[u8; 16], rid: &[u8; 16], now_ms: i64) -> std::io::Result<()> {
        for m in self.seen.values_mut() {
            m.retain(|_, at| now_ms.saturating_sub(*at) <= SEEN_TTL_MS);
        }
        self.seen.retain(|_, m| !m.is_empty());
        self.seen
            .entry(hex(mailbox))
            .or_default()
            .insert(hex(rid), now_ms);
        if let Some(p) = &self.path {
            let bytes = serde_json::to_vec(&self.seen).map_err(std::io::Error::other)?;
            crate::private_file::write_private_atomic(p, &bytes)?;
        }
        Ok(())
    }
}

/// A claim another (or this) device posted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenClaim {
    pub approver_device_id: Uuid,
    /// A display hint only.
    pub approver_name: String,
    pub at: i64,
    pub alg: SigAlg,
    pub key: Vec<u8>,
    pub seq: u64,
}

/// A live request an approver can open.
#[derive(Debug, Clone)]
pub struct LiveRequest {
    pub relay: String,
    pub epoch: u32,
    pub mailbox: [u8; 16],
    pub request: VerifiedRequest,
    pub put_at: i64,
    pub expires_at: i64,
    pub msg_count: u32,
    /// Valid claims, in order.
    pub claims: Vec<SeenClaim>,
    /// A message went to the requester (an `open`) before any valid claim.
    pub opened_without_claim: bool,
    /// Ended by an abort members can read: its reason and signing device.
    pub ended: Option<(AbortReason, Uuid)>,
    /// Messages to the requester (opens) before its first reveal.
    pub requester_msgs_before_reveal: usize,
    /// Whether the requester has revealed (to some approver).
    pub revealed: bool,
    /// When this listing was read (unix ms, this device's clock).
    pub listed_at: i64,
}

impl LiveRequest {
    pub fn kind(&self) -> MatchKind {
        self.request.kind
    }
    pub fn rid_hex(&self) -> String {
        self.request.rid_hex()
    }
    pub fn label(&self) -> String {
        self.request.subject.label()
    }
    pub fn fingerprint(&self) -> String {
        self.request.subject.fingerprint()
    }
    pub fn created_at(&self) -> i64 {
        self.request.created_at
    }
}

/// What [`list_live`] found.
#[derive(Debug, Clone, Default)]
pub struct Listing {
    pub live: Vec<LiveRequest>,
    /// Requests that didn't verify (rid hex, why): shown nowhere, counted.
    pub refused: Vec<(String, MsgError)>,
    /// Epochs whose mailbox isn't registered on this relay.
    pub unregistered: Vec<u32>,
}

/// The live requests in the mailboxes of the newest three of `epochs`
/// (design 5.1 Epochs), each verified (wire note 3.3), with the claims and
/// aborts members can read. A relay without mailbox routes is
/// [`MailboxError::NoRoutes`].
pub async fn list_live(
    client: &MailboxClient,
    epochs: &[EpochKey],
) -> Result<Listing, MailboxError> {
    let mut sorted: Vec<&EpochKey> = epochs.iter().collect();
    sorted.sort_by_key(|e| std::cmp::Reverse(e.epoch));
    sorted.dedup_by_key(|e| e.epoch);
    let mut out = Listing::default();
    for e in sorted.into_iter().take(EPOCHS_POLLED) {
        let keys = MailboxKeys::derive(&e.key);
        let mailbox = keys.mailbox();
        let rows = match client.list_requests(&keys).await {
            Ok(r) => r,
            Err(MailboxError::MailboxUnknown) => {
                out.unregistered.push(e.epoch);
                continue;
            }
            Err(err) => return Err(err),
        };
        let seal = keys.seal();
        for row in rows {
            let now = client.now();
            let Some(rid) = hex_fixed::<16>(&row.rid) else {
                continue;
            };
            let verified = open_blob(&seal, &row.blob)
                .ok_or(MsgError::Malformed)
                .and_then(|p| VerifiedRequest::verify(&p, &rid, &mailbox, now));
            let request = match verified {
                Ok(r) => r,
                Err(why) => {
                    out.refused.push((row.rid, why));
                    continue;
                }
            };
            let msgs = match client
                .get_msgs(MailboxAuth::Member(&keys), &mailbox, &rid, 0, 0)
                .await
            {
                Ok(m) => m,
                Err(MailboxError::RequestUnknown) => continue,
                Err(err) => return Err(err),
            };
            let mut claims = Vec::new();
            let mut ended = None;
            let mut first_to_requester = None;
            let mut before_reveal = 0usize;
            let mut revealed = false;
            for m in &msgs {
                if m.to == "requester" && first_to_requester.is_none() {
                    first_to_requester = Some(m.seq);
                }
                if m.to == "approver" && m.from == "requester" {
                    revealed = true;
                }
                if m.to == "requester" && !revealed {
                    before_reveal += 1;
                }
                let Some(v) = open_blob(&seal, &m.blob).and_then(|p| Verified::parse(&p).ok())
                else {
                    continue;
                };
                match v.t {
                    MsgType::Claim => {
                        if let Ok(c) = verify_claim(&v, &rid) {
                            claims.push(SeenClaim {
                                approver_device_id: c.approver_device_id,
                                approver_name: super::envelope::hint(&c.approver_name),
                                at: c.at,
                                alg: v.alg,
                                key: v.key.clone(),
                                seq: m.seq,
                            });
                        }
                    }
                    MsgType::Abort => {
                        if let Ok(reason) = verify_abort(&v, &rid) {
                            ended.get_or_insert((reason, v.signer_device_id()));
                        }
                    }
                    _ => {}
                }
            }
            let opened_without_claim =
                first_to_requester.is_some_and(|s| claims.first().is_none_or(|c| c.seq > s));
            out.live.push(LiveRequest {
                relay: client.relay().to_string(),
                epoch: e.epoch,
                mailbox,
                request,
                put_at: row.put_at,
                expires_at: row.expires_at,
                msg_count: row.msg_count,
                claims,
                opened_without_claim,
                ended,
                requester_msgs_before_reveal: before_reveal,
                revealed,
                listed_at: now,
            });
        }
    }
    Ok(out)
}

/// Whether a device can approve a kind of request in this workspace (an
/// Admin's device for a box; the person's own device for a computer or a
/// phone; certified and not revoked).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approvability {
    CanApprove,
    Cannot,
    Unknown,
}

/// The caller's answer to "can this device approve this kind?", from its
/// folded workspace state (design 6.4, the contested sheet).
pub trait ApproverClassifier {
    fn classify(&self, kind: MatchKind, device_id: Uuid, key: &[u8]) -> Approvability;
}

/// What an approver's sheet says about who is checking a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimView {
    /// Nobody claimed it yet.
    Unclaimed,
    /// This device claimed it.
    Mine,
    /// "Ben's desktop is checking this request".
    ClaimedBy { name: String, device_id: Uuid },
    /// "Opened by a device that can't approve this. It may be an attempt to
    /// intercept it." (`by`: the claim, when there was one.)
    Contested { by: Option<(String, Uuid)> },
}

/// How `live`'s claims read to the device `me`.
/// How long after its claim a claimer waits for the reveal before its own
/// view reads contested (someone else's open was taken).
pub const REVEAL_WAIT_MS: i64 = 30_000;

/// How `live`'s claims read to the device `me`. Contested when an open went
/// to the requester before any claim, when more messages went to the
/// requester before its reveal than there are claimers (an open with no
/// claim of its own raced in), when the first claimer can't approve this
/// kind, or, for this device's own claim, when no reveal came within
/// [`REVEAL_WAIT_MS`].
pub fn claim_view(live: &LiveRequest, me: Uuid, classifier: &dyn ApproverClassifier) -> ClaimView {
    if live.opened_without_claim {
        return ClaimView::Contested { by: None };
    }
    let Some(c) = live.claims.first() else {
        return ClaimView::Unclaimed;
    };
    let claimers: std::collections::BTreeSet<Uuid> =
        live.claims.iter().map(|c| c.approver_device_id).collect();
    if live.requester_msgs_before_reveal > claimers.len() {
        return ClaimView::Contested { by: None };
    }
    if c.approver_device_id == me {
        if !live.revealed && live.listed_at.saturating_sub(c.at) > REVEAL_WAIT_MS {
            return ClaimView::Contested { by: None };
        }
        return ClaimView::Mine;
    }
    match classifier.classify(live.request.kind, c.approver_device_id, &c.key) {
        Approvability::CanApprove => ClaimView::ClaimedBy {
            name: c.approver_name.clone(),
            device_id: c.approver_device_id,
        },
        _ => ClaimView::Contested {
            by: Some((c.approver_name.clone(), c.approver_device_id)),
        },
    }
}

/// Who approves: the device key (Ed25519 or a phone's P-256), the name a
/// claim shows, and the person the `open` names.
pub struct ApproverIdentity {
    pub signer: Signer,
    /// The device's display name (`Ana's desktop`), a hint only.
    pub device_name: String,
    pub person: ApproverPerson,
}

/// Why the approver's side failed.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum ApproverError {
    #[error(transparent)]
    Mailbox(#[from] MailboxError),
    #[error("this device already opened that request: a request can't be answered twice")]
    ReusedRid,
    #[error("this request is too old (over 15 minutes): ask for a new one")]
    RequestExpired,
    #[error("the requester's reveal doesn't match its commitment: the request was ended, nothing was granted")]
    RevealMismatch,
    #[error("the request was ended ({0:?}): nothing was granted")]
    Aborted(AbortReason),
    #[error("the code expired (5 minutes after it appeared): ask for a new request")]
    CodeExpired,
    #[error("there is no code yet: the requester hasn't answered")]
    NoCode,
    #[error("this request is already over")]
    Ended,
    /// No ack within [`ACK_WINDOW_MS`] of the offer: "Linking <label>
    /// didn't finish. Nothing was added."
    #[error("the computer didn't acknowledge in time: linking didn't finish, nothing was added")]
    DidntFinish,
    #[error("a computer link sends its grant only after an offer the computer acknowledged")]
    NotAcknowledged,
    #[error("the grant isn't the one offered")]
    GrantChanged,
    #[error("only a computer link makes an offer")]
    NotAComputer,
    #[error("couldn't keep the seen requests: {0}")]
    Io(String),
    #[error("{0}")]
    Msg(#[from] MsgError),
}

impl ApproverError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Mailbox(e) => e.exit_code(),
            _ => 1,
        }
    }
}

fn seal_to(recipient: &[u8; 32], msg: &str) -> Result<String, ApproverError> {
    Ok(seal_blob(recipient, msg.as_bytes())?)
}

/// End `request` without a session (the contested sheet's **End this
/// request**, or a stale one): an abort sealed to the members and one to
/// the requester.
pub async fn end_request(
    client: &MailboxClient,
    epoch: &EpochKey,
    request: &VerifiedRequest,
    signer: &Signer,
    reason: AbortReason,
) -> Result<(), ApproverError> {
    let keys = MailboxKeys::derive(&epoch.key);
    post_abort(client, &keys, request, signer, reason).await
}

async fn post_abort(
    client: &MailboxClient,
    keys: &MailboxKeys,
    request: &VerifiedRequest,
    signer: &Signer,
    reason: AbortReason,
) -> Result<(), ApproverError> {
    let msg = sign_message(
        MsgType::Abort,
        &AbortBody {
            rid: request.rid_hex(),
            reason,
        },
        signer,
    );
    let auth = MailboxAuth::Member(keys);
    let members = seal_to(&keys.seal().public_key_bytes(), &msg)?;
    client
        .post_msg(auth, &request.mailbox, &request.rid, &members, "members")
        .await?;
    let requester = seal_to(&request.requester_ka, &msg)?;
    client
        .post_msg(
            auth,
            &request.mailbox,
            &request.rid,
            &requester,
            "requester",
        )
        .await?;
    Ok(())
}

/// One approval in progress on this device.
pub struct ApproverSession {
    client: MailboxClient,
    keys: MailboxKeys,
    request: VerifiedRequest,
    signer: Signer,
    approver_device_id: Uuid,
    ka: KeyAgreementKeypair,
    n_a: [u8; 32],
    opened_at: i64,
    cursor: u64,
    code: Option<MatchCode>,
    ended: bool,
    person: ApproverPerson,
    offered: Option<Offered>,
    acked: bool,
}

/// The grant built and sealed at "Codes match", kept in memory until the ack.
struct Offered {
    grant: zeroize::Zeroizing<String>,
    digest: String,
    at: i64,
}

impl std::fmt::Debug for ApproverSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApproverSession")
            .field("rid", &self.request.rid_hex())
            .field("ended", &self.ended)
            .finish()
    }
}

impl ApproverSession {
    /// Claim `request` (sealed to the members) and open it (sealed to the
    /// requester) with a fresh `nA` and a fresh key-agreement key. Refuses a
    /// `rid` this device opened before or a request older than 15 minutes.
    pub async fn begin(
        client: MailboxClient,
        epoch: &EpochKey,
        request: &VerifiedRequest,
        me: ApproverIdentity,
        seen: &mut SeenSet,
    ) -> Result<Self, ApproverError> {
        let now = client.now();
        if seen.contains(&request.mailbox, &request.rid, now) {
            return Err(ApproverError::ReusedRid);
        }
        if now.saturating_sub(request.created_at) > mc::REQUEST_TTL_MS
            || request.created_at > now.saturating_add(super::CLOCK_SKEW_MS)
        {
            return Err(ApproverError::RequestExpired);
        }
        let keys = MailboxKeys::derive(&epoch.key);
        if keys.mailbox() != request.mailbox {
            return Err(ApproverError::Msg(MsgError::WrongMailbox));
        }
        seen.mark(&request.mailbox, &request.rid, now)
            .map_err(|e| ApproverError::Io(e.to_string()))?;
        let approver_device_id = me.signer.device_id();
        let ka = KeyAgreementKeypair::generate().map_err(|e| ApproverError::Io(e.to_string()))?;
        let n_a: [u8; 32] = rand_bytes();
        let auth = MailboxAuth::Member(&keys);
        let claim = sign_message(
            MsgType::Claim,
            &ClaimBody {
                rid: request.rid_hex(),
                approver_device_id,
                approver_name: me.device_name.clone(),
                at: now,
            },
            &me.signer,
        );
        client
            .post_msg(
                auth,
                &request.mailbox,
                &request.rid,
                &seal_to(&keys.seal().public_key_bytes(), &claim)?,
                "members",
            )
            .await?;
        let open = sign_message(
            MsgType::Open,
            &OpenBody {
                rid: request.rid_hex(),
                approver_device_id,
                approver_sig_key: b64::enc(&me.signer.public_key()),
                approver_ka: b64::enc(&ka.public_key_bytes()),
                approver_person: me.person.clone(),
                n_a: b64::enc(&n_a),
                at: now,
            },
            &me.signer,
        );
        client
            .post_msg(
                auth,
                &request.mailbox,
                &request.rid,
                &seal_to(&request.requester_ka, &open)?,
                "requester",
            )
            .await?;
        Ok(Self {
            client,
            keys,
            request: request.clone(),
            signer: me.signer,
            approver_device_id,
            ka,
            n_a,
            opened_at: now,
            cursor: 0,
            code: None,
            ended: false,
            person: me.person,
            offered: None,
            acked: false,
        })
    }

    pub fn request(&self) -> &VerifiedRequest {
        &self.request
    }

    /// The code, once the reveal arrived and checked.
    pub fn code(&self) -> Option<&MatchCode> {
        self.code.as_ref()
    }

    /// When the code stops being valid (5 minutes after the open).
    pub fn code_expires_at(&self) -> i64 {
        self.opened_at.saturating_add(mc::CODE_TTL_MS)
    }

    /// Wait (up to `wait_secs`) for the requester's reveal. Checks
    /// `commit == H(rid ‖ nR)` and aborts the request on a mismatch. Returns
    /// the code once it is known, `None` while waiting.
    pub async fn await_reveal(
        &mut self,
        wait_secs: u64,
    ) -> Result<Option<MatchCode>, ApproverError> {
        if self.ended {
            return Err(ApproverError::Ended);
        }
        if let Some(c) = &self.code {
            return Ok(Some(c.clone()));
        }
        if self.client.now() > self.code_expires_at() {
            self.expire().await?;
            return Err(ApproverError::CodeExpired);
        }
        let msgs = self
            .client
            .get_msgs(
                MailboxAuth::Member(&self.keys),
                &self.request.mailbox,
                &self.request.rid,
                self.cursor,
                wait_secs,
            )
            .await?;
        let seal = self.keys.seal();
        for m in msgs {
            self.cursor = self.cursor.max(m.seq);
            let plain = open_blob(&self.ka, &m.blob).or_else(|| open_blob(&seal, &m.blob));
            let Some(v) = plain.and_then(|p| Verified::parse(&p).ok()) else {
                continue;
            };
            match v.t {
                MsgType::Reveal => {
                    let Ok(n_r) = verify_reveal(
                        &v,
                        &self.request.rid,
                        self.request.signer_alg,
                        &self.request.signer_key,
                    ) else {
                        continue;
                    };
                    if mc::commit(&self.request.rid, &n_r) != self.request.commit {
                        self.ended = true;
                        post_abort(
                            &self.client,
                            &self.keys,
                            &self.request,
                            &self.signer,
                            AbortReason::Mismatch,
                        )
                        .await?;
                        return Err(ApproverError::RevealMismatch);
                    }
                    let code = MatchCode::new(
                        mc::code40(&self.transcript(), &n_r),
                        self.code_expires_at(),
                    );
                    self.code = Some(code.clone());
                    return Ok(Some(code));
                }
                MsgType::Abort => {
                    if let Ok(reason) = verify_abort(&v, &self.request.rid) {
                        self.ended = true;
                        return Err(ApproverError::Aborted(reason));
                    }
                }
                _ => {}
            }
        }
        Ok(None)
    }

    fn transcript(&self) -> [u8; 32] {
        Transcript {
            kind: self.request.kind,
            mailbox: &self.request.mailbox,
            rid: &self.request.rid,
            request_code: self.request.code.as_bytes(),
            requester_ka: &self.request.requester_ka,
            commit: &self.request.commit,
            approver_device_id: self.approver_device_id,
            approver_sig_key: &self.signer.public_key(),
            approver_ka: &self.ka.public_key_bytes(),
            n_a: &self.n_a,
        }
        .digest()
    }

    async fn post_result(
        &mut self,
        outcome: Outcome,
        grant: Option<&str>,
    ) -> Result<(), ApproverError> {
        let body = ResultBody {
            rid: self.request.rid_hex(),
            outcome,
            grant: grant.map(str::to_string),
            grant_digest: grant.map(grant_digest),
        };
        let msg = zeroize::Zeroizing::new(sign_message(MsgType::Result, &body, &self.signer));
        let blob = seal_to(&self.request.requester_ka, &msg)?;
        self.client
            .post_msg(
                MailboxAuth::Member(&self.keys),
                &self.request.mailbox,
                &self.request.rid,
                &blob,
                "requester",
            )
            .await?;
        self.ended = true;
        Ok(())
    }

    /// **Computers** (identity model v2, 8.1): the person chose Codes
    /// match. `grant` is the `ligusgrant1:` built and sealed now (before any
    /// passkey or publish); only its digest is sent, in a signed `offer`.
    /// The grant stays in memory until [`Self::finish_approved`]. If the
    /// workspace key rotates before that, [`Self::decline`] and start again.
    pub async fn offer(&mut self, grant: String) -> Result<(), ApproverError> {
        if self.ended {
            return Err(ApproverError::Ended);
        }
        if self.request.kind != MatchKind::Computer {
            return Err(ApproverError::NotAComputer);
        }
        if self.offered.is_some() {
            return Err(ApproverError::Ended);
        }
        let code = self.code.as_ref().ok_or(ApproverError::NoCode)?;
        let now = self.client.now();
        if code.expired(now) {
            self.expire().await?;
            return Err(ApproverError::CodeExpired);
        }
        let digest = grant_digest(&grant);
        let body = OfferBody {
            rid: self.request.rid_hex(),
            grant_digest: digest.clone(),
            person: self.person.clone(),
            approver_device_id: self.approver_device_id,
        };
        let msg = sign_message(MsgType::Offer, &body, &self.signer);
        let blob = seal_to(&self.request.requester_ka, &msg)?;
        self.client
            .post_msg(
                MailboxAuth::Member(&self.keys),
                &self.request.mailbox,
                &self.request.rid,
                &blob,
                "requester",
            )
            .await?;
        self.offered = Some(Offered {
            grant: zeroize::Zeroizing::new(grant),
            digest,
            at: now,
        });
        Ok(())
    }

    /// Wait (up to `wait_secs`) for the computer's ack of the offer, signed
    /// by the request's device key over the offered digest. `Ok(true)` once
    /// acknowledged: **only then** the caller runs the passkey ceremony and
    /// publishes the certificate, then calls [`Self::finish_approved`]. An
    /// ack by another key or for another digest is ignored. Past
    /// [`ACK_WINDOW_MS`] the grant is dropped, nothing is sent, and this is
    /// [`ApproverError::DidntFinish`].
    pub async fn await_ack(&mut self, wait_secs: u64) -> Result<bool, ApproverError> {
        if self.acked {
            return Ok(true);
        }
        if self.ended {
            return Err(ApproverError::Ended);
        }
        let Some(at) = self.offered.as_ref().map(|o| o.at) else {
            return Err(ApproverError::NotAcknowledged);
        };
        let deadline = at.saturating_add(ACK_WINDOW_MS);
        if self.client.now() > deadline {
            return Err(self.didnt_finish());
        }
        let remaining = ((deadline - self.client.now()).max(0) / 1000) as u64;
        let msgs = self
            .client
            .get_msgs(
                MailboxAuth::Member(&self.keys),
                &self.request.mailbox,
                &self.request.rid,
                self.cursor,
                wait_secs.min(remaining),
            )
            .await?;
        let seal = self.keys.seal();
        for m in msgs {
            self.cursor = self.cursor.max(m.seq);
            let plain = open_blob(&self.ka, &m.blob).or_else(|| open_blob(&seal, &m.blob));
            let Some(v) = plain.and_then(|p| Verified::parse(&p).ok()) else {
                continue;
            };
            match v.t {
                MsgType::Ack => {
                    let Ok(d) = verify_ack(
                        &v,
                        &self.request.rid,
                        self.request.signer_alg,
                        &self.request.signer_key,
                    ) else {
                        continue;
                    };
                    if self.offered.as_ref().is_some_and(|o| o.digest == d)
                        && self.client.now() <= deadline
                    {
                        self.acked = true;
                        return Ok(true);
                    }
                }
                MsgType::Abort => {
                    if let Ok(reason) = verify_abort(&v, &self.request.rid) {
                        self.ended = true;
                        self.offered = None;
                        return Err(ApproverError::Aborted(reason));
                    }
                }
                _ => {}
            }
        }
        if self.client.now() > deadline {
            return Err(self.didnt_finish());
        }
        Ok(false)
    }

    fn didnt_finish(&mut self) -> ApproverError {
        self.ended = true;
        self.offered = None;
        ApproverError::DidntFinish
    }

    /// Whether the offer was acknowledged (computers).
    pub fn acknowledged(&self) -> bool {
        self.acked
    }

    /// Whether the approval may be written now, checked right before the
    /// passkey ceremony and the log write (and again after the person typed
    /// or confirmed the code): the session isn't over, the code hasn't
    /// expired (a box or a phone), the computer acknowledged within
    /// [`ACK_WINDOW_MS`] of the offer, and no abort arrived (one read of the
    /// mailbox, no wait). Anything else is an error and nothing should be
    /// written.
    pub async fn ready_to_commit(&mut self) -> Result<(), ApproverError> {
        if self.ended {
            return Err(ApproverError::Ended);
        }
        let now = self.client.now();
        let code = self.code.as_ref().ok_or(ApproverError::NoCode)?;
        if self.request.kind == MatchKind::Computer {
            if let Some(o) = &self.offered {
                if now > o.at.saturating_add(ACK_WINDOW_MS) {
                    return Err(self.didnt_finish());
                }
            } else if code.expired(now) {
                self.expire().await?;
                return Err(ApproverError::CodeExpired);
            }
        } else if code.expired(now) {
            self.expire().await?;
            return Err(ApproverError::CodeExpired);
        }
        let msgs = self
            .client
            .get_msgs(
                MailboxAuth::Member(&self.keys),
                &self.request.mailbox,
                &self.request.rid,
                self.cursor,
                0,
            )
            .await?;
        let seal = self.keys.seal();
        for m in msgs {
            self.cursor = self.cursor.max(m.seq);
            let plain = open_blob(&self.ka, &m.blob).or_else(|| open_blob(&seal, &m.blob));
            let Some(v) = plain.and_then(|p| Verified::parse(&p).ok()) else {
                continue;
            };
            if v.t == MsgType::Abort {
                if let Ok(reason) = verify_abort(&v, &self.request.rid) {
                    self.ended = true;
                    self.offered = None;
                    return Err(ApproverError::Aborted(reason));
                }
            }
            if v.t == MsgType::Ack && self.request.kind == MatchKind::Computer && !self.acked {
                if let Ok(d) = verify_ack(
                    &v,
                    &self.request.rid,
                    self.request.signer_alg,
                    &self.request.signer_key,
                ) {
                    self.acked = self.offered.as_ref().is_some_and(|o| o.digest == d);
                }
            }
        }
        if self.request.kind == MatchKind::Computer && self.offered.is_some() && !self.acked {
            return Err(ApproverError::NotAcknowledged);
        }
        Ok(())
    }

    /// The approval is on the log: send the result with the grant and its
    /// digest, signed by this device.
    ///
    /// * A box or a phone: `grant` is the `ligusadmit1:` (`None` for a
    ///   phone). Refused once the code expired.
    /// * A computer: only after [`Self::offer`] and a valid
    ///   [`Self::await_ack`]; the offered grant is sent (`grant`, if given,
    ///   must be that one).
    pub async fn finish_approved(&mut self, grant: Option<&str>) -> Result<(), ApproverError> {
        if self.ended {
            return Err(ApproverError::Ended);
        }
        if self.request.kind == MatchKind::Computer {
            if !self.acked {
                return Err(ApproverError::NotAcknowledged);
            }
            let offered = self.offered.take().ok_or(ApproverError::NotAcknowledged)?;
            if grant.is_some_and(|g| grant_digest(g) != offered.digest) {
                self.offered = Some(offered);
                return Err(ApproverError::GrantChanged);
            }
            return self
                .post_result(Outcome::Approved, Some(&offered.grant))
                .await;
        }
        let code = self.code.as_ref().ok_or(ApproverError::NoCode)?;
        if code.expired(self.client.now()) {
            self.expire().await?;
            return Err(ApproverError::CodeExpired);
        }
        self.post_result(Outcome::Approved, grant).await
    }

    /// The person said the codes don't match: a `mismatch` result to the
    /// requester and an abort to the members and the requester.
    pub async fn mismatch(&mut self) -> Result<(), ApproverError> {
        self.offered = None;
        self.post_result(Outcome::Mismatch, None).await?;
        post_abort(
            &self.client,
            &self.keys,
            &self.request,
            &self.signer,
            AbortReason::Mismatch,
        )
        .await
    }

    /// The person declined: a `declined` result and a `cancelled` abort.
    pub async fn decline(&mut self) -> Result<(), ApproverError> {
        self.offered = None;
        self.post_result(Outcome::Declined, None).await?;
        post_abort(
            &self.client,
            &self.keys,
            &self.request,
            &self.signer,
            AbortReason::Cancelled,
        )
        .await
    }

    /// **End this request** from the contested sheet.
    pub async fn contested(&mut self) -> Result<(), ApproverError> {
        self.ended = true;
        post_abort(
            &self.client,
            &self.keys,
            &self.request,
            &self.signer,
            AbortReason::Contested,
        )
        .await
    }

    /// The code ran out: an `expired` result to the requester.
    pub async fn expire(&mut self) -> Result<(), ApproverError> {
        if self.ended {
            return Ok(());
        }
        self.post_result(Outcome::Expired, None).await
    }
}

//! The passkey hand-off (workspace model v3, 17.3; bytes in
//! `docs/design/code-match-wire.md` section 8): a terminal that has no
//! passkey (the CLI, lt) asks its person's own desktop or phone to confirm one
//! drafted event with the passkey.
//!
//! * The terminal drafts the event exactly as it will store it, signed by
//!   its device key, and posts a `presence` request to the workspace's
//!   approval mailbox: `liguspresence1:` carrying that draft, with the
//!   request's `commit` set to the draft's passkey challenge
//!   ([`ligus_core::presence::challenge`]).
//! * Both screens show the same 8-symbol code, derived from the request alone
//!   ([`ligus_core::match_code::presence_code40`]), so the terminal can show
//!   it straight away and may exit while it waits (`--no-wait`).
//! * The approver checks that the draft is stamped with its own account and
//!   signed by one of that account's devices that isn't revoked
//!   ([`check_presence_ask`]), renders the summary **from the draft**, runs
//!   the passkey over the draft's challenge and seals the assertion back in
//!   a signed `result` ([`PresenceApproval`]).
//! * The terminal takes a result only from one of its account's own devices,
//!   attaches the assertion and stores the draft; the fold checks presence
//!   exactly as for any other event. Nothing about the hand-off goes on the
//!   log.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ligus_core::crypto::{device_id_for, verify_envelope, SigAlg};
use ligus_core::e2ee::KeyAgreementKeypair;
use ligus_core::match_code::{self as mc, hex, MailboxKeys, MatchKind};
use ligus_core::presence::PresenceAssertion;
use ligus_core::{SessionEvent, SessionEventEnvelope};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::approver::{ApproverError, ApproverIdentity, EpochKey, SeenSet};
use super::client::{MailboxAuth, MailboxClient, MailboxError, MAX_REQUEST_BLOB};
use super::envelope::{
    grant_digest, open_blob, seal_blob, sign_message, verify_abort, verify_result_any, AbortBody, AbortReason, ClaimBody, MsgError, MsgType, Outcome,
    RequestBody, RequestSubject, ResultBody, Signer, Verified, VerifiedRequest,
};
use super::requester::{DeadLog, RequesterError, DEAD_LIMIT};
use super::{b64, hex_fixed, rand_bytes, ApprovalAddress, MatchCode};

/// Prefix of a hand-off's request code.
pub const PRESENCE_PREFIX: &str = "liguspresence1:";
/// Prefix of the assertion an approved hand-off's result carries (in
/// `grant`): `ligusassert1:<b64u(JSON PresenceAssertion)>`.
pub const ASSERTION_PREFIX: &str = "ligusassert1:";
/// The largest drafted envelope a hand-off carries (its JSON text): the
/// sealed request must fit the relay's 8 KiB request blob.
pub const MAX_DRAFT_BYTES: usize = 4 * 1024;
/// Hand-offs one device may ask for in an hour before an approver stops
/// showing its requests.
pub const ASK_RATE_PER_HOUR: usize = 6;
/// The requester's pending hand-offs in a data dir (owner-only).
pub const PRESENCE_PENDING_FILE: &str = "presence-pending.json";
const HOUR_MS: i64 = 60 * 60 * 1000;

// ---- The request code ----

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AskWire {
    v: u32,
    /// The requesting device's name (a hint; approvers prefer the log's).
    label: String,
    /// The requesting device's Ed25519 key.
    device_key: String,
    /// The drafted envelope's JSON text, byte for byte.
    draft: String,
}

/// A drafted event one of a person's devices asks that person to confirm.
#[derive(Debug, Clone)]
pub struct PresenceAsk {
    /// A display hint ([`super::hint`] cleans it everywhere it is shown).
    pub label: String,
    /// The requesting device's Ed25519 key (it signed the draft).
    pub device_key: Vec<u8>,
    /// The drafted envelope, parsed from [`Self::draft_text`].
    pub draft: SessionEventEnvelope,
    draft_text: String,
}

impl PresenceAsk {
    /// The ask for `draft`, signed by the device whose key is `device_key`.
    pub fn new(label: &str, device_key: &[u8], draft: &SessionEventEnvelope) -> Result<Self, MsgError> {
        let draft_text = serde_json::to_string(draft).map_err(|_| MsgError::Malformed)?;
        let wire = AskWire { v: 1, label: super::hint(label), device_key: b64::enc(device_key), draft: draft_text };
        let code = format!("{PRESENCE_PREFIX}{}", b64::enc(&serde_json::to_vec(&wire).map_err(|_| MsgError::Malformed)?));
        Self::decode(&code)
    }

    /// The `liguspresence1:` text.
    pub fn encode(&self) -> String {
        let wire = AskWire { v: 1, label: self.label.clone(), device_key: b64::enc(&self.device_key), draft: self.draft_text.clone() };
        format!("{PRESENCE_PREFIX}{}", b64::enc(&serde_json::to_vec(&wire).expect("serialize an ask")))
    }

    /// Parse and check an ask: the draft is signed by the key it names, that
    /// key derives the draft's signing device, the draft is stamped with an
    /// account, carries no presence yet, and is a change this build can read.
    pub fn decode(code: &str) -> Result<Self, MsgError> {
        let body = code.trim().strip_prefix(PRESENCE_PREFIX).ok_or(MsgError::KindMismatch)?;
        let bytes = b64::dec(body).ok_or(MsgError::Malformed)?;
        let w: AskWire = serde_json::from_slice(&bytes).map_err(|_| MsgError::Malformed)?;
        if w.v != 1 {
            return Err(MsgError::Malformed);
        }
        if w.draft.len() > MAX_DRAFT_BYTES {
            return Err(MsgError::BadCode("the change is too large to confirm from another device".into()));
        }
        let device_key = b64::dec(&w.device_key).ok_or(MsgError::Malformed)?;
        if device_key.len() != 32 {
            return Err(MsgError::Malformed);
        }
        let draft: SessionEventEnvelope = serde_json::from_str(&w.draft).map_err(|_| MsgError::Malformed)?;
        if draft.signer_device_id != Some(device_id_for(SigAlg::Ed25519, &device_key)) {
            return Err(MsgError::WrongDeviceId);
        }
        if verify_envelope(&draft, &device_key).is_err() {
            return Err(MsgError::BadSignature);
        }
        let Some(stamp) = draft.actor_stamp.as_ref() else { return Err(MsgError::BadCode("the change has no author stamp".into())) };
        if stamp.actor.account_id.is_none() {
            return Err(MsgError::BadCode("the change isn't stamped with an account".into()));
        }
        if stamp.presence.is_some() {
            return Err(MsgError::BadCode("the change is already confirmed".into()));
        }
        if matches!(draft.payload, SessionEvent::Unknown) {
            return Err(MsgError::BadCode("this build can't read that change: update Ligus".into()));
        }
        Ok(Self { label: w.label, device_key, draft, draft_text: w.draft })
    }

    /// The draft's passkey challenge (what the approver's passkey signs).
    pub fn challenge(&self) -> [u8; 32] {
        ligus_core::presence::challenge(&self.draft)
    }

    /// The requesting device.
    pub fn device_id(&self) -> Uuid {
        device_id_for(SigAlg::Ed25519, &self.device_key)
    }

    /// The account the draft is stamped with.
    pub fn account_id(&self) -> Uuid {
        self.draft.actor_stamp.as_ref().and_then(|s| s.actor.account_id).unwrap_or_default()
    }

    /// The draft's JSON text, byte for byte.
    pub fn draft_text(&self) -> &str {
        &self.draft_text
    }
}

/// A hand-off's code: the same on the terminal and on the approver, from
/// the request alone. Valid while the request lives (15 minutes).
pub fn presence_code(request: &VerifiedRequest) -> MatchCode {
    MatchCode::new(mc::presence_code40(&request.mailbox, &request.rid, request.code.as_bytes()), request.created_at.saturating_add(mc::REQUEST_TTL_MS))
}

fn code_for(mailbox: &[u8; 16], rid: &[u8; 16], ask_text: &str, created_at: i64) -> MatchCode {
    MatchCode::new(mc::presence_code40(mailbox, rid, ask_text.as_bytes()), created_at.saturating_add(mc::REQUEST_TTL_MS))
}

// ---- The approver's checks ----

/// Why an approver won't show a hand-off as confirmable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PresenceRefusal {
    #[error("not a passkey hand-off")]
    NotAHandoff,
    #[error("the change is for another workspace")]
    OtherWorkspace,
    #[error("the change isn't yours: it is stamped with another account")]
    NotYourAccount,
    #[error("the asking device isn't one of your devices here (or it was revoked)")]
    NotYourDevice,
    #[error("{0} can't be confirmed from another device: do it on this one")]
    CantConfirmHere(String),
}

/// A hand-off an approver may show: everything rendered from the draft.
#[derive(Debug, Clone)]
pub struct PresenceView {
    pub rid: String,
    pub workspace_id: Uuid,
    pub device_id: Uuid,
    /// The asking device's label hint (the approver prefers the log's name).
    pub label: String,
    /// The event kind (`workspaceRuntimeUpserted`).
    pub kind: String,
    /// What the change does, in words, from the draft's payload.
    pub summary: String,
    pub code: MatchCode,
    pub challenge: [u8; 32],
}

/// Check a hand-off for the approving device: the draft is for
/// `workspace_id`, stamped with `my_account`, signed by a device
/// `is_my_device` says is that account's (certified, not revoked), and a
/// kind that can be confirmed this way (a new passkey is registered on the
/// device that holds it). The summary is rendered from the draft.
pub fn check_presence_ask(
    request: &VerifiedRequest,
    workspace_id: Uuid,
    my_account: Uuid,
    is_my_device: &dyn Fn(Uuid) -> bool,
) -> Result<PresenceView, PresenceRefusal> {
    let RequestSubject::Presence(ask) = &request.subject else { return Err(PresenceRefusal::NotAHandoff) };
    if ask.draft.workspace_id != workspace_id {
        return Err(PresenceRefusal::OtherWorkspace);
    }
    if ask.account_id() != my_account {
        return Err(PresenceRefusal::NotYourAccount);
    }
    if !is_my_device(ask.device_id()) {
        return Err(PresenceRefusal::NotYourDevice);
    }
    if matches!(ask.draft.payload, SessionEvent::PasskeyRegistered { .. }) {
        return Err(PresenceRefusal::CantConfirmHere("Registering a passkey".into()));
    }
    Ok(PresenceView {
        rid: request.rid_hex(),
        workspace_id,
        device_id: ask.device_id(),
        label: super::hint(&ask.label),
        kind: ask.draft.payload.kind_str().to_string(),
        summary: crate::chat_service::summarize(&ask.draft.payload),
        code: presence_code(request),
        challenge: ask.challenge(),
    })
}

/// Hand-offs shown per asking device: at most [`ASK_RATE_PER_HOUR`] distinct
/// requests in any hour (device-local, in memory).
#[derive(Debug, Clone, Default)]
pub struct AskRate {
    seen: HashMap<Uuid, Vec<(String, i64)>>,
}

impl AskRate {
    /// Whether `rid` from `device` may be shown at `now_ms` (a request
    /// already admitted stays admitted).
    pub fn admit(&mut self, device: Uuid, rid: &str, now_ms: i64) -> bool {
        let rows = self.seen.entry(device).or_default();
        rows.retain(|(_, at)| now_ms.saturating_sub(*at) <= HOUR_MS);
        if rows.iter().any(|(r, _)| r == rid) {
            return true;
        }
        if rows.len() >= ASK_RATE_PER_HOUR {
            return false;
        }
        rows.push((rid.to_string(), now_ms));
        true
    }
}

/// The returned assertion is over `challenge` (a WebAuthn assertion names
/// it in its client data; a recovery key's carries none, and the fold checks
/// its signature over the challenge).
fn assertion_names(a: &PresenceAssertion, challenge: &[u8; 32]) -> bool {
    a.client_data_json.is_empty() || ligus_core::presence::returned_challenge(&a.client_data_json).as_deref() == Some(challenge.as_slice())
}

fn encode_assertion(a: &PresenceAssertion) -> String {
    format!("{ASSERTION_PREFIX}{}", b64::enc(&serde_json::to_vec(a).expect("serialize an assertion")))
}

fn decode_assertion(text: &str) -> Option<PresenceAssertion> {
    let body = text.strip_prefix(ASSERTION_PREFIX)?;
    if body.len() > 8 * 1024 {
        return None;
    }
    serde_json::from_slice(&b64::dec(body)?).ok()
}

// ---- The approver's session ----

/// One hand-off being confirmed on this device.
pub struct PresenceApproval {
    client: MailboxClient,
    keys: MailboxKeys,
    request: VerifiedRequest,
    signer: Signer,
    cursor: u64,
    ended: bool,
    code: MatchCode,
}

impl std::fmt::Debug for PresenceApproval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PresenceApproval").field("rid", &self.request.rid_hex()).field("ended", &self.ended).finish()
    }
}

impl PresenceApproval {
    /// Claim `request` (sealed to the members, so the person's other devices
    /// show who is checking it). Refuses a request that isn't a hand-off, a
    /// `rid` this device answered before, and an expired request.
    pub async fn begin(client: MailboxClient, epoch: &EpochKey, request: &VerifiedRequest, me: ApproverIdentity, seen: &mut SeenSet) -> Result<Self, ApproverError> {
        if request.kind != MatchKind::Presence {
            return Err(ApproverError::Msg(MsgError::KindMismatch));
        }
        let now = client.now();
        if seen.contains(&request.mailbox, &request.rid, now) {
            return Err(ApproverError::ReusedRid);
        }
        if now.saturating_sub(request.created_at) > mc::REQUEST_TTL_MS || request.created_at > now.saturating_add(super::CLOCK_SKEW_MS) {
            return Err(ApproverError::RequestExpired);
        }
        let keys = MailboxKeys::derive(&epoch.key);
        if keys.mailbox() != request.mailbox {
            return Err(ApproverError::Msg(MsgError::WrongMailbox));
        }
        seen.mark(&request.mailbox, &request.rid, now).map_err(|e| ApproverError::Io(e.to_string()))?;
        let claim = sign_message(
            MsgType::Claim,
            &ClaimBody { rid: request.rid_hex(), approver_device_id: me.signer.device_id(), approver_name: me.device_name.clone(), at: now },
            &me.signer,
        );
        let blob = seal_blob(&keys.seal().public_key_bytes(), claim.as_bytes())?;
        client.post_msg(MailboxAuth::Member(&keys), &request.mailbox, &request.rid, &blob, "members").await?;
        Ok(Self { client, keys, request: request.clone(), signer: me.signer, cursor: 0, ended: false, code: presence_code(request) })
    }

    pub fn request(&self) -> &VerifiedRequest {
        &self.request
    }

    /// The code both screens show.
    pub fn code(&self) -> &MatchCode {
        &self.code
    }

    /// The draft being confirmed.
    pub fn ask(&self) -> &PresenceAsk {
        match &self.request.subject {
            RequestSubject::Presence(a) => a,
            _ => unreachable!("begin takes only a hand-off"),
        }
    }

    /// Whether the passkey may run now: not ended, not expired, and no
    /// abort arrived (one read of the mailbox, no wait).
    pub async fn ready(&mut self) -> Result<(), ApproverError> {
        if self.ended {
            return Err(ApproverError::Ended);
        }
        if self.code.expired(self.client.now()) {
            self.ended = true;
            return Err(ApproverError::RequestExpired);
        }
        let msgs = self.client.get_msgs(MailboxAuth::Member(&self.keys), &self.request.mailbox, &self.request.rid, self.cursor, 0).await?;
        let seal = self.keys.seal();
        for m in msgs {
            self.cursor = self.cursor.max(m.seq);
            let Some(v) = open_blob(&seal, &m.blob).and_then(|p| Verified::parse(&p).ok()) else { continue };
            if v.t == MsgType::Abort && v.alg == self.request.signer_alg && v.key == self.request.signer_key {
                if let Ok(reason) = verify_abort(&v, &self.request.rid) {
                    self.ended = true;
                    return Err(ApproverError::Aborted(reason));
                }
            }
        }
        Ok(())
    }

    async fn post_result(&mut self, outcome: Outcome, grant: Option<String>) -> Result<(), ApproverError> {
        let body = ResultBody { rid: self.request.rid_hex(), outcome, grant_digest: grant.as_deref().map(grant_digest), grant };
        let msg = sign_message(MsgType::Result, &body, &self.signer);
        let blob = seal_blob(&self.request.requester_ka, msg.as_bytes())?;
        self.client.post_msg(MailboxAuth::Member(&self.keys), &self.request.mailbox, &self.request.rid, &blob, "requester").await?;
        self.ended = true;
        Ok(())
    }

    /// The person confirmed with their passkey: send `assertion` (over this
    /// draft's challenge, else refused) to the terminal.
    pub async fn approve(&mut self, assertion: &PresenceAssertion) -> Result<(), ApproverError> {
        self.ready().await?;
        if !assertion_names(assertion, &self.ask().challenge()) {
            return Err(ApproverError::Msg(MsgError::BadCode("the passkey signed something else than this change".into())));
        }
        self.post_result(Outcome::Approved, Some(encode_assertion(assertion))).await
    }

    /// Not me (or Decline): a `declined` result to the terminal and a
    /// `cancelled` abort to the members. Revokes nothing by itself.
    pub async fn decline(&mut self) -> Result<(), ApproverError> {
        if self.ended {
            return Ok(());
        }
        self.post_result(Outcome::Declined, None).await?;
        let msg = sign_message(MsgType::Abort, &AbortBody { rid: self.request.rid_hex(), reason: AbortReason::Cancelled }, &self.signer);
        let blob = seal_blob(&self.keys.seal().public_key_bytes(), msg.as_bytes())?;
        self.client.post_msg(MailboxAuth::Member(&self.keys), &self.request.mailbox, &self.request.rid, &blob, "members").await?;
        Ok(())
    }
}

// ---- The requester's side ----

/// One pending hand-off (owner-only file): what `--no-wait` leaves for
/// `ligus handoff wait`. Holds no workspace secret; `readKey` and `kaSeed`
/// only read this one request's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresencePending {
    pub rid: String,
    pub relay: String,
    pub mailbox: String,
    /// The members' seal key (a cancel is sealed to it).
    pub seal_key: String,
    pub workspace_id: Uuid,
    pub created_at: i64,
    pub read_key: String,
    pub ka_seed: String,
    /// The `liguspresence1:` text (it carries the draft).
    pub ask: String,
    /// What it confirms, in words (shown again by `ligus handoff list`).
    pub summary: String,
    /// The code as shown (`K7Q4 M9X2`).
    pub code: String,
}

/// `presence-pending.json` in a data dir (owner-only).
#[derive(Debug, Clone)]
pub struct PresencePendingStore {
    path: PathBuf,
}

impl PresencePendingStore {
    pub fn new(data_dir: &Path) -> Self {
        Self { path: data_dir.join(PRESENCE_PENDING_FILE) }
    }

    pub fn load(&self) -> std::io::Result<Vec<PresencePending>> {
        match std::fs::read(&self.path) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// The live entries at `now_ms` (expired ones are dropped from the file).
    pub fn live(&self, now_ms: i64) -> std::io::Result<Vec<PresencePending>> {
        let all = self.load()?;
        let (live, old): (Vec<_>, Vec<_>) = all.into_iter().partition(|e| now_ms.saturating_sub(e.created_at) <= mc::REQUEST_TTL_MS);
        if !old.is_empty() {
            self.save(&live)?;
        }
        Ok(live)
    }

    fn save(&self, all: &[PresencePending]) -> std::io::Result<()> {
        if all.is_empty() {
            return match std::fs::remove_file(&self.path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        let bytes = serde_json::to_vec_pretty(all).map_err(std::io::Error::other)?;
        crate::private_file::write_private_atomic(&self.path, &bytes)
    }

    pub fn upsert(&self, e: &PresencePending) -> std::io::Result<()> {
        let mut all = self.load()?;
        all.retain(|x| x.rid != e.rid);
        all.push(e.clone());
        self.save(&all)
    }

    pub fn remove(&self, rid: &str) -> std::io::Result<()> {
        let mut all = self.load()?;
        let before = all.len();
        all.retain(|x| x.rid != rid);
        if all.len() != before {
            self.save(&all)?;
        }
        Ok(())
    }
}

/// How a hand-off ended, as the terminal reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresenceOutcome {
    /// One of the person's devices confirmed with the passkey: attach the
    /// assertion and store the draft.
    Approved { assertion: PresenceAssertion, by_device: Uuid },
    /// The person said Not me (or declined).
    Declined { by_device: Uuid },
    /// Ended by one of the person's devices.
    Aborted { reason: AbortReason, by_device: Uuid },
    /// Fifteen minutes passed.
    Expired,
}

/// A hand-off in flight on the terminal.
pub struct PresenceRequester {
    client: MailboxClient,
    mailbox: [u8; 16],
    seal_key: [u8; 32],
    rid: [u8; 16],
    read_key: [u8; 16],
    ka: KeyAgreementKeypair,
    ask: PresenceAsk,
    code: MatchCode,
    created_at: i64,
    signer: Signer,
    cursor: u64,
    ended: bool,
    pending: Option<PresencePendingStore>,
    dead: Option<DeadLog>,
}

impl std::fmt::Debug for PresenceRequester {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PresenceRequester").field("rid", &hex(&self.rid)).field("ended", &self.ended).finish()
    }
}

fn io(e: std::io::Error) -> RequesterError {
    RequesterError::Io(e.to_string())
}

impl PresenceRequester {
    /// Post a hand-off for `ask` (signed by `signer`, the draft's device) to
    /// `address`. With `data_dir`, the hand-off is kept in
    /// [`PRESENCE_PENDING_FILE`] until settled, and the dead-request limit
    /// applies (3 refused or expired in 15 minutes stop the terminal).
    pub async fn post(client: MailboxClient, address: &ApprovalAddress, signer: Signer, ask: PresenceAsk, summary: &str, data_dir: Option<&Path>) -> Result<Self, RequesterError> {
        let now = client.now();
        let dead = data_dir.map(DeadLog::new);
        if let Some(d) = &dead {
            if d.recent(now).map_err(io)? >= DEAD_LIMIT {
                return Err(RequesterError::TooManyDead);
            }
        }
        if signer.alg() != SigAlg::Ed25519 || signer.public_key() != ask.device_key {
            return Err(RequesterError::BadSpec("the signer isn't the draft's device key".into()));
        }
        if ask.draft.workspace_id != address.workspace_id {
            return Err(RequesterError::BadSpec("the draft is for another workspace than the approval address".into()));
        }
        let ka = KeyAgreementKeypair::generate().map_err(|e| RequesterError::BadSpec(e.to_string()))?;
        let rid: [u8; 16] = rand_bytes();
        let read_key: [u8; 16] = rand_bytes();
        let ask_text = ask.encode();
        let body = RequestBody {
            v: 1,
            kind: MatchKind::Presence,
            rid: hex(&rid),
            mailbox: address.mailbox_hex(),
            code: ask_text.clone(),
            requester_ka: b64::enc(&ka.public_key_bytes()),
            read_key: b64::enc(&read_key),
            commit: b64::enc(&ask.challenge()),
            created_at: now,
        };
        let plain = sign_message(MsgType::Request, &body, &signer);
        VerifiedRequest::verify(plain.as_bytes(), &rid, &address.mailbox, now).map_err(|e| RequesterError::BadSpec(e.to_string()))?;
        let blob = seal_blob(&address.seal_key, plain.as_bytes())?;
        if blob.len() > MAX_REQUEST_BLOB {
            return Err(RequesterError::Mailbox(MailboxError::TooLarge));
        }
        let code = code_for(&address.mailbox, &rid, &ask_text, now);
        let pending = data_dir.map(PresencePendingStore::new);
        let entry = PresencePending {
            rid: hex(&rid),
            relay: address.relay.clone(),
            mailbox: address.mailbox_hex(),
            seal_key: b64::enc(&address.seal_key),
            workspace_id: address.workspace_id,
            created_at: now,
            read_key: b64::enc(&read_key),
            ka_seed: b64::enc(&ka.seed_bytes()),
            ask: ask_text,
            summary: summary.to_string(),
            code: code.shown.clone(),
        };
        if let Some(p) = &pending {
            p.upsert(&entry).map_err(io)?;
        }
        if let Err(e) = client.put_request(&address.mailbox, &rid, &address.put_cap, &blob, &read_key).await {
            if let Some(p) = &pending {
                let _ = p.remove(&entry.rid);
            }
            return Err(e.into());
        }
        Ok(Self {
            client,
            mailbox: address.mailbox,
            seal_key: address.seal_key,
            rid,
            read_key,
            ka,
            ask,
            code,
            created_at: now,
            signer,
            cursor: 0,
            ended: false,
            pending,
            dead,
        })
    }

    /// Pick up a hand-off a terminal left waiting (`--no-wait`), on the
    /// relay it was posted to.
    pub fn resume(client: MailboxClient, entry: &PresencePending, signer: Signer, data_dir: Option<&Path>) -> Result<Self, RequesterError> {
        let bad = || RequesterError::BadSpec("the pending hand-off is damaged".into());
        let ask = PresenceAsk::decode(&entry.ask)?;
        if signer.public_key() != ask.device_key {
            return Err(RequesterError::BadSpec("that hand-off was asked by another device".into()));
        }
        let mailbox = hex_fixed::<16>(&entry.mailbox).ok_or_else(bad)?;
        let rid = hex_fixed::<16>(&entry.rid).ok_or_else(bad)?;
        let seed = b64::fixed::<32>(&entry.ka_seed).ok_or_else(bad)?;
        Ok(Self {
            client,
            mailbox,
            seal_key: b64::fixed::<32>(&entry.seal_key).ok_or_else(bad)?,
            rid,
            read_key: b64::fixed::<16>(&entry.read_key).ok_or_else(bad)?,
            ka: KeyAgreementKeypair::from_seed(&seed).map_err(|_| bad())?,
            code: code_for(&mailbox, &rid, &entry.ask, entry.created_at),
            ask,
            created_at: entry.created_at,
            signer,
            cursor: 0,
            ended: false,
            pending: data_dir.map(PresencePendingStore::new),
            dead: data_dir.map(DeadLog::new),
        })
    }

    pub fn rid_hex(&self) -> String {
        hex(&self.rid)
    }

    /// The code both screens show (from the request alone).
    pub fn code(&self) -> &MatchCode {
        &self.code
    }

    /// The draft this hand-off confirms, exactly as it will be stored.
    pub fn draft(&self) -> &SessionEventEnvelope {
        &self.ask.draft
    }

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

    /// Read the answer (long-polling up to `wait_secs`). Only an answer
    /// signed by a device `is_my_device` accepts (one of this account's own,
    /// on this device's log) counts; an approval must carry an assertion over
    /// this draft's challenge. `None` while waiting.
    pub async fn poll(&mut self, wait_secs: u64, is_my_device: &(dyn Fn(Uuid) -> bool + Sync)) -> Result<Option<PresenceOutcome>, RequesterError> {
        if self.ended {
            return Ok(None);
        }
        let msgs = match self.client.get_msgs(MailboxAuth::Requester(&self.read_key), &self.mailbox, &self.rid, self.cursor, wait_secs).await {
            Ok(m) => m,
            // The relay dropped it at its expiry.
            Err(MailboxError::RequestUnknown) if self.code.expired(self.client.now()) => {
                self.mark_dead();
                return Ok(Some(PresenceOutcome::Expired));
            }
            Err(e) => return Err(e.into()),
        };
        for m in msgs {
            self.cursor = self.cursor.max(m.seq);
            let Some(v) = open_blob(&self.ka, &m.blob).and_then(|p| Verified::parse(&p).ok()) else { continue };
            let by = v.signer_device_id();
            if !is_my_device(by) {
                continue;
            }
            match v.t {
                MsgType::Result => {
                    let Ok(r) = verify_result_any(&v, &self.rid) else { continue };
                    match r.outcome {
                        Outcome::Approved => {
                            let (Some(g), Some(d)) = (&r.grant, &r.grant_digest) else { continue };
                            if grant_digest(g) != *d {
                                continue;
                            }
                            let Some(a) = decode_assertion(g) else { continue };
                            if !assertion_names(&a, &self.ask.challenge()) {
                                continue;
                            }
                            self.ended = true;
                            return Ok(Some(PresenceOutcome::Approved { assertion: a, by_device: by }));
                        }
                        Outcome::Expired => {
                            self.mark_dead();
                            return Ok(Some(PresenceOutcome::Expired));
                        }
                        Outcome::Declined | Outcome::Mismatch => {
                            self.mark_dead();
                            return Ok(Some(PresenceOutcome::Declined { by_device: by }));
                        }
                    }
                }
                MsgType::Abort => {
                    let Ok(reason) = verify_abort(&v, &self.rid) else { continue };
                    self.mark_dead();
                    return Ok(Some(PresenceOutcome::Aborted { reason, by_device: by }));
                }
                _ => {}
            }
        }
        if self.code.expired(self.client.now()) {
            self.mark_dead();
            return Ok(Some(PresenceOutcome::Expired));
        }
        Ok(None)
    }

    /// Cancel from the terminal (Ctrl-C, `ligus handoff cancel`, a timeout):
    /// an abort sealed to the members, so the person's desktop drops the card.
    pub async fn cancel(&mut self) -> Result<(), RequesterError> {
        let msg = sign_message(MsgType::Abort, &AbortBody { rid: hex(&self.rid), reason: AbortReason::Cancelled }, &self.signer);
        self.mark_dead();
        let blob = seal_blob(&self.seal_key, msg.as_bytes())?;
        self.client.post_msg(MailboxAuth::Requester(&self.read_key), &self.mailbox, &self.rid, &blob, "members").await?;
        Ok(())
    }

    /// Over (whatever the outcome): delete the request from the relay and
    /// drop the pending entry.
    pub async fn settle(&mut self) -> Result<(), RequesterError> {
        self.ended = true;
        let r = self.client.delete_request(&self.mailbox, &self.rid, &self.read_key).await;
        if let Some(p) = &self.pending {
            p.remove(&hex(&self.rid)).map_err(io)?;
        }
        match r {
            Ok(()) | Err(MailboxError::RequestUnknown) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// The request's age at `now_ms`.
    pub fn age_ms(&self, now_ms: i64) -> i64 {
        now_ms.saturating_sub(self.created_at)
    }
}

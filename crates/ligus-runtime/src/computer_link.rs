//! "Add a computer": linking another computer (the `lt` terminal app, the
//! `ligus` CLI) as one of **your own devices**, so what it does is yours and
//! not a separate member's. Design: `docs/design/link-a-computer.md`.
//!
//! No new event and no new rule. The new computer becomes a device of the
//! person through an ordinary `DeviceCertificateAdded` for its own key under
//! the person's account, which the person's desktop publishes (with their
//! passkey): every fold (0.1.0 on) reads it as a device that acts for the
//! person without holding their account key (a *delegated* device, the shape
//! the rules already have for it). It never gets the account key; account-key
//! acts (certifying devices or phones, rotating keys, registering passkeys,
//! identity links), applying a proposal itself, sharing files and votes
//! without a passkey stay with the desktop.
//!
//! 1. The new computer prints a **link request** ([`LinkRequest`],
//!    `liguslink1:`): its device key, its key-agreement key, a label and the
//!    time, signed by the device key. Nothing secret. It remembers the
//!    request as pending ([`LinkState`]).
//! 2. The person's desktop compares the fingerprint, publishes the
//!    certificate with **one passkey ceremony**, and seals the workspace
//!    connection and the epoch keys to the request's key-agreement key: the
//!    **grant** ([`Grant`], `ligusgrant1:`), valid for [`GRANT_TTL_MS`]
//!    ([`crate::ChatService::issue_computer_link`]).
//! 3. The new computer opens the grant, shows who approved it (the person,
//!    the workspace, the approving desktop's fingerprint) for the person to
//!    confirm, pulls the workspace and checks the certificate is there
//!    ([`crate::ChatService::check_computer_link`]). From then on it acts as
//!    the person (`ChatService::act_as_owner_device`).
//!
//! An install linked this way acts as that person in every workspace it holds
//! ("one person per install", [`LinkState`]).

use std::path::Path;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ligus_core::crypto::{device_id_for, verify, SigAlg, SigningKeypair};
use ligus_core::{ActorIdentity, ActorKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Prefix of a link request.
pub const REQUEST_PREFIX: &str = "liguslink1:";
/// Prefix of a grant.
pub const GRANT_PREFIX: &str = "ligusgrant1:";
// Signature contexts (new, never renamed). Pinned in the tests below.
const REQUEST_TAG: &[u8] = b"ligus-link-request-v1\0";
const PREVIOUS_TAG: &[u8] = b"ligus-link-previous-v1\0";
const GRANT_TAG: &[u8] = b"ligus-link-grant-v1\0";
/// The code format version both codes carry.
pub const VERSION: u32 = 1;
/// How long a request may be approved after it was made.
pub const REQUEST_TTL_MS: i64 = 15 * 60 * 1000;
/// How long a grant may be redeemed after it was issued.
pub const GRANT_TTL_MS: i64 = 30 * 60 * 1000;
/// How far ahead of this device's clock a request's time may be.
pub const CLOCK_SKEW_MS: i64 = 5 * 60 * 1000;
/// The longest code read (a pasted or file one).
pub const MAX_CODE_LEN: usize = 64 * 1024;
/// The file in a data dir that remembers pending requests and the person
/// this install acts for.
pub const STATE_FILE: &str = "computer-link.json";

/// What redeeming says while the desktop's certificate hasn't reached this
/// computer (the desktop syncs it within seconds): retry.
pub const NOT_ON_RELAY_YET: &str =
    "your desktop's approval hasn't reached this computer yet: make sure the desktop is online and synced, then try again";

/// Why a code or a link was refused. The text is what a person reads.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum LinkError {
    #[error("not a Ligus link request (expected {REQUEST_PREFIX}...)")]
    NotARequest,
    #[error("not a Ligus device grant (expected {GRANT_PREFIX}...)")]
    NotAGrant,
    #[error("the code is damaged or incomplete")]
    Malformed,
    #[error("code version {0} isn't supported here; update Ligus")]
    Version(u32),
    #[error("the request's signature doesn't match its device key")]
    BadSignature,
    #[error("the request's device id isn't the one its key derives")]
    WrongDeviceId,
    #[error("this request is too old (over 15 minutes): make a new one on the computer")]
    RequestExpired,
    #[error("this request is dated in the future: check the computer's clock")]
    RequestFromTheFuture,
    #[error("this grant was sealed to another computer")]
    NotForThisDevice,
    #[error("the grant's signature doesn't verify: it was changed or isn't from a Ligus desktop")]
    BadGrantSignature,
    #[error("this grant expired: on your desktop, add the computer again for a new one")]
    GrantExpired,
    #[error("this grant doesn't answer a request made on this computer (or it was used already): make a new request")]
    NotPending,
    #[error("{0}")]
    Refused(String),
}

/// A request's optional proof that the computer was already a member here
/// under another identity: a signature by **that member's account key** over
/// the request. The desktop only shows it ("this computer was …"); it
/// changes no rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviousMember {
    pub account_id: Uuid,
    #[serde(with = "b64")]
    pub signature: Vec<u8>,
}

/// What a computer asks to be linked as: its keys, a label, the time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkRequest {
    pub v: u32,
    pub device_id: Uuid,
    /// Ed25519 device key (32 bytes).
    #[serde(with = "b64")]
    pub device_public_key: Vec<u8>,
    /// X25519 key-agreement key (32 bytes): the grant is sealed to it.
    #[serde(with = "b64")]
    pub ka_public: Vec<u8>,
    #[serde(default)]
    pub label: String,
    /// Unix ms.
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<PreviousMember>,
    /// Ed25519 by the device key over [`LinkRequest::preimage`].
    #[serde(with = "b64")]
    pub signature: Vec<u8>,
}

fn previous_preimage(device_id: Uuid, key: &[u8], ka: &[u8], created_at: i64) -> Vec<u8> {
    let mut out = Vec::with_capacity(PREVIOUS_TAG.len() + 16 + 64 + 8);
    out.extend_from_slice(PREVIOUS_TAG);
    out.extend_from_slice(device_id.as_bytes());
    out.extend_from_slice(key);
    out.extend_from_slice(ka);
    out.extend_from_slice(&created_at.to_le_bytes());
    out
}

fn put(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

impl LinkRequest {
    /// A request for the device whose key is `device`, sealed-to key
    /// `ka_public`, made at `created_at` (unix ms). With `previous`, the
    /// request also carries that account's proof (see [`PreviousMember`]).
    pub fn new(
        device: &SigningKeypair,
        ka_public: &[u8; 32],
        label: &str,
        created_at: i64,
        previous: Option<(Uuid, &SigningKeypair)>,
    ) -> Self {
        let key = device.public_key_bytes();
        let device_id = device_id_for(SigAlg::Ed25519, &key);
        let previous = previous.map(|(account_id, account)| PreviousMember {
            account_id,
            signature: account.sign(&previous_preimage(device_id, &key, ka_public, created_at)).to_vec(),
        });
        let mut r = Self {
            v: VERSION,
            device_id,
            device_public_key: key.to_vec(),
            ka_public: ka_public.to_vec(),
            label: crate::phone_enroll::clamp_label(label),
            created_at,
            previous,
            signature: Vec::new(),
        };
        r.signature = device.sign(&r.preimage()).to_vec();
        r
    }

    /// What the device key signs: the tag, the version, every field but the
    /// signature.
    pub fn preimage(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(256);
        out.extend_from_slice(REQUEST_TAG);
        out.extend_from_slice(&self.v.to_le_bytes());
        out.extend_from_slice(self.device_id.as_bytes());
        put(&mut out, &self.device_public_key);
        put(&mut out, &self.ka_public);
        out.extend_from_slice(&self.created_at.to_le_bytes());
        put(&mut out, self.label.as_bytes());
        match &self.previous {
            None => out.push(0),
            Some(p) => {
                out.push(1);
                out.extend_from_slice(p.account_id.as_bytes());
                put(&mut out, &p.signature);
            }
        }
        out
    }

    /// The `liguslink1:` code.
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("serialize a link request");
        format!("{REQUEST_PREFIX}{}", URL_SAFE_NO_PAD.encode(json))
    }

    /// Parse and verify a code (whitespace from a terminal copy is ignored):
    /// the version, the keys' lengths, the device id the key derives, a label
    /// that is its own clamp, and the device key's signature.
    pub fn decode(code: &str) -> Result<Self, LinkError> {
        let compact: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.len() > MAX_CODE_LEN {
            return Err(LinkError::Malformed);
        }
        let body = compact.strip_prefix(REQUEST_PREFIX).ok_or(LinkError::NotARequest)?;
        let bytes = URL_SAFE_NO_PAD.decode(body.as_bytes()).map_err(|_| LinkError::Malformed)?;
        let r: Self = serde_json::from_slice(&bytes).map_err(|_| LinkError::Malformed)?;
        if r.v != VERSION {
            return Err(LinkError::Version(r.v));
        }
        if r.device_public_key.len() != 32 || r.ka_public.len() != 32 || r.signature.len() != 64 {
            return Err(LinkError::Malformed);
        }
        if r.label != crate::phone_enroll::clamp_label(&r.label) {
            return Err(LinkError::Malformed);
        }
        if r.previous.as_ref().is_some_and(|p| p.signature.len() != 64) {
            return Err(LinkError::Malformed);
        }
        verify(&r.device_public_key, &r.preimage(), &r.signature).map_err(|_| LinkError::BadSignature)?;
        if r.device_id != device_id_for(SigAlg::Ed25519, &r.device_public_key) {
            return Err(LinkError::WrongDeviceId);
        }
        Ok(r)
    }

    /// The device fingerprint both screens show (`XXXX-XXXX-XXXX-XXXX-XXXX`),
    /// the one My devices shows for the computer afterwards.
    pub fn fingerprint(&self) -> String {
        crate::device_enroll::fingerprint(&self.device_public_key)
    }

    /// The request's identity: SHA-256 over what its key signed and the
    /// signature. A grant names it.
    pub fn digest(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.preimage());
        h.update(&self.signature);
        hex(&h.finalize())
    }

    /// Refuse a request older than [`REQUEST_TTL_MS`] or dated more than
    /// [`CLOCK_SKEW_MS`] ahead of `now_ms`.
    pub fn check_fresh(&self, now_ms: i64) -> Result<(), LinkError> {
        if self.created_at > now_ms.saturating_add(CLOCK_SKEW_MS) {
            return Err(LinkError::RequestFromTheFuture);
        }
        if now_ms.saturating_sub(self.created_at) > REQUEST_TTL_MS {
            return Err(LinkError::RequestExpired);
        }
        Ok(())
    }

    /// Whether the request's [`PreviousMember`] proof verifies under
    /// `account_key` (that account's registered signing key).
    pub fn previous_verifies(&self, account_key: &[u8]) -> bool {
        self.previous.as_ref().is_some_and(|p| {
            verify(account_key, &previous_preimage(self.device_id, &self.device_public_key, &self.ka_public, self.created_at), &p.signature)
                .is_ok()
        })
    }
}

/// The plaintext inside a grant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrantBody {
    v: u32,
    /// [`LinkRequest::digest`] of the request it answers.
    request: String,
    workspace_id: Uuid,
    /// The workspace connection (`hivews1:` codec), without an invite.
    conn: String,
    /// The device the desktop certified (the request's).
    device_id: Uuid,
    /// The person it acts for (JSON of their identity).
    person: String,
    approver_device_id: Uuid,
    #[serde(with = "b64")]
    approver_key: Vec<u8>,
    expires_at: i64,
    #[serde(with = "b64")]
    signature: Vec<u8>,
}

impl GrantBody {
    fn preimage(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(GRANT_TAG.len() + self.conn.len() + self.person.len() + 256);
        out.extend_from_slice(GRANT_TAG);
        out.extend_from_slice(&self.v.to_le_bytes());
        put(&mut out, self.request.as_bytes());
        out.extend_from_slice(self.workspace_id.as_bytes());
        put(&mut out, self.conn.as_bytes());
        out.extend_from_slice(self.device_id.as_bytes());
        put(&mut out, self.person.as_bytes());
        out.extend_from_slice(self.approver_device_id.as_bytes());
        put(&mut out, &self.approver_key);
        out.extend_from_slice(&self.expires_at.to_le_bytes());
        out
    }
}

/// Building and opening grants (`ligusgrant1:`).
pub struct Grant;

impl Grant {
    /// Seal a grant for `request`: `conn` (a `hivews1:` connection without an
    /// invite) and the `person` the computer now acts for, signed by the
    /// approving device.
    pub fn seal(
        request: &LinkRequest,
        workspace_id: Uuid,
        conn: &str,
        person: &ActorIdentity,
        approver_device_id: Uuid,
        approver: &SigningKeypair,
        expires_at: i64,
    ) -> Result<String, LinkError> {
        let mut body = GrantBody {
            v: VERSION,
            request: request.digest(),
            workspace_id,
            conn: conn.to_string(),
            device_id: request.device_id,
            person: serde_json::to_string(person).map_err(|_| LinkError::Malformed)?,
            approver_device_id,
            approver_key: approver.public_key_bytes().to_vec(),
            expires_at,
            signature: Vec::new(),
        };
        body.signature = approver.sign(&body.preimage()).to_vec();
        let plain = zeroize::Zeroizing::new(serde_json::to_vec(&body).map_err(|_| LinkError::Malformed)?);
        let blob = ligus_core::e2ee::seal(&request.ka_public, &plain).map_err(|_| LinkError::Malformed)?;
        let json = serde_json::to_vec(&blob).map_err(|_| LinkError::Malformed)?;
        Ok(format!("{GRANT_PREFIX}{}", URL_SAFE_NO_PAD.encode(json)))
    }

    /// Open a grant with this computer's key-agreement key and check its
    /// approver signature and version. Nothing else (see
    /// [`OpenedGrant::check`]).
    pub fn open(code: &str, ka: &ligus_core::KeyAgreementKeypair) -> Result<OpenedGrant, LinkError> {
        let compact: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.len() > MAX_CODE_LEN {
            return Err(LinkError::Malformed);
        }
        let body = compact.strip_prefix(GRANT_PREFIX).ok_or(LinkError::NotAGrant)?;
        let bytes = URL_SAFE_NO_PAD.decode(body.as_bytes()).map_err(|_| LinkError::Malformed)?;
        let blob: ligus_core::SealedBlob = serde_json::from_slice(&bytes).map_err(|_| LinkError::Malformed)?;
        let plain = zeroize::Zeroizing::new(ligus_core::e2ee::open(ka, &blob).map_err(|_| LinkError::NotForThisDevice)?);
        let g: GrantBody = serde_json::from_slice(&plain).map_err(|_| LinkError::Malformed)?;
        if g.v != VERSION {
            return Err(LinkError::Version(g.v));
        }
        if g.approver_key.len() != 32 {
            return Err(LinkError::Malformed);
        }
        verify(&g.approver_key, &g.preimage(), &g.signature).map_err(|_| LinkError::BadGrantSignature)?;
        if g.approver_device_id != device_id_for(SigAlg::Ed25519, &g.approver_key) {
            return Err(LinkError::BadGrantSignature);
        }
        let person: ActorIdentity = serde_json::from_str(&g.person).map_err(|_| LinkError::Malformed)?;
        let conn = crate::workspace_invite::decode_workspace_invite(&g.conn).map_err(|_| LinkError::Malformed)?;
        Ok(OpenedGrant {
            request: g.request,
            workspace_id: g.workspace_id,
            conn,
            conn_code: g.conn,
            device_id: g.device_id,
            person,
            approver_device_id: g.approver_device_id,
            approver_key: g.approver_key,
            expires_at: g.expires_at,
        })
    }
}

/// An opened grant.
#[derive(Debug, Clone)]
pub struct OpenedGrant {
    /// The digest of the request it answers.
    pub request: String,
    pub workspace_id: Uuid,
    pub conn: crate::workspace_invite::WorkspaceConn,
    /// The connection as it was sealed (`hivews1:…`).
    pub conn_code: String,
    /// The device the desktop certified.
    pub device_id: Uuid,
    /// The person, as the approving desktop named them.
    pub person: ActorIdentity,
    pub approver_device_id: Uuid,
    pub approver_key: Vec<u8>,
    pub expires_at: i64,
}

impl OpenedGrant {
    /// What a computer checks before it writes anything (no workspace needed
    /// yet): not expired; the connection is the grant's workspace and admits
    /// no one; it certifies **this** device (`device_key`); it names a human
    /// with an account. Returns the person, with the fields that belong to
    /// the approving desktop (its key-agreement key, its contact id) cleared.
    pub fn check(&self, device_key: &[u8], now_ms: i64) -> Result<ActorIdentity, LinkError> {
        let bad = |s: &str| Err(LinkError::Refused(s.to_string()));
        if now_ms > self.expires_at {
            return Err(LinkError::GrantExpired);
        }
        if self.conn.id() != self.workspace_id || self.conn.dm_account.is_some() {
            return bad("the grant's connection is for another workspace");
        }
        if self.conn.inv.is_some() {
            return bad("the grant carries an invite: a computer joins as your device, never as a new member");
        }
        if self.device_id != device_id_for(SigAlg::Ed25519, device_key) {
            return bad("the grant certifies another device, not this computer");
        }
        if self.person.kind != ActorKind::Human || self.person.account_id.is_none() {
            return bad("the grant doesn't name a person");
        }
        Ok(device_local_cleared(&self.person))
    }

    /// The approving desktop's device fingerprint (its Settings shows the
    /// same one as This device).
    pub fn approver_fingerprint(&self) -> String {
        crate::device_enroll::fingerprint(&self.approver_key)
    }
}

/// `person` as this computer stamps it: the fields that are a device's own
/// (its device id, its key-agreement key, its relay contact id) are not
/// copied from the desktop that named them.
pub fn device_local_cleared(person: &ActorIdentity) -> ActorIdentity {
    let mut p = person.clone();
    p.device_id = None;
    p.key_agreement_public = None;
    p.contact = None;
    p
}

/// A request this install made and hasn't redeemed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pending {
    pub digest: String,
    /// The device the request names: this install's, or a new one prepared
    /// next to it (switched to only on redeem).
    pub device_id: Uuid,
    pub created_at: i64,
    #[serde(default)]
    pub label: String,
    /// The request itself, to show again.
    pub code: String,
}

/// What a data dir remembers about linking (`computer-link.json`, owner-only;
/// nothing secret): its pending requests, and the person it acts for, in which
/// workspaces.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkState {
    #[serde(default)]
    pub pending: Vec<Pending>,
    /// The person this install is a device of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<ActorIdentity>,
    /// The workspaces it was linked into.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspaces: Vec<Uuid>,
}

impl LinkState {
    pub fn path(data_dir: &Path) -> std::path::PathBuf {
        data_dir.join(STATE_FILE)
    }

    /// The state in `data_dir` (empty when there is none). An unreadable or
    /// damaged file is an error: it may name the person this install acts
    /// for, and acting as no one instead would author as the wrong identity.
    pub fn load(data_dir: &Path) -> std::io::Result<Self> {
        match std::fs::read(Self::path(data_dir)) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        crate::private_file::write_private_atomic(&Self::path(data_dir), &bytes)
    }

    /// How long a pending request stays redeemable: it may be approved until
    /// [`REQUEST_TTL_MS`], and the grant then lives [`GRANT_TTL_MS`].
    pub const PENDING_TTL_MS: i64 = REQUEST_TTL_MS + GRANT_TTL_MS;

    /// Remember `request` as pending (requests past [`LinkState::PENDING_TTL_MS`] are
    /// dropped as they go).
    pub fn add_pending(&mut self, request: &LinkRequest, now_ms: i64) {
        self.pending.retain(|p| now_ms.saturating_sub(p.created_at) <= Self::PENDING_TTL_MS);
        let digest = request.digest();
        if !self.pending.iter().any(|p| p.digest == digest) {
            self.pending.push(Pending {
                digest,
                device_id: request.device_id,
                created_at: request.created_at,
                label: request.label.clone(),
                code: request.encode(),
            });
        }
    }

    /// The pending request `digest` names, while a grant for it can still be
    /// valid ([`LinkState::PENDING_TTL_MS`]).
    pub fn pending_for(&self, digest: &str, now_ms: i64) -> Option<&Pending> {
        self.pending.iter().find(|p| p.digest == digest && now_ms.saturating_sub(p.created_at) <= Self::PENDING_TTL_MS)
    }

    /// The newest pending request that is still fresh.
    pub fn latest_pending(&self, now_ms: i64) -> Option<&Pending> {
        self.pending.iter().filter(|p| now_ms.saturating_sub(p.created_at) <= REQUEST_TTL_MS).max_by_key(|p| p.created_at)
    }

    /// The person this install acts for in `workspace_id`, if it was linked
    /// there.
    pub fn linked_in(&self, workspace_id: Uuid) -> Option<&ActorIdentity> {
        self.person.as_ref().filter(|_| self.workspaces.contains(&workspace_id))
    }

    /// Record a redeemed link: the install is `person`'s device in
    /// `workspace_id`, and every pending request is used up (the device they
    /// named is either the one now linked, or unused). Refused when the
    /// install is already someone else's device.
    pub fn record_link(&mut self, person: &ActorIdentity, workspace_id: Uuid) -> Result<(), LinkError> {
        if let Some(p) = &self.person {
            if p.account_id != person.account_id {
                return Err(LinkError::Refused(format!(
                    "this computer is already a device of {}: it can't be someone else's too (use another data dir)",
                    crate::display::shown(&p.display_name)
                )));
            }
        }
        self.person = Some(device_local_cleared(person));
        if !self.workspaces.contains(&workspace_id) {
            self.workspaces.push(workspace_id);
        }
        self.pending.clear();
        Ok(())
    }
}

/// What the guided setup engine (`ligus_runtime::setup`, flow `device`)
/// registers. Until it lands, the CLI, the desktop and lt call the steps
/// directly: [`LinkRequest::new`] and [`LinkState::add_pending`] (request),
/// the person's approval on their desktop, then [`Grant::open`],
/// [`OpenedGrant::check`] and [`crate::ChatService::check_computer_link`]
/// (redeem).
pub mod setup {
    /// The flow id (`ligus setup device`).
    pub const FLOW_ID: &str = "device";
    /// The palette label.
    pub const TITLE: &str = "Add a computer";
    /// One line.
    pub const SUMMARY: &str = "Link this computer to your devices, so it acts as you (not a separate member).";
    /// The (i).
    pub const WHY: &str = "Joining with an invite makes a new member. Linking makes this computer one of your devices: \
        your desktop signs it with your passkey, and the workspace key is sealed to this computer.";
    /// The steps, in order: (id, title, a person's step).
    pub const STEPS: &[(&str, &str, bool)] = &[
        ("request", "Show this computer's link request", false),
        ("approve", "On your desktop: My devices > Add a computer, compare, confirm with your passkey", true),
        ("redeem", "Paste the code your desktop shows", false),
        ("confirm", "Check who approved it: the person, the workspace, the desktop's fingerprint", true),
        ("verify", "This computer acts as you", false),
    ];
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

mod b64 {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s.as_bytes()).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> (SigningKeypair, ligus_core::KeyAgreementKeypair) {
        (SigningKeypair::generate().unwrap(), ligus_core::KeyAgreementKeypair::generate().unwrap())
    }

    /// The prefixes and signature contexts are wire ids: pinned as hex so a
    /// product rename can't change them.
    #[test]
    fn link_constants_are_frozen() {
        let h = |b: &[u8]| hex(b);
        assert_eq!(h(REQUEST_TAG), "6c696775732d6c696e6b2d726571756573742d763100");
        assert_eq!(h(PREVIOUS_TAG), "6c696775732d6c696e6b2d70726576696f75732d763100");
        assert_eq!(h(GRANT_TAG), "6c696775732d6c696e6b2d6772616e742d763100");
        assert_eq!(REQUEST_PREFIX, "liguslink1:");
        assert_eq!(GRANT_PREFIX, "ligusgrant1:");
    }

    #[test]
    fn a_request_round_trips_and_shows_the_device_fingerprint() {
        let (d, ka) = keys();
        let r = LinkRequest::new(&d, &ka.public_key_bytes(), "  lt on \u{202E}mbp\n ", 1_000, None);
        assert_eq!(r.label, "lt on mbp", "hidden and reordering characters are dropped");
        let code = r.encode();
        let wrapped = format!("{}\n   {}", &code[..30], &code[30..]);
        let back = LinkRequest::decode(&wrapped).unwrap();
        assert_eq!(back, r);
        assert_eq!(back.fingerprint(), crate::device_enroll::fingerprint(&d.public_key_bytes()));
        assert_eq!(back.digest(), r.digest());
    }

    /// T1: every field is under the device key's signature.
    #[test]
    fn a_tampered_request_is_refused() {
        let (d, ka) = keys();
        let (_, other_ka) = keys();
        let r = LinkRequest::new(&d, &ka.public_key_bytes(), "lt", 1_000, None);
        let reencode = |m: &LinkRequest| format!("{REQUEST_PREFIX}{}", URL_SAFE_NO_PAD.encode(serde_json::to_vec(m).unwrap()));
        let mut m = r.clone();
        m.ka_public = other_ka.public_key_bytes().to_vec();
        assert_eq!(LinkRequest::decode(&reencode(&m)), Err(LinkError::BadSignature), "a swapped key-agreement key");
        let mut m = r.clone();
        m.label = "Alex's MacBook".into();
        assert_eq!(LinkRequest::decode(&reencode(&m)), Err(LinkError::BadSignature), "a relabel");
        let mut m = r.clone();
        m.created_at += 1;
        assert_eq!(LinkRequest::decode(&reencode(&m)), Err(LinkError::BadSignature), "another time");
        let mut m = r.clone();
        m.device_public_key = SigningKeypair::generate().unwrap().public_key_bytes().to_vec();
        assert_eq!(LinkRequest::decode(&reencode(&m)), Err(LinkError::BadSignature), "someone else's key");
        // Another id, signed by the key all the same: not the id it derives.
        let mut m = r.clone();
        m.device_id = Uuid::new_v4();
        m.signature = d.sign(&m.preimage()).to_vec();
        assert_eq!(LinkRequest::decode(&reencode(&m)), Err(LinkError::WrongDeviceId));
        // A version this build doesn't know.
        let mut m = r.clone();
        m.v = 2;
        m.signature = d.sign(&m.preimage()).to_vec();
        assert_eq!(LinkRequest::decode(&reencode(&m)), Err(LinkError::Version(2)));
        assert_eq!(LinkRequest::decode("hivews1:abc"), Err(LinkError::NotARequest));
        assert_eq!(LinkRequest::decode("liguslink1:abc"), Err(LinkError::Malformed));
    }

    #[test]
    fn a_request_is_fresh_for_an_hour() {
        let (d, ka) = keys();
        let r = LinkRequest::new(&d, &ka.public_key_bytes(), "lt", 10_000_000, None);
        assert!(r.check_fresh(10_000_000 + REQUEST_TTL_MS).is_ok());
        assert_eq!(r.check_fresh(10_000_000 + REQUEST_TTL_MS + 1), Err(LinkError::RequestExpired));
        assert_eq!(r.check_fresh(10_000_000 - CLOCK_SKEW_MS - 1), Err(LinkError::RequestFromTheFuture));
    }

    #[test]
    fn the_previous_member_proof_verifies_only_under_that_accounts_key() {
        let (d, ka) = keys();
        let old = SigningKeypair::generate().unwrap();
        let acct = Uuid::new_v4();
        let r = LinkRequest::new(&d, &ka.public_key_bytes(), "lt", 5, Some((acct, &old)));
        let back = LinkRequest::decode(&r.encode()).unwrap();
        assert!(back.previous_verifies(&old.public_key_bytes()));
        assert!(!back.previous_verifies(&SigningKeypair::generate().unwrap().public_key_bytes()));
        // Moved onto another request, the proof no longer verifies.
        let (d2, _) = keys();
        let mut other = LinkRequest::new(&d2, &ka.public_key_bytes(), "lt", 5, None);
        other.previous = r.previous.clone();
        other.signature = d2.sign(&other.preimage()).to_vec();
        assert!(!other.previous_verifies(&old.public_key_bytes()));
    }

    fn person(account_id: Uuid) -> ActorIdentity {
        let mut actor = ActorIdentity::new(account_id.to_string(), "Alex", ActorKind::Human);
        actor.account_id = Some(account_id);
        actor.key_agreement_public = Some(vec![7; 32]);
        actor.contact = Some("desktop-contact".into());
        actor.device_id = Some(Uuid::new_v4());
        actor
    }

    fn conn(ws_room: &str) -> String {
        crate::workspace_invite::encode_workspace_invite(&crate::workspace_invite::WorkspaceConn {
            name: "Acme".into(),
            relay_url: "https://relay.example".into(),
            room: ws_room.into(),
            key: Some("k".into()),
            icon: None,
            dm_account: None,
            founder_actor_id: None,
            fk: None,
            inv: None,
            ek: Vec::new(),
            sig: None,
        })
    }

    /// T2: a grant opens only on the computer that asked; it names that
    /// request; tampering breaks its signature; it expires.
    #[test]
    fn a_grant_opens_only_on_the_requesting_computer_and_checks_out() {
        let (d, ka) = keys();
        let (_, other_ka) = keys();
        let r = LinkRequest::new(&d, &ka.public_key_bytes(), "lt", 5, None);
        let ws = ligus_core::room_workspace_id("acme");
        let acct = Uuid::new_v4();
        let approver = SigningKeypair::generate().unwrap();
        let approver_id = device_id_for(SigAlg::Ed25519, &approver.public_key_bytes());
        let code = Grant::seal(&r, ws, &conn("acme"), &person(acct), approver_id, &approver, 1_000).unwrap();
        assert!(!code.contains("relay.example"), "sealed");
        assert_eq!(Grant::open(&code, &other_ka).unwrap_err(), LinkError::NotForThisDevice);
        let g = Grant::open(&code, &ka).unwrap();
        assert_eq!(g.request, r.digest());
        assert_eq!(g.approver_fingerprint(), crate::device_enroll::fingerprint(&approver.public_key_bytes()));
        let p = g.check(&d.public_key_bytes(), 999).unwrap();
        assert_eq!(p.account_id, Some(acct));
        assert!(p.key_agreement_public.is_none() && p.contact.is_none() && p.device_id.is_none(), "the desktop's own fields aren't copied");
        assert_eq!(g.check(&d.public_key_bytes(), 1_001).unwrap_err(), LinkError::GrantExpired);
        // T4: another computer's key, refused.
        let (d2, _) = keys();
        assert!(matches!(g.check(&d2.public_key_bytes(), 999), Err(LinkError::Refused(_))));
        // Not a person.
        let mut agent = person(acct);
        agent.kind = ActorKind::Agent;
        let code2 = Grant::seal(&r, ws, &conn("acme"), &agent, approver_id, &approver, 1_000).unwrap();
        assert!(matches!(Grant::open(&code2, &ka).unwrap().check(&d.public_key_bytes(), 1), Err(LinkError::Refused(_))));
        // A connection for another workspace than the grant's.
        let code4 = Grant::seal(&r, ws, &conn("other"), &person(acct), approver_id, &approver, 1_000).unwrap();
        assert!(matches!(Grant::open(&code4, &ka).unwrap().check(&d.public_key_bytes(), 1), Err(LinkError::Refused(_))));
    }

    #[test]
    fn a_tampered_grant_fails_its_signature() {
        let (d, ka) = keys();
        let r = LinkRequest::new(&d, &ka.public_key_bytes(), "lt", 5, None);
        let ws = ligus_core::room_workspace_id("acme");
        let approver = SigningKeypair::generate().unwrap();
        let approver_id = device_id_for(SigAlg::Ed25519, &approver.public_key_bytes());
        let mut body = GrantBody {
            v: VERSION,
            request: r.digest(),
            workspace_id: ws,
            conn: conn("acme"),
            device_id: r.device_id,
            person: serde_json::to_string(&person(Uuid::new_v4())).unwrap(),
            approver_device_id: approver_id,
            approver_key: approver.public_key_bytes().to_vec(),
            expires_at: 1_000,
            signature: Vec::new(),
        };
        body.signature = approver.sign(&body.preimage()).to_vec();
        body.person = serde_json::to_string(&person(Uuid::new_v4())).unwrap();
        let blob = ligus_core::e2ee::seal(&ka.public_key_bytes(), &serde_json::to_vec(&body).unwrap()).unwrap();
        let code = format!("{GRANT_PREFIX}{}", URL_SAFE_NO_PAD.encode(serde_json::to_vec(&blob).unwrap()));
        assert_eq!(Grant::open(&code, &ka).unwrap_err(), LinkError::BadGrantSignature, "another person");
    }

    #[test]
    fn pending_requests_are_single_use_and_one_person_per_install() {
        let dir = tempfile::tempdir().unwrap();
        let (d, ka) = keys();
        let r = LinkRequest::new(&d, &ka.public_key_bytes(), "lt", 1_000, None);
        let mut s = LinkState::load(dir.path()).unwrap();
        s.add_pending(&r, 1_000);
        s.add_pending(&r, 1_000);
        assert_eq!(s.pending.len(), 1);
        s.save(dir.path()).unwrap();
        let mut s = LinkState::load(dir.path()).unwrap();
        assert!(s.pending_for(&r.digest(), 1_000).is_some());
        assert!(s.pending_for(&r.digest(), 1_000 + REQUEST_TTL_MS + 1).is_some(), "approved late, the grant still has its life");
        assert!(s.pending_for(&r.digest(), 1_000 + LinkState::PENDING_TTL_MS + 1).is_none(), "stale");
        let mut alex = ActorIdentity::new("a", "Alex", ActorKind::Human);
        alex.account_id = Some(Uuid::new_v4());
        let ws = Uuid::new_v4();
        s.record_link(&alex, ws).unwrap();
        assert!(s.pending.is_empty(), "used up");
        assert_eq!(s.linked_in(ws).map(|p| p.display_name.as_str()), Some("Alex"));
        assert!(s.linked_in(Uuid::new_v4()).is_none());
        let mut sam = ActorIdentity::new("s", "Sam", ActorKind::Human);
        sam.account_id = Some(Uuid::new_v4());
        assert!(s.record_link(&sam, Uuid::new_v4()).is_err(), "one person per install");
        std::fs::write(LinkState::path(dir.path()), b"{not json").unwrap();
        assert!(LinkState::load(dir.path()).is_err(), "a damaged file is an error, never 'no one'");
    }
}

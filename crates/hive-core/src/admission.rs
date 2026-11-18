//! Box admissions (Phase B): how a `ligus serve` or `ligus worker` box joins a
//! workspace as its **own service member**, without a person at the box.
//!
//! An Admin issues a single-use invite whose `JoinApproved` carries an
//! [`Admission`]: it admits only a member of kind `Service`, at most
//! Contributor, until `expires_ms`, and (when the box first sent a join
//! request) only the account that request named. Issuing it is a
//! membership change, so it needs the Admin's passkey presence like every
//! invite; a phone may issue one with its own key (the phone tier allows
//! nothing else about membership).
//!
//! The box redeems it by adding itself (`MemberAdded { invite, proof }`),
//! signed by its own device: the redemption binds the invite to that key, and
//! the invite is spent (`GuardState::used_invites`). The invite id is on the
//! log for every member to read, so it is not what admits the box: the
//! admission names a **redeem key** whose secret half rides only in the
//! admission code, and the redemption carries that key's signature over the
//! workspace, the invite, the new member and the redeeming device
//! ([`redemption_message`]). Another member, who can read the invite id and
//! write to the log, can't redeem it in the box's place, before or after it.
//!
//! The workspace key never rides the event log: it reaches the box only
//! inside the invite code (sealed to the box's key-agreement key when the
//! box sent a request) or through a later rotation sealed to the box's
//! published key.
//!
//! **Expiry and closing.** Every time on the log (lamport, timestamp, event
//! id) is chosen by its author, so a redeemer holding the code can date its
//! redemption before `expires_ms` and slot it right after the issue: no rule
//! over the redemption alone bounds it. What bounds it is an honest Admin's
//! later word: [`SessionEvent::AdmissionClosed`] names the one redemption the
//! closer had seen (or none), and the fold denies every other redemption of
//! that invite **wherever it sorts** (a second pass, as for cuts). An Admin's
//! device closes an admission as soon as it sees it redeemed in time, and
//! once it lapses; after that, neither a backdated redemption nor one a
//! thief of the code slots in before the box's can admit anyone.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::events::{SessionEvent, SessionEventEnvelope};
use crate::identity::{ActorKind, WorkspaceMember, WorkspaceRole};
use crate::session::ChatSession;

/// The longest an admission may stay redeemable after it was issued.
pub const MAX_ADMISSION_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// The terms of a box admission (the optional part of a `JoinApproved`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Admission {
    /// The kind of member it admits. Only `service` is defined; any other
    /// kind is refused, so an older build's reading can't widen it.
    pub kind: ActorKind,
    /// Unix milliseconds after which a redemption no longer counts.
    pub expires_ms: i64,
    /// When set, only this account may redeem it (the account the box's
    /// join request named). Left out when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<Uuid>,
    /// The Ed25519 public key whose secret rides only in the admission code:
    /// a redemption carries its signature ([`redemption_message`]). Required
    /// of every admission; left out when absent (none is then redeemable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redeem_key: Option<Vec<u8>>,
    /// When set, only a redemption signed by this Ed25519 device key counts:
    /// the key of the box whose join request it was issued for. Unlike the
    /// account id (any keyholder can claim one first), only that box can sign
    /// with it, so whoever else learns the code (the serve box that relayed a
    /// phone's admission, say) can't take the admission. Left out when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_key: Option<Vec<u8>>,
}

impl Admission {
    /// A service admission until `expires_ms`, optionally bound to `account`,
    /// redeemable with the secret of `redeem_key`.
    pub fn service(expires_ms: i64, account_id: Option<Uuid>, redeem_key: [u8; 32]) -> Self {
        Self { kind: ActorKind::Service, expires_ms, account_id, redeem_key: Some(redeem_key.to_vec()), device_key: None }
    }

    /// This admission, redeemable only through the device whose signing key
    /// is `device_key` (the box's, from its join request).
    pub fn for_device(mut self, device_key: &[u8]) -> Self {
        self.device_key = Some(device_key.to_vec());
        self
    }
}

/// The signing context of a redemption proof. Frozen: never rename it.
pub const REDEEM_CONTEXT: &[u8] = b"ligus-box-admission-redeem-v1\0";

/// What a redemption's proof signs: the workspace, the invite, the member it
/// adds and the device that signs the redemption, so the proof admits that
/// member through that device only.
pub fn redemption_message(workspace_id: Uuid, invite: Uuid, member_id: &str, device_id: Uuid) -> Vec<u8> {
    let mut m = Vec::with_capacity(REDEEM_CONTEXT.len() + 52 + member_id.len());
    m.extend_from_slice(REDEEM_CONTEXT);
    m.extend_from_slice(workspace_id.as_bytes());
    m.extend_from_slice(invite.as_bytes());
    m.extend_from_slice(&(member_id.len() as u32).to_le_bytes());
    m.extend_from_slice(member_id.as_bytes());
    m.extend_from_slice(device_id.as_bytes());
    m
}

/// The secret half of an admission's redeem key (the issuer puts its seed in
/// the admission code and keeps nothing).
pub struct RedeemKey(crate::crypto::SigningKeypair);

impl RedeemKey {
    pub fn generate() -> Result<Self, crate::crypto::CryptoError> {
        Ok(Self(crate::crypto::SigningKeypair::generate()?))
    }
    pub fn from_seed(seed: &[u8]) -> Result<Self, crate::crypto::CryptoError> {
        Ok(Self(crate::crypto::SigningKeypair::from_seed(seed)?))
    }
    pub fn seed(&self) -> [u8; 32] {
        self.0.seed_bytes()
    }
    pub fn public(&self) -> [u8; 32] {
        self.0.public_key_bytes()
    }
    /// The proof that redeems `invite` for `member_id` through `device_id`.
    pub fn prove(&self, workspace_id: Uuid, invite: Uuid, member_id: &str, device_id: Uuid) -> Vec<u8> {
        self.0.sign(&redemption_message(workspace_id, invite, member_id, device_id)).to_vec()
    }
}

/// Unix milliseconds of an envelope's signed timestamp.
pub fn envelope_ms(env: &SessionEventEnvelope) -> i64 {
    let t = env.timestamp.inner();
    (t.unix_timestamp_nanos() / 1_000_000) as i64
}

/// The invite id a `JoinApproved` subject issues, if it issues one.
pub fn invite_of_subject(subject: &str) -> Option<Uuid> {
    subject.strip_prefix("invite:").and_then(|s| Uuid::parse_str(s).ok())
}

/// The admission terms of invite `id`, if it is a box admission.
pub fn admission_of(fold: &ChatSession, id: Uuid) -> Option<&Admission> {
    fold.guards.service_invites.get(&id)
}

/// Why issuing `admission` under `subject` is refused, if it is: it names a
/// fresh invite, admits a service, and expires after it was issued and within
/// [`MAX_ADMISSION_TTL_MS`]. A plain `JoinApproved` may not re-issue a box
/// admission's invite as a person's.
pub fn issuance_refusal(env: &SessionEventEnvelope, fold: &ChatSession, on_config: bool) -> Option<&'static str> {
    let SessionEvent::JoinApproved { subject, admission } = &env.payload else { return None };
    let invite = invite_of_subject(subject);
    let Some(a) = admission else {
        return invite
            .filter(|id| fold.guards.service_invites.contains_key(id))
            .map(|_| "that invite is a box admission");
    };
    if !on_config {
        return Some("a box admission is issued on the workspace config log");
    }
    let Some(id) = invite else {
        return Some("a box admission names an invite");
    };
    if a.kind != ActorKind::Service {
        return Some("an admission admits only a service member");
    }
    if a.redeem_key.as_ref().is_none_or(|k| k.len() != 32) {
        return Some("an admission names a redeem key");
    }
    if a.device_key.as_ref().is_some_and(|k| k.len() != 32) {
        return Some("an admission's device key is malformed");
    }
    if fold.guards.join_approvals.contains_key(subject) || fold.guards.used_invites.contains(&id) {
        return Some("that invite already exists");
    }
    let issued = envelope_ms(env);
    if a.expires_ms <= issued || a.expires_ms - issued > MAX_ADMISSION_TTL_MS {
        return Some("an admission expires after it is issued and within 7 days");
    }
    None
}

/// Why a self-enrollment through `invite` is refused, if it is.
///
/// * A box admission admits a `Service` member, at most Contributor, signed
///   no later than it expires, by the account it names (if it names one).
/// * A `Service` member joins only through a box admission: a person's
///   invite never admits a box.
pub fn redemption_refusal(
    env: &SessionEventEnvelope,
    member: &WorkspaceMember,
    invite: Option<Uuid>,
    proof: Option<&[u8]>,
    fold: &ChatSession,
) -> Option<&'static str> {
    let author = env.actor_stamp.as_ref().map(|s| s.actor.id.as_str()).unwrap_or("");
    if member.id != author {
        // Not a self-enrollment: the role gate decides.
        return None;
    }
    let terms = invite.and_then(|id| admission_of(fold, id));
    if let Some(existing) = fold.members.iter().find(|m| m.id == member.id) {
        // A member re-adding itself never spends a box admission.
        if terms.is_some() {
            return Some("already a member: a box admission admits a new box");
        }
        // Nor changes what it is: a box never re-adds itself as a person (or
        // the reverse), and no one moves their membership to another account.
        if existing.actor.kind != member.actor.kind {
            return Some("a member's kind never changes when it re-adds itself");
        }
        if existing.actor.account_id.is_some() && existing.actor.account_id != member.actor.account_id {
            return Some("a member's account never changes when it re-adds itself");
        }
        // A service member's record is the one its admission bound: its
        // account (never filled in later) and its actor id stay put.
        if existing.actor.kind == ActorKind::Service
            && (existing.actor.account_id != member.actor.account_id || member.actor.id != member.id)
        {
            return Some("a service member's account and identity never change when it re-adds itself");
        }
        return None;
    }
    let Some(terms) = terms else {
        return (member.actor.kind == ActorKind::Service).then_some("a service member joins only through a box admission");
    };
    if member.actor.kind != terms.kind {
        return Some("a box admission admits only a service member");
    }
    if member.role.rank() > WorkspaceRole::Contributor.rank() {
        return Some("a service member holds at most Contributor");
    }
    if envelope_ms(env) > terms.expires_ms {
        return Some("that box admission has expired");
    }
    // The code's holder, through this device: the invite id alone (on the
    // log for every member) admits no one.
    let (Some(id), Some(key), Some(device), Some(proof)) = (invite, terms.redeem_key.as_deref(), env.signer_device_id, proof) else {
        return Some("a box admission is redeemed with its code's proof");
    };
    if fold.guards.closed_admissions.contains_key(&id) {
        return Some("that box admission was closed");
    }
    if crate::crypto::verify(key, &redemption_message(env.workspace_id, id, &member.id, device), proof).is_err() {
        return Some("that redemption's proof doesn't verify");
    }
    if let Some(key) = terms.device_key.as_deref() {
        if crate::crypto::verify_envelope(env, key).is_err() {
            return Some("that box admission is for another box");
        }
    }
    // The new member's record names the account of the device that redeems
    // it (the stamp's, which the signer's certificate vouches for) and its
    // own id, bound or bearer: a bearer code doesn't let a box join under a
    // person's account, or none to fill in later.
    let stamped = env.actor_stamp.as_ref().and_then(|s| s.actor.account_id);
    if stamped.is_none() || member.actor.account_id != stamped || member.actor.id != member.id {
        return Some("a box joins under its own account");
    }
    if let Some(account) = terms.account_id {
        if stamped != Some(account) {
            return Some("that box admission is for another box");
        }
    }
    None
}

/// Why closing an admission (`AdmissionClosed`) is refused, if it is: on the
/// config log, of an admission issued here, naming (if anything) a
/// redemption of that very invite.
pub fn closure_refusal(env: &SessionEventEnvelope, fold: &ChatSession, on_config: bool) -> Option<&'static str> {
    let SessionEvent::AdmissionClosed { invite, .. } = &env.payload else { return None };
    if !on_config {
        return Some("an admission is closed on the workspace config log");
    }
    if !fold.guards.service_invites.contains_key(invite) {
        return Some("no such box admission");
    }
    None
}

/// The redemptions the allowed closures deny, wherever they sort: every
/// self-enrollment through a closed invite but the one each closure names.
/// `closures` are the allowed closures of a first pass (invite → each
/// closure's named redemption).
pub fn closure_denials(
    envelopes: &[SessionEventEnvelope],
    closures: &std::collections::BTreeMap<Uuid, Vec<Option<Uuid>>>,
) -> std::collections::BTreeSet<Uuid> {
    let mut out = std::collections::BTreeSet::new();
    if closures.is_empty() {
        return out;
    }
    for e in envelopes {
        let SessionEvent::MemberAdded { member, invite: Some(id), .. } = &e.payload else { continue };
        let author = e.actor_stamp.as_ref().map(|s| s.actor.id.as_str()).unwrap_or("");
        if member.id != author {
            continue;
        }
        if let Some(named) = closures.get(id) {
            if named.iter().any(|n| *n != Some(e.event_id)) {
                out.insert(e.event_id);
            }
        }
    }
    out
}

/// What a phone checks before signing the preimage a box hands it for a box
/// admission (`POST /v1/admissions/presence`): the box drafts the event, so
/// the bytes are the box's word until the phone has read them. The preimage
/// must be a `hive-envelope-v3` preimage of a `JoinApproved` box admission on
/// `workspace_id`'s config log, stamped for this phone (`phone`, P-256), whose
/// presence digest is `challenge`, the one the person's passkey just signed.
/// Anything else (a vote, a post, an answer, another workspace, another
/// event than the passkey confirmed) is refused. `device_key` is the device
/// key of the join request the phone itself pasted (none for a bearer
/// admission): the admission must be bound to exactly that. Returns the
/// invite and the admission's terms for the phone to show.
pub fn check_phone_admission_preimage(
    preimage: &[u8],
    challenge: &[u8; 32],
    workspace_id: Uuid,
    phone: Uuid,
    device_key: Option<&[u8]>,
) -> Result<(Uuid, Admission), &'static str> {
    use sha2::{Digest, Sha256};
    const PREFIX: &[u8] = b"hive-envelope-v3\0";
    let mut rest = preimage.strip_prefix(PREFIX).ok_or("not an envelope preimage")?;
    let mut take = |n: usize| -> Result<&[u8], &'static str> {
        if rest.len() < n {
            return Err("a truncated preimage");
        }
        let (a, b) = rest.split_at(n);
        rest = b;
        Ok(a)
    };
    let uuid = |b: &[u8]| Uuid::from_slice(b).map_err(|_| "a malformed preimage");
    let workspace = uuid(take(16)?)?;
    let session = uuid(take(16)?)?;
    let _sequence = take(8)?;
    let lamport = u64::from_le_bytes(take(8)?.try_into().map_err(|_| "a malformed preimage")?);
    let event_id = uuid(take(16)?)?;
    let mut fields: Vec<&[u8]> = Vec::with_capacity(4);
    for _ in 0..4 {
        let len = u32::from_le_bytes(take(4)?.try_into().map_err(|_| "a malformed preimage")?) as usize;
        fields.push(take(len)?);
    }
    if !rest.is_empty() {
        return Err("trailing bytes after the preimage");
    }
    let [_scope, _timestamp, stamp_bytes, payload] = [fields[0], fields[1], fields[2], fields[3]];
    if workspace != workspace_id || session != crate::channel::workspace_config_session_id(workspace) {
        return Err("not on this workspace's config log");
    }
    let stamp: Option<crate::identity::ActorStamp> = serde_json::from_slice(stamp_bytes).map_err(|_| "a malformed stamp")?;
    let stamp_raw: serde_json::Value = serde_json::from_slice(stamp_bytes).map_err(|_| "a malformed stamp")?;
    if serde_json::to_value(&stamp).ok().as_ref() != Some(&stamp_raw) {
        return Err("a stamp with fields this build doesn't read");
    }
    let stamp = stamp.ok_or("an unstamped event")?;
    if stamp.actor.device_id != Some(phone) || stamp.sig_alg != crate::crypto::SigAlg::P256 || stamp.presence.is_none() {
        return Err("not stamped for this phone with its person's presence");
    }
    let event: SessionEvent = serde_json::from_slice(payload).map_err(|_| "a malformed payload")?;
    // Only what this build reads: a field it doesn't know (one a newer build
    // might honour) is not something the person was shown.
    let raw: serde_json::Value = serde_json::from_slice(payload).map_err(|_| "a malformed payload")?;
    if serde_json::to_value(&event).ok().as_ref() != Some(&raw) {
        return Err("a payload with fields this build doesn't read");
    }
    let SessionEvent::JoinApproved { subject, admission: Some(terms) } = event else {
        return Err("not a box admission");
    };
    let invite = invite_of_subject(&subject).ok_or("not a box admission")?;
    if terms.kind != ActorKind::Service || terms.redeem_key.as_ref().is_none_or(|k| k.len() != 32) {
        return Err("not a box admission");
    }
    if terms.device_key.as_deref() != device_key {
        return Err("not bound to the box whose join request the phone holds");
    }
    // The presence digest (`presence::challenge`) over these very bytes.
    let frontier = stamp.cfg_frontier.map(|f| serde_json::to_vec(&f).unwrap_or_default()).unwrap_or_default();
    let account = stamp.actor.account_id.unwrap_or_else(Uuid::nil);
    let mut h = Sha256::new();
    h.update(crate::presence::PRESENCE_CONTEXT);
    h.update(workspace.as_bytes());
    h.update(session.as_bytes());
    h.update(event_id.as_bytes());
    h.update(lamport.to_le_bytes());
    h.update((payload.len() as u32).to_le_bytes());
    h.update(payload);
    h.update((frontier.len() as u32).to_le_bytes());
    h.update(&frontier);
    h.update(account.as_bytes());
    let digest: [u8; 32] = h.finalize().into();
    if &digest != challenge {
        return Err("not the event the passkey confirmed");
    }
    Ok((invite, terms))
}

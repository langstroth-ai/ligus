//! Client side of the relay v0.3 contact routes (`/v1/contacts/`; spec:
//! IDENTITY-AND-FORGES §3.2-3.3).
//!
//! A contact is an opaque record plus a sealed drop box, both keyed by a
//! random contact id (`cid`, 16 random bytes, base64url) the account chooses.
//! The relay does no cryptography here: the record is a bundle of objects
//! that already exist (account key, device certificates, signed key
//! publications) which the reader verifies, and a drop is a blob sealed
//! client-side to the owner's key-agreement keys. Only the holder of the
//! write key (32 random bytes, base64url) may change the record or read the
//! box.
//!
//! This module is the transport seam only: the typed calls and the "this
//! relay has no contacts" degrade. Publishing a verifiable record, polling the
//! box and the contact-card QR (CT-1) build on it. A relay without the routes
//! (v0.2) answers 404 to the owner's first `PUT`; the caller then offers
//! links and QR codes only and says "this relay doesn't support contacts".
//!
//! No call here, nor any other relay call, carries a forge token.

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::relay_client::RelayClient;

/// The header that carries the owner's write key.
pub const CONTACT_KEY_HEADER: &str = "X-Ligus-Contact-Key";

/// A fresh contact id: 16 random bytes, base64url without padding.
pub fn new_cid() -> String {
    random_b64(16)
}

/// A fresh write key: 32 random bytes, base64url without padding.
pub fn new_write_key() -> String {
    random_b64(32)
}

fn random_b64(n: usize) -> String {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).expect("OS randomness");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// The write key one relay sees: derived from the device's master write key
/// and the relay's base URL. Every relay sees its key in clear on each call,
/// so a single shared key would let any one relay read, acknowledge (delete)
/// or overwrite the same contact on every other relay. The master key never
/// leaves the device.
pub fn relay_write_key(master: &str, relay_base: &str) -> String {
    let relay = relay_base.trim().trim_end_matches('/').to_ascii_lowercase();
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(None, master.as_bytes());
    let mut out = [0u8; 32];
    let info = format!("ligus-contact-write-key-v1\0{relay}");
    hk.expand(info.as_bytes(), &mut out).expect("32 bytes is a valid HKDF-SHA256 length");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(out)
}

/// Whether `s` is a well-formed contact id (base64url of exactly 16 bytes).
pub fn valid_cid(s: &str) -> bool {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).is_ok_and(|b| b.len() == 16)
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ContactsError {
    /// The relay has no contact routes (a v0.2 relay, or contacts turned off).
    #[error("this relay doesn't support contacts")]
    Unsupported,
    /// No live record for that contact id.
    #[error("no such contact")]
    NotFound,
    /// The write key is missing or wrong.
    #[error("the relay refused the contact key")]
    Unauthorized,
    #[error("relay returned HTTP {0}")]
    Status(u16),
    /// The relay's answer was larger than any honest one.
    #[error("the relay's answer is too large")]
    TooLarge,
    #[error("{0}")]
    Transport(String),
    /// A `429`/`507` the relay sent (a contact rate limit already retried in
    /// place, or `contacts_full`); see [`crate::relay_refusal`].
    #[error("{0}")]
    Refused(crate::relay_refusal::RelayRefusal),
}

impl From<reqwest::Error> for ContactsError {
    fn from(e: reqwest::Error) -> Self {
        Self::Transport(e.to_string())
    }
}

impl From<crate::relay_client::RelayError> for ContactsError {
    fn from(e: crate::relay_client::RelayError) -> Self {
        match e {
            crate::relay_client::RelayError::Refused(r) => Self::Refused(r),
            crate::relay_client::RelayError::Unauthorized => Self::Unauthorized,
            crate::relay_client::RelayError::Status(c) => status_err(c, false),
            other => Self::Transport(other.to_string()),
        }
    }
}

/// `GET /v1/contacts/{cid}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactRecordResponse {
    pub record: serde_json::Value,
    #[serde(default)]
    pub updated_at: i64,
}

/// `PUT /v1/contacts/{cid}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactPutResponse {
    #[serde(default)]
    pub created: bool,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub expires_at: i64,
}

/// One sealed blob in a drop box.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactDrop {
    pub seq: u64,
    pub blob: serde_json::Value,
    #[serde(default)]
    pub created_at: i64,
}

/// One page of a drop box.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ContactDropPage {
    #[serde(default)]
    pub items: Vec<ContactDrop>,
    #[serde(default)]
    pub more: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PutBody<'a> {
    record: &'a serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    write_key: Option<&'a str>,
}

/// The largest record read accepted (a record is a few certificates, key
/// publications and at most a handful of links: a few KiB).
pub const MAX_RECORD_BODY: usize = 256 * 1024;
/// The largest drop-box page accepted.
pub const MAX_DROP_PAGE_BODY: usize = 4 * 1024 * 1024;
/// The largest answer to a write or delete.
const MAX_SMALL_BODY: usize = 64 * 1024;

/// Read a response body up to `cap` bytes and parse it as JSON; a longer
/// body is refused without being read to the end.
async fn json_capped<T: serde::de::DeserializeOwned>(mut resp: reqwest::Response, cap: usize) -> Result<T, ContactsError> {
    if resp.content_length().is_some_and(|n| n as usize > cap) {
        return Err(ContactsError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if body.len() + chunk.len() > cap {
            return Err(ContactsError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|e| ContactsError::Transport(format!("malformed answer: {e}")))
}

fn status_err(code: u16, unsupported_on_404: bool) -> ContactsError {
    match code {
        404 | 405 if unsupported_on_404 => ContactsError::Unsupported,
        404 => ContactsError::NotFound,
        401 | 403 => ContactsError::Unauthorized,
        c => ContactsError::Status(c),
    }
}

impl RelayClient {
    fn contact_url(&self, cid: &str, tail: &str) -> Result<String, ContactsError> {
        if !valid_cid(cid) {
            return Err(ContactsError::Status(400));
        }
        Ok(format!("{}/v1/contacts/{cid}{tail}", self.base_url()))
    }

    /// Create or replace this account's record. The first write sets the
    /// write key (sent in the body when `first`); every write presents it in
    /// the header. A 404 here means the relay has no contact routes.
    pub async fn contact_put(
        &self,
        cid: &str,
        write_key: &str,
        record: &serde_json::Value,
        first: bool,
    ) -> Result<ContactPutResponse, ContactsError> {
        let url = self.contact_url(cid, "")?;
        let write_key = &relay_write_key(write_key, self.base_url());
        let body = PutBody { record, write_key: first.then_some(write_key.as_str()) };
        let resp = self.send_retrying(|| self.http_client().put(&url).header(CONTACT_KEY_HEADER, write_key).json(&body)).await?;
        let code = resp.status().as_u16();
        if !resp.status().is_success() {
            return Err(status_err(code, true));
        }
        json_capped(resp, MAX_SMALL_BODY).await
    }

    /// Read someone's record. `Ok(None)` when there is none (or the relay
    /// has no contact routes: a reader can't tell, and needn't).
    pub async fn contact_get(&self, cid: &str) -> Result<Option<ContactRecordResponse>, ContactsError> {
        let url = self.contact_url(cid, "")?;
        let resp = self.send_retrying(|| self.http_client().get(&url)).await?;
        match resp.status().as_u16() {
            200..=299 => Ok(Some(json_capped(resp, MAX_RECORD_BODY).await?)),
            404 => Ok(None),
            c => Err(status_err(c, false)),
        }
    }

    /// Delete this account's record and its drop box.
    pub async fn contact_delete(&self, cid: &str, write_key: &str) -> Result<(), ContactsError> {
        let url = self.contact_url(cid, "")?;
        let write_key = relay_write_key(write_key, self.base_url());
        let resp = self.send_retrying(|| self.http_client().delete(&url).header(CONTACT_KEY_HEADER, &write_key)).await?;
        if !resp.status().is_success() {
            return Err(status_err(resp.status().as_u16(), false));
        }
        Ok(())
    }

    /// Drop a blob (already sealed to the owner's key-agreement keys) into
    /// someone's box.
    pub async fn contact_drop(&self, cid: &str, blob: &serde_json::Value) -> Result<(), ContactsError> {
        let url = self.contact_url(cid, "/drop")?;
        let body = serde_json::json!({ "blob": blob });
        let resp = self.send_retrying(|| self.http_client().post(&url).json(&body)).await?;
        if !resp.status().is_success() {
            return Err(status_err(resp.status().as_u16(), false));
        }
        Ok(())
    }

    /// Read this account's box past `after` (non-destructive).
    pub async fn contact_drops(&self, cid: &str, write_key: &str, after: u64) -> Result<ContactDropPage, ContactsError> {
        let url = self.contact_url(cid, &format!("/drop?after={after}"))?;
        let write_key = relay_write_key(write_key, self.base_url());
        let resp = self.send_retrying(|| self.http_client().get(&url).header(CONTACT_KEY_HEADER, &write_key)).await?;
        if !resp.status().is_success() {
            return Err(status_err(resp.status().as_u16(), false));
        }
        json_capped(resp, MAX_DROP_PAGE_BODY).await
    }

    /// Read this account's whole box, page by page (at most 64 pages; a
    /// relay that doesn't advance the seqs stops the read).
    pub async fn contact_drops_all(&self, cid: &str, write_key: &str) -> Result<Vec<ContactDrop>, ContactsError> {
        let mut out: Vec<ContactDrop> = Vec::new();
        let mut after = 0u64;
        for _ in 0..64 {
            let page = self.contact_drops(cid, write_key, after).await?;
            let next = page.items.iter().map(|d| d.seq).max().unwrap_or(after);
            out.extend(page.items.into_iter().filter(|d| d.seq > after));
            if !page.more || next <= after {
                break;
            }
            after = next;
        }
        Ok(out)
    }

    /// Delete every drop up to and including `through` from this account's
    /// box.
    pub async fn contact_drops_ack(&self, cid: &str, write_key: &str, through: u64) -> Result<(), ContactsError> {
        let url = self.contact_url(cid, &format!("/drop?through={through}"))?;
        let write_key = relay_write_key(write_key, self.base_url());
        let resp = self.send_retrying(|| self.http_client().delete(&url).header(CONTACT_KEY_HEADER, &write_key)).await?;
        if !resp.status().is_success() {
            return Err(status_err(resp.status().as_u16(), false));
        }
        Ok(())
    }
}


// ── The record (spec §3.2): a bundle of objects that already exist ─────────

/// The longest a record may claim to be valid.
pub const RECORD_MAX_LIFETIME_DAYS: i64 = 30;

/// One device in a contact record.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordDevice {
    /// `hive-device-cert-v1`, signed by the account key.
    pub certificate: hive_core::crypto::DeviceCertificate,
    /// `DeviceKeyAgreementPublished`, signed by that device
    /// (`hive-envelope-v3`), in the contact's workspace.
    pub ka_publication: hive_core::SessionEventEnvelope,
}

/// The account a record speaks for.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordAccount {
    pub account_id: uuid::Uuid,
    /// base64url (no padding) of the 32-byte Ed25519 account key.
    pub signing_public_key: String,
}

/// A contact record (`v: 1`). `profile` is left out until a signed profile
/// event exists; readers ignore it. `links` carries the identity links the
/// account chose to make discoverable by this contact (opt-in per link):
/// `IdentityLinkPublished` envelopes signed by one of the record's devices in
/// the contact's workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactRecord {
    pub v: u32,
    pub cid: String,
    pub account: RecordAccount,
    pub devices: Vec<RecordDevice>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<hive_core::SessionEventEnvelope>,
    pub issued_at: hive_core::Timestamp,
    pub expires_at: hive_core::Timestamp,
}

/// The workspace id a contact's envelopes carry.
pub fn contact_workspace_id(cid: &str) -> uuid::Uuid {
    hive_core::room_workspace_id(&format!("contact-{cid}"))
}

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// Build this account's record from its key, this device's certificate and
/// a key publication this device signed for the contact's workspace.
pub fn build_record(
    cid: &str,
    account_id: uuid::Uuid,
    account_public_key: &[u8],
    devices: Vec<RecordDevice>,
    now: hive_core::Timestamp,
) -> ContactRecord {
    let expires = hive_core::Timestamp(now.0 + time::Duration::days(RECORD_MAX_LIFETIME_DAYS));
    ContactRecord {
        v: 1,
        cid: cid.to_string(),
        account: RecordAccount { account_id, signing_public_key: b64(account_public_key) },
        devices,
        links: Vec::new(),
        issued_at: now,
        expires_at: expires,
    }
}

/// The most identity links a record carries.
pub const RECORD_MAX_LINKS: usize = hive_core::identity_link::MAX_LIVE_LINKS;

/// A record that passed every check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedContact {
    pub account_id: uuid::Uuid,
    /// Hex of the first 16 bytes of SHA-256 of the account key (`fk`).
    pub fingerprint: String,
    /// The devices' key-agreement keys (device id, key).
    pub devices: Vec<crate::directory_trust::Recipient>,
    pub issued_at: hive_core::Timestamp,
    /// The account key matched the `fk` the person got out of band (or an
    /// account already pinned from a shared workspace). Otherwise sealing to
    /// it needs the person's confirmation.
    pub anchored: bool,
    /// The identity links the record carries that passed the offline
    /// envelope checks (signed by one of its devices, for its account, bound
    /// to the contact's workspace and key). Whether each verifies is still
    /// for `identity_links::check_link`; a link never anchors the contact.
    pub links: Vec<hive_core::identity_link::IdentityLink>,
}

/// Check a record fetched for `cid` (§3.2): every certificate verifies under
/// the account key and names that account, every key publication verifies
/// under its certified device key, names that device and the contact's
/// workspace, the record is current (not expired, not issued in the future,
/// at most 30 days long) and not older than the newest one seen for this
/// `cid` (`newest_seen`, kept locally: anti-rollback). `expected_fk` is the
/// out-of-band anchor; a mismatch is not an error but an unanchored result.
pub fn verify_record(
    record: &serde_json::Value,
    cid: &str,
    expected_fk: Option<&str>,
    newest_seen: Option<hive_core::Timestamp>,
    now: hive_core::Timestamp,
) -> Result<VerifiedContact, String> {
    let rec: ContactRecord = serde_json::from_value(record.clone()).map_err(|e| format!("malformed contact record: {e}"))?;
    if rec.v != 1 {
        return Err(format!("unsupported contact record version {}", rec.v));
    }
    if rec.cid != cid {
        return Err("the record names another contact id".into());
    }
    let skew = time::Duration::minutes(5);
    if rec.expires_at <= now {
        return Err("the contact record expired".into());
    }
    if rec.issued_at.0 > now.0 + skew {
        return Err("the contact record is from the future".into());
    }
    if rec.expires_at.0 - rec.issued_at.0 > time::Duration::days(RECORD_MAX_LIFETIME_DAYS) + skew {
        return Err("the contact record claims too long a lifetime".into());
    }
    if newest_seen.is_some_and(|n| rec.issued_at < n) {
        return Err("an older contact record than one already seen (rollback)".into());
    }
    let key = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(rec.account.signing_public_key.as_bytes())
        .ok()
        .filter(|k| k.len() == 32)
        .ok_or("the record's account key is malformed")?;
    let workspace = contact_workspace_id(cid);
    let mut devices = Vec::new();
    for d in &rec.devices {
        if d.certificate.account_id != rec.account.account_id {
            return Err("a device certificate names another account".into());
        }
        d.certificate.verify(&key).map_err(|_| "a device certificate doesn't verify".to_string())?;
        let env = &d.ka_publication;
        if env.workspace_id != workspace {
            return Err("a key publication belongs to another contact".into());
        }
        hive_core::crypto::verify_envelope(env, &d.certificate.device_public_key)
            .map_err(|_| "a key publication doesn't verify".to_string())?;
        match &env.payload {
            hive_core::SessionEvent::DeviceKeyAgreementPublished { device_id, ka_public } if *device_id == d.certificate.device_id => {
                let k: [u8; 32] = ka_public.as_slice().try_into().map_err(|_| "a key-agreement key is malformed".to_string())?;
                devices.push(crate::directory_trust::Recipient { device_id: device_id.to_string(), ka_public: k });
            }
            _ => return Err("a key publication names another device".into()),
        }
    }
    let fingerprint = crate::envelope_verifier::FounderPin::fingerprint_hex(&key);
    let anchored = expected_fk.is_some_and(|f| f.trim().eq_ignore_ascii_case(&fingerprint));
    // Links: a bad one is dropped, never the contact.
    let mut links = Vec::new();
    for env in rec.links.iter().take(RECORD_MAX_LINKS) {
        let hive_core::SessionEvent::IdentityLinkPublished { link } = &env.payload else { continue };
        let Some(device) = rec.devices.iter().find(|d| Some(d.certificate.device_id) == env.signer_device_id) else { continue };
        let ok = env.workspace_id == workspace
            && link.account_id == rec.account.account_id
            && env.actor_stamp.as_ref().is_some_and(|s| s.actor.account_id == Some(link.account_id) && s.actor.kind == hive_core::ActorKind::Human)
            && hive_core::crypto::verify_envelope(env, &device.certificate.device_public_key).is_ok()
            && hive_core::identity_link::check_shape(link).is_ok()
            && hive_core::identity_link::check_statement(link, workspace, Some(&key)).is_ok();
        if ok && !links.iter().any(|l: &hive_core::identity_link::IdentityLink| l.subject_key() == link.subject_key()) {
            links.push(link.clone());
        }
    }
    Ok(VerifiedContact { account_id: rec.account.account_id, fingerprint, devices, issued_at: rec.issued_at, anchored, links })
}

/// Read and check the record at `cid` (§3.2) the way every production read
/// does: [`RelayClient::contact_get`] (capped), [`verify_record`] against the
/// newest record this device has seen for the cid (kept in hive.db, so a
/// relay can't roll it back across restarts), then without the devices
/// `revoked` names (revoked on a log this device holds). The newest-seen
/// time is recorded on success. `Ok(None)`: no record there.
pub async fn fetch_verified_contact(
    client: &RelayClient,
    store: &crate::EventStore,
    cid: &str,
    expected_fk: Option<&str>,
    revoked: &dyn Fn(uuid::Uuid) -> bool,
) -> Result<Option<VerifiedContact>, String> {
    let Some(resp) = client.contact_get(cid).await.map_err(|e| e.to_string())? else { return Ok(None) };
    accept_contact_record(store, cid, &resp.record, expected_fk, revoked).map(Some)
}

/// The checks of [`fetch_verified_contact`] on a record already read (for
/// callers that can't hold the store across the read).
pub fn accept_contact_record(
    store: &crate::EventStore,
    cid: &str,
    record: &serde_json::Value,
    expected_fk: Option<&str>,
    revoked: &dyn Fn(uuid::Uuid) -> bool,
) -> Result<VerifiedContact, String> {
    let newest = store.contact_newest_seen(cid).map_err(|e| e.to_string())?;
    let mut v = verify_record(record, cid, expected_fk, newest, hive_core::Timestamp::now())?;
    let before = v.devices.len();
    v.devices.retain(|d| uuid::Uuid::parse_str(&d.device_id).map(|id| !revoked(id)).unwrap_or(false));
    if before > 0 && v.devices.is_empty() {
        return Err("every device in the contact record was revoked".into());
    }
    store.note_contact_seen(cid, v.issued_at).map_err(|e| e.to_string())?;
    Ok(v)
}

// ── Drops: an invite sealed to the recipient's devices ─────────────────────

/// Seal `plaintext` (a `hivews1:` invite) once per recipient key, as the
/// opaque drop blob `{"v":1,"sealed":[…]}`. The blob names no device: the
/// recipient tries each part.
pub fn seal_drop(plaintext: &[u8], recipients: &[crate::directory_trust::Recipient]) -> Result<serde_json::Value, String> {
    let mut parts = Vec::new();
    for r in recipients {
        let blob = hive_core::e2ee::seal(&r.ka_public, plaintext).map_err(|e| e.to_string())?;
        parts.push(serde_json::to_value(blob).map_err(|e| e.to_string())?);
    }
    if parts.is_empty() {
        return Err("no recipient keys".into());
    }
    Ok(serde_json::json!({ "v": 1, "sealed": parts }))
}

/// Open a drop blob with this device's key-agreement key. `None` when no
/// part opens (not for this device, or garbage).
pub fn open_drop(blob: &serde_json::Value, ka: &hive_core::e2ee::KeyAgreementKeypair) -> Option<Vec<u8>> {
    blob.get("sealed")?.as_array()?.iter().find_map(|p| {
        let b: hive_core::e2ee::SealedBlob = serde_json::from_value(p.clone()).ok()?;
        hive_core::e2ee::open(ka, &b).ok()
    })
}

/// Whether a device should give up its contact id after some relays refused
/// its write key (the id lapsed there and someone else holds it now). Only
/// when no relay holds the record any more: one refusing (or hostile) relay
/// must not orphan the record and drop box on every other relay, with the
/// invites co-members already dropped there.
pub fn rotate_cid_after_refusal(refused: usize, holding: usize) -> bool {
    refused > 0 && holding == 0
}

/// The highest `through` an owner may acknowledge: every drop up to it is
/// handled (acknowledging deletes all of them). `handled` need not be
/// sorted; `pending` are seqs still waiting for the person.
pub fn ack_through(handled: &[u64], pending: &[u64]) -> Option<u64> {
    let first_pending = pending.iter().min().copied();
    handled.iter().copied().filter(|h| first_pending.is_none_or(|p| *h < p)).max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::extract::{Path, Query, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, put};
    use axum::{Json, Router};

    #[derive(Default)]
    struct Fake {
        records: HashMap<String, (String, serde_json::Value)>,
        drops: HashMap<String, Vec<serde_json::Value>>,
    }
    type S = Arc<Mutex<Fake>>;

    fn key_of(h: &HeaderMap) -> Option<String> {
        h.get(CONTACT_KEY_HEADER).and_then(|v| v.to_str().ok()).map(str::to_string)
    }

    async fn put_rec(State(s): State<S>, Path(cid): Path<String>, h: HeaderMap, Json(b): Json<serde_json::Value>) -> (StatusCode, Json<serde_json::Value>) {
        let mut f = s.lock().unwrap();
        let key = key_of(&h).or_else(|| b["writeKey"].as_str().map(str::to_string)).unwrap_or_default();
        let created = !f.records.contains_key(&cid);
        if let Some((k, _)) = f.records.get(&cid) {
            if *k != key {
                return (StatusCode::FORBIDDEN, Json(serde_json::json!({})));
            }
        }
        f.records.insert(cid, (key, b["record"].clone()));
        (StatusCode::OK, Json(serde_json::json!({ "created": created, "updatedAt": 1, "expiresAt": 2 })))
    }
    async fn get_rec(State(s): State<S>, Path(cid): Path<String>) -> (StatusCode, Json<serde_json::Value>) {
        match s.lock().unwrap().records.get(&cid) {
            Some((_, r)) => (StatusCode::OK, Json(serde_json::json!({ "record": r, "updatedAt": 1 }))),
            None => (StatusCode::NOT_FOUND, Json(serde_json::json!({}))),
        }
    }
    async fn post_drop(State(s): State<S>, Path(cid): Path<String>, Json(b): Json<serde_json::Value>) -> StatusCode {
        let mut f = s.lock().unwrap();
        if !f.records.contains_key(&cid) {
            return StatusCode::NOT_FOUND;
        }
        f.drops.entry(cid).or_default().push(b["blob"].clone());
        StatusCode::OK
    }
    async fn list_drops(
        State(s): State<S>,
        Path(cid): Path<String>,
        h: HeaderMap,
        Query(q): Query<HashMap<String, String>>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        let f = s.lock().unwrap();
        let Some((k, _)) = f.records.get(&cid) else { return (StatusCode::NOT_FOUND, Json(serde_json::json!({}))) };
        if key_of(&h).as_deref() != Some(k.as_str()) {
            return (StatusCode::FORBIDDEN, Json(serde_json::json!({})));
        }
        let after: usize = q.get("after").and_then(|a| a.parse().ok()).unwrap_or(0);
        let items: Vec<_> = f
            .drops
            .get(&cid)
            .map(|v| v.iter().enumerate().skip(after).map(|(i, b)| serde_json::json!({ "seq": i + 1, "blob": b, "createdAt": 3 })).collect())
            .unwrap_or_default();
        (StatusCode::OK, Json(serde_json::json!({ "items": items, "more": false })))
    }

    async fn spawn(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{addr}")
    }

    #[test]
    fn one_refusing_relay_does_not_orphan_the_others() {
        assert!(!rotate_cid_after_refusal(1, 1), "another relay still holds the record");
        assert!(!rotate_cid_after_refusal(0, 0));
        assert!(rotate_cid_after_refusal(2, 0));
    }

    #[test]
    fn ids_are_random_and_well_formed() {
        let (a, b) = (new_cid(), new_cid());
        assert_ne!(a, b);
        assert!(valid_cid(&a) && a.len() == 22, "{a}");
        assert_eq!(new_write_key().len(), 43);
        assert!(!valid_cid("github:42"));
        assert!(!valid_cid(&new_write_key()));
    }

    #[tokio::test]
    async fn contact_record_and_drop_box_round_trip() {
        let s: S = Arc::default();
        let router = Router::new()
            .route("/v1/contacts/{cid}", put(put_rec).get(get_rec))
            .route("/v1/contacts/{cid}/drop", get(list_drops).post(post_drop))
            .with_state(s);
        let base = spawn(router).await;
        let owner = RelayClient::new(&base);
        let stranger = RelayClient::new(&base);
        let (cid, key) = (new_cid(), new_write_key());
        let rec = serde_json::json!({ "v": 1, "cid": cid });
        assert!(owner.contact_put(&cid, &key, &rec, true).await.unwrap().created);
        assert_eq!(stranger.contact_get(&cid).await.unwrap().unwrap().record, rec);
        // Someone else can't overwrite it or read the box.
        let err = stranger.contact_put(&cid, &new_write_key(), &rec, false).await.unwrap_err();
        assert_eq!(err, ContactsError::Unauthorized);
        stranger.contact_drop(&cid, &serde_json::json!({ "sealed": {} })).await.unwrap();
        assert_eq!(stranger.contact_drops(&cid, &new_write_key(), 0).await.unwrap_err(), ContactsError::Unauthorized);
        let page = owner.contact_drops(&cid, &key, 0).await.unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].seq, 1);
        assert_eq!(stranger.contact_get(&new_cid()).await.unwrap().map(|r| r.record), None);
    }

    /// The write key is a bearer capability every relay sees in clear on each
    /// call. One key for every relay would let any one of them read, ack
    /// (delete) or overwrite the same contact on every other relay. Each relay
    /// must get its own key, and none of them the master key.
    #[tokio::test]
    async fn contact_write_key_is_per_relay() {
        let mk = || {
            let s: S = Arc::default();
            let router = Router::new()
                .route("/v1/contacts/{cid}", put(put_rec).get(get_rec))
                .route("/v1/contacts/{cid}/drop", get(list_drops).post(post_drop))
                .with_state(s.clone());
            (s, router)
        };
        let ((sa, ra), (sb, rb)) = (mk(), mk());
        let (a, b) = (spawn(ra).await, spawn(rb).await);
        let (cid, master) = (new_cid(), new_write_key());
        let rec = serde_json::json!({ "v": 1, "cid": cid });
        RelayClient::new(&a).contact_put(&cid, &master, &rec, true).await.unwrap();
        RelayClient::new(&b).contact_put(&cid, &master, &rec, true).await.unwrap();
        let ka = sa.lock().unwrap().records[&cid].0.clone();
        let kb = sb.lock().unwrap().records[&cid].0.clone();
        assert_ne!(ka, master, "relay A saw the master key");
        assert_ne!(kb, master, "relay B saw the master key");
        assert_ne!(ka, kb, "both relays got the same capability");
        // What relay A saw is useless on relay B.
        let stolen = RelayClient::new(&b);
        let raw = hive_forge::new_http_client()
            .get(format!("{b}/v1/contacts/{cid}/drop?after=0"))
            .header(CONTACT_KEY_HEADER, &ka)
            .send()
            .await
            .unwrap();
        assert_eq!(raw.status().as_u16(), 403);
        // The owner still reads and writes both with the one master key.
        assert!(RelayClient::new(&a).contact_drops(&cid, &master, 0).await.is_ok());
        assert!(stolen.contact_drops(&cid, &master, 0).await.is_ok());
        assert!(RelayClient::new(&b).contact_put(&cid, &master, &rec, false).await.is_ok());
    }

    /// A box with more drops than one page (junk sealed to the owner's public
    /// keys, or invites left unanswered) must not hide later invites: the
    /// owner reads every page.
    #[tokio::test]
    async fn drop_flood_does_not_hide_later_invites() {
        async fn paged(State(s): State<S>, Path(cid): Path<String>, Query(q): Query<HashMap<String, String>>) -> Json<serde_json::Value> {
            let f = s.lock().unwrap();
            let after: usize = q.get("after").and_then(|a| a.parse().ok()).unwrap_or(0);
            let all = f.drops.get(&cid).cloned().unwrap_or_default();
            let items: Vec<_> = all.iter().enumerate().skip(after).take(3).map(|(i, b)| serde_json::json!({ "seq": i + 1, "blob": b })).collect();
            Json(serde_json::json!({ "items": items, "more": after + 3 < all.len() }))
        }
        let s: S = Arc::default();
        let cid = new_cid();
        let mut drops: Vec<serde_json::Value> = (0..7).map(|i| serde_json::json!({ "junk": i })).collect();
        drops.push(serde_json::json!({ "real": true }));
        s.lock().unwrap().drops.insert(cid.clone(), drops);
        let base = spawn(Router::new().route("/v1/contacts/{cid}/drop", get(paged)).with_state(s)).await;
        let items = RelayClient::new(&base).contact_drops_all(&cid, &new_write_key()).await.unwrap();
        assert_eq!(items.len(), 8);
        assert_eq!(items.last().unwrap().blob, serde_json::json!({ "real": true }));
    }

    /// S15: a 0.1 client against a relay without the contact routes (v0.2)
    /// finds out on its first write and degrades to links and QR codes.
    #[tokio::test]
    async fn client_degrades_without_contacts_routes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, crate::test_relay::router()).await.unwrap() });
        let client = RelayClient::new(format!("http://{addr}"));
        let err = client.contact_put(&new_cid(), &new_write_key(), &serde_json::json!({}), true).await.unwrap_err();
        assert_eq!(err, ContactsError::Unsupported);
        assert_eq!(err.to_string(), "this relay doesn't support contacts");
        assert!(client.contact_get(&new_cid()).await.unwrap().is_none());
    }

    fn now() -> hive_core::Timestamp {
        hive_core::Timestamp::now()
    }

    /// A record as a device would publish it, plus the account and device keys.
    fn record(cid: &str) -> (serde_json::Value, String, [u8; 32]) {
        let (rec, fk, ka, _, _) = record_at(cid, now());
        (rec, fk, ka)
    }

    /// [`record`] issued at `at`, also returning its account and device ids.
    fn record_at(cid: &str, at: hive_core::Timestamp) -> (serde_json::Value, String, [u8; 32], uuid::Uuid, uuid::Uuid) {
        use hive_core::crypto::DeviceCertificate;
        use hive_core::e2ee::KeyAgreementKeypair;
        use hive_core::SigningKeypair;
        let account = SigningKeypair::generate().unwrap();
        let account_id = uuid::Uuid::new_v4();
        let device = SigningKeypair::generate().unwrap();
        let device_id = uuid::Uuid::new_v4();
        let cert = DeviceCertificate::issue(&account, account_id, device_id, &device.public_key_bytes(), now());
        let ka = KeyAgreementKeypair::generate().unwrap().public_key_bytes();
        let ws = contact_workspace_id(cid);
        let mut env = hive_core::SessionEventEnvelope::new(
            ws,
            ws,
            1,
            hive_core::SessionEvent::DeviceKeyAgreementPublished { device_id, ka_public: ka.to_vec() },
        );
        hive_core::crypto::sign_envelope(&mut env, device_id, &device);
        let rec = build_record(cid, account_id, &account.public_key_bytes(), vec![RecordDevice { certificate: cert, ka_publication: env }], at);
        let fk = crate::envelope_verifier::FounderPin::fingerprint_hex(&account.public_key_bytes());
        (serde_json::to_value(rec).unwrap(), fk, ka, account_id, device_id)
    }

    fn fake_relay(s: S) -> Router {
        Router::new()
            .route("/v1/contacts/{cid}", put(put_rec).get(get_rec))
            .route("/v1/contacts/{cid}/drop", get(list_drops).post(post_drop))
            .with_state(s)
    }

    /// S4: the newest record seen for a cid is kept in hive.db, so a relay
    /// that later serves an older one (a rollback to a key or device the
    /// account dropped) is refused, across restarts.
    #[tokio::test]
    async fn contact_newest_seen_persists_for_anti_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("h.db");
        let s: S = Arc::default();
        let base = spawn(fake_relay(s.clone())).await;
        let client = RelayClient::new(&base);
        let cid = new_cid();
        let (newer, fk, _, account, _) = record_at(&cid, now());
        s.lock().unwrap().records.insert(cid.clone(), ("k".into(), newer));
        {
            let store = crate::EventStore::open(&db).unwrap();
            let v = fetch_verified_contact(&client, &store, &cid, Some(&fk), &|_| false).await.unwrap().unwrap();
            assert_eq!(v.account_id, account);
        }
        let older = record_at(&cid, hive_core::Timestamp(now().0 - time::Duration::hours(1))).0;
        s.lock().unwrap().records.insert(cid.clone(), ("k".into(), older));
        let store = crate::EventStore::open(&db).unwrap();
        let err = fetch_verified_contact(&client, &store, &cid, None, &|_| false).await.unwrap_err();
        assert!(err.contains("rollback"), "{err}");
    }

    /// S4: a record's device that a shared log revoked is not used; a record
    /// with only revoked devices is refused.
    #[tokio::test]
    async fn contact_record_revoked_device_refused() {
        let s: S = Arc::default();
        let base = spawn(fake_relay(s.clone())).await;
        let client = RelayClient::new(&base);
        let store = crate::EventStore::open_in_memory().unwrap();
        let cid = new_cid();
        let (rec, fk, _, _, device) = record_at(&cid, now());
        s.lock().unwrap().records.insert(cid.clone(), ("k".into(), rec));
        let err = fetch_verified_contact(&client, &store, &cid, Some(&fk), &|d| d == device).await.unwrap_err();
        assert!(err.contains("revoked"), "{err}");
    }

    /// S4: a relay answering a record read (or a drop page) with an endless
    /// body is cut off at the cap instead of filling memory.
    #[tokio::test]
    async fn contact_response_body_capped() {
        async fn huge() -> String {
            format!("{{\"record\":\"{}\"}}", "x".repeat(MAX_RECORD_BODY + 1))
        }
        async fn huge_page() -> String {
            format!("{{\"items\":[],\"pad\":\"{}\"}}", "x".repeat(MAX_DROP_PAGE_BODY + 1))
        }
        let base = spawn(Router::new().route("/v1/contacts/{cid}", get(huge)).route("/v1/contacts/{cid}/drop", get(huge_page))).await;
        let client = RelayClient::new(&base);
        let cid = new_cid();
        assert!(matches!(client.contact_get(&cid).await, Err(ContactsError::TooLarge)));
        assert!(matches!(client.contact_drops(&cid, &new_write_key(), 0).await, Err(ContactsError::TooLarge)));
    }

    /// Opt-in identity links ride a record as signed envelopes: one signed
    /// by the record's device for its account in the contact's workspace
    /// passes; a forged one, another account's, or one bound to another
    /// workspace is dropped (the contact still verifies). Records without
    /// links serialize exactly as before (skip-empty).
    #[test]
    fn record_links_are_opt_in_and_checked() {
        use hive_core::crypto::DeviceCertificate;
        use hive_core::identity_link::{render_statement, IdentityLink, LinkMethod};
        use hive_core::{ActorIdentity, ActorKind, ActorStamp, SessionEvent, SessionEventEnvelope, SigningKeypair};
        let cid = new_cid();
        let ws = contact_workspace_id(&cid);
        let account = SigningKeypair::generate().unwrap();
        let account_id = uuid::Uuid::new_v4();
        let device = SigningKeypair::generate().unwrap();
        let device_id = uuid::Uuid::new_v4();
        let cert = DeviceCertificate::issue(&account, account_id, device_id, &device.public_key_bytes(), now());
        let mut ka = SessionEventEnvelope::new(ws, ws, 1, SessionEvent::DeviceKeyAgreementPublished { device_id, ka_public: vec![1; 32] });
        hive_core::crypto::sign_envelope(&mut ka, device_id, &device);
        let mut rec = build_record(&cid, account_id, &account.public_key_bytes(), vec![RecordDevice { certificate: cert, ka_publication: ka }], now());
        assert!(!serde_json::to_string(&rec).unwrap().contains("\"links\""), "skip-empty");
        let link = |subject: &str, in_ws: uuid::Uuid| {
            let t = hive_core::Timestamp::from_unix_seconds(1_800_000_000);
            let mut l = IdentityLink {
                link_id: uuid::Uuid::new_v4(),
                account_id,
                provider: "github".into(),
                host: "github.com".into(),
                subject_id: subject.into(),
                handle_at_link: "ana".into(),
                method: LinkMethod::SshSig,
                statement: String::new(),
                proof: "sig".into(),
                key_fingerprint: "SHA256:x".into(),
                nonce: "AAECAwQFBgcICQoLDA0ODw".into(),
                issued_at: t,
                expires_at: hive_core::Timestamp::from_unix_seconds(1_800_000_000 + 86_400),
            };
            l.statement = render_statement(&l, in_ws, &account.public_key_bytes());
            l
        };
        let env = |l: IdentityLink, key: &SigningKeypair| {
            let mut e = SessionEventEnvelope::new(ws, ws, 1, SessionEvent::IdentityLinkPublished { link: l });
            let mut actor = ActorIdentity::new(account_id.to_string(), "Ana", ActorKind::Human);
            actor.account_id = Some(account_id);
            e.actor_stamp = Some(ActorStamp { actor, recorded_at: now(), cfg_frontier: None, presence: None, runtime_effect: false, prev: None, sig_alg: Default::default(), log_head: None });
            hive_core::crypto::sign_envelope(&mut e, device_id, key);
            e
        };
        rec.links = vec![
            env(link("1", ws), &device),
            env(link("2", ws), &SigningKeypair::generate().unwrap()),
            env(link("3", uuid::Uuid::new_v4()), &device),
        ];
        let fk = crate::envelope_verifier::FounderPin::fingerprint_hex(&account.public_key_bytes());
        let v = verify_record(&serde_json::to_value(&rec).unwrap(), &cid, Some(&fk), None, now()).unwrap();
        assert_eq!(v.links.iter().map(|l| l.subject_id.as_str()).collect::<Vec<_>>(), vec!["1"]);
    }

    #[test]
    fn record_verifies_and_is_anchored_by_the_fk() {
        let cid = new_cid();
        let (rec, fk, ka) = record(&cid);
        let v = verify_record(&rec, &cid, Some(&fk), None, now()).unwrap();
        assert!(v.anchored);
        assert_eq!(v.fingerprint, fk);
        assert_eq!(v.devices.iter().map(|d| d.ka_public).collect::<Vec<_>>(), vec![ka]);
        assert!(verify_record(&rec, &new_cid(), Some(&fk), None, now()).is_err(), "bound to its cid");
    }

    /// S6 `contact_record_wrong_fk_unverified`: a record whose account key
    /// doesn't match the out-of-band safety code is shown unverified.
    #[test]
    fn contact_record_wrong_fk_unverified() {
        let cid = new_cid();
        let (rec, _, _) = record(&cid);
        let v = verify_record(&rec, &cid, Some("00112233445566778899aabbccddeeff"), None, now()).unwrap();
        assert!(!v.anchored);
        assert!(!verify_record(&rec, &cid, None, None, now()).unwrap().anchored);
    }

    /// S6 `contact_record_rollback_rejected`: an older record than the newest
    /// seen for the cid is refused, as is an expired one.
    #[test]
    fn contact_record_rollback_rejected() {
        let cid = new_cid();
        let (rec, fk, _) = record(&cid);
        let issued: hive_core::Timestamp = serde_json::from_value(rec["issuedAt"].clone()).unwrap();
        let later = hive_core::Timestamp(issued.0 + time::Duration::seconds(10));
        let err = verify_record(&rec, &cid, Some(&fk), Some(later), now()).unwrap_err();
        assert!(err.contains("rollback"), "{err}");
        let future = hive_core::Timestamp(now().0 + time::Duration::days(31));
        assert!(verify_record(&rec, &cid, Some(&fk), None, future).is_err(), "expired");
    }

    /// A relay that swaps in its own key-agreement key (re-signing the
    /// publication with a key the account never certified) is caught.
    #[test]
    fn substituted_device_key_fails_verification() {
        let cid = new_cid();
        let (rec, fk, _) = record(&cid);
        // Edit the received bytes the way a relay could: swap the published
        // key-agreement key for its own.
        let text = rec.to_string();
        let ka: Vec<u8> = match &serde_json::from_value::<ContactRecord>(rec.clone()).unwrap().devices[0].ka_publication.payload {
            hive_core::SessionEvent::DeviceKeyAgreementPublished { ka_public, .. } => ka_public.clone(),
            _ => unreachable!(),
        };
        let orig = serde_json::to_string(&ka).unwrap();
        assert!(text.contains(&orig), "the key appears in the record");
        let swapped = text.replace(&orig, &serde_json::to_string(&vec![9u8; 32]).unwrap());
        let tampered: serde_json::Value = serde_json::from_str(&swapped).unwrap();
        assert!(verify_record(&tampered, &cid, Some(&fk), None, now()).is_err());
    }

    #[test]
    fn drop_opens_only_for_its_recipient() {
        use hive_core::e2ee::KeyAgreementKeypair;
        let bob = KeyAgreementKeypair::generate().unwrap();
        let eve = KeyAgreementKeypair::generate().unwrap();
        let r = crate::directory_trust::Recipient { device_id: "b".into(), ka_public: bob.public_key_bytes() };
        let blob = seal_drop(b"hivews1:abc", &[r]).unwrap();
        assert!(!blob.to_string().contains("\"b\""), "the blob names no device");
        assert_eq!(open_drop(&blob, &bob).as_deref(), Some(&b"hivews1:abc"[..]));
        assert_eq!(open_drop(&blob, &eve), None);
        assert_eq!(open_drop(&serde_json::json!({"junk": 1}), &bob), None);
    }

    #[test]
    fn ack_never_passes_a_pending_drop() {
        assert_eq!(ack_through(&[1, 2, 3], &[]), Some(3));
        assert_eq!(ack_through(&[1, 2, 4], &[3]), Some(2));
        assert_eq!(ack_through(&[4], &[3]), None);
        assert_eq!(ack_through(&[], &[]), None);
    }
}

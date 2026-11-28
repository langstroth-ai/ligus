//! The box's contact (relay v0.3, IDENTITY-AND-FORGES §3.2-§3.3): a random
//! contact id with a signed record on the relays the box syncs with, and a
//! sealed drop box where co-members deliver invites to it.
//!
//! As on the desktop, contacts are per device. Only a box that holds its
//! account's key publishes one (a box acting for its owner can't sign a
//! record for the owner); it announces the id on what it stamps, reads its
//! own box, and acknowledges each drop once handled. The write key never
//! leaves the box in clear: each relay gets its own key derived from it
//! (`ligus_runtime::contacts::relay_write_key`).
//!
//! Delivery to someone else's box reads their record first and drops only
//! when it verifies, is anchored to the account key the shared logs trust
//! for them and names their account; the invite is sealed to the keys from
//! the config logs, never to the record's.

use std::collections::BTreeMap;

use ligus_core::Timestamp;
use ligus_runtime::contacts::{self, ContactsError};
use ligus_runtime::directory_trust::{CoMember, SealPlan};
use ligus_runtime::dm_delivery::{self, Delivery};
use ligus_runtime::secret_store::SecretId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::invite_code::{trust_str, InviteConn};
use super::Shared;

/// Republish a record this long after the last write (a relay keeps it 30
/// days from its last write).
const REPUBLISH_SECS: i64 = 24 * 3600;
/// Ask a relay that had no contact routes again after this long.
const RECHECK_UNSUPPORTED_SECS: i64 = 24 * 3600;
/// Pending invites shown at once (older ones wait for the next read).
const MAX_PENDING: usize = 50;

/// This box's contact, device-local (`serve-contact.json`, owner-only; the
/// write key in the secret store).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnContact {
    pub cid: String,
    #[serde(skip)]
    pub write_key: String,
    /// relay URL → unix seconds of the last successful write.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub published: BTreeMap<String, i64>,
    /// relay URL → unix seconds it last answered "no contact routes".
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unsupported: BTreeMap<String, i64>,
    /// relay URL → drop seqs handled and not acknowledged yet.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub handled: BTreeMap<String, Vec<u64>>,
}

fn path(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("serve-contact.json")
}

fn write_key_id() -> SecretId {
    SecretId::new("cli", "contact-write-key")
}

impl OwnContact {
    fn fresh() -> Self {
        Self { cid: contacts::new_cid(), write_key: contacts::new_write_key(), ..Default::default() }
    }

    pub fn load(data_dir: &std::path::Path) -> Option<Self> {
        let mut c: OwnContact = serde_json::from_str(&std::fs::read_to_string(path(data_dir)).ok()?).ok()?;
        let store = ligus_runtime::secret_store::for_data_dir(data_dir);
        c.write_key = store.get_string(&write_key_id()).ok().flatten().map(|z| z.to_string()).unwrap_or_default();
        ligus_runtime::secret_redact::register(&c.write_key);
        (contacts::valid_cid(&c.cid) && !c.write_key.is_empty()).then_some(c)
    }

    fn save(&self, data_dir: &std::path::Path) {
        let store = ligus_runtime::secret_store::for_data_dir(data_dir);
        if let Err(e) = store.put(&write_key_id(), self.write_key.as_bytes()) {
            eprintln!("serve: saving the contact write key failed: {e}");
            return;
        }
        match serde_json::to_string_pretty(self) {
            Ok(t) => {
                if let Err(e) = super::tunnel::write_private(&path(data_dir), t.as_bytes()) {
                    eprintln!("serve: saving the contact failed: {e}");
                }
            }
            Err(e) => eprintln!("serve: saving the contact failed: {e}"),
        }
    }
}

pub fn unix_now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// The relays this box syncs with: the workspace's and every DM's.
fn my_relays(s: &Shared) -> Vec<String> {
    let mut out: Vec<String> = s.cfg.relay_url.iter().map(|r| r.trim().to_string()).collect();
    out.extend(s.dms.list().into_iter().map(|d| d.relay_url.trim().to_string()));
    out.retain(|r| !r.is_empty());
    out.sort();
    out.dedup();
    out
}

/// Whether this box can speak for its account (holds its key, has a
/// certificate for this device under it).
fn can_publish(s: &Shared) -> bool {
    !s.lock().is_delegated()
}

/// This box's record for `cid`, or `None` when it can't speak for its
/// account (delegated, no certificate yet).
fn own_record(s: &Shared, cid: &str) -> Option<Value> {
    let identity = ligus_runtime::IdentityStore::new(&s.cfg.data_dir, ligus_runtime::secret_store::StoreKeyVault::for_data_dir(&s.cfg.data_dir));
    let stored = identity.load().ok().flatten()?;
    let svc = s.lock();
    if svc.is_delegated() {
        return None;
    }
    let account_id = svc.author().account_id?;
    if stored.device.certificate.account_id != account_id || stored.device.id != svc.device_id() {
        return None;
    }
    let ka = identity.key_agreement(svc.device_id()).ok()?;
    let env = svc.signed_ka_publication(contacts::contact_workspace_id(cid), ka.public_key_bytes().to_vec());
    let device = contacts::RecordDevice { certificate: stored.device.certificate.clone(), ka_publication: env };
    let rec = contacts::build_record(cid, account_id, &svc.account_public_key(), vec![device], Timestamp::now());
    serde_json::to_value(rec).ok()
}

/// `on` (a relay holds this box's contact), `unsupported` (no relay has the
/// contact routes: invites come as links), `noRelay`, or `delegated` (a box
/// acting for its owner has no contact of its own).
pub fn status(s: &Shared, on: bool) -> &'static str {
    if !can_publish(s) {
        "delegated"
    } else if my_relays(s).is_empty() {
        "noRelay"
    } else if on {
        "on"
    } else {
        "unsupported"
    }
}

/// Publish (or refresh) this box's record on every relay it uses, then
/// announce the id on what it stamps. A relay without contact routes is
/// asked again a day later; an id refused everywhere is replaced. Returns
/// whether any relay holds the record.
pub async fn ensure_contact(s: &Shared) -> bool {
    if !can_publish(s) {
        return false;
    }
    let _serial = s.contact_lock.lock().await;
    let contact = match OwnContact::load(&s.cfg.data_dir) {
        Some(c) => c,
        None => {
            let c = OwnContact::fresh();
            c.save(&s.cfg.data_dir);
            c
        }
    };
    let relays = my_relays(s);
    let now = unix_now();
    let mut c = contact.clone();
    let mut refused = 0usize;
    c.published.retain(|r, _| relays.contains(r));
    for relay in &relays {
        if c.published.get(relay).is_some_and(|t| now - t < REPUBLISH_SECS) {
            continue;
        }
        if c.unsupported.get(relay).is_some_and(|t| now - t < RECHECK_UNSUPPORTED_SECS) {
            continue;
        }
        let Some(record) = own_record(s, &c.cid) else { break };
        if let Err(e) = super::dms::foreign_relay_check(&s.cfg, relay).await {
            eprintln!("serve: skipping a relay for the contact record ({e})");
            continue;
        }
        let client = super::dms::relay_client_for(&s.cfg, relay);
        let first = !c.published.contains_key(relay);
        match client.contact_put(&c.cid, &c.write_key, &record, first).await {
            Ok(_) => {
                c.published.insert(relay.clone(), now);
                c.unsupported.remove(relay);
            }
            Err(ContactsError::Unsupported) => {
                c.unsupported.insert(relay.clone(), now);
            }
            Err(ContactsError::Unauthorized) => {
                eprintln!("serve: the contact id was refused on a relay");
                c.published.remove(relay);
                c.unsupported.insert(relay.clone(), now);
                refused += 1;
            }
            Err(e) => eprintln!("serve: publishing the contact failed: {e}"),
        }
    }
    if contacts::rotate_cid_after_refusal(refused, c.published.len()) {
        eprintln!("serve: the contact id was refused everywhere; making a new one");
        c = OwnContact::fresh();
    }
    if c != contact || c.write_key != contact.write_key {
        c.save(&s.cfg.data_dir);
    }
    let announced = (!c.published.is_empty()).then(|| c.cid.clone());
    announce(s, announced.clone());
    announced.is_some()
}

/// Stamp `cid` on what this box writes. Whether the log carries it yet is
/// checked on the next host beat ([`stamp_contact_in_beat`]): liveness and
/// this one-off announcement ride the beat, not a write of their own here
/// (workspace model v3, 5.10).
fn announce(s: &Shared, cid: Option<String>) {
    let mut svc = s.lock();
    if svc.author().contact == cid {
        if cid.is_none() {
            return;
        }
    } else {
        svc.set_author_contact(cid.clone());
    }
    if cid.is_some() {
        drop(svc);
        s.contact_unstamped.store(true, std::sync::atomic::Ordering::SeqCst);
        s.beat_kick.notify_one();
    }
}

/// On a host beat: when this box's contact id isn't on the workspace log
/// yet (no write of this box carried it since it changed), write its host
/// record once with the stamp, so co-members learn where to deliver.
/// Returns whether it wrote.
pub(crate) fn stamp_contact_in_beat(s: &Shared) -> bool {
    use std::sync::atomic::Ordering;
    if !s.contact_unstamped.load(Ordering::SeqCst) {
        return false;
    }
    let mut svc = s.lock();
    let (Some(cid), Some(account)) = (svc.author().contact.clone(), svc.author().account_id) else {
        s.contact_unstamped.store(false, Ordering::SeqCst);
        return false;
    };
    if svc.write_block().is_some() {
        return false;
    }
    let me = svc.device_id();
    let my_key = svc.device_public_key().to_vec();
    let stamped = svc
        .store()
        .latest_contact_of(s.ws, account, &|d| (d == me).then(|| my_key.clone()))
        .ok()
        .flatten();
    if stamped.as_deref() == Some(cid.as_str()) {
        s.contact_unstamped.store(false, Ordering::SeqCst);
        return false;
    }
    match svc.touch_host(s.ws, &s.host_id, 0) {
        Ok(wrote) => {
            s.contact_unstamped.store(false, Ordering::SeqCst);
            drop(svc);
            if wrote {
                s.sync_kick.notify_one();
            }
            wrote
        }
        Err(e) => {
            eprintln!("serve: announcing the contact failed: {e}");
            false
        }
    }
}

/// Drop `invite` into `person`'s box, sealed to `plan` (their verified
/// devices), through the relay of the workspace their contact id was seen
/// in ([`dm_delivery::drop_to_box`], shared with the desktop). `NotYet` when
/// they have no contact, their relay has no contact routes, their record
/// doesn't verify for them, or the drop failed: share the link, or hold it
/// for a retry ([`dm_delivery`]).
pub async fn deliver_invite(s: &Shared, person: &CoMember, plan: &SealPlan, invite: &str) -> Delivery {
    let Some((cid, ws)) = person.contact.clone() else { return Delivery::NotYet("no contact id for them yet".into()) };
    let relay = if ws == s.ws {
        s.cfg.relay_url.clone()
    } else {
        s.dms.get(ws).map(|d| d.relay_url)
    };
    let Some(relay) = relay.filter(|r| !r.trim().is_empty()) else { return Delivery::NotYet("no relay for their contact".into()) };
    if let Err(e) = super::dms::foreign_relay_check(&s.cfg, &relay).await {
        eprintln!("serve: their relay was refused ({e}); share the link");
        return Delivery::NotYet("their relay was refused".into());
    }
    let client = super::dms::relay_client_for(&s.cfg, &relay);
    dm_delivery::drop_to_box(&client, &cid, person, plan, invite, |record| {
        let svc = s.lock();
        contacts::accept_contact_record(svc.store(), &cid, record, person.fingerprint.as_deref(), &|d| {
            person.revoked_devices.contains(&d)
        })
    })
    .await
}

/// [`dm_delivery::Host`] for the box.
pub struct BoxHost<'a>(pub &'a Shared);

impl dm_delivery::Host for BoxHost<'_> {
    fn co_members(&self) -> Vec<CoMember> {
        self.0.co_members()
    }
    fn dm_exists(&self, ws: Uuid) -> bool {
        self.0.dms.get(ws).is_some() && !self.0.dms.is_archived(ws)
    }
    fn own_ka(&self) -> Option<ligus_core::e2ee::KeyAgreementKeypair> {
        own_ka(self.0)
    }
    fn deliver(&self, person: &CoMember, plan: &SealPlan, invite: &str) -> impl std::future::Future<Output = Delivery> + Send {
        deliver_invite(self.0, person, plan, invite)
    }
}

/// This box's key-agreement key (what held invites are sealed to).
pub fn own_ka(s: &Shared) -> Option<ligus_core::e2ee::KeyAgreementKeypair> {
    let identity = ligus_runtime::IdentityStore::new(&s.cfg.data_dir, ligus_runtime::secret_store::StoreKeyVault::for_data_dir(&s.cfg.data_dir));
    let device = s.lock().device_id();
    identity.key_agreement(device).ok()
}

/// Hold a DM invite that couldn't be delivered yet; `None` when it can't be
/// held (no key, too many waiting): share the link.
pub fn hold_invite(s: &Shared, ws: Uuid, person: &CoMember, plan: &SealPlan, invite: &str) -> Option<dm_delivery::HeldInvite> {
    let ka = own_ka(s)?;
    match s.held.hold(ws, person, plan, invite, &ka.public_key_bytes(), unix_now()) {
        Ok(h) => {
            s.held_kick.notify_one();
            Some(h)
        }
        Err(e) => {
            eprintln!("serve: a DM invite can't wait for delivery ({e}); share the link");
            None
        }
    }
}

/// How often the box looks at held invites while any waits (and the
/// longest it idles otherwise).
const HELD_EVERY: std::time::Duration = std::time::Duration::from_secs(3);
const HELD_IDLE: std::time::Duration = std::time::Duration::from_secs(60);

/// Deliver held DM invites once their peer's contact id is known: after
/// every sync that pulled something, and every few seconds while any waits.
pub async fn held_loop(s: std::sync::Arc<Shared>, mut stop: tokio::sync::watch::Receiver<bool>) {
    loop {
        let changed = dm_delivery::run_pass(&BoxHost(&s), &s.held, unix_now()).await;
        if !changed.is_empty() {
            for h in &changed {
                s.hub.emit("dm.invite", serde_json::json!({ "workspaceId": h.workspace_id, "inviteDelivery": h.status_json() }));
            }
            s.hub.emit("dms.changed", serde_json::json!({}));
        }
        let wait = if s.held.any_open() { HELD_EVERY } else { HELD_IDLE };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = s.held_kick.notified() => {}
            _ = stop.wait_for(|x| *x) => return,
        }
    }
}

/// One invite waiting in the box's drop box.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingInvite {
    pub relay: String,
    pub seq: u64,
    pub is_dm: bool,
    pub workspace_name: String,
    pub founder_account: Option<String>,
    pub founder_name: Option<String>,
    pub safety_code: Option<String>,
    /// As the desktop's: `known`, `claimed`, `mismatch`, `unknown`, `none`.
    pub trust: String,
    /// Whether this box can accept it: a DM invite that passes the DM rule
    /// can be; a workspace invite is for a desktop (a box serves one
    /// workspace).
    pub acceptable: bool,
    /// Why not, in words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    /// The `hivews1:` code (a bearer secret: only to the phone, which hands
    /// it back to accept).
    pub code: String,
}

fn mark_handled(s: &Shared, relay: &str, seq: u64) {
    let Some(mut c) = OwnContact::load(&s.cfg.data_dir) else { return };
    let h = c.handled.entry(relay.to_string()).or_default();
    if !h.contains(&seq) {
        h.push(seq);
    }
    c.save(&s.cfg.data_dir);
}

/// Read the drop boxes: invites sealed to this box that wait for a
/// decision. Garbage, invites for rooms already held and anything addressed
/// to another account are handled (and acknowledged) silently.
pub async fn pending(s: &Shared) -> (String, Vec<PendingInvite>) {
    let on = ensure_contact(s).await;
    let status = status(s, on).to_string();
    let Some(contact) = OwnContact::load(&s.cfg.data_dir) else { return (status, Vec::new()) };
    let identity = ligus_runtime::IdentityStore::new(&s.cfg.data_dir, ligus_runtime::secret_store::StoreKeyVault::for_data_dir(&s.cfg.data_dir));
    let device = s.lock().device_id();
    let Ok(ka) = identity.key_agreement(device) else { return (status, Vec::new()) };
    let me = s.my_account();
    let people = s.co_members();
    let mut out = Vec::new();
    let mut handled_now: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for relay in contact.published.keys() {
        if super::dms::foreign_relay_check(&s.cfg, relay).await.is_err() {
            continue;
        }
        let client = super::dms::relay_client_for(&s.cfg, relay);
        let Ok(items) = client.contact_drops_all(&contact.cid, &contact.write_key).await else { continue };
        let already = contact.handled.get(relay).cloned().unwrap_or_default();
        let mut waiting = Vec::new();
        let mut handled = already.clone();
        for item in items {
            if already.contains(&item.seq) {
                continue;
            }
            let opened = contacts::open_drop(&item.blob, &ka)
                .and_then(|b| String::from_utf8(b).ok())
                .and_then(|code| InviteConn::decode(&code).ok().map(|c| (code, c)));
            let Some((code, conn)) = opened else {
                handled.push(item.seq);
                continue;
            };
            let for_me = !conn.is_dm() || me.is_some_and(|m| conn.dm_account.as_deref().map(str::trim) == Some(m.to_string().as_str()));
            if s.holds_room(&conn.room) || !for_me {
                handled.push(item.seq);
                continue;
            }
            // Past the cap it still waits (never acknowledged unseen).
            waiting.push(item.seq);
            if out.len() < MAX_PENDING {
                out.push(describe(s, &people, relay, item.seq, code, &conn));
            }
        }
        if let Some(through) = contacts::ack_through(&handled, &waiting) {
            if client.contact_drops_ack(&contact.cid, &contact.write_key, through).await.is_ok() {
                handled.retain(|h| *h > through);
            }
        }
        handled_now.insert(relay.clone(), handled);
    }
    if let Some(mut c) = OwnContact::load(&s.cfg.data_dir) {
        if c.cid == contact.cid {
            for (relay, h) in handled_now {
                if h.is_empty() {
                    c.handled.remove(&relay);
                } else {
                    c.handled.insert(relay, h);
                }
            }
            c.save(&s.cfg.data_dir);
        }
    }
    (status, out)
}

fn describe(s: &Shared, people: &[CoMember], relay: &str, seq: u64, code: String, conn: &InviteConn) -> PendingInvite {
    let trust = conn.trust(people);
    let founder = conn.founder_actor_id.as_deref().and_then(|f| Uuid::parse_str(f.trim()).ok());
    let refusal = if conn.is_dm() {
        s.dm_refusal(conn, trust, true).err()
    } else {
        Some("This is an invite to another workspace. A box serves one workspace: open it on your desktop.".into())
    };
    PendingInvite {
        relay: relay.to_string(),
        seq,
        is_dm: conn.is_dm(),
        workspace_name: if conn.is_dm() { String::new() } else { ligus_proto::names::clean_display_name(&conn.display_name()) },
        founder_account: founder.map(|f| f.to_string()),
        founder_name: founder.and_then(|f| people.iter().find(|p| p.account_id == f)).map(|p| p.display_name.clone()),
        safety_code: conn.fk.as_deref().and_then(ligus_runtime::directory_trust::safety_code),
        trust: trust_str(trust).into(),
        acceptable: refusal.is_none(),
        refusal,
        code,
    }
}

/// Dismiss a drop (acknowledged on the relay at the next read).
pub fn dismiss(s: &Shared, relay: &str, seq: u64) {
    mark_handled(s, relay, seq);
}

/// After accepting a drop.
pub fn accepted(s: &Shared, relay: &str, seq: u64) {
    mark_handled(s, relay, seq);
}

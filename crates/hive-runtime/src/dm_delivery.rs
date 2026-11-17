//! Held DM invites: delivery to a co-member's drop box, retried until it
//! lands.
//!
//! Opening a DM seals its invite to the peer's verified devices and drops it
//! into their contact box (relay v0.3). Right after someone joins a shared
//! workspace this device often knows their devices but not their contact id
//! yet, so the drop can't happen. Instead of only handing back a link, the
//! opener's device *holds* the invite and delivers it, once, when the id
//! arrives (contact sync or a roster update). The desktop and `ligus serve`
//! both run this module: the decision, the store and the pass are shared;
//! each host only says how to reach a relay and read its logs ([`Host`]).
//!
//! Rules:
//!
//! - **At rest, sealed.** The invite (a bearer secret: it carries the DM's
//!   key) is sealed to this device's own key-agreement key and kept in the
//!   device's secret store (`dm-invite:<workspace>`); the held list
//!   (`held-dm-invites.json`, owner-only) holds no secret.
//! - **Once.** A held invite moves `waiting → sending` on disk before the
//!   drop, and only `waiting` is ever sent: a second pass, a restart or a
//!   concurrent caller can't send it again. One the peer already accepted
//!   (they joined from a link the opener shared by hand) is never sent.
//! - **The same recipients.** The devices approved when the DM was opened
//!   are recorded (their key-agreement keys and the account fingerprint). At
//!   delivery the peer's keys are checked again on the signed config logs
//!   ([`CoMember::seal_plan`]): a changed account key, a key conflict or a
//!   device key that wasn't approved stops delivery with an
//!   [`UNVERIFIED_RECIPIENT`] warning until the opener confirms. A device
//!   revoked since is simply left out.
//! - **Bounded.** At most [`MAX_HELD`] wait at once, and each expires
//!   [`HOLD_SECS`] after it was opened. An invite link carries no expiry of
//!   its own (it is single-use), so this is the delivery's own lifetime: past
//!   it the DM stays and the link still works by hand.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hive_core::e2ee::KeyAgreementKeypair;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::directory_trust::{CoMember, Recipient, SealPlan, UNVERIFIED_RECIPIENT};
use crate::secret_store::{SecretId, SecretStore};

/// Held invites waiting at once (waiting, warning or sending).
pub const MAX_HELD: usize = 20;
/// How long a held invite waits for the peer's contact id.
pub const HOLD_SECS: i64 = 7 * 24 * 3600;
/// A finished entry (delivered, joined, expired, dismissed) is kept this long
/// past its expiry so the DM view can still say what happened.
const KEEP_FINISHED_SECS: i64 = 24 * 3600;
/// Retry backoff after a failed drop: 5 s doubling to 10 min.
const RETRY_MIN_SECS: i64 = 5;
const RETRY_MAX_SECS: i64 = 600;

const FILE: &str = "held-dm-invites.json";
const SCOPE: &str = "dm-invite";

/// Where a held invite is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HeldState {
    /// Waiting for the peer's contact id (or a retry after a failed drop).
    Waiting,
    /// Their keys changed since the DM was opened: held until the opener
    /// confirms ([`HeldInvites::confirm`]) or dismisses it.
    Warning,
    /// Being dropped. Left over after a crash it is never sent again (it may
    /// have reached the box): the link still works by hand.
    Sending,
    Delivered,
    /// They joined the DM (from a link shared by hand): nothing to send.
    Joined,
    Expired,
    Dismissed,
    /// The sealed invite can't be opened any more (this device's key
    /// changed): share the link.
    Failed,
}

impl HeldState {
    fn is_open(self) -> bool {
        matches!(self, HeldState::Waiting | HeldState::Warning | HeldState::Sending)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            HeldState::Waiting => "waiting",
            HeldState::Warning => "warning",
            HeldState::Sending => "sending",
            HeldState::Delivered => "delivered",
            HeldState::Joined => "joined",
            HeldState::Expired => "expired",
            HeldState::Dismissed => "dismissed",
            HeldState::Failed => "failed",
        }
    }
}

/// One held DM invite (no secret: the invite itself is sealed in the secret
/// store).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldInvite {
    /// The DM.
    pub workspace_id: Uuid,
    /// The peer.
    pub account_id: Uuid,
    pub created_at: i64,
    pub expires_at: i64,
    /// The peer's account fingerprint when the DM was opened.
    pub fingerprint: String,
    /// Hex of the key-agreement keys approved when the DM was opened.
    pub keys: Vec<String>,
    pub state: HeldState,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub next_try_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<i64>,
    /// Why it waits or stopped, in words (never a secret).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl HeldInvite {
    /// What the API shows: `{state, pending, delivered, deliveredAt,
    /// expiresAt, warning}`.
    pub fn status_json(&self) -> serde_json::Value {
        serde_json::json!({
            "state": self.state.as_str(),
            "pending": self.state.is_open(),
            "delivered": self.state == HeldState::Delivered,
            "deliveredAt": self.delivered_at,
            "expiresAt": self.expires_at,
            "warning": (self.state == HeldState::Warning).then(|| self.note.clone()).flatten(),
        })
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn secret_id(ws: Uuid) -> SecretId {
    SecretId::new(SCOPE, ws.to_string())
}

/// Seconds to wait after `attempts` failed drops.
pub fn backoff_secs(attempts: u32) -> i64 {
    (RETRY_MIN_SECS << attempts.min(16)).min(RETRY_MAX_SECS)
}

/// What a pass does with one held invite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Nothing yet (no contact id, backing off, held for a warning).
    Wait,
    Expire,
    /// The peer is a member of the DM already.
    Joined,
    /// The DM is gone from this device.
    Gone,
    /// Their keys changed: hold for the opener with this warning.
    Warn(String),
    /// Seal to these devices and drop it into their box.
    Deliver(SealPlan),
}

/// The decision for one held invite, given what this device knows of the
/// peer now. Pure: the store and the pass apply it.
pub fn decide(item: &HeldInvite, person: Option<&CoMember>, dm_exists: bool, now: i64) -> Step {
    if !matches!(item.state, HeldState::Waiting | HeldState::Warning) {
        return Step::Wait;
    }
    if now >= item.expires_at {
        return Step::Expire;
    }
    if !dm_exists {
        return Step::Gone;
    }
    let Some(person) = person else { return Step::Wait };
    if person.workspaces.contains(&item.workspace_id) {
        return Step::Joined;
    }
    if item.state == HeldState::Warning {
        return Step::Wait;
    }
    match recheck(item, person) {
        Err(w) => Step::Warn(w),
        Ok(_) if person.contact.is_none() || now < item.next_try_at => Step::Wait,
        Ok(plan) => Step::Deliver(plan),
    }
}

/// The recipients to seal to now: the peer's current verified devices, all
/// of which were approved when the DM was opened, under the same account key.
fn recheck(item: &HeldInvite, person: &CoMember) -> Result<SealPlan, String> {
    let plan = person.seal_plan()?;
    if person.fingerprint.as_deref() != Some(item.fingerprint.as_str()) {
        return Err(format!(
            "{UNVERIFIED_RECIPIENT}: {}'s account key changed since you opened the DM; compare safety codes before it is sent",
            person.display_name
        ));
    }
    let new: Vec<&Recipient> = plan.recipients.iter().filter(|r| !item.keys.contains(&hex(&r.ka_public))).collect();
    if !new.is_empty() {
        return Err(format!(
            "{UNVERIFIED_RECIPIENT}: {} has {} device key{} you didn't approve when you opened the DM; confirm before it is sent to {}",
            person.display_name,
            new.len(),
            if new.len() == 1 { "" } else { "s" },
            if new.len() == 1 { "it" } else { "them" },
        ));
    }
    Ok(plan)
}

/// The outcome of one drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    Delivered,
    /// Not this time (no record yet, relay down…): retried with backoff.
    NotYet(String),
}

/// Drop `invite` into the box at `cid` on `client`'s relay, sealed to
/// `plan`'s devices: only when the record there verifies (`accept` runs
/// [`crate::contacts::accept_contact_record`] against the host's store), is
/// anchored to the account key the shared logs trust for `person` and names
/// their account. The invite is sealed to the keys from the config logs,
/// never to the record's. The desktop and `ligus serve` both drop through
/// here.
pub async fn drop_to_box(
    client: &crate::RelayClient,
    cid: &str,
    person: &CoMember,
    plan: &SealPlan,
    invite: &str,
    accept: impl FnOnce(&serde_json::Value) -> Result<crate::contacts::VerifiedContact, String>,
) -> Delivery {
    let record = match client.contact_get(cid).await {
        Ok(Some(r)) => r.record,
        Ok(None) => return Delivery::NotYet("their contact record isn't on their relay yet".into()),
        Err(e) => return Delivery::NotYet(format!("their contact record wasn't readable ({e})")),
    };
    match accept(&record) {
        Ok(v) if v.anchored && v.account_id == person.account_id => {}
        Ok(_) => {
            tracing::warn!(target: "contacts", "a co-member's contact id holds someone else's record; not delivering");
            return Delivery::NotYet("their contact id holds someone else's record".into());
        }
        Err(e) => return Delivery::NotYet(format!("their contact record was refused ({e})")),
    }
    let blob = match crate::contacts::seal_drop(invite.as_bytes(), &plan.recipients) {
        Ok(b) => b,
        Err(e) => return Delivery::NotYet(e),
    };
    match client.contact_drop(cid, &blob).await {
        Ok(()) => Delivery::Delivered,
        Err(e) => Delivery::NotYet(format!("the drop box didn't take it ({e})")),
    }
}

/// What a host provides to a pass.
pub trait Host {
    /// The co-members this device knows now (from its signed config logs).
    fn co_members(&self) -> Vec<CoMember>;
    /// Whether the DM is still on this device.
    fn dm_exists(&self, ws: Uuid) -> bool;
    /// This device's key-agreement key (opens the held invite).
    fn own_ka(&self) -> Option<KeyAgreementKeypair>;
    /// Drop `invite`, sealed to `plan`'s devices, into `person`'s box.
    fn deliver(&self, person: &CoMember, plan: &SealPlan, invite: &str) -> impl Future<Output = Delivery> + Send;
}

/// The held invites of one device.
pub struct HeldInvites {
    path: PathBuf,
    secrets: Arc<dyn SecretStore>,
    /// Serializes read-modify-write of the list.
    file: std::sync::Mutex<()>,
    /// Serializes passes.
    pass: tokio::sync::Mutex<()>,
}

impl HeldInvites {
    pub fn new(data_dir: &Path, secrets: Arc<dyn SecretStore>) -> Self {
        Self { path: data_dir.join(FILE), secrets, file: std::sync::Mutex::new(()), pass: tokio::sync::Mutex::new(()) }
    }

    pub fn for_data_dir(data_dir: &Path) -> Self {
        Self::new(data_dir, crate::secret_store::for_data_dir(data_dir))
    }

    /// The list file (metadata only).
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn load(&self) -> Vec<HeldInvite> {
        std::fs::read_to_string(&self.path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    fn save(&self, items: &[HeldInvite]) {
        let body = match serde_json::to_vec_pretty(items) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(target: "contacts", error = %e, "held DM invites not saved");
                return;
            }
        };
        if let Err(e) = crate::private_file::write_private_atomic(&self.path, &body) {
            tracing::warn!(target: "contacts", error = %e, "held DM invites not saved");
        }
    }

    /// Apply `f` to the list under the file lock and save it when it says so.
    fn update<T>(&self, f: impl FnOnce(&mut Vec<HeldInvite>) -> (T, bool)) -> T {
        let _g = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let mut items = self.load();
        let (out, changed) = f(&mut items);
        if changed {
            self.save(&items);
        }
        out
    }

    pub fn list(&self) -> Vec<HeldInvite> {
        let _g = self.file.lock().unwrap_or_else(|e| e.into_inner());
        self.load()
    }

    pub fn get(&self, ws: Uuid) -> Option<HeldInvite> {
        self.list().into_iter().find(|h| h.workspace_id == ws)
    }

    /// Hold `invite` for `person` (the DM `ws`), sealed to `own_ka_public`,
    /// approved for `plan`'s devices. Refused past [`MAX_HELD`].
    pub fn hold(
        &self,
        ws: Uuid,
        person: &CoMember,
        plan: &SealPlan,
        invite: &str,
        own_ka_public: &[u8; 32],
        now: i64,
    ) -> Result<HeldInvite, String> {
        let fingerprint = person.fingerprint.clone().ok_or_else(|| format!("{UNVERIFIED_RECIPIENT}: no account key for them"))?;
        let item = HeldInvite {
            workspace_id: ws,
            account_id: person.account_id,
            created_at: now,
            expires_at: now + HOLD_SECS,
            fingerprint,
            keys: plan.recipients.iter().map(|r| hex(&r.ka_public)).collect(),
            state: HeldState::Waiting,
            attempts: 0,
            next_try_at: now,
            delivered_at: None,
            note: None,
        };
        let (removed, full) = self.update(|items| {
            let (removed, _) = prune(items, now);
            let open = items.iter().filter(|h| h.state.is_open() && h.workspace_id != ws).count();
            let changed = !removed.is_empty();
            ((removed, open >= MAX_HELD), changed)
        });
        self.forget_all(&removed);
        if full {
            return Err(format!("{MAX_HELD} DM invites are already waiting for delivery; share this one's link"));
        }
        let sealed = hive_core::e2ee::seal(own_ka_public, invite.as_bytes()).map_err(|e| e.to_string())?;
        let blob = serde_json::to_vec(&sealed).map_err(|e| e.to_string())?;
        self.secrets.put(&secret_id(ws), &blob).map_err(|e| e.to_string())?;
        self.update(|items| {
            items.retain(|h| h.workspace_id != ws);
            items.push(item.clone());
            ((), true)
        });
        Ok(item)
    }

    /// The held invite's link, opened with this device's key (for sharing by
    /// hand). `None` once it is no longer held.
    pub fn open_invite(&self, ws: Uuid, ka: &KeyAgreementKeypair) -> Option<Zeroizing<String>> {
        let blob = self.secrets.get(&secret_id(ws)).ok().flatten()?;
        let sealed: hive_core::e2ee::SealedBlob = serde_json::from_slice(&blob).ok()?;
        let plain = Zeroizing::new(hive_core::e2ee::open(ka, &sealed).ok()?);
        String::from_utf8(plain.to_vec()).ok().map(Zeroizing::new)
    }

    fn forget(&self, ws: Uuid) {
        let _ = self.secrets.delete(&secret_id(ws));
    }

    fn forget_all(&self, ws: &[Uuid]) {
        for w in ws {
            self.forget(*w);
        }
    }

    /// Move `ws` from one of `from` to `to` (with `f` for the other fields),
    /// on disk. `None` when it wasn't in one of `from`: someone else moved
    /// it first.
    fn transition(&self, ws: Uuid, from: &[HeldState], to: HeldState, f: impl FnOnce(&mut HeldInvite)) -> Option<HeldInvite> {
        self.update(|items| match items.iter_mut().find(|h| h.workspace_id == ws && from.contains(&h.state)) {
            Some(h) => {
                h.state = to;
                f(h);
                (Some(h.clone()), true)
            }
            None => (None, false),
        })
    }

    /// The opener dismissed it (sharing the link by hand instead).
    pub fn dismiss(&self, ws: Uuid) -> Option<HeldInvite> {
        let out = self.transition(ws, &[HeldState::Waiting, HeldState::Warning], HeldState::Dismissed, |h| h.note = None);
        if out.is_some() {
            self.forget(ws);
        }
        out
    }

    /// After a warning, the opener confirmed `person`'s current keys: they
    /// become the approved recipients and delivery resumes. Still only to
    /// keys their account signed on the config logs.
    pub fn confirm(&self, ws: Uuid, person: &CoMember, now: i64) -> Result<HeldInvite, String> {
        let plan = person.seal_plan()?;
        let fingerprint = person.fingerprint.clone().ok_or_else(|| format!("{UNVERIFIED_RECIPIENT}: no account key for them"))?;
        self.transition(ws, &[HeldState::Warning], HeldState::Waiting, |h| {
            h.fingerprint = fingerprint;
            h.keys = plan.recipients.iter().map(|r| hex(&r.ka_public)).collect();
            h.note = None;
            h.next_try_at = now;
        })
        .ok_or_else(|| "nothing is waiting for your confirmation".to_string())
    }

    /// Whether anything still waits (a host idles its loop otherwise).
    pub fn any_open(&self) -> bool {
        self.list().iter().any(|h| h.state.is_open())
    }
}

/// Expire open entries past their expiry (a `sending` one too: it is never
/// resent) and drop finished ones past their keep time. Returns the
/// workspaces whose secret to forget and the entries that expired now.
fn prune(items: &mut Vec<HeldInvite>, now: i64) -> (Vec<Uuid>, Vec<HeldInvite>) {
    let mut forget = Vec::new();
    let mut expired = Vec::new();
    for h in items.iter_mut() {
        if h.state.is_open() && now >= h.expires_at {
            h.state = HeldState::Expired;
            h.note = None;
            forget.push(h.workspace_id);
            expired.push(h.clone());
        }
    }
    items.retain(|h| {
        let keep = h.state.is_open() || now < h.expires_at + KEEP_FINISHED_SECS;
        if !keep {
            forget.push(h.workspace_id);
        }
        keep
    });
    (forget, expired)
}

/// One pass over the held invites: expire, notice joins, warn on changed
/// keys, and deliver each one whose peer's contact id is known now. Returns
/// the entries that changed (for the host to announce).
pub async fn run_pass<H: Host>(host: &H, store: &HeldInvites, now: i64) -> Vec<HeldInvite> {
    let _serial = store.pass.lock().await;
    let (removed, mut changed) = store.update(|items| {
        let r = prune(items, now);
        let n = !r.0.is_empty();
        (r, n)
    });
    store.forget_all(&removed);
    let items = store.list();
    if !items.iter().any(|h| matches!(h.state, HeldState::Waiting | HeldState::Warning)) {
        return changed;
    }
    let people = host.co_members();
    for item in items {
        let person = people.iter().find(|p| p.account_id == item.account_id);
        let step = decide(&item, person, host.dm_exists(item.workspace_id), now);
        let open = [HeldState::Waiting, HeldState::Warning];
        match step {
            Step::Wait => {}
            Step::Expire | Step::Joined | Step::Gone => {
                let to = match step {
                    Step::Expire => HeldState::Expired,
                    Step::Joined => HeldState::Joined,
                    _ => HeldState::Dismissed,
                };
                if let Some(h) = store.transition(item.workspace_id, &open, to, |h| h.note = None) {
                    store.forget(item.workspace_id);
                    changed.push(h);
                }
            }
            Step::Warn(w) => {
                tracing::warn!(target: "contacts", "a held DM invite waits: the recipient's keys changed");
                if let Some(h) = store.transition(item.workspace_id, &[HeldState::Waiting], HeldState::Warning, |h| h.note = Some(w)) {
                    changed.push(h);
                }
            }
            Step::Deliver(plan) => {
                let Some(person) = person else { continue };
                // On disk before the drop: never sent twice.
                if store.transition(item.workspace_id, &[HeldState::Waiting], HeldState::Sending, |_| {}).is_none() {
                    continue;
                }
                let invite = host.own_ka().and_then(|ka| store.open_invite(item.workspace_id, &ka));
                let Some(invite) = invite else {
                    if let Some(h) = store.transition(item.workspace_id, &[HeldState::Sending], HeldState::Failed, |h| {
                        h.note = Some("this device can no longer open the held invite; share the link".into())
                    }) {
                        store.forget(item.workspace_id);
                        changed.push(h);
                    }
                    continue;
                };
                match host.deliver(person, &plan, &invite).await {
                    Delivery::Delivered => {
                        if let Some(h) = store.transition(item.workspace_id, &[HeldState::Sending], HeldState::Delivered, |h| {
                            h.delivered_at = Some(now);
                            h.note = None;
                        }) {
                            store.forget(item.workspace_id);
                            changed.push(h);
                        }
                    }
                    Delivery::NotYet(why) => {
                        if let Some(h) = store.transition(item.workspace_id, &[HeldState::Sending], HeldState::Waiting, |h| {
                            h.attempts = h.attempts.saturating_add(1);
                            h.next_try_at = now + backoff_secs(h.attempts);
                            h.note = Some(why);
                        }) {
                            changed.push(h);
                        }
                    }
                }
            }
        }
    }
    changed
}

#[cfg(test)]
#[path = "dm_delivery_tests.rs"]
mod tests;

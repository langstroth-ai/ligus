//! The paired-phone registry, `tunnel/phones.json` (0600).
//!
//! `ligus serve` holds it in memory and writes it through on every change;
//! `ligus phones list|revoke` read and edit the file from another process (a
//! revocation also goes through `pending/`, so `ligus serve` re-applies it even
//! if one of its own writes raced the CLI's).
//!
//! Format v2 (workspace model v3, 10.2): a new pairing names its workspace
//! device (`deviceId`); a pairing ended on this box (`ligus phones revoke`)
//! or by the phone (Forget host) says so (`unpaired`), so the phone's person
//! is asked whether to remove the device too; a revoked record is dropped
//! [`PURGE_REVOKED_AFTER_SECS`] after it was revoked; `ownerPhone` is gone.
//! Version 1 files load unchanged (their extra field is ignored).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{b64u_fixed, read_json, write_json};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhoneRecord {
    pub pid: String,
    pub name: String,
    /// b64u(phone auth Ed25519 public key): the credential's `pk`.
    pub auth_pub: String,
    /// b64u(phone Noise static public key).
    pub static_pub: String,
    /// b64u(push key) = HKDF(h_enroll, "push-key", 32).
    pub push_key: String,
    pub enrolled_at: u64,
    /// The latest `exp` issued to this phone (what a revocation lasts until).
    pub cred_exp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<u64>,
    #[serde(default)]
    pub push_enabled: bool,
    /// Whether pushes carry a message preview (§14). On by default, also for
    /// records written before the preference existed.
    #[serde(default = "default_true")]
    pub push_previews: bool,
    /// The push kinds the phone's notification switches allow (its `kinds`
    /// in `PUT /v1/tunnel/push`); `None` when it never said (an older app):
    /// every kind. See [`PhoneRecord::wants_kind`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_kinds: Option<Vec<String>>,
    /// Last push `seq` sent (strictly increasing, survives restarts).
    #[serde(default)]
    pub push_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<u64>,
    /// Who ended this pairing, when it was unpaired rather than cut by a
    /// revoked device certificate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unpaired: Option<Unpaired>,
    /// The phone's own workspace device (its P-256 Secure Enclave key,
    /// certified on the config log; Phase A, item 6). A `DeviceRevoked` of
    /// it revokes this pairing too: the tunnel is cut and pushes stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<uuid::Uuid>,
}

fn default_true() -> bool {
    true
}

/// Who ended a pairing (workspace model v3, 10.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Unpaired {
    /// `ligus phones revoke` on the box.
    Box,
    /// The phone's Forget host (`DELETE /v1/tunnel/pairing`).
    Phone,
}

impl Unpaired {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Box => "box",
            Self::Phone => "phone",
        }
    }
}

/// The current `phones.json` format.
pub const PHONES_FILE_VERSION: u32 = 2;

/// A revoked pairing is dropped from `phones.json` this long after it was
/// revoked (30 days). A phone without a record is refused like a revoked one.
pub const PURGE_REVOKED_AFTER_SECS: u64 = 30 * 86_400;

/// Keep only what isn't due for purging at `now`.
fn purge(phones: &mut Vec<PhoneRecord>, now: u64) -> bool {
    let before = phones.len();
    phones.retain(|r| r.revoked_at.is_none_or(|t| now.saturating_sub(t) < PURGE_REVOKED_AFTER_SECS));
    phones.len() != before
}

/// The push kinds a phone's notification switches turn on and off (the app
/// sends the ones it wants as `kinds`). Any other kind (a new phone paired,
/// a test push) isn't one of its switches and always goes.
pub const SWITCHED_PUSH_KINDS: &[&str] =
    &["proposal.created", "workflow.approval", "review.recruit", "ask.pending", "approval.request", "mention", "message.done"];

impl PhoneRecord {
    /// Whether the phone's notification switches let a `kind` push through.
    pub fn wants_kind(&self, kind: &str) -> bool {
        match &self.push_kinds {
            Some(kinds) if SWITCHED_PUSH_KINDS.contains(&kind) => kinds.iter().any(|k| k == kind),
            _ => true,
        }
    }

    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }

    pub fn static_key(&self) -> Option<[u8; 32]> {
        b64u_fixed(&self.static_pub).ok()
    }
}

#[derive(Default, Serialize, Deserialize)]
struct PhonesFile {
    #[serde(default)]
    v: u32,
    #[serde(default)]
    phones: Vec<PhoneRecord>,
}

pub struct Registry {
    path: PathBuf,
    inner: Mutex<Vec<PhoneRecord>>,
    /// Bumped on every save, so the revocation watch also re-checks a
    /// pairing stored after it last looked.
    generation: std::sync::atomic::AtomicU64,
}

impl Registry {
    pub fn file(dir: &Path) -> PathBuf {
        dir.join("phones.json")
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let path = Self::file(dir);
        let mut phones = read_json::<PhonesFile>(&path)?.map(|f| f.phones).unwrap_or_default();
        // In memory only: a read-only command (`ligus phones list`) never
        // rewrites the file under a running serve; the next save purges it.
        purge(&mut phones, super::now());
        Ok(Self { path, inner: Mutex::new(phones), generation: Default::default() })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<PhoneRecord>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, phones: &[PhoneRecord]) -> Result<()> {
        self.generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut phones = phones.to_vec();
        purge(&mut phones, super::now());
        write_json(&self.path, &PhonesFile { v: PHONES_FILE_VERSION, phones })
    }

    /// Changes whenever the registry is saved.
    pub fn generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn list(&self) -> Vec<PhoneRecord> {
        self.lock().clone()
    }

    pub fn get(&self, pid: &str) -> Option<PhoneRecord> {
        self.lock().iter().find(|r| r.pid == pid).cloned()
    }

    pub fn is_active(&self, pid: &str) -> bool {
        self.lock().iter().any(|r| r.pid == pid && r.is_active())
    }

    /// The active record whose Noise static is `static_pub` (S1).
    pub fn active_by_static(&self, static_pub: &[u8; 32]) -> Option<PhoneRecord> {
        self.lock().iter().find(|r| r.is_active() && r.static_key().as_ref() == Some(static_pub)).cloned()
    }

    /// Find by pid, or by name when it names exactly one phone.
    pub fn resolve(&self, pid_or_name: &str) -> Option<PhoneRecord> {
        let phones = self.lock();
        if let Some(r) = phones.iter().find(|r| r.pid == pid_or_name) {
            return Some(r.clone());
        }
        let named: Vec<_> = phones.iter().filter(|r| r.name == pid_or_name && r.is_active()).collect();
        (named.len() == 1).then(|| named[0].clone())
    }

    /// E3/E4: store an enrolled phone. An active record with the same static
    /// is replaced (the caller reused its pid).
    pub fn activate(&self, rec: PhoneRecord) -> Result<()> {
        // E3a: a pairing is always bound to its phone's workspace device.
        if rec.device_id.is_none() {
            anyhow::bail!("a phone pairing must be bound to its workspace device");
        }
        let mut phones = self.lock();
        phones.retain(|r| !(r.pid == rec.pid || (r.is_active() && r.static_pub == rec.static_pub)));
        phones.push(rec);
        self.save(&phones)
    }

    /// Mark `pid` revoked. Returns its latest credential `exp`, or `None` when
    /// there is no such phone.
    pub fn revoke(&self, pid: &str) -> Result<Option<u64>> {
        let mut phones = self.lock();
        let Some(r) = phones.iter_mut().find(|r| r.pid == pid) else { return Ok(None) };
        let exp = r.cred_exp;
        if r.revoked_at.is_none() {
            r.revoked_at = Some(super::now());
            self.save(&phones)?;
        }
        Ok(Some(exp))
    }

    /// Mark `pid`'s pairing as ended by `by` (it is revoked, or about to be).
    /// Pushes stop at once.
    pub fn mark_unpaired(&self, pid: &str, by: Unpaired) -> Result<bool> {
        let mut phones = self.lock();
        let Some(r) = phones.iter_mut().find(|r| r.pid == pid) else { return Ok(false) };
        if r.unpaired == Some(by) && !r.push_enabled {
            return Ok(true);
        }
        r.unpaired = Some(by);
        r.push_enabled = false;
        self.save(&phones)?;
        Ok(true)
    }

    /// The pairings unpaired on this box or by their phone (not cut by a
    /// revoked certificate) whose device has no other active pairing here
    /// and isn't revoked (`revoked`): the phone's person is asked whether to
    /// remove the device too. One per device, the newest, to the minute.
    pub fn unpaired_report(
        &self,
        host_id: &str,
        workspace_id: uuid::Uuid,
        at: i64,
        revoked: impl Fn(uuid::Uuid) -> bool,
    ) -> ligus_runtime::revocation_status::UnpairedReport {
        use ligus_runtime::revocation_status::{UnpairedPhone, UnpairedReport, MAX_UNPAIRED_REPORTED};
        // A copy: `revoked` may look at the roster (never under this lock).
        let phones = self.list();
        let active: std::collections::BTreeSet<uuid::Uuid> = phones.iter().filter(|r| r.is_active()).filter_map(|r| r.device_id).collect();
        let mut by_device: std::collections::BTreeMap<uuid::Uuid, UnpairedPhone> = Default::default();
        for r in phones.iter() {
            let (Some(d), Some(by), Some(t)) = (r.device_id, r.unpaired, r.revoked_at) else { continue };
            if active.contains(&d) || revoked(d) || (at.max(0) as u64).saturating_sub(t) >= PURGE_REVOKED_AFTER_SECS {
                continue;
            }
            let t = (t - t % 60) as i64;
            let e = by_device.entry(d).or_insert_with(|| UnpairedPhone { device_id: d.to_string(), at: t, by: by.as_str().into() });
            if t >= e.at {
                e.at = t;
                e.by = by.as_str().into();
            }
        }
        let mut list: Vec<UnpairedPhone> = by_device.into_values().collect();
        list.sort_by_key(|p| std::cmp::Reverse(p.at));
        list.truncate(MAX_UNPAIRED_REPORTED);
        UnpairedReport { host_id: host_id.to_string(), workspace_id, at, phones: list, signature: Vec::new() }
    }

    /// The active pairings of phones whose workspace device is in `revoked`.
    pub fn pairings_of_revoked(&self, revoked: &std::collections::BTreeSet<uuid::Uuid>) -> Vec<String> {
        self.lock()
            .iter()
            .filter(|r| r.is_active() && r.device_id.is_some_and(|d| revoked.contains(&d)))
            .map(|r| r.pid.clone())
            .collect()
    }

    /// The paired-phones report for the desktops' My devices: one entry per
    /// workspace device with an active pairing that `report` accepts (the
    /// box's owner's own phones: every workspace-key holder can open the
    /// report), its newest pairing's name, the latest it was seen, to the
    /// minute; unsigned.
    pub fn pairing_report(
        &self,
        host_id: &str,
        workspace_id: uuid::Uuid,
        at: i64,
        report: impl Fn(uuid::Uuid) -> bool,
    ) -> ligus_runtime::revocation_status::PairingReport {
        use ligus_runtime::revocation_status::{PairedPhone, PairingReport, MAX_PAIRED_REPORTED};
        let mut by_device: std::collections::BTreeMap<uuid::Uuid, (u64, PairedPhone)> = Default::default();
        for r in self.lock().iter().filter(|r| r.is_active()) {
            let Some(d) = r.device_id.filter(|d| report(*d)) else { continue };
            // To the minute: every workspace-key holder can read the report.
            let seen = r.last_seen.map(|t| (t - t % 60) as i64);
            let e = by_device.entry(d).or_insert_with(|| (r.enrolled_at, PairedPhone { device_id: d.to_string(), name: r.name.clone(), last_seen: seen }));
            if r.enrolled_at >= e.0 {
                e.0 = r.enrolled_at;
                e.1.name = r.name.clone();
            }
            e.1.last_seen = e.1.last_seen.max(seen);
        }
        let mut phones: Vec<PairedPhone> = by_device.into_values().map(|(_, p)| p).collect();
        phones.sort_by_key(|p| std::cmp::Reverse(p.last_seen));
        phones.truncate(MAX_PAIRED_REPORTED);
        PairingReport { host_id: host_id.to_string(), workspace_id, at, phones, signature: Vec::new() }
    }

    /// Record a newly issued credential `exp` (never moves backwards).
    pub fn set_cred_exp(&self, pid: &str, exp: u64) -> Result<()> {
        let mut phones = self.lock();
        if let Some(r) = phones.iter_mut().find(|r| r.pid == pid) {
            r.cred_exp = r.cred_exp.max(exp);
            self.save(&phones)?;
        }
        Ok(())
    }

    /// Set the push preference; `previews: None` and `kinds: None` keep the
    /// stored values. Returns the resulting `(enabled, previews)`, or `None`
    /// for no such active phone.
    pub fn set_push(&self, pid: &str, enabled: bool, previews: Option<bool>, kinds: Option<Vec<String>>) -> Result<Option<(bool, bool)>> {
        let mut phones = self.lock();
        let Some(r) = phones.iter_mut().find(|r| r.pid == pid && r.is_active()) else { return Ok(None) };
        r.push_enabled = enabled;
        if let Some(p) = previews {
            r.push_previews = p;
        }
        if kinds.is_some() {
            r.push_kinds = kinds;
        }
        let out = (r.push_enabled, r.push_previews);
        self.save(&phones)?;
        Ok(Some(out))
    }

    pub fn touch(&self, pid: &str) {
        let mut phones = self.lock();
        if let Some(r) = phones.iter_mut().find(|r| r.pid == pid) {
            r.last_seen = Some(super::now());
            let _ = self.save(&phones);
        }
    }

    /// Bump and persist the push `seq` for `pid`; returns it with the key.
    pub fn next_push_seq(&self, pid: &str) -> Option<(u64, [u8; 32])> {
        let mut phones = self.lock();
        let r = phones.iter_mut().find(|r| r.pid == pid)?;
        let key = b64u_fixed(&r.push_key).ok()?;
        r.push_seq += 1;
        let seq = r.push_seq;
        self.save(&phones).ok()?;
        Some((seq, key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(pid: &str, device: Option<uuid::Uuid>) -> PhoneRecord {
        PhoneRecord {
            pid: pid.into(),
            name: format!("phone {pid}"),
            auth_pub: format!("a-{pid}"),
            static_pub: format!("s-{pid}"),
            push_key: String::new(),
            enrolled_at: 10,
            cred_exp: 20,
            revoked_at: None,
            push_enabled: true,
            push_previews: true,
            push_kinds: None,
            push_seq: 0,
            last_seen: None,
            unpaired: None,
            device_id: device,
        }
    }

    /// `phones.json` v2: written as v2 without `ownerPhone`; a v1 file with
    /// it still loads; a pairing revoked over 30 days ago is dropped.
    #[test]
    fn v2_drops_owner_phone_and_purges_old_revocations() {
        let dir = tempfile::tempdir().unwrap();
        let now = crate::serve::tunnel::now();
        let d = uuid::Uuid::new_v4();
        let v1 = serde_json::json!({ "v": 1, "phones": [
            { "pid": "old", "name": "Old", "authPub": "a", "staticPub": "s", "pushKey": "", "enrolledAt": 1, "credExp": 2,
              "revokedAt": now - PURGE_REVOKED_AFTER_SECS - 5, "ownerPhone": true, "deviceId": d },
            { "pid": "recent", "name": "Recent", "authPub": "b", "staticPub": "t", "pushKey": "", "enrolledAt": 1, "credExp": 2,
              "revokedAt": now - 60, "deviceId": d },
            { "pid": "live", "name": "Live", "authPub": "c", "staticPub": "u", "pushKey": "", "enrolledAt": 1, "credExp": 2, "ownerPhone": true },
        ]});
        std::fs::write(Registry::file(dir.path()), v1.to_string()).unwrap();
        let reg = Registry::load(dir.path()).unwrap();
        let pids: Vec<String> = reg.list().into_iter().map(|r| r.pid).collect();
        assert_eq!(pids, ["recent", "live"], "the 30-day-old revocation is purged");
        let untouched: serde_json::Value = serde_json::from_slice(&std::fs::read(Registry::file(dir.path())).unwrap()).unwrap();
        assert_eq!(untouched, v1, "loading (ligus phones list) never rewrites the file");
        reg.save(&reg.list()).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(Registry::file(dir.path())).unwrap()).unwrap();
        assert_eq!(saved["v"], PHONES_FILE_VERSION);
        assert!(!saved.to_string().contains("ownerPhone"), "{saved}");
        // An unverified (older) pairing stays and is listed as such.
        assert!(reg.get("live").unwrap().device_id.is_none());
        // A new pairing must name its device.
        assert!(reg.activate(rec("new", None)).is_err());
    }

    /// The unpaired report: pairings ended on the box or by the phone,
    /// not cut by a certificate revoke, whose device has no other active
    /// pairing here and isn't revoked.
    #[test]
    fn the_unpaired_report_names_only_devices_still_worth_asking_about() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::load(dir.path()).unwrap();
        let (a, b, c, r) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        for (pid, d) in [("a", a), ("b1", b), ("b2", b), ("c", c), ("r", r)] {
            reg.activate(rec(pid, Some(d))).unwrap();
        }
        // a: the phone forgot the box. b: unpaired once, but paired again.
        // c: cut by a certificate revoke (no question). r: unpaired, then
        // its device revoked (answered).
        reg.mark_unpaired("a", Unpaired::Phone).unwrap();
        reg.revoke("a").unwrap();
        reg.mark_unpaired("b1", Unpaired::Box).unwrap();
        reg.revoke("b1").unwrap();
        reg.revoke("c").unwrap();
        reg.mark_unpaired("r", Unpaired::Box).unwrap();
        reg.revoke("r").unwrap();
        assert!(!reg.get("a").unwrap().push_enabled, "unpairing stops pushes at once");
        let now = crate::serve::tunnel::now() as i64;
        let report = reg.unpaired_report("box", uuid::Uuid::nil(), now, |d| d == r);
        let named: Vec<(String, String)> = report.phones.iter().map(|p| (p.device_id.clone(), p.by.clone())).collect();
        assert_eq!(named, vec![(a.to_string(), "phone".to_string())]);
        assert_eq!(report.phones[0].at % 60, 0, "to the minute");
        // Past the purge window it isn't asked about any more.
        let later = now + PURGE_REVOKED_AFTER_SECS as i64 + 60;
        assert!(reg.unpaired_report("box", uuid::Uuid::nil(), later, |_| false).phones.is_empty());
    }
}

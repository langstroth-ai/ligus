//! The paired-phone registry, `tunnel/phones.json` (0600).
//!
//! `ligus serve` holds it in memory and writes it through on every change;
//! `ligus phones list|revoke` read and edit the file from another process (a
//! revocation also goes through `pending/`, so `ligus serve` re-applies it even
//! if one of its own writes raced the CLI's).

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
    /// Last push `seq` sent (strictly increasing, survives restarts).
    #[serde(default)]
    pub push_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<u64>,
    /// The owner's own phone (P0.6 interim): set only by the desktop owner
    /// over a signed pairing confirmation. While serve acts for the owner
    /// (delegated), only an owner phone may vote, and its votes are recorded
    /// as delegated. No path sets it yet, so a delegated serve refuses every
    /// phone vote until passkey presence lands.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub owner_phone: bool,
    /// The phone's own workspace device (its P-256 Secure Enclave key,
    /// certified on the config log; Phase A, item 6). A `DeviceRevoked` of
    /// it revokes this pairing too: the tunnel is cut and pushes stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<uuid::Uuid>,
}

fn default_true() -> bool {
    true
}

impl PhoneRecord {
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
        let phones = read_json::<PhonesFile>(&path)?
            .map(|f| f.phones)
            .unwrap_or_default();
        Ok(Self {
            path,
            inner: Mutex::new(phones),
            generation: Default::default(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<PhoneRecord>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, phones: &[PhoneRecord]) -> Result<()> {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        write_json(
            &self.path,
            &PhonesFile {
                v: 1,
                phones: phones.to_vec(),
            },
        )
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
        self.lock()
            .iter()
            .find(|r| r.is_active() && r.static_key().as_ref() == Some(static_pub))
            .cloned()
    }

    /// Find by pid, or by name when it names exactly one phone.
    pub fn resolve(&self, pid_or_name: &str) -> Option<PhoneRecord> {
        let phones = self.lock();
        if let Some(r) = phones.iter().find(|r| r.pid == pid_or_name) {
            return Some(r.clone());
        }
        let named: Vec<_> = phones
            .iter()
            .filter(|r| r.name == pid_or_name && r.is_active())
            .collect();
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
        let Some(r) = phones.iter_mut().find(|r| r.pid == pid) else {
            return Ok(None);
        };
        let exp = r.cred_exp;
        if r.revoked_at.is_none() {
            r.revoked_at = Some(super::now());
            self.save(&phones)?;
        }
        Ok(Some(exp))
    }

    /// The active pairings of phones whose workspace device is in `revoked`.
    pub fn pairings_of_revoked(
        &self,
        revoked: &std::collections::BTreeSet<uuid::Uuid>,
    ) -> Vec<String> {
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
        let mut by_device: std::collections::BTreeMap<uuid::Uuid, (u64, PairedPhone)> =
            Default::default();
        for r in self.lock().iter().filter(|r| r.is_active()) {
            let Some(d) = r.device_id.filter(|d| report(*d)) else {
                continue;
            };
            // To the minute: every workspace-key holder can read the report.
            let seen = r.last_seen.map(|t| (t - t % 60) as i64);
            let e = by_device.entry(d).or_insert_with(|| {
                (
                    r.enrolled_at,
                    PairedPhone {
                        device_id: d.to_string(),
                        name: r.name.clone(),
                        last_seen: seen,
                    },
                )
            });
            if r.enrolled_at >= e.0 {
                e.0 = r.enrolled_at;
                e.1.name = r.name.clone();
            }
            e.1.last_seen = e.1.last_seen.max(seen);
        }
        let mut phones: Vec<PairedPhone> = by_device.into_values().map(|(_, p)| p).collect();
        phones.sort_by_key(|p| std::cmp::Reverse(p.last_seen));
        phones.truncate(MAX_PAIRED_REPORTED);
        PairingReport {
            host_id: host_id.to_string(),
            workspace_id,
            at,
            phones,
            signature: Vec::new(),
        }
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

    /// Set the push preference; `previews: None` keeps the stored value.
    /// Returns the resulting `(enabled, previews)`, or `None` for no such
    /// active phone.
    pub fn set_push(
        &self,
        pid: &str,
        enabled: bool,
        previews: Option<bool>,
    ) -> Result<Option<(bool, bool)>> {
        let mut phones = self.lock();
        let Some(r) = phones.iter_mut().find(|r| r.pid == pid && r.is_active()) else {
            return Ok(None);
        };
        r.push_enabled = enabled;
        if let Some(p) = previews {
            r.push_previews = p;
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

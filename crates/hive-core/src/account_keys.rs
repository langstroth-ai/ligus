//! Which signing key an account has on a workspace's log, and why a
//! registration or rotation does or doesn't count.
//!
//! Every reader of account keys (the ingest roster, the config fold's
//! roster and the signer directory) goes through [`KeyBook`], so they agree.
//!
//! An `account_id` is a random id, not derived from the key, so anyone who can
//! write to a log can publish `AccountKeyRegistered { account_id: <yours>,
//! key: <theirs> }`. The rules below decide which publications count. They
//! read only the event set (the log's registrations, rotations, certificates
//! and the anchors a fold found), in canonical order, so every device that
//! holds the same events reaches the same keys whatever order they arrived in.
//!
//! 1. **Self-evidence.** A registration of key `K` for account `A` counts
//!    only if its signing device published its *own* certificate for `A`
//!    under `K` (the certificate event is signed by the device it certifies)
//!    and the registration verifies under that device's key. A registration
//!    from any other device (a fresh, unknown one, or another member's) is
//!    refused, wherever it sorts, and never disputes `A`.
//! 2. **Anchor.** An account's first word on a log fixes its key when the
//!    fold can tell who that is:
//!    - the founder: the device that signed the base seed;
//!    - anyone else: the device that signed the account's own admission (the
//!      first allowed self-enrollment that made it a member: an invite or a
//!      box admission redeemed through that device).
//!
//!    The anchored key is the earliest self-evident registration whose key
//!    certified the anchor device in a certificate **the device published
//!    itself** (nobody else can sign as that device, so someone who certifies
//!    the device's key under a key of their own gains nothing). Every other
//!    key is then judged against the anchored lineage, wherever it sorts: a
//!    registration backdated before the account's own is refused too.
//! 3. **Lineage.** Once an account has a key, a registration of a different
//!    key counts only when the account's own device signed it: a device that
//!    published its own certificate under the account's current key. That is
//!    a **genuine dispute** (the account's own key material asserts two
//!    keys) and the account fails closed. Anyone else's registration is
//!    refused and attributed to its signer ([`KeyBook::refused`]).
//! 4. **Rotation.** `AccountKeyRotated` moves the account to a new key when
//!    the current key signed it. One signed by a key the account already
//!    rotated away from is a fork (that key designated two successors): a
//!    genuine dispute. Anything else (no key yet, a signature no key of the
//!    account made) is refused, never a dispute.
//! 5. **Unanchored accounts** (a member an Admin added directly, who never
//!    self-enrolled; a log with no allowed admission of the account) have no
//!    first word to go by: the earliest self-evident registration pins the
//!    key, and a later self-evident registration of another key from a device
//!    outside its lineage disputes the account (fail closed, as before),
//!    because nothing tells the genuine one apart. See the threat model.
//!
//! A founder pin (the invite's `fk`) still overrides all of this for the
//! founder: a registration of the founder's account under any other key is
//! refused.
//!
//! **Why not first-wins for everyone.** Canonical order is `(lamport, id)`,
//! and an author may name an older log head (offline devices do), so whoever
//! drafts "earliest" is the author's choice. With first-wins alone, a member
//! who backdates a self-certified registration before the owner's would own
//! the account; with dispute-on-conflict alone (the old rule), any member
//! silences anyone with one event. The anchor is the one fact on the log a
//! forger can't produce: a signature by the owner's own device on the
//! owner's own first word. Where there is none (rule 5), the old fail-closed
//! dispute stays, and only for self-evident conflicts.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use uuid::Uuid;

use crate::crypto::DeviceCertificate;
use crate::events::{SessionEvent, SessionEventEnvelope};

/// At most this many refusals are kept for attribution (the newest go).
pub const REFUSED_KEEP: usize = 256;

/// Certificates of one (account, device) kept for the rules: only the
/// device itself can add one ([`KeyIndex`]), so the cap can't crowd out an
/// honest device's own, and checking them stays linear.
pub const OWN_CERTS_KEEP: usize = 16;

/// A device's own certificates for one account (checked).
type OwnCerts = Arc<Vec<DeviceCertificate>>;
/// An account's self-evident registrations: (signing device, key).
type Evident = Arc<Vec<(Uuid, Vec<u8>)>>;

/// The certificates and registrations of one log, indexed for the rules
/// (structural: built from the whole event set, in canonical order).
/// Signatures are checked only for the (account, device) pairs a rule asks
/// about, once each.
#[derive(Debug, Default)]
pub struct KeyIndex {
    /// (account, device) → the certificate events of that device for that
    /// account whose envelope names the device itself as its signer
    /// (unchecked; earliest first).
    candidates: BTreeMap<(Uuid, Uuid), Vec<SessionEventEnvelope>>,
    /// (account, device) → the candidates that check out: the device
    /// published them itself (its signature verifies under the certified
    /// key), at most [`OWN_CERTS_KEEP`]. Filled on first ask.
    own: std::sync::Mutex<BTreeMap<(Uuid, Uuid), OwnCerts>>,
    /// account → its registrations in canonical order (unchecked).
    registrations: BTreeMap<Uuid, Vec<SessionEventEnvelope>>,
    /// account → its self-evident registrations in canonical order:
    /// (signing device, key). Filled on first ask.
    evident: std::sync::Mutex<BTreeMap<Uuid, Evident>>,
}

impl KeyIndex {
    /// Index the certificates and registrations among `envelopes`.
    pub fn of<'a>(envelopes: impl IntoIterator<Item = &'a SessionEventEnvelope>) -> Self {
        let mut ordered: Vec<&SessionEventEnvelope> = envelopes
            .into_iter()
            .filter(|e| matches!(e.payload, SessionEvent::DeviceCertificateAdded { .. } | SessionEvent::AccountKeyRegistered { .. }))
            .collect();
        ordered.sort_by_key(|e| e.canonical_key());
        let mut idx = Self::default();
        for e in ordered {
            match &e.payload {
                SessionEvent::DeviceCertificateAdded { certificate: c } if e.signer_device_id == Some(c.device_id) => {
                    idx.candidates.entry((c.account_id, c.device_id)).or_default().push(e.clone());
                }
                SessionEvent::AccountKeyRegistered { account_id, .. } => {
                    idx.registrations.entry(*account_id).or_default().push(e.clone());
                }
                _ => {}
            }
        }
        idx
    }

    /// The certificates `device` published of itself for `account` (checked
    /// once, then remembered).
    fn own_certs(&self, account: Uuid, device: Uuid) -> Arc<Vec<DeviceCertificate>> {
        if let Some(v) = self.own.lock().unwrap_or_else(|e| e.into_inner()).get(&(account, device)) {
            return Arc::clone(v);
        }
        let mut out: Vec<DeviceCertificate> = Vec::new();
        for e in self.candidates.get(&(account, device)).into_iter().flatten() {
            if out.len() >= OWN_CERTS_KEEP {
                break;
            }
            let SessionEvent::DeviceCertificateAdded { certificate: c } = &e.payload else { continue };
            if c.id_matches_key() && !out.contains(c) && crate::crypto::verify_envelope(e, &c.device_public_key).is_ok() {
                out.push(c.clone());
            }
        }
        let out = Arc::new(out);
        self.own.lock().unwrap_or_else(|e| e.into_inner()).insert((account, device), Arc::clone(&out));
        out
    }

    /// Whether `env` is signed by a device that published its own
    /// certificate for `account` under `key`, and verifies under that
    /// device's key: the registration is the device's own word about its
    /// account's key.
    pub fn self_evident(&self, env: &SessionEventEnvelope, account: Uuid, key: &[u8]) -> bool {
        let Some(device) = env.signer_device_id else { return false };
        self.own_certs(account, device)
            .iter()
            .any(|c| c.verify(key).is_ok() && crate::crypto::verify_envelope(env, &c.device_public_key).is_ok())
    }

    /// Whether `device` published, itself, a certificate of itself for
    /// `account` under `key` (only the device can sign that).
    pub fn self_vouched(&self, account: Uuid, device: Uuid, key: &[u8]) -> bool {
        self.own_certs(account, device).iter().any(|c| c.verify(key).is_ok())
    }

    /// The account's self-evident registrations, (device, key), in canonical
    /// order (checked once, then remembered).
    fn evident(&self, account: Uuid) -> Arc<Vec<(Uuid, Vec<u8>)>> {
        if let Some(v) = self.evident.lock().unwrap_or_else(|e| e.into_inner()).get(&account) {
            return Arc::clone(v);
        }
        let mut out: Vec<(Uuid, Vec<u8>)> = Vec::new();
        for e in self.registrations.get(&account).into_iter().flatten() {
            let SessionEvent::AccountKeyRegistered { signing_public_key: k, .. } = &e.payload else { continue };
            let Some(device) = e.signer_device_id else { continue };
            if out.iter().any(|(d, x)| *d == device && x == k) {
                continue;
            }
            // A device registers a handful of keys per account at most; past
            // that its registrations aren't checked (linear).
            if out.iter().filter(|(d, _)| *d == device).count() >= OWN_CERTS_KEEP {
                continue;
            }
            if self.self_evident(e, account, k) {
                out.push((device, k.clone()));
            }
        }
        let out = Arc::new(out);
        self.evident.lock().unwrap_or_else(|e| e.into_inner()).insert(account, Arc::clone(&out));
        out
    }

    /// Whether `device` itself vouches for `account` (disputed or not): it
    /// published its own certificate for the account under a key it
    /// registered itself, and `env` verifies under the device's key.
    pub fn device_vouches(&self, env: &SessionEventEnvelope, account: Uuid, device: Uuid) -> bool {
        let keys: Vec<Vec<u8>> = self.evident(account).iter().filter(|(d, _)| *d == device).map(|(_, k)| k.clone()).collect();
        self.own_certs(account, device)
            .iter()
            .any(|c| keys.iter().any(|k| c.verify(k).is_ok()) && crate::crypto::verify_envelope(env, &c.device_public_key).is_ok())
    }

    /// The keys `device` registered for `account` (self-evident ones).
    pub fn registered_by(&self, account: Uuid, device: Uuid) -> Vec<Vec<u8>> {
        self.evident(account).iter().filter(|(d, _)| *d == device).map(|(_, k)| k.clone()).collect()
    }

    /// Every self-evident registration's key, by account (in canonical
    /// order, each key once).
    pub fn registered_keys(&self) -> BTreeMap<Uuid, Vec<Vec<u8>>> {
        let mut out: BTreeMap<Uuid, Vec<Vec<u8>>> = BTreeMap::new();
        for a in self.registrations.keys() {
            let ks = out.entry(*a).or_default();
            for (_, k) in self.evident(*a).iter() {
                if !ks.contains(k) {
                    ks.push(k.clone());
                }
            }
        }
        out.retain(|_, ks| !ks.is_empty());
        out
    }

    /// The key anchored by `device` for `account` (rule 2): the earliest
    /// self-evident registration whose key certified the device in a
    /// certificate the device published itself.
    pub fn anchored_key(&self, account: Uuid, device: Uuid) -> Option<Vec<u8>> {
        self.evident(account).iter().find(|(_, k)| self.self_vouched(account, device, k)).map(|(_, k)| k.clone())
    }
}

/// What the book made of one key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyVerdict {
    /// The account's first key.
    Pinned,
    /// A key the account already has (or had): nothing changes.
    Same,
    /// The account moved to a new key.
    Rotated,
    /// The account's own lineage asserts two keys: the account fails closed.
    Disputed(&'static str),
    /// Doesn't count, and doesn't dispute anyone.
    Refused(&'static str),
    /// A rotation the log didn't allow (rules v2 presence): left out.
    Ignored,
}

/// One refused key event, for attribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedKey {
    pub event_id: Uuid,
    /// The account the event named.
    pub account_id: Uuid,
    /// The device that signed it.
    pub signer_device_id: Option<Uuid>,
    /// The account its stamp claims.
    pub stamp_account: Option<Uuid>,
    pub reason: &'static str,
}

#[derive(Debug, Clone)]
enum KeyEvent {
    Register { env: SessionEventEnvelope, key: Vec<u8> },
    Rotate { env: SessionEventEnvelope, new_key: Vec<u8>, sig: Vec<u8>, allowed: bool },
}

#[derive(Debug, Clone, Default)]
struct AccountState {
    /// Every key the account held, oldest first; the last is current.
    lineage: Vec<Vec<u8>>,
    disputed: Option<&'static str>,
    anchor: Option<Uuid>,
    /// The account's key events so far (replayed when an anchor arrives).
    events: Vec<KeyEvent>,
}

/// The account keys of one log (see the module docs).
#[derive(Debug, Clone, Default)]
pub struct KeyBook {
    index: Arc<KeyIndex>,
    /// A founder pin: (founder account, `fk`).
    pin: Option<(Uuid, [u8; 16])>,
    accounts: BTreeMap<Uuid, AccountState>,
    refused: Vec<RefusedKey>,
}

/// The `fk` of an account signing key: the first 16 bytes of its SHA-256.
pub fn key_fingerprint(signing_public_key: &[u8]) -> [u8; 16] {
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(signing_public_key);
    let mut out = [0u8; 16];
    out.copy_from_slice(&d[..16]);
    out
}

impl KeyBook {
    /// A book over `index`, with the founder pinned to `fk` when given.
    pub fn new(index: Arc<KeyIndex>, pin: Option<(Uuid, [u8; 16])>) -> Self {
        Self { index, pin, accounts: BTreeMap::new(), refused: Vec::new() }
    }

    /// The index this book reads.
    pub fn index(&self) -> &KeyIndex {
        &self.index
    }

    /// Judge an `AccountKeyRegistered` (in canonical order).
    pub fn register(&mut self, env: &SessionEventEnvelope) -> KeyVerdict {
        let SessionEvent::AccountKeyRegistered { account_id, signing_public_key } = &env.payload else {
            return KeyVerdict::Ignored;
        };
        let ev = KeyEvent::Register { env: env.clone(), key: signing_public_key.clone() };
        self.apply_new(*account_id, ev)
    }

    /// Judge an `AccountKeyRotated` (in canonical order). `allowed`: the log
    /// let it count (rules v2 presence); otherwise it is left out.
    pub fn rotate(&mut self, env: &SessionEventEnvelope, allowed: bool) -> KeyVerdict {
        let SessionEvent::AccountKeyRotated { account_id, new_signing_public_key, succession_signature } = &env.payload else {
            return KeyVerdict::Ignored;
        };
        let ev = KeyEvent::Rotate {
            env: env.clone(),
            new_key: new_signing_public_key.clone(),
            sig: succession_signature.clone(),
            allowed,
        };
        self.apply_new(*account_id, ev)
    }

    /// The account's first word on the log came from `device` (rule 2). The
    /// first anchor stands; the account's key events so far are judged again
    /// as if it had been known from the start.
    pub fn anchor(&mut self, account: Uuid, device: Uuid) -> bool {
        let st = self.accounts.entry(account).or_default();
        if st.anchor.is_some() {
            return false;
        }
        st.anchor = Some(device);
        let events = std::mem::take(&mut st.events);
        st.lineage.clear();
        st.disputed = None;
        // Refusals of this account are recomputed with the replay.
        self.refused.retain(|r| r.account_id != account);
        for ev in events {
            self.apply_new(account, ev);
        }
        true
    }

    /// Every anchor set so far: account → device.
    pub fn anchors(&self) -> BTreeMap<Uuid, Uuid> {
        self.accounts.iter().filter_map(|(a, s)| s.anchor.map(|d| (*a, d))).collect()
    }

    /// The account's current key, if it has one and isn't disputed.
    pub fn key(&self, account: Uuid) -> Option<&[u8]> {
        self.accounts.get(&account).filter(|s| s.disputed.is_none()).and_then(|s| s.lineage.last()).map(Vec::as_slice)
    }

    /// The account's current key even when disputed (what its certificates
    /// are checked against for revocation bookkeeping).
    pub fn key_any(&self, account: Uuid) -> Option<&[u8]> {
        self.accounts.get(&account).and_then(|s| s.lineage.last()).map(Vec::as_slice)
    }

    /// Whether the account's own lineage asserted two keys.
    pub fn is_disputed(&self, account: Uuid) -> bool {
        self.accounts.get(&account).is_some_and(|s| s.disputed.is_some())
    }

    /// Why the account is disputed, if it is.
    pub fn dispute_reason(&self, account: Uuid) -> Option<&'static str> {
        self.accounts.get(&account).and_then(|s| s.disputed)
    }

    /// Every disputed account.
    pub fn disputed(&self) -> BTreeSet<Uuid> {
        self.accounts.iter().filter(|(_, s)| s.disputed.is_some()).map(|(a, _)| *a).collect()
    }

    /// Every undisputed account with its current key.
    pub fn keys(&self) -> BTreeMap<Uuid, Vec<u8>> {
        self.accounts
            .iter()
            .filter(|(_, s)| s.disputed.is_none())
            .filter_map(|(a, s)| s.lineage.last().map(|k| (*a, k.clone())))
            .collect()
    }

    /// Every account with a key, disputed or not.
    pub fn keys_any(&self) -> BTreeMap<Uuid, Vec<u8>> {
        self.accounts.iter().filter_map(|(a, s)| s.lineage.last().map(|k| (*a, k.clone()))).collect()
    }

    /// The key events refused so far (at most [`REFUSED_KEEP`]), each with
    /// its signer.
    pub fn refused(&self) -> &[RefusedKey] {
        &self.refused
    }

    fn apply_new(&mut self, account: Uuid, ev: KeyEvent) -> KeyVerdict {
        let index = Arc::clone(&self.index);
        let pin = self.pin;
        let st = self.accounts.entry(account).or_default();
        st.events.push(ev.clone());
        let anchored = st.anchor.and_then(|d| index.anchored_key(account, d));
        let verdict = judge(&index, pin, account, st, &ev, anchored.as_deref());
        match verdict {
            KeyVerdict::Refused(reason) => {
                let env = match &ev {
                    KeyEvent::Register { env, .. } | KeyEvent::Rotate { env, .. } => env,
                };
                if self.refused.len() >= REFUSED_KEEP {
                    self.refused.remove(0);
                }
                self.refused.push(RefusedKey {
                    event_id: env.event_id,
                    account_id: account,
                    signer_device_id: env.signer_device_id,
                    stamp_account: env.actor_stamp.as_ref().and_then(|s| s.actor.account_id),
                    reason,
                });
            }
            KeyVerdict::Disputed(reason) => {
                let st = self.accounts.entry(account).or_default();
                st.disputed.get_or_insert(reason);
            }
            _ => {}
        }
        verdict
    }
}

/// Whether `env` was published by a device of `account` (signed, stamped as
/// that account): the only publisher an account key event can have.
fn published_by_account(env: &SessionEventEnvelope, account: Uuid) -> bool {
    env.signer_device_id.is_some()
        && env.signature.is_some()
        && env.actor_stamp.as_ref().and_then(|s| s.actor.account_id) == Some(account)
}

/// The rules (see the module docs) for one event, updating `st`.
fn judge(
    index: &KeyIndex,
    pin: Option<(Uuid, [u8; 16])>,
    account: Uuid,
    st: &mut AccountState,
    ev: &KeyEvent,
    anchored: Option<&[u8]>,
) -> KeyVerdict {
    match ev {
        KeyEvent::Register { env, key } => {
            if !published_by_account(env, account) {
                return KeyVerdict::Refused("not published by a device of the account it names");
            }
            if let Some((founder, fk)) = pin {
                if founder == account && key_fingerprint(key) != fk {
                    return KeyVerdict::Refused("not the founder key the invite pins");
                }
            }
            if !index.self_evident(env, account, key) {
                return KeyVerdict::Refused("not signed by a device its key certified");
            }
            if st.lineage.iter().any(|k| k == key) {
                return KeyVerdict::Same;
            }
            // The key the account's lineage is judged against: its current
            // key, or (before the anchored registration sorts) the anchored
            // key itself.
            let current: Option<&[u8]> = st.lineage.last().map(Vec::as_slice).or(anchored);
            let Some(current) = current else {
                st.lineage.push(key.clone());
                return KeyVerdict::Pinned;
            };
            if st.lineage.is_empty() && anchored == Some(key.as_slice()) {
                st.lineage.push(key.clone());
                return KeyVerdict::Pinned;
            }
            let signer = env.signer_device_id.unwrap_or_default();
            if index.self_vouched(account, signer, current) {
                return KeyVerdict::Disputed("the account's own device registered a second key");
            }
            if anchored.is_some() {
                return KeyVerdict::Refused("not signed by the account's own lineage");
            }
            // Unanchored: two self-evident registrations, nothing to tell the
            // genuine one apart (rule 5).
            KeyVerdict::Disputed("two self-evident registrations and no anchor to tell them apart")
        }
        KeyEvent::Rotate { env, new_key, sig, allowed } => {
            if !published_by_account(env, account) {
                return KeyVerdict::Refused("not published by a device of the account it names");
            }
            if !allowed {
                return KeyVerdict::Ignored;
            }
            let Some(current) = st.lineage.last().cloned() else {
                return KeyVerdict::Refused("a rotation of an account with no key");
            };
            if &current == new_key {
                return KeyVerdict::Same;
            }
            if crate::crypto::verify(&current, new_key, sig).is_ok() {
                st.lineage.push(new_key.clone());
                return KeyVerdict::Rotated;
            }
            let superseded = &st.lineage[..st.lineage.len() - 1];
            if superseded.iter().any(|k| crate::crypto::verify(k, new_key, sig).is_ok()) {
                return KeyVerdict::Disputed("a key the account rotated away from designated another successor");
            }
            KeyVerdict::Refused("not signed by the account's current key")
        }
    }
}

/// The anchor a log's base seed gives its founder: (account, signing device),
/// when the seed is stamped by an account and signed.
pub fn seed_anchor(seed: &SessionEventEnvelope) -> Option<(Uuid, Uuid)> {
    let stamp = seed.actor_stamp.as_ref()?;
    seed.signature.as_ref()?;
    Some((stamp.actor.account_id?, seed.signer_device_id?))
}

/// The anchor an allowed `MemberAdded` gives (rule 2): the joiner's own
/// device, when it is a self-enrollment (the member is its author, stamped
/// as its account) that admits someone who wasn't a member before.
pub fn admission_anchor(env: &SessionEventEnvelope, was_member: bool) -> Option<(Uuid, Uuid)> {
    let SessionEvent::MemberAdded { member, .. } = &env.payload else { return None };
    if was_member {
        return None;
    }
    let stamp = env.actor_stamp.as_ref()?;
    let account = member.actor.account_id?;
    if stamp.actor.id != member.id || stamp.actor.account_id != Some(account) || env.signature.is_none() {
        return None;
    }
    Some((account, env.signer_device_id?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{sign_envelope, SigningKeypair};
    use crate::identity::{ActorIdentity, ActorKind, ActorStamp};
    use proptest::prelude::*;

    struct Dev {
        id: Uuid,
        kp: SigningKeypair,
    }

    fn dev() -> Dev {
        let kp = SigningKeypair::generate().unwrap();
        Dev { id: crate::crypto::device_id_for(crate::crypto::SigAlg::Ed25519, &kp.public_key_bytes()), kp }
    }

    fn signed(d: &Dev, account: Uuid, lamport: u64, payload: SessionEvent) -> SessionEventEnvelope {
        let mut e = SessionEventEnvelope::new(Uuid::nil(), Uuid::nil(), lamport as i64, payload);
        e.lamport = lamport;
        let mut actor = ActorIdentity::new(account.to_string(), "x", ActorKind::Human);
        actor.account_id = Some(account);
        e.actor_stamp = Some(ActorStamp {
            actor,
            recorded_at: crate::Timestamp::epoch(),
            cfg_frontier: None,
            presence: None,
            runtime_effect: false,
            prev: None,
            sig_alg: Default::default(),
            log_head: None,
        });
        sign_envelope(&mut e, d.id, &d.kp);
        e
    }

    /// A device's identity for `account` under `key`: its own certificate
    /// and its registration.
    fn identity(d: &Dev, account: Uuid, key: &SigningKeypair, lamport: u64) -> Vec<SessionEventEnvelope> {
        let cert = DeviceCertificate::issue(key, account, &d.kp.public_key_bytes(), crate::Timestamp::epoch());
        vec![
            signed(d, account, lamport, SessionEvent::DeviceCertificateAdded { certificate: cert }),
            signed(d, account, lamport + 1, SessionEvent::AccountKeyRegistered { account_id: account, signing_public_key: key.public_key_bytes().to_vec() }),
        ]
    }

    fn run(envs: &[SessionEventEnvelope], anchor: Option<(Uuid, Uuid)>, anchor_at: usize) -> (BTreeMap<Uuid, Vec<u8>>, BTreeSet<Uuid>) {
        let mut ordered: Vec<&SessionEventEnvelope> = envs.iter().collect();
        ordered.sort_by_key(|e| e.canonical_key());
        let mut book = KeyBook::new(Arc::new(KeyIndex::of(ordered.iter().copied())), None);
        for (i, e) in ordered.iter().enumerate() {
            if i == anchor_at {
                if let Some((a, d)) = anchor {
                    book.anchor(a, d);
                }
            }
            match &e.payload {
                SessionEvent::AccountKeyRegistered { .. } => {
                    book.register(e);
                }
                SessionEvent::AccountKeyRotated { .. } => {
                    book.rotate(e, true);
                }
                _ => {}
            }
        }
        if anchor_at >= ordered.len() {
            if let Some((a, d)) = anchor {
                book.anchor(a, d);
            }
        }
        (book.keys(), book.disputed())
    }

    #[test]
    fn a_registration_no_certificate_under_its_key_names_is_refused() {
        let (owner, ghost) = (dev(), dev());
        let account = Uuid::new_v4();
        let k = SigningKeypair::generate().unwrap();
        let mut envs = identity(&owner, account, &k, 10);
        let other = SigningKeypair::generate().unwrap();
        for l in [1, 50] {
            envs.push(signed(&ghost, account, l, SessionEvent::AccountKeyRegistered { account_id: account, signing_public_key: other.public_key_bytes().to_vec() }));
        }
        let (keys, disputed) = run(&envs, None, 0);
        assert_eq!(keys.get(&account), Some(&k.public_key_bytes().to_vec()));
        assert!(disputed.is_empty());
    }

    proptest! {
        /// The book a fold builds when an anchor shows up partway through
        /// (the config timeline, which meets the admission in order) ends
        /// where a book with the anchor known from the start does (the ingest
        /// roster): the anchor replays the account's events.
        #[test]
        fn prop_a_late_anchor_ends_where_an_early_one_does(
            forgers in proptest::collection::vec((0u64..40, any::<bool>()), 0..4),
            own_at in 0u64..40,
            anchor_at in 0usize..16,
        ) {
            let (owner, other) = (dev(), dev());
            let account = Uuid::new_v4();
            let k = SigningKeypair::generate().unwrap();
            let mut envs = identity(&owner, account, &k, own_at);
            for (l, cert) in &forgers {
                let f = dev();
                let fk = SigningKeypair::generate().unwrap();
                if *cert {
                    envs.extend(identity(&f, account, &fk, *l));
                } else {
                    envs.push(signed(&f, account, *l, SessionEvent::AccountKeyRegistered { account_id: account, signing_public_key: fk.public_key_bytes().to_vec() }));
                }
            }
            let _ = other;
            let early = run(&envs, Some((account, owner.id)), 0);
            let late = run(&envs, Some((account, owner.id)), anchor_at);
            prop_assert_eq!(&early, &late);
            // Anchored: the owner's key, never disputed by forgers.
            prop_assert_eq!(early.0.get(&account), Some(&k.public_key_bytes().to_vec()));
            prop_assert!(early.1.is_empty());
        }
    }
}

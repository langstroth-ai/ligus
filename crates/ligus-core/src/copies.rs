//! Copies of one event id: which ones a fold reads.
//!
//! An event id is chosen by whoever signs the event, so a log can hold more
//! than one envelope under one id. Some of those are the same event (a copy
//! that travelled, or was re-sealed: same signer, signature and preimage).
//! The others are told apart by **who can be shown to have signed them**:
//!
//! * a copy is **attested** when its signature verifies under the key that
//!   a certificate on the log binds to its `signer_device_id` (a device id is
//!   derived from its key, [`crate::crypto::device_id_for`], so there is one
//!   such key), or under an agent key a certified host bound to that key id
//!   for the agent the stamp names;
//! * the **signer** of an attested copy is its device id together with that
//!   key.
//!
//! The rules, for an id held under more than one different copy:
//!
//! 1. If any copy is attested, every copy that isn't is left out: an unsigned
//!    copy, or one whose signature no certified key verifies, never displaces,
//!    invalidates or shadows a verified event, whichever arrived first.
//! 2. One signer with two or more different attested copies under the id has
//!    **equivocated** (owner decision on review P-A2c): every copy of that
//!    signer is left out, on every device. Only that signer's: someone else
//!    signing under the id can't make an honest signer equivocate, since that
//!    takes the honest signer's private key.
//! 3. The attested copies of every other signer are kept, one each. Two or
//!    more kept copies (different signers under one id) are each their own
//!    event: they fold side by side, in canonical order and then by
//!    [`CopyRef`], and everything the fold keys by event (denials, coverage,
//!    verdicts) is keyed by the copy.
//! 4. With no attested copy at all (a signer whose certificate hasn't
//!    arrived, or nothing signed), only the copies of the best kind are
//!    candidates (signed over unsigned); one candidate is kept, two or more
//!    are all left out until a certificate tells them apart (nothing verified
//!    is at stake yet).
//!
//! Every rule reads only the event set (the log, plus the config log's
//! certificates for a chat), so every device holding the same copies keeps
//! the same ones, whatever order they arrived in. Lamport witnesses
//! ([`crate::lamport::is_witness`]) are never copies: they are left out
//! before any of this.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use uuid::Uuid;

use crate::events::{SessionEvent, SessionEventEnvelope};

/// What makes two envelopes under one id the same event: the signer, the
/// signature and the signed preimage, however its bytes were encoded
/// ([`crate::crypto::canonical_preimage`]: a re-encoded copy of a genuine
/// envelope is that envelope, never its signer's second event).
pub(crate) type CopyKey = (Option<Uuid>, Option<Vec<u8>>, Vec<u8>);

pub(crate) fn copy_key(e: &SessionEventEnvelope) -> CopyKey {
    (e.signer_device_id, e.signature.clone(), crate::crypto::canonical_preimage(e))
}

/// One kept copy as the fold tells them apart: its event id, and `0` for an
/// id kept under a single copy, else its place among the id's kept copies
/// (from 1, in the order of their [`CopyKey`]s).
pub type CopyRef = (Uuid, u32);

/// Who can sign under each device id and agent key id, from the
/// certificates a log holds. Account keys and certificates are
/// self-validating (a certificate is signed by its account's key, an agent
/// key certificate by its host device's key), so every copy of them counts,
/// whatever the fold later decides about the events that carried them: a
/// key here only lets a copy signed with it be told apart from the others.
///
/// A device id is derived from its key ([`crate::crypto::device_id_for`]),
/// and a certificate under any other id never verifies, so each device id
/// here has exactly one key, whichever accounts certified it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct SignerDirectory {
    /// Device id → its key, with every account that certified it.
    devices: BTreeMap<Uuid, (Vec<u8>, BTreeSet<Uuid>)>,
    /// Agent key id → each key a host certified under it, with its agent.
    agents: BTreeMap<Uuid, Vec<(Vec<u8>, Uuid)>>,
}

impl SignerDirectory {
    /// The signers `envelopes` certify.
    ///
    /// Anyone able to deliver events controls how many keys, rotations and
    /// certificates there are, so building this stays near-linear in them:
    /// a certificate is checked only against the keys its own device or its
    /// publisher registered and the account's rotations, rotations chain
    /// only up to [`MAX_ACCOUNT_KEYS`] keys per account, and every (key,
    /// rotation) pair is checked at most once. A cap only stops further
    /// rotations from telling copies apart (no honest account rotates that
    /// often).
    pub fn of(envelopes: &[SessionEventEnvelope]) -> Self {
        let mut ordered: Vec<&SessionEventEnvelope> = envelopes.iter().collect();
        ordered.sort_by_key(|e| e.canonical_key());
        // Only a self-evident registration (signed by a device that
        // published its own certificate under that key for that account,
        // `crate::account_keys`) names a key here: a registration anyone else
        // published under someone's account id adds nothing and takes
        // nothing away. Every self-evident one counts, whoever's it is: a key
        // here only lets the devices it certified attest their *own*
        // signatures (a device id is its key's), so a second lineage under
        // an account (the roster refuses or disputes it) can't make the
        // genuine one's copies unattested.
        let index = crate::account_keys::KeyIndex::of(ordered.iter().copied());
        let registered = index.registered_keys();
        // A rotation counts once a key of its account signed it (rotations
        // chain): each newly known key is checked against the rotations
        // still pending for its account, once, and only while the account
        // holds fewer than MAX_ACCOUNT_KEYS keys (so this stays linear).
        // Account → (new key, succession signature) of each rotation.
        type Rotations<'r> = Vec<(&'r [u8], &'r [u8])>;
        let mut pending: BTreeMap<Uuid, Rotations<'_>> = BTreeMap::new();
        for e in &ordered {
            if let SessionEvent::AccountKeyRotated { account_id, new_signing_public_key, succession_signature } = &e.payload {
                pending.entry(*account_id).or_default().push((new_signing_public_key, succession_signature));
            }
        }
        // Account → the keys rotations reached.
        let mut rotated: BTreeMap<Uuid, Vec<Vec<u8>>> = BTreeMap::new();
        for (account, mut rotations) in pending {
            let Some(regs) = registered.get(&account) else { continue };
            let mut known: Vec<Vec<u8>> = regs.iter().take(MAX_ACCOUNT_KEYS).cloned().collect();
            let base = known.len();
            let mut next = 0;
            while next < known.len() && !rotations.is_empty() && known.len() < MAX_ACCOUNT_KEYS {
                let k = known[next].clone();
                next += 1;
                rotations.retain(|(new, sig)| {
                    if known.iter().any(|x| x.as_slice() == *new) {
                        return false;
                    }
                    if known.len() < MAX_ACCOUNT_KEYS && crate::crypto::verify(&k, new, sig).is_ok() {
                        known.push(new.to_vec());
                        return false;
                    }
                    true
                });
            }
            rotated.insert(account, known.split_off(base));
        }
        let mut out = Self::default();
        for e in &ordered {
            if let SessionEvent::DeviceCertificateAdded { certificate: c } = &e.payload {
                // An account already known to certify this device adds
                // nothing (its certificate is checked once).
                if out.devices.get(&c.device_id).is_some_and(|(_, a)| a.contains(&c.account_id)) {
                    continue;
                }
                // The keys it may be under: the ones the certified device
                // registered, the ones the device that published it
                // registered (an enrollment), and the account's rotations.
                // A few each, so this stays linear however many keys others
                // registered under the account. `verify` refuses an id its key
                // doesn't derive.
                let publisher = e.signer_device_id.unwrap_or_default();
                let ok = index
                    .registered_by(c.account_id, c.device_id)
                    .iter()
                    .chain(index.registered_by(c.account_id, publisher).iter())
                    .chain(rotated.get(&c.account_id).into_iter().flatten())
                    .any(|k| c.verify(k).is_ok());
                if ok {
                    let entry = out.devices.entry(c.device_id).or_insert_with(|| (c.device_public_key.clone(), BTreeSet::new()));
                    entry.1.insert(c.account_id);
                }
            }
        }
        for e in &ordered {
            if let SessionEvent::AgentKeyCertified { cert } = &e.payload {
                let host_ok = out.devices.get(&cert.host_device_id).is_some_and(|(k, _)| cert.verify(k).is_ok());
                if host_ok {
                    push_unique(out.agents.entry(cert.key_id).or_default(), (cert.signing_public_key.clone(), cert.agent_id));
                }
            }
        }
        out
    }

    /// Add every signer `other` knows.
    pub fn extend(&mut self, other: &SignerDirectory) {
        for (d, (k, accounts)) in &other.devices {
            let mine = self.devices.entry(*d).or_insert_with(|| (k.clone(), BTreeSet::new()));
            mine.1.extend(accounts.iter().copied());
        }
        for (d, ks) in &other.agents {
            let mine = self.agents.entry(*d).or_default();
            for k in ks {
                push_unique(mine, k.clone());
            }
        }
    }

    /// Each device id with its key and every account that certified it.
    pub fn devices(&self) -> impl Iterator<Item = (Uuid, (&[u8], &BTreeSet<Uuid>))> {
        self.devices.iter().map(|(d, (k, a))| (*d, (k.as_slice(), a)))
    }

    /// The key that attests `e` (see the module docs), if any: the one key of
    /// its device id, or an agent key certified for the agent its stamp
    /// names. Only that device (or agent) can sign under it.
    pub fn attesting_key(&self, e: &SessionEventEnvelope) -> Option<&[u8]> {
        let signer = e.signer_device_id?;
        e.signature.as_ref()?;
        let stamp = e.actor_stamp.as_ref();
        if let Some(keys) = self.agents.get(&signer) {
            for (k, agent) in keys {
                if stamp.is_some_and(|s| s.actor.id == agent.to_string()) && crate::crypto::verify_envelope(e, k).is_ok() {
                    return Some(k);
                }
            }
        }
        let (k, _) = self.devices.get(&signer)?;
        crate::crypto::verify_envelope(e, k).is_ok().then_some(k.as_slice())
    }
}

/// The envelopes a [`SignerDirectory`] is built from: account keys, device
/// certificates and agent-key certificates.
pub fn signer_events(envelopes: &[SessionEventEnvelope]) -> Vec<SessionEventEnvelope> {
    envelopes
        .iter()
        .filter(|e| {
            matches!(
                e.payload,
                SessionEvent::AccountKeyRegistered { .. }
                    | SessionEvent::AccountKeyRotated { .. }
                    | SessionEvent::DeviceCertificateAdded { .. }
                    | SessionEvent::AgentKeyCertified { .. }
            )
        })
        .cloned()
        .collect()
}

/// At most this many keys of one account tell copies apart (registrations
/// and the rotations they reach, in canonical order).
pub const MAX_ACCOUNT_KEYS: usize = 64;

fn push_unique<T: PartialEq>(v: &mut Vec<T>, x: T) {
    if !v.contains(&x) {
        v.push(x);
    }
}

/// How the kept copies of the ids held under more than one different copy
/// are told apart.
#[derive(Debug, Clone, Default)]
pub struct Copies {
    /// Each such id → the [`CopyKey`]s of its kept copies, in their
    /// [`CopyRef`] order (empty when none is kept).
    multi: HashMap<Uuid, Vec<CopyKey>>,
}

impl Copies {
    /// The copy `e` is, if a fold keeps it (`None`: a copy left out).
    pub fn of(&self, e: &SessionEventEnvelope) -> Option<CopyRef> {
        match self.multi.get(&e.event_id) {
            None => Some((e.event_id, 0)),
            Some(kept) if kept.len() == 1 => (kept[0] == copy_key(e)).then_some((e.event_id, 0)),
            Some(kept) => {
                let k = copy_key(e);
                kept.iter().position(|x| *x == k).map(|i| (e.event_id, i as u32 + 1))
            }
        }
    }

    /// [`Copies::of`] for a copy the fold kept.
    pub(crate) fn kept(&self, e: &SessionEventEnvelope) -> CopyRef {
        self.of(e).unwrap_or((e.event_id, u32::MAX))
    }
}

/// The copies a fold reads (see the module docs).
pub(crate) struct Resolved<'a> {
    /// The kept copies: one per id, or one per signer for a contested id
    /// (sorted canonically, then by copy, when there is one).
    pub kept: Cow<'a, [SessionEventEnvelope]>,
    /// Ids no copy of which is kept.
    pub dropped: BTreeSet<Uuid>,
    pub copies: Copies,
}

/// Resolve `envelopes` (witnesses already left out). `directory` is built
/// only when some id is held under more than one different copy.
pub(crate) fn resolve<'a>(envelopes: &'a [SessionEventEnvelope], directory: impl FnOnce() -> SignerDirectory) -> Resolved<'a> {
    let mut count: HashMap<Uuid, usize> = HashMap::new();
    for e in envelopes {
        *count.entry(e.event_id).or_default() += 1;
    }
    if count.values().all(|c| *c == 1) {
        return Resolved { kept: Cow::Borrowed(envelopes), dropped: BTreeSet::new(), copies: Copies::default() };
    }
    // The different copies of each repeated id, by first position.
    let mut groups: BTreeMap<Uuid, Vec<(usize, CopyKey)>> = BTreeMap::new();
    for (i, e) in envelopes.iter().enumerate().filter(|(_, e)| count[&e.event_id] > 1) {
        let k = copy_key(e);
        let g = groups.entry(e.event_id).or_default();
        if !g.iter().any(|(_, x)| *x == k) {
            g.push((i, k));
        }
    }
    let mut directory = Some(directory);
    let mut built: Option<SignerDirectory> = None;
    let mut keep: BTreeSet<usize> = BTreeSet::new();
    let mut dropped = BTreeSet::new();
    let mut multi: HashMap<Uuid, Vec<CopyKey>> = HashMap::new();
    for (id, group) in groups {
        if group.len() == 1 {
            keep.insert(group[0].0);
            continue;
        }
        if built.is_none() {
            built = directory.take().map(|f| f());
        }
        let d = built.as_ref().expect("built once");
        let attested: Vec<(usize, &CopyKey, &[u8])> =
            group.iter().filter_map(|(i, k)| d.attesting_key(&envelopes[*i]).map(|key| (*i, k, key))).collect();
        let kept: Vec<(usize, &CopyKey)> = if attested.is_empty() {
            let signed = |i: usize| envelopes[i].signer_device_id.is_some() && envelopes[i].signature.is_some();
            let best = group.iter().any(|(i, _)| signed(*i));
            let candidates: Vec<(usize, &CopyKey)> = group.iter().filter(|(i, _)| signed(*i) == best).map(|(i, k)| (*i, k)).collect();
            if candidates.len() == 1 {
                candidates
            } else {
                Vec::new()
            }
        } else {
            // (device id, verifying key) → that signer's copies.
            type Signer<'k> = (Uuid, &'k [u8]);
            let mut by_signer: BTreeMap<Signer<'_>, Vec<(usize, &CopyKey)>> = BTreeMap::new();
            for (i, k, key) in attested {
                let signer = envelopes[i].signer_device_id.expect("an attested copy is signed");
                by_signer.entry((signer, key)).or_default().push((i, k));
            }
            by_signer.into_values().filter(|v| v.len() == 1).map(|v| v[0]).collect()
        };
        let mut keys: Vec<CopyKey> = kept.iter().map(|(_, k)| (*k).clone()).collect();
        keys.sort();
        if keys.is_empty() {
            dropped.insert(id);
        }
        keep.extend(kept.iter().map(|(i, _)| *i));
        multi.insert(id, keys);
    }
    let copies = Copies { multi };
    let mut kept: Vec<SessionEventEnvelope> =
        envelopes.iter().enumerate().filter(|(i, e)| count[&e.event_id] == 1 || keep.contains(i)).map(|(_, e)| e.clone()).collect();
    if copies.multi.values().any(|k| k.len() > 1) {
        kept.sort_by_cached_key(|e| (e.canonical_key(), copies.kept(e).1));
    }
    Resolved { kept: Cow::Owned(kept), dropped, copies }
}

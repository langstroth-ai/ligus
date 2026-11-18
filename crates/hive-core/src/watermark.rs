//! Removal watermarks (Phase A, item 2).
//!
//! A chat event is judged against the workspace roster at the config frontier
//! its author claims (`ActorStamp::cfg_frontier`). A removed (or demoted)
//! member, or a revoked device, still holds its keys and could claim a
//! frontier from before the removal: without more, such a backdated event
//! folds as if it predated the removal.
//!
//! Every event's stamp names the **previous event its signer put on the same
//! log** (`ActorStamp::prev`), so a signer's events on one log form a chain.
//! A `MemberRemoved`, a demoting `MemberRoleChanged` or a `DeviceRevoked`
//! carries a watermark: for each signer it cuts and each log, the chain
//! heads the author held. The fold then treats an event from a cut signer as
//! **covered** only if it is reachable back from those heads; every other
//! event of that signer is judged as of the cut (a removal: not a member; a
//! demotion: the new role; a revocation: refused), whatever frontier it
//! claims. Reachability is a function of the event set, so the verdict is the
//! same in every delivery order. A forged event can't join a chain: the
//! heads' `prev` links are signed and fixed.
//!
//! What a cut applies to: a removal or demotion of member `m` cuts every
//! event authored by `m` (any signer) and, for a removal, every event signed
//! by an agent key of an agent `m` owns; a revocation cuts every event the
//! device signed. A signer or log the watermark doesn't name has no covered
//! events.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::events::SessionEventEnvelope;

/// The newest events one signer had on one log, as the cut's author held
/// them (usually one head; more when the signer's chain forked, e.g. two
/// processes sharing a device key).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Watermark {
    /// A device id or an agent key id (an envelope's `signer_device_id`).
    pub signer: Uuid,
    /// The log (session id).
    pub log: Uuid,
    pub heads: Vec<Uuid>,
    /// The ids the signer's held events name as `prev` that the author did
    /// not hold. Coverage stops there: an id a member names but never
    /// publishes is not a covered slot they can fill after the cut (review
    /// P-A2c). Left out when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<Uuid>,
}

/// At most this many entries in one watermark (a removal event stays small:
/// signers × logs the remover held).
pub const MAX_WATERMARK_ENTRIES: usize = 4096;
/// At most this many heads per entry.
pub const MAX_HEADS: usize = 8;

/// One chain link: an event's signer and the `prev` it names.
type Link = (Option<Uuid>, Option<Uuid>);

/// One log's signer chains: event id → each (signer, prev) under it, one
/// per different copy (more than one only when signers' copies share the
/// id, see [`crate::copies`]).
#[derive(Debug, Default, Clone)]
pub struct Chains {
    links: BTreeMap<Uuid, Vec<Link>>,
}

impl Chains {
    pub fn of(envelopes: &[&SessionEventEnvelope]) -> Self {
        let mut count: BTreeMap<Uuid, usize> = BTreeMap::new();
        for e in envelopes {
            *count.entry(e.event_id).or_default() += 1;
        }
        let mut links: BTreeMap<Uuid, Vec<Link>> = BTreeMap::new();
        // The different copies of a repeated id (an exact duplicate is one).
        let mut seen: BTreeMap<Uuid, Vec<crate::copies::CopyKey>> = BTreeMap::new();
        for e in envelopes {
            if count[&e.event_id] > 1 {
                let k = crate::copies::copy_key(e);
                let v = seen.entry(e.event_id).or_default();
                if v.contains(&k) {
                    continue;
                }
                v.push(k);
            }
            links.entry(e.event_id).or_default().push((e.signer_device_id, e.actor_stamp.as_ref().and_then(|s| s.prev)));
        }
        Self { links }
    }

    /// Every event of `signer` reachable back from `heads` through `prev`
    /// links (heads included), staying on that signer's events and never
    /// entering one of `gaps` (an id the watermark's author didn't hold).
    ///
    /// An id under which the log holds more than one copy signed under
    /// `signer` (two keys bound to one device id: a second key of the
    /// member's own, or another account's binding of it) is never covered
    /// and coverage stops there: which copy the watermark's author held
    /// can't be told, and following either's `prev` would let a copy signed
    /// after the cut extend a chain it was never part of (fail closed, as at
    /// a gap).
    pub fn ancestors(&self, signer: Uuid, heads: &[Uuid], gaps: &[Uuid]) -> BTreeSet<Uuid> {
        let mut out = BTreeSet::new();
        let mut stack: Vec<Uuid> = heads.iter().filter(|h| !gaps.contains(h)).copied().collect();
        while let Some(id) = stack.pop() {
            if gaps.contains(&id) {
                continue;
            }
            let Some(links) = self.links.get(&id) else { continue };
            let mut mine = links.iter().filter(|(s, _)| *s == Some(signer));
            let (Some((_, prev)), None) = (mine.next(), mine.next()) else { continue };
            if !out.insert(id) {
                continue;
            }
            stack.extend(*prev);
        }
        out
    }
}

/// The heads of every signer in `signers` on one log: their events no other
/// event of the same signer names as `prev` (what a watermark records).
///
/// Each entry also lists its gaps: the `prev` ids its signer's events name
/// that aren't on the log here. An entry with more than [`MAX_HEADS`] gaps
/// is left out (it covers nothing: fail closed).
pub fn heads_of(envelopes: &[SessionEventEnvelope], log: Uuid, signers: &BTreeSet<Uuid>) -> Vec<Watermark> {
    let held: BTreeSet<Uuid> = envelopes.iter().filter(|e| e.session_id == log).map(|e| e.event_id).collect();
    let mut by_signer: BTreeMap<Uuid, (BTreeSet<Uuid>, BTreeSet<Uuid>)> = BTreeMap::new();
    for e in envelopes.iter().filter(|e| e.session_id == log) {
        let Some(s) = e.signer_device_id.filter(|s| signers.contains(s)) else { continue };
        let entry = by_signer.entry(s).or_default();
        entry.0.insert(e.event_id);
        if let Some(p) = e.actor_stamp.as_ref().and_then(|st| st.prev) {
            entry.1.insert(p);
        }
    }
    by_signer
        .into_iter()
        .filter_map(|(signer, (all, named))| {
            let mut heads: Vec<Uuid> = all.difference(&named).copied().collect();
            heads.truncate(MAX_HEADS);
            let gaps: Vec<Uuid> = named.difference(&held).copied().collect();
            (gaps.len() <= MAX_HEADS).then_some(Watermark { signer, log, heads, gaps })
        })
        .collect()
}

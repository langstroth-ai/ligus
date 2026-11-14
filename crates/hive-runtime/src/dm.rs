//! Direct messages between two accounts (spec: IDENTITY-AND-FORGES §4).
//!
//! A DM is an ordinary two-person workspace. Its room and its single-use
//! invite id are random, chosen by whoever opens it, and travel only inside
//! the invite (sealed to the peer's verified keys, or as a link or QR the
//! two people hand over themselves). Nothing about a DM is derived from a
//! public identifier, so neither the relay nor anyone else can compute
//! whether two people have one.
//!
//! - The initiator founds it: the invite's `founder_actor_id` is the
//!   initiator's account id and its `fk` the initiator's account key
//!   fingerprint, pinned by the peer on join like any founder pin.
//! - The invite's `dm_account` names the peer's account id, so it can't be
//!   replayed to someone else.
//! - The peer accepts only when the founder's account and `fk` match what
//!   the config logs they share register for that account
//!   ([`accept_dm_invite`]); a stranger needs the safety code compared first.
//! - Two people who open a DM at the same moment get two; both sides keep the
//!   one with the smaller `(created_at, room)` and archive the other
//!   ([`resolve_concurrent`]).

use std::collections::BTreeMap;

use hive_core::Timestamp;
use uuid::Uuid;

use crate::directory_trust::CoMember;

/// The room prefix of a direct message.
pub const DM_ROOM_PREFIX: &str = "dm-";

/// A fresh DM room: `dm-` and 128 random bits.
pub fn new_room() -> String {
    format!("{DM_ROOM_PREFIX}{}", Uuid::new_v4().simple())
}

/// A fresh single-use invite id for a DM (random, never derived).
pub fn new_invite_id() -> Uuid {
    Uuid::new_v4()
}

/// What this device knows about who an invite says founded it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FounderTrust {
    /// A co-member whose account key, on the logs this device holds, has the
    /// invite's fingerprint.
    KnownMatch,
    /// A co-member whose account key is a different one (or disputed): the
    /// invite is not from them.
    Mismatch,
    /// An account this device shares no workspace with: compare the safety
    /// code out of band.
    Unknown,
    /// The invite names no founder or carries no fingerprint.
    NoFingerprint,
}

/// Each co-member's account fingerprint (`None` when the logs disagree).
pub fn known_fingerprints(co_members: &[CoMember]) -> BTreeMap<Uuid, Option<String>> {
    co_members
        .iter()
        .map(|c| (c.account_id, if c.key_conflict { None } else { c.fingerprint.clone() }))
        .collect()
}

/// Check an invite's founder account and `fk` against what this device
/// knows.
pub fn check_founder(founder: Option<&str>, fk: Option<&str>, known: &BTreeMap<Uuid, Option<String>>) -> FounderTrust {
    let (Some(founder), Some(fk)) = (founder.and_then(|f| Uuid::parse_str(f.trim()).ok()), fk) else {
        return FounderTrust::NoFingerprint;
    };
    let fk = fk.trim().to_ascii_lowercase();
    if fk.len() != 32 {
        return FounderTrust::NoFingerprint;
    }
    match known.get(&founder) {
        Some(Some(k)) if *k == fk => FounderTrust::KnownMatch,
        Some(_) => FounderTrust::Mismatch,
        None => FounderTrust::Unknown,
    }
}

/// Decide whether this device (account `me`) may accept a DM invite. On
/// success, returns the peer's account id (the DM's founder).
///
/// - `dm_account` must name `me`: an invite addressed to someone else is
///   refused.
/// - The founder must be someone else, named by account id, with an `fk`.
/// - A co-member must match the key the shared logs register for them.
/// - An account this device shares nothing with is accepted only once the
///   person confirmed the safety code (`safety_code_confirmed`).
pub fn accept_dm_invite(
    dm_account: Option<&str>,
    founder: Option<&str>,
    fk: Option<&str>,
    me: Uuid,
    known: &BTreeMap<Uuid, Option<String>>,
    safety_code_confirmed: bool,
) -> Result<Uuid, String> {
    let to = dm_account.and_then(|a| Uuid::parse_str(a.trim()).ok());
    if to != Some(me) {
        return Err("this direct message invite is addressed to someone else".into());
    }
    let peer = founder.and_then(|f| Uuid::parse_str(f.trim()).ok()).ok_or("the invite doesn't say who sent it")?;
    if peer == me {
        return Err("that's your own invite".into());
    }
    match check_founder(founder, fk, known) {
        FounderTrust::KnownMatch => Ok(peer),
        FounderTrust::Unknown if safety_code_confirmed => Ok(peer),
        FounderTrust::Unknown => Err("unknown sender: compare their safety code before accepting".into()),
        FounderTrust::Mismatch => {
            Err("the invite's key doesn't match the one your shared workspaces have for that person".into())
        }
        FounderTrust::NoFingerprint => Err("the invite carries no safety code".into()),
    }
}

/// One DM workspace with a given peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmCandidate {
    pub room: String,
    /// When the DM was founded (the earliest event on its config log); `None`
    /// until this device holds any of it.
    pub created_at: Option<Timestamp>,
}

/// Which DM to keep when two exist for one pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmResolution {
    pub keep: String,
    pub archive: Vec<String>,
}

/// Owner decision D12: of several DMs with one peer, keep the one whose
/// `(created_at, room)` is smallest and archive the rest. Both sides hold the
/// same logs, so both compute the same answer. `None` while fewer than two
/// exist or any founding time is still unknown (decide once synced).
pub fn resolve_concurrent(candidates: &[DmCandidate]) -> Option<DmResolution> {
    if candidates.len() < 2 || candidates.iter().any(|c| c.created_at.is_none()) {
        return None;
    }
    let mut sorted: Vec<&DmCandidate> = candidates.iter().collect();
    sorted.sort_by(|a, b| (a.created_at, &a.room).cmp(&(b.created_at, &b.room)));
    sorted.dedup_by(|a, b| a.room == b.room);
    let keep = sorted[0].room.clone();
    let archive = sorted[1..].iter().map(|c| c.room.clone()).collect();
    Some(DmResolution { keep, archive })
}

/// Apply a resolution to this device's archived rooms: the kept room is
/// (again) live and the others are archived. Recomputed every time, so a
/// resolution that changes as logs sync never leaves both DMs archived.
pub fn apply_resolution(archived: &mut Vec<String>, resolution: &DmResolution) {
    archived.retain(|r| *r != resolution.keep);
    for room in &resolution.archive {
        if !archived.contains(room) {
            archived.push(room.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(secs: i64) -> Timestamp {
        Timestamp(time::OffsetDateTime::from_unix_timestamp(secs).unwrap())
    }

    /// S9: rooms and invite ids are random, so nobody can compute them.
    #[test]
    fn dm_room_is_random() {
        let a = new_room();
        let b = new_room();
        assert_ne!(a, b);
        assert!(a.starts_with(DM_ROOM_PREFIX));
        let hex = &a[DM_ROOM_PREFIX.len()..];
        assert_eq!(hex.len(), 32);
        let id = Uuid::parse_str(hex).unwrap();
        assert_eq!(id.get_version_num(), 4, "a v4 (random) id, not a v5 derivation");
        // Not the 1.x derivation over the same pair, for any pair.
        let legacy = Uuid::new_v5(&crate::frozen_ns::DM_NS, b"1001\x002002");
        assert_ne!(hex, legacy.simple().to_string());
        assert_ne!(new_invite_id(), new_invite_id());
        assert_eq!(new_invite_id().get_version_num(), 4);
    }

    /// S9: an id an observer could enumerate from public data (two account
    /// ids) gives no information about the room: 1 000 DMs opened between the
    /// same two accounts never repeat a room.
    #[test]
    fn dm_id_unguessable_from_the_pair() {
        let rooms: std::collections::BTreeSet<String> = (0..1000).map(|_| new_room()).collect();
        assert_eq!(rooms.len(), 1000);
    }

    /// D12: both sides converge on the same DM whatever order they list them.
    #[test]
    fn simultaneous_dm_converges() {
        let a = DmCandidate { room: new_room(), created_at: Some(ts(1_800_000_000)) };
        let b = DmCandidate { room: new_room(), created_at: Some(ts(1_800_000_001)) };
        let mine = resolve_concurrent(&[a.clone(), b.clone()]).unwrap();
        let theirs = resolve_concurrent(&[b.clone(), a.clone()]).unwrap();
        assert_eq!(mine, theirs);
        assert_eq!(mine.keep, a.room, "the older one is kept");
        assert_eq!(mine.archive, vec![b.room.clone()]);
        // Same second: the smaller room wins, on both sides.
        let c = DmCandidate { room: "dm-0000".into(), created_at: Some(ts(5)) };
        let d = DmCandidate { room: "dm-ffff".into(), created_at: Some(ts(5)) };
        assert_eq!(resolve_concurrent(&[d.clone(), c.clone()]).unwrap().keep, "dm-0000");
        assert_eq!(resolve_concurrent(&[c, d]).unwrap().keep, "dm-0000");
        // Not yet synced: no decision.
        let e = DmCandidate { room: new_room(), created_at: None };
        assert_eq!(resolve_concurrent(&[a.clone(), e]), None);
        assert_eq!(resolve_concurrent(&[a]), None);
    }

    #[test]
    fn dm_invite_accepted_only_for_me_from_a_matching_key() {
        let me = Uuid::new_v4();
        let peer = Uuid::new_v4();
        let fk = "00112233445566778899aabbccddeeff";
        let other = "ffeeddccbbaa99887766554433221100";
        let mut known = BTreeMap::new();
        known.insert(peer, Some(fk.to_string()));
        let (me_s, peer_s) = (me.to_string(), peer.to_string());
        assert_eq!(accept_dm_invite(Some(&me_s), Some(&peer_s), Some(fk), me, &known, false), Ok(peer));
        // Addressed to someone else (a replayed invite).
        assert!(accept_dm_invite(Some(&Uuid::new_v4().to_string()), Some(&peer_s), Some(fk), me, &known, true).is_err());
        // The co-member's name with another key: refused even when "confirmed".
        assert!(accept_dm_invite(Some(&me_s), Some(&peer_s), Some(other), me, &known, true).is_err());
        // No fingerprint at all.
        assert!(accept_dm_invite(Some(&me_s), Some(&peer_s), None, me, &known, true).is_err());
        // A stranger: only after the safety code was compared.
        let stranger = Uuid::new_v4().to_string();
        assert!(accept_dm_invite(Some(&me_s), Some(&stranger), Some(fk), me, &known, false).is_err());
        assert!(accept_dm_invite(Some(&me_s), Some(&stranger), Some(fk), me, &known, true).is_ok());
        // A co-member whose key the logs dispute is a mismatch.
        known.insert(peer, None);
        assert_eq!(check_founder(Some(&peer_s), Some(fk), &known), FounderTrust::Mismatch);
        // A legacy `github:<id>` founder is no account id.
        assert_eq!(check_founder(Some("github:42"), Some(fk), &known), FounderTrust::NoFingerprint);
    }

    /// D12 over time: Alice first sees DM A as the older one and archives B;
    /// later the founding times she holds say B is older. She must end with
    /// exactly one live DM (B), not two archived ones.
    #[test]
    fn d12_archive_set_is_recomputed_not_accumulated() {
        let a = DmCandidate { room: "dm-a".into(), created_at: Some(ts(100)) };
        let b = DmCandidate { room: "dm-b".into(), created_at: Some(ts(101)) };
        let mut archived = Vec::new();
        apply_resolution(&mut archived, &resolve_concurrent(&[a.clone(), b.clone()]).unwrap());
        assert_eq!(archived, vec!["dm-b".to_string()]);
        let b_older = DmCandidate { created_at: Some(ts(50)), ..b };
        apply_resolution(&mut archived, &resolve_concurrent(&[a, b_older]).unwrap());
        assert_eq!(archived, vec!["dm-a".to_string()], "exactly one of the two stays live");
    }
}

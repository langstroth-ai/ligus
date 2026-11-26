//! Prepared drafts waiting for a phone (presence, admissions, phone
//! actions): single use, in memory, for a few minutes, and kept per
//! credential ([`super::api::Caller::key`]: a tunnel pairing, the direct-path
//! token, the operator).
//!
//! A caller sees and spends only its own drafts, and its cap evicts only its
//! own oldest one: one phone can't flush, use or confirm another's.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ligus_core::presence::b64url;

/// A fresh draft id: 16 random bytes, base64url. No OS randomness, no id (an
/// all-zero id would be guessable, and the next one would collide with it).
pub fn draft_id(
    fill: fn(&mut [u8]) -> Result<(), getrandom::Error>,
) -> Result<String, getrandom::Error> {
    let mut raw = [0u8; 16];
    fill(&mut raw)?;
    Ok(b64url(&raw))
}

struct Entry<T> {
    owner: String,
    at: Instant,
    draft: T,
}

/// Why a draft wasn't kept.
#[derive(Debug, PartialEq, Eq)]
pub enum PutError {
    /// No OS randomness for its id.
    Randomness,
    /// The box holds as many drafts as it keeps, none of them this caller's.
    Full,
}

/// Drafts by id, each with the credential that made it.
pub struct OwnedDrafts<T> {
    m: Mutex<HashMap<String, Entry<T>>>,
    ttl: Duration,
    per_owner: usize,
    total: usize,
}

impl<T> OwnedDrafts<T> {
    pub fn new(ttl: Duration, per_owner: usize, total: usize) -> Self {
        Self {
            m: Mutex::new(HashMap::new()),
            ttl,
            per_owner,
            total,
        }
    }

    /// Keep `draft` for `owner`. At its cap, the owner's oldest draft goes;
    /// with the box full of other callers' drafts, this one is refused.
    pub fn put(&self, owner: &str, draft: T) -> Result<String, PutError> {
        let id = draft_id(getrandom::fill).map_err(|_| PutError::Randomness)?;
        let mut m = self.m.lock().unwrap_or_else(|e| e.into_inner());
        let ttl = self.ttl;
        m.retain(|_, e| e.at.elapsed() < ttl);
        let mine = m.values().filter(|e| e.owner == owner).count();
        if mine >= self.per_owner {
            if let Some(oldest) = m
                .iter()
                .filter(|(_, e)| e.owner == owner)
                .min_by_key(|(_, e)| e.at)
                .map(|(k, _)| k.clone())
            {
                m.remove(&oldest);
            }
        } else if m.len() >= self.total {
            return Err(PutError::Full);
        }
        m.insert(
            id.clone(),
            Entry {
                owner: owner.to_string(),
                at: Instant::now(),
                draft,
            },
        );
        Ok(id)
    }

    /// Take `owner`'s draft `id` (single use). Another caller's draft is
    /// left where it is and answers `None`, like an unknown id.
    pub fn take(&self, owner: &str, id: &str) -> Option<T> {
        let mut m = self.m.lock().unwrap_or_else(|e| e.into_inner());
        if m.get(id).is_some_and(|e| e.owner != owner) {
            return None;
        }
        let ttl = self.ttl;
        m.remove(id)
            .filter(|e| e.at.elapsed() < ttl)
            .map(|e| e.draft)
    }

    /// Put a taken draft back under its id, keeping its age.
    pub fn put_back(&self, owner: &str, id: String, draft: T, at: Instant) {
        self.m.lock().unwrap_or_else(|e| e.into_inner()).insert(
            id,
            Entry {
                owner: owner.to_string(),
                at,
                draft,
            },
        );
    }

    /// Take `owner`'s draft `id` with its age (for [`Self::put_back`]).
    pub fn take_aged(&self, owner: &str, id: &str) -> Option<(T, Instant)> {
        let mut m = self.m.lock().unwrap_or_else(|e| e.into_inner());
        if m.get(id).is_some_and(|e| e.owner != owner) {
            return None;
        }
        let ttl = self.ttl;
        m.remove(id)
            .filter(|e| e.at.elapsed() < ttl)
            .map(|e| (e.draft, e.at))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_ids_need_randomness() {
        assert!(draft_id(|_| Err(getrandom::Error::UNSUPPORTED)).is_err());
        let (a, b) = (
            draft_id(getrandom::fill).unwrap(),
            draft_id(getrandom::fill).unwrap(),
        );
        assert_ne!(a, b);
        assert_eq!(a.len(), 22);
    }

    /// Serve hardening: one phone filling its drafts never evicts another's,
    /// and can't take or confirm one that isn't its own.
    #[test]
    fn one_caller_cannot_flush_or_spend_anothers_drafts() {
        let d: OwnedDrafts<u32> = OwnedDrafts::new(Duration::from_secs(300), 4, 6);
        let a = d.put("tunnel:a", 1).unwrap();
        for i in 0..20 {
            d.put("tunnel:b", 100 + i).unwrap();
        }
        assert_eq!(d.take("tunnel:b", &a), None, "not b's to take");
        assert_eq!(
            d.take("tunnel:a", &a),
            Some(1),
            "b's flood left it in place"
        );
        assert_eq!(d.take("tunnel:a", &a), None, "single use");
        // b holds 4 (its cap), a holds 1, c 1: the box is full for a newcomer.
        d.put("tunnel:a", 2).unwrap();
        d.put("tunnel:c", 3).unwrap();
        assert_eq!(d.put("tunnel:d", 4), Err(PutError::Full));
        // At its own cap a caller evicts only its own oldest.
        for _ in 0..4 {
            d.put("tunnel:b", 9).unwrap();
        }
        let mine =
            d.m.lock()
                .unwrap()
                .values()
                .filter(|e| e.owner == "tunnel:a")
                .count();
        assert_eq!(mine, 1);
    }

    #[test]
    fn drafts_expire() {
        let d: OwnedDrafts<u32> = OwnedDrafts::new(Duration::from_millis(1), 4, 6);
        let id = d.put("operator", 1).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(d.take("operator", &id), None);
    }
}

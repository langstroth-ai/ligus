//! The per-turn MCP credential (D.3.2).
//!
//! `LIGUS_TURN_CRED = base64url(turn_id(16) || chat_id(16) || agent_id(16) ||
//! u64le(expires_unix) || HMAC-SHA256(k_host, "ligus-mcp-turn-cred-v1\0" ||
//! turn_id || chat_id || agent_id || u64le(expires_unix)))`.
//!
//! `k_host` is random per host process and never leaves memory. The
//! credential is device-local and never synced: it binds one Ligus MCP
//! connection to one chat, one agent and one turn, and stops working when
//! the turn ends (expiry, plus revocation of the turn id by the server).

use base64::Engine;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Domain-separation tag (Phase 0 list, section 0.1).
pub const TURN_CRED_TAG: &[u8] = b"ligus-mcp-turn-cred-v1\0";

const CRED_LEN: usize = 16 * 3 + 8 + 32;

/// What a credential binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnBinding {
    pub turn_id: Uuid,
    pub chat_id: Uuid,
    pub agent_id: Uuid,
    pub expires_unix: u64,
}

/// HMAC-SHA256 (RFC 2104) over `parts`.
pub fn hmac_sha256(key: &[u8; 32], parts: &[&[u8]]) -> [u8; 32] {
    let mut k = [0u8; 64];
    k[..32].copy_from_slice(key);
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    for p in parts {
        inner.update(p);
    }
    let ih = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(ih);
    outer.finalize().into()
}

fn mac(key: &[u8; 32], b: &TurnBinding) -> [u8; 32] {
    hmac_sha256(
        key,
        &[
            TURN_CRED_TAG,
            b.turn_id.as_bytes(),
            b.chat_id.as_bytes(),
            b.agent_id.as_bytes(),
            &b.expires_unix.to_le_bytes(),
        ],
    )
}

/// Mint a credential for `b` under `key`.
pub fn mint(key: &[u8; 32], b: &TurnBinding) -> String {
    let mut raw = Vec::with_capacity(CRED_LEN);
    raw.extend_from_slice(b.turn_id.as_bytes());
    raw.extend_from_slice(b.chat_id.as_bytes());
    raw.extend_from_slice(b.agent_id.as_bytes());
    raw.extend_from_slice(&b.expires_unix.to_le_bytes());
    raw.extend_from_slice(&mac(key, b));
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

/// Why a credential was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredError {
    Malformed,
    BadMac,
    Expired,
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Verify `cred` under `key` at `now_unix`: its MAC, then its expiry.
pub fn verify(key: &[u8; 32], cred: &str, now_unix: u64) -> Result<TurnBinding, CredError> {
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cred.trim())
        .map_err(|_| CredError::Malformed)?;
    if raw.len() != CRED_LEN {
        return Err(CredError::Malformed);
    }
    let id = |r: &[u8]| Uuid::from_slice(r).map_err(|_| CredError::Malformed);
    let b = TurnBinding {
        turn_id: id(&raw[0..16])?,
        chat_id: id(&raw[16..32])?,
        agent_id: id(&raw[32..48])?,
        expires_unix: u64::from_le_bytes(raw[48..56].try_into().map_err(|_| CredError::Malformed)?),
    };
    if !ct_eq(&mac(key, &b), &raw[56..]) {
        return Err(CredError::BadMac);
    }
    if now_unix >= b.expires_unix {
        return Err(CredError::Expired);
    }
    Ok(b)
}

/// A fresh random host key.
pub fn host_key() -> [u8; 32] {
    let mut k = [0u8; 32];
    getrandom::fill(&mut k).expect("os randomness");
    k
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc4231_case_1() {
        // RFC 4231 test case 1 uses a 20-byte key; ours is fixed at 32 bytes,
        // so check against a zero-padded 32-byte key (HMAC pads keys anyway).
        let mut key = [0u8; 32];
        key[..20].copy_from_slice(&[0x0b; 20]);
        let tag = hmac_sha256(&key, &[b"Hi There"]);
        let hex: String = tag.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn cred_round_trips_and_is_tamper_evident() {
        let k = host_key();
        let b = TurnBinding {
            turn_id: Uuid::new_v4(),
            chat_id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
            expires_unix: 2_000,
        };
        let c = mint(&k, &b);
        assert_eq!(verify(&k, &c, 1_000), Ok(b));
        assert_eq!(verify(&k, &c, 2_000), Err(CredError::Expired));
        assert_eq!(
            verify(&host_key(), &c, 1_000),
            Err(CredError::BadMac),
            "another host's key"
        );
        // Flip each byte of the bound fields: always refused.
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&c)
            .unwrap();
        for i in 0..56 {
            let mut r = raw.clone();
            r[i] ^= 1;
            let t = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r);
            assert!(verify(&k, &t, 1_000).is_err(), "byte {i}");
        }
        assert_eq!(verify(&k, "x", 0), Err(CredError::Malformed));
    }
}

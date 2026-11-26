//! The founder's signature on a workspace or DM invite (spec:
//! IDENTITY-AND-FORGES §4; review item R1).
//!
//! An invite (`hivews1:`, pasted or dropped into a contact box) names its
//! founder by account id and key fingerprint (`fk`). Both are public, so on
//! their own they prove nothing about who sent it: anyone can write a
//! co-member's id and fingerprint into an invite. The founder's device
//! therefore signs the invite's security-relevant fields with its device
//! key, and attaches its device certificate and the account key. A reader
//! labels the invite as coming from the founder only when:
//!
//! - the account key's fingerprint is the invite's `fk`,
//! - the certificate names the invite's founder and verifies under that key,
//! - the certified device is not one the reader's logs revoked, and
//! - the device signature verifies over the invite's fields.
//!
//! Anything else is shown as "says it's from …".
//!
//! Domain string (Phase 0 §0.1): `ligus-invite-v1\0`.

use base64::Engine;
use ligus_core::crypto::{DeviceCertificate, SigningKeypair};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::envelope_verifier::FounderPin;

/// Domain separation for an invite signature (Phase 0 §0.1).
pub const INVITE_SIG_CONTEXT: &[u8] = b"ligus-invite-v1\0";

/// The founder's signature an invite carries (`sig` in the invite JSON).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteSignature {
    /// The founder's account signing key (base64url, 32 bytes).
    pub k: String,
    /// The signing device's certificate under that key.
    pub cert: DeviceCertificate,
    /// Unix seconds the invite was signed.
    pub at: i64,
    /// Ed25519 by the certified device over [`preimage`] (base64url).
    pub s: String,
}

/// The fields of an invite a signature covers.
#[derive(Debug, Clone, Copy, Default)]
pub struct InviteClaims<'a> {
    pub name: &'a str,
    pub relay_url: &'a str,
    pub room: &'a str,
    pub key: Option<&'a str>,
    pub dm_account: Option<&'a str>,
    pub founder: Option<&'a str>,
    pub fk: Option<&'a str>,
    pub inv: Option<&'a str>,
    /// The canonical JSON of the delivered epoch keys (empty when none).
    pub ek_json: &'a [u8],
}

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn unb64(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s.trim().as_bytes()).ok()
}

/// What the device signs: the tag, then each field NUL-terminated (an absent
/// field is empty), the SHA-256 of the bearer secrets (key and epoch keys)
/// rather than the secrets, and the signing time.
pub fn preimage(c: &InviteClaims<'_>, at: i64) -> Vec<u8> {
    let mut out = Vec::with_capacity(256);
    out.extend_from_slice(INVITE_SIG_CONTEXT);
    for field in [
        c.name.trim(),
        c.relay_url.trim(),
        c.room.trim(),
        c.dm_account.unwrap_or("").trim(),
        c.founder.unwrap_or("").trim(),
        &c.fk.unwrap_or("").trim().to_ascii_lowercase(),
        c.inv.unwrap_or("").trim(),
    ] {
        out.extend_from_slice(field.as_bytes());
        out.push(0);
    }
    out.extend_from_slice(&Sha256::digest(c.key.unwrap_or("").as_bytes()));
    out.extend_from_slice(&Sha256::digest(c.ek_json));
    out.extend_from_slice(&at.to_be_bytes());
    out
}

/// Sign `claims` with this device's key.
pub fn sign(
    claims: &InviteClaims<'_>,
    at: i64,
    device: &SigningKeypair,
    cert: &DeviceCertificate,
    account_public_key: &[u8],
) -> InviteSignature {
    let s = device.sign(&preimage(claims, at));
    InviteSignature { k: b64(account_public_key), cert: cert.clone(), at, s: b64(&s) }
}

/// Check `sig` over `claims`. On success, the founder account it proves.
/// `revoked` answers whether a device is revoked on the reader's logs.
pub fn verify(sig: &InviteSignature, claims: &InviteClaims<'_>, revoked: &dyn Fn(Uuid) -> bool) -> Result<Uuid, String> {
    let founder = claims
        .founder
        .and_then(|f| Uuid::parse_str(f.trim()).ok())
        .ok_or("the invite names no founder account")?;
    let fk = claims.fk.map(|f| f.trim().to_ascii_lowercase()).ok_or("the invite carries no fingerprint")?;
    let key = unb64(&sig.k).filter(|k| k.len() == 32).ok_or("the signature's account key is malformed")?;
    if FounderPin::fingerprint_hex(&key) != fk {
        return Err("the signing account key isn't the invite's fingerprint".into());
    }
    if sig.cert.account_id != founder {
        return Err("the signing device is certified for another account".into());
    }
    sig.cert.verify(&key).map_err(|_| "the signing device's certificate doesn't verify".to_string())?;
    if revoked(sig.cert.device_id) {
        return Err("the signing device was revoked".into());
    }
    let s = unb64(&sig.s).ok_or("the signature is malformed")?;
    ligus_core::crypto::verify(&sig.cert.device_public_key, &preimage(claims, sig.at), &s)
        .map_err(|_| "the invite signature doesn't verify".to_string())?;
    Ok(founder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ligus_core::Timestamp;

    struct Founder {
        account_id: Uuid,
        account: SigningKeypair,
        device: SigningKeypair,
        cert: DeviceCertificate,
    }

    fn founder() -> Founder {
        let account_id = Uuid::new_v4();
        let account = SigningKeypair::generate().unwrap();
        let device = SigningKeypair::generate().unwrap();
        let cert = DeviceCertificate::issue(&account, account_id, &device.public_key_bytes(), Timestamp::epoch());
        Founder { account_id, account, device, cert }
    }

    #[test]
    fn context_is_frozen() {
        assert_eq!(INVITE_SIG_CONTEXT, b"ligus-invite-v1\0");
    }

    #[test]
    fn founder_signature_verifies_and_binds_every_field() {
        let f = founder();
        let (founder_s, fk) = (f.account_id.to_string(), FounderPin::fingerprint_hex(&f.account.public_key_bytes()));
        let claims = InviteClaims {
            name: "Team",
            relay_url: "https://relay.example",
            room: "room1",
            key: Some("secret"),
            founder: Some(&founder_s),
            fk: Some(&fk),
            inv: Some("inv"),
            ..Default::default()
        };
        let sig = sign(&claims, 100, &f.device, &f.cert, &f.account.public_key_bytes());
        let none = |_| false;
        assert_eq!(verify(&sig, &claims, &none), Ok(f.account_id));
        // Any field changed: no longer the founder's.
        for changed in [
            InviteClaims { relay_url: "https://evil.example", ..claims },
            InviteClaims { room: "room2", ..claims },
            InviteClaims { key: Some("other"), ..claims },
            InviteClaims { name: "Other", ..claims },
            InviteClaims { inv: None, ..claims },
            InviteClaims { ek_json: b"[1]", ..claims },
        ] {
            assert!(verify(&sig, &changed, &none).is_err());
        }
        // A revoked device.
        assert!(verify(&sig, &claims, &|d| d == f.cert.device_id).is_err());
        // Another account's valid signature, relabelled as this founder's.
        let m = founder();
        let msig = sign(&claims, 100, &m.device, &m.cert, &m.account.public_key_bytes());
        assert!(verify(&msig, &claims, &none).is_err(), "fingerprint mismatch");
        let mut swapped = msig.clone();
        swapped.k = sig.k.clone();
        assert!(verify(&swapped, &claims, &none).is_err(), "the certificate is for another account");
    }
}

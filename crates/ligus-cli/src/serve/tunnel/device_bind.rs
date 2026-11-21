//! Binding a tunnel pairing to the phone's workspace device (Phase B).
//!
//! A phone has two identities on a box: its tunnel pairing (`pid`, Noise
//! static, auth key; `phones.json`) and its workspace device (a P-256 Secure
//! Enclave key certified on the config log). Revoking the device must cut the
//! pairing, so the box has to know which pairing is which device. The phone
//! proves it with its device key: an ECDSA P-256 signature over
//!
//! ```text
//! ctx("device-bind", host_id, phone_noise_static)
//! ```
//!
//! sent in the enrollment payload (`"device": {"id", "sig"}`, required). The
//! static is unique to the pairing, so the signature can only bind this
//! pairing on this box.
//!
//! An enrollment without a claim, or with a claim of a device this box
//! doesn't know, isn't a phone of this workspace, is revoked, or doesn't
//! verify, is refused; so is one from a Noise static or auth key that belonged
//! to a pairing of a revoked device.

use serde::Deserialize;
use uuid::Uuid;

use super::{b64u_decode, ctx};

/// What the box knows about a workspace device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceLookup {
    /// Not on this box's roster (not certified yet, or another workspace).
    Unknown,
    /// Revoked on the roster.
    Revoked,
    /// Certified and not revoked.
    Certified { public_key: Vec<u8>, phone: bool },
}

/// The device claim a phone sends.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceClaim {
    pub id: Uuid,
    /// b64u(64-byte P-256 signature, r ‖ s, low-S).
    pub sig: String,
}

/// The outcome of checking a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindCheck {
    /// Valid: bind the pairing to this device.
    Bind(Uuid),
    /// The device is revoked: refuse the pairing.
    Revoked(Uuid),
    /// The device isn't certified (yet): refuse; the phone pairs again once
    /// this box has synced its certificate.
    Unknown,
    /// A bad signature, or a device that isn't a phone.
    Invalid(&'static str),
}

/// The signed preimage.
pub fn bind_message(host_id: &str, static_pub: &[u8; 32]) -> Vec<u8> {
    ctx("device-bind", &[host_id.as_bytes(), static_pub])
}

/// Check `claim` for the pairing with Noise static `static_pub` on host `host_id`.
pub fn check(claim: &DeviceClaim, host_id: &str, static_pub: &[u8; 32], lookup: impl Fn(Uuid) -> DeviceLookup) -> BindCheck {
    match lookup(claim.id) {
        // A revoked device is refused whatever the signature: claiming one
        // can only lock the claimant out.
        DeviceLookup::Revoked => BindCheck::Revoked(claim.id),
        DeviceLookup::Unknown => BindCheck::Unknown,
        DeviceLookup::Certified { phone: false, .. } => BindCheck::Invalid("the device is not a phone"),
        DeviceLookup::Certified { public_key, phone: true } => {
            let Ok(sig) = b64u_decode(&claim.sig) else { return BindCheck::Invalid("malformed signature") };
            match ligus_core::crypto::verify_p256(&public_key, &bind_message(host_id, static_pub), &sig) {
                Ok(()) => BindCheck::Bind(claim.id),
                Err(_) => BindCheck::Invalid("the device signature does not verify"),
            }
        }
    }
}

/// E3b, P1 (§7.2): a claim for a device the box doesn't know yet, proved by
/// the key the phone sends along (`enrollKey`, b64u of the 65-byte SEC1
/// P-256 point): the key is a valid point, the claim's id is the one it
/// derives, and the claim's signature verifies under it over the same
/// `ctx("device-bind", host_id, phone_static)`. Returns the key.
pub fn check_provisional(claim: &DeviceClaim, enroll_key: &str, host_id: &str, static_pub: &[u8; 32]) -> Result<Vec<u8>, &'static str> {
    let key = b64u_decode(enroll_key).map_err(|_| "malformed enrollKey")?;
    if ligus_runtime::phone_enroll::check_device_id(claim.id, &key).is_err() {
        return Err("the device id isn't the one enrollKey derives");
    }
    let sig = b64u_decode(&claim.sig).map_err(|_| "malformed signature")?;
    ligus_core::crypto::verify_p256(&key, &bind_message(host_id, static_pub), &sig).map_err(|_| "the device signature does not verify under enrollKey")?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::tunnel::b64u;

    #[test]
    fn a_valid_claim_binds_and_everything_else_does_not() {
        let p = ligus_core::testkit::Phone::new();
        let (id, key) = (p.device_id, p.key);
        let pubkey = ligus_core::crypto::p256_public_key(&key);
        let st = [9u8; 32];
        let sig = ligus_core::crypto::sign_p256(&key, &bind_message("HOST", &st));
        let claim = DeviceClaim { id, sig: b64u(sig) };
        let certified = |_| DeviceLookup::Certified { public_key: pubkey.clone(), phone: true };
        assert_eq!(check(&claim, "HOST", &st, certified), BindCheck::Bind(id));
        // Another pairing (static) or another host: refused.
        assert!(matches!(check(&claim, "HOST", &[8u8; 32], certified), BindCheck::Invalid(_)));
        assert!(matches!(check(&claim, "OTHER", &st, certified), BindCheck::Invalid(_)));
        // A desktop device can't be a tunnel phone.
        assert!(matches!(check(&claim, "HOST", &st, |_| DeviceLookup::Certified { public_key: pubkey.clone(), phone: false }), BindCheck::Invalid(_)));
        assert_eq!(check(&claim, "HOST", &st, |_| DeviceLookup::Revoked), BindCheck::Revoked(id));
        assert_eq!(check(&claim, "HOST", &st, |_| DeviceLookup::Unknown), BindCheck::Unknown);
        let bad = DeviceClaim { id, sig: "!!".into() };
        assert!(matches!(check(&bad, "HOST", &st, certified), BindCheck::Invalid(_)));
    }

    /// E3b, P1: only a key that derives the claim's id and signed this
    /// pairing's binding passes.
    #[test]
    fn a_provisional_claim_needs_its_own_key() {
        let p = ligus_core::testkit::Phone::new();
        let st = [7u8; 32];
        let key = b64u(p.public_key());
        let claim = DeviceClaim { id: p.device_id, sig: b64u(ligus_core::crypto::sign_p256(&p.key, &bind_message("HOST", &st))) };
        assert_eq!(check_provisional(&claim, &key, "HOST", &st), Ok(p.public_key()));
        assert!(check_provisional(&claim, &key, "HOST", &[8u8; 32]).is_err(), "another pairing");
        assert!(check_provisional(&claim, &key, "OTHER", &st).is_err(), "another host");
        let other = ligus_core::testkit::Phone::new();
        assert!(check_provisional(&claim, &b64u(other.public_key()), "HOST", &st).is_err(), "a key that doesn't derive the id");
        let theirs = DeviceClaim { id: other.device_id, sig: claim.sig.clone() };
        assert!(check_provisional(&theirs, &b64u(other.public_key()), "HOST", &st).is_err(), "a signature by another key");
        assert!(check_provisional(&claim, "!!", "HOST", &st).is_err());
        assert!(check_provisional(&claim, &b64u([4u8; 65]), "HOST", &st).is_err(), "not a point");
    }

    #[test]
    fn preimage_layout() {
        let m = bind_message("H", &[1u8; 32]);
        let mut want = b"hive-tunnel/1 device-bind\0H".to_vec();
        want.extend_from_slice(&[1u8; 32]);
        assert_eq!(m, want);
    }
}

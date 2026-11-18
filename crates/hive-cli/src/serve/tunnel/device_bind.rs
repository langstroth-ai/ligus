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
//! sent in the enrollment payload (`"device": {"id", "sig"}`, optional) or
//! later over the tunnel (`PUT /v1/tunnel/device`). The static is unique to
//! the pairing, so the signature can only bind this pairing on this box.
//!
//! A pairing that claims a revoked device is refused (enrollment) or revoked
//! (later claim); so is a new enrollment from a Noise static or auth key that
//! belonged to a pairing of a revoked device.

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
    /// The device isn't certified (yet): leave the pairing unbound.
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
            match hive_core::crypto::verify_p256(&public_key, &bind_message(host_id, static_pub), &sig) {
                Ok(()) => BindCheck::Bind(claim.id),
                Err(_) => BindCheck::Invalid("the device signature does not verify"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::tunnel::b64u;

    #[test]
    fn a_valid_claim_binds_and_everything_else_does_not() {
        let p = hive_core::testkit::Phone::new();
        let (id, key) = (p.device_id, p.key);
        let pubkey = hive_core::crypto::p256_public_key(&key);
        let st = [9u8; 32];
        let sig = hive_core::crypto::sign_p256(&key, &bind_message("HOST", &st));
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

    #[test]
    fn preimage_layout() {
        let m = bind_message("H", &[1u8; 32]);
        let mut want = b"hive-tunnel/1 device-bind\0H".to_vec();
        want.extend_from_slice(&[1u8; 32]);
        assert_eq!(m, want);
    }
}

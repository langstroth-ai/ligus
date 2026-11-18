//! Box admissions on the wire between people (Phase B): the join request a
//! box shows, and the admission code an Admin hands back.
//!
//! * **Join request** (`ligusjoin1:…`): what a `ligus serve` or `ligus
//!   worker` box prints when it has no admission yet: its account id, its
//!   device's public key and key-agreement key, and a label. Nothing secret.
//!   An Admin compares its fingerprint (over the account, the device and both
//!   keys) with the one the box printed, so a swapped request is caught
//!   before anything is issued.
//! * **Admission code**: the workspace invite (`hivews1:…`, relay, room,
//!   workspace key, the invite id) of a box admission. Issued for a join
//!   request it is **sealed to the box's key-agreement key**
//!   (`ligusadmit1:…`), so only that box opens it and the admission itself is
//!   bound to the box's account. Issued for Kubernetes or another headless
//!   box (no request) it is a plain bearer `hivews1:` code: keep it in a
//!   Secret, single use, and short-lived.
//!
//! The event-log rules are in `hive_core::admission`.

use base64::Engine;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The join request prefix.
pub const JOIN_PREFIX: &str = "ligusjoin1:";
/// The sealed admission code prefix.
pub const SEALED_PREFIX: &str = "ligusadmit1:";
/// The longest code read (a pasted or mounted one).
pub const MAX_CODE_LEN: usize = 64 * 1024;
/// Default lifetime of an admission for a box that asked (minutes count).
pub const DEFAULT_TTL_MS: i64 = 60 * 60 * 1000;
/// Default lifetime of a bearer admission (a Kubernetes Secret).
pub const DEFAULT_BEARER_TTL_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinRequest {
    pub v: u32,
    pub account_id: Uuid,
    pub device_id: Uuid,
    /// The device's Ed25519 public key (32 bytes).
    #[serde(with = "b64")]
    pub device_public_key: Vec<u8>,
    /// The device's X25519 key-agreement key (32 bytes): the code is sealed to it.
    #[serde(with = "b64")]
    pub ka_public: Vec<u8>,
    #[serde(default)]
    pub label: String,
}

impl JoinRequest {
    pub fn new(account_id: Uuid, device_id: Uuid, device_public_key: &[u8], ka_public: &[u8], label: &str) -> Self {
        Self {
            v: 1,
            account_id,
            device_id,
            device_public_key: device_public_key.to_vec(),
            ka_public: ka_public.to_vec(),
            label: label.chars().filter(|c| !c.is_control()).take(64).collect(),
        }
    }

    /// The fingerprint both sides show (`XXXX-XXXX-XXXX-XXXX-XXXX`, 80
    /// bits): over everything the admission trusts from the request, the
    /// account it binds, the device, its signing key and the key-agreement
    /// key the code is sealed to (the label is only a hint). A request
    /// swapped in transit shows another fingerprint, and the account id (free
    /// to grind) is too long to match by search.
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"ligus-box-join-v1\0");
        h.update(self.account_id.as_bytes());
        h.update(self.device_id.as_bytes());
        h.update((self.device_public_key.len() as u32).to_le_bytes());
        h.update(&self.device_public_key);
        h.update((self.ka_public.len() as u32).to_le_bytes());
        h.update(&self.ka_public);
        let d = h.finalize();
        let hex: String = d[..10].iter().map(|b| format!("{b:02X}")).collect();
        hex.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap_or_default()).collect::<Vec<_>>().join("-")
    }

    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("serialize join request");
        format!("{JOIN_PREFIX}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json))
    }

    pub fn decode(code: &str) -> Result<Self, String> {
        let code = code.trim();
        if code.len() > MAX_CODE_LEN {
            return Err("that join request is too long".into());
        }
        let body = code.strip_prefix(JOIN_PREFIX).ok_or("not a box join request (expected ligusjoin1:…)")?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(body.trim().as_bytes())
            .map_err(|_| "the join request isn't valid base64url")?;
        let r: Self = serde_json::from_slice(&bytes).map_err(|_| "the join request is malformed")?;
        if r.v != 1 {
            return Err(format!("join request version {} isn't supported; update Ligus", r.v));
        }
        if r.device_public_key.len() != 32 || r.ka_public.len() != 32 {
            return Err("the join request's keys are malformed".into());
        }
        Ok(r)
    }

    /// Seal `invite` (a `hivews1:` code) to this box: `ligusadmit1:…`.
    pub fn seal(&self, invite: &str) -> Result<String, String> {
        let blob = hive_core::e2ee::seal(&self.ka_public, invite.as_bytes()).map_err(|e| e.to_string())?;
        let json = serde_json::to_vec(&blob).map_err(|e| e.to_string())?;
        Ok(format!("{SEALED_PREFIX}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)))
    }
}

/// The `hivews1:` invite inside an admission code: opened with this box's
/// key-agreement key when sealed, as is when it is a bearer code.
pub fn open_admission(code: &str, ka: &hive_core::KeyAgreementKeypair) -> Result<String, String> {
    let code = code.trim();
    if code.len() > MAX_CODE_LEN {
        return Err("that admission code is too long".into());
    }
    if code.starts_with("hivews1:") {
        return Ok(code.to_string());
    }
    let body = code
        .strip_prefix(SEALED_PREFIX)
        .ok_or("not an admission code (expected ligusadmit1:… or hivews1:…)")?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(body.trim().as_bytes())
        .map_err(|_| "the admission code isn't valid base64url")?;
    let blob: hive_core::SealedBlob = serde_json::from_slice(&bytes).map_err(|_| "the admission code is malformed")?;
    let plain = zeroize::Zeroizing::new(
        hive_core::e2ee::open(ka, &blob).map_err(|_| "this admission code was sealed to another box".to_string())?,
    );
    let s = std::str::from_utf8(&plain).map_err(|_| "the admission code is malformed")?;
    if !s.starts_with("hivews1:") {
        return Err("the admission code is malformed".into());
    }
    Ok(s.to_string())
}

mod b64 {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s.as_bytes()).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(ka: &hive_core::KeyAgreementKeypair) -> JoinRequest {
        JoinRequest::new(Uuid::new_v4(), Uuid::new_v4(), &[3; 32], &ka.public_key_bytes(), "build-1\n")
    }

    #[test]
    fn a_join_request_round_trips_and_shows_its_fingerprint() {
        let ka = hive_core::KeyAgreementKeypair::generate().unwrap();
        let r = request(&ka);
        assert_eq!(r.label, "build-1", "control characters are dropped");
        let back = JoinRequest::decode(&r.encode()).unwrap();
        assert_eq!(back, r);
        assert_eq!(back.fingerprint(), r.fingerprint());
        assert_eq!(r.fingerprint().len(), 24, "five groups of four");
        assert!(JoinRequest::decode("hivews1:abc").is_err());
        assert!(JoinRequest::decode(&format!("{JOIN_PREFIX}{}", "A".repeat(MAX_CODE_LEN))).is_err());
    }

    #[test]
    fn the_fingerprint_covers_every_key_the_admission_trusts() {
        // Review R2: the code is sealed to `ka_public` and bound to
        // `account_id`, so a request swapped in transit (the box's device key
        // kept, its key-agreement key or account replaced) must not show the
        // fingerprint the box printed.
        let ka = hive_core::KeyAgreementKeypair::generate().unwrap();
        let mitm = hive_core::KeyAgreementKeypair::generate().unwrap();
        let r = request(&ka);
        let mut swapped_ka = r.clone();
        swapped_ka.ka_public = mitm.public_key_bytes().to_vec();
        assert_ne!(swapped_ka.fingerprint(), r.fingerprint(), "another key-agreement key");
        let mut swapped_account = r.clone();
        swapped_account.account_id = Uuid::new_v4();
        assert_ne!(swapped_account.fingerprint(), r.fingerprint(), "another account");
        let mut swapped_device = r.clone();
        swapped_device.device_id = Uuid::new_v4();
        assert_ne!(swapped_device.fingerprint(), r.fingerprint(), "another device id");
        let mut relabelled = r.clone();
        relabelled.label = "other".into();
        assert_eq!(relabelled.fingerprint(), r.fingerprint(), "the label is only a hint");
    }

    #[test]
    fn a_sealed_admission_opens_only_on_the_box_that_asked() {
        let ka = hive_core::KeyAgreementKeypair::generate().unwrap();
        let other = hive_core::KeyAgreementKeypair::generate().unwrap();
        let sealed = request(&ka).seal("hivews1:secret").unwrap();
        assert!(sealed.starts_with(SEALED_PREFIX));
        assert!(!sealed.contains("secret"));
        assert_eq!(open_admission(&sealed, &ka).unwrap(), "hivews1:secret");
        assert!(open_admission(&sealed, &other).is_err());
        // A bearer code passes through; anything else is refused.
        assert_eq!(open_admission(" hivews1:x\n", &other).unwrap(), "hivews1:x");
        assert!(open_admission("hivedev1:x", &ka).is_err());
    }
}

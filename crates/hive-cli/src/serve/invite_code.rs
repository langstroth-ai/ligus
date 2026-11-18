//! `hivews1:` invite codes on the box: the same JSON the desktop's
//! `WorkspaceConn` encodes (snake_case fields, empty ones left out), so a
//! code made here joins on a desktop and a desktop's code is read here.
//!
//! The founder's device signature (`sig`, `hive_runtime::invite_sig`,
//! `ligus-invite-v1\0`) is checked exactly as the desktop checks it: the
//! signed name is the name a reader decodes (the room when the code names
//! none), and the epoch keys are covered as their canonical JSON.

use base64::Engine;
use hive_runtime::directory_trust::CoMember;
use hive_runtime::dm::{self, FounderTrust};
use hive_runtime::invite_sig::{InviteClaims, InviteSignature};
use hive_runtime::keyring::DeliveredKey;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One workspace or DM connection as an invite carries it. Field names and
/// skip rules are the desktop's (`app/src/lib.rs` `WorkspaceConn`); a field
/// the desktop doesn't know would be dropped there, so there is none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteConn {
    #[serde(default)]
    pub name: String,
    pub relay_url: String,
    pub room: String,
    #[serde(default)]
    pub key: Option<String>,
    /// Never in an invite (the desktop strips it); kept for the shape.
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub dm_account: Option<String>,
    #[serde(default)]
    pub founder_actor_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inv: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ek: Vec<DeliveredKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig: Option<InviteSignature>,
}

/// The longest invite code read (a pasted or dropped code; a real one is a
/// few KiB even with a dozen epoch keys).
pub const MAX_CODE_LEN: usize = 64 * 1024;

impl InviteConn {
    pub fn workspace_id(&self) -> Uuid {
        hive_core::room_workspace_id(&self.room)
    }

    /// The name a reader shows (and a signature covers): the code's name, or
    /// its room when it names none.
    pub fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            self.room.clone()
        } else {
            self.name.clone()
        }
    }

    pub fn is_dm(&self) -> bool {
        self.dm_account.is_some()
    }

    fn ek_json(&self) -> Vec<u8> {
        if self.ek.is_empty() {
            Vec::new()
        } else {
            serde_json::to_vec(&self.ek).unwrap_or_default()
        }
    }

    /// What a signature covers (`name` is [`Self::display_name`]).
    pub fn with_claims<T>(&self, f: impl FnOnce(&InviteClaims<'_>) -> T) -> T {
        let (name, ek) = (self.display_name(), self.ek_json());
        let claims = InviteClaims {
            name: &name,
            relay_url: &self.relay_url,
            room: &self.room,
            key: self.key.as_deref(),
            dm_account: self.dm_account.as_deref(),
            founder: self.founder_actor_id.as_deref(),
            fk: self.fk.as_deref(),
            inv: self.inv.as_deref(),
            ek_json: &ek,
        };
        f(&claims)
    }

    /// The founder account the signature proves, if it verifies and the
    /// signing device isn't one `revoked` names.
    pub fn signed_by(&self, revoked: &dyn Fn(Uuid) -> bool) -> Option<Uuid> {
        let sig = self.sig.as_ref()?;
        self.with_claims(|c| hive_runtime::invite_sig::verify(sig, c, revoked).ok())
    }

    /// Who the invite is from, as far as this box can tell (R1): a
    /// co-member's name and safety code count as theirs only with their
    /// device's valid signature.
    pub fn trust(&self, people: &[CoMember]) -> FounderTrust {
        let known = dm::known_fingerprints(people);
        let signer = self.signed_by(&|d| people.iter().any(|p| p.revoked_devices.contains(&d)));
        dm::check_founder(self.founder_actor_id.as_deref(), self.fk.as_deref(), &known, signer)
    }

    /// The code: `hivews1:` + base64url(JSON), icon left out.
    pub fn encode(&self) -> String {
        let conn = InviteConn { icon: None, ..self.clone() };
        let json = serde_json::to_vec(&conn).unwrap_or_default();
        format!("hivews1:{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json))
    }

    pub fn decode(code: &str) -> Result<Self, String> {
        let code = code.trim();
        if code.len() > MAX_CODE_LEN {
            return Err("that invite is too long".into());
        }
        let body = code
            .strip_prefix("hivews1:")
            .ok_or_else(|| "not a Ligus workspace invite (expected hivews1:…)".to_string())?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(body.trim().as_bytes())
            .map_err(|_| "the invite isn't encoded correctly".to_string())?;
        let mut conn: InviteConn = serde_json::from_slice(&bytes).map_err(|_| "the invite isn't readable".to_string())?;
        if conn.room.trim().is_empty() {
            return Err("the invite is missing a room".into());
        }
        conn.name = conn.display_name();
        Ok(conn)
    }
}

/// The wire name of a trust label (as the desktop's `trust` field).
pub fn trust_str(t: FounderTrust) -> &'static str {
    match t {
        FounderTrust::KnownMatch => "known",
        FounderTrust::Claimed => "claimed",
        FounderTrust::Mismatch => "mismatch",
        FounderTrust::Unknown => "unknown",
        FounderTrust::NoFingerprint => "none",
    }
}

/// A fresh random hex secret of `len` characters (a DM's E2EE passphrase).
pub fn random_hex(len: usize) -> Result<String, getrandom::Error> {
    let mut raw = vec![0u8; len.div_ceil(2)];
    getrandom::fill(&mut raw)?;
    let mut s: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    s.truncate(len);
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::crypto::{DeviceCertificate, SigningKeypair};

    fn sample() -> InviteConn {
        InviteConn {
            name: "Team Rocket".into(),
            relay_url: "wss://relay.example/v1".into(),
            room: "team-rocket-9f3a2b".into(),
            key: Some("hunter2".into()),
            icon: None,
            dm_account: None,
            founder_actor_id: Some("6f1c2d3e-0000-4000-8000-000000000042".into()),
            fk: Some("00112233445566778899aabbccddeeff".into()),
            inv: None,
            ek: Vec::new(),
            sig: None,
        }
    }

    /// The desktop's `invite_round_trips` connection encodes to exactly this
    /// JSON (field order is the struct's, empty `fk`/`inv`/`ek`/`sig` left
    /// out, `icon` and `dm_account` written as null): a box and a desktop
    /// read each other's codes.
    #[test]
    fn encoding_matches_the_desktop_shape() {
        let code = sample().encode();
        let body = code.strip_prefix("hivews1:").unwrap();
        let json = String::from_utf8(base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(body).unwrap()).unwrap();
        assert_eq!(
            json,
            r#"{"name":"Team Rocket","relay_url":"wss://relay.example/v1","room":"team-rocket-9f3a2b","key":"hunter2","icon":null,"dm_account":null,"founder_actor_id":"6f1c2d3e-0000-4000-8000-000000000042","fk":"00112233445566778899aabbccddeeff"}"#
        );
        assert_eq!(InviteConn::decode(&code).unwrap(), sample());
    }

    #[test]
    fn decode_refuses_junk_and_oversize() {
        assert!(InviteConn::decode("hivedev1:abc").is_err());
        assert!(InviteConn::decode("hivews1:!!!").is_err());
        let no_room = InviteConn { room: " ".into(), ..sample() }.encode();
        assert!(InviteConn::decode(&no_room).is_err());
        assert!(InviteConn::decode(&format!("hivews1:{}", "A".repeat(MAX_CODE_LEN))).is_err());
    }

    /// R1 on the box: a signature over the decoded name (the room when the
    /// code names none) verifies after a round trip and binds the relay,
    /// room, key and DM recipient.
    #[test]
    fn signature_survives_encoding_and_binds_fields() {
        let account = SigningKeypair::generate().unwrap();
        let device = SigningKeypair::generate().unwrap();
        let founder = Uuid::new_v4();
        let cert = DeviceCertificate::issue(&account, founder, Uuid::new_v4(), &device.public_key_bytes(), hive_core::Timestamp::epoch());
        let fk = hive_runtime::envelope_verifier::FounderPin::fingerprint_hex(&account.public_key_bytes());
        for name in ["", "Team"] {
            let mut conn = InviteConn {
                name: name.into(),
                dm_account: Some(Uuid::new_v4().to_string()),
                founder_actor_id: Some(founder.to_string()),
                fk: Some(fk.clone()),
                inv: Some(Uuid::new_v4().to_string()),
                ..sample()
            };
            conn.sig = Some(conn.with_claims(|c| hive_runtime::invite_sig::sign(c, 1, &device, &cert, &account.public_key_bytes())));
            let back = InviteConn::decode(&conn.encode()).unwrap();
            assert_eq!(back.signed_by(&|_| false), Some(founder));
            assert_eq!(back.signed_by(&|d| d == cert.device_id), None, "a revoked device's signature counts for nothing");
            for changed in [
                InviteConn { relay_url: "https://evil.example".into(), ..back.clone() },
                InviteConn { room: "other".into(), ..back.clone() },
                InviteConn { key: Some("other".into()), ..back.clone() },
                InviteConn { dm_account: Some(Uuid::new_v4().to_string()), ..back.clone() },
                InviteConn { sig: None, ..back.clone() },
            ] {
                assert_eq!(changed.signed_by(&|_| false), None);
            }
        }
    }

    #[test]
    fn random_hex_is_random_and_sized() {
        let (a, b) = (random_hex(24).unwrap(), random_hex(24).unwrap());
        assert_eq!(a.len(), 24);
        assert_ne!(a, b);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
    }
}

/// The `hivews1:` code of invite `inv` to `cfg`'s workspace, as `svc` (an
/// Admin) issued it: relay, room, workspace key, every authorized epoch key
/// `svc` holds, the founder pin, and the founder's signature when `svc` is
/// the founder's device. A bearer secret: hand it only to the box it admits.
pub fn admission_code(cfg: &crate::Config, svc: &hive_runtime::ChatService, inv: Uuid) -> anyhow::Result<String> {
    let relay = cfg
        .relay_url
        .clone()
        .filter(|r| crate::usable_relay_url(r))
        .ok_or_else(|| anyhow::anyhow!("this workspace has no relay URL, so no box could join it (set LIGUS_RELAY_URL)"))?;
    let ws = crate::uuid_of_room(&cfg.room);
    let pin = svc.store().workspace_pin(ws)?;
    let mut conn = InviteConn {
        name: String::new(),
        relay_url: relay,
        room: cfg.room.clone(),
        key: cfg.key.clone(),
        icon: None,
        dm_account: None,
        founder_actor_id: pin.as_ref().map(|p| p.founder_account.to_string()),
        fk: pin.as_ref().map(|p| p.fk.iter().map(|b| format!("{b:02x}")).collect()),
        inv: Some(inv.to_string()),
        ek: Vec::new(),
        sig: None,
    };
    if let Ok(src) = crate::key_source(cfg) {
        conn.ek = svc.delivered_epoch_keys(ws, &src).unwrap_or_default();
    }
    let identity = hive_runtime::IdentityStore::new(&cfg.data_dir, hive_runtime::secret_store::StoreKeyVault::for_data_dir(&cfg.data_dir));
    if let Some(stored) = identity.load()? {
        conn.sig = conn.with_claims(|c| svc.sign_invite(c, &stored.device.certificate));
    }
    Ok(conn.encode())
}

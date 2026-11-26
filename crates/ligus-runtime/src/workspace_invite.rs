//! The `hivews1:` workspace invite and the connection it carries.
//!
//! A joined or created team workspace is a relay room plus the E2EE key to
//! read it ([`WorkspaceConn`]). An invite is that connection, encoded as
//! `hivews1:<base64url(json)>`, with the single-use invite an Admin issued,
//! the epoch keys the inviter held and the founder's signature. The desktop
//! app, the `ligus` CLI and other clients (the terminal app) share this one
//! codec, so an invite any of them prints is one every other reads.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::invite_sig::{InviteClaims, InviteSignature};
use crate::keyring::DeliveredKey;

/// The invite prefix (format version 1).
pub const INVITE_PREFIX: &str = "hivews1:";

/// The typed error a client returns for a workspace with no usable relay;
/// the web UI matches this prefix and shows its own message with a link to
/// Settings → Team sync.
pub const NO_RELAY_ERR: &str = "workspace-no-relay";

/// A joined/created team workspace: a relay room plus the (optional) E2EE key
/// to read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceConn {
    /// Friendly display name (defaults to the room name).
    #[serde(default)]
    pub name: String,
    /// The relay. An invite without one decodes to an empty relay, which
    /// every client then refuses with its own no-relay error
    /// ([`invite_relay_problem`]), as the CLI's decoder always did.
    #[serde(default)]
    pub relay_url: String,
    pub room: String,
    /// E2EE passphrase (derives the workspace key); None = plaintext room.
    #[serde(default)]
    pub key: Option<String>,
    /// Optional workspace icon as a `data:` URL, set by an owner/admin.
    #[serde(default)]
    pub icon: Option<String>,
    /// When set, this workspace is a 1:1 direct message. In an invite it names
    /// the account id the DM is addressed to; in a device's settings, the
    /// other person's account id. Account ids are random UUIDs, never derived
    /// from a public identifier (see [`crate::dm`]).
    #[serde(default)]
    pub dm_account: Option<String>,
    /// The account id that legitimately founded this workspace (for a DM, the
    /// person who opened it): the out-of-band trust anchor for projection's
    /// base-seed selection (B2). The founder embeds their own id when
    /// generating an invite; a joiner records it from the invite and pins it
    /// on the `ChatService` when the workspace goes active, so a forged
    /// low-lamport `SessionSnapshot` from another keyholder can't displace
    /// the founder's seed. `None` on legacy invites: the in-band
    /// self-consistency fallback.
    #[serde(default)]
    pub founder_actor_id: Option<String>,
    /// The founder's key fingerprint: hex of the first 16 bytes of SHA-256 of
    /// the founder's account signing key (Phase 0, P0.4). A joiner pins it with
    /// `founder_actor_id`, so a registration of the founder's account under
    /// any other key is rejected by the trust roster. Left out when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fk: Option<String>,
    /// The single-use invite an Admin issued with this code (a `JoinApproved`
    /// of `invite:<id>` on the config log). A joiner redeems it to enroll
    /// itself; key possession alone never admits anyone. Left out when absent
    /// (and never stored in the founder's own settings).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inv: Option<String>,
    /// Every authorized epoch key the inviter held (Phase 0, B1): a newcomer
    /// reads the history and seals at the head before any rotation names its
    /// device. A bearer secret, like `key`. Left out when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ek: Vec<DeliveredKey>,
    /// The founder's device signature over the invite's fields
    /// ([`crate::invite_sig`], R1). Only an invite whose signature verifies
    /// is shown as coming from its founder; anything else only says it is.
    /// Invite-only: never kept in settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig: Option<InviteSignature>,
}

impl WorkspaceConn {
    /// The workspace id: UUID v5 over the room ([`ligus_core::room_workspace_id`]).
    pub fn id(&self) -> Uuid {
        ligus_core::room_workspace_id(&self.room)
    }

    /// The canonical bytes of the epoch keys a signature covers.
    pub fn ek_json(&self) -> Vec<u8> {
        if self.ek.is_empty() {
            Vec::new()
        } else {
            serde_json::to_vec(&self.ek).unwrap_or_default()
        }
    }

    /// What an invite signature covers ([`crate::invite_sig`]). `name` is
    /// [`Self::display_name`], what a reader decodes the name to.
    pub fn sig_claims<'a>(&'a self, name: &'a str, ek_json: &'a [u8]) -> InviteClaims<'a> {
        InviteClaims {
            name,
            relay_url: &self.relay_url,
            room: &self.room,
            key: self.key.as_deref(),
            dm_account: self.dm_account.as_deref(),
            founder: self.founder_actor_id.as_deref(),
            fk: self.fk.as_deref(),
            inv: self.inv.as_deref(),
            ek_json,
        }
    }

    /// The founder account the invite's signature proves, if it verifies
    /// (`revoked`: devices the reader's logs revoked).
    pub fn signed_by(&self, revoked: &dyn Fn(Uuid) -> bool) -> Option<Uuid> {
        let sig = self.sig.as_ref()?;
        let (name, ek) = (self.display_name(), self.ek_json());
        crate::invite_sig::verify(sig, &self.sig_claims(&name, &ek), revoked).ok()
    }

    /// The name, or the room when it has none.
    pub fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            self.room.clone()
        } else {
            self.name.clone()
        }
    }

    /// The single-use invite id, when it parses.
    pub fn invite_id(&self) -> Option<Uuid> {
        self.inv
            .as_deref()
            .and_then(|i| Uuid::parse_str(i.trim()).ok())
    }

    /// The founder pin `(founder account id, key fingerprint)`, when the
    /// invite carries both.
    pub fn founder_pin(&self) -> Option<(String, String)> {
        self.founder_actor_id.clone().zip(self.fk.clone())
    }
}

/// Shareable invite encoding for a workspace: `hivews1:<base64url(json)>`. It
/// bundles everything a peer needs to join (relay, room, and the E2EE key),
/// so joining is a single paste. The icon is never embedded.
pub fn encode_workspace_invite(conn: &WorkspaceConn) -> String {
    use base64::Engine;
    // Icons are local rail decoration; never embed a (potentially large) data
    // URL in an invite code.
    let conn = WorkspaceConn {
        icon: None,
        ..conn.clone()
    };
    let json = serde_json::to_vec(&conn).unwrap_or_default();
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json);
    format!("{INVITE_PREFIX}{b64}")
}

/// Decode a `hivews1:` invite. Whitespace around it is ignored; an empty
/// name decodes to the room.
pub fn decode_workspace_invite(invite: &str) -> Result<WorkspaceConn, String> {
    use base64::Engine;
    let body = invite
        .trim()
        .strip_prefix(INVITE_PREFIX)
        .ok_or_else(|| "not a Ligus workspace invite (expected hivews1:…)".to_string())?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(body.trim().as_bytes())
        .map_err(|e| format!("invalid invite encoding: {e}"))?;
    let mut conn: WorkspaceConn =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid invite payload: {e}"))?;
    if conn.room.trim().is_empty() {
        return Err("invite is missing a room".to_string());
    }
    conn.name = conn.display_name();
    Ok(conn)
}

/// A relay URL a client can sync with: an http(s) address with a host.
pub fn usable_relay_url(raw: &str) -> bool {
    url::Url::parse(raw.trim()).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https") && u.host_str().is_some_and(|h| !h.is_empty())
    })
}

/// Why an invite for a workspace at `relay_url` couldn't be used, if it
/// couldn't. Joining needs the relay: a box (`ligus enroll`) refuses an
/// invite without one, and a desktop that joined one would never sync. So no
/// invite is issued, and none is joined, without an http(s) relay. The error
/// starts with [`NO_RELAY_ERR`].
pub fn invite_relay_problem(relay_url: &str) -> Option<String> {
    (!usable_relay_url(relay_url)).then(|| {
        format!(
            "{NO_RELAY_ERR}: This workspace has no relay, so an invite to it can't be used. \
             Set one under Settings → Team sync (Relay URL), then copy a new invite."
        )
    })
}

/// A random hex token (CSPRNG via uuid v4) of `len` characters: an opaque
/// room id (32) or a workspace passphrase (24), as workspace creation mints
/// them.
pub fn random_token(len: usize) -> String {
    let mut s = String::new();
    while s.len() < len {
        s.push_str(&Uuid::new_v4().simple().to_string());
    }
    s.truncate(len);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(name: &str, room: &str) -> WorkspaceConn {
        WorkspaceConn {
            name: name.into(),
            relay_url: "https://relay.example".into(),
            room: room.into(),
            key: Some("k".into()),
            icon: None,
            dm_account: None,
            founder_actor_id: None,
            fk: None,
            inv: None,
            ek: Vec::new(),
            sig: None,
        }
    }

    #[test]
    fn round_trips_and_drops_the_icon() {
        let mut c = conn("Team", "r00m");
        c.icon = Some("data:image/png;base64,AAAA".into());
        c.inv = Some(Uuid::new_v4().to_string());
        c.founder_actor_id = Some(Uuid::new_v4().to_string());
        c.fk = Some("00112233445566778899aabbccddeeff".into());
        let code = encode_workspace_invite(&c);
        assert!(code.starts_with(INVITE_PREFIX));
        let back = decode_workspace_invite(&format!("  {code}\n")).unwrap();
        assert_eq!(
            back,
            WorkspaceConn {
                icon: None,
                ..c.clone()
            }
        );
        assert_eq!(back.invite_id().map(|u| u.to_string()), c.inv);
        assert_eq!(
            back.founder_pin(),
            c.founder_actor_id.clone().zip(c.fk.clone())
        );
        assert_eq!(back.id(), ligus_core::room_workspace_id("r00m"));
    }

    #[test]
    fn an_unnamed_invite_decodes_to_its_room() {
        let back =
            decode_workspace_invite(&encode_workspace_invite(&conn("", "plain-room"))).unwrap();
        assert_eq!(back.name, "plain-room");
    }

    #[test]
    fn bad_invites_are_rejected() {
        assert!(decode_workspace_invite("nope").is_err());
        assert!(decode_workspace_invite("hivews1:!!!notbase64").is_err());
        let no_room = encode_workspace_invite(&conn("x", " "));
        assert_eq!(
            decode_workspace_invite(&no_room).unwrap_err(),
            "invite is missing a room"
        );
    }

    #[test]
    fn the_signature_binds_the_fields() {
        use ligus_core::crypto::{DeviceCertificate, SigningKeypair};
        let account = SigningKeypair::generate().unwrap();
        let device = SigningKeypair::generate().unwrap();
        let founder = Uuid::new_v4();
        let cert = DeviceCertificate::issue(
            &account,
            founder,
            &device.public_key_bytes(),
            ligus_core::Timestamp::epoch(),
        );
        let mut c = conn("", "r00m");
        c.founder_actor_id = Some(founder.to_string());
        c.fk = Some(crate::envelope_verifier::FounderPin::fingerprint_hex(
            &account.public_key_bytes(),
        ));
        let (n, ek) = (c.display_name(), c.ek_json());
        c.sig = Some(crate::invite_sig::sign(
            &c.sig_claims(&n, &ek),
            1,
            &device,
            &cert,
            &account.public_key_bytes(),
        ));
        let back = decode_workspace_invite(&encode_workspace_invite(&c)).unwrap();
        assert_eq!(back.signed_by(&|_| false), Some(founder));
        assert_eq!(
            WorkspaceConn {
                relay_url: "https://evil.example".into(),
                ..back.clone()
            }
            .signed_by(&|_| false),
            None
        );
        assert_eq!(
            WorkspaceConn { sig: None, ..back }.signed_by(&|_| false),
            None
        );
    }

    /// An invite with no relay field decodes (to an empty relay) so the
    /// client can say why it can't be used; one with no room doesn't.
    #[test]
    fn a_relayless_invite_decodes_to_an_empty_relay() {
        use base64::Engine;
        let code = |json: &str| {
            format!(
                "{INVITE_PREFIX}{}",
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
            )
        };
        let c = decode_workspace_invite(&code(r#"{"room":"r","key":"k"}"#)).unwrap();
        assert_eq!((c.relay_url.as_str(), c.room.as_str()), ("", "r"));
        assert!(invite_relay_problem(&c.relay_url)
            .unwrap()
            .starts_with(NO_RELAY_ERR));
        assert!(
            decode_workspace_invite(&code(r#"{"relay_url":"https://relay.example"}"#)).is_err()
        );
    }

    #[test]
    fn relayless_workspaces_get_the_typed_error() {
        for bad in ["", "  ", "relay.example", "ftp://relay.example"] {
            let e = invite_relay_problem(bad).unwrap_or_else(|| panic!("{bad:?} accepted"));
            assert!(e.starts_with(NO_RELAY_ERR), "{e}");
        }
        for good in [
            "https://relay.example",
            "http://127.0.0.1:8443",
            " https://relay.example/ ",
        ] {
            assert_eq!(invite_relay_problem(good), None, "{good:?}");
            assert!(usable_relay_url(good));
        }
    }

    #[test]
    fn random_tokens_are_hex_of_the_asked_length() {
        let (a, b) = (random_token(32), random_token(32));
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(random_token(24).len(), 24);
    }
}

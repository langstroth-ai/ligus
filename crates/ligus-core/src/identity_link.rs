//! Linked identities (IDENTITY-AND-FORGES §2.2-2.4): the optional claim
//! "this Ligus account is also `<subject>` on `<provider>`", published per
//! workspace on its config log.
//!
//! This module is the wire shape and the offline, network-free checks the fold
//! runs. A link grants nothing: no membership, no role, no trust for sealing
//! keys. Whether a link currently verifies against its forge is a device-local
//! view result (`ligus_runtime::identity_links`), never fold state.
//!
//! The one new string is the SSHSIG namespace [`LINK_NAMESPACE`] (owner
//! decision D1; PHASE0 §0.1). SSHSIG carries it as a length-prefixed field
//! given on the `ssh-keygen -Y sign -n` command line, so it has no `\0`
//! terminator.

use base64::Engine;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::time_util::Timestamp;

/// The SSHSIG namespace of an identity-link signature. Frozen (pinned by
/// `link_namespace_is_frozen`): changing it invalidates every link ever made.
pub const LINK_NAMESPACE: &str = "ligus-identity-link-v1@ligus.ai";

/// The first line of a link statement.
pub const STATEMENT_TAG: &str = "ligus-identity-link-v1";

/// A link event's serialized size cap (§2.2).
pub const MAX_LINK_BYTES: usize = 8 * 1024;

/// Live links per account per workspace (§2.2).
pub const MAX_LIVE_LINKS: usize = 16;

/// The longest a link may claim to last: two years (731 days, a leap day
/// included).
pub const MAX_LIFETIME_DAYS: i64 = 731;

/// The default lifetime of a new link: one year.
pub const DEFAULT_LIFETIME_DAYS: i64 = 365;

/// How a link is proven.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LinkMethod {
    /// An SSHSIG by a key the forge lists for the subject, over the statement.
    SshSig,
    /// The Ligus account key itself is on the forge's *signing* key list
    /// (GitHub, GitLab only; owner decision D15: a fallback, with a
    /// disclosure that it links the accounts publicly).
    KeyPublished,
    /// [2.x] Bitbucket, Azure DevOps: no public key list. Never "verified".
    SelfAsserted,
    /// A method this build doesn't know (a newer client's, or the reserved
    /// `atprotoRecord` and `oidcAttestation`, never emitted): folded as a
    /// claim, shown as "not checked".
    #[serde(other)]
    Unknown,
}

/// One linked identity (`identityLinkPublished`). Fields new after 0.1 must
/// skip when empty.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityLink {
    /// Random v4.
    pub link_id: Uuid,
    /// Must equal the signer's certified account.
    pub account_id: Uuid,
    /// "github" | "gitlab" | "forgejo" | "gitea" | ... (any lowercase name
    /// folds; links made by a 0.1 pre-release may say "sourcehut" or "tangled").
    pub provider: String,
    /// "github.com", "gitlab.example.com", "codeberg.org", ...
    pub host: String,
    /// The stable id: the forge's numeric account id (a pre-release SourceHut
    /// or Tangled link holds a "~user" or an atproto DID).
    pub subject_id: String,
    /// Display only; never used for lookup.
    pub handle_at_link: String,
    pub method: LinkMethod,
    /// The exact signed statement (UTF-8), for `SshSig`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub statement: String,
    /// Method-specific proof: the armored SSHSIG for `SshSig`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub proof: String,
    /// `SHA256:...` of the forge key that signed (`SshSig`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key_fingerprint: String,
    /// base64url (no padding) of 16 random bytes, the statement's `nonce`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub nonce: String,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
}

/// Providers whose links a forge can verify (key lists anyone may read).
pub const VERIFIABLE_PROVIDERS: &[&str] = &["github", "gitlab", "forgejo", "gitea"];

/// Providers whose verifiers 0.1 dropped (owner decision 2026-11-16). A link
/// naming one still folds unchanged (the wire shape and the statement are
/// untouched), but no device checks it: it shows as self-asserted, never as
/// verified, and no new one is offered.
pub const DROPPED_PROVIDERS: &[&str] = &["sourcehut", "tangled"];

/// Providers whose forge keeps a separate signing-key list, where the
/// account key grants no push access (KeyPublished is safe there).
pub const KEY_PUBLISHED_PROVIDERS: &[&str] = &["github", "gitlab"];

impl IdentityLink {
    /// The `(provider, host, subject)` a newer link with the same triple
    /// supersedes.
    pub fn subject_key(&self) -> (&str, &str, &str) {
        (&self.provider, &self.host, &self.subject_id)
    }
}

fn b64url(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// RFC 3339 in UTC at whole seconds (`2026-11-14T12:00:00Z`).
pub fn statement_time(t: Timestamp) -> String {
    let t = t.0.to_offset(time::UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// The statement a link's SSHSIG signs: UTF-8, fixed field order, LF line
/// ends, exactly one trailing LF. It binds the account and its key, the
/// workspace the link is published in (so it can't be replayed into another
/// workspace), the forge and the stable subject id, and the validity window.
pub fn render_statement(link: &IdentityLink, workspace_id: Uuid, account_key: &[u8]) -> String {
    format!(
        "{STATEMENT_TAG}\naccount: {}\naccount-key: ed25519:{}\nworkspace: {}\nprovider: {}\nhost: {}\nsubject: {}\nhandle: {}\nissued: {}\nexpires: {}\nnonce: {}\n",
        link.account_id.hyphenated(),
        b64url(account_key),
        workspace_id.hyphenated(),
        link.provider,
        link.host,
        link.subject_id,
        link.handle_at_link,
        statement_time(link.issued_at),
        statement_time(link.expires_at),
        link.nonce,
    )
}

/// `s` without Unicode bidi controls and zero-width characters, for showing
/// a name a forge (or a DID document) supplied.
pub fn strip_bidi_controls(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() && !is_invisible_format(*c))
        .collect()
}

/// Bidi embeddings, overrides, isolates and marks, zero-width and other
/// invisible format characters (and the fillers that render as nothing):
/// none belongs in a handle or an id, and each can make one name look like
/// another or reorder the text after it.
pub fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{17B4}'
            | '\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
}

fn token_ok(s: &str, max: usize, allow_space: bool) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.chars().all(|c| {
            !c.is_control() && !is_invisible_format(c) && (allow_space || !c.is_whitespace())
        })
        && s.trim() == s
}

/// The shape checks every device runs before a link folds (offline). The
/// statement fields can't carry a line break (no field injection), the
/// window is at most two years, and the proof fields match the method.
pub fn check_shape(link: &IdentityLink) -> Result<(), String> {
    let size = serde_json::to_vec(link)
        .map(|v| v.len())
        .unwrap_or(usize::MAX);
    if size > MAX_LINK_BYTES {
        return Err(format!(
            "an identity link is at most {MAX_LINK_BYTES} bytes"
        ));
    }
    if link.link_id.is_nil() || link.account_id.is_nil() {
        return Err("an identity link names its id and account".into());
    }
    let provider_ok = !link.provider.is_empty()
        && link.provider.len() <= 32
        && link
            .provider
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !provider_ok {
        return Err("malformed provider".into());
    }
    let host_ok = !link.host.is_empty()
        && link.host.len() <= 253
        && link.host.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b':')
        });
    if !host_ok {
        return Err("malformed host (lowercase host name, optional port)".into());
    }
    if !token_ok(&link.subject_id, 256, false) {
        return Err("malformed subject id".into());
    }
    if !link.handle_at_link.is_empty() && !token_ok(&link.handle_at_link, 256, false) {
        return Err("malformed handle".into());
    }
    if link.expires_at <= link.issued_at {
        return Err("an identity link expires after it is issued".into());
    }
    if link.expires_at.0 - link.issued_at.0 > time::Duration::days(MAX_LIFETIME_DAYS) {
        return Err("an identity link lasts at most two years".into());
    }
    if link.issued_at.0.nanosecond() != 0 || link.expires_at.0.nanosecond() != 0 {
        return Err("identity link times are whole seconds".into());
    }
    match link.method {
        LinkMethod::SshSig => {
            if link.statement.is_empty() || link.proof.is_empty() || link.key_fingerprint.is_empty()
            {
                return Err(
                    "an SSH-signed link carries its statement, signature and key fingerprint"
                        .into(),
                );
            }
            let nonce_ok = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&link.nonce)
                .is_ok_and(|n| n.len() == 16);
            if !nonce_ok {
                return Err("an SSH-signed link carries a 16-byte nonce".into());
            }
            if !token_ok(&link.key_fingerprint, 128, false) {
                return Err("malformed key fingerprint".into());
            }
        }
        LinkMethod::KeyPublished => {
            if !KEY_PUBLISHED_PROVIDERS.contains(&link.provider.as_str()) {
                return Err("a published-key link is only for GitHub and GitLab (elsewhere the key would grant push access)".into());
            }
            if !link.statement.is_empty() || !link.proof.is_empty() {
                return Err("a published-key link carries no statement".into());
            }
        }
        _ => {}
    }
    Ok(())
}

/// The fold's statement check for an `SshSig` link: the statement is exactly
/// the rendering of the link's fields, in the workspace it is published in,
/// under the account key in force at its position.
pub fn check_statement(
    link: &IdentityLink,
    workspace_id: Uuid,
    account_key: Option<&[u8]>,
) -> Result<(), String> {
    if link.method != LinkMethod::SshSig {
        return Ok(());
    }
    let Some(key) = account_key else {
        return Err("the account's key isn't registered (undisputed) here".into());
    };
    if link.statement != render_statement(link, workspace_id, key) {
        return Err(
            "the statement doesn't match the link, its workspace or its account key".into(),
        );
    }
    Ok(())
}

/// A random link id and nonce for a new link.
pub fn new_nonce() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("OS randomness");
    b64url(&b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        serde_json::from_value(serde_json::json!(s)).unwrap()
    }

    pub(crate) fn sample() -> IdentityLink {
        IdentityLink {
            link_id: Uuid::from_u128(1),
            account_id: Uuid::from_u128(0x6f1c),
            provider: "github".into(),
            host: "github.com".into(),
            subject_id: "1234567".into(),
            handle_at_link: "alice".into(),
            method: LinkMethod::SshSig,
            statement: String::new(),
            proof: "-----BEGIN SSH SIGNATURE-----".into(),
            key_fingerprint: "SHA256:abc".into(),
            nonce: "AAAAAAAAAAAAAAAAAAAAAA".into(),
            issued_at: ts("2026-11-14T12:00:00Z"),
            expires_at: ts("2027-11-14T12:00:00Z"),
        }
    }

    /// Pins the namespace as hex: a textual rename can't slip through.
    #[test]
    fn link_namespace_is_frozen() {
        let hex: String = LINK_NAMESPACE.bytes().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "6c696775732d6964656e746974792d6c696e6b2d7631406c696775732e6169"
        );
        assert_eq!(STATEMENT_TAG, "ligus-identity-link-v1");
    }

    #[test]
    fn statement_is_byte_exact() {
        let l = sample();
        let s = render_statement(&l, Uuid::from_u128(7), &[1u8; 32]);
        assert_eq!(
            s,
            "ligus-identity-link-v1\naccount: 00000000-0000-0000-0000-000000006f1c\naccount-key: ed25519:AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE\nworkspace: 00000000-0000-0000-0000-000000000007\nprovider: github\nhost: github.com\nsubject: 1234567\nhandle: alice\nissued: 2026-11-14T12:00:00Z\nexpires: 2027-11-14T12:00:00Z\nnonce: AAAAAAAAAAAAAAAAAAAAAA\n"
        );
        assert!(s.ends_with('\n') && !s.ends_with("\n\n"));
    }

    #[test]
    fn shape_rejects_injection_and_long_windows() {
        let mut l = sample();
        l.statement = "x".into();
        assert!(check_shape(&l).is_ok());
        let mut bad = l.clone();
        bad.handle_at_link = "alice\nsubject: 1".into();
        assert!(
            check_shape(&bad).is_err(),
            "a newline can't forge a statement field"
        );
        let mut bad = l.clone();
        bad.host = "GitHub.com".into();
        assert!(check_shape(&bad).is_err());
        let mut bad = l.clone();
        bad.expires_at = ts("2029-11-15T12:00:00Z");
        assert!(check_shape(&bad).is_err(), "more than two years");
        let mut bad = l.clone();
        bad.proof = "x".repeat(MAX_LINK_BYTES);
        assert!(check_shape(&bad).is_err(), "size cap");
        let mut kp = l.clone();
        kp.method = LinkMethod::KeyPublished;
        kp.statement.clear();
        kp.proof.clear();
        assert!(check_shape(&kp).is_ok());
        kp.provider = "forgejo".into();
        assert!(
            check_shape(&kp).is_err(),
            "published key would grant push on Forgejo"
        );
    }

    /// Bidi overrides, isolates and zero-width characters (Unicode format
    /// characters) can't hide in the fields a chip shows: "alice\u{200B}"
    /// would render as "alice", and "\u{202E}" can reorder what follows.
    #[test]
    fn shape_rejects_bidi_and_zero_width_in_handles() {
        for bad in [
            "alice\u{200B}",
            "\u{202E}ecila",
            "al\u{2066}ice\u{2069}",
            "alice\u{FEFF}",
            "a\u{200D}b",
            "a\u{061C}b",
        ] {
            let mut l = sample();
            l.statement = "x".into();
            l.handle_at_link = bad.into();
            assert!(check_shape(&l).is_err(), "handle {bad:?}");
            let mut l = sample();
            l.statement = "x".into();
            l.subject_id = bad.into();
            assert!(check_shape(&l).is_err(), "subject {bad:?}");
        }
        assert_eq!(
            strip_bidi_controls("\u{202E}al\u{200B}ice\u{2069}"),
            "alice"
        );
        assert_eq!(strip_bidi_controls("erin.example.com"), "erin.example.com");
    }

    #[test]
    fn statement_check_binds_workspace_and_key() {
        let mut l = sample();
        l.statement = render_statement(&l, Uuid::from_u128(7), &[1u8; 32]);
        assert!(check_statement(&l, Uuid::from_u128(7), Some(&[1u8; 32])).is_ok());
        assert!(
            check_statement(&l, Uuid::from_u128(8), Some(&[1u8; 32])).is_err(),
            "another workspace"
        );
        assert!(
            check_statement(&l, Uuid::from_u128(7), Some(&[2u8; 32])).is_err(),
            "another account key"
        );
        assert!(check_statement(&l, Uuid::from_u128(7), None).is_err());
    }

    /// The reserved `atprotoRecord` and `oidcAttestation` methods were never
    /// emitted and are no longer variants: an event carrying either still
    /// parses, as a method this build doesn't know.
    #[test]
    fn retired_methods_parse_as_unknown() {
        for m in ["atprotoRecord", "oidcAttestation"] {
            let mut v = serde_json::to_value(sample()).unwrap();
            v["method"] = serde_json::json!(m);
            let l: IdentityLink = serde_json::from_value(v).unwrap();
            assert_eq!(l.method, LinkMethod::Unknown, "{m}");
        }
        let names: Vec<String> = [
            LinkMethod::SshSig,
            LinkMethod::KeyPublished,
            LinkMethod::SelfAsserted,
        ]
        .iter()
        .map(|m| {
            serde_json::to_value(m)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
        assert_eq!(
            names,
            ["sshSig", "keyPublished", "selfAsserted"],
            "wire ids unchanged"
        );
    }

    #[test]
    fn unknown_method_and_skip_empty() {
        let mut v = serde_json::to_value(sample()).unwrap();
        v["method"] = serde_json::json!("somethingNew");
        let l: IdentityLink = serde_json::from_value(v).unwrap();
        assert_eq!(l.method, LinkMethod::Unknown);
        let mut kp = sample();
        kp.statement.clear();
        kp.proof.clear();
        kp.key_fingerprint.clear();
        kp.nonce.clear();
        let json = serde_json::to_string(&kp).unwrap();
        for f in ["statement", "proof", "keyFingerprint", "nonce"] {
            assert!(!json.contains(f), "{f} skips when empty");
        }
    }
}

//! Who a person is shown as, and what their devices are called (identity
//! model v2, sections 5 and 6).
//!
//! A person's name and picture have one authority: the newest
//! [`ProfileUpdated`](crate::events::SessionEvent::ProfileUpdated) for their
//! account on the workspace's config log. A device's name is the newest
//! [`DeviceLabelSet`](crate::events::SessionEvent::DeviceLabelSet) for the
//! pair (device, account). Both are **render-only** (class R): no rule reads
//! them, so a 0.3 build, which folds them as `Unknown`, agrees on every
//! verdict. They are folded only from signed events and never serialized
//! with a snapshot, so a snapshot can't plant one.
//!
//! The content checks are deterministic and pinned: the name rule is a frozen
//! copy ([`PROFILE_NAME_RULES_V1`]) of the display-name cleaner, written out
//! character by character rather than through the standard library's Unicode
//! tables, so a later toolchain or an edit of the cleaner can't make two
//! builds judge the same event differently.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// The name rules' version (identity model v2, 5.2). Bumped only with a
/// new rules version: the fold of every build must agree.
pub const PROFILE_NAME_RULES_V1: u32 = 1;
/// The longest profile name, in bytes.
pub const MAX_NAME_BYTES: usize = 256;
/// The longest profile name, in Unicode scalar values.
pub const MAX_NAME_CHARS: usize = 64;
/// The longest device label, in Unicode scalar values.
pub const MAX_LABEL_CHARS: usize = 48;
/// The largest avatar, decoded (decision O1).
pub const MAX_AVATAR_BYTES: usize = 16 * 1024;
/// The avatar content types accepted (decision O1: never SVG).
pub const AVATAR_TYPES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];

/// A picture carried by a profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Avatar {
    /// `image/png`, `image/jpeg` or `image/webp`.
    pub content_type: String,
    /// Standard base64 of the image bytes.
    pub data: String,
    /// Lower-case hex SHA-256 of the decoded bytes.
    pub sha256: String,
}

impl Avatar {
    /// A picture of `bytes`, checked ([`Avatar::check`]).
    pub fn new(content_type: &str, bytes: &[u8]) -> Result<Self, &'static str> {
        let a = Self {
            content_type: content_type.to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            sha256: hex(&Sha256::digest(bytes)),
        };
        a.check()?;
        Ok(a)
    }

    /// The decoded bytes, when the picture passes every content check.
    pub fn check(&self) -> Result<Vec<u8>, &'static str> {
        if !AVATAR_TYPES.contains(&self.content_type.as_str()) {
            return Err("an avatar is a PNG, JPEG or WebP image");
        }
        // Bound the work before decoding: base64 is 4 bytes per 3.
        if self.data.len() > MAX_AVATAR_BYTES.div_ceil(3) * 4 + 4 {
            return Err("an avatar is at most 16 KiB");
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(self.data.as_bytes()).map_err(|_| "an avatar's data isn't base64")?;
        if bytes.len() > MAX_AVATAR_BYTES {
            return Err("an avatar is at most 16 KiB");
        }
        let magic_ok = match self.content_type.as_str() {
            "image/png" => bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
            "image/jpeg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
            "image/webp" => bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
            _ => false,
        };
        if !magic_ok {
            return Err("an avatar's bytes aren't the image type it names");
        }
        if hex(&Sha256::digest(&bytes)) != self.sha256 {
            return Err("an avatar's sha256 doesn't match its bytes");
        }
        Ok(bytes)
    }

    /// A `data:` URL for rendering.
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.content_type, self.data)
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// What a `ProfileUpdated` says: absent fields are unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileChange {
    /// Must be the stamp's account.
    pub account_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<Avatar>,
    /// Remove the picture (`avatar` must then be absent).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clear_avatar: bool,
    /// The author's clock (Unix ms): read only by the cross-workspace
    /// directory, never by the fold.
    #[serde(default)]
    pub issued_at_ms: i64,
}

/// A device's name under one account (`DeviceLabelSet`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceLabel {
    pub device_id: Uuid,
    /// Must be the stamp's account.
    pub account_id: Uuid,
    /// 0 to 48 characters; empty clears it.
    #[serde(default)]
    pub label: String,
}

/// A person's folded profile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    pub display_name: Option<String>,
    pub avatar: Option<Avatar>,
    /// The newest picture change removed the picture (render initials, not
    /// an older copy).
    pub avatar_cleared: bool,
    /// The `issuedAtMs` and event id of the newest change folded (the
    /// directory picks across workspaces by them).
    pub issued_at_ms: i64,
    pub event_id: Option<Uuid>,
}

impl Profile {
    /// Fold one allowed change (last writer wins per field, in fold order).
    pub fn apply(&mut self, change: &ProfileChange, event_id: Uuid) {
        if let Some(n) = &change.display_name {
            self.display_name = Some(n.clone());
        }
        if let Some(a) = &change.avatar {
            self.avatar = Some(a.clone());
            self.avatar_cleared = false;
        } else if change.clear_avatar {
            self.avatar = None;
            self.avatar_cleared = true;
        }
        self.issued_at_ms = change.issued_at_ms;
        self.event_id = Some(event_id);
    }
}

/// Whether `c` is removed from a displayed name under
/// [`PROFILE_NAME_RULES_V1`]: a frozen copy of
/// `ligus_proto::names::is_hidden_name_char`, with the control characters
/// (Unicode category Cc) written out.
fn hidden_v1(c: char) -> bool {
    matches!(
        c,
        '\u{0000}'..='\u{001F}'
        | '\u{007F}'..='\u{009F}'
        | '\u{00AD}'
        | '\u{034F}'
        | '\u{061C}'
        | '\u{115F}' | '\u{1160}'
        | '\u{17B4}' | '\u{17B5}'
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

/// White space under [`PROFILE_NAME_RULES_V1`]: the Unicode White_Space
/// property written out (the controls among it are hidden first).
fn space_v1(c: char) -> bool {
    matches!(
        c,
        ' ' | '\u{0085}' | '\u{00A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

/// The cleaned form of `name` under [`PROFILE_NAME_RULES_V1`] (hidden
/// characters removed, white space runs collapsed to one space, trimmed).
pub fn clean_v1(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut space = false;
    for c in name.chars().filter(|c| !hidden_v1(*c)) {
        if space_v1(c) {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    out
}

/// A profile name the fold accepts: at most 256 bytes, 1 to 64 characters,
/// and already in its cleaned form.
pub fn name_ok(name: &str) -> Result<(), &'static str> {
    if name.len() > MAX_NAME_BYTES {
        return Err("a name is at most 256 bytes");
    }
    let n = name.chars().count();
    if n == 0 || n > MAX_NAME_CHARS {
        return Err("a name is 1 to 64 characters");
    }
    if clean_v1(name) != name {
        return Err("a name has no hidden or control characters and no extra spaces");
    }
    Ok(())
}

/// A device label the fold accepts: empty (cleared), or at most 48
/// characters already in its cleaned form.
pub fn label_ok(label: &str) -> Result<(), &'static str> {
    if label.len() > MAX_NAME_BYTES || label.chars().count() > MAX_LABEL_CHARS {
        return Err("a device name is at most 48 characters");
    }
    if clean_v1(label) != label {
        return Err("a device name has no hidden or control characters and no extra spaces");
    }
    Ok(())
}

/// The content checks of a profile change (who may write it is
/// `authz::profiles`).
pub fn change_ok(change: &ProfileChange) -> Result<(), &'static str> {
    if change.display_name.is_none() && change.avatar.is_none() && !change.clear_avatar {
        return Err("a profile change changes something");
    }
    if change.clear_avatar && change.avatar.is_some() {
        return Err("a profile change either sets or clears the picture");
    }
    if let Some(n) = &change.display_name {
        name_ok(n)?;
    }
    if let Some(a) = &change.avatar {
        a.check()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        v.extend_from_slice(&[0u8; 32]);
        v
    }

    #[test]
    fn frozen_cleaner_matches_the_display_cleaner_on_its_own_set() {
        for s in ["Ana", "  Ana  Ruiz ", "A\u{202E}na", "A\u{200B}na", "Ana\u{0007}", "Ana\u{3000}Ruiz", "Zoë", "李华"] {
            assert_eq!(clean_v1(s), ligus_proto::names::clean_display_name(s), "{s:?}");
        }
    }

    #[test]
    fn names_are_checked() {
        assert!(name_ok("Ana Ruiz").is_ok());
        for bad in ["", " Ana", "Ana ", "A  na", "A\u{202E}na", "A\u{200B}na", "Ana\n"] {
            assert!(name_ok(bad).is_err(), "{bad:?}");
        }
        assert!(name_ok(&"a".repeat(64)).is_ok());
        assert!(name_ok(&"a".repeat(65)).is_err());
        assert!(name_ok(&"\u{1F41D}".repeat(65)).is_err());
        assert!(name_ok(&"é".repeat(64)).is_ok());
        assert!(label_ok("").is_ok(), "empty clears");
        assert!(label_ok(&"x".repeat(49)).is_err());
    }

    #[test]
    fn avatars_are_checked() {
        let a = Avatar::new("image/png", &png()).unwrap();
        assert!(a.check().is_ok());
        assert!(Avatar::new("image/svg+xml", b"<svg/>").is_err(), "never SVG");
        assert!(Avatar::new("image/jpeg", &png()).is_err(), "magic bytes");
        let mut big = png();
        big.resize(MAX_AVATAR_BYTES + 1, 0);
        assert!(Avatar::new("image/png", &big).is_err(), "17 KiB");
        let mut wrong = a.clone();
        wrong.sha256 = "00".repeat(32);
        assert!(wrong.check().is_err(), "sha256");
        let mut webp = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        webp.extend_from_slice(&[0u8; 16]);
        assert!(Avatar::new("image/webp", &webp).is_ok());
        assert!(Avatar::new("image/jpeg", &[0xFF, 0xD8, 0xFF, 0xE0]).is_ok());
    }

    #[test]
    fn a_change_changes_something_and_never_both_sets_and_clears() {
        let id = Uuid::new_v4();
        let mut c = ProfileChange { account_id: id, display_name: None, avatar: None, clear_avatar: false, issued_at_ms: 0 };
        assert!(change_ok(&c).is_err());
        c.clear_avatar = true;
        assert!(change_ok(&c).is_ok());
        c.avatar = Some(Avatar::new("image/png", &png()).unwrap());
        assert!(change_ok(&c).is_err());
    }
}

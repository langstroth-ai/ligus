//! Short link codes (Ligus 0.5, `docs/design/workspace-model-v3.md` 17.2):
//! the device a person already uses shows `K7Q-4MX`, and their new device
//! types it instead of pasting the approval address.
//!
//! The relay holds the code's lookup id, the mailbox and the sealed address,
//! never the code or the address:
//!
//! ```text
//! stretch = Argon2id(code, salt = "ligus-link-code-v1\0" ‖ relay,
//!                    m = 64 MiB, t = 3, p = 1, 48 bytes)
//! lookup  = stretch[0..16]           (the relay's {code}, 32 lowercase hex)
//! key     = stretch[16..48]
//! blob    = b64u(nonce(12) ‖ ChaCha20-Poly1305(key, nonce, address,
//!                aad = "ligus-link-code-v1\0" ‖ lookup ‖ mailbox))
//! ```
//!
//! `code` is the 6 symbols of Crockford base32 (30 bits), upper case, with
//! no separator; `relay` is the relay's base URL as [`relay_salt`] spells
//! it. The approval address is public and grants nothing, so a code for it
//! carries nothing secret; Argon2id keeps the relay (and anyone guessing
//! online) from reading which workspace a code is for without ~2^30
//! memory-hard guesses. All byte strings here are wire ids: never rename
//! them with the product.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

use crate::match_code::ALPHABET;

/// FROZEN: the salt prefix and the AEAD context.
pub const CTX: &[u8] = b"ligus-link-code-v1\0";
/// Symbols in a code.
pub const LEN: usize = 6;
/// Bits in a code.
pub const BITS: u32 = 30;
/// How long a code lives on the relay (its longest).
pub const TTL_MS: i64 = 10 * 60 * 1000;

const ARGON_M_KIB: u32 = 64 * 1024;
const ARGON_T: u32 = 3;

/// A short code: 6 Crockford base32 symbols, upper case.
#[derive(Clone, PartialEq, Eq)]
pub struct ShortCode(String);

impl std::fmt::Debug for ShortCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ShortCode(..)")
    }
}

impl ShortCode {
    /// A fresh random code.
    pub fn generate() -> Self {
        let mut b = [0u8; 4];
        getrandom::fill(&mut b).expect("the OS random source");
        let mut n = u32::from_le_bytes(b) & ((1 << BITS) - 1);
        let mut out = [0u8; LEN];
        for slot in out.iter_mut().rev() {
            *slot = ALPHABET[(n & 31) as usize];
            n >>= 5;
        }
        Self(String::from_utf8(out.to_vec()).expect("ascii"))
    }

    /// Read what a person typed: case, spaces and dashes don't matter, and
    /// `O` reads as `0`, `I` and `L` as `1` (Crockford). `None` when it isn't
    /// six code symbols.
    pub fn parse(text: &str) -> Option<Self> {
        let mut out = String::with_capacity(LEN);
        for c in text.trim().chars() {
            if c.is_whitespace() || c == '-' || c == '\u{2010}' || c == '\u{2011}' || c == '\u{2013}' {
                continue;
            }
            let c = match c.to_ascii_uppercase() {
                'O' => '0',
                'I' | 'L' => '1',
                c => c,
            };
            if !c.is_ascii() || !ALPHABET.contains(&(c as u8)) {
                return None;
            }
            out.push(c);
            if out.len() > LEN {
                return None;
            }
        }
        (out.len() == LEN).then_some(Self(out))
    }

    /// `K7Q4MX`: what `--code` takes and the stretch reads.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `K7Q-4MX`: what a screen shows.
    pub fn shown(&self) -> String {
        format!("{}-{}", &self.0[..3], &self.0[3..])
    }

    /// One symbol at a time, for a screen reader ("K 7 Q, 4 M X").
    pub fn spelled(&self) -> String {
        let s: Vec<String> = self.0.chars().map(|c| c.to_string()).collect();
        format!("{}, {}", s[..3].join(" "), s[3..].join(" "))
    }

    /// The lookup id and the sealing key for this code on `relay`.
    pub fn stretch(&self, relay: &str) -> Stretched {
        use argon2::{Algorithm, Argon2, Params, Version};
        let params = Params::new(ARGON_M_KIB, ARGON_T, 1, Some(48)).expect("valid argon2 params");
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let mut salt = CTX.to_vec();
        salt.extend_from_slice(relay_salt(relay).as_bytes());
        let mut out = [0u8; 48];
        argon.hash_password_into(self.0.as_bytes(), &salt, &mut out).expect("argon2 derive");
        let mut lookup = [0u8; 16];
        lookup.copy_from_slice(&out[..16]);
        let mut key = [0u8; 32];
        key.copy_from_slice(&out[16..]);
        out.fill(0);
        Stretched { lookup, key }
    }
}

/// A code's lookup id (public: the relay's `{code}`) and sealing key.
#[derive(Clone)]
pub struct Stretched {
    pub lookup: [u8; 16],
    key: [u8; 32],
}

impl std::fmt::Debug for Stretched {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stretched").field("lookup", &crate::match_code::hex(&self.lookup)).finish_non_exhaustive()
    }
}

impl Drop for Stretched {
    fn drop(&mut self) {
        self.key.fill(0);
    }
}

fn aad(lookup: &[u8; 16], mailbox: &[u8; 16]) -> Vec<u8> {
    let mut a = CTX.to_vec();
    a.extend_from_slice(lookup);
    a.extend_from_slice(mailbox);
    a
}

impl Stretched {
    /// 32 lowercase hex: the path segment the relay sees.
    pub fn lookup_hex(&self) -> String {
        crate::match_code::hex(&self.lookup)
    }

    /// Seal `address` (the `ligusaddr1:` text) for the code's `mailbox`.
    pub fn seal(&self, mailbox: &[u8; 16], address: &str) -> String {
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).expect("the OS random source");
        let cipher = ChaCha20Poly1305::new(&Key::from(self.key));
        let ct = cipher
            .encrypt(&Nonce::from(nonce), Payload { msg: address.as_bytes(), aad: &aad(&self.lookup, mailbox) })
            .expect("chacha20poly1305 seal");
        let mut raw = nonce.to_vec();
        raw.extend_from_slice(&ct);
        b64u(&raw)
    }

    /// Open a blob the relay returned for `mailbox`. `None` when it doesn't
    /// open (another code, another relay, or tampered with).
    pub fn open(&self, mailbox: &[u8; 16], blob: &str) -> Option<String> {
        use base64::Engine as _;
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(blob.trim()).ok()?;
        if raw.len() < 12 + 16 {
            return None;
        }
        let cipher = ChaCha20Poly1305::new(&Key::from(self.key));
        let nonce: [u8; 12] = raw[..12].try_into().ok()?;
        let pt = cipher.decrypt(&Nonce::from(nonce), Payload { msg: &raw[12..], aad: &aad(&self.lookup, mailbox) }).ok()?;
        String::from_utf8(pt).ok()
    }
}

fn b64u(b: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// The relay as the salt spells it: trimmed, no trailing `/`, and the
/// scheme and host in lower case (`https://Relay.Example/` and
/// `https://relay.example` are one relay).
pub fn relay_salt(relay: &str) -> String {
    let r = relay.trim().trim_end_matches('/');
    match r.split_once("://") {
        Some((scheme, rest)) => {
            let (host, path) = rest.split_once('/').map_or((rest, ""), |(h, p)| (h, p));
            let mut out = format!("{}://{}", scheme.to_ascii_lowercase(), host.to_ascii_lowercase());
            if !path.is_empty() {
                out.push('/');
                out.push_str(path);
            }
            out
        }
        None => r.to_ascii_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_codes_are_six_symbols_and_parse_back() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let c = ShortCode::generate();
            assert_eq!(c.as_str().len(), LEN);
            assert_eq!(ShortCode::parse(&c.shown()), Some(c.clone()));
            assert_eq!(ShortCode::parse(&c.as_str().to_lowercase()), Some(c.clone()));
            seen.insert(c.0);
        }
        assert!(seen.len() > 190, "codes repeat: {}", seen.len());
    }

    #[test]
    fn typing_is_forgiving_but_strict_about_length_and_symbols() {
        let want = ShortCode::parse("K7Q4MX").unwrap();
        for t in ["k7q-4mx", " K7Q 4MX ", "K7Q\u{2011}4MX", "k7q4mx\n"] {
            assert_eq!(ShortCode::parse(t), Some(want.clone()), "{t:?}");
        }
        assert_eq!(ShortCode::parse("O1L-I00").unwrap().as_str(), "011100");
        for t in ["", "K7Q4M", "K7Q4MXX", "K7Q4MU", "K7Q4M!", "ligusaddr1:abc", "K7Q4MÉ"] {
            assert_eq!(ShortCode::parse(t), None, "{t:?}");
        }
        assert_eq!(want.shown(), "K7Q-4MX");
        assert_eq!(want.spelled(), "K 7 Q, 4 M X");
        assert_eq!(format!("{want:?}"), "ShortCode(..)");
    }

    #[test]
    fn relay_salt_ignores_case_of_scheme_and_host_and_a_trailing_slash() {
        assert_eq!(relay_salt(" HTTPS://Relay.Example.org/ "), "https://relay.example.org");
        assert_eq!(relay_salt("https://r.example/Base/"), "https://r.example/Base");
        assert_eq!(relay_salt("http://127.0.0.1:8080"), "http://127.0.0.1:8080");
    }

    // FROZEN vector: the phone apps and the relay docs follow this
    // derivation byte for byte.
    #[test]
    fn stretch_vector() {
        let s = ShortCode::parse("K7Q4MX").unwrap().stretch("https://relay.langstroth.ai");
        assert_eq!(s.lookup_hex(), STRETCH_VECTOR_LOOKUP);
        assert_eq!(crate::match_code::hex(&s.key), STRETCH_VECTOR_KEY);
        // The relay matters, its spelling doesn't.
        assert_eq!(ShortCode::parse("k7q-4mx").unwrap().stretch("https://Relay.Langstroth.ai/").lookup, s.lookup);
        assert_ne!(ShortCode::parse("K7Q4MX").unwrap().stretch("https://other.example").lookup, s.lookup);
        assert_ne!(ShortCode::parse("K7Q4MY").unwrap().stretch("https://relay.langstroth.ai").lookup, s.lookup);
    }
    // Checked against an independent Argon2id (Go x/crypto/argon2.IDKey).
    const STRETCH_VECTOR_LOOKUP: &str = "6446ec733620d5bfbe4c83470ecb89ee";
    const STRETCH_VECTOR_KEY: &str = "61225f5b5766d7472f70dfd0ac4fe93314a7c8bc13582a3cad5cae42169f8847";

    #[test]
    fn seal_opens_only_with_its_code_lookup_and_mailbox() {
        let relay = "https://relay.example";
        let s = ShortCode::parse("ABC123").unwrap().stretch(relay);
        let mb = [7u8; 16];
        let blob = s.seal(&mb, "ligusaddr1:hello");
        assert_eq!(s.open(&mb, &blob).as_deref(), Some("ligusaddr1:hello"));
        assert_ne!(s.seal(&mb, "ligusaddr1:hello"), blob, "a fresh nonce each time");
        assert_eq!(s.open(&[8u8; 16], &blob), None, "another mailbox");
        assert_eq!(ShortCode::parse("ABC124").unwrap().stretch(relay).open(&mb, &blob), None, "another code");
        let mut bad = blob.clone();
        bad.replace_range(20..21, if &blob[20..21] == "A" { "B" } else { "A" });
        assert_eq!(s.open(&mb, &bad), None, "tampered");
        assert_eq!(s.open(&mb, "short"), None);
        assert!(!format!("{s:?}").contains(&crate::match_code::hex(&s.key)));
    }
}

//! Redaction of Ligus's **own** secrets: the serve token, relay `hrt1`
//! tokens, phone pairing credentials, device and workspace key seeds, and
//! every provider key or OAuth token in this device's settings.
//!
//! The heuristic credential patterns in [`crate::guard_scan`] catch
//! third-party keys by shape. Ligus's own secrets mostly have no recognisable
//! shape (the serve token is 64 hex characters, a seed is 32 random bytes),
//! so they are matched **exactly**: each host registers the values it holds
//! when it loads them ([`register`], [`register_bytes`]), and
//! [`redact_ligus_secrets`] replaces every occurrence with `[ligus-secret]`
//! using one Aho-Corasick automaton over all registered values. The few
//! that do have a shape (`hrt1.…`, `hpc1…`) are matched by pattern too, so
//! one this device never held is still caught.
//!
//! Agent replies, streamed chunks and proposal bodies pass through this on
//! their way into the log ([`crate::guard_scan::redact_secrets`] calls it
//! first), as do facts.

use std::sync::{OnceLock, RwLock};

use aho_corasick::AhoCorasick;
use regex::Regex;

/// What a Ligus secret is replaced with.
pub const MARK: &str = "[ligus-secret]";

/// Shortest value registered: anything shorter would mask ordinary text.
const MIN_LEN: usize = 16;

#[derive(Default)]
struct Registry {
    values: Vec<String>,
    automaton: Option<AhoCorasick>,
}

fn registry() -> &'static RwLock<Registry> {
    static R: OnceLock<RwLock<Registry>> = OnceLock::new();
    R.get_or_init(Default::default)
}

fn patterns() -> &'static [Regex] {
    static P: OnceLock<Vec<Regex>> = OnceLock::new();
    P.get_or_init(|| {
        [
            // Relay access tokens: hrt1.<payload>.<sig>.
            r"\bhrt1\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}",
            // Phone pairing credentials and codes: hpc1 + an opaque body.
            r"\bhpc1[.:_\-][A-Za-z0-9_\-.:]{16,}",
        ]
        .iter()
        .map(|re| Regex::new(re).expect("ligus secret pattern"))
        .collect()
    })
}

/// Register one secret value held by this process (a token, a key). Values
/// shorter than 16 characters are ignored.
pub fn register(value: &str) {
    let v = value.trim();
    if v.len() < MIN_LEN {
        return;
    }
    let mut r = registry().write().unwrap_or_else(|e| e.into_inner());
    if r.values.iter().any(|x| x == v) {
        return;
    }
    r.values.push(v.to_string());
    // Longest first, so a value that contains another is masked whole.
    let mut vals = r.values.clone();
    vals.sort_by_key(|v| std::cmp::Reverse(v.len()));
    r.automaton = AhoCorasick::builder()
        .match_kind(aho_corasick::MatchKind::LeftmostLongest)
        .build(&vals)
        .ok();
}

/// Register raw secret bytes (a key seed) in every text form it could leak
/// as: lowercase and uppercase hex, standard and URL-safe base64 (padded and
/// unpadded).
pub fn register_bytes(bytes: &[u8]) {
    use base64::Engine;
    if bytes.len() < 12 {
        return;
    }
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    register(&hex.to_ascii_uppercase());
    register(&hex);
    for engine in [
        &base64::engine::general_purpose::STANDARD,
        &base64::engine::general_purpose::STANDARD_NO_PAD,
        &base64::engine::general_purpose::URL_SAFE,
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
    ] {
        register(&engine.encode(bytes));
    }
}

/// Register the values of this process's Ligus secret env vars (see
/// [`hive_core::process_util::ligus_secret_env_names`]).
pub fn register_env_secrets() {
    for k in hive_core::process_util::ligus_secret_env_names() {
        if let Some(v) = std::env::var_os(&k) {
            register(&v.to_string_lossy());
        }
    }
}

/// Register every string under a secret-named field of a settings document
/// (any key containing key, token, secret, passphrase or password, at any
/// depth: `providerKeys`, `mcpOauth.*.accessToken`, …).
pub fn register_json_secrets(v: &serde_json::Value) {
    fn walk(v: &serde_json::Value, secret: bool) {
        match v {
            serde_json::Value::String(s) if secret => register(s),
            serde_json::Value::Array(xs) => xs.iter().for_each(|x| walk(x, secret)),
            serde_json::Value::Object(m) => {
                for (k, x) in m {
                    let k = k.to_ascii_lowercase();
                    let named = ["key", "token", "secret", "passphrase", "password"].iter().any(|w| k.contains(w));
                    walk(x, secret || named);
                }
            }
            _ => {}
        }
    }
    walk(v, false);
}

/// Mask every Ligus secret in `text` as [`MARK`]. Returns the masked text and
/// how many were masked.
pub fn redact_ligus_secrets(text: &str) -> (String, u32) {
    let mut count = 0u32;
    let mut out = {
        let r = registry().read().unwrap_or_else(|e| e.into_inner());
        match &r.automaton {
            Some(ac) => {
                let n = ac.find_iter(text).count() as u32;
                if n == 0 {
                    text.to_string()
                } else {
                    count += n;
                    let marks = vec![MARK; r.values.len()];
                    ac.replace_all(text, &marks)
                }
            }
            None => text.to_string(),
        }
    };
    for re in patterns() {
        let n = re.find_iter(&out).count() as u32;
        if n > 0 {
            count += n;
            out = re.replace_all(&out, MARK).into_owned();
        }
    }
    (out, count)
}

/// [`redact_ligus_secrets`], text only.
pub fn redacted(text: &str) -> String {
    redact_ligus_secrets(text).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_values_and_shapes_are_masked() {
        let token = "4f1d0c2b9e8a7f6e5d4c3b2a19087f6e5d4c3b2a19087f6e5d4c3b2a19087f6e";
        register(token);
        register("short"); // ignored
        let seed = [7u8; 32];
        register_bytes(&seed);
        let hex: String = seed.iter().map(|b| format!("{b:02x}")).collect();
        let text = format!(
            "token={token} seed={hex} relay=hrt1.eyJzdWIiOiJ4In0.c2lnbmF0dXJlMTIz pair=hpc1.AAAABBBBCCCCDDDDEEEE ok short"
        );
        let (m, n) = redact_ligus_secrets(&text);
        assert_eq!(n, 4, "{m}");
        assert!(!m.contains(token) && !m.contains(&hex) && !m.contains("hrt1.") && !m.contains("hpc1."), "{m}");
        assert!(m.contains("ok short"));
        // Base64 of the seed too.
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(seed);
        assert_eq!(redacted(&b64), MARK);
        register_json_secrets(&serde_json::json!({"providerKeys": {"openai": "sk-ligus-test-provider-key-0001"}, "theme": "dark-mode-default-theme"}));
        assert_eq!(redacted("k=sk-ligus-test-provider-key-0001"), format!("k={MARK}"));
        assert_eq!(redacted("dark-mode-default-theme"), "dark-mode-default-theme");
        // Ordinary text is untouched.
        assert_eq!(redact_ligus_secrets("nothing to see"), ("nothing to see".to_string(), 0));
    }
}

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

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use aho_corasick::AhoCorasick;
use regex::Regex;

/// What a Ligus secret is replaced with.
pub const MARK: &str = "[ligus-secret]";

/// Shortest value registered: anything shorter would mask ordinary text.
const MIN_LEN: usize = 16;

/// The registered values, and the automaton over them as of some
/// generation. Registering only adds a value (the write lock is held for a
/// moment); the automaton is rebuilt by the next redaction that finds it
/// behind, outside this lock ([`automaton`]). Rebuilding it inside the lock
/// on every registration made registration cost grow with the registry and
/// starved every reader (a turn's reply, a streamed chunk, a log line) while
/// registrations kept coming, each holding the lock for the whole rebuild.
#[derive(Default)]
struct Registry {
    values: Vec<String>,
    seen: HashSet<String>,
    /// Bumped by every value added.
    generation: u64,
    /// The matcher over `values` as of `built`.
    automaton: Option<Arc<Matcher>>,
    built: u64,
}

fn registry() -> &'static RwLock<Registry> {
    static R: OnceLock<RwLock<Registry>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// One rebuild at a time: a redaction that finds the automaton behind waits
/// here for the rebuild in progress instead of starting its own.
fn rebuilding() -> &'static Mutex<()> {
    static B: OnceLock<Mutex<()>> = OnceLock::new();
    B.get_or_init(Default::default)
}

/// The automaton over every value registered before this call. Rebuilt
/// here when registrations came since the last build: the values are
/// copied under the read lock and the automaton is built with no lock on
/// the registry, so registering and redacting never wait for a build.
fn automaton() -> Option<Arc<Matcher>> {
    {
        let r = registry().read().unwrap_or_else(|e| e.into_inner());
        if r.built == r.generation {
            return r.automaton.clone();
        }
    }
    let _one = rebuilding().lock().unwrap_or_else(|e| e.into_inner());
    let (mut vals, generation) = {
        let r = registry().read().unwrap_or_else(|e| e.into_inner());
        if r.built == r.generation {
            return r.automaton.clone();
        }
        (r.values.clone(), r.generation)
    };
    // Longest first, so a value that contains another is masked whole.
    vals.sort_by_key(|v| std::cmp::Reverse(v.len()));
    let built = Some(Arc::new(build_matcher(vals, false)));
    let mut r = registry().write().unwrap_or_else(|e| e.into_inner());
    if r.built < generation {
        r.automaton = built.clone();
        r.built = generation;
    }
    built
}

/// What masks the registered values: one automaton over them all, or, when
/// that can't be built (too many or too long values for its limits), each
/// value in turn, longest first. Slower, but a registered secret is never
/// left unmasked because the fast path failed (fail closed).
enum Matcher {
    Automaton(AhoCorasick),
    Each(Vec<String>),
}

/// The matcher over `vals` (longest first). `force_fail` (tests) takes the
/// path a failed automaton build takes.
fn build_matcher(vals: Vec<String>, force_fail: bool) -> Matcher {
    let built = if force_fail {
        None
    } else {
        match AhoCorasick::builder().match_kind(aho_corasick::MatchKind::LeftmostLongest).build(&vals) {
            Ok(ac) => Some(ac),
            Err(e) => {
                tracing::error!(target: "secret_redact", values = vals.len(), "building the secret matcher failed, masking value by value: {e}");
                None
            }
        }
    };
    match built {
        Some(ac) => Matcher::Automaton(ac),
        None => Matcher::Each(vals),
    }
}

/// Mask every registered value `m` matches in `text`: the masked text and
/// how many were masked.
fn mask_values(m: &Matcher, text: &str) -> (String, u32) {
    match m {
        Matcher::Automaton(ac) => {
            let n = ac.find_iter(text).count() as u32;
            if n == 0 {
                (text.to_string(), 0)
            } else {
                let marks = vec![MARK; ac.patterns_len()];
                (ac.replace_all(text, &marks), n)
            }
        }
        Matcher::Each(vals) => {
            let mut out = text.to_string();
            let mut n = 0u32;
            for v in vals {
                let k = out.matches(v.as_str()).count() as u32;
                if k > 0 {
                    n += k;
                    out = out.replace(v.as_str(), MARK);
                }
            }
            (out, n)
        }
    }
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
    if !r.seen.insert(v.to_string()) {
        return;
    }
    r.values.push(v.to_string());
    r.generation += 1;
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
/// [`ligus_core::process_util::ligus_secret_env_names`]).
pub fn register_env_secrets() {
    for k in ligus_core::process_util::ligus_secret_env_names() {
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
    let mut out = match automaton() {
        Some(m) => {
            let (out, n) = mask_values(&m, text);
            count += n;
            out
        }
        None => text.to_string(),
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

/// Where the longest suffix of `text` that is the start of a registered
/// secret begins (a stream cut in the middle of one), if any. A streamed
/// chunk holds that much back until the text after it shows whether the
/// whole secret follows.
pub fn partial_secret_start(text: &str) -> Option<usize> {
    let r = registry().read().unwrap_or_else(|e| e.into_inner());
    let mut start: Option<usize> = None;
    for v in &r.values {
        let longest = (v.len() - 1).min(text.len());
        for k in (1..=longest).rev() {
            if !v.is_char_boundary(k) || !text.is_char_boundary(text.len() - k) {
                continue;
            }
            if text.ends_with(&v[..k]) {
                let at = text.len() - k;
                start = Some(start.map_or(at, |s| s.min(at)));
                break;
            }
        }
    }
    start
}

/// [`redact_ligus_secrets`], text only.
pub fn redacted(text: &str) -> String {
    redact_ligus_secrets(text).0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A matcher whose automaton couldn't be built still masks every
    /// registered value (fails closed), the longest whole.
    #[test]
    fn a_failed_automaton_build_still_masks_every_value() {
        let long = "0123456789abcdef0123456789abcdef-long".to_string();
        let short = "0123456789abcdef0123456789abcdef".to_string();
        let other = "ZYXWVUTSRQPONMLKJIHG".to_string();
        let m = build_matcher(vec![long.clone(), short.clone(), other.clone()], true);
        assert!(matches!(m, Matcher::Each(_)));
        let text = format!("a {long} b {short} c {other} d {other}");
        let (out, n) = mask_values(&m, &text);
        assert_eq!(out, format!("a {MARK} b {MARK} c {MARK} d {MARK}"));
        assert_eq!(n, 4);
        let fast = build_matcher(vec![long, short, other], false);
        assert!(matches!(fast, Matcher::Automaton(_)));
        assert_eq!(mask_values(&fast, &text), (out, n), "both paths mask alike");
    }

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

    /// Many secrets registered (a test process starts hundreds of devices;
    /// a long-running host keeps adding keys) while other threads redact:
    /// registering stays cheap, a redaction never waits long behind it, and
    /// each value is masked as soon as its registration returns. Building
    /// the automaton inside the registry's write lock on every registration
    /// made this quadratic and kept readers (a turn's reply, the serve
    /// worker) waiting while registrations kept coming.
    #[test]
    fn registering_many_secrets_never_starves_redaction() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::{Duration, Instant};
        let n = 3000;
        let done = std::sync::Arc::new(AtomicBool::new(false));
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let done = done.clone();
                std::thread::spawn(move || {
                    let mut worst = Duration::ZERO;
                    while !done.load(Ordering::SeqCst) {
                        let t = Instant::now();
                        let _ = redacted("a reply with nothing secret in it");
                        worst = worst.max(t.elapsed());
                    }
                    worst
                })
            })
            .collect();
        let t = Instant::now();
        for i in 0..n {
            let v = format!("starve-test-secret-{i:08}-padding");
            register(&v);
            if i % 500 == 0 {
                assert_eq!(redacted(&format!("x {v} y")), format!("x {MARK} y"), "masked as soon as registered");
            }
        }
        let registering = t.elapsed();
        done.store(true, Ordering::SeqCst);
        let worst = readers.into_iter().map(|h| h.join().unwrap()).max().unwrap();
        assert!(registering < Duration::from_secs(10), "registering {n} secrets took {registering:?}");
        assert!(worst < Duration::from_secs(5), "a redaction waited {worst:?}");
    }
}

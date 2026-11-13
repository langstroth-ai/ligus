//! Name-compatibility shims for the product rename (Hive -> Ligus).
//!
//! Environment variables have a canonical `LIGUS_` name and a legacy `HIVE_`
//! name. Every runtime read goes through [`var`] / [`env_opt`]: the `LIGUS_`
//! name wins, the `HIVE_` name is the fallback. Callers may pass either
//! spelling (`"HIVE_DATA_DIR"` or `"LIGUS_DATA_DIR"`); names with neither
//! prefix (`HOME`, `PATH`) are read as-is, so the helpers are drop-in
//! replacements for `std::env::var`.
//!
//! When both names are set to different values a one-time warning goes to
//! stderr (the `LIGUS_` value is used). Falling back to a `HIVE_` name warns
//! once per variable only when [`WARN_ON_LEGACY_ENV`] is on (from the release
//! that makes `LIGUS_*` the documented spelling).

use std::collections::HashSet;
use std::env::VarError;
use std::ffi::OsString;
use std::sync::Mutex;

/// Canonical environment-variable prefix.
pub const ENV_PREFIX: &str = "LIGUS_";
/// Legacy environment-variable prefix, still honoured as a fallback.
pub const LEGACY_ENV_PREFIX: &str = "HIVE_";

/// Whether reading a value from a legacy `HIVE_` name prints a deprecation
/// note. Off while `HIVE_*` is still the documented spelling.
pub const WARN_ON_LEGACY_ENV: bool = false;

/// Where a resolved value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvSource {
    /// The canonical `LIGUS_` name.
    Canonical,
    /// The legacy `HIVE_` name.
    Legacy,
    /// The name has no product prefix and was read as-is.
    Plain,
}

/// The result of resolving one variable against both spellings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub value: OsString,
    pub source: EnvSource,
    /// The canonical name (or the plain name when unprefixed).
    pub canonical: String,
    /// The legacy name, when the variable has one.
    pub legacy: Option<String>,
    /// Both spellings were set, non-empty and different (the canonical won).
    pub conflict: bool,
}

/// `(canonical, legacy)` names for `name`, or `None` when `name` carries
/// neither product prefix.
pub fn env_names(name: &str) -> Option<(String, String)> {
    let suffix = name.strip_prefix(ENV_PREFIX).or_else(|| name.strip_prefix(LEGACY_ENV_PREFIX))?;
    Some((format!("{ENV_PREFIX}{suffix}"), format!("{LEGACY_ENV_PREFIX}{suffix}")))
}

/// Resolve `name` with an injected lookup (pure; used by tests and by
/// [`var_os`]). A canonical value that is set but empty yields to a non-empty
/// legacy value, so an exported-but-blank `LIGUS_X=` doesn't mask `HIVE_X`.
pub fn resolve_with(name: &str, lookup: impl Fn(&str) -> Option<OsString>) -> Option<Resolved> {
    let Some((canonical, legacy)) = env_names(name) else {
        return lookup(name).map(|value| Resolved {
            value,
            source: EnvSource::Plain,
            canonical: name.to_string(),
            legacy: None,
            conflict: false,
        });
    };
    let new = lookup(&canonical);
    let old = lookup(&legacy);
    let filled = |v: &Option<OsString>| v.as_ref().is_some_and(|s| !s.is_empty());
    let (value, source, conflict) = if filled(&new) {
        let conflict = filled(&old) && old != new;
        (new?, EnvSource::Canonical, conflict)
    } else if filled(&old) {
        (old?, EnvSource::Legacy, false)
    } else if let Some(n) = new {
        (n, EnvSource::Canonical, false)
    } else {
        (old?, EnvSource::Legacy, false)
    };
    Some(Resolved { value, source, canonical, legacy: Some(legacy), conflict })
}

static WARNED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// True the first time `key` is seen (process-wide).
fn first_time(key: &str) -> bool {
    let mut guard = WARNED.lock().unwrap_or_else(|e| e.into_inner());
    guard.get_or_insert_with(HashSet::new).insert(key.to_string())
}

/// The one-line notes to print for `r`, if any (pure; tested).
pub fn warnings_for(r: &Resolved, warn_legacy: bool) -> Vec<String> {
    let mut out = Vec::new();
    let Some(legacy) = r.legacy.as_deref() else { return out };
    if r.conflict {
        out.push(format!("warning: {} and {legacy} are both set and differ; using {}", r.canonical, r.canonical));
    }
    if warn_legacy && r.source == EnvSource::Legacy {
        out.push(format!(
            "note: {legacy} is a legacy name; rename it to {} ({LEGACY_ENV_PREFIX}* names stop working in a future release)",
            r.canonical
        ));
    }
    out
}

fn report(r: &Resolved) {
    for line in warnings_for(r, WARN_ON_LEGACY_ENV) {
        if first_time(&line) {
            eprintln!("{line}");
        }
    }
}

/// Like `std::env::var_os`, resolving `LIGUS_X` then `HIVE_X`.
pub fn var_os(name: &str) -> Option<OsString> {
    let r = resolve_with(name, |n| std::env::var_os(n))?;
    report(&r);
    Some(r.value)
}

/// Like `std::env::var`, resolving `LIGUS_X` then `HIVE_X`.
pub fn var(name: &str) -> Result<String, VarError> {
    match var_os(name) {
        Some(v) => v.into_string().map_err(VarError::NotUnicode),
        None => Err(VarError::NotPresent),
    }
}

/// The variable's value when set and not blank.
pub fn env_opt(name: &str) -> Option<String> {
    var(name).ok().filter(|s| !s.trim().is_empty())
}

/// Whether either spelling of the variable is set (any value).
pub fn is_set(name: &str) -> bool {
    var_os(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let m: HashMap<String, OsString> = pairs.iter().map(|(k, v)| (k.to_string(), OsString::from(v))).collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn names_accept_either_prefix() {
        let want = Some(("LIGUS_DATA_DIR".to_string(), "HIVE_DATA_DIR".to_string()));
        assert_eq!(env_names("HIVE_DATA_DIR"), want);
        assert_eq!(env_names("LIGUS_DATA_DIR"), want);
        assert_eq!(env_names("HOME"), None);
    }

    #[test]
    fn canonical_wins_then_legacy_falls_back() {
        let r = resolve_with("HIVE_X", env(&[("LIGUS_X", "new"), ("HIVE_X", "new")])).unwrap();
        assert_eq!((r.value.to_str().unwrap(), r.source, r.conflict), ("new", EnvSource::Canonical, false));

        let r = resolve_with("LIGUS_X", env(&[("HIVE_X", "old")])).unwrap();
        assert_eq!((r.value.to_str().unwrap(), r.source), ("old", EnvSource::Legacy));

        let r = resolve_with("HIVE_X", env(&[("LIGUS_X", "new")])).unwrap();
        assert_eq!((r.value.to_str().unwrap(), r.source), ("new", EnvSource::Canonical));

        assert!(resolve_with("HIVE_X", env(&[])).is_none());
    }

    #[test]
    fn conflicting_values_use_canonical_and_flag_it() {
        let r = resolve_with("HIVE_X", env(&[("LIGUS_X", "new"), ("HIVE_X", "old")])).unwrap();
        assert_eq!(r.value.to_str().unwrap(), "new");
        assert!(r.conflict);
        let w = warnings_for(&r, false);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("LIGUS_X and HIVE_X are both set and differ; using LIGUS_X"), "{w:?}");
    }

    #[test]
    fn blank_canonical_does_not_mask_legacy() {
        let r = resolve_with("HIVE_X", env(&[("LIGUS_X", ""), ("HIVE_X", "old")])).unwrap();
        assert_eq!((r.value.to_str().unwrap(), r.source), ("old", EnvSource::Legacy));
        // Set-but-empty with nothing to fall back to stays set (empty).
        let r = resolve_with("HIVE_X", env(&[("LIGUS_X", "")])).unwrap();
        assert_eq!((r.value.to_str().unwrap(), r.source), ("", EnvSource::Canonical));
    }

    #[test]
    fn unprefixed_names_read_as_is() {
        let r = resolve_with("HOME", env(&[("HOME", "/h")])).unwrap();
        assert_eq!((r.value.to_str().unwrap(), r.source, r.legacy.clone()), ("/h", EnvSource::Plain, None));
        assert!(warnings_for(&r, true).is_empty());
    }

    #[test]
    fn legacy_note_only_when_enabled() {
        let r = resolve_with("HIVE_X", env(&[("HIVE_X", "old")])).unwrap();
        assert!(warnings_for(&r, false).is_empty());
        let w = warnings_for(&r, true);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("HIVE_X is a legacy name; rename it to LIGUS_X"), "{w:?}");
    }

    #[test]
    fn warnings_print_once_per_line() {
        assert!(first_time("compat-test-unique-line"));
        assert!(!first_time("compat-test-unique-line"));
    }

    #[test]
    fn process_env_round_trip() {
        // Unique names so parallel tests can't collide.
        std::env::set_var("HIVE_COMPAT_RT_A", "old");
        assert_eq!(var("HIVE_COMPAT_RT_A").as_deref(), Ok("old"));
        assert_eq!(var("LIGUS_COMPAT_RT_A").as_deref(), Ok("old"));
        std::env::set_var("LIGUS_COMPAT_RT_A", "new");
        assert_eq!(var("HIVE_COMPAT_RT_A").as_deref(), Ok("new"));
        std::env::remove_var("LIGUS_COMPAT_RT_A");
        std::env::remove_var("HIVE_COMPAT_RT_A");
        assert_eq!(var("HIVE_COMPAT_RT_A"), Err(VarError::NotPresent));
        assert_eq!(env_opt("HIVE_COMPAT_RT_A"), None);
    }
}

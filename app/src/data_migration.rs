//! First-launch copy of the desktop data dir across a bundle-identifier change
//! (`com.hive.desktop` -> `ai.ligus.desktop`).
//!
//! The OS app-data dir is named after the bundle id, so a new id starts with an
//! empty dir. Before anything opens the database, [`migrate`] copies the legacy
//! dir into the new location: it copies into a temporary sibling, fsyncs every
//! file, then renames it into place, so a crash leaves either no new dir or a
//! complete one. The legacy dir is never modified or deleted (a rollback to the
//! old app still finds its data). The identity files (`identity.json`, `keys/`)
//! live in the data dir, so the device keeps its identity: no re-enrollment.
//!
//! Dormant while the app's identifier is still the legacy one (new == legacy).

use std::io;
use std::path::{Path, PathBuf};

/// Written into the new data dir after a migration; holds the legacy path.
pub const MIGRATED_FROM_FILE: &str = "MIGRATED_FROM";
/// Files whose presence means a dir holds real app data.
const DATA_MARKERS: [&str; 2] = ["hive.db", "identity.json"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The app still runs under the legacy identifier.
    SameDir,
    /// The new dir already has data (a later launch, or a fresh install that
    /// already ran): nothing to do.
    AlreadyPresent,
    /// No legacy data on this machine.
    NoLegacy,
    /// The legacy dir was copied into the new one.
    Migrated { from: PathBuf },
}

fn has_data(dir: &Path) -> bool {
    DATA_MARKERS.iter().any(|m| dir.join(m).exists())
}

/// The legacy data dir that sits next to `new_dir` (same OS base dir).
pub fn legacy_sibling(new_dir: &Path) -> Option<PathBuf> {
    Some(new_dir.parent()?.join(hive_core::compat::LEGACY_DESKTOP_APP_ID))
}

/// Copy `legacy` into `new_dir` if `new_dir` has no data yet and `legacy` does.
/// On error nothing usable is left half-written at `new_dir`, and the caller
/// must refuse to start (starting empty would look like data loss and mint a
/// new device identity).
pub fn migrate(new_dir: &Path, legacy: &Path) -> Result<Outcome, String> {
    if same_path(new_dir, legacy) {
        return Ok(Outcome::SameDir);
    }
    if has_data(new_dir) {
        return Ok(Outcome::AlreadyPresent);
    }
    if !legacy.is_dir() || !has_data(legacy) {
        return Ok(Outcome::NoLegacy);
    }
    let parent = new_dir.parent().ok_or_else(|| format!("{} has no parent dir", new_dir.display()))?;
    let name = new_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let staging = parent.join(format!(".{name}.migrating-{}", std::process::id()));
    let fail = |what: &str, e: io::Error| format!("{what}: {e}");

    let _ = std::fs::remove_dir_all(&staging);
    if let Err(e) = copy_tree(legacy, &staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(fail(&format!("copying {} to {}", legacy.display(), staging.display()), e));
    }
    if let Err(e) = write_synced(&staging.join(MIGRATED_FROM_FILE), legacy.display().to_string().as_bytes()) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(fail("writing the migration marker", e));
    }

    // The new dir may already exist without data (the OS or webview created
    // it). Move it aside rather than merging into it, so the rename stays atomic.
    let mut aside = None;
    if new_dir.exists() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let to = parent.join(format!(".{name}.pre-migration-{stamp}"));
        if let Err(e) = std::fs::rename(new_dir, &to) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(fail(&format!("moving the empty {} aside", new_dir.display()), e));
        }
        aside = Some(to);
    }
    if let Err(e) = std::fs::rename(&staging, new_dir) {
        if let Some(to) = &aside {
            let _ = std::fs::rename(to, new_dir);
        }
        let _ = std::fs::remove_dir_all(&staging);
        return Err(fail(&format!("renaming the copy into {}", new_dir.display()), e));
    }
    sync_dir(parent);
    Ok(Outcome::Migrated { from: legacy.to_path_buf() })
}

fn same_path(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// Recursive copy of regular files and dirs (symlinks are copied as the files
/// they point to; anything else is skipped). Every file is fsynced.
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let meta = std::fs::metadata(&src)?;
        if meta.is_dir() {
            copy_tree(&src, &dst)?;
        } else if meta.is_file() {
            std::fs::copy(&src, &dst)?;
            std::fs::File::open(&dst)?.sync_all()?;
        }
    }
    sync_dir(to);
    Ok(())
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// Best-effort directory fsync (not supported on Windows).
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp dir removed on drop (the app has no tempfile dependency).
    struct Tmp(PathBuf);
    fn tempdir() -> Tmp {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("data-migration-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    impl Tmp {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn legacy_fixture(base: &Path) -> PathBuf {
        let legacy = base.join(hive_core::compat::LEGACY_DESKTOP_APP_ID);
        std::fs::create_dir_all(legacy.join("keys")).unwrap();
        std::fs::write(legacy.join("hive.db"), b"sqlite bytes").unwrap();
        std::fs::write(legacy.join("identity.json"), b"{\"deviceId\":\"d1\"}").unwrap();
        std::fs::write(legacy.join("keys").join("device.key"), b"secret").unwrap();
        std::fs::write(legacy.join("settings.json"), b"{}").unwrap();
        legacy
    }

    fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(root, &p, out);
                } else {
                    let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                    out.push((rel, std::fs::read(&p).unwrap()));
                }
            }
        }
        walk(dir, dir, &mut out);
        out.sort();
        out
    }

    #[test]
    fn copies_legacy_dir_and_leaves_it_untouched() {
        let tmp = tempdir();
        let legacy = legacy_fixture(tmp.path());
        let before = snapshot(&legacy);
        let new_dir = tmp.path().join(hive_core::compat::DESKTOP_APP_ID);
        assert_eq!(legacy_sibling(&new_dir).unwrap(), legacy);

        let out = migrate(&new_dir, &legacy).unwrap();
        assert_eq!(out, Outcome::Migrated { from: legacy.clone() });
        // Same identity and keys: no re-enrollment.
        assert_eq!(std::fs::read(new_dir.join("identity.json")).unwrap(), b"{\"deviceId\":\"d1\"}");
        assert_eq!(std::fs::read(new_dir.join("keys/device.key")).unwrap(), b"secret");
        assert_eq!(std::fs::read(new_dir.join("hive.db")).unwrap(), b"sqlite bytes");
        assert_eq!(std::fs::read_to_string(new_dir.join(MIGRATED_FROM_FILE)).unwrap(), legacy.display().to_string());
        // Legacy dir byte-for-byte unchanged, and no staging leftovers.
        assert_eq!(snapshot(&legacy), before);
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("migrating"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn is_idempotent_and_never_overwrites_new_data() {
        let tmp = tempdir();
        let legacy = legacy_fixture(tmp.path());
        let new_dir = tmp.path().join("ai.ligus.desktop");
        assert!(matches!(migrate(&new_dir, &legacy).unwrap(), Outcome::Migrated { .. }));
        std::fs::write(new_dir.join("hive.db"), b"newer events").unwrap();
        std::fs::write(legacy.join("hive.db"), b"changed by an old build").unwrap();
        assert_eq!(migrate(&new_dir, &legacy).unwrap(), Outcome::AlreadyPresent);
        assert_eq!(std::fs::read(new_dir.join("hive.db")).unwrap(), b"newer events");
    }

    #[test]
    fn dormant_under_the_legacy_id_and_without_legacy_data() {
        let tmp = tempdir();
        let legacy = legacy_fixture(tmp.path());
        assert_eq!(migrate(&legacy, &legacy).unwrap(), Outcome::SameDir);
        let fresh = tempdir();
        let new_dir = fresh.path().join("ai.ligus.desktop");
        let missing = fresh.path().join("com.hive.desktop");
        assert_eq!(migrate(&new_dir, &missing).unwrap(), Outcome::NoLegacy);
        std::fs::create_dir_all(&missing).unwrap();
        assert_eq!(migrate(&new_dir, &missing).unwrap(), Outcome::NoLegacy, "an empty legacy dir is not data");
        assert!(!new_dir.exists());
    }

    #[test]
    fn an_existing_empty_new_dir_is_moved_aside_not_merged() {
        let tmp = tempdir();
        let legacy = legacy_fixture(tmp.path());
        let new_dir = tmp.path().join("ai.ligus.desktop");
        std::fs::create_dir_all(new_dir.join("EBWebView")).unwrap();
        assert!(matches!(migrate(&new_dir, &legacy).unwrap(), Outcome::Migrated { .. }));
        assert!(new_dir.join("hive.db").exists());
        assert!(!new_dir.join("EBWebView").exists());
        let aside = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains("pre-migration"));
        assert!(aside, "the pre-existing dir is kept, renamed aside");
    }

    #[cfg(unix)]
    #[test]
    fn copy_failure_errors_and_leaves_no_new_dir() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempdir();
        let legacy = legacy_fixture(tmp.path());
        let unreadable = legacy.join("keys").join("device.key");
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&unreadable).is_ok() {
            // Running as root: permissions don't block reads; nothing to test.
            return;
        }
        let new_dir = tmp.path().join("ai.ligus.desktop");
        let err = migrate(&new_dir, &legacy).unwrap_err();
        assert!(err.contains("copying"), "{err}");
        assert!(!new_dir.exists(), "no half-copied data dir");
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(legacy.join("hive.db").exists());
    }
}

//! Owner-only files written atomically.
//!
//! Ligus keeps secrets in a few plain files on the data dir (`settings.json`
//! carries provider keys and OAuth tokens). They must never be readable by
//! other users, and a crash mid-write must never leave a truncated file
//! behind: [`write_private_atomic`] writes a sibling temp file created 0600,
//! fsyncs it, and renames it over the target.

use std::io::Write;
use std::path::Path;

/// Write `bytes` to `path` with owner-only permissions (0600 on Unix),
/// atomically: readers see the old file or the new one, never a partial one.
/// An existing file's looser mode is tightened too.
pub fn write_private_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_private_atomic_with(path, bytes, getrandom::fill)
}

/// [`write_private_atomic`] with the randomness source for the temp name.
fn write_private_atomic_with(path: &Path, bytes: &[u8], fill: fn(&mut [u8]) -> Result<(), getrandom::Error>) -> std::io::Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut rnd = [0u8; 6];
    fill(&mut rnd).map_err(|e| std::io::Error::other(format!("no OS randomness for a temp file name: {e}")))?;
    let suffix: String = rnd.iter().map(|b| format!("{b:02x}")).collect();
    let tmp = dir.join(format!(".{name}.{suffix}.tmp"));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_owner_only_and_replaces_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(&p, b"old").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        write_private_atomic(&p, b"{\"k\":1}").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"{\"k\":1}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // No temp files left behind.
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "{names:?}");
    }

    /// No OS randomness: the write fails instead of using a fixed temp name
    /// (which another local user could pre-create or race), and nothing is
    /// written.
    #[test]
    fn refuses_to_write_without_randomness() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        let err = write_private_atomic_with(&p, b"secret", |_| Err(getrandom::Error::UNSUPPORTED)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Other, "{err}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}

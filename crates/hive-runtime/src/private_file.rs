//! Owner-only files written atomically.
//!
//! Ligus keeps secrets in a few plain files on the data dir (`settings.json`
//! carries provider keys and OAuth tokens). They must never be readable by
//! other users, and a crash mid-write must never leave a truncated file
//! behind: [`write_private_atomic`] writes a sibling temp file created 0600,
//! fsyncs it, and renames it over the target.
//!
//! Directories that hold them are made owner-only by [`ensure_private_dir`],
//! which tightens directories Ligus owns to 0700 and never chmods one it
//! doesn't (a volume's mount root, see there).

use std::io::Write;
use std::path::Path;

/// Names the root of a volume that only this pod (or container) mounts, so a
/// data dir there may stay as the storage driver made it (Kubernetes
/// `local-path` makes volume roots `0777`, root-owned). The Helm chart sets
/// it to its data volume's mount path. Only that exact directory is trusted;
/// everything Ligus creates under it is still its own and 0700.
pub const POD_VOLUME_ENV: &str = "LIGUS_POD_VOLUME";

/// Create `dir` (and missing parents, 0700) and make sure only Ligus's user
/// can use it.
///
/// * **Ligus owns it**: tightened to 0700 if group or others have any access.
/// * **Another user owns it** (typically root: a Kubernetes volume's mount
///   root, a Docker bind mount, a systemd `StateDirectory` made by an
///   administrator): it is never chmodded (the kernel refuses, and it isn't
///   ours to change). It is accepted when it is root's and others (not group)
///   have no access: under a pod's `fsGroup` the kubelet makes the root
///   `root:<fsGroup>` with group access, and that group is the pod's own, so
///   group access is the pod and nobody else. A world-writable directory would
///   let any local user replace Ligus's files, and a world-readable one would
///   expose files that are not 0600 (`hive.db`), so either is refused with
///   what to do; so is a directory owned by any other non-root user, who
///   could change its mode at will. The exception is the directory named by
///   [`POD_VOLUME_ENV`]: a volume root only this pod mounts.
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let pod_volume = std::env::var_os(POD_VOLUME_ENV).filter(|v| !v.is_empty()).map(std::path::PathBuf::from);
        ensure_private_dir_with(dir, &UnixDirs, pod_volume.as_deref())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// A directory's owner and permission bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirFacts {
    pub owner: u32,
    pub mode: u32,
}

/// The filesystem calls [`ensure_private_dir`] makes (a seam for tests: a
/// test can't make a root-owned directory).
pub trait DirOps {
    /// Create `dir` and missing parents, new ones 0700.
    fn create(&self, dir: &Path) -> std::io::Result<()>;
    fn facts(&self, dir: &Path) -> std::io::Result<DirFacts>;
    fn chmod_owner_only(&self, dir: &Path) -> std::io::Result<()>;
    /// This process's effective uid.
    fn euid(&self) -> u32;
}

/// The real filesystem.
#[cfg(unix)]
pub struct UnixDirs;

#[cfg(unix)]
impl DirOps for UnixDirs {
    fn create(&self, dir: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
    }
    fn facts(&self, dir: &Path) -> std::io::Result<DirFacts> {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(dir)?;
        if !m.is_dir() {
            return Err(std::io::Error::other(format!("{} is not a directory", dir.display())));
        }
        Ok(DirFacts { owner: m.uid(), mode: m.mode() & 0o7777 })
    }
    fn chmod_owner_only(&self, dir: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }
    fn euid(&self) -> u32 {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }
}

/// What [`ensure_private_dir`] does with a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirVerdict {
    /// Private enough as it is.
    Keep,
    /// Ours, but group or others have access: chmod 0700.
    Tighten,
    /// Not ours and not private: refuse, saying what to do.
    Refuse(String),
}

/// The decision, without touching the filesystem.
pub fn dir_verdict(dir: &Path, facts: DirFacts, euid: u32, pod_volume: bool) -> DirVerdict {
    if facts.owner == euid {
        return if facts.mode & 0o077 != 0 { DirVerdict::Tighten } else { DirVerdict::Keep };
    }
    if pod_volume {
        return DirVerdict::Keep;
    }
    let d = dir.display();
    let fix = format!(
        "make it Ligus's: `chown {euid} {d}` (Ligus then makes it 0700), or point LIGUS_DATA_DIR at a directory Ligus creates; \
         if it is the root of a volume only this pod or container mounts, set {POD_VOLUME_ENV}={d}"
    );
    if facts.owner != 0 {
        return DirVerdict::Refuse(format!(
            "{d} belongs to uid {}, not to Ligus's uid {euid}, so that user controls who can read it; {fix}",
            facts.owner
        ));
    }
    if facts.mode & 0o002 != 0 {
        return DirVerdict::Refuse(format!(
            "{d} is writable by every user (mode {:04o}) and isn't Ligus's to tighten (owner root), so anyone could replace Ligus's files; \
             `chmod o-rwx {d}` (keeping group access for Ligus's group), or {fix}",
            facts.mode
        ));
    }
    if facts.mode & 0o005 != 0 {
        return DirVerdict::Refuse(format!(
            "{d} is open to every user (mode {:04o}) and isn't Ligus's to tighten (owner root), so others could read Ligus's database; \
             `chmod o-rwx {d}` (keeping group access for Ligus's group), or {fix}",
            facts.mode
        ));
    }
    DirVerdict::Keep
}

/// [`ensure_private_dir`] over `ops`, trusting `pod_volume` (if any) as a
/// volume root only this pod mounts.
pub fn ensure_private_dir_with(dir: &Path, ops: &impl DirOps, pod_volume: Option<&Path>) -> std::io::Result<()> {
    ops.create(dir)?;
    let facts = ops.facts(dir)?;
    let trusted = pod_volume.is_some_and(|v| same_dir(v, dir));
    match dir_verdict(dir, facts, ops.euid(), trusted) {
        DirVerdict::Keep => Ok(()),
        DirVerdict::Tighten => ops.chmod_owner_only(dir),
        DirVerdict::Refuse(why) => Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, why)),
    }
}

/// Whether `a` and `b` name the same directory (lexically after resolving,
/// so `/var/lib/ligus/` matches `/var/lib/ligus`).
fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a.components().eq(b.components()),
    }
}

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
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;

    /// A directory Ligus can't chmod (the kernel would say EPERM), as a test
    /// can't make a root-owned one.
    struct FakeDirs {
        facts: Cell<DirFacts>,
        euid: u32,
        chmods: RefCell<Vec<PathBuf>>,
    }

    impl FakeDirs {
        fn new(owner: u32, mode: u32, euid: u32) -> Self {
            Self { facts: Cell::new(DirFacts { owner, mode }), euid, chmods: RefCell::new(Vec::new()) }
        }
    }

    impl DirOps for FakeDirs {
        fn create(&self, _: &Path) -> std::io::Result<()> {
            Ok(())
        }
        fn facts(&self, _: &Path) -> std::io::Result<DirFacts> {
            Ok(self.facts.get())
        }
        fn chmod_owner_only(&self, dir: &Path) -> std::io::Result<()> {
            if self.facts.get().owner != self.euid {
                return Err(std::io::Error::from_raw_os_error(1)); // EPERM, like the kernel
            }
            self.chmods.borrow_mut().push(dir.to_path_buf());
            self.facts.set(DirFacts { owner: self.euid, mode: 0o700 });
            Ok(())
        }
        fn euid(&self) -> u32 {
            self.euid
        }
    }

    const ME: u32 = 65532;
    const DATA: &str = "/var/lib/ligus";

    /// The kubelet's mount root under fsGroup (`root:<fsGroup>`, setgid,
    /// group rwx, no access for others): kept as is, never chmodded.
    #[test]
    fn root_owned_group_only_mount_root_is_kept_not_chmodded() {
        let fs = FakeDirs::new(0, 0o2770, ME);
        ensure_private_dir_with(Path::new(DATA), &fs, None).unwrap();
        assert!(fs.chmods.borrow().is_empty());
        let fs = FakeDirs::new(0, 0o700, ME);
        ensure_private_dir_with(Path::new(DATA), &fs, None).unwrap();
    }

    /// The CI failure: a root-owned mount root Ligus can't chmod no longer
    /// fails with EPERM. World-readable or world-writable, it is refused
    /// with what to do, unless it is the declared pod volume.
    #[test]
    fn root_owned_world_accessible_mount_root_is_refused_with_a_fix_unless_declared() {
        for mode in [0o777, 0o2775, 0o755, 0o711, 0o1777] {
            let fs = FakeDirs::new(0, mode, ME);
            let err = ensure_private_dir_with(Path::new(DATA), &fs, None).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{mode:o}: {err}");
            let msg = err.to_string();
            assert!(msg.contains("chmod o-rwx /var/lib/ligus"), "{msg}");
            assert!(msg.contains("chown 65532 /var/lib/ligus"), "{msg}");
            assert!(msg.contains("LIGUS_POD_VOLUME=/var/lib/ligus"), "{msg}");
            if mode & 0o002 != 0 {
                assert!(msg.contains("writable by every user"), "{msg}");
            }
            assert!(fs.chmods.borrow().is_empty());
            // Declared as this pod's own volume (the chart): accepted, untouched.
            ensure_private_dir_with(Path::new(DATA), &fs, Some(Path::new("/var/lib/ligus/"))).unwrap();
            assert!(fs.chmods.borrow().is_empty());
        }
    }

    /// The declaration covers only that directory: a subdirectory someone
    /// else made inside the volume is still refused.
    #[test]
    fn pod_volume_trust_is_only_the_named_dir() {
        let fs = FakeDirs::new(1000, 0o777, ME);
        let err = ensure_private_dir_with(Path::new("/var/lib/ligus/secrets"), &fs, Some(Path::new(DATA))).unwrap_err();
        assert!(err.to_string().contains("belongs to uid 1000"), "{err}");
    }

    /// Another non-root user's directory is refused even when private: its
    /// owner can open it up at any time.
    #[test]
    fn other_users_dir_is_refused() {
        let fs = FakeDirs::new(1000, 0o700, ME);
        let err = ensure_private_dir_with(Path::new(DATA), &fs, None).unwrap_err();
        assert!(err.to_string().contains("belongs to uid 1000, not to Ligus's uid 65532"), "{err}");
    }

    /// Ours: tightened to 0700 whenever group or others have any access.
    #[test]
    fn own_dir_is_tightened() {
        for mode in [0o755, 0o770, 0o701, 0o2770, 0o777] {
            let fs = FakeDirs::new(ME, mode, ME);
            ensure_private_dir_with(Path::new(DATA), &fs, None).unwrap();
            assert_eq!(fs.chmods.borrow().len(), 1, "{mode:o}");
            assert_eq!(fs.facts.get().mode, 0o700);
        }
        let fs = FakeDirs::new(ME, 0o700, ME);
        ensure_private_dir_with(Path::new(DATA), &fs, None).unwrap();
        assert!(fs.chmods.borrow().is_empty());
    }

    /// The real filesystem: a dir we own ends up 0700, new or loosened, and
    /// new parents are 0700 too.
    #[cfg(unix)]
    #[test]
    fn owned_dirs_end_up_0700_on_disk() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let data = tmp.path().join("data");
        std::fs::create_dir(&data).unwrap();
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&data).unwrap();
        assert_eq!(mode(&data), 0o700);
        let deep = data.join("secrets").join("cli");
        ensure_private_dir(&deep).unwrap();
        assert_eq!(mode(&data.join("secrets")), 0o700);
        assert_eq!(mode(&deep), 0o700);
    }

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

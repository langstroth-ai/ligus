//! Opening a file beneath a directory without following anything an agent
//! could have planted on the way (hardening of check-then-open races).
//!
//! The host serves an agent's file requests (ACP `fs/*`, the built-in file
//! tools, the CLI-home copy-back) in its own, unsandboxed process. Checking a
//! path and then opening it by name leaves a window in which the agent (or a
//! process it started) swaps a directory on the way for a symlink, and the
//! open then lands outside the turn's directory. Here every component is
//! opened relative to the previous one with `openat`: directories with
//! `O_DIRECTORY | O_NOFOLLOW`, the last component with `O_NOFOLLOW`, so a
//! symlink anywhere below the root fails the open instead of being followed.
//! A swap after a component was opened changes nothing: the walk holds its
//! descriptor.

/// How [`open_beneath`] opens the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Read,
    /// Create or truncate for writing; missing parent dirs are created.
    WriteTruncate,
}

#[cfg(unix)]
pub use imp::open_beneath;

/// Elsewhere (Windows): plain opens under the root, after the callers'
/// path checks (no agent sandbox runs write-capable turns there).
#[cfg(not(unix))]
pub fn open_beneath(
    root: &std::path::Path,
    rel: &std::path::Path,
    mode: Mode,
) -> std::io::Result<std::fs::File> {
    if rel.components().any(|c| {
        !matches!(
            c,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    }) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "the path must stay beneath its root",
        ));
    }
    let p = root.join(rel);
    match mode {
        Mode::Read => std::fs::File::open(p),
        Mode::WriteTruncate => {
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::File::create(p)
        }
    }
}

/// Read at most `max` bytes of `rel` beneath `root` (see [`open_beneath`]).
pub fn read_beneath(
    root: &std::path::Path,
    rel: &std::path::Path,
    max: u64,
) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    race_point(&root.join(rel));
    let f = open_beneath(root, rel, Mode::Read)?;
    if f.metadata()?.len() > max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file too large",
        ));
    }
    let mut buf = Vec::new();
    f.take(max + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file too large",
        ));
    }
    Ok(buf)
}

/// Create or replace `rel` beneath `root` with `bytes` (see [`open_beneath`]).
pub fn write_beneath(
    root: &std::path::Path,
    rel: &std::path::Path,
    bytes: &[u8],
) -> std::io::Result<()> {
    use std::io::Write;
    race_point(&root.join(rel));
    let mut f = open_beneath(root, rel, Mode::WriteTruncate)?;
    f.write_all(bytes)
}

#[cfg(unix)]
mod imp {
    use std::ffi::{CString, OsStr};
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Component, Path};

    use super::Mode;

    fn cstr(name: &OsStr) -> io::Result<CString> {
        CString::new(name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
    }

    fn last_os_error_or_symlink() -> io::Error {
        let e = io::Error::last_os_error();
        match e.raw_os_error() {
            // O_NOFOLLOW on a symlink: ELOOP (Linux, macOS), EMLINK (FreeBSD),
            // ENOTDIR with O_DIRECTORY on some systems.
            Some(libc::ELOOP) | Some(libc::EMLINK) => io::Error::new(
                io::ErrorKind::PermissionDenied,
                "a path component is a symbolic link",
            ),
            _ => e,
        }
    }

    fn openat(
        dir: &OwnedFd,
        name: &OsStr,
        flags: libc::c_int,
        mode: libc::mode_t,
    ) -> io::Result<OwnedFd> {
        let c = cstr(name)?;
        // SAFETY: `dir` is a valid descriptor and `c` a NUL-terminated name.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                c.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                mode as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(last_os_error_or_symlink());
        }
        // SAFETY: a fresh descriptor we own.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// The plain names of `rel`; anything else (`..`, a root, a prefix) is
    /// refused. `.` is skipped.
    fn names(rel: &Path) -> io::Result<Vec<&OsStr>> {
        let mut v = Vec::new();
        for c in rel.components() {
            match c {
                Component::Normal(n) => v.push(n),
                Component::CurDir => {}
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "the path must stay beneath its root",
                    ))
                }
            }
        }
        Ok(v)
    }

    /// Open `root` itself as a directory (not following a symlink at its last
    /// component).
    pub fn open_root(root: &Path) -> io::Result<OwnedFd> {
        let c = cstr(root.as_os_str())?;
        // SAFETY: a NUL-terminated path.
        let fd = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(last_os_error_or_symlink());
        }
        // SAFETY: a fresh descriptor we own.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Walk `dirs` beneath `root`, one `openat(O_DIRECTORY | O_NOFOLLOW)` per
    /// component; `create` makes missing ones (`0755`, umask applies).
    pub fn open_dirs(root: &OwnedFd, dirs: &[&OsStr], create: bool) -> io::Result<OwnedFd> {
        let mut cur = root.try_clone()?;
        for d in dirs {
            let next = match openat(&cur, d, libc::O_RDONLY | libc::O_DIRECTORY, 0) {
                Err(e) if create && e.kind() == io::ErrorKind::NotFound => {
                    let c = cstr(d)?;
                    // SAFETY: valid descriptor and name.
                    if unsafe { libc::mkdirat(cur.as_raw_fd(), c.as_ptr(), 0o755) } < 0 {
                        let e = io::Error::last_os_error();
                        if e.kind() != io::ErrorKind::AlreadyExists {
                            return Err(e);
                        }
                    }
                    openat(&cur, d, libc::O_RDONLY | libc::O_DIRECTORY, 0)?
                }
                r => r?,
            };
            cur = next;
        }
        Ok(cur)
    }

    /// Open `rel` (plain names only) beneath `root` without following a symlink
    /// at any component. The file is opened `O_NONBLOCK` (a FIFO never blocks
    /// the host) and must be a regular file.
    pub fn open_beneath(root: &Path, rel: &Path, mode: Mode) -> io::Result<File> {
        let parts = names(rel)?;
        let (last, dirs) = parts
            .split_last()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
        let rootfd = open_root(root)?;
        let dir = open_dirs(&rootfd, dirs, mode == Mode::WriteTruncate)?;
        let flags = match mode {
            Mode::Read => libc::O_RDONLY | libc::O_NONBLOCK,
            // No O_TRUNC here: a FIFO or device must be refused before anything
            // is truncated; the caller's file is truncated once it is known to be
            // a regular file.
            Mode::WriteTruncate => libc::O_WRONLY | libc::O_CREAT | libc::O_NONBLOCK,
        };
        let fd = openat(&dir, last, flags, 0o644)?;
        let f = File::from(fd);
        let meta = f.metadata()?;
        if !meta.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        if mode == Mode::WriteTruncate {
            f.set_len(0)?;
        }
        Ok(f)
    }
}

#[cfg(test)]
type Hook = (std::path::PathBuf, Box<dyn FnOnce() + Send>);

#[cfg(test)]
static RACE_HOOKS: std::sync::Mutex<Vec<Hook>> = std::sync::Mutex::new(Vec::new());

/// Test hook: `f` runs once, at the moment between the checks and the open
/// of a path beneath `under` (a test swaps a directory for a symlink there).
#[cfg(test)]
pub(crate) fn set_race_hook(under: &std::path::Path, f: impl FnOnce() + Send + 'static) {
    RACE_HOOKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((under.to_path_buf(), Box::new(f)));
}

/// Where a concurrent swap would land: after `path` was validated, before
/// it is opened.
pub(crate) fn race_point(_path: &std::path::Path) {
    #[cfg(test)]
    {
        let hook = {
            let mut hooks = RACE_HOOKS.lock().unwrap_or_else(|e| e.into_inner());
            hooks
                .iter()
                .position(|(u, _)| _path.starts_with(u))
                .map(|i| hooks.remove(i).1)
        };
        if let Some(f) = hook {
            f()
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::path::Path;

    #[test]
    fn opens_plain_paths_and_refuses_symlinks_anywhere() {
        let t = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(t.path()).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/b/f.txt"), "hi").unwrap();
        let mut s = String::new();
        open_beneath(&root, Path::new("a/b/f.txt"), Mode::Read)
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        assert_eq!(s, "hi");
        let mut w = open_beneath(&root, Path::new("new/dir/g.txt"), Mode::WriteTruncate).unwrap();
        w.write_all(b"x").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("new/dir/g.txt")).unwrap(),
            "x"
        );
        std::os::unix::fs::symlink(outside.path(), root.join("link")).unwrap();
        assert!(open_beneath(&root, Path::new("link/x.txt"), Mode::WriteTruncate).is_err());
        assert!(!outside.path().join("x.txt").exists());
        std::os::unix::fs::symlink(root.join("a/b/f.txt"), root.join("flink")).unwrap();
        assert!(open_beneath(&root, Path::new("flink"), Mode::Read).is_err());
        assert!(open_beneath(&root, Path::new("../x"), Mode::Read).is_err());
    }
}

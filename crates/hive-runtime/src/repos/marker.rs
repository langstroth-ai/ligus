//! One managing host per checkout (repos-and-checkouts 4.5, review F11).
//!
//! Two Ligus processes on one machine (the desktop app and `ligus serve`)
//! must never both drive `.ligus/worktrees/chat-<id>` and `hive/chat/<id>`
//! in one checkout: their hygiene sweeps would remove each other's trees.
//! The first host to manage a checkout writes `ligus-host` in the checkout's
//! **git common dir** (never the work tree, so a commit or a hostile repo
//! can't supply one), created exclusively so two hosts racing get one
//! winner. Another host sees a foreign marker and refuses; the fix is a
//! separate clone (`--reference-if-able … --dissociate`), or an explicit
//! take-over when the marker's host is gone.

use std::io::Write;
use std::path::{Path, PathBuf};

use uuid::Uuid;

/// The marker's file name inside the git common dir.
pub const MARKER: &str = "ligus-host";

/// Who manages a checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Manager {
    /// This host (it just claimed it, or already had).
    Me,
    /// Another host: its device id and label.
    Other { device_id: Uuid, label: String },
}

fn marker_path(dir: &Path) -> Option<PathBuf> {
    super::git::git_common_dir(dir).map(|c| c.join(MARKER))
}

fn parse(text: &str) -> Option<(Uuid, String)> {
    let mut lines = text.lines();
    let id = Uuid::parse_str(lines.next()?.trim()).ok()?;
    let label: String = lines.next().unwrap_or("").chars().filter(|c| !c.is_control()).take(80).collect();
    Some((id, label))
}

/// Who manages the checkout at `dir`, if anyone has claimed it.
pub fn manager_of(dir: &Path, me: Uuid) -> Option<Manager> {
    let text = std::fs::read_to_string(marker_path(dir)?).ok()?;
    let (id, label) = parse(&text)?;
    Some(if id == me { Manager::Me } else { Manager::Other { device_id: id, label } })
}

fn write_new(path: &Path, me: Uuid, label: &str) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    let label: String = label.chars().filter(|c| !c.is_control()).take(80).collect();
    f.write_all(format!("{me}\n{label}\n").as_bytes())?;
    f.sync_all()
}

/// The label of the other host that manages the checkout at `dir`, when
/// one does: work that doesn't claim a checkout (a chat without a repo in
/// the workspace's bound folder, the hygiene sweep) must leave such a
/// checkout alone, or two hosts would drive one set of chat worktrees and
/// branches (review residual 6). An unreadable marker counts as foreign.
pub fn managed_elsewhere(dir: &Path, me: Uuid) -> Option<String> {
    let path = marker_path(dir)?;
    let text = std::fs::read_to_string(path).ok()?;
    match parse(&text) {
        Some((id, _)) if id == me => None,
        Some((_, label)) if !label.is_empty() => Some(label),
        Some(_) => Some("another host".into()),
        None => Some("an unreadable marker".into()),
    }
}

/// Claim the checkout at `dir` for this host: `Me` when it was unclaimed
/// (exactly one of several racing hosts wins) or already ours; `Other` when
/// another host manages it. A marker that isn't readable as one (garbage)
/// is treated as foreign until someone takes over.
pub fn claim(dir: &Path, me: Uuid, label: &str) -> std::io::Result<Manager> {
    let path = marker_path(dir).ok_or_else(|| std::io::Error::other("not a git checkout"))?;
    match write_new(&path, me, label) {
        Ok(()) => Ok(Manager::Me),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            Ok(match parse(&text) {
                Some((id, _)) if id == me => Manager::Me,
                Some((id, label)) => Manager::Other { device_id: id, label },
                None => Manager::Other { device_id: Uuid::nil(), label: "an unreadable marker".into() },
            })
        }
        Err(e) => Err(e),
    }
}

/// Take a checkout over from a host that is gone (a re-enrolled device has a
/// new id, so its old marker would otherwise lock the checkout forever).
/// Only on a person's explicit action.
pub fn take_over(dir: &Path, me: Uuid, label: &str) -> std::io::Result<()> {
    let path = marker_path(dir).ok_or_else(|| std::io::Error::other("not a git checkout"))?;
    let tmp = path.with_extension(format!("{}.tmp", Uuid::new_v4().simple()));
    write_new(&tmp, me, label)?;
    std::fs::rename(&tmp, &path)
}

/// Give a checkout up (unbinding it on this host): only our own marker.
pub fn release(dir: &Path, me: Uuid) {
    if manager_of(dir, me) == Some(Manager::Me) {
        if let Some(p) = marker_path(dir) {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Holds the checkout's git operations for this process (an exclusive
/// advisory lock on `ligus-host.lock` next to the marker), so this host's
/// own threads (a turn cutting a worktree, an apply, a hygiene sweep) never
/// interleave ref updates. Released on drop.
pub struct ManageLock {
    _file: std::fs::File,
}

/// Lock the checkout for a git operation, waiting at most `wait`.
pub fn lock(dir: &Path, wait: std::time::Duration) -> std::io::Result<ManageLock> {
    let path = marker_path(dir).ok_or_else(|| std::io::Error::other("not a git checkout"))?.with_extension("lock");
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path)?;
    let start = std::time::Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(ManageLock { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) if start.elapsed() < wait => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(std::fs::TryLockError::WouldBlock) => return Err(std::io::Error::other("the checkout is busy")),
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::git::testutil::*;
    use super::*;

    #[test]
    fn concurrent_marker_create_one_winner() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("a");
        repo_with_origin(&a, None);
        let ids: Vec<Uuid> = (0..8).map(|_| Uuid::new_v4()).collect();
        let wins: Vec<bool> = std::thread::scope(|s| {
            let hs: Vec<_> = ids.iter().map(|id| { let a = a.clone(); let id = *id; s.spawn(move || claim(&a, id, "h").unwrap() == Manager::Me) }).collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(wins.iter().filter(|w| **w).count(), 1);
        let winner = ids[wins.iter().position(|w| *w).unwrap()];
        assert_eq!(claim(&a, winner, "h").unwrap(), Manager::Me, "idempotent for the owner");
        let other = ids.iter().find(|i| **i != winner).unwrap();
        assert!(matches!(claim(&a, *other, "x").unwrap(), Manager::Other { device_id, .. } if device_id == winner));
        // Take-over is explicit and then sticks.
        take_over(&a, *other, "box").unwrap();
        assert_eq!(manager_of(&a, *other), Some(Manager::Me));
        release(&a, winner);
        assert_eq!(manager_of(&a, *other), Some(Manager::Me), "only the owner releases");
    }

    #[test]
    fn managed_elsewhere_names_the_other_host_only() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("a");
        repo_with_origin(&a, None);
        let (me, box_) = (Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(managed_elsewhere(&a, me), None, "unclaimed");
        claim(&a, me, "mac").unwrap();
        assert_eq!(managed_elsewhere(&a, me), None, "mine");
        take_over(&a, box_, "box").unwrap();
        assert_eq!(managed_elsewhere(&a, me).as_deref(), Some("box"));
        // A linked worktree of that checkout shares its common dir.
        assert_eq!(managed_elsewhere(&t.path().join("nope"), me), None, "not a checkout");
        std::fs::write(super::super::git::git_common_dir(&a).unwrap().join(MARKER), "garbage").unwrap();
        assert!(managed_elsewhere(&a, me).is_some(), "garbage is foreign");
    }

    #[test]
    fn marker_in_work_tree_ignored() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("a");
        repo_with_origin(&a, None);
        // A repo shipping its own marker in the work tree (and .ligus/host).
        let foreign = Uuid::new_v4();
        std::fs::write(a.join(MARKER), format!("{foreign}\nevil\n")).unwrap();
        std::fs::create_dir_all(a.join(".ligus")).unwrap();
        std::fs::write(a.join(".ligus/host"), format!("{foreign}\nevil\n")).unwrap();
        sh(&a, &["add", "-A"]);
        sh(&a, &["commit", "-qm", "ship a marker"]);
        let me = Uuid::new_v4();
        assert_eq!(manager_of(&a, me), None);
        assert_eq!(claim(&a, me, "me").unwrap(), Manager::Me);
    }

    #[test]
    fn the_lock_excludes_a_second_holder() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("a");
        repo_with_origin(&a, None);
        let held = lock(&a, std::time::Duration::from_millis(10)).unwrap();
        assert!(lock(&a, std::time::Duration::from_millis(100)).is_err());
        drop(held);
        assert!(lock(&a, std::time::Duration::from_millis(10)).is_ok());
    }
}

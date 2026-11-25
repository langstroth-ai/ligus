//! Scratch repositories on one host: creation, limits, keep, cleanup and
//! the path checks that keep a removal inside the scratch root.

use super::*;

const DAY_S: i64 = 86_400;

struct Env {
    t: tempfile::TempDir,
    store: EventStore,
    ws: Uuid,
    settings: Settings,
}

fn env() -> Env {
    let t = tempfile::tempdir().unwrap();
    let store = EventStore::open_in_memory().unwrap();
    let settings = settings(&store, &t.path().join("data").join("scratch"));
    Env { t, store, ws: Uuid::new_v4(), settings }
}

fn mk(e: &Env, name: &str, who: &str, now: i64) -> ScratchRepo {
    create(&e.store, &e.settings, &[], e.ws, name, who, now).unwrap()
}

#[test]
fn creates_an_empty_repo_on_main_under_the_root() {
    let e = env();
    let r = mk(&e, "Try a parser", "acct-1", 1_000);
    let root = super::super::git::canonical(&e.t.path().join("data/scratch"));
    assert_eq!(r.path.parent(), Some(root.as_path()));
    assert!(r.dir_name.starts_with("try-a-parser-") && r.dir_name.len() == "try-a-parser-".len() + 8, "{}", r.dir_name);
    assert_eq!(r.name, "Try a parser");
    assert_eq!(super::super::git::git(&r.path, &["symbolic-ref", "--short", "HEAD"]).unwrap(), "main");
    assert_eq!(super::super::git::git(&r.path, &["rev-list", "--count", "HEAD"]).unwrap(), "1");
    assert_eq!(super::super::git::git(&r.path, &["ls-tree", "HEAD"]).unwrap(), "", "the first commit is empty");
    assert_eq!(super::super::git::root_commit(&r.path).as_deref(), Some(r.root_commit.as_str()));
    assert!(has_marker(&r.path, r.id));
    assert_eq!(by_path(&e.store, &r.path).map(|x| x.id), Some(r.id));
    assert_eq!(live(&e.store, e.ws).len(), 1);
    assert!(live(&e.store, Uuid::new_v4()).is_empty(), "per workspace");
    // Two at once never share a root commit (their local: identity).
    let r2 = mk(&e, "Try a parser", "acct-1", 1_000);
    assert_ne!(r.root_commit, r2.root_commit);
    assert_ne!(r.dir_name, r2.dir_name);
    // No name: a folder name of its own.
    let r3 = mk(&e, "", "", 1_000);
    assert!(r3.dir_name.starts_with("scratch-") && r3.name == r3.dir_name);
}

#[test]
fn bad_names_and_roots_are_refused() {
    let e = env();
    assert!(matches!(create(&e.store, &e.settings, &[], e.ws, "We\u{202E}Fit", "", 0), Err(ScratchError::Invalid(_))));
    assert_eq!(slug("../../etc/passwd"), "etc-passwd");
    assert_eq!(slug("..."), "scratch");
    // A relative root, a root inside a checkout, a root in the desktop's
    // data dir, a symlinked root.
    assert!(check_root(Path::new("relative/scratch"), &[]).is_err());
    let repo = e.t.path().join("code");
    std::fs::create_dir_all(&repo).unwrap();
    super::super::git::git(&repo, &["init", "-q"]).unwrap();
    let err = check_root(&repo.join("scratch"), &[]).unwrap_err();
    assert!(err.to_string().contains("inside another repository"), "{err}");
    let desk = e.t.path().join("desktop-data");
    std::fs::create_dir_all(&desk).unwrap();
    assert!(check_root(&desk.join("scratch"), std::slice::from_ref(&desk)).is_err());
    assert!(check_root(&e.t.path().join("other"), std::slice::from_ref(&desk)).is_ok());
    #[cfg(unix)]
    {
        let real = e.t.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, e.t.path().join("link")).unwrap();
        assert!(check_root(&e.t.path().join("link"), &[]).is_err());
    }
    assert!(set_root(&e.store, Some(&repo.join("x")), &[]).is_err());
    assert!(settings(&e.store, Path::new("/d")).root_is_default, "a refused root is never stored");
}

#[test]
fn limits_per_host_and_per_member() {
    let e = env();
    set_limits(&e.store, Some(3), Some(2)).unwrap();
    let s = settings(&e.store, &e.settings.root);
    assert_eq!((s.max_per_host, s.max_per_member), (3, 2));
    create(&e.store, &s, &[], e.ws, "a", "ana", 0).unwrap();
    create(&e.store, &s, &[], e.ws, "b", "ana", 0).unwrap();
    let err = create(&e.store, &s, &[], e.ws, "c", "ana", 0).unwrap_err();
    assert_eq!(err.code(), "scratch_limit");
    create(&e.store, &s, &[], e.ws, "c", "ben", 0).unwrap();
    assert_eq!(create(&e.store, &s, &[], e.ws, "d", "cy", 0).unwrap_err().code(), "scratch_limit", "the host's cap");
    // Nothing was left behind by the refused ones.
    let dirs = std::fs::read_dir(&s.root).unwrap().count();
    assert_eq!(dirs, 3);
    assert!(set_limits(&e.store, Some(0), None).is_err());
    assert!(set_limits(&e.store, Some(HARD_MAX_PER_HOST + 1), None).is_err());
    // A removed one frees its place.
    let first = live(&e.store, e.ws)[0].clone();
    remove_dir_checked(&first).unwrap();
    mark_removed(&e.store, first.id, 1).unwrap();
    create(&e.store, &s, &[], e.ws, "d", "cy", 0).unwrap();
}

#[test]
fn cleanup_notices_then_removes_and_can_be_off() {
    let e = env();
    let t0 = super::super::scratch::now() - 30 * DAY_S;
    let r = mk(&e, "old", "", t0);
    // Git traces are "now" (just created): age them by recording old activity
    // only through the registry, so look at the expiry from a later now.
    let fresh = expiry(&r, &e.settings, t0).unwrap();
    assert_eq!(e.settings.cleanup_days, Some(DEFAULT_CLEANUP_DAYS));
    let act = activity(&r, now());
    assert!(act >= now() - 60, "the repo's own git dir counts as activity");
    assert!(fresh.remove_at >= t0 + 14 * DAY_S);
    let x = expiry(&r, &e.settings, act + 12 * DAY_S + 1).unwrap();
    assert!(x.notice, "{x:?}");
    assert_eq!(x.remove_at, act + 14 * DAY_S);
    assert_eq!(x.notice_from, act + 12 * DAY_S);
    let d = due(&e.store, e.ws, &e.settings, act + 12 * DAY_S + 1);
    assert_eq!(d.notice.len(), 1);
    assert!(d.remove.is_empty());
    assert!(notice_text(&d.notice[0].0, &d.notice[0].1, "box").contains("will be removed on"));
    mark_noticed(&e.store, r.id, act + 12 * DAY_S + 1).unwrap();
    assert!(due(&e.store, e.ws, &e.settings, act + 12 * DAY_S + 2).notice.is_empty(), "noticed once");
    // Activity resets the notice.
    assert!(touch(&e.store, &r.path, act + 13 * DAY_S));
    let r = by_path(&e.store, &r.path).unwrap();
    assert!(r.noticed_at.is_none());
    let later = act + 13 * DAY_S + 14 * DAY_S;
    let d = due(&e.store, e.ws, &e.settings, later);
    assert_eq!(d.remove.len(), 1);
    remove_dir_checked(&d.remove[0]).unwrap();
    mark_removed(&e.store, r.id, later).unwrap();
    assert!(!r.path.exists());
    assert!(live(&e.store, e.ws).is_empty());
    assert_eq!(list(&e.store, e.ws)[0].state, ScratchState::Removed);
    // Off: nothing is ever due.
    set_cleanup_days(&e.store, Some(0)).unwrap();
    let off = settings(&e.store, &e.settings.root);
    assert_eq!(off.cleanup_days, None);
    let r2 = create(&e.store, &off, &[], e.ws, "kept around", "", 0).unwrap();
    assert!(expiry(&r2, &off, i64::MAX / 2).is_none());
    assert!(due(&e.store, e.ws, &off, i64::MAX / 2).remove.is_empty());
    assert!(set_cleanup_days(&e.store, Some(MAX_CLEANUP_DAYS + 1)).is_err());
    set_cleanup_days(&e.store, Some(3)).unwrap();
    assert_eq!(settings(&e.store, &e.settings.root).cleanup_days, Some(3));
}

/// Path safety (security pass): a removal never leaves the scratch root,
/// never follows a symlink, and never touches a folder that isn't this
/// host's own scratch repository.
#[test]
fn removal_refuses_traversal_symlinks_and_foreign_folders() {
    let e = env();
    let victim = e.t.path().join("precious");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("keep.txt"), "x").unwrap();
    super::super::git::git(&victim, &["init", "-q"]).unwrap();

    // A traversal in the recorded name or path.
    let r = mk(&e, "t", "", 0);
    for bad in [
        ScratchRepo { dir_name: "..".into(), ..r.clone() },
        ScratchRepo { dir_name: "../precious".into(), ..r.clone() },
        ScratchRepo { path: victim.clone(), ..r.clone() },
        ScratchRepo { root: e.t.path().to_path_buf(), dir_name: "precious".into(), path: super::super::git::canonical(&victim), ..r.clone() },
    ] {
        assert!(remove_dir_checked(&bad).is_err(), "{bad:?}");
    }
    assert!(victim.join("keep.txt").exists());

    // Not live: refused.
    assert_eq!(remove_dir_checked(&ScratchRepo { state: ScratchState::Kept, ..r.clone() }).unwrap_err(), ScratchError::NotFound);

    // The marker must be this entry's own.
    let other = ScratchRepo { id: Uuid::new_v4(), ..r.clone() };
    assert!(remove_dir_checked(&other).is_err());
    assert!(r.path.exists());

    #[cfg(unix)]
    {
        // The scratch folder swapped for a symlink to a real checkout that
        // carries a copied marker: refused, the target untouched.
        let r2 = mk(&e, "swap", "", 0);
        let common = super::super::git::git_common_dir(&victim).unwrap();
        std::fs::write(common.join(MARKER), format!("{}\n", r2.id)).unwrap();
        std::fs::remove_dir_all(&r2.path).unwrap();
        std::os::unix::fs::symlink(&victim, &r2.path).unwrap();
        let err = remove_dir_checked(&r2).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
        assert!(victim.join("keep.txt").exists());

        // A symlink inside the scratch repo pointing out: removing the repo
        // removes the link, never its target.
        let r3 = mk(&e, "links", "", 0);
        std::os::unix::fs::symlink(&victim, r3.path.join("escape")).unwrap();
        remove_dir_checked(&r3).unwrap();
        assert!(!r3.path.exists());
        assert!(victim.join("keep.txt").exists(), "a symlink's target survives");

        // A .git that is a gitfile pointing at another repository's git dir.
        let r4 = mk(&e, "gitfile", "", 0);
        std::fs::remove_dir_all(r4.path.join(".git")).unwrap();
        std::fs::write(r4.path.join(".git"), format!("gitdir: {}\n", common.display())).unwrap();
        assert!(remove_dir_checked(&r4).is_err());
        assert!(r4.path.exists());
    }

    // The scratch root itself moved under a symlink after creation.
    let r5 = mk(&e, "rootmove", "", 0);
    let moved = ScratchRepo { root: e.t.path().join("data/./scratch"), ..r5.clone() };
    assert!(remove_dir_checked(&moved).is_err());
    remove_dir_checked(&r5).unwrap();
}

#[test]
fn keep_moves_it_out_with_its_worktrees() {
    let e = env();
    let r = mk(&e, "keep me", "", 0);
    // A chat worktree inside, like a turn leaves.
    crate::git_worktree::chat_worktree_at(&r.path, "chat-1", "HEAD").unwrap();
    let wt = crate::git_worktree::chat_worktree(&r.path, "chat-1").unwrap();
    std::fs::write(wt.join("a.txt"), "hello").unwrap();

    let home = e.t.path().join("projects");
    std::fs::create_dir_all(&home).unwrap();
    // Refused destinations: inside the root, an existing folder, inside a
    // checkout, a relative path, the desktop's data dir.
    assert!(keep_dir(&r, &r.root.join("x"), &[]).is_err());
    assert!(keep_dir(&r, &home, &[]).is_err(), "exists");
    assert!(keep_dir(&r, Path::new("rel/x"), &[]).is_err());
    let code = e.t.path().join("code");
    std::fs::create_dir_all(&code).unwrap();
    super::super::git::git(&code, &["init", "-q"]).unwrap();
    assert!(keep_dir(&r, &code.join("x"), &[]).is_err());
    assert!(keep_dir(&r, &home.join("x"), std::slice::from_ref(&home)).is_err());

    let dest = keep_dir(&r, &home.join("parser"), &[]).unwrap();
    assert!(!r.path.exists());
    assert_eq!(dest, super::super::git::canonical(&home.join("parser")));
    assert!(!has_marker(&dest, r.id), "no longer scratch");
    let moved_wt = dest.join(wt.strip_prefix(&r.path).unwrap());
    assert_eq!(std::fs::read_to_string(moved_wt.join("a.txt")).unwrap(), "hello");
    assert_eq!(super::super::git::git(&moved_wt, &["rev-parse", "--show-toplevel"]).map(PathBuf::from).map(|p| super::super::git::canonical(&p)).unwrap(), super::super::git::canonical(&moved_wt), "the worktree link was repaired");
    let k = mark_kept(&e.store, r.id, &dest).unwrap();
    assert_eq!(k.state, ScratchState::Kept);
    assert!(by_path(&e.store, &dest).is_none(), "kept ones aren't live scratch repos");
    // A kept repo is never due.
    assert!(due(&e.store, e.ws, &e.settings, i64::MAX / 2).remove.is_empty());
}

#[test]
fn chats_and_lookup() {
    let e = env();
    let r = mk(&e, "Lookup", "", 0);
    let c = Uuid::new_v4();
    set_chat(&e.store, r.id, c, true, 5).unwrap();
    set_chat(&e.store, r.id, c, true, 6).unwrap();
    assert_eq!(by_path(&e.store, &r.path).unwrap().chats, vec![c]);
    assert_eq!(find(&e.store, e.ws, &r.short_id()).map(|x| x.id), Some(r.id));
    assert_eq!(find(&e.store, e.ws, &r.dir_name).map(|x| x.id), Some(r.id));
    assert_eq!(find(&e.store, e.ws, "lookup").map(|x| x.id), Some(r.id));
    assert!(find(&e.store, e.ws, "abc").is_none(), "too short a prefix");
    forget_chat(&e.store, c).unwrap();
    assert!(by_path(&e.store, &r.path).unwrap().chats.is_empty());
    let rid = Uuid::new_v4();
    set_repo_id(&e.store, r.id, Some(rid)).unwrap();
    assert_eq!(by_repo(&e.store, rid).map(|x| x.id), Some(r.id));
    assert!(set_chat(&e.store, Uuid::new_v4(), c, true, 0).is_err());
}

/// Security pass: an agent can make part of its tree unwritable; the
/// removal still completes (the folder is gone at once, the trash is
/// retried until it is deleted), so it never sticks around holding a slot.
#[cfg(unix)]
#[test]
fn an_unwritable_tree_is_still_removed() {
    use std::os::unix::fs::PermissionsExt;
    let e = env();
    let r = mk(&e, "stuck", "", 0);
    let locked = r.path.join("x");
    std::fs::create_dir_all(locked.join("y")).unwrap();
    std::fs::write(locked.join("y/f"), "x").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
    remove_dir_checked(&r).unwrap();
    mark_removed(&e.store, r.id, 1).unwrap();
    assert!(!r.path.exists());
    assert!(live(&e.store, e.ws).is_empty(), "no live entry holds a slot");
    assert_eq!(purge_trash(&e.store), 0);
    assert!(pending_trash(&e.store).is_empty());
    assert_eq!(std::fs::read_dir(&r.root).unwrap().count(), 0, "no trash left behind");
}

#[cfg(unix)]
#[test]
fn a_root_anyone_may_write_to_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let e = env();
    let open = e.t.path().join("open");
    std::fs::create_dir_all(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(check_root(&open, &[]).is_err());
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(check_root(&open, &[]).is_ok());
}

#[test]
fn the_scratch_hint_shows_on_local_repos_only() {
    use ligus_core::workspace_repo::{repo_id, WorkspaceRepo};
    let c = format!("local:{}", "a".repeat(40));
    let local = WorkspaceRepo { id: repo_id(&c), canonical: c, display_name: "s".into(), forge_kind: String::new(), default_branch: "main".into(), root_commit: None, scratch: true };
    assert!(local.is_scratch());
    let real = WorkspaceRepo { id: repo_id("github.com/a/b"), canonical: "github.com/a/b".into(), ..local.clone() };
    assert!(!real.is_scratch(), "nobody labels a forge repository throwaway");
    // Additive on the wire: absent when false, absent fields decode as false.
    let plain = WorkspaceRepo { scratch: false, ..local.clone() };
    assert!(!serde_json::to_string(&plain).unwrap().contains("scratch"));
    let old: WorkspaceRepo = serde_json::from_str(&serde_json::to_string(&plain).unwrap()).unwrap();
    assert!(!old.scratch);
    assert!(serde_json::to_string(&local).unwrap().contains("\"scratch\":true"));
}

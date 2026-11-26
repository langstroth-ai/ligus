//! Scratch repositories on a box (`ligus serve`, a worker, a person's CLI):
//! the folder and its registry are `ligus_runtime::repos::scratch`; this
//! module binds them the way a box binds every checkout, device-locally in
//! `repo-bindings.toml` (a box is a Contributor service member and can't
//! add workspace repositories), and implements `ligus repo scratch` and
//! `ligus repo keep`.
//!
//! The scratch root is `LIGUS_SCRATCH_DIR`, else `<data dir>/scratch`, or
//! what `ligus repo scratch --set-root` stored; it is never inside another
//! repository or the desktop app's data folder.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use ligus_runtime::repos::scratch::{self, ScratchRepo, Settings};
use ligus_runtime::ChatService;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::repos::{repos_dir, resolve_path, RepoBindings};

/// `LIGUS_SCRATCH_DIR`, else `<data dir>/scratch`.
pub fn default_root(data_dir: &Path) -> PathBuf {
    crate::env_opt("LIGUS_SCRATCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join("scratch"))
}

/// Folders a box's scratch root (or a kept scratch repo) may never be in:
/// the desktop app's data dir.
pub fn forbidden() -> Vec<PathBuf> {
    let mut v = crate::setup_cmd::desktop_data_dirs();
    if let Some(d) = crate::env_opt("LIGUS_DESKTOP_DATA_DIR") {
        v.push(PathBuf::from(d));
    }
    v
}

pub fn settings(svc: &ChatService, data_dir: &Path) -> Settings {
    scratch::settings(svc.store(), &default_root(data_dir))
}

/// The phone's (and `--json`'s) view of one scratch repository: names and
/// dates, never a path.
pub fn dto(e: &ScratchRepo, settings: &Settings, now: i64) -> Value {
    let x = scratch::expiry(e, settings, now);
    json!({
        "id": e.id,
        "name": ligus_runtime::display::shown(&e.name),
        "checkout": e.dir_name,
        "state": e.state,
        "createdAt": iso(e.created_at),
        "lastActivity": iso(scratch::activity(e, now)),
        "removeAt": x.map(|x| iso(x.remove_at)),
        "noticeFrom": x.map(|x| iso(x.notice_from)),
        "notice": x.is_some_and(|x| x.notice),
        "chats": e.chats.len(),
        "registered": e.repo_id.is_some(),
        "published": e.published,
    })
}

/// The settings as the phone sees them (no paths).
pub fn settings_dto(s: &Settings, live: usize) -> Value {
    json!({
        "cleanupDays": s.cleanup_days,
        "noticeDays": scratch::NOTICE_DAYS,
        "maxPerHost": s.max_per_host,
        "maxPerMember": s.max_per_member,
        "live": live,
        "customRoot": !s.root_is_default,
    })
}

fn iso(unix: i64) -> String {
    let t =
        time::OffsetDateTime::from_unix_timestamp(unix).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Bind `chat` to scratch repo `e` on this box (its own binding).
pub fn bind_chat(data_dir: &Path, e: &ScratchRepo, chat: Uuid) -> Result<()> {
    let mut b = RepoBindings::load(data_dir);
    b.chats
        .insert(chat.to_string(), e.path.to_string_lossy().into_owned());
    b.save(data_dir)
}

/// The live scratch repository a chat's binding on this box points at.
pub fn of_chat(data_dir: &Path, svc: &ChatService, chat: Uuid) -> Option<ScratchRepo> {
    let b = RepoBindings::load(data_dir);
    let (raw, _) = b.raw_for(chat)?;
    let p = resolve_path(&repos_dir(data_dir), raw);
    scratch::by_path(svc.store(), &p)
}

/// Create a scratch repository on this box, bound to `chat` when given.
/// `created_by` is the asking person's account id (`""` for the operator).
pub fn create(
    data_dir: &Path,
    svc: &ChatService,
    ws: Uuid,
    chat: Option<Uuid>,
    name: &str,
    created_by: &str,
) -> std::result::Result<ScratchRepo, scratch::ScratchError> {
    let st = settings(svc, data_dir);
    scratch::reserve(svc.store(), &st, created_by)?;
    // Never the repos dir itself (or around it): keep moves into it.
    let mut no = forbidden();
    no.push(repos_dir(data_dir));
    let root = scratch::check_root(&st.root, &no)?;
    let now = scratch::now();
    let e = scratch::init_dir(&root, ws, name, created_by, now)?;
    if let Err(err) = scratch::record(svc.store(), &st, &e) {
        scratch::discard_dir(&e);
        return Err(err);
    }
    match chat {
        None => Ok(e),
        Some(c) => {
            bind_chat(data_dir, &e, c).map_err(|x| scratch::ScratchError::Store(x.to_string()))?;
            scratch::set_chat(svc.store(), e.id, c, true, now)
        }
    }
}

/// Clear every binding on this box that points at `path`; the chats it
/// unbound.
fn unbind_path(data_dir: &Path, path: &Path) -> Result<Vec<Uuid>> {
    let mut b = RepoBindings::load(data_dir);
    let dir = repos_dir(data_dir);
    let same = |raw: &str| {
        ligus_runtime::repos::git::canonical(&resolve_path(&dir, raw)) == path
            || resolve_path(&dir, raw) == path
    };
    let chats: Vec<Uuid> = b
        .chats
        .iter()
        .filter(|(_, r)| !r.trim().is_empty() && same(r))
        .filter_map(|(c, _)| Uuid::parse_str(c).ok())
        .collect();
    // "No repository" for each of them (not the workspace default): the
    // chat was working in this scratch repository, not in that one.
    for c in &chats {
        b.chats.insert(c.to_string(), String::new());
    }
    if b.workspace.as_deref().is_some_and(same) {
        b.workspace = None;
    }
    b.save(data_dir)?;
    Ok(chats)
}

/// Rewrite every binding that points at `from` to `to` (a name under the
/// repos dir, or an absolute path).
fn rebind_path(data_dir: &Path, from: &Path, to: &str) -> Result<()> {
    let mut b = RepoBindings::load(data_dir);
    let dir = repos_dir(data_dir);
    let same = |raw: &str| {
        resolve_path(&dir, raw) == from
            || ligus_runtime::repos::git::canonical(&resolve_path(&dir, raw)) == from
    };
    for v in b.chats.values_mut() {
        if same(v) {
            *v = to.to_string();
        }
    }
    if b.workspace.as_deref().is_some_and(same) {
        b.workspace = Some(to.to_string());
    }
    b.save(data_dir)
}

/// Where a kept scratch repository goes on this box by default: the repos
/// dir, under its folder name without the scratch id (or with it, when
/// that name is taken).
pub fn default_keep_dest(data_dir: &Path, e: &ScratchRepo, name: Option<&str>) -> PathBuf {
    let dir = repos_dir(data_dir);
    let base = scratch::slug(name.filter(|n| !n.trim().is_empty()).unwrap_or(&e.name));
    let plain = dir.join(&base);
    if std::fs::symlink_metadata(&plain).is_err() {
        plain
    } else {
        dir.join(format!("{base}-{}", e.short_id()))
    }
}

/// Keep a scratch repository on this box: move it to `dest` (default: the
/// repos dir) and point its chats' bindings there. Returns the kept entry.
pub fn keep(
    data_dir: &Path,
    svc: &ChatService,
    ws: Uuid,
    id: Uuid,
    dest: Option<PathBuf>,
    name: Option<&str>,
) -> std::result::Result<ScratchRepo, scratch::ScratchError> {
    let e = scratch::list(svc.store(), ws)
        .into_iter()
        .find(|x| x.id == id && x.is_live())
        .ok_or(scratch::ScratchError::NotFound)?;
    let dest = match dest {
        Some(d) => d,
        None => {
            let d = default_keep_dest(data_dir, &e, name);
            std::fs::create_dir_all(d.parent().unwrap_or(Path::new(".")))
                .map_err(|x| scratch::ScratchError::Io(x.to_string()))?;
            d
        }
    };
    let new_path = scratch::keep_dir(&e, &dest, &forbidden())?;
    let kept = scratch::mark_kept(svc.store(), id, &new_path)?;
    // Under the repos dir: bind by name, so the phone lists it as one of
    // this box's checkouts.
    let dir = ligus_runtime::repos::git::canonical(&repos_dir(data_dir));
    let raw = match new_path.strip_prefix(&dir) {
        Ok(rel) if rel.components().count() == 1 => rel.to_string_lossy().into_owned(),
        _ => new_path.to_string_lossy().into_owned(),
    };
    rebind_path(data_dir, &e.path, &raw)
        .map_err(|x| scratch::ScratchError::Store(x.to_string()))?;
    Ok(kept)
}

/// Remove a live scratch repository's folder on this box and leave its
/// chats here with no repository, each with a note.
pub fn remove(
    data_dir: &Path,
    svc: &mut ChatService,
    ws: Uuid,
    id: Uuid,
    host_label: &str,
    why: Option<&str>,
) -> std::result::Result<ScratchRepo, scratch::ScratchError> {
    let e = scratch::list(svc.store(), ws)
        .into_iter()
        .find(|x| x.id == id && x.is_live())
        .ok_or(scratch::ScratchError::NotFound)?;
    scratch::remove_dir_checked(&e)?;
    let removed = scratch::mark_removed(svc.store(), id, scratch::now())?;
    let mut chats =
        unbind_path(data_dir, &e.path).map_err(|x| scratch::ScratchError::Store(x.to_string()))?;
    for c in &e.chats {
        if !chats.contains(c) {
            chats.push(*c);
        }
    }
    let note = why
        .map(str::to_string)
        .unwrap_or_else(|| scratch::removed_text(&e, host_label));
    for c in chats {
        let _ = svc.post_system_note(c, ws, note.clone());
    }
    Ok(removed)
}

/// One cleanup pass on this box at `now`.
pub fn sweep(
    data_dir: &Path,
    svc: &mut ChatService,
    ws: Uuid,
    host_label: &str,
    now: i64,
) -> ligus_runtime::repos::scratch_host::SweepReport {
    let st = settings(svc, data_dir);
    let mut out = ligus_runtime::repos::scratch_host::SweepReport::default();
    let due = scratch::due(svc.store(), ws, &st, now);
    for (e, x) in due.notice {
        let text = scratch::notice_text(&e, &x, host_label);
        let b = RepoBindings::load(data_dir);
        let dir = repos_dir(data_dir);
        let mut chats: Vec<Uuid> = b
            .chats
            .iter()
            .filter(|(_, r)| resolve_path(&dir, r) == e.path)
            .filter_map(|(c, _)| Uuid::parse_str(c).ok())
            .collect();
        for c in &e.chats {
            if !chats.contains(c) {
                chats.push(*c);
            }
        }
        for c in chats {
            let _ = svc.post_system_note(c, ws, text.clone());
        }
        if scratch::mark_noticed(svc.store(), e.id, now).is_ok() {
            out.noticed.push(e.id);
        }
    }
    for e in due.remove {
        match remove(data_dir, svc, ws, e.id, host_label, None) {
            Ok(_) => out.removed.push(e.id),
            Err(err) => out.failed.push((e.id, err.to_string())),
        }
    }
    out
}

/// `ligus repo scratch …` and `ligus repo keep …` (the other repository
/// commands are `ligus repos …`).
pub fn cmd_repo(
    data_dir: &Path,
    svc: &mut ChatService,
    ws: Uuid,
    host_label: &str,
    pos: &[&str],
    flag: &dyn Fn(&str) -> Option<String>,
) -> Result<()> {
    match pos.first().copied() {
        Some("scratch") => cmd_scratch(data_dir, svc, ws, host_label, pos.get(1).copied(), flag),
        Some("keep") => cmd_keep(data_dir, svc, ws, host_label, pos.get(1).copied(), flag),
        _ => Err(crate::help::usage_error("usage: ligus repo scratch [--chat ID] | ligus repo keep <scratch> (ligus repos for the others)")),
    }
}

fn find(svc: &ChatService, ws: Uuid, sel: &str) -> Result<ScratchRepo> {
    scratch::find(svc.store(), ws, sel).ok_or_else(|| {
        anyhow!("no scratch repository on this host matches '{sel}' (ligus repo scratch --list)")
    })
}

/// `ligus repo scratch [--chat ID] [--name N] [--workspace] [--json]`,
/// `--list`, `--remove <scratch>`, `--set-root PATH | --reset-root`,
/// `--cleanup-days N` (0 = off), `--sweep`.
pub fn cmd_scratch(
    data_dir: &Path,
    svc: &mut ChatService,
    ws: Uuid,
    host_label: &str,
    target: Option<&str>,
    flag: &dyn Fn(&str) -> Option<String>,
) -> Result<()> {
    let json_out = flag("--json").is_some();
    let now = scratch::now();
    if let Some(root) = flag("--set-root").filter(|r| !r.is_empty()) {
        let p = crate::repos_cmd::abs(&root)?;
        let mut no = forbidden();
        no.push(repos_dir(data_dir));
        let c = scratch::set_root(svc.store(), Some(&p), &no).map_err(|e| anyhow!("{e}"))?;
        println!(
            "scratch root: {}",
            c.map(|c| c.display().to_string()).unwrap_or_default()
        );
        return Ok(());
    }
    if flag("--reset-root").is_some() {
        scratch::set_root(svc.store(), None, &[]).map_err(|e| anyhow!("{e}"))?;
        println!(
            "scratch root: {} (the default)",
            default_root(data_dir).display()
        );
        return Ok(());
    }
    if let Some(d) = flag("--cleanup-days").filter(|d| !d.is_empty()) {
        let n: u32 = d.trim().parse().map_err(|_| {
            crate::help::usage_error(
                "--cleanup-days takes a number of days (0 turns the cleanup off)",
            )
        })?;
        scratch::set_cleanup_days(svc.store(), Some(n)).map_err(|e| anyhow!("{e}"))?;
        println!(
            "{}",
            if n == 0 {
                "scratch cleanup: off".to_string()
            } else {
                format!("scratch cleanup: after {n} days without activity")
            }
        );
        return Ok(());
    }
    if flag("--sweep").is_some() {
        let r = sweep(data_dir, svc, ws, host_label, now);
        let r2 = ligus_runtime::repos::scratch_host::sweep(
            svc,
            ws,
            &settings(svc, data_dir),
            host_label,
            now,
        );
        scratch::purge_trash(svc.store());
        let (noticed, removed) = (
            r.noticed.len() + r2.noticed.len(),
            r.removed.len() + r2.removed.len(),
        );
        if json_out {
            println!(
                "{}",
                json!({ "noticed": noticed, "removed": removed, "failed": r.failed.iter().chain(&r2.failed).map(|(i, e)| json!({"id": i, "error": e})).collect::<Vec<_>>() })
            );
        } else {
            println!("scratch cleanup: {noticed} noticed, {removed} removed");
            for (i, e) in r.failed.iter().chain(&r2.failed) {
                eprintln!("  {i}: {e}");
            }
        }
        return Ok(());
    }
    if flag("--remove").is_some() {
        let sel = target.ok_or_else(|| {
            crate::help::usage_error("usage: ligus repo scratch --remove <scratch id | name>")
        })?;
        let e = find(svc, ws, sel)?;
        let removed = if e.repo_id.is_some() {
            ligus_runtime::repos::scratch_host::remove(svc, ws, e.id, host_label, None)
                .map_err(|x| anyhow!("{x}"))?
        } else {
            remove(data_dir, svc, ws, e.id, host_label, Some(&format!("The scratch repository {} was removed from {host_label}. This chat has no repository now; its proposals and diffs stay in the chat.", e.name)))
                .map_err(|x| anyhow!("{x}"))?
        };
        scratch::purge_trash(svc.store());
        println!("removed the scratch repository {}.", removed.name);
        return Ok(());
    }
    let st = settings(svc, data_dir);
    if flag("--list").is_some() {
        let all = scratch::list(svc.store(), ws);
        if json_out {
            let live = all.iter().filter(|e| e.is_live()).count();
            println!(
                "{}",
                json!({ "scratch": all.iter().map(|e| dto(e, &st, now)).collect::<Vec<_>>(), "settings": settings_dto(&st, live), "root": st.root })
            );
            return Ok(());
        }
        println!(
            "scratch root : {}{}",
            st.root.display(),
            if st.root_is_default { " (default)" } else { "" }
        );
        println!(
            "cleanup      : {}",
            st.cleanup_days
                .map(|d| format!(
                    "after {d} days without activity, with a notice {} days before",
                    scratch::NOTICE_DAYS
                ))
                .unwrap_or_else(|| "off".into())
        );
        println!(
            "limits       : {} per host, {} per member",
            st.max_per_host, st.max_per_member
        );
        for e in &all {
            let state = match e.state {
                scratch::ScratchState::Live => scratch::expiry(e, &st, now)
                    .map(|x| format!("removed on {} unless kept", scratch::date(x.remove_at)))
                    .unwrap_or_else(|| "kept until removed".into()),
                scratch::ScratchState::Kept => format!("kept at {}", e.path.display()),
                scratch::ScratchState::Removed => "removed".into(),
            };
            println!(
                "{}  {}  {}  ({state})",
                e.short_id(),
                e.name,
                if e.is_live() {
                    e.path.display().to_string()
                } else {
                    String::new()
                }
            );
        }
        if all.is_empty() {
            println!("no scratch repositories yet: ligus repo scratch [--chat ID]");
        }
        return Ok(());
    }
    // Create.
    let chat = flag("--chat")
        .filter(|c| !c.is_empty())
        .map(|c| {
            Uuid::parse_str(c.trim())
                .map_err(|_| crate::help::usage_error(format!("not a chat id: {c:?}")))
        })
        .transpose()?;
    let name = flag("--name").unwrap_or_default();
    let who = svc
        .author()
        .account_id
        .map(|a| a.to_string())
        .unwrap_or_default();
    let e = if flag("--workspace").is_some() {
        // A person's own CLI device: register it workspace-wide (Admin).
        let (e, _) = ligus_runtime::repos::scratch_host::create(
            svc,
            ws,
            chat,
            &st,
            &forbidden(),
            &name,
            host_label,
        )
        .map_err(|x| anyhow!("{x}"))?;
        e
    } else {
        create(data_dir, svc, ws, chat, &name, &who).map_err(|x| anyhow!("{x}"))?
    };
    if let Some(c) = chat {
        let _ = svc.post_system_note(
            c,
            ws,
            format!(
                "This chat now works in a new scratch repository, {}, on {host_label} (branch {}).",
                e.name,
                crate::repos::chat_branch(c)
            ),
        );
    }
    if json_out {
        let mut v = dto(&e, &st, now);
        v["path"] = json!(e.path);
        println!("{v}");
    } else {
        println!(
            "created the scratch repository {} at {}.",
            e.name,
            e.path.display()
        );
        if let Some(c) = chat {
            println!("chat {c} works in it on this host.");
        }
        if let Some(x) = scratch::expiry(&e, &st, now) {
            println!("unless kept (ligus repo keep {}), it is removed after {} days without activity (on {} at the earliest).", e.short_id(), st.cleanup_days.unwrap_or(0), scratch::date(x.remove_at));
        }
    }
    Ok(())
}

/// `ligus repo keep <scratch> [--to PATH] [--name N] [--json]`.
pub fn cmd_keep(
    data_dir: &Path,
    svc: &mut ChatService,
    ws: Uuid,
    host_label: &str,
    sel: Option<&str>,
    flag: &dyn Fn(&str) -> Option<String>,
) -> Result<()> {
    let Some(sel) = sel else {
        return Err(crate::help::usage_error(
            "usage: ligus repo keep <scratch id | name> [--to PATH] [--name N]",
        ));
    };
    let e = find(svc, ws, sel)?;
    if !e.is_live() {
        bail!(
            "{} isn't a live scratch repository (it was kept or removed)",
            e.name
        );
    }
    let name = flag("--name").filter(|n| !n.is_empty());
    let dest = flag("--to")
        .filter(|d| !d.is_empty())
        .map(|d| crate::repos_cmd::abs(&d))
        .transpose()?;
    let (kept, workspace) = if e.repo_id.is_some() {
        let dest = dest.unwrap_or_else(|| default_keep_dest(data_dir, &e, name.as_deref()));
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p)?;
        }
        let (k, dropped) = ligus_runtime::repos::scratch_host::keep(
            svc,
            ws,
            e.id,
            &dest,
            name.as_deref(),
            &forbidden(),
            host_label,
        )
        .map_err(|x| anyhow!("{x}"))?;
        (k, Some(dropped))
    } else {
        (
            keep(data_dir, svc, ws, e.id, dest, name.as_deref()).map_err(|x| anyhow!("{x}"))?,
            None,
        )
    };
    if flag("--json").is_some() {
        println!(
            "{}",
            json!({ "id": kept.id, "name": kept.name, "path": kept.path, "workspaceFlagDropped": workspace })
        );
    } else {
        println!(
            "kept {} at {}; it is an ordinary repository now.",
            kept.name,
            kept.path.display()
        );
        if workspace == Some(false) {
            println!("the workspace still tags it scratch (dropping that needs an Admin).");
        }
    }
    Ok(())
}

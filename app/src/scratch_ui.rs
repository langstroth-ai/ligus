//! Scratch repositories on the desktop: "New scratch repo" in the chat's
//! repo picker, the "scratch" tag and removal notice, "Keep it as a repo"
//! (move it to a chosen folder, optionally publish it to GitHub as a
//! private repository), and the cleanup. The logic is
//! `ligus_runtime::repos::scratch_host`; this module is the Tauri surface.
//!
//! The desktop is the host for its own chats (hosts and bindings): the
//! scratch folder is created here, under `<app data dir>/scratch` unless the
//! person picked another root, registered as a workspace repository flagged
//! scratch (an Admin's, with the passkey prompt the service brings), bound
//! here, and set as the chat's repository.

use std::path::{Path, PathBuf};

use ligus_runtime::repos::scratch::{self, ScratchRepo, Settings};
use ligus_runtime::repos::scratch_host;
use serde_json::{json, Value};
use tauri::State;
use uuid::Uuid;

use crate::{map_err, AppState};

fn settings(state: &AppState) -> Settings {
    let svc = state.service.lock().unwrap();
    scratch::settings(svc.store(), &state.data_dir.join("scratch"))
}

fn ws_of(state: &AppState) -> Uuid {
    state.active_workspace_id()
}

fn parse(id: &str) -> Result<Uuid, String> {
    Uuid::parse_str(id.trim()).map_err(map_err)
}

/// One scratch repository for the desktop (its own path is shown: it is
/// this device's).
pub(crate) fn dto(e: &ScratchRepo, st: &Settings, now: i64) -> Value {
    let x = scratch::expiry(e, st, now);
    json!({
        "id": e.id,
        "name": crate::shown(&e.name),
        "folder": e.dir_name,
        "path": e.path,
        "state": e.state,
        "repoId": e.repo_id,
        "createdAt": e.created_at,
        "lastActivity": scratch::activity(e, now),
        "removeAt": x.map(|x| x.remove_at),
        "noticeFrom": x.map(|x| x.notice_from),
        "notice": x.is_some_and(|x| x.notice),
        "chats": e.chats.len(),
        "published": e.published,
    })
}

fn settings_dto(st: &Settings, live: usize) -> Value {
    json!({
        "root": st.root,
        "rootIsDefault": st.root_is_default,
        "cleanupDays": st.cleanup_days,
        "noticeDays": scratch::NOTICE_DAYS,
        "maxPerHost": st.max_per_host,
        "maxPerMember": st.max_per_member,
        "live": live,
    })
}

/// This device's live scratch entry for workspace repo `repo`, as the
/// picker, Settings and the chat header show it.
pub(crate) fn info_for_repo(state: &AppState, repo: Uuid) -> Option<Value> {
    let st = settings(state);
    let svc = state.service.lock().unwrap();
    scratch::by_repo(svc.store(), repo).map(|e| dto(&e, &st, scratch::now()))
}

#[tauri::command]
pub async fn list_scratch_repos(ligus_app: tauri::AppHandle) -> Result<Value, String> {
    crate::off_main(ligus_app, move |a| {
        list_scratch_repos_sync(tauri::Manager::state::<AppState>(a))
    })
    .await
}

pub fn list_scratch_repos_sync(state: State<AppState>) -> Result<Value, String> {
    let st = settings(&state);
    let ws = ws_of(&state);
    let svc = state.service.lock().unwrap();
    let all = scratch::list(svc.store(), ws);
    let live = all.iter().filter(|e| e.is_live()).count();
    let now = scratch::now();
    Ok(
        json!({ "scratch": all.iter().rev().map(|e| dto(e, &st, now)).collect::<Vec<_>>(), "settings": settings_dto(&st, live) }),
    )
}

/// "New scratch repo": for `session_id` (or none, from Settings).
#[tauri::command]
pub async fn create_scratch_repo(
    ligus_app: tauri::AppHandle,
    session_id: Option<String>,
    name: Option<String>,
) -> Result<Value, String> {
    crate::off_main(ligus_app, move |a| {
        create_scratch_repo_sync(tauri::Manager::state::<AppState>(a), session_id, name)
    })
    .await
}

pub fn create_scratch_repo_sync(
    state: State<AppState>,
    session_id: Option<String>,
    name: Option<String>,
) -> Result<Value, String> {
    let chat = session_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse)
        .transpose()?;
    let st = settings(&state);
    let mut svc = state.service.lock().unwrap();
    let ws = match chat {
        Some(c) => svc
            .workspace_of(c)
            .map_err(map_err)?
            .ok_or("unknown chat")?,
        None => ws_of(&state),
    };
    let (e, repo) = scratch_host::create(
        &mut svc,
        ws,
        chat,
        &st,
        &[],
        name.as_deref().unwrap_or(""),
        &crate::repos_ui::host_label(),
    )
    .map_err(map_err)?;
    if let Some(c) = chat {
        let _ = svc.post_system_note(
            c,
            ws,
            format!(
                "This chat now works in a new scratch repository, {}, on {} (branch {}).",
                e.name,
                crate::repos_ui::host_label(),
                ligus_runtime::git_worktree::chat_branch_name(&c.to_string())
            ),
        );
    }
    let mut v = dto(&e, &st, scratch::now());
    v["repoId"] = json!(repo.id);
    Ok(v)
}

/// "Keep it as a repo": move it to `path` (a folder that doesn't exist yet,
/// outside the scratch root and other repositories), bind it there, and
/// drop the workspace's scratch flag.
#[tauri::command]
pub async fn keep_scratch_repo(
    ligus_app: tauri::AppHandle,
    id: String,
    path: String,
    name: Option<String>,
) -> Result<Value, String> {
    crate::off_main(ligus_app, move |a| {
        keep_scratch_repo_sync(tauri::Manager::state::<AppState>(a), id, path, name)
    })
    .await
}

pub fn keep_scratch_repo_sync(
    state: State<AppState>,
    id: String,
    path: String,
    name: Option<String>,
) -> Result<Value, String> {
    let id = parse(&id)?;
    let dest = PathBuf::from(path.trim());
    if !dest.is_absolute() {
        return Err("pick a folder (an absolute path)".into());
    }
    let st = settings(&state);
    let ws = ws_of(&state);
    let mut svc = state.service.lock().unwrap();
    let (k, dropped) = scratch_host::keep(
        &mut svc,
        ws,
        id,
        &dest,
        name.as_deref(),
        &[],
        &crate::repos_ui::host_label(),
    )
    .map_err(map_err)?;
    let mut v = dto(&k, &st, scratch::now());
    v["workspaceFlagDropped"] = json!(dropped);
    Ok(v)
}

/// "Publish to GitHub (private)", for a kept scratch repository, after the
/// confirmation that named `owner/name` (`confirm` repeats it).
#[tauri::command]
pub async fn publish_scratch_repo(
    state: State<'_, AppState>,
    id: String,
    owner: String,
    name: String,
    confirm: String,
    forge: Option<String>,
) -> Result<Value, String> {
    let id = parse(&id)?;
    let ws = ws_of(&state);
    let e = {
        let svc = state.service.lock().unwrap();
        scratch::list(svc.store(), ws)
            .into_iter()
            .find(|e| e.id == id)
            .ok_or("no such scratch repository on this device")?
    };
    if e.state != scratch::ScratchState::Kept {
        return Err("keep it as a repo first; only a kept scratch repository is published".into());
    }
    let f = crate::forges::forge_for(&state.data_dir, forge.as_deref()).ok_or(
        "no code host connection for that on this device: add GitHub under Settings > Code hosts",
    )?;
    let url = scratch_host::publish(f.as_ref(), &e, &owner, &name, &confirm).await?;
    {
        let svc = state.service.lock().unwrap();
        let _ = scratch::set_published(svc.store(), id, &url);
    }
    Ok(json!({ "url": url, "private": true }))
}

/// "Remove now".
#[tauri::command]
pub async fn remove_scratch_repo(ligus_app: tauri::AppHandle, id: String) -> Result<(), String> {
    crate::off_main(ligus_app, move |a| {
        remove_scratch_repo_sync(tauri::Manager::state::<AppState>(a), id)
    })
    .await
}

pub fn remove_scratch_repo_sync(state: State<AppState>, id: String) -> Result<(), String> {
    let id = parse(&id)?;
    let ws = ws_of(&state);
    let mut svc = state.service.lock().unwrap();
    let label = crate::repos_ui::host_label();
    let e = scratch::list(svc.store(), ws)
        .into_iter()
        .find(|e| e.id == id && e.is_live())
        .ok_or("no such scratch repository on this device")?;
    let note = format!("The scratch repository {} was removed from {label}. This chat has no repository now; its proposals and diffs stay in the chat.", e.name);
    scratch_host::remove(&mut svc, ws, id, &label, Some(&note)).map_err(map_err)?;
    // A person's explicit act: its workspace entry goes too (an Admin's,
    // with the passkey; refused, it stays pending for the next time).
    scratch_host::retire_removed(&mut svc, ws);
    let trash = scratch::pending_trash(svc.store());
    drop(svc);
    for t in trash {
        let _ = scratch::delete_trash(&t);
    }
    Ok(())
}

/// The cleanup (`cleanup_days`: 1 to 365, 0 off, none the default) and the
/// scratch root (`root`: a folder, "" back to the default).
#[tauri::command]
pub async fn set_scratch_settings(
    ligus_app: tauri::AppHandle,
    cleanup_days: Option<u32>,
    reset_days: Option<bool>,
    root: Option<String>,
) -> Result<Value, String> {
    crate::off_main(ligus_app, move |a| {
        set_scratch_settings_sync(
            tauri::Manager::state::<AppState>(a),
            cleanup_days,
            reset_days,
            root,
        )
    })
    .await
}

pub fn set_scratch_settings_sync(
    state: State<AppState>,
    cleanup_days: Option<u32>,
    reset_days: Option<bool>,
    root: Option<String>,
) -> Result<Value, String> {
    {
        let svc = state.service.lock().unwrap();
        if reset_days == Some(true) {
            scratch::set_cleanup_days(svc.store(), None).map_err(map_err)?;
        } else if let Some(d) = cleanup_days {
            scratch::set_cleanup_days(svc.store(), Some(d)).map_err(map_err)?;
        }
        match root.as_deref().map(str::trim) {
            Some("") => {
                scratch::set_root(svc.store(), None, &[]).map_err(map_err)?;
            }
            Some(r) => {
                scratch::set_root(svc.store(), Some(Path::new(r)), &[]).map_err(map_err)?;
            }
            None => {}
        }
    }
    list_scratch_repos_sync(state)
}

/// The cleanup on this desktop, about hourly (and a minute after start):
/// notices two days ahead, then removal of unkept scratch repositories.
pub(crate) async fn run_sweep_loop(app: tauri::AppHandle) {
    let mut wait = std::time::Duration::from_secs(60);
    loop {
        tokio::time::sleep(wait).await;
        wait = std::time::Duration::from_secs(3600);
        let a = app.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let state = tauri::Manager::state::<AppState>(&a);
            let st = settings(&state);
            let label = crate::repos_ui::host_label();
            let trash = {
                let mut svc = state.service.lock().unwrap();
                let workspaces: std::collections::BTreeSet<Uuid> = scratch::live_all(svc.store())
                    .into_iter()
                    .map(|e| e.workspace_id)
                    .collect();
                for ws in workspaces {
                    let r = scratch_host::sweep(&mut svc, ws, &st, &label, scratch::now());
                    for (id, e) in r.failed {
                        tracing::warn!(target: "repos", "scratch: couldn't remove {id}: {e}");
                    }
                }
                scratch::pending_trash(svc.store())
            };
            // Without the service lock: a big tree never stalls the window.
            for t in trash {
                let _ = scratch::delete_trash(&t);
            }
        })
        .await;
    }
}

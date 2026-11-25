//! Scratch repositories on the home host, from the phone (contract in
//! `docs/ios-app.md`, "Scratch repositories"):
//!
//! - `GET    /v1/repos/scratch` → `{scratch:[…], settings}`: this box's
//!   scratch repositories (live, kept and removed) and its cleanup settings.
//!   Names and dates only, never a path.
//! - `POST   /v1/repos/scratch {chatId?, name?}` → `{scratch, binding?}`:
//!   create an empty repository (one empty commit on `main`) under the
//!   box's scratch root and, with `chatId`, bind that chat to it here.
//! - `POST   /v1/repos/scratch/{id}/keep {name?}` → `{scratch}`: move it
//!   to the box's repos dir; an ordinary checkout of the box from then on.
//! - `POST   /v1/repos/scratch/{id}/publish {owner, name, forge?, confirm}`
//!   → `{url}`: a kept one only. Creates a **private** repository named
//!   `owner/name` with the box's code-host connection and pushes its
//!   branches; `confirm` must repeat `owner/name` exactly (what the phone's
//!   confirmation showed). Never public, never automatic.
//! - `DELETE /v1/repos/scratch/{id}`: remove a live one now.
//! - `PUT    /v1/repos/scratch/settings {cleanupDays}`: days without
//!   activity before removal (0 = off, null = the default 14).
//!
//! Every route is the box owner's (`owner_gate`, like a clone): it spends
//! the box's disk and, for publish, its code-host access. Creation counts
//! against a per-host and a per-member limit (by the asking person's
//! account id).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use ligus_runtime::repos::scratch::{self, ScratchError};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::api::{load_chat, ApiError, ApiResult};
use super::repos_api::{ensure_can_write, owner_gate};
use super::tunnel::TunnelPeer;
use super::Shared;
use crate::scratch_cmd;

type S = State<Arc<Shared>>;

fn api_err(e: ScratchError) -> ApiError {
    let code = match &e {
        ScratchError::Limit(_) => StatusCode::TOO_MANY_REQUESTS,
        ScratchError::NotFound => StatusCode::NOT_FOUND,
        ScratchError::Path(_) | ScratchError::Invalid(_) => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    ApiError::new(code, e.to_string())
}

fn parse_id(raw: &str) -> ApiResult<Uuid> {
    Uuid::parse_str(raw.trim()).map_err(|_| ApiError::bad_request("not a scratch repository id"))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// The asking person's account id: the paired phone's person, else (the
/// operator's bearer on loopback) this box's own.
fn caller_account(s: &Shared, peer: Option<&TunnelPeer>) -> String {
    let svc = s.lock();
    let device = peer.and_then(|p| s.tunnel.as_ref().and_then(|t| t.registry.get(&p.pid))).and_then(|r| r.device_id);
    let account = match device {
        Some(d) => svc.phone_person(s.ws, d).ok().and_then(|a| a.account_id),
        None => svc.author().account_id,
    };
    account.map(|a| a.to_string()).unwrap_or_default()
}

pub fn listing(s: &Shared) -> Value {
    let svc = s.lock();
    let st = scratch_cmd::settings(&svc, &s.cfg.data_dir);
    let now = scratch::now();
    let all = scratch::list(svc.store(), s.ws);
    let live = all.iter().filter(|e| e.is_live()).count();
    json!({
        "scratch": all.iter().rev().map(|e| scratch_cmd::dto(e, &st, now)).collect::<Vec<_>>(),
        "settings": scratch_cmd::settings_dto(&st, live),
    })
}

pub async fn list(State(s): S, peer: Option<Extension<TunnelPeer>>) -> ApiResult<Json<Value>> {
    owner_gate(&s, peer.as_deref())?;
    let sh = s.clone();
    Ok(Json(blocking(move || listing(&sh)).await?))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CreateReq {
    #[serde(default)]
    chat_id: Option<String>,
    #[serde(default)]
    name: String,
}

pub async fn create(State(s): S, peer: Option<Extension<TunnelPeer>>, Json(req): Json<CreateReq>) -> ApiResult<Json<Value>> {
    owner_gate(&s, peer.as_deref())?;
    let chat = match req.chat_id.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => Some(load_chat(&s, c)?),
        None => None,
    };
    ensure_can_write(&s, chat.as_ref())?;
    if req.name.chars().count() > ligus_core::workspace_repo::MAX_DISPLAY_NAME {
        return Err(ApiError::bad_request("that name is too long"));
    }
    let who = caller_account(&s, peer.as_deref());
    let sh = s.clone();
    let chat_id = chat.as_ref().map(|c| c.id);
    let name = req.name.clone();
    let e = blocking(move || {
        let _g = sh.repos.bindings.lock().unwrap_or_else(|e| e.into_inner());
        let svc = sh.lock();
        scratch_cmd::create(&sh.cfg.data_dir, &svc, sh.ws, chat_id, &name, &who)
    })
    .await?
    .map_err(api_err)?;
    let st = scratch_cmd::settings(&s.lock(), &s.cfg.data_dir);
    let mut out = json!({ "scratch": scratch_cmd::dto(&e, &st, scratch::now()) });
    if let Some(c) = chat {
        let note = format!(
            "This chat now works in a new scratch repository, {}, on {} (branch {}). Keep it as a repo to hold on to it; otherwise it is removed after a quiet spell.",
            e.name,
            s.opts.label,
            crate::repos::chat_branch(c.id)
        );
        s.lock().post_system_note(c.id, c.workspace_id, note)?;
        s.after_write();
        out["binding"] = super::repos_api::details_repo(&s, c.id);
    }
    Ok(Json(out))
}

#[derive(Deserialize, Default)]
pub struct KeepReq {
    #[serde(default)]
    name: Option<String>,
}

pub async fn keep(State(s): S, peer: Option<Extension<TunnelPeer>>, Path(id): Path<String>, body: axum::body::Bytes) -> ApiResult<Json<Value>> {
    owner_gate(&s, peer.as_deref())?;
    ensure_can_write(&s, None)?;
    let id = parse_id(&id)?;
    let req: KeepReq = if body.iter().all(u8::is_ascii_whitespace) { KeepReq::default() } else { serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))? };
    let name = req.name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    if name.as_deref().is_some_and(|n| !ligus_core::workspace_repo::display_name_ok(n)) {
        return Err(ApiError::bad_request("bad repository name"));
    }
    let sh = s.clone();
    let kept = blocking(move || {
        let _g = sh.repos.bindings.lock().unwrap_or_else(|e| e.into_inner());
        let svc = sh.lock();
        scratch_cmd::keep(&sh.cfg.data_dir, &svc, sh.ws, id, None, name.as_deref())
    })
    .await?
    .map_err(api_err)?;
    for c in &kept.chats {
        let folder = kept.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let _ = s.lock().post_system_note(*c, s.ws, format!("{} is kept: an ordinary repository on {} now ({folder}).", kept.name, s.opts.label));
    }
    s.after_write();
    let st = scratch_cmd::settings(&s.lock(), &s.cfg.data_dir);
    let mut v = scratch_cmd::dto(&kept, &st, scratch::now());
    v["checkout"] = json!(kept.path.file_name().map(|n| n.to_string_lossy().into_owned()));
    Ok(Json(json!({ "scratch": v })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishReq {
    owner: String,
    name: String,
    /// `owner/name` again, exactly as the confirmation showed it.
    #[serde(default)]
    confirm: String,
    #[serde(default)]
    forge: Option<String>,
}

pub async fn publish(State(s): S, peer: Option<Extension<TunnelPeer>>, Path(id): Path<String>, Json(req): Json<PublishReq>) -> ApiResult<Json<Value>> {
    owner_gate(&s, peer.as_deref())?;
    ensure_can_write(&s, None)?;
    let id = parse_id(&id)?;
    let e = {
        let svc = s.lock();
        scratch::list(svc.store(), s.ws).into_iter().find(|e| e.id == id).ok_or_else(|| ApiError::not_found("scratch repository"))?
    };
    if e.state != scratch::ScratchState::Kept {
        return Err(ApiError::new(StatusCode::CONFLICT, "keep it as a repo first; only a kept scratch repository is published"));
    }
    if !ligus_runtime::git_worktree::is_git_repo(&e.path) {
        return Err(ApiError::new(StatusCode::CONFLICT, "its checkout is missing on this host"));
    }
    let handle = match req.forge.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
        Some(raw) => {
            let fid = ligus_forge::ForgeId::parse(raw).map_err(|e| ApiError::bad_request(e.to_string()))?;
            s.repos.forge(&s.cfg.data_dir, &fid).ok_or_else(|| ApiError::not_found("code host connection"))?
        }
        None => s.repos.github(&s.cfg.data_dir).ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "no GitHub connection on this host"))?,
    };
    let url = ligus_runtime::repos::scratch_host::publish(handle.forge.as_ref(), &e, &req.owner, &req.name, &req.confirm)
        .await
        .map_err(|m| ApiError::new(StatusCode::CONFLICT, m))?;
    {
        let svc = s.lock();
        let _ = scratch::set_published(svc.store(), id, &url);
    }
    s.repos.invalidate();
    Ok(Json(json!({ "url": url, "private": true })))
}

pub async fn remove(State(s): S, peer: Option<Extension<TunnelPeer>>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    owner_gate(&s, peer.as_deref())?;
    ensure_can_write(&s, None)?;
    let id = parse_id(&id)?;
    let sh = s.clone();
    let removed = blocking(move || {
        let _g = sh.repos.bindings.lock().unwrap_or_else(|e| e.into_inner());
        let mut svc = sh.lock();
        let e = scratch::list(svc.store(), sh.ws).into_iter().find(|e| e.id == id && e.is_live()).ok_or(ScratchError::NotFound)?;
        let note = format!("The scratch repository {} was removed from {}. This chat has no repository now; its proposals and diffs stay in the chat.", e.name, sh.opts.label);
        scratch_cmd::remove(&sh.cfg.data_dir, &mut svc, sh.ws, id, &sh.opts.label, Some(&note))
    })
    .await?
    .map_err(api_err)?;
    s.after_write();
    let trash = scratch::pending_trash(s.lock().store());
    let _ = blocking(move || trash.iter().for_each(|t| drop(scratch::delete_trash(t)))).await;
    Ok(Json(json!({ "ok": true, "id": removed.id })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsReq {
    /// Days (1 to 365), 0 = off, null = the default.
    cleanup_days: Option<u32>,
}

pub async fn put_settings(State(s): S, peer: Option<Extension<TunnelPeer>>, Json(req): Json<SettingsReq>) -> ApiResult<Json<Value>> {
    owner_gate(&s, peer.as_deref())?;
    {
        let svc = s.lock();
        scratch::set_cleanup_days(svc.store(), req.cleanup_days).map_err(api_err)?;
    }
    let sh = s.clone();
    Ok(Json(blocking(move || listing(&sh)).await?))
}

/// The cleanup, about hourly: notices in chats two days ahead, then
/// removal of unkept scratch repositories past their quiet spell.
pub async fn sweep_loop(shared: Arc<Shared>, mut stop: tokio::sync::watch::Receiver<bool>) {
    let first = std::time::Duration::from_secs(60);
    let every = std::time::Duration::from_secs(crate::env_opt("LIGUS_SCRATCH_SWEEP_SECS").and_then(|v| v.trim().parse().ok()).unwrap_or(3600).max(30));
    let mut wait = first;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = stop.wait_for(|s| *s) => return,
        }
        wait = every;
        if shared.lock().write_block().is_some() {
            continue;
        }
        let sh = shared.clone();
        let r = tokio::task::spawn_blocking(move || sweep_once(&sh, scratch::now())).await;
        if let Ok(r) = r {
            for (id, e) in &r.failed {
                eprintln!("scratch: couldn't remove {id}: {e}");
            }
            if !r.noticed.is_empty() || !r.removed.is_empty() {
                shared.after_write();
            }
        }
    }
}

/// One cleanup pass at `now` (the loop's body; tests call it directly).
pub fn sweep_once(s: &Shared, now: i64) -> ligus_runtime::repos::scratch_host::SweepReport {
    let (report, trash) = {
        let _g = s.repos.bindings.lock().unwrap_or_else(|e| e.into_inner());
        let mut svc = s.lock();
        let r = scratch_cmd::sweep(&s.cfg.data_dir, &mut svc, s.ws, &s.opts.label, now);
        (r, scratch::pending_trash(svc.store()))
    };
    // The deletes run without any lock: a big tree never stalls the API.
    for t in trash {
        let _ = scratch::delete_trash(&t);
    }
    report
}

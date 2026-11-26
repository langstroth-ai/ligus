//! Workspace repos for the phone (repos-and-checkouts 6.2):
//!
//! - `GET    /v1/workspace-repos` → `{repos:[{id, name, canonical, forgeKind,
//!   defaultBranch, removed, here:{state, code, viaFork, checkout?, found?},
//!   hosts:[{hostId, label, kind, online, state, viaFork}]}], hostId}`. Every
//!   host's coverage as it last reported it (`unknown` when it reports
//!   nothing). **No paths**: `checkout` and `found` are folder names on this
//!   box only.
//! - `POST   /v1/workspace-repos` `{input, name?, kind?, branch?}`: add one
//!   (Admin; needs the owner's passkey presence, `presence_required`).
//! - `PATCH  /v1/workspace-repos/{id}` `{name, branch?}`: rename (Admin).
//! - `DELETE /v1/workspace-repos/{id}`: remove (Admin, presence).
//! - `POST   /v1/workspace-repos/{id}/confirm` `{checkout}`: "use it" for a
//!   checkout a scan found on this box (by folder name).
//! - `POST   /v1/workspace-repos/rescan`: scan the box's roots again.
//! - `PUT    /v1/chats/{id}/workspace-repo` `{repoId|null, baseBranch?}`: the
//!   chat's repo (`ChatRepoSet`, signed by the box for the owner's explicit
//!   act).
//!
//! Clone one here with `POST /v1/repos/clone {repoId}`.

use ligus_runtime::display::shown;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use ligus_runtime::repos as rr;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::api::{load_chat, ApiError, ApiResult};
use super::repos_api::{ensure_can_write, owner_gate};
use super::tunnel::TunnelPeer;
use axum::Extension;
use super::Shared;

type S = State<Arc<Shared>>;

fn folder_name(p: &std::path::Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn parse_repo(raw: &str) -> ApiResult<Uuid> {
    Uuid::parse_str(raw.trim()).map_err(|_| ApiError::bad_request("not a repository id"))
}

/// The phone's view of the workspace's repos and every host's coverage.
pub fn listing(s: &Shared) -> ApiResult<Value> {
    let svc = s.lock();
    let roster = svc.workspace_roster(s.ws)?;
    let hosts = svc.load_in(s.ws, ligus_core::workspace_config_session_id(s.ws))?.map(|c| c.workspace_hosts).unwrap_or_default();
    let reports = rr::host_reports(&roster, &hosts, &s.presence);
    let entries = rr::workspace_repos(&roster);
    let live: Vec<_> = entries.iter().filter(|e| !e.removed).map(|e| e.repo.clone()).collect();
    let mine = rr::coverage_in(svc.store(), s.ws, &roster, &rr::cloning());
    let maps = svc.store().repo_mappings(s.ws).map_err(ligus_runtime::chat_service::ChatError::from)?;
    let looks = ligus_core::workspace_repo::lookalikes(&live);
    let repos: Vec<Value> = entries
        .iter()
        .map(|e| {
            let r = &e.repo;
            let lookalike_of = looks.get(&r.id).and_then(|o| live.iter().find(|x| x.id == *o)).map(|x| shown(&x.display_name));
            let here = mine.iter().find(|c| c.repo_id == r.id);
            let checkout = maps.iter().find(|m| m.repo_id == r.id && m.confirmed).map(|m| folder_name(&m.path));
            let found: Vec<String> = maps.iter().filter(|m| m.repo_id == r.id && !m.confirmed).map(|m| folder_name(&m.path)).collect();
            let hosts: Vec<Value> = reports
                .iter()
                .filter(|h| h.host_id != s.host_id)
                .map(|h| {
                    let c = h.coverage.as_ref().map(|c| c.iter().find(|x| x.repo_id == r.id).cloned());
                    let (state, via_fork) = match c {
                        None => ("unknown".to_string(), false),
                        Some(None) => ("missing".to_string(), false),
                        Some(Some(x)) => (x.state.clone(), x.via_fork),
                    };
                    json!({ "hostId": h.host_id, "label": shown(&h.label), "kind": h.kind, "online": h.online, "state": state, "viaFork": via_fork })
                })
                .collect();
            json!({
                "id": r.id,
                "name": shown(&r.display_name),
                "canonical": r.canonical,
                "forgeKind": r.forge_kind,
                "defaultBranch": r.default_branch,
                "removed": e.removed,
                // A scratch repository someone's device created (a display
                // hint: the "scratch" tag).
                "scratch": r.is_scratch(),
                "here": {
                    "state": here.map(|c| c.state.as_str()).unwrap_or(if e.removed { "removed" } else { "missing" }),
                    "code": here.map(|c| c.code.as_str()).unwrap_or(""),
                    "viaFork": here.is_some_and(|c| c.via_fork),
                    "checkout": checkout,
                    "found": found,
                },
                "hosts": hosts,
                "lookalikeOf": lookalike_of,
                // Synced aliases (design 12): names only.
                "aliases": roster.repo_aliases.get(&r.id).map(|v| v.iter().map(|a| shown(a)).collect::<Vec<_>>()).unwrap_or_default(),
            })
        })
        .collect();
    let host_aliases: Vec<Value> = roster.host_aliases.iter().map(|(a, h)| json!({ "alias": shown(a), "host": h })).collect();
    Ok(json!({ "repos": repos, "hostAliases": host_aliases, "hostId": s.host_id, "label": s.opts.label }))
}

pub async fn list(State(s): S) -> ApiResult<Json<Value>> {
    Ok(Json(listing(&s)?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddReq {
    input: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    branch: String,
}

pub async fn add(State(s): S, peer: Option<Extension<TunnelPeer>>, Json(req): Json<AddReq>) -> ApiResult<Json<Value>> {
    ensure_can_write(&s, None)?;
    // Publishes a repo workspace-wide and runs the (owner-gated) rescan.
    owner_gate(&s, peer.as_deref())?;
    let kind = req.kind.unwrap_or_else(|| crate::repos_cmd::guess_kind(&req.input));
    let repo = s.lock().add_workspace_repo(s.ws, &req.input, &req.name, &kind, &req.branch, None)?;
    s.after_write();
    let sh = s.clone();
    let _ = tokio::task::spawn_blocking(move || super::repos_refresh(&sh)).await;
    Ok(Json(json!({ "id": repo.id, "canonical": repo.canonical, "name": shown(&repo.display_name) })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameReq {
    name: String,
    #[serde(default)]
    branch: Option<String>,
}

pub async fn rename(State(s): S, peer: Option<Extension<TunnelPeer>>, Path(id): Path<String>, Json(req): Json<RenameReq>) -> ApiResult<Json<Value>> {
    ensure_can_write(&s, None)?;
    owner_gate(&s, peer.as_deref())?;
    let id = parse_repo(&id)?;
    let branch = match req.branch {
        Some(b) => b,
        None => s.lock().workspace_roster(s.ws)?.repos.get(&id).map(|e| e.repo.default_branch.clone()).unwrap_or_default(),
    };
    s.lock().update_workspace_repo(s.ws, id, &req.name, &branch)?;
    s.after_write();
    Ok(Json(json!({ "ok": true })))
}

pub async fn remove(State(s): S, peer: Option<Extension<TunnelPeer>>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    ensure_can_write(&s, None)?;
    owner_gate(&s, peer.as_deref())?;
    let id = parse_repo(&id)?;
    s.lock().remove_workspace_repo(s.ws, id)?;
    s.after_write();
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct ConfirmReq {
    checkout: String,
}

/// "Found WeFit on this host as WeFit. Use it?": confirm by folder name.
pub async fn confirm(State(s): S, peer: Option<Extension<TunnelPeer>>, Path(id): Path<String>, Json(req): Json<ConfirmReq>) -> ApiResult<Json<Value>> {
    ensure_can_write(&s, None)?;
    owner_gate(&s, peer.as_deref())?;
    let id = parse_repo(&id)?;
    let sh = s.clone();
    tokio::task::spawn_blocking(move || -> ApiResult<()> {
        let svc = sh.lock();
        let roster = svc.workspace_roster(sh.ws)?;
        let repo = roster.repos.get(&id).filter(|e| !e.removed).map(|e| e.repo.clone()).ok_or_else(|| ApiError::not_found("workspace repository"))?;
        let m = svc
            .store()
            .repo_mappings(sh.ws).map_err(ligus_runtime::chat_service::ChatError::from)?
            .into_iter()
            .find(|m| m.repo_id == id && !m.confirmed && folder_name(&m.path) == req.checkout)
            .ok_or_else(|| ApiError::not_found("a found checkout by that name"))?;
        rr::confirm(svc.store(), svc.device_id(), &sh.opts.label, sh.ws, &repo, &m.path).map_err(|e| ApiError::bad_request(e.to_string()))?;
        Ok(())
    })
    .await
    .map_err(|e| ApiError::bad_request(e.to_string()))??;
    Ok(Json(listing(&s)?))
}

pub async fn rescan(State(s): S, peer: Option<Extension<TunnelPeer>>) -> ApiResult<Json<Value>> {
    owner_gate(&s, peer.as_deref())?;
    let sh = s.clone();
    let _ = tokio::task::spawn_blocking(move || super::repos_refresh(&sh)).await;
    Ok(Json(listing(&s)?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetChatRepo {
    #[serde(default)]
    repo_id: Option<Uuid>,
    #[serde(default)]
    base_branch: String,
}

pub async fn set_chat_repo(State(s): S, peer: Option<Extension<TunnelPeer>>, Path(id): Path<String>, Json(req): Json<SetChatRepo>) -> ApiResult<Json<Value>> {
    let chat = load_chat(&s, &id)?;
    ensure_can_write(&s, Some(&chat))?;
    owner_gate(&s, peer.as_deref())?;
    s.lock().set_chat_repo(chat.id, chat.workspace_id, req.repo_id, &req.base_branch)?;
    s.after_write();
    Ok(Json(chat_repo(&s, chat.id)?))
}

/// A chat's workspace repo for `GET /v1/chats/:id/details` (`workspaceRepo`).
pub fn chat_repo(s: &Shared, chat: Uuid) -> ApiResult<Value> {
    let svc = s.lock();
    let Some(c) = svc.load_resolving(chat)? else { return Ok(Value::Null) };
    let Some(cr) = c.repo.clone().filter(|r| r.repo_id.is_some()) else { return Ok(Value::Null) };
    let roster = svc.workspace_roster(s.ws)?;
    let id = cr.repo_id.unwrap_or_default();
    let entry = roster.repos.get(&id);
    let gate = rr::turn_checkout(svc.store(), svc.device_id(), &s.opts.label, &c, &roster, &rr::cloning());
    let (state, reason) = match gate {
        rr::TurnCheckout::Ready(_) => ("present", String::new()),
        rr::TurnCheckout::Cloning { .. } => ("cloning", String::new()),
        rr::TurnCheckout::Refused { code, reason } => (code, reason),
        rr::TurnCheckout::NoRepo => ("none", String::new()),
    };
    Ok(json!({
        "id": id,
        "name": entry.map(|e| shown(&e.repo.display_name)),
        "canonical": entry.map(|e| e.repo.canonical.clone()),
        "removed": entry.is_some_and(|e| e.removed),
        "scratch": entry.is_some_and(|e| e.repo.is_scratch()),
        "baseBranch": cr.base_branch,
        "basePinned": cr.base_commit.is_some(),
        "here": { "state": state, "reason": reason },
    }))
}

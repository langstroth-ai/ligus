//! Workspace repositories on the desktop (docs/design/v2/repos-and-checkouts.md
//! 6.1): Settings → Repositories, the chat's repo chip, the "Add WeFit to the
//! workspace?" prompt after binding a folder, and the turn gate the desktop
//! runs before every turn in a chat with a repo.
//!
//! Only this device's own paths are ever shown; other hosts appear by label
//! and coverage.

use std::path::{Path, PathBuf};

use hive_core::ChatSession;
use hive_runtime::event_store::CheckoutSource;
use hive_runtime::repos::{self as rr, TurnCheckout};
use serde_json::{json, Value};
use tauri::State;
use uuid::Uuid;

use crate::{map_err, AppState};

/// This desktop's host label (its computer name).
pub(crate) fn host_label() -> String {
    static L: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    L.get_or_init(|| crate::computer_name().unwrap_or_else(|| "this device".into())).clone()
}

/// Whether this desktop reports coverage by default in `ws`: when its
/// person owns the workspace (review Q6; members' own machines opt in).
fn coverage_default(state: &AppState, ws: Uuid) -> bool {
    let svc = state.service.lock().unwrap();
    let me = svc.author().id.clone();
    svc.workspace_roster(ws).ok().and_then(|r| r.role_of(&me)).is_some_and(|r| r == hive_core::WorkspaceRole::Owner)
}

/// This desktop's coverage of the active workspace's repos, for its beat.
pub(crate) fn beat_coverage(state: &AppState, ws: Uuid) -> Vec<hive_core::workspace_repo::CoverageEntry> {
    let default_on = coverage_default(state, ws);
    let svc = state.service.lock().unwrap();
    if !rr::coverage_reporting(svc.store(), ws, default_on) {
        return Vec::new();
    }
    let Ok(roster) = svc.workspace_roster(ws) else { return Vec::new() };
    rr::coverage(svc.store(), ws, &rr::live_repos(&roster), &rr::cloning())
}

/// Map the workspace's bound folder onto the workspace repo its remote is
/// (locally; nothing is published, review F7/F16). Idempotent.
pub(crate) fn migrate_bound_folder(state: &AppState, ws: Uuid) {
    let folder = crate::workspace_dir_for(&state.settings.lock().unwrap().workspace_dirs, &ws.to_string());
    if folder.trim().is_empty() || !hive_runtime::git_worktree::is_git_repo(Path::new(&folder)) {
        return;
    }
    let svc = state.service.lock().unwrap();
    let Ok(roster) = svc.workspace_roster(ws) else { return };
    let aliases = svc.store().ssh_host_aliases().unwrap_or_default();
    let path = PathBuf::from(&folder);
    if let Some(repo) = rr::live_repos(&roster).into_iter().find(|r| rr::checkout_matches(&path, r, &aliases).is_some()) {
        if svc.store().repo_mappings(ws).unwrap_or_default().iter().any(|m| m.repo_id == repo.id) {
            return;
        }
        if let Err(e) = rr::bind(svc.store(), svc.device_id(), &host_label(), ws, &repo, &path, CheckoutSource::Migrated) {
            tracing::info!(target: "repos", "the bound folder isn't usable for {}: {e}", repo.display_name);
        }
    }
}

/// A turn's repo on this desktop.
pub(crate) struct DeskRepoTurn {
    pub checkout: PathBuf,
    pub chat_worktree: PathBuf,
    pub tip: String,
    pub base_branch: String,
    pub context: String,
}

/// The desktop's host-side gate (5.3, review F8): `Ok(None)` for a chat
/// without a repo (today's behaviour), `Ok(Some)` for one ready on this
/// device (the chat branch cut from its pinned base and brought up to date
/// with its ledger; a base this device just cut is pinned), `Err` with the
/// reason otherwise. The turn is already claimed when this runs.
pub(crate) fn turn_repo(state: &AppState, session: &ChatSession) -> Result<Option<DeskRepoTurn>, String> {
    if session.repo.as_ref().and_then(|r| r.repo_id).is_none() {
        return Ok(None);
    }
    migrate_bound_folder(state, session.workspace_id);
    let label = host_label();
    let gate = {
        let svc = state.service.lock().unwrap();
        let roster = svc.workspace_roster(session.workspace_id).map_err(map_err)?;
        rr::turn_checkout(svc.store(), svc.device_id(), &label, session, &roster, &rr::cloning())
    };
    let ready = match gate {
        TurnCheckout::NoRepo => return Ok(None),
        TurnCheckout::Ready(r) => r,
        TurnCheckout::Cloning { repo_name } => return Err(format!("{repo_name} is still cloning on {label}; ask again when it finishes")),
        TurnCheckout::Refused { reason, .. } => return Err(reason),
    };
    let prepared = rr::prepare_chat(&ready, session, &label)?;
    if let Some(pin) = &prepared.pin {
        if let Err(e) = state.service.lock().unwrap().pin_chat_base(session.id, session.workspace_id, ready.repo.id, pin) {
            tracing::warn!(target: "repos", "couldn't pin the chat's base: {e}");
        }
    }
    let base_branch = session.repo.as_ref().map(|r| r.base_branch.clone()).unwrap_or_default();
    let context = rr::turn_context(&TurnCheckout::Ready(ready.clone()), session.id, &base_branch);
    Ok(Some(DeskRepoTurn { checkout: ready.path, chat_worktree: prepared.chat_worktree, tip: prepared.tip, base_branch, context }))
}

/// The checkout a chat's files come from on this desktop, for the editor and
/// the file tree: the chat's repo checkout when it has one here, else `None`
/// (the workspace's bound folder).
pub(crate) fn chat_checkout(state: &AppState, session_id: Uuid) -> Option<PathBuf> {
    let svc = state.service.lock().unwrap();
    let s = svc.load(session_id).ok().flatten()?;
    let repo = s.repo.as_ref().and_then(|r| r.repo_id)?;
    svc.store().chat_checkout(session_id, repo).ok().flatten().or_else(|| svc.store().primary_checkout(s.workspace_id, repo).ok().flatten().map(|m| m.path))
}

fn ws_of(state: &AppState) -> Uuid {
    state.active_workspace_id()
}

/// Settings → Repositories: the workspace's repos, this device's checkout
/// of each (its own path), every other host's coverage, whether this person
/// may add or remove, and a suggestion from the bound folder.
#[tauri::command]
pub fn list_workspace_repos(state: State<AppState>) -> Result<Value, String> {
    let ws = ws_of(&state);
    migrate_bound_folder(&state, ws);
    let folder = crate::workspace_dir_for(&state.settings.lock().unwrap().workspace_dirs, &ws.to_string());
    let svc = state.service.lock().unwrap();
    let roster = svc.workspace_roster(ws).map_err(map_err)?;
    let me = svc.author().id.clone();
    let can_admin = roster.role_of(&me).is_some_and(|r| r.rank() >= hive_core::WorkspaceRole::Admin.rank());
    let hosts = svc.load(hive_core::workspace_config_session_id(ws)).map_err(map_err)?.map(|c| c.workspace_hosts).unwrap_or_default();
    let my_host = svc.device_id().to_string();
    let reports = rr::host_reports(&roster, &hosts, &state.host_presence);
    let entries = rr::workspace_repos(&roster);
    let live: Vec<_> = entries.iter().filter(|e| !e.removed).map(|e| e.repo.clone()).collect();
    let mine = rr::coverage(svc.store(), ws, &live, &rr::cloning());
    let maps = svc.store().repo_mappings(ws).map_err(map_err)?;
    let repos: Vec<Value> = entries
        .iter()
        .map(|e| {
            let r = &e.repo;
            let here = mine.iter().find(|c| c.repo_id == r.id);
            let path = maps.iter().find(|m| m.repo_id == r.id && m.confirmed).map(|m| m.path.to_string_lossy().into_owned());
            let found: Vec<String> = maps.iter().filter(|m| m.repo_id == r.id && !m.confirmed).map(|m| m.path.to_string_lossy().into_owned()).collect();
            let hosts: Vec<Value> = reports
                .iter()
                .filter(|h| h.host_id != my_host)
                .map(|h| {
                    let c = h.coverage.as_ref().map(|c| c.iter().find(|x| x.repo_id == r.id).cloned());
                    let (st, fork) = match c {
                        None => ("unknown".to_string(), false),
                        Some(None) => ("missing".to_string(), false),
                        Some(Some(x)) => (x.state.clone(), x.via_fork),
                    };
                    json!({ "hostId": h.host_id, "label": h.label, "kind": h.kind, "online": h.online, "state": st, "viaFork": fork })
                })
                .collect();
            json!({
                "id": r.id, "name": r.display_name, "canonical": r.canonical, "forgeKind": r.forge_kind,
                "defaultBranch": r.default_branch, "removed": e.removed,
                "here": {
                    "state": here.map(|c| c.state.as_str()).unwrap_or(if e.removed { "removed" } else { "missing" }),
                    "code": here.map(|c| c.code.as_str()).unwrap_or(""),
                    "viaFork": here.is_some_and(|c| c.via_fork),
                    "path": path, "found": found,
                },
                "hosts": hosts,
            })
        })
        .collect();
    // "This folder is github.com/zazulam/wefit. Add WeFit to the workspace?"
    let aliases = svc.store().ssh_host_aliases().unwrap_or_default();
    let suggestion = (!folder.trim().is_empty())
        .then(|| rr::git::suggested_canonical(Path::new(&folder), &aliases))
        .flatten()
        .filter(|(c, _)| !live.iter().any(|r| r.canonical == *c || r.canonical == c.to_ascii_lowercase()))
        .map(|(c, fork)| {
            let name = Path::new(&folder).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            json!({ "canonical": c, "name": name, "viaFork": fork, "path": folder })
        });
    let coverage_on = rr::coverage_reporting(svc.store(), ws, roster.role_of(&me) == Some(hive_core::WorkspaceRole::Owner));
    Ok(json!({ "repos": repos, "canAdmin": can_admin, "suggestion": suggestion, "hostLabel": host_label(), "coverageReporting": coverage_on, "scanRoots": svc.store().scan_roots().unwrap_or_default() }))
}

/// Add a repo (Admin; the passkey prompt comes from the service). With
/// `path`, the folder is bound for it on this device too.
#[tauri::command]
pub fn add_workspace_repo(state: State<AppState>, input: String, name: Option<String>, kind: Option<String>, branch: Option<String>, path: Option<String>) -> Result<Value, String> {
    let ws = ws_of(&state);
    let dir = path.as_deref().map(str::trim).filter(|p| !p.is_empty()).map(PathBuf::from);
    let root = dir.as_deref().and_then(rr::git::root_commit);
    let branch = branch.filter(|b| !b.trim().is_empty()).or_else(|| {
        dir.as_deref()
            .and_then(|d| rr::git::git(d, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]).ok())
            .and_then(|b| b.strip_prefix("origin/").map(str::to_string))
    });
    let kind = kind.filter(|k| !k.trim().is_empty()).unwrap_or_else(|| guess_kind(&input));
    let repo = state
        .service
        .lock()
        .unwrap()
        .add_workspace_repo(ws, &input, name.as_deref().unwrap_or(""), &kind, branch.as_deref().unwrap_or(""), root)
        .map_err(map_err)?;
    if let Some(d) = dir {
        let svc = state.service.lock().unwrap();
        rr::bind(svc.store(), svc.device_id(), &host_label(), ws, &repo, &d, CheckoutSource::Bound).map_err(map_err)?;
    }
    Ok(json!({ "id": repo.id, "canonical": repo.canonical, "name": repo.display_name }))
}

fn guess_kind(input: &str) -> String {
    let host = hive_core::workspace_repo::canonical_repo(input, "", &Default::default())
        .ok()
        .unwrap_or_else(|| input.to_string())
        .split('/')
        .next()
        .unwrap_or("")
        .to_string();
    hive_forge::conn::known_kind(&host).map(|(k, _)| k.as_str().to_string()).unwrap_or_default()
}

fn parse(id: &str) -> Result<Uuid, String> {
    Uuid::parse_str(id.trim()).map_err(map_err)
}

#[tauri::command]
pub fn rename_workspace_repo(state: State<AppState>, repo_id: String, name: String, branch: Option<String>) -> Result<(), String> {
    let ws = ws_of(&state);
    let id = parse(&repo_id)?;
    let mut svc = state.service.lock().unwrap();
    let branch = match branch {
        Some(b) => b,
        None => svc.workspace_roster(ws).map_err(map_err)?.repos.get(&id).map(|e| e.repo.default_branch.clone()).unwrap_or_default(),
    };
    svc.update_workspace_repo(ws, id, &name, &branch).map_err(map_err)
}

#[tauri::command]
pub fn remove_workspace_repo(state: State<AppState>, repo_id: String) -> Result<(), String> {
    let ws = ws_of(&state);
    state.service.lock().unwrap().remove_workspace_repo(ws, parse(&repo_id)?).map_err(map_err)
}

fn live_repo(state: &AppState, ws: Uuid, id: Uuid) -> Result<hive_core::workspace_repo::WorkspaceRepo, String> {
    let roster = state.service.lock().unwrap().workspace_roster(ws).map_err(map_err)?;
    roster.repos.get(&id).filter(|e| !e.removed).map(|e| e.repo.clone()).ok_or_else(|| "no such repository in this workspace".into())
}

/// Bind a folder on this device for a repo ("Bind folder…"), or confirm a
/// found one ("Use it").
#[tauri::command]
pub fn bind_repo_checkout(state: State<AppState>, repo_id: String, path: String) -> Result<(), String> {
    let ws = ws_of(&state);
    let repo = live_repo(&state, ws, parse(&repo_id)?)?;
    let svc = state.service.lock().unwrap();
    rr::bind(svc.store(), svc.device_id(), &host_label(), ws, &repo, Path::new(path.trim()), CheckoutSource::Bound).map(|_| ()).map_err(map_err)
}

#[tauri::command]
pub fn unbind_repo_checkout(state: State<AppState>, repo_id: String, path: String) -> Result<(), String> {
    let ws = ws_of(&state);
    let svc = state.service.lock().unwrap();
    rr::unbind(svc.store(), svc.device_id(), ws, parse(&repo_id)?, Path::new(&path)).map_err(map_err)
}

/// Scan this device's roots (adding `root` first when given).
#[tauri::command]
pub fn rescan_repos(state: State<AppState>, root: Option<String>) -> Result<usize, String> {
    let ws = ws_of(&state);
    let svc = state.service.lock().unwrap();
    if let Some(r) = root.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        let p = PathBuf::from(r);
        if rr::scan::is_shallow_or_system_dir(&p) {
            return Err(format!("{r} is too broad to scan; pick a folder of checkouts"));
        }
        svc.store().add_scan_root(&p).map_err(map_err)?;
    }
    let roster = svc.workspace_roster(ws).map_err(map_err)?;
    rr::detect(svc.store(), ws, &roster, &[]).map(|f| f.len()).map_err(map_err)
}

#[tauri::command]
pub fn set_repo_coverage_reporting(state: State<AppState>, on: Option<bool>) -> Result<(), String> {
    let ws = ws_of(&state);
    let svc = state.service.lock().unwrap();
    rr::set_coverage_reporting(svc.store(), ws, on).map_err(map_err)
}

/// The chat's repo chip: the repo, its base branch, and whether this device
/// can run a turn in it (and why not).
#[tauri::command]
pub fn chat_workspace_repo(state: State<AppState>, session_id: String) -> Result<Value, String> {
    let sid = parse(&session_id)?;
    let svc = state.service.lock().unwrap();
    let Some(s) = svc.load(sid).map_err(map_err)? else { return Ok(Value::Null) };
    let Some(cr) = s.repo.clone().filter(|r| r.repo_id.is_some()) else { return Ok(Value::Null) };
    let roster = svc.workspace_roster(s.workspace_id).map_err(map_err)?;
    let id = cr.repo_id.unwrap_or_default();
    let entry = roster.repos.get(&id);
    let (st, reason) = match rr::turn_checkout(svc.store(), svc.device_id(), &host_label(), &s, &roster, &rr::cloning()) {
        TurnCheckout::Ready(_) => ("present", String::new()),
        TurnCheckout::Cloning { .. } => ("cloning", String::new()),
        TurnCheckout::Refused { code, reason } => (code, reason),
        TurnCheckout::NoRepo => ("none", String::new()),
    };
    Ok(json!({
        "id": id,
        "name": entry.map(|e| e.repo.display_name.clone()),
        "canonical": entry.map(|e| e.repo.canonical.clone()),
        "removed": entry.is_some_and(|e| e.removed),
        "baseBranch": cr.base_branch,
        "basePinned": cr.base_commit.is_some(),
        "here": { "state": st, "reason": reason },
    }))
}

/// Point a chat at a workspace repo (`None`: no repo), Contributor+.
#[tauri::command]
pub fn set_chat_workspace_repo(state: State<AppState>, session_id: String, repo_id: Option<String>, base_branch: Option<String>) -> Result<(), String> {
    let sid = parse(&session_id)?;
    let repo = repo_id.as_deref().map(parse).transpose()?;
    let mut svc = state.service.lock().unwrap();
    let ws = svc.load(sid).map_err(map_err)?.map(|s| s.workspace_id).ok_or("unknown chat")?;
    svc.set_chat_repo(sid, ws, repo, base_branch.as_deref().unwrap_or("")).map_err(map_err)
}

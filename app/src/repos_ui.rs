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
    let (maps, aliases, repos) = {
        let svc = state.service.lock().unwrap();
        if !rr::coverage_reporting(svc.store(), ws, default_on) {
            return Vec::new();
        }
        let Ok(roster) = svc.workspace_roster(ws) else { return Vec::new() };
        let repos = rr::live_repos(&roster);
        if repos.is_empty() {
            return Vec::new();
        }
        (svc.store().repo_mappings(ws).unwrap_or_default(), rr::aliases(svc.store(), ws, Some(&roster)), repos)
    };
    // Git runs without the service lock.
    rr::coverage_from(&maps, &aliases, &repos, &rr::cloning())
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
    let aliases = rr::aliases(svc.store(), ws, Some(&roster));
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

/// This device's checkout of `session`'s repo when it passes the turn gate
/// (still that repo by its remote, managed by this host), without cutting
/// or preparing the chat branch: for read-only probes such as "Ask the
/// agent to rebase".
pub(crate) fn gated_checkout(state: &AppState, session: &ChatSession) -> Result<PathBuf, String> {
    let label = host_label();
    let svc = state.service.lock().unwrap();
    let roster = svc.workspace_roster(session.workspace_id).map_err(map_err)?;
    match rr::turn_checkout(svc.store(), svc.device_id(), &label, session, &roster, &rr::cloning()) {
        TurnCheckout::Ready(r) => Ok(r.path),
        TurnCheckout::NoRepo => Err("this chat has no repository".into()),
        TurnCheckout::Cloning { repo_name } => Err(format!("{repo_name} is still cloning on {label}")),
        TurnCheckout::Refused { reason, .. } => Err(reason),
    }
}

/// The folder the window shows for a repo chat with `checkout` here: its
/// chat worktree once its branch exists (a turn cuts it from the pinned
/// base), else the checkout itself. Viewing never cuts a chat branch: one
/// cut from this device's HEAD would never be pinned (review F3).
pub(crate) fn view_dir(checkout: &Path, session_id: Uuid) -> String {
    let chat = session_id.to_string();
    if hive_runtime::git_worktree::chat_branch_exists(checkout, &chat) {
        if let Ok(wt) = hive_runtime::git_worktree::chat_worktree(checkout, &chat) {
            return wt.to_string_lossy().into_owned();
        }
    }
    checkout.to_string_lossy().into_owned()
}

/// Advisory (coverage, review F8): whether `host_id` says it has no
/// checkout of `repo` (it reports coverage and the repo isn't present or
/// cloning there). A host that reports nothing is unknown, not lacking.
pub(crate) fn host_lacks_repo(state: &AppState, host_id: &str, repo: Uuid) -> bool {
    state
        .host_presence
        .coverage_of(host_id)
        .is_some_and(|c| !c.iter().any(|e| e.repo_id == repo && matches!(e.state.as_str(), "present" | "cloning")))
}

/// Authoritative for this device: whether a turn in `session` could run
/// here now (no repo, or this device's checkout of it is ready).
pub(crate) fn runs_here(state: &AppState, svc: &hive_runtime::ChatService, session: &ChatSession) -> bool {
    if session.repo.as_ref().and_then(|r| r.repo_id).is_none() {
        return true;
    }
    let _ = state;
    let Ok(roster) = svc.workspace_roster(session.workspace_id) else { return false };
    matches!(rr::turn_checkout(svc.store(), svc.device_id(), &host_label(), session, &roster, &rr::cloning()), TurnCheckout::Ready(_))
}

/// This device's coverage of `session`'s repo for the dispatcher (design
/// repos-and-checkouts 11.4): read-only, what its own gate would say.
pub(crate) fn dispatch_coverage(svc: &hive_runtime::ChatService, session: &ChatSession) -> hive_core::dispatch::Coverage {
    if session.repo.as_ref().and_then(|r| r.repo_id).is_none() {
        return hive_core::dispatch::Coverage::NoRepo;
    }
    let Ok(roster) = svc.workspace_roster(session.workspace_id) else { return hive_core::dispatch::Coverage::Missing };
    rr::dispatch_coverage(svc.store(), svc.device_id(), session, &roster, &rr::cloning())
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
    let mine = rr::coverage_in(svc.store(), ws, &roster, &rr::cloning());
    let maps = svc.store().repo_mappings(ws).map_err(map_err)?;
    let looks = hive_core::workspace_repo::lookalikes(&live);
    let accepted = svc.store().accepted_aliases(ws).unwrap_or_default();
    let is_accepted = |kind: &str, alias: &str, target: &str| accepted.iter().any(|(k, a, t)| k == kind && a == alias && t == target);
    let al = rr::aliases(svc.store(), ws, Some(&roster));
    let repos: Vec<Value> = entries
        .iter()
        .map(|e| {
            let r = &e.repo;
            let lookalike_of = looks.get(&r.id).and_then(|o| live.iter().find(|x| x.id == *o)).map(|x| x.display_name.clone());
            let here = mine.iter().find(|c| c.repo_id == r.id);
            let confirmed = maps.iter().find(|m| m.repo_id == r.id && m.confirmed);
            let path = confirmed.map(|m| m.path.to_string_lossy().into_owned());
            // Matched only through an accepted synced alias (design 12).
            let via_alias = confirmed.and_then(|m| rr::checkout_match(&m.path, r, &al)).is_some_and(|(_, a)| a);
            let aliases: Vec<Value> = roster
                .repo_aliases
                .get(&r.id)
                .into_iter()
                .flatten()
                .map(|a| json!({ "alias": a, "accepted": is_accepted("repo", a, &r.id.to_string()) }))
                .collect();
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
                    "viaAlias": via_alias,
                    "path": path, "found": found,
                },
                "hosts": hosts,
                "lookalikeOf": lookalike_of,
                "aliases": aliases,
            })
        })
        .collect();
    // "This folder is github.com/zazulam/wefit. Add WeFit to the workspace?"
    let aliases = svc.store().ssh_host_aliases().unwrap_or_default();
    let suggestion = (!folder.trim().is_empty())
        .then(|| rr::git::suggested_canonical(Path::new(&folder), &aliases))
        .flatten()
        .filter(|(c, _)| !live.iter().any(|r| r.canonical == *c || r.canonical == c.to_ascii_lowercase()))
        .filter(|(c, _)| !roster.repo_aliases.values().flatten().any(|a| a == c || *a == c.to_ascii_lowercase()))
        .map(|(c, fork)| {
            let name = Path::new(&folder).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            json!({ "canonical": c, "name": name, "viaFork": fork, "path": folder })
        });
    let coverage_on = rr::coverage_reporting(svc.store(), ws, roster.role_of(&me) == Some(hive_core::WorkspaceRole::Owner));
    let host_aliases: Vec<Value> = roster
        .host_aliases
        .iter()
        .map(|(a, h)| json!({ "alias": a, "host": h, "accepted": is_accepted("host", a, h) }))
        .collect();
    Ok(json!({
        "repos": repos, "canAdmin": can_admin, "suggestion": suggestion, "hostLabel": host_label(), "coverageReporting": coverage_on,
        "scanRoots": svc.store().scan_roots().unwrap_or_default(), "hostAliases": host_aliases,
    }))
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

/// Set a repo's synced aliases (design repos-and-checkouts 12; Admin, the
/// passkey prompt comes from the service): other spellings of the same
/// repo, names only.
#[tauri::command]
pub fn set_repo_aliases(state: State<AppState>, repo_id: String, aliases: Vec<String>) -> Result<Vec<String>, String> {
    let ws = ws_of(&state);
    state.service.lock().unwrap().set_repo_aliases(ws, parse(&repo_id)?, &aliases).map_err(map_err)
}

/// Map a host alias to a repository host for the workspace, or clear it
/// with an empty `host` (Admin, with presence).
#[tauri::command]
pub fn set_host_alias(state: State<AppState>, alias: String, host: String) -> Result<(), String> {
    let ws = ws_of(&state);
    state.service.lock().unwrap().set_host_alias(ws, &alias, &host).map_err(map_err)
}

/// Accept a synced alias on this device (or stop using it). Device-local.
#[tauri::command]
pub fn accept_repo_alias(state: State<AppState>, alias: String, accept: bool, target: Option<String>) -> Result<String, String> {
    let ws = ws_of(&state);
    state.service.lock().unwrap().accept_alias_for(ws, &alias, target.as_deref(), accept).map_err(map_err)
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
    let here_ok = st == "present";
    let me = svc.device_id().to_string();
    let my_actor = svc.author().id.clone();
    // Re-pinning (review residual 3): an Admin's, or the chat's creator's.
    let admin = roster.role_of(&my_actor).is_some_and(|r| r.rank() >= hive_core::WorkspaceRole::Admin.rank());
    let can_repin = cr.base_commit.is_some() && (admin || (!s.creator_actor_id.is_empty() && s.creator_actor_id == my_actor));
    drop(svc);
    let pinned = pinned_elsewhere(&state, &s, id, &me, &my_actor, here_ok);
    Ok(json!({
        "id": id,
        "name": entry.map(|e| e.repo.display_name.clone()),
        "canonical": entry.map(|e| e.repo.canonical.clone()),
        "removed": entry.is_some_and(|e| e.removed),
        "baseBranch": cr.base_branch,
        "basePinned": cr.base_commit.is_some(),
        "baseCommit": cr.base_commit,
        "canRepin": can_repin,
        "here": { "state": st, "reason": reason },
        "pinnedElsewhere": pinned,
    }))
}

/// The composer's warning (6.1, 6.3): each of the chat's agents pinned to
/// another host that says it has no checkout of `repo` (advisory coverage;
/// that host refuses the turn anyway), with that host's label and whether
/// this device could take the turn this once instead (it has the repo, can
/// run the agent's runtime, and the agent is this person's).
fn pinned_elsewhere(state: &AppState, session: &ChatSession, repo: Uuid, me: &str, my_actor: &str, here_ok: bool) -> Vec<Value> {
    session
        .workspace_agents
        .iter()
        .filter(|a| !a.host_id.is_empty() && a.host_id != me && host_lacks_repo(state, &a.host_id, repo))
        .map(|a| {
            let label = session.workspace_hosts.iter().find(|h| h.id == a.host_id).map(|h| h.label.clone()).unwrap_or_else(|| a.host_id.chars().take(8).collect());
            let can_run_here = here_ok && a.owner_actor_id == my_actor && crate::flow::can_run_here(state, a);
            json!({ "agentId": a.id, "agentName": a.name, "hostId": a.host_id, "hostLabel": label, "canRunHere": can_run_here })
        })
        .collect()
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

/// Move a chat's pinned base (`ChatBaseRepinned`, review residual 3): to
/// `commit`, or with none to the tip of the chat's base branch (else the
/// repo's default) as this device's checkout has it. An Admin's or the
/// chat's creator's, with passkey presence; every host with the chat's
/// branch rebuilds it on the new base from the chat's ledger on its next
/// turn, or refuses that turn and says why. Returns the new base.
#[tauri::command]
pub fn repin_chat_base(state: State<AppState>, session_id: String, commit: Option<String>) -> Result<String, String> {
    let sid = parse(&session_id)?;
    let (s, roster) = {
        let svc = state.service.lock().unwrap();
        let s = svc.load(sid).map_err(map_err)?.ok_or("unknown chat")?;
        let roster = svc.workspace_roster(s.workspace_id).map_err(map_err)?;
        (s, roster)
    };
    let repo = s.repo.as_ref().and_then(|r| r.repo_id).ok_or("this chat has no repository")?;
    let commit = match commit.map(|c| c.trim().to_ascii_lowercase()).filter(|c| !c.is_empty()) {
        Some(c) => c,
        None => {
            let gate = {
                let svc = state.service.lock().unwrap();
                rr::turn_checkout(svc.store(), svc.device_id(), &host_label(), &s, &roster, &rr::cloning())
            };
            let TurnCheckout::Ready(ready) = gate else {
                return Err("this chat's repository isn't ready on this device; name a commit to pin instead".into());
            };
            let branch = s.repo.as_ref().map(|r| r.base_branch.clone()).unwrap_or_default();
            rr::resolve_base(&ready.path, &branch, &ready.repo, ready.via_fork).ok_or("no commit to pin on this device")?
        }
    };
    let mut svc = state.service.lock().unwrap();
    svc.repin_chat_base(sid, s.workspace_id, repo, &commit).map_err(map_err)?;
    Ok(commit)
}

/// Desktop "Clone into…" (4.3): clone workspace repo `repo_id` into
/// `parent/<name>-<id8>` through this device's connection to its host, on
/// the box's safe clone-by-id path: the URL rebuilt from the repo's
/// canonical (never a string from an event), the host's address re-checked
/// and pinned for git, credentials only through the connection's helper,
/// hardened git, no submodules. The clone is bound here as the repo's
/// checkout. Asking another host to clone stays on the phone (review Q9).
#[tauri::command]
pub async fn clone_workspace_repo(state: State<'_, AppState>, repo_id: String, parent: String) -> Result<Value, String> {
    let ws = ws_of(&state);
    let repo = live_repo(&state, ws, parse(&repo_id)?)?;
    let (url, pin, auth) = crate::forges::clone_source_for(&state.data_dir, &repo).await?;
    let parent = PathBuf::from(parent.trim());
    clone_repo_into(&state, ws, &repo, &url, &parent, pin.as_deref(), &auth).await
}

/// The rest of [`clone_workspace_repo`] once the source is known: clone,
/// bind (a checkout this device cloned), and say whether the claimed root
/// commit is there.
pub(crate) async fn clone_repo_into(
    state: &AppState,
    ws: Uuid,
    repo: &hive_core::workspace_repo::WorkspaceRepo,
    url: &str,
    parent: &Path,
    pin: Option<&str>,
    auth: &[String],
) -> Result<Value, String> {
    let _cloning = rr::start_cloning(repo.id);
    let (r, u, p, pin, auth) = (repo.clone(), url.to_string(), parent.to_path_buf(), pin.map(str::to_string), auth.to_vec());
    let cloned = tauri::async_runtime::spawn_blocking(move || rr::clone::clone_into(&r, &u, &p, pin.as_deref(), &auth)).await.map_err(map_err)??;
    {
        let svc = state.service.lock().unwrap();
        rr::bind(svc.store(), svc.device_id(), &host_label(), ws, repo, &cloned.path, CheckoutSource::Cloned).map_err(map_err)?;
    }
    if cloned.root_matches == Some(false) {
        tracing::warn!(target: "repos", "{} doesn't contain the root commit the workspace entry claims", repo.display_name);
    }
    Ok(json!({ "path": cloned.path.to_string_lossy(), "rootMatches": cloned.root_matches }))
}

/// "Open in editor" after an apply conflict: the chat's folder for this
/// repo on this device (its chat worktree, else the checkout), opened with
/// the OS default handler for folders. No shell, no editor command line,
/// and only a real folder inside the checkout ([`openable_dir`]).
#[tauri::command]
pub fn open_chat_checkout(state: State<AppState>, session_id: String) -> Result<String, String> {
    let sid = parse(&session_id)?;
    let checkout = chat_checkout(&state, sid).ok_or("this chat has no repository checkout on this device")?;
    let wt = hive_runtime::git_worktree::chat_worktree_path_of(&checkout, &sid.to_string()).filter(|p| std::fs::symlink_metadata(p).is_ok());
    let dir = openable_dir(&checkout, wt.as_deref())?;
    open_folder(&dir)?;
    Ok(dir.to_string_lossy().into_owned())
}

/// The folder "Open in editor" may hand to the OS opener: the chat's
/// worktree when there is one, else the checkout. The worktree must be a
/// real directory (never a link: a hostile repo can track a symlink at
/// `.ligus/worktrees/chat-<id>` pointing at an app) that resolves inside
/// the checkout. On macOS a package (an app, a workflow, anything with
/// `Contents/Info.plist`) is refused: `open` would launch it, not show it.
pub(crate) fn openable_dir(checkout: &Path, worktree: Option<&Path>) -> Result<PathBuf, String> {
    let root = rr::git::canonical(checkout);
    let dir = match worktree {
        Some(wt) => {
            let meta = std::fs::symlink_metadata(wt).map_err(|_| "that folder isn't there".to_string())?;
            if !meta.is_dir() {
                return Err("the chat's folder isn't a plain folder; open the checkout yourself".into());
            }
            let real = rr::git::canonical(wt);
            if !real.starts_with(&root) {
                return Err("the chat's folder isn't inside the checkout".into());
            }
            real
        }
        None => root,
    };
    if !dir.is_absolute() || !dir.is_dir() {
        return Err("that folder isn't there".into());
    }
    if looks_like_package(&dir) {
        return Err("that folder is an app or package; open it yourself".into());
    }
    Ok(dir)
}

/// A directory the macOS opener would launch or install rather than show.
fn looks_like_package(dir: &Path) -> bool {
    const LAUNCHABLE: &[&str] = &[
        "app", "bundle", "framework", "plugin", "kext", "appex", "prefpane", "saver", "workflow", "action", "mpkg", "pkg",
        "qlgenerator", "mdimporter", "service", "wdgt", "xpc", "systemextension", "dext",
    ];
    let ext = dir.extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
    ext.is_some_and(|e| LAUNCHABLE.contains(&e.as_str())) || dir.join("Contents").join("Info.plist").exists() || dir.join("Contents").join("MacOS").exists()
}

/// The OS's "open this folder" program and its one argument: the path,
/// never a shell string. On Windows the path is passed quoted as one raw
/// argument: `explorer` parses its own command line, where an unquoted
/// comma separates switches (`/root,`, `/select,`).
pub(crate) fn open_folder_command(dir: &Path) -> std::process::Command {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let program = "xdg-open";
    let mut c = hive_core::process_util::command(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let p = rr::git::simplify(dir.to_path_buf());
        c.raw_arg(format!("\"{}\"", p.to_string_lossy()));
    }
    #[cfg(not(target_os = "windows"))]
    c.arg(dir);
    c
}

type OpenHook = Box<dyn Fn(&Path) + Send + Sync>;
static OPEN_HOOK: std::sync::Mutex<Option<OpenHook>> = std::sync::Mutex::new(None);

/// Tests record what would open instead of opening it.
#[cfg(any(test, feature = "e2e"))]
pub(crate) fn set_open_hook(hook: Option<OpenHook>) {
    *OPEN_HOOK.lock().unwrap_or_else(|p| p.into_inner()) = hook;
}

fn open_folder(dir: &Path) -> Result<(), String> {
    if !dir.is_absolute() || !dir.is_dir() {
        return Err("that folder isn't there".into());
    }
    // An absolute path never reads as an option or a URL to the opener;
    // a quote never belongs in a folder name we open.
    let s = dir.to_string_lossy();
    if s.starts_with('-') || s.contains('"') || s.chars().any(char::is_control) {
        return Err("that folder's name can't be opened safely".into());
    }
    if let Some(hook) = OPEN_HOOK.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
        hook(dir);
        return Ok(());
    }
    open_folder_command(dir).spawn().map(|_| ()).map_err(map_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_folder_runs_the_os_opener_with_only_the_path() {
        let dir = std::env::temp_dir();
        let c = open_folder_command(&dir);
        let args: Vec<_> = c.get_args().collect();
        #[cfg(not(windows))]
        assert_eq!(args, vec![dir.as_os_str()]);
        // One raw argument: the path, quoted, so explorer never splits it.
        #[cfg(windows)]
        assert_eq!(args, vec![std::ffi::OsString::from(format!("\"{}\"", rr::git::simplify(dir.clone()).to_string_lossy())).as_os_str()]);
        let program = c.get_program().to_string_lossy().into_owned();
        assert!(["open", "explorer", "xdg-open"].contains(&program.as_str()), "{program}");
        assert!(!["sh", "bash", "cmd", "zsh"].contains(&program.as_str()));
    }

    /// Only a real folder inside the checkout opens: never a link (a hostile
    /// repo can track one at the chat worktree's path), never somewhere
    /// else, never an app or package the opener would launch.
    #[test]
    fn openable_dir_refuses_links_escapes_and_packages() {
        let t = std::env::temp_dir().join(format!("ligus-openable-{}", Uuid::new_v4().simple()));
        let checkout = t.join("co");
        let wt = checkout.join(".ligus/worktrees/chat-x");
        std::fs::create_dir_all(&wt).unwrap();
        assert_eq!(openable_dir(&checkout, Some(&wt)).unwrap(), rr::git::canonical(&wt));
        assert_eq!(openable_dir(&checkout, None).unwrap(), rr::git::canonical(&checkout));
        // A package inside the checkout.
        let app = checkout.join(".ligus/worktrees/chat-y");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(app.join("Contents/Info.plist"), "x").unwrap();
        assert!(openable_dir(&checkout, Some(&app)).is_err());
        let named = checkout.join(".ligus/worktrees/Evil.app");
        std::fs::create_dir_all(&named).unwrap();
        assert!(openable_dir(&checkout, Some(&named)).is_err());
        #[cfg(unix)]
        {
            let outside = t.join("outside");
            std::fs::create_dir_all(&outside).unwrap();
            let link = checkout.join(".ligus/worktrees/chat-z");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert!(openable_dir(&checkout, Some(&link)).is_err(), "a link is never opened");
            let inner = checkout.join(".ligus/worktrees/chat-w");
            std::os::unix::fs::symlink(&wt, &inner).unwrap();
            assert!(openable_dir(&checkout, Some(&inner)).is_err(), "not even one pointing inside");
        }
        let file = checkout.join("f");
        std::fs::write(&file, "x").unwrap();
        assert!(openable_dir(&checkout, Some(&file)).is_err());
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn open_folder_refuses_relative_or_missing_and_tests_never_spawn() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let s2 = seen.clone();
        set_open_hook(Some(Box::new(move |p: &Path| s2.lock().unwrap().push(p.to_path_buf()))));
        assert!(open_folder(Path::new("relative")).is_err());
        assert!(open_folder(&std::env::temp_dir().join("ligus-no-such-dir-xyz")).is_err());
        let t = std::env::temp_dir().join(format!("ligus-open-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&t).unwrap();
        open_folder(&t).unwrap();
        set_open_hook(None);
        let _ = std::fs::remove_dir_all(&t);
        assert_eq!(*seen.lock().unwrap(), vec![t]);
    }
}

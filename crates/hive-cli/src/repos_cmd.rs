//! Workspace repos on a box (repos-and-checkouts): the turn gate the worker
//! runs before every turn, the migration of `repo-bindings.toml`, and the
//! `ligus repos …` commands.


use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use hive_core::workspace_repo::{RepoEntry, WorkspaceRepo};
use hive_core::ChatSession;
use hive_runtime::event_store::CheckoutSource;
use hive_runtime::repos::{self as rr, TurnCheckout};
use hive_runtime::ChatService;
use uuid::Uuid;

use crate::repos::{repos_dir, resolve_path, RepoBindings};

/// How long a claimed turn waits for a clone of its chat's repo.
pub const CLONE_WAIT: Duration = Duration::from_secs(600);

/// Where a turn runs on this box and what its prompt says about it.
#[derive(Debug, Clone, Default)]
pub struct RepoTurn {
    pub repo_root: Option<PathBuf>,
    pub context: String,
    /// For a chat with a repo: (repo id, chat branch tip, base branch).
    pub base: Option<(Uuid, String, String)>,
}

fn lock(svc: &Mutex<ChatService>) -> std::sync::MutexGuard<'_, ChatService> {
    svc.lock().unwrap_or_else(|e| e.into_inner())
}

/// The host-side gate for a turn in `s` (review F8: this host is the only
/// gate). A chat without a repo keeps the box's legacy binding. A chat with
/// one runs only in this box's confirmed checkout of it, on the chat branch
/// brought up to date with the chat's ledger; while this box clones it the
/// (already claimed) turn waits, at most [`CLONE_WAIT`]. `Err` is the
/// refusal to post in the chat.
pub async fn repo_for_turn(data_dir: &Path, svc: &Mutex<ChatService>, s: &ChatSession, host_label: &str) -> std::result::Result<RepoTurn, String> {
    let start = std::time::Instant::now();
    loop {
        let gate = {
            let svc = lock(svc);
            let roster = svc.workspace_roster(s.workspace_id).map_err(|e| e.to_string())?;
            rr::turn_checkout(svc.store(), svc.device_id(), host_label, s, &roster, &rr::cloning())
        };
        match gate {
            TurnCheckout::NoRepo => {
                let repo_root = RepoBindings::load(data_dir).resolve(&repos_dir(data_dir), s.id);
                if let Some(r) = repo_root.as_deref().filter(|r| hive_runtime::git_worktree::is_git_repo(r)) {
                    // A legacy binding to a checkout another host on this
                    // machine manages (the desktop) never runs here: its
                    // chat worktrees and branches are that host's.
                    let me = lock(svc).device_id();
                    if let Some(label) = rr::marker::managed_elsewhere(r, me) {
                        return Err(format!("{} on {host_label} is managed by {label}; give this chat a workspace repository and clone it separately here", r.display()));
                    }
                    crate::repos::ensure_chat_base(r);
                }
                return Ok(RepoTurn { repo_root, context: crate::repos::turn_context(data_dir, s.id), base: None });
            }
            TurnCheckout::Cloning { repo_name } => {
                if start.elapsed() > CLONE_WAIT {
                    return Err(format!("{repo_name} is still cloning on {host_label}; ask again when it finishes"));
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            TurnCheckout::Refused { reason, .. } => return Err(reason),
            TurnCheckout::Ready(ready) => {
                let prepared = {
                    let ready = ready.clone();
                    let s = s.clone();
                    let label = host_label.to_string();
                    tokio::task::spawn_blocking(move || rr::prepare_chat(&ready, &s, &label)).await.map_err(|e| e.to_string())??
                };
                if let Some(pin) = &prepared.pin {
                    if let Err(e) = lock(svc).pin_chat_base(s.id, s.workspace_id, ready.repo.id, pin) {
                        eprintln!("repos: couldn't pin the chat's base: {e}");
                    }
                }
                let base_branch = s.repo.as_ref().map(|r| r.base_branch.clone()).unwrap_or_default();
                let context = rr::turn_context(&TurnCheckout::Ready(ready.clone()), s.id, &base_branch);
                return Ok(RepoTurn { repo_root: Some(ready.path.clone()), context, base: Some((ready.repo.id, prepared.tip, base_branch)) });
            }
        }
    }
}

/// Map the box's legacy bindings onto workspace repos, locally (review
/// F7, F16: nothing is published; the toml stays as it was, for a
/// downgrade). The workspace binding becomes a migrated mapping of the repo
/// its remote is, when that repo is in the workspace; a chat binding to
/// another checkout becomes that chat's override. Idempotent.
pub fn migrate_bindings(data_dir: &Path, svc: &ChatService, workspace_id: Uuid, host_label: &str) -> Result<usize> {
    let b = RepoBindings::load(data_dir);
    let roster = svc.workspace_roster(workspace_id)?;
    let repos = rr::live_repos(&roster);
    if repos.is_empty() {
        return Ok(0);
    }
    let dir = repos_dir(data_dir);
    let aliases = rr::aliases(svc.store(), workspace_id, Some(&roster));
    let mut n = 0;
    let known = svc.store().repo_mappings(workspace_id)?;
    let mut map = |raw: &str| -> Option<(Uuid, PathBuf)> {
        let path = resolve_path(&dir, raw);
        let canon = rr::git::canonical(&path);
        // Already mapped (the common case after the first run): no git.
        if let Some(m) = known.iter().find(|m| m.path == canon && m.confirmed) {
            return Some((m.repo_id, m.path.clone()));
        }
        let repo = repos.iter().find(|r| rr::checkout_matches(&path, r, &aliases).is_some())?;
        rr::bind(svc.store(), svc.device_id(), host_label, workspace_id, repo, &path, CheckoutSource::Migrated).ok()?;
        n += 1;
        Some((repo.id, path))
    };
    if let Some(w) = &b.workspace {
        map(w);
    }
    for (chat, raw) in &b.chats {
        let Ok(chat) = Uuid::parse_str(chat) else { continue };
        if let Some((repo, path)) = map(raw) {
            svc.store().set_chat_checkout(chat, repo, Some(&path))?;
        }
    }
    Ok(n)
}

/// After a clone by repo id: with no workspace binding yet, the clone
/// becomes the box's workspace default too, so a chat without a repo runs
/// in it as well (review F18; dual-written for a downgrade, F16).
pub fn bind_default_if_empty(data_dir: &Path, name: &str) -> Result<bool> {
    let mut b = RepoBindings::load(data_dir);
    if b.workspace.is_some() {
        return Ok(false);
    }
    b.workspace = Some(name.to_string());
    b.save(data_dir)?;
    Ok(true)
}

/// Find a workspace repo by id (or its prefix), display name or canonical.
pub fn find_repo(entries: &[RepoEntry], sel: &str) -> Result<WorkspaceRepo> {
    let sel = sel.trim();
    let lower = sel.to_ascii_lowercase();
    let hits: Vec<&RepoEntry> = entries
        .iter()
        .filter(|e| !e.removed)
        .filter(|e| {
            e.repo.id.to_string().starts_with(&lower)
                || e.repo.display_name.to_lowercase() == lower
                || e.repo.canonical == sel
                || e.repo.canonical.to_ascii_lowercase() == lower
        })
        .collect();
    match hits.as_slice() {
        [one] => Ok(one.repo.clone()),
        [] => bail!("no workspace repository matches '{sel}' (see `ligus repos`)"),
        _ => bail!("'{sel}' matches several repositories; use more of its id"),
    }
}

fn abs(p: &str) -> Result<PathBuf> {
    let p = PathBuf::from(p);
    let p = if p.is_absolute() { p } else { std::env::current_dir()?.join(p) };
    Ok(rr::git::canonical(&p))
}

/// `ligus repos [list | add | remove | rename | bind | unbind | confirm |
/// takeover | scan | roots | alias | aliases | repo-alias | host-alias |
/// accept | chat | coverage]`.
///
/// `alias` is this host's own SSH host alias (never synced); `repo-alias`
/// and `host-alias` are the workspace-synced ones (design
/// repos-and-checkouts 12: Admin with a passkey, names only), which this
/// host uses only after `accept`.
pub fn cmd(data_dir: &Path, svc: &mut ChatService, ws: Uuid, host_label: &str, pos: &[&str], flag: &dyn Fn(&str) -> Option<String>) -> Result<()> {
    let entries = || -> Result<Vec<RepoEntry>> { svc_entries(svc, ws) };
    match pos.first().copied() {
        None | Some("list") => {
            let es = entries()?;
            let maps = svc.store().repo_mappings(ws)?;
            if es.is_empty() {
                println!("This workspace has no repositories yet. Add one: ligus repos add <remote | path>");
            }
            for e in &es {
                let tag = if e.removed { " (removed)" } else { "" };
                println!("{}  {}  {}{tag}", &e.repo.id.to_string()[..8], e.repo.display_name, e.repo.canonical);
                for m in maps.iter().filter(|m| m.repo_id == e.repo.id) {
                    let state = if m.confirmed { if m.primary { "primary" } else { "confirmed" } } else { "found, unconfirmed" };
                    println!("    here: {} ({}, {state}{})", m.path.display(), m.source.as_str(), if m.via_fork { ", via a fork" } else { "" });
                }
            }
            println!();
            crate::repos::cmd_repos(data_dir)
        }
        Some("add") => {
            let what = pos.get(1).ok_or_else(|| anyhow!("usage: ligus repos add <remote | canonical | path> [--name N] [--kind K] [--branch B]"))?;
            let as_path = abs(what).ok().filter(|p| p.is_dir());
            let aliases = svc.store().ssh_host_aliases()?;
            let (input, root, branch) = match &as_path {
                Some(p) => {
                    let (c, _) = rr::git::suggested_canonical(p, &aliases).ok_or_else(|| anyhow!("{} has no forge remote", p.display()))?;
                    let branch = rr::git::git(p, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]).ok().and_then(|b| b.strip_prefix("origin/").map(str::to_string));
                    (c, rr::git::root_commit(p), branch)
                }
                None => (what.to_string(), None, None),
            };
            let kind = flag("--kind").unwrap_or_else(|| guess_kind(&input));
            let repo = svc.add_workspace_repo(ws, &input, &flag("--name").unwrap_or_default(), &kind, &flag("--branch").or(branch).unwrap_or_default(), root)?;
            println!("added {} ({}) to the workspace.", repo.display_name, repo.canonical);
            if let Some(p) = as_path {
                let m = rr::bind(svc.store(), svc.device_id(), host_label, ws, &repo, &p, CheckoutSource::Bound)?;
                println!("bound {} here.", m.path.display());
            }
            Ok(())
        }
        Some("remove") => {
            let r = find_repo(&entries()?, pos.get(1).ok_or_else(|| anyhow!("usage: ligus repos remove <repo>"))?)?;
            svc.remove_workspace_repo(ws, r.id)?;
            println!("removed {} from the workspace (its chats keep naming it).", r.display_name);
            Ok(())
        }
        Some("rename") => {
            let (Some(sel), Some(name)) = (pos.get(1), pos.get(2)) else { bail!("usage: ligus repos rename <repo> <name> [--branch B]") };
            let r = find_repo(&entries()?, sel)?;
            svc.update_workspace_repo(ws, r.id, name, &flag("--branch").unwrap_or(r.default_branch.clone()))?;
            println!("renamed {} to {name}.", r.display_name);
            Ok(())
        }
        Some(verb @ ("bind" | "confirm")) => {
            let (Some(sel), Some(path)) = (pos.get(1), pos.get(2)) else { bail!("usage: ligus repos {verb} <repo> <path>") };
            let r = find_repo(&entries()?, sel)?;
            let p = resolve_path(&repos_dir(data_dir), path);
            let m = rr::bind(svc.store(), svc.device_id(), host_label, ws, &r, &p, if verb == "bind" { CheckoutSource::Bound } else { CheckoutSource::Detected })?;
            println!("{} is {} on this host{}.", m.path.display(), r.display_name, if m.via_fork { " (through a fork)" } else { "" });
            Ok(())
        }
        Some("unbind") => {
            let r = find_repo(&entries()?, pos.get(1).ok_or_else(|| anyhow!("usage: ligus repos unbind <repo> [path]"))?)?;
            let maps = svc.store().repo_mappings(ws)?;
            for m in maps.iter().filter(|m| m.repo_id == r.id && pos.get(2).is_none_or(|p| resolve_path(&repos_dir(data_dir), p) == m.path)) {
                rr::unbind(svc.store(), svc.device_id(), ws, r.id, &m.path)?;
                println!("unbound {}.", m.path.display());
            }
            Ok(())
        }
        Some("takeover") => {
            let p = abs(pos.get(1).ok_or_else(|| anyhow!("usage: ligus repos takeover <path>"))?)?;
            rr::marker::take_over(&p, svc.device_id(), host_label)?;
            println!("this host now manages {}.", p.display());
            Ok(())
        }
        Some("scan") => {
            let roster = svc.workspace_roster(ws)?;
            let found = rr::detect(svc.store(), ws, &roster, &[repos_dir(data_dir)])?;
            for f in &found {
                let name = roster.repos.get(&f.repo_id).map(|e| e.repo.display_name.clone()).unwrap_or_default();
                println!("found {name} at {} (confirm: ligus repos confirm {} {})", f.path.display(), &f.repo_id.to_string()[..8], f.path.display());
            }
            if found.is_empty() {
                println!("no checkouts of this workspace's repositories under the scan roots.");
            }
            Ok(())
        }
        Some("roots") => match (pos.get(1).copied(), pos.get(2)) {
            (Some("add"), Some(p)) => {
                let p = abs(p)?;
                if rr::scan::is_shallow_or_system_dir(&p) {
                    bail!("{} is too broad to scan; pick a folder of checkouts", p.display());
                }
                svc.store().add_scan_root(&p)?;
                println!("scanning {} from now on (ligus repos scan).", p.display());
                Ok(())
            }
            (Some("remove"), Some(p)) => {
                svc.store().remove_scan_root(&abs(p)?)?;
                Ok(())
            }
            _ => {
                println!("{} (always)", repos_dir(data_dir).display());
                for r in svc.store().scan_roots()? {
                    println!("{}", r.display());
                }
                Ok(())
            }
        },
        Some("alias") => match (pos.get(1), pos.get(2)) {
            (Some(a), Some(h)) => {
                svc.store().set_ssh_host_alias(a, Some(h))?;
                println!("{a} means {h} here (identity only; git still connects to {a}).");
                Ok(())
            }
            (Some(a), None) if flag("--remove").is_some() => Ok(svc.store().set_ssh_host_alias(a, None)?),
            _ => {
                for (a, h) in svc.store().ssh_host_aliases()? {
                    println!("{a} -> {h}");
                }
                Ok(())
            }
        },
        Some("aliases") => {
            let roster = svc.workspace_roster(ws)?;
            let accepted = svc.store().accepted_aliases(ws)?;
            let used = |kind: &str, a: &str, t: &str| if accepted.iter().any(|(k, x, y)| k == kind && x == a && y == t) { "accepted here" } else { "not accepted here (ligus repos accept <alias>)" };
            for (id, list) in &roster.repo_aliases {
                let name = roster.repos.get(id).map(|e| e.repo.display_name.clone()).unwrap_or_default();
                for a in list {
                    println!("{a}  is {name}  ({})", used("repo", a, &id.to_string()));
                }
            }
            for (a, h) in &roster.host_aliases {
                println!("{a}  means {h}  ({})", used("host", a, h));
            }
            if roster.repo_aliases.is_empty() && roster.host_aliases.is_empty() {
                println!("This workspace has no synced aliases. This host's own SSH aliases: ligus repos alias");
            }
            Ok(())
        }
        Some("repo-alias") => {
            let r = find_repo(&entries()?, pos.get(1).ok_or_else(|| anyhow!("usage: ligus repos repo-alias <repo> [names…]   (no names clears the list)"))?)?;
            let names: Vec<String> = pos[2..].iter().map(|s| s.to_string()).collect();
            let set = svc.set_repo_aliases(ws, r.id, &names)?;
            if set.is_empty() {
                println!("{} has no aliases now (sync to share).", r.display_name);
            } else {
                println!("{} is also known as {} (sync to share; each host uses them after `ligus repos accept`).", r.display_name, set.join(", "));
            }
            Ok(())
        }
        Some("host-alias") => match (pos.get(1), pos.get(2)) {
            (Some(a), Some(h)) => {
                svc.set_host_alias(ws, a, h)?;
                println!("{a} means {h} for this workspace (identity only; sync to share; each host uses it after `ligus repos accept {a}`).");
                Ok(())
            }
            (Some(a), None) if flag("--remove").is_some() => {
                svc.set_host_alias(ws, a, "")?;
                println!("removed the host alias {a} (sync to share).");
                Ok(())
            }
            _ => bail!("usage: ligus repos host-alias <alias-host> <repository-host> | <alias-host> --remove"),
        },
        Some("accept") => {
            let a = pos.get(1).ok_or_else(|| anyhow!("usage: ligus repos accept <alias> [--revoke]"))?;
            let revoke = flag("--revoke").is_some();
            let what = svc.accept_alias(ws, a, !revoke)?;
            println!("{} on this host: {what} (identity only; git still connects where it did).", if revoke { "no longer used" } else { "accepted" });
            Ok(())
        }
        Some("chat") => {
            let (Some(chat), Some(sel)) = (pos.get(1), pos.get(2)) else { bail!("usage: ligus repos chat <chat-id> <repo | none> [--branch B]") };
            let chat = Uuid::parse_str(chat)?;
            let repo = if *sel == "none" { None } else { Some(find_repo(&entries()?, sel)?.id) };
            svc.set_chat_repo(chat, ws, repo, &flag("--branch").unwrap_or_default())?;
            println!("set (sync to share).");
            Ok(())
        }
        Some("coverage") => {
            let on = match pos.get(1).copied() {
                Some("on") => Some(true),
                Some("off") => Some(false),
                Some("default") => None,
                _ => bail!("usage: ligus repos coverage on|off|default"),
            };
            rr::set_coverage_reporting(svc.store(), ws, on)?;
            Ok(())
        }
        Some(other) => bail!("unknown `ligus repos {other}`"),
    }
}

fn svc_entries(svc: &ChatService, ws: Uuid) -> Result<Vec<RepoEntry>> {
    Ok(svc.workspace_repos(ws)?)
}

/// The forge kind a well-known host runs (a display hint and the case rule).
pub fn guess_kind(input: &str) -> String {
    let host = hive_core::workspace_repo::canonical_repo(input, "", &Default::default())
        .ok()
        .or_else(|| Some(input.to_string()))
        .and_then(|c| c.split('/').next().map(str::to_string))
        .unwrap_or_default();
    match hive_forge::conn::known_kind(&host) {
        Some((k, _)) => k.as_str().to_string(),
        None => String::new(),
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn e(c: &str, name: &str, removed: bool) -> RepoEntry {
        RepoEntry {
            repo: WorkspaceRepo { id: hive_core::workspace_repo::repo_id(c), canonical: c.into(), display_name: name.into(), forge_kind: String::new(), default_branch: String::new(), root_commit: None },
            removed,
        }
    }

    #[test]
    fn repos_are_found_by_name_canonical_or_id_prefix() {
        let es = [e("github.com/a/wefit", "WeFit", false), e("github.com/a/infra", "Infra", false), e("github.com/a/old", "Old", true)];
        assert_eq!(find_repo(&es, "wefit").unwrap().display_name, "WeFit");
        assert_eq!(find_repo(&es, "github.com/a/infra").unwrap().display_name, "Infra");
        assert_eq!(find_repo(&es, &es[0].repo.id.to_string()[..6]).unwrap().display_name, "WeFit");
        assert!(find_repo(&es, "old").is_err(), "removed repos aren't chosen");
        assert_eq!(guess_kind("git@github.com:a/b.git"), "github");
        assert_eq!(guess_kind("git.example.com/a/b"), "");
    }
}

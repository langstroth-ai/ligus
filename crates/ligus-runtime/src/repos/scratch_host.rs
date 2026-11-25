//! Scratch repositories on a person's own device (the desktop, `lt`, a
//! person's CLI device): the scratch folder ([`super::scratch`]) is
//! registered as a workspace repository flagged `scratch` (Admin, with
//! passkey presence under rules v2, exactly as adding any repository),
//! bound on this device, and set as the chat's repository. Agents then
//! work on the chat's own branch and worktree in it, with proposals,
//! approvals and the sandbox as for any repository.
//!
//! A box (`ligus serve`) is a Contributor service member and can't add
//! workspace repositories: it binds its scratch repositories device-locally
//! instead (`ligus-cli`'s `scratch_cmd`).

use std::path::{Path, PathBuf};

use ligus_core::workspace_repo::WorkspaceRepo;
use uuid::Uuid;

use super::scratch::{self, ScratchError, ScratchRepo, Settings};
use crate::chat_service::{ChatError, ChatService};
use crate::event_store::CheckoutSource;

/// What went wrong, as a sentence for the person.
#[derive(Debug)]
pub enum HostError {
    Scratch(ScratchError),
    /// The workspace refused (a role, presence, lockdown, a limit).
    Refused(String),
    Other(String),
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostError::Scratch(e) => e.fmt(f),
            HostError::Refused(m) | HostError::Other(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for HostError {}

impl From<ScratchError> for HostError {
    fn from(e: ScratchError) -> Self {
        HostError::Scratch(e)
    }
}

impl From<ChatError> for HostError {
    fn from(e: ChatError) -> Self {
        match e {
            ChatError::Unauthorized(m) => HostError::Refused(m),
            e => HostError::Other(e.to_string()),
        }
    }
}

type Result<T> = std::result::Result<T, HostError>;

/// The account id of this device's person (`""` when none), for the
/// per-member limit (identity model v2: who is an account id).
pub fn my_account(svc: &ChatService) -> String {
    svc.author().account_id.map(|a| a.to_string()).unwrap_or_default()
}

/// Create a scratch repository for `chat` (or none) on this device: the
/// folder, the workspace entry (`scratch: true`), the binding here, and the
/// chat's repository. Anything that fails after the folder exists removes
/// it again; nothing is half-made.
pub fn create(
    svc: &mut ChatService,
    workspace_id: Uuid,
    chat: Option<Uuid>,
    settings: &Settings,
    forbidden: &[PathBuf],
    name: &str,
    host_label: &str,
) -> Result<(ScratchRepo, WorkspaceRepo)> {
    let me = my_account(svc);
    let now = scratch::now();
    // The role first: a member who can't add repositories gets the reason
    // before anything is made on disk.
    let roster = svc.workspace_roster(workspace_id)?;
    let admin = roster.role_of(&svc.author().id).is_some_and(|r| r.rank() >= ligus_core::WorkspaceRole::Admin.rank());
    if !admin {
        return Err(HostError::Refused(
            "only an Admin can add a repository to this workspace, scratch ones too; ask an Admin, or use the phone's home host".into(),
        ));
    }
    scratch::reserve(svc.store(), settings, &me)?;
    let root = scratch::check_root(&settings.root, forbidden)?;
    let entry = scratch::init_dir(&root, workspace_id, name, &me, now)?;
    let made = (|| -> Result<(ScratchRepo, WorkspaceRepo)> {
        scratch::record(svc.store(), settings, &entry)?;
        let repo = match svc.add_scratch_workspace_repo(workspace_id, &entry.root_commit, &entry.name) {
            Ok(r) => r,
            Err(e) => {
                let _ = scratch::mark_removed(svc.store(), entry.id, now);
                return Err(e.into());
            }
        };
        let e = scratch::set_repo_id(svc.store(), entry.id, Some(repo.id))?;
        super::bind(svc.store(), svc.device_id(), host_label, workspace_id, &repo, &e.path, CheckoutSource::Bound)
            .map_err(|b| HostError::Other(b.to_string()))?;
        let e = match chat {
            Some(c) => {
                svc.set_chat_repo(c, workspace_id, Some(repo.id), "main")?;
                scratch::set_chat(svc.store(), e.id, c, true, now)?
            }
            None => e,
        };
        Ok((e, repo))
    })();
    if made.is_err() {
        scratch::discard_dir(&entry);
    }
    made
}

/// Point `chat` at an existing live scratch repository of this device.
pub fn use_in_chat(svc: &mut ChatService, workspace_id: Uuid, id: Uuid, chat: Uuid) -> Result<ScratchRepo> {
    let e = scratch::list(svc.store(), workspace_id).into_iter().find(|e| e.id == id && e.is_live()).ok_or(ScratchError::NotFound)?;
    let repo = e.repo_id.ok_or_else(|| HostError::Other("that scratch repository isn't in the workspace".into()))?;
    svc.set_chat_repo(chat, workspace_id, Some(repo), "main")?;
    Ok(scratch::set_chat(svc.store(), id, chat, true, scratch::now())?)
}

/// Keep a scratch repository: move it to `dest`, bind it there instead,
/// and (with Admin) drop the workspace's scratch flag. Returns the kept
/// entry and whether the flag was dropped workspace-wide.
pub fn keep(
    svc: &mut ChatService,
    workspace_id: Uuid,
    id: Uuid,
    dest: &Path,
    name: Option<&str>,
    forbidden: &[PathBuf],
    host_label: &str,
) -> Result<(ScratchRepo, bool)> {
    let e = scratch::list(svc.store(), workspace_id).into_iter().find(|e| e.id == id && e.is_live()).ok_or(ScratchError::NotFound)?;
    let new_path = scratch::keep_dir(&e, dest, forbidden)?;
    let kept = scratch::mark_kept(svc.store(), id, &new_path)?;
    let mut flag_dropped = false;
    if let Some(repo_id) = e.repo_id {
        let roster = svc.workspace_roster(workspace_id)?;
        if let Some(repo) = roster.repos.get(&repo_id).filter(|r| !r.removed).map(|r| r.repo.clone()) {
            let _ = super::unbind(svc.store(), svc.device_id(), workspace_id, repo_id, &e.path);
            super::bind(svc.store(), svc.device_id(), host_label, workspace_id, &repo, &new_path, CheckoutSource::Bound)
                .map_err(|b| HostError::Other(format!("moved to {}, but couldn't bind it there: {b}", new_path.display())))?;
            for c in &e.chats {
                if svc.store().chat_checkout(*c, repo_id).ok().flatten().is_some() {
                    let _ = svc.store().set_chat_checkout(*c, repo_id, Some(&new_path));
                }
            }
            flag_dropped = svc.keep_workspace_repo(workspace_id, repo_id, name).is_ok();
        }
    }
    Ok((kept, flag_dropped))
}

/// Remove a live scratch repository's folder (the cleanup, or a person's
/// "Remove now"), unbind it, and leave each of its chats here with no
/// repository and a note; history (proposals, diffs) stays in the chats.
/// The workspace entry stays (removing it needs an Admin's passkey); it
/// shows as scratch with no checkout.
pub fn remove(svc: &mut ChatService, workspace_id: Uuid, id: Uuid, host_label: &str, note: Option<&str>) -> Result<ScratchRepo> {
    let e = scratch::list(svc.store(), workspace_id).into_iter().find(|e| e.id == id && e.is_live()).ok_or(ScratchError::NotFound)?;
    scratch::remove_dir_checked(&e)?;
    let removed = scratch::mark_removed(svc.store(), id, scratch::now())?;
    if let Some(repo_id) = e.repo_id {
        let _ = super::unbind(svc.store(), svc.device_id(), workspace_id, repo_id, &e.path);
        for c in &e.chats {
            let on_it = svc.load_in(workspace_id, *c).ok().flatten().is_some_and(|s| s.repo.as_ref().and_then(|r| r.repo_id) == Some(repo_id));
            if on_it {
                let _ = svc.set_chat_repo(*c, workspace_id, None, "");
            }
            let text = note.map(str::to_string).unwrap_or_else(|| scratch::removed_text(&e, host_label));
            let _ = svc.post_system_note(*c, workspace_id, text);
        }
    }
    Ok(removed)
}

/// What one cleanup pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SweepReport {
    pub noticed: Vec<Uuid>,
    pub removed: Vec<Uuid>,
    pub failed: Vec<(Uuid, String)>,
}

/// One cleanup pass at `now`: notices to chats entering the last
/// [`scratch::NOTICE_DAYS`] days, removal of those past their time.
pub fn sweep(svc: &mut ChatService, workspace_id: Uuid, settings: &Settings, host_label: &str, now: i64) -> SweepReport {
    let mut out = SweepReport::default();
    let due = scratch::due(svc.store(), workspace_id, settings, now);
    for (e, x) in due.notice {
        let text = scratch::notice_text(&e, &x, host_label);
        for c in &e.chats {
            let _ = svc.post_system_note(*c, workspace_id, text.clone());
        }
        if scratch::mark_noticed(svc.store(), e.id, now).is_ok() {
            out.noticed.push(e.id);
        }
    }
    for e in due.remove {
        match remove(svc, workspace_id, e.id, host_label, None) {
            Ok(_) => out.removed.push(e.id),
            Err(err) => out.failed.push((e.id, err.to_string())),
        }
    }
    out
}

/// Publish a kept scratch repository as a new **private** repository on
/// `forge` and push its branches (`main` and the chats' branches). Only on
/// an explicit request whose `confirm` repeats `owner/name` exactly (what
/// the confirmation showed); never public, never a force push. Returns the
/// web URL.
pub async fn publish(forge: &dyn ligus_forge::Forge, dir: &Path, owner: &str, name: &str, confirm: &str) -> std::result::Result<String, String> {
    let owner = owner.trim();
    let name = name.trim();
    if confirm.trim() != format!("{owner}/{name}") {
        return Err("confirm the account and the repository name first (they must match what you confirmed)".into());
    }
    if !ligus_forge::spec::valid_segment(owner) || !ligus_forge::spec::valid_segment(name) || name.ends_with(".git") {
        return Err("not a valid account and repository name".into());
    }
    if !forge.has_write() {
        return Err(forge.signed_out_reason().unwrap_or_else(|| "this host has no write access to that code host; sign in first".into()));
    }
    if super::git::git(dir, &["remote", "get-url", "origin"]).is_ok() {
        return Err("it already has an origin remote; push it yourself".into());
    }
    let login = forge.whoami().await.map_err(|e| e.summary())?.map(|u| u.login).ok_or_else(|| "this host isn't signed in to that code host".to_string())?;
    let created = forge.create_private_repo(owner, &login, name).await.map_err(|e| e.summary())?;
    let url = forge.clone_url(&created, ligus_forge::Transport::Https);
    super::git::git(dir, &["remote", "add", "origin", &url]).map_err(|e| format!("created {}, but couldn't add it as origin: {e}", created.full_name()))?;
    let mut cmd = ligus_core::process_util::git();
    cmd.args(forge.git_auth_args()).arg("-C").arg(dir).args(["push", "--quiet", "origin", "refs/heads/*:refs/heads/*"]);
    let out = cmd.output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("created {}, but the push failed: {}", created.full_name(), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(format!("{}/{}", forge.web_base().trim_end_matches('/'), created.full_name()))
}

#[cfg(test)]
#[path = "scratch_host_tests.rs"]
mod tests;

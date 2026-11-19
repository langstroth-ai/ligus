//! Workspace repos and chat repos (repos-and-checkouts 3.1): the signed
//! side. Device-local mappings live in `crate::repos`.

use hive_core::authz::RosterState;
use hive_core::workspace_repo::{canonical_repo, display_name_ok, repo_id, RepoEntry, WorkspaceRepo};
use hive_core::SessionEvent;
use uuid::Uuid;

use super::{ChatError, ChatService, Result};

/// Read what a person typed as a repo: a remote URL in any git form, or an
/// already-canonical `host/owner/name`.
pub fn canonical_from_input(input: &str, forge_kind: &str, aliases: &std::collections::BTreeMap<String, String>) -> std::result::Result<String, String> {
    let raw = input.trim();
    if let Some(root) = raw.strip_prefix("local:") {
        let c = format!("local:{}", root.to_ascii_lowercase());
        return hive_core::workspace_repo::canonical_is_normal(&c, "").then_some(c).ok_or_else(|| "not a root commit id".into());
    }
    let looks_canonical = !raw.contains("://") && !raw.contains(':') && raw.split('/').next().is_some_and(|h| h.contains('.'));
    let remote = if looks_canonical { format!("https://{raw}") } else { raw.to_string() };
    canonical_repo(&remote, forge_kind, aliases).map_err(|e| e.to_string())
}

impl ChatService {
    /// The workspace roster at this device's config head.
    pub fn workspace_roster(&self, workspace_id: Uuid) -> Result<RosterState> {
        let (_, t) = self.store.load_config(workspace_id, self.trusted_founder.as_deref(), self.local_workspace_id)?;
        Ok(t.current().clone())
    }

    /// The workspace's repos (removed ones as tombstones).
    pub fn workspace_repos(&self, workspace_id: Uuid) -> Result<Vec<RepoEntry>> {
        Ok(crate::repos::workspace_repos(&self.workspace_roster(workspace_id)?))
    }

    /// Add a repo to the workspace (Admin, with passkey presence under
    /// rules v2). `input` is a remote in any git form or a canonical; the
    /// id is derived from the canonical, so a concurrent add of the same
    /// repo by someone else folds into one.
    pub fn add_workspace_repo(
        &mut self,
        workspace_id: Uuid,
        input: &str,
        display_name: &str,
        forge_kind: &str,
        default_branch: &str,
        root_commit: Option<String>,
    ) -> Result<WorkspaceRepo> {
        let aliases = self.store.ssh_host_aliases()?;
        let canonical = canonical_from_input(input, forge_kind, &aliases).map_err(ChatError::Unauthorized)?;
        let name = display_name.trim();
        let name = if name.is_empty() { canonical.rsplit('/').next().unwrap_or(&canonical).to_string() } else { name.to_string() };
        if !display_name_ok(&name) {
            return Err(ChatError::Unauthorized("bad repository name".into()));
        }
        let repo = WorkspaceRepo {
            id: repo_id(&canonical),
            canonical,
            display_name: name,
            forge_kind: forge_kind.trim().to_ascii_lowercase(),
            default_branch: default_branch.trim().to_string(),
            root_commit: root_commit.filter(|r| hive_core::workspace_repo::is_commit_id(r)),
        };
        hive_core::workspace_repo::repo_well_formed(&repo).map_err(|e| ChatError::Unauthorized(e.into()))?;
        let config = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(config, workspace_id, SessionEvent::WorkspaceRepoAdded { repo: repo.clone() })?;
        Ok(repo)
    }

    /// Rename a repo or change its default-branch hint (Admin; cosmetic, no
    /// presence).
    pub fn update_workspace_repo(&mut self, workspace_id: Uuid, repo: Uuid, display_name: &str, default_branch: &str) -> Result<()> {
        let config = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(
            config,
            workspace_id,
            SessionEvent::WorkspaceRepoUpdated { repo_id: repo, display_name: display_name.trim().into(), default_branch: default_branch.trim().into() },
        )?;
        Ok(())
    }

    /// Remove a repo (Admin, with presence). Chats keep naming it and show
    /// "removed repository"; a later add of the same canonical brings it
    /// back for them.
    pub fn remove_workspace_repo(&mut self, workspace_id: Uuid, repo: Uuid) -> Result<()> {
        let config = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(config, workspace_id, SessionEvent::WorkspaceRepoRemoved { repo_id: repo })?;
        Ok(())
    }

    /// Point a chat at a workspace repo (`None`: no repo) on `base_branch`
    /// ("" = the repo's default). A Contributor's act; never an agent's.
    pub fn set_chat_repo(&mut self, chat: Uuid, workspace_id: Uuid, repo: Option<Uuid>, base_branch: &str) -> Result<()> {
        let base_branch = base_branch.trim();
        if !base_branch.is_empty() && !hive_forge::spec::valid_git_ref(base_branch) {
            return Err(ChatError::Unauthorized("that isn't a branch name".into()));
        }
        self.append_signed(chat, workspace_id, SessionEvent::ChatRepoSet { repo_id: repo, base_branch: base_branch.into() })?;
        Ok(())
    }

    /// Pin the commit a chat's branch was cut from for `repo` (written by
    /// the host that cut it first), so every device replays the chat's
    /// applied proposals onto the same base.
    pub fn pin_chat_base(&mut self, chat: Uuid, workspace_id: Uuid, repo: Uuid, commit: &str) -> Result<()> {
        // Never as an agent or a runtime effect: the host's own act.
        let agent = self.acting_agent.take();
        let effect = std::mem::replace(&mut self.runtime_effect, false);
        let r = self.append_signed(chat, workspace_id, SessionEvent::ChatBasePinned { repo_id: repo, commit: commit.into() });
        self.acting_agent = agent;
        self.runtime_effect = effect;
        r.map(|_| ())
    }
}

impl ChatService {
    /// Move a chat's pinned base for `repo` to `commit` (`ChatBaseRepinned`,
    /// review residual 3): from the pin in force, as this person (never an
    /// agent or a runtime effect), an Admin's or the chat's creator's, with
    /// presence under rules v2. Every host with the chat's branch follows
    /// on its next turn (`repos::prepare_chat`) or refuses it visibly.
    pub fn repin_chat_base(&mut self, chat: Uuid, workspace_id: Uuid, repo: Uuid, commit: &str) -> Result<()> {
        if !hive_core::workspace_repo::is_commit_id(commit) {
            return Err(ChatError::Unauthorized("that isn't a commit id".into()));
        }
        let session = self.load(chat)?.ok_or_else(|| ChatError::Unauthorized("unknown chat".into()))?;
        if session.repo.as_ref().and_then(|r| r.repo_id) != Some(repo) {
            return Err(ChatError::Unauthorized("this chat doesn't work in that repository".into()));
        }
        let previous = session
            .repo_bases
            .get(&repo)
            .and_then(|h| h.last())
            .cloned()
            .ok_or_else(|| ChatError::Unauthorized("this chat has no pinned base yet: its first turn pins one".into()))?;
        if previous == commit {
            return Err(ChatError::Unauthorized("the chat is already pinned there".into()));
        }
        let agent = self.acting_agent.take();
        let effect = std::mem::replace(&mut self.runtime_effect, false);
        let r = self.append_signed(chat, workspace_id, SessionEvent::ChatBaseRepinned { repo_id: repo, commit: commit.into(), previous });
        self.acting_agent = agent;
        self.runtime_effect = effect;
        r.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EventStore;
    use hive_core::{ActorIdentity, ActorKind, SigningKeypair};

    fn svc() -> (ChatService, Uuid) {
        let account = Uuid::new_v4();
        let mut author = ActorIdentity::new(account.to_string(), "Ana", ActorKind::Human);
        author.account_id = Some(account);
        let mut s = ChatService::new(EventStore::open_in_memory().unwrap(), Uuid::new_v4(), SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), author);
        s.enable_test_presence();
        let ws = Uuid::new_v4();
        s.ensure_workspace_config(ws).unwrap();
        (s, ws)
    }

    #[test]
    fn add_set_pin_and_remove() {
        let (mut s, ws) = svc();
        let r = s.add_workspace_repo(ws, "git@github.com:ZaZulam/WeFit.git", "", "github", "main", None).unwrap();
        assert_eq!((r.canonical.as_str(), r.display_name.as_str()), ("github.com/zazulam/wefit", "wefit"));
        // Same repo again, typed as a canonical: already there.
        assert!(s.add_workspace_repo(ws, "github.com/zazulam/wefit", "WeFit", "", "", None).is_err());
        s.update_workspace_repo(ws, r.id, "WeFit", "main").unwrap();
        assert_eq!(s.workspace_repos(ws).unwrap()[0].repo.display_name, "WeFit");
        let chat = s.create_chat("c", ws, "anthropic").unwrap();
        s.set_chat_repo(chat.id, ws, Some(r.id), "main").unwrap();
        assert!(s.set_chat_repo(chat.id, ws, Some(r.id), "a..b").is_err());
        s.pin_chat_base(chat.id, ws, r.id, &"a".repeat(40)).unwrap();
        let c = s.load(chat.id).unwrap().unwrap();
        let cr = c.repo.unwrap();
        assert_eq!((cr.repo_id, cr.base_branch.as_str(), cr.base_commit.as_deref()), (Some(r.id), "main", Some("a".repeat(40).as_str())));
        // A second pin (another host, or a member) doesn't move it; a re-pin
        // by the chat's creator, with presence, does.
        s.pin_chat_base(chat.id, ws, r.id, &"b".repeat(40)).unwrap();
        assert_eq!(s.load(chat.id).unwrap().unwrap().repo.unwrap().base_commit, Some("a".repeat(40)));
        assert!(s.repin_chat_base(chat.id, ws, r.id, "HEAD").is_err());
        assert!(s.repin_chat_base(chat.id, ws, r.id, &"a".repeat(40)).is_err(), "already there");
        s.repin_chat_base(chat.id, ws, r.id, &"c".repeat(40)).unwrap();
        let c = s.load(chat.id).unwrap().unwrap();
        assert_eq!(c.repo.unwrap().base_commit, Some("c".repeat(40)));
        assert_eq!(c.repo_bases[&r.id], vec!["a".repeat(40), "c".repeat(40)]);
        s.remove_workspace_repo(ws, r.id).unwrap();
        assert!(s.workspace_repos(ws).unwrap()[0].removed);
        assert!(s.set_chat_repo(chat.id, ws, Some(r.id), "").is_err(), "a removed repo can't be chosen");
    }

    #[test]
    fn inputs_are_canonicalized_or_refused() {
        let none = Default::default();
        assert_eq!(canonical_from_input("github.com/A/B", "", &none).unwrap(), "github.com/a/b");
        assert_eq!(canonical_from_input("https://git.example.com/A/B.git", "", &none).unwrap(), "git.example.com/A/B");
        assert!(canonical_from_input("../../etc", "", &none).is_err());
        assert!(canonical_from_input("ext::sh -c id", "", &none).is_err());
        assert_eq!(canonical_from_input(&format!("local:{}", "A".repeat(40)), "", &none).unwrap(), format!("local:{}", "a".repeat(40)));
    }
}

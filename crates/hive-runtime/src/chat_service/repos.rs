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

    /// Set a repo's synced aliases (design 12; Admin, with presence under
    /// rules v2): other normal-form spellings of the same repo. Each input
    /// may be a remote in any git form or a canonical; the list replaces the
    /// repo's. Names only: no device uses one until a person there accepts it.
    pub fn set_repo_aliases(&mut self, workspace_id: Uuid, repo: Uuid, inputs: &[String]) -> Result<Vec<String>> {
        let roster = self.workspace_roster(workspace_id)?;
        let entry = roster.repos.get(&repo).filter(|e| !e.removed).ok_or_else(|| ChatError::Unauthorized("no such repository".into()))?;
        let mut aliases = Vec::new();
        for i in inputs.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            // Never through this device's own SSH aliases: the synced name is
            // the remote as written.
            let c = canonical_from_input(i, &entry.repo.forge_kind, &Default::default()).map_err(ChatError::Unauthorized)?;
            if !aliases.contains(&c) {
                aliases.push(c);
            }
        }
        hive_core::workspace_repo::repo_aliases_ok(&roster, repo, &aliases).map_err(ChatError::Unauthorized)?;
        let config = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(config, workspace_id, SessionEvent::WorkspaceRepoAliasesSet { repo_id: repo, aliases: aliases.clone() })?;
        Ok(aliases)
    }

    /// Map host `alias` to `host` (a live repo's canonical host) for the
    /// workspace, or clear it with `host` empty (design 12; Admin, with
    /// presence under rules v2).
    pub fn set_host_alias(&mut self, workspace_id: Uuid, alias: &str, host: &str) -> Result<()> {
        let (alias, host) = (alias.trim().to_ascii_lowercase(), host.trim().to_ascii_lowercase());
        let roster = self.workspace_roster(workspace_id)?;
        hive_core::workspace_repo::host_alias_ok(&roster, &alias, &host).map_err(ChatError::Unauthorized)?;
        let config = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(config, workspace_id, SessionEvent::WorkspaceHostAliasSet { alias, host })?;
        Ok(())
    }

    /// Accept (or take back) a synced alias on this device: `alias` is a
    /// repo alias (a canonical) or a host alias the workspace names now.
    /// Accepting records its current target, so a later change of target
    /// needs a new acceptance. Device-local; nothing is published.
    pub fn accept_alias(&mut self, workspace_id: Uuid, alias: &str, accept: bool) -> Result<String> {
        self.accept_alias_for(workspace_id, alias, None, accept)
    }

    /// [`Self::accept_alias`], refused when the workspace now names the alias
    /// with another target than `shown` (the repo id or host the person saw
    /// when they chose to accept it), so an Admin's change in between is
    /// never accepted unseen.
    pub fn accept_alias_for(&mut self, workspace_id: Uuid, alias: &str, shown: Option<&str>, accept: bool) -> Result<String> {
        let alias = alias.trim();
        let roster = self.workspace_roster(workspace_id)?;
        let changed = |target: &str| accept && shown.is_some_and(|s| !s.eq_ignore_ascii_case(target));
        if let Some(host) = roster.host_aliases.get(&alias.to_ascii_lowercase()) {
            if changed(host) {
                return Err(ChatError::Unauthorized(format!("{alias} now means {host}, not what was shown; look again before accepting")));
            }
            self.store.set_alias_accepted(workspace_id, "host", &alias.to_ascii_lowercase(), host, accept)?;
            return Ok(format!("{} means {host}", alias.to_ascii_lowercase()));
        }
        for (repo, list) in &roster.repo_aliases {
            if roster.repo_live(*repo) && list.iter().any(|a| a == alias) {
                if changed(&repo.to_string()) {
                    return Err(ChatError::Unauthorized(format!("{alias} now names another repository than was shown; look again before accepting")));
                }
                self.store.set_alias_accepted(workspace_id, "repo", alias, &repo.to_string(), accept)?;
                let name = roster.repos.get(repo).map(|e| e.repo.display_name.clone()).unwrap_or_default();
                return Ok(format!("{alias} is {name}"));
            }
        }
        if !accept {
            // Taking back an acceptance the workspace no longer names.
            for kind in ["host", "repo"] {
                self.store.set_alias_accepted(workspace_id, kind, alias, "", false)?;
            }
            return Ok(format!("{alias} is no longer accepted here"));
        }
        Err(ChatError::Unauthorized(format!("the workspace has no alias {alias}")))
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
    fn synced_aliases_are_set_checked_and_accepted_here() {
        let (mut s, ws) = svc();
        let r = s.add_workspace_repo(ws, "git@github.com:neworg/wefit.git", "WeFit", "github", "main", None).unwrap();
        let infra = s.add_workspace_repo(ws, "https://git.example.com/Ops/Infra", "Infra", "", "", None).unwrap();
        // Any git form is read as a canonical; duplicates collapse.
        let set = s.set_repo_aliases(ws, r.id, &["git@github.com:ZaZulam/WeFit.git".into(), "github.com/zazulam/wefit".into()]).unwrap();
        assert_eq!(set, vec!["github.com/zazulam/wefit".to_string()]);
        // Disjointness and look-alikes are refused before anything is signed.
        assert!(s.set_repo_aliases(ws, infra.id, &["github.com/zazulam/wefit".into()]).is_err());
        assert!(s.set_repo_aliases(ws, infra.id, &["github.com/neworg/wefit".into()]).is_err());
        assert!(s.set_repo_aliases(ws, r.id, &["github.com/neworg/wef1t".into()]).is_err());
        assert!(s.set_host_alias(ws, "github.com", "git.example.com").is_err());
        s.set_host_alias(ws, "SSH.git.example.com", "git.example.com").unwrap();
        let roster = s.workspace_roster(ws).unwrap();
        assert_eq!(roster.host_aliases["ssh.git.example.com"], "git.example.com");
        assert_eq!(roster.repo_aliases[&r.id], vec!["github.com/zazulam/wefit".to_string()]);
        // Nothing is used here until accepted; accepting records the target.
        assert!(crate::repos::aliases(s.store(), ws, Some(&roster)).repos.is_empty());
        s.accept_alias(ws, "github.com/zazulam/wefit", true).unwrap();
        s.accept_alias(ws, "ssh.git.example.com", true).unwrap();
        let al = crate::repos::aliases(s.store(), ws, Some(&roster));
        assert_eq!(al.repos[&r.id], vec!["github.com/zazulam/wefit".to_string()]);
        assert_eq!(al.hosts["ssh.git.example.com"], "git.example.com");
        assert!(s.accept_alias(ws, "github.com/nobody/x", true).is_err());
        // Review: what the person saw is what they accept.
        assert!(s.accept_alias_for(ws, "ssh.git.example.com", Some("git.other.example"), true).is_err());
        assert!(s.accept_alias_for(ws, "ssh.git.example.com", Some("git.example.com"), true).is_ok());
        s.accept_alias(ws, "ssh.git.example.com", false).unwrap();
        assert!(crate::repos::aliases(s.store(), ws, Some(&roster)).hosts.is_empty());
        // Clearing a host alias.
        s.set_host_alias(ws, "ssh.git.example.com", "").unwrap();
        assert!(s.workspace_roster(ws).unwrap().host_aliases.is_empty());
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

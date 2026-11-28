//! Typed views over the two kinds of log a workspace has.
//!
//! A workspace's **config log** (`workspace_config_session_id`) holds what
//! is workspace-wide: members, agents, runtimes, hosts, skills, vaults,
//! guards (dearth mode, lockdowns, auto-archive), profiles and device
//! labels. A **chat log** holds one chat: its messages, turns, proposals and
//! flow. Both fold into a [`ChatSession`]; these wrappers say which one a
//! value is and expose that log's own fields.
//!
//! What a reader of a chat sees is [`ChatState::view`]: the chat with the
//! workspace's config at the reader's frontier laid over it (members,
//! records, dearth mode, the chat's lockdown). Every reader goes through
//! that one function, so a chat's view never depends on which load path
//! built it.

use uuid::Uuid;

use crate::agent::WorkspaceAgent;
use crate::guard::GuardState;
use crate::identity::WorkspaceMember;
use crate::session::ChatSession;
use crate::turn::Turn;
use crate::workspace_host::WorkspaceHost;
use crate::workspace_runtime::WorkspaceRuntime;
use crate::ChatMessage;

/// A workspace's config log, folded.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceConfig(ChatSession);

impl WorkspaceConfig {
    /// `session`, folded from a workspace's config log.
    pub fn new(session: ChatSession) -> Self {
        Self(session)
    }

    pub fn members(&self) -> &[WorkspaceMember] {
        &self.0.members
    }
    pub fn agents(&self) -> &[WorkspaceAgent] {
        &self.0.workspace_agents
    }
    pub fn agent(&self, id: Uuid) -> Option<&WorkspaceAgent> {
        self.0.workspace_agents.iter().find(|a| a.id == id)
    }
    pub fn runtimes(&self) -> &[WorkspaceRuntime] {
        &self.0.workspace_runtimes
    }
    pub fn hosts(&self) -> &[WorkspaceHost] {
        &self.0.workspace_hosts
    }
    pub fn guards(&self) -> &GuardState {
        &self.0.guards
    }
    /// The fold itself, for readers not yet on the typed accessors.
    pub fn session(&self) -> &ChatSession {
        &self.0
    }
    pub fn into_session(self) -> ChatSession {
        self.0
    }
}

/// One chat's log, folded on its own (before the workspace's config is laid
/// over it).
#[derive(Debug, Clone, PartialEq)]
pub struct ChatState(ChatSession);

impl ChatState {
    /// `session`, folded from one chat's log.
    pub fn new(session: ChatSession) -> Self {
        Self(session)
    }

    pub fn id(&self) -> Uuid {
        self.0.id
    }
    pub fn messages(&self) -> &[ChatMessage] {
        &self.0.messages
    }
    /// [`Turn::of`] this chat.
    pub fn turn(&self, trigger: Uuid, agent: Option<Uuid>) -> Option<Turn> {
        Turn::of(&self.0, trigger, agent)
    }
    pub fn session(&self) -> &ChatSession {
        &self.0
    }

    /// What a reader of this chat sees: the chat with `config` (the
    /// workspace's config at the reader's frontier; `None` before the config
    /// log has synced) laid over it.
    ///
    /// - Members are workspace-wide: a config member replaces the chat's
    ///   entry of the same id; a member only the chat knows (an older
    ///   chat-level `MemberAdded`, the creator seed) stays, unless dearth mode
    ///   is on, which keeps only config members and the chat's creator.
    /// - Dearth mode, auto-archive and this chat's own lockdown come from
    ///   the config log; another chat's lockdown never shows here.
    /// - Agents, skills, vaults, runtimes and hosts are the union; a chat's
    ///   own older copy of a record keeps its place.
    pub fn view(self, config: Option<WorkspaceConfig>) -> ChatSession {
        let mut session = self.0;
        let Some(WorkspaceConfig(cfg)) = config else { return session };
        session.guards.dearth = cfg.guards.dearth.clone();
        session.guards.proposal_archive = cfg.guards.proposal_archive;
        session.guards.chat_id = Some(session.id);
        session.guards.lockdowns = cfg
            .guards
            .lockdowns
            .get(&session.id)
            .map(|l| std::iter::once((session.id, l.clone())).collect())
            .unwrap_or_default();
        if session.guards.dearth_on().is_some() {
            let creator = session.creator_actor_id.clone();
            session.members.retain(|m| m.id == creator || cfg.members.iter().any(|c| c.id == m.id));
        }
        for m in cfg.members {
            if let Some(slot) = session.members.iter_mut().find(|x| x.id == m.id) {
                *slot = m;
            } else {
                session.members.push(m);
            }
        }
        // Rules 4 (workspace model v3, 11, G5): a chat runs the workspace's
        // record of an agent; an older build's copy on the chat's own log
        // keeps folding but no longer shadows it.
        let workspace_wins = cfg.authz.rules >= crate::authz::RULES_V4;
        for a in cfg.workspace_agents {
            match session.workspace_agents.iter_mut().find(|x| x.id == a.id) {
                Some(slot) if workspace_wins => *slot = a,
                Some(_) => {}
                None => session.workspace_agents.push(a),
            }
        }
        for s in cfg.loaded_skills {
            // By stable id (names are display-only).
            if !session.loaded_skills.iter().any(|x| x.id == s.id) {
                session.loaded_skills.push(s);
            }
        }
        for v in cfg.vault_sources {
            if !session.vault_sources.iter().any(|x| x.raw_url() == v.raw_url()) {
                session.vault_sources.push(v);
            }
        }
        for r in cfg.workspace_runtimes {
            if !session.workspace_runtimes.iter().any(|x| x.id == r.id) {
                session.workspace_runtimes.push(r);
            }
        }
        for h in cfg.workspace_hosts {
            if !session.workspace_hosts.iter().any(|x| x.id == h.id) {
                session.workspace_hosts.push(h);
            }
        }
        session
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guard::ChatLockdown;

    fn config(ws: Uuid) -> ChatSession {
        let mut c = ChatSession::new("cfg", ws, "x");
        c.id = crate::workspace_config_session_id(ws);
        c
    }

    #[test]
    fn a_chat_sees_only_its_own_lockdown_and_the_workspace_records() {
        let ws = Uuid::new_v4();
        let chat = ChatSession::new("c", ws, "x");
        let other = Uuid::new_v4();
        let mut cfg = config(ws);
        let lockdown = |id| ChatLockdown {
            id,
            signal: crate::guard::LockdownSignal::InjectionDetected,
            detail: String::new(),
            raised_by: String::new(),
            raised_at: crate::Timestamp::from_unix_seconds(1_800_000_000),
            message_id: None,
            raise_event_id: None,
        };
        cfg.guards.lockdowns.insert(chat.id, vec![lockdown(Uuid::new_v4())]);
        cfg.guards.lockdowns.insert(other, vec![lockdown(Uuid::new_v4())]);
        let agent = WorkspaceAgent::new("helper", "anthropic");
        cfg.workspace_agents.push(agent.clone());
        let view = ChatState::new(chat.clone()).view(Some(WorkspaceConfig::new(cfg)));
        assert_eq!(view.guards.chat_id, Some(chat.id));
        assert_eq!(view.guards.lockdowns.keys().copied().collect::<Vec<_>>(), vec![chat.id]);
        assert!(view.workspace_agents.iter().any(|a| a.id == agent.id));
        // Before the config log syncs, the chat as it folded.
        assert_eq!(ChatState::new(chat.clone()).view(None), chat);
    }

    /// Workspace model v3, 11 (G5): an older build's copy of an agent on a
    /// chat's own log shadows the workspace's record on rules 3 and below;
    /// on rules 4 the chat runs the workspace's record.
    #[test]
    fn on_rules4_the_workspace_record_wins_over_a_chats_old_copy() {
        let ws = Uuid::new_v4();
        let agent = WorkspaceAgent::new("helper", "claude-code");
        let mut chat = ChatSession::new("c", ws, "x");
        let mut copy = agent.clone();
        copy.runtime_id = "old-runtime".into();
        chat.workspace_agents.push(copy);
        let mut cfg = config(ws);
        cfg.workspace_agents.push(agent.clone());
        cfg.authz.rules = crate::authz::RULES_V3;
        let view = ChatState::new(chat.clone()).view(Some(WorkspaceConfig::new(cfg.clone())));
        assert_eq!(view.workspace_agents.iter().find(|a| a.id == agent.id).unwrap().runtime_id, "old-runtime");
        cfg.authz.rules = crate::authz::RULES_V4;
        let view = ChatState::new(chat).view(Some(WorkspaceConfig::new(cfg)));
        assert_eq!(view.workspace_agents.iter().find(|a| a.id == agent.id).unwrap().runtime_id, "claude-code");
        assert_eq!(view.workspace_agents.iter().filter(|a| a.id == agent.id).count(), 1);
    }
}

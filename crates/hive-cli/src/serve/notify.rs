//! Which projection changes notify the user, and what the notification says
//! (ntfy [`Notice`] + end-to-end encrypted [`PushContent`], §14; the kinds and
//! fields are listed in `docs/push-notifications.md`).
//!
//! - an agent reply finished → `message.done` ("coder replied" / "coder hit
//!   an error"), or `mention` when the reply `@mentions` the user (one
//!   notification, never both);
//! - a message from another member that `@mentions` the user → `mention`;
//! - a proposal waiting for review → `proposal.created`, except a workflow
//!   gate's proposal, which is announced as `workflow.approval` instead;
//! - a workflow run's gate waiting for approval → `workflow.approval`;
//! - a reviewer recruit (`flowRecorded` recruit `approvers`) naming the user
//!   → `review.recruit`, pointing at the proposal that has waited longest for
//!   them.
//!
//! "The user" is the identity the phone posts as — the owner, once this box is
//! enrolled as their device — plus every actor that is the same person: a
//! member sharing the account, the box's retired pre-enrollment member, or a
//! linked device ([`hive_core::DeviceLinks`]). Identity is by account / actor
//! id, never by display name (two people can share a name). Their own messages
//! never notify.

use std::collections::HashSet;

use hive_core::{
    ActionProposal, ActorIdentity, ChatMessage, ChatSession, DeviceLinks, MessageRole, ProposalKind, TranscriptEventKind, WorkflowRun,
    WorkspaceRole,
};

use super::ntfy::{self, Notice};
use super::tunnel::PushContent;

/// Who "you" are in a chat, for mentions and own-message checks.
#[derive(Debug, Default)]
pub struct Me {
    /// Member ids and actor ids.
    ids: HashSet<String>,
    roles: Vec<WorkspaceRole>,
    display_name: String,
}

impl Me {
    pub fn in_chat(author: &ActorIdentity, s: &ChatSession, links: &DeviceLinks) -> Self {
        let mut me = Me { display_name: author.display_name.clone(), ..Default::default() };
        let person = links.person(&author.id).to_string();
        me.ids.insert(author.id.clone());
        me.ids.extend(links.ids_of(&person).map(str::to_string));
        for m in &s.members {
            let same = me.ids.contains(&m.actor.id)
                || me.ids.contains(&m.id)
                || (author.account_id.is_some() && m.actor.account_id == author.account_id);
            if same {
                me.ids.insert(m.id.clone());
                me.ids.insert(m.actor.id.clone());
                if !me.roles.contains(&m.role) {
                    me.roles.push(m.role);
                }
            }
        }
        me
    }

    /// Whether `id` (a member or actor id) is the user.
    pub fn is(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    /// Written by the user (here, on the phone, or on their desktop).
    pub fn wrote(&self, m: &ChatMessage) -> bool {
        if m.role != MessageRole::User {
            return false;
        }
        match &m.actor_identity {
            Some(a) => self.ids.contains(&a.id),
            None => !self.display_name.is_empty() && m.author == self.display_name,
        }
    }

    /// Message `m` mentions the user: by name/handle, `@all`, a role group the
    /// user is in (`@owners`, …), or — in an agent's reply — `@you` when the
    /// user is the person the reply answers (its requester). `@you` never
    /// reaches anyone else, and means nothing in a person's own message.
    pub fn mentioned_in(&self, m: &ChatMessage, s: &ChatSession) -> bool {
        if !m.body.contains('@') {
            return false;
        }
        hive_runtime::message_mentions(m, s).reaches(|id| self.ids.contains(id), &self.roles, &self.display_name)
    }
}

fn is_reply(role: MessageRole) -> bool {
    matches!(role, MessageRole::Assistant | MessageRole::Agent)
}

fn base(kind: &str, s: &ChatSession, title: String) -> PushContent {
    PushContent { kind: kind.into(), chat_id: Some(s.id.to_string()), title, ..Default::default() }
}

/// A message just finished (agent reply) or appeared (anyone else's):
/// `message.done`, `mention`, or nothing.
pub fn for_message(s: &ChatSession, m: &ChatMessage, done: bool, me: &Me) -> Option<(Notice, PushContent)> {
    if m.is_streaming || m.role == MessageRole::System || m.kind != TranscriptEventKind::Message || me.wrote(m) {
        return None;
    }
    let reply = is_reply(m.role);
    // Agent replies count once finished (`done`); other messages when they
    // appear (they never stream).
    if reply != done {
        return None;
    }
    let failed = reply && m.body.starts_with("[error]");
    // A name in a notification is shown like anywhere else (S3).
    let author = hive_proto::names::clean_display_name(&m.author);
    if !failed && me.mentioned_in(m, s) {
        let notice = ntfy::mention_notice(&author, &s.title, s.id);
        let mut push = base("mention", s, notice.title.clone());
        push.sender = Some(author.clone());
        push.preview = Some(m.body.clone());
        return Some((notice, push));
    }
    if !reply {
        return None;
    }
    let notice = ntfy::reply_notice(&author, &s.title, s.id, failed);
    let mut push = base("message.done", s, notice.title.clone());
    push.sender = Some(author);
    push.preview = Some(m.body.clone());
    Some((notice, push))
}

/// A workflow gate's backing proposal: announced as `workflow.approval` (from
/// the run), not `proposal.created`. An authorless decision is a gate whose
/// run hasn't synced in yet.
pub fn is_gate_proposal(s: &ChatSession, p: &ActionProposal) -> bool {
    s.workflow_runs.iter().any(|r| r.nodes.iter().any(|n| n.proposal_id == Some(p.id)))
        || (p.author_actor_id.is_empty() && p.kind == ProposalKind::Decision)
}

/// A new proposal waiting for review.
pub fn for_proposal(s: &ChatSession, p: &ActionProposal) -> Option<(Notice, PushContent)> {
    if is_gate_proposal(s, p) {
        return None;
    }
    let notice = ntfy::proposal_notice(&p.title, &s.title, s.id);
    let mut push = base("proposal.created", s, notice.title.clone());
    push.proposal_id = Some(p.id.to_string());
    push.preview = Some(p.title.clone());
    Some((notice, push))
}

/// An agent's question waiting for a person (`ask.pending`).
pub fn for_ask(s: &ChatSession, a: &hive_core::flow::HumanAsk) -> Option<(Notice, PushContent)> {
    if a.closed {
        return None;
    }
    let agent = hive_core::flow::agent_from_key(&a.agent_key)
        .and_then(|id| s.workspace_agents.iter().find(|x| x.id == id))
        .map(|x| format!("@{}", x.name))
        .unwrap_or_else(|| "An agent".into());
    let notice = ntfy::ask_notice(&agent, &s.title, s.id);
    let mut push = base("ask.pending", s, notice.title.clone());
    push.sender = Some(agent);
    push.preview = Some(a.question.clone());
    Some((notice, push))
}

/// A workflow run's gate `node` waiting for approval.
pub fn for_workflow_gate(s: &ChatSession, run: &WorkflowRun, node: &str) -> Option<(Notice, PushContent)> {
    let state = run.node_state(node)?;
    let stage = run.definition.nodes.iter().find(|n| n.id == node).map_or(node, |n| n.name.as_str());
    let notice = ntfy::workflow_notice(&run.definition.name, stage, &s.title, s.id);
    let mut push = base("workflow.approval", s, notice.title.clone());
    push.proposal_id = state.proposal_id.map(|p| p.to_string());
    push.workflow_run_id = Some(run.id.to_string());
    push.workflow_id = Some(run.definition_id.to_string());
    push.preview = Some(stage.to_string());
    Some((notice, push))
}

/// The proposal a reviewer recruit points the user at: the open one that has
/// waited longest for them (they haven't voted on it and didn't write it),
/// preferring the chat the recruit fired in.
pub fn waiting_for(sessions: &[ChatSession], chat: uuid::Uuid, me: &Me) -> Option<(uuid::Uuid, uuid::Uuid)> {
    let waiting = |s: &ChatSession| {
        s.proposals
            .iter()
            .filter(|p| super::hub::is_open(p) && !me.is(&p.author_actor_id) && !p.approvals.iter().any(|a| me.is(&a.actor_id)))
            .map(|p| (p.created_at, s.id, p.id))
            .collect::<Vec<_>>()
    };
    let oldest = |v: Vec<(hive_core::Timestamp, uuid::Uuid, uuid::Uuid)>| v.into_iter().min_by_key(|(t, _, _)| *t).map(|(_, c, p)| (c, p));
    let here = sessions.iter().find(|s| s.id == chat).map(waiting).unwrap_or_default();
    oldest(here).or_else(|| oldest(sessions.iter().filter(|s| !s.archived).flat_map(waiting).collect()))
}

/// A reviewer recruit that names the user (by member or actor id):
/// `review.recruit`, deep-linking to the proposal waiting longest for them.
/// The preview is the proposal's title, so it obeys the phone's previews
/// setting like every other push.
pub fn for_recruit(sessions: &[ChatSession], s: &ChatSession, r: &hive_core::flow::RecruitNotice, me: &Me) -> Option<(Notice, PushContent)> {
    if r.action != "approvers" || !r.member_ids.iter().any(|id| me.is(id)) {
        return None;
    }
    let target = waiting_for(sessions, s.id, me);
    let (chat, title) = match target.and_then(|(c, _)| sessions.iter().find(|x| x.id == c)) {
        Some(c) => (c, c.title.clone()),
        None => (s, s.title.clone()),
    };
    let notice = ntfy::recruit_notice(r.queue_len, &title, chat.id);
    let mut push = base("review.recruit", chat, notice.title.clone());
    if let Some((_, pid)) = target {
        push.proposal_id = Some(pid.to_string());
        push.preview = chat.proposals.iter().find(|p| p.id == pid).map(|p| p.title.clone());
    } else {
        push.fixed_body = notice.body.clone();
    }
    Some((notice, push))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::tunnel::push_plaintext;
    use hive_core::{workflow, ActorKind, NodeRunStatus, WorkspaceAgent, WorkspaceMember};
    use uuid::Uuid;

    fn actor(id: &str, name: &str) -> ActorIdentity {
        let a: ActorIdentity = serde_json::from_value(serde_json::json!({ "id": id, "displayName": name, "kind": "human" })).unwrap();
        assert_eq!(a.kind, ActorKind::Human);
        a
    }

    fn member(a: &ActorIdentity, role: WorkspaceRole, index: u32) -> WorkspaceMember {
        WorkspaceMember { id: a.id.clone(), actor: a.clone(), role, title: String::new(), index, joined_at: Default::default() }
    }

    struct Fixture {
        s: ChatSession,
        me_actor: ActorIdentity,
        other: ActorIdentity,
        links: DeviceLinks,
    }

    /// Zoe's box posts as its own (pre-enrollment) member "box-1", linked to
    /// her desktop identity "desk-1" (a retired alias or a host claim).
    fn fixture() -> Fixture {
        let me_actor = actor("box-1", "Zoe");
        let desktop = actor("desk-1", "zoe");
        let other = actor("ana-1", "Ana");
        let mut s = ChatSession::new("Fix the build", Uuid::new_v4(), "rt");
        s.members = vec![
            member(&desktop, WorkspaceRole::Owner, 1),
            member(&other, WorkspaceRole::Admin, 3),
        ];
        s.workspace_agents = vec![WorkspaceAgent::new("coder", "rt")];
        let mut links = DeviceLinks::default();
        assert!(links.link_retired(&s, "box-1", "desk-1"));
        s.members.push(member(&me_actor, WorkspaceRole::Contributor, 2));
        Fixture { s, me_actor, other, links }
    }

    impl Fixture {
        fn me(&self) -> Me {
            Me::in_chat(&self.me_actor, &self.s, &self.links)
        }
    }

    fn reply(body: &str) -> ChatMessage {
        ChatMessage::new(MessageRole::Assistant, "coder", body)
    }

    fn from(a: &ActorIdentity, body: &str) -> ChatMessage {
        let mut m = ChatMessage::new(MessageRole::User, &a.display_name, body);
        m.actor_identity = Some(a.clone());
        m
    }

    #[test]
    fn finished_agent_reply_is_message_done_with_preview_only_when_on() {
        let f = fixture();
        let me = f.me();
        let (notice, p) = for_message(&f.s, &reply("Done: the build is green."), true, &me).unwrap();
        assert_eq!((p.kind.as_str(), p.title.as_str(), p.sender.as_deref()), ("message.done", "coder replied", Some("coder")));
        assert_eq!(p.chat_id, Some(f.s.id.to_string()));
        assert_eq!(notice.title, "coder replied");
        let on = push_plaintext(&p, true, 1, 0);
        assert_eq!((on["body"].as_str(), on["preview"].as_bool()), (Some("Done: the build is green."), Some(true)));
        let off = push_plaintext(&p, false, 2, 0);
        assert_eq!(off["body"], "");
        assert!(!off.to_string().contains("green"));
        // Errored turn.
        let (_, p) = for_message(&f.s, &reply("[error] model unreachable"), true, &me).unwrap();
        assert_eq!((p.kind.as_str(), p.title.as_str()), ("message.done", "coder hit an error"));
        // Not on creation (still streaming / not done), never for a streaming one.
        assert!(for_message(&f.s, &reply("x"), false, &me).is_none());
        let mut streaming = reply("x");
        streaming.is_streaming = true;
        assert!(for_message(&f.s, &streaming, true, &me).is_none());
        // User messages and system notes are never message.done.
        assert!(for_message(&f.s, &from(&f.other, "hello"), false, &me).is_none());
        assert!(for_message(&f.s, &ChatMessage::new(MessageRole::System, "hive", "note"), true, &me).is_none());
    }

    #[test]
    fn a_reply_that_mentions_you_is_one_mention_push() {
        let f = fixture();
        let me = f.me();
        let (notice, p) = for_message(&f.s, &reply("@Zoe can you check the token?"), true, &me).unwrap();
        assert_eq!((p.kind.as_str(), p.title.as_str(), p.sender.as_deref()), ("mention", "coder mentioned you", Some("coder")));
        assert_eq!(notice.title, "coder mentioned you");
        assert!(!p.title.contains("token"), "titles never carry message text");
        assert_eq!(push_plaintext(&p, true, 1, 0)["body"], "@Zoe can you check the token?");
        assert_eq!(push_plaintext(&p, false, 1, 0)["body"], "");
    }

    #[test]
    fn mentions_from_other_members_by_name_broadcast_or_role() {
        let f = fixture();
        let me = f.me();
        let (_, p) = for_message(&f.s, &from(&f.other, "@zoe take a look"), false, &me).unwrap();
        assert_eq!((p.kind.as_str(), p.title.as_str(), p.sender.as_deref()), ("mention", "Ana mentioned you", Some("Ana")));
        assert!(for_message(&f.s, &from(&f.other, "heads up @all"), false, &me).is_some());
        // The owner's desktop is the same person (linked): @owners includes "you".
        assert!(for_message(&f.s, &from(&f.other, "@owners please approve"), false, &me).is_some());
        assert!(for_message(&f.s, &from(&f.other, "@viewers fyi"), false, &me).is_none());
        // No mention of you → nothing for a user message.
        assert!(for_message(&f.s, &from(&f.other, "@coder fix it"), false, &me).is_none());
        assert!(for_message(&f.s, &from(&f.other, "mail zoe@example.com"), false, &me).is_none());
        // Mentioning someone else.
        assert!(for_message(&f.s, &from(&f.me_actor, "@Ana ping"), false, &Me::in_chat(&f.other, &f.s, &f.links)).is_some());
    }

    /// Three people, each with their own box: Ana asks (contributor), Ben is
    /// another member (contributor), Olu owns the workspace.
    fn trio() -> (ChatSession, [ActorIdentity; 3]) {
        let (ana, ben, olu) = (actor("ana-1", "Ana"), actor("ben-1", "Ben"), actor("olu-1", "Olu"));
        let mut s = ChatSession::new("Release", Uuid::new_v4(), "rt");
        s.members = vec![
            member(&olu, WorkspaceRole::Owner, 1),
            member(&ana, WorkspaceRole::Contributor, 2),
            member(&ben, WorkspaceRole::Contributor, 3),
        ];
        s.workspace_agents = vec![WorkspaceAgent::new("coder", "rt")];
        (s, [ana, ben, olu])
    }

    /// Who of the three gets a `mention` push for message `m`.
    fn pinged(s: &ChatSession, m: &ChatMessage, people: &[ActorIdentity; 3]) -> Vec<String> {
        let done = matches!(m.role, MessageRole::Assistant | MessageRole::Agent);
        people
            .iter()
            .filter(|p| {
                let me = Me::in_chat(p, s, &DeviceLinks::default());
                for_message(s, m, done, &me).is_some_and(|(_, push)| push.kind == "mention")
            })
            .map(|p| p.display_name.clone())
            .collect()
    }

    fn answer(s: &mut ChatSession, to: &ChatMessage, body: &str) -> ChatMessage {
        s.messages.push(to.clone());
        let mut r = reply(body);
        r.reply_to_message_id = Some(to.id);
        s.messages.push(r.clone());
        r
    }

    #[test]
    fn an_agents_at_you_pings_only_the_requester() {
        let (mut s, people) = trio();
        let ask = from(&people[0], "@coder fix the build");
        let r = answer(&mut s, &ask, "Fixed. @you can you confirm on your machine?");
        assert_eq!(pinged(&s, &r, &people), vec!["Ana"]);
        // The others still get the ordinary reply notice, not a mention.
        let ben = Me::in_chat(&people[1], &s, &DeviceLinks::default());
        assert_eq!(for_message(&s, &r, true, &ben).unwrap().1.kind, "message.done");
    }

    #[test]
    fn an_agents_at_all_pings_everyone_but_a_second_one_soon_after_only_the_requester() {
        let (mut s, people) = trio();
        let ask = from(&people[0], "@coder ship it");
        let r = answer(&mut s, &ask, "@all v2 is out");
        assert_eq!(pinged(&s, &r, &people), vec!["Ana", "Ben", "Olu"]);
        let again = from(&people[0], "@coder and the notes?");
        let r2 = answer(&mut s, &again, "@all notes are up");
        assert_eq!(pinged(&s, &r2, &people), vec!["Ana"]);
    }

    #[test]
    fn the_phone_is_told_who_at_you_is_and_whether_it_is_them() {
        let (mut s, people) = trio();
        let ask = from(&people[0], "@coder fix it");
        let r = answer(&mut s, &ask, "@you fixed");
        for (p, mine) in people.iter().zip([true, false, false]) {
            let me = Me::in_chat(p, &s, &DeviceLinks::default());
            let mut v = crate::serve::api::message_dto(s.id, &r, crate::serve::api::Viewer::default());
            crate::serve::api::add_requester(&mut v, &s, &r, &me);
            assert_eq!(v["requester"]["name"], "Ana", "{}", p.display_name);
            assert_eq!(v["requester"]["actorId"], "ana-1");
            assert_eq!(v["requester"]["isYou"], mine, "{}", p.display_name);
            assert_eq!(v["body"], "@you fixed", "the body is not rewritten");
        }
        // A person's @you carries no requester.
        let m = from(&people[1], "thanks @you");
        s.messages.push(m.clone());
        let mut v = crate::serve::api::message_dto(s.id, &m, crate::serve::api::Viewer::default());
        crate::serve::api::add_requester(&mut v, &s, &m, &Me::in_chat(&people[0], &s, &DeviceLinks::default()));
        assert!(v.get("requester").is_none());
    }

    #[test]
    fn a_persons_at_you_pings_nobody() {
        let (mut s, people) = trio();
        let m = from(&people[1], "thanks @you");
        s.messages.push(m.clone());
        assert!(pinged(&s, &m, &people).is_empty());
        // …while a person's @all still reaches the other two.
        let all = from(&people[1], "@all standup");
        s.messages.push(all.clone());
        assert_eq!(pinged(&s, &all, &people), vec!["Ana", "Olu"]);
    }

    #[test]
    fn at_you_with_no_requester_pings_nobody() {
        let (mut s, people) = trio();
        let r = reply("@you done");
        s.messages.push(r.clone());
        assert!(pinged(&s, &r, &people).is_empty());
    }

    #[test]
    fn solo_owner_still_gets_at_you() {
        // The single-user case tested on the real phone: the owner's box.
        let f = fixture();
        let mut s = f.s.clone();
        let desktop = s.members[0].actor.clone();
        let ask = from(&desktop, "status?");
        let r = answer(&mut s, &ask, "@you all green");
        let me = Me::in_chat(&f.me_actor, &s, &f.links);
        let (_, p) = for_message(&s, &r, true, &me).unwrap();
        assert_eq!(p.kind, "mention");
    }

    #[test]
    fn your_own_messages_never_notify() {
        let f = fixture();
        let me = f.me();
        assert!(for_message(&f.s, &from(&f.me_actor, "@all @Zoe note to self"), false, &me).is_none());
        // …nor from the owner's desktop identity (the same person).
        let desktop = f.s.members[0].actor.clone();
        assert!(for_message(&f.s, &from(&desktop, "@all hi"), false, &me).is_none());
    }

    #[test]
    fn a_namesake_is_not_you() {
        // Another person who happens to be called "Zoe", with no link to you:
        // their messages notify, and their role isn't yours.
        let mut f = fixture();
        f.links = DeviceLinks::default();
        let me = f.me();
        let namesake = f.s.members[0].actor.clone();
        assert!(for_message(&f.s, &from(&namesake, "@all hi"), false, &me).is_some());
        assert!(for_message(&f.s, &from(&f.other, "@owners please approve"), false, &me).is_none());
    }

    #[test]
    fn enrolled_box_is_the_owner() {
        // After enrollment the box posts as the owner's own actor: mentions of
        // the owner are mentions of you, with no link needed.
        let f = fixture();
        let desktop = f.s.members[0].actor.clone();
        let me = Me::in_chat(&desktop, &f.s, &DeviceLinks::default());
        assert!(for_message(&f.s, &from(&f.other, "@zoe take a look"), false, &me).is_some());
        assert!(for_message(&f.s, &from(&desktop, "@all from my desktop"), false, &me).is_none());
    }

    #[test]
    fn proposal_push_carries_ids_and_title_preview() {
        let f = fixture();
        let p = ActionProposal::new("coder proposed changes to 2 files", ProposalKind::FileDiff, "agent-1");
        let (notice, push) = for_proposal(&f.s, &p).unwrap();
        assert_eq!((push.kind.as_str(), push.title.as_str()), ("proposal.created", "Change ready for review"));
        assert_eq!(notice.title, "Change ready for review");
        let pt = push_plaintext(&push, true, 3, 0);
        assert_eq!((pt["chatId"].as_str(), pt["proposalId"].as_str()), (Some(f.s.id.to_string().as_str()), Some(p.id.to_string().as_str())));
        assert_eq!(pt["body"], "coder proposed changes to 2 files");
        let off = push_plaintext(&push, false, 4, 0);
        assert_eq!((off["body"].as_str(), off["proposalId"].as_str()), (Some(""), Some(p.id.to_string().as_str())));
    }

    #[test]
    fn workflow_gate_is_workflow_approval_not_proposal_created() {
        let mut f = fixture();
        let def = workflow::preset_review_gate();
        let mut run = workflow::new_run(&def, "build it", "u1");
        let gate = ActionProposal::new("Approve the change?", ProposalKind::Decision, "");
        // Before the run syncs in, the authorless decision is already a gate.
        assert!(for_proposal(&f.s, &gate).is_none());
        let st = run.node_state_mut("approval").unwrap();
        st.status = NodeRunStatus::AwaitingApproval;
        st.proposal_id = Some(gate.id);
        f.s.workflow_runs.push(run.clone());
        f.s.proposals.push(gate.clone());
        assert!(for_proposal(&f.s, &gate).is_none());
        let (notice, p) = for_workflow_gate(&f.s, &run, "approval").unwrap();
        assert_eq!(p.kind, "workflow.approval");
        assert_eq!(p.title, format!("{} needs approval", def.name));
        assert_eq!(notice.title, p.title);
        let pt = push_plaintext(&p, true, 5, 0);
        assert_eq!(pt["chatId"], f.s.id.to_string());
        assert_eq!(pt["proposalId"], gate.id.to_string());
        assert_eq!(pt["workflowRunId"], run.id.to_string());
        assert_eq!(pt["workflowId"], def.id.to_string());
        assert_eq!(pt["body"], "Approval", "the stage name is the preview");
        assert_eq!(push_plaintext(&p, false, 6, 0)["body"], "");
        assert!(for_workflow_gate(&f.s, &run, "nope").is_none());
    }

    #[test]
    fn a_recruit_naming_you_points_at_the_oldest_proposal_waiting_for_you() {
        let mut f = fixture();
        let mut old = ActionProposal::new("Pick a cache", ProposalKind::Decision, "ana-1");
        old.author_actor_id = "ana-1".into();
        old.created_at = hive_core::Timestamp::from_unix_seconds(1_000);
        let newer = ActionProposal::new("Rename module", ProposalKind::Decision, "ana-1");
        // One the user wrote doesn't count.
        let mut own = ActionProposal::new("My own", ProposalKind::Decision, "desk-1");
        own.author_actor_id = "desk-1".into();
        own.created_at = hive_core::Timestamp::from_unix_seconds(10);
        let old_id = old.id;
        f.s.proposals = vec![own, newer, old];
        let me = f.me();
        let r = |members: Vec<&str>| hive_core::flow::RecruitNotice {
            id: Uuid::new_v4(),
            action: "approvers".into(),
            queue_len: 6,
            oldest_wait_ms: 0,
            member_ids: members.into_iter().map(String::from).collect(),
            to_host: String::new(),
            from_host: String::new(),
            reference: String::new(),
            at_ms: 0,
        };
        let sessions = vec![f.s.clone()];
        // Named by a linked identity (the desktop member) still counts.
        let (notice, p) = for_recruit(&sessions, &f.s, &r(vec!["desk-1"]), &me).unwrap();
        assert_eq!(p.kind, "review.recruit");
        assert_eq!(p.proposal_id, Some(old_id.to_string()));
        assert_eq!(notice.title, "The review queue needs you");
        assert!(notice.body.contains("6 proposals waiting"));
        let on = push_plaintext(&p, true, 1, 0);
        assert_eq!(on["body"], "Pick a cache");
        let off = push_plaintext(&p, false, 2, 0);
        assert_eq!(off["body"], "");
        assert_eq!(off["proposalId"], old_id.to_string());
        assert!(!off.to_string().contains("cache"), "previews off hides the title");
        // Someone else's recruit, or another action, isn't yours.
        assert!(for_recruit(&sessions, &f.s, &r(vec!["ana-1"]), &me).is_none());
        let mut stop = r(vec!["desk-1"]);
        stop.action = "stop_upstream".into();
        assert!(for_recruit(&sessions, &f.s, &stop, &me).is_none());
    }
}

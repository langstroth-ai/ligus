//! `@`-mention routing — ported from the mention matrix in `PrototypeStore.swift`
//! (`humanMentionTargets` / `mentionsPrimaryRuntime` / `mentionsHumanBroadcast`
//! / agent mentions). Pure text analysis over a session's roster.
//!
//! Recognized targets:
//! - `@primary` — the workspace's primary runtime
//! - `@all` — broadcast to every human (drives a notification)
//! - `@you` — in an agent's reply, the *requester*: the human whose message
//!   started the turn (see [`requester_of`]). In a person's own message it
//!   means nothing. Never a broadcast.
//! - `@<agent name>` — a specific workspace agent (case-insensitive)
//! - `@<role>` — a role group: `@owners` / `@admins` / `@contributors` / `@viewers`
//! - `@<handle>` — a specific human member by handle (actor id)

use ligus_core::{ActorKind, ChatMessage, ChatSession, MessageRole, WorkspaceRole};
use uuid::Uuid;

/// How long after an agent's `@all` a further agent `@all` in the same chat is
/// narrowed to its requester (see [`message_mentions`]).
pub const AGENT_BROADCAST_WINDOW_SECS: i64 = 10 * 60;

/// The person an agent reply answers: the human author of the message that
/// started the turn. What `@you` in that reply resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requester {
    /// Their actor id (empty for a legacy message with no stamped identity).
    pub actor_id: String,
    /// Their member id, when they're on the roster.
    pub member_id: Option<String>,
    pub display_name: String,
    /// Their per-workspace join index (0 = none), for `@Name#N`.
    pub index: u32,
    pub account_id: Option<Uuid>,
}

impl Requester {
    /// The handle that `@`-mentions exactly them: `Name`, or `Name#N` when
    /// another member shares the name.
    pub fn handle(&self, session: &ChatSession) -> String {
        let shared = session
            .members
            .iter()
            .filter(|m| m.actor.display_name.eq_ignore_ascii_case(&self.display_name))
            .count()
            > 1;
        if shared && self.index > 0 {
            format!("{}#{}", self.display_name, self.index)
        } else {
            self.display_name.clone()
        }
    }

    /// Whether `id` (a member or actor id) is this person.
    pub fn is(&self, id: &str) -> bool {
        !id.is_empty() && (self.actor_id == id || self.member_id.as_deref() == Some(id))
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MentionTargets {
    /// `@primary` was mentioned.
    pub primary: bool,
    /// `@all` — broadcast to every human (notification trigger).
    pub human_broadcast: bool,
    /// `@you` appeared. On its own this targets nobody: [`message_mentions`]
    /// resolves it to the requester for an agent's reply.
    pub you: bool,
    /// For an agent reply that wrote `@you` (or whose `@all` was narrowed):
    /// the person it addresses.
    pub requester: Option<Requester>,
    /// An agent's `@all` that was narrowed to its requester because another
    /// agent `@all` went out in this chat less than
    /// [`AGENT_BROADCAST_WINDOW_SECS`] earlier.
    pub broadcast_throttled: bool,
    /// Specific workspace agents mentioned, by id.
    pub agents: Vec<Uuid>,
    /// Specific human members mentioned, by member id.
    pub humans: Vec<String>,
    /// Role groups mentioned.
    pub roles: Vec<WorkspaceRole>,
}

impl MentionTargets {
    /// No one is addressed. A bare `@you` (nobody resolved) counts as empty.
    pub fn is_empty(&self) -> bool {
        !self.primary
            && self.requester.is_none()
            && !self.human_broadcast
            && self.agents.is_empty()
            && self.humans.is_empty()
            && self.roles.is_empty()
    }
}

fn role_from_group(token: &str) -> Option<WorkspaceRole> {
    match token {
        "owners" | "owner" => Some(WorkspaceRole::Owner),
        "admins" | "admin" => Some(WorkspaceRole::Admin),
        "contributors" | "contributor" => Some(WorkspaceRole::Contributor),
        "viewers" | "viewer" => Some(WorkspaceRole::Viewer),
        _ => None,
    }
}

/// True if `body` contains `@needle` as a mention (case-insensitive, not part of
/// a longer word). `needle` may contain spaces (agent display names).
fn mentions_phrase(body_lower: &str, needle: &str) -> bool {
    mentions_phrase_opts(body_lower, needle, true)
}

/// `@needle` mention check. When `allow_hash_after` is false, a following `#`
/// also disqualifies the match — so a bare `@Sam` won't swallow `@Sam#2`.
fn mentions_phrase_opts(body_lower: &str, needle: &str, allow_hash_after: bool) -> bool {
    let needle = needle.to_lowercase();
    if needle.is_empty() {
        return false;
    }
    let pat = format!("@{needle}");
    let mut from = 0;
    while let Some(idx) = body_lower[from..].find(&pat) {
        let start = from + idx;
        let after = start + pat.len();
        // The char BEFORE the `@` must be a boundary too, or an email address /
        // inline handle (`me@ligus.io`, `ping@scout`) would count as a mention and
        // wrongly summon the primary/an agent to answer an ordinary message.
        let ok_before = body_lower[..start]
            .chars()
            .next_back()
            .map(|c| !c.is_alphanumeric() && c != '_')
            .unwrap_or(true);
        // the char following the match must not continue an identifier
        let ok_after = body_lower[after..]
            .chars()
            .next()
            .map(|c| {
                !c.is_alphanumeric() && c != '_' && c != '-' && (allow_hash_after || c != '#')
            })
            .unwrap_or(true);
        if ok_before && ok_after {
            return true;
        }
        from = after;
    }
    false
}

/// Parse mention targets from a message body against a session's roster.
pub fn parse_mentions(body: &str, session: &ChatSession) -> MentionTargets {
    let lower = body.to_lowercase();
    let mut targets = MentionTargets::default();

    // `@ligus` is the default agent's handle; `@primary` is the reserved alias
    // (a role, not a name). Both summon the workspace's primary runtime. The
    // Hive builds' `@hive` is a silent alias for the same agent (old messages,
    // scripts, older phones), unless the workspace has a roster agent of that
    // name or a member called Hive, who keep their mentions
    // (`legacy_primary_alias_active`).
    let legacy_alias = ligus_core::agent::legacy_primary_alias_active(session)
        && mentions_phrase(&lower, ligus_core::agent::LEGACY_PRIMARY_HANDLE);
    if mentions_phrase(&lower, "primary") || mentions_phrase(&lower, ligus_core::agent::PRIMARY_HANDLE) || legacy_alias {
        targets.primary = true;
    }
    if mentions_phrase(&lower, "all") {
        targets.human_broadcast = true;
    }
    targets.you = mentions_phrase(&lower, "you");

    // Agents by display name (longest names first so "@code reviewer" wins
    // over "@code").
    let mut agents: Vec<_> = session.workspace_agents.iter().collect();
    agents.sort_by_key(|a| std::cmp::Reverse(a.name.len()));
    for agent in agents {
        // A roster agent named after the default agent (`ligus`, `primary`)
        // can't be made by a current client; one that syncs in anyway never
        // answers the default agent's mentions.
        if ligus_core::agent::names_primary(&agent.name) {
            continue;
        }
        if !agent.name.is_empty() && mentions_phrase(&lower, &agent.name) {
            if !targets.agents.contains(&agent.id) {
                targets.agents.push(agent.id);
            }
        }
    }

    // Role groups + specific human handles.
    for role in [
        WorkspaceRole::Owner,
        WorkspaceRole::Admin,
        WorkspaceRole::Contributor,
        WorkspaceRole::Viewer,
    ] {
        let group = match role {
            WorkspaceRole::Owner => "owners",
            WorkspaceRole::Admin => "admins",
            WorkspaceRole::Contributor => "contributors",
            WorkspaceRole::Viewer => "viewers",
        };
        if mentions_phrase(&lower, group) {
            targets.roles.push(role);
        }
    }
    let _ = role_from_group; // singular forms accepted via the helper if needed later

    for member in &session.members {
        let name = &member.actor.display_name;
        if name.is_empty() {
            continue;
        }
        // Precise `@Name#index` (disambiguates duplicate display names), or a
        // bare `@Name` not followed by `#` (which would target a specific one).
        let indexed =
            member.index > 0 && mentions_phrase(&lower, &format!("{name}#{}", member.index));
        let bare = mentions_phrase_opts(&lower, name, false);
        if (indexed || bare) && !targets.humans.contains(&member.id) {
            targets.humans.push(member.id.clone());
        }
    }

    targets
}

/// Whether `m` is an agent's reply (the primary runtime or a named agent).
fn is_agent_reply(m: &ChatMessage) -> bool {
    matches!(m.role, MessageRole::Assistant | MessageRole::Agent)
}

/// The requester of a turn that answers message `trigger`: follow
/// `reply_to_message_id` through agent replies (hand-offs, `@mention`
/// cascades) to the first message a person wrote. A workflow stage's prompt is
/// posted by the person who started the run, so a stage resolves to them.
/// `None` when the chain ends without a person (a legacy reply with no
/// trigger link, or a chain an agent started).
pub fn requester_from(session: &ChatSession, trigger: Option<Uuid>) -> Option<Requester> {
    let mut cur = trigger?;
    for _ in 0..64 {
        let m = session.messages.iter().find(|m| m.id == cur)?;
        if m.role == MessageRole::User {
            match &m.actor_identity {
                Some(a) if a.kind == ActorKind::Human => return Some(requester_for(session, a)),
                // A legacy message with no stamped identity: the roster member
                // of that name, when exactly one.
                None => return requester_by_name(session, &m.author),
                // Posted by an agent or the system: keep walking.
                Some(_) => {}
            }
        }
        match m.reply_to_message_id {
            Some(p) if p != cur => cur = p,
            _ => return None,
        }
    }
    None
}

/// The requester `@you` in message `m` addresses: for an agent's reply, the
/// person whose message started its turn; for anything else, nobody.
pub fn requester_of(session: &ChatSession, m: &ChatMessage) -> Option<Requester> {
    if !is_agent_reply(m) {
        return None;
    }
    requester_from(session, m.reply_to_message_id)
}

fn requester_for(session: &ChatSession, a: &ligus_core::ActorIdentity) -> Requester {
    let member = session
        .members
        .iter()
        .find(|m| m.actor.id == a.id || m.id == a.id)
        .or_else(|| a.account_id.and_then(|acct| session.members.iter().find(|m| m.actor.account_id == Some(acct))));
    Requester {
        actor_id: a.id.clone(),
        member_id: member.map(|m| m.id.clone()),
        display_name: member.map(|m| m.actor.display_name.clone()).unwrap_or_else(|| a.display_name.clone()),
        index: member.map(|m| m.index).unwrap_or(0),
        account_id: a.account_id,
    }
}

fn requester_by_name(session: &ChatSession, author: &str) -> Option<Requester> {
    if author.trim().is_empty() {
        return None;
    }
    let mut named = session.members.iter().filter(|m| m.actor.kind == ActorKind::Human && m.actor.display_name == author);
    match (named.next(), named.next()) {
        (Some(m), None) => Some(requester_for(session, &m.actor)),
        (Some(_), Some(_)) => None,
        (None, _) => Some(Requester {
            actor_id: String::new(),
            member_id: None,
            display_name: author.to_string(),
            index: 0,
            account_id: None,
        }),
    }
}

/// Whether another agent reply in this chat wrote `@all` within
/// [`AGENT_BROADCAST_WINDOW_SECS`] before `m`.
fn recent_agent_broadcast(session: &ChatSession, m: &ChatMessage) -> bool {
    let Some(at) = session.messages.iter().position(|x| x.id == m.id) else { return false };
    session.messages[..at].iter().rev().any(|p| {
        is_agent_reply(p)
            && !p.is_streaming
            && !p.superseded
            && (m.created_at.0 - p.created_at.0).whole_seconds() < AGENT_BROADCAST_WINDOW_SECS
            && (m.created_at.0 - p.created_at.0).whole_seconds() >= 0
            && mentions_phrase(&p.body.to_lowercase(), "all")
    })
}

/// The mention targets of message `m` in `session` — [`parse_mentions`] plus
/// what depends on who wrote it:
///
/// - `@you` in an agent's reply targets its requester ([`requester_of`]):
///   their member id joins `humans` and `requester` is set. No requester
///   found → nobody. In a person's own message `@you` targets nobody.
/// - `@all` from an agent broadcasts, but at most once per
///   [`AGENT_BROADCAST_WINDOW_SECS`] per chat: a further agent `@all` inside
///   the window is narrowed to the requester (`broadcast_throttled`). A
///   person's `@all` always broadcasts.
pub fn message_mentions(m: &ChatMessage, session: &ChatSession) -> MentionTargets {
    let mut t = parse_mentions(&m.body, session);
    if !is_agent_reply(m) || !(t.you || t.human_broadcast) {
        return t;
    }
    if t.human_broadcast && recent_agent_broadcast(session, m) {
        t.human_broadcast = false;
        t.broadcast_throttled = true;
    }
    if t.you || t.broadcast_throttled {
        if let Some(r) = requester_of(session, m) {
            if let Some(id) = &r.member_id {
                if !t.humans.contains(id) {
                    t.humans.push(id.clone());
                }
            }
            t.requester = Some(r);
        }
    }
    t
}

impl MentionTargets {
    /// Whether these targets reach the person known by `is_me` (their member
    /// and actor ids) holding `roles`: a broadcast, a name/handle, a role
    /// group, or the requester `@you` resolved to. `my_name` matches a legacy
    /// requester that has no actor id.
    pub fn reaches(&self, is_me: impl Fn(&str) -> bool, roles: &[WorkspaceRole], my_name: &str) -> bool {
        self.human_broadcast
            || self.humans.iter().any(|h| is_me(h))
            || self.roles.iter().any(|r| roles.contains(r))
            || self.requester.as_ref().is_some_and(|r| {
                if r.actor_id.is_empty() {
                    !my_name.is_empty() && r.display_name == my_name
                } else {
                    is_me(&r.actor_id) || r.member_id.as_deref().is_some_and(&is_me)
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ligus_core::{ActorIdentity, ActorKind, WorkspaceAgent, WorkspaceMember};

    fn session_with(agents: Vec<WorkspaceAgent>, members: Vec<WorkspaceMember>) -> ChatSession {
        let mut s = ChatSession::new("t", Uuid::nil(), "anthropic");
        s.workspace_agents = agents;
        s.members = members;
        s
    }

    #[test]
    fn detects_primary_and_broadcast() {
        let s = session_with(vec![], vec![]);
        let t = parse_mentions("hey @primary can you and @all look?", &s);
        assert!(t.primary);
        assert!(t.human_broadcast);
    }

    #[test]
    fn at_you_is_not_a_broadcast() {
        let s = session_with(vec![], vec![]);
        let t = parse_mentions("@you can you look?", &s);
        assert!(!t.human_broadcast);
        assert!(t.you);
        // On its own (nobody resolved) it addresses no one.
        assert!(t.is_empty());
        // "you" inside a word or an email is not `@you`.
        assert!(!parse_mentions("ping me@you.io or @young", &s).you);
    }

    // --- requester (`@you`) -------------------------------------------------

    fn human(id: &str, name: &str, role: WorkspaceRole, index: u32) -> WorkspaceMember {
        WorkspaceMember {
            id: format!("m-{id}"),
            actor: ActorIdentity::new(id, name, ActorKind::Human),
            role,
            title: String::new(),
            index,
            joined_at: Default::default(),
        }
    }

    /// Three people: Ana (requester, contributor), Ben (another member,
    /// contributor), Olu (owner).
    fn team() -> ChatSession {
        session_with(
            vec![WorkspaceAgent::new("coder", "r1"), WorkspaceAgent::new("reviewer", "r1")],
            vec![
                human("olu", "Olu", WorkspaceRole::Owner, 1),
                human("ana", "Ana", WorkspaceRole::Contributor, 2),
                human("ben", "Ben", WorkspaceRole::Contributor, 3),
            ],
        )
    }

    fn said(s: &mut ChatSession, who: &str, body: &str) -> Uuid {
        let a = s.members.iter().find(|m| m.actor.id == who).unwrap().actor.clone();
        let mut m = ChatMessage::new(MessageRole::User, a.display_name.clone(), body);
        m.actor_identity = Some(a);
        let id = m.id;
        s.messages.push(m);
        id
    }

    fn replied(s: &mut ChatSession, agent: &str, to: Option<Uuid>, body: &str) -> ChatMessage {
        let mut m = ChatMessage::new(MessageRole::Agent, agent, body);
        m.reply_to_message_id = to;
        s.messages.push(m.clone());
        m
    }

    #[test]
    fn agent_at_you_targets_only_the_requester() {
        let mut s = team();
        let q = said(&mut s, "ana", "@coder fix the build");
        let r = replied(&mut s, "coder", Some(q), "Done — @you can you check the token?");
        let t = message_mentions(&r, &s);
        assert!(!t.human_broadcast);
        assert_eq!(t.humans, vec!["m-ana".to_string()]);
        let req = t.requester.clone().unwrap();
        assert_eq!((req.actor_id.as_str(), req.display_name.as_str()), ("ana", "Ana"));
        assert!(t.reaches(|id| id == "ana" || id == "m-ana", &[WorkspaceRole::Contributor], "Ana"));
        assert!(!t.reaches(|id| id == "ben" || id == "m-ben", &[WorkspaceRole::Contributor], "Ben"));
        assert!(!t.reaches(|id| id == "olu" || id == "m-olu", &[WorkspaceRole::Owner], "Olu"));
    }

    #[test]
    fn agent_at_all_reaches_everyone() {
        let mut s = team();
        let q = said(&mut s, "ana", "@coder ship it");
        let r = replied(&mut s, "coder", Some(q), "@all the release is out");
        let t = message_mentions(&r, &s);
        for who in ["ana", "ben", "olu"] {
            assert!(t.reaches(|id| id == who, &[], ""), "{who}");
        }
    }

    #[test]
    fn a_humans_at_you_targets_nobody() {
        let mut s = team();
        said(&mut s, "ben", "thanks @you");
        let m = s.messages.last().unwrap().clone();
        let t = message_mentions(&m, &s);
        assert!(!t.human_broadcast && t.requester.is_none() && t.humans.is_empty());
        for who in ["ana", "ben", "olu"] {
            assert!(!t.reaches(|id| id == who, &[], ""), "{who}");
        }
    }

    #[test]
    fn handoffs_resolve_to_the_human_who_started_the_chain() {
        let mut s = team();
        let q = said(&mut s, "ben", "@coder refactor the parser");
        let first = replied(&mut s, "coder", Some(q), "@reviewer please review");
        let second = replied(&mut s, "reviewer", Some(first.id), "LGTM, @you it's ready");
        let t = message_mentions(&second, &s);
        assert_eq!(t.requester.unwrap().actor_id, "ben");
        assert_eq!(t.humans, vec!["m-ben".to_string()]);
    }

    #[test]
    fn workflow_stage_resolves_to_the_person_who_posted_the_stage_prompt() {
        // The run's driver posts each stage prompt as its own user.
        let mut s = team();
        let prompt = said(&mut s, "olu", "**[Workflow · Ship → build]**\n\nbuild it");
        let r = replied(&mut s, "coder", Some(prompt), "@you build is green");
        assert_eq!(requester_of(&s, &r).unwrap().actor_id, "olu");
    }

    #[test]
    fn no_requester_means_at_you_targets_nobody() {
        let mut s = team();
        // A legacy reply with no trigger link.
        let r = replied(&mut s, "coder", None, "@you done");
        let t = message_mentions(&r, &s);
        assert!(t.requester.is_none() && t.humans.is_empty() && !t.human_broadcast);
        // A chain an agent started (its own user-role post, not a person's).
        let mut post = ChatMessage::new(MessageRole::User, "coder", "@reviewer look");
        post.actor_identity = Some(ActorIdentity::new("agent-1", "coder", ActorKind::Agent));
        let pid = post.id;
        s.messages.push(post);
        let r = replied(&mut s, "reviewer", Some(pid), "@you looks fine");
        assert!(message_mentions(&r, &s).requester.is_none());
    }

    #[test]
    fn requester_handle_disambiguates_shared_names() {
        let mut s = team();
        s.members.push(human("ana2", "Ana", WorkspaceRole::Viewer, 4));
        let q = said(&mut s, "ana2", "@coder hi");
        let r = replied(&mut s, "coder", Some(q), "@you hi");
        let req = requester_of(&s, &r).unwrap();
        assert_eq!(req.handle(&s), "Ana#4");
        // `@Ana#4` written by the agent targets the same person.
        assert_eq!(parse_mentions("@Ana#4 hi", &s).humans, vec!["m-ana2".to_string()]);
        let q = said(&mut s, "ben", "@coder hi");
        let r = replied(&mut s, "coder", Some(q), "@you hi");
        assert_eq!(requester_of(&s, &r).unwrap().handle(&s), "Ben");
    }

    #[test]
    fn single_user_owner_still_gets_at_you() {
        let mut s = session_with(vec![], vec![human("me", "Zaz", WorkspaceRole::Owner, 1)]);
        let q = said(&mut s, "me", "what's the status?");
        let mut r = ChatMessage::new(MessageRole::Assistant, "Ligus", "@you all green");
        r.reply_to_message_id = Some(q);
        s.messages.push(r.clone());
        assert!(message_mentions(&r, &s).reaches(|id| id == "me", &[WorkspaceRole::Owner], "Zaz"));
    }

    #[test]
    fn a_legacy_unstamped_request_resolves_by_name() {
        let mut s = session_with(vec![], vec![]);
        let q = ChatMessage::new(MessageRole::User, "You", "hi");
        let qid = q.id;
        s.messages.push(q);
        let r = replied(&mut s, "Ligus", Some(qid), "@you hello");
        let t = message_mentions(&r, &s);
        assert!(t.reaches(|_| false, &[], "You"));
        assert!(!t.reaches(|_| false, &[], "Ben"));
    }

    #[test]
    fn a_second_agent_broadcast_within_the_window_narrows_to_the_requester() {
        let mut s = team();
        let q1 = said(&mut s, "ana", "@coder deploy");
        let first = replied(&mut s, "coder", Some(q1), "@all deploying now");
        assert!(message_mentions(&first, &s).human_broadcast);
        let q2 = said(&mut s, "ana", "@coder again");
        let second = replied(&mut s, "coder", Some(q2), "@all deploying again");
        let t = message_mentions(&second, &s);
        assert!(!t.human_broadcast && t.broadcast_throttled);
        assert!(t.reaches(|id| id == "ana", &[], ""));
        assert!(!t.reaches(|id| id == "ben", &[], ""));
        // A person's @all is never throttled.
        said(&mut s, "olu", "@all standup in 5");
        let human_all = s.messages.last().unwrap().clone();
        assert!(message_mentions(&human_all, &s).human_broadcast);
        // Past the window, an agent may broadcast again.
        let q3 = said(&mut s, "ana", "@coder later");
        let mut third = ChatMessage::new(MessageRole::Agent, "coder", "@all done");
        third.reply_to_message_id = Some(q3);
        third.created_at = ligus_core::Timestamp(second.created_at.0 + time::Duration::seconds(AGENT_BROADCAST_WINDOW_SECS + 1));
        s.messages.push(third.clone());
        assert!(message_mentions(&third, &s).human_broadcast);
    }

    #[test]
    fn ligus_is_an_alias_for_primary() {
        let s = session_with(vec![], vec![]);
        assert!(parse_mentions("can you take this @ligus?", &s).primary);
        // Bare "ligus" in prose (no @) must not summon the primary.
        assert!(!parse_mentions("the ligus is busy today", &s).primary);
    }

    /// `@hive` (the Hive builds' handle) is the default agent, unless a person
    /// or a roster agent is called exactly that: then it reaches them, and
    /// never wakes the default agent on someone's credentials as well.
    #[test]
    fn legacy_hive_alias_yields_to_a_person_or_agent_named_hive() {
        let s = session_with(vec![], vec![human("h1", "Hive", WorkspaceRole::Contributor, 0)]);
        let t = parse_mentions("@Hive can you look?", &s);
        assert!(!t.primary && t.humans == vec!["m-h1".to_string()], "{t:?}");
        // A name that `@hive` doesn't match (" hive") leaves the alias on.
        let s = session_with(vec![WorkspaceAgent::new(" hive", "r")], vec![]);
        let t = parse_mentions("@hive x", &s);
        assert!(t.primary && t.agents.is_empty(), "{t:?}");
    }

    /// A look-alike of `ligus` on the roster (Cyrillic і, a zero-width space)
    /// is never a mention target, so it can't answer in the default agent's
    /// place.
    #[test]
    fn look_alike_ligus_agents_are_never_mention_targets() {
        let s = session_with(vec![WorkspaceAgent::new("l\u{0456}gus", "evil"), WorkspaceAgent::new("li\u{200B}gus", "evil")], vec![]);
        for body in ["@l\u{0456}gus hi", "@li\u{200B}gus hi", "@ligus hi"] {
            assert!(parse_mentions(body, &s).agents.is_empty(), "{body:?}");
        }
        assert!(parse_mentions("@ligus hi", &s).primary);
    }

    #[test]
    fn at_needs_a_left_boundary_so_emails_arent_mentions() {
        let scout = WorkspaceAgent::new("scout", "r1");
        let s = session_with(vec![scout], vec![]);
        // Email / inline-handle forms must NOT summon the primary or an agent.
        assert!(!parse_mentions("reach me at me@ligus.io", &s).primary);
        assert!(!parse_mentions("email support@primary please", &s).primary);
        assert!(parse_mentions("ping user@scout for that", &s).agents.is_empty());
        // A real mention (boundary before @) still resolves.
        assert!(parse_mentions("hey @ligus can you look?", &s).primary);
        assert!(parse_mentions("(@scout) take a look", &s).agents.len() == 1);
    }

    #[test]
    fn matches_agent_by_name_not_substring() {
        let scout = WorkspaceAgent::new("Scout", "r1");
        let scout_id = scout.id;
        let s = session_with(vec![scout], vec![]);
        assert_eq!(parse_mentions("ask @Scout please", &s).agents, vec![scout_id]);
        // substring should not match
        assert!(parse_mentions("email scout@example.com", &s).agents.is_empty());
        assert!(parse_mentions("@Scouts go", &s).agents.is_empty());
    }

    #[test]
    fn matches_role_group_and_human() {
        let mara = WorkspaceMember {
            id: "m1".into(),
            actor: ActorIdentity::new("u1", "Mara", ActorKind::Human),
            role: WorkspaceRole::Owner,
            title: String::new(),
            index: 1,
            joined_at: Default::default(),
        };
        let s = session_with(vec![], vec![mara]);
        let t = parse_mentions("@admins please review, cc @Mara", &s);
        assert_eq!(t.roles, vec![WorkspaceRole::Admin]);
        assert_eq!(t.humans, vec!["m1".to_string()]);
    }

    #[test]
    fn empty_when_no_mentions() {
        let s = session_with(vec![], vec![]);
        assert!(parse_mentions("just a normal message", &s).is_empty());
    }

    #[test]
    fn name_collision_pings_all_matching_humans() {
        // Two members share the display name "Sam" — `@Sam` must not silently
        // fail; it notifies every Sam (graceful disambiguation).
        let make = |id: &str, actor: &str, index: u32| WorkspaceMember {
            id: id.into(),
            actor: ActorIdentity::new(actor, "Sam", ActorKind::Human),
            role: WorkspaceRole::Contributor,
            title: String::new(),
            index,
            joined_at: Default::default(),
        };
        let s = session_with(vec![], vec![make("m1", "u1", 1), make("m2", "u2", 2)]);
        // Bare `@Sam` is ambiguous → pings both.
        let all = parse_mentions("@Sam can you take this?", &s);
        assert_eq!(all.humans.len(), 2);
        assert!(all.humans.contains(&"m1".to_string()));
        assert!(all.humans.contains(&"m2".to_string()));
        // `@Sam#2` targets exactly the second Sam.
        let precise = parse_mentions("@Sam#2 specifically", &s);
        assert_eq!(precise.humans, vec!["m2".to_string()]);
    }
}

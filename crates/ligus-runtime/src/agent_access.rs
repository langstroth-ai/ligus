//! Agent access controls: who may use an agent, its daily budgets, and the
//! usage it is counted against. The same on every host that runs agents
//! (the desktop, `ligus worker` / `ligus serve`, `lt`).
//!
//! An agent's turn runs on its owner's device or its host worker, on their
//! credentials (an API key, a subscription CLI, a local model). So the
//! device or worker that would run it decides, **before it claims the turn
//! or calls a model**:
//!
//! - **Who may use it** (`WorkspaceAgent::access`, signed, owner-only): a
//!   personal agent is its owner's alone unless they open it to everyone; a
//!   shared (unowned) agent is everyone's unless an Admin restricts it to
//!   Admins. **Every** person behind the turn must be allowed: the message's
//!   author, every human the agent-to-agent chain reaches, a hand-off's
//!   originator and a workflow run's initiator
//!   ([`ChatService::trigger_people`], the author gate's walk). A chain with
//!   no person, or one that can't be resolved, runs only on an agent open to
//!   everyone.
//! - **Its daily budgets** (`WorkspaceAgent::limits`): turns per member per
//!   UTC day (the owner's own turns are never limited), and an optional
//!   estimated USD cap for the agent overall (once reached, other members'
//!   turns are refused; the owner's still run). Counted in this device's own
//!   database (`event_store::agent_usage`); a runtime that reports no token
//!   usage is counted in turns only.
//!
//! A refused turn gets one short reply in the agent's name that no model
//! wrote ([`ChatService::refuse_agent_turn`]); nothing is claimed or spent.
//! It answers the trigger, so no other device (the sender's deferred
//! fallback included) picks it up.

use ligus_core::agent::{effective_access, usable_by, Access};
use ligus_core::{DeviceLinks, WorkspaceAgent, WorkspaceRole};
use uuid::Uuid;

use crate::chat_service::Result;
use crate::event_store::{utc_day, AgentUsageRow};
use crate::ChatService;

/// An allowed turn: who it is counted against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentUse {
    /// The person nearest the trigger (a user message's author), as a
    /// member id mapped to its person; empty when no person could be traced
    /// (only on an agent open to everyone).
    pub requester: String,
    /// The requester isn't the agent's owner: someone else is using a
    /// personal agent. Their turn is background work for the plan-window
    /// throttle on the owner's runtime, and the owner may be notified.
    pub teammate: bool,
    /// The owner asked to be told when someone else uses the agent.
    pub notify: bool,
}

/// What the device that would run an agent decides about a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessDecision {
    Allow(AgentUse),
    /// The short note the turn is answered with instead.
    Refuse(String),
}

impl AccessDecision {
    pub fn allowed(&self) -> bool {
        matches!(self, AccessDecision::Allow(_))
    }
}

/// What the device has counted today for one agent.
#[derive(Debug, Clone, Default)]
pub struct Today {
    pub rows: Vec<AgentUsageRow>,
}

impl Today {
    pub fn turns_of(&self, member: &str) -> u64 {
        self.rows
            .iter()
            .filter(|r| r.member == member)
            .map(|r| r.turns)
            .sum()
    }
    pub fn cost_usd(&self) -> f64 {
        self.rows.iter().map(|r| r.cost_usd).sum()
    }
}

/// How long after a refusal note to one member about one agent further
/// refusals of theirs are silent (a member mentioning an agent they can't
/// use in a loop would otherwise make its host write a note per message).
pub const REFUSAL_NOTE_WINDOW: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// The device-local memory behind refusal notes: when each (member, agent)
/// was last told, and the turns refused silently since (so a host's queue
/// doesn't judge them again every tick). Never synced.
#[derive(Debug, Default)]
pub struct RefusalMemory {
    told: std::collections::HashMap<(String, String), std::time::Instant>,
    silent: std::collections::HashMap<(Uuid, String), String>,
}

impl RefusalMemory {
    const CAP: usize = 8192;

    /// Whether to post a note to `member` about `agent` now (and remember
    /// that it was).
    fn tell(&mut self, member: &str, agent: &str, now: std::time::Instant) -> bool {
        if self.told.len() >= Self::CAP {
            self.told
                .retain(|_, at| now.duration_since(*at) < REFUSAL_NOTE_WINDOW);
        }
        let key = (member.to_string(), agent.to_string());
        match self.told.get(&key) {
            Some(at) if now.duration_since(*at) < REFUSAL_NOTE_WINDOW => false,
            _ => {
                self.told.insert(key, now);
                true
            }
        }
    }

    fn hush(&mut self, trigger: Uuid, agent: &str, note: &str) {
        if self.silent.len() >= Self::CAP {
            self.silent.clear();
        }
        self.silent
            .insert((trigger, agent.to_string()), note.to_string());
    }

    /// The note a turn was silently refused with, if it was.
    fn hushed(&self, trigger: Uuid, agent: &str) -> Option<&String> {
        self.silent.get(&(trigger, agent.to_string()))
    }
}

/// The note for a teammate's `@ligus` (the chat's primary runtime) on a
/// device whose person hasn't opened it to members. Names no one and no
/// setting of theirs.
pub const PRIMARY_NOT_AVAILABLE: &str =
    "Ligus in this chat runs on another member's device and account, and only answers them. Mention one of your own agents instead, or ask them to let members use it.";

/// The note for a person the agent isn't open to.
pub fn not_available_note(agent: &WorkspaceAgent) -> String {
    if agent.owner_actor_id.is_empty() {
        format!("{} is only available to workspace Admins.", agent.name)
    } else {
        format!("{} is only available to its owner.", agent.name)
    }
}

/// The decision, from its inputs. `people` are the persons behind the
/// trigger chain, nearest first, already mapped through device links (and
/// `owner` likewise); `None` = no person or unresolved. `role_of` is a
/// person's current workspace role.
pub fn decide(
    agent: &WorkspaceAgent,
    owner: &str,
    people: Option<&[String]>,
    role_of: &dyn Fn(&str) -> Option<WorkspaceRole>,
    today: &Today,
) -> AccessDecision {
    let access = effective_access(&agent.access, &agent.owner_actor_id);
    let requester = match people {
        Some(p) if !p.is_empty() => {
            if p.iter()
                .any(|who| !usable_by(access, owner, who, role_of(who)))
            {
                return AccessDecision::Refuse(not_available_note(agent));
            }
            p[0].clone()
        }
        _ if access == Access::Everyone => String::new(),
        _ => return AccessDecision::Refuse(not_available_note(agent)),
    };
    let personal = !owner.is_empty();
    let owner_turn = personal && requester == owner;
    if !owner_turn {
        if let Some(cap) = agent.limits.member_turn_cap(personal) {
            // Every person behind the turn other than the owner must be under
            // the cap (the requester first): a member past theirs can't ask
            // through another member's agent.
            let over =
                |who: &String| !(personal && who == owner) && today.turns_of(who) >= u64::from(cap);
            if over(&requester) || people.is_some_and(|p| p.iter().any(over)) {
                return AccessDecision::Refuse(format!(
                    "{} reached today's limit of {cap} turn{} per member (resets at 00:00 UTC).",
                    agent.name,
                    if cap == 1 { "" } else { "s" }
                ));
            }
        }
        if let Some(cents) = agent.limits.usd_cents_per_day {
            if today.cost_usd() * 100.0 >= cents as f64 {
                return AccessDecision::Refuse(format!(
                    "{} reached its daily spending cap of ${}.{:02} (resets at 00:00 UTC; its owner's own turns still run).",
                    agent.name,
                    cents / 100,
                    cents % 100
                ));
            }
        }
    }
    let teammate = personal && !owner_turn;
    AccessDecision::Allow(AgentUse {
        requester,
        teammate,
        notify: teammate && agent.notify_on_use,
    })
}

/// Whether `member` (mapped through device links) may use `agent` at all,
/// for the @mention autocomplete and the phone's `available` flag; the
/// reason when not. Budgets aren't checked here.
pub fn availability(
    agent: &WorkspaceAgent,
    links: &DeviceLinks,
    member: &str,
    role: Option<WorkspaceRole>,
) -> Option<String> {
    let owner = links.person(&agent.owner_actor_id);
    let access = effective_access(&agent.access, &agent.owner_actor_id);
    if usable_by(access, owner, links.person(member), role) {
        None
    } else if agent.owner_actor_id.is_empty() {
        Some("Only workspace Admins can use this agent".into())
    } else {
        Some("Only its owner can use this agent".into())
    }
}

/// A person's best current role among their member records (their own and
/// a linked box's service member), mapped through `links`.
pub fn person_role(
    roster: Option<&ligus_core::authz::RosterState>,
    links: &DeviceLinks,
    person: &str,
) -> Option<WorkspaceRole> {
    roster?
        .members
        .iter()
        .filter(|(id, _)| links.person(id) == person)
        .map(|(_, m)| m.role)
        .max_by_key(|r| r.rank())
}

/// The name the primary runtime's replies (and refusal notes) carry.
pub const PRIMARY_NAME: &str = ligus_core::agent::PRIMARY_NAME;

/// The message a turn answering `trigger` is judged by: a turn answering a
/// System row (a note trailing the message, as the "last message" rule
/// picks it) answers the message before it, since a note isn't anyone
/// speaking.
fn judged_trigger(session: &ligus_core::ChatSession, trigger: Uuid) -> Uuid {
    match session.messages.iter().position(|m| m.id == trigger) {
        Some(at) if session.messages[at].role == ligus_core::MessageRole::System => session
            .messages[..at]
            .iter()
            .rev()
            .find(|m| m.role != ligus_core::MessageRole::System)
            .map_or(trigger, |m| m.id),
        _ => trigger,
    }
}

/// `"owner"` or `"everyone"`: who may use `agent` as enforced.
pub fn access_label(agent: &WorkspaceAgent) -> &'static str {
    match agent.effective_access() {
        Access::Owner => ligus_core::agent::ACCESS_OWNER,
        Access::Everyone => ligus_core::agent::ACCESS_EVERYONE,
    }
}

/// One agent's usage on this device, per member: today and this month (UTC).
#[derive(Debug, Clone, Default)]
pub struct UsageView {
    pub day: String,
    pub today: Vec<AgentUsageRow>,
    pub month: Vec<AgentUsageRow>,
}

impl ChatService {
    /// Whether this device may run `agent_id`'s turn answering `trigger` in
    /// `session_id`, and who it is counted against (module docs). In the
    /// device-local workspace (one person, never synced) every turn is the
    /// owner's.
    pub fn agent_turn_access(
        &self,
        workspace_id: Uuid,
        session_id: Uuid,
        trigger: Uuid,
        agent_id: Uuid,
    ) -> Result<AccessDecision> {
        Ok(self
            .agent_turn_access_for(workspace_id, session_id, trigger, agent_id)?
            .0)
    }

    /// [`Self::agent_turn_access`] and the person the turn is for (the
    /// requester, for a refusal note's rate limit; empty when untraced).
    fn agent_turn_access_for(
        &self,
        workspace_id: Uuid,
        session_id: Uuid,
        trigger: Uuid,
        agent_id: Uuid,
    ) -> Result<(AccessDecision, String)> {
        let Some(session) = self.load_in(workspace_id, session_id)? else {
            return Ok((
                AccessDecision::Refuse("the chat is gone".into()),
                String::new(),
            ));
        };
        let Some(agent) = session.workspace_agents.iter().find(|a| a.id == agent_id) else {
            return Ok((
                AccessDecision::Refuse("the agent is no longer in this workspace".into()),
                String::new(),
            ));
        };
        if self.is_local_workspace(session.workspace_id) {
            let me = self.author().id.clone();
            return Ok((
                AccessDecision::Allow(AgentUse {
                    requester: me.clone(),
                    teammate: false,
                    notify: false,
                }),
                me,
            ));
        }
        let trigger = judged_trigger(&session, trigger);
        let links = self.device_links(session.workspace_id).unwrap_or_default();
        let roster = self.workspace_roster(session.workspace_id).ok();
        let mut people = self.people_behind(workspace_id, session_id, trigger, &links)?;
        // A tool or directive hand-off card waking this agent on `trigger`
        // is a request by whoever signed it.
        match self.handoff_card_people_in(workspace_id, session_id, trigger, agent_id)? {
            Some(cards) => {
                if let Some(p) = people.as_mut() {
                    for who in cards {
                        let person = links.person(&who).to_string();
                        if !p.contains(&person) {
                            p.push(person);
                        }
                    }
                }
            }
            None => people = None,
        }
        let requester = people
            .as_ref()
            .and_then(|p| p.first().cloned())
            .unwrap_or_default();
        // The only person in the room asking on their own device: an owner
        // id that isn't theirs is a stale identity of their own (a reset,
        // an import), as dispatch's solo rule already assumes. Nobody else
        // is there to protect.
        let me = links.person(&self.author().id).to_string();
        if crate::turn_dispatch::human_member_count(&session, &links) <= 1
            && people
                .as_ref()
                .is_some_and(|p| !p.is_empty() && p.iter().all(|x| *x == me))
        {
            return Ok((
                AccessDecision::Allow(AgentUse {
                    requester: me,
                    teammate: false,
                    notify: false,
                }),
                requester,
            ));
        }
        let role_of = |who: &str| person_role(roster.as_ref(), &links, who);
        let day = utc_day(time::OffsetDateTime::now_utc());
        let today = Today {
            rows: self.store().agent_usage(agent_id, &day, &day)?,
        };
        let owner = links.person(&agent.owner_actor_id).to_string();
        Ok((
            decide(agent, &owner, people.as_deref(), &role_of, &today),
            requester,
        ))
    }

    /// The people behind `trigger`, mapped through `links`, deduplicated,
    /// nearest (the requester) first.
    fn people_behind(
        &self,
        workspace_id: Uuid,
        session_id: Uuid,
        trigger: Uuid,
        links: &DeviceLinks,
    ) -> Result<Option<Vec<String>>> {
        Ok(self
            .trigger_people_in(workspace_id, session_id, trigger)?
            .map(|p| {
                let mut out: Vec<String> = Vec::new();
                for who in p {
                    let person = links.person(&who).to_string();
                    if !out.contains(&person) {
                        out.push(person);
                    }
                }
                out
            }))
    }

    /// Whether the person behind `trigger` may use `agent_id` at all (who,
    /// not budgets): what the router asks before offering an agent an
    /// un-addressed message.
    pub fn agent_usable_for(
        &self,
        workspace_id: Uuid,
        session_id: Uuid,
        trigger: Uuid,
        agent_id: Uuid,
    ) -> bool {
        let Ok(Some(session)) = self.load_in(workspace_id, session_id) else {
            return false;
        };
        let Some(agent) = session.workspace_agents.iter().find(|a| a.id == agent_id) else {
            return false;
        };
        if self.is_local_workspace(session.workspace_id) {
            return true;
        }
        let links = self.device_links(session.workspace_id).unwrap_or_default();
        let roster = self.workspace_roster(session.workspace_id).ok();
        let Ok(people) = self.people_behind(workspace_id, session_id, trigger, &links) else {
            return false;
        };
        let me = links.person(&self.author().id).to_string();
        if crate::turn_dispatch::human_member_count(&session, &links) <= 1
            && people
                .as_ref()
                .is_some_and(|p| !p.is_empty() && p.iter().all(|x| *x == me))
        {
            return true;
        }
        let owner = links.person(&agent.owner_actor_id).to_string();
        let access = effective_access(&agent.access, &agent.owner_actor_id);
        match people {
            Some(p) if !p.is_empty() => p.iter().all(|who| {
                usable_by(
                    access,
                    &owner,
                    who,
                    person_role(roster.as_ref(), &links, who),
                )
            }),
            _ => access == Access::Everyone,
        }
    }

    /// Whether this device may run the chat's **primary** runtime (`@ligus`,
    /// no agent) answering `trigger`. It runs on this device's own
    /// credentials, so in a shared room only this device's person may use it
    /// unless they opened it to members (`open`, a device-local setting).
    /// Alone in the room, or in the device-local workspace, it always
    /// answers.
    pub fn primary_turn_access(
        &self,
        workspace_id: Uuid,
        session_id: Uuid,
        trigger: Uuid,
        open: bool,
    ) -> Result<AccessDecision> {
        Ok(self
            .primary_turn_access_for(workspace_id, session_id, trigger, open)?
            .0)
    }

    fn primary_turn_access_for(
        &self,
        workspace_id: Uuid,
        session_id: Uuid,
        trigger: Uuid,
        open: bool,
    ) -> Result<(AccessDecision, String)> {
        let Some(session) = self.load_in(workspace_id, session_id)? else {
            return Ok((
                AccessDecision::Refuse("the chat is gone".into()),
                String::new(),
            ));
        };
        let links = self.device_links(session.workspace_id).unwrap_or_default();
        let me = links.person(&self.author().id).to_string();
        if self.is_local_workspace(session.workspace_id)
            || crate::turn_dispatch::human_member_count(&session, &links) <= 1
        {
            return Ok((
                AccessDecision::Allow(AgentUse {
                    requester: me.clone(),
                    teammate: false,
                    notify: false,
                }),
                me,
            ));
        }
        let trigger = judged_trigger(&session, trigger);
        let people = self.people_behind(workspace_id, session_id, trigger, &links)?;
        let requester = people
            .as_ref()
            .and_then(|p| p.first().cloned())
            .unwrap_or_default();
        Ok(match people {
            Some(p) if !p.is_empty() && p.iter().all(|x| *x == me) => (
                AccessDecision::Allow(AgentUse {
                    requester: me,
                    teammate: false,
                    notify: false,
                }),
                requester,
            ),
            _ if open => (
                AccessDecision::Allow(AgentUse {
                    requester: requester.clone(),
                    teammate: true,
                    notify: false,
                }),
                requester,
            ),
            _ => (
                AccessDecision::Refuse(PRIMARY_NOT_AVAILABLE.into()),
                requester,
            ),
        })
    }

    /// Answer a refused turn with `note`, in the agent's name, unless it is
    /// answered already (another device refused it first). No model runs.
    /// Returns whether a note was posted. Not rate limited: see
    /// [`Self::note_refusal`].
    pub fn refuse_agent_turn(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        trigger: Uuid,
        agent: &WorkspaceAgent,
        note: &str,
    ) -> Result<bool> {
        self.refuse_turn(session_id, workspace_id, trigger, Some(agent), note)
    }

    /// [`Self::refuse_agent_turn`] for an agent, or for the primary
    /// (`None`: the note is in Ligus's name, with no responder).
    fn refuse_turn(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        trigger: Uuid,
        agent: Option<&WorkspaceAgent>,
        note: &str,
    ) -> Result<bool> {
        let Some(s) = self.load_in(workspace_id, session_id)? else {
            return Ok(false);
        };
        let (name, runtime) = match agent {
            Some(a) => (a.name.clone(), a.runtime_id.clone()),
            None => (PRIMARY_NAME.to_string(), s.runtime_id.clone()),
        };
        let answered = {
            let Some(at) = s.messages.iter().position(|m| m.id == trigger) else {
                return Ok(false);
            };
            s.messages[at + 1..].iter().any(|m| {
                matches!(
                    m.role,
                    ligus_core::MessageRole::Assistant | ligus_core::MessageRole::Agent
                ) && !m.superseded
                    && m.reply_to_message_id == Some(trigger)
                    && match agent {
                        Some(a) => m.responder_id == Some(a.id) || m.author == a.name,
                        None => m.responder_id.is_none(),
                    }
            })
        };
        if answered {
            return Ok(false);
        }
        let mid = self.begin_assistant_message(
            session_id,
            workspace_id,
            name,
            runtime,
            agent.map(|a| a.id),
            Some(trigger),
        )?;
        self.complete_assistant_message(session_id, workspace_id, mid, note.to_string())?;
        Ok(true)
    }

    /// Answer a refused turn with `note` (as [`Self::refuse_agent_turn`]),
    /// at most once per [`REFUSAL_NOTE_WINDOW`] for `requester` and the
    /// agent (`None`: the primary); otherwise silently, and the turn is
    /// remembered as refused so this device doesn't judge it again. Returns
    /// whether a note was posted.
    pub fn note_refusal(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        trigger: Uuid,
        agent: Option<&WorkspaceAgent>,
        requester: &str,
        note: &str,
    ) -> Result<bool> {
        let key = ligus_core::flow::agent_key(agent.map(|a| a.id));
        if self.refusals.hushed(trigger, &key).is_some() {
            return Ok(false);
        }
        if !self
            .refusals
            .tell(requester, &key, std::time::Instant::now())
        {
            self.refusals.hush(trigger, &key, note);
            return Ok(false);
        }
        self.refuse_turn(session_id, workspace_id, trigger, agent, note)
    }

    /// Check `agent_id`'s turn and, when refused, answer it with the note
    /// (rate limited, [`Self::note_refusal`]). `Ok(Ok(use))` = run it (with
    /// who it's counted against); `Ok(Err(note))` = refused.
    pub fn gate_agent_turn(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        trigger: Uuid,
        agent_id: Uuid,
    ) -> Result<std::result::Result<AgentUse, String>> {
        // Refused silently already: not judged again (a host's queue asks
        // every tick).
        if let Some(note) = self
            .refusals
            .hushed(trigger, &ligus_core::flow::agent_key(Some(agent_id)))
        {
            return Ok(Err(note.clone()));
        }
        let (decision, requester) =
            self.agent_turn_access_for(workspace_id, session_id, trigger, agent_id)?;
        match decision {
            AccessDecision::Allow(u) => Ok(Ok(u)),
            AccessDecision::Refuse(note) => {
                let agent = self
                    .load_in(workspace_id, session_id)?
                    .and_then(|s| s.workspace_agents.into_iter().find(|a| a.id == agent_id));
                if let Some(agent) = agent {
                    if self.note_refusal(
                        session_id,
                        workspace_id,
                        trigger,
                        Some(&agent),
                        &requester,
                        &note,
                    )? {
                        tracing::info!(target: "dispatch", agent = %agent.name, "agent turn refused: {note}");
                    }
                }
                Ok(Err(note))
            }
        }
    }

    /// [`Self::gate_agent_turn`] and, when allowed, the turn counted in the
    /// same step (so two turns admitted back to back can't both pass a cap
    /// of one). Returns the use and the UTC day it was counted on.
    pub fn admit_agent_turn(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        trigger: Uuid,
        agent_id: Uuid,
    ) -> Result<std::result::Result<(AgentUse, String), String>> {
        match self.gate_agent_turn(session_id, workspace_id, trigger, agent_id)? {
            Ok(u) => {
                let day = self.record_agent_turn(workspace_id, agent_id, &u.requester)?;
                Ok(Ok((u, day)))
            }
            Err(note) => Ok(Err(note)),
        }
    }

    /// [`Self::primary_turn_access`] and, when refused, the note in Ligus's
    /// name (rate limited).
    pub fn gate_primary_turn(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        trigger: Uuid,
        open: bool,
    ) -> Result<std::result::Result<AgentUse, String>> {
        if let Some(note) = self.refusals.hushed(trigger, ligus_core::flow::PRIMARY) {
            return Ok(Err(note.clone()));
        }
        let (decision, requester) =
            self.primary_turn_access_for(workspace_id, session_id, trigger, open)?;
        match decision {
            AccessDecision::Allow(u) => Ok(Ok(u)),
            AccessDecision::Refuse(note) => {
                if self.note_refusal(session_id, workspace_id, trigger, None, &requester, &note)? {
                    tracing::info!(target: "dispatch", "primary turn refused: {note}");
                }
                Ok(Err(note))
            }
        }
    }

    /// Count one turn of `agent` for `requester` today (UTC).
    pub fn record_agent_turn(
        &self,
        workspace_id: Uuid,
        agent: Uuid,
        requester: &str,
    ) -> Result<String> {
        let day = utc_day(time::OffsetDateTime::now_utc());
        self.store()
            .record_agent_turn(workspace_id, agent, requester, &day)?;
        Ok(day)
    }

    /// Add a finished turn's estimated cost to `day`'s count (`None`: the
    /// runtime reported no usage).
    pub fn record_agent_cost(
        &self,
        workspace_id: Uuid,
        agent: Uuid,
        requester: &str,
        day: &str,
        usd: Option<f64>,
    ) -> Result<()> {
        Ok(self
            .store()
            .add_agent_cost(workspace_id, agent, requester, day, usd)?)
    }

    /// Change who may use `agent_id` and its budgets (`access`: `"owner"`
    /// or `"everyone"`). Its owner's alone (an Admin's on a shared agent):
    /// the fold refuses anyone else's (`authz::agent_policy_refusal`), and
    /// this refuses it up front with the reason.
    pub fn set_agent_policy(
        &mut self,
        workspace_id: Uuid,
        agent_id: Uuid,
        policy: ligus_core::AgentPolicy,
    ) -> Result<()> {
        use crate::chat_service::ChatError;
        if !matches!(
            policy.access.as_str(),
            ligus_core::agent::ACCESS_OWNER | ligus_core::agent::ACCESS_EVERYONE
        ) {
            return Err(ChatError::Unauthorized(format!(
                "unknown access {:?}: use \"owner\" or \"everyone\"",
                policy.access
            )));
        }
        let config_id = self.ensure_workspace_config(workspace_id)?;
        let agents = self
            .load_in(workspace_id, config_id)?
            .map(|s| s.workspace_agents)
            .unwrap_or_default();
        let Some(mut agent) = agents.into_iter().find(|a| a.id == agent_id) else {
            return Err(ChatError::NotFound("unknown agent".into()));
        };
        if !self.may_manage_agent(workspace_id, &agent) {
            return Err(ChatError::Unauthorized(
                if agent.owner_actor_id.is_empty() {
                    "only an Admin changes who may use a shared agent".into()
                } else {
                    "only the agent's owner changes who may use it and its budgets".into()
                },
            ));
        }
        agent.access = policy.access;
        agent.limits = policy.limits;
        agent.notify_on_use = policy.notify_on_use;
        self.add_agent(config_id, workspace_id, agent)
    }

    /// Whether this device's person may change `agent`'s access and budgets
    /// (and see its usage): its owner, or on a shared agent an Admin.
    pub fn may_manage_agent(&self, workspace_id: Uuid, agent: &WorkspaceAgent) -> bool {
        if self.is_local_workspace(workspace_id) {
            return true;
        }
        let links = self.device_links(workspace_id).unwrap_or_default();
        let me = links.person(&self.author().id).to_string();
        if agent.owner_actor_id.is_empty() {
            let roster = self.workspace_roster(workspace_id).ok();
            person_role(roster.as_ref(), &links, &me)
                .is_some_and(|r| r.rank() >= WorkspaceRole::Admin.rank())
        } else {
            links.person(&agent.owner_actor_id) == me
        }
    }

    /// `agent`'s usage on this device: today and this month (UTC).
    pub fn agent_usage_view(&self, agent: Uuid) -> Result<UsageView> {
        let now = time::OffsetDateTime::now_utc();
        let day = utc_day(now);
        let month_start = format!("{}-01", &day[..7]);
        Ok(UsageView {
            today: self.store().agent_usage(agent, &day, &day)?,
            month: self.store().agent_usage(agent, &month_start, &day)?,
            day,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ligus_core::agent::{ACCESS_EVERYONE, ACCESS_OWNER};
    use WorkspaceRole::*;

    fn personal() -> WorkspaceAgent {
        let mut a = WorkspaceAgent::new("scout", "rt");
        a.owner_actor_id = "mara".into();
        a
    }

    fn roles(who: &str) -> Option<WorkspaceRole> {
        match who {
            "owner" => Some(Owner),
            "adm" => Some(Admin),
            "mara" | "carl" => Some(Contributor),
            _ => None,
        }
    }

    fn p(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    fn row(member: &str, turns: u64, cost: f64) -> AgentUsageRow {
        AgentUsageRow {
            member: member.into(),
            turns,
            cost_usd: cost,
            uncosted: 0,
        }
    }

    #[test]
    fn a_personal_agent_is_its_owners_until_opened() {
        let a = personal();
        let t = Today::default();
        assert_eq!(
            decide(&a, "mara", Some(&p(&["mara"])), &roles, &t),
            AccessDecision::Allow(AgentUse {
                requester: "mara".into(),
                teammate: false,
                notify: false
            })
        );
        assert_eq!(
            decide(&a, "mara", Some(&p(&["carl"])), &roles, &t),
            AccessDecision::Refuse("scout is only available to its owner.".into())
        );
        assert!(
            !decide(&a, "mara", Some(&p(&["owner"])), &roles, &t).allowed(),
            "a workspace Owner is someone else too"
        );
        assert!(
            !decide(&a, "mara", None, &roles, &t).allowed(),
            "no person behind it"
        );
        let mut open = a.clone();
        open.access = ACCESS_EVERYONE.into();
        open.notify_on_use = true;
        assert_eq!(
            decide(&open, "mara", Some(&p(&["carl"])), &roles, &t),
            AccessDecision::Allow(AgentUse {
                requester: "carl".into(),
                teammate: true,
                notify: true
            })
        );
        assert!(
            decide(&open, "mara", None, &roles, &t).allowed(),
            "open to everyone: an untraced chain runs"
        );
    }

    #[test]
    fn every_person_behind_a_chain_must_be_allowed() {
        let a = personal();
        // The owner's message is nearest, but a teammate started the chain.
        assert!(!decide(
            &a,
            "mara",
            Some(&p(&["mara", "carl"])),
            &roles,
            &Today::default()
        )
        .allowed());
        assert!(decide(&a, "mara", Some(&p(&["mara"])), &roles, &Today::default()).allowed());
    }

    #[test]
    fn a_shared_agent_restricted_is_for_admins() {
        let mut a = WorkspaceAgent::new("helper", "rt");
        assert!(decide(&a, "", Some(&p(&["carl"])), &roles, &Today::default()).allowed());
        a.access = ACCESS_OWNER.into();
        assert_eq!(
            decide(&a, "", Some(&p(&["carl"])), &roles, &Today::default()),
            AccessDecision::Refuse("helper is only available to workspace Admins.".into())
        );
        assert!(decide(&a, "", Some(&p(&["adm"])), &roles, &Today::default()).allowed());
    }

    #[test]
    fn member_budgets_spare_the_owner() {
        let mut a = personal();
        a.access = ACCESS_EVERYONE.into();
        let full = Today {
            rows: vec![row("carl", 50, 0.0), row("mara", 400, 0.0)],
        };
        let r = decide(&a, "mara", Some(&p(&["carl"])), &roles, &full);
        assert_eq!(
            r,
            AccessDecision::Refuse(
                "scout reached today's limit of 50 turns per member (resets at 00:00 UTC).".into()
            )
        );
        assert!(
            decide(&a, "mara", Some(&p(&["mara"])), &roles, &full).allowed(),
            "the owner is never limited"
        );
        assert!(
            decide(&a, "mara", Some(&p(&["adm"])), &roles, &full).allowed(),
            "per member"
        );
        a.limits.member_turns_per_day = Some(0);
        assert!(
            decide(&a, "mara", Some(&p(&["carl"])), &roles, &full).allowed(),
            "0 is unlimited"
        );
        a.limits.member_turns_per_day = Some(1);
        let one = Today {
            rows: vec![row("carl", 1, 0.0)],
        };
        assert!(
            decide(&a, "mara", Some(&p(&["carl"])), &roles, &one)
                == AccessDecision::Refuse(
                    "scout reached today's limit of 1 turn per member (resets at 00:00 UTC)."
                        .into()
                )
        );
        // A shared agent has no default cap.
        let shared = WorkspaceAgent::new("helper", "rt");
        assert!(decide(
            &shared,
            "",
            Some(&p(&["carl"])),
            &roles,
            &Today {
                rows: vec![row("carl", 10_000, 0.0)]
            }
        )
        .allowed());
    }

    #[test]
    fn the_spending_cap_stops_others_only() {
        let mut a = personal();
        a.access = ACCESS_EVERYONE.into();
        a.limits.usd_cents_per_day = Some(250);
        let under = Today {
            rows: vec![row("carl", 1, 1.0), row("mara", 1, 1.49)],
        };
        assert!(decide(&a, "mara", Some(&p(&["carl"])), &roles, &under).allowed());
        let over = Today {
            rows: vec![row("carl", 1, 1.0), row("mara", 1, 1.5)],
        };
        assert_eq!(
            decide(&a, "mara", Some(&p(&["carl"])), &roles, &over),
            AccessDecision::Refuse("scout reached its daily spending cap of $2.50 (resets at 00:00 UTC; its owner's own turns still run).".into())
        );
        assert!(decide(&a, "mara", Some(&p(&["mara"])), &roles, &over).allowed());
    }

    #[test]
    fn availability_for_the_autocomplete() {
        let links = DeviceLinks::default();
        let a = personal();
        assert_eq!(availability(&a, &links, "mara", Some(Contributor)), None);
        assert_eq!(
            availability(&a, &links, "carl", Some(Admin)).as_deref(),
            Some("Only its owner can use this agent")
        );
        let mut shared = WorkspaceAgent::new("h", "rt");
        assert_eq!(
            availability(&shared, &links, "carl", Some(Contributor)),
            None
        );
        shared.access = ACCESS_OWNER.into();
        assert!(availability(&shared, &links, "carl", Some(Contributor)).is_some());
        assert!(availability(&shared, &links, "adm", Some(Admin)).is_none());
    }
}

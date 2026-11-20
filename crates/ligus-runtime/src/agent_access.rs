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

use crate::event_store::{utc_day, AgentUsageRow};
use crate::chat_service::Result;
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
        self.rows.iter().filter(|r| r.member == member).map(|r| r.turns).sum()
    }
    pub fn cost_usd(&self) -> f64 {
        self.rows.iter().map(|r| r.cost_usd).sum()
    }
}

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
            if p.iter().any(|who| !usable_by(access, owner, who, role_of(who))) {
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
            if today.turns_of(&requester) >= u64::from(cap) {
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
    AccessDecision::Allow(AgentUse { requester, teammate, notify: teammate && agent.notify_on_use })
}

/// Whether `member` (mapped through device links) may use `agent` at all,
/// for the @mention autocomplete and the phone's `available` flag; the
/// reason when not. Budgets aren't checked here.
pub fn availability(agent: &WorkspaceAgent, links: &DeviceLinks, member: &str, role: Option<WorkspaceRole>) -> Option<String> {
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
pub fn person_role(roster: Option<&ligus_core::authz::RosterState>, links: &DeviceLinks, person: &str) -> Option<WorkspaceRole> {
    roster?.members.iter().filter(|(id, _)| links.person(id) == person).map(|(_, m)| m.role).max_by_key(|r| r.rank())
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
    pub fn agent_turn_access(&self, session_id: Uuid, trigger: Uuid, agent_id: Uuid) -> Result<AccessDecision> {
        let Some(session) = self.load(session_id)? else {
            return Ok(AccessDecision::Refuse("the chat is gone".into()));
        };
        let Some(agent) = session.workspace_agents.iter().find(|a| a.id == agent_id) else {
            return Ok(AccessDecision::Refuse("the agent is no longer in this workspace".into()));
        };
        if self.is_local_workspace(session.workspace_id) {
            return Ok(AccessDecision::Allow(AgentUse { requester: self.author().id.clone(), teammate: false, notify: false }));
        }
        let links = self.device_links(session.workspace_id).unwrap_or_default();
        let roster = self.workspace_roster(session.workspace_id).ok();
        let people: Option<Vec<String>> = self.trigger_people(session_id, trigger)?.map(|p| {
            let mut out: Vec<String> = Vec::new();
            for who in p {
                let person = links.person(&who).to_string();
                if !out.contains(&person) {
                    out.push(person);
                }
            }
            out
        });
        let role_of = |who: &str| person_role(roster.as_ref(), &links, who);
        let day = utc_day(time::OffsetDateTime::now_utc());
        let today = Today { rows: self.store().agent_usage(agent_id, &day, &day)? };
        let owner = links.person(&agent.owner_actor_id).to_string();
        Ok(decide(agent, &owner, people.as_deref(), &role_of, &today))
    }

    /// Answer a refused turn with `note`, in the agent's name, unless it is
    /// answered already (another device refused it first). No model runs.
    /// Returns whether a note was posted.
    pub fn refuse_agent_turn(&mut self, session_id: Uuid, workspace_id: Uuid, trigger: Uuid, agent: &WorkspaceAgent, note: &str) -> Result<bool> {
        let answered = self.load(session_id)?.is_some_and(|s| {
            let Some(at) = s.messages.iter().position(|m| m.id == trigger) else { return true };
            s.messages[at + 1..].iter().any(|m| {
                matches!(m.role, ligus_core::MessageRole::Assistant | ligus_core::MessageRole::Agent)
                    && !m.superseded
                    && m.reply_to_message_id == Some(trigger)
                    && (m.responder_id == Some(agent.id) || m.author == agent.name)
            })
        });
        if answered {
            return Ok(false);
        }
        let mid = self.begin_assistant_message(session_id, workspace_id, agent.name.clone(), agent.runtime_id.clone(), Some(agent.id), Some(trigger))?;
        self.complete_assistant_message(session_id, workspace_id, mid, note.to_string())?;
        Ok(true)
    }

    /// Check `agent_id`'s turn and, when refused, answer it with the note.
    /// `Ok(None)` = run it (with who it's counted against); `Ok(Some(note))`
    /// = refused (and answered).
    pub fn gate_agent_turn(&mut self, session_id: Uuid, workspace_id: Uuid, trigger: Uuid, agent_id: Uuid) -> Result<std::result::Result<AgentUse, String>> {
        match self.agent_turn_access(session_id, trigger, agent_id)? {
            AccessDecision::Allow(u) => Ok(Ok(u)),
            AccessDecision::Refuse(note) => {
                let agent = self.load(session_id)?.and_then(|s| s.workspace_agents.into_iter().find(|a| a.id == agent_id));
                if let Some(agent) = agent {
                    if self.refuse_agent_turn(session_id, workspace_id, trigger, &agent, &note)? {
                        tracing::info!(target: "dispatch", agent = %agent.name, "agent turn refused: {note}");
                    }
                }
                Ok(Err(note))
            }
        }
    }

    /// Count one turn of `agent` for `requester` today (UTC).
    pub fn record_agent_turn(&self, workspace_id: Uuid, agent: Uuid, requester: &str) -> Result<String> {
        let day = utc_day(time::OffsetDateTime::now_utc());
        self.store().record_agent_turn(workspace_id, agent, requester, &day)?;
        Ok(day)
    }

    /// Add a finished turn's estimated cost to `day`'s count (`None`: the
    /// runtime reported no usage).
    pub fn record_agent_cost(&self, workspace_id: Uuid, agent: Uuid, requester: &str, day: &str, usd: Option<f64>) -> Result<()> {
        Ok(self.store().add_agent_cost(workspace_id, agent, requester, day, usd)?)
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
        AgentUsageRow { member: member.into(), turns, cost_usd: cost, uncosted: 0 }
    }

    #[test]
    fn a_personal_agent_is_its_owners_until_opened() {
        let a = personal();
        let t = Today::default();
        assert_eq!(
            decide(&a, "mara", Some(&p(&["mara"])), &roles, &t),
            AccessDecision::Allow(AgentUse { requester: "mara".into(), teammate: false, notify: false })
        );
        assert_eq!(decide(&a, "mara", Some(&p(&["carl"])), &roles, &t), AccessDecision::Refuse("scout is only available to its owner.".into()));
        assert!(!decide(&a, "mara", Some(&p(&["owner"])), &roles, &t).allowed(), "a workspace Owner is someone else too");
        assert!(!decide(&a, "mara", None, &roles, &t).allowed(), "no person behind it");
        let mut open = a.clone();
        open.access = ACCESS_EVERYONE.into();
        open.notify_on_use = true;
        assert_eq!(
            decide(&open, "mara", Some(&p(&["carl"])), &roles, &t),
            AccessDecision::Allow(AgentUse { requester: "carl".into(), teammate: true, notify: true })
        );
        assert!(decide(&open, "mara", None, &roles, &t).allowed(), "open to everyone: an untraced chain runs");
    }

    #[test]
    fn every_person_behind_a_chain_must_be_allowed() {
        let a = personal();
        // The owner's message is nearest, but a teammate started the chain.
        assert!(!decide(&a, "mara", Some(&p(&["mara", "carl"])), &roles, &Today::default()).allowed());
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
        let full = Today { rows: vec![row("carl", 50, 0.0), row("mara", 400, 0.0)] };
        let r = decide(&a, "mara", Some(&p(&["carl"])), &roles, &full);
        assert_eq!(r, AccessDecision::Refuse("scout reached today's limit of 50 turns per member (resets at 00:00 UTC).".into()));
        assert!(decide(&a, "mara", Some(&p(&["mara"])), &roles, &full).allowed(), "the owner is never limited");
        assert!(decide(&a, "mara", Some(&p(&["adm"])), &roles, &full).allowed(), "per member");
        a.limits.member_turns_per_day = Some(0);
        assert!(decide(&a, "mara", Some(&p(&["carl"])), &roles, &full).allowed(), "0 is unlimited");
        a.limits.member_turns_per_day = Some(1);
        let one = Today { rows: vec![row("carl", 1, 0.0)] };
        assert!(decide(&a, "mara", Some(&p(&["carl"])), &roles, &one) == AccessDecision::Refuse("scout reached today's limit of 1 turn per member (resets at 00:00 UTC).".into()));
        // A shared agent has no default cap.
        let shared = WorkspaceAgent::new("helper", "rt");
        assert!(decide(&shared, "", Some(&p(&["carl"])), &roles, &Today { rows: vec![row("carl", 10_000, 0.0)] }).allowed());
    }

    #[test]
    fn the_spending_cap_stops_others_only() {
        let mut a = personal();
        a.access = ACCESS_EVERYONE.into();
        a.limits.usd_cents_per_day = Some(250);
        let under = Today { rows: vec![row("carl", 1, 1.0), row("mara", 1, 1.49)] };
        assert!(decide(&a, "mara", Some(&p(&["carl"])), &roles, &under).allowed());
        let over = Today { rows: vec![row("carl", 1, 1.0), row("mara", 1, 1.5)] };
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
        assert_eq!(availability(&a, &links, "carl", Some(Admin)).as_deref(), Some("Only its owner can use this agent"));
        let mut shared = WorkspaceAgent::new("h", "rt");
        assert_eq!(availability(&shared, &links, "carl", Some(Contributor)), None);
        shared.access = ACCESS_OWNER.into();
        assert!(availability(&shared, &links, "carl", Some(Contributor)).is_some());
        assert!(availability(&shared, &links, "adm", Some(Admin)).is_none());
    }
}

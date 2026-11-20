//! Workspace agents — ported from `LigusModels.swift` (`WorkspaceAgent`). An
//! agent is a named participant bound to a runtime and owned by an actor; the
//! `role` is a short human-set specialty shared with the whole workspace.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::time_util::Timestamp;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAgent {
    pub id: Uuid,
    pub name: String,
    pub runtime_id: String,
    #[serde(default)]
    pub owner_actor_id: String,
    /// Short human-set role/specialty (e.g. "Reviewer", "Researcher"),
    /// surfaced to every participant so the workspace shares a roster of who
    /// does what. Empty when unset.
    #[serde(default)]
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_color_hex: Option<String>,
    /// Optional avatar as a bounded `data:image/…` URL. Rides in the synced
    /// roster (`AgentRosterUpdated`), so every member renders the agent's
    /// picture. Size-capped at the edit boundary. `None` = fall back to
    /// `avatar_color_hex`, then to initials-on-color.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    /// Host this agent runs on (spec §12.4) — a device or worker id. Empty =
    /// the owner's device (default). A worker id makes the agent detached.
    #[serde(default)]
    pub host_id: String,
    #[serde(default)]
    pub created_at: Timestamp,
    /// Who may use this agent: `"owner"` (only its owner) or `"everyone"`
    /// (every member who can post). Absent: `"owner"` for a personal agent
    /// (one with an owner) and `"everyone"` for an unowned one. An unknown
    /// value (a later build's member list, say) is read as `"owner"`, so an
    /// older device never opens an agent up. Only the owner changes it (an
    /// Admin, on an unowned agent): `authz::agent_policy_refusal`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub access: String,
    /// Daily budgets, enforced and counted on the device that runs the agent.
    #[serde(default, skip_serializing_if = "AgentLimits::is_default")]
    pub limits: AgentLimits,
    /// Tell the owner when someone else uses this agent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub notify_on_use: bool,
}

/// `WorkspaceAgent::access` values.
pub const ACCESS_OWNER: &str = "owner";
pub const ACCESS_EVERYONE: &str = "everyone";

/// What an agent's [`AgentLimits::member_turns_per_day`] is when unset.
pub const DEFAULT_MEMBER_TURNS_PER_DAY: u32 = 50;

/// An agent's daily budgets (part of its signed settings; the owner's to
/// set). Days are UTC days. Counted on the device or worker that runs the
/// agent, in its own database: two devices that both run an agent (the
/// owner's laptop and desktop) each count their own turns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentLimits {
    /// Turns each member other than the owner may start per day. Absent:
    /// [`DEFAULT_MEMBER_TURNS_PER_DAY`] on a personal agent, no limit on a
    /// shared one. `0`: no per-member limit. The owner's own turns are never
    /// limited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_turns_per_day: Option<u32>,
    /// Estimated spend per day for the agent overall, in US cents, from the
    /// runtime's reported token usage (`spend.rs`). Absent: no cap. Once the
    /// day's spend reaches it, other members' turns are refused (the
    /// owner's still run). A runtime that reports no usage (most
    /// subscription CLIs) can't be counted in dollars, only in turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usd_cents_per_day: Option<u64>,
}

impl AgentLimits {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    /// The per-member daily turn cap, `None` = unlimited. `personal`: the
    /// agent has an owner (only a personal agent has a default cap).
    pub fn member_turn_cap(&self, personal: bool) -> Option<u32> {
        match self.member_turns_per_day {
            None if personal => Some(DEFAULT_MEMBER_TURNS_PER_DAY),
            None => None,
            Some(0) => None,
            Some(n) => Some(n),
        }
    }
}

/// The settings only an agent's owner changes (`access`, `limits`,
/// `notify_on_use`), compared as one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct AgentPolicy {
    pub access: String,
    pub limits: AgentLimits,
    pub notify_on_use: bool,
}

impl AgentPolicy {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

/// Who may use an agent, as enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Its owner only (on an unowned agent: workspace Admins and Owners).
    Owner,
    Everyone,
}

impl WorkspaceAgent {
    pub fn new(name: impl Into<String>, runtime_id: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            runtime_id: runtime_id.into(),
            owner_actor_id: String::new(),
            role: String::new(),
            avatar_color_hex: None,
            avatar_url: None,
            host_id: String::new(),
            created_at: Timestamp::now(),
            access: String::new(),
            limits: AgentLimits::default(),
            notify_on_use: false,
        }
    }

    /// The owner-only settings, as one value.
    pub fn policy(&self) -> AgentPolicy {
        AgentPolicy { access: self.access.clone(), limits: self.limits.clone(), notify_on_use: self.notify_on_use }
    }

    /// Who may use this agent: its `access`, or the default for its kind.
    pub fn effective_access(&self) -> Access {
        effective_access(&self.access, &self.owner_actor_id)
    }

    /// Whether `member` (a member id, already mapped to its person) with
    /// workspace role `role` may use this agent. The owner always may.
    pub fn usable_by(&self, member: &str, role: Option<crate::WorkspaceRole>) -> bool {
        usable_by(self.effective_access(), &self.owner_actor_id, member, role)
    }
}

/// [`WorkspaceAgent::effective_access`] from the raw field and owner.
pub fn effective_access(access: &str, owner: &str) -> Access {
    match access {
        ACCESS_EVERYONE => Access::Everyone,
        ACCESS_OWNER => Access::Owner,
        "" if owner.is_empty() => Access::Everyone,
        // Absent on a personal agent, or a value this build doesn't know.
        _ => Access::Owner,
    }
}

/// [`WorkspaceAgent::usable_by`] from its parts.
pub fn usable_by(access: Access, owner: &str, member: &str, role: Option<crate::WorkspaceRole>) -> bool {
    if member.is_empty() {
        return false;
    }
    match access {
        Access::Everyone => true,
        Access::Owner if owner.is_empty() => role.is_some_and(|r| r.rank() >= crate::WorkspaceRole::Admin.rank()),
        Access::Owner => member == owner,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_minimal_agent_with_defaults() {
        let json = r#"{
            "id":"00000000-0000-0000-0000-0000000000aa",
            "name":"Scout",
            "runtimeId":"local-qwen"
        }"#;
        let agent: WorkspaceAgent = serde_json::from_str(json).unwrap();
        assert_eq!(agent.name, "Scout");
        assert_eq!(agent.runtime_id, "local-qwen");
        assert_eq!(agent.role, "");
        assert_eq!(agent.owner_actor_id, "");
        assert!(agent.access.is_empty() && agent.limits.is_default() && !agent.notify_on_use);
    }

    #[test]
    fn default_settings_serialize_as_before() {
        let a = WorkspaceAgent::new("Scout", "rt");
        let v = serde_json::to_value(&a).unwrap();
        for k in ["access", "limits", "notifyOnUse"] {
            assert!(v.get(k).is_none(), "{k} must be left out when unset");
        }
        let mut b = a.clone();
        b.access = ACCESS_EVERYONE.into();
        b.limits.member_turns_per_day = Some(5);
        b.notify_on_use = true;
        let v = serde_json::to_value(&b).unwrap();
        assert_eq!(v["access"], "everyone");
        assert_eq!(v["limits"]["memberTurnsPerDay"], 5);
        assert!(v["limits"].get("usdCentsPerDay").is_none());
        assert_eq!(v["notifyOnUse"], true);
        let back: WorkspaceAgent = serde_json::from_value(v).unwrap();
        assert_eq!(back, b);
    }

    #[test]
    fn access_defaults_by_ownership_and_fails_closed() {
        use crate::WorkspaceRole::*;
        let mut a = WorkspaceAgent::new("x", "rt");
        assert_eq!(a.effective_access(), Access::Everyone, "unowned: everyone");
        assert!(a.usable_by("anyone", Some(Viewer)));
        a.owner_actor_id = "me".into();
        assert_eq!(a.effective_access(), Access::Owner, "personal: only me");
        assert!(a.usable_by("me", Some(Contributor)));
        assert!(!a.usable_by("you", Some(Owner)), "an Owner role doesn't open someone's personal agent");
        a.access = "members:you".into();
        assert_eq!(a.effective_access(), Access::Owner, "an unknown value fails closed");
        a.access = ACCESS_EVERYONE.into();
        assert!(a.usable_by("you", Some(Viewer)));
        assert!(!a.usable_by("", Some(Owner)), "no person, no use");
        let mut shared = WorkspaceAgent::new("y", "rt");
        shared.access = ACCESS_OWNER.into();
        assert!(shared.usable_by("adm", Some(Admin)));
        assert!(!shared.usable_by("c", Some(Contributor)));
        assert!(!shared.usable_by("c", None));
    }

    #[test]
    fn member_turn_cap() {
        let mut l = AgentLimits::default();
        assert_eq!(l.member_turn_cap(true), Some(DEFAULT_MEMBER_TURNS_PER_DAY));
        assert_eq!(l.member_turn_cap(false), None, "a shared agent has no default cap");
        l.member_turns_per_day = Some(0);
        assert_eq!(l.member_turn_cap(true), None);
        l.member_turns_per_day = Some(3);
        assert_eq!(l.member_turn_cap(true), Some(3));
        assert_eq!(l.member_turn_cap(false), Some(3));
    }
}

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

/// Whether `s` is a plain CSS hex color: `#` and 3 to 8 hex digits. The
/// roster is synced, so an agent's accent color can come from another
/// member's client; anything else (a `url(...)`, a gradient) would reach the
/// viewer's CSS, so it is dropped on fold and refused on edit.
pub fn is_valid_color_hex(s: &str) -> bool {
    let Some(hex) = s.strip_prefix('#') else { return false };
    (3..=8).contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

// ── The default agent's names ──────────────────────────────────────────────
//
// The default agent (the one that answers `@ligus`, `@primary` and untagged
// messages in a solo workspace) has no roster record: every device synthesizes
// it at dispatch time, so its name lives in code, not in the workspace. Before
// the rename (the Hive builds) the same agent was `@hive` and its replies were
// stored with the author "Hive". Nothing about that is migrated on the wire:
// stored messages keep their signed author, and these helpers only decide how
// they are read and routed.

/// The default agent's display name (its replies' author, avatar initials).
pub const PRIMARY_NAME: &str = "Ligus";
/// The default agent's mentionable handle, written `@ligus`.
pub const PRIMARY_HANDLE: &str = "ligus";
/// The default agent's handle in the Hive builds. Still accepted as a silent
/// alias for the default agent (old messages, scripts, older phones), unless
/// the workspace has a roster agent of that name: see
/// [`legacy_primary_alias_active`].
pub const LEGACY_PRIMARY_HANDLE: &str = "hive";

/// Mention words no new agent may take: they would shadow the default agent
/// (`primary`, `ligus`, the legacy `hive`) or a group (`you`, `all`, ...).
pub const RESERVED_AGENT_NAMES: &[&str] =
    &["primary", PRIMARY_HANDLE, LEGACY_PRIMARY_HANDLE, "you", "all", "everyone", "owners", "admins", "contributors", "viewers"];

/// A name's comparison form for the reserved checks: cleaned (invisible and
/// bidi characters dropped), lowercased, spaces and `.`/`_`/`-` removed and
/// look-alike letters folded (`ligus_proto::names::name_skeleton`), with `i`
/// folded to `l` as well, so `lіgus` (Cyrillic і), `li\u{200B}gus` and
/// `LIGUS` all read as `ligus`.
fn reserved_skeleton(name: &str) -> String {
    ligus_proto::names::name_skeleton(name).replace('i', "l")
}

/// Whether `name` is reserved for a new agent: a mention word or a look-alike
/// of one.
pub fn is_reserved_agent_name(name: &str) -> bool {
    let n = reserved_skeleton(name);
    RESERVED_AGENT_NAMES.iter().any(|r| reserved_skeleton(r) == n)
}

/// Whether `name` means the default agent (`ligus` or `primary`, or a
/// look-alike). No current client creates a roster agent with such a name;
/// one that arrives anyway (an older client, a hand-made event) is never
/// matched as a mention, offered in the mention list or taken as the author
/// of the default agent's replies, so it can't take over the default agent's
/// mentions or its face. It stays on the roster, where an Admin can remove it.
pub fn names_primary(name: &str) -> bool {
    let n = reserved_skeleton(name);
    n == reserved_skeleton(PRIMARY_HANDLE) || n == reserved_skeleton("primary")
}

/// Whether the legacy `@hive` handle means the default agent in `session`:
/// yes, unless a roster agent or a member is named exactly "hive" (any case),
/// so that `@hive` already reaches them. The default agent never had a
/// roster record, so a roster "hive" is always a custom agent (possible in
/// the Ligus builds before the name was reserved again), and it keeps its
/// name and its mentions; a person called Hive keeps theirs without waking
/// the default agent. The test is the same exact match the mention parser
/// uses for names, so the alias is never off without someone answering.
pub fn legacy_primary_alias_active(session: &crate::ChatSession) -> bool {
    let named = |n: &str| n.eq_ignore_ascii_case(LEGACY_PRIMARY_HANDLE);
    !session.workspace_agents.iter().any(|a| named(&a.name))
        && !session.members.iter().any(|m| named(&m.actor.display_name))
}

/// Whether `m` is a reply of the default agent written by a Hive build (author
/// "Hive"/"hive", no roster responder). Replies of named agents have carried
/// their roster id (`responder_id`) since before the rename, so a reply of a
/// custom agent named "hive" is not mistaken for one.
pub fn is_legacy_primary_reply(m: &crate::ChatMessage) -> bool {
    m.role == crate::MessageRole::Assistant
        && m.responder_id.is_none()
        && m.author.trim().eq_ignore_ascii_case(LEGACY_PRIMARY_HANDLE)
}

/// The author to show for `m`: [`PRIMARY_NAME`] for a Hive-era reply of the
/// default agent, else its stored author. Display only: the signed author is
/// unchanged.
pub fn display_author(m: &crate::ChatMessage) -> &str {
    if is_legacy_primary_reply(m) {
        PRIMARY_NAME
    } else {
        &m.author
    }
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
    /// Drops an accent color that isn't a plain hex color (see
    /// [`is_valid_color_hex`]).
    pub fn sanitize_color(&mut self) {
        if self.avatar_color_hex.as_deref().is_some_and(|c| !is_valid_color_hex(c)) {
            self.avatar_color_hex = None;
        }
    }

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
    fn the_default_agents_names() {
        for n in ["ligus", " Ligus ", "PRIMARY", "hive", "Hive", "all", "you"] {
            assert!(is_reserved_agent_name(n), "{n}");
        }
        assert!(!is_reserved_agent_name("scout"));
        // Look-alikes and invisible characters are reserved too.
        for n in ["l\u{0456}gus", "li\u{200B}gus", "LIGUS", "li-gus", "pr\u{0456}mary", "h\u{0456}ve"] {
            assert!(is_reserved_agent_name(n), "{n:?}");
        }
        assert!(names_primary("Ligus") && names_primary("primary") && names_primary("L\u{0456}gus") && !names_primary("hive"));
        assert!(!names_primary("ligusbot") && !names_primary("scout"));
        let mut s = crate::ChatSession::new("t", Uuid::new_v4(), "rt");
        s.workspace_agents = vec![WorkspaceAgent::new("scout", "rt")];
        assert!(legacy_primary_alias_active(&s));
        // " hive" isn't what `@hive` matches, so it doesn't switch the alias off.
        s.workspace_agents.push(WorkspaceAgent::new(" hive", "rt"));
        assert!(legacy_primary_alias_active(&s));
        s.workspace_agents.push(WorkspaceAgent::new("Hive", "rt"));
        assert!(!legacy_primary_alias_active(&s));
        let mut m = crate::ChatMessage::new(crate::MessageRole::Assistant, "Hive", "x");
        assert!(is_legacy_primary_reply(&m) && display_author(&m) == PRIMARY_NAME);
        m.responder_id = Some(Uuid::new_v4());
        assert!(!is_legacy_primary_reply(&m) && display_author(&m) == "Hive");
        let human = crate::ChatMessage::new(crate::MessageRole::User, "hive", "x");
        assert_eq!(display_author(&human), "hive");
    }

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

//! Workspace guard state folded onto a session: coordinator leases, and on the
//! workspace-config log the workspace's guard settings (dearth mode, pending
//! joins). Kept in one field on [`crate::ChatSession`] so the projection grows
//! by a single, defaulted member and older stored sessions deserialize
//! unchanged.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::identity::{ActorKind, WorkspaceMember, WorkspaceRole};
use crate::lease::LeaseTable;
use crate::time_util::Timestamp;

/// Default "brood" paths: changes here need a human approval in dearth mode.
pub const DEFAULT_BROOD_PATHS: &[&str] = &[
    ".github/**",
    ".gitlab-ci.yml",
    ".circleci/**",
    "Jenkinsfile",
    "azure-pipelines.yml",
    ".buildkite/**",
    "deploy/**",
    "infra/**",
];

/// Dearth mode: a workspace-wide "tight entrance" for hostile periods (a
/// leaked invite, an untrusted repo being processed, a new worker host).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DearthMode {
    pub enabled: bool,
    /// Short note shown on the banner ("leaked invite link", ...).
    #[serde(default)]
    pub reason: String,
    /// Glob patterns of paths whose changes need a human approval.
    #[serde(default)]
    pub brood_paths: Vec<String>,
    /// MCP server ids agents may still reach; everything else is closed.
    #[serde(default)]
    pub allowed_mcp_servers: Vec<String>,
    /// Devices already trusted when dearth mode was switched on. Any other
    /// device needs a second approval from an Admin of another account.
    #[serde(default)]
    pub admitted_devices: Vec<Uuid>,
    #[serde(default)]
    pub set_by: String,
    #[serde(default)]
    pub set_at: Timestamp,
}

/// A member an Admin added during dearth mode, waiting for a second Admin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingMember {
    pub member: WorkspaceMember,
    pub added_by: String,
}

/// What made a chat lock itself down (self-medication).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LockdownSignal {
    /// A prompt-injection marker in content an agent read (a tool result).
    InjectionDetected,
    /// A credential in the added lines of an agent's diff.
    SecretInDiff,
    /// An agent changed CI configuration nobody asked about.
    CiConfigChanged,
    /// A member locked it by hand.
    Manual,
}

/// One infection signal that tightened a chat's envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatLockdown {
    pub id: Uuid,
    pub signal: LockdownSignal,
    /// Short, content-free code: a credential kind, a marker name, a count.
    #[serde(default)]
    pub detail: String,
    /// Actor (or host) that raised it.
    #[serde(default)]
    pub raised_by: String,
    #[serde(default)]
    pub raised_at: Timestamp,
    /// The reply whose turn raised it (the message whose tool call read the
    /// marker or whose changes tripped the scan), so the UI can jump to it.
    /// Absent for a manual lockdown, and left out of the JSON when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<Uuid>,
    /// The `ChatLockdownRaised` event that raised it: what a
    /// `ChatLockdownCleared` names to clear it. Set by the fold from the
    /// raise's own event id, never by the raiser (a raise carrying it is
    /// refused). Left out of the JSON when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raise_event_id: Option<Uuid>,
}

/// Guard state carried by a projected session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardState {
    /// Coordinator leases folded on this log, keyed by `lease::lease_key`.
    #[serde(default, skip_serializing_if = "LeaseTable::is_empty")]
    pub leases: LeaseTable,
    /// Dearth mode (config log; overlaid onto chats by `ChatService::load`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dearth: Option<DearthMode>,
    /// Members added during dearth mode, awaiting a second approval.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_members: Vec<PendingMember>,
    /// `JoinApproved` approvals: subject (member id or device id) → approver
    /// actor ids. Only Admin+ approvals are folded.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub join_approvals: BTreeMap<String, BTreeSet<String>>,
    /// Box admissions (Phase B): invite id → the terms its `JoinApproved`
    /// carried (the first one folded; a later one for the same invite is
    /// refused). Config log.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub service_invites: BTreeMap<Uuid, crate::admission::Admission>,
    /// Person invites issued with a redeem key: invite id → the key (the
    /// first one folded). A redemption must carry its signature
    /// ([`crate::admission::person_redemption_message`]). Config log.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub invite_keys: BTreeMap<Uuid, Vec<u8>>,
    /// Closed box admissions: invite id → the redemption each allowed
    /// `AdmissionClosed` named (none: nobody). Config log.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub closed_admissions: BTreeMap<Uuid, Vec<Option<Uuid>>>,
    /// The sandbox grants in force (`WorkspaceSandboxSet`). Config log.
    #[serde(default, skip_serializing_if = "crate::sandbox::SandboxGrants::is_empty")]
    pub sandbox_grants: crate::sandbox::SandboxGrants,
    /// Approvals cast through a vote whose stated voter is the event's signed
    /// author (proposal id → voter ids). Approvals carried inside a proposal
    /// snapshot are not here, so they can't satisfy a human gate.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub verified_approvals: BTreeMap<Uuid, BTreeSet<String>>,
    /// Active chat lockdowns, by chat id (config log; a chat's own entry is
    /// overlaid onto it by `ChatService::load`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lockdowns: BTreeMap<Uuid, Vec<ChatLockdown>>,
    /// Proposals the hygiene sweep found stale (their diff no longer applies).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub stale_proposals: BTreeSet<Uuid>,
    /// Members an authorized `MemberRemoved` took out. A removed member keeps
    /// old-epoch keys, so they could otherwise self-enroll straight back;
    /// only an authorized re-add (an Admin's) lifts the removal.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub removed_members: BTreeSet<String>,
    /// Devices an authorized `DeviceRevoked` cut off (config log). The trust
    /// roster reads this rather than raw events, so a revocation projection
    /// refuses never revokes anyone.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub revoked_devices: BTreeSet<Uuid>,
    /// Invites (see `authorization::invite_subject`) already redeemed by a
    /// self-enrollment: each admits one member.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub used_invites: BTreeSet<Uuid>,
    /// The workspace's auto-archive setting (`ProposalArchivePolicySet`,
    /// config log; overlaid onto chats by `ChatService::load`). Absent = the
    /// default ([`crate::proposal_archive::DEFAULT_AFTER_DAYS`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_archive: Option<crate::proposal_archive::ProposalArchivePolicy>,
    /// Set on a chat session by the overlay: this chat's id, so
    /// [`GuardState::lockdown_active`] can find its entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<Uuid>,
}

impl GuardState {
    /// True when nothing is set, so the field can be left out of a
    /// serialized session.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[cfg(test)]
mod empty_state_tests {
    use super::GuardState;

    #[test]
    fn default_guard_state_is_empty() {
        assert!(GuardState::default().is_empty());
    }
}

impl GuardState {
    /// The active dearth mode, if switched on.
    pub fn dearth_on(&self) -> Option<&DearthMode> {
        self.dearth.as_ref().filter(|d| d.enabled)
    }

    /// Whether this chat is locked down (self-medication). Only meaningful on
    /// a chat session the config overlay has stamped with its `chat_id`.
    pub fn lockdown_active(&self) -> bool {
        self.chat_id.is_some_and(|c| self.lockdowns.get(&c).is_some_and(|l| !l.is_empty()))
    }

    /// The active lockdown signals of a chat.
    pub fn lockdowns_of(&self, chat_id: Uuid) -> &[ChatLockdown] {
        self.lockdowns.get(&chat_id).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// Fold the guard side of an event that needs its **signed author** (which
/// `ChatSession::apply` doesn't see). Called from projection after the event
/// passed authorization. Returns true when the event was fully consumed here
/// (so `apply` must not run).
pub fn fold_authored(session: &mut crate::ChatSession, event: &crate::SessionEvent, author: &str) -> bool {
    use crate::SessionEvent;
    match event {
        // Dearth mode: a new member added by an Admin waits for a second Admin.
        SessionEvent::MemberAdded { member, .. } => {
            if session.guards.dearth_on().is_none()
                || session.members.iter().any(|m| m.id == member.id)
                || session.members.is_empty()
            {
                return false;
            }
            let approved = session
                .guards
                .join_approvals
                .get(&member.id)
                .is_some_and(|a| a.iter().any(|x| x != author));
            if approved {
                session.guards.pending_members.retain(|p| p.member.id != member.id);
                return false;
            }
            session.guards.pending_members.retain(|p| p.member.id != member.id);
            session.guards.pending_members.push(PendingMember { member: member.clone(), added_by: author.to_string() });
            true
        }
        SessionEvent::JoinApproved { subject, admission, redeem_key } => {
            if let (Some(a), Some(id)) = (admission, crate::admission::invite_of_subject(subject)) {
                session.guards.service_invites.entry(id).or_insert_with(|| a.clone());
            }
            if let (None, Some(k), Some(id)) = (admission, redeem_key, crate::admission::invite_of_subject(subject)) {
                session.guards.invite_keys.entry(id).or_insert_with(|| k.clone());
            }
            session.guards.join_approvals.entry(subject.clone()).or_default().insert(author.to_string());
            if let Some(i) = session
                .guards
                .pending_members
                .iter()
                .position(|p| &p.member.id == subject && p.added_by != author)
            {
                let p = session.guards.pending_members.remove(i);
                session.guards.removed_members.remove(&p.member.id);
                if !session.members.iter().any(|m| m.id == p.member.id) {
                    session.members.push(p.member);
                }
            }
            true
        }
        // A vote counts toward a human gate only when the voter it names is
        // the author who signed it.
        SessionEvent::ProposalVoteCast { proposal_id, approval } => {
            if !author.is_empty() && approval.actor_id == author {
                let set = session.guards.verified_approvals.entry(*proposal_id).or_default();
                if approval.approved {
                    set.insert(author.to_string());
                } else {
                    set.remove(author);
                }
            }
            false
        }
        _ => false,
    }
}

/// Whether `proposal_id` has an approval from a human member, cast and signed
/// by that member themself (see [`GuardState::verified_approvals`]).
pub fn has_human_approval(session: &crate::ChatSession, proposal_id: Uuid) -> bool {
    let producing_agent = session.proposals.iter().find(|p| p.id == proposal_id).and_then(|p| p.producing_agent_id);
    session
        .guards
        .verified_approvals
        .get(&proposal_id)
        .is_some_and(|voters| {
            voters.iter().any(|v| {
                is_human_member(&session.members, v) && producing_agent.is_none_or(|a| a.to_string() != *v)
            })
        })
}

/// Why a proposal may not be applied without a human approval, if it can't.
pub fn human_gate_reason(session: &crate::ChatSession, proposal: &crate::ActionProposal) -> Option<String> {
    if has_human_approval(session, proposal.id) {
        return None;
    }
    if session.guards.lockdown_active() {
        return Some("this chat is locked down: every proposal needs a human member's approval".into());
    }
    if let Some(d) = session.guards.dearth_on() {
        if touches_any(&d.brood_paths, &touched_paths(proposal)) {
            return Some("dearth mode: this change touches protected (brood) paths and needs a human member's approval".into());
        }
    }
    None
}

/// Every path a proposal may write: its declared `changed_files` plus every
/// path named in its diff's headers. The declared list is the author's own
/// summary, so a gate must never rely on it alone: a patch that rewrites
/// `.github/workflows/ci.yml` while declaring only `src/lib.rs` would slip by.
/// Over-approximates (both sides of a rename, prefixed and unprefixed forms),
/// which can only make a gate stricter.
pub fn touched_paths(proposal: &crate::ActionProposal) -> Vec<String> {
    let mut out: Vec<String> = proposal.changed_files.clone();
    let mut push = |raw: &str| {
        let p = raw.trim().trim_end_matches('\t').trim_matches('"');
        if p.is_empty() || p == "/dev/null" {
            return;
        }
        for cand in [p, p.strip_prefix("a/").unwrap_or(p), p.strip_prefix("b/").unwrap_or(p)] {
            if !out.iter().any(|x| x == cand) {
                out.push(cand.to_string());
            }
        }
    };
    for line in proposal.diff.as_deref().unwrap_or("").lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            match rest.find(" b/").or_else(|| rest.find(" \"b/")) {
                Some(i) => {
                    push(&rest[..i]);
                    push(&rest[i + 1..]);
                }
                None => push(rest),
            }
        } else if let Some(rest) = line
            .strip_prefix("--- ")
            .or_else(|| line.strip_prefix("+++ "))
            .or_else(|| line.strip_prefix("rename from "))
            .or_else(|| line.strip_prefix("rename to "))
            .or_else(|| line.strip_prefix("copy from "))
            .or_else(|| line.strip_prefix("copy to "))
        {
            push(rest.split('\t').next().unwrap_or(rest));
        }
    }
    out
}

/// Whether a change to `files` in `session` is under a workspace guard's human
/// gate (a chat lockdown, or dearth mode with a file in a brood path). Such a
/// change's review bar may be raised but never lowered (see
/// `flow::zones::apply_review_bar`).
pub fn guards_protect(session: &crate::ChatSession, files: &[String]) -> bool {
    session.guards.lockdown_active() || session.guards.dearth_on().is_some_and(|d| touches_any(&d.brood_paths, files))
}

/// Match a workspace-relative path against a glob: `**` spans directories,
/// `*` stays within one segment, `?` is one character. A pattern without a
/// slash also matches the file name anywhere (like `.gitignore`).
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let path = path.trim_start_matches("./");
    let pattern = pattern.trim().trim_start_matches("./");
    if pattern.is_empty() {
        return false;
    }
    if !pattern.contains('/') {
        let name = path.rsplit('/').next().unwrap_or(path);
        return seg_match(pattern.as_bytes(), name.as_bytes()) || seg_match(pattern.as_bytes(), path.as_bytes());
    }
    let pat: Vec<&str> = pattern.split('/').collect();
    let segs: Vec<&str> = path.split('/').collect();
    match_segments(&pat, &segs)
}

fn match_segments(pat: &[&str], segs: &[&str]) -> bool {
    match pat.first() {
        None => segs.is_empty(),
        Some(&"**") => (0..=segs.len()).any(|i| match_segments(&pat[1..], &segs[i..])),
        Some(p) => !segs.is_empty() && seg_match(p.as_bytes(), segs[0].as_bytes()) && match_segments(&pat[1..], &segs[1..]),
    }
}

fn seg_match(p: &[u8], s: &[u8]) -> bool {
    match (p.first(), s.first()) {
        (None, None) => true,
        (Some(b'*'), _) => seg_match(&p[1..], s) || (!s.is_empty() && s[0] != b'/' && seg_match(p, &s[1..])),
        (Some(b'?'), Some(c)) if *c != b'/' => seg_match(&p[1..], &s[1..]),
        (Some(a), Some(b)) if a == b => seg_match(&p[1..], &s[1..]),
        _ => false,
    }
}

/// Whether any of `paths` falls under one of `patterns`. Case-insensitive:
/// macOS and Windows volumes usually are, so `.GitHub/workflows/x.yml` lands
/// in `.github/` and must be guarded like it (as the runtime's write guard
/// already matches).
pub fn touches_any(patterns: &[String], paths: &[String]) -> bool {
    paths.iter().any(|f| {
        let f = f.to_lowercase();
        patterns.iter().any(|p| glob_match(&p.to_lowercase(), &f))
    })
}

/// Whether `actor_id` is a human member at Contributor or above in `members`.
pub fn is_human_member(members: &[WorkspaceMember], actor_id: &str) -> bool {
    members.iter().any(|m| {
        m.id == actor_id && m.actor.kind == ActorKind::Human && m.role.rank() >= WorkspaceRole::Contributor.rank()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match(".github/**", ".github/workflows/ci.yml"));
        assert!(glob_match(".github/**", ".github/CODEOWNERS"));
        assert!(!glob_match(".github/**", "src/.github.rs"));
        assert!(glob_match("Jenkinsfile", "Jenkinsfile"));
        assert!(glob_match("Jenkinsfile", "services/api/Jenkinsfile"));
        assert!(glob_match("deploy/**", "deploy/helm/values.yaml"));
        assert!(glob_match("src/*.rs", "src/main.rs"));
        assert!(!glob_match("src/*.rs", "src/a/main.rs"));
        assert!(glob_match("**/*.lock", "a/b/Cargo.lock"));
        assert!(glob_match("*.lock", "Cargo.lock"));
        assert!(!glob_match("", "x"));
        let brood: Vec<String> = DEFAULT_BROOD_PATHS.iter().map(|s| s.to_string()).collect();
        assert!(touches_any(&brood, &["README.md".into(), ".github/workflows/release.yml".into()]));
        assert!(!touches_any(&brood, &["src/lib.rs".into()]));
    }
}

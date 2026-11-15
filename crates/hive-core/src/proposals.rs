//! Action proposals + quorum voting — ported from `ActionProposal` /
//! `ProposalApproval` in `HiveModels.swift`. A proposal is something a
//! participant wants the workspace to approve (a file diff, a command, a
//! decision). Approval requires a quorum: at least `required_approvals`
//! up-votes from members whose role meets `approval_role_floor`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::identity::WorkspaceRole;
use crate::time_util::Timestamp;
use crate::workflow::WorkflowDefinition;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProposalKind {
    FileDiff,
    Command,
    Decision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProposalStatus {
    Open,
    Approved,
    Rejected,
    Applied,
}

/// One actor's vote on a proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposalApproval {
    pub actor_id: String,
    pub role: WorkspaceRole,
    /// `true` = approve, `false` = reject.
    pub approved: bool,
    #[serde(default)]
    pub created_at: Timestamp,
    /// Lowercase hex SHA-256 of the content voted on
    /// ([`crate::authz::votes::diff_digest`]). A vote counts only for the
    /// content it names, so swapping the patch under a vote never carries
    /// the vote over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_digest: Option<String>,
}

/// One note on a proposal's refinement thread — a comment or an explicit request
/// for changes, from a human member or a role-qualified agent. The thread is the
/// discussion that precedes a "Send to agent to revise", which bundles these
/// notes into an instruction that produces a linked new version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposalRefinement {
    pub id: Uuid,
    pub actor_id: String,
    pub role: WorkspaceRole,
    pub text: String,
    /// A change request (vs. a plain comment) — flags that the author wants the
    /// proposal revised. The "Send to agent" action forwards these.
    #[serde(default)]
    pub request_changes: bool,
    #[serde(default)]
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionProposal {
    pub id: Uuid,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// Actor who created the proposal. Their own approval does not count toward
    /// quorum (no self-approval). Empty for authorless proposals (e.g. workflow
    /// gates), where the guard excludes no one.
    #[serde(default)]
    pub author_actor_id: String,
    pub kind: ProposalKind,
    pub status: ProposalStatus,
    /// How many qualifying up-votes are needed (0 = no quorum required).
    #[serde(default)]
    pub required_approvals: u32,
    /// Minimum role an approval must carry to count.
    #[serde(default = "default_floor")]
    pub approval_role_floor: WorkspaceRole,
    #[serde(default)]
    pub approvals: Vec<ProposalApproval>,
    #[serde(default)]
    pub created_at: Timestamp,
    /// For a [`ProposalKind::FileDiff`]: the unified diff the agent produced in
    /// its isolated worktree, applied to the workspace root with `git apply` on
    /// Implement. Stored on the proposal (not a branch ref) so it's portable —
    /// every reviewer sees it, and any device whose tree matches the base can
    /// apply it, not just the one that ran the turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// Paths the diff touches — a compact summary without parsing the patch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    /// Hidden from the Review inbox. A settled proposal is a paper trail, not
    /// work, so dismissing it reclaims the pane without losing the record — the
    /// proposal stays in the folded state and can be shown again.
    ///
    /// This is a field on the proposal rather than a local UI preference so a
    /// dismiss replicates: clearing the inbox on one device clears it on the
    /// others, the same way a vote does.
    #[serde(default)]
    pub dismissed: bool,
    /// The refinement discussion thread (comments + change requests).
    #[serde(default)]
    pub refinements: Vec<ProposalRefinement>,
    /// The proposal this one revises, if any — set when an agent produces a new
    /// version in response to "Send to agent to revise". Forms a version chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    /// The newer version that superseded this one (the other end of `parent_id`).
    /// A superseded proposal is history — the UI folds it under its successor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<Uuid>,
    /// The agent whose turn produced this proposal, so "Send to agent to revise"
    /// knows who to re-dispatch. `None` = the chat's primary runtime (@hive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producing_agent_id: Option<Uuid>,
    /// An agent-authored workflow definition up for review. Approving the
    /// proposal saves it to the chat's definition set (replacing the one with
    /// the same id, for a revision). Carried on a `Decision` proposal rather
    /// than a new kind so older peers still decode the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<Box<WorkflowDefinition>>,
    /// The proposal set this proposal is an alternative in, if any (see
    /// [`crate::proposal_sets`]). The set's own list is the source of truth;
    /// the fold re-stamps this after an older peer's upsert drops it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_id: Option<Uuid>,
    /// Short class of the proposed work (`refactor`, `docs`, `triage`…): a
    /// grouping label, never a quality claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_class: Option<String>,
    /// An applied change that was later reverted (`ProposalReverted`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reverted: bool,
    /// The human gate this proposal was filed with (its own bar, including a
    /// path zone's), kept when a proposal set stamps its gate on it so the
    /// set can only raise the bar and re-tuning the set never loses it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub own_gate: Option<OwnGate>,
    /// For a [`ProposalKind::Command`] filed through `ligus_propose_command`:
    /// the exact argv Implement runs (in the chat's worktree, inside the
    /// sandbox; no model reinterprets it). Bound into the vote digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<ProposedCommand>,
}

/// A structured command proposal (Track D4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposedCommand {
    /// Program and arguments; never run through a shell.
    pub argv: Vec<String>,
    /// Worktree-relative directory; `None` = the worktree root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// Bounds on a structured command (checked when it is filed).
pub const MAX_COMMAND_ARGS: usize = 64;
pub const MAX_COMMAND_ARG_BYTES: usize = 4096;

impl ProposedCommand {
    /// Checks the shape: a non-empty program, bounded args, no NULs, and a
    /// relative `cwd` that stays inside the worktree.
    pub fn validate(&self) -> Result<(), String> {
        if self.argv.is_empty() || self.argv[0].trim().is_empty() {
            return Err("argv needs a program".into());
        }
        if self.argv.len() > MAX_COMMAND_ARGS {
            return Err(format!("at most {MAX_COMMAND_ARGS} arguments"));
        }
        if self.argv.iter().any(|a| a.len() > MAX_COMMAND_ARG_BYTES || a.contains('\0')) {
            return Err("an argument is too long or contains NUL".into());
        }
        if let Some(c) = &self.cwd {
            let p = std::path::Path::new(c);
            // `has_root`: on Windows `/etc` isn't absolute (no drive) but
            // joining it onto the worktree still lands outside it.
            if p.is_absolute() || p.has_root() || p.components().any(|x| matches!(x, std::path::Component::ParentDir | std::path::Component::Prefix(_))) {
                return Err("cwd must be relative to the worktree, without ..".into());
            }
        }
        Ok(())
    }
}

/// A proposal's own human gate (see [`ActionProposal::own_gate`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnGate {
    pub required_approvals: u32,
    pub approval_role_floor: WorkspaceRole,
}

fn default_floor() -> WorkspaceRole {
    WorkspaceRole::Viewer
}

impl ActionProposal {
    pub fn new(
        title: impl Into<String>,
        kind: ProposalKind,
        author_actor_id: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            title: title.into(),
            body: String::new(),
            author_actor_id: author_actor_id.into(),
            kind,
            status: ProposalStatus::Open,
            required_approvals: 1,
            approval_role_floor: WorkspaceRole::Viewer,
            approvals: Vec::new(),
            created_at: Timestamp::now(),
            diff: None,
            changed_files: Vec::new(),
            dismissed: false,
            refinements: Vec::new(),
            parent_id: None,
            superseded_by: None,
            producing_agent_id: None,
            workflow: None,
            set_id: None,
            task_class: None,
            command: None,
            reverted: false,
            own_gate: None,
        }
    }

    /// A proposal to add (or, when `definition.id` matches an existing one,
    /// revise) a workflow definition. `body` is the human-readable rendering
    /// older peers and the phone show.
    pub fn workflow(
        title: impl Into<String>,
        author_actor_id: impl Into<String>,
        body: impl Into<String>,
        definition: WorkflowDefinition,
    ) -> Self {
        Self {
            body: body.into(),
            workflow: Some(Box::new(definition)),
            ..Self::new(title, ProposalKind::Decision, author_actor_id)
        }
    }

    /// A file-diff proposal carrying the patch an agent produced in an isolated
    /// worktree. `Implement` applies it to the workspace root.
    pub fn file_diff(
        title: impl Into<String>,
        author_actor_id: impl Into<String>,
        diff: String,
        changed_files: Vec<String>,
    ) -> Self {
        Self {
            diff: Some(diff),
            changed_files,
            ..Self::new(title, ProposalKind::FileDiff, author_actor_id)
        }
    }

    /// Record a vote, replacing any prior vote by the same actor (latest wins).
    pub fn cast_vote(&mut self, vote: ProposalApproval) {
        self.approvals.retain(|a| a.actor_id != vote.actor_id);
        self.approvals.push(vote);
        self.recompute_status();
    }

    /// Set the human gate (approvals needed + role floor) and recompute the
    /// status — a proposal set applies its gate to every alternative. The
    /// strictest bar wins: the proposal's own gate (what it was filed with,
    /// a path zone's bar included) is remembered the first time and the
    /// set's gate can only raise it, so re-tuning the set never drops below it.
    pub fn set_human_gate(&mut self, required_approvals: u32, floor: WorkspaceRole) {
        let own = *self.own_gate.get_or_insert(OwnGate {
            required_approvals: self.required_approvals,
            approval_role_floor: self.approval_role_floor,
        });
        self.required_approvals = required_approvals.max(own.required_approvals);
        self.approval_role_floor =
            if floor.rank() >= own.approval_role_floor.rank() { floor } else { own.approval_role_floor };
        self.recompute_status();
    }

    /// Append a refinement note (comment or change request). Idempotent by id, so
    /// a re-applied event from sync doesn't duplicate the note.
    pub fn add_refinement(&mut self, note: ProposalRefinement) {
        if self.refinements.iter().any(|r| r.id == note.id) {
            return;
        }
        self.refinements.push(note);
    }

    /// Whether `actor_id` authored this proposal (never true for an authorless
    /// proposal, so the self-approval guard excludes no one there).
    fn is_author(&self, actor_id: &str) -> bool {
        !self.author_actor_id.is_empty() && self.author_actor_id == actor_id
    }

    /// Up-votes from members whose role meets the floor. The author's own
    /// approval does not count — a proposal can't self-satisfy its quorum.
    pub fn qualifying_approvals(&self) -> usize {
        let floor = self.approval_role_floor.rank();
        self.approvals
            .iter()
            .filter(|a| a.approved && a.role.rank() >= floor && !self.is_author(&a.actor_id))
            .count()
    }

    /// Whether enough qualifying up-votes exist.
    pub fn is_quorum_met(&self) -> bool {
        self.required_approvals > 0 && self.qualifying_approvals() >= self.required_approvals as usize
    }

    /// Whether the proposal may be carried out now: its folded status is
    /// `Approved` (quorum met and no qualifying rejection — `is_quorum_met`
    /// alone stays true after a reject) and no newer version superseded it.
    /// Never true once `Applied`.
    pub fn ready_to_apply(&self) -> bool {
        self.status == ProposalStatus::Approved && self.superseded_by.is_none()
    }

    /// A qualifying down-vote rejects the proposal outright.
    fn has_qualifying_rejection(&self) -> bool {
        let floor = self.approval_role_floor.rank();
        self.approvals
            .iter()
            .any(|a| !a.approved && a.role.rank() >= floor)
    }

    /// Recompute the status from the votes on record (never leaves `Applied`).
    pub fn rederive_status(&mut self) {
        self.recompute_status();
    }

    fn recompute_status(&mut self) {
        // Don't override a terminal "applied" state.
        if self.status == ProposalStatus::Applied {
            return;
        }
        self.status = if self.has_qualifying_rejection() {
            ProposalStatus::Rejected
        } else if self.is_quorum_met() {
            ProposalStatus::Approved
        } else {
            ProposalStatus::Open
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vote(actor: &str, role: WorkspaceRole, approved: bool) -> ProposalApproval {
        ProposalApproval {
            actor_id: actor.into(),
            role,
            approved,
            created_at: Timestamp::epoch(), diff_digest: None }
    }

    #[test]
    fn quorum_counts_only_qualifying_roles() {
        let mut p = ActionProposal::new("Ship it", ProposalKind::Decision, "");
        p.required_approvals = 2;
        p.approval_role_floor = WorkspaceRole::Contributor;

        p.cast_vote(vote("viewer", WorkspaceRole::Viewer, true)); // below floor
        assert_eq!(p.qualifying_approvals(), 0);
        assert!(!p.is_quorum_met());
        assert_eq!(p.status, ProposalStatus::Open);

        p.cast_vote(vote("c1", WorkspaceRole::Contributor, true));
        p.cast_vote(vote("a1", WorkspaceRole::Admin, true));
        assert_eq!(p.qualifying_approvals(), 2);
        assert!(p.is_quorum_met());
        assert_eq!(p.status, ProposalStatus::Approved);
    }

    #[test]
    fn rejected_proposal_with_quorum_is_not_ready_to_apply() {
        let mut p = ActionProposal::new("X", ProposalKind::FileDiff, "");
        p.cast_vote(vote("admin", WorkspaceRole::Admin, false));
        p.cast_vote(vote("phone", WorkspaceRole::Contributor, true));
        assert!(p.is_quorum_met(), "the up-vote alone meets quorum");
        assert_eq!(p.status, ProposalStatus::Rejected);
        assert!(!p.ready_to_apply());

        let mut ok = ActionProposal::new("Y", ProposalKind::FileDiff, "");
        ok.cast_vote(vote("phone", WorkspaceRole::Contributor, true));
        assert!(ok.ready_to_apply());
        ok.superseded_by = Some(Uuid::new_v4());
        assert!(!ok.ready_to_apply(), "a superseded version is history");
    }

    #[test]
    fn latest_vote_per_actor_wins() {
        let mut p = ActionProposal::new("X", ProposalKind::Command, "");
        p.required_approvals = 1;
        p.cast_vote(vote("c1", WorkspaceRole::Contributor, true));
        assert_eq!(p.status, ProposalStatus::Approved);
        // same actor changes their mind → reject
        p.cast_vote(vote("c1", WorkspaceRole::Contributor, false));
        assert_eq!(p.approvals.len(), 1);
        assert_eq!(p.status, ProposalStatus::Rejected);
    }

    #[test]
    fn qualifying_rejection_blocks() {
        let mut p = ActionProposal::new("X", ProposalKind::FileDiff, "");
        p.required_approvals = 1;
        p.approval_role_floor = WorkspaceRole::Admin;
        p.cast_vote(vote("a1", WorkspaceRole::Admin, true));
        assert_eq!(p.status, ProposalStatus::Approved);
        p.cast_vote(vote("o1", WorkspaceRole::Owner, false));
        assert_eq!(p.status, ProposalStatus::Rejected);
    }

    #[test]
    fn author_self_approval_does_not_reach_quorum() {
        let mut p = ActionProposal::new("Mine", ProposalKind::Decision, "author");
        p.required_approvals = 1;
        // Author approves their own proposal — does not count toward quorum.
        p.cast_vote(vote("author", WorkspaceRole::Owner, true));
        assert_eq!(p.qualifying_approvals(), 0);
        assert!(!p.is_quorum_met());
        assert_eq!(p.status, ProposalStatus::Open);
        // A second, non-author approval satisfies the gate.
        p.cast_vote(vote("other", WorkspaceRole::Contributor, true));
        assert_eq!(p.qualifying_approvals(), 1);
        assert_eq!(p.status, ProposalStatus::Approved);
    }

    #[test]
    fn role_floor_enforced_against_voter_role() {
        let mut p = ActionProposal::new("Floor", ProposalKind::Decision, "author");
        p.required_approvals = 1;
        p.approval_role_floor = WorkspaceRole::Contributor;
        // Viewer up-vote is below the floor → doesn't qualify.
        p.cast_vote(vote("v1", WorkspaceRole::Viewer, true));
        assert_eq!(p.qualifying_approvals(), 0);
        assert_eq!(p.status, ProposalStatus::Open);
        // Contributor up-vote qualifies.
        p.cast_vote(vote("c1", WorkspaceRole::Contributor, true));
        assert_eq!(p.qualifying_approvals(), 1);
        assert_eq!(p.status, ProposalStatus::Approved);
    }

    #[test]
    fn author_downvote_still_vetoes() {
        let mut p = ActionProposal::new("Mine", ProposalKind::Decision, "author");
        p.required_approvals = 1;
        p.cast_vote(vote("other", WorkspaceRole::Contributor, true));
        assert_eq!(p.status, ProposalStatus::Approved);
        // A qualifying down-vote vetoes regardless of who casts it.
        p.cast_vote(vote("veto", WorkspaceRole::Contributor, false));
        assert_eq!(p.status, ProposalStatus::Rejected);
    }

    #[test]
    fn refinements_append_and_dedup_by_id() {
        let mut p = ActionProposal::new("Refine me", ProposalKind::FileDiff, "");
        let note = ProposalRefinement {
            id: Uuid::new_v4(),
            actor_id: "u1".into(),
            role: WorkspaceRole::Contributor,
            text: "rename the helper".into(),
            request_changes: true,
            created_at: Timestamp::epoch(),
        };
        p.add_refinement(note.clone());
        p.add_refinement(note.clone()); // re-applied event (sync) must not duplicate
        assert_eq!(p.refinements.len(), 1);
        p.add_refinement(ProposalRefinement {
            id: Uuid::new_v4(),
            actor_id: "u2".into(),
            role: WorkspaceRole::Admin,
            text: "looks good otherwise".into(),
            request_changes: false,
            created_at: Timestamp::epoch(),
        });
        assert_eq!(p.refinements.len(), 2);
        assert!(p.refinements[0].request_changes);
    }

    #[test]
    fn empty_author_excludes_no_one() {
        // Authorless (e.g. workflow gate) proposal: any qualifying approval counts.
        let mut p = ActionProposal::new("Gate", ProposalKind::Decision, "");
        p.required_approvals = 1;
        p.cast_vote(vote("someone", WorkspaceRole::Contributor, true));
        assert_eq!(p.qualifying_approvals(), 1);
        assert_eq!(p.status, ProposalStatus::Approved);
    }
}

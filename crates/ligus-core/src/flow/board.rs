//! The threshold task board: open, unassigned work — failing tests, proposals
//! nobody has reviewed, failed workflow stages, needs-review markers — that no
//! one was asked to do. Instead of handing it all to the primary agent, each
//! item has a **stimulus** (age, severity, how much of its kind is waiting)
//! and each agent a **threshold** per kind (set by its career and persona,
//! plus a small fixed per-agent offset so otherwise identical agents don't
//! all react at once). The agent whose threshold the stimulus crosses by the
//! most picks the item up.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::career::{AgentFlowProfile, Career};
use super::marks::{MarkKind, WorkspaceMark};
use super::{agent_key, task_class};
use crate::proposals::ProposalStatus;
use crate::session::ChatSession;
use crate::workflow::{NodeRunStatus, WorkflowRunStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardItemKind {
    /// A turn's last test run failed and nothing has passed since.
    FailingTests,
    /// A proposal open with no votes and no review notes.
    UnreviewedProposal,
    /// A workflow run that failed at a stage.
    FailedStage,
    /// A needs-review marker someone left.
    NeedsReview,
}

impl BoardItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BoardItemKind::FailingTests => "failing tests",
            BoardItemKind::UnreviewedProposal => "unreviewed proposal",
            BoardItemKind::FailedStage => "failed stage",
            BoardItemKind::NeedsReview => "needs review",
        }
    }

    fn severity(self) -> f64 {
        match self {
            BoardItemKind::FailingTests => 1.5,
            BoardItemKind::FailedStage => 1.2,
            BoardItemKind::UnreviewedProposal => 1.0,
            BoardItemKind::NeedsReview => 0.8,
        }
    }

    /// Task class the work belongs to (reviews and markers are review work).
    pub fn task_class(self) -> &'static str {
        match self {
            BoardItemKind::FailingTests => task_class::BUGFIX,
            BoardItemKind::FailedStage => task_class::BUGFIX,
            BoardItemKind::UnreviewedProposal | BoardItemKind::NeedsReview => task_class::REVIEW,
        }
    }
}

/// One open item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardItem {
    /// Stable id: `tests:<chat>`, `proposal:<id>`, `stage:<run>:<node>`,
    /// `mark:<id>`.
    pub id: String,
    pub kind: BoardItemKind,
    pub title: String,
    pub session_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<Uuid>,
    /// Files involved, for path zones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    pub since_ms: i64,
    /// Computed: how strongly the item calls for someone.
    #[serde(default)]
    pub stimulus: f64,
    /// Who has claimed it (a claim mark), if anyone.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub claimed_by: String,
}

/// Open items across a workspace's chats plus its marks.
pub fn open_items(
    sessions: &[ChatSession],
    marks: &[&WorkspaceMark],
    now_ms: i64,
) -> Vec<BoardItem> {
    let mut items = Vec::new();
    for s in sessions.iter().filter(|s| !s.archived) {
        // Failing tests: the latest measured test result in the chat failed.
        if let Some(t) = s.flow.turns.iter().rev().find(|t| t.tests_passed.is_some()) {
            if t.tests_passed == Some(false) {
                items.push(BoardItem {
                    id: format!("tests:{}", s.id),
                    kind: BoardItemKind::FailingTests,
                    title: format!("Tests failing in “{}”", s.title),
                    session_id: s.id,
                    proposal_id: None,
                    files: vec![],
                    since_ms: t.at_ms,
                    stimulus: 0.0,
                    claimed_by: String::new(),
                });
            }
        }
        for p in &s.proposals {
            if p.status != ProposalStatus::Open || p.dismissed || p.superseded_by.is_some() {
                continue;
            }
            if !p.approvals.is_empty() || !p.refinements.is_empty() {
                continue;
            }
            // Workflow gates wait for people by design.
            if p.author_actor_id.is_empty() {
                continue;
            }
            items.push(BoardItem {
                id: format!("proposal:{}", p.id),
                kind: BoardItemKind::UnreviewedProposal,
                title: format!("Review “{}”", p.title),
                session_id: s.id,
                proposal_id: Some(p.id),
                files: p.changed_files.clone(),
                since_ms: super::ms(&p.created_at),
                stimulus: 0.0,
                claimed_by: String::new(),
            });
        }
        for run in &s.workflow_runs {
            if run.status != WorkflowRunStatus::Failed {
                continue;
            }
            for n in run
                .nodes
                .iter()
                .filter(|n| n.status == NodeRunStatus::Failed)
            {
                items.push(BoardItem {
                    id: format!("stage:{}:{}", run.id, n.node_id),
                    kind: BoardItemKind::FailedStage,
                    title: format!("Stage “{}” of {} failed", n.node_id, run.definition.name),
                    session_id: s.id,
                    proposal_id: None,
                    files: vec![],
                    since_ms: super::ms(&run.updated_at),
                    stimulus: 0.0,
                    claimed_by: String::new(),
                });
            }
        }
    }
    for m in marks
        .iter()
        .filter(|m| m.kind == MarkKind::NeedsReview && m.active(now_ms))
    {
        items.push(BoardItem {
            id: format!("mark:{}", m.id),
            kind: BoardItemKind::NeedsReview,
            title: format!(
                "Review {}{}",
                m.path,
                if m.note.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", m.note)
                }
            ),
            session_id: m.session_id.unwrap_or_default(),
            proposal_id: None,
            files: if m.path.is_empty() {
                vec![]
            } else {
                vec![m.path.clone()]
            },
            since_ms: m.at_ms,
            stimulus: 0.0,
            claimed_by: String::new(),
        });
    }
    // Stimulus: severity × (1 + hours waiting, saturating) × (1 + share of
    // its kind waiting).
    let counts = |k: BoardItemKind| items.iter().filter(|i| i.kind == k).count() as f64;
    let kinds: Vec<(BoardItemKind, f64)> = items.iter().map(|i| (i.kind, counts(i.kind))).collect();
    for (item, (_, n)) in items.iter_mut().zip(kinds) {
        let hours = ((now_ms - item.since_ms).max(0) as f64) / 3_600_000.0;
        let age = 1.0 + (hours / 4.0).min(3.0);
        let crowd = 1.0 + ((n - 1.0) / 5.0).min(1.0);
        item.stimulus = item.kind.severity() * age * crowd;
    }
    // Claims.
    for item in &mut items {
        if let Some(m) = marks
            .iter()
            .find(|m| m.kind == MarkKind::Claim && m.task_id == item.id && m.active(now_ms))
        {
            item.claimed_by = m.by.clone();
        }
    }
    items.sort_by(|a, b| b.stimulus.total_cmp(&a.stimulus).then(a.id.cmp(&b.id)));
    items
}

/// Deterministic per-agent offset in [-0.25, 0.25): threshold diversity.
pub fn threshold_offset(agent: &str, kind: BoardItemKind) -> f64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in agent.bytes().chain(kind.as_str().bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    ((h % 1000) as f64 / 1000.0 - 0.5) * 0.5
}

/// An agent's threshold for a kind of item. Lower = keener.
pub fn threshold(
    agent: &str,
    career: Career,
    kind: BoardItemKind,
    profile: Option<&AgentFlowProfile>,
    core_path: bool,
) -> f64 {
    let base = match (career, kind) {
        // Reviewers (and agents on probation, whose work *is* review) jump on
        // review work.
        (
            Career::Reviewer | Career::Probation,
            BoardItemKind::UnreviewedProposal | BoardItemKind::NeedsReview,
        ) => 1.0,
        (Career::Proposer, BoardItemKind::UnreviewedProposal | BoardItemKind::NeedsReview) => 2.2,
        // Fixing code is proposer work; probation agents only after a long wait.
        (Career::Proposer, BoardItemKind::FailingTests | BoardItemKind::FailedStage) => 1.6,
        (Career::Reviewer, BoardItemKind::FailingTests | BoardItemKind::FailedStage) => 3.0,
        (Career::Probation, BoardItemKind::FailingTests | BoardItemKind::FailedStage) => 5.0,
    };
    let core = if core_path && career == Career::Probation {
        f64::INFINITY
    } else if core_path {
        0.5
    } else {
        0.0
    };
    base + core + threshold_offset(agent, kind) + profile.map(|p| p.threshold_bias).unwrap_or(0.0)
}

/// An agent that could pick work up.
#[derive(Debug, Clone)]
pub struct Picker<'a> {
    pub agent: String,
    pub name: String,
    pub career: Career,
    pub profile: Option<&'a AgentFlowProfile>,
    /// Busy right now (a turn running, or stopped): skip.
    pub busy: bool,
}

/// Who picks `item` up: the free agent whose threshold the stimulus exceeds by
/// the most, or `None` if nobody's threshold is crossed. The author of a
/// proposal never reviews their own.
pub fn pick<'a>(
    item: &BoardItem,
    pickers: &'a [Picker<'a>],
    author: Option<&str>,
    core_path: bool,
) -> Option<&'a Picker<'a>> {
    pickers
        .iter()
        .filter(|p| !p.busy && !p.profile.is_some_and(|x| x.board_opt_out))
        .filter(|p| author.is_none_or(|a| a != p.agent && a != p.name))
        .map(|p| {
            (
                p,
                item.stimulus - threshold(&p.agent, p.career, item.kind, p.profile, core_path),
            )
        })
        .filter(|(_, margin)| *margin > 0.0)
        .max_by(|a, b| a.1.total_cmp(&b.1).then_with(|| b.0.agent.cmp(&a.0.agent)))
        .map(|(p, _)| p)
}

/// The message that hands an item to its picker (posted in the item's chat,
/// addressed by name, so it runs through the normal dispatch on whichever
/// device or host runs that agent).
pub fn pickup_message(item: &BoardItem, agent_name: &str) -> String {
    let what = match item.kind {
        BoardItemKind::FailingTests => "the tests are failing here. Find the cause and propose a fix.".to_string(),
        BoardItemKind::UnreviewedProposal => format!(
            "{} is waiting for a first review. Read it and leave review notes: what's right, what to change.",
            item.title.trim_start_matches("Review ")
        ),
        BoardItemKind::FailedStage => format!("{}. Look at why and propose a fix or a retry.", item.title),
        BoardItemKind::NeedsReview => format!("someone marked this for review: {}.", item.title.trim_start_matches("Review ")),
    };
    format!("@{agent_name} [Task board · {}] {what}", item.kind.as_str())
}

/// Every agent key a session's roster offers, with the primary.
pub fn roster_keys(session: &ChatSession) -> Vec<String> {
    let mut v = vec![agent_key(None)];
    v.extend(
        session
            .workspace_agents
            .iter()
            .map(|a| agent_key(Some(a.id))),
    );
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proposals::{ActionProposal, ProposalKind};

    const H: i64 = 3_600_000;

    fn picker(agent: &str, career: Career) -> Picker<'static> {
        Picker {
            agent: agent.into(),
            name: agent.into(),
            career,
            profile: None,
            busy: false,
        }
    }

    #[test]
    fn collects_open_work() {
        let mut s = ChatSession::new("chat", Uuid::nil(), "rt");
        s.proposals.push(ActionProposal::new(
            "Unreviewed",
            ProposalKind::Decision,
            "coder",
        ));
        let mut voted = ActionProposal::new("Voted", ProposalKind::Decision, "coder");
        voted.cast_vote(crate::proposals::ProposalApproval {
            actor_id: "u".into(),
            role: crate::identity::WorkspaceRole::Owner,
            approved: false,
            created_at: crate::Timestamp::now(),
            diff_digest: None,
        });
        s.proposals.push(voted);
        s.proposals
            .push(ActionProposal::new("Gate", ProposalKind::Decision, ""));
        let items = open_items(&[s], &[], super::super::now_ms());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, BoardItemKind::UnreviewedProposal);
        assert!(items[0].stimulus > 0.0);
    }

    #[test]
    fn stimulus_grows_with_age() {
        let mut s = ChatSession::new("chat", Uuid::nil(), "rt");
        s.proposals
            .push(ActionProposal::new("a", ProposalKind::Decision, "coder"));
        let now = super::super::ms(&s.proposals[0].created_at);
        let fresh = open_items(std::slice::from_ref(&s), &[], now)[0].stimulus;
        let old = open_items(&[s], &[], now + 8 * H)[0].stimulus;
        assert!(old > fresh * 2.0, "{fresh} {old}");
    }

    #[test]
    fn reviewers_pick_reviews_and_proposers_pick_fixes() {
        let pickers = [
            picker("rev", Career::Reviewer),
            picker("pro", Career::Proposer),
        ];
        let mut review = BoardItem {
            id: "proposal:x".into(),
            kind: BoardItemKind::UnreviewedProposal,
            title: "Review x".into(),
            session_id: Uuid::nil(),
            proposal_id: None,
            files: vec![],
            since_ms: 0,
            stimulus: 1.5,
            claimed_by: String::new(),
        };
        assert_eq!(pick(&review, &pickers, None, false).unwrap().agent, "rev");
        let mut fix = review.clone();
        fix.kind = BoardItemKind::FailingTests;
        fix.stimulus = 2.0;
        assert_eq!(pick(&fix, &pickers, None, false).unwrap().agent, "pro");
        // Below every threshold: nobody yet.
        review.stimulus = 0.5;
        assert!(pick(&review, &pickers, None, false).is_none());
        // The author doesn't review their own proposal.
        review.stimulus = 3.0;
        assert_eq!(
            pick(&review, &pickers, Some("rev"), false).unwrap().agent,
            "pro"
        );
    }

    #[test]
    fn probation_agents_never_take_core_path_fixes() {
        let pickers = [picker("new", Career::Probation)];
        let item = BoardItem {
            id: "tests:x".into(),
            kind: BoardItemKind::FailingTests,
            title: String::new(),
            session_id: Uuid::nil(),
            proposal_id: None,
            files: vec![],
            since_ms: 0,
            stimulus: 100.0,
            claimed_by: String::new(),
        };
        assert!(pick(&item, &pickers, None, false).is_some());
        assert!(pick(&item, &pickers, None, true).is_none());
    }

    #[test]
    fn identical_agents_get_different_thresholds() {
        let a = threshold(
            "agent-a",
            Career::Proposer,
            BoardItemKind::FailingTests,
            None,
            false,
        );
        let b = threshold(
            "agent-b",
            Career::Proposer,
            BoardItemKind::FailingTests,
            None,
            false,
        );
        assert_ne!(a, b);
        for x in [a, b] {
            assert!((1.35..1.85).contains(&x), "{x}");
        }
    }

    #[test]
    fn busy_and_opted_out_agents_are_skipped() {
        let prof = AgentFlowProfile {
            agent_id: Uuid::nil(),
            career: None,
            threshold_bias: 0.0,
            board_opt_out: true,
            at_ms: 0,
        };
        let mut busy = picker("busy", Career::Reviewer);
        busy.busy = true;
        let out = Picker {
            profile: Some(&prof),
            ..picker("out", Career::Reviewer)
        };
        let pickers = [busy, out];
        let item = BoardItem {
            id: "mark:x".into(),
            kind: BoardItemKind::NeedsReview,
            title: String::new(),
            session_id: Uuid::nil(),
            proposal_id: None,
            files: vec![],
            since_ms: 0,
            stimulus: 10.0,
            claimed_by: String::new(),
        };
        assert!(pick(&item, &pickers, None, false).is_none());
    }

    #[test]
    fn pickup_message_addresses_the_agent() {
        let item = BoardItem {
            id: "proposal:x".into(),
            kind: BoardItemKind::UnreviewedProposal,
            title: "Review “Rename”".into(),
            session_id: Uuid::nil(),
            proposal_id: None,
            files: vec![],
            since_ms: 0,
            stimulus: 1.0,
            claimed_by: String::new(),
        };
        let m = pickup_message(&item, "reviewer");
        assert!(m.starts_with("@reviewer [Task board · unreviewed proposal]"));
        assert!(m.contains("“Rename” is waiting"));
    }
}

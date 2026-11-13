//! Backpressure at hand-off points. The capacity signal is the **waiting
//! time where work is handed on** — at the human review gate, at a workflow
//! stage waiting for its host, at an agent hand-off — not how busy either
//! side is. When the gate is the bottleneck the response is two-sided:
//! recruit more capacity downstream (ask more eligible reviewers, offer a
//! queued stage to an idle host; both opt-in) **and** tell upstream agents to
//! stop opening new work until the queue drains (the stop-upstream note).

use serde::{Deserialize, Serialize};

use super::{ms, EdgeWait};
use crate::identity::WorkspaceRole;
use crate::proposals::{ActionProposal, ProposalStatus};
use crate::session::ChatSession;

/// Gate and edge thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TremblePolicy {
    /// Open proposals at the gate above which it is "over its queue limit".
    pub queue_limit: u32,
    /// A wait longer than this counts as a long wait (the tremble share).
    pub long_wait_ms: i64,
}

impl Default for TremblePolicy {
    fn default() -> Self {
        Self { queue_limit: 5, long_wait_ms: 2 * 3_600_000 }
    }
}

/// The human gate's state across a workspace.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GateQueue {
    /// Proposals waiting for a decision.
    pub open: u32,
    pub oldest_wait_ms: i64,
    pub median_wait_ms: i64,
    /// Share of the waiting proposals over the long-wait threshold.
    pub tremble_share: f64,
    /// Proposals opened in the last hour (upstream intake).
    pub opened_last_hour: u32,
    /// Proposals decided in the last hour (gate throughput).
    pub decided_last_hour: u32,
    pub over_limit: bool,
}

fn waiting(p: &ActionProposal) -> bool {
    p.status == ProposalStatus::Open && !p.dismissed && p.superseded_by.is_none()
}

fn median(mut v: Vec<i64>) -> i64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[v.len() / 2]
}

/// Measure the gate.
pub fn gate_queue(sessions: &[ChatSession], policy: &TremblePolicy, now_ms: i64) -> GateQueue {
    let mut waits = Vec::new();
    let mut opened = 0;
    let mut decided = 0;
    for s in sessions.iter().filter(|s| !s.archived) {
        for p in &s.proposals {
            let created = ms(&p.created_at);
            if now_ms - created <= 3_600_000 {
                opened += 1;
            }
            if waiting(p) {
                waits.push((now_ms - created).max(0));
            } else if p.status != ProposalStatus::Open {
                let at = p.approvals.iter().map(|a| ms(&a.created_at)).max().unwrap_or(created);
                if now_ms - at <= 3_600_000 {
                    decided += 1;
                }
            }
        }
    }
    let open = waits.len() as u32;
    let long = waits.iter().filter(|w| **w > policy.long_wait_ms).count();
    GateQueue {
        open,
        oldest_wait_ms: waits.iter().copied().max().unwrap_or(0),
        tremble_share: if waits.is_empty() { 0.0 } else { long as f64 / waits.len() as f64 },
        median_wait_ms: median(waits),
        opened_last_hour: opened,
        decided_last_hour: decided,
        over_limit: open > policy.queue_limit,
    }
}

/// Measured wait per edge kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeStats {
    /// `gate`, `stage` or `handoff`.
    pub edge: String,
    pub samples: u32,
    pub median_wait_ms: i64,
    pub tremble_share: f64,
}

/// Wait statistics per edge kind from recorded waits since `since_ms`.
pub fn edge_stats(waits: &[&EdgeWait], policy: &TremblePolicy, since_ms: i64) -> Vec<EdgeStats> {
    ["gate", "stage", "handoff"]
        .iter()
        .map(|edge| {
            let v: Vec<i64> = waits.iter().filter(|w| w.edge == *edge && w.at_ms >= since_ms).map(|w| w.wait_ms).collect();
            let long = v.iter().filter(|w| **w > policy.long_wait_ms).count();
            EdgeStats {
                edge: edge.to_string(),
                samples: v.len() as u32,
                tremble_share: if v.is_empty() { 0.0 } else { long as f64 / v.len() as f64 },
                median_wait_ms: median(v),
            }
        })
        .collect()
}

fn human(ms: i64) -> String {
    let m = ms / 60_000;
    if m < 60 {
        format!("{m} min")
    } else {
        let h = m / 60;
        format!("{h} h")
    }
}

/// The two-sided bottleneck alert: names where to add capacity and where to
/// slow down. `None` when nothing is backed up.
pub fn bottleneck_alert(gate: &GateQueue, edges: &[EdgeStats], policy: &TremblePolicy) -> Option<String> {
    if gate.over_limit || (gate.open > 0 && gate.tremble_share >= 0.5) {
        return Some(format!(
            "The review gate is the bottleneck: {} proposal(s) waiting, median wait {}, oldest {}. \
             {} more were opened in the last hour and {} decided — add reviewers, and have agents \
             finish or refine open proposals before opening new ones.",
            gate.open,
            human(gate.median_wait_ms),
            human(gate.oldest_wait_ms),
            gate.opened_last_hour,
            gate.decided_last_hour,
        ));
    }
    let worst = edges
        .iter()
        .filter(|e| e.edge != "gate" && e.samples >= 3 && e.median_wait_ms > policy.long_wait_ms / 4)
        .max_by_key(|e| e.median_wait_ms)?;
    Some(match worst.edge.as_str() {
        "stage" => format!(
            "Workflow stages are queuing on their hosts: median wait {} over {} stage(s). Bring another host online or let an idle host take queued stages.",
            human(worst.median_wait_ms),
            worst.samples
        ),
        _ => format!(
            "Agent hand-offs are slow to be picked up: median wait {} over {} hand-off(s).",
            human(worst.median_wait_ms),
            worst.samples
        ),
    })
}

/// The stop-upstream note agents feeding the gate get while it is over its
/// queue limit.
pub fn stop_upstream_note(gate: &GateQueue) -> String {
    format!(
        "The review queue is full ({} proposal(s) waiting, oldest {}). Finish or refine existing \
         proposals before opening new ones; prefer answering in chat over filing new proposals.",
        gate.open,
        human(gate.oldest_wait_ms)
    )
}

/// The stop-upstream A/B arm for a chat: `note` or `control`, split by the
/// chat id so a chat stays in one arm.
pub fn upstream_arm(session_id: uuid::Uuid) -> &'static str {
    if session_id.as_u128() % 2 == 0 {
        "note"
    } else {
        "control"
    }
}

/// Members who could clear the gate but haven't been asked: role at or above
/// the floor of a waiting proposal, not its author, not yet voted on it.
pub fn eligible_approvers(sessions: &[ChatSession]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in sessions.iter().filter(|s| !s.archived) {
        for p in s.proposals.iter().filter(|p| waiting(p)) {
            for m in &s.members {
                let qualifies = m.role.rank() >= p.approval_role_floor.rank() && m.role != WorkspaceRole::Viewer;
                let voted = p.approvals.iter().any(|a| a.actor_id == m.id);
                if qualifies && !voted && m.id != p.author_actor_id && !out.contains(&m.id) {
                    out.push(m.id.clone());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proposals::{ActionProposal, ProposalKind};
    use uuid::Uuid;

    const H: i64 = 3_600_000;

    fn chat_with(n: usize) -> ChatSession {
        let mut s = ChatSession::new("c", Uuid::nil(), "rt");
        for i in 0..n {
            s.proposals.push(ActionProposal::new(format!("p{i}"), ProposalKind::Decision, "coder"));
        }
        s
    }

    #[test]
    fn gate_over_limit_and_tremble_share() {
        let s = chat_with(7);
        let now = ms(&s.proposals[0].created_at) + 3 * H;
        let g = gate_queue(&[s], &TremblePolicy::default(), now);
        assert_eq!(g.open, 7);
        assert!(g.over_limit);
        assert!(g.tremble_share > 0.99);
        let alert = bottleneck_alert(&g, &[], &TremblePolicy::default()).unwrap();
        assert!(alert.contains("review gate is the bottleneck"));
        assert!(stop_upstream_note(&g).contains("Finish or refine existing proposals"));
    }

    #[test]
    fn calm_gate_raises_nothing() {
        let s = chat_with(2);
        let now = ms(&s.proposals[0].created_at) + 60_000;
        let g = gate_queue(&[s], &TremblePolicy::default(), now);
        assert!(!g.over_limit);
        assert_eq!(g.opened_last_hour, 2);
        assert!(bottleneck_alert(&g, &[], &TremblePolicy::default()).is_none());
    }

    #[test]
    fn stage_edges_name_the_stage_bottleneck() {
        let waits: Vec<EdgeWait> = (0..4)
            .map(|i| EdgeWait { edge: "stage".into(), reference: format!("r:{i}"), wait_ms: H, host_id: "box".into(), at_ms: 10 })
            .collect();
        let refs: Vec<&EdgeWait> = waits.iter().collect();
        let stats = edge_stats(&refs, &TremblePolicy::default(), 0);
        assert_eq!(stats.iter().find(|e| e.edge == "stage").unwrap().samples, 4);
        let g = GateQueue::default();
        assert!(bottleneck_alert(&g, &stats, &TremblePolicy::default()).unwrap().contains("Workflow stages are queuing"));
    }

    #[test]
    fn approvers_exclude_authors_voters_and_viewers() {
        use crate::identity::{ActorIdentity, ActorKind, WorkspaceMember};
        let mut s = chat_with(1);
        let member = |id: &str, role| WorkspaceMember {
            id: id.into(),
            actor: ActorIdentity::new(id, id, ActorKind::Human),
            role,
            title: String::new(),
            index: 0,
            joined_at: crate::Timestamp::now(),
        };
        s.members = vec![member("coder", WorkspaceRole::Admin), member("ana", WorkspaceRole::Admin), member("v", WorkspaceRole::Viewer)];
        assert_eq!(eligible_approvers(&[s]), vec!["ana".to_string()]);
    }

    #[test]
    fn arm_split_is_stable() {
        let id = Uuid::new_v4();
        assert_eq!(upstream_arm(id), upstream_arm(id));
    }
}

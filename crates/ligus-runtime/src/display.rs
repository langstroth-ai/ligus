//! Peer-chosen names and titles as every surface shows them (the desktop,
//! the `serve` API that the phone and `lt` read, and CLI output).
//!
//! Memory titles, workflow and stage names, agent names, repository names
//! and host labels are written by other members and synced as signed: the
//! fold keeps the bytes. Before one is shown it goes through
//! [`ligus_proto::names::clean_display_name`] (bidi overrides and isolates,
//! zero-width and other invisible characters, controls and line separators
//! removed, whitespace collapsed), so `Notes\u{202E}txt.exe` can't reorder a
//! list row or a "Remove …?" confirm. Bodies and prose are left as written:
//! they are rendered as text, not as one-line labels.

use ligus_core::memory::{MemoryAuthor, MemoryEntry, MemoryOp, MemoryRevision, ProposalState, TaskSlot};
use ligus_core::workflow::{WorkflowDefinition, WorkflowRun};

pub use ligus_proto::names::clean_display_name as shown;

/// The longest workspace name shown (and written by
/// `ChatService::publish_workspace_name`).
pub const MAX_WORKSPACE_NAME_CHARS: usize = 120;

/// A workspace's name (its config log's title) as the phone and the `serve`
/// API show it: cleaned like any peer-chosen label and at most
/// [`MAX_WORKSPACE_NAME_CHARS`] characters, whoever wrote it.
pub fn workspace_name(title: &str) -> String {
    shown(title).chars().take(MAX_WORKSPACE_NAME_CHARS).collect::<String>().trim().to_string()
}

fn author(a: &mut MemoryAuthor) {
    a.name = shown(&a.name);
}

fn revision(r: &mut MemoryRevision) {
    r.title = shown(&r.title);
    author(&mut r.author);
}

/// A memory entry with its titles and author names cleaned (head,
/// conflicts, tombstone, edits after removal).
pub fn memory_entry(e: &MemoryEntry) -> MemoryEntry {
    let mut e = e.clone();
    revision(&mut e.current);
    author(&mut e.created_by);
    e.conflicts.iter_mut().for_each(revision);
    if let Some(r) = e.removed.as_mut() {
        revision(r);
    }
    e.edited_after_removal.iter_mut().for_each(revision);
    e
}

/// A memory proposal with the title it proposes and the names of who
/// proposed and decided it cleaned.
pub fn memory_proposal(p: &ProposalState) -> ProposalState {
    let mut p = p.clone();
    if let MemoryOp::Create { title, .. } | MemoryOp::Edit { title, .. } = &mut p.proposal.op {
        *title = shown(title);
    }
    author(&mut p.by);
    if let Some(d) = p.decision.as_mut() {
        author(&mut d.by);
    }
    p
}

/// A task-state slot with its writer's name cleaned.
pub fn task_slot(t: &TaskSlot) -> TaskSlot {
    let mut t = t.clone();
    author(&mut t.by);
    t
}

/// A workflow definition with its name and stage names cleaned.
pub fn workflow_definition(d: &WorkflowDefinition) -> WorkflowDefinition {
    let mut d = d.clone();
    d.name = shown(&d.name);
    for n in &mut d.nodes {
        n.name = shown(&n.name);
    }
    d
}

/// A workflow run whose definition snapshot is cleaned as above.
pub fn workflow_run(r: &WorkflowRun) -> WorkflowRun {
    let mut r = r.clone();
    r.definition = workflow_definition(&r.definition);
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use ligus_core::memory::{DecisionState, MemoryProposal, MemorySource};

    const EVIL: &str = "Notes\u{202E}txt.exe\u{200B}\u{2066}\u{2028}x\u{0007}";

    fn hidden(s: &str) -> bool {
        s.chars().any(ligus_proto::names::is_hidden_name_char) || s.contains('\u{2028}')
    }

    fn rev(title: &str) -> MemoryRevision {
        MemoryRevision { title: title.into(), body: EVIL.into(), author: MemoryAuthor { name: EVIL.into(), ..Default::default() }, ..Default::default() }
    }

    #[test]
    fn a_workspace_name_is_cleaned_and_bounded() {
        assert_eq!(workspace_name("Comb\u{202E} Council"), "Comb Council");
        assert_eq!(workspace_name(&"x".repeat(5000)).chars().count(), MAX_WORKSPACE_NAME_CHARS);
    }

    #[test]
    fn memory_titles_and_authors_are_cleaned_bodies_kept() {
        let e = MemoryEntry {
            current: rev(EVIL),
            created_by: MemoryAuthor { name: EVIL.into(), ..Default::default() },
            conflicts: vec![rev(EVIL)],
            removed: Some(rev(EVIL)),
            edited_after_removal: vec![rev(EVIL)],
            ..Default::default()
        };
        let c = memory_entry(&e);
        assert_eq!(c.current.title, "Notestxt.exe x");
        for r in [&c.current, &c.conflicts[0], c.removed.as_ref().unwrap(), &c.edited_after_removal[0]] {
            assert!(!hidden(&r.title) && !hidden(&r.author.name), "{r:?}");
            assert_eq!(r.body, EVIL, "a body is prose, rendered as text");
        }
        assert!(!hidden(&c.created_by.name));
    }

    #[test]
    fn memory_proposals_are_cleaned() {
        let p = ProposalState {
            proposal: MemoryProposal {
                proposal_id: Default::default(),
                op: MemoryOp::Create { entry_id: Default::default(), title: EVIL.into(), body: EVIL.into(), tags: vec![] },
                proposer: String::new(),
                source: MemorySource::default(),
                rationale: String::new(),
            },
            by: MemoryAuthor { name: EVIL.into(), ..Default::default() },
            signer: None,
            at_ms: 0,
            decision: Some(DecisionState { accepted: true, by: MemoryAuthor { name: EVIL.into(), ..Default::default() }, at_ms: 0, note: String::new(), rev: Default::default(), conflicted: false }),
        };
        let c = memory_proposal(&p);
        let MemoryOp::Create { title, body, .. } = &c.proposal.op else { unreachable!() };
        assert!(!hidden(title) && body == EVIL);
        assert!(!hidden(&c.by.name) && !hidden(&c.decision.unwrap().by.name));
    }

    #[test]
    fn workflow_names_are_cleaned() {
        let node = ligus_core::workflow::WorkflowNode {
            id: "a".into(),
            name: EVIL.into(),
            depends_on: vec![],
            kind: ligus_core::workflow::WorkflowNodeKind::Agent { agent_id: None, prompt_template: EVIL.into() },
            x: None,
            y: None,
        };
        let d = WorkflowDefinition {
            id: Default::default(),
            name: EVIL.into(),
            description: String::new(),
            input_label: None,
            nodes: vec![node],
            created_at: Default::default(),
            disabled: false,
        };
        let c = workflow_definition(&d);
        assert!(!hidden(&c.name), "{:?}", c.name);
        assert!(c.nodes.iter().all(|n| !hidden(&n.name)));
    }
}

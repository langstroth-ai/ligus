//! Workspace memory and live task state on [`ChatService`]
//! (`ligus_core::memory`; design `docs/design/v2/workspace-memory.md`).
//!
//! Memory rides the workspace config log; a person writes, edits, removes
//! and decides; agents (and the chat's runtime, and phones) only propose.
//! Task state rides the chat log, one single-writer slot per task and agent.
//! Every write here goes through `append_signed`, so it is judged exactly as
//! every device's fold will judge it.

use ligus_core::flow::{self, FlowRecord};
use ligus_core::memory::{
    MemoryDecision, MemoryOp, MemoryProposal, MemoryRemoval, MemorySource, MemoryState, MemoryWrite, TaskSlot, TaskStateUpdate,
};
use ligus_core::{workspace_config_session_id, SessionEvent};
use uuid::Uuid;

use super::{ChatError, ChatService, Result};

/// A person's edit or new entry, as the app, CLI or a script sends it.
#[derive(Debug, Clone, Default)]
pub struct MemoryDraft {
    /// `None` creates a new entry (a fresh id).
    pub entry_id: Option<Uuid>,
    /// The revision being edited (required with `entry_id`).
    pub base: Option<Uuid>,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub source: MemorySource,
    pub resolves: Vec<Uuid>,
}

impl ChatService {
    /// The workspace memory, folded from the config log (empty before it
    /// syncs).
    pub fn memory(&self, workspace_id: Uuid) -> Result<MemoryState> {
        Ok(self.load_in(workspace_id, workspace_config_session_id(workspace_id))?.map(|s| s.flow.memory).unwrap_or_default())
    }

    /// The task-state slots of a chat.
    pub fn task_state(&self, chat_id: Uuid) -> Result<Vec<TaskSlot>> {
        Ok(self.load(chat_id)?.map(|s| s.flow.tasks).unwrap_or_default())
    }

    fn refuse_effects(&self) -> Result<()> {
        if self.acting_agent.is_some() || self.runtime_effect {
            return Err(ChatError::Unauthorized("agents propose memory changes; a person writes them".into()));
        }
        Ok(())
    }

    /// Record the source chat's egress on `source` (design R10): the
    /// stricter of what the caller gave and that chat's sandbox egress now.
    /// No source chat (or one this device doesn't hold) records nothing: the
    /// workspace default, which every turn in the workspace satisfies.
    pub fn stamp_memory_egress(&self, workspace_id: Uuid, source: &mut MemorySource) {
        let chat = source.chat_id.and_then(|c| self.load_in(workspace_id, c).ok().flatten()).filter(|c| c.workspace_id == workspace_id);
        let now = chat.map(|c| ligus_core::sandbox::envelope_for_session(&c).egress);
        source.egress = ligus_core::memory::tighten(source.egress.as_ref(), now.as_ref());
    }

    fn append_config_record(&mut self, workspace_id: Uuid, record: FlowRecord) -> Result<Uuid> {
        let target = self.ensure_workspace_config(workspace_id)?;
        let env = self.append_signed(target, workspace_id, SessionEvent::FlowRecorded { record })?;
        Ok(env.event_id)
    }

    /// Write a new entry or edit one, as this device's person. Returns the
    /// entry id and the new revision id.
    pub fn write_memory(&mut self, workspace_id: Uuid, draft: MemoryDraft) -> Result<(Uuid, Uuid)> {
        self.refuse_effects()?;
        if draft.entry_id.is_some() != draft.base.is_some() {
            return Err(ChatError::Unauthorized("an edit names the entry and the version it was made from".into()));
        }
        let entry_id = draft.entry_id.unwrap_or_else(Uuid::new_v4);
        let mut source = draft.source;
        self.stamp_memory_egress(workspace_id, &mut source);
        let w = MemoryWrite {
            entry_id,
            base: draft.base,
            title: draft.title.trim().to_string(),
            body: draft.body,
            tags: ligus_core::memory::normalize_tags(&draft.tags),
            source,
            resolves: draft.resolves,
        };
        let rev = self.append_config_record(workspace_id, FlowRecord::MemoryWritten(w))?;
        Ok((entry_id, rev))
    }

    /// Remove an entry (a tombstone; the log keeps its history).
    pub fn remove_memory(&mut self, workspace_id: Uuid, entry_id: Uuid, base: Uuid, resolves: Vec<Uuid>) -> Result<Uuid> {
        self.refuse_effects()?;
        self.append_config_record(
            workspace_id,
            FlowRecord::MemoryRemoved(MemoryRemoval { entry_id, base, source: MemorySource::default(), resolves }),
        )
    }

    /// Accept or reject a memory proposal, as this device's person.
    pub fn decide_memory_proposal(&mut self, workspace_id: Uuid, proposal_id: Uuid, accepted: bool, note: &str, resolves: Vec<Uuid>) -> Result<Uuid> {
        self.refuse_effects()?;
        self.append_config_record(
            workspace_id,
            FlowRecord::MemoryProposalDecided(MemoryDecision { proposal_id, accepted, note: note.trim().to_string(), resolves }),
        )
    }

    /// File a memory proposal as this device's person (or service member).
    pub fn propose_memory(&mut self, workspace_id: Uuid, op: MemoryOp, rationale: &str, mut source: MemorySource) -> Result<Uuid> {
        self.refuse_effects()?;
        let proposal_id = Uuid::new_v4();
        self.stamp_memory_egress(workspace_id, &mut source);
        let p = MemoryProposal { proposal_id, op: normalized(op, proposal_id), proposer: String::new(), source, rationale: rationale.trim().to_string() };
        self.append_config_record(workspace_id, FlowRecord::MemoryProposed(p))?;
        Ok(proposal_id)
    }

    /// File a memory proposal as an agent's effect (`None` = the chat's
    /// runtime). Signed with the agent's own key when this host holds one;
    /// otherwise by the host stamped as a runtime effect, so no device ever
    /// reads an agent's proposal as its host person's (design T3).
    pub fn propose_memory_as_agent(
        &mut self,
        workspace_id: Uuid,
        agent: Option<Uuid>,
        op: MemoryOp,
        rationale: &str,
        mut source: MemorySource,
    ) -> Result<Uuid> {
        let proposal_id = Uuid::new_v4();
        self.stamp_memory_egress(workspace_id, &mut source);
        let p = MemoryProposal { proposal_id, op: normalized(op, proposal_id), proposer: flow::agent_key(agent), source, rationale: rationale.trim().to_string() };
        let record = FlowRecord::MemoryProposed(p);
        let target = self.ensure_workspace_config(workspace_id)?;
        if let Some(agent_id) = agent {
            let event = SessionEvent::FlowRecorded { record: record.clone() };
            let fold = self.load_in(workspace_id, target)?.unwrap_or_else(|| ligus_core::ChatSession::new("", workspace_id, ""));
            if let Err(why) = ligus_core::authz::agent::tier_allows(&event, agent_id, self.device_id, &fold) {
                return Err(ChatError::Unauthorized(format!("agent tier: {why}")));
            }
            if self.append_signed_as_agent(target, workspace_id, agent_id, event)?.is_some() {
                return Ok(proposal_id);
            }
        }
        let prev_agent = self.acting_agent.take();
        let prev_rt = std::mem::replace(&mut self.runtime_effect, true);
        let r = self.append_signed(target, workspace_id, SessionEvent::FlowRecorded { record });
        self.acting_agent = prev_agent;
        self.runtime_effect = prev_rt;
        r.map(|_| proposal_id)
    }

    /// Write `agent`'s own task-state slot on `chat_id` (`None` = the chat's
    /// runtime, slot `primary`).
    pub fn update_task_state(&mut self, chat_id: Uuid, workspace_id: Uuid, agent: Option<Uuid>, mut update: TaskStateUpdate) -> Result<()> {
        update.agent_key = flow::agent_key(agent);
        self.as_agent(agent, |svc| {
            svc.append_signed(chat_id, workspace_id, SessionEvent::FlowRecorded { record: FlowRecord::TaskState(update) })
        })?;
        Ok(())
    }
}

/// Trimmed title, normalised tags, and a create's entry named by its
/// proposal (`ligus_core::memory`: a proposed new entry's id is the
/// proposal's id, so it can't be taken first).
pub(crate) fn normalized(op: MemoryOp, proposal_id: Uuid) -> MemoryOp {
    match op {
        MemoryOp::Create { title, body, tags, .. } => {
            MemoryOp::Create { entry_id: proposal_id, title: title.trim().to_string(), body, tags: ligus_core::memory::normalize_tags(&tags) }
        }
        MemoryOp::Edit { entry_id, base, title, body, tags } => {
            MemoryOp::Edit { entry_id, base, title: title.trim().to_string(), body, tags: ligus_core::memory::normalize_tags(&tags) }
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_store::EventStore;
    use ligus_core::memory::TaskStatus;

    fn service() -> (tempfile::TempDir, ChatService, Uuid, Uuid) {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(dir.path().join("ligus.db")).unwrap();
        let mut svc = ChatService::new(
            store,
            ligus_core::SigningKeypair::generate().unwrap(),
            ligus_core::SigningKeypair::generate().unwrap(),
            ligus_core::ActorIdentity::new("ana", "Ana", ligus_core::ActorKind::Human),
        );
        svc.set_author_account(Uuid::new_v4().to_string(), "Ana", None);
        let chat = svc.create_chat("work", Uuid::new_v4(), "anthropic").unwrap();
        (dir, svc, chat.workspace_id, chat.id)
    }

    #[test]
    fn a_person_writes_edits_and_removes_and_conflicts_show() {
        let (_d, mut svc, ws, _) = service();
        let (id, r0) = svc.write_memory(ws, MemoryDraft { title: "Relay".into(), body: "8443".into(), tags: vec!["Infra".into()], ..Default::default() }).unwrap();
        let m = svc.memory(ws).unwrap();
        assert_eq!(m.entries[&id].current.tags, vec!["infra".to_string()]);
        let (_, r1) = svc.write_memory(ws, MemoryDraft { entry_id: Some(id), base: Some(r0), title: "Relay".into(), body: "443".into(), ..Default::default() }).unwrap();
        // A stale edit is kept as a conflict.
        svc.write_memory(ws, MemoryDraft { entry_id: Some(id), base: Some(r0), title: "Relay".into(), body: "8080".into(), ..Default::default() }).unwrap();
        let e = &svc.memory(ws).unwrap().entries[&id];
        assert_eq!((e.head(), e.conflicts.len()), (r1, 1));
        let c = e.conflicts[0].rev;
        let (_, r2) = svc.write_memory(ws, MemoryDraft { entry_id: Some(id), base: Some(r1), title: "Relay".into(), body: "443 or 8080".into(), resolves: vec![c], ..Default::default() }).unwrap();
        svc.remove_memory(ws, id, r2, vec![]).unwrap();
        assert!(!svc.memory(ws).unwrap().entries[&id].live());
        // An edit without a base is refused before signing.
        assert!(svc.write_memory(ws, MemoryDraft { entry_id: Some(id), title: "x".into(), body: "y".into(), ..Default::default() }).is_err());
    }

    #[test]
    fn an_agent_effect_only_proposes_and_a_person_decides() {
        let (_d, mut svc, ws, chat) = service();
        // As the chat's runtime: a direct write is refused, a proposal files.
        let direct = svc.as_agent(None, |svc| svc.write_memory(ws, MemoryDraft { title: "t".into(), body: "b".into(), ..Default::default() }));
        assert!(direct.is_err());
        let op = MemoryOp::Create { entry_id: Uuid::new_v4(), title: "Ports".into(), body: "8443".into(), tags: vec![] };
        let pid = svc
            .propose_memory_as_agent(ws, None, op, "seen", MemorySource { chat_id: Some(chat), ..Default::default() })
            .unwrap();
        let m = svc.memory(ws).unwrap();
        assert!(m.entries.is_empty());
        assert_eq!(m.proposals[&pid].proposal.proposer, "primary");
        svc.decide_memory_proposal(ws, pid, true, "ok", vec![]).unwrap();
        let m = svc.memory(ws).unwrap();
        assert_eq!(m.live_count(), 1);
        assert_eq!(m.live().next().unwrap().current.via_proposal, Some(pid));
    }

    #[test]
    fn task_state_slots_update_in_place() {
        let (_d, mut svc, ws, chat) = service();
        let mut u = TaskStateUpdate { task_id: chat, turn_id: Uuid::new_v4(), summary: "reading".into(), ..Default::default() };
        svc.update_task_state(chat, ws, None, u.clone()).unwrap();
        u.status = TaskStatus::Done;
        u.summary = "done".into();
        svc.update_task_state(chat, ws, None, u).unwrap();
        let slots = svc.task_state(chat).unwrap();
        assert_eq!(slots.len(), 1);
        assert_eq!((slots[0].update.agent_key.as_str(), slots[0].update.status, slots[0].updates), ("primary", TaskStatus::Done, 2));
    }
}

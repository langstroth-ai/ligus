//! Workspace memory and live task state on the desktop
//! (`hive_core::memory`; design `docs/design/v2/workspace-memory.md`): the
//! Memory pane's list, search, edit, remove, conflict resolution and proposal
//! inbox, and the chat's task-state panel. Every write is this device's
//! person's, judged by the fold's own rules in `ChatService`.

use std::collections::BTreeMap;

use hive_core::memory::{self, MemoryEntry, MemoryOp, MemorySource, ProposalState, TaskSlot};
use hive_runtime::chat_service::MemoryDraft;
use hive_runtime::ChatService;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

use crate::AppState;

fn id(s: &str) -> Result<Uuid, String> {
    Uuid::parse_str(s.trim()).map_err(|_| format!("not an id: {s:?}"))
}

fn ids(v: &[String]) -> Result<Vec<Uuid>, String> {
    v.iter().map(|s| id(s)).collect()
}

/// What the Memory pane shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MemoryView {
    /// Live entries matching the search, best first.
    pub entries: Vec<MemoryEntry>,
    /// Every live entry (the count before the search).
    pub total: usize,
    /// Proposals, open first, newest first within each group.
    pub proposals: Vec<ProposalState>,
    /// Titles of the chats entries and proposals came from.
    pub chat_titles: BTreeMap<String, String>,
    /// This device's person (their entries are theirs to change).
    pub me: String,
    /// Whether this device's person is an Admin or Owner (moderation).
    pub moderator: bool,
}

pub(crate) fn view_in(svc: &ChatService, wid: Uuid, query: &str, tags: &[String]) -> Result<MemoryView, String> {
    let m = svc.memory(wid).map_err(|e| e.to_string())?;
    let entries: Vec<MemoryEntry> = memory::search(&m, query, tags, memory::MAX_LIVE_ENTRIES).into_iter().map(|h| h.entry.clone()).collect();
    let mut proposals: Vec<ProposalState> = m.proposals.values().cloned().collect();
    proposals.sort_by(|a, b| a.decision.is_some().cmp(&b.decision.is_some()).then(b.at_ms.cmp(&a.at_ms)));
    let mut chat_titles = BTreeMap::new();
    let chats = entries.iter().filter_map(|e| e.current.source.chat_id).chain(proposals.iter().filter_map(|p| p.proposal.source.chat_id));
    for c in chats {
        if let Ok(Some(s)) = svc.load(c) {
            chat_titles.insert(c.to_string(), s.title);
        }
    }
    let me = svc.author().id.clone();
    let config = svc.load(hive_core::workspace_config_session_id(wid)).ok().flatten();
    let moderator = config.is_some_and(|c| c.role_of(&me).rank() >= hive_core::WorkspaceRole::Admin.rank());
    Ok(MemoryView { total: m.live_count(), entries, proposals, chat_titles, me, moderator })
}

/// The Memory pane: entries matching `query` and `tags`, and the proposals.
#[tauri::command]
pub(crate) fn memory_view(state: tauri::State<'_, AppState>, query: Option<String>, tags: Option<Vec<String>>) -> Result<MemoryView, String> {
    let wid = state.active_workspace_id();
    let svc = state.service.lock().unwrap();
    view_in(&svc, wid, query.as_deref().unwrap_or(""), &tags.unwrap_or_default())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Written {
    pub entry_id: String,
    pub rev: String,
}

/// Write a new entry (no `entryId`) or edit one from `base`, optionally
/// resolving conflicts. `sourceChatId` records the chat it was written from.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) fn memory_write(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    entry_id: Option<String>,
    base: Option<String>,
    title: String,
    body: String,
    tags: Vec<String>,
    resolves: Option<Vec<String>>,
    source_chat_id: Option<String>,
) -> Result<Written, String> {
    let wid = state.active_workspace_id();
    let draft = MemoryDraft {
        entry_id: entry_id.as_deref().map(id).transpose()?,
        base: base.as_deref().map(id).transpose()?,
        title,
        body,
        tags,
        source: MemorySource { chat_id: source_chat_id.as_deref().map(id).transpose()?, ..Default::default() },
        resolves: ids(&resolves.unwrap_or_default())?,
    };
    let (entry, rev) = state.service.lock().unwrap().write_memory(wid, draft).map_err(|e| e.to_string())?;
    let _ = app.emit("workspace://synced", 1);
    Ok(Written { entry_id: entry.to_string(), rev: rev.to_string() })
}

/// Remove an entry from `base` (a tombstone; the history stays in the log).
#[tauri::command]
pub(crate) fn memory_remove(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    entry_id: String,
    base: String,
    resolves: Option<Vec<String>>,
) -> Result<(), String> {
    let wid = state.active_workspace_id();
    state
        .service
        .lock()
        .unwrap()
        .remove_memory(wid, id(&entry_id)?, id(&base)?, ids(&resolves.unwrap_or_default())?)
        .map_err(|e| e.to_string())?;
    let _ = app.emit("workspace://synced", 1);
    Ok(())
}

/// Accept or reject a memory proposal.
#[tauri::command]
pub(crate) fn memory_decide(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    proposal_id: String,
    accepted: bool,
    note: Option<String>,
) -> Result<(), String> {
    let wid = state.active_workspace_id();
    state
        .service
        .lock()
        .unwrap()
        .decide_memory_proposal(wid, id(&proposal_id)?, accepted, note.as_deref().unwrap_or(""), vec![])
        .map_err(|e| e.to_string())?;
    let _ = app.emit("workspace://synced", 1);
    Ok(())
}

/// Propose a change as this device's person (for a member who can't write:
/// a Contributor proposing an edit of someone else's entry).
#[tauri::command]
pub(crate) fn memory_propose(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    op: String,
    entry_id: Option<String>,
    base: Option<String>,
    title: Option<String>,
    body: Option<String>,
    tags: Option<Vec<String>>,
    rationale: Option<String>,
) -> Result<String, String> {
    let wid = state.active_workspace_id();
    let (title, body, tags) = (title.unwrap_or_default(), body.unwrap_or_default(), tags.unwrap_or_default());
    let need = |o: &Option<String>| o.as_deref().ok_or_else(|| "an edit or removal names the entry and the version".to_string()).and_then(id);
    let op = match op.as_str() {
        "create" => MemoryOp::Create { entry_id: Uuid::new_v4(), title, body, tags },
        "edit" => MemoryOp::Edit { entry_id: need(&entry_id)?, base: need(&base)?, title, body, tags },
        "remove" => MemoryOp::Remove { entry_id: need(&entry_id)?, base: need(&base)? },
        _ => return Err("op is create, edit or remove".into()),
    };
    let pid = state
        .service
        .lock()
        .unwrap()
        .propose_memory(wid, op, rationale.as_deref().unwrap_or(""), MemorySource::default())
        .map_err(|e| e.to_string())?;
    let _ = app.emit("workspace://synced", 1);
    Ok(pid.to_string())
}

/// A task-state slot as the chat's panel shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TaskSlotDto {
    #[serde(flatten)]
    pub slot: TaskSlot,
    /// The agent's display name (or the chat's runtime).
    pub agent_name: String,
    pub active: bool,
    pub stale: bool,
    pub expires_at_ms: i64,
}

pub(crate) fn task_dtos(session: &hive_core::ChatSession, agents: &[hive_core::WorkspaceAgent], now_ms: i64) -> Vec<TaskSlotDto> {
    let mut out: Vec<TaskSlotDto> = session
        .flow
        .tasks
        .iter()
        .map(|s| {
            let agent_name = hive_core::flow::agent_from_key(&s.update.agent_key)
                .and_then(|a| agents.iter().find(|x| x.id == a).map(|x| x.name.clone()))
                .unwrap_or_else(|| if s.update.agent_key == hive_core::flow::PRIMARY { "Ligus".into() } else { s.by.name.clone() });
            TaskSlotDto { agent_name, active: s.active(now_ms), stale: s.stale(now_ms), expires_at_ms: s.expires_at_ms(), slot: s.clone() }
        })
        .collect();
    out.sort_by(|a, b| a.stale.cmp(&b.stale).then(b.slot.at_ms.cmp(&a.slot.at_ms)));
    out
}

/// The chat's live task state, current first.
#[tauri::command]
pub(crate) fn task_state(state: tauri::State<'_, AppState>, session_id: String) -> Result<Vec<TaskSlotDto>, String> {
    let sid = id(&session_id)?;
    let svc = state.service.lock().unwrap();
    let Some(session) = svc.load(sid).map_err(|e| e.to_string())? else { return Ok(vec![]) };
    let agents = svc
        .load(hive_core::workspace_config_session_id(session.workspace_id))
        .ok()
        .flatten()
        .map(|c| c.workspace_agents)
        .unwrap_or_else(|| session.workspace_agents.clone());
    Ok(task_dtos(&session, &agents, hive_core::flow::now_ms()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_dtos_name_agents_and_order_current_first() {
        let mut s = hive_core::ChatSession::new("c", Uuid::new_v4(), "rt");
        let agent = hive_core::WorkspaceAgent::new("coder", "rt");
        let slot = |key: String, at_ms: i64| TaskSlot {
            update: hive_core::memory::TaskStateUpdate { task_id: s.id, agent_key: key, turn_id: Uuid::new_v4(), summary: "s".into(), ..Default::default() },
            writer: Uuid::new_v4(),
            by: Default::default(),
            at_ms,
            rev: Uuid::new_v4(),
            updates: 1,
        };
        s.flow.tasks = vec![slot(agent.id.to_string(), 0), slot("primary".into(), 10_000_000)];
        let d = task_dtos(&s, std::slice::from_ref(&agent), 10_000_001);
        assert_eq!(d[0].agent_name, "Ligus");
        assert!(d[0].active && !d[0].stale);
        assert_eq!(d[1].agent_name, "coder");
        assert!(d[1].stale, "two hours past its update");
        let json = serde_json::to_value(&d[0]).unwrap();
        assert_eq!(json["update"]["agentKey"], "primary");
        assert_eq!(json["agentName"], "Ligus");
    }
}

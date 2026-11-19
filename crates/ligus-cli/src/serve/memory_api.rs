//! Workspace memory and live task state for the phone, read-only
//! (`ligus_core::memory`; design `docs/design/v2/workspace-memory.md`).
//! A phone proposes a memory change with the `proposeMemory` phone action,
//! signed by its own key; nothing here writes. Writing, editing, removing
//! and deciding proposals stay on a person's desktop (a box is a service
//! member, and the fold refuses a service's memory writes).

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::Json;
use ligus_core::memory;
use serde_json::{json, Value};

use super::api::{load_chat, ApiError, ApiResult};
use super::Shared;

type S = State<std::sync::Arc<Shared>>;

/// The most entries one search returns.
const MAX_LIST: usize = 100;

fn agent_name(s: &Shared, key: &str) -> String {
    if key == ligus_core::flow::PRIMARY {
        return "Ligus".into();
    }
    let Some(id) = ligus_core::flow::agent_from_key(key) else { return String::new() };
    let config = s.lock().load(ligus_core::workspace_config_session_id(s.ws)).ok().flatten();
    config.and_then(|c| c.workspace_agents.into_iter().find(|a| a.id == id).map(|a| a.name)).unwrap_or_default()
}

/// `GET /v1/memory?q=…&tag=…&limit=…`: live entries matching every keyword
/// (and every tag), best first.
pub async fn list(State(s): S, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let m = s.lock().memory(s.ws)?;
    let tags: Vec<String> = q.get("tag").map(|t| t.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default();
    let limit = q.get("limit").and_then(|l| l.parse::<usize>().ok()).unwrap_or(50).clamp(1, MAX_LIST);
    let hits = memory::search(&m, q.get("q").map(String::as_str).unwrap_or(""), &tags, limit);
    let entries: Vec<Value> = hits.iter().map(|h| json!({"entry": h.entry, "score": h.score})).collect();
    Ok(Json(json!({ "total": m.live_count(), "entries": entries, "openProposals": m.open_proposals().count() })))
}

/// `GET /v1/memory/{id}`: one live entry, with its conflicts and provenance.
pub async fn get_entry(State(s): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let id = uuid::Uuid::parse_str(id.trim()).map_err(|_| ApiError::bad_request("not an entry id"))?;
    let m = s.lock().memory(s.ws)?;
    let e = m.entries.get(&id).filter(|e| e.live()).ok_or_else(|| ApiError::not_found("memory entry"))?;
    let proposer = (!e.current.proposer.is_empty()).then(|| agent_name(&s, &e.current.proposer));
    Ok(Json(json!({ "entry": e, "proposerName": proposer })))
}

/// `GET /v1/memory/proposals`: open proposals first, then the decided ones.
pub async fn proposals(State(s): S) -> ApiResult<Json<Value>> {
    let m = s.lock().memory(s.ws)?;
    let mut ps: Vec<&memory::ProposalState> = m.proposals.values().collect();
    ps.sort_by(|a, b| a.decision.is_some().cmp(&b.decision.is_some()).then(b.at_ms.cmp(&a.at_ms)));
    let out: Vec<Value> = ps.into_iter().take(MAX_LIST).map(|p| json!({"proposal": p, "proposerName": agent_name(&s, &p.proposal.proposer)})).collect();
    Ok(Json(json!({ "proposals": out })))
}

/// `GET /v1/chats/{id}/task-state`: the chat's task-state slots that are
/// still current (active or recently finished), newest first.
pub async fn task_state(State(s): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let chat = load_chat(&s, &id)?;
    let now = ligus_core::flow::now_ms();
    let mut slots: Vec<&memory::TaskSlot> = chat.flow.tasks.iter().filter(|t| !t.stale(now)).collect();
    slots.sort_by_key(|t| std::cmp::Reverse(t.at_ms));
    let out: Vec<Value> = slots
        .into_iter()
        .map(|t| json!({"slot": t, "agentName": agent_name(&s, &t.update.agent_key), "active": t.active(now), "expiresAtMs": t.expires_at_ms()}))
        .collect();
    Ok(Json(json!({ "slots": out })))
}

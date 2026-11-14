//! Flow for the phone, read-only: the task board, the review queue and its
//! backpressure alert, workspace marks, a summary of the routing shadow
//! report, and each chat's hand-off cards. Computed from the same folded
//! `FlowRecord`s the desktop's Flow pane reads (`hive_core::flow`), with the
//! default thresholds (queue limit 5, long wait 2 h, a 14-day routing window):
//! those knobs are a desktop's local settings and this box has none.
//!
//! Nothing here writes: picking up board work, leaving or clearing marks,
//! zones, careers and route stops stay on the desktop.

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::Json;
use hive_core::flow::{self, tremble, FlowState};
use hive_core::{ChatSession, ProposalStatus};
use serde_json::{json, Value};

use super::api::{load_chat, ApiResult};
use super::Shared;

type S = State<std::sync::Arc<Shared>>;

/// Routing report window.
const ROUTING_DAYS: i64 = 14;
/// Waits and recruits older than this are left out of the queue view.
const QUEUE_WINDOW_MS: i64 = 7 * 86_400_000;
const MAX_BOARD: usize = 50;
const MAX_MARKS: usize = 50;
const MAX_RECRUITS: usize = 10;

/// Display names for agent keys: the primary, then every workspace agent.
fn agent_names(chats: &[ChatSession], config: &ChatSession) -> HashMap<String, String> {
    let mut out = HashMap::new();
    out.insert(flow::PRIMARY.to_string(), "Ligus (primary)".to_string());
    for a in chats.iter().flat_map(|c| c.workspace_agents.iter()).chain(config.workspace_agents.iter()) {
        out.insert(flow::agent_key(Some(a.id)), a.name.clone());
    }
    out
}

fn name_of(names: &HashMap<String, String>, key: &str) -> String {
    names.get(key).cloned().unwrap_or_else(|| key.get(..8).unwrap_or(key).to_string())
}

fn chat_title(chats: &[ChatSession], id: uuid::Uuid) -> String {
    chats.iter().find(|c| c.id == id).map(|c| c.title.clone()).unwrap_or_default()
}

fn kind_str<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// Gate waits measured from decided proposals (created → last vote).
fn gate_waits(chats: &[ChatSession], since: i64) -> Vec<flow::EdgeWait> {
    let mut out = Vec::new();
    for c in chats {
        for p in c.proposals.iter().filter(|p| p.status != ProposalStatus::Open) {
            let Some(decided) = p.approvals.iter().map(|a| flow::ms(&a.created_at)).max() else { continue };
            if decided < since {
                continue;
            }
            out.push(flow::EdgeWait {
                edge: "gate".into(),
                reference: p.id.to_string(),
                wait_ms: (decided - flow::ms(&p.created_at)).max(0),
                host_id: String::new(),
                at_ms: decided,
            });
        }
    }
    out
}

pub fn board(chats: &[ChatSession], config: &FlowState, now: i64) -> Vec<Value> {
    let marks: Vec<&flow::WorkspaceMark> = config.marks.values().collect();
    flow::board::open_items(chats, &marks, now)
        .into_iter()
        .take(MAX_BOARD)
        .map(|i| {
            let mut v = json!({
                "id": i.id,
                "kind": kind_str(&i.kind),
                "kindLabel": i.kind.as_str(),
                "title": i.title,
                "sinceMs": i.since_ms,
                "stimulus": (i.stimulus * 100.0).round() / 100.0,
                "claimedBy": i.claimed_by,
            });
            if !i.session_id.is_nil() {
                v["chatId"] = json!(i.session_id);
                v["chatTitle"] = json!(chat_title(chats, i.session_id));
            }
            if let Some(p) = i.proposal_id {
                v["proposalId"] = json!(p);
            }
            v
        })
        .collect()
}

pub fn queue(chats: &[ChatSession], now: i64) -> Value {
    let policy = tremble::TremblePolicy::default();
    let since = now - QUEUE_WINDOW_MS;
    let gate = tremble::gate_queue(chats, &policy, now);
    let gate_w = gate_waits(chats, since);
    let mut waits: Vec<&flow::EdgeWait> = chats.iter().flat_map(|c| c.flow.waits.iter()).collect();
    waits.extend(gate_w.iter());
    let edges = tremble::edge_stats(&waits, &policy, since);
    let alert = tremble::bottleneck_alert(&gate, &edges, &policy);
    let mut recruits: Vec<(&ChatSession, &flow::RecruitNotice)> = chats
        .iter()
        .flat_map(|c| c.flow.recruits.iter().map(move |r| (c, r)))
        .filter(|(_, r)| r.at_ms >= since)
        .collect();
    recruits.sort_by_key(|(_, r)| std::cmp::Reverse(r.at_ms));
    let recruits: Vec<Value> = recruits
        .into_iter()
        .take(MAX_RECRUITS)
        .map(|(c, r)| {
            json!({
                "id": r.id,
                "action": r.action,
                "queueLen": r.queue_len,
                "oldestWaitMs": r.oldest_wait_ms,
                "memberCount": r.member_ids.len(),
                "chatId": c.id,
                "atMs": r.at_ms,
            })
        })
        .collect();
    json!({
        "gate": kind_str(&gate),
        "edges": kind_str(&edges),
        "alert": alert,
        "queueLimit": policy.queue_limit,
        "longWaitMs": policy.long_wait_ms,
        "recruits": recruits,
    })
}

pub fn marks(chats: &[ChatSession], config: &FlowState, now: i64) -> Vec<Value> {
    flow::marks::active(config.marks.values(), now)
        .into_iter()
        .take(MAX_MARKS)
        .map(|m| {
            let mut v = json!({
                "id": m.id,
                "kind": kind_str(&m.kind),
                "kindLabel": m.kind.as_str(),
                "path": m.path,
                "taskId": m.task_id,
                "note": m.note,
                "by": m.by,
                "byAgent": !m.agent.is_empty(),
                "atMs": m.at_ms,
                "expiresMs": m.expires_ms,
                "describe": m.describe(),
            });
            if let Some((a, b)) = m.lines {
                v["lines"] = json!([a, b]);
            }
            if let Some(sid) = m.session_id {
                v["chatId"] = json!(sid);
                v["chatTitle"] = json!(chat_title(chats, sid));
            }
            v
        })
        .collect()
}

/// The routing shadow report, summarized: how many un-addressed messages the
/// router decided on in the window, how often it would have picked someone
/// other than the primary, its most common picks, per-arm outcomes, and the
/// routes in cool-down.
pub fn routing(chats: &[ChatSession], config: &FlowState, names: &HashMap<String, String>, now: i64) -> Value {
    let since = now - ROUTING_DAYS * 86_400_000;
    let decisions: Vec<&flow::RouteDecision> =
        chats.iter().flat_map(|c| c.flow.route_decisions.values()).filter(|d| d.at_ms >= since).collect();
    let n = decisions.len();
    let off_primary = decisions.iter().filter(|d| d.shadow_choice != flow::PRIMARY).count();
    let explored = decisions.iter().filter(|d| d.explore).count();
    let live = decisions.iter().filter(|d| d.mode == flow::RouteMode::Live).count();
    let mut picks: HashMap<&str, u32> = HashMap::new();
    for d in &decisions {
        *picks.entry(d.shadow_choice.as_str()).or_default() += 1;
    }
    let mut picks: Vec<(&str, u32)> = picks.into_iter().collect();
    picks.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let top: Vec<Value> =
        picks.into_iter().take(3).map(|(k, c)| json!({ "agent": k, "name": name_of(names, k), "count": c })).collect();

    // Per-arm outcomes from tagged turns and the proposals they filed.
    #[derive(Default)]
    struct Arm {
        turns: u32,
        proposals: u32,
        accepted: u32,
        judged: u32,
    }
    let mut arms: HashMap<String, Arm> = HashMap::new();
    for c in chats {
        for t in c.flow.turns.iter().filter(|t| t.at_ms >= since && !t.arm.is_empty()) {
            let row = arms.entry(t.arm.clone()).or_default();
            row.turns += 1;
            for pid in &t.proposal_ids {
                let Some(p) = c.proposals.iter().find(|p| p.id == *pid) else { continue };
                row.proposals += 1;
                match p.status {
                    ProposalStatus::Approved | ProposalStatus::Applied => {
                        row.accepted += 1;
                        row.judged += 1;
                    }
                    ProposalStatus::Rejected => row.judged += 1,
                    ProposalStatus::Open if p.superseded_by.is_some() => row.judged += 1,
                    ProposalStatus::Open => {}
                }
            }
        }
    }
    let mut arms: Vec<(String, Arm)> = arms.into_iter().collect();
    arms.sort_by(|a, b| a.0.cmp(&b.0));
    let arms: Vec<Value> = arms
        .into_iter()
        .map(|(arm, r)| {
            json!({
                "arm": arm,
                "turns": r.turns,
                "proposals": r.proposals,
                "accepted": r.accepted,
                "judged": r.judged,
                "acceptanceRate": (r.judged > 0).then(|| r.accepted as f64 / r.judged as f64),
            })
        })
        .collect();
    let mut stops: Vec<Value> = config
        .route_stops
        .values()
        .filter(|st| st.active(now))
        .map(|st| {
            json!({
                "route": st.route,
                "name": match st.route.strip_prefix("runtime:") {
                    Some(rt) => format!("every agent on {rt}"),
                    None => name_of(names, &st.route),
                },
                "reason": st.reason.as_str(),
                "untilMs": st.until_ms,
                "failures": st.failures,
            })
        })
        .collect();
    stops.sort_by_key(|v| v["untilMs"].as_i64().unwrap_or(0));
    json!({
        "windowDays": ROUTING_DAYS,
        "decisions": n,
        "liveDecisions": live,
        "offPrimaryShare": if n == 0 { 0.0 } else { off_primary as f64 / n as f64 },
        "explored": explored,
        "topPicks": top,
        "arms": arms,
        "stops": stops,
    })
}

/// `GET /v1/flow`: board, queue, marks and the routing summary.
pub async fn flow_view(State(s): S) -> ApiResult<Json<Value>> {
    let chats = s.workspace_chats()?;
    let (_, config_session) = super::api::hosts_and_agents(&s)?;
    let config = s.lock().flow_config(s.ws)?;
    let names = agent_names(&chats, &config_session);
    let now = flow::now_ms();
    Ok(Json(json!({
        "board": board(&chats, &config, now),
        "queue": queue(&chats, now),
        "marks": marks(&chats, &config, now),
        "routing": routing(&chats, &config, &names, now),
    })))
}

pub fn handoff_dto(h: &flow::HandoffCard, names: &HashMap<String, String>) -> Value {
    let pick = |name: &str, key: &str| if name.trim().is_empty() { name_of(names, key) } else { name.to_string() };
    let mut v = json!({
        "id": h.id,
        "from": h.from,
        "to": h.to,
        "fromName": pick(&h.from_name, &h.from),
        "toName": pick(&h.to_name, &h.to),
        "task": h.task,
        "taskClass": h.task_class,
        "branch": h.branch,
        "files": h.files,
        "via": h.via,
        "quality": kind_str(&h.quality),
        "atMs": h.at_ms,
    });
    if let Some(m) = h.message_id {
        v["messageId"] = json!(m);
    }
    if let Some(p) = h.parcel.as_ref().filter(|p| !p.is_empty()) {
        v["parcel"] = kind_str(&p.bounded());
    }
    v
}

/// `GET /v1/chats/:id/handoffs`: the chat's hand-off cards, oldest first
/// (each names the reply message that handed the work on, so the phone can
/// place it in the transcript, and once the receiver has answered, that
/// answer as `replyMessageId`).
pub async fn chat_handoffs(State(s): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let chat = load_chat(&s, &id)?;
    let (_, config_session) = super::api::hosts_and_agents(&s)?;
    let names = agent_names(std::slice::from_ref(&chat), &config_session);
    let mut cards: Vec<&flow::HandoffCard> = chat.flow.handoffs.iter().collect();
    cards.sort_by_key(|h| h.at_ms);
    let dtos: Vec<Value> = cards
        .into_iter()
        .map(|h| {
            let mut v = handoff_dto(h, &names);
            if let Some(r) = chat.flow.handoff_reply(h) {
                v["replyMessageId"] = json!(r);
            }
            v
        })
        .collect();
    Ok(Json(json!({ "handoffs": dtos })))
}

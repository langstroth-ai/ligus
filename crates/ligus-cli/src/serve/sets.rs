//! Proposal sets for the phone, read-only: the alternatives of each set with
//! their standing (decayed scout support, qualifying scouts, agent quorum),
//! the posted scout assessments, the front-runner / frozen / decided state and
//! each alternative's latest pre-flight. The same shape as the desktop's
//! `ProposalSetDto` (`ligus_proto::proposal_sets`), plus the chat it lives in
//! and what this box's identity can do on each alternative (`myVote`,
//! `canVote`).
//!
//! Blind until posted: a scout's assessment exists in the log only once the
//! scout posted it, and only posted assessments are listed. Scouts still in
//! flight (a desktop-local hint) are never shown.
//!
//! Votes on an alternative go through `POST /v1/proposals/:id/approve` /
//! `reject` like any proposal. Tuning the dial, pre-flight, apply and deciding
//! a set stay on the desktop: there is no route here that writes a set.

use ligus_runtime::display::shown;
use axum::extract::{Path, Query, State};
use axum::Json;
use ligus_core::proposal_sets::{
    is_self_assessment, CheckOutcome, CheckStatus, ProposalSet, QuorumDial, QuorumKnobs, SetPhase, StopReason,
};
use ligus_core::{ChatSession, Timestamp};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::api::{self, iso, load_chat, ApiError, ApiResult};
use super::Shared;

type S = State<std::sync::Arc<Shared>>;

/// "A", "B", … for an alternative's position (as the desktop labels them).
fn label(i: usize) -> String {
    char::from(b'A' + (i.min(25) as u8)).to_string()
}

fn phase_str(p: SetPhase) -> &'static str {
    match p {
        SetPhase::Scouting => "scouting",
        SetPhase::Piping => "piping",
        SetPhase::Decided => "decided",
        SetPhase::Unknown => "unknown",
    }
}

fn stop_str(r: StopReason) -> &'static str {
    match r {
        StopReason::Quorum => "quorum",
        StopReason::TieDeadline => "tieDeadline",
        StopReason::Withdrawn => "withdrawn",
        StopReason::Unknown => "unknown",
    }
}

fn check(c: &CheckOutcome) -> Value {
    let status = match c.status {
        CheckStatus::Passed => "passed",
        CheckStatus::Failed => "failed",
        CheckStatus::Skipped => "skipped",
        CheckStatus::Unknown => "unknown",
    };
    json!({ "status": status, "note": c.note })
}

fn dial_of(set: &ProposalSet) -> &'static str {
    let now = QuorumKnobs {
        agent_quorum: set.agent_quorum,
        required_approvals: set.required_approvals,
        quality_bar: set.quality_bar,
    };
    [(QuorumDial::Fast, "fast"), (QuorumDial::Balanced, "balanced"), (QuorumDial::Careful, "careful")]
        .into_iter()
        .find(|(d, _)| d.knobs() == now)
        .map_or("custom", |(_, n)| n)
}

/// One set as the phone shows it. `me` is who is asking ([`api::viewer`]).
pub fn set_dto(set: &ProposalSet, chat: &ChatSession, me: &str) -> Value {
    let alternatives: Vec<Value> = set
        .alternatives
        .iter()
        .enumerate()
        .filter_map(|(i, id)| {
            let p = chat.proposals.iter().find(|p| p.id == *id)?;
            let st = set.standing(&chat.proposals, *id);
            let stop = set.stop_of(*id);
            // Posted assessments only, newest round last.
            let assessments: Vec<Value> = set
                .assessments
                .iter()
                .filter(|a| a.proposal_id == *id)
                .map(|a| {
                    json!({
                        "id": a.id,
                        "scoutName": shown(&a.scout_name),
                        "scoutRuntime": a.scout_runtime,
                        "score": a.score,
                        "currentSupport": set.current_support(a),
                        "rationale": a.rationale,
                        "round": a.round,
                        "isSelf": is_self_assessment(p, a),
                        "createdAt": iso(a.created_at),
                    })
                })
                .collect();
            // What this identity can do on it, from the ordinary proposal view.
            let mine = api::proposal_dto_as(chat, p, me);
            let mut v = json!({
                "proposalId": id,
                "label": label(i),
                "title": shown(&p.title),
                "body": p.body,
                "kind": api::kind_str(p.kind),
                "status": api::status_str(p.status),
                "author": api::proposal_author(chat, p),
                "changedFiles": p.changed_files,
                "support": st.support,
                "qualifyingScouts": st.qualifying_scouts,
                "scouts": st.scouts,
                "runtimes": st.runtimes,
                "meetsQuorum": set.meets_quorum(&chat.proposals, *id),
                "frozen": set.is_frozen(*id),
                "active": set.is_active(&chat.proposals, *id),
                "superseded": p.superseded_by.is_some(),
                "isFrontRunner": set.front_runner == Some(*id),
                "isWinner": set.winner == Some(*id),
                "stopReason": stop.map(|s| stop_str(s.reason)),
                "stoppedBy": stop.and_then(|s| s.stopped_by),
                "assessments": assessments,
                "qualifyingApprovals": p.qualifying_approvals(),
                "requiredApprovals": p.required_approvals,
                "votes": mine["votes"],
                "myVote": mine["myVote"],
                "canVote": mine["canVote"],
                "isMine": mine["isMine"],
                "reverted": p.reverted,
            });
            if let Some(r) = set.latest_preflight(*id) {
                v["preflight"] = json!({
                    "passed": r.report.passed(),
                    "tests": check(&r.report.tests),
                    "baseUnchanged": check(&r.report.base_unchanged),
                    "hostsOnline": check(&r.report.hosts_online),
                    "at": iso(r.at),
                });
            }
            Some(v)
        })
        .collect();
    let author = if set.author_actor_id.trim().is_empty() {
        set.producing_agent_id
            .and_then(|id| chat.workspace_agents.iter().find(|a| a.id == id))
            .map(|a| format!("@{}", shown(&a.name)))
            .unwrap_or_default()
    } else {
        api::actor_name(chat, &set.author_actor_id)
    };
    json!({
        "id": set.id,
        "chatId": chat.id,
        "chatTitle": match shown(&chat.title) { t if t.is_empty() => "Untitled".to_string(), t => t },
        "question": set.question,
        "body": set.body,
        "author": author,
        "taskClass": set.task_class,
        "phase": phase_str(set.phase),
        "open": set.is_open(),
        "round": set.round,
        "attempt": set.attempt,
        "agentQuorum": set.agent_quorum,
        "qualityBar": set.quality_bar,
        "decayPerRound": set.decay_per_round,
        "requiredApprovals": set.required_approvals,
        "approvalRoleFloor": api::role_name(set.approval_role_floor),
        "dial": dial_of(set),
        "scoutingDeadline": set.scouting_deadline.map(iso),
        "deadlinePassed": set.deadline_passed(Timestamp::now()),
        "frontRunner": set.front_runner,
        "tieBreak": set.tie_break,
        "winner": set.winner,
        "decidedAt": set.decided_at.map(iso),
        "createdAt": iso(set.created_at),
        "alternatives": alternatives,
        // Pre-flight, apply and the decision are desktop-only.
        "decideOn": "desktop",
    })
}

#[derive(Deserialize)]
pub struct SetFilter {
    status: Option<String>,
}

fn wants_all(f: &SetFilter) -> ApiResult<bool> {
    match f.status.as_deref().map(str::trim) {
        None | Some("") | Some("open") => Ok(false),
        Some("all") => Ok(true),
        Some(other) => Err(ApiError::bad_request(format!("status must be open or all (got '{other}')"))),
    }
}

fn sets_of(chat: &ChatSession, all: bool, me: &str) -> Vec<(Timestamp, Value)> {
    chat.proposal_sets
        .iter()
        .filter(|s| all || s.is_open())
        .map(|s| (s.created_at, set_dto(s, chat, me)))
        .collect()
}

fn newest_first(mut v: Vec<(Timestamp, Value)>) -> Vec<Value> {
    v.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    v.into_iter().map(|(_, v)| v).collect()
}

/// `GET /v1/proposal-sets?status=open|all`: every set in the workspace,
/// newest first (open ones by default).
pub async fn list_sets(State(s): S, Query(f): Query<SetFilter>) -> ApiResult<Json<Value>> {
    let all = wants_all(&f)?;
    let me = api::viewer(&s);
    let mut out = Vec::new();
    for chat in s.workspace_chats()? {
        out.extend(sets_of(&chat, all, &me));
    }
    Ok(Json(json!({ "sets": newest_first(out) })))
}

/// `GET /v1/chats/:id/proposal-sets?status=open|all`: one chat's sets
/// (all of them by default, as the desktop's set view lists them).
pub async fn chat_sets(State(s): S, Path(id): Path<String>, Query(f): Query<SetFilter>) -> ApiResult<Json<Value>> {
    let all = f.status.is_none() || wants_all(&f)?;
    let chat = load_chat(&s, &id)?;
    let me = api::viewer(&s);
    Ok(Json(json!({ "sets": newest_first(sets_of(&chat, all, &me)) })))
}

/// `GET /v1/proposal-sets/:id`: one set.
pub async fn get_set(State(s): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let set_id = Uuid::parse_str(id.trim()).map_err(|_| ApiError::not_found("proposal set"))?;
    let me = api::viewer(&s);
    for chat in s.workspace_chats()? {
        if let Some(set) = chat.proposal_sets.iter().find(|x| x.id == set_id) {
            return Ok(Json(json!({ "set": set_dto(set, &chat, &me) })));
        }
    }
    Err(ApiError::not_found("proposal set"))
}

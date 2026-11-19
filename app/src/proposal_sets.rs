//! Proposal sets in the desktop app: filing an agent's `[[alternatives:]]`,
//! recording scouts' `[[assess:]]` blind, settling quorum with stop signals,
//! sending scouts, the quorum dial, and the two-phase commit (pre-flight, then
//! apply). The process and its rules live in `ligus_core::proposal_sets`; the
//! signed writes in `ligus_runtime::chat_service`.
//!
//! Agents reach a set only through their reply directives: they can open one
//! and score alternatives. Votes, the dial, pre-flight, apply and close are
//! commands a person invokes from the set view.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use ligus_core::proposal_sets::{
    is_self_assessment, CheckOutcome, CheckStatus, PreflightReport, ProposalSet, QuorumDial, QuorumKnobs,
    ScoutAssessment, SetPhase, StopReason,
};
use ligus_core::{ActionProposal, ChatSession, ProposalKind, ProposalStatus, Timestamp};
use ligus_proto::{PreflightCheckDto, PreflightDto, ProposalSetDto, ScoutAssessmentDto, SetAlternativeDto};
use ligus_runtime::chat_service::{ChatService, QuorumOutcome};
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

use crate::{map_err, rfc3339, role_name, AppState};

/// set id → (alternative id, scout name).
type PendingScouts = Mutex<HashMap<Uuid, Vec<(Uuid, String)>>>;

/// Scouts sent from this device whose assessment hasn't arrived. In memory
/// only; a hint for the view.
fn pending() -> &'static PendingScouts {
    static PENDING: OnceLock<PendingScouts> = OnceLock::new();
    PENDING.get_or_init(Default::default)
}

/// "A", "B", … for an alternative's position.
fn label(i: usize) -> String {
    char::from(b'A' + (i.min(25) as u8)).to_string()
}

fn kind_str(k: ProposalKind) -> &'static str {
    match k {
        ProposalKind::FileDiff => "fileDiff",
        ProposalKind::Command => "command",
        ProposalKind::Decision => "decision",
    }
}

fn status_str(s: ProposalStatus) -> &'static str {
    match s {
        ProposalStatus::Open => "open",
        ProposalStatus::Approved => "approved",
        ProposalStatus::Rejected => "rejected",
        ProposalStatus::Applied => "applied",
    }
}

fn phase_str(p: SetPhase) -> &'static str {
    match p {
        SetPhase::Scouting => "scouting",
        SetPhase::Piping => "piping",
        SetPhase::Decided => "decided",
        SetPhase::Unknown => "unknown",
    }
}

fn check_dto(c: &CheckOutcome) -> PreflightCheckDto {
    let status = match c.status {
        CheckStatus::Passed => "passed",
        CheckStatus::Failed => "failed",
        CheckStatus::Skipped => "skipped",
        CheckStatus::Unknown => "unknown",
    };
    PreflightCheckDto { status: status.into(), note: c.note.clone() }
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

pub(crate) fn set_dto(set: &ProposalSet, session: &ChatSession) -> ProposalSetDto {
    let pending = pending().lock().unwrap().get(&set.id).cloned().unwrap_or_default();
    let alternatives = set
        .alternatives
        .iter()
        .enumerate()
        .filter_map(|(i, id)| {
            let p = session.proposals.iter().find(|p| p.id == *id)?;
            let st = set.standing(&session.proposals, *id);
            let stop = set.stop_of(*id);
            let assessments = set
                .assessments
                .iter()
                .filter(|a| a.proposal_id == *id)
                .map(|a| ScoutAssessmentDto {
                    id: a.id.to_string(),
                    scout_name: crate::shown(&a.scout_name),
                    scout_runtime: a.scout_runtime.clone(),
                    score: a.score,
                    current_support: set.current_support(a),
                    rationale: a.rationale.clone(),
                    round: a.round,
                    is_self: is_self_assessment(p, a),
                    created_at: rfc3339(a.created_at),
                })
                .collect::<Vec<_>>();
            let posted: Vec<&str> = assessments.iter().map(|a| a.scout_name.as_str()).collect();
            Some(SetAlternativeDto {
                proposal_id: id.to_string(),
                label: label(i),
                title: crate::shown(&p.title),
                body: p.body.clone(),
                kind: kind_str(p.kind).into(),
                status: status_str(p.status).into(),
                changed_files: p.changed_files.clone(),
                support: st.support,
                qualifying_scouts: st.qualifying_scouts,
                scouts: st.scouts,
                runtimes: st.runtimes,
                meets_quorum: set.meets_quorum(&session.proposals, *id),
                frozen: set.is_frozen(*id),
                active: set.is_active(&session.proposals, *id),
                superseded: p.superseded_by.is_some(),
                stop_reason: stop.map(|s| {
                    match s.reason {
                        StopReason::Quorum => "quorum",
                        StopReason::TieDeadline => "tieDeadline",
                        StopReason::Withdrawn => "withdrawn",
                        StopReason::Unknown => "unknown",
                    }
                    .to_string()
                }),
                stopped_by: stop.and_then(|s| s.stopped_by).map(|u| u.to_string()),
                pending_scouts: pending
                    .iter()
                    .filter(|(pid, name)| pid == id && !posted.contains(&name.as_str()))
                    .map(|(_, n)| n.clone())
                    .collect(),
                assessments,
                qualifying_approvals: p.qualifying_approvals() as u32,
                required_approvals: p.required_approvals,
                preflight: set.latest_preflight(*id).map(|r| PreflightDto {
                    passed: r.report.passed(),
                    tests: check_dto(&r.report.tests),
                    base_unchanged: check_dto(&r.report.base_unchanged),
                    hosts_online: check_dto(&r.report.hosts_online),
                    at: rfc3339(r.at),
                }),
                reverted: p.reverted,
            })
        })
        .collect();
    ProposalSetDto {
        id: set.id.to_string(),
        question: set.question.clone(),
        body: set.body.clone(),
        author: set.author_actor_id.clone(),
        task_class: set.task_class.clone(),
        phase: phase_str(set.phase).into(),
        round: set.round,
        attempt: set.attempt,
        agent_quorum: set.agent_quorum,
        quality_bar: set.quality_bar,
        decay_per_round: set.decay_per_round,
        required_approvals: set.required_approvals,
        approval_role_floor: role_name(set.approval_role_floor).into(),
        dial: dial_of(set).into(),
        scouting_deadline: set.scouting_deadline.map(rfc3339),
        deadline_passed: set.deadline_passed(Timestamp::now()),
        front_runner: set.front_runner.map(|u| u.to_string()),
        tie_break: set.tie_break,
        winner: set.winner.map(|u| u.to_string()),
        decided_at: set.decided_at.map(rfc3339),
        alternatives,
        created_at: rfc3339(set.created_at),
    }
}

fn label_of(set: &ProposalSet, id: Uuid) -> String {
    set.alternatives.iter().position(|a| *a == id).map(label).unwrap_or_else(|| "?".into())
}

fn title_of(session: &ChatSession, id: Uuid) -> String {
    session.proposals.iter().find(|p| p.id == id).map(|p| crate::shown(&p.title)).unwrap_or_default()
}

/// Post the chat notes for a settled quorum: the front-runner, the frozen
/// rivals, and a stop signal naming each rival's scouts.
fn announce(svc: &mut ChatService, sid: Uuid, wid: Uuid, set_id: Uuid, out: &QuorumOutcome) {
    let Ok(Some(session)) = svc.load(sid) else { return };
    let Some(set) = session.proposal_sets.iter().find(|s| s.id == set_id) else { return };
    let lead = format!("{} \u{201c}{}\u{201d}", label_of(set, out.front_runner), title_of(&session, out.front_runner));
    let how = if out.tie_break {
        "nothing reached quorum by the deadline, and it has the most support".to_string()
    } else {
        format!("{} scouts independently cleared the bar", set.agent_quorum)
    };
    let _ = svc.post_system_note(
        sid,
        wid,
        format!(
            "\u{1f41d} Quorum on \u{201c}{}\u{201d}: {lead} is the front-runner ({how}). Rival alternatives are frozen. \
A person makes the final call: approve it, run the pre-flight, then apply.",
            set.question
        ),
    );
    for (rival, scouts) in &out.stopped {
        if scouts.is_empty() {
            continue;
        }
        let _ = svc.post_system_note(
            sid,
            wid,
            format!(
                "\u{270b} Stop signal for {} ({}): {} is the front-runner. Stand down, or re-evaluate against it if the set returns to scouting.",
                label_of(set, *rival),
                scouts.join(", "),
                label_of(set, out.front_runner),
            ),
        );
    }
}

/// Assessments a reply recorded (through the shared finalize): clear the
/// matching pending-scout hints.
pub(crate) fn assessments_arrived(author: &str, recorded: &[(Uuid, Uuid)]) {
    let mut p = pending().lock().unwrap();
    for (set_id, pid) in recorded {
        if let Some(list) = p.get_mut(set_id) {
            list.retain(|(x, n)| !(x == pid && n == author));
        }
    }
}

/// Group a just-filed agent proposal with others answering the same request,
/// when that request asked for options. Joins an existing set for the same
/// trigger, or opens one once a second producer has proposed.
pub(crate) fn group_same_trigger(svc: &mut ChatService, sid: Uuid, wid: Uuid, new_id: Uuid) {
    let Ok(Some(session)) = svc.load(sid) else { return };
    let Some(new) = session.proposals.iter().find(|p| p.id == new_id) else { return };
    if new.set_id.is_some() || new.workflow.is_some() || new.parent_id.is_some() {
        return;
    }
    let Some(trigger) = session.messages.iter().rev().find(|m| m.role == ligus_core::MessageRole::User) else {
        return;
    };
    if !ligus_core::proposal_sets::wants_options(&trigger.body) {
        return;
    }
    let producer = |p: &ActionProposal| {
        p.producing_agent_id.map(|a| a.to_string()).unwrap_or_else(|| p.author_actor_id.clone())
    };
    if let Some(set) = session
        .proposal_sets
        .iter()
        .find(|s| s.is_open() && s.trigger_message_id == Some(trigger.id))
    {
        let _ = svc.add_set_alternative(sid, wid, set.id, new_id, None);
        return;
    }
    let siblings: Vec<&ActionProposal> = session
        .proposals
        .iter()
        .filter(|p| {
            p.set_id.is_none()
                && p.workflow.is_none()
                && p.parent_id.is_none()
                && p.status == ProposalStatus::Open
                && p.created_at >= trigger.created_at
        })
        .collect();
    let mut producers: Vec<String> = siblings.iter().map(|p| producer(p)).collect();
    producers.sort();
    producers.dedup();
    if producers.len() < 2 {
        return;
    }
    let question: String = trigger.body.chars().take(120).collect();
    let mut set = ProposalSet::new(question, "", siblings.iter().map(|p| p.id).collect());
    set.trigger_message_id = Some(trigger.id);
    if let Ok(set) = svc.group_into_proposal_set(sid, wid, set) {
        let _ = svc.post_system_note(
            sid,
            wid,
            format!(
                "\u{1f41d} {} competing proposals answer the same request, so they're grouped as a proposal set. Send scouts from the Review pane.",
                set.alternatives.len()
            ),
        );
    }
}

/// A possible scout: an agent in the chat, or the primary runtime.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Scout {
    pub agent_id: Option<Uuid>,
    pub name: String,
    pub runtime: String,
}

/// Choose scouts for the alternatives that still need assessments this round.
/// Each needs `agent_quorum` fresh assessments; a scout never rates an
/// alternative it produced; scouts on a runtime not yet used for that
/// alternative go first (diversity stands in for numbers), then the least
/// loaded. Deterministic.
pub(crate) fn plan_scouts(set: &ProposalSet, proposals: &[ActionProposal], scouts: &[Scout]) -> Vec<(Uuid, usize)> {
    let mut load = vec![0usize; scouts.len()];
    let mut plan = Vec::new();
    for pid in &set.alternatives {
        if !set.is_active(proposals, *pid) || set.is_frozen(*pid) {
            continue;
        }
        let Some(p) = proposals.iter().find(|p| p.id == *pid) else { continue };
        let fresh: Vec<&ScoutAssessment> = set
            .assessments
            .iter()
            .filter(|a| a.proposal_id == *pid && a.round == set.round && !is_self_assessment(p, a))
            .collect();
        let mut used: Vec<String> = fresh.iter().map(|a| a.scout_runtime.clone()).collect();
        let needed = (set.agent_quorum as usize).saturating_sub(fresh.len());
        let probe = |s: &Scout| ScoutAssessment {
            id: Uuid::nil(),
            proposal_id: *pid,
            scout_agent_id: s.agent_id,
            scout_name: s.name.clone(),
            scout_runtime: s.runtime.clone(),
            score: 0,
            rationale: String::new(),
            round: 0,
            attempt: 0,
            created_at: Timestamp::epoch(),
        };
        let mut candidates: Vec<usize> = (0..scouts.len())
            .filter(|&i| {
                let a = probe(&scouts[i]);
                !is_self_assessment(p, &a) && !fresh.iter().any(|f| f.scout_key() == a.scout_key())
            })
            .collect();
        for _ in 0..needed {
            candidates.sort_by_key(|&i| (used.contains(&scouts[i].runtime), load[i], i));
            let Some(&pick) = candidates.first() else { break };
            candidates.retain(|&i| i != pick);
            used.push(scouts[pick].runtime.clone());
            load[pick] += 1;
            plan.push((*pid, pick));
        }
    }
    plan
}

fn scouts_for(session: &ChatSession) -> Vec<Scout> {
    let mut agents: Vec<&ligus_core::WorkspaceAgent> = session
        .workspace_agents
        .iter()
        .filter(|a| session.participant_agent_ids.is_empty() || session.participant_agent_ids.contains(&a.id))
        .collect();
    agents.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out: Vec<Scout> = agents
        .into_iter()
        .map(|a| Scout { agent_id: Some(a.id), name: a.name.clone(), runtime: a.runtime_id.clone() })
        .collect();
    out.push(Scout { agent_id: None, name: "Ligus".into(), runtime: session.runtime_id.clone() });
    out
}

/// The scout's instruction: one alternative, nothing about the others' scores.
fn scout_prompt(set: &ProposalSet, p: &ActionProposal, scout: &Scout) -> String {
    let who = match scout.agent_id {
        Some(_) => format!("@{}", scout.name),
        None => "@primary".to_string(),
    };
    let mut detail = String::new();
    if !p.body.trim().is_empty() {
        detail.push_str(&format!("\n\n{}", p.body.trim()));
    }
    if let Some(diff) = &p.diff {
        let clipped: String = diff.chars().take(8000).collect();
        detail.push_str(&format!("\n\n```diff\n{clipped}\n```"));
        if clipped.len() < diff.len() {
            detail.push_str("\n(diff truncated)");
        }
    }
    format!(
        "{who} Scout assessment for the proposal set \u{201c}{q}\u{201d}. Assess only this alternative, on its own merits. \
Other scouts are assessing the alternatives independently, and you won't see their scores.\n\n\
**{label}. {title}**{detail}\n\n\
End your reply with [[assess: {{\"alternative\": \"{id}\", \"score\": <0-100>, \"rationale\": \"<why>\"}}]].",
        q = set.question,
        label = label_of(set, p.id),
        title = p.title,
        id = p.id,
    )
}

/// Send scouts: one turn per (alternative, scout) assignment, each asked about
/// a single alternative. Runs in the background; returns how many were sent.
/// The person clicking is who posts the instructions (it costs turns).
#[tauri::command]
pub(crate) async fn send_scouts(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    set_id: String,
) -> Result<u32, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let set_id = Uuid::parse_str(&set_id).map_err(map_err)?;
    let (prompts, names) = {
        let svc = state.service.lock().unwrap();
        let session = svc.load(sid).map_err(map_err)?.ok_or("unknown session")?;
        let set = session.proposal_sets.iter().find(|s| s.id == set_id).ok_or("unknown proposal set")?;
        if set.phase != SetPhase::Scouting {
            return Err("scouts only go out while the set is scouting".into());
        }
        let scouts = scouts_for(&session);
        let plan = plan_scouts(set, &session.proposals, &scouts);
        let mut prompts = Vec::new();
        let mut names = Vec::new();
        for (pid, i) in plan {
            let Some(p) = session.proposals.iter().find(|p| p.id == pid) else { continue };
            prompts.push(scout_prompt(set, p, &scouts[i]));
            names.push((pid, scouts[i].name.clone()));
        }
        (prompts, names)
    };
    if prompts.is_empty() {
        return Err("no scouts are available: every alternative already has fresh assessments, or only its own producer could assess it".into());
    }
    pending().lock().unwrap().entry(set_id).or_default().extend(names);
    let n = prompts.len() as u32;
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        for body in prompts {
            let state = app2.state::<AppState>();
            if let Err(e) = crate::send_message(app2.clone(), state, sid.to_string(), body, None).await {
                tracing::warn!(target: "proposal_sets", "scout turn failed: {e}");
            }
            let _ = app2.emit("workspace://synced", 1);
        }
    });
    Ok(n)
}

#[tauri::command]
pub(crate) async fn list_proposal_sets(ligus_app: tauri::AppHandle, session_id: String) -> Result<Vec<ProposalSetDto>, String> {
    crate::off_main(ligus_app, move |ligus_app| list_proposal_sets_sync(tauri::Manager::state::<AppState>(ligus_app), session_id)).await
}

pub(crate) fn list_proposal_sets_sync(state: State<AppState>, session_id: String) -> Result<Vec<ProposalSetDto>, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let mut svc = state.service.lock().unwrap();
    let Some(mut session) = svc.load(sid).map_err(map_err)? else { return Ok(Vec::new()) };
    // A scouting deadline that passed with a leader: settle it now (any
    // device may; the first tie-break in canonical order wins).
    let now = Timestamp::now();
    let due: Vec<Uuid> = session
        .proposal_sets
        .iter()
        .filter(|s| s.deadline_leader(&session.proposals, now).is_some())
        .map(|s| s.id)
        .collect();
    if !due.is_empty() {
        let wid = session.workspace_id;
        for set_id in due {
            if let Ok(Some(out)) = svc.settle_proposal_set(sid, wid, set_id, now) {
                announce(&mut svc, sid, wid, set_id, &out);
            }
        }
        session = svc.load(sid).map_err(map_err)?.ok_or("unknown session")?;
    }
    Ok(session.proposal_sets.iter().rev().map(|s| set_dto(s, &session)).collect())
}

fn one_set(svc: &ChatService, sid: Uuid, set_id: Uuid) -> Result<ProposalSetDto, String> {
    let session = svc.load(sid).map_err(map_err)?.ok_or("unknown session")?;
    let set = session.proposal_sets.iter().find(|s| s.id == set_id).ok_or("unknown proposal set")?;
    Ok(set_dto(set, &session))
}

/// The "fast ↔ careful" dial (`dial`), or explicit knobs.
#[tauri::command]
pub(crate) async fn tune_proposal_set(ligus_app: tauri::AppHandle, session_id: String, set_id: String, dial: Option<String>, agent_quorum: Option<u32>, required_approvals: Option<u32>, quality_bar: Option<u32>) -> Result<ProposalSetDto, String> {
    crate::off_main(ligus_app, move |ligus_app| tune_proposal_set_sync(tauri::Manager::state::<AppState>(ligus_app), session_id, set_id, dial, agent_quorum, required_approvals, quality_bar)).await
}

pub(crate) fn tune_proposal_set_sync(
    state: State<AppState>,
    session_id: String,
    set_id: String,
    dial: Option<String>,
    agent_quorum: Option<u32>,
    required_approvals: Option<u32>,
    quality_bar: Option<u32>,
) -> Result<ProposalSetDto, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let set_id = Uuid::parse_str(&set_id).map_err(map_err)?;
    let mut svc = state.service.lock().unwrap();
    let session = svc.load(sid).map_err(map_err)?.ok_or("unknown session")?;
    let set = session.proposal_sets.iter().find(|s| s.id == set_id).ok_or("unknown proposal set")?;
    let mut knobs = match dial.as_deref() {
        Some("fast") => QuorumDial::Fast.knobs(),
        Some("careful") => QuorumDial::Careful.knobs(),
        Some("balanced") => QuorumDial::Balanced.knobs(),
        _ => QuorumKnobs {
            agent_quorum: set.agent_quorum,
            required_approvals: set.required_approvals,
            quality_bar: set.quality_bar,
        },
    };
    knobs.agent_quorum = agent_quorum.unwrap_or(knobs.agent_quorum);
    knobs.required_approvals = required_approvals.unwrap_or(knobs.required_approvals);
    knobs.quality_bar = quality_bar.unwrap_or(knobs.quality_bar);
    svc.tune_proposal_set(sid, session.workspace_id, set_id, knobs).map_err(map_err)?;
    // A lower bar may have reached quorum.
    if let Ok(Some(out)) = svc.settle_proposal_set(sid, session.workspace_id, set_id, Timestamp::now()) {
        announce(&mut svc, sid, session.workspace_id, set_id, &out);
    }
    one_set(&svc, sid, set_id)
}

/// New evidence: advance the round (agent support decays) so scouts
/// re-evaluate. Also called when a person comments on an alternative.
#[tauri::command]
pub(crate) async fn advance_proposal_set_round(ligus_app: tauri::AppHandle, session_id: String, set_id: String) -> Result<ProposalSetDto, String> {
    crate::off_main(ligus_app, move |ligus_app| advance_proposal_set_round_sync(tauri::Manager::state::<AppState>(ligus_app), session_id, set_id)).await
}

pub(crate) fn advance_proposal_set_round_sync(
    state: State<AppState>,
    session_id: String,
    set_id: String,
) -> Result<ProposalSetDto, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let set_id = Uuid::parse_str(&set_id).map_err(map_err)?;
    let mut svc = state.service.lock().unwrap();
    let wid = svc.load(sid).map_err(map_err)?.ok_or("unknown session")?.workspace_id;
    svc.advance_set_round(sid, wid, set_id, "re-evaluate").map_err(map_err)?;
    one_set(&svc, sid, set_id)
}

/// A person commented on (or asked for changes to) an alternative: that is
/// new evidence, so the set's round advances.
pub(crate) fn on_alternative_feedback(svc: &mut ChatService, sid: Uuid, wid: Uuid, proposal_id: Uuid) {
    let Ok(Some(session)) = svc.load(sid) else { return };
    if let Some(set) = session.proposal_sets.iter().find(|s| s.is_open() && s.contains(proposal_id)) {
        let _ = svc.advance_set_round(sid, wid, set.id, "comment");
    }
}

/// Refuse agent work on a frozen alternative (a rival while piping, or stopped).
pub(crate) fn ensure_not_frozen(session: &ChatSession, proposal_id: Uuid) -> Result<(), String> {
    match session.proposal_sets.iter().find(|s| s.contains(proposal_id)) {
        Some(set) if set.is_frozen(proposal_id) && set.phase != SetPhase::Decided => {
            Err("this alternative is frozen while another is the front-runner".into())
        }
        _ => Ok(()),
    }
}

/// After a revision turn: put the new version in its parent's set.
pub(crate) fn link_revision(svc: &mut ChatService, sid: Uuid, wid: Uuid, parent: Uuid, child: Uuid) {
    let Ok(Some(session)) = svc.load(sid) else { return };
    if let Some(set) = session.proposal_sets.iter().find(|s| s.is_open() && s.contains(parent)) {
        let _ = svc.add_set_alternative(sid, wid, set.id, child, Some(parent));
    }
}

/// Whether `proposal_id` must go through its set's two-phase commit rather
/// than a plain Implement.
pub(crate) fn guarded_by_open_set(session: &ChatSession, proposal_id: Uuid) -> bool {
    session.proposal_sets.iter().any(|s| s.is_open() && s.contains(proposal_id))
}

#[tauri::command]
pub(crate) async fn withdraw_set_alternative(ligus_app: tauri::AppHandle, session_id: String, set_id: String, proposal_id: String) -> Result<ProposalSetDto, String> {
    crate::off_main(ligus_app, move |ligus_app| withdraw_set_alternative_sync(tauri::Manager::state::<AppState>(ligus_app), session_id, set_id, proposal_id)).await
}

pub(crate) fn withdraw_set_alternative_sync(
    state: State<AppState>,
    session_id: String,
    set_id: String,
    proposal_id: String,
) -> Result<ProposalSetDto, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let set_id = Uuid::parse_str(&set_id).map_err(map_err)?;
    let pid = Uuid::parse_str(&proposal_id).map_err(map_err)?;
    let mut svc = state.service.lock().unwrap();
    let wid = svc.load(sid).map_err(map_err)?.ok_or("unknown session")?.workspace_id;
    svc.withdraw_set_alternative(sid, wid, set_id, pid).map_err(map_err)?;
    one_set(&svc, sid, set_id)
}

/// Close the set without choosing.
#[tauri::command]
pub(crate) async fn close_proposal_set(ligus_app: tauri::AppHandle, session_id: String, set_id: String) -> Result<ProposalSetDto, String> {
    crate::off_main(ligus_app, move |ligus_app| close_proposal_set_sync(tauri::Manager::state::<AppState>(ligus_app), session_id, set_id)).await
}

pub(crate) fn close_proposal_set_sync(
    state: State<AppState>,
    session_id: String,
    set_id: String,
) -> Result<ProposalSetDto, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let set_id = Uuid::parse_str(&set_id).map_err(map_err)?;
    let mut svc = state.service.lock().unwrap();
    let wid = svc.load(sid).map_err(map_err)?.ok_or("unknown session")?.workspace_id;
    svc.decide_proposal_set(sid, wid, set_id, None).map_err(map_err)?;
    one_set(&svc, sid, set_id)
}

/// Why a set alternative can't move toward apply yet because of a workspace
/// guard (a chat lockdown, or dearth mode with a change to a brood path): it
/// needs a human member's own approval first, set or no set. Checked before
/// the pre-flight and the apply so a guard block is never recorded as a failed
/// pre-flight (which would send the whole set back to scouting).
pub(crate) fn guard_block(session: &ChatSession, proposal: &ActionProposal) -> Option<String> {
    ligus_core::guard::human_gate_reason(session, proposal).map(|r| format!("Not yet: {r}."))
}

/// Phase one: run and record the pre-flight for an approved alternative. A
/// failure sends the set back to scouting.
#[tauri::command]
pub(crate) async fn preflight_proposal_set(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    set_id: String,
    proposal_id: String,
) -> Result<ProposalSetDto, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let set_id = Uuid::parse_str(&set_id).map_err(map_err)?;
    let pid = Uuid::parse_str(&proposal_id).map_err(map_err)?;
    let (wid, set, proposal, agent_host) = {
        let svc = state.service.lock().unwrap();
        let session = svc.load(sid).map_err(map_err)?.ok_or("unknown session")?;
        let set = session.proposal_sets.iter().find(|s| s.id == set_id).cloned().ok_or("unknown proposal set")?;
        let p = session.proposals.iter().find(|p| p.id == pid).cloned().ok_or("unknown proposal")?;
        if !set.is_open() || !set.is_active(&session.proposals, pid) {
            return Err("that alternative isn't live in an open set".into());
        }
        if !p.ready_to_apply() {
            return Err(format!(
                "approve it first: it needs {} qualifying approval{}",
                p.required_approvals,
                if p.required_approvals == 1 { "" } else { "s" }
            ));
        }
        if let Some(reason) = guard_block(&session, &p) {
            return Err(reason);
        }
        let host = p
            .producing_agent_id
            .and_then(|a| session.workspace_agents.iter().find(|x| x.id == a))
            .filter(|a| !a.host_id.is_empty())
            .map(|a| a.host_id.clone());
        (session.workspace_id, set, p, host)
    };
    // A proposal for a workspace repo is checked against this device's
    // checkout of that repo (5.7), never the workspace's bound folder.
    let repo = match proposal.repo_id {
        Some(r) => {
            let svc = state.service.lock().unwrap();
            svc.store().chat_checkout(sid, r).ok().flatten().or_else(|| svc.store().primary_checkout(wid, r).ok().flatten().map(|m| m.path)).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
        }
        None => state.workspace_repo_root(),
    };
    let repo_ok = !repo.trim().is_empty() && ligus_runtime::git_worktree::is_git_repo(std::path::Path::new(&repo));
    let mut hosts = Vec::new();
    if proposal.diff.is_some() {
        hosts.push(("this device".to_string(), true));
    }
    if let Some(h) = agent_host.filter(|h| *h != state.local_host_id()) {
        let up = crate::host_online(&state, wid, &h);
        hosts.push((h, up));
    }
    let chat_id = sid.to_string();
    let diff = proposal.diff.clone();
    let base = set.base_commit.clone();
    let report: PreflightReport = tauri::async_runtime::spawn_blocking(move || {
        let root = repo_ok.then(|| std::path::PathBuf::from(&repo));
        let inputs = ligus_runtime::proposal_preflight::PreflightInputs {
            repo_root: root.as_deref(),
            chat_id: &chat_id,
            diff: diff.as_deref(),
            base_commit: base.as_deref(),
            test_command: ligus_runtime::proposal_preflight::test_command(),
            test_timeout: None,
            hosts,
        };
        ligus_runtime::proposal_preflight::run(&inputs, &format!("preflight-{}", Uuid::new_v4()))
    })
    .await
    .map_err(map_err)?;
    let passed = report.passed();
    let dto = {
        let mut svc = state.service.lock().unwrap();
        svc.record_set_preflight(sid, wid, set_id, pid, report).map_err(map_err)?;
        if !passed {
            let _ = svc.post_system_note(
                sid,
                wid,
                format!(
                    "\u{1f6d1} Pre-flight failed for {} in \u{201c}{}\u{201d}: the set is back to scouting, and earlier assessments have decayed a round.",
                    label_of(&set, pid),
                    set.question
                ),
            );
        }
        one_set(&svc, sid, set_id)?
    };
    let _ = app.emit("workspace://synced", 1);
    Ok(dto)
}

/// Phase two: apply the alternative whose pre-flight passed, then record the
/// decision. A failed apply is recorded as a failed pre-flight (back to scouting).
#[tauri::command]
pub(crate) async fn apply_proposal_set(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    set_id: String,
    proposal_id: String,
) -> Result<ProposalSetDto, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let set_id = Uuid::parse_str(&set_id).map_err(map_err)?;
    let pid = Uuid::parse_str(&proposal_id).map_err(map_err)?;
    let wid = {
        let svc = state.service.lock().unwrap();
        let session = svc.load(sid).map_err(map_err)?.ok_or("unknown session")?;
        let set = session.proposal_sets.iter().find(|s| s.id == set_id).ok_or("unknown proposal set")?;
        if !set.can_decide_for(&session.proposals, pid) {
            return Err("run a passing pre-flight on an approved alternative first".into());
        }
        if let Some(p) = session.proposals.iter().find(|p| p.id == pid) {
            if let Some(reason) = guard_block(&session, p) {
                return Err(reason);
            }
        }
        // Authorize the decision before touching the code.
        let actor = crate::local_actor(&state);
        let probe = ligus_core::SessionEvent::ProposalSetDecided { set_id, winner: Some(pid), at: Timestamp::now() };
        let d = ligus_core::authorize(&probe, &actor.id, session.role_of(&actor.id), &session);
        if !d.allowed {
            return Err(d.summary);
        }
        session.workspace_id
    };
    if let Err(e) = crate::implement_proposal_inner(app.clone(), state.clone(), session_id.clone(), proposal_id.clone()).await {
        let mut svc = state.service.lock().unwrap();
        let report = PreflightReport {
            tests: CheckOutcome::skipped("not run"),
            base_unchanged: CheckOutcome::failed(format!("apply failed: {e}")),
            hosts_online: CheckOutcome::skipped("not run"),
        };
        let _ = svc.record_set_preflight(sid, wid, set_id, pid, report);
        return Err(e);
    }
    let dto = {
        let mut svc = state.service.lock().unwrap();
        let set = svc.decide_proposal_set(sid, wid, set_id, Some(pid)).map_err(map_err)?;
        let session = svc.load(sid).map_err(map_err)?.ok_or("unknown session")?;
        let _ = svc.post_system_note(
            sid,
            wid,
            format!(
                "\u{2705} Decided \u{201c}{}\u{201d}: {} \u{201c}{}\u{201d} was applied.",
                set.question,
                label_of(&set, pid),
                title_of(&session, pid)
            ),
        );
        pending().lock().unwrap().remove(&set_id);
        set_dto(&set, &session)
    };
    let _ = app.emit("workspace://synced", 1);
    Ok(dto)
}

/// Revert an applied file change where it was applied, through the same
/// path as the apply: a workspace repo's proposal on this device's checkout
/// of that repo (after the turn gate: still that repo, managed by this
/// host), as a commit on the chat branch carrying the `Ligus-Revert`
/// trailer a ledger replay reads; a proposal without a repo in its own
/// workspace's folder (never one another host manages), else that folder's
/// root. Then mark it reverted.
#[tauri::command]
pub(crate) async fn revert_proposal(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    proposal_id: String,
) -> Result<(), String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let pid = Uuid::parse_str(&proposal_id).map_err(map_err)?;
    let (session, p) = {
        let svc = state.service.lock().unwrap();
        let session = svc.load(sid).map_err(map_err)?.ok_or("unknown session")?;
        let p = session.proposals.iter().find(|p| p.id == pid).cloned().ok_or("unknown proposal")?;
        (session, p)
    };
    let wid = session.workspace_id;
    if p.status != ProposalStatus::Applied || p.reverted {
        return Err("only an applied change that hasn't been reverted can be reverted".into());
    }
    let diff = p.diff.clone().ok_or("only file changes can be reverted here")?;
    let title = p.title.clone();
    match revert_target(p.repo_id, session.repo.as_ref().and_then(|r| r.repo_id))? {
        RevertTarget::Repo => {
            let prepared = crate::repos_ui::turn_repo(&state, &session)?.ok_or("this chat has no repository on this device")?;
            let checkout = prepared.checkout.clone();
            tauri::async_runtime::spawn_blocking(move || {
                ligus_runtime::repos::apply::revert_on_chat_branch(&checkout, &sid.to_string(), &diff, &title, pid)
            })
            .await
            .map_err(map_err)?
            .map_err(|e| format!("Not reverted: {e}. Nothing changed."))?;
        }
        RevertTarget::Legacy => {
            let repo = crate::legacy_folder(&state, wid)?;
            if repo.trim().is_empty() {
                return Err("this workspace has no folder on this device to revert in".into());
            }
            tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
                let repo_path = std::path::Path::new(&repo);
                let chat = ligus_runtime::git_worktree::is_git_repo(repo_path)
                    .then(|| ligus_runtime::git_worktree::chat_worktree(repo_path, &sid.to_string()).ok())
                    .flatten();
                match chat {
                    Some(wt) => {
                        let wt = wt.to_string_lossy().into_owned();
                        reverse_patch(&wt, &diff)?;
                        crate::commit_worktree(&wt, &ligus_runtime::repos::apply::legacy_revert_message(&title))
                    }
                    None => reverse_patch(&repo, &diff),
                }
            })
            .await
            .map_err(map_err)??;
        }
    }
    {
        let mut svc = state.service.lock().unwrap();
        svc.mark_proposal_reverted(sid, wid, pid).map_err(map_err)?;
        let _ = svc.post_system_note(sid, wid, format!("\u{21a9}\u{fe0f} Reverted \u{201c}{}\u{201d}.", p.title));
    }
    let _ = app.emit("workspace://synced", 1);
    Ok(())
}

/// Where a revert runs, by the proposal's repository and the chat's.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RevertTarget {
    /// The chat's repository checkout on this device (its chat branch).
    Repo,
    /// The workspace's bound folder (a proposal without a repository).
    Legacy,
}

/// A proposal made in a repository reverts only in that repository's
/// checkout, never the workspace folder (as its apply, review F1); a chat
/// that moved to another repository can't revert it here.
pub(crate) fn revert_target(proposal_repo: Option<Uuid>, chat_repo: Option<Uuid>) -> Result<RevertTarget, String> {
    match proposal_repo {
        None => Ok(RevertTarget::Legacy),
        Some(r) if chat_repo == Some(r) => Ok(RevertTarget::Repo),
        Some(_) => Err("This chat now works in another repository; switch it back to revert this change.".into()),
    }
}

fn reverse_patch(dir: &str, diff: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = ligus_core::process_util::git()
        .arg("-C")
        .arg(dir)
        .args(["apply", "-R", "--whitespace=nowarn"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git apply: {e}"))?;
    child.stdin.take().ok_or("git apply: no stdin")?.write_all(diff.as_bytes()).map_err(map_err)?;
    let out = child.wait_with_output().map_err(map_err)?;
    if out.status.success() {
        Ok(())
    } else {
        Err("couldn't revert: the code has moved on since the change was applied".into())
    }
}

/// The chat's current base commit on this device, recorded when a set opens.
pub(crate) fn base_commit_for(workspace_root: &str, sid: Uuid) -> Option<String> {
    let root = std::path::Path::new(workspace_root);
    if workspace_root.trim().is_empty() || !ligus_runtime::git_worktree::is_git_repo(root) {
        return None;
    }
    ligus_runtime::proposal_preflight::current_base(root, &sid.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ligus_core::crypto::SigningKeypair;
    use ligus_core::{ActorIdentity, ActorKind, WorkspaceAgent};
    use ligus_runtime::EventStore;

    fn service() -> ChatService {
        let store = EventStore::open_in_memory().unwrap();
        let author = ActorIdentity::new("u1", "Mara", ActorKind::Human);
        let mut svc = ChatService::new(store, SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), author);
        svc.enable_test_presence();
        svc
    }

    const ALTS: &str = r#"Here are three. [[alternatives: {"question": "Which cache?", "taskClass": "design", "options": [{"title": "Redis"}, {"title": "In-process LRU"}, {"title": "No cache"}]}]]"#;

    fn finalize(svc: &mut ChatService, sid: Uuid, wid: Uuid, who: &str, agent: Option<Uuid>, body: &str) -> String {
        let mid = svc.begin_assistant_message(sid, wid, who, "rt", agent, None).unwrap();
        crate::finalize_reply(svc, sid, wid, mid, who, agent, "", body, false).unwrap()
    }

    #[test]
    fn an_alternatives_directive_opens_a_set_and_assessments_are_blind_and_settle_quorum() {
        let mut svc = service();
        let wid = Uuid::new_v4();
        svc.ensure_workspace_config(wid).unwrap();
        let chat = svc.create_chat("A", wid, "anthropic").unwrap();
        let sid = chat.id;
        let planner = WorkspaceAgent::new("planner", "claude-code");
        let coder = WorkspaceAgent::new("coder", "ollama");
        let reviewer = WorkspaceAgent::new("reviewer", "copilot");
        let (pl, co, re) = (planner.id, coder.id, reviewer.id);
        for a in [planner, coder, reviewer] {
            svc.add_agent(sid, wid, a).unwrap();
        }
        let cleaned = finalize(&mut svc, sid, wid, "planner", Some(pl), ALTS);
        assert_eq!(cleaned, "Here are three.");
        let s = svc.load(sid).unwrap().unwrap();
        let set = s.proposal_sets[0].clone();
        assert_eq!(set.alternatives.len(), 3);
        assert_eq!(set.task_class.as_deref(), Some("design"));
        assert!(s.proposals.iter().all(|p| p.producing_agent_id == Some(pl) && p.set_id == Some(set.id)));

        // The producer's own assessment never counts; the visible reply is
        // replaced so the next scout can't read the score.
        let b = set.alternatives[1];
        let own = format!(r#"Mine is great. [[assess: {{"alternative": "{b}", "score": 100}}]]"#);
        let shown = finalize(&mut svc, sid, wid, "planner", Some(pl), &own);
        assert!(shown.contains("scout assessment") && !shown.contains("100"), "{shown}");
        let r1 = format!(r#"It scores 88 from me. [[assess: {{"alternative": "{b}", "score": 88, "rationale": "simple"}}]]"#);
        let shown = finalize(&mut svc, sid, wid, "coder", Some(co), &r1);
        assert!(!shown.contains("88"), "the score never reaches the transcript: {shown}");
        let s = svc.load(sid).unwrap().unwrap();
        assert_eq!(s.proposal_sets[0].phase, SetPhase::Scouting, "one counting scout < k=2");
        // A letter works when one set is open.
        finalize(&mut svc, sid, wid, "reviewer", Some(re), r#"[[assess: {"alternative": "B", "score": 75}]]"#);
        let s = svc.load(sid).unwrap().unwrap();
        let set = &s.proposal_sets[0];
        assert_eq!(set.phase, SetPhase::Piping);
        assert_eq!(set.front_runner, Some(b));
        assert_eq!(set.assessments.iter().find(|a| a.scout_name == "coder").unwrap().scout_runtime, "ollama");
        assert!(s.messages.iter().any(|m| m.body.contains("Quorum on")));
        // Agents can't approve or decide: nothing an agent writes moves it on.
        finalize(&mut svc, sid, wid, "coder", Some(co), "[[propose: {\"title\": \"decide B\"}]] [[vote: 👍]]");
        assert_eq!(svc.load(sid).unwrap().unwrap().proposal_sets[0].phase, SetPhase::Piping);
    }

    #[test]
    fn competing_proposals_for_an_options_request_are_grouped() {
        let mut svc = service();
        let wid = Uuid::new_v4();
        svc.ensure_workspace_config(wid).unwrap();
        let sid = svc.create_chat("A", wid, "anthropic").unwrap().id;
        let (a1, a2) = (Uuid::new_v4(), Uuid::new_v4());
        svc.post_user_message(sid, wid, "@coder @local explore options for the retry logic").unwrap();
        let mut p1 = ActionProposal::new("Exponential backoff", ProposalKind::Decision, "coder");
        p1.producing_agent_id = Some(a1);
        // (An agent files a proposal as itself: the Agent tier.)
        p1.author_actor_id = a1.to_string();
        let id1 = p1.id;
        svc.upsert_proposal(sid, wid, p1).unwrap();
        group_same_trigger(&mut svc, sid, wid, id1);
        assert!(svc.load(sid).unwrap().unwrap().proposal_sets.is_empty(), "one producer: no set yet");
        let mut p2 = ActionProposal::new("Circuit breaker", ProposalKind::Decision, "local");
        p2.producing_agent_id = Some(a2);
        p2.author_actor_id = a2.to_string();
        let id2 = p2.id;
        svc.upsert_proposal(sid, wid, p2).unwrap();
        group_same_trigger(&mut svc, sid, wid, id2);
        let s = svc.load(sid).unwrap().unwrap();
        assert_eq!(s.proposal_sets.len(), 1);
        assert_eq!(s.proposal_sets[0].alternatives, vec![id1, id2]);

        // A plain request (complementary work) is never grouped.
        let sid2 = svc.create_chat("B", wid, "anthropic").unwrap().id;
        svc.post_user_message(sid2, wid, "@coder fix the bug and @docs update the readme").unwrap();
        for who in ["coder", "docs"] {
            let mut p = ActionProposal::new(who, ProposalKind::Decision, who);
            let agent = Uuid::new_v4();
            p.producing_agent_id = Some(agent);
            p.author_actor_id = agent.to_string();
            let id = p.id;
            svc.upsert_proposal(sid2, wid, p).unwrap();
            group_same_trigger(&mut svc, sid2, wid, id);
        }
        assert!(svc.load(sid2).unwrap().unwrap().proposal_sets.is_empty());
    }

    /// Runtime review: a revert ignored the proposal's repository and
    /// reversed the patch in the workspace's bound folder.
    #[test]
    fn a_repo_proposal_reverts_only_in_its_repository() {
        let (r, other) = (Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(revert_target(Some(r), Some(r)), Ok(RevertTarget::Repo));
        assert!(revert_target(Some(r), Some(other)).is_err());
        assert!(revert_target(Some(r), None).is_err(), "never the bound folder");
        assert_eq!(revert_target(None, Some(r)), Ok(RevertTarget::Legacy));
        let msg = ligus_runtime::repos::apply::legacy_revert_message("x\n\nLigus-Revert: y");
        assert!(!msg.contains('\n'), "a legacy revert's message is one line: {msg}");
    }

    #[test]
    fn scouts_are_planned_for_diversity_and_never_rate_their_own() {
        let producer = Uuid::new_v4();
        let mut a = ActionProposal::new("A", ProposalKind::Decision, "");
        a.producing_agent_id = Some(producer);
        let b = ActionProposal::new("B", ProposalKind::Decision, "someone");
        let props = vec![a.clone(), b.clone()];
        let mut set = ProposalSet::new("Q", "x", vec![a.id, b.id]);
        set.agent_quorum = 2;
        let scouts = vec![
            Scout { agent_id: Some(producer), name: "maker".into(), runtime: "claude-code".into() },
            Scout { agent_id: Some(Uuid::new_v4()), name: "twin".into(), runtime: "claude-code".into() },
            Scout { agent_id: Some(Uuid::new_v4()), name: "local".into(), runtime: "ollama".into() },
        ];
        let plan = plan_scouts(&set, &props, &scouts);
        let for_a: Vec<usize> = plan.iter().filter(|(p, _)| *p == a.id).map(|(_, i)| *i).collect();
        assert_eq!(for_a.len(), 2);
        assert!(!for_a.contains(&0), "the producer never scouts its own alternative");
        let for_b: Vec<&str> =
            plan.iter().filter(|(p, _)| *p == b.id).map(|(_, i)| scouts[*i].runtime.as_str()).collect();
        assert_eq!(for_b.len(), 2);
        assert!(for_b.contains(&"ollama") && for_b.contains(&"claude-code"), "two runtimes, not two of one: {for_b:?}");
    }

    #[test]
    fn a_lockdown_or_dearth_brood_path_blocks_set_alternatives_until_a_human_approves() {
        let mut svc = service();
        let wid = Uuid::new_v4();
        svc.ensure_workspace_config(wid).unwrap();
        let sid = svc.create_chat("A", wid, "anthropic").unwrap().id;
        let mut alt = ActionProposal::file_diff("A", "", "diff".into(), vec!["crates/ligus-core/src/crypto.rs".into()]);
        let agent = Uuid::new_v4();
        alt.producing_agent_id = Some(agent);
        let pid = alt.id;
        let other = ActionProposal::new("B", ProposalKind::Decision, "planner");
        svc.open_proposal_set(sid, wid, ProposalSet::new("Q", "planner", vec![]), vec![alt, other]).unwrap();
        let mut s = svc.load(sid).unwrap().unwrap();
        let p = s.proposals.iter().find(|p| p.id == pid).cloned().unwrap();
        assert!(guard_block(&s, &p).is_none(), "no guard on: nothing extra");
        // Dearth mode protecting the crypto paths.
        s.guards.dearth = Some(ligus_core::guard::DearthMode {
            enabled: true,
            reason: String::new(),
            brood_paths: vec!["crates/ligus-core/src/crypto*.rs".into()],
            allowed_mcp_servers: vec![],
            admitted_devices: vec![],
            set_by: String::new(),
            set_at: ligus_core::Timestamp::now(),
        });
        assert!(guard_block(&s, &p).unwrap().contains("dearth"));
        // A chat lockdown blocks any alternative, brood path or not.
        s.guards.dearth = None;
        s.guards.chat_id = Some(sid);
        s.guards.lockdowns.insert(
            sid,
            vec![ligus_core::guard::ChatLockdown {
                id: Uuid::new_v4(),
                signal: ligus_core::guard::LockdownSignal::Manual,
                detail: String::new(),
                raised_by: String::new(),
                raised_at: ligus_core::Timestamp::now(),
                message_id: None,
                raise_event_id: None,
            }],
        );
        assert!(guard_block(&s, &p).unwrap().contains("locked down"));
        // The producing agent's vote never counts; a human member's does.
        let me = s.members.iter().find(|m| m.actor.kind == ligus_core::ActorKind::Human).unwrap().id.clone();
        s.guards.verified_approvals.entry(pid).or_default().insert(agent.to_string());
        assert!(guard_block(&s, &p).is_some());
        s.guards.verified_approvals.entry(pid).or_default().insert(me);
        assert!(guard_block(&s, &p).is_none());
    }

    #[test]
    fn frozen_alternatives_refuse_agent_work_and_open_sets_guard_implement() {
        let mut svc = service();
        let wid = Uuid::new_v4();
        svc.ensure_workspace_config(wid).unwrap();
        let sid = svc.create_chat("A", wid, "anthropic").unwrap().id;
        let alts: Vec<ActionProposal> =
            ["A", "B"].iter().map(|t| ActionProposal::new(*t, ProposalKind::Decision, "planner")).collect();
        let (a, b) = (alts[0].id, alts[1].id);
        let set = svc.open_proposal_set(sid, wid, ProposalSet::new("Q", "planner", vec![]), alts).unwrap();
        svc.tune_proposal_set(sid, wid, set.id, QuorumDial::Fast.knobs()).unwrap();
        let s = svc.load(sid).unwrap().unwrap();
        assert!(guarded_by_open_set(&s, a));
        assert!(ensure_not_frozen(&s, b).is_ok());
        let cleaned = finalize(&mut svc, sid, wid, "coder", Some(Uuid::new_v4()), &format!(r#"[[assess: {{"alternative": "{a}", "score": 90}}]]"#));
        assert!(cleaned.contains("scout assessment"));
        let s = svc.load(sid).unwrap().unwrap();
        assert_eq!(s.proposal_sets[0].front_runner, Some(a));
        assert!(ensure_not_frozen(&s, b).is_err());
        let dto = set_dto(&s.proposal_sets[0], &s);
        assert_eq!(dto.phase, "piping");
        assert_eq!(dto.dial, "fast");
        assert_eq!(dto.alternatives[1].stop_reason.as_deref(), Some("quorum"), "every live rival is stopped");
        assert!(dto.alternatives[1].frozen);
    }
}

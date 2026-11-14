//! Agent effects, shared by the Ligus MCP tools and the `[[…]]` reply
//! directives (Track D): one filing path, so a runtime with tool use and one
//! without produce the same proposals, sets, assessments, marks, hand-offs
//! and reactions (directive fallback parity).
//!
//! Every signed effect runs inside [`ChatService::as_agent`] for the
//! responding agent: held to the Agent tier (proposals only; no admin or
//! governance events, no votes, no `ProposalApplied`) and signed with the
//! agent's own key where this host holds one. Chat notes about an effect are
//! the host's own system messages, posted outside that scope.

use hive_core::flow::directives::{MarkDirective, UnmarkDirective};
use hive_core::flow::{self, ContextParcel, FlowRecord, HandoffCard, WorkspaceMark};
use hive_core::proposal_sets::ProposalSet;
use hive_core::{ActionProposal, ChatSession, MessageRole, ProposalKind, ProposedCommand, Timestamp};
use uuid::Uuid;

use crate::directives::{ParsedAlternatives, ParsedAssessment};
use crate::ChatService;

/// Where an effect came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Directive,
    Mcp,
}

impl Via {
    pub fn as_str(self) -> &'static str {
        match self {
            Via::Directive => "directive",
            Via::Mcp => "tool",
        }
    }
}

/// Who an effect is for.
#[derive(Debug, Clone)]
pub struct EffectCtx {
    pub session_id: Uuid,
    pub workspace_id: Uuid,
    /// The responder's display name (`@coder`, or the primary's label).
    pub author: String,
    /// The responding agent (`None` = the chat's primary runtime).
    pub agent_id: Option<Uuid>,
    /// The turn's reply message.
    pub message_id: Option<Uuid>,
    pub via: Via,
}

impl EffectCtx {
    pub fn agent_key(&self) -> String {
        flow::agent_key(self.agent_id)
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// A proposal to file.
#[derive(Debug, Clone, Default)]
pub struct NewProposal {
    pub title: String,
    pub body: String,
    pub kind: Option<ProposalKind>,
    /// Raises the chat's default only (never lowers it).
    pub required_approvals: Option<u32>,
    pub command: Option<ProposedCommand>,
    pub task_class: Option<String>,
}

/// File an agent's proposal for human review (never executed here). The
/// author is the responder and the producing agent is recorded, so
/// Implement targets it and its own vote never counts.
pub fn propose(svc: &mut ChatService, ctx: &EffectCtx, np: NewProposal) -> Result<Uuid, String> {
    let title = np.title.trim();
    if title.is_empty() {
        return Err("a proposal needs a title".into());
    }
    let kind = np.kind.unwrap_or(if np.command.is_some() { ProposalKind::Command } else { ProposalKind::Decision });
    if let Some(c) = &np.command {
        c.validate()?;
        if kind != ProposalKind::Command {
            return Err("argv belongs on a command proposal".into());
        }
    }
    let mut p = ActionProposal::new(title, kind, ctx.author.as_str());
    p.body = np.body;
    let default = p.required_approvals;
    p.required_approvals = np.required_approvals.map_or(default, |r| r.max(default));
    p.producing_agent_id = ctx.agent_id;
    p.command = np.command;
    p.task_class = np.task_class.filter(|c| !c.trim().is_empty()).map(|c| c.trim().chars().take(40).collect());
    let id = p.id;
    svc.as_agent(ctx.agent_id, |svc| svc.upsert_proposal(ctx.session_id, ctx.workspace_id, p)).map_err(err)?;
    let _ = svc.post_system_note(
        ctx.session_id,
        ctx.workspace_id,
        format!("\u{1f4dd} {} proposed \u{201c}{title}\u{201d}. Review and approve it in the Review pane.", ctx.author),
    );
    Ok(id)
}

/// React to a message as the agent.
pub fn react(svc: &mut ChatService, ctx: &EffectCtx, message_id: Uuid, emoji: &str) -> Result<(), String> {
    let emoji = emoji.trim();
    if emoji.is_empty() || emoji.chars().count() > 8 {
        return Err("one emoji".into());
    }
    let id = ctx.agent_id.map(|a| a.to_string()).unwrap_or_else(|| ctx.author.clone());
    let actor = hive_core::ActorIdentity::new(id, ctx.author.as_str(), hive_core::ActorKind::Agent);
    svc.as_agent(ctx.agent_id, |svc| svc.add_reaction(ctx.session_id, ctx.workspace_id, message_id, emoji, &actor)).map_err(err)
}

/// Validate and file a workflow definition as a proposal.
pub fn file_workflow(svc: &mut ChatService, ctx: &EffectCtx, json: &str) -> Result<String, String> {
    let session = svc.load(ctx.session_id).map_err(err)?.ok_or("unknown chat")?;
    let def = crate::workflow_directive::definition_from_directive(json, &session)?;
    let name = def.name.clone();
    svc.as_agent(ctx.agent_id, |svc| {
        crate::workflow_directive::file_workflow_proposal(svc, &session, ctx.workspace_id, def, &ctx.author, ctx.agent_id)
    })?;
    let _ = svc.post_system_note(
        ctx.session_id,
        ctx.workspace_id,
        format!("\u{1f4dd} {} proposed the workflow \u{201c}{name}\u{201d} \u{2014} review it in the Review pane.", ctx.author),
    );
    Ok(name)
}

/// Open a proposal set over an agent's alternatives (2 to 6).
pub fn open_alternatives(
    svc: &mut ChatService,
    ctx: &EffectCtx,
    alts: &ParsedAlternatives,
    trigger_message_id: Option<Uuid>,
    base_commit: Option<String>,
) -> Result<Uuid, String> {
    let options: Vec<_> = alts.options.iter().filter(|o| !o.title.trim().is_empty()).take(hive_core::proposal_sets::MAX_ALTERNATIVES).collect();
    if options.len() < 2 {
        return Err("a proposal set needs at least two options".into());
    }
    let proposals: Vec<ActionProposal> = options
        .iter()
        .map(|o| {
            let mut p = ActionProposal::new(o.title.trim(), o.kind.unwrap_or(ProposalKind::Decision), ctx.author.as_str());
            p.body = o.body.clone();
            p.producing_agent_id = ctx.agent_id;
            p
        })
        .collect();
    let mut set = ProposalSet::new(alts.question.trim(), ctx.author.as_str(), Vec::new());
    set.body = alts.body.clone();
    set.task_class = alts.task_class.clone().filter(|c| !c.trim().is_empty());
    set.producing_agent_id = ctx.agent_id;
    set.trigger_message_id = trigger_message_id;
    set.base_commit = base_commit;
    if let Some(k) = alts.agent_quorum {
        set.agent_quorum = k;
    }
    let n = proposals.len();
    let set = svc.as_agent(ctx.agent_id, |svc| svc.open_proposal_set(ctx.session_id, ctx.workspace_id, set, proposals)).map_err(err)?;
    let _ = svc.post_system_note(
        ctx.session_id,
        ctx.workspace_id,
        format!(
            "\u{1f41d} {} opened \u{201c}{}\u{201d} with {n} alternatives. Send scouts from the Review pane to assess them independently.",
            ctx.author, set.question
        ),
    );
    Ok(set.id)
}

/// Resolve an assessment target: a proposal id (optionally within `set`),
/// or a letter ("B") when exactly one set is open (or `set` is given).
pub fn resolve_alternative(session: &ChatSession, set: Option<Uuid>, target: &str) -> Option<(Uuid, Uuid)> {
    let t = target.trim();
    if let Ok(id) = Uuid::parse_str(t) {
        return session.proposal_sets.iter().filter(|s| set.is_none_or(|x| x == s.id)).find(|s| s.contains(id)).map(|s| (s.id, id));
    }
    let chosen = match set {
        Some(id) => session.proposal_sets.iter().find(|s| s.id == id)?,
        None => {
            let mut open = session.proposal_sets.iter().filter(|s| s.is_open());
            let s = open.next()?;
            if open.next().is_some() {
                return None;
            }
            s
        }
    };
    if t.len() != 1 {
        return None;
    }
    let idx = (t.to_ascii_uppercase().as_bytes()[0]).checked_sub(b'A')? as usize;
    chosen.alternatives.get(idx).map(|id| (chosen.id, *id))
}

/// Record one scout assessment. The scout is the responder, never the
/// payload. Returns `(set, proposal)` when the fold kept it.
pub fn assess(svc: &mut ChatService, ctx: &EffectCtx, set: Option<Uuid>, a: &ParsedAssessment) -> Result<(Uuid, Uuid), String> {
    let session = svc.load(ctx.session_id).map_err(err)?.ok_or("unknown chat")?;
    let (set_id, pid) = resolve_alternative(&session, set, &a.alternative).ok_or("no such alternative in an open proposal set")?;
    let runtime = match ctx.agent_id {
        Some(id) => session.workspace_agents.iter().find(|x| x.id == id).map(|x| x.runtime_id.clone()),
        None => Some(session.runtime_id.clone()),
    }
    .unwrap_or_default();
    let assessment = hive_core::proposal_sets::ScoutAssessment {
        id: Uuid::new_v4(),
        proposal_id: pid,
        scout_agent_id: ctx.agent_id,
        scout_name: ctx.author.clone(),
        scout_runtime: runtime,
        score: a.score.min(hive_core::proposal_sets::MAX_SCORE),
        rationale: a.rationale.trim().chars().take(2000).collect(),
        round: 0,
        attempt: 0,
        created_at: Timestamp::now(),
    };
    match svc.as_agent(ctx.agent_id, |svc| svc.post_scout_assessment(ctx.session_id, ctx.workspace_id, set_id, assessment)) {
        Ok(Some(_)) => Ok((set_id, pid)),
        Ok(None) => Err("the alternative is frozen or the set is closed".into()),
        Err(e) => Err(err(e)),
    }
}

/// Leave a workspace mark.
pub fn mark(svc: &mut ChatService, ctx: &EffectCtx, m: &MarkDirective) -> Result<Uuid, String> {
    if m.path.trim().is_empty() || m.path.len() > 1024 || std::path::Path::new(&m.path).is_absolute() {
        return Err("a mark needs a workspace-relative path".into());
    }
    let now = flow::now_ms();
    let mark = WorkspaceMark {
        id: Uuid::new_v4(),
        kind: m.kind,
        path: m.path.trim().to_string(),
        lines: m.lines,
        task_id: String::new(),
        note: m.note.chars().take(400).collect(),
        agent: ctx.agent_key(),
        by: ctx.author.clone(),
        session_id: Some(ctx.session_id),
        at_ms: now,
        expires_ms: m.kind.default_ttl_ms().map(|t| now + t),
    };
    let id = mark.id;
    svc.as_agent(ctx.agent_id, |svc| svc.record_flow(ctx.session_id, ctx.workspace_id, FlowRecord::Mark(mark))).map_err(err)?;
    Ok(id)
}

/// Clear the agent's own marks matching `u`. Returns how many.
pub fn unmark(svc: &mut ChatService, ctx: &EffectCtx, u: &UnmarkDirective) -> Result<usize, String> {
    let me = ctx.agent_key();
    let config = svc.flow_config(ctx.workspace_id).map_err(err)?;
    let mut n = 0;
    for m in config.marks.values().filter(|m| m.agent == me) {
        let hit = u.id == Some(m.id)
            || (u.path.as_deref().is_some_and(|p| m.covers(p, None)) && u.kind.is_none_or(|k| k == m.kind))
            || (u.id.is_none() && u.path.is_none() && u.kind == Some(m.kind));
        if hit {
            svc.as_agent(ctx.agent_id, |svc| svc.record_flow(ctx.session_id, ctx.workspace_id, FlowRecord::MarkCleared { mark_id: m.id }))
                .map_err(err)?;
            n += 1;
        }
    }
    Ok(n)
}

/// The outcome of a hand-off.
#[derive(Debug, Clone)]
pub struct Handoff {
    pub card: HandoffCard,
    /// The receiving agent (`None` = a person: only notified).
    pub agent: Option<Uuid>,
}

/// Record a hand-off card to `to` (an agent or a person, by name) with a
/// parcel. A hand-off to an agent queues its turn (`pending_turns`); the host
/// starts it immediately.
pub fn handoff(svc: &mut ChatService, ctx: &EffectCtx, to: &str, parcel: ContextParcel) -> Result<Handoff, String> {
    let session = svc.load(ctx.session_id).map_err(err)?.ok_or("unknown chat")?;
    let name = to.trim().trim_start_matches('@');
    if name.is_empty() {
        return Err("hand off to whom?".into());
    }
    let agent = session.workspace_agents.iter().find(|a| a.name.eq_ignore_ascii_case(name));
    let person = session.members.iter().find(|m| m.actor.display_name.eq_ignore_ascii_case(name) || m.id == name);
    let (to_key, to_name, agent_id) = match (agent, person) {
        (Some(a), _) => (flow::agent_key(Some(a.id)), a.name.clone(), Some(a.id)),
        (None, Some(p)) => (format!("member:{}", p.id), p.actor.display_name.clone(), None),
        (None, None) => return Err(format!("no agent or person named {name:?} in this chat")),
    };
    if agent_id.is_some() && agent_id == ctx.agent_id {
        return Err("an agent can't hand off to itself".into());
    }
    let parcel = parcel.bounded();
    let reply = ctx.message_id.and_then(|id| session.messages.iter().find(|m| m.id == id));
    let card = HandoffCard {
        id: Uuid::new_v4(),
        from: ctx.agent_key(),
        to: to_key,
        from_name: ctx.author.clone(),
        to_name: to_name.clone(),
        message_id: ctx.message_id,
        task: reply.map(|r| flow::handoff::task_line(&r.body, &to_name)).filter(|t| !t.is_empty()).unwrap_or_else(|| parcel.notes.clone()),
        task_class: String::new(),
        branch: format!("ligus/chat/{}", ctx.session_id),
        files: parcel.key_files.clone(),
        quality: Default::default(),
        via: ctx.via.as_str().into(),
        parcel: (!parcel.is_empty()).then_some(parcel),
        at_ms: flow::now_ms(),
    };
    svc.as_agent(ctx.agent_id, |svc| svc.record_flow(ctx.session_id, ctx.workspace_id, FlowRecord::Handoff(card.clone()))).map_err(err)?;
    if agent_id.is_none() {
        let _ = svc.post_system_note(ctx.session_id, ctx.workspace_id, format!("\u{1f91d} {} handed this to {to_name}.", ctx.author));
    }
    Ok(Handoff { card, agent: agent_id })
}

/// The trigger of a turn: the last human message before `reply`.
pub fn trigger_of(session: &ChatSession, reply: Option<Uuid>) -> Option<Uuid> {
    let upto = reply.and_then(|r| session.messages.iter().position(|m| m.id == r)).unwrap_or(session.messages.len());
    session.messages[..upto].iter().rev().find(|m| m.role == MessageRole::User).map(|m| m.id)
}

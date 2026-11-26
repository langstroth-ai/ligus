//! One `finalize_reply` for every host (D.4.3): the desktop, `ligus worker`
//! and `ligus serve` strip and apply a reply's `[[…]]` directives the same
//! way, through the shared [`crate::effects`]. Before this, headless hosts
//! never parsed directives.
//!
//! When the turn also used the Ligus MCP tools, MCP effects win: a directive
//! that duplicates one (same kind and title, same mark path, same hand-off
//! receiver, same set question, same workflow name) is dropped (D.4.4).

use std::collections::BTreeSet;

use ligus_core::flow::directives::{self as flow_directives, FlowDirectives};
use ligus_core::ProposalKind;
use uuid::Uuid;

use crate::directives::parse_reply_directives;
use crate::effects::{self, EffectCtx, NewProposal};
use crate::ChatService;

/// What the Ligus MCP tools already did this turn (for dedupe).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpEffectsLog {
    /// `(kind, lowercased title)` of filed proposals.
    pub proposals: BTreeSet<(String, String)>,
    pub set_questions: BTreeSet<String>,
    pub workflows: BTreeSet<String>,
    pub mark_paths: BTreeSet<String>,
    pub handoffs_to: BTreeSet<String>,
    pub reactions: BTreeSet<String>,
    pub assessed: BTreeSet<String>,
    /// A `ligus_propose_diff` intent: the title and body for the turn's
    /// captured diff.
    pub diff_intent: Option<(String, String, Option<String>)>,
}

impl McpEffectsLog {
    pub fn is_empty(&self) -> bool {
        *self == McpEffectsLog::default()
    }
}

pub fn kind_key(kind: ProposalKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The finalize inputs.
pub struct FinalizeInput<'a> {
    pub ctx: EffectCtx,
    /// The absolute workspace root, rewritten out of the synced reply.
    pub workspace_root: &'a str,
    pub full: &'a str,
    /// The turn's own file edits are already up for review as a real diff.
    pub edits_in_review: bool,
    /// What the turn's Ligus MCP tools already did, if it had them.
    pub mcp: Option<&'a McpEffectsLog>,
    /// The chat's base commit on this host (recorded on proposal sets).
    pub base_commit: Option<String>,
    /// Apply the flow directives (marks, hand-off parcels) here. The desktop
    /// keeps them for its after-turn hook instead (`false`).
    pub apply_flow: bool,
}

/// What finalizing did.
#[derive(Debug, Default)]
pub struct Finalized {
    /// The visible reply (completed on the message).
    pub cleaned: String,
    /// The flow directives (applied when `apply_flow`, else for the host).
    pub flow: FlowDirectives,
    pub filed: Vec<Uuid>,
    pub opened_sets: Vec<Uuid>,
    /// Sets an assessment was recorded in (the host settles their quorum).
    pub assessed: Vec<(Uuid, Uuid)>,
    pub handoffs: Vec<effects::Handoff>,
    /// Notes about directives that couldn't be filed (posted to the chat).
    pub rejected: Vec<String>,
}

/// Rewrite the absolute workspace root out of a synced reply (it would leak
/// the author's local layout): occurrences become its basename.
pub fn redact_workspace_path(body: &str, workspace_root: &str) -> String {
    let root = workspace_root.trim_end_matches('/');
    if root.is_empty() || !body.contains(root) {
        return body.to_string();
    }
    let base = std::path::Path::new(root)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(root);
    body.replace(root, base)
}

/// Strip and apply a finished reply's directives, then complete the message
/// with the visible text. See the module docs.
pub fn finalize_reply(
    svc: &mut ChatService,
    input: FinalizeInput<'_>,
) -> Result<Finalized, String> {
    let ctx = &input.ctx;
    let mcp = input.mcp.cloned().unwrap_or_default();
    let mut flow = flow_directives::parse(input.full);
    let parsed = parse_reply_directives(&flow.cleaned);
    let mut out = Finalized::default();
    let mut cleaned = redact_workspace_path(&parsed.cleaned, input.workspace_root);

    // Assessments (a scout's reply text is replaced so later scouts assess blind).
    let mut recorded: Vec<String> = Vec::new();
    for a in parsed
        .assessments
        .iter()
        .filter(|a| !mcp.assessed.contains(a.alternative.trim()))
    {
        match effects::assess(svc, ctx, None, a) {
            Ok((set, pid)) => {
                out.assessed.push((set, pid));
                recorded.push(a.alternative.trim().to_string());
            }
            Err(e) => tracing::info!(target: "finalize", "assessment not recorded: {e}"),
        }
    }
    if !recorded.is_empty() {
        let session = svc.load_in(ctx.workspace_id, ctx.session_id).ok().flatten();
        let what: Vec<String> = out
            .assessed
            .iter()
            .map(|(set, pid)| {
                session
                    .as_ref()
                    .map(|s| alternative_label(s, *set, *pid))
                    .unwrap_or_default()
            })
            .collect();
        cleaned = format!(
            "\u{1f41d} Posted a scout assessment of {}. Scores stay in the proposal set so other scouts assess blind.",
            what.join(", ")
        );
    }

    let did_nothing = parsed.emojis.is_empty()
        && parsed.workflows.is_empty()
        && parsed.proposals.is_empty()
        && parsed.alternatives.is_empty()
        && !input.edits_in_review
        && mcp.is_empty();
    if cleaned.trim().is_empty() && did_nothing {
        cleaned = crate::empty_reply_notice(&ctx.author);
    }
    if let Some(mid) = ctx.message_id {
        svc.complete_assistant_message(ctx.session_id, ctx.workspace_id, mid, &cleaned)
            .map_err(|e| e.to_string())?;
        for emoji in parsed
            .emojis
            .iter()
            .filter(|e| !mcp.reactions.contains(e.as_str()))
        {
            let _ = effects::react(svc, ctx, mid, emoji);
        }
    }
    for json in &parsed.workflows {
        let name = serde_json::from_str::<serde_json::Value>(json)
            .ok()
            .and_then(|v| {
                v.get("name")
                    .and_then(|n| n.as_str())
                    .map(|s| s.trim().to_lowercase())
            })
            .unwrap_or_default();
        if !name.is_empty() && mcp.workflows.contains(&name) {
            continue;
        }
        if let Err(e) = effects::file_workflow(svc, ctx, json) {
            let note = format!("{} suggested a workflow, but it couldn't be filed ({e}) \u{2014} no changes were made.", ctx.author);
            let _ = svc.post_system_note(ctx.session_id, ctx.workspace_id, note.clone());
            out.rejected.push(note);
        }
    }
    for p in &parsed.proposals {
        // The turn's own edits are already in review as a real diff; a
        // fileDiff directive would only duplicate it.
        if input.edits_in_review && p.kind == ProposalKind::FileDiff {
            continue;
        }
        if mcp
            .proposals
            .contains(&(kind_key(p.kind), p.title.trim().to_lowercase()))
        {
            continue;
        }
        let np = NewProposal {
            title: p.title.clone(),
            body: p.body.clone(),
            kind: Some(p.kind),
            required_approvals: Some(p.required_approvals),
            ..Default::default()
        };
        match effects::propose(svc, ctx, np) {
            Ok(id) => out.filed.push(id),
            Err(e) => tracing::warn!(target: "finalize", "proposal not filed: {e}"),
        }
    }
    if !parsed.alternatives.is_empty() {
        let trigger = svc
            .load_in(ctx.workspace_id, ctx.session_id)
            .ok()
            .flatten()
            .and_then(|s| effects::trigger_of(&s, ctx.message_id));
        for alts in parsed.alternatives.iter().filter(|a| {
            !mcp.set_questions
                .contains(&a.question.trim().to_lowercase())
        }) {
            match effects::open_alternatives(svc, ctx, alts, trigger, input.base_commit.clone()) {
                Ok(id) => out.opened_sets.push(id),
                Err(e) => tracing::warn!(target: "finalize", "proposal set not opened: {e}"),
            }
        }
    }

    // Flow directives: MCP marks and hand-offs win.
    flow.marks
        .retain(|m| !mcp.mark_paths.contains(m.path.trim()));
    flow.parcels.retain(|p| {
        p.to.as_deref().is_none_or(|t| {
            !mcp.handoffs_to
                .contains(&t.trim().trim_start_matches('@').to_lowercase())
        })
    });
    if input.apply_flow {
        for m in &flow.marks {
            let _ = effects::mark(svc, ctx, m);
        }
        for u in &flow.unmarks {
            let _ = effects::unmark(svc, ctx, u);
        }
        // A parcel naming its receiver is a hand-off on its own; one without
        // `to` rides the reply's @mention (the mention cascade starts it).
        for p in flow.parcels.iter().filter(|p| p.to.is_some()) {
            match effects::handoff(svc, ctx, p.to.as_deref().unwrap_or(""), p.parcel.clone()) {
                Ok(h) => out.handoffs.push(h),
                Err(e) => tracing::info!(target: "finalize", "hand-off not recorded: {e}"),
            }
        }
    }
    // Settle quorum on sets an assessment landed in (host action).
    let mut sets: Vec<Uuid> = out.assessed.iter().map(|(s, _)| *s).collect();
    sets.sort();
    sets.dedup();
    for set_id in sets {
        if let Ok(Some(q)) = svc.settle_proposal_set(
            ctx.session_id,
            ctx.workspace_id,
            set_id,
            ligus_core::Timestamp::now(),
        ) {
            announce_quorum(svc, ctx.session_id, ctx.workspace_id, set_id, &q);
        }
    }
    out.flow = flow;
    out.cleaned = cleaned;
    Ok(out)
}

fn letter(i: usize) -> String {
    char::from(b'A' + (i % 26) as u8).to_string()
}

/// `B “title”` for an alternative of a set.
pub fn alternative_label(session: &ligus_core::ChatSession, set_id: Uuid, pid: Uuid) -> String {
    let label = session
        .proposal_sets
        .iter()
        .find(|s| s.id == set_id)
        .and_then(|s| s.alternatives.iter().position(|a| *a == pid))
        .map(letter)
        .unwrap_or_else(|| "?".into());
    let title = session
        .proposals
        .iter()
        .find(|p| p.id == pid)
        .map(|p| p.title.clone())
        .unwrap_or_default();
    format!("{label} \u{201c}{title}\u{201d}")
}

/// Post the chat notes for a settled quorum: the front-runner, the frozen
/// rivals, and a stop signal naming each rival's scouts.
pub fn announce_quorum(
    svc: &mut ChatService,
    sid: Uuid,
    wid: Uuid,
    set_id: Uuid,
    out: &crate::chat_service::QuorumOutcome,
) {
    let Ok(Some(session)) = svc.load_in(wid, sid) else {
        return;
    };
    let Some(set) = session.proposal_sets.iter().find(|s| s.id == set_id) else {
        return;
    };
    let lead = alternative_label(&session, set_id, out.front_runner);
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
    let label_of = |id: Uuid| {
        set.alternatives
            .iter()
            .position(|a| *a == id)
            .map(letter)
            .unwrap_or_else(|| "?".into())
    };
    for (rival, scouts) in &out.stopped {
        if scouts.is_empty() {
            continue;
        }
        let _ = svc.post_system_note(
            sid,
            wid,
            format!(
                "\u{270b} Stop signal for {} ({}): {} is the front-runner. Stand down, or re-evaluate against it if the set returns to scouting.",
                label_of(*rival),
                scouts.join(", "),
                label_of(out.front_runner),
            ),
        );
    }
}

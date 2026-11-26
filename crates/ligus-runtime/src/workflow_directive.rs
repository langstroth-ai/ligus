//! Agent-authored workflows: the `[[workflow: {…}]]` reply directive and the
//! `ligus_propose_workflow` / `propose_workflow` tools share this parser and
//! filing path on every host (desktop, `ligus worker`, `ligus serve`). A
//! workflow is only ever filed as a reviewable proposal; approving it adds
//! the definition.

use ligus_core::workflow as wf;
use ligus_core::{ActionProposal, ChatSession, Timestamp};
use uuid::Uuid;

/// Roster lookup for rendering a stage's agent by name.
fn agent_name_in(session: &ChatSession) -> impl Fn(Uuid) -> Option<String> + '_ {
    move |id| session.workspace_agents.iter().find(|a| a.id == id).map(|a| a.name.clone())
}

/// The lenient JSON shape agents emit. Friendlier than the wire DTO: stages
/// address agents by roster *name*, ids default to slugified names, and
/// `onReject` is either "halt" or {"retryFrom": "<stage id>"}.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DirectiveWorkflow {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    input_label: Option<String>,
    /// Id or name of an existing definition in the chat this one revises; on
    /// approval it replaces that definition (same id) instead of adding one.
    #[serde(default)]
    revises: Option<String>,
    #[serde(default, alias = "nodes")]
    stages: Vec<DirectiveStage>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DirectiveStage {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    /// "agent" (default) | "gate"
    #[serde(default)]
    kind: Option<String>,
    #[serde(default, alias = "dependsOn", alias = "runsAfter")]
    after: Vec<String>,
    /// Roster name of the agent to run; absent ⇒ primary runtime.
    #[serde(default)]
    agent: Option<String>,
    #[serde(default, alias = "promptTemplate")]
    prompt: Option<String>,
    #[serde(default, alias = "gateTitle")]
    title: Option<String>,
    #[serde(default, alias = "gateBody")]
    body: Option<String>,
    #[serde(default, alias = "requiredApprovals")]
    approvals: Option<u32>,
    #[serde(default)]
    on_reject: Option<DirectiveReject>,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum DirectiveReject {
    /// "halt"
    Word(String),
    /// {"retryFrom": "<stage id>"}
    #[serde(rename_all = "camelCase")]
    Retry { retry_from: String },
}

/// Relax the near-miss JSON LLMs emit: drop a trailing comma sitting just before
/// a `}` or `]` (respecting string literals + escapes). Structural only — never
/// touches commas inside strings, so it can't corrupt content.
pub fn relax_json(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let (mut in_str, mut esc) = (false, false);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                i += 1; // drop the trailing comma
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Parse an agent's `[[workflow: …]]` payload (or a `propose_workflow` tool
/// call) into a validated definition. Agent names resolve against the session
/// roster, `revises` against its definitions (the result then keeps that
/// definition's id, so approving it replaces it); everything else goes
/// through `ligus_core::workflow::validate`.
pub fn definition_from_directive(
    json: &str,
    session: &ChatSession,
) -> Result<wf::WorkflowDefinition, String> {
    // Tolerate the near-miss JSON LLMs commonly emit (a trailing comma before a
    // `}`/`]`): try strict first, then a relaxed pass, before rejecting.
    let dw: DirectiveWorkflow = serde_json::from_str(json)
        .or_else(|_| serde_json::from_str(&relax_json(json)))
        .map_err(|e| format!("invalid workflow JSON: {e}"))?;
    let mut nodes = Vec::with_capacity(dw.stages.len());
    for (i, s) in dw.stages.iter().enumerate() {
        let name = s
            .name
            .clone()
            .or_else(|| s.id.clone())
            .unwrap_or_else(|| format!("Stage {}", i + 1));
        let id = s.id.clone().unwrap_or_else(|| slug(&name));
        let kind = match s.kind.as_deref() {
            None | Some("agent") => wf::WorkflowNodeKind::Agent {
                agent_id: match &s.agent {
                    None => None,
                    Some(agent_name) => Some(
                        session
                            .workspace_agents
                            .iter()
                            .find(|a| a.name.eq_ignore_ascii_case(agent_name))
                            .map(|a| a.id)
                            .ok_or_else(|| {
                                let roster: Vec<&str> = session
                                    .workspace_agents
                                    .iter()
                                    .map(|a| a.name.as_str())
                                    .collect();
                                format!(
                                    "stage {id:?} names unknown agent {agent_name:?} (roster: {})",
                                    if roster.is_empty() { "empty".into() } else { roster.join(", ") }
                                )
                            })?,
                    ),
                },
                prompt_template: s
                    .prompt
                    .clone()
                    .ok_or_else(|| format!("agent stage {id:?} needs a \"prompt\""))?,
            },
            Some("gate") => wf::WorkflowNodeKind::Gate {
                title_template: s.title.clone().unwrap_or_else(|| name.clone()),
                body_template: s.body.clone().unwrap_or_default(),
                required_approvals: s.approvals.unwrap_or(1).max(1),
                on_reject: match &s.on_reject {
                    None => wf::GateRejectPolicy::Halt,
                    Some(DirectiveReject::Word(w)) if w == "halt" => wf::GateRejectPolicy::Halt,
                    Some(DirectiveReject::Word(w)) => {
                        return Err(format!("gate {id:?}: unknown onReject {w:?}"))
                    }
                    Some(DirectiveReject::Retry { retry_from }) => {
                        wf::GateRejectPolicy::RouteTo { node: retry_from.clone() }
                    }
                },
            },
            Some(other) => return Err(format!("stage {id:?}: unknown kind {other:?}")),
        };
        nodes.push(wf::WorkflowNode {
            id,
            name,
            depends_on: s.after.clone(),
            kind,
            x: None,
            y: None,
        });
    }
    let revised = match dw.revises.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        None => None,
        Some(target) => Some(
            session
                .workflow_definitions
                .iter()
                .find(|d| d.id.to_string() == target || d.name.eq_ignore_ascii_case(target))
                .ok_or_else(|| {
                    let names: Vec<&str> = session.workflow_definitions.iter().map(|d| d.name.as_str()).collect();
                    format!(
                        "\"revises\" names no workflow in this chat ({target:?}; workflows: {})",
                        if names.is_empty() { "none".into() } else { names.join(", ") }
                    )
                })?,
        ),
    };
    if dw.name.trim().is_empty() {
        return Err("a workflow needs a \"name\"".into());
    }
    let def = wf::WorkflowDefinition {
        id: revised.map_or_else(Uuid::new_v4, |d| d.id),
        name: dw.name,
        description: dw.description,
        input_label: dw.input_label.filter(|l| !l.trim().is_empty()),
        nodes,
        created_at: revised.map_or_else(Timestamp::now, |d| d.created_at),
        disabled: revised.is_some_and(|d| d.disabled),
    };
    wf::validate(&def)?;
    Ok(def)
}

/// File an agent-authored definition as a reviewable proposal. The vote that
/// approves it adds the definition (see `ChatService::vote_on_proposal`).
/// Returns the proposal's title.
pub fn file_workflow_proposal(
    svc: &mut crate::ChatService,
    session: &ChatSession,
    workspace_id: Uuid,
    def: wf::WorkflowDefinition,
    author_actor_id: &str,
    producing_agent_id: Option<Uuid>,
) -> Result<String, String> {
    let revising = session.workflow_definitions.iter().any(|d| d.id == def.id);
    let title = if revising {
        format!("Revise workflow \u{201c}{}\u{201d}", def.name)
    } else {
        format!("Add workflow \u{201c}{}\u{201d}", def.name)
    };
    let body = wf::describe_definition(&def, &agent_name_in(session));
    let mut proposal = ActionProposal::workflow(title.clone(), author_actor_id, body, def);
    proposal.producing_agent_id = producing_agent_id;
    svc.upsert_proposal(session.id, workspace_id, proposal).map_err(|e| e.to_string())?;
    Ok(title)
}

/// Mirror of the frontend's slugify, for defaulted stage ids.
pub fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "stage".into() } else { out }
}


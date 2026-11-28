//! Change model… and Runs on… (workspace model v3, 5.7 and 17.4): the
//! agent flow with `--agent` set, on rules 3 and rules 4.

use ligus_core::WorkspaceRole;
use uuid::Uuid;

use super::fake::{offer, Fake};
use super::*;

fn run_(ops: &mut Fake, pairs: &[(&str, &str)]) -> Outcome {
    run(FlowId::Agent, ops, &inputs(pairs), &Secrets::new(), &RunOpts::default())
}

fn inputs(pairs: &[(&str, &str)]) -> Inputs {
    let mut i = Inputs::new();
    for (k, v) in pairs {
        i.set(k, *v);
    }
    i
}

fn ids(o: &Outcome) -> Vec<String> {
    o.report.checks.iter().map(|c| format!("{} {} {}", c.id, c.status.as_str(), c.detail)).collect()
}

fn field<'a>(form: &'a Form, id: &str) -> &'a Field {
    form.fields().find(|f| f.id == id).unwrap_or_else(|| panic!("no field {id}"))
}

fn values(f: &Field) -> Vec<(String, bool)> {
    match &f.kind {
        FieldKind::Choice { options } | FieldKind::Suggest { options } => options.iter().map(|o| (o.value.clone(), o.disabled)).collect(),
        _ => Vec::new(),
    }
}

fn agent(name: &str, runtime_id: &str, host_id: &str) -> AgentFact {
    AgentFact { id: Uuid::new_v4(), name: name.into(), runtime_id: runtime_id.into(), host_id: host_id.into(), owner: "me".into(), mine: true, ..Default::default() }
}

fn rt(id: &str, harness: &str, model: &str, endpoint: &str, shared: bool) -> RuntimeFact {
    RuntimeFact { id: id.into(), harness: harness.into(), model: model.into(), endpoint: endpoint.into(), shared, ..Default::default() }
}

/// A shared workspace: Claude Code signed in here, a box offering a gateway
/// with `auto`, and a worker of mine offering Claude Code.
fn world(rules: u32) -> Fake {
    let mut mine = offer("build-1-id", "build-1", "cli", "claude-code", "", Some(true), &["sonnet", "opus"]);
    mine.mine = true;
    Fake {
        surface: Surface::Desktop,
        passkey_writes: true,
        rules,
        host_sources: vec![offer("box-1-id", "box-1", "gateway", "gw-home", "openai", Some(true), &["auto", "qwen3-coder", "glm-4.6"]), mine],
        ..Fake::default()
    }
}

// ── the form ────────────────────────────────────────────────────────────────

#[test]
fn the_form_lists_the_agents_own_sources_models_and_placeable_hosts() {
    let mut f = world(4);
    let mut a = agent("coder", "run:x", "box-1-id");
    (a.source, a.model) = ("gw-home".into(), "qwen3-coder".into());
    f.agents.push(a.clone());
    let form = super::form(FlowId::Agent, &f, &inputs(&[("agent", "coder")]));
    // The gateway's own list where the agent runs, auto first, in words.
    let models = values(field(&form, "model"));
    assert_eq!(models.iter().map(|(v, _)| v.as_str()).collect::<Vec<_>>(), ["auto", "qwen3-coder", "glm-4.6"]);
    if let FieldKind::Suggest { options } = &field(&form, "model").kind {
        assert_eq!(options[0].label, "auto (the gateway picks)");
    }
    assert_eq!(field(&form, "model").default.as_deref(), Some("qwen3-coder"));
    // Runs on: the host that offers it, with its state and one line; this
    // device and hosts that don't offer it are there but can't be picked.
    let hosts = values(field(&form, "runs-on"));
    assert!(hosts.contains(&("box-1-id".into(), false)), "{hosts:?}");
    assert!(hosts.contains(&("here".into(), true)), "{hosts:?}");
    assert!(hosts.contains(&("build-1-id".into(), true)), "{hosts:?}");
    assert_eq!(field(&form, "runs-on").default.as_deref(), Some("box-1-id"));
    assert_eq!(field(&form, "runs-on").note.as_deref(), Some("Turns run on box-1. When it's offline, @coder waits."));
    // A model change within one source on your own agent: no passkey.
    let form = super::form(FlowId::Agent, &f, &inputs(&[("agent", "coder"), ("model", "glm-4.6")]));
    assert_eq!((form.action.as_deref(), form.confirmations), (Some("Change model"), 0));
    // By id too.
    let form = super::form(FlowId::Agent, &f, &inputs(&[("agent", &a.id.to_string())]));
    assert_eq!(field(&form, "model").default.as_deref(), Some("qwen3-coder"));
}

#[test]
fn an_unknown_agent_is_not_found() {
    let mut f = world(4);
    let o = run_(&mut f, &[("agent", "ghost"), ("model", "opus")]);
    assert_eq!(o.exit, exit::NOT_FOUND, "{:?}", ids(&o));
    assert!(f.writes.is_empty());
}

// ── rules 4 ─────────────────────────────────────────────────────────────────

#[test]
fn rules4_a_model_change_within_one_source_is_one_event_and_no_passkey() {
    let mut f = world(4);
    let mut a = agent("coder", "run:x", "box-1-id");
    (a.source, a.model) = ("gw-home".into(), "qwen3-coder".into());
    f.agents.push(a);
    f.role = Some(WorkspaceRole::Contributor);
    let o = run_(&mut f, &[("agent", "@coder"), ("model", "glm-4.6")]);
    assert_eq!(o.exit, exit::OK, "{:?}", ids(&o));
    assert_eq!(f.writes, ["change_agent coder runtime=- run=/gw-home/glm-4.6 host=-"]);
    assert!(f.confirms.is_empty(), "no passkey: {:?}", f.confirms);
    assert!(o.report.checks.iter().any(|c| c.detail == "@coder now uses glm-4.6 on box-1"), "{:?}", ids(&o));
    // Again: nothing to do.
    let o = run_(&mut f, &[("agent", "coder"), ("model", "glm-4.6")]);
    assert_eq!(o.exit, exit::OK);
    assert_eq!(f.writes.len(), 1);
}

#[test]
fn rules4_a_model_a_complete_list_lacks_is_refused() {
    let mut f = world(4);
    let mut a = agent("coder", "run:x", "box-1-id");
    (a.source, a.model) = ("gw-home".into(), "qwen3-coder".into());
    f.agents.push(a);
    let o = run_(&mut f, &[("agent", "coder"), ("model", "gpt-9")]);
    assert_eq!(o.exit, exit::USAGE, "{:?}", ids(&o));
    assert!(f.writes.is_empty());
}

#[test]
fn rules4_an_agent_naming_a_runtime_is_pinned_to_its_source_with_one_passkey() {
    let mut f = world(4);
    f.runtimes.push(rt("claude-code", "claude-code", "", "", false));
    f.agents.push(agent("scout", "claude-code", ""));
    let form = super::form(FlowId::Agent, &f, &inputs(&[("agent", "scout"), ("model", "opus")]));
    assert_eq!(form.confirmations, 1, "pinning is a change of source");
    assert!(field(&form, "model").note.as_deref().unwrap_or("").contains("pins it to"), "{:?}", field(&form, "model").note);
    let o = run_(&mut f, &[("agent", "scout"), ("model", "opus")]);
    assert_eq!(o.exit, exit::OK, "{:?}", ids(&o));
    assert_eq!(f.writes, ["change_agent scout runtime=- run=claudeCode/claude-code/opus host=-"]);
    assert_eq!(f.confirms, ["1 of 1: Change @scout"]);
    // Pinned now: the next model change is free.
    let form = super::form(FlowId::Agent, &f, &inputs(&[("agent", "scout"), ("model", "sonnet")]));
    assert_eq!(form.confirmations, 0);
}

#[test]
fn rules4_moves_follow_the_gates() {
    // Your own agent to your own worker: Contributor, one passkey.
    let mut f = world(4);
    f.role = Some(WorkspaceRole::Contributor);
    let mut a = agent("scout", "run:x", "");
    (a.source, a.harness, a.model) = ("claude-code".into(), "claudeCode".into(), "sonnet".into());
    f.agents.push(a);
    let form = super::form(FlowId::Agent, &f, &inputs(&[("agent", "scout"), ("runs-on", "build-1-id")]));
    assert_eq!((form.action.as_deref(), form.confirmations), (Some("Move @scout"), 1));
    let o = run_(&mut f, &[("agent", "scout"), ("runs-on", "build-1")]);
    assert_eq!(o.exit, exit::OK, "{:?}", ids(&o));
    assert_eq!(f.writes, ["change_agent scout runtime=- run=claudeCode/claude-code/sonnet host=build-1-id"]);
    assert_eq!(f.confirms, ["1 of 1: Change @scout"]);

    // To a host that isn't theirs: an Admin's.
    let mut f = world(4);
    f.role = Some(WorkspaceRole::Contributor);
    f.host_sources.push(offer("box-2-id", "box-2", "cli", "claude-code", "", Some(true), &[]));
    let mut a = agent("scout", "run:x", "");
    (a.source, a.model) = ("claude-code".into(), "sonnet".into());
    f.agents.push(a);
    let o = run_(&mut f, &[("agent", "scout"), ("runs-on", "box-2-id")]);
    assert_eq!(o.exit, exit::REFUSED, "{:?}", ids(&o));
    assert!(f.writes.is_empty());

    // A host that doesn't offer the source: refused before anything.
    let o = run_(&mut f, &[("agent", "scout"), ("runs-on", "box-1-id")]);
    assert_eq!(o.exit, exit::USAGE, "{:?}", ids(&o));
    assert!(f.writes.is_empty());
}

#[test]
fn rules4_a_shared_agent_is_an_admins_for_a_model_change_too() {
    let mut f = world(4);
    f.role = Some(WorkspaceRole::Contributor);
    let mut a = agent("team", "run:x", "box-1-id");
    (a.source, a.model, a.owner, a.mine) = ("gw-home".into(), "qwen3-coder".into(), String::new(), false);
    f.agents.push(a);
    let form = super::form(FlowId::Agent, &f, &inputs(&[("agent", "team")]));
    assert_eq!(field(&form, "model").note.as_deref(), Some("@team is shared: an Admin changes its model."));
    let o = run_(&mut f, &[("agent", "team"), ("model", "glm-4.6")]);
    assert_eq!(o.exit, exit::REFUSED, "{:?}", ids(&o));
    assert!(f.writes.is_empty());
    // An Admin may.
    f.role = Some(WorkspaceRole::Admin);
    let o = run_(&mut f, &[("agent", "team"), ("model", "glm-4.6")]);
    assert_eq!(o.exit, exit::OK, "{:?}", ids(&o));
}

#[test]
fn rules4_a_passkey_the_rules_ask_for_is_a_persons_step() {
    let mut f = world(4);
    f.needs_presence = vec!["change_agent"];
    f.runtimes.push(rt("claude-code", "claude-code", "", "", false));
    f.agents.push(agent("scout", "claude-code", ""));
    let o = run_(&mut f, &[("agent", "scout"), ("model", "opus")]);
    assert_eq!(o.exit, exit::PERSON, "{:?}", ids(&o));
}

// ── rules 3 ─────────────────────────────────────────────────────────────────

#[test]
fn rules3_a_model_change_reuses_or_defines_a_runtime_and_is_an_admins() {
    let mut f = world(3);
    f.runtimes.push(rt("gw-home-auto", "gateway", "auto", "gw-home", true));
    f.agents.push(agent("coder", "gw-home-auto", "box-1-id"));
    let form = super::form(FlowId::Agent, &f, &inputs(&[("agent", "coder")]));
    assert!(field(&form, "model").note.as_deref().unwrap_or("").contains("an Admin's, with a passkey"));
    let form = super::form(FlowId::Agent, &f, &inputs(&[("agent", "coder"), ("model", "glm-4.6")]));
    assert_eq!(form.confirmations, 2, "a workspace runtime, then the agent");
    let o = run_(&mut f, &[("agent", "coder"), ("model", "glm-4.6")]);
    assert_eq!(o.exit, exit::OK, "{:?}", ids(&o));
    assert_eq!(f.writes, ["add_runtime gw-home-glm-4-6", "change_agent coder runtime=gw-home-glm-4-6 run=- host=-"]);
    assert_eq!(f.confirms, ["1 of 2: Define gw-home (OpenAI API) for @coder", "2 of 2: Change @coder"]);
    // Back to auto: the old runtime is reused, one passkey.
    f.confirms.clear();
    let o = run_(&mut f, &[("agent", "coder"), ("model", "auto")]);
    assert_eq!(o.exit, exit::OK, "{:?}", ids(&o));
    assert_eq!(f.writes.last().map(String::as_str), Some("change_agent coder runtime=gw-home-auto run=- host=-"));
    assert_eq!(f.confirms, ["1 of 1: Change @coder"]);
    // A Contributor can't on rules 3.
    f.role = Some(WorkspaceRole::Contributor);
    let o = run_(&mut f, &[("agent", "coder"), ("model", "glm-4.6")]);
    assert_eq!(o.exit, exit::REFUSED, "{:?}", ids(&o));
}

#[test]
fn rules3_a_move_off_this_device_needs_a_workspace_runtime() {
    let mut f = world(3);
    f.runtimes.push(rt("claude-code", "claude-code", "", "", false));
    f.agents.push(agent("scout", "claude-code", ""));
    let o = run_(&mut f, &[("agent", "scout"), ("runs-on", "build-1-id")]);
    assert_eq!(o.exit, exit::OK, "{:?}", ids(&o));
    // build-1 can't read this Mac's own runtime: a workspace one, then the move.
    assert_eq!(f.writes, ["add_runtime claude-code-2", "change_agent scout runtime=claude-code-2 run=- host=build-1-id"]);
    assert!(f.runtimes.iter().any(|r| r.id == "claude-code-2" && r.shared));
}

#[test]
fn a_dry_run_writes_nothing() {
    let mut f = world(4);
    f.runtimes.push(rt("claude-code", "claude-code", "", "", false));
    f.agents.push(agent("scout", "claude-code", ""));
    let o = run(FlowId::Agent, &mut f, &inputs(&[("agent", "scout"), ("model", "opus")]), &Secrets::new(), &RunOpts { dry_run: true, ..Default::default() });
    assert_eq!(o.exit, exit::OK, "{:?}", ids(&o));
    assert!(f.writes.is_empty());
}

#[test]
fn pin_offers_list_runtime_agents_on_rules4_only() {
    let mut f = world(3);
    f.runtimes.push(rt("claude-code-opus", "claude-code", "opus", "", false));
    f.agents.push(agent("scout", "claude-code-opus", ""));
    let mut theirs = agent("theirs", "claude-code-opus", "");
    (theirs.owner, theirs.mine) = ("ben".into(), false);
    f.agents.push(theirs);
    let mut pinned = agent("pinned", "run:x", "");
    (pinned.source, pinned.model) = ("claude-code".into(), "sonnet".into());
    f.agents.push(pinned);
    assert!(pin_offers(&f).is_empty(), "rules 3 keeps runtimes");
    f.rules = 4;
    let p = pin_offers(&f);
    let labels: Vec<String> = p.iter().map(PinOffer::label).collect();
    assert_eq!(labels, ["Pin @scout to Claude Code / opus", "Pin @theirs to Claude Code / opus"], "an Owner sees the others too");
    f.role = Some(WorkspaceRole::Contributor);
    assert_eq!(pin_offers(&f).len(), 1, "a Contributor pins only their own");
    // Pinning is the flow with nothing changed: one record, one passkey.
    let o = run_(&mut f, &[("agent", "scout")]);
    assert_eq!(o.exit, exit::OK, "{:?}", ids(&o));
    assert_eq!(f.writes, ["change_agent scout runtime=- run=claudeCode/claude-code/opus host=-"]);
}

#[test]
fn a_model_name_must_be_a_name() {
    let mut f = world(4);
    let mut a = agent("scout", "run:x", "");
    (a.source, a.model) = ("claude-code".into(), "sonnet".into());
    f.agents.push(a);
    let long = "m".repeat(200);
    for bad in [long.as_str(), "opus\nrm -rf"] {
        let o = run_(&mut f, &[("agent", "scout"), ("model", bad)]);
        assert_eq!(o.exit, exit::USAGE, "{:?}", ids(&o));
    }
    assert!(f.writes.is_empty());
}

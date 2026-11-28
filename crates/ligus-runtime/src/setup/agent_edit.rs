//! Change model… and Runs on… for an existing agent (workspace model v3,
//! 5.7 and 17.4): the agent flow's form with `--agent` set.
//!
//! The source stays the agent's own. The model list is that source's,
//! where the agent runs (a gateway's from the host's beat, with `auto` when
//! it lists it); "Runs on" lists the hosts the person can place it on, each
//! with its state, and one line on what that means.
//!
//! What it writes is one record, one event:
//!
//! - **rules 4**: the agent carries its own run spec `{harness, source,
//!   model}` (an agent still naming a runtime is pinned to the source that
//!   runtime runs). A model change within one source on your own agent needs
//!   no passkey; a move, or a change of source, follows the placement rules
//!   (G1, G2); a shared agent is an Admin's.
//! - **rules 3**: the model belongs to the runtime. The engine reuses a
//!   runtime that runs the new model (or defines one) and points the agent
//!   at it; any change of runtime or host is an Admin's, with a passkey.

use ligus_core::WorkspaceRole;

use super::agent::{
    default_model, host_name, host_word, login_word, model_choices, model_list, needs_model, ops_code, placeable, runs_on_note, runtime_matches, sources,
    spec_for, target, Kind, Source, Target, HERE,
};
use super::{exit, gate, AgentEdit, AgentFact, Choice, Ctx, Field, FieldKind, HostSourceFact, Inputs, RuntimeFact, SetupOps, StepDef};

/// Whether the inputs ask to change an existing agent.
pub(crate) fn editing(inputs: &Inputs) -> bool {
    inputs.get("agent").is_some()
}

/// The agent `v` names: its id, or its name (with or without `@`).
pub(crate) fn find_agent(ops: &dyn SetupOps, v: &str) -> Option<AgentFact> {
    let want = v.trim().trim_start_matches('@');
    let agents = ops.agents();
    agents.iter().find(|a| a.id.to_string() == want).or_else(|| agents.iter().find(|a| a.name.eq_ignore_ascii_case(want))).cloned()
}

/// What an agent runs and where, as the form reads it.
struct Current {
    agent: AgentFact,
    /// Its source in the picker's terms (`None`: not one this device can
    /// tell, e.g. a runtime only another device keeps).
    source: Option<Source>,
    /// Its runtime, when it names one.
    runtime: Option<RuntimeFact>,
    target: Target,
    model: String,
}

/// The picker's source for a runtime: a CLI by its id, a gateway or a
/// hosted API on a host by name, a model server or a key on this device.
fn source_of_runtime<'a>(list: &'a [Source], r: &RuntimeFact) -> Option<&'a Source> {
    let value = match r.harness.as_str() {
        "gateway" => format!("gateway:{}", r.endpoint),
        "local" => "local".to_string(),
        "anthropic" | "openai" | "openrouter" if !r.secret_ref.is_empty() => format!("provider:{}", r.secret_ref),
        "anthropic" | "openai" => format!("key:{}", r.harness),
        h => h.to_string(),
    };
    list.iter().find(|s| s.value == value)
}

/// A source no picker entry stands for (a model server a host declares by
/// name): built from the hosts that offer it.
fn offered(ops: &dyn SetupOps, name: &str) -> Option<Source> {
    let facts: Vec<HostSourceFact> = ops.host_sources().into_iter().filter(|h| h.name == name && (placeable(h) || h.this_device)).collect();
    let first = facts.first()?.clone();
    let kind = match first.kind.as_str() {
        "cli" => Kind::Cli,
        "gateway" => Kind::Gateway,
        // A model server or a hosted API a host declares: its models come
        // from the host's beat, like a gateway's.
        _ => Kind::Provider,
    };
    Some(Source {
        value: name.to_string(),
        label: name.to_string(),
        kind,
        harness: first.harness.clone(),
        provider: first.harness.clone(),
        offer: name.to_string(),
        here: None,
        runs_here: false,
        hosts: facts,
    })
}

/// A host the agent is on that doesn't list its source (it may not have
/// published yet): named from the hosts this device knows.
fn host_stub(ops: &dyn SetupOps, host_id: &str) -> HostSourceFact {
    let any = ops.host_sources().into_iter().find(|h| h.host_id == host_id);
    match any {
        Some(mut h) => {
            h.models.clear();
            h.complete = false;
            h
        }
        None => {
            let label = ops.hosts().into_iter().find(|h| h.id == host_id).map(|h| h.label).unwrap_or_default();
            HostSourceFact { host_id: host_id.to_string(), host_label: label, ..Default::default() }
        }
    }
}

fn current(ops: &dyn SetupOps, list: &[Source], agent: AgentFact) -> Current {
    let runtime = ops.runtimes().into_iter().find(|r| r.id == agent.runtime_id);
    let (source, model) = if !agent.source.is_empty() {
        let s = list.iter().find(|s| s.offer == agent.source && !matches!(s.kind, Kind::Local | Kind::Key)).cloned().or_else(|| offered(ops, &agent.source));
        (s, agent.model.clone())
    } else {
        let s = runtime.as_ref().and_then(|r| source_of_runtime(list, r)).cloned();
        (s, runtime.as_ref().map(|r| r.model.clone()).unwrap_or_default())
    };
    let target = if agent.host_id.is_empty() {
        Target::Here
    } else {
        let h = source.as_ref().and_then(|s| s.hosts.iter().find(|h| h.host_id == agent.host_id).cloned());
        Target::Host(h.unwrap_or_else(|| host_stub(ops, &agent.host_id)))
    };
    Current { agent, source, runtime, target, model }
}

/// The agent the inputs name, read.
fn load(ops: &dyn SetupOps, inputs: &Inputs) -> Option<(Vec<Source>, Current)> {
    let agent = find_agent(ops, inputs.get("agent")?)?;
    let list = sources(ops);
    let cur = current(ops, &list, agent);
    Some((list, cur))
}

/// Where the inputs put it now: the named place, or where it is.
fn new_target(cur: &Current, inputs: &Inputs) -> Result<Target, String> {
    let Some(s) = &cur.source else { return Ok(cur.target.clone()) };
    match inputs.get("runs-on") {
        None => Ok(cur.target.clone()),
        Some(v) if v == cur.target.host_id() || (v == HERE && cur.target == Target::Here) => Ok(cur.target.clone()),
        Some(_) => match target(s, inputs)? {
            Some(t) => Ok(t),
            None => Ok(cur.target.clone()),
        },
    }
}

fn where_of(t: &Target, this: &str) -> String {
    match t {
        Target::Here => format!("{this} (your devices)"),
        Target::Host(h) => host_name(h),
    }
}

/// The rules version, as the flow reads it.
fn on_rules4(ops: &dyn SetupOps) -> bool {
    ops.rules() >= ligus_core::authz::RULES_V4
}

/// Whether the run spec route applies: rules 4 and a source a host offers
/// by name (a CLI, a gateway, a hosted API or a model server on a host).
fn full_route(ops: &dyn SetupOps, s: &Source) -> bool {
    on_rules4(ops) && !s.offer.is_empty() && !matches!(s.kind, Kind::Local | Kind::Key)
}

/// The harness a run spec names for `s`: a CLI's or a hosted API's own; a
/// gateway's comes from its wire on the host, never from here.
fn run_harness(s: &Source, cur: &Current) -> String {
    if !cur.agent.harness.is_empty() && cur.agent.source == s.offer {
        return cur.agent.harness.clone();
    }
    match s.kind {
        Kind::Gateway => String::new(),
        _ => s.provider.clone(),
    }
}

/// What a change would write.
enum Write {
    /// Nothing changes.
    Nothing,
    /// Rules 4: the run spec, and the host when it moves.
    Run { harness: String, source: String, model: String, host: Option<String>, presence: bool },
    /// A runtime (reused, or defined first) and the host when it moves.
    Runtime { spec: super::RuntimeSpec, reuse: Option<String>, change: bool, host: Option<String> },
}

fn plan(ops: &dyn SetupOps, cur: &Current, s: &Source, t: &Target, model: &str) -> Write {
    let moved = t.host_id() != cur.agent.host_id;
    let host = moved.then(|| t.host_id().to_string());
    if full_route(ops, s) {
        let pinned = cur.agent.source == s.offer;
        if pinned && !moved && model == cur.model {
            return Write::Nothing;
        }
        // A model change within one source is no move (5.7); pinning a
        // runtime to its source, or a move, is.
        return Write::Run { harness: run_harness(s, cur), source: s.offer.clone(), model: model.to_string(), host, presence: moved || !pinned };
    }
    let endpoint = cur.runtime.as_ref().map(|r| r.endpoint.clone()).unwrap_or_default();
    let runtimes = ops.runtimes();
    let taken: Vec<&str> = runtimes.iter().map(|r| r.id.as_str()).collect();
    let spec = spec_for(s, t, model, &endpoint, &taken);
    let keeps = cur.runtime.as_ref().is_some_and(|r| runtime_matches(r, &spec)) && cur.agent.source.is_empty();
    if keeps && !moved {
        return Write::Nothing;
    }
    let reuse = if keeps { Some(cur.agent.runtime_id.clone()) } else { runtimes.iter().find(|r| runtime_matches(r, &spec)).map(|r| r.id.clone()) };
    Write::Runtime { spec, reuse, change: !keeps, host }
}

/// How many passkey confirmations `w` asks for.
fn confirmations(ops: &dyn SetupOps, w: &Write) -> usize {
    if !ops.passkey_writes() {
        return 0;
    }
    match w {
        Write::Nothing => 0,
        Write::Run { presence, .. } => usize::from(*presence),
        Write::Runtime { reuse, spec, .. } => 1 + usize::from(reuse.is_none() && ops.runtime_on_log(spec.shared)),
    }
}

/// Who may make the change, before anything is written (the fold decides
/// again): `Some((role, what))` when it needs more than a Contributor.
fn needed_role(ops: &dyn SetupOps, cur: &Current, w: &Write, t: &Target) -> Option<(WorkspaceRole, &'static str)> {
    if matches!(w, Write::Nothing) {
        return None;
    }
    if !on_rules4(ops) {
        return Some((WorkspaceRole::Admin, "changing an agent's model or where it runs on this workspace's rules"));
    }
    if cur.agent.owner.is_empty() {
        return Some((WorkspaceRole::Admin, "changing a shared agent"));
    }
    if !cur.agent.mine {
        return Some((WorkspaceRole::Admin, "changing someone else's agent"));
    }
    let moved = t.host_id() != cur.agent.host_id;
    if let (true, Target::Host(h)) = (moved, t) {
        if !h.mine && !h.this_device {
            return Some((WorkspaceRole::Admin, "placing an agent on a host you don't own"));
        }
    }
    None
}

/// The line under Model: what a change asks of you here.
fn model_note(ops: &dyn SetupOps, cur: &Current, s: Option<&Source>) -> String {
    let name = &cur.agent.name;
    if !on_rules4(ops) {
        return "On this workspace an agent's model belongs to its runtime: changing it is an Admin's, with a passkey.".into();
    }
    if cur.agent.owner.is_empty() {
        return format!("@{name} is shared: an Admin changes its model.");
    }
    if !cur.agent.mine {
        return format!("@{name} is someone else's: an Admin changes its model.");
    }
    match s {
        Some(s) if full_route(ops, s) && cur.agent.source != s.offer => {
            format!("@{name} still names a runtime. Choosing here pins it to {} itself, once, with your passkey.", s.label)
        }
        _ => String::new(),
    }
}

pub(crate) fn steps(ops: &dyn SetupOps, inputs: &Inputs) -> Vec<StepDef> {
    let this = ops.this_device_label();
    let agent_field = Field::new("agent", "Agent", FieldKind::Text).required().help("The agent to change: its name or id.");
    let Some((_, cur)) = load(ops, inputs) else {
        return vec![
            StepDef::input("agent", "Agent", vec![agent_field]),
            StepDef::input("model", "Model", vec![Field::new("model", "Model", FieldKind::Text)]),
            StepDef::input("runs-on", "Runs on", vec![Field::new("runs-on", "Runs on", FieldKind::Text)]),
        ];
    };
    let name = cur.agent.name.clone();
    let agent_field = agent_field.default(cur.agent.id.to_string());
    let t = new_target(&cur, inputs).unwrap_or_else(|_| cur.target.clone());
    let (model_field, host_field) = match &cur.source {
        None => (
            Field::new("model", "Model", FieldKind::Text).default(cur.model.clone()).note(format!("This device can't tell what @{name} runs, so it can't list models.")),
            Field::new("runs-on", "Runs on", FieldKind::Text).default(match &cur.target {
                Target::Here => HERE.to_string(),
                Target::Host(h) => h.host_id.clone(),
            }),
        ),
        Some(s) => {
            let endpoint = cur.runtime.as_ref().map(|r| r.endpoint.clone()).unwrap_or_default();
            let list = model_list(ops, s, Some(&t), &endpoint);
            let mut options = model_choices(&list);
            if !cur.model.is_empty() && !options.iter().any(|o| o.value == cur.model) {
                options.insert(0, Choice::new(cur.model.clone(), cur.model.clone()).hint("now"));
            }
            let model_default = if cur.model.is_empty() && needs_model(s) { default_model(s, &list) } else { cur.model.clone() };
            let model = Field::new("model", "Model", FieldKind::Suggest { options })
                .required_if(needs_model(s))
                .default(model_default)
                .note(model_note(ops, &cur, Some(s)))
                .help("The model this agent uses, from what its source lists where it runs. For a coding CLI, empty is the CLI's own default.");
            let mut hosts = Vec::new();
            if s.runs_here {
                let h = match s.here.as_ref() {
                    Some(f) => format!("{this} · {}", login_word(f)),
                    None => this.clone(),
                };
                hosts.push(Choice::new(HERE, "Your devices").hint(h));
            }
            for h in &s.hosts {
                hosts.push(Choice::new(h.host_id.clone(), host_name(h)).hint(host_word(h, s.kind == Kind::Cli)));
            }
            // Where it is now stays listed, even when that host stopped
            // offering the source.
            if let Target::Host(h) = &cur.target {
                if !hosts.iter().any(|c| c.value == h.host_id) {
                    hosts.push(Choice::new(h.host_id.clone(), host_name(h)).hint(format!("now; doesn't list {}", s.label)));
                }
            }
            if !s.runs_here {
                hosts.push(Choice::new(HERE, "Your devices").hint(format!("{} runs only on a host that offers it", s.label)).disabled(true));
            }
            let mut seen: Vec<String> = hosts.iter().map(|c| c.value.clone()).collect();
            for h in ops.host_sources().into_iter().filter(placeable) {
                if !seen.contains(&h.host_id) {
                    seen.push(h.host_id.clone());
                    hosts.push(Choice::new(h.host_id.clone(), host_name(&h)).hint(format!("doesn't offer {}", s.label)).disabled(true));
                }
            }
            let mut note = runs_on_note(Some(s), Some(&t), &name);
            if t.host_id() != cur.agent.host_id {
                let w = plan(ops, &cur, s, &t, inputs.get("model").unwrap_or(&cur.model));
                if let Some((role, _)) = needed_role(ops, &cur, &w, &t) {
                    if role == WorkspaceRole::Admin {
                        note = format!("{note} Moving it is an Admin's.");
                    }
                }
            }
            let runs_on = Field::new("runs-on", "Runs on", FieldKind::Choice { options: hosts })
                .required()
                .default(match &cur.target {
                    Target::Here => HERE.to_string(),
                    Target::Host(h) => h.host_id.clone(),
                })
                .note(note.trim().to_string())
                .help("Where its turns run: your own devices for a CLI signed in on them, or a host that offers its source. When that host is offline, turns wait for it; nothing runs anywhere else.");
            (model, runs_on)
        }
    };
    vec![
        StepDef::input("agent", "Agent", vec![agent_field]),
        StepDef::input("model", "Model", vec![model_field]),
        StepDef::input("runs-on", "Runs on", vec![host_field]),
        StepDef::verify("verify", "Saved"),
    ]
}

/// The one button's label and its confirmations.
pub(crate) fn action(ops: &dyn SetupOps, inputs: &Inputs) -> (Option<String>, usize) {
    let Some((_, cur)) = load(ops, inputs) else { return (Some("Save".into()), 0) };
    let Some(s) = cur.source.clone() else { return (Some("Save".into()), 0) };
    let Ok(t) = new_target(&cur, inputs) else { return (Some(format!("Move @{}", cur.agent.name)), 0) };
    let model = inputs.get("model").unwrap_or(&cur.model).to_string();
    let w = plan(ops, &cur, &s, &t, &model);
    let label = if t.host_id() != cur.agent.host_id {
        format!("Move @{}", cur.agent.name)
    } else if model != cur.model || matches!(w, Write::Run { presence: true, .. }) {
        "Change model".to_string()
    } else {
        "Save".to_string()
    };
    (Some(label), confirmations(ops, &w))
}

pub(crate) fn run(ctx: &mut Ctx, ops: &mut dyn SetupOps, inputs: &Inputs) {
    let this = ops.this_device_label();
    let want = inputs.get("agent").unwrap_or("").trim().to_string();
    let Some((_, cur)) = load(ops, inputs) else {
        return ctx.fail("input.agent", "agent", format!("no agent {want} in this workspace"), "ligus agents lists them", exit::NOT_FOUND);
    };
    let name = cur.agent.name.clone();
    let Some(s) = cur.source.clone() else {
        return ctx.fail(
            "source",
            "what answers",
            format!("this device can't tell what @{name} runs (runtime '{}')", cur.agent.runtime_id),
            "change it on the device that set it up, or add it again with ligus setup agent",
            exit::FIXABLE,
        );
    };
    ctx.ok("agent", "agent", format!("@{name} · {}", s.label));
    let t = match new_target(&cur, inputs) {
        Ok(t) => t,
        Err(why) => return ctx.fail("input.runs-on", "runs on", why, "pick a host that offers it (ligus hosts lists what each offers)", exit::USAGE),
    };
    if t == Target::Here && !s.runs_here && !cur.target.host_id().is_empty() {
        return ctx.fail("input.runs-on", "runs on", format!("{} runs only on a host that offers it", s.label), "pass --runs-on with a host that offers it", exit::USAGE);
    }
    let model = inputs.get("model").map(|m| m.trim().to_string()).unwrap_or_else(|| cur.model.clone());
    if model.chars().count() > ligus_core::workspace_host::MAX_MODEL_NAME || model.chars().any(char::is_control) {
        return ctx.fail("input.model", "model", "that isn't a model name", format!("pass --model with at most {} characters", ligus_core::workspace_host::MAX_MODEL_NAME), exit::USAGE);
    }
    let endpoint = cur.runtime.as_ref().map(|r| r.endpoint.clone()).unwrap_or_default();
    let models = model_list(ops, &s, Some(&t), &endpoint);
    if model.is_empty() && needs_model(&s) {
        let some: Vec<&str> = models.models.iter().take(6).map(|m| m.id.as_str()).collect();
        let fix = if some.is_empty() { "pass --model <name>".to_string() } else { format!("pass --model with one of: {}", some.join(", ")) };
        return ctx.fail("input.model", "model", format!("{} needs a model name", s.label), fix, exit::USAGE);
    }
    if !model.is_empty() && model != cur.model && models.complete && !models.models.iter().any(|m| m.id == model) {
        let some: Vec<&str> = models.models.iter().take(6).map(|m| m.id.as_str()).collect();
        return ctx.fail("input.model", "model", format!("'{model}' isn't available where @{name} runs"), format!("pick one of: {}", some.join(", ")), exit::USAGE);
    }
    let w = plan(ops, &cur, &s, &t, &model);
    if let Some((role, what)) = needed_role(ops, &cur, &w, &t) {
        if !gate::need(ctx, ops, role, what) {
            return;
        }
    }
    let model_word = if model.is_empty() { "the CLI's own default".to_string() } else { model.clone() };
    let where_ = where_of(&t, &this);
    let total = confirmations(ops, &w);
    if total > 0 {
        ctx.result("confirmations", total.to_string());
    }
    ctx.result("agentId", cur.agent.id.to_string());
    let edit = match w {
        Write::Nothing => {
            ctx.ok("change", "change", format!("@{name} already uses {model_word} on {where_}"));
            return ctx.done();
        }
        Write::Run { harness, source, model, host, .. } => AgentEdit { runtime_id: None, run: Some((harness, source, model)), host_id: host },
        Write::Runtime { spec, reuse, change, host } => {
            let on_log = reuse.is_none() && ops.runtime_on_log(spec.shared);
            if reuse.is_none() && (on_log || ops.runtimes_need_admin()) && !gate::need(ctx, ops, WorkspaceRole::Admin, "defining a workspace runtime") {
                return;
            }
            let id = match reuse {
                Some(id) => {
                    if change {
                        ctx.ok("runtime", "runtime", format!("reusing '{id}'"));
                    }
                    id
                }
                None if ctx.dry_run() => {
                    ctx.skip("runtime", "runtime", format!("would define runtime '{}'", spec.id));
                    spec.id.clone()
                }
                None => {
                    if on_log && total > 1 {
                        ops.confirming(1, total, &format!("Define {} for @{name}", s.label));
                    }
                    if let Err(e) = ops.add_runtime(&spec) {
                        return ctx.fail("runtime", "runtime", format!("couldn't define runtime '{}': {e}", spec.id), "fix what it says and run this again", ops_code(&e));
                    }
                    let rid = spec.id.clone();
                    ctx.undo(format!("runtime '{rid}'"), Box::new(move |o| o.remove_runtime(&rid)));
                    ctx.ok("runtime", "runtime", format!("defined '{}'", spec.id));
                    spec.id.clone()
                }
            };
            AgentEdit { runtime_id: change.then_some(id), run: None, host_id: host }
        }
    };
    if ctx.cancelled() {
        return;
    }
    if ctx.dry_run() {
        ctx.skip("change", "change", format!("would set @{name} to {model_word} on {where_}"));
        return ctx.done();
    }
    if total > 0 {
        ops.confirming(total, total, &format!("Change @{name}"));
    }
    match ops.change_agent(cur.agent.id, &edit) {
        Ok(()) => {
            ctx.ok("change", "change", format!("@{name} now uses {model_word} on {where_}"));
            ctx.commit();
            ctx.done();
        }
        Err(e) => ctx.fail("change", "change", format!("couldn't change @{name}: {e}"), "fix what it says and run this again", ops_code(&e)),
    }
}

/// An agent that still names a runtime on a rules-4 workspace, and the
/// source it would be pinned to (M13: "Pin @coder to Claude Code / opus").
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PinOffer {
    pub agent_id: uuid::Uuid,
    pub name: String,
    /// The source in words ("Claude Code", "gw-home (OpenAI API)").
    pub source: String,
    /// Its model now ("" = the CLI's own default).
    pub model: String,
    /// This device's person owns it (else it is listed for an Admin).
    pub mine: bool,
}

impl PinOffer {
    /// "Pin @coder to Claude Code / opus".
    pub fn label(&self) -> String {
        if self.model.is_empty() {
            format!("Pin @{} to {}", self.name, self.source)
        } else {
            format!("Pin @{} to {} / {}", self.name, self.source, self.model)
        }
    }
}

/// The agents a person here can pin to the source their runtime runs: on
/// rules 4 only, their own, and every other one for an Admin. Pinning is
/// the agent flow with `--agent` and nothing else changed (one passkey per
/// agent: it changes what the agent names).
pub fn pin_offers(ops: &dyn SetupOps) -> Vec<PinOffer> {
    if !on_rules4(ops) {
        return Vec::new();
    }
    let admin = ops.my_role().is_some_and(|r| r.rank() >= WorkspaceRole::Admin.rank());
    let list = sources(ops);
    ops.agents()
        .into_iter()
        .filter(|a| a.source.is_empty() && (a.mine || admin))
        .filter_map(|a| {
            let cur = current(ops, &list, a);
            let s = cur.source.as_ref().filter(|s| full_route(ops, s))?;
            Some(PinOffer { agent_id: cur.agent.id, name: cur.agent.name.clone(), source: s.label.clone(), model: cur.model.clone(), mine: cur.agent.mine })
        })
        .collect()
}

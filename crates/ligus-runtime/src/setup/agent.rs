//! Set up an agent in one step (workspace model v3, 17.4): name → what
//! answers → model → runs on → (agy: may it run local servers) → optional
//! test turn, then one button.
//!
//! "What answers" is one list built from this device's coding CLIs and the
//! offers of every host the person can place an agent on (boxes, workers
//! and their own other devices), each with its state from the host's beat:
//! a coding CLI ("Claude Code · signed in · this Mac"), a model gateway a
//! host offers, a hosted API whose key a host holds by secret reference, a
//! model server this device reaches, or an API key kept on this device.
//!
//! The model is per agent here. Underneath, on a rules-3 workspace the
//! agent still names a runtime (5.9): the engine reuses one with the same
//! harness, model and endpoint, gateway or secret reference, or defines one
//! in the same run. A runtime another host must run is a workspace runtime
//! (an event that asks for the passkey too), so that case says "Confirm 1
//! of 2"; everything else is one confirmation. The agent itself is one
//! record carrying its name, runtime and host (one event).

use ligus_core::WorkspaceRole;

use super::validate::{agent_name_problem, url_problem, value};
use super::{
    exit, gate, Choice, Ctx, Field, FieldKind, HarnessFact, HostSourceFact, Inputs, Progress, RuntimeFact, RuntimeSpec, Secrets, SetupOps,
    StepDef,
};
use crate::cli_detect::LoginState;
use crate::model_catalog::{ModelList, ModelOption, ModelSource};

/// The coding CLIs in the order the picker shows them (their ids are also
/// the offer names hosts list).
pub const HARNESSES: &[(&str, &str)] = &[
    ("claude-code", "Claude Code"),
    ("codex", "Codex"),
    ("copilot", "GitHub Copilot CLI"),
    ("antigravity", "Google Antigravity CLI"),
    ("aider", "Aider"),
    ("pi", "Pi"),
];

/// The default model server address on this device (Ollama's).
pub const LOCAL_DEFAULT: &str = "http://localhost:11434";
/// The default OpenAI-compatible API base for a key on this device.
const OPENAI_DEFAULT: &str = "https://api.openai.com/v1";
/// `--runs-on here`: this device (the agent's owner's own devices).
pub const HERE: &str = "here";

/// What kind of thing answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A coding CLI, on this device or a host.
    Cli,
    /// A model server this device reaches at a URL (Ollama by default).
    Local,
    /// A hosted API with a key kept on this device.
    Key,
    /// A model gateway a host offers.
    Gateway,
    /// A hosted API whose key a host holds (by secret reference).
    Provider,
}

/// One entry of "What answers".
#[derive(Debug, Clone)]
pub(crate) struct Source {
    /// The choice's value (`--source`): a CLI id, `local`, `key:<api>`,
    /// `gateway:<name>`, `provider:<name>`.
    pub value: String,
    pub label: String,
    pub kind: Kind,
    /// The engine's harness id for its runtime (a CLI id, `local`,
    /// `gateway`, `anthropic`, `openai`, `openrouter`).
    pub harness: String,
    /// The provider kind's wire name for a new runtime.
    pub provider: String,
    /// The offer's name on hosts (a CLI id, a gateway or provider name).
    pub offer: String,
    /// This device's facts about the CLI (installed, signed in).
    pub here: Option<HarnessFact>,
    /// It can run on this device.
    pub runs_here: bool,
    /// The other hosts that list it, that the person can place an agent on.
    pub hosts: Vec<HostSourceFact>,
}

/// Where a run goes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Target {
    Here,
    Host(HostSourceFact),
}

impl Target {
    fn host_id(&self) -> &str {
        match self {
            Target::Here => "",
            Target::Host(h) => &h.host_id,
        }
    }
}

fn wire_label(wire: &str) -> &'static str {
    match wire {
        "anthropic" => "Anthropic API",
        _ => "OpenAI API",
    }
}

/// The engine's harness id and label of a hosted API's provider wire name.
fn api_of(provider: &str) -> (&'static str, &'static str) {
    match provider.to_ascii_lowercase().as_str() {
        "anthropic" => ("anthropic", "Anthropic API"),
        "openrouter" => ("openrouter", "OpenRouter"),
        _ => ("openai", "OpenAI-compatible API"),
    }
}

/// Hosts the person can place an agent on: boxes and workers, and their
/// own other devices (an agent on someone else's computer runs only while
/// they're at it, so those are left out). This device is "here".
fn placeable(h: &HostSourceFact) -> bool {
    !h.this_device && (h.host_kind == "worker" || h.mine)
}

/// Every source, in the picker's order: coding CLIs, gateways, hosted APIs
/// on hosts, a model server, an API key on this device.
pub(crate) fn sources(ops: &dyn SetupOps) -> Vec<Source> {
    let facts = ops.harnesses();
    let hosts: Vec<HostSourceFact> = ops.host_sources().into_iter().filter(placeable).collect();
    let mut out = Vec::new();
    for (id, label) in HARNESSES {
        let here = facts.iter().find(|f| f.id == *id).cloned();
        let provider = super::service::provider_of(id).map(crate::host_offers::harness_name).unwrap_or_default();
        out.push(Source {
            value: (*id).into(),
            label: (*label).into(),
            kind: Kind::Cli,
            harness: (*id).into(),
            provider,
            offer: (*id).into(),
            runs_here: here.as_ref().is_some_and(|f| f.installed),
            here,
            hosts: hosts.iter().filter(|h| h.kind == "cli" && h.name == *id).cloned().collect(),
        });
    }
    let named = |kind: &str| {
        let mut names: Vec<HostSourceFact> = Vec::new();
        for h in hosts.iter().filter(|h| h.kind == kind) {
            if !names.iter().any(|n| n.name == h.name) {
                names.push(h.clone());
            }
        }
        names
    };
    let offering = |kind: &str, name: &str| hosts.iter().filter(|h| h.kind == kind && h.name == name).cloned().collect::<Vec<_>>();
    for g in named("gateway") {
        out.push(Source {
            value: format!("gateway:{}", g.name),
            label: format!("{} ({})", g.name, wire_label(&g.wire)),
            kind: Kind::Gateway,
            harness: "gateway".into(),
            // The gateway's wire decides the client, never a member (H6).
            provider: if g.wire == "anthropic" { "anthropic".into() } else { "custom".into() },
            offer: g.name.clone(),
            here: None,
            runs_here: false,
            hosts: offering("gateway", &g.name),
        });
    }
    for p in named("provider") {
        let (harness, api) = api_of(&p.harness);
        out.push(Source {
            value: format!("provider:{}", p.name),
            label: format!("{} ({api})", p.name),
            kind: Kind::Provider,
            harness: harness.into(),
            provider: p.harness.clone(),
            offer: p.name.clone(),
            here: None,
            runs_here: false,
            hosts: offering("provider", &p.name),
        });
    }
    out.push(Source {
        value: "local".into(),
        label: "A model server (Ollama or OpenAI-compatible)".into(),
        kind: Kind::Local,
        harness: "local".into(),
        provider: "ollama".into(),
        offer: String::new(),
        here: None,
        runs_here: true,
        hosts: Vec::new(),
    });
    if ops.keeps_model_keys() {
        for (api, label, provider) in [("anthropic", "Anthropic API key", "anthropic"), ("openai", "OpenAI-compatible API key", "openAI")] {
            out.push(Source {
                value: format!("key:{api}"),
                label: label.into(),
                kind: Kind::Key,
                harness: api.into(),
                provider: provider.into(),
                offer: String::new(),
                here: None,
                runs_here: true,
                hosts: Vec::new(),
            });
        }
    }
    out
}

fn login_word(f: &HarnessFact) -> &'static str {
    match f.login {
        LoginState::LoggedIn => "signed in",
        LoginState::NotLoggedIn => "not signed in",
        LoginState::Unknown => "installed",
    }
}

/// A host's state for one offer, in a word or two.
fn host_word(h: &HostSourceFact, cli: bool) -> String {
    let w = match h.ready {
        Some(true) if cli => "signed in".to_string(),
        Some(true) => "ready".to_string(),
        Some(false) if !h.why.is_empty() => h.why.clone(),
        Some(false) => "not ready".to_string(),
        None => "listed".to_string(),
    };
    if h.online {
        w
    } else {
        format!("{w}, offline")
    }
}

fn host_name(h: &HostSourceFact) -> String {
    if h.host_label.trim().is_empty() {
        h.host_id.chars().take(8).collect()
    } else {
        h.host_label.clone()
    }
}

fn names(hosts: &[HostSourceFact]) -> String {
    hosts.iter().map(host_name).collect::<Vec<_>>().join(", ")
}

/// The one-line state next to a source ("signed in · this Mac · also
/// build-1", "offered by box-1").
fn hint(s: &Source, this: &str) -> String {
    match s.kind {
        Kind::Cli => {
            let here = s.here.as_ref().filter(|f| f.installed);
            match (here, s.hosts.split_first()) {
                (Some(f), _) if s.hosts.is_empty() => format!("{} · {this}", login_word(f)),
                (Some(f), _) => format!("{} · {this} · also {}", login_word(f), names(&s.hosts)),
                (None, Some((h, []))) => format!("{} · {}", host_word(h, true), host_name(h)),
                (None, Some((h, rest))) => format!("{} · {} · also {}", host_word(h, true), host_name(h), names(rest)),
                (None, None) => "not installed".into(),
            }
        }
        Kind::Gateway => format!("offered by {}", names(&s.hosts)),
        Kind::Provider => format!("key by secret reference on {}", names(&s.hosts)),
        Kind::Local => this.to_string(),
        Kind::Key => format!("the key stays on {this}"),
    }
}

fn usable_here(s: &Source) -> bool {
    match s.kind {
        Kind::Cli => s.here.as_ref().is_some_and(|f| f.installed && f.login != LoginState::NotLoggedIn),
        _ => s.runs_here,
    }
}

/// The best source to start from: a CLI signed in here, then one ready on a
/// host, then a gateway or a hosted API a host offers, then a CLI installed
/// here.
fn default_source(list: &[Source]) -> Option<&Source> {
    let cli = |s: &&Source| s.kind == Kind::Cli;
    list.iter()
        .filter(cli)
        .find(|s| s.here.as_ref().is_some_and(|f| f.installed && f.login == LoginState::LoggedIn))
        .or_else(|| list.iter().filter(cli).find(|s| s.hosts.iter().any(|h| h.ready == Some(true))))
        .or_else(|| list.iter().find(|s| matches!(s.kind, Kind::Gateway | Kind::Provider)))
        .or_else(|| list.iter().filter(cli).find(|s| usable_here(s)))
}

/// Where a source runs by default: a CLI on the device where it's signed
/// in (this device first); a gateway or a hosted API on the only host that
/// offers it (or the only one online); a model server or a key here. `None`:
/// the person picks.
pub(crate) fn default_target(s: &Source) -> Option<Target> {
    match s.kind {
        Kind::Local | Kind::Key => Some(Target::Here),
        Kind::Cli => {
            if usable_here(s) {
                return Some(Target::Here);
            }
            let ready: Vec<&HostSourceFact> = s.hosts.iter().filter(|h| h.ready != Some(false)).collect();
            match ready.as_slice() {
                [h] => Some(Target::Host((*h).clone())),
                // Installed here but not signed in, or nowhere at all: the
                // run says what to do on this device.
                [] if s.runs_here || s.hosts.is_empty() => Some(Target::Here),
                _ => None,
            }
        }
        Kind::Gateway | Kind::Provider => {
            if let [h] = s.hosts.as_slice() {
                return Some(Target::Host(h.clone()));
            }
            let online: Vec<&HostSourceFact> = s.hosts.iter().filter(|h| h.online).collect();
            match online.as_slice() {
                [h] => Some(Target::Host((*h).clone())),
                _ => None,
            }
        }
    }
}

/// The source the inputs name, or the default.
fn chosen<'a>(list: &'a [Source], inputs: &Inputs) -> Option<&'a Source> {
    match inputs.get("source") {
        Some(v) => list.iter().find(|s| s.value == v),
        None => default_source(list),
    }
}

/// Where the inputs say to run `s`, or its default. `Err` says why the
/// named place can't run it ("build-1 doesn't offer gw-home").
fn target(s: &Source, inputs: &Inputs) -> Result<Option<Target>, String> {
    match inputs.get("runs-on") {
        None => Ok(default_target(s)),
        Some(HERE) if s.runs_here => Ok(Some(Target::Here)),
        Some(HERE) => Err(format!("{} doesn't run on this device", s.label)),
        Some(v) => match s.hosts.iter().find(|h| h.host_id == v) {
            Some(h) => Ok(Some(Target::Host(h.clone()))),
            None => Err(format!("{v} doesn't offer {}", s.label)),
        },
    }
}

fn endpoint_default(s: Option<&Source>) -> &'static str {
    match s.map(|s| (s.kind, s.harness.as_str())) {
        Some((Kind::Key, "openai")) => OPENAI_DEFAULT,
        _ => LOCAL_DEFAULT,
    }
}

/// The models `s` offers where it would run.
fn model_list(ops: &dyn SetupOps, s: &Source, t: Option<&Target>, endpoint: &str) -> ModelList {
    let from_hosts = |hosts: Vec<&HostSourceFact>| -> ModelList {
        let mut models: Vec<String> = Vec::new();
        for h in &hosts {
            for m in &h.models {
                if !models.contains(m) {
                    models.push(m.clone());
                }
            }
        }
        // Only one host's own complete list says what won't work.
        let complete = hosts.len() == 1 && hosts[0].complete;
        ModelList { models: models.into_iter().map(|m| ModelOption::new(m, None)).collect(), source: ModelSource::Catalog, complete, note: None, error: None }
    };
    match (s.kind, t) {
        (Kind::Cli, Some(Target::Host(h))) if !h.models.is_empty() => from_hosts(vec![h]),
        (Kind::Cli, _) => ops.models(&s.harness, ""),
        (Kind::Local, _) => ops.models("local", endpoint),
        (Kind::Key, _) => {
            let mut l = ops.models(&s.harness, endpoint);
            if l.models.is_empty() && s.harness == "anthropic" {
                l.models = ["claude-sonnet-4-6", "claude-opus-4-8", "claude-haiku-4-5"].iter().map(|m| ModelOption::new(*m, None)).collect();
            }
            l
        }
        (Kind::Gateway | Kind::Provider, Some(Target::Host(h))) => from_hosts(vec![h]),
        (Kind::Gateway | Kind::Provider, _) => from_hosts(s.hosts.iter().collect()),
    }
}

/// The model to start from: a CLI's own default (empty); otherwise the
/// first one the source lists.
fn default_model(s: &Source, list: &ModelList) -> String {
    match s.kind {
        Kind::Cli => String::new(),
        Kind::Key if s.harness != "anthropic" => String::new(),
        _ => list.models.first().map(|m| m.id.clone()).unwrap_or_default(),
    }
}

/// Everything but a coding CLI (which has its own default) needs a model.
fn needs_model(s: &Source) -> bool {
    s.kind != Kind::Cli
}

/// The line under "Runs on".
fn runs_on_note(s: Option<&Source>, t: Option<&Target>, name: &str) -> String {
    let who = if name.is_empty() { "the agent".to_string() } else { format!("@{name}") };
    match (s, t) {
        (_, Some(Target::Here)) => "Turns run on this device while Ligus is open.".into(),
        (_, Some(Target::Host(h))) => format!("Turns run on {}. When it's offline, {who} waits.", host_name(h)),
        (Some(s), None) if !s.hosts.is_empty() => format!("{} hosts offer {}: pick where turns run.", s.hosts.len(), s.label),
        _ => String::new(),
    }
}

/// Map older spellings and a host's label to today's inputs: `--harness X`
/// is `--source X`, `--harness gateway --gateway G` is `--source
/// gateway:G`, and `--runs-on build-1` (a label) is that host's id.
pub(crate) fn normalize(ops: &dyn SetupOps, inputs: &Inputs) -> Inputs {
    let mut i = inputs.clone();
    let harness = i.0.remove("harness").unwrap_or_default();
    let gateway = i.0.remove("gateway").unwrap_or_default();
    let (harness, gateway) = (harness.trim(), gateway.trim());
    if i.get("source").is_none() {
        if !gateway.is_empty() && (harness.is_empty() || harness == "gateway") {
            i.set("source", format!("gateway:{gateway}"));
        } else if !harness.is_empty() {
            i.set("source", harness);
        }
    }
    if let Some(v) = i.get("runs-on").map(str::to_string) {
        if v != HERE {
            let hosts = ops.host_sources();
            if !hosts.iter().any(|h| h.host_id == v) {
                if let Some(h) = hosts.iter().find(|h| h.host_label.eq_ignore_ascii_case(&v)) {
                    i.set("runs-on", h.host_id.clone());
                }
            }
        }
    }
    i
}

pub(crate) fn status(ops: &dyn SetupOps) -> (Progress, String) {
    let n = ops.agents().len();
    match n {
        0 => (Progress::Todo, "no agents yet".into()),
        1 => (Progress::Done, "1 agent".into()),
        n => (Progress::Done, format!("{n} agents")),
    }
}

pub(crate) fn steps(ops: &dyn SetupOps, inputs: &Inputs) -> Vec<StepDef> {
    let this = ops.this_device_label();
    let list = sources(ops);
    let source = chosen(&list, inputs);
    let options = list.iter().map(|s| Choice::new(s.value.clone(), s.label.clone()).hint(hint(s, &this))).collect();
    let t = source.and_then(|s| target(s, inputs).ok().flatten());
    let ep_default = endpoint_default(source);
    let endpoint = inputs.get("endpoint").unwrap_or(ep_default);
    let models = source.map(|s| model_list(ops, s, t.as_ref(), endpoint));
    let model_options: Vec<Choice> = models
        .as_ref()
        .map(|l| l.models.iter().map(|m| Choice::new(m.id.clone(), m.label.clone().unwrap_or_else(|| m.id.clone()))).collect())
        .unwrap_or_default();
    let model_default = match (source, &models) {
        (Some(s), Some(l)) => default_model(s, l),
        _ => String::new(),
    };
    let host_options: Vec<Choice> = match source {
        None => Vec::new(),
        Some(s) => {
            let mut o = Vec::new();
            if s.runs_here {
                let h = match s.here.as_ref() {
                    Some(f) => format!("{this} · {}", login_word(f)),
                    None => this.clone(),
                };
                o.push(Choice::new(HERE, "This device").hint(h));
            }
            for h in &s.hosts {
                o.push(Choice::new(h.host_id.clone(), host_name(h)).hint(host_word(h, s.kind == Kind::Cli)));
            }
            // The rest, so a pick of one says why it can't (surfaces hide
            // these): this device for a host-only source, and hosts that
            // don't offer it ("box-2: doesn't offer gw-home").
            if !s.runs_here {
                o.push(Choice::new(HERE, "This device").hint(format!("{} runs only on a host that offers it", s.label)).disabled(true));
            }
            let mut seen: Vec<String> = s.hosts.iter().map(|h| h.host_id.clone()).collect();
            for h in ops.host_sources().into_iter().filter(placeable) {
                if !seen.contains(&h.host_id) {
                    seen.push(h.host_id.clone());
                    o.push(Choice::new(h.host_id.clone(), host_name(&h)).hint(format!("doesn't offer {}", s.label)).disabled(true));
                }
            }
            o
        }
    };
    let name = inputs.get("name").unwrap_or("");
    // The first agent gets a test turn by default where a person sees the
    // sheet; the CLI's `--test-turn` stays opt-in (it may spend quota).
    let first = ops.agents().is_empty() && ops.surface() != super::Surface::Cli;
    let needs_key = source.is_some_and(|s| s.kind == Kind::Key && !ops.has_model_key(&s.harness));
    let mut source_fields = vec![
        Field::new("source", "What answers", FieldKind::Choice { options })
            .required()
            .default(default_source(&list).map(|s| s.value.clone()).unwrap_or_default())
            .help("A coding CLI signed in on this device or a host, a model gateway or hosted API a host offers, or a model server. Hosts list what they can run; their keys and addresses stay on them."),
        Field::new("endpoint", "Address", FieldKind::Text)
            .default(ep_default)
            .when("source", &["local", "key:openai"])
            .help("The server's URL: https://, or http:// on this machine."),
    ];
    // Only where this device keeps model keys (the desktop): elsewhere a
    // key lives on the host that uses it (a secret reference).
    if ops.keeps_model_keys() {
        source_fields.push(
            Field::new("api-key", "API key", FieldKind::Secret)
                .required_if(needs_key)
                .when("source", &["key:anthropic", "key:openai"])
                .help("Kept in this device's secret store, never in the workspace."),
        );
    }
    // Off unless the person turns it on; offered for agy only.
    source_fields.push(
        Field::new("local-servers", "May run local servers", FieldKind::Toggle)
            .when("source", &["antigravity"])
            .help("agy runs its own helper server and needs this. Linux with bubblewrap only for now: not available on macOS yet (ligus help agents)."),
    );
    vec![
        StepDef::input(
            "name",
            "Name",
            vec![Field::new("name", "Name", FieldKind::Text)
                .required()
                .help("You @mention it by this name. One word, not taken by another agent or a member.")],
        ),
        StepDef::input("source", "What answers", source_fields),
        StepDef::input(
            "model",
            "Model",
            vec![Field::new("model", "Model", FieldKind::Suggest { options: model_options })
                .required_if(source.is_some_and(needs_model))
                .default(model_default)
                .help("The model this agent uses. For a coding CLI, empty is the CLI's own default.")],
        ),
        StepDef::input(
            "runs-on",
            "Runs on",
            vec![Field::new("runs-on", "Runs on", FieldKind::Choice { options: host_options })
                .required()
                .default(match &t {
                    Some(Target::Here) => HERE.to_string(),
                    Some(Target::Host(h)) => h.host_id.clone(),
                    None => String::new(),
                })
                .note(runs_on_note(source, t.as_ref(), name))
                .help("Where its turns run: this device for a CLI signed in here; the host that offers a gateway or holds a key. On the command line: here, or a host's id or name.")],
        ),
        StepDef::input(
            "test",
            "Test",
            vec![Field::new("test-turn", "Run a test turn", FieldKind::Toggle)
                .default(if first { "true" } else { "" })
                .help("Sends one short message and checks the reply. It may use a little quota.")],
        ),
        StepDef::verify("verify", "Ready"),
    ]
}

/// The one button's label and how many passkey confirmations it will ask
/// for, for the current inputs.
pub(crate) fn action(ops: &dyn SetupOps, inputs: &Inputs) -> (Option<String>, usize) {
    let name = inputs.get("name").unwrap_or("").trim().to_string();
    let label = if name.is_empty() { "Add the agent".to_string() } else { format!("Add @{name}") };
    let n = match plan(ops, inputs) {
        Some(p) if !p.exists => confirmations(ops, &p),
        _ => 0,
    };
    (Some(label), n)
}

/// What a run would write, when the inputs say enough.
struct Plan {
    spec: RuntimeSpec,
    reuse: Option<String>,
    /// An agent of this name already runs on a matching runtime.
    exists: bool,
}

fn spec_for(s: &Source, t: &Target, model: &str, endpoint: &str, taken: &[&str]) -> RuntimeSpec {
    let shared = matches!(t, Target::Host(_));
    let name = if model.is_empty() { s.label.clone() } else { format!("{} ({model})", s.label) };
    let (endpoint, secret_ref) = match s.kind {
        Kind::Local => (endpoint.to_string(), String::new()),
        Kind::Key if s.harness == "openai" => (endpoint.to_string(), String::new()),
        Kind::Gateway => (s.offer.clone(), String::new()),
        // The host resolves this name only when its host.toml declares it.
        Kind::Provider => (String::new(), s.offer.clone()),
        _ => (String::new(), String::new()),
    };
    let base = match s.kind {
        Kind::Gateway | Kind::Provider => s.offer.clone(),
        _ => s.harness.clone(),
    };
    RuntimeSpec {
        id: free_runtime_id(&base, model, taken),
        name,
        harness: s.harness.clone(),
        model: model.to_string(),
        endpoint,
        provider: s.provider.clone(),
        secret_ref,
        shared,
    }
}

/// Whether runtime `r` runs what `spec` asks for (a device-local one only
/// when it needn't run elsewhere).
fn runtime_matches(r: &RuntimeFact, spec: &RuntimeSpec) -> bool {
    r.harness == spec.harness
        && r.model == spec.model
        && r.endpoint == spec.endpoint
        && r.secret_ref == spec.secret_ref
        && (r.shared || !spec.shared)
        && (r.provider.is_empty() || spec.provider.is_empty() || r.provider.eq_ignore_ascii_case(&spec.provider))
}

fn plan(ops: &dyn SetupOps, inputs: &Inputs) -> Option<Plan> {
    let list = sources(ops);
    let s = chosen(&list, inputs)?;
    let t = target(s, inputs).ok().flatten()?;
    let endpoint = inputs.get("endpoint").unwrap_or(endpoint_default(Some(s))).to_string();
    let model = match inputs.get("model") {
        Some(m) => m.to_string(),
        // A CLI's default is its own (empty): no listing needed.
        None if s.kind == Kind::Cli => String::new(),
        None => default_model(s, &model_list(ops, s, Some(&t), &endpoint)),
    };
    let runtimes = ops.runtimes();
    let taken: Vec<&str> = runtimes.iter().map(|r| r.id.as_str()).collect();
    let spec = spec_for(s, &t, &model, &endpoint, &taken);
    let reuse = runtimes.iter().find(|r| runtime_matches(r, &spec)).map(|r| r.id.clone());
    let name = inputs.get("name").unwrap_or("");
    let exists = ops
        .agents()
        .iter()
        .any(|a| a.name.eq_ignore_ascii_case(name) && runtimes.iter().any(|r| r.id == a.runtime_id && runtime_matches(r, &spec)));
    Some(Plan { spec, reuse, exists })
}

/// Passkey confirmations a plan asks for: the agent, and a workspace
/// runtime when one is defined (rules 3, 5.9).
fn confirmations(ops: &dyn SetupOps, p: &Plan) -> usize {
    if !ops.passkey_writes() {
        return 0;
    }
    1 + usize::from(p.reuse.is_none() && ops.runtime_on_log(p.spec.shared))
}

pub(crate) fn preflight(ctx: &mut Ctx, ops: &dyn SetupOps) {
    let sb = ops.sandbox();
    if sb.confined() {
        ctx.ok("sandbox", "agent sandbox", sb.tier.clone());
    } else {
        // Shown, never offered to turn off: an unconfined host refuses CLI
        // agent turns (the source step decides whether that blocks).
        ctx.warn(
            "sandbox",
            "agent sandbox",
            "none on this device: coding CLIs won't run here, models chat without tools",
            Some("ligus doctor --sandbox says what is missing".into()),
        );
    }
}

pub(crate) fn run(ctx: &mut Ctx, ops: &mut dyn SetupOps, inputs: &Inputs, secrets: &Secrets) {
    let fields: Vec<Field> = steps(ops, inputs).into_iter().flat_map(|s| s.fields).collect();
    let list = sources(ops);
    let this = ops.this_device_label();
    let name = value(inputs, &fields, "name").unwrap_or("").to_string();

    // 1. What answers, and where.
    let Some(source) = chosen(&list, inputs).cloned() else {
        let some: Vec<&str> = list.iter().map(|s| s.value.as_str()).collect();
        return ctx.fail("input.source", "what answers", "nothing was chosen to run the agent", format!("pass --source with one of: {}", some.join(", ")), exit::USAGE);
    };
    let label = source.label.clone();
    let t = match target(&source, inputs) {
        Ok(Some(t)) => t,
        Ok(None) => {
            let fix = if source.hosts.is_empty() {
                format!("install {label} here or on a host, or pick another source")
            } else {
                format!("pass --runs-on with one of: {}", names(&source.hosts))
            };
            return ctx.fail("input.runs-on", "runs on", format!("choose where {label} runs"), fix, exit::USAGE);
        }
        Err(why) => return ctx.fail("input.runs-on", "runs on", why, "pick a host that offers it (ligus hosts lists what each offers)", exit::USAGE),
    };
    let endpoint = value(inputs, &fields, "endpoint").unwrap_or(LOCAL_DEFAULT).to_string();
    match (&t, source.kind) {
        (Target::Here, Kind::Cli) => {
            if !here_cli_ready(ctx, ops, &source) {
                return;
            }
        }
        (Target::Here, Kind::Local) => {
            if let Some(p) = url_problem(&endpoint) {
                return ctx.fail("input.endpoint", "address", p, "pass --endpoint https://… (or http://localhost:…)", exit::USAGE);
            }
            ctx.ok("source", label.as_str(), super::validate::origin(&endpoint));
        }
        (Target::Here, Kind::Key) => {
            if source.harness == "openai" {
                if let Some(p) = url_problem(&endpoint) {
                    return ctx.fail("input.endpoint", "address", p, "pass --endpoint https://…", exit::USAGE);
                }
            }
            if secrets.get("api-key").is_none() && !ops.has_model_key(&source.harness) {
                return ctx.fail("input.api-key", "API key", format!("{label} needs a key on {this}"), "pass --api-key - (stdin) or @FILE (owner-only)", exit::USAGE);
            }
            ctx.ok("source", label.as_str(), format!("the key stays on {this}"));
        }
        (Target::Here, Kind::Gateway | Kind::Provider) => {
            return ctx.fail("input.runs-on", "runs on", format!("{label} runs only on a host that offers it"), "pass --runs-on with a host that offers it", exit::USAGE);
        }
        (Target::Host(h), kind) => {
            let state = host_word(h, kind == Kind::Cli);
            if h.ready == Some(false) {
                let fix = match (kind, source.here.as_ref()) {
                    (Kind::Cli, Some(f)) if !f.login_command.is_empty() => format!("on {}: run `{}`; the agent answers once it's ready", host_name(h), f.login_command),
                    _ => format!("ligus hosts shows what {} reports", host_name(h)),
                };
                ctx.warn("source", label.as_str(), format!("{} reports: {state}", host_name(h)), Some(fix));
            } else {
                ctx.ok("source", label.as_str(), format!("{state} · {}", host_name(h)));
            }
        }
    }

    // 2. The name (an agent already set up this way is a no-op rerun).
    let model = value(inputs, &fields, "model").unwrap_or("").to_string();
    let runtimes = ops.runtimes();
    let agents = ops.agents();
    let taken_ids: Vec<&str> = runtimes.iter().map(|r| r.id.as_str()).collect();
    let spec = spec_for(&source, &t, &model, &endpoint, &taken_ids);
    if let Some(a) = agents.iter().find(|a| a.name.eq_ignore_ascii_case(&name)) {
        if runtimes.iter().any(|r| r.id == a.runtime_id && runtime_matches(r, &spec)) {
            ctx.ok("agent", "agent", format!("@{} is already set up on {label}", a.name));
            ctx.result("agentId", a.id.to_string());
            ctx.result("runtimeId", a.runtime_id.clone());
            let (id, rid) = (a.id, a.runtime_id.clone());
            if !local_servers(ctx, ops, inputs, id, &name) {
                return;
            }
            return verify(ctx, ops, inputs, &fields, &rid, &name, &t);
        }
    }
    let mut taken: Vec<String> = agents.iter().map(|a| a.name.clone()).collect();
    taken.extend(ops.member_names());
    if let Some(p) = agent_name_problem(&name, &taken) {
        return ctx.fail("input.name", "name", p, "pass another --name", exit::USAGE);
    }
    ctx.ok("name", "name", format!("@{name}"));

    // 3. The model.
    let models = model_list(ops, &source, Some(&t), &endpoint);
    if let Some(e) = &models.error {
        if source.kind == Kind::Local {
            return ctx.fail("model.list", "models", format!("{}: {e}", super::validate::origin(&endpoint)), "start the model server, or check --endpoint", exit::OFFLINE);
        }
        ctx.warn("model.list", "models", format!("couldn't list models: {e}"), None);
    }
    if model.is_empty() && needs_model(&source) {
        let fix = match source.kind {
            Kind::Local => "pass --model <name> (ollama list shows them)".to_string(),
            _ if !models.models.is_empty() => {
                format!("pass --model with one of: {}", models.models.iter().take(6).map(|m| m.id.as_str()).collect::<Vec<_>>().join(", "))
            }
            _ => "pass --model <name>".to_string(),
        };
        return ctx.fail("input.model", "model", format!("{label} needs a model name"), fix, exit::USAGE);
    }
    if !model.is_empty() && models.complete && !models.models.iter().any(|m| m.id == model) {
        let some: Vec<&str> = models.models.iter().take(6).map(|m| m.id.as_str()).collect();
        return ctx.fail("input.model", "model", format!("'{model}' isn't available here"), format!("pick one of: {}", some.join(", ")), exit::USAGE);
    }
    ctx.ok("model", "model", if model.is_empty() { "the CLI's own default".to_string() } else { model.clone() });

    // 4. Where it runs: a host is an Admin's choice on a rules-3 workspace.
    let where_ = match &t {
        Target::Here => this.clone(),
        Target::Host(h) => host_name(h),
    };
    if matches!(t, Target::Host(_)) && !gate::need(ctx, ops, WorkspaceRole::Admin, "placing an agent on a host") {
        return;
    }
    ctx.ok("runs-on", "runs on", where_.clone());

    // 5. The runtime: reuse one that runs the same thing, else define one.
    let reuse = runtimes.iter().find(|r| runtime_matches(r, &spec)).map(|r| r.id.clone());
    let on_log = reuse.is_none() && ops.runtime_on_log(spec.shared);
    let total = if ops.passkey_writes() { 1 + usize::from(on_log) } else { 0 };
    if total > 0 {
        ctx.result("confirmations", total.to_string());
    }
    if reuse.is_none() && (on_log || ops.runtimes_need_admin()) && !gate::need(ctx, ops, WorkspaceRole::Admin, "defining a workspace runtime") {
        return;
    }
    if inputs.on("local-servers") && !ctx.dry_run() && !gate::need(ctx, ops, WorkspaceRole::Owner, "letting an agent run local servers") {
        return;
    }
    if ctx.cancelled() {
        return;
    }
    if let (Kind::Key, Some(key)) = (source.kind, secrets.get("api-key")) {
        if ctx.dry_run() {
            ctx.skip("key", "API key", format!("would save the key on {this}"));
        } else if let Err(e) = ops.put_model_key(&source.harness, key) {
            return ctx.fail("key", "API key", format!("couldn't save the key: {e}"), "fix what it says and run this again", ops_code(&e));
        } else {
            ctx.ok("key", "API key", format!("saved on {this}"));
        }
    }
    let mut step = 0;
    let runtime_id = match reuse {
        Some(id) => {
            ctx.ok("runtime", "runtime", format!("reusing '{id}'"));
            id
        }
        None => {
            let id = spec.id.clone();
            if ctx.dry_run() {
                ctx.skip("runtime", "runtime", format!("would define runtime '{id}'"));
            } else {
                if on_log && total > 1 {
                    step += 1;
                    ops.confirming(step, total, &format!("Define {label} for @{name}"));
                }
                if let Err(e) = ops.add_runtime(&spec) {
                    return ctx.fail("runtime", "runtime", format!("couldn't define runtime '{id}': {e}"), "fix what it says and run this again", ops_code(&e));
                }
                let rid = id.clone();
                ctx.undo(format!("runtime '{id}'"), Box::new(move |o| o.remove_runtime(&rid)));
                ctx.ok("runtime", "runtime", format!("defined '{id}'"));
                if on_log && total > 1 {
                    ctx.notes(vec![format!("Confirm {step} of {total}")]);
                }
            }
            id
        }
    };
    ctx.result("runtimeId", runtime_id.clone());

    // 6. The agent: name, runtime and host in one record (one event).
    if ctx.cancelled() {
        return;
    }
    if ctx.dry_run() {
        ctx.skip("agent", "agent", format!("would create @{name} on {where_} ('{runtime_id}')"));
        if inputs.on("local-servers") {
            ctx.skip("local-servers", "local servers", format!("would let @{name} run local servers"));
        }
        if test_turn_on(inputs, &fields) {
            ctx.skip("test-turn", "test turn", "would send one test message");
        }
        return ctx.done();
    }
    if total > 0 {
        step += 1;
        ops.confirming(step, total, &format!("Add @{name}"));
    }
    let id = match ops.add_agent_placed(&name, &runtime_id, t.host_id()) {
        Ok(id) => {
            ctx.ok("agent", "agent", format!("@{name} created, runs on {where_}"));
            if total > 1 {
                ctx.notes(vec![format!("Confirm {step} of {total}")]);
            }
            ctx.result("agentId", id.to_string());
            ctx.commit();
            id
        }
        Err(e) => return ctx.fail("agent", "agent", format!("couldn't create @{name}: {e}"), "fix what it says and run this again", ops_code(&e)),
    };
    if !local_servers(ctx, ops, inputs, id, &name) {
        return;
    }
    verify(ctx, ops, inputs, &fields, &runtime_id, &name, &t);
}

/// Only what was given: a surface sends the toggle's default as an input
/// (the sheet prefills it), and the CLI's `--test-turn` stays opt-in.
fn test_turn_on(inputs: &Inputs, _fields: &[Field]) -> bool {
    inputs.on("test-turn")
}

/// A coding CLI on this device: installed, signed in, and a sandbox to run in.
fn here_cli_ready(ctx: &mut Ctx, ops: &dyn SetupOps, s: &Source) -> bool {
    let label = s.label.as_str();
    match &s.here {
        Some(f) if f.installed => {
            let v = f.version.as_deref().map(|v| format!(" {v}")).unwrap_or_default();
            ctx.ok("harness.installed", label, format!("installed{v}"));
            match f.login {
                LoginState::LoggedIn => ctx.ok("harness.signed-in", "sign-in", f.login_detail.clone()),
                LoginState::NotLoggedIn => {
                    ctx.fail(
                        "harness.signed-in",
                        "sign-in",
                        format!("{label} isn't signed in on this device"),
                        format!("run `{}` in a terminal on this device, then run this again", f.login_command),
                        exit::FIXABLE,
                    );
                    return false;
                }
                LoginState::Unknown if !f.login_command.is_empty() => ctx.warn(
                    "harness.signed-in",
                    "sign-in",
                    "can't tell from here whether it is signed in",
                    Some(format!("if the test turn fails, run `{}` in a terminal", f.login_command)),
                ),
                LoginState::Unknown => {}
            }
        }
        Some(f) => {
            ctx.fail(
                "harness.installed",
                label,
                "not installed on this device",
                if f.install_command.is_empty() { format!("install {label}, then run this again") } else { f.install_command.clone() },
                exit::FIXABLE,
            );
            return false;
        }
        None => {
            ctx.fail("harness.installed", label, "not found on this device", format!("install {label}, then run this again"), exit::FIXABLE);
            return false;
        }
    }
    if !ops.sandbox().confined() {
        ctx.fail(
            "harness.sandbox",
            "agent sandbox",
            format!("{label} turns are refused without an agent sandbox on this device"),
            "ligus doctor --sandbox says what is missing (macOS: Seatbelt; Linux: bwrap or Landlock)",
            exit::FIXABLE,
        );
        return false;
    }
    true
}

/// The agent's local-servers grant, when asked for (`--local-servers`):
/// on, the Owner's signed switch. Never turned off here (a rerun without
/// the flag leaves it as it is). `false` when the flow stopped.
fn local_servers(ctx: &mut Ctx, ops: &mut dyn SetupOps, inputs: &Inputs, id: uuid::Uuid, name: &str) -> bool {
    if !inputs.on("local-servers") {
        return true;
    }
    if ops.local_servers(id) {
        ctx.ok("local-servers", "local servers", format!("@{name} may already run local servers"));
        held_on_macos(ctx, ops);
        return true;
    }
    if ctx.dry_run() {
        ctx.skip("local-servers", "local servers", format!("would let @{name} run local servers"));
        return true;
    }
    if !gate::need(ctx, ops, WorkspaceRole::Owner, "letting an agent run local servers") {
        return false;
    }
    match ops.set_local_servers(id, true) {
        Ok(()) => {
            ctx.ok("local-servers", "local servers", format!("@{name} may run local servers"));
            held_on_macos(ctx, ops);
            true
        }
        Err(e) => {
            ctx.fail(
                "local-servers",
                "local servers",
                format!("couldn't let @{name} run local servers: {e}"),
                format!("the workspace Owner runs ligus agents set {name} --local-servers on, or turns it on in Settings, Sandbox"),
                ops_code(&e),
            );
            false
        }
    }
}

/// One line when this host is a Mac: the grant is stored but has no effect.
fn held_on_macos(ctx: &mut Ctx, ops: &dyn SetupOps) {
    if ops.sandbox().tier == "seatbelt" {
        ctx.warn("local-servers.macos", "local servers on this Mac", "not available on macOS yet: the Mac's sandbox can't keep a listener loopback-only, so it has no effect here", Some("run agy agents on a Linux host with the bubblewrap sandbox".into()));
    }
}

fn verify(ctx: &mut Ctx, ops: &mut dyn SetupOps, inputs: &Inputs, fields: &[Field], runtime_id: &str, name: &str, t: &Target) {
    if !ops.agents().iter().any(|a| a.name.eq_ignore_ascii_case(name)) {
        return ctx.fail("verify", "verify", format!("@{name} isn't on the roster"), "ligus agents lists the roster; run this again", exit::FAILED);
    }
    if test_turn_on(inputs, fields) {
        if ctx.cancelled() {
            return;
        }
        if let Target::Host(h) = t {
            ctx.skip("test-turn", "test turn", format!("@{name} runs on {}: ask it something in a chat", host_name(h)));
            return ctx.done();
        }
        let local = ops.agents().iter().find(|a| a.name.eq_ignore_ascii_case(name)).is_some_and(|a| ops.local_servers(a.id));
        match ops.test_turn_with(runtime_id, local) {
            Ok(tt) if tt.ok => ctx.ok("test-turn", "test turn", format!("answered in {} ms", tt.latency_ms)),
            Ok(tt) => {
                return ctx.fail("test-turn", "test turn", tt.detail, "check the sign-in and the model, then run this again with --test-turn", exit::FAILED)
            }
            Err(super::OpsError::Unsupported) => ctx.skip("test-turn", "test turn", "not available here; ask the agent something"),
            Err(e) => return ctx.fail("test-turn", "test turn", e.to_string(), "check the sign-in and the model", ops_code(&e)),
        }
    }
    ctx.done();
}

/// `harness` or `harness-model`, made unique.
pub(crate) fn free_runtime_id(harness: &str, model: &str, taken: &[&str]) -> String {
    let slug = |s: &str| {
        let mut o = String::new();
        for c in s.chars() {
            if c.is_ascii_alphanumeric() {
                o.push(c.to_ascii_lowercase());
            } else if !o.ends_with('-') {
                o.push('-');
            }
        }
        o.trim_matches('-').chars().take(40).collect::<String>()
    };
    let base = if model.is_empty() { slug(harness) } else { format!("{}-{}", slug(harness), slug(model)) };
    if !taken.contains(&base.as_str()) {
        return base;
    }
    (2..).map(|n| format!("{base}-{n}")).find(|c| !taken.contains(&c.as_str())).unwrap_or(base)
}

/// The exit code for an operation's error.
pub(crate) fn ops_code(e: &super::OpsError) -> i32 {
    match e {
        super::OpsError::Unsupported => exit::FIXABLE,
        super::OpsError::Refused(_) => exit::REFUSED,
        super::OpsError::Offline(_) => exit::OFFLINE,
        super::OpsError::Failed(_) => exit::FAILED,
    }
}

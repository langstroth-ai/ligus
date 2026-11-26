//! Guided setup flows: one engine for `ligus setup`, the desktop's wizards
//! and lt (docs/design/setup-flows.md).
//!
//! The engine owns what every surface must agree on: the flows and their
//! steps and fields, validation, the check ids and fix lines, the order, the
//! role gates, idempotency (a finished step is skipped), rollback of a
//! flow's own writes, and the secret rules. It does no I/O of its own:
//! everything that touches the world goes through [`SetupOps`], which each
//! surface implements.
//!
//! ```text
//! flows()                         the seven flows, in order
//! status(ops)                     what is already done (cheap, no network)
//! form(flow, ops, inputs)         preflight checks + steps + fields to render
//! run(flow, ops, inputs, secrets, opts) -> Outcome
//! ```
//!
//! [`run`] is gate → preflight → validate → steps (each skipped when already
//! done) → verify, and stops at the first blocking check. Its [`Outcome`]
//! carries a `ligus.checks.v1` [`Report`] and the exit code from the shared
//! table (`ligus help exit-codes`).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ligus_core::WorkspaceRole;
use serde::Serialize;
use zeroize::Zeroizing;

use crate::checks::{Check, Report, Status};

mod agent;
mod folder;
mod gate;
mod human;
mod ops;
mod relay;
pub mod service;
mod tool;
pub mod validate;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

pub use ops::*;

/// The exit codes of the shared table (`ligus help exit-codes`).
pub mod exit {
    pub const OK: i32 = 0;
    pub const FAILED: i32 = 1;
    pub const USAGE: i32 = 2;
    pub const FIXABLE: i32 = 3;
    pub const PERSON: i32 = 4;
    pub const NOT_FOUND: i32 = 5;
    pub const REFUSED: i32 = 6;
    pub const OFFLINE: i32 = 7;
}

/// The seven flows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FlowId {
    Agent,
    Tool,
    Folder,
    Box,
    Phone,
    Invite,
    Relay,
}

impl FlowId {
    pub const ALL: [FlowId; 7] = [
        FlowId::Agent,
        FlowId::Tool,
        FlowId::Folder,
        FlowId::Box,
        FlowId::Phone,
        FlowId::Invite,
        FlowId::Relay,
    ];

    /// The flow's id on the command line (`ligus setup <id>`).
    pub fn as_str(self) -> &'static str {
        match self {
            FlowId::Agent => "agent",
            FlowId::Tool => "tool",
            FlowId::Folder => "folder",
            FlowId::Box => "box",
            FlowId::Phone => "phone",
            FlowId::Invite => "invite",
            FlowId::Relay => "relay",
        }
    }

    pub fn parse(s: &str) -> Option<FlowId> {
        FlowId::ALL.into_iter().find(|f| f.as_str() == s.trim())
    }

    pub fn def(self) -> &'static FlowDef {
        &FLOWS[FlowId::ALL.iter().position(|f| *f == self).unwrap_or(0)]
    }
}

/// A flow: what it is called and who may run it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowDef {
    pub id: FlowId,
    /// The palette and wizard title ("Set up an agent").
    pub title: &'static str,
    /// One line for lists.
    pub summary: &'static str,
    /// One line behind the (i): why you would do this.
    pub why: &'static str,
    /// Words the palette also matches.
    pub keywords: &'static str,
    /// The lowest role that may run it (`None`: anyone, even before a
    /// workspace exists). Some steps raise it (see each flow).
    #[serde(serialize_with = "role_str")]
    pub min_role: Option<WorkspaceRole>,
    /// It has a step only a person can do (a passkey, pairing, an invite).
    pub person_steps: bool,
}

fn role_str<S: serde::Serializer>(r: &Option<WorkspaceRole>, s: S) -> Result<S::Ok, S::Error> {
    match r {
        None => s.serialize_none(),
        Some(r) => s.serialize_str(role_name(*r)),
    }
}

/// A role's name as people read it.
pub fn role_name(r: WorkspaceRole) -> &'static str {
    match r {
        WorkspaceRole::Owner => "Owner",
        WorkspaceRole::Admin => "Admin",
        WorkspaceRole::Contributor => "Contributor",
        WorkspaceRole::Viewer => "Viewer",
    }
}

pub static FLOWS: [FlowDef; 7] = [
    FlowDef {
        id: FlowId::Agent,
        title: "Set up an agent",
        summary: "an agent on a coding CLI or model that is installed and signed in",
        why: "An agent answers when you @mention it. It runs one harness (Claude Code, Codex, Copilot, \
              Antigravity, Aider, Pi or a local model) inside this device's agent sandbox.",
        keywords: "agent harness runtime claude codex copilot agy aider pi ollama model",
        min_role: Some(WorkspaceRole::Contributor),
        person_steps: false,
    },
    FlowDef {
        id: FlowId::Tool,
        title: "Connect a tool",
        summary: "an MCP server your agents can call",
        why: "A tool is an MCP server: a command on this device or an https URL. Its token stays in this \
              device's secret store, and you choose which agents may reach it.",
        keywords: "tool mcp server connect integration",
        min_role: None,
        person_steps: false,
    },
    FlowDef {
        id: FlowId::Folder,
        title: "Add a code folder",
        summary: "a git checkout on this device, bound to a workspace repository",
        why: "Agents work on a repository in its own worktree per chat. Binding tells this device which \
              folder holds the workspace's repository.",
        keywords: "folder repo repository git checkout bind code",
        min_role: Some(WorkspaceRole::Contributor),
        person_steps: false,
    },
    FlowDef {
        id: FlowId::Box,
        title: "Add a box",
        summary: "an always-on machine (ligus serve) that joins as its own member",
        why: "A box keeps agents working while your laptop sleeps. It joins only through an admission an \
              Admin approves with a passkey.",
        keywords: "box server serve worker home always-on admit admission",
        min_role: None,
        person_steps: true,
    },
    FlowDef {
        id: FlowId::Phone,
        title: "Add a phone",
        summary: "pair your phone with this workspace",
        why: "The phone app follows your chats and approvals. Pairing happens in your own window; the \
              pairing secret is never logged.",
        keywords: "phone iphone mobile pair qr device",
        min_role: None,
        person_steps: true,
    },
    FlowDef {
        id: FlowId::Invite,
        title: "Invite a person",
        summary: "invite someone to this workspace and see when they join",
        why: "An invite is single use and carries the workspace key: send it only to the person it is for. \
              Their role decides what they may do.",
        keywords: "invite person people member teammate join role",
        min_role: Some(WorkspaceRole::Admin),
        person_steps: true,
    },
    FlowDef {
        id: FlowId::Relay,
        title: "Set up a relay",
        summary: "the relay this workspace syncs through (community or your own, https only)",
        why: "The relay carries end-to-end encrypted events between devices; it never sees the workspace key. \
              Use the community relay or run your own.",
        keywords: "relay sync server hosted self-hosted community https",
        min_role: None,
        person_steps: false,
    },
];

/// The seven flows, in order.
pub fn flows() -> &'static [FlowDef] {
    &FLOWS
}

/// A flow by its command-line id.
pub fn flow(id: &str) -> Option<&'static FlowDef> {
    FlowId::parse(id).map(FlowId::def)
}

// ── Forms ───────────────────────────────────────────────────────────────────

/// How a field is entered.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum FieldKind {
    Text,
    /// One of `options` (the CLI takes the option's `value`).
    Choice {
        options: Vec<Choice>,
    },
    /// Several of `options`, comma-separated on the command line.
    Multi {
        options: Vec<Choice>,
    },
    /// Free text, with `options` offered (a model list that may be
    /// incomplete).
    Suggest {
        options: Vec<Choice>,
    },
    /// Masked; never echoed, logged or returned. The CLI reads it from stdin
    /// (`-`) or an owner-only file (`@PATH`), never from the command line.
    Secret,
    /// A folder on this device.
    Path,
    /// On or off (`--flag` alone on the command line).
    Toggle,
}

/// One option of a choice.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    pub value: String,
    pub label: String,
    /// A short state shown next to it ("signed in", "not installed").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Can't be picked now (`hint` says why).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
}

impl Choice {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Choice {
            value: value.into(),
            label: label.into(),
            hint: None,
            disabled: false,
        }
    }
    pub fn hint(mut self, h: impl Into<String>) -> Self {
        let h = h.into();
        self.hint = (!h.is_empty()).then_some(h);
        self
    }
    pub fn disabled(mut self, d: bool) -> Self {
        self.disabled = d;
        self
    }
}

/// One thing to enter.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    /// Also the command-line flag: `--<id>`.
    pub id: &'static str,
    pub label: &'static str,
    #[serde(flatten)]
    pub kind: FieldKind,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// One line behind the (i).
    #[serde(skip_serializing_if = "str::is_empty")]
    pub help: &'static str,
    /// Shown only when another field has one of these values
    /// (`("harness", ["local"])`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<(&'static str, Vec<&'static str>)>,
}

impl Field {
    fn new(id: &'static str, label: &'static str, kind: FieldKind) -> Self {
        Field {
            id,
            label,
            kind,
            required: false,
            default: None,
            help: "",
            when: None,
        }
    }
    fn required(mut self) -> Self {
        self.required = true;
        self
    }
    fn required_if(mut self, r: bool) -> Self {
        self.required = r;
        self
    }
    fn default(mut self, d: impl Into<String>) -> Self {
        let d = d.into();
        self.default = (!d.is_empty()).then_some(d);
        self
    }
    fn help(mut self, h: &'static str) -> Self {
        self.help = h;
        self
    }
    fn when(mut self, field: &'static str, values: &[&'static str]) -> Self {
        self.when = Some((field, values.to_vec()));
        self
    }
    /// Whether the field applies given the other inputs.
    pub fn applies(&self, inputs: &Inputs) -> bool {
        match &self.when {
            None => true,
            Some((f, vals)) => inputs.get(f).is_some_and(|v| vals.contains(&v)),
        }
    }
}

/// What a step screen is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StepKind {
    /// Fields to fill in.
    Input,
    /// Only a person can do it, in the UI named (a passkey, pairing).
    Person,
    /// Checks the result.
    Verify,
}

/// One screen of a wizard (one group of flags on the command line).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepDef {
    pub id: &'static str,
    pub title: &'static str,
    pub kind: StepKind,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Field>,
    /// For a person step: which surface it happens in ("desktop: workspace
    /// menu > Admit a box").
    #[serde(rename = "where", skip_serializing_if = "str::is_empty")]
    pub where_: &'static str,
}

impl StepDef {
    fn input(id: &'static str, title: &'static str, fields: Vec<Field>) -> Self {
        StepDef {
            id,
            title,
            kind: StepKind::Input,
            fields,
            where_: "",
        }
    }
    fn person(id: &'static str, title: &'static str, where_: &'static str) -> Self {
        StepDef {
            id,
            title,
            kind: StepKind::Person,
            fields: Vec::new(),
            where_,
        }
    }
    fn verify(id: &'static str, title: &'static str) -> Self {
        StepDef {
            id,
            title,
            kind: StepKind::Verify,
            fields: Vec::new(),
            where_: "",
        }
    }
}

/// What a surface renders: the preflight checks, then the steps.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Form {
    pub flow: &'static FlowDef,
    /// Preflight and the role gate, as checks (a `fail` blocks the flow).
    #[serde(serialize_with = "checks_json")]
    pub preflight: Vec<Check>,
    pub blocked: bool,
    pub steps: Vec<StepDef>,
}

fn checks_json<S: serde::Serializer>(c: &[Check], s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(c.iter().map(Check::to_json))
}

/// Every field of a form, in order.
impl Form {
    pub fn fields(&self) -> impl Iterator<Item = &Field> {
        self.steps.iter().flat_map(|s| s.fields.iter())
    }
}

// ── Inputs ──────────────────────────────────────────────────────────────────

/// Non-secret field values by field id. Blank means unset.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, Serialize)]
#[serde(transparent)]
pub struct Inputs(pub BTreeMap<String, String>);

impl Inputs {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with(mut self, k: &str, v: impl Into<String>) -> Self {
        self.set(k, v);
        self
    }
    pub fn set(&mut self, k: &str, v: impl Into<String>) {
        self.0.insert(k.to_string(), v.into());
    }
    /// The trimmed value, `None` when blank.
    pub fn get(&self, k: &str) -> Option<&str> {
        self.0.get(k).map(|v| v.trim()).filter(|v| !v.is_empty())
    }
    pub fn on(&self, k: &str) -> bool {
        matches!(self.get(k), Some("true" | "on" | "yes" | "1"))
    }
}

/// Secret field values by field id. Zeroized on drop, redacted in `Debug`,
/// and not serializable: nothing the engine returns can carry one.
#[derive(Default)]
pub struct Secrets(BTreeMap<String, Zeroizing<String>>);

impl Secrets {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set(&mut self, k: &str, v: Zeroizing<String>) {
        self.0.insert(k.to_string(), v);
    }
    pub fn get(&self, k: &str) -> Option<&Zeroizing<String>> {
        self.0.get(k).filter(|v| !v.trim().is_empty())
    }
    pub fn is_empty(&self) -> bool {
        self.0.values().all(|v| v.trim().is_empty())
    }
    /// The ids that hold a value (never the values).
    pub fn ids(&self) -> Vec<&str> {
        self.0
            .iter()
            .filter(|(_, v)| !v.trim().is_empty())
            .map(|(k, _)| k.as_str())
            .collect()
    }
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.0.keys().map(|k| (k, "***")))
            .finish()
    }
}

/// Who is driving the flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Driver {
    /// A person at the CLI, the desktop or lt.
    #[default]
    Person,
    /// An agent acting for someone (a future @ligus "set it up"). Refused
    /// for the flows that admit, pair, invite or move the relay, and for any
    /// run that carries a secret.
    Agent,
}

/// How to run.
#[derive(Debug, Clone, Default)]
pub struct RunOpts {
    /// Check and validate, write nothing.
    pub dry_run: bool,
    pub driver: Driver,
    /// Set from another thread to stop between steps.
    pub cancel: Option<Arc<AtomicBool>>,
}

/// What a run did.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub flow: FlowId,
    /// The checks, in order, and the commands to run next.
    pub report: Report,
    /// The exit code (shared table).
    pub exit: i32,
    /// Everything finished and verified.
    pub done: bool,
    /// Non-secret results by name (`agentId`, `runtimeId`, `repoId`,
    /// `joinRequest`).
    pub results: BTreeMap<String, String>,
}

impl Outcome {
    /// The `ligus.checks.v1` document, with `flow`, `done`, `dryRun` and
    /// `results` added (consumers ignore fields they don't know).
    pub fn to_json(&self, dry_run: bool) -> serde_json::Value {
        let mut d = self.report.to_json(self.exit);
        d["flow"] = self.flow.as_str().into();
        d["done"] = self.done.into();
        d["dryRun"] = dry_run.into();
        d["results"] = serde_json::to_value(&self.results).unwrap_or_default();
        d
    }
}

// ── Status ──────────────────────────────────────────────────────────────────

/// How far along a flow is on this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Progress {
    /// Something is set up already (the flow can still add more).
    Done,
    /// Started and waiting (an admission to approve, an invite not used yet).
    Waiting,
    /// Nothing yet.
    Todo,
    /// Not possible here or not for your role.
    Unavailable,
}

/// One line of `ligus setup`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowStatus {
    pub flow: &'static FlowDef,
    pub progress: Progress,
    /// One line ("2 agents", "no relay yet").
    pub detail: String,
}

/// What each flow has done on this device (cheap: no network, no probes).
pub fn status(ops: &dyn SetupOps) -> Vec<FlowStatus> {
    FlowId::ALL
        .into_iter()
        .map(|f| {
            let (progress, detail) = match gate::role_gate(f, ops) {
                Err(why) => (Progress::Unavailable, why),
                Ok(()) => match f {
                    FlowId::Agent => agent::status(ops),
                    FlowId::Tool => tool::status(ops),
                    FlowId::Folder => folder::status(ops),
                    FlowId::Box => human::box_status(ops),
                    FlowId::Phone => human::phone_status(ops),
                    FlowId::Invite => human::invite_status(ops),
                    FlowId::Relay => relay::status(ops),
                },
            };
            FlowStatus {
                flow: f.def(),
                progress,
                detail,
            }
        })
        .collect()
}

/// The form for a flow: its preflight (with the role gate) and its steps,
/// with choices filled from `ops` and `inputs` (the models of the chosen
/// harness).
pub fn form(flow: FlowId, ops: &dyn SetupOps, inputs: &Inputs) -> Form {
    let mut ctx = Ctx::new(flow, &RunOpts::default());
    gate::run_gate(&mut ctx, ops);
    if !ctx.stopped() {
        preflight(flow, &mut ctx, ops, inputs);
    }
    let steps = match flow {
        FlowId::Agent => agent::steps(ops, inputs),
        FlowId::Tool => tool::steps(ops, inputs),
        FlowId::Folder => folder::steps(ops, inputs),
        FlowId::Box => human::box_steps(ops),
        FlowId::Phone => human::phone_steps(ops),
        FlowId::Invite => human::invite_steps(ops),
        FlowId::Relay => relay::steps(ops, inputs),
    };
    Form {
        flow: flow.def(),
        blocked: ctx.stopped(),
        preflight: ctx.checks,
        steps,
    }
}

fn preflight(flow: FlowId, ctx: &mut Ctx, ops: &dyn SetupOps, inputs: &Inputs) {
    match flow {
        FlowId::Agent => agent::preflight(ctx, ops),
        FlowId::Tool => tool::preflight(ctx, ops),
        FlowId::Folder => folder::preflight(ctx, ops),
        FlowId::Box => human::box_preflight(ctx, ops),
        FlowId::Phone => human::phone_preflight(ctx, ops),
        FlowId::Invite => human::invite_preflight(ctx, ops),
        FlowId::Relay => relay::preflight(ctx, ops, inputs),
    }
}

/// Run a flow: gate, preflight, validation, the steps (each skipped when
/// already done), verify. See the module docs.
pub fn run(
    flow: FlowId,
    ops: &mut dyn SetupOps,
    inputs: &Inputs,
    secrets: &Secrets,
    opts: &RunOpts,
) -> Outcome {
    let mut ctx = Ctx::new(flow, opts);
    ctx.surface = Some(ops.surface());
    gate::driver_gate(&mut ctx, secrets);
    if !ctx.stopped() {
        gate::run_gate(&mut ctx, ops);
    }
    if !ctx.stopped() {
        preflight(flow, &mut ctx, ops, inputs);
    }
    if !ctx.stopped() {
        validate::unknown_inputs(&mut ctx, &form_fields(flow, ops, inputs), inputs, secrets);
    }
    if !ctx.stopped() {
        match flow {
            FlowId::Agent => agent::run(&mut ctx, ops, inputs),
            FlowId::Tool => tool::run(&mut ctx, ops, inputs, secrets),
            FlowId::Folder => folder::run(&mut ctx, ops, inputs),
            FlowId::Box => human::box_run(&mut ctx, ops, inputs, secrets),
            FlowId::Phone => human::phone_run(&mut ctx, ops),
            FlowId::Invite => human::invite_run(&mut ctx, ops, inputs),
            FlowId::Relay => relay::run(&mut ctx, ops, inputs, secrets),
        }
    }
    ctx.finish(ops)
}

fn form_fields(flow: FlowId, ops: &dyn SetupOps, inputs: &Inputs) -> Vec<Field> {
    let steps = match flow {
        FlowId::Agent => agent::steps(ops, inputs),
        FlowId::Tool => tool::steps(ops, inputs),
        FlowId::Folder => folder::steps(ops, inputs),
        FlowId::Box => human::box_steps(ops),
        FlowId::Phone => human::phone_steps(ops),
        FlowId::Invite => human::invite_steps(ops),
        FlowId::Relay => relay::steps(ops, inputs),
    };
    steps.into_iter().flat_map(|s| s.fields).collect()
}

// ── The run context ─────────────────────────────────────────────────────────

type Undo = Box<dyn FnOnce(&mut dyn SetupOps) -> Result<(), OpsError>>;

/// A run in progress: its checks, its exit code, its undo list.
pub(crate) struct Ctx {
    flow: FlowId,
    checks: Vec<Check>,
    next: Vec<String>,
    exit: i32,
    stopped: bool,
    done: bool,
    dry_run: bool,
    driver: Driver,
    cancel: Option<Arc<AtomicBool>>,
    results: BTreeMap<String, String>,
    undo: Vec<(String, Undo)>,
    /// Where the flow runs, for the one line a passkey refusal gets.
    surface: Option<Surface>,
}

/// The fix line for a write the workspace's rules hold for its author's
/// passkey, by surface. The CLI has no passkey of its own: the desktop app
/// confirms it, or a recovery-words file does.
pub(crate) fn presence_fix(surface: Option<Surface>) -> &'static str {
    match surface {
        Some(Surface::Cli) => {
            "this needs your passkey: do it in the desktop app, or run this again with --recovery-words-file PATH (your recovery key's 24 words, an owner-only file)"
        }
        _ => "confirm with your passkey when asked, then run this again",
    }
}

/// Whether a failure is the rules asking for its author's passkey (a
/// person's step, exit 4), not a refusal.
pub(crate) fn is_presence_refusal(detail: &str) -> bool {
    detail.contains(ligus_core::authz::PRESENCE_REQUIRED)
}

impl Ctx {
    fn new(flow: FlowId, opts: &RunOpts) -> Self {
        Ctx {
            flow,
            checks: Vec::new(),
            next: Vec::new(),
            exit: exit::OK,
            stopped: false,
            done: false,
            dry_run: opts.dry_run,
            driver: opts.driver,
            cancel: opts.cancel.clone(),
            results: BTreeMap::new(),
            undo: Vec::new(),
            surface: None,
        }
    }

    /// `<flow>.<id>`.
    fn id(&self, id: &str) -> String {
        format!("{}.{id}", self.flow.as_str())
    }

    pub(crate) fn stopped(&self) -> bool {
        self.stopped
    }

    pub(crate) fn ok(&mut self, id: &str, title: &str, detail: impl Into<String>) {
        let id = self.id(id);
        self.checks.push(Check::ok(&id, title, detail));
    }

    pub(crate) fn warn(
        &mut self,
        id: &str,
        title: &str,
        detail: impl Into<String>,
        fix: Option<String>,
    ) {
        let id = self.id(id);
        let mut c = Check::warn(&id, title, detail);
        if let Some(f) = fix {
            c = c.fix(f);
        }
        self.checks.push(c);
    }

    pub(crate) fn skip(&mut self, id: &str, title: &str, detail: impl Into<String>) {
        let id = self.id(id);
        self.checks.push(Check::skip(&id, title, detail));
    }

    /// A blocking failure: the run stops with `code`. A write the rules
    /// hold for its author's passkey is a person's step instead (exit 4),
    /// with the one line that says where to confirm it.
    pub(crate) fn fail(
        &mut self,
        id: &str,
        title: &str,
        detail: impl Into<String>,
        fix: impl Into<String>,
        code: i32,
    ) {
        let detail = detail.into();
        if is_presence_refusal(&detail) {
            let what = presence_fix(self.surface);
            return self.person(id, title, detail, what);
        }
        let id = self.id(id);
        self.checks.push(Check::fail(&id, title, detail).fix(fix));
        self.stop(code);
    }

    /// A step only a person can do: a warning, `next` says what, exit 4.
    pub(crate) fn person(
        &mut self,
        id: &str,
        title: &str,
        detail: impl Into<String>,
        what: impl Into<String>,
    ) {
        let what = what.into();
        let id = self.id(id);
        self.checks
            .push(Check::warn(&id, title, detail).fix(what.clone()));
        self.next.push(what);
        self.stop(exit::PERSON);
    }

    pub(crate) fn notes(&mut self, notes: Vec<String>) {
        if let Some(c) = self.checks.pop() {
            self.checks.push(c.notes(notes));
        }
    }

    pub(crate) fn next(&mut self, cmd: impl Into<String>) {
        let cmd = cmd.into();
        if !self.next.contains(&cmd) {
            self.next.push(cmd);
        }
    }

    pub(crate) fn result(&mut self, k: &str, v: impl Into<String>) {
        self.results.insert(k.to_string(), v.into());
    }

    fn stop(&mut self, code: i32) {
        if !self.stopped {
            self.stopped = true;
            self.exit = code;
        }
    }

    pub(crate) fn dry_run(&self) -> bool {
        self.dry_run
    }

    pub(crate) fn driver(&self) -> Driver {
        self.driver
    }

    /// Mark the flow finished and verified.
    pub(crate) fn done(&mut self) {
        if !self.stopped {
            self.done = true;
        }
    }

    /// The writes so far are final (a later failure is a verify failure,
    /// not a reason to undo them).
    pub(crate) fn commit(&mut self) {
        self.undo.clear();
    }

    /// Register how to undo a write this run made.
    pub(crate) fn undo(&mut self, what: impl Into<String>, f: Undo) {
        self.undo.push((what.into(), f));
    }

    /// Between steps: stop when cancelled. Returns whether to stop.
    pub(crate) fn cancelled(&mut self) -> bool {
        if self.stopped {
            return true;
        }
        if self
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::SeqCst))
        {
            let id = self.id("cancelled");
            self.checks.push(
                Check::warn(
                    &id,
                    "cancelled",
                    "stopped before the next step; this run's own writes are undone",
                )
                .fix(format!(
                    "run `ligus setup {}` again to finish",
                    self.flow.as_str()
                )),
            );
            self.stop(exit::FAILED);
            return true;
        }
        false
    }

    /// The outcome; undoes this run's writes when it stopped on a failure
    /// (not on a person step: what came before it is kept for the rerun).
    fn finish(mut self, ops: &mut dyn SetupOps) -> Outcome {
        let rollback = self.stopped && self.exit != exit::PERSON && !self.undo.is_empty();
        if rollback {
            let mut undone = Vec::new();
            let mut failed = Vec::new();
            while let Some((what, f)) = self.undo.pop() {
                match f(ops) {
                    Ok(()) => undone.push(what),
                    Err(e) => failed.push(format!("{what}: {e}")),
                }
            }
            let id = self.id("rolled-back");
            if failed.is_empty() {
                self.checks.push(Check::ok(
                    &id,
                    "rolled back",
                    format!("undid {}", undone.join(", ")),
                ));
            } else {
                self.checks.push(
                    Check::fail(
                        &id,
                        "rolled back",
                        format!("could not undo {}", failed.join("; ")),
                    )
                    .fix(format!(
                        "remove it by hand; `ligus setup {}` lists what is set up",
                        self.flow.as_str()
                    )),
                );
            }
        }
        if !self.stopped && !self.done {
            // Every flow ends in done() or a stop; this is a safety net.
            self.done = true;
        }
        let ok = !self.checks.iter().any(|c| c.status == Status::Fail);
        if self.exit == exit::OK && !ok {
            self.exit = exit::FAILED;
        }
        let mut report = Report::new(&format!("setup {}", self.flow.as_str()));
        report.checks = self.checks;
        for n in self.next {
            report.next(n);
        }
        Outcome {
            flow: self.flow,
            report,
            exit: self.exit,
            done: self.done,
            results: self.results,
        }
    }
}

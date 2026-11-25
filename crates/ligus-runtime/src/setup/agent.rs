//! Set up an agent: harness (installed, signed in) → name → model →
//! sandbox → optional test turn.

use ligus_core::WorkspaceRole;

use super::validate::{agent_name_problem, url_problem, value};
use super::{exit, gate, Choice, Ctx, Field, FieldKind, Inputs, Progress, RuntimeSpec, SetupOps, StepDef};
use crate::cli_detect::LoginState;

/// The harnesses in the order the picker shows them.
pub const HARNESSES: &[(&str, &str)] = &[
    ("claude-code", "Claude Code"),
    ("codex", "Codex"),
    ("copilot", "GitHub Copilot CLI"),
    ("antigravity", "Google Antigravity CLI"),
    ("aider", "Aider"),
    ("pi", "Pi"),
    ("local", "Local model (Ollama or OpenAI-compatible)"),
    ("gateway", "Model gateway on a host"),
];

/// Harnesses that run an HTTP model rather than a coding CLI: they still
/// chat without a sandbox, but get no tools there.
fn is_http(h: &str) -> bool {
    matches!(h, "local" | "gateway")
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
    let facts = ops.harnesses();
    let options = HARNESSES
        .iter()
        .map(|(id, label)| {
            let hint = match facts.iter().find(|f| f.id == *id) {
                None if is_http(id) => String::new(),
                None => "not detected".into(),
                Some(f) if !f.installed => "not installed".into(),
                Some(f) => match f.login {
                    LoginState::LoggedIn => "signed in".into(),
                    LoginState::NotLoggedIn => "not signed in".into(),
                    LoginState::Unknown if f.login_command.is_empty() => "installed".into(),
                    LoginState::Unknown => "installed, sign-in unknown".into(),
                },
            };
            Choice::new(*id, *label).hint(hint)
        })
        .collect();
    let harness = inputs.get("harness").unwrap_or("");
    let endpoint = inputs.get("endpoint").unwrap_or("http://localhost:11434");
    let models = if harness.is_empty() { Vec::new() } else { ops.models(harness, endpoint).models };
    let model_options = models.into_iter().map(|m| Choice::new(m.id.clone(), m.label.unwrap_or(m.id))).collect();
    vec![
        StepDef::input(
            "harness",
            "Harness",
            vec![
                Field::new("harness", "Harness", FieldKind::Choice { options })
                    .required()
                    .help("The coding CLI or model the agent runs. It must be installed and signed in on this device."),
                Field::new("endpoint", "Endpoint", FieldKind::Text)
                    .default("http://localhost:11434")
                    .when("harness", &["local"])
                    .help("The model server's URL: https://, or http:// on this machine."),
                Field::new("gateway", "Gateway", FieldKind::Text)
                    .required()
                    .when("harness", &["gateway"])
                    .help("The gateway's name, as the host lists it (ligus gateway-check)."),
            ],
        ),
        StepDef::input(
            "name",
            "Name",
            vec![Field::new("name", "Name", FieldKind::Text)
                .required()
                .help("You @mention it by this name. One word, not taken by another agent or a member.")],
        ),
        StepDef::input(
            "model",
            "Model",
            vec![Field::new("model", "Model", FieldKind::Suggest { options: model_options })
                .required_if(harness == "local")
                .help("Leave it empty for the harness's own default.")],
        ),
        StepDef::input(
            "test",
            "Test",
            vec![Field::new("test-turn", "Run a test turn", FieldKind::Toggle)
                .help("Sends one short message and checks the reply. It may use a little quota.")],
        ),
        StepDef::verify("verify", "Ready"),
    ]
}

pub(crate) fn preflight(ctx: &mut Ctx, ops: &dyn SetupOps) {
    let sb = ops.sandbox();
    if sb.confined() {
        ctx.ok("sandbox", "agent sandbox", sb.tier.clone());
    } else {
        // Shown, never offered to turn off: an unconfined host refuses CLI
        // agent turns (the harness step decides whether that blocks).
        ctx.warn(
            "sandbox",
            "agent sandbox",
            "none on this device: coding CLIs won't run, models chat without tools",
            Some("ligus doctor --sandbox says what is missing".into()),
        );
    }
}

pub(crate) fn run(ctx: &mut Ctx, ops: &mut dyn SetupOps, inputs: &Inputs) {
    let fields: Vec<Field> = steps(ops, inputs).into_iter().flat_map(|s| s.fields).collect();
    let harness = value(inputs, &fields, "harness").unwrap_or("").to_string();
    let label = HARNESSES.iter().find(|(id, _)| *id == harness).map(|(_, l)| *l).unwrap_or("harness");

    // 1. The harness: installed and signed in.
    let endpoint = match harness.as_str() {
        "local" => {
            let e = value(inputs, &fields, "endpoint").unwrap_or("http://localhost:11434").to_string();
            if let Some(p) = url_problem(&e) {
                return ctx.fail("input.endpoint", "endpoint", p, "pass --endpoint https://… (or http://localhost:…)", exit::USAGE);
            }
            e
        }
        "gateway" => {
            let g = value(inputs, &fields, "gateway").unwrap_or("").to_string();
            if !ligus_core::HostModelGateway::valid_name(&g) {
                return ctx.fail("input.gateway", "gateway", format!("'{g}' is not a gateway name"), "ligus hosts lists the gateways hosts offer", exit::USAGE);
            }
            g
        }
        _ => String::new(),
    };
    if !is_http(&harness) {
        let fact = ops.harnesses().into_iter().find(|f| f.id == harness);
        match fact {
            Some(f) if f.installed => {
                let v = f.version.as_deref().map(|v| format!(" {v}")).unwrap_or_default();
                ctx.ok("harness.installed", label, format!("installed{v}"));
                match f.login {
                    LoginState::LoggedIn => ctx.ok("harness.signed-in", "sign-in", f.login_detail.clone()),
                    LoginState::NotLoggedIn => {
                        return ctx.fail(
                            "harness.signed-in",
                            "sign-in",
                            format!("{label} isn't signed in on this device"),
                            format!("run `{}` in a terminal on this device, then run this again", f.login_command),
                            exit::FIXABLE,
                        )
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
                return ctx.fail(
                    "harness.installed",
                    label,
                    "not installed on this device",
                    if f.install_command.is_empty() { format!("install {label}, then run this again") } else { f.install_command },
                    exit::FIXABLE,
                )
            }
            None => {
                return ctx.fail("harness.installed", label, "not found on this device", format!("install {label}, then run this again"), exit::FIXABLE)
            }
        }
        if !ops.sandbox().confined() {
            return ctx.fail(
                "harness.sandbox",
                "agent sandbox",
                format!("{label} turns are refused without an agent sandbox on this device"),
                "ligus doctor --sandbox says what is missing (macOS: Seatbelt; Linux: bwrap or Landlock)",
                exit::FIXABLE,
            );
        }
    }

    // 2. The name.
    let name = value(inputs, &fields, "name").unwrap_or("").to_string();
    let model = value(inputs, &fields, "model").unwrap_or("").to_string();
    let runtimes = ops.runtimes();
    let agents = ops.agents();
    if let Some(a) = agents.iter().find(|a| a.name.eq_ignore_ascii_case(&name)) {
        let same = runtimes.iter().any(|r| r.id == a.runtime_id && r.harness == harness && r.model == model);
        if same {
            ctx.ok("agent", "agent", format!("@{} is already set up on {label}", a.name));
            ctx.result("agentId", a.id.to_string());
            ctx.result("runtimeId", a.runtime_id.clone());
            return verify(ctx, ops, inputs, &a.runtime_id, &name);
        }
    }
    let mut taken: Vec<String> = agents.iter().map(|a| a.name.clone()).collect();
    taken.extend(ops.member_names());
    if let Some(p) = agent_name_problem(&name, &taken) {
        return ctx.fail("input.name", "name", p, "pass another --name", exit::USAGE);
    }
    ctx.ok("name", "name", format!("@{name}"));

    // 3. The model.
    let list = ops.models(&harness, &endpoint);
    if let Some(e) = &list.error {
        if harness == "local" {
            return ctx.fail("model.list", "models", format!("{}: {e}", super::validate::origin(&endpoint)), "start the model server, or check --endpoint", exit::OFFLINE);
        }
        ctx.warn("model.list", "models", format!("couldn't list models: {e}"), None);
    }
    if harness == "local" && model.is_empty() {
        return ctx.fail("input.model", "model", "a local model needs a model name", "pass --model <name> (ollama list shows them)", exit::USAGE);
    }
    if !model.is_empty() && list.complete && !list.models.iter().any(|m| m.id == model) {
        let some: Vec<&str> = list.models.iter().take(6).map(|m| m.id.as_str()).collect();
        return ctx.fail("input.model", "model", format!("'{model}' isn't available here"), format!("pick one of: {}", some.join(", ")), exit::USAGE);
    }
    ctx.ok("model", "model", if model.is_empty() { "the harness's default".to_string() } else { model.clone() });

    // 4. The runtime: reuse one this device may reuse, else define one.
    let reuse = runtimes.iter().find(|r| r.harness == harness && r.model == model && r.endpoint == endpoint).map(|r| r.id.clone());
    let runtime_id = match reuse {
        Some(id) => {
            ctx.ok("runtime", "runtime", format!("reusing '{id}'"));
            id
        }
        None => {
            if ops.runtimes_need_admin() && !gate::need(ctx, ops, WorkspaceRole::Admin, "defining a workspace runtime") {
                return;
            }
            let id = free_runtime_id(&harness, &model, &runtimes.iter().map(|r| r.id.as_str()).collect::<Vec<_>>());
            let spec = RuntimeSpec {
                id: id.clone(),
                name: if model.is_empty() { label.to_string() } else { format!("{label} ({model})") },
                harness: harness.clone(),
                model: model.clone(),
                endpoint: endpoint.clone(),
            };
            if ctx.cancelled() {
                return;
            }
            if ctx.dry_run() {
                ctx.skip("runtime", "runtime", format!("would define runtime '{id}'"));
            } else {
                if let Err(e) = ops.add_runtime(&spec) {
                    return ctx.fail("runtime", "runtime", format!("couldn't define runtime '{id}': {e}"), "ligus doctor checks this device", ops_code(&e));
                }
                let rid = id.clone();
                ctx.undo(format!("runtime '{id}'"), Box::new(move |o| o.remove_runtime(&rid)));
                ctx.ok("runtime", "runtime", format!("defined '{id}'"));
            }
            id
        }
    };
    ctx.result("runtimeId", runtime_id.clone());

    // 5. The agent (the synced write, last).
    if ctx.cancelled() {
        return;
    }
    if ctx.dry_run() {
        ctx.skip("agent", "agent", format!("would create @{name} on '{runtime_id}'"));
        if inputs.on("test-turn") {
            ctx.skip("test-turn", "test turn", "would send one test message");
        }
        return ctx.done();
    }
    match ops.add_agent(&name, &runtime_id) {
        Ok(id) => {
            ctx.ok("agent", "agent", format!("@{name} created"));
            ctx.result("agentId", id.to_string());
            ctx.commit();
        }
        Err(e) => return ctx.fail("agent", "agent", format!("couldn't create @{name}: {e}"), "fix what it says and run this again", ops_code(&e)),
    }
    verify(ctx, ops, inputs, &runtime_id, &name);
}

fn verify(ctx: &mut Ctx, ops: &mut dyn SetupOps, inputs: &Inputs, runtime_id: &str, name: &str) {
    if !ops.agents().iter().any(|a| a.name.eq_ignore_ascii_case(name)) {
        return ctx.fail("verify", "verify", format!("@{name} isn't on the roster"), "ligus agents lists the roster; run this again", exit::FAILED);
    }
    if inputs.on("test-turn") {
        if ctx.cancelled() {
            return;
        }
        match ops.test_turn(runtime_id) {
            Ok(t) if t.ok => ctx.ok("test-turn", "test turn", format!("answered in {} ms", t.latency_ms)),
            Ok(t) => {
                return ctx.fail("test-turn", "test turn", t.detail, "check the sign-in and the model, then run this again with --test-turn", exit::FAILED)
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

//! Connect a tool: an MCP server (a command, or an https URL with an
//! optional bearer token), its handshake on the unsaved spec, which agents
//! may reach it, then one write of spec and reach.

use ligus_core::WorkspaceRole;
use uuid::Uuid;

use super::agent::ops_code;
use super::validate::{arg_looks_secret, mcp_id_problem, origin, split_command, url_problem, value};
use super::{exit, gate, Choice, Ctx, Field, FieldKind, Inputs, Progress, Secrets, SetupOps, StepDef};
use crate::mcp::{McpServerSpec, McpTransport};

pub(crate) fn status(ops: &dyn SetupOps) -> (Progress, String) {
    let n = ops.mcp_servers().iter().filter(|s| s.enabled).count();
    match n {
        0 => (Progress::Todo, "no tools connected".into()),
        1 => (Progress::Done, "1 tool".into()),
        n => (Progress::Done, format!("{n} tools")),
    }
}

pub(crate) fn steps(ops: &dyn SetupOps, _inputs: &Inputs) -> Vec<StepDef> {
    let mut reach = vec![Choice::new("all", "Every agent")];
    reach.extend(ops.agents().into_iter().map(|a| Choice::new(a.name.clone(), format!("@{}", a.name))));
    vec![
        StepDef::input(
            "server",
            "Server",
            vec![
                Field::new("id", "Name", FieldKind::Text).required().help("A short id for the server: lowercase letters, digits and -."),
                Field::new(
                    "kind",
                    "Kind",
                    FieldKind::Choice { options: vec![Choice::new("command", "A command on this device"), Choice::new("url", "A URL (https)")] },
                )
                .required()
                .default("command"),
                Field::new("command", "Command", FieldKind::Text)
                    .required()
                    .when("kind", &["command"])
                    .help("The command and its arguments, e.g. npx -y @modelcontextprotocol/server-filesystem ~/notes. Never put a token here."),
                Field::new("url", "URL", FieldKind::Text).required().when("kind", &["url"]).help("https://, or http:// on this machine."),
            ],
        ),
        StepDef::input(
            "token",
            "Token",
            vec![Field::new("token", "Bearer token", FieldKind::Secret)
                .when("kind", &["url"])
                .help("Only if the server needs one. It stays in this device's secret store and is sent only to this URL's origin.")],
        ),
        StepDef::input(
            "reach",
            "Agents",
            vec![Field::new("reach", "Agents that may use it", FieldKind::Multi { options: reach })
                .default("all")
                .help("Only these agents' turns are offered its tools. The default agent is offered only tools every agent may use.")],
        ),
        StepDef::verify("verify", "Connected"),
    ]
}

pub(crate) fn preflight(ctx: &mut Ctx, ops: &dyn SetupOps) {
    // Through shared agents a tool reaches other people's prompts.
    if !gate::need(ctx, ops, WorkspaceRole::Contributor, "connecting a tool") {
        return;
    }
    let sb = ops.sandbox();
    if !sb.confined() {
        ctx.fail(
            "sandbox",
            "agent sandbox",
            "MCP servers are refused without an agent sandbox on this device",
            "ligus doctor --sandbox says what is missing",
            exit::FIXABLE,
        );
    } else {
        ctx.ok("sandbox", "agent sandbox", sb.tier);
    }
}

/// The spec the inputs describe, or a usage failure.
fn spec_of(ctx: &mut Ctx, inputs: &Inputs, fields: &[Field]) -> Option<McpServerSpec> {
    let id = value(inputs, fields, "id").unwrap_or("").to_string();
    if let Some(p) = mcp_id_problem(&id) {
        ctx.fail("input.id", "name", p, "pass --id <name>", exit::USAGE);
        return None;
    }
    let transport = match value(inputs, fields, "kind").unwrap_or("command") {
        "url" => {
            let url = value(inputs, fields, "url").unwrap_or("").to_string();
            if let Some(p) = url_problem(&url) {
                ctx.fail("input.url", "URL", p, "pass --url https://…", exit::USAGE);
                return None;
            }
            McpTransport::Http { url }
        }
        _ => {
            let Some(words) = split_command(value(inputs, fields, "command").unwrap_or("")) else {
                ctx.fail("input.command", "command", "a quote isn't closed", "check the quoting of --command", exit::USAGE);
                return None;
            };
            let Some((cmd, args)) = words.split_first().filter(|(c, _)| !c.is_empty()) else {
                ctx.fail("input.command", "command", "--command is empty", "pass --command \"<program> <args>\"", exit::USAGE);
                return None;
            };
            let mut prev: Option<&str> = None;
            for a in std::iter::once(cmd).chain(args) {
                if arg_looks_secret(a, prev) {
                    ctx.fail(
                        "input.command",
                        "command",
                        "an argument looks like a token: it would sit on the command line and in the catalog",
                        "give the server its token through its own config or environment, not its arguments",
                        exit::USAGE,
                    );
                    return None;
                }
                prev = Some(a);
            }
            McpTransport::Stdio { command: cmd.clone(), args: args.to_vec() }
        }
    };
    let mut spec = McpServerSpec::new(id, transport);
    spec.enabled = true;
    Some(spec)
}

fn target(t: &McpTransport) -> String {
    match t {
        McpTransport::Stdio { command, args } => std::iter::once(command.as_str()).chain(args.iter().map(String::as_str)).collect::<Vec<_>>().join(" "),
        McpTransport::Http { url } => origin(url),
    }
}

pub(crate) fn run(ctx: &mut Ctx, ops: &mut dyn SetupOps, inputs: &Inputs, secrets: &Secrets) {
    let fields: Vec<Field> = steps(ops, inputs).into_iter().flat_map(|s| s.fields).collect();
    let Some(spec) = spec_of(ctx, inputs, &fields) else { return };
    let token = secrets.get("token");
    if token.is_some() && !matches!(spec.transport, McpTransport::Http { .. }) {
        return ctx.fail("input.token", "token", "a token is for a URL server", "drop --token, or use --kind url", exit::USAGE);
    }
    if let Some(t) = token {
        if let Err(e) = crate::mcp::normalize_bearer_token(t) {
            return ctx.fail("input.token", "token", e, "paste the token alone, without 'Bearer '", exit::USAGE);
        }
    }

    // Lockdown (dearth mode) wins: only servers it allows.
    if ops.lockdown() && !ops.lockdown_mcp_allowed().contains(&spec.id) {
        return ctx.fail(
            "lockdown",
            "lockdown",
            "the workspace is in lockdown, and this server isn't one it allows",
            "an Admin lifts the lockdown, or adds the server to its allowed list",
            exit::REFUSED,
        );
    }

    // Reach.
    let agents = ops.agents();
    let reach_raw = value(inputs, &fields, "reach").unwrap_or("all").to_string();
    let mut reach: Vec<Uuid> = Vec::new();
    let mut names = Vec::new();
    for w in reach_raw.split(',').map(str::trim).filter(|w| !w.is_empty()) {
        if w == "all" {
            continue;
        }
        let n = w.trim_start_matches('@');
        match agents.iter().find(|a| a.name.eq_ignore_ascii_case(n)) {
            Some(a) => {
                if !reach.contains(&a.id) {
                    reach.push(a.id);
                    names.push(format!("@{}", a.name));
                }
            }
            None => return ctx.fail("input.reach", "agents", format!("no agent named '{n}'"), "ligus agents lists them; or pass --reach all", exit::USAGE),
        }
    }
    if reach_raw.split(',').any(|w| w.trim() == "all") && !reach.is_empty() {
        return ctx.fail("input.reach", "agents", "'all' and named agents together", "pass --reach all, or a list of agent names", exit::USAGE);
    }
    reach.sort();
    let reach_text = if reach.is_empty() { "every agent".to_string() } else { names.join(", ") };

    // Already connected the same way: nothing to do.
    if let Some(have) = ops.mcp_servers().into_iter().find(|s| s.id == spec.id) {
        let same = have.target == target(&spec.transport) && {
            let mut a = have.agents.clone();
            a.sort();
            a == reach
        };
        if same && have.enabled && (token.is_none() || have.has_secret) {
            ctx.ok("server", "server", format!("'{}' is already connected for {reach_text}", spec.id));
            return ctx.done();
        }
        return ctx.fail(
            "input.id",
            "name",
            format!("a server named '{}' already exists, set up differently", spec.id),
            "pick another --id, or change that server in Settings > Tools",
            exit::USAGE,
        );
    }
    ctx.ok("server", "server", format!("{} ({})", spec.id, target(&spec.transport)));

    // Handshake on the unsaved spec, under the agent sandbox.
    if ctx.cancelled() {
        return;
    }
    if ctx.dry_run() {
        ctx.skip("handshake", "handshake", "would start the server and list its tools");
        ctx.skip("write", "save", format!("would save '{}' for {reach_text}", spec.id));
        return ctx.done();
    }
    match ops.mcp_handshake(&spec, token) {
        Ok(tools) => {
            ctx.ok("handshake", "handshake", format!("{} tools", tools.len()));
            let shells: Vec<String> = tools.iter().filter(|t| crate::mcp::is_shell_tool(t)).cloned().collect();
            if !shells.is_empty() {
                ctx.warn(
                    "handshake.shell",
                    "shell tools",
                    format!("it offers shell tools ({}): only turns that have the shell get them", shells.join(", ")),
                    None,
                );
            }
        }
        Err(e) => {
            let code = if matches!(spec.transport, McpTransport::Http { .. }) && matches!(e, super::OpsError::Offline(_)) { exit::OFFLINE } else { ops_code(&e) };
            return ctx.fail("handshake", "handshake", e.to_string(), "check the command or URL (and the token), then run this again", code);
        }
    }

    // One write: the token first, then the spec with its reach.
    if ctx.cancelled() {
        return;
    }
    if let (Some(t), McpTransport::Http { url }) = (token, &spec.transport) {
        if let Err(e) = ops.put_mcp_secret(&spec.id, url, t) {
            return ctx.fail("token", "token", format!("couldn't save the token: {e}"), "ligus doctor checks the secret store", ops_code(&e));
        }
        let (id, url) = (spec.id.clone(), url.clone());
        ctx.undo("the token", Box::new(move |o| o.delete_mcp_secret(&id, &url)));
        ctx.ok("token", "token", "saved in this device's secret store");
    }
    if let Err(e) = ops.add_mcp_server(&spec, &reach) {
        return ctx.fail("write", "save", format!("couldn't save '{}': {e}", spec.id), "fix what it says and run this again", ops_code(&e));
    }
    ctx.commit();
    ctx.ok("write", "save", format!("'{}' for {reach_text}", spec.id));

    match ops.mcp_servers().into_iter().find(|s| s.id == spec.id) {
        Some(s) if s.enabled => {
            ctx.result("serverId", spec.id.clone());
            ctx.done();
        }
        _ => ctx.fail("verify", "verify", format!("'{}' isn't listed as enabled", spec.id), "Settings > Tools lists the servers", exit::FAILED),
    }
}

//! `ligus setup`: the guided setup flows on the command line, on the shared
//! engine (`ligus_runtime::setup`, docs/design/setup-flows.md).
//!
//! * `ligus setup` lists the seven flows and what is already done here.
//! * `ligus setup <flow> [--<field> <value> …] [--json] [--dry-run]` runs
//!   one. Every field is a flag; a secret field takes `-` (stdin) or
//!   `@FILE` (owner-only), never its value on the command line.
//! * `ligus setup runtimes [--apply]` is the earlier `ligus setup`.

use std::path::{Path, PathBuf};

use anyhow::Result;
use ligus_core::WorkspaceRole;
use ligus_runtime::checks::{Check, Report, Status};
use ligus_runtime::cli_detect::DetectEnv;
use ligus_runtime::mcp::{McpServerSpec, McpTransport};
use ligus_runtime::model_catalog::ModelList;
use ligus_runtime::setup::{self, service, FieldKind, FlowId, Inputs, Outcome, Progress, RunOpts, Secrets, Surface};
use ligus_runtime::setup::{
    AgentEdit, AgentFact, BoxAsk, BoxFact, FolderFact, HarnessFact, HostFact, HostSourceFact, InviteFact, McpFact, OpsError, OpsResult, ProbeFact, RepoFact, RuntimeFact, RuntimeSpec, ServeFact,
    SetupOps, TestTurn,
};
use ligus_runtime::{ChatService, McpReach};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{help, mcp_config, Config};

/// `ligus setup …`. `rest` is everything after `setup`.
pub fn cmd(cfg: &Config, rest: &[String]) -> Result<()> {
    let positional = crate::positional_args(rest);
    let json = rest.iter().any(|a| a == "--json");
    match positional.first().map(|s| s.as_str()) {
        None if rest.iter().any(|a| a == "--apply") => {
            eprintln!("ligus: `ligus setup --apply` is now `ligus setup runtimes --apply`; this spelling goes in the next release");
            crate::cmd_setup(cfg, true)
        }
        None => list(cfg, json),
        Some("runtimes") => crate::cmd_setup(cfg, rest.iter().any(|a| a == "--apply")),
        Some("agent") => run_flow(cfg, FlowId::Agent, rest),
        Some("tool") => run_flow(cfg, FlowId::Tool, rest),
        Some("folder") => run_flow(cfg, FlowId::Folder, rest),
        Some("box") => run_flow(cfg, FlowId::Box, rest),
        Some("phone") => run_flow(cfg, FlowId::Phone, rest),
        Some("invite") => run_flow(cfg, FlowId::Invite, rest),
        Some("relay") => run_flow(cfg, FlowId::Relay, rest),
        Some(other) => Err(help::usage_error(format!(
            "unknown `ligus setup {other}`: use agent, tool, folder, box, phone, invite, relay or runtimes"
        ))),
    }
}

/// `ligus setup`: the flows and what is done, as checks (`<flow>.status`).
fn list(cfg: &Config, json: bool) -> Result<()> {
    let ops = CliOps::open(cfg)?;
    let mut r = Report::new("setup");
    for s in setup::status(&ops) {
        let id = format!("{}.status", s.flow.id.as_str());
        let c = match s.progress {
            Progress::Done => Check::ok(&id, s.flow.title, s.detail),
            Progress::Waiting => Check::warn(&id, s.flow.title, s.detail).fix(format!("ligus setup {}", s.flow.id.as_str())),
            Progress::Todo => Check::skip(&id, s.flow.title, s.detail).fix(format!("ligus setup {}", s.flow.id.as_str())),
            Progress::Unavailable => Check::skip(&id, s.flow.title, s.detail),
        };
        if s.progress == Progress::Todo && r.next.len() < 2 {
            r.next(format!("ligus setup {}", s.flow.id.as_str()));
        }
        r.add(c);
    }
    if json {
        println!("{}", r.to_json(0));
        return Ok(());
    }
    for c in &r.checks {
        let mark = match c.status {
            Status::Ok => "done ",
            Status::Warn => "wait ",
            _ => "     ",
        };
        let flow = c.id.split('.').next().unwrap_or("");
        println!("{mark} {:<7} {:<18} {}", flow, c.title, c.detail);
    }
    println!("\nRun one: ligus setup <flow> --help (flags, checks, exit codes). Every flow is safe to run again.");
    Ok(())
}

/// Parse a flow's flags against its fields, run it, print the outcome, and
/// exit with its code.
fn run_flow(cfg: &Config, flow: FlowId, rest: &[String]) -> Result<()> {
    run_flow_as(cfg, flow, rest, None)
}

/// `ligus agents set <agent> --model M --runs-on H`: the agent flow's
/// Change model… and Runs on… (`--agent` set), with `rerun` as the command
/// a hand-off that didn't finish says to run again.
pub fn change_agent(cfg: &Config, args: &[String], rerun: String) -> Result<()> {
    run_flow_as(cfg, FlowId::Agent, args, Some(rerun))
}

fn run_flow_as(cfg: &Config, flow: FlowId, rest: &[String], rerun_as: Option<String>) -> Result<()> {
    let mut ops = CliOps::open(cfg)?;
    // Changing an existing agent (`--agent`) has its own fields.
    let mut base = Inputs::new();
    if flow == FlowId::Agent {
        if let Some(v) = rest.iter().position(|a| a == "--agent").and_then(|i| rest.get(i + 1)) {
            base.set("agent", v.clone());
        }
    }
    let fields: Vec<setup::Field> = setup::form(flow, &ops, &base).fields().cloned().collect();
    let mut inputs = Inputs::new();
    let mut secrets = Secrets::new();
    let (mut json, mut dry_run) = (false, false);
    let mut stdin = crate::secret_arg::stdin_once();
    // The passkey hand-off to your desktop or phone (unless a recovery-words file
    // confirms instead): how long to wait for it, or not at all.
    let mut recovery = false;
    let mut handoff_wait = Some(crate::handoff::DEFAULT_TIMEOUT);
    let mut admit_wait = None;
    let mut i = 0;
    let mut seen_flow = false;
    while i < rest.len() {
        let a = rest[i].as_str();
        i += 1;
        match a {
            "--json" => json = true,
            "--dry-run" => dry_run = true,
            // Your presence for a write the rules hold for its author's
            // passkey (a workspace runtime, a repository): the CLI has no
            // passkey, so the recovery key confirms it, as for approvals.
            "--recovery-words-file" => {
                let path = rest.get(i).ok_or_else(|| help::usage_error("--recovery-words-file needs a path"))?;
                i += 1;
                let key = crate::secret_arg::recovery_key_from_file(Path::new(path))?;
                ops.svc.set_presence_provider(Some(std::sync::Arc::new(crate::admit::RecoveryPresence(key))));
                recovery = true;
            }
            // A step that needs your passkey goes to your desktop or phone: post it
            // and exit 4 (`ligus handoff wait` finishes it), or wait so long.
            "--no-wait" => handoff_wait = None,
            "--timeout" => {
                let v = rest.get(i).ok_or_else(|| help::usage_error("--timeout needs a duration (10m, 90s)"))?;
                i += 1;
                handoff_wait = Some(crate::cli_flags::parse_duration(v)?);
                admit_wait = handoff_wait;
            }
            _ if !a.starts_with("--") => {
                if !seen_flow && a == flow.as_str() {
                    seen_flow = true;
                    continue;
                }
                // Not echoed: it may be a code pasted where a flag was meant.
                return Err(help::usage_error(format!(
                    "unexpected argument for `ligus setup {}` (not shown); secrets go in --code or --token as - or @FILE",
                    flow.as_str()
                )));
            }
            _ => {
                let name = &a[2..];
                if flow == FlowId::Agent && matches!(name, "harness" | "gateway") {
                    // The 0.4 spellings of --source (--harness H, --gateway G).
                    let v = rest.get(i).ok_or_else(|| help::usage_error(format!("{a} needs a value")))?;
                    i += 1;
                    inputs.set(name, v.clone());
                    continue;
                }
                let Some(f) = fields.iter().find(|f| f.id == name) else {
                    return Err(help::usage_error(format!("`ligus setup {}` has no flag {a}", flow.as_str())));
                };
                match f.kind {
                    FieldKind::Toggle => inputs.set(f.id, "true"),
                    FieldKind::Secret => {
                        let v = rest.get(i).cloned();
                        i += 1;
                        let secret = read_secret(a, v, &mut stdin)?;
                        secrets.set(f.id, secret);
                    }
                    _ => {
                        let v = rest.get(i).ok_or_else(|| help::usage_error(format!("{a} needs a value")))?;
                        i += 1;
                        inputs.set(f.id, v.clone());
                    }
                }
            }
        }
    }
    ops.json = json;
    if let Some(w) = admit_wait {
        ops.admit_wait = w;
    }
    let handoff = if recovery || dry_run {
        None
    } else {
        let label = ops.label.clone();
        crate::handoff::HandoffPresence::for_service(cfg, &ops.svc, &label, handoff_wait).map(|(p, end)| {
            ops.svc.set_presence_provider(Some(std::sync::Arc::new(p)));
            end
        })
    };
    let mut out = setup::run(flow, &mut ops, &inputs, &secrets, &RunOpts { dry_run, ..Default::default() });
    if let Some(end) = handoff.and_then(|e| e.lock().unwrap().clone()) {
        let quote = |a: &str| if a.chars().any(char::is_whitespace) { format!("'{a}'") } else { a.to_string() };
        let rerun = rerun_as.clone().unwrap_or_else(|| format!("ligus setup {}", rest.iter().filter(|a| *a != "--no-wait").map(|a| quote(a)).collect::<Vec<_>>().join(" ")));
        if let Some(next) = crate::handoff::next_lines(&end, &rerun) {
            // The step's fix says where to confirm it now, not the desktop-only line.
            if let Some(c) = out.report.checks.iter_mut().rev().find(|c| c.fix.as_deref().is_some_and(|f| f.contains("--recovery-words-file"))) {
                c.fix = Some(next[0].clone());
            }
            out.report.next = next;
        }
    }
    print_outcome(&out, json, dry_run);
    if out.exit != 0 {
        std::process::exit(out.exit);
    }
    Ok(())
}

/// A secret flag's value: `-` reads stdin, `@PATH` an owner-only file. A
/// value on the command line is refused without being echoed (it is
/// already in ps and the shell history: rotate it).
fn read_secret(flag: &str, v: Option<String>, stdin: &mut dyn FnMut() -> std::io::Result<String>) -> Result<Zeroizing<String>> {
    match v.as_deref() {
        Some("-") => Ok(Zeroizing::new(stdin()?.trim().to_string())),
        Some(p) if p.starts_with('@') => Ok(Zeroizing::new(crate::secret_arg::read_owner_only(Path::new(&p[1..]), 64 * 1024)?.trim().to_string())),
        Some(_) => Err(help::usage_error(format!(
            "{flag} takes - (stdin) or @FILE (owner-only), never the secret itself on the command line; if you typed one, rotate it"
        ))),
        None => Err(help::usage_error(format!("{flag} needs - (stdin) or @FILE"))),
    }
}

fn print_outcome(o: &Outcome, json: bool, dry_run: bool) {
    if json {
        println!("{}", o.to_json(dry_run));
        return;
    }
    let color = crate::cli_flags::stdout_color();
    for c in &o.report.checks {
        print!("{}", Report::check_text_styled(c, color));
    }
    let head = if o.done && o.exit == 0 {
        if dry_run { "dry run: nothing was written".to_string() } else { "done".to_string() }
    } else if o.exit == setup::exit::PERSON {
        "waiting for a person".to_string()
    } else {
        o.report.summary()
    };
    let next = if o.report.next.is_empty() { String::new() } else { format!(" · next: {}", o.report.next.join(" ; ")) };
    println!("{head}{next}");
}

// ── CliOps ──────────────────────────────────────────────────────────────────

/// The engine's side on the CLI: this data dir, its workspace, its service.
pub struct CliOps {
    cfg: Config,
    svc: ChatService,
    ws: Uuid,
    env: DetectEnv,
    label: String,
    /// What the workspace's hosts list, read once per command (it fetches
    /// the hosts' beats from the relay).
    host_sources: std::cell::OnceCell<Vec<HostSourceFact>>,
    /// How long `ligus setup box --to` waits for an approver (`--timeout`).
    admit_wait: std::time::Duration,
    /// `--json`: what the code-match step prints goes to stderr.
    json: bool,
    /// Ask at a terminal whether the approver's screen shows the code (off
    /// in tests).
    ask_on_tty: bool,
    /// Hears the code as it is shown (tests).
    code_tx: Option<std::sync::mpsc::Sender<String>>,
}

impl CliOps {
    pub fn open(cfg: &Config) -> Result<Self> {
        let svc = crate::open_service(cfg)?;
        let label = crate::env_opt("LIGUS_HOST_LABEL").or_else(|| crate::env_opt("HOSTNAME")).unwrap_or_else(|| "this host".into());
        Ok(CliOps {
            cfg: cfg.clone(),
            ws: crate::uuid_of_room(&cfg.room),
            svc,
            env: DetectEnv::from_process(),
            label,
            host_sources: Default::default(),
            admit_wait: crate::approvals_cmd::DEFAULT_WAIT,
            json: false,
            ask_on_tty: true,
            code_tx: None,
        })
    }

    /// Use `p` for the steps that need the person's passkey (tests: a
    /// recovery key standing in for the desktop).
    #[cfg(test)]
    pub(crate) fn set_presence(&mut self, p: std::sync::Arc<dyn ligus_runtime::chat_service::PresenceProvider>) {
        self.svc.set_presence_provider(Some(p));
    }

    fn reach(&self) -> Result<McpReach, OpsError> {
        McpReach::load(&self.cfg.data_dir).map_err(OpsError::Failed)
    }

    fn joined(&self) -> bool {
        self.cfg.relay_url.is_some() && self.svc.my_workspace_role(self.ws).is_some()
    }
}

/// What the box's code-match step came to, as the engine reads it. A relay
/// without the mailbox routes falls back to the paste.
fn box_ask(done: Result<(String, String)>) -> BoxAsk {
    match done {
        Ok((workspace, approver)) => BoxAsk::Joined { workspace, by: approver },
        Err(e) => {
            let (exit, code) = crate::problem::classify(&e);
            if code == "no_mailbox_routes" {
                return BoxAsk::NoMailbox;
            }
            let fix = e.chain().find_map(|c| c.downcast_ref::<crate::problem::Problem>()).and_then(|p| p.fix.clone());
            BoxAsk::Stopped {
                exit,
                why: ligus_runtime::secret_redact::redacted(&format!("{e:#}")),
                fix: fix.unwrap_or_else(|| "start again: ligus setup box --to ligusaddr1:…".into()),
            }
        }
    }
}

/// The desktop app's data dir on this OS (a box must never use it).
pub fn desktop_data_dirs() -> Vec<PathBuf> {
    let Some(home) = crate::env_opt("HOME").map(PathBuf::from) else { return Vec::new() };
    let id = ligus_core::names::DESKTOP_APP_ID;
    let mut v = vec![home.join("Library/Application Support").join(id), home.join(".local/share").join(id)];
    if let Some(x) = crate::env_opt("XDG_DATA_HOME") {
        v.push(PathBuf::from(x).join(id));
    }
    if let Some(a) = crate::env_opt("APPDATA") {
        v.push(PathBuf::from(a).join(id));
    }
    v
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

impl SetupOps for CliOps {
    fn surface(&self) -> Surface {
        Surface::Cli
    }
    fn my_role(&self) -> Option<WorkspaceRole> {
        service::my_role(&self.svc, self.ws)
    }
    fn lockdown(&self) -> bool {
        service::lockdown(&self.svc, self.ws).0
    }
    fn lockdown_mcp_allowed(&self) -> Vec<String> {
        service::lockdown(&self.svc, self.ws).1
    }
    fn relay_url(&self) -> Option<String> {
        self.cfg.relay_url.clone()
    }

    // agent
    fn harnesses(&self) -> Vec<HarnessFact> {
        service::harnesses(&self.env, true)
    }
    fn models(&self, harness: &str, endpoint: &str) -> ModelList {
        service::models(harness, endpoint, &self.env)
    }
    fn runtimes(&self) -> Vec<RuntimeFact> {
        // A runtime naming a secret reference is reused only for the same
        // reference (its key stays on the host that declares it).
        service::workspace_runtimes(&self.svc, self.ws)
    }
    fn runtimes_need_admin(&self) -> bool {
        true
    }
    fn runtime_on_log(&self, _shared: bool) -> bool {
        // The CLI's runtimes are workspace runtimes (a worker or box on this
        // data dir runs them too).
        true
    }
    fn passkey_writes(&self) -> bool {
        self.joined()
    }
    fn confirming(&mut self, step: usize, of: usize, what: &str) {
        if of > 1 {
            eprintln!("passkey {step} of {of}: {what}");
        }
    }
    fn host_sources(&self) -> Vec<HostSourceFact> {
        self.host_sources
            .get_or_init(|| {
                // The hosts' beats come from the relay, on a thread of their
                // own (a caller may already be inside an async runtime), read
                // and checked there against this data dir's own fold.
                let cfg = self.cfg.clone();
                let ws = self.ws;
                std::thread::spawn(move || {
                    let Ok(svc) = crate::open_service(&cfg) else { return Vec::new() };
                    let presence = crate::fetch_presence_with(&cfg, Some(&svc));
                    let now = ligus_core::Timestamp::now();
                    service::host_sources(&svc, ws, |id| presence.status_of(id), |h| presence.is_online(h, &now))
                })
                .join()
                .unwrap_or_default()
            })
            .clone()
    }
    fn this_device_label(&self) -> String {
        self.label.clone()
    }
    fn add_runtime(&mut self, spec: &RuntimeSpec) -> OpsResult<()> {
        service::add_workspace_runtime(&mut self.svc, self.ws, spec)
    }
    fn remove_runtime(&mut self, id: &str) -> OpsResult<()> {
        self.svc.remove_workspace_runtime(self.ws, id).map_err(OpsError::from)
    }
    fn agents(&self) -> Vec<AgentFact> {
        service::agents(&self.svc, self.ws)
    }
    fn member_names(&self) -> Vec<String> {
        service::member_names(&self.svc, self.ws)
    }
    fn add_agent(&mut self, name: &str, runtime_id: &str) -> OpsResult<Uuid> {
        service::add_agent(&mut self.svc, self.ws, name, runtime_id)
    }
    fn add_agent_placed(&mut self, name: &str, runtime_id: &str, host_id: &str) -> OpsResult<Uuid> {
        service::add_agent_placed(&mut self.svc, self.ws, name, runtime_id, host_id)
    }
    fn change_agent(&mut self, agent_id: Uuid, edit: &AgentEdit) -> OpsResult<()> {
        service::change_agent(&mut self.svc, self.ws, agent_id, edit)
    }
    fn rules(&self) -> u32 {
        service::rules(&self.svc, self.ws)
    }
    fn local_servers(&self, agent_id: Uuid) -> bool {
        service::local_servers(&self.svc, self.ws, agent_id)
    }
    fn set_local_servers(&mut self, agent_id: Uuid, on: bool) -> OpsResult<()> {
        service::set_local_servers(&mut self.svc, self.ws, agent_id, on)
    }
    fn test_turn(&mut self, runtime_id: &str) -> OpsResult<TestTurn> {
        self.test_turn_with(runtime_id, false)
    }
    fn test_turn_with(&mut self, runtime_id: &str, local_servers: bool) -> OpsResult<TestTurn> {
        let wr = self
            .svc
            .list_workspace_runtimes(self.ws)
            .map_err(OpsError::from)?
            .into_iter()
            .find(|r| r.id == runtime_id)
            .ok_or_else(|| OpsError::Failed(format!("no runtime '{runtime_id}'")))?;
        let rt = crate::workspace_runtime_resolved(self.ws, &wr);
        service::block_on(async move { service::ping_with(&rt, None, local_servers).await }).ok_or_else(|| OpsError::Failed("couldn't run the test turn".into()))
    }

    // tool
    fn mcp_servers(&self) -> Vec<McpFact> {
        let reach = self.reach().unwrap_or_default();
        mcp_config::managed_entries(&self.cfg.data_dir)
            .unwrap_or_default()
            .into_iter()
            .map(|e| McpFact {
                transport: if e.transport == "stdio" { "stdio".into() } else { "http".into() },
                target: match &e.url {
                    Some(u) => setup::validate::origin(u),
                    None => std::iter::once(e.command.clone().unwrap_or_default()).chain(e.args.clone()).collect::<Vec<_>>().join(" "),
                },
                enabled: e.enabled,
                has_secret: e.token_secret,
                agents: reach.get(self.ws, &e.id),
                id: e.id,
            })
            .collect()
    }
    fn mcp_handshake(&mut self, spec: &McpServerSpec, token: Option<&Zeroizing<String>>) -> OpsResult<Vec<String>> {
        service::mcp_handshake(spec, token)
    }
    fn put_mcp_secret(&mut self, id: &str, url: &str, secret: &Zeroizing<String>) -> OpsResult<()> {
        // Bound to the URL's origin, in the store (not the editable file).
        let store = ligus_runtime::secret_store::for_data_dir(&self.cfg.data_dir);
        mcp_config::save_token(store.as_ref(), id, url, secret).map_err(|e| OpsError::Failed(format!("{e:#}")))
    }
    fn delete_mcp_secret(&mut self, id: &str, _url: &str) -> OpsResult<()> {
        let store = ligus_runtime::secret_store::for_data_dir(&self.cfg.data_dir);
        mcp_config::delete_token(store.as_ref(), id).map_err(|e| OpsError::Failed(format!("{e:#}")))
    }
    fn add_mcp_server(&mut self, spec: &McpServerSpec, reach: &[Uuid]) -> OpsResult<()> {
        let mut entries = mcp_config::managed_entries(&self.cfg.data_dir).map_err(OpsError::from)?;
        if entries.iter().any(|e| e.id == spec.id) {
            return Err(OpsError::Failed(format!("'{}' is already in {}", spec.id, mcp_config::MANAGED_FILE)));
        }
        // A token counts only when it was saved for this URL's origin; one
        // left from an earlier server of the same id is dropped.
        let store = ligus_runtime::secret_store::for_data_dir(&self.cfg.data_dir);
        let has_token = match &spec.transport {
            McpTransport::Http { url } => mcp_config::token_for(store.as_ref(), &spec.id, url).is_some(),
            McpTransport::Stdio { .. } => false,
        };
        if !has_token {
            mcp_config::delete_token(store.as_ref(), &spec.id).map_err(|e| OpsError::Failed(format!("{e:#}")))?;
        }
        let entry = match &spec.transport {
            McpTransport::Stdio { command, args } => mcp_config::ManagedEntry {
                id: spec.id.clone(),
                transport: "stdio".into(),
                command: Some(command.clone()),
                args: args.clone(),
                url: None,
                enabled: true,
                token_secret: false,
            },
            McpTransport::Http { url } => mcp_config::ManagedEntry {
                id: spec.id.clone(),
                transport: "http".into(),
                command: None,
                args: Vec::new(),
                url: Some(url.clone()),
                enabled: true,
                token_secret: has_token,
            },
        };
        // Reach first: if the server entry can't be written, an orphan reach
        // only narrows a server that doesn't exist.
        let mut r = self.reach()?;
        r.set(self.ws, &spec.id, reach);
        r.save(&self.cfg.data_dir).map_err(OpsError::Failed)?;
        entries.push(entry);
        mcp_config::save_managed(&self.cfg.data_dir, entries).map_err(OpsError::from)
    }
    fn remove_mcp_server(&mut self, id: &str) -> OpsResult<()> {
        let mut entries = mcp_config::managed_entries(&self.cfg.data_dir).map_err(OpsError::from)?;
        entries.retain(|e| e.id != id);
        mcp_config::save_managed(&self.cfg.data_dir, entries).map_err(OpsError::from)?;
        let store = ligus_runtime::secret_store::for_data_dir(&self.cfg.data_dir);
        mcp_config::delete_token(store.as_ref(), id).map_err(|e| OpsError::Failed(format!("{e:#}")))
    }

    // folder
    fn repos(&self) -> Vec<RepoFact> {
        service::repos(&self.svc, self.ws)
    }
    fn inspect_folder(&self, path: &str) -> FolderFact {
        let abs = crate::repos_cmd::abs(path).map(|p| p.display().to_string()).unwrap_or_else(|_| path.to_string());
        service::inspect_folder(&self.svc, self.ws, &abs)
    }
    fn add_repo(&mut self, folder: &FolderFact, name: &str, branch: &str) -> OpsResult<String> {
        service::add_repo(&mut self.svc, self.ws, folder, name, branch)
    }
    fn bind_folder(&mut self, repo_id: &str, path: &str) -> OpsResult<()> {
        let abs = crate::repos_cmd::abs(path).map(|p| p.display().to_string()).unwrap_or_else(|_| path.to_string());
        service::bind_folder(&self.svc, self.ws, &self.label, repo_id, &abs)
    }
    fn create_scratch(&mut self, name: &str) -> OpsResult<(String, String)> {
        let root = crate::scratch_cmd::default_root(&self.cfg.data_dir);
        service::create_scratch(&mut self.svc, self.ws, &self.label, &root, &crate::scratch_cmd::forbidden(), name)
    }

    // box
    fn box_state(&self) -> OpsResult<BoxFact> {
        let ids = ligus_runtime::IdentityStore::new(&self.cfg.data_dir, ligus_runtime::secret_store::StoreKeyVault::for_data_dir(&self.cfg.data_dir));
        let person = ids.load().ok().flatten().is_some_and(|s| !s.service);
        if person && self.joined() {
            return Err(OpsError::Failed(
                "this data dir is a person's device in a workspace; a box needs its own (LIGUS_DATA_DIR=~/.ligus-serve)".into(),
            ));
        }
        Ok(BoxFact {
            desktop_data_dir: desktop_data_dirs().iter().any(|d| same_dir(d, &self.cfg.data_dir)),
            joined: self.joined(),
            workspace: (self.cfg.room != "default").then(|| self.cfg.room.clone()),
            relay: self.cfg.relay_url.clone(),
        })
    }
    fn join_request(&mut self, label: &str) -> OpsResult<(String, String)> {
        let label = if label.is_empty() { self.label.clone() } else { label.to_string() };
        let r = crate::admit::join_request(&self.cfg, &label).map_err(OpsError::from)?;
        Ok((r.encode(), r.fingerprint()))
    }
    fn ask_to_join(&mut self, to: &str, label: &str) -> OpsResult<BoxAsk> {
        let label = if label.is_empty() { self.label.clone() } else { label.to_string() };
        let mut o = crate::approvals_cmd::ToOptions::new(to.to_string(), label, self.admit_wait, false, self.json);
        o.code_tx = self.code_tx.clone();
        let cfg = self.cfg.clone();
        // At a terminal the box asks whether the approver's screen shows the code.
        let interactive = self.ask_on_tty && std::io::IsTerminal::is_terminal(&std::io::stdin());
        let done = service::block_on(async move {
            let mut ask = crate::devices_cmd::confirm_on_tty;
            let mut r = Report::new("setup box");
            crate::approvals_cmd::ask_as_box(&cfg, &o, interactive.then_some(&mut ask as crate::approvals_cmd::Ask<'_>), &mut r)
                .await
                .map(|a| (a.workspace, a.approver))
        })
        .ok_or_else(|| OpsError::Failed("the code-match step stopped".into()))?;
        Ok(box_ask(done))
    }
    fn redeem(&mut self, code: &Zeroizing<String>) -> OpsResult<String> {
        let r = crate::admit::redeem(&mut self.cfg, code.trim()).map_err(OpsError::from)?;
        Ok(r.room)
    }
    fn serve_running(&self) -> ServeFact {
        let listen = crate::env_opt("LIGUS_SERVE_LISTEN").unwrap_or_else(|| crate::serve::DEFAULT_LISTEN.into());
        let url = format!("http://{listen}/v1/health");
        let ok = service::block_on(async move {
            let c = reqwest::Client::builder().timeout(std::time::Duration::from_secs(3)).build().ok()?;
            Some(c.get(&url).send().await.ok()?.status().is_success())
        })
        .flatten()
        .unwrap_or(false);
        if ok {
            ServeFact::Running(listen)
        } else {
            ServeFact::NotRunning(format!("nothing answers on {listen}"))
        }
    }
    fn hosts(&self) -> Vec<HostFact> {
        service::hosts(&self.svc, self.ws)
    }

    // phone
    fn phone_precheck(&self) -> OpsResult<Vec<Check>> {
        let mut v = Vec::new();
        v.push(if self.joined() {
            Check::ok("phone.precheck.member", "box", "a member of the workspace")
        } else {
            Check::fail("phone.precheck.member", "box", "this box isn't a member yet").fix("ligus setup box")
        });
        v.push(match self.serve_running() {
            ServeFact::Running(at) => Check::ok("phone.precheck.serve", "ligus serve", format!("answering on {at}")),
            ServeFact::NotRunning(why) => Check::fail("phone.precheck.serve", "ligus serve", why).fix("deploy/local/ligus-local start --role serve"),
        });
        Ok(v)
    }

    fn phones(&self) -> Vec<String> {
        service::phones(&self.svc, self.ws)
    }

    // invite
    fn invites(&self) -> Vec<InviteFact> {
        service::invites(&self.svc, self.ws)
    }

    // relay
    fn probe_relay(&self, url: &str, token: Option<&Zeroizing<String>>) -> ProbeFact {
        // The saved access token goes only to the relay it was saved for.
        let same = self.cfg.relay_url.as_deref().is_some_and(|r| setup::validate::same_origin(r, url));
        let token = token.cloned().or_else(|| same.then(|| self.cfg.token.clone().map(Zeroizing::new)).flatten());
        service::probe_relay(url, token.as_ref(), &self.cfg.room)
    }
    fn switch_relay(&mut self, url: &str, token: Option<&Zeroizing<String>>) -> OpsResult<()> {
        if crate::env_opt("LIGUS_RELAY_URL").is_some() {
            return Err(OpsError::Failed("LIGUS_RELAY_URL is set and wins over config.toml: change it there".into()));
        }
        let mut f = crate::load_file_config(&self.cfg.data_dir);
        let moved = !self.cfg.relay_url.as_deref().is_some_and(|r| setup::validate::same_origin(r, url));
        f.relay_url = Some(url.to_string());
        match token {
            Some(t) => f.token = Some(t.to_string()),
            // The old relay's token never follows the workspace to another.
            None if moved => f.token = None,
            None => {}
        }
        // Secrets go to the store; config.toml is written to a temp file
        // and renamed.
        f.save(&self.cfg.data_dir).map_err(OpsError::from)?;
        self.cfg.relay_url = Some(url.to_string());
        match token {
            Some(t) => self.cfg.token = Some(t.to_string()),
            None if moved => self.cfg.token = None,
            None => {}
        }
        Ok(())
    }
}

/// Every field of every flow is a documented flag of `ligus setup <flow>`.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_field_is_a_documented_flag() {
        struct NoOps;
        impl SetupOps for NoOps {
            fn surface(&self) -> Surface {
                Surface::Cli
            }
        }
        for flow in FlowId::ALL {
            let name = format!("setup {}", flow.as_str());
            let c = help::COMMANDS.iter().find(|c| c.name == name).unwrap_or_else(|| panic!("no help for {name}"));
            for f in setup::form(flow, &NoOps, &Inputs::new()).fields() {
                let flag = format!("--{}", f.id);
                assert!(c.flags.iter().any(|h| h.flag.split([' ', ',']).any(|w| w == flag)), "{name}: {flag} isn't documented");
            }
        }
    }

    #[test]
    fn secrets_are_never_taken_from_argv() {
        let mut no_stdin = || -> std::io::Result<String> { panic!("stdin read") };
        let e = read_secret("--token", Some("hrt1-canary-value".into()), &mut no_stdin).unwrap_err();
        assert!(help::is_usage(&e));
        assert!(!format!("{e:#}").contains("hrt1-canary-value"), "{e:#}");
        let mut stdin = || Ok("  tok\n".to_string());
        assert_eq!(read_secret("--token", Some("-".into()), &mut stdin).unwrap().as_str(), "tok");
    }

    /// `ligus setup box --to`: the box asks through the mailbox, the Admin
    /// approves with the code it shows, and the box keeps the admission
    /// (after finding the approval on the workspace log) with nothing pasted.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn setup_box_to_joins_by_code_match() {
        use crate::approvals_cmd_tests as t;
        let w = t::world().await;
        let addr = t::address(&w).await.encode();
        let boxc = t::fresh(&w.root.join("box"));
        let (tx, rx) = std::sync::mpsc::channel();
        let bc = boxc.clone();
        let boxed = tokio::task::spawn_blocking(move || {
            let mut ops = CliOps::open(&bc).unwrap();
            ops.ask_on_tty = false;
            ops.json = true;
            ops.code_tx = Some(tx);
            ops.ask_to_join(&addr, "build-1")
        });
        let rid = t::first_rid(&w).await;
        let mut typed = || Ok(rx.recv_timeout(std::time::Duration::from_secs(30))?);
        crate::approvals_cmd::cmd_approve(&w.desk, t::approve(&w, &rid), Some(&mut typed)).await.unwrap();
        match boxed.await.unwrap().unwrap() {
            BoxAsk::Joined { workspace, by: approver } => {
                assert_eq!(workspace, w.room);
                assert!(!approver.is_empty());
            }
            other => panic!("not admitted: {other:?}"),
        }
        assert_eq!(crate::load_file_config(&boxc.data_dir).room.as_deref(), Some(w.room.as_str()), "the admission is kept");
    }

    /// A relay without the approval mailbox routes: the box falls back to
    /// the paste (the flow prints the join request), and nothing is kept.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn setup_box_to_falls_back_to_the_paste_without_a_mailbox() {
        use crate::approvals_cmd_tests as t;
        let w = t::world().await;
        let mut addr = t::address(&w).await;
        addr.relay = t::spawn(axum::Router::new().route("/v1/health", axum::routing::get(|| async { "ok" }))).await;
        let boxc = t::fresh(&w.root.join("box"));
        let bc = boxc.clone();
        let to = addr.encode();
        let got = tokio::task::spawn_blocking(move || {
            let mut ops = CliOps::open(&bc).unwrap();
            ops.ask_on_tty = false;
            ops.ask_to_join(&to, "build-1")
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(got, BoxAsk::NoMailbox);
        assert!(crate::load_file_config(&boxc.data_dir).room.is_none(), "nothing kept");
        // A damaged address is a usage error, not a fallback.
        let bc = boxc.clone();
        let got = tokio::task::spawn_blocking(move || CliOps::open(&bc).unwrap().ask_to_join("ligusaddr1:nope", "x")).await.unwrap().unwrap();
        assert!(matches!(got, BoxAsk::Stopped { exit: 2, .. }), "{got:?}");
    }

    #[test]
    fn the_desktop_data_dir_is_known() {
        let dirs = desktop_data_dirs();
        assert!(dirs.iter().all(|d| d.ends_with(ligus_core::names::DESKTOP_APP_ID)));
    }
}

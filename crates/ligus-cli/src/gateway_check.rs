//! `ligus gateway-check`: prove this host's model gateways work end to end,
//! the two ways agent turns use them (docs/design/v2/model-gateways.md):
//!
//! 1. **hosted**: a one-message turn through `provider::dispatch::stream`,
//!    the call serve and workers make for an HTTP-model agent, with the
//!    gateway's credential and attribution;
//! 2. **cli**: a confined agent child (this binary in probe mode, standing in
//!    for a CLI) that gets the egress proxy's gateway route as its base URL.
//!    It dumps its environment and argv, tries to read the credential files,
//!    sends a request carrying a credential of its own, and tries the gateway
//!    directly. The check fails if the child saw the credential, read a
//!    credential file, reached the gateway directly, or didn't get an answer.
//!
//! Exit 0 when every check passes, 1 otherwise (6 with --legacy-exit-codes). Nothing secret is printed.
//! `ligus doctor` runs the same checks ([`checks`]).

use std::time::Duration;

use anyhow::{anyhow, Result};
use ligus_core::ModelProviderKind;
use ligus_runtime::executor::{self, HostKind, Purpose, RuntimeScope, SpawnSpec, TurnPolicy};
use ligus_runtime::model_gateway::{self, Api, GatewaySelection};
use ligus_runtime::provider::{ChatTurn, ResolvedRuntime};
use uuid::Uuid;

use crate::checks::{Check, Status};

/// The probe subcommand the child runs (hidden).
pub const PROBE_ARG: &str = "__gateway-probe";

/// The agent id the check's attribution hashes.
pub fn check_agent_id() -> Uuid {
    // A fixed id ("ligus_gw_check" in ASCII, then 1): not derived from anything.
    Uuid::from_u128(0x6c69_6775_735f_6777_5f63_6865_636b_0001)
}

/// Exit 0 when every check passes, else 1 (6 with `legacy_exit_codes`, as
/// before 0.2, for one release).
pub async fn run(workspace: Option<Uuid>, only: Option<String>, model: String, legacy_exit_codes: bool) -> Result<i32> {
    if model_gateway::host().is_empty() {
        return Err(anyhow!("this host lists no model gateways (set LIGUS_MODEL_GATEWAYS_FILE)"));
    }
    let checks = checks(workspace, only, model, true).await;
    let failed = checks.iter().any(|c| c.status == Status::Fail);
    Ok(match (failed, legacy_exit_codes) {
        (false, _) => 0,
        (true, true) => 6,
        (true, false) => crate::help::EXIT_ERROR,
    })
}

/// The gateway checks as `ligus.checks.v1` checks (`gateway.<name>.hosted`,
/// `gateway.<name>.cli`, `gateways.unlisted`); one `skip` when this host
/// lists none. With `print`, also prints `gateway-check`'s own text as it
/// goes.
pub async fn checks(workspace: Option<Uuid>, only: Option<String>, model: String, print: bool) -> Vec<Check> {
    let say = |line: String| {
        if print {
            println!("{line}");
        }
    };
    let gws = model_gateway::host();
    let mut out = Vec::new();
    if gws.is_empty() {
        out.push(Check::skip("gateways", "model gateways", "this host lists none (LIGUS_MODEL_GATEWAYS_FILE)"));
        return out;
    }
    const FIX: &str = "check the gateway's entry in LIGUS_MODEL_GATEWAYS_FILE and its credential file; ligus gateway-check prints the probe's findings";
    for g in gws.all().iter().filter(|g| only.as_deref().is_none_or(|n| n == g.name())) {
        say(format!("gateway {} ({} wire, {})", g.name(), g.api().as_str(), if gws.exclusive() { "exclusive host" } else { "non-exclusive host" }));
        let sel = GatewaySelection::named(g.name()).with_ids(workspace, Some(check_agent_id()));
        let (id, title) = (format!("gateway.{}.hosted", g.name()), format!("{} hosted", g.name()));
        match hosted(&sel, g.api(), &model).await {
            Ok(n) => {
                say(format!("  hosted: ok ({n} bytes of reply)"));
                out.push(Check::ok(&id, &title, format!("a hosted-API turn answered ({n} bytes)")));
            }
            Err(e) => {
                say(format!("  hosted: FAILED: {e}"));
                out.push(Check::fail(&id, &title, format!("{e:#}")).fix(FIX));
            }
        }
        let (id, title) = (format!("gateway.{}.cli", g.name()), format!("{} cli", g.name()));
        let mut probe = Vec::new();
        let r = cli(g, &sel, &model, &mut probe).await;
        for line in &probe {
            say(format!("    {line}"));
        }
        match r {
            Ok(()) => {
                say("  cli: ok (credential injected by the proxy; the child never saw it)".into());
                out.push(Check::ok(&id, &title, "credential injected by the proxy; the sandboxed child never saw it").notes(probe));
            }
            Err(e) => {
                say(format!("  cli: FAILED: {e}"));
                out.push(Check::fail(&id, &title, format!("{e:#}")).fix(FIX).notes(probe));
            }
        }
    }
    // An endpoint that isn't listed is refused on an exclusive host.
    if gws.exclusive() {
        let rt = runtime(ModelProviderKind::Custom, "https://unlisted.invalid/v1/chat/completions", &model, None);
        match ligus_runtime::provider::dispatch::model_route(&rt) {
            Err(e) => {
                say(format!("unlisted endpoint: refused ({e})"));
                out.push(Check::ok("gateways.unlisted", "unlisted model", format!("an unlisted endpoint is refused ({e})")));
            }
            Ok(_) => {
                say("unlisted endpoint: FAILED: it was not refused".into());
                out.push(
                    Check::fail("gateways.unlisted", "unlisted model", "an endpoint the gateways file doesn't list was not refused on an exclusive host")
                        .fix("report it: an exclusive host must refuse every unlisted model endpoint"),
                );
            }
        }
    }
    out
}

fn runtime(provider: ModelProviderKind, endpoint: &str, model: &str, gateway: Option<GatewaySelection>) -> ResolvedRuntime {
    ResolvedRuntime {
        provider,
        model: model.to_string(),
        endpoint: endpoint.to_string(),
        api_key: None,
        args: Vec::new(),
        model_provider_id: None,
        model_base_url: None,
        context_window_tokens: None,
        keep_alive: None,
        think: None,
        tools: false,
        max_tool_rounds: None,
        allow_remote_file_tools: false,
        driver: Default::default(),
        gateway,
    }
}

async fn hosted(sel: &GatewaySelection, api: Api, model: &str) -> Result<usize> {
    let provider = match api {
        Api::Anthropic => ModelProviderKind::Anthropic,
        Api::Openai => ModelProviderKind::Custom,
    };
    let rt = runtime(provider, "", model, Some(sel.clone()));
    let turns = [ChatTurn::user("ligus gateway check: reply with one word.")];
    let reply = tokio::time::timeout(
        Duration::from_secs(60),
        ligus_runtime::provider::stream(&rt, None, &turns, None, &[], 32, |_| {}, |_| {}),
    )
    .await
    .map_err(|_| anyhow!("no answer in 60 s"))??;
    if reply.trim().is_empty() {
        return Err(anyhow!("empty reply"));
    }
    Ok(reply.len())
}

async fn cli(g: &model_gateway::Gateway, sel: &GatewaySelection, model: &str, probe: &mut Vec<String>) -> Result<()> {
    let exe = std::env::current_exe()?;
    let secrets: Vec<String> = g.secret_files().iter().map(|p| p.display().to_string()).collect();
    let target = gateway_host_port(g);
    let mut args: Vec<String> = vec![PROBE_ARG.into(), "--api".into(), g.api().as_str().into(), "--model".into(), model.into(), "--direct".into(), target];
    for s in &secrets {
        args.push("--secret".into());
        args.push(s.clone());
    }
    let scope = RuntimeScope { gateway: Some(sel.clone()), ..Default::default() };
    let policy = TurnPolicy { worktree_is_isolated: true, ..TurnPolicy::baseline(HostKind::Serve) };
    let out = executor::with_runtime(
        scope,
        executor::with_turn(policy, async {
            let (mut cmd, guard) = executor::tokio_command(SpawnSpec::new(Purpose::Agent(ModelProviderKind::Hermes), exe.as_os_str(), &args))?;
            cmd.stdin(std::process::Stdio::null());
            let out = tokio::time::timeout(Duration::from_secs(90), cmd.output()).await.map_err(|_| anyhow!("probe timed out"))??;
            drop(guard);
            Ok::<_, anyhow::Error>(out)
        }),
    )
    .await?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    // The credential values, compared here and never printed.
    let creds: Vec<String> = g
        .auth_headers()?
        .iter()
        .map(|(_, v)| v.as_str().trim_start_matches("Bearer ").to_string())
        .filter(|v| v.len() >= 8)
        .collect();
    if creds.iter().any(|c| text.contains(c.as_str())) {
        return Err(anyhow!("the child's environment, argv or output contained the gateway credential"));
    }
    // The probe's findings (never its `env` dump, which may hold anything).
    probe.extend(text.lines().filter(|l| l.starts_with("probe:")).map(str::to_string));
    if text.lines().any(|l| l.starts_with("probe: read ") && l.ends_with(" READABLE")) {
        return Err(anyhow!("the child could read a credential file"));
    }
    if text.contains("probe: direct CONNECTED") {
        return Err(anyhow!("the child reached the gateway directly, around the proxy"));
    }
    if !text.contains("probe: route status 200") {
        return Err(anyhow!("the request through the route didn't succeed"));
    }
    Ok(())
}

fn gateway_host_port(g: &model_gateway::Gateway) -> String {
    g.host_port()
}

/// The child side: runs inside the agent sandbox. Prints what it can see
/// (lines starting `probe:`) and the full environment for the parent to scan.
pub fn maybe_run_probe() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some(PROBE_ARG) {
        return;
    }
    let val = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    let secrets: Vec<String> = args.windows(2).filter(|w| w[0] == "--secret").map(|w| w[1].clone()).collect();
    println!("probe: argv {}", args.join(" "));
    for (k, v) in std::env::vars() {
        println!("env {k}={v}");
    }
    for s in &secrets {
        let r = std::fs::read(s);
        println!("probe: read {s} {}", if r.is_ok() { "READABLE" } else { "denied" });
    }
    let api = val("--api").unwrap_or_else(|| "openai".into());
    let model = val("--model").unwrap_or_else(|| "ligus-gateway-check".into());
    let (url, body) = if api == "anthropic" {
        let base = std::env::var("ANTHROPIC_BASE_URL").unwrap_or_default();
        (format!("{base}/v1/messages"), serde_json::json!({"model": model, "max_tokens": 16, "messages": [{"role": "user", "content": "ping"}]}).to_string())
    } else {
        let base = std::env::var("OPENAI_BASE_URL").unwrap_or_default();
        (format!("{base}/chat/completions"), serde_json::json!({"model": model, "messages": [{"role": "user", "content": "ping"}]}).to_string())
    };
    println!("probe: base url is the proxy route: {}", url.contains("/ligus-gw/"));
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    rt.block_on(async {
        let client = ligus_forge::http_client_builder().no_proxy().timeout(Duration::from_secs(30)).build().expect("client");
        // A credential of the child's own: the proxy must replace it.
        let r = client
            .post(&url)
            .header("authorization", "Bearer forged-by-child")
            .header("x-api-key", "forged-by-child")
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await;
        match r {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let text = resp.text().await.unwrap_or_default();
                println!("probe: route status {status}");
                println!("probe: route body {}", text.chars().take(200).collect::<String>().replace('\n', " "));
            }
            Err(e) => println!("probe: route error {e}"),
        }
        if let Some(target) = val("--direct") {
            let r = tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(target.as_str())).await;
            println!("probe: direct {}", if matches!(r, Ok(Ok(_))) { "CONNECTED" } else { "blocked" });
        }
    });
    std::process::exit(0);
}

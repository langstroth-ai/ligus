//! `ligus workspace rules [upgrade]`: the workspace's rules version, and the
//! Owner's switch to rules 4 (workspace model v3, 15.2).
//!
//! * `ligus workspace rules [--json]` prints the version and, below rules 4,
//!   what the switch changes and the readiness list: devices on a build
//!   below 0.5 by name and build (they go read-only), devices that haven't
//!   said which build they run, agents whose placement wouldn't resolve,
//!   host records not signed by their device, accountless members and model
//!   server addresses on the log.
//! * `ligus workspace rules upgrade [--dry-run] [--json] [--no-wait |
//!   --timeout D] [--recovery-words-file PATH]` switches: the Owner only,
//!   one passkey (handed off to the person's desktop or phone: exit 4 with
//!   `--no-wait`, finished with `ligus handoff wait`), and never back.

use anyhow::Result;

use crate::problem::{Problem, EXIT_HUMAN, EXIT_REFUSED};
use crate::{help, Config};

fn report_json(r: &ligus_runtime::chat_service::Rules4Report) -> serde_json::Value {
    let dev = |d: &ligus_runtime::chat_service::DeviceBuild| serde_json::json!({ "device": d.device_id, "name": d.name, "person": d.person, "build": d.build });
    serde_json::json!({
        "rules": r.rules,
        "isOwner": r.is_owner,
        "changes": ligus_runtime::chat_service::RULES4_CHANGES,
        "readiness": if r.rules >= ligus_core::authz::RULES_V4 { Vec::new() } else { r.lines() },
        "oldDevices": r.old_devices.iter().map(dev).collect::<Vec<_>>(),
        "unknownDevices": r.unknown_devices.iter().map(dev).collect::<Vec<_>>(),
        "unresolvedAgents": r.unresolved_agents.iter().map(|(id, n)| serde_json::json!({ "id": id, "name": n })).collect::<Vec<_>>(),
        "unsignedHosts": r.unsigned_hosts,
        "accountlessMembers": r.accountless_members,
        "endpointRuntimes": r.endpoint_runtimes.iter().map(|e| serde_json::json!({ "id": e.runtime_id, "name": e.name, "agents": e.agents.iter().map(|a| a.1.clone()).collect::<Vec<_>>() })).collect::<Vec<_>>(),
    })
}

fn print_readiness(r: &ligus_runtime::chat_service::Rules4Report) {
    println!("Switching to rules 4:");
    for c in ligus_runtime::chat_service::RULES4_CHANGES {
        println!("  - {c}");
    }
    let lines = r.lines();
    if lines.is_empty() {
        println!("Readiness: nothing here changes for anyone.");
    } else {
        println!("Readiness:");
        for l in lines {
            println!("  - {l}");
        }
    }
}

/// `ligus workspace rules …`. `rest` is everything after `workspace`.
pub fn cmd(cfg: &Config, rest: &[String]) -> Result<()> {
    let positional = crate::positional_args(rest);
    let sub = positional.get(1).map(|s| s.as_str());
    let has = |f: &str| rest.iter().any(|a| a == f);
    let json = has("--json");
    let mut svc = crate::open_service(cfg)?;
    let ws = crate::uuid_of_room(&cfg.room);
    // The builds come from the hosts' signed beats, over the relay.
    svc.set_host_presence(crate::fetch_presence_with(cfg, Some(&svc)));
    let report = svc.rules4_report(ws)?;
    match sub {
        None => {
            if json {
                println!("{}", report_json(&report));
                return Ok(());
            }
            println!("rules {}", report.rules);
            if report.rules < ligus_core::authz::RULES_V4 {
                print_readiness(&report);
                let next = if report.is_owner { "ligus workspace rules upgrade --dry-run" } else { "the workspace's Owner runs ligus workspace rules upgrade" };
                println!("next: {next}");
            }
            Ok(())
        }
        Some("upgrade") => upgrade(cfg, &mut svc, ws, &report, rest, json),
        Some(other) => Err(help::usage_error(format!("unknown `ligus workspace rules {other}`; use `ligus workspace rules [upgrade]`"))),
    }
}

fn upgrade(cfg: &Config, svc: &mut ligus_runtime::ChatService, ws: uuid::Uuid, report: &ligus_runtime::chat_service::Rules4Report, rest: &[String], json: bool) -> Result<()> {
    let has = |f: &str| rest.iter().any(|a| a == f);
    let val = |f: &str| rest.iter().position(|a| a == f).and_then(|i| rest.get(i + 1)).cloned();
    if report.rules >= ligus_core::authz::RULES_V4 {
        if json {
            println!("{}", serde_json::json!({ "rules": report.rules, "changed": false }));
        } else {
            println!("already on rules {}: nothing to do", report.rules);
        }
        return Ok(());
    }
    if !report.is_owner {
        return Err(anyhow::Error::new(Problem {
            exit: EXIT_REFUSED,
            code: "refused",
            message: "only the workspace's Owner upgrades its rules".into(),
            fix: Some("ask the Owner to run ligus workspace rules upgrade, or use Settings > Workspace rules on their desktop".into()),
        }));
    }
    if has("--dry-run") {
        if json {
            let mut v = report_json(report);
            v["dryRun"] = true.into();
            println!("{v}");
        } else {
            print_readiness(report);
            println!("dry run: nothing was written · next: ligus workspace rules upgrade");
        }
        return Ok(());
    }
    // The passkey: a recovery-words file, else the hand-off to the person's
    // desktop or phone (waiting, or exit 4 with --no-wait).
    let mut end = None;
    if let Some(path) = val("--recovery-words-file") {
        let key = crate::secret_arg::recovery_key_from_file(std::path::Path::new(&path))?;
        svc.set_presence_provider(Some(std::sync::Arc::new(crate::admit::RecoveryPresence(key))));
    } else {
        let wait = if has("--no-wait") {
            None
        } else {
            Some(match val("--timeout") {
                Some(v) => crate::cli_flags::parse_duration(&v)?,
                None => crate::handoff::DEFAULT_TIMEOUT,
            })
        };
        if let Some((p, e)) = crate::handoff::HandoffPresence::for_service(cfg, svc, &crate::handoff::command_label(), wait) {
            svc.set_presence_provider(Some(std::sync::Arc::new(p)));
            end = Some(e);
        }
    }
    match svc.upgrade_workspace_rules(ws) {
        Ok(_) => {
            if json {
                println!("{}", serde_json::json!({ "rules": ligus_core::authz::RULES_V4, "changed": true, "readOnly": report.old_devices.iter().map(|d| d.name.clone()).collect::<Vec<_>>() }));
            } else {
                println!("done: this workspace is on rules 4. Share it with `ligus sync`.");
                for d in &report.old_devices {
                    println!("  {} is read-only here until it's updated to 0.5", d.name);
                }
            }
            Ok(())
        }
        Err(e) if e.to_string().contains(ligus_core::authz::PRESENCE_REQUIRED) => {
            let rerun = "ligus workspace rules upgrade";
            let next = end.and_then(|e| e.lock().unwrap().clone()).and_then(|e| crate::handoff::next_lines(&e, rerun)).unwrap_or_else(|| {
                vec![format!("this needs your passkey: confirm it on your desktop (Settings > Workspace rules), or run {rerun} --recovery-words-file PATH")]
            });
            if json {
                println!("{}", serde_json::json!({ "rules": report.rules, "changed": false, "next": next }));
            } else {
                println!("waiting for a person · next: {}", next.join(" ; "));
            }
            Err(anyhow::Error::new(Problem { exit: EXIT_HUMAN, code: "passkey_needed", message: "the switch needs your passkey".into(), fix: next.first().cloned() }))
        }
        Err(e) => Err(e.into()),
    }
}

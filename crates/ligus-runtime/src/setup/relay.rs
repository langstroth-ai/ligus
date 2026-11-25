//! Set up a relay: the community relay or your own (https only; plain http
//! only on this machine) → check it → switch this device's copy of the
//! workspace to it → check again.

use super::agent::ops_code;
use super::validate::{origin, url_problem, value};
use super::{exit, Choice, Ctx, Field, FieldKind, Inputs, ProbeFact, Progress, Secrets, SetupOps, StepDef};

pub(crate) fn status(ops: &dyn SetupOps) -> (Progress, String) {
    match ops.relay_url() {
        Some(r) => (Progress::Done, origin(&r)),
        None => (Progress::Todo, "no relay yet (this device only)".into()),
    }
}

pub(crate) fn steps(_ops: &dyn SetupOps, _inputs: &Inputs) -> Vec<StepDef> {
    vec![
        StepDef::input(
            "relay",
            "Relay",
            vec![
                Field::new(
                    "kind",
                    "Relay",
                    FieldKind::Choice {
                        options: vec![
                            Choice::new("community", "Community relay").hint("relay.langstroth.ai, best-effort"),
                            Choice::new("own", "Your own relay").hint("langstroth-relay on a server you run"),
                        ],
                    },
                )
                .required()
                .default("community")
                .help("The relay stores only end-to-end encrypted events. Every member's devices use the same one."),
                Field::new("url", "URL", FieldKind::Text)
                    .required()
                    .when("kind", &["own"])
                    .help("https://, or http:// only for a relay on this machine."),
            ],
        ),
        StepDef::input(
            "token",
            "Access token",
            vec![Field::new("token", "Access token", FieldKind::Secret)
                .help("Only if the relay asks for one (hrt1…). It stays in this device's secret store.")],
        ),
        StepDef::verify("verify", "Connected"),
    ]
}

pub(crate) fn preflight(_ctx: &mut Ctx, _ops: &dyn SetupOps, _inputs: &Inputs) {}

/// Judge one probe. `Ok(())` passes; `Err` has stopped the run.
fn judge(ctx: &mut Ctx, id: &str, url: &str, probe: ProbeFact, had_token: bool) -> Result<(), ()> {
    let o = origin(url);
    match probe {
        ProbeFact::Ok => {
            ctx.ok(id, "relay", format!("{o}: reachable, access accepted"));
            Ok(())
        }
        // A relay with nothing stored for this workspace yet.
        ProbeFact::HttpStatus(404) => {
            ctx.warn(id, "relay", format!("{o}: reachable; nothing stored for this workspace yet"), None);
            Ok(())
        }
        ProbeFact::Unauthorized if had_token => {
            ctx.fail(id, "relay", format!("{o}: the access token was refused"), "ask the relay's operator for a valid token, then pass --token -", exit::FIXABLE);
            Err(())
        }
        ProbeFact::Unauthorized => {
            ctx.fail(id, "relay", format!("{o}: it wants an access token"), "pass --token - (on stdin) or --token @FILE", exit::FIXABLE);
            Err(())
        }
        ProbeFact::HttpStatus(s) => {
            ctx.fail(id, "relay", format!("{o}: answered HTTP {s}"), format!("on the relay host: langstroth-relay check --url {o}"), exit::FAILED);
            Err(())
        }
        ProbeFact::Unreachable(why) => {
            ctx.fail(id, "relay", format!("{o}: {why}"), format!("check the URL and this network; on the relay host: langstroth-relay check --url {o}"), exit::OFFLINE);
            Err(())
        }
    }
}

pub(crate) fn run(ctx: &mut Ctx, ops: &mut dyn SetupOps, inputs: &Inputs, secrets: &Secrets) {
    let fields: Vec<Field> = steps(ops, inputs).into_iter().flat_map(|s| s.fields).collect();
    let url = match value(inputs, &fields, "kind").unwrap_or("community") {
        "own" => value(inputs, &fields, "url").unwrap_or("").trim().trim_end_matches('/').to_string(),
        _ => ligus_core::COMMUNITY_RELAY_URL.to_string(),
    };
    if let Some(p) = url_problem(&url) {
        return ctx.fail("input.url", "URL", p, "pass --url https://…", exit::USAGE);
    }
    let token = secrets.get("token");
    let current = ops.relay_url().map(|u| u.trim().trim_end_matches('/').to_string());
    if current.as_deref() == Some(url.as_str()) && token.is_none() {
        if judge(ctx, "check", &url, ops.probe_relay(&url, None), false).is_err() {
            return;
        }
        ctx.ok("switch", "workspace", "already uses this relay");
        return ctx.done();
    }
    if ctx.cancelled() {
        return;
    }
    if judge(ctx, "check", &url, ops.probe_relay(&url, token), token.is_some()).is_err() {
        return;
    }
    if ctx.dry_run() {
        ctx.skip("switch", "workspace", format!("would switch this device to {}", origin(&url)));
        return ctx.done();
    }
    if ctx.cancelled() {
        return;
    }
    if let Err(e) = ops.switch_relay(&url, token) {
        return ctx.fail("switch", "workspace", format!("couldn't switch: {e}"), "nothing changed; fix what it says and run this again", ops_code(&e));
    }
    ctx.commit();
    ctx.ok("switch", "workspace", format!("this device now syncs through {}", origin(&url)));
    if ops.my_role().is_some() {
        ctx.warn(
            "members",
            "members",
            "every member's devices must use the same relay; new invites carry this one",
            Some("on each device: Set up a relay (ligus setup relay)".into()),
        );
    }
    if ops.relay_url().map(|u| u.trim().trim_end_matches('/').to_string()).as_deref() != Some(url.as_str()) {
        return ctx.fail("verify", "verify", "the workspace doesn't name the new relay", "run this again", exit::FAILED);
    }
    if judge(ctx, "verify", &url, ops.probe_relay(&url, token), token.is_some()).is_ok() {
        ctx.done();
    }
}
